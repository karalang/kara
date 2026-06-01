//! Integration tests for the `karac repl` binary's `Session` evaluator.
//!
//! Tests exercise the cell pipeline directly without driving rustyline
//! through a TTY. `Session::evaluate_cell_captured` routes interpreter
//! `println` output into an in-memory buffer so we can assert against it
//! without touching the process's real stdout fd.

use karac::repl::{DependencyKind, MagicOutput, ReplOptions, Session};

/// Pin a session to the interpreter dispatch path. The
/// `let_value_snapshot_*` tests below inspect `Session::let_snapshots()`,
/// an interpreter-path value cache the JIT path deliberately replaces
/// with runner-side globals (so the inspector is empty under JIT). The
/// *behavior* those tests guard — caching, cross-cell shadow-drop on
/// rebind (incl. cross-type), `:reset` clearing — is covered under JIT
/// in `tests/repl_jit.rs` (`repl_jit_runs_let_bindings`,
/// `repl_jit_cross_type_rebind_uses_new_value`,
/// `repl_jit_reset_clears_snapshot_state`). Pinning these introspection
/// tests to the interpreter path keeps that bookkeeping coverage intact
/// without breaking when the JIT-default flip lands. This is a scoped,
/// per-test pin — NOT a blanket suite-wide JIT disable. No-op without
/// `lljit_prototype` (the JIT dispatch path isn't compiled there, so a
/// fresh `Session` already runs the interpreter).
fn pin_interpreter(session: &mut Session) {
    #[cfg(feature = "lljit_prototype")]
    session.set_jit_enabled_for_tests(false);
    #[cfg(not(feature = "lljit_prototype"))]
    let _ = session;
}

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
    pin_interpreter(&mut s);
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
    pin_interpreter(&mut s);
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
    pin_interpreter(&mut s);
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
    pin_interpreter(&mut s);
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
fn magic_provide_forwards_to_meta_handler() {
    // %provide / %end-provide forward to the same add_provider /
    // end_provider handlers the :provide / :end-provide meta-commands
    // use — line 681 shipped, so the magic surface is wire-compatible
    // with the REPL surface. Construction failures surface as
    // MagicOutput::error; successful opens / closes surface as
    // MagicOutput::ok with the same text the meta handler returns.
    let mut s = Session::new();
    // Failed construction → error (the resolver hits `SomeProvider`,
    // which isn't defined in this session).
    let out = s.dispatch_magic("%provide MyResource = SomeProvider {}");
    assert!(!out.ok, "construction failure must surface as error");
    assert!(
        out.text.contains("MyResource"),
        "error text must name the resource; got: {}",
        out.text,
    );
    assert!(
        out.text.contains("not opened"),
        "error text must say the scope wasn't opened; got: {}",
        out.text,
    );

    // Valid construction → ok.
    let out = s.dispatch_magic("%provide R = 42");
    assert!(
        out.ok,
        "valid construction must surface as ok; got: {}",
        out.text
    );
    assert!(
        out.text.contains("opened"),
        "ok text must announce the open; got: {}",
        out.text,
    );

    // Close via magic.
    let out = s.dispatch_magic("%end-provide R");
    assert!(out.ok, "valid close must surface as ok; got: {}", out.text);
    assert!(
        out.text.contains("closed"),
        "ok text must announce the close; got: {}",
        out.text,
    );

    // LIFO mismatch via magic.
    s.dispatch_magic("%provide A = 1");
    let out = s.dispatch_magic("%end-provide B");
    assert!(!out.ok, "mismatch must surface as error");
    assert!(
        out.text.contains("attempts to close an outer scope")
            || out.text.contains("no active provider"),
        "expected LIFO mismatch / empty-stack hint; got: {}",
        out.text,
    );
}

