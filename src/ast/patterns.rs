//! Pattern AST — patterns, range patterns, slice patterns, literal
//! patterns, match arms.

use crate::token::{FloatSuffix, IntSuffix, Span};

use super::Expr;

// ── Patterns ─────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

impl Pattern {
    /// Collect all binding names from this pattern.
    pub fn binding_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        self.collect_bindings(&mut names);
        names
    }

    /// Sibling to `binding_names` that pairs each binding name with the
    /// `Span` of the leaf pattern that introduced it. Source-order is
    /// identical to `binding_names`. Consumed by phase 6 line 26 slice 4
    /// (state-struct layout synthesis) to look up each captured local's
    /// surface type via the typechecker's `pattern_binding_types` map.
    /// For `Slice { rest: Bound(name) }` and `Struct { fields }` shorthand
    /// fields, the leaf's own span is unavailable from the AST shape;
    /// the parent pattern's span is used as a best-effort proxy (the
    /// typechecker's `pattern_binding_types` keys those bindings off
    /// their leaf-binding pattern spans, so those proxies miss type
    /// records — codegen falls through to primitive sizing on `None`).
    pub fn binding_name_spans(&self) -> Vec<(String, Span)> {
        let mut out = Vec::new();
        self.collect_binding_name_spans(&mut out);
        out
    }

    fn collect_bindings(&self, out: &mut Vec<String>) {
        match &self.kind {
            PatternKind::Binding(name) => out.push(name.clone()),
            PatternKind::Tuple(patterns) => {
                for p in patterns {
                    p.collect_bindings(out);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(ref sub) = f.pattern {
                        sub.collect_bindings(out);
                    } else {
                        out.push(f.name.clone());
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for p in patterns {
                    p.collect_bindings(out);
                }
            }
            PatternKind::Or(alts) => {
                if let Some(first) = alts.first() {
                    first.collect_bindings(out);
                }
            }
            PatternKind::AtBinding { name, pattern } => {
                out.push(name.clone());
                pattern.collect_bindings(out);
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix {
                    p.collect_bindings(out);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    out.push(name.clone());
                }
                for p in suffix {
                    p.collect_bindings(out);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }

    fn collect_binding_name_spans(&self, out: &mut Vec<(String, Span)>) {
        match &self.kind {
            PatternKind::Binding(name) => out.push((name.clone(), self.span.clone())),
            PatternKind::Tuple(patterns) => {
                for p in patterns {
                    p.collect_binding_name_spans(out);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for f in fields {
                    if let Some(ref sub) = f.pattern {
                        sub.collect_binding_name_spans(out);
                    } else {
                        out.push((f.name.clone(), f.span.clone()));
                    }
                }
            }
            PatternKind::TupleVariant { patterns, .. } => {
                for p in patterns {
                    p.collect_binding_name_spans(out);
                }
            }
            PatternKind::Or(alts) => {
                if let Some(first) = alts.first() {
                    first.collect_binding_name_spans(out);
                }
            }
            PatternKind::AtBinding { name, pattern } => {
                out.push((name.clone(), self.span.clone()));
                pattern.collect_binding_name_spans(out);
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix {
                    p.collect_binding_name_spans(out);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    out.push((name.clone(), self.span.clone()));
                }
                for p in suffix {
                    p.collect_binding_name_spans(out);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
        }
    }
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Binding(String),
    Literal(LiteralPattern),
    // `a..=b` → start=Some, end=Some
    // `..=b`  → start=None, end=Some
    // `a..`   → start=Some, end=None
    // bare `..` is rejected (not a valid pattern; use `_`)
    RangePattern {
        start: Option<LiteralPattern>,
        end: Option<LiteralPattern>,
        inclusive: bool,
    },
    AtBinding {
        name: String,
        pattern: Box<Pattern>,
    },
    Struct {
        path: Vec<String>,
        fields: Vec<FieldPattern>,
        /// `..` rest-binding marker — `true` when the pattern ends with
        /// `..` after the (possibly empty) field list, signalling
        /// "match any remaining fields without binding them". The
        /// presence of `..` flips the pattern from exhaustive to
        /// open: missing-field checking is suppressed in the
        /// typechecker, and `#[non_exhaustive]` cross-package struct
        /// patterns require `..` (slice 4 pattern half — see
        /// `phase-5-diagnostics.md` § `#[non_exhaustive]` for
        /// Evolvable Public Types).
        has_rest: bool,
    },
    TupleVariant {
        path: Vec<String>,
        patterns: Vec<Pattern>,
    },
    Tuple(Vec<Pattern>),
    Or(Vec<Pattern>),
    /// `[p1, p2, ..rest, p_n-1, p_n]` — `prefix`/`suffix` are leading/trailing
    /// element patterns, `rest` is the optional `..` or `..name` marker. At
    /// most one `..` is permitted per slice pattern (enforced at parse time).
    /// Sub-item 1 of the slice/array patterns entry (phase 5.2): AST shape
    /// lands now; typechecker rejects with a focused stub diagnostic until
    /// sub-item 2 lands.
    Slice {
        prefix: Vec<Pattern>,
        rest: Option<RestPattern>,
        suffix: Vec<Pattern>,
    },
}

/// Rest marker inside a slice pattern. `..` is `Ignored`; `..name` is
/// `Bound(name)` and introduces a fresh binding into the arm scope.
#[derive(Debug, Clone)]
pub enum RestPattern {
    Ignored,
    Bound(String),
}

#[derive(Debug, Clone)]
pub enum LiteralPattern {
    Integer(i64, Option<IntSuffix>),
    Float(f64, Option<FloatSuffix>),
    Char(char),
    String(String),
    Bool(bool),
}

#[derive(Debug, Clone)]
pub struct FieldPattern {
    pub name: String,
    pub pattern: Option<Pattern>,
    pub span: Span,
}

// ── Match ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}
