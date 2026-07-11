//! Differential oracle for the self-hosted **resolver** (Phase 12, Resolver
//! port Slice 1). Sibling of `tests/selfhost_parser{,_items,_types}.rs`: a
//! shared corpus of bare top-level `fn` items is name-resolved by BOTH the
//! Rust seed (`karac::resolve(karac::parse(src).program)`) and the Kāra
//! resolver (`selfhost/src/resolver.kara::resolve_item`, built AOT via
//! `karac build`), each rendered to the same canonical form — the ordered
//! list of `(kind @offset:length)` tuples — and the two streams are diffed.
//!
//! ## Slice-1 scope + corpus discipline
//!
//! Slice 1 seeds ONLY the 16 `PRELUDE_PRIMITIVES` and resolves a single `fn`:
//! generics (bare params), `self`, params + types, return type, and the body
//! (Let/Assign/expr statements; the reachable `Expr` forms; block/if/loop/
//! match scopes). Only four error kinds are in scope — UndefinedName,
//! UndefinedType, DuplicateDefinition, ReservedIdentifier. The corpus must
//! therefore reference no prelude name outside the 16 primitives (`Vec` /
//! `Option` / `println` / `Some` / ... are seeded on the Rust side but not yet
//! on the Kāra side, so they would diverge), carry no attributes (the Rust
//! `resolve()` runs attribute validation the Kāra side does not), and exercise
//! duplicate / reserved definitions only through GENERIC params — whose span
//! is the bare identifier. A `FnParamNode` has no separate name span (its span
//! covers `name: TYPE`), so a duplicate / reserved PARAM would diverge on span
//! (a name span is a later slice's work).
//!
//! The Rust seed asserts every produced error is one of the four in-scope
//! kinds, so a corpus entry that drifts out of the slice fails loudly rather
//! than silently diffing clean.

use karac::resolver::ResolveErrorKind;
use std::path::PathBuf;

/// Bare top-level `fn` items — the only forms Slice 1 resolves. Types use only
/// the 16 primitives; no prelude functions/types/variants, no attributes.
const CORPUS: &[&str] = &[
    // Clean shapes.
    "fn ok() {}",
    "fn typed(x: i64) -> i64 { x }",
    "fn two_params(a: i64, b: bool) -> bool { b }",
    "fn recurse() { recurse() }",
    "fn shadow() { let x = 1; let x = 2; x }",
    "fn selfmeth(ref self) { self }",
    "fn use_generic[T](x: T) -> T { x }",
    "fn tuple_ty(p: (i64, bool)) {}",
    "fn nested_ok() { let a = 1; if true { let b = a; b } else { a } }",
    "fn while_ok() { let mut i = 0; while true { i = i + 1 } }",
    "fn for_ok(n: i64) { for k in n { k } }",
    "fn loop_ok() { loop { let z = 1; z } }",
    // Undefined name.
    "fn undef() { y }",
    "fn rhs_first() { let x = x; x }",
    "fn two_errs() { a; b }",
    "fn nested_undef() { if true { z } }",
    "fn call_undef(a: i64) { a; missing(a) }",
    "fn for_iter_undef() { for k in src { k } }",
    // Undefined type.
    "fn bad_ret() -> Nope { }",
    "fn bad_param(a: Missing) {}",
    "fn bad_tuple(p: (Nope, i64)) {}",
    // Duplicate / reserved — via generics (name-only span).
    "fn dup_generic[T, T]() {}",
    "fn reserved_generic[Fn]() {}",
    "fn reserved_split[split_by_variant]() {}",
];

/// Byte offset shift between the Rust and Kāra spans — 0 (both resolve the
/// identical bare item; no wrapper).
const OFFSET_SHIFT: i64 = 0;

/// The Rust seed's canonical render of `src`'s resolve errors — the ordered
/// `(kind @offset:length)` list, `(ok)` when clean. Asserts every error is one
/// of the four Slice-1 kinds so a drifting corpus entry fails loudly.
fn rust_render(src: &str) -> String {
    let parsed = karac::parse(src);
    let result = karac::resolve(&parsed.program);
    if result.errors.is_empty() {
        return "(ok)".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for e in &result.errors {
        let tag = match &e.kind {
            ResolveErrorKind::UndefinedName => "undef-name",
            ResolveErrorKind::UndefinedType => "undef-type",
            ResolveErrorKind::DuplicateDefinition => "dup-def",
            ResolveErrorKind::ReservedIdentifier => "reserved",
            other => panic!(
                "corpus entry {src:?} produced an out-of-Slice-1 resolve error kind {other:?} \
                 (message: {}); trim the corpus or extend the slice",
                e.message
            ),
        };
        let off = e.span.offset as i64 + OFFSET_SHIFT;
        parts.push(format!("({tag} @{off}:{})", e.span.length));
    }
    parts.join(" ")
}

/// Resolver differential gate (Slice 1). Same harness as the parser oracles:
/// build the real selfhost modules + resolver into a temp project with a
/// per-input driver, run, and diff each line against the Rust seed's render.
#[test]
fn selfhost_resolver_matches_rust_resolver() {
    // 1. Crate-root driver over the Kāra `parse_item_str` + `resolve_item`.
    let mut prog = String::from(
        "import parser.parse_item_str;\n\
         import resolver.{resolve_item, render_errors};\n\
         \n\
         fn check(src: String) with panics {\n\
         \x20   println(render_errors(resolve_item(parse_item_str(src))));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        let escaped = input
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t");
        prog.push_str(&format!("    check(\"{escaped}\");\n"));
    }
    prog.push_str("}\n");

    // 2. Assemble a temp PROJECT reusing the real selfhost modules.
    let tmp = std::env::temp_dir().join(format!("karac-selfhost-resolver-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"resolve\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    let selfhost_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src");
    for f in [
        "span.kara",
        "token.kara",
        "lexer.kara",
        "ast.kara",
        "parser.kara",
        "resolver.kara",
    ] {
        std::fs::copy(selfhost_src.join(f), tmp.join("src").join(f))
            .unwrap_or_else(|e| panic!("copy selfhost module {f}: {e}"));
    }
    std::fs::write(tmp.join("src").join("main.kara"), &prog).unwrap();

    let build = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("resolve");

    if !bin.exists() {
        // A compiler PANIC / signal-kill / error is a real bug, never a benign
        // skip (mirrors the item oracle's hard-failure guard).
        let compiler_crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = compiler_crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted resolver FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated source ---\n{prog}"
        );
        eprintln!(
            "skip: selfhost_resolver_matches_rust_resolver — did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    // 3. Run the Kāra resolver binary.
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara resolver binary");
    assert!(
        run.status.success(),
        "kara resolver binary exited nonzero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let kout = String::from_utf8_lossy(&run.stdout);
    let kara_lines: Vec<String> = kout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    // 4. Expected = the Rust seed's render of every input, in corpus order.
    let rust_lines: Vec<String> = CORPUS.iter().map(|input| rust_render(input)).collect();

    if let Some((i, (k, r))) = kara_lines
        .iter()
        .zip(rust_lines.iter())
        .enumerate()
        .find(|(_, (k, r))| k != r)
    {
        panic!(
            "self-hosted resolver diverged from the Rust resolver at input {i} ({:?}):\n  \
             Kāra: {k}\n  Rust: {r}\n--- full Kāra output ---\n{kout}",
            CORPUS[i]
        );
    }
    assert_eq!(
        kara_lines.len(),
        rust_lines.len(),
        "line-count mismatch (Kāra {} vs Rust {})\n--- full Kāra output ---\n{kout}",
        kara_lines.len(),
        rust_lines.len()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
