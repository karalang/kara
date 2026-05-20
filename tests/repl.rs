//! Integration tests for the `karac repl` binary's `Session` evaluator.
//!
//! Tests exercise the cell pipeline directly without driving rustyline
//! through a TTY. `Session::evaluate_cell_captured` routes interpreter
//! `println` output into an in-memory buffer so we can assert against it
//! without touching the process's real stdout fd.

use karac::repl::{MagicOutput, ReplOptions, Session};

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

// ── Session export — `:save` fidelity ──────────────────────────────────────

/// Helper: parse + resolve + typecheck an exported session string. Returns
/// the joined error message (empty string on clean compile) so the test
/// can assert the fidelity guarantee from line 679 of the tracker:
/// "the exported file compiles with `karac build`".
fn compile_exported(src: &str) -> String {
    let parsed = karac::parse(src);
    if !parsed.errors.is_empty() {
        return parsed
            .errors
            .iter()
            .map(|e| format!("parse: {}", e.message))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let resolved = karac::resolve(&parsed.program);
    if !resolved.errors.is_empty() {
        return resolved
            .errors
            .iter()
            .map(|e| format!("resolve: {}", e.message))
            .collect::<Vec<_>>()
            .join("\n");
    }
    let typed = karac::typecheck(&parsed.program, &resolved);
    if !typed.errors.is_empty() {
        return typed
            .errors
            .iter()
            .map(|e| format!("type: {}", e.message))
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

#[test]
fn export_wraps_statements_in_synthetic_main() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn add(a: i64, b: i64) -> i64 { a + b }");
    s.evaluate_cell_captured("let x = add(2, 3);");
    s.evaluate_cell_captured("println(x);");

    let exported = s.render_exported_session();
    // Items section sits at file scope (before `fn main()`).
    let items_idx = exported.find("fn add").expect("items missing from export");
    let main_idx = exported
        .find("fn main()")
        .expect("synthetic main missing from export");
    assert!(
        items_idx < main_idx,
        "items must appear before `fn main()` in the export; got:\n{exported}",
    );
    // Statement cells got hoisted into the main body.
    let body_start = main_idx + "fn main()".len();
    let body = &exported[body_start..];
    assert!(
        body.contains("let x = add(2, 3);"),
        "expected the `let x` cell inside `fn main()` body; got body:\n{body}",
    );
    assert!(
        body.contains("println(x);"),
        "expected the println cell inside `fn main()` body; got body:\n{body}",
    );
}

#[test]
fn export_compiles_with_karac_build() {
    // Fidelity guarantee #1: the exported file compiles cleanly.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn add(a: i64, b: i64) -> i64 { a + b }");
    s.evaluate_cell_captured("let x = add(2, 3);");
    s.evaluate_cell_captured("println(x);");

    let exported = s.render_exported_session();
    let errs = compile_exported(&exported);
    assert!(
        errs.is_empty(),
        "exported session must compile cleanly; errors:\n{errs}\nexported:\n{exported}",
    );
}

#[test]
fn export_reproduces_observable_behavior() {
    // Fidelity guarantee #2: running the exported file produces the same
    // observable behavior as the original session. We approximate "run"
    // by re-evaluating the exported source through a fresh Session as a
    // single statement-style cell (the source IS already a `fn main()`,
    // so we feed it via items_source + a trivial trigger cell).
    let mut s = Session::new();
    s.evaluate_cell_captured("fn square(n: i64) -> i64 { n * n }");
    let r1 = s.evaluate_cell_captured("println(square(7));");
    let r2 = s.evaluate_cell_captured("let q = square(3); println(q);");
    let session_output = format!("{}{}", r1.stdout, r2.stdout);

    let exported = s.render_exported_session();

    // Run the export end-to-end via the same interpreter the REPL uses.
    let parsed = karac::parse(&exported);
    assert!(
        parsed.errors.is_empty(),
        "exported parse: {:?}",
        parsed.errors,
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "exported resolve: {:?}",
        resolved.errors,
    );
    let mut program = parsed.program;
    let typed = karac::typecheck(&program, &resolved);
    assert!(typed.errors.is_empty(), "exported type: {:?}", typed.errors);
    karac::lower(&mut program, &typed);
    let mut interp = karac::interpreter::Interpreter::new(&program, &typed);
    interp.captured_output = Some(Vec::new());
    interp.run();
    let exported_output = interp.captured_output.take().unwrap_or_default().join("");
    assert_eq!(
        exported_output, session_output,
        "exported run must reproduce the session's observable output;\nexported:\n{exported}",
    );
}

#[test]
fn export_declares_effect_set_on_main() {
    // Spec promise: declared effects on main mirror the session's
    // accumulated effect set. `env.set` is seeded as `writes(Env)` by
    // the effect checker (see `EffectCheck::check`'s stdlib block);
    // a session calling it lands at least that verb on main's
    // signature. We pick `env.set` because it's a clean, single-effect
    // stdlib call — `println` is intentionally NOT effect-propagating
    // in the current design (matches the "fn main() {}" wrapper the
    // REPL itself uses for plain print sessions).
    let mut s = Session::new();
    s.evaluate_cell_captured("env.set(\"KARA_REPL_SAVE_TEST\", \"1\");");

    let exported = s.render_exported_session();
    let main_line = exported
        .lines()
        .find(|l| l.starts_with("fn main()"))
        .expect("main missing");
    assert!(
        main_line.contains("writes(Env)"),
        "expected `writes(Env)` on the exported main; got: {main_line}\nexport:\n{exported}",
    );
    // And the exported file still compiles — over-declaration is
    // legal; the body must respect the declared upper bound.
    let errs = compile_exported(&exported);
    assert!(
        errs.is_empty(),
        "effect-declared export must still compile; errors:\n{errs}\nexport:\n{exported}",
    );
}

#[test]
fn export_omits_item_cells_from_main_body() {
    // Items get hoisted to file scope; they must NOT appear duplicated
    // inside `fn main()`. Cells that are *pure items* are filtered out
    // of the main-body assembly walk.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    s.evaluate_cell_captured("println(one());");

    let exported = s.render_exported_session();
    let main_idx = exported.find("fn main()").expect("main missing");
    let body = &exported[main_idx..];
    // `fn one()` should appear once (at file scope, before main), never
    // duplicated inside main's body.
    let fn_count = exported.matches("fn one()").count();
    assert_eq!(
        fn_count, 1,
        "expected one definition of fn one (file scope), got {fn_count}; export:\n{exported}",
    );
    assert!(
        body.contains("println(one());"),
        "expected the call cell inside main; got body:\n{body}",
    );
}

#[test]
fn export_no_statement_cells_emits_empty_main() {
    // Item-only sessions still produce a valid main wrapper — empty
    // body, no effect annotations. This is the regression pin for the
    // "no statements" edge case.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn ten() -> i64 { 10 }");

    let exported = s.render_exported_session();
    assert!(exported.contains("fn ten"), "items must export");
    assert!(
        exported.contains("fn main() {\n}\n"),
        "expected an empty `fn main()` wrapper for the item-only session; got:\n{exported}",
    );
    let errs = compile_exported(&exported);
    assert!(
        errs.is_empty(),
        "empty-main session must still compile cleanly; errors:\n{errs}",
    );
}

#[test]
fn export_preserves_auto_clone_insertions_in_main_body() {
    // Auto-clone insertions land in cell_history at insertion time
    // (via apply_auto_clone_rewrites). The exported main body picks
    // them up from cell_history unchanged — fidelity guarantee from
    // the spec's "if auto-clone was enabled, the inserted clones
    // appear in the exported file unchanged."
    let mut s = Session::with_options(ReplOptions { auto_clone: true });
    s.evaluate_cell_captured("fn consume(s: String) {}");
    s.evaluate_cell_captured("let s = \"persist\";");
    s.evaluate_cell_captured("let _t = consume(s);");
    let _ = s.evaluate_cell_captured("println(s);");

    let exported = s.render_exported_session();
    assert!(
        exported.contains("consume(s.clone())"),
        "expected the auto-clone rewrite to ride into the exported main body; export:\n{exported}",
    );
    assert!(
        !exported.contains("consume(s);"),
        "bare consume(s) must NOT appear after the auto-clone rewrite; export:\n{exported}",
    );
    // Sanity: header note announces the bake-in so a reader of the
    // exported file knows what they're looking at.
    assert!(
        exported.contains("--auto-clone"),
        "auto-clone bake-in should be noted in the export header; export:\n{exported}",
    );
}

// ── Value-snapshot persistent-let model ────────────────────────────────────

#[test]
fn let_value_snapshot_records_cached_value() {
    // Cell 1 binds `x = 5`. The session's value-snapshot store must
    // record the bound value so cell 2 can replay it without re-
    // evaluating the RHS. Inspects `let_snapshots()` directly so the
    // test exercises the snapshot bookkeeping even before any cross-
    // cell reuse fires.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("let x = 5;");
    assert!(r.errors.is_empty(), "cell 1 errors: {:?}", r.errors);
    let snap = s.let_snapshots();
    let v = snap
        .get("x")
        .expect("expected `x` to be in the snapshot store after cell 1");
    // The captured value is the integer literal 5.
    assert!(
        format!("{:?}", v).contains('5'),
        "expected snapshot value to be the bound integer; got {:?}",
        v,
    );
}

#[test]
fn let_value_snapshot_rhs_does_not_re_fire_across_cells() {
    // The headline guarantee. Cell 1 binds `x` to the result of a fn
    // call with an observable side effect (a `println` line); cell 2
    // reads `x`. The source-replay form would re-emit the let into
    // cell 2's synthetic main, RE-EVALUATING the side-effecting RHS
    // and emitting a duplicate println line. The value-snapshot path
    // pre-loads `x` from the snapshot store and skips the RHS — so
    // only ONE "compute" line should fire across the two cells.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("fn compute() -> i64 { println(\"compute fired\"); 42 }");
    let r1 = s.evaluate_cell_captured("let x = compute();");
    let r2 = s.evaluate_cell_captured("println(x);");
    assert!(r1.errors.is_empty(), "cell 2 errors: {:?}", r1.errors);
    assert!(r2.errors.is_empty(), "cell 3 errors: {:?}", r2.errors);
    // Cell 2 ran compute() once.
    assert_eq!(
        r1.stdout.matches("compute fired").count(),
        1,
        "cell 2 should fire `compute()` exactly once; stdout:\n{}",
        r1.stdout,
    );
    // Cell 3 must NOT re-fire compute(). The value-snapshot path
    // skipped the RHS and used the cached value. We see the printed
    // `x` (`42`) but NOT a second "compute fired".
    assert!(
        !r2.stdout.contains("compute fired"),
        "cell 3 must reuse the snapshot — `compute()` must not re-run; stdout:\n{}",
        r2.stdout,
    );
    assert!(
        r2.stdout.contains("42"),
        "cell 3 should still print the cached value; stdout:\n{}",
        r2.stdout,
    );
}

#[test]
fn let_value_snapshot_chains_across_three_cells() {
    // Pinning that the snapshot model survives more than one hop.
    // Cell 1 binds x; cell 2 binds y from x; cell 3 reads both. The
    // side-effecting RHS on x must fire exactly once across all three
    // cells.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("fn once() -> i64 { println(\"once\"); 7 }");
    let _ = s.evaluate_cell_captured("let x = once();");
    let r2 = s.evaluate_cell_captured("let y = x + 1;");
    let r3 = s.evaluate_cell_captured("println(x); println(y);");
    assert!(r2.errors.is_empty(), "cell 3 errors: {:?}", r2.errors);
    assert!(r3.errors.is_empty(), "cell 4 errors: {:?}", r3.errors);
    assert!(
        !r2.stdout.contains("once"),
        "cell 3 must not re-run `once()`; stdout:\n{}",
        r2.stdout,
    );
    assert!(
        !r3.stdout.contains("once"),
        "cell 4 must not re-run `once()`; stdout:\n{}",
        r3.stdout,
    );
    let trimmed = r3.stdout.trim();
    assert_eq!(
        trimmed, "7\n8",
        "cell 4 should print the chained snapshot values; got: {trimmed:?}",
    );
}

#[test]
fn let_value_snapshot_rebinding_drops_stale_entry() {
    // When a later cell re-binds `x` to a different type, the stale
    // snapshot must be dropped so the new RHS evaluates normally and
    // seeds a fresh snapshot. Without this guard, the override would
    // splice in the prior `i64` value where the typechecker expects a
    // `String` — type-confusion at runtime.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("let x = 5;");
    assert_eq!(
        s.let_snapshots()
            .get("x")
            .map(|v| format!("{v:?}").contains('5')),
        Some(true)
    );
    let _ = s.evaluate_cell_captured("let x = \"hello\";");
    let snap = s.let_snapshots();
    let v = snap
        .get("x")
        .expect("expected the new `x` in the snapshot store");
    assert!(
        format!("{v:?}").contains("hello"),
        "expected the snapshot to reflect the latest binding; got {v:?}",
    );
    // And reading the new x in a third cell uses the new value.
    let r = s.evaluate_cell_captured("println(x);");
    assert_eq!(r.stdout.trim(), "hello");
}

#[test]
fn let_value_snapshot_reset_clears_cache() {
    // `:reset` clears the persistent-let source-replay buffer; it
    // must also clear the snapshot cache so a subsequent re-bind
    // does not accidentally pick up a stale value. The source-replay
    // buffer being empty after reset means no override would fire on
    // the next cell anyway, but the explicit clear keeps the two
    // stores in lockstep.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("let x = 99;");
    assert!(s.let_snapshots().contains_key("x"));
    let _ = s.dispatch_meta(":reset");
    assert!(
        s.let_snapshots().is_empty(),
        "expected :reset to clear the value-snapshot cache; got {:?}",
        s.let_snapshots(),
    );
}

#[test]
fn let_value_snapshot_unused_binding_still_visible() {
    // A let whose binding is never referenced in subsequent cells
    // must still snapshot — future cells might pull it in. Edge
    // case: the watch set must include *every* persistent_lets
    // binding name, not just ones the current cell touches.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("let unused = 123;");
    assert!(
        s.let_snapshots().contains_key("unused"),
        "snapshot should capture every persistent-let binding regardless of later use",
    );
    // Confirm that reading it in a later cell works — the override
    // wires the binding through without re-running the literal.
    let r = s.evaluate_cell_captured("println(unused);");
    assert_eq!(r.stdout.trim(), "123");
}

// ── Jupyter %magic surface ────────────────────────────────────────────────

#[test]
fn magic_unknown_returns_structured_error() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%not-a-real-magic");
    assert!(!out.ok, "unknown magic must surface as an error reply");
    assert!(
        out.text.contains("unknown magic") && out.text.contains("not-a-real-magic"),
        "expected the error text to name the bad magic; got: {}",
        out.text,
    );
    // Listing of supported magics is part of the error UX.
    assert!(
        out.text.contains("%effects")
            && out.text.contains("%ownership")
            && out.text.contains("%explain")
            && out.text.contains("%set"),
        "expected the error text to list supported magics; got: {}",
        out.text,
    );
}

#[test]
fn magic_effects_renders_inferred_effect_set() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn touch_env() writes(Env) { env.set(\"X\", \"1\"); }");
    let out = s.dispatch_magic("%effects");
    assert!(out.ok, "expected %effects to succeed; got: {}", out.text);
    assert!(
        out.text.contains("touch_env") && out.text.contains("writes(Env)"),
        "expected %effects output to surface the inferred effect set; got: {}",
        out.text,
    );
}

#[test]
fn magic_effects_empty_when_session_pure() {
    // No items_source yet — the magic explains the state instead of
    // emitting a blank cell.
    let mut s = Session::new();
    let out = s.dispatch_magic("%effects");
    assert!(out.ok);
    assert!(
        out.text.contains("no items defined yet"),
        "expected the empty-session hint; got: {}",
        out.text,
    );
}

#[test]
fn magic_ownership_lists_binding_modes() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let n = 7;");
    let out = s.dispatch_magic("%ownership");
    assert!(out.ok, "expected %ownership to succeed; got: {}", out.text);
    // The ownership pass classifies a bare `let n = 7;` as an owned
    // stack binding; the exact representation string matches what
    // `OwnershipCheckResult.representations` records.
    assert!(
        out.text.contains("n:"),
        "expected `n:` row in the %ownership table; got: {}",
        out.text,
    );
}

