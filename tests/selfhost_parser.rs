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

use karac::ast::{BinOp, FieldPattern, LiteralPattern, MatchArm, Pattern, PatternKind};
use karac::ast::{Block, CallArg, Expr, ExprKind, FieldInit, Item, Stmt, StmtKind, UnaryOp};
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
    // Postfix (slice 2a) — field / tuple-index / index / call / method-call.
    "a.b",
    "a.b.c",
    "foo.bar",
    "a.0",
    "a.1.2",
    "v[0]",
    "v[i]",
    "m[k][j]",
    "a[i + 1]",
    "f()",
    "f(x)",
    "f(x, y)",
    "g(1, 2, 3)",
    "a.m()",
    "a.m(x)",
    "obj.field.method(arg)",
    "f(g(x))",
    "v[0].field",
    "a.b(c).d",
    // Postfix mixed with prefix / binary.
    "-a.b",
    "a.b + c",
    "f(x) * 2",
    "not a.flag",
    "a.b == c.d",
    // Labeled + mut-marker arguments.
    "f(x: 1)",
    "g(a, b: 2)",
    "h(mut x)",
    // Control flow (slice 2a) — blocks, statements, if/else, return.
    "{ a }",
    "{ 1 }",
    "{ a; b }",
    "{ a; b; c }",
    "{ let x = 1; x }",
    "{ let mut y = a; y = b; y }",
    "{ let z = a + b; z }",
    // Annotated `let` (`: T` now captured, not desynced) — the annotation is not
    // rendered (seed uses `..`), so both sides keep the `(let SPAN pat val)` shape.
    "{ let x: i64 = 1; x }",
    "{ let mut z: bool = true; z }",
    "{ let p: (i64, bool) = t; p }",
    "{ f(x); g(y) }",
    "{ a.b = c; a.b }",
    "{ v[0] = x; v[0] }",
    "if a { b }",
    "if a { b } else { c }",
    "if a { b } else if c { d } else { e }",
    "if x < y { a } else { b }",
    "if a { let p = b; p } else { c }",
    "{ if a { b } else { c } }",
    "{ let r = if a { b } else { c }; r }",
    "{ return a; }",
    "{ return; }",
    "return a + b",
    "{ let w = f(x); w.y }",
    // Loops + loop control (slice 2b) — while / for / loop / break / continue.
    // `while`/`for`/`loop` are block-STATEMENTS even when trailing (never a
    // tail); `if`/`break`/`continue` remain valid tails.
    "loop { break }",
    "loop { break 42 }",
    "loop { continue }",
    "loop { break a + b }",
    "loop { break (1 + 2) }",
    "while a { b }",
    "while a < b { c }",
    "while a { b; c }",
    "while c { break }",
    "for x in xs { f(x) }",
    "for i in ns { g(i); }",
    "for i in r { continue }",
    "loop { if done { break } }",
    "loop { if a { break } else { continue } }",
    "{ let mut i = 0; while i < n { i = i + 1; } }",
    "{ for x in xs { f(x) } }",
    "{ loop { break } x }",
    "loop { while a { break } }",
    // Labeled loops + labeled break/continue (slice 2b). Note the seed's span
    // quirk: a labeled `loop` span starts at the LABEL, while a labeled
    // `while`/`for` span starts at the KEYWORD (the label is excluded).
    "outer: loop { break outer }",
    "outer: loop { break outer 42 }",
    "outer: while a { continue outer }",
    "outer: for x in xs { break outer }",
    "loop { outer: loop { break outer } }",
    // Adversarial: a NON-label value identifier inside a labeled loop must stay
    // a value (`break x`), exercising the non-consuming known-label peek.
    "outer: loop { break x }",
    "outer: loop { break a + b }",
    "row: while a { col: while b { break row } }",
    // match + patterns (slice 3b core) — wildcard, binding, literals, tuple,
    // single-segment tuple-variant, or-patterns, guards, block/non-block arms.
    "match x { 1 => a }",
    "match x { _ => a }",
    "match x { y => y }",
    "match x { 1 => a, 2 => b }",
    "match x { 1 => a, _ => b }",
    "match x { 0 => a, n => n }",
    "match x { true => a, false => b }",
    "match c { 'a' => 1, 'b' => 2, _ => 0 }",
    "match s { \"hi\" => 1, _ => 0 }",
    "match x { 1i32 => a, _ => b }",
    "match opt { Some(y) => y, None => 0 }",
    "match e { Foo(a) => a, Bar(b, c) => b, _ => 0 }",
    "match p { (a, b) => a }",
    "match p { (a, b, c) => a, _ => 0 }",
    "match p { (x, y) => x + y, _ => 0 }",
    "match n { 1 | 2 | 3 => a, _ => b }",
    "match v { Some(a) | None => a, _ => b }",
    "match x { 1 if a => b, _ => c }",
    "match x { Some(n) if n > 0 => n, _ => 0 }",
    "match x { 1 => { a }, 2 => { b } }",
    "match x { 1 => { f(y); g(z) } _ => h }",
    "match f(x) { 0 => a, _ => b }",
    "match pair { (Some(a), b) => a, _ => 0 }",
    "match x { n => match n { 0 => a, _ => b } }",
    // Struct patterns (struct-pattern slice) — shorthand fields, explicit
    // sub-patterns, `..` rest, and nesting; in match arms and `let` bindings.
    "match p { Point { x, y } => x }",
    "match p { Point { x: a, y: b } => a }",
    "match n { Node { val, .. } => val }",
    "match e { CallExpr { callee, args, span } => callee }",
    "match p { Wrap { inner: Point { x, y } } => x }",
    "match o { Some(Point { x, y }) => x, None => 0 }",
    "{ let Point { x, y } = p; x }",
    "{ let CallArg { label, value, span } = a; value }",
    "{ let Node { val, .. } = n; val }",
    // Struct literals (struct-literal slice) — explicit fields, trailing comma,
    // empty, shorthand, mixed, spread, nested value, field-value expressions.
    "Point { x: 1 }",
    "Point { x: 1, y: 2 }",
    "Point { x: 1, y: 2, }",
    "Foo {}",
    "Point { x }",
    "Point { x, y }",
    "Point { x: 1, y }",
    "Point { x: a + b }",
    "Point { x: f(y) }",
    "Wrapper { inner: Inner { v: 0 } }",
    "Config { base: Base { n: 1 }, flag: true }",
    "Point { x: 1, ..base }",
    "Point { ..base }",
    // Struct literal in value positions — call arg, return, let-binding, tuple.
    "f(Point { x: 1 })",
    "g(Point { x: 1 }, other)",
    "{ return Point { x: 1 }; }",
    "{ let p = Point { x: 1 }; p }",
    "(Point { x: 1 }, Point { y: 2 })",
    "Point { x: 1 }.x",
    // Disambiguation: an uppercase name in a `no_struct_literal` position (a
    // loop/branch condition) is a bare identifier, and the trailing `{ … }`
    // opens the body — NOT a single-field shorthand struct literal.
    "if Flag { x }",
    "while Flag { x }",
    "for i in Src { g(i) }",
    // But a struct literal nested in a call inside a condition is unrestricted.
    "if has(Point { x: 1 }) { y }",
    // Real-world shapes lifted verbatim from the selfhost sources — field-access
    // values (`clone_span`'s `Span { line: s.line, … }`), method-call values,
    // literal fields, and the multi-field constructor forms the front-end must
    // handle to process its own source.
    "Span { line: s.line, column: s.column, offset: s.offset, length: s.length }",
    "Span { line: 1, column: 1, offset: 0, length: 0 }",
    "ResolveError { kind: kind, off: off, len: len }",
    "IdentExpr { name: name, span: span }",
    "CallExpr { callee: callee, args: args, span: sp }",
    "SpanNode { span: self.span_from(start) }",
    // Uppercase-rooted dotted paths (path-expression slice) — bare unit-variant
    // values, associated-function calls, and enum-variant construction. Rooted
    // as `Path` (bare) / `Call` with a `Path` callee, NOT `MethodCall`.
    "Vec.new()",
    "Token.Error",
    "SelfMode.NoSelf",
    "Parser.new(tokens)",
    "Map.new()",
    "Expr.Unary(u)",
    "Expr.Ident(IdentExpr { name: name, span: span })",
    "Color.Red.next",
    "Vec.new().push(x)",
    "f(Token.Error)",
    "Point { tag: Tag.A }",
    // Lowercase-rooted `.` stays a field/method access (postfix loop), NOT a path.
    "obj.method()",
    "module.func(a)",
    "v.field",
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
        Some(FloatSuffix::F16) => "f16",
        Some(FloatSuffix::BF16) => "bf16",
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

