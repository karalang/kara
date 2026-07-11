//! Differential oracle for the self-hosted **resolver** (Phase 12, Resolver
//! port Slices 1–2c). Sibling of `tests/selfhost_parser{,_items,_types}.rs`:
//! a shared corpus of bare top-level items is name-resolved by BOTH the Rust
//! seed (`karac::resolve(karac::parse(src).program)`) and the Kāra resolver
//! (`selfhost/src/resolver.kara::resolve_item`, built AOT via `karac build`),
//! each rendered to the same canonical form — the ordered list of
//! `(kind @offset:length)` tuples — and the two streams are diffed.
//!
//! ## Scope + corpus discipline
//!
//! - **Slice 1** resolves a single `fn`: generics (bare params), `self`,
//!   params + types, return type, and the body (Let/Assign/expr statements;
//!   the reachable `Expr` forms; block/if/loop/match scopes).
//! - **Slice 2a** adds the type/value DECLARATION items — `struct`, `enum`,
//!   `type` alias, and `const` — each resolved in fused "collect + resolve"
//!   form (the declaration's own name is defined FIRST, so a self-referential
//!   field / variant / const value resolves like the seed's two-pass
//!   `collect` + `resolve_items` does for a single item).
//! - **Slice 2b** adds `trait` and `impl`: a trait scope exposing `Self` + the
//!   trait's generics over each method's signature / optional default body; an
//!   impl scope resolving the target type, then the (optional) trait path, then
//!   `Self`, then each method body. Impl-method names are NOT scope bindings
//!   (siblings dispatch via `self.m()`), mirroring the seed. Operator-trait /
//!   `Into` impl restrictions produce out-of-slice error kinds, so the corpus
//!   avoids those trait names as impl targets. `use a.b.c;` binds the last path
//!   segment `c` (`collect_use`), so a reference to it resolves and a repeated
//!   import is a duplicate — the standalone `resolve()` validates no path.
//! - **Slice 2c** seeds the FULL prelude — the 16 primitives plus every
//!   `PRELUDE_{FUNCTIONS,TYPES,TRAITS,VARIANTS,EFFECT_RESOURCES}` name and the
//!   magic modules / comptime pseudotypes / stdlib module aliases the seed's
//!   `register_primitives` registers — so the corpus may now reference real
//!   prelude names (`Vec` / `Option` / `println` / `Some` / `process` / ...)
//!   exactly as the seed resolves them (a typo of one still surfaces as
//!   undefined; a user definition shadows the line-0 prelude entry).
//!
//! Only four error kinds are in scope — UndefinedName, UndefinedType,
//! DuplicateDefinition, ReservedIdentifier. The corpus must therefore carry no
//! attributes (the Rust `resolve()` runs attribute validation the Kāra side
//! does not) and exercise duplicate / reserved definitions only through GENERIC
//! params — whose span is the bare identifier. A `FnParamNode` /
//! `StructFieldNode` has no separate name span (its span covers `name: TYPE`),
//! so a duplicate / reserved PARAM or FIELD would diverge on span (a name span
//! is a later slice's work) — the type/value declarations therefore exercise
//! dup/reserved only via generics.
//!
//! The Rust seed asserts every produced error is one of the four in-scope
//! kinds, so a corpus entry that drifts out of the slice fails loudly rather
//! than silently diffing clean.

use karac::resolver::ResolveErrorKind;
use std::path::PathBuf;

