//! Span-keyed side tables must not leak from the user program into the baked
//! stdlib body pass (B-2026-09-04-5).
//!
//! # Why this class exists at all
//!
//! `Span` is `(line, column, offset, length)` — it carries **no file
//! identity**. Every span-keyed side table is therefore keyed by byte offset
//! alone, in a namespace shared by the user program and each baked stdlib
//! module (`process.kara`, `cli.kara`, …). `Codegen::compile_stdlib_program`
//! already knows this: it swaps the stdlib program's own tables in for the
//! duration of body emission, and its doc comment states the rule as "swap ALL
//! program-derived span tables (not just the few the current bodies touch) so a
//! future edit doesn't silently miscompile".
//!
//! Fourteen tables had fallen out of that list. `fn_value_typed_exprs` was the
//! one that bit: `process.kara` byte 5150 length 13 is `self.cmd_args`, and
//! byte 5150 length 13 of a kata in the corpus was `merge_k_lists` — a free-fn
//! name the lowering pass records as a `Fn(..)`-typed expression. Compiling
//! `Command.arg`'s `let mut new_args = self.cmd_args;` then read the USER's
//! entry, took `let_binding_fn_value_type`'s first-class-fn-value branch, bound
//! the `Vec[String]` as a closure fat pointer and returned early — so
//! `vec_elem_types` / `var_type_names` were never registered and the next
//! line's `new_args.push(a)` fell out of method dispatch entirely.
//!
//! Two properties make this class expensive to diagnose, and both are why this
//! file holds a STATIC guard as well as a functional one:
//!
//! - **The blast radius is a module the program never mentions.** The kata has
//!   no imports and never touches `Command`; the failure named a variable and a
//!   file it had never heard of, at a span rendered against its own source (a
//!   comment line).
//! - **It is invisible in the small.** A program is only exposed when its own
//!   text is long enough to reach the colliding offset, so every reduction of
//!   the failing kata passed. There is no minimal repro to find.

/// The functional half: a user program whose call-site callee identifier sits
/// at exactly the same `(offset, length)` as a `Vec`-typed field read inside a
/// baked stdlib method body still compiles.
///
/// The offset is RECOMPUTED from `runtime/stdlib/process.kara` at test time
/// rather than hard-coded, so an edit to that file moves the test with it
/// instead of quietly making it vacuous.
#[cfg(feature = "llvm")]
#[test]
fn a_user_expr_colliding_with_a_stdlib_span_does_not_break_the_stdlib_body() {
    const PROCESS_SRC: &str = include_str!("../runtime/stdlib/process.kara");

    // `Command.arg`'s `let mut new_args = self.cmd_args;` — the RHS is the
    // Vec-typed field read whose binding the collision mis-classified.
    const ANCHOR_STMT: &str = "let mut new_args = self.cmd_args;";
    const ANCHOR_RHS: &str = "self.cmd_args";

    let stmt_at = PROCESS_SRC.find(ANCHOR_STMT).unwrap_or_else(|| {
        panic!(
            "`{ANCHOR_STMT}` is gone from runtime/stdlib/process.kara. This test \
             pins a span COLLISION against that statement; pick another baked \
             stdlib `let <name> = self.<vec field>;` and update the anchors, \
             rather than deleting the test — the hazard it guards is structural \
             (`Span` carries no file identity), not specific to `Command.arg`."
        )
    });
    let target_offset = stmt_at + ANCHOR_STMT.find(ANCHOR_RHS).unwrap();
    let target_len = ANCHOR_RHS.len();

    // A free fn whose NAME is exactly as long as the stdlib expression, so the
    // user call site can be padded to land on the same `(offset, length)` key.
    let callee = "c".repeat(target_len);
    let head = format!("fn {callee}(x: i64) -> i64 {{ x + 1 }}\n");
    let tail = format!("fn main() {{\n    let r = {callee}(41);\n    println(f\"{{r}}\");\n}}\n");

    // Pad with a line comment so the call-site callee identifier begins at
    // `target_offset`. The callee sits `tail_prefix` bytes into `tail`.
    let tail_prefix = tail.find(&format!("{callee}(41)")).unwrap();
    let fixed = head.len() + tail_prefix;
    assert!(
        target_offset > fixed + 3,
        "stdlib anchor at {target_offset} is too early to pad up to; \
         fixed prefix is {fixed} bytes"
    );
    // "//" + pad + "\n" occupies `pad + 3` bytes.
    let pad = target_offset - fixed - 3;
    let src = format!("{head}//{}\n{tail}", "-".repeat(pad));

    // The padding must have landed the identifier exactly on the stdlib key,
    // or the test would pass vacuously against no collision at all.
    assert_eq!(
        src.len() - tail.len() + tail_prefix,
        target_offset,
        "padding arithmetic is off; the user identifier did not land on the \
         stdlib span"
    );
    assert_eq!(&src[target_offset..target_offset + target_len], callee);
    assert_eq!(
        &PROCESS_SRC[target_offset..target_offset + target_len],
        ANCHOR_RHS,
        "the recomputed offset does not point at `{ANCHOR_RHS}` in process.kara"
    );

    let mut parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    karac::prepare_for_resolve(&mut parsed.program);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);

    // Before the fix this failed with "no handler for method 'push' on variable
    // 'new_args'" — an error naming a local in a module this program never
    // references.
    let ir = karac::codegen::compile_to_ir(&parsed.program, None, None)
        .unwrap_or_else(|e| panic!("codegen failed on a span-colliding program: {}", e.message));
    assert!(ir.contains("define"), "expected a non-empty module");
}

