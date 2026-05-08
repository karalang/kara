//! Integration tests for the `karac repl` binary's `Session` evaluator.
//!
//! Tests exercise the cell pipeline directly without driving rustyline
//! through a TTY. `Session::evaluate_cell_captured` routes interpreter
//! `println` output into an in-memory buffer so we can assert against it
//! without touching the process's real stdout fd.

use karac::repl::{ReplOptions, Session};

// ── Item accumulation ──────────────────────────────────────────────────────

#[test]
fn item_definition_persists_across_cells() {
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("fn double(n: i64) -> i64 { n * 2 }");
    assert!(r.errors.is_empty(), "fn definition: {:?}", r.errors);
    assert!(s.items_source().contains("fn double"));
    let r = s.evaluate_cell_captured("println(double(7));");
    assert!(r.errors.is_empty(), "call site: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "14");
}

#[test]
fn struct_definition_persists_across_cells() {
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("struct Point { x: i64, y: i64 }");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let r = s.evaluate_cell_captured("let p: Point = Point { x: 3, y: 4 }; println(p.x + p.y);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "7");
}

#[test]
fn redeclaring_item_within_cell_does_not_panic() {
    // Two `fn f` in the SAME cell is the resolver's call to make — the
    // REPL does not pre-prune intra-cell duplicates. Whether it accepts
    // or rejects, the surface must not panic.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured(
        "fn f() -> i64 { 1 }\n\
         fn f() -> i64 { 2 }",
    );
    let _ = r;
}

#[test]
fn redeclaring_item_across_cells_shadows() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn f() -> i64 { 1 }");
    s.evaluate_cell_captured("fn f() -> i64 { 2 }");
    let r = s.evaluate_cell_captured("println(f());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "2");
}

#[test]
fn redeclaring_struct_across_cells_shadows() {
    let mut s = Session::new();
    s.evaluate_cell_captured("struct Point { x: i64 }");
    s.evaluate_cell_captured("struct Point { x: i64, y: i64 }");
    let r = s.evaluate_cell_captured("let p = Point { x: 3, y: 4 }; println(p.x + p.y);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "7");
}

#[test]
fn redeclaring_enum_across_cells_shadows() {
    let mut s = Session::new();
    s.evaluate_cell_captured("enum Color { Red, Green }");
    s.evaluate_cell_captured("enum Color { Red, Green, Blue }");
    let r = s.evaluate_cell_captured(
        "let c = Color.Blue; \
         match c { Color.Blue => println(3), _ => println(0) }",
    );
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "3");
}

#[test]
fn redeclaring_const_across_cells_shadows() {
    let mut s = Session::new();
    s.evaluate_cell_captured("const MAX_VALUE: i64 = 1;");
    s.evaluate_cell_captured("const MAX_VALUE: i64 = 99;");
    let r = s.evaluate_cell_captured("println(MAX_VALUE);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "99");
}

#[test]
fn redeclaring_distinct_type_across_cells_shadows() {
    let mut s = Session::new();
    s.evaluate_cell_captured("distinct type UserId = i64;");
    s.evaluate_cell_captured("distinct type UserId = i64;");
    // The second submission must not duplicate the prior decl in
    // items_source. The assertion guards against the resolver seeing two
    // copies of `distinct type UserId` (which would surface a
    // duplicate-name error).
    let count = s.items_source().matches("distinct type UserId").count();
    assert_eq!(
        count,
        1,
        "expected the prior `distinct type UserId` to be pruned, got items_source: {:?}",
        s.items_source(),
    );
}

#[test]
fn redeclaring_only_strips_matching_items() {
    // Cell 1 introduces fn one + fn two; cell 2 redeclares only one.
    // After the prune, items_source must still contain `fn two`.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn one() -> i64 { 1 }\nfn two() -> i64 { 2 }");
    s.evaluate_cell_captured("fn one() -> i64 { 100 }");
    let r = s.evaluate_cell_captured("println(one() + two());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "102");
}

#[test]
fn impl_blocks_are_not_shadowed_by_other_impl_blocks() {
    // Impl blocks are anonymous — multiple impls for the same target type
    // can coexist, and the prune must leave them all in place.
    let mut s = Session::new();
    s.evaluate_cell_captured("struct P { x: i64 }");
    s.evaluate_cell_captured("impl P { fn get_x(ref self) -> i64 { self.x } }");
    s.evaluate_cell_captured("impl P { fn double_x(ref self) -> i64 { self.x * 2 } }");
    let r = s.evaluate_cell_captured("let p = P { x: 5 }; println(p.get_x() + p.double_x());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "15");
}

#[test]
fn redeclaring_keeps_items_source_parseable() {
    // After several rounds of shadowing, the buffer should still be a
    // syntactically valid sequence of items.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn f() -> i64 { 1 }");
    s.evaluate_cell_captured("struct S { v: i64 }");
    s.evaluate_cell_captured("fn f() -> i64 { 2 }");
    s.evaluate_cell_captured("struct S { v: i64, w: i64 }");
    let r = s.evaluate_cell_captured("let s = S { v: 10, w: 20 }; println(f() + s.v + s.w);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "32");
}

// ── Statement cells ────────────────────────────────────────────────────────

#[test]
fn statement_cell_executes_and_prints() {
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("println(1 + 2);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "3");
}

#[test]
fn statement_cell_can_use_session_items() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn add(a: i64, b: i64) -> i64 { a + b }");
    let r = s.evaluate_cell_captured("println(add(5, 6));");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "11");
}

#[test]
fn statement_cell_with_user_main_shadows_synthetic() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn main() { println(99); }");
    let r = s.evaluate_cell_captured("println(42);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "42");
}

#[test]
fn statement_cell_supports_question_operator() {
    // The synthetic main wraps each cell, so `?` would need the wrapper to
    // return a Result. The MVP uses `fn main()` (Unit return), which the
    // typechecker rejects `?` against. Verify the diagnostic is surfaced
    // cleanly rather than silently miscompiling — this is an explicit
    // limitation of the MVP cell shape, tracked as a follow-up to upgrade
    // the wrapper to `fn main() -> Result[Unit, Error]`.
    let mut s = Session::new();
    s.evaluate_cell_captured(
        "fn parse(flag: bool) -> Result[i64, i64] { if flag { Ok(1_i64) } else { Err(0_i64) } }",
    );
    let r = s.evaluate_cell_captured("let _ = parse(true)?; println(42);");
    // Either the type error fires, or `?` is accepted. Both are
    // observable; the test just pins that the path doesn't panic.
    let surfaced_error = !r.errors.is_empty();
    let printed = r.stdout.trim() == "42";
    assert!(
        surfaced_error || printed,
        "expected either an error message or '42' output; got errors={:?}, stdout={:?}",
        r.errors,
        r.stdout
    );
}

// ── Meta-commands ──────────────────────────────────────────────────────────

#[test]
fn meta_quit_returns_false() {
    let mut s = Session::new();
    assert!(!s.dispatch_meta(":quit"));
    assert!(!s.dispatch_meta(":q"));
    assert!(!s.dispatch_meta(":exit"));
}

#[test]
fn meta_help_does_not_quit() {
    let mut s = Session::new();
    assert!(s.dispatch_meta(":help"));
}

#[test]
fn meta_unknown_does_not_quit() {
    let mut s = Session::new();
    assert!(s.dispatch_meta(":frobnicate"));
}

#[test]
fn meta_save_writes_session_to_file() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    s.evaluate_cell_captured("println(one());");

    let path = std::env::temp_dir().join(format!(
        "karac_repl_save_{}_{}.kara",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    s.dispatch_meta(&format!(":save {}", path.display()));

    let written = std::fs::read_to_string(&path).expect("save wrote a file");
    assert!(written.contains("fn one"));
    assert!(written.contains("println(one())"));
    let _ = std::fs::remove_file(&path);
}

// ── History bookkeeping ────────────────────────────────────────────────────

#[test]
fn cell_history_excludes_meta_commands() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    s.dispatch_meta(":help");
    s.evaluate_cell_captured("println(one());");
    assert_eq!(s.cell_history().len(), 2);
    assert!(s.cell_history()[0].contains("fn one"));
    assert!(s.cell_history()[1].contains("println"));
}

#[test]
fn parse_error_in_cell_does_not_corrupt_session() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn ok() -> i64 { 1 }");
    let r = s.evaluate_cell_captured("fn broken( {");
    assert!(
        !r.errors.is_empty(),
        "expected parse error; got stdout={:?} errors={:?}",
        r.stdout,
        r.errors
    );
    assert!(s.items_source().contains("fn ok"));
    let history_has_broken = s.cell_history().iter().any(|c| c.contains("fn broken("));
    assert!(!history_has_broken, "broken cell should not enter history");
}

// ── Persistent let bindings ────────────────────────────────────────────────

#[test]
fn let_in_cell_n_visible_in_cell_n_plus_1() {
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("let x = 5;");
    assert!(r.errors.is_empty(), "let cell errored: {:?}", r.errors);
    let r = s.evaluate_cell_captured("println(x);");
    assert!(r.errors.is_empty(), "use of x errored: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "5");
}

#[test]
fn let_persistence_chains_across_cells() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let x = 5;");
    s.evaluate_cell_captured("let y = x + 10;");
    let r = s.evaluate_cell_captured("println(y);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "15");
}

#[test]
fn let_persistence_rebinding_shadows_earlier_value() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let x = 1;");
    s.evaluate_cell_captured("let x = 99;");
    let r = s.evaluate_cell_captured("println(x);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "99");
}

#[test]
fn let_persistence_carries_type_annotation() {
    // `let x: i64 = 5;` — the annotation is part of the captured slice.
    let mut s = Session::new();
    s.evaluate_cell_captured("let x: i64 = 5;");
    assert_eq!(s.persistent_lets().len(), 1);
    assert!(
        s.persistent_lets()[0].contains(": i64"),
        "type annotation lost in capture: {:?}",
        s.persistent_lets()
    );
    let r = s.evaluate_cell_captured("println(x);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "5");
}

#[test]
fn let_persistence_multiple_lets_in_one_cell() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let a = 1; let b = 2; let c = 3;");
    assert_eq!(
        s.persistent_lets().len(),
        3,
        "expected 3 captured lets, got: {:?}",
        s.persistent_lets()
    );
    let r = s.evaluate_cell_captured("println(a + b + c);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "6");
}

#[test]
fn let_persistence_skips_non_let_statements() {
    // `println(1)` is a statement but not a `let` — it must not enter the
    // persistent buffer.
    let mut s = Session::new();
    s.evaluate_cell_captured("let x = 7; println(x);");
    assert_eq!(
        s.persistent_lets().len(),
        1,
        "only `let x = 7;` should persist, got: {:?}",
        s.persistent_lets()
    );
}

#[test]
fn failed_cell_does_not_pollute_persistent_lets() {
    // The cell starts with a clean `let x = 1;` but the SECOND statement
    // is a type error. The whole cell must fail-and-rollback — neither
    // the good binding nor anything else may land in persistent_lets.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("let x = 1; let y: bool = 5;");
    assert!(
        !r.errors.is_empty(),
        "expected type error, got stdout={:?}",
        r.stdout
    );
    assert!(
        s.persistent_lets().is_empty(),
        "failed cell leaked bindings: {:?}",
        s.persistent_lets()
    );
}

#[test]
fn meta_reset_clears_persistent_lets_only() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn helper() -> i64 { 7 }");
    s.evaluate_cell_captured("let x = helper();");
    assert_eq!(s.persistent_lets().len(), 1);
    assert!(s.dispatch_meta(":reset"));
    assert!(s.persistent_lets().is_empty());
    // Items survive — a follow-up cell can still call `helper`.
    let r = s.evaluate_cell_captured("println(helper());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "7");
}

#[test]
fn let_persistence_let_mut_carries_mut_keyword() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let mut counter = 0;");
    assert_eq!(s.persistent_lets().len(), 1);
    assert!(
        s.persistent_lets()[0].contains("mut counter"),
        "let mut keyword lost: {:?}",
        s.persistent_lets()
    );
}

#[test]
fn let_persistence_works_with_session_items() {
    // Cross-paradigm: an item-defined helper plus a persistent let.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn double(n: i64) -> i64 { n * 2 }");
    s.evaluate_cell_captured("let x = double(21);");
    let r = s.evaluate_cell_captured("println(x);");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "42");
}

// ── :dep meta-command ──────────────────────────────────────────────────────

#[test]
fn dep_bare_semver_string_is_recorded() {
    let mut s = Session::new();
    assert!(s.dispatch_meta(":dep http = \"1.2\""));
    let deps = s.pending_deps();
    assert_eq!(deps.len(), 1);
    assert_eq!(
        deps.get("http").map(String::as_str),
        Some("\"1.2\""),
        "stored value should round-trip the version literal"
    );
}

#[test]
fn dep_inline_table_git_form_is_recorded() {
    let mut s = Session::new();
    assert!(s.dispatch_meta(":dep myutil = { git = \"https://github.com/me/myutil-kara\" }"));
    let deps = s.pending_deps();
    assert_eq!(deps.len(), 1);
    let stored = deps.get("myutil").expect("myutil registered");
    assert!(
        stored.starts_with('{'),
        "table form should be stored as-is, got: {stored}"
    );
    assert!(stored.contains("git"));
    assert!(stored.contains("github.com/me/myutil-kara"));
}

#[test]
fn dep_inline_table_path_form_is_recorded() {
    let mut s = Session::new();
    assert!(s.dispatch_meta(":dep mylib = { path = \"./mylib\" }"));
    let stored = s.pending_deps().get("mylib").expect("mylib registered");
    assert!(stored.contains("path"));
    assert!(stored.contains("./mylib"));
}

#[test]
fn dep_repeated_name_overwrites_prior_spec() {
    let mut s = Session::new();
    s.dispatch_meta(":dep http = \"1.2\"");
    s.dispatch_meta(":dep http = \"2.0\"");
    let deps = s.pending_deps();
    assert_eq!(deps.len(), 1, "second :dep should not duplicate the entry");
    assert_eq!(deps.get("http").map(String::as_str), Some("\"2.0\""));
}

#[test]
fn dep_multiple_distinct_names_accumulate() {
    let mut s = Session::new();
    s.dispatch_meta(":dep http = \"1.2\"");
    s.dispatch_meta(":dep json = \"0.9\"");
    s.dispatch_meta(":dep regex = { version = \"1.10\" }");
    assert_eq!(s.pending_deps().len(), 3);
    assert!(s.pending_deps().contains_key("http"));
    assert!(s.pending_deps().contains_key("json"));
    assert!(s.pending_deps().contains_key("regex"));
}

#[test]
fn dep_invalid_syntax_does_not_corrupt_state() {
    let mut s = Session::new();
    s.dispatch_meta(":dep http = \"1.2\"");
    // No `=`; not a valid TOML key/value pair.
    assert!(s.dispatch_meta(":dep totally bogus"));
    // The bad command must not register anything new and must leave the
    // prior good entry alone.
    assert_eq!(s.pending_deps().len(), 1);
    assert!(s.pending_deps().contains_key("http"));
}

#[test]
fn dep_empty_argument_surfaces_usage() {
    let mut s = Session::new();
    // Bare `:dep` with no rest must not panic and must not register
    // anything.
    assert!(s.dispatch_meta(":dep"));
    assert!(s.pending_deps().is_empty());
}

#[test]
fn dep_does_not_break_subsequent_cells() {
    // After a :dep registration, regular item / statement cells should
    // continue to evaluate normally — the meta-command is in-memory only
    // and does not touch the items / let / history buffers.
    let mut s = Session::new();
    s.dispatch_meta(":dep http = \"1.2\"");
    s.evaluate_cell_captured("fn add(a: i64, b: i64) -> i64 { a + b }");
    let r = s.evaluate_cell_captured("println(add(2, 3));");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.stdout.trim(), "5");
    // Dep registry persists across the cells.
    assert_eq!(s.pending_deps().len(), 1);
}

#[test]
fn dep_excluded_from_cell_history() {
    // :dep is a meta-command; it must not enter cell_history (so :save
    // doesn't include it as a Kara source line).
    let mut s = Session::new();
    s.dispatch_meta(":dep http = \"1.2\"");
    s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    assert_eq!(s.cell_history().len(), 1);
    assert!(s.cell_history()[0].contains("fn one"));
}

// ── Notebook-aware use-after-move diagnostic ──────────────────────────────

#[test]
fn cross_cell_uam_names_consuming_cell() {
    // Cell 1 declares the consume sink; cell 2 binds `s`; cell 3 consumes it
    // via `let _ = consume(s);` (the `let` is what makes the consume survive
    // across cells in the v1 source-replay model — `cell_history` doesn't
    // re-execute the bare statement on the next compilation, but
    // `persistent_lets` does). When cell 4 references `s` again, the
    // ownership checker fires UseAfterMove against the synthetic source —
    // and the REPL-aware diagnostic names cell 3 as the consumer.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hello\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let r = s.evaluate_cell_captured("let _u = consume(s);");
    assert!(
        !r.errors.is_empty(),
        "expected UAM error on cross-cell reuse; stdout={:?}, errors={:?}",
        r.stdout,
        r.errors,
    );
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("consumed by cell 3"),
        "expected diagnostic to name cell 3 as consumer; got:\n{joined}",
    );
}

#[test]
fn cross_cell_uam_suggests_clone() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hello\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let r = s.evaluate_cell_captured("let _u = consume(s);");
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("add `.clone()` at the consume site"),
        "expected diagnostic to suggest .clone() at the consume site; got:\n{joined}",
    );
}