/// Bare top-level items — the forms Slices 1–2b resolve (`fn` / `struct` /
/// `enum` / `type` / `const` / `trait` / `impl`). Names come from the 16
/// primitives + the full seeded prelude (Slice 2c) + generics + self-refs; no
/// attributes.
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
    // ── Slice 2a: type/value declaration items ──
    // Structs — clean, generic, self-referential, tuple field.
    "struct Unit {}",
    "struct Point { x: i64, y: i64 }",
    "struct Wrap[T] { val: T }",
    "struct SelfRef { next: SelfRef }",
    "struct TupField { p: (i64, bool) }",
    // Struct undefined field type.
    "struct BadField { x: Nope }",
    "struct TwoField { a: i64, b: Missing }",
    // Struct generic dup / reserved (name-only span).
    "struct DupGen[T, T] {}",
    "struct ResGen[Fn] {}",
    // Enums — unit / tuple / struct variants, generic, undefined payloads.
    "enum Dir { North, South }",
    "enum MyOpt[T] { Nil, Held(T) }",
    "enum Mixed { Plain, Pair(i64, bool), Named { f: i64 } }",
    "enum BadTuple { V(Nope) }",
    "enum BadStructV { V { f: Missing } }",
    "enum EnumDupGen[T, T] { A }",
    // Type aliases — clean, tuple, generic, undefined target, reserved param.
    "type MyInt = i64;",
    "type Pair = (i64, bool);",
    "type Ident[T] = T;",
    "type BadAlias = Nope;",
    "type ResAlias[Fn] = i64;",
    // Consts — clean, undefined type, self-referential value, undefined value.
    "const N: i64 = 42;",
    "const BadTy: Nope = 0;",
    "const Recur: i64 = Recur;",
    "const BadVal: i64 = missing;",
    // ── Slice 2b: trait / impl items ──
    // Traits — required method, default body, `Self` return, generic, bad
    // param type, duplicate generic.
    "trait Greet { fn hi(ref self) -> i64; }",
    "trait Def { fn base(ref self) -> i64 { 0 } }",
    "trait Mk { fn make() -> Self; }",
    "trait Con[T] { fn get(ref self) -> T; }",
    "trait BadParam { fn m(x: Nope); }",
    "trait TDupGen[T, T] {}",
    // Inherent impls on a seeded primitive target — clean, `Self` return +
    // `self` body, undefined param type; plus undefined-target and (undefined)
    // trait-path forms.
    "impl i64 { fn dbl(ref self) -> i64 { 0 } }",
    "impl i64 { fn id(ref self) -> Self { self } }",
    "impl String { fn two(ref self) -> i64 { 0 } }",
    "impl i64 { fn m(x: Missing) {} }",
    "impl Nope { fn m() {} }",
    "impl Show for i64 { fn show(ref self) -> i64 { 0 } }",
    // ── Slice 2c: full prelude seeding ──
    // Prelude functions / types / variants / modules now resolve like the seed.
    "fn uses_println() { println(\"hi\") }",
    "fn uses_panic() { panic(\"boom\") }",
    "fn uses_vec(v: Vec[i64]) {}",
    "fn uses_map(m: Map[String, i64]) {}",
    "fn ret_option() -> Option[i64] { None }",
    "fn uses_some() { let x = Some(5); x }",
    "fn uses_result() -> Result[i64, bool] { Ok(1) }",
    "fn uses_module() { process.exit(0) }",
    "type AliasTrait = Display;",
    // User definition shadows a prelude type (line-0 carve-out) — no dup.
    "struct Vec {}",
    // Typos of prelude names still surface as undefined (seeding is exact).
    "fn typo_fn() { printlnn(\"x\") }",
    "fn typo_ty(x: Veec) {}",
    // ── use imports — bind the last path segment (no module graph to validate
    // the path in isolation, so a bare `use` is clean).
    "use foo.bar;",
    "use a.b.c;",
    // ── match-arm pattern BINDINGS ── the arm pattern binds its variables into
    // the arm scope (and resolves any variant path) before the guard/body. A
    // prelude variant path (`Some`) resolves; a bare uppercase pattern (`None`)
    // is a fresh binding; a tuple pattern binds each element; an undefined
    // variant-constructor path is UndefinedName at the pattern span.
    "fn f(o: Option[i64]) -> i64 { match o { Some(v) => v, None => 0 } }",
    "fn f(o: Option[i64]) -> i64 { match o { Some(v) if v > 0 => v, _ => 0 } }",
    "fn f(p: (i64, i64)) -> i64 { match p { (a, b) => a } }",
    "fn f(p: (i64, i64)) -> i64 { match p { (a, b) => a + b } }",
    "fn f(e: i64) -> i64 { match e { Undef(x) => x, _ => 0 } }",
    "fn f(o: Option[i64]) -> i64 { match o { Some(Some(v)) => v, _ => 0 } }",
    // ── Struct literals ── A single-item body can only reference an UNDEFINED
    // struct name (no cross-item def), so the type name surfaces as
    // UndefinedName at the WHOLE struct-literal span (the seed's
    // `error_undefined_name`); field values / spread resolve independently and
    // AFTER the name.
    "fn mk() { Undef { x: 1 } }",
    "fn mk(a: i64) { Undef { x: a } }",
    "fn mk() { Undef { x: missing } }",
    "fn mk() { Undef {} }",
    "fn mk(x: i64) { Undef { x } }",
    "fn mk(b: i64) { Undef { x: 1, ..b } }",
    "fn mk() { Undef { x: 1, ..missing } }",
    // ── Path expressions ── The first segment is resolved (UndefinedName at the
    // whole path span on miss); prelude-seeded roots (`Vec`, `Map`) are clean,
    // undefined roots error. Both the bare-path and `Path(args)` call forms.
    "fn mk() { Vec.new() }",
    "fn mk() { Map.new() }",
    "fn bad() { Undef.make() }",
    "fn bad() { Nope.Variant }",
    "fn mk(x: i64) { Vec.new().push(x) }",
];