/// ` :LABEL` for a loop / break / continue label — must match
/// `ast_render.kara::render_label`.
fn render_rust_label(label: &Option<String>) -> String {
    match label {
        Some(l) => format!(" :{l}"),
        None => String::new(),
    }
}

/// `(arg[ :LABEL][ mut] @off:len VALUE)` — must match `ast_render.kara::render_arg`.
fn render_rust_arg(a: &CallArg) -> String {
    let mut out = String::from("(arg");
    if let Some(l) = &a.label {
        out.push_str(" :");
        out.push_str(l);
    }
    if a.mut_marker {
        out.push_str(" mut");
    }
    out.push_str(&format!(
        " @{}:{}",
        a.span.offset as i64 - OFFSET_SHIFT,
        a.span.length
    ));
    out.push(' ');
    out.push_str(&render_rust_expr(&a.value));
    out.push(')');
    out
}

/// ` @<offset-shift>:<length>` for a raw (non-Expr) span — stmt/block heads.
fn span_tag(offset: usize, length: usize) -> String {
    format!(" @{}:{}", offset as i64 - OFFSET_SHIFT, length)
}

/// `(fld NAME @off:len VALUE)` — one struct-literal field. Must match
/// `ast_render.kara::render_struct_field`.
fn render_rust_field(f: &FieldInit) -> String {
    format!(
        "(fld {}{} {})",
        f.name,
        span_tag(f.span.offset, f.span.length),
        render_rust_expr(&f.value)
    )
}