#[test]
fn magic_ownership_empty_when_no_bindings() {
    let mut s = Session::new();
    s.evaluate_cell_captured("fn foo() {}");
    let out = s.dispatch_magic("%ownership");
    assert!(out.ok);
    assert!(
        out.text.contains("no bindings"),
        "expected an empty-bindings hint when no lets exist; got: {}",
        out.text,
    );
}

#[test]
fn magic_explain_concept_lookup() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%explain closures");
    assert!(
        out.ok,
        "expected %explain closures to succeed; got: {}",
        out.text
    );
    assert!(
        out.text.to_lowercase().contains("closure"),
        "expected the closures concept body; got: {}",
        out.text,
    );
}

#[test]
fn magic_explain_class_lookup() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%explain TYPE_MISMATCH");
    assert!(
        out.ok,
        "expected %explain class lookup to succeed; got: {}",
        out.text
    );
    assert!(
        out.text.contains("TYPE_MISMATCH"),
        "expected the class name in the output; got: {}",
        out.text,
    );
}

#[test]
fn magic_explain_unknown_target() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%explain not-a-real-thing");
    assert!(!out.ok, "unknown explain target must surface as error");
    // The lookup tries concept then class; the final error should
    // name the class-list since that's the last failure.
    assert!(
        out.text.contains("unknown") && out.text.contains("not-a-real-thing"),
        "expected error text naming the bad lookup; got: {}",
        out.text,
    );
}

