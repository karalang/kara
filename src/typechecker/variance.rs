//! Per-stdlib-type variance declarations — design.md § Variance (v60
//! item 57; phase-8-stdlib-floor.md line 364).
//!
//! Three layers live here:
//!
//! 1. **User-side rejection** (`reject_user_variance_markers`) — the
//!    `+T` / `-T` markers are reserved for stdlib parametric *type*
//!    declarations at v1. Markers anywhere else (any user decl; fn /
//!    trait / impl / alias generic params regardless of origin) emit
//!    `E_VARIANCE_USER_DECL_NOT_YET`. Explicit `=T` is identical to the
//!    no-marker default and is accepted everywhere.
//!
//! 2. **Stdlib-side verifier** (`verify_variance`) — for stdlib
//!    struct/enum declarations carrying `+`/`-` markers, walk the
//!    type's *data* structure and prove each marker consistent with the
//!    positions the parameter occupies, per the position-based variance
//!    table (design.md § Variance). Method signatures do NOT constrain
//!    a type's variance: a widening coercion re-types the whole value,
//!    so subsequent method calls resolve at the widened type — only
//!    stored data (fields, payloads) can witness the old parameter.
//!    Violations emit `E_VARIANCE_DECLARATION_INCONSISTENT` with a
//!    fix-it suggesting the next-most-conservative declaration (`=T`).
//!
//! 3. **Stdlib explicit-marker lint** (`lint_stdlib_explicit_variance`)
//!    — every parametric stdlib type must mark every type parameter
//!    explicitly (`+`/`-`/`=`); the implicit default is rejected so
//!    variance is always a deliberate choice in stdlib source. Enforced
//!    on stdlib-origin decls flowing through `check_items` (gated-module
//!    splices) and, for the baked set, by the hygiene tests below.
//!
//! The use-site subtyping rule that *consumes* the declarations lives
//! in `types.rs::types_compatible` (Named-type arm), reading the
//! `prelude::STDLIB_VARIANCE` table.

use crate::ast::*;
use crate::token::Span;

use super::TypeErrorKind;

/// Compose the polarity of an occurrence context with the declared
/// variance of the slot it appears in — the standard variance
/// transform: invariance is absorbing, matching signs are covariant,
/// mixed signs are contravariant.
fn compose(ctx: Variance, slot: Variance) -> Variance {
    use Variance::*;
    match (ctx, slot) {
        (Invariant, _) | (_, Invariant) => Invariant,
        (Covariant, Covariant) | (Contravariant, Contravariant) => Covariant,
        _ => Contravariant,
    }
}

fn marker_str(v: Variance) -> &'static str {
    match v {
        Variance::Covariant => "+",
        Variance::Contravariant => "-",
        Variance::Invariant => "=",
    }
}

fn variance_word(v: Variance) -> &'static str {
    match v {
        Variance::Covariant => "covariant",
        Variance::Contravariant => "contravariant",
        Variance::Invariant => "invariant",
    }
}

/// One occurrence of a tracked parameter inside a type's data
/// structure: the polarity the position imposes plus a human-readable
/// position description for the diagnostic.
struct Occurrence {
    param: String,
    polarity: Variance,
    position: String,
    span: Span,
}

