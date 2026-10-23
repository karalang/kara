//! v2 core §14.9 (`docs/core-semantics.md`, B-2026-10-20-8): a provider
//! must be `CrossTask` only when `par` branches or tasks inside the
//! `with_provider` share it. Everything a provider's scope can run is either
//! in its body or reached from it through a call, so the provider is shared
//! only if the body, or a function it reaches by name, can start a branch or
//! a task. That is what this answers, conservatively: an instance method is
//! matched by its name across every impl.

use crate::ast::*;
use rustc_hash::FxHashSet;

/// What a body starts or calls, by name.
#[derive(Default)]
struct Reach {
    forks: bool,
    fns: Vec<String>,
    methods: Vec<String>,
}

fn reach_of(e: &Expr, r: &mut Reach) {
    match &e.kind {
        ExprKind::Par(_) | ExprKind::For { par: Some(_), .. } => r.forks = true,
        ExprKind::Call { callee, .. } => match &callee.kind {
            ExprKind::Identifier(n) if n == "spawn" => r.forks = true,
            ExprKind::Identifier(n) => r.fns.push(n.clone()),
            ExprKind::Path { segments, .. } => r.fns.push(segments.join(".")),
            _ => {}
        },
        ExprKind::MethodCall { method, .. } if method == "spawn" => r.forks = true,
        ExprKind::MethodCall { method, .. } => r.methods.push(method.clone()),
        // A function taken as a value may be called by whoever receives it.
        ExprKind::Identifier(n) => r.fns.push(n.clone()),
        _ => {}
    }
}

/// The functions (by name, and methods as `Type.method`) that can start a
/// `par` branch or a task, directly or through a callee, and the bare names
/// of the methods among them.
pub(crate) struct Forking {
    fns: FxHashSet<String>,
    methods: FxHashSet<String>,
}

impl Forking {
    pub(super) fn of(program: &Program) -> Self {
        let mut all: Vec<(String, Option<String>, Reach)> = Vec::new();
        for item in &program.items {
            match item {
                Item::Function(f) => {
                    let mut r = Reach::default();
                    crate::span_visitor::for_each_expr_in_block(&f.body, &mut |e| {
                        reach_of(e, &mut r)
                    });
                    all.push((f.name.clone(), None, r));
                }
                Item::ImplBlock(imp) => {
                    let head = match &imp.target_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    };
                    for it in &imp.items {
                        if let ImplItem::Method(m) = it {
                            let mut r = Reach::default();
                            crate::span_visitor::for_each_expr_in_block(&m.body, &mut |e| {
                                reach_of(e, &mut r)
                            });
                            let key = match &head {
                                Some(h) => format!("{h}.{}", m.name),
                                None => m.name.clone(),
                            };
                            all.push((key, Some(m.name.clone()), r));
                        }
                    }
                }
                _ => {}
            }
        }
        let mut out = Forking {
            fns: FxHashSet::default(),
            methods: FxHashSet::default(),
        };
        loop {
            let mut grew = false;
            for (key, method, r) in &all {
                if !out.fns.contains(key) && out.reaches(r) {
                    out.fns.insert(key.clone());
                    if let Some(m) = method {
                        out.methods.insert(m.clone());
                    }
                    grew = true;
                }
            }
            if !grew {
                return out;
            }
        }
    }

    fn reaches(&self, r: &Reach) -> bool {
        r.forks
            || r.fns.iter().any(|f| self.fns.contains(f))
            || r.methods.iter().any(|m| self.methods.contains(m))
    }

    /// Whether `body` can start a branch or a task that shares the provider.
    pub(super) fn may_share_expr(&self, body: &Expr) -> bool {
        let mut r = Reach::default();
        crate::span_visitor::for_each_expr(body, &mut |e| reach_of(e, &mut r));
        self.reaches(&r)
    }

    /// [`Self::may_share_expr`] for a `providers { } in { body }` block.
    pub(super) fn may_share_block(&self, body: &Block) -> bool {
        let mut r = Reach::default();
        crate::span_visitor::for_each_expr_in_block(body, &mut |e| reach_of(e, &mut r));
        self.reaches(&r)
    }
}

impl super::TypeChecker<'_> {
    /// Whether a provider bound around `body` (or `block`) must be
    /// `CrossTask`: on v2 (the strict commands, and the typecheck the MIR
    /// builder runs) only when the scope can start a branch or a task that
    /// shares it (§14.9); on legacy always, its older rule.
    pub(super) fn provider_may_be_shared(
        &mut self,
        body: Option<&Expr>,
        block: Option<&Block>,
    ) -> bool {
        if !self.cli_lint_overrides.strict_core && !self.library_methods_from_source {
            return true;
        }
        let program = self.program;
        let forking = self.forking.get_or_insert_with(|| Forking::of(program));
        body.is_some_and(|b| forking.may_share_expr(b))
            || block.is_some_and(|b| forking.may_share_block(b))
    }
}