#[test]
fn magic_explain_no_argument() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%explain");
    assert!(!out.ok);
    assert!(
        out.text.contains("usage:"),
        "expected usage text on empty %explain; got: {}",
        out.text,
    );
}

#[test]
fn magic_set_auto_clone_toggles_flag() {
    let mut s = Session::new();
    assert!(!s.auto_clone(), "default off");

    let on = s.dispatch_magic("%set auto-clone on");
    assert!(on.ok, "expected `on` toggle to succeed; got: {}", on.text);
    assert!(s.auto_clone(), "flag should be on after toggle");
    assert!(on.text.contains("auto-clone: on"));

    let off = s.dispatch_magic("%set auto-clone off");
    assert!(off.ok);
    assert!(!s.auto_clone(), "flag should be off after toggle");
    assert!(off.text.contains("auto-clone: off"));
}

#[test]
fn magic_set_auto_clone_rejects_bad_value() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%set auto-clone maybe");
    assert!(!out.ok, "bad value must surface as error");
    assert!(
        out.text.contains("maybe") && out.text.contains("on") && out.text.contains("off"),
        "expected the error to name the bad value + accepted options; got: {}",
        out.text,
    );
    assert!(!s.auto_clone(), "flag must NOT change on rejected value");
}

#[test]
fn magic_set_unknown_setting() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%set frobnicate true");
    assert!(!out.ok);
    assert!(
        out.text.contains("frobnicate") && out.text.contains("auto-clone"),
        "expected the error to name the bad setting + the supported set; got: {}",
        out.text,
    );
}

