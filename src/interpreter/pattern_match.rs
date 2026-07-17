//! Match expression evaluation + pattern try/bind helpers.
//!
//! Houses `eval_match` (the entry from `eval_expr_inner` /
//! `eval_stmt_cf`), `try_match_pattern` (read-only pattern probe —
//! does this value match without binding?), `bind_pattern` (the
//! bind half — push pattern bindings into the current scope on a
//! known-match), and the two pattern helpers
//! `value_in_range_pattern` and `literal_to_value`.
//!
//! Lives in a sibling `impl<'a> super::Interpreter<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::exec::slice_pattern_view;
use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    // ── Match evaluation ────────────────────────────────────────

    pub(crate) fn eval_match(
        &mut self,
        scrutinee: &Value,
        arms: &[MatchArm],
        span: &Span,
    ) -> Value {
        for arm in arms {
            if self.try_match_pattern(&arm.pattern, scrutinee) {
                // Check guard if present
                if let Some(ref guard) = arm.guard {
                    self.env.push_scope();
                    self.bind_pattern(&arm.pattern, scrutinee.clone());
                    let guard_val = self.eval_expr_inner(guard);
                    self.env.pop_scope();
                    if !self.is_truthy(&guard_val) {
                        continue;
                    }
                }
                self.env.push_scope();
                self.bind_pattern(&arm.pattern, scrutinee.clone());
                let result = self.eval_expr_inner(&arm.body);
                self.env.pop_scope();
                return result;
            }
        }
        // Defense in depth: the typechecker's exhaustiveness check plus the
        // pattern-scrutinee-mismatch gate (B-2026-07-17-6) should make this
        // path unreachable, but a future front-end gap must degrade to a
        // clean runtime diagnostic rather than panic the whole process (the
        // old `unreachable!` turned an accepted-but-wrong program into a Rust
        // backtrace instead of a Kāra error).
        self.record_runtime_error(
            format!(
                "internal error: non-exhaustive match at {}:{} — no arm matched \
                 the scrutinee value (the typechecker should have rejected this)",
                span.line, span.column
            ),
            span,
        )
    }

    // ── Pattern matching ────────────────────────────────────────

    pub(crate) fn try_match_pattern(&self, pattern: &Pattern, value: &Value) -> bool {
        match &pattern.kind {
            PatternKind::Wildcard => true,
            PatternKind::Binding(name) => {
                // A `Binding` node doubles as a unit-variant pattern. The name
                // may be dotted (`Side.Left`) or bare (`Left`). A dotted name
                // is unambiguously a variant — a real value binding can never
                // contain `.` — so we strip the enum prefix and compare the
                // last segment to the scrutinee's tag. A bare name is a
                // variant only when one is in scope (otherwise it's a true
                // binding that matches anything). Before this, dotted names
                // failed the `env.get(name)` lookup and fell through to the
                // catch-all `true`, so `Side.Left` matched EVERY value (a
                // silent wrong-arm-selection bug for any enum with >1 unit
                // variant matched by dotted name).
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                // A bare name is a unit-variant pattern only when it is
                // PascalCase — Kāra's case-class invariant (design.md) makes
                // Type/variant identifiers PascalCase and value bindings
                // snake_case, so a lowercase name is ALWAYS a fresh binding,
                // never a variant. Gating on this is load-bearing: the prior
                // heuristic checked only `env.get(name)`, which also matches an
                // ordinary local that happens to hold a unit-variant *value*
                // (e.g. `let c = Color.Green` shadowing the binding `c` inside
                // `match m { Info(c) => … }`) — that made the constructor's
                // sub-binding misfire as a unit-variant test, so the arm failed
                // to match/bind and surfaced as a spurious runtime
                // "non-exhaustive match". A dotted name (`Side.Left`) is
                // unambiguously a variant.
                let bare_could_be_variant = variant_name
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_uppercase());
                let is_unit_variant = name.contains('.')
                    || (bare_could_be_variant
                        && matches!(
                            self.env.get(variant_name),
                            Some(Value::EnumVariant {
                                data: EnumData::Unit,
                                ..
                            })
                        ));
                if is_unit_variant {
                    if let Value::EnumVariant { variant: v2, .. } = value {
                        return variant_name == v2.as_str();
                    }
                    return false;
                }
                true // actual binding — matches anything
            }
            PatternKind::Literal(lit) => {
                let lit_val = self.literal_to_value(lit);
                lit_val == *value
            }
            PatternKind::TupleVariant { path, patterns } => {
                let variant_name = path.last().cloned().unwrap_or_default();
                match value {
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Tuple(vals),
                        ..
                    } => {
                        variant == &variant_name
                            && patterns.len() == vals.len()
                            && patterns
                                .iter()
                                .zip(vals)
                                .all(|(p, v)| self.try_match_pattern(p, v))
                    }
                    _ => false,
                }
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest: _, // The runtime matcher checks each named field's
                             // sub-pattern. Unlisted fields are unconstrained
                             // whether `..` is present or not — the matcher
                             // never required all fields to be enumerated —
                             // so `has_rest` is a typechecker concern only.
            } => {
                let name = path.last().cloned().unwrap_or_default();
                match value {
                    Value::Struct {
                        name: sn,
                        fields: sfields,
                    } if *sn == name => fields.iter().all(|fp| {
                        if let Some(val) = sfields.get(&fp.name) {
                            if let Some(ref sub) = fp.pattern {
                                self.try_match_pattern(sub, val)
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    }),
                    Value::EnumVariant {
                        variant,
                        data: EnumData::Struct(sfields),
                        ..
                    } if *variant == name => fields.iter().all(|fp| {
                        if let Some(val) = sfields.get(&fp.name) {
                            if let Some(ref sub) = fp.pattern {
                                self.try_match_pattern(sub, val)
                            } else {
                                true
                            }
                        } else {
                            false
                        }
                    }),
                    _ => false,
                }
            }
            PatternKind::Tuple(patterns) => match value {
                Value::Tuple(vals) => {
                    patterns.len() == vals.len()
                        && patterns
                            .iter()
                            .zip(vals)
                            .all(|(p, v)| self.try_match_pattern(p, v))
                }
                _ => false,
            },
            PatternKind::Or(alternatives) => alternatives
                .iter()
                .any(|p| self.try_match_pattern(p, value)),
            PatternKind::RangePattern {
                start,
                end,
                inclusive,
            } => self.value_in_range_pattern(value, start.as_ref(), end.as_ref(), *inclusive),
            PatternKind::AtBinding { pattern, .. } => self.try_match_pattern(pattern, value),
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((storage, offset, total_len, _)) = slice_pattern_view(value) else {
                    return false;
                };
                let min_len = prefix.len() + suffix.len();
                if rest.is_none() {
                    if total_len != min_len {
                        return false;
                    }
                } else if total_len < min_len {
                    return false;
                }
                let storage_read = storage.read().unwrap();
                for (i, sub) in prefix.iter().enumerate() {
                    if !self.try_match_pattern(sub, &storage_read[offset + i]) {
                        return false;
                    }
                }
                for (i, sub) in suffix.iter().enumerate() {
                    let idx = offset + total_len - suffix.len() + i;
                    if !self.try_match_pattern(sub, &storage_read[idx]) {
                        return false;
                    }
                }
                true
            }
        }
    }

    pub(crate) fn bind_pattern(&mut self, pattern: &Pattern, value: Value) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Binding(name) => {
                // Don't create a binding for a unit-variant pattern (dotted
                // `Side.Left`, or a bare name resolving to a unit variant in
                // scope) — mirrors the detection in `try_match_pattern`. The
                // dotted case previously fell through and defined a spurious
                // `"Side.Left"` binding.
                let variant_name = name.rsplit('.').next().unwrap_or(name);
                // A bare name is a unit-variant pattern only when it is
                // PascalCase — Kāra's case-class invariant (design.md) makes
                // Type/variant identifiers PascalCase and value bindings
                // snake_case, so a lowercase name is ALWAYS a fresh binding,
                // never a variant. Gating on this is load-bearing: the prior
                // heuristic checked only `env.get(name)`, which also matches an
                // ordinary local that happens to hold a unit-variant *value*
                // (e.g. `let c = Color.Green` shadowing the binding `c` inside
                // `match m { Info(c) => … }`) — that made the constructor's
                // sub-binding misfire as a unit-variant test, so the arm failed
                // to match/bind and surfaced as a spurious runtime
                // "non-exhaustive match". A dotted name (`Side.Left`) is
                // unambiguously a variant.
                let bare_could_be_variant = variant_name
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_uppercase());
                let is_unit_variant = name.contains('.')
                    || (bare_could_be_variant
                        && matches!(
                            self.env.get(variant_name),
                            Some(Value::EnumVariant {
                                data: EnumData::Unit,
                                ..
                            })
                        ));
                if is_unit_variant {
                    return;
                }
                self.env.define(name.clone(), value);
            }
            PatternKind::Literal(_) => {}
            PatternKind::TupleVariant { patterns, .. } => {
                if let Value::EnumVariant {
                    data: EnumData::Tuple(vals),
                    ..
                } = value
                {
                    for (p, v) in patterns.iter().zip(vals) {
                        self.bind_pattern(p, v);
                    }
                }
            }
            PatternKind::Struct { fields, .. } => {
                let field_vals = match value {
                    Value::Struct { fields: f, .. } => f,
                    Value::EnumVariant {
                        data: EnumData::Struct(f),
                        ..
                    } => f,
                    _ => return,
                };
                for fp in fields {
                    if let Some(val) = field_vals.get(&fp.name) {
                        if let Some(ref sub) = fp.pattern {
                            self.bind_pattern(sub, val.clone());
                        } else {
                            self.env.define(fp.name.clone(), val.clone());
                        }
                    }
                }
            }
            PatternKind::Tuple(patterns) => {
                if let Value::Tuple(vals) = value {
                    for (p, v) in patterns.iter().zip(vals) {
                        self.bind_pattern(p, v);
                    }
                }
            }
            PatternKind::Or(alternatives) => {
                // Bind from first matching alternative
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value);
                }
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                self.env.define(name.clone(), value.clone());
                self.bind_pattern(pattern, value);
            }
            PatternKind::RangePattern { .. } => {}
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let Some((storage, offset, total_len, source_mutable)) = slice_pattern_view(&value)
                else {
                    return;
                };
                let prefix_vals: Vec<Value>;
                let suffix_vals: Vec<Value>;
                {
                    let storage_read = storage.read().unwrap();
                    prefix_vals = (0..prefix.len())
                        .map(|i| storage_read[offset + i].clone())
                        .collect();
                    suffix_vals = (0..suffix.len())
                        .map(|i| storage_read[offset + total_len - suffix.len() + i].clone())
                        .collect();
                }
                for (sub, val) in prefix.iter().zip(prefix_vals) {
                    self.bind_pattern(sub, val);
                }
                for (sub, val) in suffix.iter().zip(suffix_vals) {
                    self.bind_pattern(sub, val);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    let rest_start = offset + prefix.len();
                    let rest_len = total_len - prefix.len() - suffix.len();
                    let rest_value = Value::Slice {
                        storage,
                        start: rest_start,
                        len: rest_len,
                        mutable: source_mutable,
                    };
                    self.env.define(name.clone(), rest_value);
                }
            }
        }
    }
    /// Match `value` against a range pattern with optional `start` / `end`
    /// bounds. Bounds are integer or char literals (the parser limits
    /// `LiteralPattern` in range position to those two forms). Half-open
    /// forms — `lo..` (`end = None`), `..hi` (`start = None`) — accept
    /// everything past the present bound. Bounded-exclusive (`lo..hi`),
    /// bounded-inclusive (`lo..=hi`), and the half-open inclusive form
    /// (`..=hi`) all share the same comparison.
    fn value_in_range_pattern(
        &self,
        value: &Value,
        start: Option<&RangeBound>,
        end: Option<&RangeBound>,
        inclusive: bool,
    ) -> bool {
        // Project the scrutinee value into a sortable scalar key (i128 to
        // accommodate i64 + char in the same comparison space).
        let key: i128 = match value {
            Value::Int(n) => *n as i128,
            Value::Char(c) => (*c as u32) as i128,
            _ => return false,
        };
        // Resolve a bound to its scalar key. A `Path` bound names a
        // module-level int/char const, bound in `env` at program start;
        // the typechecker already rejected non-const / non-scalar paths,
        // so a `None` here only arises in an already-erroring program.
        let bound_key = |b: &RangeBound| -> Option<i128> {
            match b {
                RangeBound::Literal(LiteralPattern::Integer(n, _)) => Some(*n as i128),
                RangeBound::Literal(LiteralPattern::Char(c)) => Some((*c as u32) as i128),
                RangeBound::Literal(_) => None,
                RangeBound::Path { segments, .. } if segments.len() == 1 => {
                    match self.env.get(&segments[0]) {
                        Some(Value::Int(n)) => Some(n as i128),
                        Some(Value::Char(c)) => Some((c as u32) as i128),
                        _ => None,
                    }
                }
                RangeBound::Path { .. } => None,
            }
        };
        if let Some(lo) = start {
            let Some(lo_key) = bound_key(lo) else {
                return false;
            };
            if key < lo_key {
                return false;
            }
        }
        if let Some(hi) = end {
            let Some(hi_key) = bound_key(hi) else {
                return false;
            };
            if inclusive {
                if key > hi_key {
                    return false;
                }
            } else if key >= hi_key {
                return false;
            }
        }
        true
    }

    fn literal_to_value(&self, lit: &LiteralPattern) -> Value {
        match lit {
            LiteralPattern::Integer(i, _) => Value::Int(*i),
            LiteralPattern::Float(f, _) => Value::Float(*f),
            LiteralPattern::String(s) => Value::String(s.clone()),
            LiteralPattern::Char(c) => Value::Char(*c),
            LiteralPattern::Bool(b) => Value::Bool(*b),
        }
    }
}
