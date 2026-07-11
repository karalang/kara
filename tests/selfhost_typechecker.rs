//! Differential oracle for the self-hosted **type checker** (Phase 12,
//! TypeChecker port Slice 1) — the peer of `selfhost_resolver.rs` /
//! `selfhost_parser.rs`. Each corpus entry is a complete program; both the Rust
//! seed (`karac::{parse, resolve, typecheck}`) and the Kāra port
//! (`typechecker.typecheck_program`, built AOT via `karac build`) render their
//! type errors as an ordered `(kind @offset:length)` list, and the two are
//! diffed line-for-line.
//!
//! ## Slice 1 scope — coarse primitive categories, two checks
//!
//! The port infers a single coarse CATEGORY per expression — NUM (every int
//! width + f32/f64, and int/float literals) / BOOL / CHAR / STR / UNKNOWN — and
//! fires only when BOTH sides of a comparison land in the known primitive set:
//!
//!   * RETURN-TYPE mismatch (`TypeMismatch`): a fn whose declared `-> T`
//!     category is a known primitive and whose body-TAIL infers to a DIFFERENT
//!     known primitive.
//!   * CONDITION-not-bool (`ConditionNotBool`): an `if <cond>` whose cond
//!     infers to a known NON-bool primitive.
//!
//! All numerics collapse to ONE category, reproducing the seed's literal
//! leniency EXACTLY (`fn f() -> f64 { 1 }` and `-> i64 { 1.5 }` are both clean).
//! UNKNOWN (calls, identifiers, `Vec[T]`, comparisons, user types) is never
//! flagged — so the corpus is curated to keep every seed-produced type error
//! within the two Slice-1 kinds; `rust_render` panics on any other kind so a
//! drifting entry fails loudly rather than silently diffing clean. The corpus is
//! also resolve-clean (no undefined names), so resolve errors never perturb the
//! type-error stream the oracle compares.

use std::path::PathBuf;

/// Complete programs — single- and multi-item — exercising the two Slice-1
/// checks and, crucially, the UNKNOWN carve-outs that must NOT false-positive.
const CORPUS: &[&str] = &[
    // ── clean: return type matches (with numeric leniency) ──
    "fn ok_unit() { }",
    "fn ret_i64() -> i64 { 1 }",
    "fn ret_f64_from_int() -> f64 { 1 }",
    "fn ret_i64_from_float() -> i64 { 1.5 }",
    "fn ret_char() -> char { 'a' }",
    "fn ret_bool() -> bool { true }",
    "fn ret_str() -> String { \"hi\" }",
    // ── return-type mismatch (kind 0) at the tail span ──
    "fn bad_bool_from_int() -> bool { 1 }",
    "fn bad_str_from_char() -> String { 'a' }",
    "fn bad_bool_from_str() -> bool { \"hi\" }",
    "fn bad_char_from_int() -> char { 1 }",
    "fn bad_i64_from_bool() -> i64 { true }",
    "fn bad_str_from_int() -> String { 42 }",
    // ── condition-not-bool (kind 1) at the cond span ──
    "fn cond_int() { if 1 { } }",
    "fn cond_char() { if 'c' { } }",
    "fn cond_str() { if \"s\" { } }",
    "fn cond_true() { if true { } }",
    "fn cond_false() { if false { } }",
    "fn cond_in_let() -> i64 { let x = 1; if 'c' { } 2 }",
    // nested / else-if condition checks reach through blocks and else branches.
    "fn nested_if_ok() { if true { if false { } } }",
    "fn nested_if_bad() { if true { if 1 { } } }",
    "fn else_if_bad() { if true { } else { if 'x' { } } }",
    // ── Slice 2: binding / parameter type environment → identifier inference ──
    // Params infer from their declared type; a return / condition referencing a
    // param of a mismatched primitive category is flagged at the identifier span.
    "fn param_ret_bad(b: bool) -> i64 { b }",
    "fn param_ret_ok(n: i64) -> i64 { n }",
    "fn param_cond_bad(n: i64) { if n { } }",
    "fn param_cond_ok(b: bool) { if b { } }",
    "fn param_str_ret(s: String) -> i64 { s }",
    "fn param_char_cond(c: char) { if c { } }",
    // Un-annotated `let x = <expr>` infers x from the RHS category; references
    // then infer through the binding (return / condition / RHS-of-another-let).
    "fn let_ret_bad() -> bool { let x = 1; x }",
    "fn let_ret_ok() -> i64 { let x = 1; x }",
    "fn let_cond_bad() { let x = 1; if x { } }",
    "fn let_cond_ok() { let b = true; if b { } }",
    "fn let_numeric_ret() -> i64 { let x = 1.5; x }",
    // Transitive: `let y = x` carries x's category; shadowing: a later `let x`
    // in the same scope wins (newest-first lookup).
    "fn transitive() -> i64 { let x = true; let y = x; if y { } 1 }",
    "fn chained_bad(n: i64) -> bool { let m = n; m }",
    "fn shadow_ok() -> bool { let x = 1; let x = true; x }",
    // A block-local does NOT leak past its block (scoped env).
    "fn scope_leak() -> i64 { if true { let z = true; } let z = 1; z }",
    // ── UNKNOWN carve-outs — must NOT flag (the seed agrees on these) ──
    // A binary/comparison tail is UNKNOWN to Slice 2 (operator result typing is
    // a later slice); the seed types `n > 0` as bool, and since ret IS bool
    // both are clean — the carve-out must not false-positive.
    "fn cmp_tail_ok(n: i64) -> bool { n > 0 }",
    // ── multi-item programs — per-fn errors in traversal order ──
    "fn a() -> bool { 1 }\nfn b() -> i64 { 2 }\nfn c() -> String { 'x' }",
    "fn ok_a() -> i64 { 1 }\nfn bad_b() -> bool { 2 }",
    "struct S { x: i64 }\nfn m() -> i64 { 0 }",
    // Empty program.
    "",
];