#[test]
fn magic_provide_returns_deferred_error() {
    // %provide / %end-provide share their compilation path with the
    // :provide / :end-provide REPL meta-commands tracked at line 681.
    // Line 681 has not shipped, so the magic dispatcher surfaces a
    // structured deferral pointer rather than a "no such magic"
    // error — the kernel can render this in the cell output without
    // hiding the spec'd surface.
    let mut s = Session::new();
    let out = s.dispatch_magic("%provide MyResource = SomeProvider {}");
    assert!(!out.ok, "deferred magic must surface as error");
    assert!(
        out.text.contains("not yet wired") && out.text.contains("line 681"),
        "expected the error to mention the tracker pointer; got: {}",
        out.text,
    );

    let out = s.dispatch_magic("%end-provide MyResource");
    assert!(!out.ok);
    assert!(
        out.text.contains("not yet wired"),
        "%end-provide must also surface as deferred; got: {}",
        out.text,
    );
}

#[test]
fn magic_rc_is_post_mvp() {
    // Spec explicitly defers %rc to post-MVP. Surfaces as error with
    // the deferral reason so the kernel reply can carry the
    // explanation.
    let mut s = Session::new();
    let out = s.dispatch_magic("%rc");
    assert!(!out.ok);
    assert!(
        out.text.contains("post-MVP") || out.text.contains("RC fallback"),
        "expected %rc to surface a deferral reason; got: {}",
        out.text,
    );
}

