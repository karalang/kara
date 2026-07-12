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
    // ── Slice 3: operator result types ──
    // Comparisons + logical → BOOL; arithmetic / bitwise → the shared operand
    // category (NUM op NUM → NUM; `+` polymorphic: STR + STR → STR); unary `-`/`~`
    // → NUM, `not` → BOOL. Mismatch / cond anchors on the WHOLE operator span.
    "fn cmp_ret_bad(n: i64) -> i64 { n > 0 }",
    "fn cmp_ret_ok(n: i64) -> bool { n > 0 }",
    "fn arith_ret_bad(n: i64) -> bool { n + 1 }",
    "fn arith_ret_ok(n: i64) -> i64 { n + 1 }",
    "fn bitwise_ret_bad(n: i64) -> bool { n & 1 }",
    "fn logical_ret_bad(a: bool, b: bool) -> i64 { a and b }",
    "fn neg_ret_bad(n: i64) -> bool { -n }",
    "fn not_ret_bad(b: bool) -> i64 { not b }",
    "fn bitnot_ret_bad(n: i64) -> bool { ~n }",
    // `+` polymorphism: String concat yields STR, numeric add yields NUM.
    "fn str_concat_bad(s: String) -> i64 { s + s }",
    "fn str_concat_ok(s: String) -> String { s + s }",
    "fn str_ne_ok(s: String) -> bool { s != s }",
    "fn char_eq_ok(c: char) -> bool { c == c }",
    // Operator result feeding a condition; and operand inference through a let.
    "fn cmp_cond_ok(n: i64) { if n > 0 { } }",
    "fn arith_cond_bad(n: i64) { if n + 1 { } }",
    "fn operand_via_let(n: i64) -> bool { let x = n + 1; x > 0 }",
    // ── Slice 4: annotated-`let` (`let x: T = v`) ──
    // The initializer is checked against the DECLARED type (mismatch at the
    // value span), and the binding takes the DECLARED category so later
    // references use `x: T` (not the initializer's).
    "fn al_bool_bad() { let x: bool = 1; }",
    "fn al_i64_ok() { let x: i64 = 1; }",
    "fn al_num_lenient() { let x: i64 = 1.5; }",
    "fn al_str_bad() { let x: String = 1; }",
    "fn al_char_ok() { let x: char = 'a'; }",
    // Binding takes the DECLARED type: after `let x: bool = 1` (a mismatch), `x`
    // is bool — so `if x` is a valid condition (only the let error fires).
    "fn al_bind_decl() { let x: bool = 1; if x { } }",
    // Declared type drives a downstream check: `x: bool` used where i64 wanted.
    "fn al_decl_ret_bad() -> i64 { let x: bool = true; x }",
    "fn al_cond_from_decl() { let n: i64 = 0; if n { } }",
    "fn al_numeric_ret_ok() -> i64 { let x: i64 = 1.5; x }",
    "fn al_str_ret_bad() -> bool { let s: String = \"a\"; s }",
    // Initializer inferred from a param, checked against the annotation.
    "fn al_from_param(n: i64) { let m: bool = n; if m { } }",
    // ── Slice 5: call-return typing ──
    // A bare-name call infers its result from the callee's declared return
    // category (a COLLECT pass records every fn's return type first, so forward
    // references work). The mismatch anchors on the callee span (no parens).
    "fn call_ret_bad() -> bool { g() }\nfn g() -> i64 { 0 }",
    "fn call_ret_ok() -> i64 { g() }\nfn g() -> i64 { 0 }",
    "fn call_cond_bad() { if g() { } }\nfn g() -> i64 { 0 }",
    "fn call_cond_ok() { if g() { } }\nfn g() -> bool { true }",
    "fn call_str_bad() -> i64 { greet() }\nfn greet() -> String { \"hi\" }",
    // Call result flows through a `let` binding, then a downstream check.
    "fn call_via_let() -> i64 { let x = g(); x }\nfn g() -> bool { true }",
    // Call with arguments; args are walked (a bad arg-internal cond is caught).
    "fn call_args(n: i64) -> bool { add(n, 1) }\nfn add(a: i64, b: i64) -> i64 { a }",
    // Forward reference the other way: callee declared BEFORE the caller.
    "fn g() -> i64 { 0 }\nfn h() -> bool { g() }",
    // Recursion: a fn calling itself resolves its own signature.
    "fn fib(n: i64) -> i64 { fib(n) }",
    // ── Slice 6: struct-literal + field-access typing ──
    // Struct-literal field VALUES are checked against declared field types
    // (mismatch at the value span); the literal's category is the struct type,
    // so `p.x` resolves through it. Distinct structs are incompatible.
    "struct P { x: i64 }\nfn f() -> P { P { x: 1 } }",
    "struct P { x: i64 }\nfn f() -> P { P { x: true } }",
    "struct P { x: i64 }\nfn f() -> bool { P { x: 1 } }",
    "struct P { x: i64, y: bool }\nfn f() -> P { P { x: 1, y: 2 } }",
    // Field access through a param, a let binding, and a call result.
    "struct P { x: i64 }\nfn f(p: P) -> i64 { p.x }",
    "struct P { x: i64 }\nfn f(p: P) -> bool { p.x }",
    "struct P { x: i64 }\nfn f(p: P) { if p.x { } }",
    "struct P { x: bool }\nfn f(p: P) { if p.x { } }",
    "struct P { x: i64 }\nfn f() { let p = P { x: 1 }; if p.x { } }",
    "struct P { x: i64 }\nfn mk() -> P { P { x: 1 } }\nfn f() -> i64 { mk().x }",
    // Struct identity: a `P` value where `Q` is declared is a mismatch; the same
    // struct is compatible.
    "struct P { x: i64 }\nfn f(p: P) -> P { p }",
    "struct P { x: i64 }\nstruct Q { x: i64 }\nfn f(p: P) -> Q { p }",
    // Annotated let with a struct type: initializer struct checked vs the annotation.
    "struct P { x: i64 }\nstruct Q { x: i64 }\nfn f(p: P) { let q: Q = p; }",
    // ── Slice 7: field-name checks (ExtraField / UndefinedField) ──
    // A struct-literal field not declared by the struct is ExtraField at the
    // `name: value` span; accessing an undeclared field is UndefinedField at the
    // receiver span. Both fire only for a KNOWN (same-module) struct.
    "struct P { x: i64 }\nfn f() -> P { P { x: 1, z: 2 } }",
    "struct P { x: i64 }\nfn f(p: P) -> i64 { p.z }",
    "struct P { x: i64 }\nfn f(p: P) -> i64 { p.w }",
    "struct P { x: i64, y: i64 }\nfn f() -> P { P { x: 1, y: 2 } }",
    "struct Q { y: bool }\nfn f(q: Q) -> bool { q.y }",
    "struct P { x: i64 }\nstruct Q { y: bool }\nfn f(q: Q) { if q.y { } }",
    // ── UNKNOWN carve-outs — must NOT flag (the seed agrees on these) ──
    // A call to a fn whose return matches the declared type is clean.
    "fn okcall() -> i64 { helper() }\nfn helper() -> i64 { 0 }",
    // A call to a UNIT-returning fn is UNKNOWN in Slice 5 (unit is not a tracked
    // category), so it is never checked — the declared return can be anything.
    "fn unit_call() { noop() }\nfn noop() { }",
    // A non-primitive annotation (`Vec[i64]`) is UNKNOWN — the initializer check
    // is skipped, and the binding stays UNKNOWN (references to it not checked).
    "fn al_unknown_ann() -> i64 { let v: Vec[i64] = mk(); 0 }\nfn mk() -> Vec[i64] { Vec.new() }",
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
            "UndefinedField" => "undef-field",
            "ExtraField" => "extra-field",
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
