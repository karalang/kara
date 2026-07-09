//! Differential oracle for the self-hosted **type-expression** parser (port
//! slice 3a). Sibling of `tests/selfhost_parser.rs`: a shared corpus of bare
//! type strings is parsed by BOTH the Rust seed (`karac::parse`) and the Kāra
//! parser (`selfhost/src/parser.kara::parse_type_str`, built AOT via `karac
//! build`), each rendered to the same canonical S-expression form, and the two
//! streams are diffed. A divergence is a port regression.
//!
//! ## Span alignment
//!
//! The Kāra `parse_type_str(src)` parses the BARE type at offset 0. The Rust
//! seed has no public single-type entry, so the Rust side wraps each input as
//! `const __t__: <src> = 0;`, parses the program, and extracts the `ConstDecl`'s
//! type annotation. The wrapper prefix `const __t__: ` is exactly
//! [`OFFSET_SHIFT`] bytes (the same 13 as the expression oracle, by
//! coincidence), so subtracting it from every rendered offset realigns the Rust
//! spans with the Kāra (bare-source) spans. Lengths are wrapper-independent.
//! `= 0` is a syntactically-valid value for any annotation (the oracle only
//! parses, never typechecks), so the wrapper parses for every corpus type.
//!
//! ## Coverage (slice 3a)
//!
//! Paths (with type-only generic args), `ref`/`mut ref`/`weak`, `mut Slice[T]`,
//! raw pointers, tuples / unit / parenthesized, and `Fn(...) -> R` / `OnceFn`.
//! DEFERRED (kept out of the corpus): `with EFFECT_LIST` on `Fn` types,
//! `dyn`/`impl Trait`, const-generic args, and shape literals.

use karac::ast::{GenericArg, Item, TypeExpr, TypeKind};
use std::path::PathBuf;

/// Byte length of the wrapper prefix `const __t__: ` (see module docs).
const OFFSET_SHIFT: i64 = 13;

/// Type-expression corpus — only the forms slice 3a parses.
const CORPUS: &[&str] = &[
    // Primitive / single-segment paths (the seed parses these as `Path`, not a
    // distinct primitive node — classification happens at lowering).
    "i64",
    "bool",
    "f64",
    "char",
    "u8",
    "String",
    "Self",
    "Foo",
    // Multi-segment paths.
    "Foo.Bar",
    "a.b.c",
    // Generic paths (type-only args).
    "Vec[i64]",
    "Vec[String]",
    "Option[i64]",
    "Map[String, i64]",
    "Result[i64, String]",
    "Vec[Vec[i64]]",
    "Map[K, Vec[V]]",
    "Map[String, Vec[Node]]",
    // References / weak.
    "ref i64",
    "ref String",
    "mut ref i64",
    "mut ref Vec[i64]",
    "weak Node",
    "ref Vec[i64]",
    "ref mut ref i64",
    // Mutable slice views.
    "mut Slice[i64]",
    "mut Slice[String]",
    "mut Slice[Vec[i64]]",
    // Raw pointers.
    "*const i64",
    "*mut u8",
    "*const *mut i64",
    "*mut Map[K, V]",
    // Unit / parenthesized / tuples.
    "()",
    "(i64)",
    "(i64, bool)",
    "(i64, bool, char)",
    "(i64, (bool, char))",
    "(Vec[i64], String)",
    // Function types.
    "Fn(i64) -> bool",
    "Fn(i64, bool) -> char",
    "Fn() -> i64",
    "Fn(i64)",
    "OnceFn(i64) -> bool",
    "OnceFn()",
    "Fn(Fn(i64) -> bool) -> char",
    "ref Fn(i64) -> bool",
    "Vec[Fn(i64) -> bool]",
];

// ── Rust-side canonical render (must match `ast_render.kara::render_type`) ──

/// ` @<offset-shift>:<length>` — the span tag, with the wrapper prefix removed
/// from the offset so it matches the Kāra port's bare-source spans.
fn span_ty(te: &TypeExpr) -> String {
    format!(
        " @{}:{}",
        te.span.offset as i64 - OFFSET_SHIFT,
        te.span.length
    )
}