#[test]
fn magic_output_construction_helpers() {
    // Sanity for the consumer-facing MagicOutput shape — the kernel
    // will rely on these for adapting cell outputs.
    let ok_msg = MagicOutput::ok("hello");
    assert!(ok_msg.ok);
    assert_eq!(ok_msg.text, "hello");

    let err = MagicOutput::error("bad");
    assert!(!err.ok);
    assert_eq!(err.text, "bad");
}

// ── Per-cell effect footer ────────────────────────────────────────────────

#[test]
fn cell_effect_footer_populates_after_run() {
    // A cell that triggers a tracked effect (env.set carries
    // `writes(Env)`) must surface the footer string the kernel will
    // render below the cell's stdout.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("env.set(\"X\", \"y\");");
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    assert!(
        r.effect_footer.contains("writes(Env)"),
        "expected `writes(Env)` in the footer; got: {:?}",
        r.effect_footer,
    );
}

#[test]
fn cell_effect_footer_empty_for_pure_cell() {
    // Pure cells (no tracked effect calls) emit an empty footer so
    // the kernel can suppress the annotation.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("let _x = 1 + 2;");
    assert!(r.errors.is_empty());
    assert!(
        r.effect_footer.is_empty(),
        "pure cells must emit an empty footer; got: {:?}",
        r.effect_footer,
    );
}

