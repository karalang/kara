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
        unreachable!(
            "non-exhaustive match at {}:{}; should be caught by exhaustiveness checker",
            span.line, span.column
        )
    }

    // ── Pattern matching ────────────────────────────────────────

    pub(crate) fn try_match_pattern(&self, pattern: &Pattern, value: &Value) -> bool {
        match &pattern.kind {
            PatternKind::Wildcard => true,
            PatternKind::Binding(name) => {
                // Check if this is actually an enum variant name (unit variant)
                if let Some(Value::EnumVariant {
                    variant,
                    data: EnumData::Unit,
                    ..
                }) = self.env.get(name)
                {
                    if let Value::EnumVariant { variant: v2, .. } = value {
                        return variant == *v2;
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
            } => Self::value_in_range_pattern(value, start.as_ref(), end.as_ref(), *inclusive),
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
                // Don't rebind if this is a unit variant name being used as a pattern
                if let Some(existing) = self.env.get(name) {
                    if matches!(
                        existing,
                        Value::EnumVariant {
                            data: EnumData::Unit,
                            ..
                        }
                    ) {
                        return;
                    }
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
        value: &Value,
        start: Option<&LiteralPattern>,
        end: Option<&LiteralPattern>,
        inclusive: bool,
    ) -> bool {
        // Project the scrutinee value into a sortable scalar key (i128 to
        // accommodate i64 + char in the same comparison space).
        let key: i128 = match value {
            Value::Int(n) => *n as i128,
            Value::Char(c) => (*c as u32) as i128,
            _ => return false,
        };
        let bound_key = |lit: &LiteralPattern| -> Option<i128> {
            match lit {
                LiteralPattern::Integer(n, _) => Some(*n as i128),
                LiteralPattern::Char(c) => Some((*c as u32) as i128),
                _ => None,
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