#[test]
fn same_cell_uam_uses_baseline_diagnostic() {
    // UAM contained within a single cell — no cross-cell phrasing should
    // be appended. The baseline ownership-checker rendering still surfaces
    // (we just don't decorate it with the notebook-aware tail).
    let mut s = Session::new();
    s.evaluate_cell_captured("fn consume(s: String) {}");
    let r = s.evaluate_cell_captured("let s = \"hi\"; consume(s); let _ = consume(s);");
    assert!(
        !r.errors.is_empty(),
        "expected UAM error in single-cell scenario; errors={:?}",
        r.errors,
    );
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("moved here, used again here"),
        "expected baseline UAM rendering; got:\n{joined}",
    );
    assert!(
        !joined.contains("consumed by cell"),
        "did not expect the cross-cell tail on same-cell UAM; got:\n{joined}",
    );
}

#[test]
fn cross_cell_uam_strictness_unchanged() {
    // The diagnostic enrichment must not change rejection behavior — the
    // program still errors and the failing cell does not enter history.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hi\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let history_before = s.cell_history().len();
    let r = s.evaluate_cell_captured("let _u = consume(s);");
    assert!(
        !r.errors.is_empty(),
        "expected the diagnostic to still reject the program (strictness == .kara)",
    );
    assert!(
        r.stdout.is_empty(),
        "rejected program must produce no stdout; got: {:?}",
        r.stdout,
    );
    // The failing cell rolled back out of history (existing behavior, just
    // pinned here so the slice doesn't accidentally regress it).
    assert_eq!(s.cell_history().len(), history_before);
}