/// Must match `ast_render.kara::render_stmt`.
fn render_rust_stmt(s: &Stmt) -> String {
    let sp = span_tag(s.span.offset, s.span.length);
    match &s.kind {
        StmtKind::Let {
            is_mut,
            pattern,
            value,
            ..
        } => {
            let m = if *is_mut { " mut" } else { "" };
            format!(
                "(let{m}{sp} {} {})",
                render_rust_pattern(pattern),
                render_rust_expr(value)
            )
        }
        StmtKind::Assign { target, value } => format!(
            "(assign{sp} {} {})",
            render_rust_expr(target),
            render_rust_expr(value)
        ),
        StmtKind::Expr(e) => format!("(exprstmt{sp} {})", render_rust_expr(e)),
        other => panic!(
            "render_rust_stmt: StmtKind {other:?} is outside parser slice 2a; \
             keep the corpus to let/expr/assign statements or extend the renderer"
        ),
    }
}

/// Must match `ast_render.kara::render_block`.
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

/// ` @<offset-shift>:<length>` for a pattern node — must match
/// `ast_render.kara::render_pattern`'s span tags.
fn pat_span(p: &Pattern) -> String {
    format!(
        " @{}:{}",
        p.span.offset as i64 - OFFSET_SHIFT,
        p.span.length
    )
}