#[test]
fn cell_effect_footer_empty_for_pure_items_cell() {
    // Item-only cells use the early-return path of evaluate_cell_captured
    // (no synthetic main is built). The footer field is empty since
    // there's no statement-side run to summarize.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    assert!(r.errors.is_empty());
    assert!(
        r.effect_footer.is_empty(),
        "pure items cell must emit an empty footer; got: {:?}",
        r.effect_footer,
    );
}

// ── Cross-cell providers — slice 2: parser + provider_stack + LIFO close ──

#[test]
fn provide_meta_rejects_missing_equals() {
    // `:provide DB` (no `= expr`) must surface a usage hint and leave
    // the stack untouched. The dispatcher prints to stderr; we assert
    // via state inspection.
    let mut s = Session::new();
    s.dispatch_meta(":provide DB");
    assert!(s.provider_stack().is_empty());
}

#[test]
fn provide_meta_rejects_invalid_resource_ident() {
    // The resource ident must be a Kāra identifier — leading digit
    // rejected, no frame pushed.
    let mut s = Session::new();
    s.dispatch_meta(":provide 123 = 42");
    assert!(s.provider_stack().is_empty());
}

#[test]
fn provide_meta_rejects_empty_expression() {
    // `:provide DB =` (trailing `=` with empty RHS) must surface a
    // usage hint and leave the stack untouched.
    let mut s = Session::new();
    s.dispatch_meta(":provide DB =");
    assert!(s.provider_stack().is_empty());
}