/// Walk a field/payload type expression, recording every occurrence of
/// a tracked parameter with the polarity its position imposes. `ctx`
/// starts at the position's base polarity (covariant for an immutable
/// field, invariant for a `mut` field) and composes inward.
fn collect_occurrences(
    ty: &TypeExpr,
    ctx: Variance,
    tracked: &std::collections::HashMap<String, Variance>,
    position: &str,
    out: &mut Vec<Occurrence>,
) {
    match &ty.kind {
        TypeKind::Path(path) => {
            if path.segments.len() == 1 && path.generic_args.is_none() {
                if tracked.contains_key(&path.segments[0]) {
                    out.push(Occurrence {
                        param: path.segments[0].clone(),
                        polarity: ctx,
                        position: position.to_string(),
                        span: ty.span.clone(),
                    });
                }
                return;
            }
            // Nested named type — each argument slot composes the
            // context with the slot's declared variance. Types without
            // a stdlib variance declaration (every user type, and any
            // stdlib type the table misses) are invariant per slot —
            // the conservative default.
            let name = path.segments.last().map(String::as_str).unwrap_or("");
            let slot_variances = crate::prelude::stdlib_variance(name);
            if let Some(args) = &path.generic_args {
                let mut type_arg_idx = 0usize;
                for arg in args {
                    let GenericArg::Type(arg_ty) = arg else {
                        continue;
                    };
                    let slot = slot_variances
                        .and_then(|v| v.get(type_arg_idx).copied())
                        .unwrap_or(Variance::Invariant);
                    type_arg_idx += 1;
                    collect_occurrences(arg_ty, compose(ctx, slot), tracked, position, out);
                }
            }
        }
        TypeKind::Tuple(elems) => {
            for e in elems {
                collect_occurrences(e, ctx, tracked, position, out);
            }
        }
        // `Array[T; N]` is invariant in T (mutable through `mut ref`).
        TypeKind::Array { element, .. } => {
            collect_occurrences(element, Variance::Invariant, tracked, position, out);
        }
        // `*const T` is covariant (read-only view); `*mut T` invariant.
        TypeKind::Pointer { is_mut, inner } => {
            let inner_ctx = if *is_mut { Variance::Invariant } else { ctx };
            collect_occurrences(inner, inner_ctx, tracked, position, out);
        }
        // Function types: arguments contravariant (flip), return
        // covariant (keep) — position-based table, design.md § Variance.
        TypeKind::FnType {
            params,
            return_type,
            ..
        } => {
            let flipped = compose(ctx, Variance::Contravariant);
            for p in params {
                collect_occurrences(p, flipped, tracked, position, out);
            }
            if let Some(rt) = return_type {
                collect_occurrences(rt, ctx, tracked, position, out);
            }
        }
        // `ref T` target is covariant; `mut ref T` / `mut Slice[T]`
        // targets are invariant (the load-bearing soundness pin).
        TypeKind::Ref(inner) => collect_occurrences(inner, ctx, tracked, position, out),
        TypeKind::MutRef(inner) | TypeKind::MutSlice(inner) => {
            collect_occurrences(inner, Variance::Invariant, tracked, position, out);
        }
        // `weak T` is an Rc-family handle — invariant like `Rc[=T]`.
        TypeKind::Weak(inner) => {
            collect_occurrences(inner, Variance::Invariant, tracked, position, out);
        }
        // Anything else (existential / dyn / future kinds): if a
        // tracked parameter appears inside, stay conservative — treat
        // the whole subtree as invariant context.
        TypeKind::ImplTrait { args, .. } | TypeKind::Dyn { args, .. } => {
            for arg in args {
                if let GenericArg::Type(t) = arg {
                    collect_occurrences(t, Variance::Invariant, tracked, position, out);
                }
            }
        }
        TypeKind::Unit | TypeKind::Error => {}
    }
}

/// Verifier walk list for a struct: every field with its base polarity
/// — covariant for immutable fields, invariant for `mut` fields
/// (read+write slots).
fn struct_walk(s: &StructDef) -> Vec<(String, Variance, &TypeExpr)> {
    s.fields
        .iter()
        .map(|f| {
            let polarity = if f.is_mut {
                Variance::Invariant
            } else {
                Variance::Covariant
            };
            (format!("field '{}'", f.name), polarity, &f.ty)
        })
        .collect()
}

/// Enum twin: variant payloads are produced by construction and
/// extracted by pattern match — covariant base polarity. Struct
/// variants follow field mutability like struct fields.
fn enum_walk(e: &EnumDef) -> Vec<(String, Variance, &TypeExpr)> {
    let mut walk: Vec<(String, Variance, &TypeExpr)> = Vec::new();
    for v in &e.variants {
        match &v.kind {
            VariantKind::Unit => {}
            VariantKind::Tuple(payloads) => {
                for ty in payloads {
                    walk.push((
                        format!("variant '{}' payload", v.name),
                        Variance::Covariant,
                        ty,
                    ));
                }
            }
            VariantKind::Struct(fields) => {
                for f in fields {
                    let polarity = if f.is_mut {
                        Variance::Invariant
                    } else {
                        Variance::Covariant
                    };
                    walk.push((
                        format!("variant '{}' field '{}'", v.name, f.name),
                        polarity,
                        &f.ty,
                    ));
                }
            }
        }
    }
    walk
}

