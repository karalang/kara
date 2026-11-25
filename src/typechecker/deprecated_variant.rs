//! `#[deprecated]` on an enum variant (design.md § `#[deprecated]`: every use
//! of the item, including a variant constructor or a pattern, emits the
//! lint; B-2026-11-04-80). The resolver records the payload against the
//! variant's symbol, but a variant is reached as `Enum.Variant` or through a
//! pattern on a known enum, and its bare name is not unique across enums, so
//! the payload is keyed here by `(enum, variant)` from the declarations.

use super::TypeChecker;
use crate::ast::Item;
use crate::token::Span;
use rustc_hash::FxHashMap;

/// `(enum, variant)` to the `#[deprecated]` note and since.
pub(super) type DeprecatedVariants = FxHashMap<(String, String), (Option<String>, Option<String>)>;

impl TypeChecker<'_> {
    /// Warn at `span` when `variant` of `enum_name` is `#[deprecated]`.
    pub(super) fn check_deprecated_variant_use(
        &mut self,
        enum_name: &str,
        variant: &str,
        span: &Span,
    ) {
        if self.deprecated_variants.is_none() {
            let mut table = DeprecatedVariants::default();
            for item in &self.program.items {
                let Item::EnumDef(e) = item else { continue };
                for v in &e.variants {
                    if let Some(d) = &v.deprecation {
                        table.insert(
                            (e.name.clone(), v.name.clone()),
                            (d.note.clone(), d.since.clone()),
                        );
                    }
                }
            }
            self.deprecated_variants = Some(table);
        }
        // `E.A` names the enum: a `#[deprecated]` enum warns at each
        // variant constructor and pattern too (design.md § `#[deprecated]`,
        // B-2026-11-18-26).
        if self.deprecated_enum_names().contains(enum_name) {
            self.check_deprecated_use_at(span, enum_name);
        }
        let Some((note, since)) = self
            .deprecated_variants
            .as_ref()
            .and_then(|t| t.get(&(enum_name.to_string(), variant.to_string())))
            .cloned()
        else {
            return;
        };
        let def = self
            .resolve_result
            .symbol_table
            .lookup_in_scope(crate::resolver::ScopeId(0), enum_name)
            .map(|s| s.span.offset);
        self.emit_deprecated_warning(
            span,
            &format!("{enum_name}.{variant}"),
            def,
            note.as_deref(),
            since.as_deref(),
        );
    }
}

impl TypeChecker<'_> {
    /// The enums of this program marked `#[deprecated]`.
    fn deprecated_enum_names(&mut self) -> &rustc_hash::FxHashSet<String> {
        if self.deprecated_enums.is_none() {
            let set = self
                .program
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::EnumDef(e) if e.deprecation.is_some() => Some(e.name.clone()),
                    _ => None,
                })
                .collect();
            self.deprecated_enums = Some(set);
        }
        self.deprecated_enums.as_ref().unwrap()
    }

    /// Each bound in `gp` and `wc` that names a `#[deprecated]` trait warns
    /// at the bound, and so does an `impl` of one at its trait's name
    /// (design.md § `#[deprecated]`: every use of the item; B-2026-11-18-26).
    pub(super) fn check_deprecated_trait_uses(
        &mut self,
        gp: Option<&crate::ast::GenericParams>,
        wc: Option<&crate::ast::WhereClause>,
        imp: Option<&crate::ast::PathExpr>,
    ) {
        let mut named: Vec<(Span, String)> = Vec::new();
        let mut bound = |b: &crate::ast::TraitBound| {
            if let [name] = b.path.as_slice() {
                named.push((b.span, name.clone()));
            }
        };
        for p in gp.iter().flat_map(|g| &g.params) {
            p.bounds.iter().for_each(&mut bound);
        }
        for c in wc.iter().flat_map(|w| &w.constraints) {
            if let crate::ast::WhereConstraint::TypeBound { bounds, .. } = c {
                bounds.iter().for_each(&mut bound);
            }
        }
        if let Some(path) = imp {
            if let [name] = path.segments.as_slice() {
                named.push((path.span, name.clone()));
            }
        }
        for (span, name) in named {
            if self.env.traits.contains_key(name.as_str()) {
                self.check_deprecated_use_at(&span, &name);
            }
        }
    }
}