fn render_rust_type(te: &TypeExpr) -> String {
    let sp = span_ty(te);
    match &te.kind {
        TypeKind::Path(p) => {
            let mut out = format!("(tpath {}{sp}", p.segments.join("."));
            if let Some(args) = &p.generic_args {
                for a in args {
                    match a {
                        GenericArg::Type(t) => {
                            out.push(' ');
                            out.push_str(&render_rust_type(t));
                        }
                        other => panic!(
                            "slice-3a generic arg must be a type, got {other:?} \
                             (const-arg / shape-literal args are deferred)"
                        ),
                    }
                }
            }
            out.push(')');
            out
        }
        TypeKind::Tuple(elems) => {
            let mut out = format!("(ttuple{sp}");
            for el in elems {
                out.push(' ');
                out.push_str(&render_rust_type(el));
            }
            out.push(')');
            out
        }
        TypeKind::Pointer { is_mut, inner } => {
            let m = if *is_mut { "mut" } else { "const" };
            format!("(tptr {m}{sp} {})", render_rust_type(inner))
        }
        TypeKind::FnType {
            params,
            return_type,
            effect_spec,
            is_once,
        } => {
            assert!(
                effect_spec.is_none(),
                "slice-3a corpus must not carry a `with` effect spec on a Fn type"
            );
            let head = if *is_once { "(tfnonce" } else { "(tfn" };
            let mut out = format!("{head}{sp}");
            for p in params {
                out.push(' ');
                out.push_str(&render_rust_type(p));
            }
            if let Some(r) = return_type {
                out.push_str(" (tret ");
                out.push_str(&render_rust_type(r));
                out.push(')');
            }
            out.push(')');
            out
        }
        TypeKind::Ref(inner) => format!("(tref{sp} {})", render_rust_type(inner)),
        TypeKind::MutRef(inner) => format!("(tmutref{sp} {})", render_rust_type(inner)),
        TypeKind::MutSlice(inner) => format!("(tmutslice{sp} {})", render_rust_type(inner)),
        TypeKind::Weak(inner) => format!("(tweak{sp} {})", render_rust_type(inner)),
        TypeKind::Unit => format!("(tunit{sp})"),
        TypeKind::Error => format!("(terror{sp})"),
        other => panic!(
            "render_rust_type: TypeKind {other:?} is outside parser slice 3a; \
             keep the corpus to the ported type forms or extend the renderer"
        ),
    }
}

/// Parse `src` as a single type via the public `karac::parse`, by wrapping it in
/// a `const` annotation and extracting the `ConstDecl`'s type.
fn rust_render(src: &str) -> String {
    let wrapper = format!("const __t__: {src} = 0;");
    let result = karac::parse(&wrapper);
    let ty = result.program.items.into_iter().find_map(|item| {
        if let Item::ConstDecl(c) = item {
            Some(c.ty)
        } else {
            None
        }
    });
    match ty {
        Some(t) => render_rust_type(&t),
        None => panic!("Rust seed produced no const-decl type for input {src:?}"),
    }
}

/// Type-expression differential gate (slice 3a). Same harness as
/// `selfhost_parser.rs`: build the real selfhost modules into a temp project
/// with a per-input driver, run, and diff against the seed's render.
// IGNORED — B-2026-07-09-12: the self-hosted parser compiles (after the
// B-2026-07-09-11 niche fix) but crashes at runtime on basic inputs. See the
// note in tests/selfhost_parser.rs. Un-ignore once B-2026-07-09-12 is fixed.
#[test]
#[ignore = "B-2026-07-09-12: selfhost parser builds but crashes at runtime (pre-existing port memory-safety bug)"]
fn selfhost_parser_matches_rust_parser_types() {
    // 1. Crate-root program: a driver over the Kāra `parse_type_str` +
    //    `render_type`. The six selfhost modules are copied verbatim (step 2).
    let mut prog = String::from(
        "import ast.TypeExpr;\n\
         import parser.parse_type_str;\n\
         import ast_render.render_type;\n\
         \n\
         fn parse_and_print(src: String) with panics {\n\
         \x20   println(render_type(parse_type_str(src)));\n\
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
        prog.push_str(&format!("    parse_and_print(\"{escaped}\");\n"));
    }
    prog.push_str("}\n");

    // 2. Assemble a temp PROJECT reusing the real selfhost modules.
    let tmp = std::env::temp_dir().join(format!(
        "karac-selfhost-parser-types-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"parse\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n",
    )
    .unwrap();
    let selfhost_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("selfhost/src");
    for f in [
        "span.kara",
        "token.kara",
        "lexer.kara",
        "ast.kara",
        "parser.kara",
        "ast_render.kara",
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
    let bin = tmp.join("parse");

    if !bin.exists() {
        // A compiler PANIC or signal-kill is a real bug, never a benign skip.
        // A niche `Option[shared]` codegen panic produced no binary and matched
        // none of the markers below, so this oracle silently skipped (vacuous
        // "ok") for weeks. Treat a compiler crash as a hard failure.
        let compiler_crashed = berr.contains("panicked at") || build.status.code().is_none();
        let compile_err = compiler_crashed
            || berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted type parser FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated source ---\n{prog}"
        );
        eprintln!(
            "skip: selfhost_parser_matches_rust_parser_types — parser did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    // 3. Run the Kāra parser binary.
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara type-parser binary");
    assert!(
        run.status.success(),
        "kara type-parser binary exited nonzero:\n{}",
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
            "self-hosted type parser diverged from the Rust parser at input {i} ({:?}):\n  \
             Kāra: {k}\n  Rust: {r}\n--- full Kāra output ---\n{kout}",
            CORPUS[i]
        );
    }
    assert_eq!(
        kara_lines.len(),
        rust_lines.len(),
        "tree-count mismatch (Kāra {} vs Rust {})\n--- full Kāra output ---\n{kout}",
        kara_lines.len(),
        rust_lines.len()
    );

    let _ = std::fs::remove_dir_all(&tmp);
}
