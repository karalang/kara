//! Differential oracle for the self-hosted **item** parser (port slice 3c).
//! Sibling of `tests/selfhost_parser{,_types}.rs`: a shared corpus of bare
//! top-level item strings is parsed by BOTH the Rust seed (`karac::parse`) and
//! the Kāra parser (`selfhost/src/parser.kara::parse_item_str`, built AOT via
//! `karac build`), each rendered to the same canonical S-expression form, and
//! the two streams are diffed. A divergence is a port regression.
//!
//! ## Span alignment
//!
//! Items are top-level, so — unlike the expression / type oracles — there is NO
//! wrapper: the Kāra `parse_item_str(src)` and the Rust `karac::parse(src)`
//! parse the SAME bare source, and the span offset shift is 0. Spans line up
//! directly. ([`OFFSET_SHIFT`] is kept as a named 0 for symmetry with the
//! sibling oracles.)
//!
//! ## Coverage (slice 3c-i)
//!
//! The leaf declarations whose sub-grammar is already ported (`parse_type` +
//! `parse_expr`): `use`, `const`, and the no-generics `type` alias. DEFERRED to
//! later 3c increments (kept out of the corpus): struct/enum (3c-ii), fn
//! (3c-iii), trait/impl (3c-iv), generic params (3c-v); and entirely out of
//! scope for now — attributes, doc comments, `private`, effects/contracts,
//! `where` refinements, distinct types, layout, extern/host, unions, module
//! bindings, imports, test cases.

use karac::ast::{
    BinOp, Block, Expr, ExprKind, GenericArg, Item, Param, PatternKind, SelfParam, Stmt, StmtKind,
    TypeExpr, TypeKind, UnaryOp,
};
use karac::token::{FloatSuffix, IntSuffix};
use std::path::PathBuf;

/// Byte offset shift between the Rust and Kāra spans — 0 (no wrapper).
const OFFSET_SHIFT: i64 = 0;

/// Item corpus — only the forms slice 3c-i parses. Const-class names stay
/// upper-case and type-alias names PascalCase so the seed never trips its
/// naming-convention diagnostics; const *values* stay literal/identifier-simple
/// so the Rust-side expr renderer below can stay compact.
const CORPUS: &[&str] = &[
    // `use` declarations.
    "use foo;",
    "use a.b.c;",
    "use a.b.c.d.e;",
    "pub use std.io;",
    // `const` declarations — primitive values.
    "const MAX: i64 = 100;",
    "const ZERO: i64 = 0;",
    "pub const LIMIT: u8 = 255;",
    "const PI: f64 = 3.5;",
    "const FLAG: bool = true;",
    "const GREETING: String = \"hi\";",
    "const TYPED: i32 = 5i32;",
    // `const` declarations — compound values + richer types.
    "const NEG: i64 = -5;",
    "const SUM: i64 = 1 + 2;",
    "const POLY: i64 = 2 * 3 + 1;",
    "const HANDLER: Fn(i64) -> bool = f;",
    "const TABLE: Vec[i64] = v;",
    // `type` aliases (no generics).
    "type Bytes = Vec[u8];",
    "pub type Pair = (i64, bool);",
    "type Handler = Fn(i64) -> bool;",
    "type IntRef = ref i64;",
    "type Ptr = *const u8;",
    "type Nested = Map[String, Vec[i64]];",
    "type Nothing = ();",
    // `struct` definitions (no generics).
    "struct Empty {}",
    "struct Point { x: i64, y: i64 }",
    "pub struct Named { pub name: String, age: i64 }",
    "struct Mutable { mut count: i64 }",
    "struct Mixed { pub mut head: i64, tail: Vec[i64] }",
    "shared struct Node { value: i64, next: Option[Node] }",
    "par struct Counter { mut hits: Atomic[i64] }",
    "struct Trailing { a: i64, b: bool, }",
    "struct Refs { left: ref i64, owner: Box[String] }",
    // `enum` definitions (no generics).
    "enum Color { Red, Green, Blue }",
    "enum Shape { Circle(i64), Rect(i64, i64) }",
    "enum Mixed2 { Unit, Pair(i64, bool), Named { x: i64, y: i64 } }",
    "pub enum Opt { Some(i64), None }",
    "shared enum Tree { Leaf(i64), Branch(Tree, Tree) }",
    "par enum Msg { Ping, Data(Vec[u8]) }",
    "enum One { Only }",
    "enum WithTrailing { A, B, }",
    // `fn` definitions (no generics / effects / contracts). Bodies stay within
    // the slice-2 expr surface (literals / ident / unary / binary / tuple as
    // tail or let/expr-stmt values) so the items oracle's compact expr renderer
    // suffices.
    "fn noop() {}",
    "fn answer() -> i64 { 42 }",
    "pub fn id(x: i64) -> i64 { x }",
    "fn add(a: i64, b: i64) -> i64 { a + b }",
    "fn make_pair(x: i64, y: bool) -> (i64, bool) { (x, y) }",
    "fn takes_ref(s: ref String) {}",
    "fn many(a: i64, b: String, c: Vec[i64]) {}",
    "fn trailing(a: i64,) {}",
    "fn with_let() -> i64 { let y = 1; y }",
    "fn stmt_only() { x; }",
    "fn ret_named() -> String { greeting }",
    "fn higher(f: Fn(i64) -> bool) {}",
    // Receiver forms (the parser accepts a receiver on any fn; self-validity is
    // a resolver check, not a parse error).
    "fn consume(self) {}",
    "fn read(ref self) -> i64 { 0 }",
    "fn write(mut ref self, n: i64) {}",
    "fn read_arg(ref self, x: i64) -> i64 { x }",
];