#[test]
fn kara_file_uam_message_unchanged_by_repl_slice() {
    // The .kara compile path goes through the public `ownershipcheck`
    // surface and surfaces the existing `OwnershipError.message` /
    // `.suggestion` fields verbatim. This slice added a `consume_span:
    // Some(span)` field to the UAM error but did not touch message or
    // suggestion text — pin that here so the .kara presentation stays
    // identical.
    let src = "fn consume(s: String) {}\n\
               fn main() {\n\
               let s = \"hi\";\n\
               consume(s);\n\
               consume(s);\n\
               }\n";
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    let owned = karac::ownershipcheck(&parsed.program, &typed);
    let uam = owned
        .errors
        .iter()
        .find(|e| e.kind == karac::ownership::OwnershipErrorKind::UseAfterMove)
        .expect("expected UseAfterMove on the second consume");
    // Baseline format that pre-dates this slice.
    assert!(
        uam.message.contains("moved here, used again here"),
        "baseline UAM message changed; got: {}",
        uam.message,
    );
    assert!(
        uam.suggestion
            .as_deref()
            .is_some_and(|s| s.contains("consider cloning")),
        "baseline UAM suggestion changed; got: {:?}",
        uam.suggestion,
    );
    // The new structural field is populated for downstream REPL-aware
    // rendering — `.kara` callers can still ignore it.
    assert!(
        uam.consume_span.is_some(),
        "UAM should now thread a consume_span (None means the populate-predicate-outputs site is not setting it)",
    );
}

