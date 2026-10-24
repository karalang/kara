//! `select { .. }` (core-semantics §11.11), rewritten before resolution
//! into the constructs every later phase already knows.
//!
//! The parser gives a `select` as `match __select { pat if op => body, ..,
//! _ => default }`: the marker scrutinee [`MARKER`], each arm's operation
//! in its guard, and the `default` arm alone with no guard. This pass turns
//! that into
//!
//! ```text
//! {
//!     // entry: each channel, value and duration, once, in source order
//!     let mut r0 = __select_recv_none(rx);      // a recv arm's result slot
//!     let mut v1 = Some(value);                 // a send arm's value
//!     let mut r1 = __select_send_none(tx);
//!     let a2 = d.__select_at();                 // an after arm's deadline
//!     let until = a2;                           // the nearest, or -1
//!     let k = loop {
//!         let e = __select_epoch();
//!         r0 = __select_recv(rx);        if r0.is_some() { break 0; }
//!         r1 = __select_send(tx, mut v1); if r1.is_some() { break 1; }
//!         if __select_due(a2) { break 2; }
//!         __select_park(e, until);              // or `break 3;` with a default
//!     };
//!     match k {
//!         0 => { let pat0 = r0.unwrap(); body0 }
//!         1 => { let pat1 = r1.unwrap(); body1 }
//!         _ => { let pat2 = (); body2 }
//!     }
//! }
//! ```
//!
//! Each try (`runtime/stdlib/source/sync.kara`) performs its operation when
//! it reports the arm ready, so the arm that runs has received or sent and
//! no other has, and source order decides between arms ready together. The
//! park waits until some channel changes or the nearest deadline passes,
//! and then the arms are tried again. The bodies run outside the loop, so
//! a `break` or `continue` in one still means the loop around the `select`.
//!
//! A channel written as a place (`rx`, `self.inbox`) is used where it is; any
//! other channel expression is bound once on entry. Every node this pass
//! makes carries the `select`'s span.

use std::collections::HashMap;

use crate::ast::*;
use crate::span_visitor::MutVisitor;
use crate::token::Span;

/// The scrutinee the parser gives a `select`.
pub const MARKER: &str = "__select";

/// Rewrites every `select` in `program`.
pub fn desugar_selects(program: &mut Program) {
    let mut pass = Pass { next: 0 };
    for item in &mut program.items {
        crate::span_visitor::walk_item_mut_with(item, &mut pass);
    }
}

struct Pass {
    /// Numbers each `select`, so nested ones name their locals apart.
    next: usize,
}

impl MutVisitor for Pass {
    fn span(&mut self, _span: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        let ExprKind::Match { scrutinee, arms } = &mut e.kind else {
            return;
        };
        if !matches!(&scrutinee.kind, ExprKind::Identifier(n) if n == MARKER) {
            return;
        }
        let n = self.next;
        self.next += 1;
        let arms = std::mem::take(arms);
        *e = rewrite(n, arms, e.span);
    }
}

/// What an arm waits on.
enum Op {
    Recv(Box<Expr>),
    Send(Box<Expr>, Box<Expr>),
    After(Box<Expr>),
    Default,
}

fn op_of(arm: &mut MatchArm) -> Op {
    let Some(guard) = arm.guard.take() else {
        return Op::Default;
    };
    match guard.kind {
        ExprKind::MethodCall {
            object,
            method,
            mut args,
            ..
        } => {
            if method == "send" && args.len() == 1 {
                Op::Send(object, Box::new(args.remove(0).value))
            } else {
                Op::Recv(object)
            }
        }
        ExprKind::Call { mut args, .. } if !args.is_empty() => {
            Op::After(Box::new(args.remove(0).value))
        }
        // The parser accepts nothing else.
        _ => Op::Default,
    }
}

/// A channel expression that names a place, which the arms can use where it
/// is: a binding, `self`, or a field of one.
fn is_place(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Identifier(_) | ExprKind::SelfValue => true,
        ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
            is_place(object)
        }
        _ => false,
    }
}

/// Keeps `e` for the placeholder `name`, and gives the name back.
fn put(exprs: &mut HashMap<String, Expr>, name: String, e: Expr) -> String {
    exprs.insert(name.clone(), e);
    name
}