// ── Rust-side canonical render (must match `ast_render.kara`) ──

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

/// ` @<offset>:<length>` for an expression span (offset realigned by the 0
/// shift).
fn span_expr(e: &Expr) -> String {
    format!(
        " @{}:{}",
        e.span.offset as i64 - OFFSET_SHIFT,
        e.span.length
    )
}

/// Compact expression render covering the const-value forms in the corpus
/// (literals / identifiers / unary / binary). Must match
/// `ast_render.kara::render_expr` for these arms; richer forms panic so the
/// corpus stays within the slice.
fn render_rust_expr(e: &Expr) -> String {
    let sp = span_expr(e);
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
        ExprKind::Unary { op, operand } => {
            format!(
                "(unary {}{sp} {})",
                unaryop_name(op),
                render_rust_expr(operand)
            )
        }
        ExprKind::Binary { op, left, right } => format!(
            "(binary {}{sp} {} {})",
            binop_name(op),
            render_rust_expr(left),
            render_rust_expr(right)
        ),
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
            "render_rust_expr: ExprKind {other:?} is outside the slice-3c const-value \
             surface; keep const values literal/identifier-simple or extend the renderer"
        ),
    }
}

/// ` @<offset>:<length>` for a type span.
fn span_ty(te: &TypeExpr) -> String {
    format!(
        " @{}:{}",
        te.span.offset as i64 - OFFSET_SHIFT,
        te.span.length
    )
}

/// Type render — must match `ast_render.kara::render_type` (copied from
/// `selfhost_parser_types.rs`).
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
                            "slice-3c generic arg must be a type, got {other:?} \
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
                "slice-3c corpus must not carry a `with` effect spec on a Fn type"
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
            "render_rust_type: TypeKind {other:?} is outside parser slice 3c; \
             keep the corpus to the ported type forms or extend the renderer"
        ),
    }
}

/// ` @<offset>:<length>` for an item span.
fn span_item_off_len(off: usize, len: usize) -> String {
    format!(" @{}:{}", off as i64 - OFFSET_SHIFT, len)
}

fn vis(is_pub: bool) -> &'static str {
    if is_pub {
        " pub"
    } else {
        ""
    }
}

/// ` pub`/` shared`/` par` struct/enum modifier flags — must match
/// `ast_render.kara::render_type_mods`.
fn type_mods(is_pub: bool, is_shared: bool, is_par: bool) -> String {
    let mut s = String::new();
    if is_pub {
        s.push_str(" pub");
    }
    if is_shared {
        s.push_str(" shared");
    }
    if is_par {
        s.push_str(" par");
    }
    s
}

/// `(field[ pub][ mut] NAME<span> TYPE)` — must match
/// `ast_render.kara::render_struct_field`.
fn render_rust_struct_field(f: &karac::ast::StructField) -> String {
    let mut out = String::from("(field");
    if f.is_pub {
        out.push_str(" pub");
    }
    if f.is_mut {
        out.push_str(" mut");
    }
    out.push(' ');
    out.push_str(&f.name);
    out.push_str(&span_item_off_len(f.span.offset, f.span.length));
    out.push(' ');
    out.push_str(&render_rust_type(&f.ty));
    out.push(')');
    out
}

