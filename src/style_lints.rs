//! The five style lints of design.md § `karac fix` and `karac explain`
//! (B-2026-11-05-31), each with a machine-applicable fix:
//!
//! - `counter_while_loop`: `let mut i = a; while i < b { ...; i += 1; }`
//!   becomes `for i in a..b { ... }`;
//! - `manual_compound_assign`: `x = x + e` becomes `x += e` on a number, and
//!   on a `String` for `+`;
//! - `needless_return`: a function's final `return e;` becomes `e`;
//! - `redundant_literal_method`: `"text".to_string()` and the `.iter()` of
//!   `for x in [1, 2].iter()` are removed;
//! - `redundant_ref_param`: `x: ref T` becomes `x: T` ([§7] parameter modes:
//!   a parameter borrows by default, so the two mean the same).
//!
//! The pass reads the source text: every fix keeps the author's own spelling
//! of the operands it moves, and each site checks the text between the parts
//! it edits, so a comment or an unusual spelling there makes the lint stay
//! silent rather than rewrite something it has not seen. It runs after the
//! typechecker, which supplies the types `manual_compound_assign` and
//! `counter_while_loop` need, and the caller resolves each hit's lint level
//! from the attributes around it.

use crate::ast::*;
use crate::resolver::{SpanKey, TextEdit};
use crate::token::Span;
use crate::typechecker::{Type, TypeCheckResult};

pub const COUNTER_WHILE_LOOP: &str = "counter_while_loop";
pub const MANUAL_COMPOUND_ASSIGN: &str = "manual_compound_assign";
pub const NEEDLESS_RETURN: &str = "needless_return";
pub const REDUNDANT_LITERAL_METHOD: &str = "redundant_literal_method";
pub const REDUNDANT_REF_PARAM: &str = "redundant_ref_param";

/// All five, which `karac fix` turns on.
pub const LINTS: [&str; 5] = [
    COUNTER_WHILE_LOOP,
    MANUAL_COMPOUND_ASSIGN,
    NEEDLESS_RETURN,
    REDUNDANT_LITERAL_METHOD,
    REDUNDANT_REF_PARAM,
];

/// One site a style lint fires on: where, why, and the edits that fix it.
#[derive(Debug, Clone)]
pub struct StyleHit {
    pub lint: &'static str,
    pub span: Span,
    pub message: String,
    pub edits: Vec<TextEdit>,
}

/// Every style-lint site in the program's own items (those before
/// `library_start`), in source order.
pub fn style_hits(program: &Program, typed: &TypeCheckResult, src: &str) -> Vec<StyleHit> {
    // `library_start` is the byte offset the appended library starts at;
    // the program's own items come before it.
    let own = match program.library_start {
        Some(start) => program.items.partition_point(|i| i.span().offset < start),
        None => program.items.len(),
    };
    let items = &program.items[..own];
    let mut cx = Cx {
        typed,
        src,
        out: Vec::new(),
        fn_values: functions_used_as_values(items),
        ref_bindings: program
            .ref_binding_spans
            .iter()
            .chain(program.mut_ref_binding_spans.iter())
            .map(|k| k.0)
            .collect(),
    };
    for item in items {
        match item {
            Item::Function(f) => {
                // A function used as a value has its parameter modes in its
                // type (`Fn(ref i64) -> String`), which a caller matches.
                let params: &[Param] = if cx.fn_values.function(&f.name) {
                    &[]
                } else {
                    &f.params
                };
                cx.function(params, f.return_type.as_ref(), Some(&f.body))
            }
            // A trait's signature and its impls' must agree, so their
            // parameters keep the mode they are written with.
            Item::ImplBlock(imp) => {
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        let params: &[Param] =
                            if imp.trait_name.is_some() || cx.fn_values.method(&m.name) {
                                &[]
                            } else {
                                &m.params
                            };
                        cx.function(params, m.return_type.as_ref(), Some(&m.body));
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let TraitItem::Method(m) = it {
                        cx.function(&[], m.return_type.as_ref(), m.body.as_ref());
                    }
                }
            }
            _ => {}
        }
    }
    cx.out.sort_by_key(|h| h.span.offset);
    cx.out
}

struct Cx<'a> {
    typed: &'a TypeCheckResult,
    src: &'a str,
    out: Vec<StyleHit>,
    /// Names written as a value rather than called (`list_repr(v, int_repr)`,
    /// `xs.map(Item.size)`): a path's last segment, or a bare name.
    fn_values: FnValues,
    /// Where an explicit `ref name` / `mut ref name` pattern binding starts.
    ref_bindings: Vec<usize>,
}