/// Layers 2+3 core, free of `TypeChecker` so the baked-stdlib hygiene
/// tests below can run it directly: the explicit-marker lint plus the
/// structural verifier for any `+`/`-`-marked parameter. Returns
/// `(message, span)` pairs.
fn stdlib_variance_decl_errors(
    generics: &Option<GenericParams>,
    type_name: &str,
    walk: &[(String, Variance, &TypeExpr)],
) -> Vec<(String, Span)> {
    let mut errors = Vec::new();
    let Some(gp) = generics else {
        return errors;
    };
    // Layer 3 — explicit-marker lint: every type parameter of a
    // parametric stdlib type must carry an explicit marker, so variance
    // is always a deliberate choice (the lint rejects ambiguity, not
    // invariance).
    for p in &gp.params {
        if p.is_const || p.is_variadic_shape {
            continue;
        }
        if p.variance_span.is_none() {
            errors.push((
                format!(
                    "error[E_STDLIB_VARIANCE_IMPLICIT]: stdlib parametric type \
                     '{type_name}' must declare explicit variance on parameter \
                     '{n}' — write '={n}' for invariant, '+{n}' for covariant, \
                     or '-{n}' for contravariant (design.md § Variance)",
                    n = p.name,
                ),
                p.span.clone(),
            ));
        }
    }
    // Layer 2 — structural verifier, only for non-invariant markers
    // (`=T` is unrestricted by definition).
    let tracked: std::collections::HashMap<String, Variance> = gp
        .params
        .iter()
        .filter(|p| !p.is_const && !p.is_variadic_shape)
        .filter(|p| p.variance != Variance::Invariant)
        .map(|p| (p.name.clone(), p.variance))
        .collect();
    if tracked.is_empty() {
        return errors;
    }
    let mut occurrences = Vec::new();
    for (position, base_polarity, ty) in walk {
        collect_occurrences(ty, *base_polarity, &tracked, position, &mut occurrences);
    }
    for occ in occurrences {
        let declared = tracked[&occ.param];
        if occ.polarity == declared {
            continue;
        }
        errors.push((
            format!(
                "error[E_VARIANCE_DECLARATION_INCONSISTENT]: parameter '{p}' \
                 declared '{m}{p}' ({dw}) appears in {ow} position '{pos}' \
                 inside type '{ty}'; declare the next-most-conservative \
                 variance instead: '={p}'",
                p = occ.param,
                m = marker_str(declared),
                dw = variance_word(declared),
                ow = variance_word(occ.polarity),
                pos = occ.position,
                ty = type_name,
            ),
            occ.span,
        ));
    }
    errors
}

