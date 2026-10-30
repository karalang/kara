//! Expression printing for the canonical formatter.

use crate::ast::*;
use std::fmt::Write;

use super::{escape_string, float_suffix_str};

/// Binding powers, as the parser's Pratt loop assigns them
/// (`src/parser/exprs.rs`): an operand is written in parentheses when its
/// own operator binds more loosely than its slot needs, so a rendered
/// expression reparses to the tree it came from. `(a + 1) * b` printed as
/// `a + 1 * b` was a different program, under `karac fmt` and in every
/// fix-it that renders an expression (B-2026-10-19-69).
const RANGE_POWER: u8 = 5;
const CAST_POWER: u8 = 23;
const UNARY_POWER: u8 = 24;

/// The left and right binding powers of a binary operator.
fn binop_power(op: &BinOp) -> (u8, u8) {
    match op {
        BinOp::Or => (6, 7),
        BinOp::And => (8, 9),
        // A comparison of comparisons keeps both sides' parentheses: the
        // chain reads as Python's `a < b < c`, which Kāra does not mean.
        BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => (11, 11),
        BinOp::BitOr => (12, 13),
        BinOp::BitXor => (14, 15),
        BinOp::BitAnd => (16, 17),
        BinOp::Shl | BinOp::Shr => (18, 19),
        BinOp::Add | BinOp::Sub => (20, 21),
        BinOp::Mul | BinOp::Div | BinOp::Mod => (22, 23),
        BinOp::Range | BinOp::RangeInclusive => (RANGE_POWER, RANGE_POWER + 1),
    }
}

/// How tightly an expression's outermost operator binds; `u8::MAX` for an
/// atom or a postfix form, which never needs parentheses as an operand.
fn expr_power(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Closure { .. } => 0,
        ExprKind::Pipe { .. } => 2,
        ExprKind::NilCoalesce { .. } => 4,
        ExprKind::Range { .. } => RANGE_POWER,
        ExprKind::Binary {
            op: BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq,
            ..
        } => 10,
        ExprKind::Binary { op, .. } => binop_power(op).0,
        ExprKind::Cast { .. } => CAST_POWER,
        ExprKind::Unary { .. } => UNARY_POWER,
        _ => u8::MAX,
    }
}

impl super::Formatter {
    /// `e` in a slot that needs binding power `min`, parenthesized when its
    /// own operator binds more loosely.
    fn format_operand(&mut self, e: &Expr, min: u8) {
        if self.paren_block_like_at_stmt_start(e) {
            return;
        }
        if expr_power(e) < min {
            self.stmt_start = false;
            self.write_str("(");
            self.format_expr(e);
            self.write_str(")");
        } else {
            self.format_expr(e);
        }
    }