/// Multi-item programs for the program-level (two-pass) gate. These exercise
/// what a single item cannot: cross-item FORWARD references (a name used before
/// its declaration, resolved because pass-1 collect registered it) and
/// top-level DuplicateDefinition. Same discipline as `CORPUS` — only seeded
/// prelude names + generics + cross-item user names, no attributes.
const PROGRAM_CORPUS: &[&str] = &[
    // Empty program.
    "",
    // Forward call: `a` calls `b` declared later.
    "fn a() { b() }\nfn b() {}",
    // Forward TYPE reference: `f`'s param is a struct declared later.
    "fn f(x: Point) {}\nstruct Point { x: i64 }",
    // Mutually-referential type declarations.
    "struct A { b: B }\nstruct B { a: i64 }",
    // Forward const / type-alias / enum references from a function.
    "fn f() -> i64 { N }\nconst N: i64 = 5;",
    "fn f(x: Id) {}\ntype Id = i64;",
    "fn f(e: E) {}\nenum E { X, Y }",
    // Inherent impl on a struct declared in the same program.
    "struct P { x: i64 }\nimpl P { fn get(ref self) -> i64 { self.x } }",
    // Several clean items together.
    "struct Point { x: i64, y: i64 }\nfn dist(p: Point) -> i64 { 0 }\nconst ZERO: i64 = 0;",
    // Struct literal constructing a struct defined in the same program — clean,
    // forward and backward, with field-value / spread resolution.
    "struct Point { x: i64 }\nfn mk() -> Point { Point { x: 1 } }",
    "fn mk() -> Point { Point { x: 1 } }\nstruct Point { x: i64 }",
    "struct Point { x: i64 }\nfn mk(v: i64) -> Point { Point { x: v } }",
    "struct Point { x: i64 }\nfn upd(base: Point) -> Point { Point { x: 1, ..base } }",
    "struct Inner { v: i64 }\nstruct Outer { i: Inner }\nfn mk() -> Outer { Outer { i: Inner { v: 0 } } }",
    // Struct literal on an UNDEFINED type, cross-item — UndefinedName at the
    // struct-literal span; the sibling item is clean.
    "fn ok_one() {}\nfn mk() { Undef { x: 1 } }",
    // Field value referencing an undefined name — the type resolves, the value
    // does not (error AFTER the clean name lookup).
    "struct Point { x: i64 }\nfn mk() -> Point { Point { x: missing } }",
    // Path expression resolving a user type/variant declared in the same
    // program — clean, forward and backward; and an associated-fn call form.
    "enum Token { Error, Ident }\nfn f() { Token.Error }",
    "fn f() { E.V }\nenum E { V }",
    "struct S { x: i64 }\nfn mk() -> S { S.new() }",
    // Top-level DuplicateDefinition — two functions with the same name.
    "fn dup() {}\nfn dup() {}",
    // Duplicate across item KINDS — a fn and a struct sharing a name.
    "fn thing() {}\nstruct thing {}",
    // Duplicate struct.
    "struct S { a: i64 }\nstruct S { b: bool }",
    // Undefined reference in one item; the other is clean (traversal order).
    "fn ok_one() {}\nfn bad() { missing() }",
    // Undefined type referenced across items where NO declaration supplies it.
    "fn f(x: Undeclared) {}\nfn g() {}",
    // `use` binds the imported name — a call to it resolves, forward or back.
    "use foo.bar;\nfn f() { bar() }",
    "fn f() { bar() }\nuse foo.bar;",
    // Enum variant names are registered globally (the seed's `collect_enum`), so
    // a variant resolves as a value, as a pattern constructor path, and forward.
    "enum E { A(i64), B }\nfn f(e: E) -> i64 { match e { A(x) => x, B => 0 } }",
    "enum E { A, B }\nfn f() -> E { A }",
    "enum E { A(i64), B(i64) }\nfn f(e: E) -> i64 { match e { A(x) => x, B(x) => x } }",
    "fn f(e: E) -> i64 { match e { A(x) => x, _ => 0 } }\nenum E { A(i64), B }",
    // Struct patterns bind their fields (shorthand + explicit + `..` rest) in
    // both `let` destructuring and `match` arms; an undefined constructor path
    // is UndefinedName at the pattern span.
    "struct Point { x: i64, y: i64 }\nfn f(p: Point) -> i64 { let Point { x, y } = p; x + y }",
    "struct Point { x: i64, y: i64 }\nfn f(p: Point) -> i64 { match p { Point { x: a, y: b } => a + b } }",
    "struct P { a: i64, b: i64 }\nfn f(p: P) -> i64 { let P { a, .. } = p; a }",
    "fn f(p: i64) -> i64 { match p { Undef { x } => x, _ => 0 } }",
    // A `use` import colliding with a declaration of the same name — dup.
    "use foo.thing;\nfn thing() {}",
    // Two imports of the same last segment — dup.
    "use a.dup;\nuse b.dup;",
    // ── `import` declarations ── single form binds the last segment; the braced
    // group binds every listed item (with `as` aliases). Single-file mode binds
    // without validating the module prefix, matching `karac::resolve`.
    "import span.Span;\nfn f(x: Span) {}",
    "import token.{Token, SpannedToken};\nfn f(t: Token) -> SpannedToken { t }",
    "import m.{A, B, C};\nfn f(a: A, b: B, c: C) {}",
    "import m.{X, Y};\nfn f() { Z }",
    "import m.{One as Two};\nfn f(x: Two) {}",
    "import m.Dup;\nfn Dup() {}",
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
/// run the resulting binary, and return its stdout lines — or `None` on a
/// benign skip (no llvm feature / missing runtime archive). A compiler
/// PANIC / error is a hard failure (a port regression), never a skip.
fn build_and_run_driver(tag: &str, driver: &str) -> Option<Vec<String>> {
    let tmp = std::env::temp_dir().join(format!(
        "karac-selfhost-resolver-{tag}-{}",
        std::process::id()
    ));
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
    std::fs::write(tmp.join("src").join("main.kara"), driver).unwrap();

    let build = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .expect("spawn karac build");
    let berr = String::from_utf8_lossy(&build.stderr);
    let bin = tmp.join("resolve");

    if !bin.exists() {
        let compiler_crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = compiler_crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted resolver FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated driver ---\n{driver}"
        );
        eprintln!(
            "skip: selfhost resolver oracle [{tag}] — did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return None;
    }

    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara resolver binary");
    assert!(
        run.status.success(),
        "kara resolver binary exited nonzero:\n{}",
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

/// Resolver differential gate (Slices 1–2c). Same harness as the parser
/// oracles: build the real selfhost modules + resolver into a temp project with
/// a per-input driver over `parse_item_str` + `resolve_item`, run, and diff each
/// line against the Rust seed's render.
#[test]
fn selfhost_resolver_matches_rust_resolver() {
    let mut driver = String::from(
        "import parser.parse_item_str;\n\
         import resolver.{resolve_item, render_errors};\n\
         \n\
         fn check(src: String) with panics {\n\
         \x20   println(render_errors(resolve_item(parse_item_str(src))));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        driver.push_str(&format!("    check(\"{}\");\n", kara_str_lit(input)));
    }
    driver.push_str("}\n");

    let Some(kara_lines) = build_and_run_driver("item", &driver) else {
        return;
    };
    let rust_lines: Vec<String> = CORPUS.iter().map(|input| rust_render(input)).collect();
    diff_or_panic(CORPUS, &kara_lines, &rust_lines);
}

/// Program-level resolver differential gate (multi-item programs). Exercises the
/// TWO-PASS resolver (`parse_program` + `resolve_program`): PASS-1 collection
/// registers every top-level name, so cross-item FORWARD references resolve and
/// a repeated top-level name is a DuplicateDefinition — none of which the
/// single-item gate above can reach. The Rust seed's `karac::resolve` is already
/// program-level, so the same source diffs directly.
#[test]
fn selfhost_resolver_program_matches_rust_resolver() {
    let mut driver = String::from(
        "import parser.parse_program;\n\
         import resolver.{resolve_program, render_errors};\n\
         \n\
         fn check(src: String) with panics {\n\
         \x20   println(render_errors(resolve_program(parse_program(src))));\n\
         }\n\
         fn main() {\n",
    );
    for input in PROGRAM_CORPUS {
        driver.push_str(&format!("    check(\"{}\");\n", kara_str_lit(input)));
    }
    driver.push_str("}\n");

    let Some(kara_lines) = build_and_run_driver("program", &driver) else {
        return;
    };
    let rust_lines: Vec<String> = PROGRAM_CORPUS
        .iter()
        .map(|input| rust_render(input))
        .collect();
    diff_or_panic(PROGRAM_CORPUS, &kara_lines, &rust_lines);
}

/// Diff the Kāra resolver's per-input output against the Rust seed's, panicking
/// with context on the first divergence or a line-count mismatch.
fn diff_or_panic(corpus: &[&str], kara_lines: &[String], rust_lines: &[String]) {
    if let Some((i, (k, r))) = kara_lines
        .iter()
        .zip(rust_lines.iter())
        .enumerate()
        .find(|(_, (k, r))| k != r)
    {
        panic!(
            "self-hosted resolver diverged from the Rust resolver at input {i} ({:?}):\n  \
             Kāra: {k}\n  Rust: {r}",
            corpus[i]
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