/// `(variant NAME<span>[ (vtuple ...)|(vstruct ...)])` — must match
/// `ast_render.kara::render_variant`.
fn render_rust_variant(v: &karac::ast::Variant) -> String {
    use karac::ast::VariantKind;
    let mut out = format!(
        "(variant {}{}",
        v.name,
        span_item_off_len(v.span.offset, v.span.length)
    );
    match &v.kind {
        VariantKind::Unit => {}
        VariantKind::Tuple(types) => {
            out.push_str(" (vtuple");
            for t in types {
                out.push(' ');
                out.push_str(&render_rust_type(t));
            }
            out.push(')');
        }
        VariantKind::Struct(fields) => {
            out.push_str(" (vstruct");
            for f in fields {
                out.push(' ');
                out.push_str(&render_rust_struct_field(f));
            }
            out.push(')');
        }
    }
    out.push(')');
    out
}

/// ` @<offset>:<length>` for a statement / block span.
fn span_tag(off: usize, len: usize) -> String {
    format!(" @{}:{}", off as i64 - OFFSET_SHIFT, len)
}

/// Statement render — must match `ast_render.kara::render_stmt`.
fn render_rust_stmt(s: &Stmt) -> String {
    let sp = span_tag(s.span.offset, s.span.length);
    match &s.kind {
        StmtKind::Let {
            is_mut,
            pattern,
            value,
            ..
        } => {
            let name = match &pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                other => {
                    panic!("slice-3c fn-body let pattern must be a plain binding, got {other:?}")
                }
            };
            let m = if *is_mut { " mut" } else { "" };
            format!("(let{m} {name}{sp} {})", render_rust_expr(value))
        }
        StmtKind::Assign { target, value } => format!(
            "(assign{sp} {} {})",
            render_rust_expr(target),
            render_rust_expr(value)
        ),
        StmtKind::Expr(e) => format!("(exprstmt{sp} {})", render_rust_expr(e)),
        other => panic!(
            "render_rust_stmt: StmtKind {other:?} is outside the slice-3c fn-body \
             surface; keep bodies to let/expr/assign statements or extend the renderer"
        ),
    }
}

/// Block render — must match `ast_render.kara::render_block`.
fn render_rust_block(b: &Block) -> String {
    let mut out = format!("(block{}", span_tag(b.span.offset, b.span.length));
    for s in &b.stmts {
        out.push(' ');
        out.push_str(&render_rust_stmt(s));
    }
    if let Some(tail) = &b.final_expr {
        out.push_str(" (tail ");
        out.push_str(&render_rust_expr(tail));
        out.push(')');
    }
    out.push(')');
    out
}

/// ` self`/` refself`/` mutrefself` receiver flag — must match
/// `ast_render.kara::render_self_mode`.
fn render_self_mode(sp: &Option<SelfParam>) -> &'static str {
    match sp {
        None => "",
        Some(SelfParam::Owned) => " self",
        Some(SelfParam::Ref) => " refself",
        Some(SelfParam::MutRef) => " mutrefself",
    }
}

/// `(param NAME<span> TYPE)` — must match `ast_render.kara::render_fn_param`.
fn render_rust_fn_param(p: &Param) -> String {
    let name = match &p.pattern.kind {
        PatternKind::Binding(n) => n.clone(),
        other => panic!("slice-3c-iii param must be a plain binding, got {other:?}"),
    };
    assert!(
        p.default_value.is_none(),
        "slice-3c-iii fn corpus must not carry default parameter values"
    );
    format!(
        "(param {}{} {})",
        name,
        span_item_off_len(p.span.offset, p.span.length),
        render_rust_type(&p.ty)
    )
}