/// Byte offset shift between the Rust and Kāra spans — 0 (both check the
/// identical bare program; no wrapper).
const OFFSET_SHIFT: i64 = 0;

/// The Rust seed's canonical render of `src`'s TYPE errors — the ordered
/// `(kind @offset:length)` list, `(ok)` when clean. Filters to the two Slice-1
/// kinds and panics on any other kind so a drifting corpus entry fails loudly.
fn rust_render(src: &str) -> String {
    let parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let result = karac::typecheck(&parsed.program, &resolved);
    if result.errors.is_empty() {
        return "(ok)".to_string();
    }
    let mut parts: Vec<String> = Vec::new();
    for e in &result.errors {
        let k = format!("{:?}", e.kind);
        let tag = match k.as_str() {
            "TypeMismatch" | "ReturnTypeMismatch" | "BranchTypeMismatch" => "type-mismatch",
            "ConditionNotBool" => "cond-not-bool",
            other => panic!(
                "corpus entry {src:?} produced an out-of-Slice-1 type-error kind {other} \
                 (message: {}); trim the corpus or extend the slice",
                e.message
            ),
        };
        let off = e.span.offset as i64 + OFFSET_SHIFT;
        parts.push(format!("({tag} @{off}:{})", e.span.length));
    }
    parts.join(" ")
}

/// A Kāra string literal escaping of `input` (for embedding in the driver).
fn kara_str_lit(input: &str) -> String {
    input
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Build the real selfhost modules + a crate-root `driver` into a temp project,
/// run the resulting binary, and return its stdout lines — or `None` on a benign
/// skip (no llvm feature / missing runtime archive). A compiler PANIC / error is
/// a hard failure (a port regression), never a skip.
fn build_and_run_driver(tag: &str, driver: &str) -> Option<Vec<String>> {
    let tmp = std::env::temp_dir().join(format!(
        "karac-selfhost-typechecker-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"tc\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    let selfhost_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src");
    for f in [
        "span.kara",
        "token.kara",
        "lexer.kara",
        "ast.kara",
        "parser.kara",
        "typechecker.kara",
    ] {
        std::fs::copy(selfhost_src.join(f), tmp.join("src").join(f))
            .unwrap_or_else(|e| panic!("copy selfhost module {f}: {e}"));
    }
    std::fs::write(tmp.join("src").join("main.kara"), driver).unwrap();

    let build = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("tc");

    if !bin.exists() {
        let compiler_crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = compiler_crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted typechecker FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated driver ---\n{driver}"
        );
        eprintln!(
            "skip: selfhost typechecker oracle [{tag}] — did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }

    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara typechecker binary");
    assert!(
        run.status.success(),
        "kara typechecker binary exited nonzero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let kout = String::from_utf8_lossy(&run.stdout);
    let lines: Vec<String> = kout
        .lines()
        .map(|l| l.trim_end().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let _ = std::fs::remove_dir_all(&tmp);
    Some(lines)
}

/// TypeChecker differential gate (Slice 1). Builds the real selfhost modules +
/// typechecker into a temp project with a per-input driver over `parse_program`
/// + `typecheck_program`, runs, and diffs each line against the Rust seed.
#[test]
fn selfhost_typechecker_matches_rust_typechecker() {
    let mut driver = String::from(
        "import parser.parse_program;\n\
         import typechecker.{typecheck_program, render_errors};\n\
         \n\
         fn check(src: String) with panics {\n\
         \x20   println(render_errors(typecheck_program(parse_program(src))));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        driver.push_str(&format!("    check(\"{}\");\n", kara_str_lit(input)));
    }
    driver.push_str("}\n");

    let Some(kara_lines) = build_and_run_driver("program", &driver) else {
        return;
    };
    let rust_lines: Vec<String> = CORPUS.iter().map(|input| rust_render(input)).collect();

    if let Some((i, (k, r))) = kara_lines
        .iter()
        .zip(rust_lines.iter())
        .enumerate()
        .find(|(_, (k, r))| k != r)
    {
        panic!(
            "self-hosted typechecker diverged from the Rust typechecker at input {i} ({:?}):\n  \
             Kāra: {k}\n  Rust: {r}",
            CORPUS[i]
        );
    }
    assert_eq!(
        kara_lines.len(),
        rust_lines.len(),
        "line-count mismatch (Kāra {} vs Rust {})",
        kara_lines.len(),
        rust_lines.len()
    );
}