/// Must match `ast_render.kara::render_pattern` (slice-3b core forms).
fn render_rust_pattern(p: &Pattern) -> String {
    let sp = pat_span(p);
    match &p.kind {
        PatternKind::Wildcard => format!("(pwild{sp})"),
        PatternKind::Binding(name) => format!("(pbind {name}{sp})"),
        PatternKind::Literal(lit) => match lit {
            LiteralPattern::Integer(v, sfx) => {
                let lex = int_suffix_lex(*sfx);
                if lex.is_empty() {
                    format!("(pint {v}{sp})")
                } else {
                    format!("(pint {v} {lex}{sp})")
                }
            }
            LiteralPattern::Float(v, sfx) => {
                let lex = float_suffix_lex(*sfx);
                if lex.is_empty() {
                    format!("(pfloat {v}{sp})")
                } else {
                    format!("(pfloat {v} {lex}{sp})")
                }
            }
            LiteralPattern::Char(c) => format!("(pchar {}{sp})", escape_for_render(&c.to_string())),
            LiteralPattern::String(s) => format!("(pstr {}{sp})", escape_for_render(s)),
            LiteralPattern::Bool(b) => format!("(pbool {b}{sp})"),
        },
        PatternKind::Tuple(elems) => {
            let mut out = format!("(ptuple{sp}");
            for el in elems {
                out.push(' ');
                out.push_str(&render_rust_pattern(el));
            }
            out.push(')');
            out
        }
        PatternKind::TupleVariant { path, patterns } => {
            let mut out = format!("(pvariant {}{sp}", path.join("."));
            for el in patterns {
                out.push(' ');
                out.push_str(&render_rust_pattern(el));
            }
            out.push(')');
            out
        }
        PatternKind::Struct {
            path,
            fields,
            has_rest,
        } => {
            let mut out = format!("(pstruct {}{sp}", path.join("."));
            for f in fields {
                out.push(' ');
                out.push_str(&render_rust_field_pat(f));
            }
            if *has_rest {
                out.push_str(" ..");
            }
            out.push(')');
            out
        }
        PatternKind::Or(alts) => {
            let mut out = format!("(por{sp}");
            for el in alts {
                out.push(' ');
                out.push_str(&render_rust_pattern(el));
            }
            out.push(')');
            out
        }
        other => panic!(
            "render_rust_pattern: PatternKind {other:?} is outside parser slice-3b core; \
             keep the corpus to the ported pattern forms or extend the renderer"
        ),
    }
}

/// `(pfield NAME @o:l [SUBPAT])` — one struct-pattern field. Must match
/// `ast_render.kara::render_struct_field_pat`.
fn render_rust_field_pat(f: &FieldPattern) -> String {
    let mut out = format!(
        "(pfield {}{}",
        f.name,
        span_tag(f.span.offset, f.span.length)
    );
    if let Some(sub) = &f.pattern {
        out.push(' ');
        out.push_str(&render_rust_pattern(sub));
    }
    out.push(')');
    out
}