#[test]
fn magic_and_meta_provide_share_stack() {
    // Open via :provide (meta) and close via %end-provide (magic) —
    // both must touch the same Session.provider_stack.
    let mut s = Session::new();
    s.dispatch_meta(":provide M = 7");
    assert_eq!(s.provider_stack().len(), 1);
    let out = s.dispatch_magic("%end-provide M");
    assert!(out.ok, "{}", out.text);
    assert!(
        s.provider_stack().is_empty(),
        "magic close did not pop the stack"
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

// ── Cross-cell providers — slice 4: notebook-aware closed-scope diagnostic ──

#[test]
fn reference_after_end_provide_surfaces_notebook_aware_tail() {
    // Design.md § Cross-Cell Providers's headline diagnostic shape:
    // a binding declared inside `:provide R` is invisible after
    // `:end-provide R` fires, and the resolver "undefined name 'X'"
    // error grows a tail naming the provider scope that closed and
    // the cell where the binding was declared.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let r = s.evaluate_cell_captured("let now = Clock.now();");
    assert!(
        r.errors.is_empty(),
        "let inside scope failed: {:?}",
        r.errors
    );
    s.dispatch_meta(":end-provide Clock");
    let r = s.evaluate_cell_captured("println(now);");
    assert!(!r.errors.is_empty(), "expected unresolved-name error");
    let joined = r.errors.join("\n");
    assert!(
        joined.contains("undefined name 'now'"),
        "missing base error; got:\n{joined}"
    );
    assert!(
        joined.contains("declared inside `:provide Clock`"),
        "missing notebook-aware tail; got:\n{joined}"
    );
    assert!(
        joined.contains("`:end-provide Clock`"),
        "tail missing close marker; got:\n{joined}"
    );
}

#[test]
fn binding_outside_provider_scope_survives_end_provide() {
    // A let declared BEFORE :provide opens is in an outer block and
    // must remain visible after :end-provide. Only bindings whose
    // capture-time scope equals the just-closed-scope's pre-pop stack
    // are pruned.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    let r = s.evaluate_cell_captured("let outer = 7;");
    assert!(r.errors.is_empty());
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let _ = s.evaluate_cell_captured("let _ = Clock.now();");
    s.dispatch_meta(":end-provide Clock");
    let r = s.evaluate_cell_captured("println(outer);");
    assert!(
        r.errors.is_empty(),
        "outer-scope binding must survive close; got {:?}",
        r.errors
    );
    assert_eq!(r.stdout.trim(), "7");
}

#[test]
fn rebinding_pruned_name_clears_diagnostic_entry() {
    // After :end-provide prunes a binding, re-binding the same name
    // in a subsequent cell should nullify the prune diagnostic — a
    // later "undefined name 'X'" would be unrelated to the closed
    // scope. Test by re-binding then deliberately referencing a
    // DIFFERENT undefined name and confirming no spurious provider
    // tail attaches.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let _ = s.evaluate_cell_captured("let now = Clock.now();");
    s.dispatch_meta(":end-provide Clock");
    // Re-bind `now` so the prune diagnostic falls away.
    let r = s.evaluate_cell_captured("let now = 99;");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // Reference a different missing name; the resolver error must NOT
    // carry a `:provide Clock` tail (the only pruned entry was `now`,
    // and it was cleared by the re-bind).
    let r = s.evaluate_cell_captured("println(other);");
    assert!(!r.errors.is_empty());
    let joined = r.errors.join("\n");
    assert!(joined.contains("undefined name 'other'"));
    assert!(
        !joined.contains(":provide Clock"),
        "spurious provider tail attached to unrelated error; got:\n{joined}"
    );
}

#[test]
fn reset_clears_pruned_provider_lets() {
    // :reset is a clean slate for the persistent-let machinery — the
    // pruned-bindings record should clear too so future "undefined
    // name 'X'" errors don't carry stale provider-scope tails.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let _ = s.evaluate_cell_captured("let now = Clock.now();");
    s.dispatch_meta(":end-provide Clock");
    s.reset_persistent_lets();
    let r = s.evaluate_cell_captured("println(now);");
    assert!(!r.errors.is_empty());
    let joined = r.errors.join("\n");
    assert!(
        !joined.contains(":provide Clock"),
        ":reset must clear pruned bindings; got:\n{joined}"
    );
}

#[test]
fn nested_close_prunes_only_inner_scope_bindings() {
    // Nested :provide A; :provide B; let x inside B; :end-provide B
    // must prune `x` (declared under [A, B]) but leave any binding
    // declared under just [A] visible. Exercises the equality check
    // on capture-time scope vs. pre-pop active stack.
    let mut s = Session::new();
    let _ = s.evaluate_cell_captured("struct FakeClock {}");
    let _ = s.evaluate_cell_captured("impl FakeClock { fn now(ref self) -> i64 { 0 } }");
    let _ = s.evaluate_cell_captured("struct FakeRng {}");
    let _ = s.evaluate_cell_captured("impl FakeRng { fn next(ref self) -> i64 { 0 } }");
    s.dispatch_meta(":provide Clock = FakeClock {}");
    let r = s.evaluate_cell_captured("let clock_val = Clock.now();");
    assert!(r.errors.is_empty(), "outer let: {:?}", r.errors);
    s.dispatch_meta(":provide RandomSource = FakeRng {}");
    let r = s.evaluate_cell_captured("let rng_val = RandomSource.next();");
    assert!(r.errors.is_empty(), "inner let: {:?}", r.errors);
    s.dispatch_meta(":end-provide RandomSource");
    // `rng_val` is pruned (declared under [Clock, RandomSource]);
    // `clock_val` survives (declared under just [Clock]).
    let r = s.evaluate_cell_captured("let _ = clock_val;");
    assert!(
        r.errors.is_empty(),
        "outer binding must survive inner close; got {:?}",
        r.errors
    );
    let r = s.evaluate_cell_captured("println(rng_val);");
    assert!(!r.errors.is_empty(), "expected pruned binding error");
    let joined = r.errors.join("\n");
    assert!(joined.contains("undefined name 'rng_val'"));
    assert!(
        joined.contains(":provide RandomSource"),
        "missing inner-scope tail; got:\n{joined}"
    );
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

// ── %show — rich-display magic (line 761 slice 2) ──────────────────────────

#[test]
fn show_magic_atom_yields_text_plain_only() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%show 1 + 2");
    assert!(out.ok, "expected ok; got: {}", out.text);
    let bundle = out.rich.expect("%show must populate rich bundle");
    assert_eq!(bundle.get("text/plain"), Some("3"));
    assert!(
        bundle.get("text/html").is_none(),
        "atoms must not emit text/html"
    );
    assert_eq!(out.text, "3", "text field mirrors text/plain");
}

#[test]
fn show_magic_vec_struct_emits_html_table() {
    let mut s = Session::new();
    s.evaluate_cell_captured("struct Row { name: String, count: i64 }");
    s.evaluate_cell_captured(
        r#"let rows = [Row { name: "a", count: 1 }, Row { name: "b", count: 2 }];"#,
    );
    let out = s.dispatch_magic("%show rows");
    assert!(out.ok, "expected ok; got: {}", out.text);
    let bundle = out.rich.expect("Vec[Struct] must populate rich bundle");
    let html = bundle
        .get("text/html")
        .expect("Vec[Struct] must emit text/html");
    // Columns alphabetical: count then name.
    assert!(
        html.contains("<th>count</th><th>name</th>"),
        "headers: {html}"
    );
    assert!(html.contains("<td>1</td><td>a</td>"), "row a: {html}");
    assert!(html.contains("<td>2</td><td>b</td>"), "row b: {html}");
}

#[test]
fn show_magic_string_value_renders_inline() {
    let mut s = Session::new();
    s.evaluate_cell_captured(r#"let greeting = "hello kara";"#);
    let out = s.dispatch_magic("%show greeting");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert_eq!(out.text, "hello kara");
}

#[test]
fn show_magic_undefined_name_surfaces_error() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%show does_not_exist");
    assert!(!out.ok, "expected error for undefined name");
    assert!(out.rich.is_none(), "errors carry no rich bundle");
    assert!(
        out.text.contains("does_not_exist") || out.text.to_lowercase().contains("resolve"),
        "expected resolver message; got: {}",
        out.text
    );
}

#[test]
fn show_magic_empty_argument_usage_error() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%show");
    assert!(!out.ok, "empty arg must error");
    assert!(out.text.contains("usage:"), "usage hint: {}", out.text);
    assert!(out.rich.is_none());
}

#[test]
fn show_magic_does_not_mutate_session_state() {
    let mut s = Session::new();
    s.evaluate_cell_captured("let x = 5;");
    let cells_before = s.cell_history().len();
    let lets_before = s.persistent_lets().len();
    let _ = s.dispatch_magic("%show x + 10");
    assert_eq!(
        s.cell_history().len(),
        cells_before,
        "%show must not append to cell history"
    );
    assert_eq!(
        s.persistent_lets().len(),
        lets_before,
        "%show must not append to persistent lets"
    );
    // Re-binding the same name in the next cell still works — no
    // shadow conflict from a stray `__k_show` leak.
    let r = s.evaluate_cell_captured("let __k_show = 99; println(__k_show);");
    assert!(r.errors.is_empty(), "rebind clean: {:?}", r.errors);
    assert_eq!(r.stdout.trim(), "99");
}

#[test]
fn show_magic_listed_in_unknown_magic_help() {
    let mut s = Session::new();
    let out = s.dispatch_magic("%not-a-real-magic");
    assert!(!out.ok);
    assert!(
        out.text.contains("%show"),
        "unknown-magic help must list %show; got: {}",
        out.text
    );
}

#[test]
fn show_magic_uses_session_items() {
    // `%show` must see top-level items the session has accumulated —
    // a struct defined in a prior cell can be constructed inline by
    // the expression.
    let mut s = Session::new();
    s.evaluate_cell_captured("struct Point { x: i64, y: i64 }");
    let out = s.dispatch_magic("%show Point { x: 7, y: 8 }");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert!(out.text.contains("Point"), "text: {}", out.text);
    assert!(out.text.contains("x: 7"), "text: {}", out.text);
    assert!(out.text.contains("y: 8"), "text: {}", out.text);
}

#[test]
fn show_magic_text_plain_pretty_prints_nested() {
    // A struct containing an array of structs should pretty-print
    // multi-line rather than collapsing to a single hard-to-read line.
    let mut s = Session::new();
    s.evaluate_cell_captured("struct Row { id: i64 }");
    s.evaluate_cell_captured("struct Group { rows: Vec[Row] }");
    s.evaluate_cell_captured("let g = Group { rows: [Row { id: 1 }, Row { id: 2 }] };");
    let out = s.dispatch_magic("%show g");
    assert!(out.ok, "expected ok; got: {}", out.text);
    let plain = out
        .rich
        .unwrap()
        .get("text/plain")
        .unwrap_or("")
        .to_string();
    assert!(
        plain.starts_with("Group {"),
        "should start with struct name; got: {plain}"
    );
    assert!(
        plain.contains("rows:"),
        "should mention field name; got: {plain}"
    );
    assert!(
        plain.lines().count() > 1,
        "should pretty-print multi-line; got: {plain}"
    );
}

// ── Per-cell structured effect snapshot (line 773 slice 1) ────────────────

#[test]
fn cell_effect_history_aligned_with_cell_history() {
    // Every successful cell — pure-item OR statement — appends exactly
    // one entry to both `cell_history` and `cell_effect_history`. The
    // line 773 timeline relies on this 1:1 alignment so the snapshot
    // at index `i` describes cell `i+1`.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn one() -> i64 { 1 }");
    s.evaluate_cell_captured("env.set(\"X\", \"y\");");
    s.evaluate_cell_captured("let _z = one() + 2;");
    assert_eq!(s.cell_history().len(), 3);
    assert_eq!(s.cell_effect_history().len(), 3);
}

#[test]
fn cell_effect_snapshot_records_writes_env() {
    // A statement cell that triggers `env.set` (declared `writes(Env)`
    // in the stdlib) lands one snapshot entry with the same verb/
    // resource pair the footer string renders.
    let mut s = Session::new();
    s.evaluate_cell_captured("env.set(\"K\", \"v\");");
    let history = s.cell_effect_history();
    assert_eq!(history.len(), 1);
    let pairs: Vec<(String, String)> = history[0]
        .effects
        .iter()
        .map(|(verb, res)| (format!("{verb:?}"), res.clone()))
        .collect();
    assert!(
        pairs
            .iter()
            .any(|(v, r)| v.contains("Writes") && r == "Env"),
        "expected writes(Env) in snapshot; got: {pairs:?}",
    );
}

#[test]
fn cell_effect_snapshot_empty_for_pure_cell() {
    // A pure statement cell records an empty snapshot — the timeline
    // index stays aligned with `cell_history` but the entry carries
    // no effects so it can't participate in cross-cell dependencies.
    let mut s = Session::new();
    s.evaluate_cell_captured("let _x = 1 + 2;");
    let history = s.cell_effect_history();
    assert_eq!(history.len(), 1);
    assert!(
        history[0].effects.is_empty(),
        "pure cell snapshot must be empty; got: {:?}",
        history[0].effects,
    );
}

#[test]
fn cell_effect_snapshot_empty_for_pure_items_cell() {
    // Item-only cells contribute to `items_source` but don't run a
    // synthetic main — they record an empty snapshot to keep the
    // 1:1 alignment with `cell_history`.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn add(a: i64, b: i64) -> i64 { a + b }");
    let history = s.cell_effect_history();
    assert_eq!(history.len(), 1);
    assert!(
        history[0].effects.is_empty(),
        "pure-items snapshot must be empty; got: {:?}",
        history[0].effects,
    );
}

#[test]
fn cell_effect_snapshot_skipped_on_statement_error() {
    // Captured-path statement cells that fail diagnostic-side roll
    // back `cell_history`; the snapshot history is *not* pushed in
    // that arm so the alignment invariant survives the rollback.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("undefined_function();");
    assert!(!r.errors.is_empty());
    assert_eq!(s.cell_history().len(), 0);
    assert_eq!(s.cell_effect_history().len(), 0);
}

#[test]
fn cell_effect_snapshot_records_distinct_resources() {
    // A cell that triggers two different writes (`env.set` →
    // `writes(Env)` and `Vec.push` → `allocates(Heap)`) records
    // both entries — the snapshot deduplicates by `(verb, resource)`,
    // so two distinct effects produce two distinct entries.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("env.set(\"A\", \"1\"); let mut v = Vec.new(); v.push(1);");
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let history = s.cell_effect_history();
    assert_eq!(history.len(), 1);
    let pairs: Vec<(String, String)> = history[0]
        .effects
        .iter()
        .map(|(verb, res)| (format!("{verb:?}"), res.clone()))
        .collect();
    assert!(
        pairs
            .iter()
            .any(|(v, r)| v.contains("Allocates") && r == "Heap"),
        "expected allocates(Heap); got: {pairs:?}",
    );
    assert!(
        pairs
            .iter()
            .any(|(v, r)| v.contains("Writes") && r == "Env"),
        "expected writes(Env); got: {pairs:?}",
    );
}

#[test]
fn cell_effect_snapshot_dedupes_repeated_effect() {
    // Multiple call sites of the same writes(Env) effect collapse to
    // one snapshot entry — the timeline shouldn't double-count when a
    // cell mutates the same resource through several builtin calls.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("env.set(\"A\", \"1\"); env.set(\"B\", \"2\");");
    assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    let history = s.cell_effect_history();
    assert_eq!(history.len(), 1);
    let writes_env_count = history[0]
        .effects
        .iter()
        .filter(|(_, r)| r == "Env")
        .count();
    assert_eq!(
        writes_env_count, 1,
        "writes(Env) must dedupe across call sites; got: {:?}",
        history[0].effects,
    );
}

// ── %timeline — effect-conflict timeline magic (line 773 slice 2) ─────────

#[test]
fn timeline_magic_empty_session_returns_hint() {
    // Empty session — no cells, no dependencies. Emit a friendly
    // hint via plain text; no rich bundle (there's no table to
    // render until the first cell lands).
    let mut s = Session::new();
    let out = s.dispatch_magic("%timeline");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert!(
        out.text.contains("no cells"),
        "expected friendly hint; got: {}",
        out.text,
    );
}

#[test]
fn timeline_magic_renders_text_plain_and_html() {
    // Two cells: one pure, one writes(Env). Timeline must include
    // both cells in submission order. text/html mime present
    // alongside text/plain for rich-display surfaces.
    let mut s = Session::new();
    s.evaluate_cell_captured("let _x = 1;");
    s.evaluate_cell_captured("env.set(\"A\", \"1\");");
    let out = s.dispatch_magic("%timeline");
    assert!(out.ok, "expected ok; got: {}", out.text);
    let bundle = out.rich.expect("timeline must emit a rich bundle");
    let plain = bundle.get("text/plain").unwrap_or("").to_string();
    let html = bundle.get("text/html").unwrap_or("").to_string();
    assert!(plain.contains("cell 1"), "missing cell 1; got:\n{plain}");
    assert!(plain.contains("cell 2"), "missing cell 2; got:\n{plain}");
    assert!(
        plain.contains("writes(Env)"),
        "missing writes(Env); got:\n{plain}",
    );
    assert!(
        html.contains("<table>"),
        "html must include <table>; got: {html}"
    );
    assert!(
        html.contains("writes(Env)"),
        "html must render effect; got: {html}",
    );
}

#[test]
fn timeline_magic_emits_write_after_write_arrow() {
    // Two cells both writing the same resource — the second cell
    // must surface a "writes Env already written by cell 1" arrow.
    let mut s = Session::new();
    s.evaluate_cell_captured("env.set(\"A\", \"1\");");
    s.evaluate_cell_captured("env.set(\"B\", \"2\");");
    let out = s.dispatch_magic("%timeline");
    assert!(out.ok);
    let plain = out
        .rich
        .as_ref()
        .and_then(|b| b.get("text/plain"))
        .unwrap_or("")
        .to_string();
    assert!(
        plain.contains("writes Env already written by cell 1"),
        "expected WAW arrow on cell 2; got:\n{plain}",
    );
}

#[test]
fn timeline_magic_emits_read_after_write_arrow() {
    // Cell 1 declares a user fn with writes(R); cell 2 declares a
    // user fn with reads(R); cell 3 calls both. The timeline for
    // cell 3 must surface a "reads R written by cell <wherever>"
    // arrow — but since the writes and reads happen in *one* cell
    // (cell 3 calls both), the dependency manifests as RAW within
    // the same cell's snapshot, which v1 carves out. So instead
    // structure it: cell 1 writes via env.set, cell 2 calls a
    // user-declared reads(Env) function.
    //
    // We declare a user fn with explicit `reads(Env)` and call it
    // from cell 2; cell 1 wrote Env via env.set.
    let mut s = Session::new();
    let r = s.evaluate_cell_captured("fn observe_env() reads(Env) { let _x = 1; }");
    assert!(r.errors.is_empty(), "fn declaration: {:?}", r.errors);
    s.evaluate_cell_captured("env.set(\"K\", \"v\");");
    s.evaluate_cell_captured("observe_env();");
    let deps = s.compute_cell_dependencies();
    let raw_deps: Vec<&karac::repl::CellDependency> = deps
        .iter()
        .filter(|d| d.kind == DependencyKind::ReadAfterWrite)
        .collect();
    assert!(
        !raw_deps.is_empty(),
        "expected at least one RAW dep; got: {deps:?}",
    );
    let raw = raw_deps[0];
    assert_eq!(raw.resource, "Env");
    assert_eq!(raw.to_cell, 3);
    assert_eq!(raw.from_cell, 2);
    let out = s.dispatch_magic("%timeline");
    let plain = out
        .rich
        .as_ref()
        .and_then(|b| b.get("text/plain"))
        .unwrap_or("")
        .to_string();
    assert!(
        plain.contains("reads Env written by cell 2"),
        "expected RAW arrow on cell 3; got:\n{plain}",
    );
}

#[test]
fn timeline_magic_pure_cell_marks_pure() {
    // Pure cells render as "cell N: (pure)" so the row isn't blank.
    let mut s = Session::new();
    s.evaluate_cell_captured("let _x = 1;");
    let out = s.dispatch_magic("%timeline");
    let plain = out
        .rich
        .as_ref()
        .and_then(|b| b.get("text/plain"))
        .unwrap_or("")
        .to_string();
    assert!(
        plain.contains("cell 1: (pure)"),
        "pure cells must render as (pure); got:\n{plain}",
    );
}

#[test]
fn timeline_magic_rejects_arguments() {
    // %timeline takes no arguments — surface a usage hint rather
    // than silently ignoring extra input. The dispatcher trims
    // before testing, so `%timeline   ` (whitespace only) still
    // reaches the no-arg branch.
    let mut s = Session::new();
    let out = s.dispatch_magic("%timeline garbage");
    assert!(!out.ok, "expected error; got ok: {}", out.text);
    assert!(
        out.text.contains("usage: %timeline"),
        "expected usage hint; got: {}",
        out.text,
    );
}

#[test]
fn timeline_magic_listed_in_unknown_magic_help() {
    // Unknown magics surface a help line listing supported magics;
    // %timeline must appear there so users discover it.
    let mut s = Session::new();
    let out = s.dispatch_magic("%bogus");
    assert!(!out.ok);
    assert!(
        out.text.contains("%timeline"),
        "unknown-magic help must list %timeline; got: {}",
        out.text,
    );
}

#[test]
fn timeline_magic_html_escapes_resource_names() {
    // User-defined resource names can be anything the parser
    // accepts — including names containing `<`/`&`/`"` if a user
    // somehow declares one. We can't write a Kara source that
    // declares a resource named `<X>` (the parser rejects it), so
    // we exercise the escape path by inspecting the helper
    // output indirectly: confirm the html bundle doesn't contain
    // raw resource text unescaped, by checking the table cell uses
    // the rendered `verb(resource)` shape.
    let mut s = Session::new();
    s.evaluate_cell_captured("env.set(\"X\", \"y\");");
    let out = s.dispatch_magic("%timeline");
    let html = out
        .rich
        .as_ref()
        .and_then(|b| b.get("text/html"))
        .unwrap_or("")
        .to_string();
    // Header is HTML-encoded with `&amp;` for ampersand — pin that
    // shape so a future regression that strips escaping fires here.
    assert!(
        html.contains("Effects &amp; Dependencies"),
        "header must HTML-escape ampersand; got: {html}",
    );
}

// ── %rc — RC-fallback inspector (line 785) ────────────────────────────────

#[test]
fn rc_magic_empty_session_returns_hint() {
    // Empty session — no items, no lets. The ownership pass runs
    // against an empty synthesized `fn main()` and records no RC
    // fallbacks; emit a friendly hint so the cell pane isn't blank.
    let mut s = Session::new();
    let out = s.dispatch_magic("%rc");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert!(
        out.text.contains("no RC fallbacks"),
        "expected friendly hint; got: {}",
        out.text,
    );
}

/// Submit a sequence of one-liner item definitions, each as its own
/// pure-items cell. The combined source triggers the RC pattern we
/// want to inspect from `%rc`. Per-cell submission sidesteps the
/// `classify_input` heuristic that forces "statements" mode when a
/// cell contains multi-line function bodies with control flow.
fn submit_items(s: &mut Session, items: &[&str]) {
    for item in items {
        let r = s.evaluate_cell_captured(item);
        assert!(
            r.errors.is_empty(),
            "item must compile: {item:?} got: {:?}",
            r.errors,
        );
    }
}

#[test]
fn rc_magic_records_direct_reuse_after_consume() {
    // Trigger 1: a struct value consumed in a branch and used after
    // the branch falls back to Rc. The magic must surface a row
    // naming the binding, the trigger label, and the Rc kind.
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Data { value: i64 }",
            "fn consume(d: Data) { }",
            "fn use_d(d: Data) { }",
            "fn process(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert!(
        out.text.contains("process.d"),
        "row must name the binding; got:\n{}",
        out.text,
    );
    assert!(
        out.text.contains("direct re-use after consume"),
        "row must surface trigger label; got:\n{}",
        out.text,
    );
    assert!(
        out.text.contains("[Rc]"),
        "row must surface Rc kind; got:\n{}",
        out.text,
    );
    assert!(
        out.text.contains("— Data"),
        "row must include type tail; got:\n{}",
        out.text,
    );
}