fn rewrite(n: usize, mut arms: Vec<MatchArm>, span: Span) -> Expr {
    let p = format!("__s{n}");
    let mut exprs: HashMap<String, Expr> = HashMap::new();
    let mut pats: HashMap<String, Pattern> = HashMap::new();
    let mut entry = String::new();
    let mut probes = String::new();
    let mut deadlines: Vec<String> = Vec::new();
    let mut default = None;
    let mut chosen: Vec<(String, usize)> = Vec::new();
    let mut waiting = 0usize;
    for (i, arm) in arms.iter_mut().enumerate() {
        let op = op_of(arm);
        let body = std::mem::replace(
            &mut arm.body,
            Expr {
                kind: ExprKind::Bool(false),
                span,
                id: crate::ids::NodeId::DUMMY,
            },
        );
        put(&mut exprs, format!("{p}_b{i}"), body);
        pats.insert(format!("{p}_p{i}"), arm.pattern.clone());
        let channel = |exprs: &mut HashMap<String, Expr>, entry: &mut String, ch: Expr| {
            let held = put(exprs, format!("{p}_x{i}"), ch.clone());
            if is_place(&ch) {
                held
            } else {
                entry.push_str(&format!("let {p}_c{i} = {held};\n"));
                format!("{p}_c{i}")
            }
        };
        let k = waiting;
        match op {
            Op::Recv(ch) => {
                let ch = channel(&mut exprs, &mut entry, *ch);
                entry.push_str(&format!("let mut {p}_r{i} = __select_recv_none({ch});\n"));
                probes.push_str(&format!(
                    "{p}_r{i} = __select_recv({ch});\nif {p}_r{i}.is_some() {{ break {k}; }}\n"
                ));
                chosen.push((format!("{p}_r{i}.unwrap()"), i));
            }
            Op::Send(ch, v) => {
                let ch = channel(&mut exprs, &mut entry, *ch);
                let v = put(&mut exprs, format!("{p}_val{i}"), *v);
                entry.push_str(&format!("let mut {p}_v{i} = Some({v});\n"));
                entry.push_str(&format!("let mut {p}_r{i} = __select_send_none({ch});\n"));
                probes.push_str(&format!(
                    "{p}_r{i} = __select_send({ch}, mut {p}_v{i});\n\
                     if {p}_r{i}.is_some() {{ break {k}; }}\n"
                ));
                chosen.push((format!("{p}_r{i}.unwrap()"), i));
            }
            Op::After(d) => {
                let d = put(&mut exprs, format!("{p}_dur{i}"), *d);
                entry.push_str(&format!("let {p}_a{i} = ({d}).__select_at();\n"));
                probes.push_str(&format!("if __select_due({p}_a{i}) {{ break {k}; }}\n"));
                deadlines.push(format!("{p}_a{i}"));
                chosen.push(("()".to_string(), i));
            }
            Op::Default => {
                default = Some(i);
                continue;
            }
        }
        waiting += 1;
    }
    let mut until = String::from("-1");
    for (j, a) in deadlines.iter().enumerate() {
        if j == 0 {
            until = a.clone();
        } else {
            entry.push_str(&format!(
                "let {p}_u{j} = if {a} < {until} {{ {a} }} else {{ {until} }};\n"
            ));
            until = format!("{p}_u{j}");
        }
    }
    let rest = match default {
        Some(_) => format!("break {waiting};"),
        None => format!("__select_park({p}_e, {until});"),
    };
    let mut cases = String::new();
    if let Some(d) = default {
        chosen.push((String::new(), d));
    }
    let last = chosen.len() - 1;
    for (k, (value, i)) in chosen.iter().enumerate() {
        let at = if k == last {
            "_".to_string()
        } else {
            k.to_string()
        };
        if Some(*i) == default {
            cases.push_str(&format!("{at} => {{ {p}_b{i} }}\n"));
        } else {
            cases.push_str(&format!("{at} => {{ let {p}_p{i} = {value}; {p}_b{i} }}\n"));
        }
    }
    let src = format!(
        "fn {p}() {{\n{{\n{entry}let {p}_k = loop {{\nlet {p}_e = __select_epoch();\n\
         {probes}{rest}\n}};\nmatch {p}_k {{\n{cases}}}\n}}\n}}\n"
    );
    let parsed = crate::parse(&src);
    debug_assert!(
        parsed.errors.is_empty(),
        "the select template failed to parse: {:?}\n{src}",
        parsed.errors
    );
    let Some(Item::Function(mut f)) = parsed.program.items.into_iter().next() else {
        return Expr {
            kind: ExprKind::Block(Block {
                stmts: Vec::new(),
                final_expr: None,
                span,
            }),
            span,
            id: crate::ids::NodeId::DUMMY,
        };
    };
    crate::span_visitor::walk_function_mut_with(&mut f, &mut Respan(span));
    crate::span_visitor::walk_function_mut_with(&mut f, &mut Fill { exprs, pats });
    match f.body.final_expr {
        Some(e) => *e,
        None => Expr {
            kind: ExprKind::Block(f.body),
            span,
            id: crate::ids::NodeId::DUMMY,
        },
    }
}

/// Gives every node of the template the `select`'s span.
struct Respan(Span);

impl MutVisitor for Respan {
    fn span(&mut self, span: &mut Span) {
        *span = self.0;
    }
}

/// Puts the arms' own expressions and patterns, which keep their spans, in
/// for the template's placeholders.
struct Fill {
    exprs: HashMap<String, Expr>,
    pats: HashMap<String, Pattern>,
}

impl MutVisitor for Fill {
    fn span(&mut self, _span: &mut Span) {}

    fn expr(&mut self, e: &mut Expr) {
        if let ExprKind::Identifier(name) = &e.kind {
            if let Some(user) = self.exprs.get(name) {
                *e = user.clone();
            }
        }
    }

    fn pattern(&mut self, p: &mut Pattern) {
        if let PatternKind::Binding(name) = &p.kind {
            if let Some(user) = self.pats.get(name) {
                *p = user.clone();
            }
        }
    }
}