impl TypeChecker<'_> {
    /// design.md § Cycles and `weak`, Ergonomics (B-2026-11-04-80): `.unwrap()`
    /// directly on a `weak` read, the slot itself (`node.parent`) or its
    /// `.upgrade()`, warns, and the fix rewrites it to `.expect("…")` so the
    /// invariant that keeps the referent alive is written where it is relied
    /// on. Any other `Option`'s `.unwrap()` is left to an editor action.
    /// `close` is the `)` of the `unwrap()` call.
    pub(super) fn warn_weak_read_unwrap(&mut self, object: &crate::ast::Expr, close: &Span) {
        use crate::ast::ExprKind;
        use crate::resolver::SpanKey;
        let is_weak_read =
            |e: &crate::ast::Expr| self.weak_slot_reads.contains(&SpanKey::from_span(&e.span));
        let receiver_end = match &object.kind {
            ExprKind::MethodCall {
                object: slot,
                method,
                args,
                args_close_span,
                ..
            } if method == "upgrade" && args.is_empty() && is_weak_read(slot) => {
                args_close_span.offset + args_close_span.length
            }
            ExprKind::Identifier(_) | ExprKind::FieldAccess { .. } | ExprKind::Index { .. }
                if is_weak_read(object) =>
            {
                object.span.offset + object.span.length
            }
            _ => {
                // An ordinary Option's: an editor code action, never a
                // check warning (B-2026-11-05-3).
                if self
                    .option_unwrap_receivers
                    .contains(&SpanKey::from_span(&object.span))
                {
                    let end = match &object.kind {
                        ExprKind::MethodCall {
                            args_close_span, ..
                        } => args_close_span.offset + args_close_span.length,
                        _ => object.span.offset + object.span.length,
                    };
                    if let Some(at) = Self::unwrap_text_after(end, close) {
                        self.option_unwrap_sites.push(at);
                    }
                }
                return;
            }
        };
        // The edit needs the written `.unwrap()` to be exactly that text
        // after the receiver; a call split over lines warns at its `)`
        // without one.
        let found = Self::unwrap_text_after(receiver_end, close);
        let contiguous = found.is_some();
        let at = found.unwrap_or(*close);
        // An expression the checker types twice (an assignment's value,
        // `a = a.parent.unwrap()`) warns once (kata 1650).
        if self
            .warnings
            .iter()
            .chain(&self.errors)
            .any(|e| e.span == at && e.lint_name.as_deref() == Some("weak_read_unwrap"))
        {
            return;
        }
        let fix = contiguous.then(|| super::FixIt {
            span: at,
            replacement: "expect(\"the referent is alive\")".to_string(),
        });
        self.type_lint_warning_with_fix(
            "`.unwrap()` on a `weak` read: the read is `None` once the referent is gone, so \
             write the invariant that keeps it alive as `.expect(\"…\")` (design.md § Cycles \
             and `weak`)"
                .to_string(),
            at,
            super::TypeErrorKind::WeakReadUnwrap,
            "weak_read_unwrap",
            fix,
        );
    }

    /// The `unwrap()` text of a call whose receiver ends at `receiver_end`
    /// and whose `)` is `close`, when it is written as exactly `.unwrap()`
    /// (a call split over lines has none).
    fn unwrap_text_after(receiver_end: usize, close: &Span) -> Option<Span> {
        let start = receiver_end + 1;
        let contiguous = close.length == 1
            && close.offset + 1 == start + "unwrap()".len()
            && close.column >= "unwrap(".len();
        contiguous.then(|| Span {
            offset: start,
            length: "unwrap()".len(),
            column: close.column - "unwrap(".len(),
            ..*close
        })
    }
}