#[test]
fn provide_meta_pushes_frame_after_valid_construction() {
    // Slice 2 validates construction by running the expression through
    // the standard pipeline. A plain integer literal types cleanly and
    // runs to completion, so the frame pushes. The resource-level
    // typecheck against the wrapping `with_provider` form lands in
    // slice 3; slice 2 just establishes the dispatching surface.
    let mut s = Session::new();
    s.dispatch_meta(":provide MyR = 42");
    let stack = s.provider_stack();
    assert_eq!(stack.len(), 1, "expected one frame; got {stack:?}");
    assert_eq!(stack[0].resource, "MyR");
    assert_eq!(stack[0].expr_src, "42");
    assert_eq!(stack[0].opened_cell, 1);
}

#[test]
fn provide_meta_does_not_open_scope_on_type_error() {
    // Construction expression that fails to resolve / typecheck
    // surfaces the error and leaves the frame un-pushed. Matches the
    // design.md guarantee: "if construction panics, the scope is not
    // opened". Type-time failures are a strict superset of runtime
    // panics (caught earlier in the pipeline).
    let mut s = Session::new();
    s.dispatch_meta(":provide DB = no_such_function_anywhere()");
    assert!(
        s.provider_stack().is_empty(),
        "type-error construction must not open the scope"
    );
}

#[test]
fn end_provide_pops_innermost_frame() {
    let mut s = Session::new();
    s.dispatch_meta(":provide A = 1");
    assert_eq!(s.provider_stack().len(), 1);
    s.dispatch_meta(":end-provide A");
    assert!(s.provider_stack().is_empty());
}

#[test]
fn end_provide_with_no_active_scope_errors() {
    let mut s = Session::new();
    s.dispatch_meta(":end-provide DB");
    assert!(s.provider_stack().is_empty());
}

#[test]
fn end_provide_wrong_name_errors_with_single_frame() {
    // `:end-provide B` while only `:provide A` is active — the top of
    // stack is `A`, not `B`, so the dispatcher surfaces a mismatch
    // error and leaves the stack untouched. Exercises the same LIFO
    // close-check branch nested provides hit, without needing real
    // resource definitions (nested-`:provide` validation eagerly wraps
    // in the active outer providers and that wrap requires a real
    // `effect resource` declaration to typecheck — slice 5 adds the
    // full end-to-end nested test with proper resource stubs).
    let mut s = Session::new();
    s.dispatch_meta(":provide A = 1");
    assert_eq!(s.provider_stack().len(), 1);
    s.dispatch_meta(":end-provide B");
    let stack = s.provider_stack();
    assert_eq!(stack.len(), 1, "mismatch must not pop; got {stack:?}");
    assert_eq!(stack[0].resource, "A");
}

#[test]
fn end_provide_validates_resource_ident() {
    let mut s = Session::new();
    s.dispatch_meta(":provide A = 1");
    s.dispatch_meta(":end-provide 123");
    // Stack untouched on invalid ident.
    assert_eq!(s.provider_stack().len(), 1);
    assert_eq!(s.provider_stack()[0].resource, "A");
}

// ── Cross-cell providers — slice 3: run-time wrap + export wrap ──

#[test]
fn export_falls_back_to_flat_form_when_provider_history_empty() {
    // Sessions that never touch :provide / :end-provide must continue
    // to emit the flat `render_main_body` shape unchanged — the
    // timeline-aware path only kicks in when provider_history is
    // non-empty, so existing 679-style export tests stay valid.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("println(\"hi\");");
    let exported = s.render_exported_session();
    assert!(
        !exported.contains("with_provider"),
        "no providers used, export must not contain with_provider; got:\n{exported}"
    );
    assert!(exported.contains("println(\"hi\");"));
}