// ── --auto-clone REPL mode ─────────────────────────────────────────────────

#[test]
fn auto_clone_inserts_clone_at_consume_site() {
    // With `--auto-clone` on: cell 1 declares the consume sink, cell 2
    // binds `s`, cell 3 consumes via `let _t = consume(s);` (the v1
    // source-replay model only carries `let`-positioned consumes across
    // cells — same caveat slice 5 documented). Cell 4 reads `s` again,
    // which would normally fire UAM. The auto-clone path rewrites cell
    // 3's stored source to `let _t = consume(s.clone());` so cell 4
    // succeeds, AND the rewrite lands in `cell_history[2]` so `:save`
    // sees the cloned form.
    let mut s = Session::with_options(ReplOptions { auto_clone: true });
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hello\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let r = s.evaluate_cell_captured("println(s);");
    assert!(
        r.errors.is_empty(),
        "expected auto-clone to rewrite the consume site so cell 4 evaluates cleanly; errors={:?}",
        r.errors,
    );
    let history = s.cell_history();
    assert!(
        history.iter().any(|c| c.contains("consume(s.clone())")),
        "expected cell_history to record the rewritten consume; history={history:?}",
    );
}

#[test]
fn auto_clone_emits_perf_note() {
    // Same setup as above; assert the perf-note channel surfaced the
    // insertion. The note carries a stable `perf[auto-clone-in-repl]`
    // code plus the binding name (`s`) so users can audit which sites
    // got rewritten.
    let mut s = Session::with_options(ReplOptions { auto_clone: true });
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hello\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let r = s.evaluate_cell_captured("println(s);");
    assert!(
        r.notes
            .iter()
            .any(|n| n.contains("perf[auto-clone-in-repl]")),
        "expected a perf[auto-clone-in-repl] note to fire; notes={:?}",
        r.notes,
    );
    assert!(
        r.notes.iter().any(|n| n.contains("`s`")),
        "expected the perf note to name the consumed binding; notes={:?}",
        r.notes,
    );
}

