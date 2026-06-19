//! Differential oracle for the self-hosted **parser** (port slice 1:
//! expression core). Mirrors `tests/selfhost_lexer.rs`: a shared corpus of
//! expression strings is parsed by BOTH the Rust seed (`karac::parse`) and the
//! Kāra parser (`selfhost/src/parser.kara`, built AOT via `karac build`), each
//! rendered to the same canonical S-expression form, and the two streams are
//! diffed. A divergence is a port regression.
//!
//! ## Span alignment
//!
//! The Kāra `parse_expr(src)` parses the BARE expression at offset 0. The Rust
//! seed has no public single-expression entry (`parse_expression` is
//! `pub(crate)`, and the pivot freezes the seed's feature surface), so the Rust
//! side wraps each input as `fn __e__() { <src>; }`, parses the program, and
//! extracts the body's expression statement. The wrapper prefix `fn __e__() { `
//! is exactly [`OFFSET_SHIFT`] bytes, so subtracting it from every rendered
//! offset realigns the Rust spans with the Kāra (bare-source) spans. Lengths
//! are wrapper-independent. The corpus is single-line, so the shift is a plain
//! constant subtraction.

use karac::ast::{BinOp, Expr, ExprKind, Item, StmtKind, UnaryOp};
use karac::token::{FloatSuffix, IntSuffix};
use std::path::PathBuf;

/// Byte length of the wrapper prefix `fn __e__() { ` (see module docs).
const OFFSET_SHIFT: i64 = 13;

/// Expression-core corpus — only the forms slice 1 parses (literals,
/// identifiers, `self`/`Self`, unary, binary with the full precedence ladder,
/// grouping, and tuples). Lowercase identifiers stay value-class so the seed
/// never routes them through path / struct-literal heuristics.
const CORPUS: &[&str] = &[
    // Literals.
    "1",
    "42",
    "0",
    "true",
    "false",
    "1.5",
    "2.5",
    "x",
    "foo",
    "bar_baz",
    "\"hi\"",
    "'a'",
    "b'A'",
    "self",
    "Self",
    // Numeric suffixes (L4).
    "5i32",
    "255u8",
    "1.5f64",
    "10i64",
    // Unary.
    "-5",
    "-x",
    "not true",
    "~7",
    "*p",
    "- - 3",
    // Binary — precedence ladder.
    "1 + 2",
    "1 + 2 * 3",
    "1 * 2 + 3",
    "a - b - c",
    "a + b * c - d / e",
    "a % b",
    "1 << 2 >> 3",
    "1 | 2 ^ 3 & 4",
    "a == b",
    "a != b",
    "a < b",
    "a <= b and c >= d",
    "a or b and c",
    "a and b or c",
    // Unary mixed with binary.
    "-a + b",
    "- a * b",
    "not a and b",
    // Grouping.
    "(1 + 2) * 3",
    "2 * (3 + 4) * 5",
    "(((1)))",
    "(a)",
    // Tuples.
    "()",
    "(1,)",
    "(1, 2)",
    "(a, b, c)",
    "(1 + 2, 3 * 4)",
    "(x, (y, z))",
];

// ── Rust-side canonical render (must match `ast_render.kara::render_expr`) ──

fn escape_for_render(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

fn binop_name(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::LtEq => "<=",
        BinOp::Gt => ">",
        BinOp::GtEq => ">=",
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Range => "..",
        BinOp::RangeInclusive => "..=",
    }
}

fn unaryop_name(op: &UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "not",
        UnaryOp::BitNot => "~",
        UnaryOp::Deref => "*",
    }
}

fn int_suffix_lex(s: Option<IntSuffix>) -> &'static str {
    match s {
        None => "",
        Some(IntSuffix::I8) => "i8",
        Some(IntSuffix::I16) => "i16",
        Some(IntSuffix::I32) => "i32",
        Some(IntSuffix::I64) => "i64",
        Some(IntSuffix::I128) => "i128",
        Some(IntSuffix::U8) => "u8",
        Some(IntSuffix::U16) => "u16",
        Some(IntSuffix::U32) => "u32",
        Some(IntSuffix::U64) => "u64",
        Some(IntSuffix::U128) => "u128",
    }
}

fn float_suffix_lex(s: Option<FloatSuffix>) -> &'static str {
    match s {
        None => "",
        Some(FloatSuffix::F32) => "f32",
        Some(FloatSuffix::F64) => "f64",
    }
}

/// ` @<offset-shift>:<length>` — the span tag, with the wrapper prefix removed
/// from the offset so it matches the Kāra port's bare-source spans.
fn span_str(e: &Expr) -> String {
    format!(
        " @{}:{}",
        e.span.offset as i64 - OFFSET_SHIFT,
        e.span.length
    )
}