/// The static half: every span table installed from the user program in
/// `compile_program` must also be swapped for the stdlib body pass.
///
/// This is the guard that generalizes. The functional test above pins ONE
/// table through ONE collision; this one fails the moment a fifteenth table is
/// added to the install list and not to the swap list — which is exactly how
/// the fourteen accumulated, since nothing about omitting one is visible at the
/// point of writing it.
#[test]
fn stdlib_body_pass_swaps_every_program_derived_span_table() {
    const CODEGEN_SRC: &str = include_str!("../src/codegen.rs");

    // `self.span_tables.<name> = program.<name>.clone();`, allowing the
    // rustfmt line break before the `=` RHS.
    let installed: std::collections::BTreeSet<String> = CODEGEN_SRC
        .split("self.span_tables.")
        .skip(1)
        .filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            let after = &rest[name.len()..];
            let after = after.trim_start_matches([' ', '\n']);
            after.starts_with("= program.").then_some(name)
        })
        .collect();

    // Everything the `swap_all!` macro body hands to `std::mem::swap`. The
    // macro is the only place in the file that takes `&mut self.span_tables.X`
    // followed by a `&mut t_` sibling.
    let swapped: std::collections::BTreeSet<String> = CODEGEN_SRC
        .split("&mut self.span_tables.")
        .skip(1)
        .filter_map(|rest| {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            rest[name.len()..]
                .trim_start_matches([',', ' ', '\n'])
                .starts_with("&mut t_")
                .then_some(name)
        })
        .collect();

    assert!(
        !installed.is_empty() && !swapped.is_empty(),
        "the source-scan patterns matched nothing — codegen.rs was refactored \
         and this guard needs updating, not deleting (installed={}, swapped={})",
        installed.len(),
        swapped.len(),
    );

    let missing: Vec<&String> = installed.difference(&swapped).collect();
    assert!(
        missing.is_empty(),
        "these span tables are installed from the USER program but not swapped \
         for the baked-stdlib body pass, so a stdlib body compiles while \
         reading the user's entries: {missing:?}\n\n\
         `Span` carries no file identity, so a stdlib expression sharing an \
         (offset, length) with a user expression silently reads the wrong \
         entry. Add each to both the `let mut t_… = tp.….clone();` block and \
         the `swap_all!` macro in `compile_stdlib_program`."
    );
}