#[test]
fn auto_clone_off_keeps_uam_diagnostic() {
    // Without the flag the slice 5 cell-aware UAM diagnostic still
    // surfaces unchanged — auto-clone is opt-in, never the default.
    let mut s = Session::new();
    assert!(!s.auto_clone(), "default Session must keep auto_clone off");
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"hi\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let r = s.evaluate_cell_captured("println(s);");
    assert!(
        !r.errors.is_empty(),
        "expected the cell-aware UAM diagnostic to fire when --auto-clone is off",
    );
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("consumed by cell 3"),
        "expected the slice-5 cross-cell tail; got:\n{joined}",
    );
    assert!(
        r.notes.is_empty(),
        "no perf notes should fire when --auto-clone is off; notes={:?}",
        r.notes,
    );
    // No history rewrite either — the consuming cell's stored source
    // stays exactly as the user typed it.
    assert!(
        s.cell_history().iter().all(|c| !c.contains("s.clone()")),
        "no auto-clone insertion expected without the flag; history={:?}",
        s.cell_history(),
    );
}

#[test]
fn auto_clone_export_preserves_inserted_clones() {
    // `:save` writes `cell_history` verbatim; verify the rewritten
    // consume site survives export. We don't drive the actual file write
    // (that's covered elsewhere); instead we inspect cell_history
    // directly — `:save` is a thin formatter over the same buffer.
    let mut s = Session::with_options(ReplOptions { auto_clone: true });
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"persist\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let _ = s.evaluate_cell_captured("println(s);");
    let history = s.cell_history();
    let consuming = history
        .iter()
        .find(|c| c.contains("let _t = consume"))
        .expect("expected the consume cell to remain in history");
    assert!(
        consuming.contains("consume(s.clone())"),
        "expected the consuming cell's history entry to read `consume(s.clone())`; got:\n{consuming}",
    );
    assert!(
        !consuming.contains("consume(s)"),
        "the bare `consume(s)` form must not survive after auto-clone rewrites; got:\n{consuming}",
    );
}