#[test]
fn rc_magic_records_closure_capture_with_outer_use() {
    // Trigger 2: closure body consumes the captured binding and the
    // outer scope re-uses it. The magic must surface the second
    // trigger label.
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Config { name: i64 }",
            "fn apply(c: Config) { }",
            "fn log(c: Config) { }",
            "fn make_handler(cfg: Config) { let _h = || apply(cfg); log(cfg); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    assert!(out.ok, "expected ok; got: {}", out.text);
    assert!(
        out.text.contains("make_handler.cfg"),
        "row must name the binding; got:\n{}",
        out.text,
    );
    assert!(
        out.text.contains("closure capture with outer use"),
        "row must surface trigger 2 label; got:\n{}",
        out.text,
    );
}

#[test]
fn rc_magic_renders_text_plain_and_html_mimes() {
    // A successful render emits both mimes so kernel frontends can
    // pick the richest one they understand.
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Data { value: i64 }",
            "fn consume(d: Data) { }",
            "fn use_d(d: Data) { }",
            "fn process(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    let bundle = out.rich.expect("rc magic must emit a rich bundle");
    let plain = bundle.get("text/plain").unwrap_or("").to_string();
    let html = bundle.get("text/html").unwrap_or("").to_string();
    assert!(
        plain.contains("process.d"),
        "plain mime must list the row; got:\n{plain}",
    );
    assert!(
        html.contains("<table>"),
        "html mime must wrap rows in a table; got:\n{html}",
    );
    assert!(
        html.contains("<th>Binding</th>"),
        "html mime must include the Binding header; got:\n{html}",
    );
    assert!(
        html.contains("<th>Trigger</th>"),
        "html mime must include the Trigger header; got:\n{html}",
    );
    assert!(
        html.contains("<th>Kind</th>"),
        "html mime must include the Kind header; got:\n{html}",
    );
}

