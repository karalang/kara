//! Drift guard between the symbols **codegen declares** and the symbols the
//! runtime **force-preserves** (`__preserve_no_mangle_symbols`).
//!
//! # Why this exists
//!
//! Under `lto = "fat"`, `#[no_mangle]` alone does not keep a symbol alive in
//! the staticlib: nothing inside the runtime crate calls these functions — only
//! JIT'd / AOT'd Kāra code does — so cross-module DCE is free to strip them.
//! `runtime/src/lib.rs`'s `__preserve_no_mangle_symbols()` exists to take their
//! addresses and defeat that. A symbol codegen emits a call to, but which the
//! keep-list never mentions, is therefore at risk of vanishing from the archive.
//!
//! Today that class is caught only *indirectly*, and only sometimes: the E2E
//! harness fails at link with `undefined reference to karac_*` — but ONLY if
//! some test program happens to exercise the symbol. A symbol with no E2E
//! coverage can drift silently until a user's program is the first to call it
//! (the `karac_realloc_or_panic` shape, B-2026-07-12-22). This test closes that
//! gap statically: it needs no program to call the symbol, only for codegen to
//! declare it.
//!
//! # Why a trivial program suffices
//!
//! `Codegen::new` declares the whole extern surface **unconditionally**, before
//! it has seen a single user item, so any successfully-compiled program's module
//! carries every declaration. `fn main() { println("hi"); }` is enough.
//!
//! # The pinned exceptions
//!
//! 17 declared symbols are absent from the keep-list today. They are all in the
//! async / net / TLS families, whose definitions live behind `cfg`-gated
//! modules (`runtime/src/event_loop.rs`, the `tls`-gated builder surface), so
//! whether each is genuinely at strip risk needs per-symbol triage rather than
//! a blanket assertion. They are pinned in `KNOWN_ABSENT` rather than ignored,
//! and the pin is two-sided: adding a pinned symbol to the keep-list turns this
//! test RED and forces the entry to be removed, exactly like
//! `tests/example_corpus.rs`'s known-broken pins. New drift — a symbol in
//! neither the keep-list nor this list — fails immediately.

#![cfg(feature = "llvm")]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Declared-but-not-preserved symbols as of 2026-07-31.
///
/// NOT an approval list. Each entry is an open question ("is this reachable
/// from other runtime code, or is it one LTO pass away from disappearing?"),
/// recorded so that the *set* stops growing silently.
const KNOWN_ABSENT: &[&str] = &[
    "karac_runtime_event_loop_register_fd_cancel",
    "karac_runtime_http_builder_add_header",
    "karac_runtime_http_builder_free",
    "karac_runtime_http_builder_new",
    "karac_runtime_http_builder_send",
    "karac_runtime_http_builder_set_body",
    "karac_runtime_http_builder_set_timeout",
    "karac_runtime_http_response_header",
    "karac_runtime_park_slot_cancel_ptr",
    "karac_runtime_park_slot_load_result",
    "karac_runtime_park_slot_store_result",
    "karac_runtime_spawn_coro",
    "karac_runtime_tcp_connect",
    "karac_runtime_tcp_connect_finish",
    "karac_runtime_tcp_connect_start",
    "karac_runtime_tcp_shutdown",
    "karac_runtime_tcp_try_clone",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `karac_*` symbol the backend declares in a freshly-built module.
fn declared_symbols() -> BTreeSet<String> {
    // Mirrors examples/dump_ir.rs's chain. `lower_program` is not optional —
    // it forwards the typechecker's span-keyed side tables into the AST.
    let src = r#"fn main() { println("hi"); }"#;
    let parsed = karac::parse(src);
    assert!(parsed.errors.is_empty(), "probe parse: {:?}", parsed.errors);
    let mut program = parsed.program;
    let res = karac::resolve(&program);
    assert!(res.errors.is_empty(), "probe resolve: {:?}", res.errors);
    let tc = karac::typecheck(&program, &res);
    assert!(tc.errors.is_empty(), "probe typecheck: {:?}", tc.errors);
    karac::lowering::lower_program(&mut program, &tc);
    let own = karac::ownershipcheck(&program, &tc);
    let ir = karac::codegen::compile_to_ir(&program, Some(&own), None).expect("probe codegen");

    ir.lines()
        .filter(|l| l.starts_with("declare"))
        .filter_map(|l| l.split_once('@'))
        .map(|(_, rest)| {
            rest.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("")
                .to_string()
        })
        .filter(|s| s.starts_with("karac_"))
        .collect()
}

/// Every `karac_*` symbol named inside `__preserve_no_mangle_symbols`.
fn keep_list_symbols() -> BTreeSet<String> {
    let path = repo_root().join("runtime/src/lib.rs");
    let src = std::fs::read_to_string(&path).expect("read runtime/src/lib.rs");
    let lines: Vec<&str> = src.lines().collect();

    let start = lines
        .iter()
        .position(|l| l.contains("fn __preserve_no_mangle_symbols"))
        .expect("__preserve_no_mangle_symbols not found — was it renamed?");

    let mut depth = 0i32;
    let mut started = false;
    let mut end = start;
    for (i, l) in lines.iter().enumerate().skip(start) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        if l.contains('{') {
            started = true;
        }
        if started && depth <= 0 {
            end = i;
            break;
        }
    }
    assert!(end > start, "could not delimit the keep-list body");

    let mut out = BTreeSet::new();
    for l in &lines[start..=end] {
        let mut rest: &str = l;
        while let Some(idx) = rest.find("karac_") {
            let tail = &rest[idx..];
            let sym: String = tail
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            out.insert(sym);
            rest = &tail[6..];
        }
    }
    out
}