/// Item render — must match `ast_render.kara::render_item`.
fn render_rust_item(item: &Item) -> String {
    match item {
        Item::UseDecl(u) => {
            format!(
                "(use{}{} {})",
                vis(u.is_pub),
                span_item_off_len(u.span.offset, u.span.length),
                u.path.join(".")
            )
        }
        Item::ConstDecl(c) => {
            format!(
                "(const{} {}{} {} {})",
                vis(c.is_pub),
                c.name,
                span_item_off_len(c.span.offset, c.span.length),
                render_rust_type(&c.ty),
                render_rust_expr(&c.value)
            )
        }
        Item::TypeAlias(t) => {
            assert!(
                t.generic_params.is_none(),
                "slice-3c-i type-alias corpus must not carry generic params"
            );
            format!(
                "(typealias{} {}{} {})",
                vis(t.is_pub),
                t.name,
                span_item_off_len(t.span.offset, t.span.length),
                render_rust_type(&t.ty)
            )
        }
        Item::StructDef(s) => {
            let mut out = format!(
                "(struct{} {}{}",
                type_mods(s.is_pub, s.is_shared, s.is_par),
                s.name,
                span_item_off_len(s.span.offset, s.span.length)
            );
            for f in &s.fields {
                out.push(' ');
                out.push_str(&render_rust_struct_field(f));
            }
            out.push(')');
            out
        }
        Item::EnumDef(e) => {
            let mut out = format!(
                "(enum{} {}{}",
                type_mods(e.is_pub, e.is_shared, e.is_par),
                e.name,
                span_item_off_len(e.span.offset, e.span.length)
            );
            for v in &e.variants {
                out.push(' ');
                out.push_str(&render_rust_variant(v));
            }
            out.push(')');
            out
        }
        Item::Function(f) => {
            assert!(
                f.generic_params.is_none(),
                "slice-3c-iii fn corpus must not carry generic params"
            );
            assert!(
                f.effects.is_none()
                    && f.requires.is_empty()
                    && f.ensures.is_empty()
                    && f.where_clause.is_none()
                    && !f.is_unsafe
                    && !f.is_comptime,
                "slice-3c-iii fn corpus must not carry effects / contracts / where / \
                 unsafe / comptime"
            );
            let mut out = format!(
                "(fn{} {}{}{} (params",
                vis(f.is_pub),
                f.name,
                span_item_off_len(f.span.offset, f.span.length),
                render_self_mode(&f.self_param),
            );
            for p in &f.params {
                out.push(' ');
                out.push_str(&render_rust_fn_param(p));
            }
            out.push(')');
            if let Some(r) = &f.return_type {
                out.push_str(" (ret ");
                out.push_str(&render_rust_type(r));
                out.push(')');
            }
            out.push(' ');
            out.push_str(&render_rust_block(&f.body));
            out.push(')');
            out
        }
        other => panic!(
            "render_rust_item: item {other:?} is outside parser slice 3c-iii; \
             keep the corpus to the ported item forms or extend the renderer"
        ),
    }
}

/// Parse `src` as a single top-level item via the public `karac::parse` and
/// render the first item.
fn rust_render(src: &str) -> String {
    let result = karac::parse(src);
    match result.program.items.first() {
        Some(item) => render_rust_item(item),
        None => panic!("Rust seed produced no item for input {src:?}"),
    }
}

/// Item differential gate (slice 3c). Same harness as the sibling oracles:
/// build the real selfhost modules into a temp project with a per-input driver,
/// run, and diff against the seed's render.
#[test]
fn selfhost_parser_matches_rust_parser_items() {
    // 1. Crate-root program: a driver over the Kāra `parse_item_str` +
    //    `render_item`. The six selfhost modules are copied verbatim (step 2).
    let mut prog = String::from(
        "import ast.Item;\n\
         import parser.parse_item_str;\n\
         import ast_render.render_item;\n\
         \n\
         fn parse_and_print(src: String) with panics {\n\
         \x20   println(render_item(parse_item_str(src)));\n\
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
        "karac-selfhost-parser-items-{}",
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
        let compile_err = berr.contains("error[")
            || berr.contains("codegen failed")
            || berr.contains("parse error")
            || berr.contains("Module verification failed");
        assert!(
            !compile_err,
            "self-hosted item parser FAILED TO COMPILE (port regression):\n{berr}\n\
             --- generated source ---\n{prog}"
        );
        eprintln!(
            "skip: selfhost_parser_matches_rust_parser_items — parser did not link \
             (no llvm feature / missing runtime archive); stderr:\n{berr}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }

    // 3. Run the Kāra parser binary.
    let run = std::process::Command::new(&bin)
        .output()
        .expect("run kara item-parser binary");
    assert!(
        run.status.success(),
        "kara item-parser binary exited nonzero:\n{}",
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
            "self-hosted item parser diverged from the Rust parser at input {i} ({:?}):\n  \
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