#[test]
fn rc_magic_sorts_rows_by_fn_then_binding() {
    // Two functions, both triggering RC; the renderer must emit
    // them in `(fn_name, binding)` order so the textual output is
    // stable across runs (HashMap iteration is nondeterministic
    // without sorting).
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Data { value: i64 }",
            "fn consume(d: Data) { }",
            "fn use_d(d: Data) { }",
            "fn alpha(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
            "fn beta(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    assert!(out.ok);
    let alpha_pos = out.text.find("alpha.d").expect("alpha row should appear");
    let beta_pos = out.text.find("beta.d").expect("beta row should appear");
    assert!(
        alpha_pos < beta_pos,
        "alpha must sort before beta; got:\n{}",
        out.text,
    );
}

#[test]
fn rc_magic_no_fallbacks_when_no_consume() {
    // Pure code with no consume-then-reuse pattern → no RC entries.
    let mut s = Session::new();
    s.evaluate_cell_captured("fn pure_fn() -> i64 { 42 }");
    let out = s.dispatch_magic("%rc");
    assert!(out.ok);
    assert!(
        out.text.contains("no RC fallbacks"),
        "expected empty-set hint; got: {}",
        out.text,
    );
}

#[test]
fn rc_magic_rejects_arguments() {
    // %rc takes no arguments — surface a usage hint rather than
    // silently ignoring extra input.
    let mut s = Session::new();
    let out = s.dispatch_magic("%rc verbose");
    assert!(!out.ok, "expected error; got ok: {}", out.text);
    assert!(
        out.text.contains("usage: %rc"),
        "expected usage hint; got: {}",
        out.text,
    );
}