/// The names `items` use other than as the callee of a call, kept apart by
/// how they are written: a bare identifier can name only a free function,
/// and a method is named by a path (`Item.size`, `Self.size`).
#[derive(Default)]
struct FnValues {
    bare: std::collections::HashSet<String>,
    paths: std::collections::HashSet<String>,
}

impl FnValues {
    /// Whether a free function of this name may be used as a value.
    fn function(&self, name: &str) -> bool {
        self.bare.contains(name) || self.paths.contains(name)
    }

    /// Whether a method of this name may be used as a value.
    fn method(&self, name: &str) -> bool {
        self.paths.contains(name)
    }
}

/// The names of a function's own parameters: a bare identifier with one of
/// these names in its body reads the parameter, not a function
/// (B-2026-11-14-21, a harness's `for _ in 0..count` beside a method `count`).
fn param_names(params: &[Param]) -> std::collections::HashSet<String> {
    params
        .iter()
        .filter_map(|p| match &p.pattern.kind {
            PatternKind::Binding(name) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

fn functions_used_as_values(items: &[Item]) -> FnValues {
    let mut callees = std::collections::HashSet::new();
    let mut bare = Vec::new();
    let mut paths = Vec::new();
    let mut visit = |params: &[Param], b: &Block| {
        let own = param_names(params);
        crate::span_visitor::for_each_expr_in_block(b, &mut |e| match &e.kind {
            ExprKind::Call { callee, .. } => {
                callees.insert((callee.span.offset, callee.span.length));
            }
            ExprKind::Identifier(n) if !own.contains(n) => bare.push((e.span, n.clone())),
            ExprKind::Path { segments, .. } => {
                if let Some(last) = segments.last() {
                    paths.push((e.span, last.clone()));
                }
            }
            _ => {}
        });
    };
    for item in items {
        match item {
            Item::Function(f) => visit(&f.params, &f.body),
            Item::ImplBlock(imp) => {
                for it in &imp.items {
                    if let ImplItem::Method(m) = it {
                        visit(&m.params, &m.body);
                    }
                }
            }
            Item::TraitDef(t) => {
                for it in &t.items {
                    if let TraitItem::Method(m) = it {
                        if let Some(b) = &m.body {
                            visit(&m.params, b);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let values = |named: Vec<(Span, String)>| {
        named
            .into_iter()
            .filter(|(span, _)| !callees.contains(&(span.offset, span.length)))
            .map(|(_, n)| n)
            .collect()
    };
    FnValues {
        bare: values(bare),
        paths: values(paths),
    }
}

fn end(span: &Span) -> usize {
    span.offset + span.length
}

fn edit(offset: usize, end: usize, replacement: &str) -> TextEdit {
    TextEdit {
        offset,
        length: end - offset,
        replacement: replacement.to_string(),
    }
}

impl Cx<'_> {
    fn text(&self, from: usize, to: usize) -> Option<&str> {
        (from <= to).then(|| self.src.get(from..to)).flatten()
    }

    fn span_text(&self, span: &Span) -> Option<&str> {
        self.text(span.offset, end(span))
    }

    /// The offset just past the `;` that ends a statement whose last
    /// expression ends at `at`, when only whitespace separates them.
    fn semicolon_after(&self, at: usize) -> Option<usize> {
        let rest = self.src.get(at..)?;
        let gap = rest.len() - rest.trim_start().len();
        rest[gap..].starts_with(';').then_some(at + gap + 1)
    }

    fn function(&mut self, params: &[Param], ret: Option<&TypeExpr>, body: Option<&Block>) {
        let Some(body) = body else {
            return;
        };
        // A returned borrow may come from a `ref` parameter, which then
        // keeps its spelling.
        let mut returns_borrow = false;
        if let Some(ret) = ret {
            crate::span_visitor::for_each_type_expr(ret, &mut |t| {
                if matches!(t.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
                    returns_borrow = true;
                }
            });
        }
        if !returns_borrow {
            for p in params {
                self.ref_param(p, body);
            }
        }
        self.needless_return(body);
        let mut blocks: Vec<Block> = vec![body.clone()];
        crate::span_visitor::for_each_block_in_block(body, &mut |b| blocks.push(b.clone()));
        for b in &blocks {
            self.counter_while_loops(b);
        }
        crate::span_visitor::for_each_stmt_in_block(body, &mut |s| self.compound_assign(s));
        crate::span_visitor::for_each_expr_in_block(body, &mut |e| self.literal_method(e));
    }

    /// `x: ref T` → `x: T`, unless the body reads `*x`: a bare parameter of
    /// a `Copy` type is the value itself, which `*` does not apply to.
    fn ref_param(&mut self, p: &Param, body: &Block) {
        let TypeKind::Ref(inner) = &p.ty.kind else {
            return;
        };
        let PatternKind::Binding(name) = &p.pattern.kind else {
            return;
        };
        // An explicit `ref name` binding over a bare parameter cannot go into
        // a tuple as it can over a `ref` one (B-2026-11-06-4), so a parameter
        // a pattern with such a binding takes apart keeps its `ref`.
        if derefs_name(body, name) || destructures_with_ref_binding(body, name, &self.ref_bindings)
        {
            return;
        }
        let Some(gap) = self.text(p.ty.span.offset, inner.span.offset) else {
            return;
        };
        if gap
            .strip_prefix("ref")
            .is_none_or(|ws| ws.is_empty() || !ws.trim().is_empty())
        {
            return;
        }
        self.out.push(StyleHit {
            lint: REDUNDANT_REF_PARAM,
            span: p.ty.span,
            message: "`ref` is redundant on a parameter: a parameter borrows by default, \
                      so `x: ref T` means the same as `x: T`"
                .to_string(),
            edits: vec![edit(p.ty.span.offset, inner.span.offset, "")],
        });
    }

    /// A body whose last statement is `return e;` and that has no tail.
    fn needless_return(&mut self, body: &Block) {
        if body.final_expr.is_some() {
            return;
        }
        let Some(Stmt {
            kind: StmtKind::Expr(ret),
            ..
        }) = body.stmts.last()
        else {
            return;
        };
        let ExprKind::Return(Some(value)) = &ret.kind else {
            return;
        };
        // A value that starts with a block-like expression (`if`, `match`, a
        // block) parses as a statement at the start of a line, not as the
        // tail, so it keeps its `return`.
        if starts_block_like(value) {
            return;
        }
        let Some(gap) = self.text(ret.span.offset, value.span.offset) else {
            return;
        };
        if gap
            .strip_prefix("return")
            .is_none_or(|ws| ws.is_empty() || !ws.trim().is_empty())
        {
            return;
        }
        let Some(semi) = self.semicolon_after(end(&value.span)) else {
            return;
        };
        self.out.push(StyleHit {
            lint: NEEDLESS_RETURN,
            span: ret.span,
            message: "a final `return` is redundant: the body's last expression is its \
                      value, so drop the `return` and the `;`"
                .to_string(),
            edits: vec![
                edit(ret.span.offset, value.span.offset, ""),
                edit(semi - 1, semi, ""),
            ],
        });
    }

    /// `x = x + e` → `x += e` on a primitive number.
    fn compound_assign(&mut self, stmt: &Stmt) {
        let StmtKind::Assign { target, value } = &stmt.kind else {
            return;
        };
        let ExprKind::Binary { op, left, .. } = &value.kind else {
            return;
        };
        let Some(sym) = compound_symbol(op) else {
            return;
        };
        if !is_place(target) {
            return;
        }
        let (Some(t), Some(l)) = (self.span_text(&target.span), self.span_text(&left.span)) else {
            return;
        };
        if t != l {
            return;
        }
        // A number, or a `String` grown with `+`; a user type may have `+`
        // without `+=`.
        let fits = match self.typed.expr_types.get(&SpanKey::from_span(&left.span)) {
            Some(Type::Int(_) | Type::UInt(_) | Type::Float(_)) => true,
            Some(Type::Str) => *op == BinOp::Add,
            _ => false,
        };
        if !fits {
            return;
        }
        // ` = x ` between the target and the operator, then the operator.
        let Some(eq) = self.text(end(&target.span), left.span.offset) else {
            return;
        };
        if eq.trim() != "=" {
            return;
        }
        let Some(after) = self.src.get(end(&left.span)..) else {
            return;
        };
        let ws = after.len() - after.trim_start().len();
        if !after[ws..].starts_with(sym) || after[ws + sym.len()..].starts_with('=') {
            return;
        }
        let op_end = end(&left.span) + ws + sym.len();
        self.out.push(StyleHit {
            lint: MANUAL_COMPOUND_ASSIGN,
            span: target.span,
            message: format!("`x = x {sym} e` is `x {sym}= e`"),
            edits: vec![edit(end(&target.span), op_end, &format!(" {sym}="))],
        });
    }

    /// `"text".to_string()` → `"text"`, and `for x in [..].iter()` →
    /// `for x in [..]`.
    fn literal_method(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::MethodCall {
                object,
                method,
                turbofish: None,
                args,
                args_close_span,
                ..
            } if method == "to_string"
                && args.is_empty()
                && matches!(
                    object.kind,
                    ExprKind::StringLit(_) | ExprKind::InterpolatedStringLit(_)
                ) =>
            {
                self.drop_call(object, args_close_span, "`.to_string()` on a string literal is redundant: a string literal is already a `String`");
            }
            ExprKind::For {
                iterable,
                pattern,
                body,
                ..
            } => {
                // The two spellings bind the item differently enough that
                // `*x` in the body can type under one and not the other.
                let PatternKind::Binding(name) = &pattern.kind else {
                    return;
                };
                if derefs_name(body, name) {
                    return;
                }
                if let ExprKind::MethodCall {
                    object,
                    method,
                    turbofish: None,
                    args,
                    args_close_span,
                    ..
                } = &iterable.kind
                {
                    if method == "iter"
                        && args.is_empty()
                        && matches!(object.kind, ExprKind::ArrayLiteral(_))
                    {
                        self.drop_call(
                            object,
                            args_close_span,
                            "`.iter()` on an array literal is redundant: `for` borrows a collection's items itself",
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn drop_call(&mut self, object: &Expr, close: &Span, message: &str) {
        let (from, to) = (end(&object.span), end(close));
        // `.name()` and nothing else between the receiver and the `)`.
        let Some(call) = self.text(from, to) else {
            return;
        };
        let compact: String = call.chars().filter(|c| !c.is_whitespace()).collect();
        if !matches!(compact.as_str(), ".to_string()" | ".iter()") || close.length == 0 {
            return;
        }
        self.out.push(StyleHit {
            lint: REDUNDANT_LITERAL_METHOD,
            span: Span {
                length: to - object.span.offset,
                ..object.span
            },
            message: message.to_string(),
            edits: vec![edit(from, to, "")],
        });
    }

    /// `let mut i = a; while i < b { ...; i += 1; }` with `i` unused after
    /// the loop, the counter written only by that last step, no `continue`
    /// in the body, and `b` unchanged by it.
    fn counter_while_loops(&mut self, block: &Block) {
        for (k, pair) in block.stmts.windows(2).enumerate() {
            let rest = &block.stmts[k + 2..];
            if let Some(hit) = self.counter_while(&pair[0], &pair[1], rest, block) {
                self.out.push(hit);
            }
        }
    }

    fn counter_while(
        &self,
        let_stmt: &Stmt,
        while_stmt: &Stmt,
        rest: &[Stmt],
        block: &Block,
    ) -> Option<StyleHit> {
        let StmtKind::Let {
            is_mut: true,
            pattern,
            ty: None,
            value: start,
        } = &let_stmt.kind
        else {
            return None;
        };
        let PatternKind::Binding(i) = &pattern.kind else {
            return None;
        };
        let StmtKind::Expr(w) = &while_stmt.kind else {
            return None;
        };
        let ExprKind::While {
            label: None,
            condition,
            body,
            attributes,
        } = &w.kind
        else {
            return None;
        };
        if !attributes.is_empty() || body.final_expr.is_some() {
            return None;
        }
        let ExprKind::Binary {
            op: op @ (BinOp::Lt | BinOp::LtEq),
            left,
            right: bound,
        } = &condition.kind
        else {
            return None;
        };
        if !matches!(&left.kind, ExprKind::Identifier(n) if n == i) {
            return None;
        }
        // The step: `i += 1;` as the body's last statement.
        let (step, others) = body.stmts.split_last()?;
        let StmtKind::CompoundAssign {
            target,
            op: CompoundOp::Add,
            value: one,
        } = &step.kind
        else {
            return None;
        };
        if !matches!(&target.kind, ExprKind::Identifier(n) if n == i)
            || !matches!(one.kind, ExprKind::Integer(1, None))
        {
            return None;
        }
        if !binds_tighter_than_range(start) || !is_invariant_bound(bound) {
            return None;
        }
        // The counter's and the bound's types agree, so the range types `i`
        // as the `let` did.
        let ty_of = |e: &Expr| self.typed.expr_types.get(&SpanKey::from_span(&e.span));
        let (ti, tb) = (ty_of(left)?, ty_of(bound)?);
        if ti != tb || !matches!(ti, Type::Int(_) | Type::UInt(_)) {
            return None;
        }
        // Nothing else in the body writes `i` or anything the bound reads,
        // and no `continue` skips the step.
        let mut bound_roots = Vec::new();
        place_roots(bound, &mut bound_roots);
        let mut written = false;
        let mut continues = false;
        for s in others {
            let probe = Block {
                stmts: vec![s.clone()],
                final_expr: None,
                span: s.span,
            };
            crate::span_visitor::for_each_stmt_in_block(&probe, &mut |s| {
                if let StmtKind::Assign { target, .. } | StmtKind::CompoundAssign { target, .. } =
                    &s.kind
                {
                    let mut roots = Vec::new();
                    place_roots(target, &mut roots);
                    if roots.iter().any(|r| r == i || bound_roots.contains(r)) {
                        written = true;
                    }
                }
            });
            crate::span_visitor::for_each_expr_in_block(&probe, &mut |e| match &e.kind {
                ExprKind::Continue { .. } => continues = true,
                ExprKind::MethodCall { object, args, .. } => {
                    // A method on what the bound reads may change it.
                    let mut roots = Vec::new();
                    place_roots(object, &mut roots);
                    if args.iter().any(|a| a.mut_marker)
                        || roots.iter().any(|r| bound_roots.contains(r))
                    {
                        written = true;
                    }
                }
                ExprKind::Call { args, .. } if args.iter().any(|a| a.mut_marker) => {
                    written = true;
                }
                _ => {}
            });
        }
        if written || continues {
            return None;
        }
        // `i` is the loop's own after the rewrite, so nothing after it may
        // read the `let`'s binding.
        let tail = Block {
            stmts: rest.to_vec(),
            final_expr: block.final_expr.clone(),
            span: block.span,
        };
        let mut used_after = false;
        crate::span_visitor::for_each_expr_in_block(&tail, &mut |e| {
            if matches!(&e.kind, ExprKind::Identifier(n) if n == i) {
                used_after = true;
            }
        });
        if used_after {
            return None;
        }
        // The text the edits replace is exactly `let mut i = ` and
        // `; while i < `, so no comment is lost.
        let head = self.text(let_stmt.span.offset, start.span.offset)?;
        let head_words: Vec<&str> = head.split_whitespace().collect();
        if head_words != ["let", "mut", i.as_str(), "="] {
            return None;
        }
        let between = self.text(end(&start.span), bound.span.offset)?;
        let cmp = if *op == BinOp::Lt { "<" } else { "<=" };
        let compact: String = between.chars().filter(|c| !c.is_whitespace()).collect();
        if compact != format!(";while{i}{cmp}") || !between.contains("while ") {
            return None;
        }
        let range = if *op == BinOp::Lt { ".." } else { "..=" };
        let step_edit = self.step_deletion(step)?;
        Some(StyleHit {
            lint: COUNTER_WHILE_LOOP,
            span: w.span,
            message: format!(
                "this `while` counts `{i}` up by one to a bound; write it as `for {i} in a{range}b`"
            ),
            edits: vec![
                edit(
                    let_stmt.span.offset,
                    start.span.offset,
                    &format!("for {i} in "),
                ),
                edit(end(&start.span), bound.span.offset, range),
                step_edit,
            ],
        })
    }

    /// Delete the step statement `i += 1;`, with its line when it has one to
    /// itself.
    fn step_deletion(&self, step: &Stmt) -> Option<TextEdit> {
        let StmtKind::CompoundAssign { value, .. } = &step.kind else {
            return None;
        };
        let semi = self.semicolon_after(end(&value.span))?;
        let start = step.span.offset;
        let line_start = self.src[..start].rfind('\n').map_or(0, |n| n + 1);
        let after = &self.src[semi..];
        let line_end = after.find('\n').map(|n| semi + n + 1);
        match line_end {
            Some(le)
                if self.src[line_start..start].trim().is_empty()
                    && self.src[semi..le].trim().is_empty() =>
            {
                Some(edit(line_start, le, ""))
            }
            _ => Some(edit(start, semi, "")),
        }
    }
}

/// Whether `body` reads `*name`.
fn derefs_name(body: &Block, name: &str) -> bool {
    let mut derefs = false;
    crate::span_visitor::for_each_expr_in_block(body, &mut |e| {
        if let ExprKind::Unary {
            op: UnaryOp::Deref,
            operand,
        } = &e.kind
        {
            if matches!(&operand.kind, ExprKind::Identifier(n) if n == name) {
                derefs = true;
            }
        }
    });
    derefs
}

/// Whether a pattern in `body` takes apart a place rooted at `name`: a
/// `match`, `if let`, `while let` or `let`-`else` scrutinee, or the value of
/// a destructuring `let`.
fn destructures_with_ref_binding(body: &Block, name: &str, ref_bindings: &[usize]) -> bool {
    let rooted = |e: &Expr| {
        let mut roots = Vec::new();
        place_roots(e, &mut roots);
        roots.iter().any(|r| r == name)
    };
    let has_ref = |p: &Pattern| {
        let (start, end) = (p.span.offset, p.span.offset + p.span.length);
        ref_bindings.iter().any(|&o| start <= o && o < end)
    };
    let mut found = false;
    crate::span_visitor::for_each_expr_in_block(body, &mut |e| match &e.kind {
        ExprKind::Match { scrutinee, arms } if rooted(scrutinee) => {
            found |= arms.iter().any(|a| has_ref(&a.pattern));
        }
        ExprKind::IfLet { pattern, value, .. } | ExprKind::WhileLet { pattern, value, .. }
            if rooted(value) =>
        {
            found |= has_ref(pattern);
        }
        _ => {}
    });
    crate::span_visitor::for_each_stmt_in_block(body, &mut |s| match &s.kind {
        StmtKind::LetElse { pattern, value, .. } | StmtKind::Let { pattern, value, .. }
            if rooted(value) =>
        {
            found |= has_ref(pattern);
        }
        _ => {}
    });
    found
}

fn compound_symbol(op: &BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        _ => return None,
    })
}

/// A plain place: a name, `self`, or a field of one.
fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } => is_place(object),
        _ => false,
    }
}

/// The names a place expression is rooted at (`a.b` → `a`, `v[i]` → `v`).
fn place_roots(e: &Expr, out: &mut Vec<String>) {
    match &e.kind {
        ExprKind::Identifier(n) => out.push(n.clone()),
        ExprKind::SelfValue => out.push("self".to_string()),
        ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. } => place_roots(object, out),
        ExprKind::MethodCall { object, .. } => place_roots(object, out),
        ExprKind::Unary { operand, .. } => place_roots(operand, out),
        _ => {}
    }
}

/// A bound `while` re-reads each turn that a range reads once: a literal, a
/// name, a field, a `.len()`, or arithmetic over those.
fn is_invariant_bound(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(..) | ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } => is_place(object),
        ExprKind::MethodCall {
            object,
            method,
            args,
            ..
        } => method == "len" && args.is_empty() && is_place(object),
        ExprKind::Binary {
            op: BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div,
            left,
            right,
        } => is_invariant_bound(left) && is_invariant_bound(right),
        _ => false,
    }
}

/// An expression that stays one operand when written before `..`.
fn binds_tighter_than_range(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Integer(..)
        | ExprKind::Identifier(_)
        | ExprKind::FieldAccess { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::Call { .. }
        | ExprKind::Index { .. } => true,
        ExprKind::Binary { op, .. } => matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ),
        _ => false,
    }
}

/// Whether the leftmost operand of `e` is block-like, so that `e` written
/// at the start of a statement would end after that operand.
fn starts_block_like(e: &Expr) -> bool {
    if is_block_like(e) {
        return true;
    }
    match &e.kind {
        ExprKind::Binary { left, .. }
        | ExprKind::NilCoalesce { left, .. }
        | ExprKind::Pipe { left, .. } => starts_block_like(left),
        ExprKind::MethodCall { object, .. }
        | ExprKind::FieldAccess { object, .. }
        | ExprKind::TupleIndex { object, .. }
        | ExprKind::Index { object, .. }
        | ExprKind::OptionalChain { object, .. } => starts_block_like(object),
        ExprKind::Call { callee, .. } => starts_block_like(callee),
        ExprKind::Cast { expr, .. } => starts_block_like(expr),
        ExprKind::Question(inner) => starts_block_like(inner),
        ExprKind::Range {
            start: Some(start), ..
        } => starts_block_like(start),
        _ => false,
    }
}

fn is_block_like(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Match { .. }
            | ExprKind::Block(_)
            | ExprKind::Loop { .. }
            | ExprKind::While { .. }
            | ExprKind::WhileLet { .. }
            | ExprKind::For { .. }
            | ExprKind::Unsafe(_)
            | ExprKind::LabeledBlock { .. }
    )
}