fn render_rust_expr(e: &Expr) -> String {
    let sp = span_str(e);
    match &e.kind {
        ExprKind::Integer(v, sfx) => {
            let lex = int_suffix_lex(*sfx);
            if lex.is_empty() {
                format!("(int {v}{sp})")
            } else {
                format!("(int {v} {lex}{sp})")
            }
        }
        ExprKind::Float(v, sfx) => {
            let lex = float_suffix_lex(*sfx);
            if lex.is_empty() {
                format!("(float {v}{sp})")
            } else {
                format!("(float {v} {lex}{sp})")
            }
        }
        ExprKind::Bool(b) => format!("(bool {b}{sp})"),
        ExprKind::CharLit(c) => format!("(char {}{sp})", escape_for_render(&c.to_string())),
        ExprKind::ByteLit(b) => format!("(byte {b}{sp})"),
        ExprKind::StringLit(s) => format!("(str {}{sp})", escape_for_render(s)),
        ExprKind::MultiStringLit(s) => format!("(mstr {}{sp})", escape_for_render(s)),
        ExprKind::Identifier(name) => format!("(ident {name}{sp})"),
        ExprKind::SelfValue => format!("(self{sp})"),
        ExprKind::SelfType => format!("(Self{sp})"),
        ExprKind::Binary { op, left, right } => format!(
            "(binary {}{sp} {} {})",
            binop_name(op),
            render_rust_expr(left),
            render_rust_expr(right)
        ),
        ExprKind::Unary { op, operand } => {
            format!(
                "(unary {}{sp} {})",
                unaryop_name(op),
                render_rust_expr(operand)
            )
        }
        ExprKind::Tuple(elems) => {
            let mut s = format!("(tuple{sp}");
            for el in elems {
                s.push(' ');
                s.push_str(&render_rust_expr(el));
            }
            s.push(')');
            s
        }
        other => panic!(
            "render_rust_expr: ExprKind {other:?} is outside parser slice 1; \
             keep the corpus to expression-core forms or extend the renderer"
        ),
    }
}

/// Parse `src` as a single expression via the public `karac::parse`, by wrapping
/// it in a function body and extracting the expression statement.
fn rust_render(src: &str) -> String {
    let wrapper = format!("fn __e__() {{ {src}; }}");
    let result = karac::parse(&wrapper);
    let expr = result.program.items.into_iter().find_map(|item| {
        if let Item::Function(f) = item {
            f.body.stmts.into_iter().find_map(|s| {
                if let StmtKind::Expr(e) = s.kind {
                    Some(e)
                } else {
                    None
                }
            })
        } else {
            None
        }
    });
    match expr {
        Some(e) => render_rust_expr(&e),
        None => panic!("Rust seed produced no expression statement for input {src:?}"),
    }
}

// IGNORED pending phase-12 codegen blocker #32 (self-field-rooted Vec-index
// reads, `self.tokens[self.pos]…`, are miscompiled — see
// docs/implementation_checklist/phase-12-self-hosting.md "Parser pre-port:
// codegen blockers"). The parser (`selfhost/src/parser.kara`) is code-complete
// in its target clone-free shape but cannot compile until #32 lands; this oracle
// is the gate for parser slice 1 the moment it does. Remove `#[ignore]` then.
#[test]
#[ignore = "blocked on phase-12 codegen blocker #32 (self-field Vec-index miscompile)"]
fn selfhost_parser_matches_rust_parser() {
    // 1. Generate the crate-root program: imports of the Kāra parser +
    //    renderer, a per-input driver, and `main`. The `span`/`token`/`lexer`/
    //    `ast`/`parser`/`ast_render` modules are copied verbatim into the temp
    //    project (step 2), exercising the real multi-file layout.
    let mut prog = String::from(
        "import ast.Expr;\n\
         import parser.parse_expr;\n\
         import ast_render.render_expr;\n\
         \n\
         fn parse_and_print(src: String) with panics {\n\
         \x20   println(render_expr(parse_expr(src)));\n\
         }\n\
         fn main() {\n",
    );
    for input in CORPUS {
        // Same escaping as the lexer oracle: backslash first (doubles existing
        // backslashes), then quote and the control chars.
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
    let tmp = std::env::temp_dir().join(format!("karac-selfhost-parser-{}", std::process::id()));
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
        let compile_err = berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted parser FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated source ---\n{prog}"
        );
        eprintln!(
            "skip: selfhost_parser_matches_rust_parser — parser did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    // 3. Run the Kāra parser binary.
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara parser binary");
    assert!(
        run.status.success(),
        "kara parser binary exited nonzero:\n{}",
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
            "self-hosted parser diverged from the Rust parser at input {i} ({:?}):\n  \
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