#[test]
fn every_declared_symbol_is_force_preserved() {
    let declared = declared_symbols();
    let keep = keep_list_symbols();

    // Guard against the check silently becoming vacuous (a renamed keep-list
    // fn, or an IR format change that stops yielding `declare` lines).
    assert!(
        declared.len() > 200,
        "only {} declared karac_* symbols — the IR probe looks broken, not the invariant",
        declared.len(),
    );
    assert!(
        keep.len() > 200,
        "only {} keep-list symbols — the extractor looks broken, not the invariant",
        keep.len(),
    );

    let pinned: BTreeSet<String> = KNOWN_ABSENT.iter().map(|s| s.to_string()).collect();
    let missing: Vec<&String> = declared
        .iter()
        .filter(|s| !keep.contains(*s) && !pinned.contains(*s))
        .collect();

    assert!(
        missing.is_empty(),
        "{} symbol(s) are declared by codegen but never referenced in \
         runtime/src/lib.rs's `__preserve_no_mangle_symbols`. Under fat LTO \
         nothing else keeps them alive, so they can be stripped from the \
         staticlib and surface later as `undefined reference to karac_*` at \
         link time. Add each to the keep-list (or, if it is genuinely \
         unreachable, to KNOWN_ABSENT with a reason):\n  {}",
        missing.len(),
        missing
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}

/// The pin is two-sided: once a `KNOWN_ABSENT` symbol gains a keep-list entry,
/// this fails and forces the stale exception to be dropped, so the list can
/// only shrink.
#[test]
fn known_absent_entries_are_still_absent() {
    let keep = keep_list_symbols();
    let declared = declared_symbols();

    let promoted: Vec<&&str> = KNOWN_ABSENT.iter().filter(|s| keep.contains(**s)).collect();
    assert!(
        promoted.is_empty(),
        "{} KNOWN_ABSENT symbol(s) are now in the keep-list. Remove them from \
         KNOWN_ABSENT — the exception is obsolete:\n  {}",
        promoted.len(),
        promoted
            .iter()
            .map(|s| **s)
            .collect::<Vec<_>>()
            .join("\n  "),
    );

    // A symbol codegen no longer declares is also a stale exception.
    let undeclared: Vec<&&str> = KNOWN_ABSENT
        .iter()
        .filter(|s| !declared.contains(**s))
        .collect();
    assert!(
        undeclared.is_empty(),
        "{} KNOWN_ABSENT symbol(s) are no longer declared by codegen. Remove \
         them from KNOWN_ABSENT:\n  {}",
        undeclared.len(),
        undeclared
            .iter()
            .map(|s| **s)
            .collect::<Vec<_>>()
            .join("\n  "),
    );
}