    /// The object of a `.method()`, `.field`, `.0` or `[i]`. A collection
    /// literal there reads back as a type application (`Vec[Vec["a"]].len()`
    /// parses as `Vec[T].len` with `T = Vec["a"]`), so a `Vec` one is written
    /// in its `vec![..]` spelling and any other is parenthesized.
    fn format_receiver(&mut self, object: &Expr) {
        if self.paren_block_like_at_stmt_start(object) {
            return;
        }
        if let ExprKind::PrefixCollectionLiteral { type_name, items } = &object.kind {
            if type_name == "Vec" {
                self.write_str("vec![");
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(e);
                }
                self.write_str("]");
            } else {
                self.write_str("(");
                self.format_expr(object);
                self.write_str(")");
            }
            return;
        }
        self.format_operand(object, u8::MAX);
    }

    /// A value path with its explicit generic arguments. They follow the
    /// type they belong to: the last segment of a call (`make[i64, 4](..)`,
    /// `Vector[f32, 4](..)`), but the first of a qualified variant
    /// (`Opt[i64].None`, `Sh[i64].Pt { .. }`).
    fn write_path_with_args(
        &mut self,
        segments: &[String],
        generic_args: &Option<Vec<GenericArg>>,
        span: &crate::token::Span,
    ) {
        let variant = segments.len() == 2 && segments[1].starts_with(|c: char| c.is_uppercase());
        if generic_args.is_none() || !variant {
            self.write_path(segments);
            self.format_generic_args_with_hasher(generic_args, span);
        } else {
            self.write_path(&segments[..1]);
            self.format_generic_args_opt(generic_args);
            self.write_str(".");
            self.write_path(&segments[1..]);
        }
    }

    /// The leftmost operand of an expression statement, when it is block-like
    /// (`match x { .. }.to_string()`, `if c { a } else { b } + 1`): the parser
    /// ends the statement at that `}`, so the parentheses the source needed
    /// are written back. True when it wrote the operand.
    fn paren_block_like_at_stmt_start(&mut self, e: &Expr) -> bool {
        // A non-block-like operand keeps the flag: its own leftmost operand
        // (`if ..` in `if .. >> a << b`) is the one at statement start.
        if !self.stmt_start || !crate::parser::Parser::is_block_like_prefix(e) {
            return false;
        }
        self.stmt_start = false;
        self.write_str("(");
        self.format_expr(e);
        self.write_str(")");
        true
    }

    pub(super) fn format_expr(&mut self, expr: &Expr) {
        // Set by the statement printer; handed on only to the operand printed
        // first, see `paren_block_like_at_stmt_start`.
        let stmt_start = std::mem::take(&mut self.stmt_start);
        match &expr.kind {
            ExprKind::Integer(n, sfx) => self.write_str(&super::int_literal(*n, *sfx)),
            ExprKind::Float(f, sfx) => {
                // `{:?}` keeps the `.0` (`1.0`, not `1`, which reads back as
                // an integer) and writes a large or tiny value with an
                // exponent rather than hundreds of digits.
                write!(self.output, "{f:?}").unwrap();
                if let Some(s) = sfx {
                    self.write_str(float_suffix_str(*s));
                }
            }
            ExprKind::CharLit(c) => write!(self.output, "'{}'", super::escape_char(*c)).unwrap(),
            // `b"..."` — re-emit the quoted form, escaping exactly what the
            // lexer accepts so the output round-trips (B-2026-08-20-37).
            ExprKind::ByteStringLit(bytes) => {
                self.write_str("b\"");
                for b in bytes {
                    match *b {
                        b'\n' => self.write_str("\\n"),
                        b'\t' => self.write_str("\\t"),
                        b'\r' => self.write_str("\\r"),
                        0 => self.write_str("\\0"),
                        b'\\' => self.write_str("\\\\"),
                        b'"' => self.write_str("\\\""),
                        c if (0x20..=0x7E).contains(&c) => self.write_str(&(c as char).to_string()),
                        c => self.write_str(&format!("\\x{c:02X}")),
                    }
                }
                self.write_str("\"");
            }
            ExprKind::ByteLit(b) => {
                // `b'X'` — mirror the lexer's accepted forms. Printable
                // ASCII (0x20..=0x7E, except `'` and `\`) round-trips as
                // `b'X'`. Simple escapes (`\n \t \r \0 \\ \'`) preserve
                // their named form. Everything else emits `b'\xHH'`.
                match *b {
                    b'\n' => self.write_str("b'\\n'"),
                    b'\t' => self.write_str("b'\\t'"),
                    b'\r' => self.write_str("b'\\r'"),
                    0 => self.write_str("b'\\0'"),
                    b'\\' => self.write_str("b'\\\\'"),
                    b'\'' => self.write_str("b'\\''"),
                    0x20..=0x7E => write!(self.output, "b'{}'", *b as char).unwrap(),
                    _ => write!(self.output, "b'\\x{:02X}'", b).unwrap(),
                }
            }
            ExprKind::StringLit(s) => {
                self.write_str("\"");
                self.write_str(&escape_string(s));
                self.write_str("\"");
            }
            ExprKind::CStringLit { bytes, .. } => {
                // `c"..."` — re-emit by escaping the bytes. The
                // formatter's job is to produce a syntactically
                // equivalent literal; the lexer's escape rules apply
                // when the byte is non-printable ASCII or non-ASCII.
                self.write_str("c\"");
                for &b in bytes {
                    match b {
                        b'"' => self.write_str("\\\""),
                        b'\\' => self.write_str("\\\\"),
                        b'\n' => self.write_str("\\n"),
                        b'\r' => self.write_str("\\r"),
                        b'\t' => self.write_str("\\t"),
                        0x20..=0x7e => self.output.push(b as char),
                        _ => write!(self.output, "\\x{:02x}", b).unwrap(),
                    }
                }
                self.write_str("\"");
            }
            ExprKind::InterpolatedStringLit(parts) => {
                self.write_str("f\"");
                for part in parts {
                    match part {
                        crate::ast::ParsedInterpolationPart::Text(s) => {
                            // A literal brace in an f-string is doubled.
                            let text = s.replace('{', "{{").replace('}', "}}");
                            self.write_str(&escape_string(&text))
                        }
                        crate::ast::ParsedInterpolationPart::Expr(e, spec) => {
                            // `{ {` keeps a block's own brace from reading as
                            // the doubled `{{` of a literal brace.
                            let pad = matches!(e.kind, ExprKind::Block(_));
                            self.write_str(if pad { "{ " } else { "{" });
                            self.format_expr(e);
                            if pad {
                                self.write_str(" ");
                            }
                            if let Some(spec) = spec {
                                self.write_str(":");
                                self.write_str(spec);
                            }
                            self.write_str("}");
                        }
                    }
                }
                self.write_str("\"");
            }
            ExprKind::Bool(b) => self.write_str(if *b { "true" } else { "false" }),
            ExprKind::Identifier(name) => self.write_ident(name),
            ExprKind::Path {
                segments,
                generic_args,
            } => self.write_path_with_args(segments, generic_args, &expr.span),
            ExprKind::SelfValue => self.write_str("self"),
            ExprKind::SelfType => self.write_str("Self"),

            ExprKind::Binary { op, left, right } => {
                let (l_bp, r_bp) = binop_power(op);
                self.stmt_start = stmt_start;
                self.format_operand(left, l_bp);
                self.write_str(match op {
                    BinOp::Add => " + ",
                    BinOp::Sub => " - ",
                    BinOp::Mul => " * ",
                    BinOp::Div => " / ",
                    BinOp::Mod => " % ",
                    BinOp::Eq => " == ",
                    BinOp::NotEq => " != ",
                    BinOp::Lt => " < ",
                    BinOp::LtEq => " <= ",
                    BinOp::Gt => " > ",
                    BinOp::GtEq => " >= ",
                    BinOp::And => " and ",
                    BinOp::Or => " or ",
                    BinOp::BitAnd => " & ",
                    BinOp::BitOr => " | ",
                    BinOp::BitXor => " ^ ",
                    BinOp::Shl => " << ",
                    BinOp::Shr => " >> ",
                    BinOp::Range => "..",
                    BinOp::RangeInclusive => "..=",
                });
                self.format_operand(right, r_bp);
            }
            ExprKind::Unary { op, operand } => {
                self.write_str(match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                    UnaryOp::BitNot => "~",
                    UnaryOp::Deref => "*",
                    UnaryOp::Ref => "ref ",
                    UnaryOp::MutRef => "mut ref ",
                });
                self.format_operand(operand, UNARY_POWER);
            }
            ExprKind::Question(inner) => {
                self.stmt_start = stmt_start;
                self.format_operand(inner, u8::MAX);
                self.write_str("?");
            }
            ExprKind::OptionalChain {
                object,
                field_or_method,
                args,
            } => {
                self.stmt_start = stmt_start;
                self.format_receiver(object);
                self.write_str("?.");
                self.write_ident(field_or_method);
                if let Some(ref a) = args {
                    self.write_str("(");
                    self.format_call_args(a);
                    self.write_str(")");
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.format_operand(left, 4);
                self.write_str(" ?? ");
                self.format_operand(right, 5);
            }
            ExprKind::Call { callee, args } => {
                // `(h.f)()` calls a function-typed field; without the
                // parentheses it reparses as the method call `h.f()`. Any
                // callee that is not a plain path or call keeps them too.
                let paren = matches!(
                    callee.kind,
                    ExprKind::FieldAccess { .. }
                        | ExprKind::TupleIndex { .. }
                        | ExprKind::Closure { .. }
                        | ExprKind::Binary { .. }
                        | ExprKind::Unary { .. }
                        | ExprKind::Cast { .. }
                        | ExprKind::If { .. }
                        | ExprKind::Match { .. }
                        | ExprKind::Range { .. }
                );
                if paren {
                    self.write_str("(");
                }
                self.format_expr(callee);
                if paren {
                    self.write_str(")");
                }
                self.write_str("(");
                self.format_call_args(args);
                self.write_str(")");
            }
            ExprKind::MethodCall {
                object,
                method,
                turbofish,
                args,
                ..
            } => {
                self.stmt_start = stmt_start;
                self.format_receiver(object);
                self.write_str(".");
                // Any keyword is a member name after `.`; no `r#` needed.
                self.write_str(method);
                if let Some(ref tf) = turbofish {
                    self.write_str("[");
                    for (i, t) in tf.iter().enumerate() {
                        if i > 0 {
                            self.write_str(", ");
                        }
                        self.format_type_expr(t);
                    }
                    self.write_str("]");
                }
                self.write_str("(");
                self.format_call_args(args);
                self.write_str(")");
            }
            ExprKind::FieldAccess { object, field } => {
                self.stmt_start = stmt_start;
                self.format_receiver(object);
                self.write_str(".");
                self.write_str(field);
            }
            ExprKind::TupleIndex { object, index } => {
                self.stmt_start = stmt_start;
                self.format_receiver(object);
                write!(self.output, ".{index}").unwrap();
            }
            ExprKind::Index { object, index } => {
                self.stmt_start = stmt_start;
                self.format_receiver(object);
                self.write_str("[");
                self.format_expr(index);
                self.write_str("]");
            }
            ExprKind::Block(block) => self.format_block(block),
            ExprKind::Comptime(block) => {
                self.write_str("comptime ");
                self.format_block(block);
            }
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.write_str("if ");
                self.format_expr(condition);
                self.write_str(" ");
                self.format_block(then_block);
                if let Some(ref eb) = else_branch {
                    self.write_str(" else ");
                    match &eb.kind {
                        ExprKind::If { .. } | ExprKind::IfLet { .. } => self.format_expr(eb),
                        ExprKind::Block(block) => self.format_block(block),
                        _ => self.format_expr(eb),
                    }
                }
            }
            ExprKind::IfLet {
                pattern,
                value,
                then_block,
                else_branch,
            } => {
                self.write_str("if let ");
                self.format_pattern(pattern);
                self.write_str(" = ");
                self.format_expr(value);
                self.write_str(" ");
                self.format_block(then_block);
                if let Some(ref eb) = else_branch {
                    self.write_str(" else ");
                    match &eb.kind {
                        ExprKind::Block(block) => self.format_block(block),
                        _ => self.format_expr(eb),
                    }
                }
            }
            // A `select`, as the parser gives it (`crate::select_desugar`):
            // each arm's op is its guard, and the arm without one is the
            // `default`.
            ExprKind::Match { scrutinee, arms } if matches!(&scrutinee.kind, ExprKind::Identifier(n) if n == crate::select_desugar::MARKER) =>
            {
                self.write_str("select {\n");
                self.push_indent();
                for arm in arms {
                    self.flush_comments_before(arm.span.offset);
                    self.write_indent();
                    match &arm.guard {
                        Some(op) => {
                            self.format_pattern(&arm.pattern);
                            self.write_str(" = ");
                            self.format_expr(op);
                        }
                        None => self.write_str("default"),
                    }
                    self.write_str(" => ");
                    self.format_expr(&arm.body);
                    self.write_str(",\n");
                }
                self.flush_comments_before(expr.span.offset + expr.span.length);
                self.pop_indent();
                self.write_indent();
                self.write_str("}");
            }
            ExprKind::Match { scrutinee, arms } => {
                self.write_str("match ");
                self.format_expr(scrutinee);
                self.write_str(" {\n");
                self.push_indent();
                for arm in arms {
                    self.flush_comments_before(arm.span.offset);
                    self.write_indent();
                    self.format_pattern(&arm.pattern);
                    if let Some(ref guard) = arm.guard {
                        self.write_str(" if ");
                        self.format_expr(guard);
                    }
                    self.write_str(" => ");
                    self.format_expr(&arm.body);
                    self.write_str(",\n");
                }
                self.flush_comments_before(expr.span.offset + expr.span.length);
                self.pop_indent();
                self.write_indent();
                self.write_str("}");
            }
            ExprKind::While {
                label,
                condition,
                body,
                ..
            } => {
                if let Some(ref l) = label {
                    write!(self.output, "'{l}: ").unwrap();
                }
                self.write_str("while ");
                self.format_expr(condition);
                self.write_str(" ");
                self.format_block(body);
            }
            ExprKind::WhileLet {
                label,
                pattern,
                value,
                body,
                ..
            } => {
                if let Some(ref l) = label {
                    write!(self.output, "'{l}: ").unwrap();
                }
                self.write_str("while let ");
                self.format_pattern(pattern);
                self.write_str(" = ");
                self.format_expr(value);
                self.write_str(" ");
                self.format_block(body);
            }
            ExprKind::For {
                label,
                pattern,
                iterable,
                body,
                par,
                ..
            } => {
                if let Some(ref l) = label {
                    write!(self.output, "'{l}: ").unwrap();
                }
                if let Some(p) = par {
                    self.write_str("par");
                    if let Some(n) = &p.limit {
                        self.write_str("(limit: ");
                        self.format_expr(n);
                        self.write_str(")");
                    }
                    self.write_str(" ");
                }
                self.write_str("for ");
                self.format_pattern(pattern);
                self.write_str(" in ");
                self.format_expr(iterable);
                self.write_str(" ");
                self.format_block(body);
            }
            ExprKind::Loop { label, body, .. } => {
                if let Some(ref l) = label {
                    write!(self.output, "'{l}: ").unwrap();
                }
                self.write_str("loop ");
                self.format_block(body);
            }
            ExprKind::LabeledBlock { label, body, .. } => {
                write!(self.output, "'{label}: ").unwrap();
                self.format_block(body);
            }
            ExprKind::Closure {
                params,
                capture_mode,
                prefix_span: _,
                body,
            } => {
                match capture_mode {
                    Some(CaptureMode::Own) => self.write_str("own "),
                    Some(CaptureMode::Ref) => self.write_str("ref "),
                    Some(CaptureMode::MutRef) => self.write_str("mut ref "),
                    None => {}
                }
                self.write_str("|");
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_pattern(&p.pattern);
                    if let Some(ref t) = p.ty {
                        self.write_str(if p.is_own { ": own " } else { ": " });
                        self.format_type_expr(t);
                    }
                }
                self.write_str("| ");
                self.format_expr(body);
            }
            ExprKind::Return(val) => {
                self.write_str("return");
                if let Some(ref v) = val {
                    self.write_str(" ");
                    self.format_expr(v);
                }
            }
            ExprKind::Break { label, value } => {
                self.write_str("break");
                if let Some(ref l) = label {
                    write!(self.output, " '{l}").unwrap();
                }
                if let Some(ref v) = value {
                    self.write_str(" ");
                    self.format_expr(v);
                }
            }
            ExprKind::Continue { label, .. } => {
                self.write_str("continue");
                if let Some(ref l) = label {
                    write!(self.output, " '{l}").unwrap();
                }
            }
            ExprKind::Tuple(elems) => {
                self.write_str("(");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(e);
                }
                if elems.len() == 1 {
                    self.write_str(",");
                }
                self.write_str(")");
            }
            ExprKind::ArrayLiteral(elems) => {
                self.write_str("[");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(e);
                }
                self.write_str("]");
            }
            ExprKind::PrefixCollectionLiteral { type_name, items } => {
                self.write_str(type_name);
                self.write_str("[");
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        self.write_str(", ");
                    }
                    self.format_expr(e);
                }
                self.write_str("]");
            }
            ExprKind::RepeatLiteral {
                type_name,
                value,
                count,
            } => {
                if let Some(name) = type_name {
                    self.write_ident(name);
                }
                self.write_str("[");
                self.format_expr(value);
                self.write_str("; ");
                self.format_expr(count);
                self.write_str("]");
            }
            ExprKind::MapLiteral { type_name, entries } => {
                // `["k": v, ...]` (bare) / `Map["k": v, ...]` /
                // `SortedMap["k": v, ...]` — the three spellings that produce
                // this node, printed back as themselves.
                //
                // The prefix name has to survive the round trip or a
                // `SortedMap` literal comes back a hashed `Map`
                // (B-2026-09-15-25). The BRACE form this arm used to print
                // (`{\n  "k": v,\n}`) is not Kāra syntax at all and never
                // re-parsed — `{"a": 1}` is `Expected expression, found Colon`
                // — so every map literal `karac fmt` touched came back a parse
                // error (B-2026-09-19-16, found while adding the `SortedMap`
                // spelling above).
                if let Some(name) = type_name {
                    self.write_ident(name);
                }
                // `[:]`, the empty map (B-2026-10-19-60).
                if type_name.is_none() && entries.is_empty() {
                    self.write_str("[:]");
                } else {
                    self.write_str("[");
                    for (i, (k, v)) in entries.iter().enumerate() {
                        if i > 0 {
                            self.write_str(", ");
                        }
                        self.format_expr(k);
                        self.write_str(": ");
                        self.format_expr(v);
                    }
                    self.write_str("]");
                }
            }
            ExprKind::StructLiteral {
                path,
                fields,
                spread,
                generic_args,
            } => {
                self.write_path_with_args(path, generic_args, &expr.span);
                self.write_str(" {\n");
                self.push_indent();
                for fi in fields {
                    self.write_indent();
                    if fi.shorthand {
                        self.write_ident(&fi.name);
                    } else {
                        self.write_ident(&fi.name);
                        self.write_str(": ");
                        self.format_expr(&fi.value);
                    }
                    self.write_str(",\n");
                }
                if let Some(ref s) = spread {
                    self.write_indent();
                    self.write_str("..");
                    self.format_expr(s);
                    self.output.push('\n');
                }
                self.pop_indent();
                self.write_indent();
                self.write_str("}");
            }
            ExprKind::Pipe { left, right } => {
                self.stmt_start = stmt_start;
                self.format_operand(left, 2);
                self.write_str(" |> ");
                self.format_operand(right, 3);
            }
            ExprKind::PipePlaceholder => self.write_str("_"),
            ExprKind::Cast { expr, .. } if layout_query_type(expr).is_some() => {
                // `T.size_of()` reaches the tree as `size_of[T]() as i64`,
                // whose one-argument brackets read back as an index.
                let (name, t) = layout_query_type(expr).unwrap();
                self.format_type_expr(t);
                self.write_str(".");
                self.write_str(name);
                self.write_str("()");
            }
            ExprKind::Cast { expr, ty } => {
                self.stmt_start = stmt_start;
                self.format_operand(expr, CAST_POWER);
                self.write_str(" as ");
                self.format_type_expr(ty);
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                if let Some(s) = start {
                    self.format_operand(s, RANGE_POWER + 1);
                }
                if *inclusive {
                    self.write_str("..=");
                } else {
                    self.write_str("..");
                }
                if let Some(e) = end {
                    self.format_operand(e, RANGE_POWER + 1);
                }
            }
            ExprKind::Unsafe(block) => {
                self.write_str("unsafe ");
                self.format_block(block);
            }
            ExprKind::Try(block) => {
                self.write_str("try ");
                self.format_block(block);
            }
            ExprKind::Seq(block) => {
                self.write_str("seq ");
                self.format_block(block);
            }
            ExprKind::Par(block) => {
                self.write_str("par ");
                // `par { a, b }`: the branches are a tuple with no statements
                // around it (see the parser's `parse_par_branches`).
                match block.final_expr.as_deref() {
                    Some(Expr {
                        kind: ExprKind::Tuple(branches),
                        ..
                    }) if block.stmts.is_empty() && !branches.is_empty() => {
                        self.write_str("{ ");
                        for (i, b) in branches.iter().enumerate() {
                            if i > 0 {
                                self.write_str(", ");
                            }
                            self.format_expr(b);
                        }
                        if branches.len() == 1 {
                            self.write_str(",");
                        }
                        self.write_str(" }");
                    }
                    _ => self.format_block(block),
                }
            }
            ExprKind::Lock { mutex, alias, body } => {
                // `lock <place> [alias] { body }` — the alias has NO `as`
                // keyword (the grammar is `lock IDENT [IDENT] BLOCK`).
                self.write_str("lock ");
                self.format_expr(mutex);
                if let Some(ref a) = alias {
                    self.write_str(" ");
                    self.write_str(a);
                }
                self.write_str(" ");
                self.format_block(body);
            }
            ExprKind::Providers { bindings, body } => {
                self.write_str("providers {\n");
                self.push_indent();
                for b in bindings {
                    self.write_indent();
                    self.write_ident(&b.resource);
                    self.write_str(" => ");
                    self.format_expr(&b.value);
                    self.write_str(",\n");
                }
                self.pop_indent();
                self.write_indent();
                self.write_str("} in ");
                self.format_block(body);
            }
            ExprKind::OffsetOf { ty, field_path } => {
                self.write_str("offset_of[");
                self.format_type_expr(ty);
                self.write_str("](");
                for (i, segment) in field_path.iter().enumerate() {
                    if i > 0 {
                        self.write_str(".");
                    }
                    self.write_ident(segment);
                }
                self.write_str(")");
            }
            ExprKind::Error => self.write_str("/* error */"),
        }
    }

    pub(super) fn format_call_args(&mut self, args: &[CallArg]) {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                self.write_str(", ");
            }
            if let Some(ref label) = arg.label {
                self.write_ident(label);
                self.write_str(": ");
            }
            if arg.mut_marker {
                self.write_str("mut ");
            }
            self.format_expr(&arg.value);
        }
    }
}

/// The type and query name of the call the parser builds for `T.size_of()` /
/// `T.align_of()`.
fn layout_query_type(e: &Expr) -> Option<(&str, &TypeExpr)> {
    let ExprKind::Call { callee, args } = &e.kind else {
        return None;
    };
    let ExprKind::Path {
        segments,
        generic_args: Some(ga),
    } = &callee.kind
    else {
        return None;
    };
    match (segments.as_slice(), ga.as_slice()) {
        ([name], [GenericArg::Type(t)])
            if args.is_empty() && matches!(name.as_str(), "size_of" | "align_of") =>
        {
            Some((name.as_str(), t))
        }
        _ => None,
    }
}
