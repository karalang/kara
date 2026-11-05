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
        let Some((note, since)) = self
            .deprecated_variants
            .as_ref()
            .and_then(|t| t.get(&(enum_name.to_string(), variant.to_string())))
            .cloned()
        else {
            return;
        };
        self.emit_deprecated_warning(
            span,
            &format!("{enum_name}.{variant}"),
            note.as_deref(),
            since.as_deref(),
        );
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