impl<'a> super::TypeChecker<'a> {
    /// Layer 1 — user-side rejection. `allow` is true only for
    /// stdlib-origin struct/enum declarations (the surfaces the stdlib
    /// audit covers); fn / trait / impl / alias generic params reject
    /// `+`/`-` regardless of origin.
    pub(super) fn reject_user_variance_markers(
        &mut self,
        generics: &Option<GenericParams>,
        allow: bool,
    ) {
        if allow {
            return;
        }
        let Some(gp) = generics else { return };
        for p in &gp.params {
            if p.variance == Variance::Invariant {
                continue;
            }
            let span = p.variance_span.clone().unwrap_or_else(|| p.span.clone());
            self.type_error(
                "error[E_VARIANCE_USER_DECL_NOT_YET]: variance declarations are \
                 reserved for stdlib types in v1; remove the marker — the parameter \
                 will be invariant"
                    .to_string(),
                span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Struct dispatch: user decls get the layer-1 rejection; stdlib
    /// decls (gated-module splices flowing through `check_items`; the
    /// baked set is covered by the hygiene tests below) get the
    /// explicit-marker lint + structural verifier.
    pub(super) fn check_struct_variance(&mut self, s: &StructDef) {
        if !s.stdlib_origin {
            self.reject_user_variance_markers(&s.generic_params, false);
            return;
        }
        for (msg, span) in stdlib_variance_decl_errors(&s.generic_params, &s.name, &struct_walk(s))
        {
            self.type_error(msg, span, TypeErrorKind::TypeMismatch);
        }
    }

    /// Enum twin of [`Self::check_struct_variance`].
    pub(super) fn check_enum_variance(&mut self, e: &EnumDef) {
        if !e.stdlib_origin {
            self.reject_user_variance_markers(&e.generic_params, false);
            return;
        }
        for (msg, span) in stdlib_variance_decl_errors(&e.generic_params, &e.name, &enum_walk(e)) {
            self.type_error(msg, span, TypeErrorKind::TypeMismatch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run lint + verifier over every struct/enum in a parsed program,
    /// treating all of them as stdlib decls.
    fn decl_errors(program: &Program) -> Vec<String> {
        let mut out = Vec::new();
        for item in &program.items {
            match item {
                Item::StructDef(s) => {
                    for (msg, _) in
                        stdlib_variance_decl_errors(&s.generic_params, &s.name, &struct_walk(s))
                    {
                        out.push(msg);
                    }
                }
                Item::EnumDef(e) => {
                    for (msg, _) in
                        stdlib_variance_decl_errors(&e.generic_params, &e.name, &enum_walk(e))
                    {
                        out.push(msg);
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The stdlib hygiene gate (design.md § Variance — "stdlib build
    /// step"): every parametric type in the baked + gated stdlib must
    /// carry explicit variance on every parameter, and every `+`/`-`
    /// declaration must verify against the type's structure. The
    /// stdlib is baked into the compiler binary, so `cargo test` IS
    /// the stdlib build step.
    #[test]
    fn baked_stdlib_variance_declarations_are_explicit_and_consistent() {
        let mut all = Vec::new();
        for (name, program) in crate::prelude::STDLIB_PROGRAMS.iter() {
            for msg in decl_errors(program) {
                all.push(format!("{name}: {msg}"));
            }
        }
        for (path, program) in crate::prelude::GATED_STDLIB_PROGRAMS.iter() {
            for msg in decl_errors(program) {
                all.push(format!("{}: {msg}", path.join(".")));
            }
        }
        assert!(
            all.is_empty(),
            "stdlib variance hygiene failures:\n  {}",
            all.join("\n  "),
        );
    }

    /// Stdlib fn / trait / impl generic params must never carry
    /// variance markers — markers are a nominal-type-decl property.
    /// (User code is policed by `reject_user_variance_markers`; this
    /// closes the stdlib-side gap, which has no other check.)
    #[test]
    fn baked_stdlib_has_no_markers_outside_type_decls() {
        fn marked(gp: &Option<GenericParams>) -> bool {
            gp.as_ref().is_some_and(|g| {
                g.params
                    .iter()
                    .any(|p| p.variance != Variance::Invariant && p.variance_span.is_some())
            })
        }
        let programs = crate::prelude::STDLIB_PROGRAMS
            .iter()
            .map(|(_, p)| p)
            .chain(crate::prelude::GATED_STDLIB_PROGRAMS.iter().map(|(_, p)| p));
        for program in programs {
            for item in &program.items {
                let (kind, name, gp) = match item {
                    Item::Function(f) => ("fn", f.name.clone(), &f.generic_params),
                    Item::TraitDef(t) => ("trait", t.name.clone(), &t.generic_params),
                    Item::ImplBlock(i) => ("impl", String::new(), &i.generic_params),
                    Item::TypeAlias(t) => ("type alias", t.name.clone(), &t.generic_params),
                    _ => continue,
                };
                assert!(
                    !marked(gp),
                    "stdlib {kind} '{name}' carries a +/- variance marker — markers \
                     belong on struct/enum declarations only",
                );
            }
        }
    }

    /// `+T` on a `mut` field is the canonical inconsistency (the spec's
    /// own negative example: `struct Cell[+T] {{ mut field: T }}`).
    #[test]
    fn verifier_rejects_covariant_param_in_mut_field() {
        let parsed = crate::parse("struct Cell[+T] { mut field: T }\nfn main() {}\n");
        assert!(parsed.errors.is_empty());
        let errs = decl_errors(&parsed.program);
        assert_eq!(errs.len(), 1, "expected one inconsistency: {errs:?}");
        assert!(errs[0].contains("E_VARIANCE_DECLARATION_INCONSISTENT"));
        assert!(errs[0].contains("'T' declared '+T' (covariant)"));
        assert!(errs[0].contains("invariant position"));
        assert!(
            errs[0].contains("'=T'"),
            "fix-it must suggest '=T': {}",
            errs[0]
        );
    }

    /// `+T` in a function-argument position is contravariant — rejected.
    #[test]
    fn verifier_rejects_covariant_param_in_fn_arg_position() {
        let parsed = crate::parse("struct Cb[+T] { f: Fn(T) -> i64 }\nfn main() {}\n");
        assert!(parsed.errors.is_empty());
        let errs = decl_errors(&parsed.program);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("contravariant position"));
    }

    /// `-T` in fn-arg position is consistent; `-T` as a plain field is not.
    #[test]
    fn verifier_contravariant_positions() {
        let ok = crate::parse("struct Sink[-T] { f: Fn(T) -> i64 }\nfn main() {}\n");
        assert!(decl_errors(&ok.program).is_empty());
        let bad = crate::parse("struct Holder[-T] { value: T }\nfn main() {}\n");
        let errs = decl_errors(&bad.program);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("covariant position"));
    }

    /// Covariant payloads verify: the spec's positive example is
    /// `Option[+T]` (T only in the `Some(T)` payload). Nested
    /// composition: `Option[T]` inside a covariant field keeps
    /// covariance (Option's slot is `+`), while `Vec[T]` forces
    /// invariance (Vec's slot is `=`).
    #[test]
    fn verifier_accepts_covariant_payloads_and_composes_slots() {
        let opt = crate::parse("enum MyOpt[+T] { Some(T), None }\nfn main() {}\n");
        assert!(decl_errors(&opt.program).is_empty());
        let nested_ok = crate::parse("struct W[+T] { inner: Option[T] }\nfn main() {}\n");
        assert!(decl_errors(&nested_ok.program).is_empty());
        let nested_bad = crate::parse("struct W[+T] { inner: Vec[T] }\nfn main() {}\n");
        let errs = decl_errors(&nested_bad.program);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("invariant position"));
    }

    /// `mut ref T` / `weak T` / `Atomic[T]` interiors force invariance.
    #[test]
    fn verifier_rejects_covariant_param_behind_invariant_interiors() {
        for (label, src) in [
            ("mut ref", "struct B[+T] { r: mut ref T }\nfn main() {}\n"),
            ("weak", "struct B[+T] { w: weak T }\nfn main() {}\n"),
            ("Atomic", "struct B[+T] { a: Atomic[T] }\nfn main() {}\n"),
        ] {
            let parsed = crate::parse(src);
            let errs = decl_errors(&parsed.program);
            assert_eq!(errs.len(), 1, "{label}: {errs:?}");
            assert!(
                errs[0].contains("invariant position"),
                "{label}: {}",
                errs[0]
            );
        }
    }

    /// Explicit `=T` is unrestricted — interior mutability verifies.
    #[test]
    fn verifier_accepts_explicit_invariant_anywhere() {
        let parsed =
            crate::parse("struct C[=T] { mut a: T, f: Fn(T) -> T, v: Vec[T] }\nfn main() {}\n");
        assert!(decl_errors(&parsed.program).is_empty());
    }

    /// The lint demands an explicit marker on every type param of a
    /// parametric stdlib type; const / variadic-shape params are exempt.
    #[test]
    fn lint_requires_explicit_markers() {
        let parsed = crate::parse("struct P[T, =U] { }\nfn main() {}\n");
        let errs = decl_errors(&parsed.program);
        assert_eq!(errs.len(), 1, "{errs:?}");
        assert!(errs[0].contains("E_STDLIB_VARIANCE_IMPLICIT"));
        assert!(errs[0].contains("'T'"));
        let exempt = crate::parse("struct A[=T, const N: i64] { }\nfn main() {}\n");
        assert!(decl_errors(&exempt.program).is_empty());
    }
}