#[test]
fn export_omits_empty_provider_scopes() {
    // `:provide A` immediately followed by `:end-provide A` with no
    // cells between is an empty scope — the export should drop it
    // entirely so the saved file stays minimal.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("println(\"before\");");
    s.dispatch_meta(":provide A = 1");
    s.dispatch_meta(":end-provide A");
    let _ = s.evaluate_cell_captured("println(\"after\");");
    let exported = s.render_exported_session();
    assert!(
        !exported.contains("with_provider"),
        "empty scope must collapse; got:\n{exported}"
    );
    assert!(exported.contains("println(\"before\");"));
    assert!(exported.contains("println(\"after\");"));
}

#[test]
fn export_wraps_cells_in_active_scope() {
    // Cells that ran between `:provide Clock` and `:end-provide Clock`
    // land inside `with_provider[Clock](expr, || { … })` in the
    // exported source. FakeClock matches the `Clock` resource's
    // Provider shape (see tests/snapshots/cost_summary_provider.kara
    // for the same construction in file form). Cells outside the
    // scope land at the top level of the body.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    let _ = s.evaluate_cell_captured("println(\"before scope\");");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let r = s.evaluate_cell_captured("println(Clock.now());");
    assert!(
        r.errors.is_empty(),
        "cell inside provider scope must run cleanly; errors: {:?}",
        r.errors
    );
    s.dispatch_meta(":end-provide Clock");
    let _ = s.evaluate_cell_captured("println(\"after scope\");");
    let exported = s.render_exported_session();
    assert!(
        exported.contains("with_provider[Clock](FakeClock {}, || {"),
        "missing scope wrap; got:\n{exported}"
    );
    assert!(
        exported.contains("println(Clock.now());"),
        "scope body missing; got:\n{exported}"
    );
    // Both outside-scope cells appear at top level, not wrapped.
    assert!(exported.contains("println(\"before scope\");"));
    assert!(exported.contains("println(\"after scope\");"));
}

#[test]
fn run_time_wraps_cell_body_when_provider_active() {
    // Confirms the run-time path: a cell whose body resolves a
    // resource call WOULD fail without an active provider, but with
    // `:provide Clock = FakeClock {}` open the wrap supplies it and
    // the cell runs. The stdout assertion proves the wrap actually
    // executed (not just that the typechecker accepted it).
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 42 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let r = s.evaluate_cell_captured("println(Clock.now());");
    assert!(r.errors.is_empty(), "cell errors: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "42");
}

#[test]
fn export_handles_nested_provider_scopes() {
    // Nested `:provide A; :provide B; cell; :end-provide B; :end-provide A`
    // renders as nested with_provider blocks (outer A wraps inner B
    // wraps the cell). Uses two distinct resources — RandomSource is
    // the other program-rooted resource with a default Provider
    // shape that an empty user struct can satisfy.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    let _ = s.evaluate_cell_captured("struct FakeRng {}");
    let _ = s.evaluate_cell_captured("impl FakeRng { fn next(ref self) -> i64 { 7 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    s.dispatch_meta(":provide RandomSource = FakeRng {}");
    let r = s.evaluate_cell_captured("println(Clock.now() + RandomSource.next());");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    s.dispatch_meta(":end-provide RandomSource");
    s.dispatch_meta(":end-provide Clock");
    let exported = s.render_exported_session();
    let outer = exported.find("with_provider[Clock]");
    let inner = exported.find("with_provider[RandomSource]");
    assert!(outer.is_some(), "missing outer wrap; got:\n{exported}");
    assert!(inner.is_some(), "missing inner wrap; got:\n{exported}");
    assert!(
        outer.unwrap() < inner.unwrap(),
        "outer wrap must precede inner; got:\n{exported}"
    );
}