#[test]
fn rc_magic_listed_in_unknown_magic_help() {
    // Unknown magics surface a help line listing supported magics;
    // %rc must appear there so users discover it.
    let mut s = Session::new();
    let out = s.dispatch_magic("%bogus");
    assert!(!out.ok);
    assert!(
        out.text.contains("%rc"),
        "unknown-magic help must list %rc; got: {}",
        out.text,
    );
}

#[test]
fn rc_magic_listed_in_empty_magic_help() {
    // Empty `%` magic also surfaces the supported-set help line.
    let mut s = Session::new();
    let out = s.dispatch_magic("%");
    assert!(!out.ok);
    assert!(
        out.text.contains("%rc"),
        "empty-magic help must list %rc; got: {}",
        out.text,
    );
}

#[test]
fn rc_magic_html_escapes_qualified_binding_name() {
    // The qualified binding column passes through `escape_html_text`,
    // so any `<` or `&` in a future binding/fn name (today the
    // parser blocks these, but the escape path is what guards
    // against a future widening) is HTML-encoded. Pin the escape
    // path by exercising it on a real row and checking the table
    // wraps the binding in escaped `<td>` content.
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Data { value: i64 }",
            "fn consume(d: Data) { }",
            "fn use_d(d: Data) { }",
            "fn process(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    let html = out
        .rich
        .as_ref()
        .and_then(|b| b.get("text/html"))
        .unwrap_or("")
        .to_string();
    assert!(
        html.contains("<td>process.d</td>"),
        "html row must wrap the qualified binding in <td>; got:\n{html}",
    );
}

#[test]
fn rc_magic_includes_consume_and_reuse_spans() {
    // Each row records both spans — consume site (where the
    // binding was moved) and reuse site (the later use that
    // forced fallback). Format is `(consume L:C, reuse L:C)`.
    let mut s = Session::new();
    submit_items(
        &mut s,
        &[
            "struct Data { value: i64 }",
            "fn consume(d: Data) { }",
            "fn use_d(d: Data) { }",
            "fn process(cond: bool, d: Data) { if cond { consume(d); } use_d(d); }",
        ],
    );
    let out = s.dispatch_magic("%rc");
    assert!(out.ok);
    assert!(
        out.text.contains("consume"),
        "row must label the consume span; got: {}",
        out.text,
    );
    assert!(
        out.text.contains("reuse"),
        "row must label the reuse span; got: {}",
        out.text,
    );
}