/// Must match `ast_render.kara::render_match_arm`.
fn render_rust_match_arm(a: &MatchArm) -> String {
    let mut out = String::from("(arm");
    out.push_str(&span_tag(a.span.offset, a.span.length));
    out.push(' ');
    out.push_str(&render_rust_pattern(&a.pattern));
    if let Some(g) = &a.guard {
        out.push_str(" (guard ");
        out.push_str(&render_rust_expr(g));
        out.push(')');
    }
    out.push(' ');
    out.push_str(&render_rust_expr(&a.body));
    out.push(')');
    out
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
        ExprKind::Path {
            segments,
            generic_args: None,
        } => format!("(path {}{sp})", segments.join(".")),
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
        ExprKind::Call { callee, args } => {
            let mut s = format!("(call{sp} {}", render_rust_expr(callee));
            for a in args {
                s.push(' ');
                s.push_str(&render_rust_arg(a));
            }
            s.push(')');
            s
        }
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => {
            let mut s = format!("(mcall {method}{sp} {}", render_rust_expr(object));
            for a in args {
                s.push(' ');
                s.push_str(&render_rust_arg(a));
            }
            s.push(')');
            s
        }
        ExprKind::FieldAccess { object, field } => {
            format!("(field {field}{sp} {})", render_rust_expr(object))
        }
        ExprKind::TupleIndex { object, index } => {
            format!("(tupidx {index}{sp} {})", render_rust_expr(object))
        }
        ExprKind::Index { object, index } => format!(
            "(index{sp} {} {})",
            render_rust_expr(object),
            render_rust_expr(index)
        ),
        ExprKind::Block(block) => render_rust_block(block),
        ExprKind::If {
            condition,
            then_block,
            else_branch,
        } => {
            let mut out = format!(
                "(if{sp} {} {}",
                render_rust_expr(condition),
                render_rust_block(then_block)
            );
            if let Some(eb) = else_branch {
                out.push(' ');
                out.push_str(&render_rust_expr(eb));
            }
            out.push(')');
            out
        }
        ExprKind::Return(value) => {
            let mut out = format!("(return{sp}");
            if let Some(v) = value {
                out.push(' ');
                out.push_str(&render_rust_expr(v));
            }
            out.push(')');
            out
        }
        ExprKind::While {
            label,
            condition,
            body,
            ..
        } => {
            let mut out = String::from("(while");
            out.push_str(&render_rust_label(label));
            out.push_str(&sp);
            out.push(' ');
            out.push_str(&render_rust_expr(condition));
            out.push(' ');
            out.push_str(&render_rust_block(body));
            out.push(')');
            out
        }
        ExprKind::For {
            label,
            pattern,
            iterable,
            body,
            ..
        } => {
            let var = match &pattern.kind {
                PatternKind::Binding(n) => n.clone(),
                other => panic!("slice-2b for pattern must be a plain binding, got {other:?}"),
            };
            let mut out = String::from("(for");
            out.push_str(&render_rust_label(label));
            out.push(' ');
            out.push_str(&var);
            out.push_str(&sp);
            out.push(' ');
            out.push_str(&render_rust_expr(iterable));
            out.push(' ');
            out.push_str(&render_rust_block(body));
            out.push(')');
            out
        }
        ExprKind::Loop { label, body, .. } => {
            let mut out = String::from("(loop");
            out.push_str(&render_rust_label(label));
            out.push_str(&sp);
            out.push(' ');
            out.push_str(&render_rust_block(body));
            out.push(')');
            out
        }
        ExprKind::Break { label, value } => {
            let mut out = String::from("(break");
            out.push_str(&render_rust_label(label));
            out.push_str(&sp);
            if let Some(v) = value {
                out.push(' ');
                out.push_str(&render_rust_expr(v));
            }
            out.push(')');
            out
        }
        ExprKind::Continue { label, .. } => {
            let mut out = String::from("(continue");
            out.push_str(&render_rust_label(label));
            out.push_str(&sp);
            out.push(')');
            out
        }
        ExprKind::Match { scrutinee, arms } => {
            let mut out = format!("(match{sp} {}", render_rust_expr(scrutinee));
            for a in arms {
                out.push(' ');
                out.push_str(&render_rust_match_arm(a));
            }
            out.push(')');
            out
        }
        ExprKind::StructLiteral {
            path,
            fields,
            spread,
        } => {
            let name = path.join("::");
            let mut out = format!("(structlit {name}{sp}");
            for f in fields {
                out.push(' ');
                out.push_str(&render_rust_field(f));
            }
            if let Some(s) = spread {
                out.push_str(&format!(" (spread {})", render_rust_expr(s)));
            }
            out.push(')');
            out
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

// Parser slice 1 (expression core) differential gate. The five codegen
// blockers it surfaced — #32 (self-field-rooted Vec-index reads), #34 (stdlib
// type collision), #36 (recursive by-value struct layout), #37 (shared-enum
// struct-payload pack), #38 (borrowed index-field enum scrutinee dangle), and
// #39 (bare variant name shared across enums mis-resolving in a match) — all
// landed, so this oracle now runs as a normal gated test.
// UN-IGNORED 2026-07-10 (B-2026-07-09-12 EXPRESSION-parser half FIXED): the
// runtime SEGV on every control-flow expression was an auto-parallelization
// CORRECTNESS bug — `parse_if` (and `parse_while`/`parse_for`/`parse_loop`)
// auto-parallelized three sequential `mut ref self` calls
// (`parse_expr_bp`/`parse_block`/`parse_else`) that share the parser cursor,
// racing them via `karac_par_run`. Fixed in `src/concurrency.rs`: a `let x =
// self.mut_method()` now records `self` as written, so the calls conflict and
// stay serial. This EXPRESSION oracle now passes; the sibling ITEM/TYPE parser
// oracles stay ignored against a DISTINCT residual (`B-2026-07-10-4`, not
// auto-par — crashes with `KARAC_AUTO_PAR=0` too).
#[test]
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
