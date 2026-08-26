//! Operator + identifier + path / offset_of / pipe / question
//! expression inference.
//!
//! Houses six per-shape inference rules that sit between the big
//! `infer_expr_inner` dispatch and the lower-level type / impl
//! helpers:
//!
//! - `infer_offset_of` — `offset_of[T](field.path)` per design.md
//!   § Field Offsets.
//! - `resolve_identifier_type` — bare-identifier resolution
//!   (locals / params / functions / constants / enum variants).
//! - `resolve_path_type` — `Foo.Bar` / `Foo.method` path resolution
//!   in expression position.
//! - `infer_binary` — typecheck binary operator expressions
//!   (arithmetic / comparison / bitwise / shift / `+` overloads).
//! - `infer_unary` — typecheck unary operator expressions
//!   (`-x`, `!b`, `~i`, deref).
//! - `infer_pipe` — `a |> f` / `a |> f(args)` desugaring inference.
//! - `infer_question` — `?` operator typechecking + `From`
//!   conversion recording.
//!
//! Lives in a sibling `impl<'a> super::TypeChecker<'a>` block.

use std::collections::HashMap;

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::Span;

use super::inference::{resolve_type_var_top, resolve_type_vars};
use super::types::{
    float_width_rank, is_integer, is_numeric, is_prelude_type_or_module_name,
    is_string_concat_operand, strip_refinement, type_display, types_compatible, ConstArg, DimArg,
    IntSize, SubstValue, Type, UIntSize, VariantTypeInfo,
};
use super::{FixIt, TypeErrorKind};

/// Auto-deref reference operands for comparison operators (`==`, `!=`,
/// `<`, `<=`, `>`, `>=`): comparing a value against a borrow of the same
/// type (`String == ref String`) is well-formed because the comparison
/// only reads through the borrow. Recurses through nested `ref` / `mut ref`
/// (mirrors `stdlib_seq::is_str_like`).
fn strip_refs_for_compare(ty: &Type) -> &Type {
    match ty {
        Type::Ref(inner) | Type::MutRef(inner) => strip_refs_for_compare(inner),
        _ => ty,
    }
}

/// The `(min, max)` bit-width an integer type can occupy across supported
/// targets (B-2026-08-17-11). Concrete widths are a point; `usize` spans
/// `[32, 64]` because wasm32 is a first-class target (`--target=wasm_wasi`),
/// so any repair the compiler names must preserve values on BOTH widths.
fn int_width_range(ty: &Type) -> Option<(u32, u32)> {
    match ty {
        Type::Int(s) => {
            let b = match s {
                IntSize::I8 => 8,
                IntSize::I16 => 16,
                IntSize::I32 => 32,
                IntSize::I64 => 64,
                IntSize::I128 => 128,
                // Pointer-width, like `Usize` below: wasm32 is a first-class
                // target, so any repair the compiler names must hold at 32 too.
                IntSize::Isize => return Some((32, 64)),
            };
            Some((b, b))
        }
        Type::UInt(s) => {
            let b = match s {
                UIntSize::U8 => 8,
                UIntSize::U16 => 16,
                UIntSize::U32 => 32,
                UIntSize::U64 => 64,
                UIntSize::U128 => 128,
                UIntSize::Usize => return Some((32, 64)),
            };
            Some((b, b))
        }
        _ => None,
    }
}

/// `true` when `src as dst` is value-preserving for EVERY value of `src` on
/// EVERY supported target: same signedness needs `dst` at least as wide;
/// unsigned → signed needs `dst` strictly wider (the sign bit costs a value
/// bit, so `u64 as i64` loses the top half); signed → unsigned is never
/// value-preserving (negatives). This is the direction test behind E0200's
/// repair (B-2026-08-17-11): the widening cast is the one the diagnostic may
/// name and apply, because the narrowing one turns a caught compile-time
/// error into an uncaught-until-runtime overflow trap.
fn int_cast_preserves_all_values(src: &Type, dst: &Type) -> bool {
    let (Some((_, src_max)), Some((dst_min, _))) = (int_width_range(src), int_width_range(dst))
    else {
        return false;
    };
    match (matches!(src, Type::UInt(_)), matches!(dst, Type::UInt(_))) {
        (true, false) => src_max < dst_min,
        (false, true) => false,
        _ => src_max <= dst_min,
    }
}

/// The byte offset just past `expr`'s full source text, when that extent is
/// exactly recoverable from the AST — the gate on emitting E0200's fix-it as
/// an ` as <wide>` insertion (B-2026-08-17-11). Two constraints select the
/// shapes below, and both are load-bearing:
///
/// TEXTUAL — the postfix parser aliases most postfix shapes' spans to the
/// RECEIVER's span (`MethodCall.span = receiver.span`, same for `Index` /
/// `Call` / `FieldAccess` / `Cast` / `Question`), so `span.offset + length`
/// is the true end only for atoms, for `MethodCall` (whose `args_close_span`
/// records the closing paren — the end of the whole chain), and for prefix
/// unary (built with `span_from`, covering every consumed token, parens
/// included). Extending this to `Index` (`b[i] as i64`, the kata-278 shape)
/// means recording the `]` span in the AST first — ~293 construction/match
/// sites at last count — so that shape stays prose-only for now.
///
/// SEMANTIC — appending must cast the WHOLE operand. `as` binds at bp 23,
/// tighter than every binary arithmetic op and looser than unary (24) and
/// postfix, so every shape here rebinds correctly (`-x as i64` casts `-x`;
/// `s.len() as i64` casts the call). A `Binary` operand can never qualify
/// even with a known extent: its parens are span-transparent, so the
/// insertion would land INSIDE them and cast only the last term.
fn appended_cast_end_offset(expr: &Expr) -> Option<usize> {
    match &expr.kind {
        ExprKind::Identifier(_)
        | ExprKind::SelfValue
        | ExprKind::Integer(..)
        | ExprKind::ByteLit(_)
        | ExprKind::ByteStringLit(_) => Some(expr.span.offset + expr.span.length),
        ExprKind::MethodCall {
            args_close_span, ..
        } => Some(args_close_span.offset + args_close_span.length),
        ExprKind::Unary { .. } => Some(expr.span.offset + expr.span.length),
        _ => None,
    }
}

/// Peel a single `ref` / `mut ref` off a numeric SCALAR operand so arithmetic
/// reads through the borrow (design.md § "Compound assignment on `mut ref`
/// lvalues": `a = a + b` on a `mut ref T` lvalue desugars to `*a = *a + b`, so
/// the RHS reads through the borrow and the binop operates on the bare scalar
/// `T`). A non-numeric or unborrowed type passes through untouched, so a borrow
/// of a non-numeric type still reaches the "requires numeric type" diagnostic.
/// Scalar borrows don't nest, so one level suffices.
fn deref_numeric_scalar(ty: Type) -> Type {
    match &ty {
        Type::Ref(inner) | Type::MutRef(inner) if is_numeric(inner) => (**inner).clone(),
        _ => ty,
    }
}

/// The `[elem, shape]` generic-arg list of a `Tensor[T, Shape]` type,
/// peeling one `ref` / `mut ref`. `None` for any non-tensor type. Used by
/// `infer_binary` to route element-wise tensor arithmetic and by the
/// element-wise scalar-broadcast path.
fn tensor_named_args(ty: &Type) -> Option<&[Type]> {
    let core = match ty {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        other => other,
    };
    match core {
        Type::Named { name, args } if name == "Tensor" && args.len() == 2 => Some(args),
        _ => None,
    }
}

/// True iff `ty` is a `Tensor[T, Shape]` (peeling one borrow).
fn is_tensor_type(ty: &Type) -> bool {
    tensor_named_args(ty).is_some()
}

/// The element type `T` of a `Column[T]`, peeling one `ref` / `mut ref`.
/// `None` for any non-Column type. Used by `infer_binary` / `infer_unary`
/// to route element-wise Column arithmetic / comparison (phase-11 Arrow).
fn column_elem(ty: &Type) -> Option<&Type> {
    let core = match ty {
        Type::Ref(inner) | Type::MutRef(inner) => inner.as_ref(),
        other => other,
    };
    match core {
        Type::Named { name, args } if name == "Column" && args.len() == 1 => Some(&args[0]),
        _ => None,
    }
}

/// True iff `ty` is a `Column[T]` (peeling one borrow).
fn is_column_type(ty: &Type) -> bool {
    column_elem(ty).is_some()
}

/// Merge two shape dims for an element-wise tensor op. Concrete-vs-concrete
/// must be equal (`Err` on mismatch — the static `E_SHAPE` case); a concrete
/// literal paired with any non-literal wins (the codegen runtime-guards the
/// `?` side); two equal non-literals (same param / `?`) survive; two distinct
/// non-literals degrade to `?` (codegen runtime-guards). Mirrors the shipped
/// cross-argument dim-assert flavors (phase-11 line 41).
fn merge_tensor_dim(l: &DimArg, r: &DimArg) -> Result<DimArg, ()> {
    match (l, r) {
        (DimArg::Const(ConstArg::Literal(a)), DimArg::Const(ConstArg::Literal(b))) => {
            if a == b {
                Ok(DimArg::Const(ConstArg::Literal(*a)))
            } else {
                Err(())
            }
        }
        (DimArg::Const(ConstArg::Literal(a)), _) => Ok(DimArg::Const(ConstArg::Literal(*a))),
        (_, DimArg::Const(ConstArg::Literal(b))) => Ok(DimArg::Const(ConstArg::Literal(*b))),
        _ => {
            if l == r {
                Ok(l.clone())
            } else {
                Ok(DimArg::Dynamic)
            }
        }
    }
}

impl<'a> super::TypeChecker<'a> {
    /// Type-check `offset_of[T](field.path)`. Per `design.md § Field
    /// Offsets`, the target type must be a struct (concrete or
    /// generic-with-fully-resolved args); opaque foreign types and
    /// generic type parameters are rejected at the first segment.
    /// Each path segment must name a field of the type at the previous
    /// segment's resolved type. Returns `usize` (also `Type::Error` on
    /// failure for downstream tolerance).
    pub(super) fn infer_offset_of(
        &mut self,
        ty: &TypeExpr,
        field_path: &[String],
        span: &Span,
    ) -> Type {
        let usize_ty = Type::UInt(UIntSize::Usize);
        // Lower the target with `parent_is_ref = true` so the slice-1b
        // walker doesn't fire E_OPAQUE_TYPE_REQUIRES_INDIRECTION; this
        // intrinsic emits E_OFFSET_OF_OPAQUE_TYPE instead.
        let resolved = self.lower_type_expr_inner(ty, &[], true);
        let (mut current_struct_name, _initial_args) = match &resolved {
            Type::Named { name, args } => (name.clone(), args.clone()),
            // Per design.md, generic type-parameter targets are rejected:
            // the typechecker can't see a layout without a concrete
            // instantiation. `Type::TypeParam` and other non-Named
            // shapes route here.
            Type::TypeParam(name) => {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_GENERIC_PARAM]: offset_of requires a \
                         concrete type; the type parameter '{name}' is not \
                         resolvable to a layout at this call site"
                    ),
                    ty.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
            _ => {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_NON_STRUCT_TARGET]: offset_of requires a \
                         struct target; got '{}'",
                        type_display(&resolved)
                    ),
                    ty.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        if self.env.opaque_foreign_types.contains(&current_struct_name) {
            self.type_error(
                format!(
                    "error[E_OFFSET_OF_OPAQUE_TYPE]: offset_of cannot be applied to \
                     opaque foreign type '{current_struct_name}'; the type's layout \
                     is unknown to Kāra"
                ),
                ty.span,
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        if field_path.is_empty() {
            self.type_error(
                "error[E_OFFSET_OF_INVALID_PATH]: offset_of requires at least \
                 one field-name segment"
                    .to_string(),
                *span,
                TypeErrorKind::WrongNumberOfArgs,
            );
            return Type::Error;
        }
        // Walk each segment, validating membership in the current struct's
        // declared field set and chasing the field's type for the next
        // segment. At each segment, the current struct is looked up by
        // name in `env.structs`; if absent (e.g., the surface type is an
        // enum or a primitive), `E_OFFSET_OF_NON_STRUCT_TARGET` fires.
        for (segment_idx, segment_name) in field_path.iter().enumerate() {
            let Some(info) = self.env.structs.get(&current_struct_name).cloned() else {
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_NON_STRUCT_TARGET]: offset_of cannot \
                         walk into '{current_struct_name}'; only struct types \
                         have field offsets"
                    ),
                    ty.span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            };
            let mut found = None;
            for (fname, ftype, is_pub) in &info.fields {
                if fname == segment_name {
                    found = Some((ftype.clone(), *is_pub));
                    break;
                }
            }
            let Some((field_ty, is_pub)) = found else {
                let available: Vec<&str> = info.fields.iter().map(|(n, _, _)| n.as_str()).collect();
                self.type_error(
                    format!(
                        "error[E_OFFSET_OF_UNKNOWN_FIELD]: type '{current_struct_name}' \
                         has no field '{segment_name}'; available fields are: {}",
                        available.join(", ")
                    ),
                    *span,
                    TypeErrorKind::UndefinedField,
                );
                return Type::Error;
            };
            if !is_pub {
                self.check_cross_module_field_access(&current_struct_name, segment_name, span);
            }
            // If this is the last segment, we're done — return usize.
            if segment_idx + 1 == field_path.len() {
                return usize_ty;
            }
            // Otherwise, the field's type must itself be a struct so the
            // next segment can walk into it.
            current_struct_name = match field_ty {
                Type::Named { name, .. } => name,
                _ => {
                    self.type_error(
                        format!(
                            "error[E_OFFSET_OF_NON_STRUCT_TARGET]: field \
                             '{segment_name}' is not a struct type; cannot walk \
                             further into the offset_of path"
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
            };
        }
        usize_ty
    }

    // ── Identifier Resolution ───────────────────────────────────

    /// The built-in enum that owns `name` as a bare constructor, if any.
    ///
    /// B-2026-08-14-10. `Option` and `Result` are ordinary prelude enums living
    /// in `env.enums` next to the user's, so their variant names are not
    /// reserved and a user enum may declare one. This says which enum a
    /// COLLIDING bare name resolves to, and it is deliberately a fixed table
    /// rather than a search: the whole bug was that the answer depended on
    /// where in a hash map the two candidates happened to land.
    ///
    /// Only the bare form is affected. `Sink.None` goes through
    /// `resolve_path_type`, which names its enum explicitly and runs first.
    /// `true` when `name` is bound as an ordinary value — a local, a function
    /// or a constant — and therefore is not an enum-variant reference at all.
    ///
    /// Mirrors the order `resolve_identifier_type` already uses: local scope,
    /// then functions, then constants, and only then the variant scan. Both the
    /// ambiguity diagnostic and the type-directed resolution consult this, so
    /// neither can start disagreeing with that order.
    fn name_shadows_variant(&self, name: &str) -> bool {
        self.local_scope.lookup(name).is_some()
            || self.env.functions.contains_key(name)
            || self.env.constants.contains_key(name)
    }

    /// Type-directed resolution of a BARE unit-variant name against the type the
    /// context expects (`let x: Second = A;`, `fn f() -> Second { A }`,
    /// `want_second(A)`).
    ///
    /// Synthesis position has no expected type and must still reject an
    /// ambiguous bare name (B-2026-08-19-17 (b)); this is the checking-position
    /// counterpart, and it is what makes that rejection a narrow one rather
    /// than a dead end — before this, the losing enum of a collision was
    /// unreachable by bare name in EVERY position, annotation included.
    ///
    /// Returns the expected type itself on a match, so a generic enum keeps the
    /// context's instantiation (`Option[i64]`, not a fresh `Option[?T]`).
    ///
    /// Declines, leaving the ordinary path to run, when:
    /// - the name is bound as a local, function or constant — a real binding
    ///   always wins over a variant name, exactly as `resolve_identifier_type`
    ///   orders them, so `let A = 5; let x: First = A;` still means the local;
    /// - the expected type is not a named enum declaring `name`;
    /// - the variant is not a UNIT variant. A bare tuple variant is a
    ///   constructor FUNCTION, not a value of the enum, so `let x: First = W;`
    ///   must keep failing; `W(7)` is a call and never reaches here.
    pub(super) fn bare_variant_from_expected(&self, name: &str, expected: &Type) -> Option<Type> {
        // `Some` / `None` / `Ok` / `Err` are pinned to their builtin owner by
        // the variant scan, so they are never the user-vs-user case this
        // serves — and `check_expr` has dedicated arms for them (the weak-slot
        // `None` coercion, the `Some(..)`/`Ok(..)`/`Err(..)` constructor
        // checks) that resolving here would sit above and skip. Measured: with
        // them included, a `weak T` field initialised to `None` bypassed
        // `karac_weak_downgrade` and the self-host parser miscompiled into a
        // double free.
        if Self::builtin_variant_owner(name).is_some() {
            return None;
        }
        if self.name_shadows_variant(name) {
            return None;
        }
        let Type::Named {
            name: enum_name, ..
        } = expected
        else {
            return None;
        };
        let info = self.env.enums.get(enum_name)?;
        let is_unit = info
            .variants
            .iter()
            .any(|(v, kind)| v == name && matches!(kind, VariantTypeInfo::Unit));
        if !is_unit {
            return None;
        }
        Some(expected.clone())
    }

    /// B-2026-08-19-17 (b) — the USER-declared enums that declare `name` as a
    /// variant, when two or more of them do.
    ///
    /// `resolve_identifier_type`'s scan below picks a winner among these by
    /// sorting the enum names, which makes the answer deterministic (its whole
    /// purpose — B-2026-08-14-10 replaced a `HashMap` scan that was a measured
    /// coin flip on identical bytes) but leaves the user-vs-user tie-break
    /// ALPHABETICAL, which nothing ever chose. The loser is not merely
    /// deprioritized, it is unreachable by bare name: expression position has no
    /// type-direction, so `let x: Second = A;`, `fn f() -> Second { A }` and
    /// `want_second(A)` are all rejected with "expected 'Second', found
    /// 'First'". So the author cannot express the losing enum bare even
    /// deliberately, and picking silently is the worst of the available
    /// behaviours.
    ///
    /// Returns the candidates sorted, for a stable diagnostic. `None` — no
    /// diagnostic — for the two collisions the language resolves ON PURPOSE:
    ///
    /// - `Some` / `None` / `Ok` / `Err`, pinned to their builtin owner so a
    ///   user enum cannot hijack a bare `None` (the scan does this explicitly).
    /// - a user enum shadowing a stdlib/prelude one, which the two-tier sort
    ///   decides in the user's favour on purpose — a user's `MyIoErr.Other`
    ///   must not be hijacked by the seeded `IoError.Other`.
    ///
    /// Both of those have a defined winner that someone reasoned about; only
    /// the user-vs-user case does not.
    pub(super) fn ambiguous_user_variant_owners(&self, name: &str) -> Option<Vec<String>> {
        if Self::builtin_variant_owner(name).is_some() || is_prelude_type_or_module_name(name) {
            return None;
        }
        // A real binding of that name is not a variant reference at all, so
        // there is nothing ambiguous about it. `resolve_identifier_type` checks
        // locals and constants BEFORE it ever reaches the variant scan, and the
        // caller runs after that resolution — so without this guard a perfectly
        // ordinary `let A = 77; println(A);` was reported as an ambiguous
        // variant whenever two enums happened to declare an `A`. Kept here
        // rather than at the call site so this helper and
        // `bare_variant_from_expected` cannot drift on which names count.
        if self.name_shadows_variant(name) {
            return None;
        }
        let mut owners: Vec<String> = self
            .env
            .enums
            .iter()
            .filter(|(enum_name, info)| {
                !info.defining_stdlib_origin
                    && !is_prelude_type_or_module_name(enum_name)
                    && info.variants.iter().any(|(v, _)| v == name)
            })
            .map(|(enum_name, _)| enum_name.clone())
            .collect();
        if owners.len() < 2 {
            return None;
        }
        owners.sort();
        Some(owners)
    }

    pub(super) fn builtin_variant_owner(name: &str) -> Option<&'static str> {
        match name {
            "Some" | "None" => Some("Option"),
            "Ok" | "Err" => Some("Result"),
            _ => None,
        }
    }

    pub(super) fn resolve_identifier_type(&mut self, name: &str, span: &Span) -> Type {
        // Check local scope first. Resolve inference vars against the current
        // substitution map before returning: a binding recorded as `Vec[?T]` at
        // `let` time (`let mut out = Vec.new();`) has its element var pinned
        // later by `out.push(x)`, which updates `env.substitutions` but NOT the
        // snapshot stored in `local_scope`. Returning the stale `Vec[?T]` makes a
        // downstream return-position / assignment check compare against an
        // unresolved var and emit a spurious `expected 'Vec[i64]', found
        // 'Vec[?T]'`. Genuinely-unresolved vars stay vars (empty id_to_name), so
        // this never over-resolves. Surfaced by examples/tangle/doubly_linked.kara.
        if let Some(ty) = self.local_scope.lookup(name).cloned() {
            return resolve_type_vars(
                &ty,
                &self.env.substitutions,
                &HashMap::new(),
                &self.env.const_substitutions,
                &HashMap::new(),
            );
        }
        // Check functions
        if let Some((params, return_type)) = self
            .env
            .functions
            .get(name)
            .map(|sig| (sig.params.clone(), sig.return_type.clone()))
        {
            // `#[deprecated]` slice 4 — emit the deprecation warning
            // BEFORE returning so the cascade has the enclosing fn /
            // impl scope on the stack (the fn body that contains this
            // identifier reference). The lookup queries the resolver's
            // symbol table by name to find the deprecation payload.
            self.check_deprecated_use_at(span, name);
            self.check_unstable_use_at(span, name);
            return Type::Function {
                params,
                return_type: Box::new(return_type),
            };
        }
        // Check constants
        if let Some(ty) = self.env.constants.get(name).cloned() {
            self.check_deprecated_use_at(span, name);
            self.check_unstable_use_at(span, name);
            return ty;
        }
        // Comptime `Type` pseudovalue (substrate 2): inside a comptime
        // context, a bare struct / enum / union name used in *value* position
        // is a `Type` value (`f(MyStruct)` into a `comptime T: Type` param,
        // `let t = MyStruct; t.fields()`). The receiver form
        // `MyStruct.method()` is intercepted earlier. Gated to comptime so
        // runtime value uses of a (unit/empty) struct name are untouched at
        // depth 0. Prelude type/module names are excluded. The
        // `E_TYPE_VALUE_AT_RUNTIME` boundary is enforced precisely on runtime
        // functions declaring a `Type` parameter (see `check_function`).
        // Spec: deferred.md § Comptime — Types as first-class values.
        if self.comptime_depth > 0
            && !is_prelude_type_or_module_name(name)
            && (self.env.structs.contains_key(name)
                || self.env.enums.contains_key(name)
                || self.env.unions.contains_key(name))
        {
            return Type::Named {
                name: "Type".to_string(),
                args: vec![],
            };
        }
        // Check enum variants (unit variants used as values; tuple variants
        // as constructor functions). Generic enums thread their declared
        // type parameters through the return type's `args` so call-site
        // inference can solve them (see `infer_call`).
        //
        // **Variant-name shadow rule (Slice F).** Skip variants whose
        // bare name collides with a primitive type name (`String`,
        // `Array`, `Map`, `Set`, etc.) — those identifiers are
        // overwhelmingly used as type/module aliases at the call-site
        // (`String.from(...)`, `Map.new()`, `Vec.new()`), not as
        // variant constructors. Without this skip, declaring an enum
        // like `Json.String(String)` retroactively breaks every
        // pre-existing `String.from("...")` call by routing it through
        // the variant-as-function dispatch instead of the impl
        // resolution. Variants are still reachable through the
        // qualified path form (`Json.String(...)`) — `resolve_path_type`
        // above runs before this fallback and finds them by enum name.
        //
        // **Order is load-bearing (B-2026-08-14-10).** `self.env.enums` is a
        // `HashMap`, whose iteration order comes from a per-process
        // `RandomState`, and this scan used to `return` on the FIRST match. The
        // prelude's `Option`/`Result` live in that same map, so a user enum
        // declaring a variant named `None`/`Some`/`Ok`/`Err` made a bare `None`
        // type as `Option[T]` on some runs and as the user enum on others —
        // `karac check` returning a coin flip on identical bytes, measured
        // 15/30, 10/20 and 7/16. Scanning a SORTED key list makes the answer a
        // function of the program alone; `builtin_variant_owner` then decides
        // which enum wins, rather than leaving it to whichever the allocator
        // handed over first.
        //
        // Two tiers, USER-declared before stdlib/prelude, each sorted by name.
        // The tiering mirrors codegen's own bare-name preference and is needed
        // for the same reason it gives: a user's `MyIoErr.Other` must not be
        // hijacked by the seeded `IoError.Other`. Sorting alone would have
        // decided that collision alphabetically, which is deterministic but
        // wrong — and wrong in a way the old coin flip hid, since it got the
        // user's enum half the time.
        let mut enum_names: Vec<&String> = self.env.enums.keys().collect();
        enum_names.sort_unstable_by_key(|n| {
            // Prelude-visible by NAME as well as by the `stdlib_origin` flag:
            // `IoError` and its siblings are listed in `PRELUDE_TYPES` but do
            // not carry the flag, and keying on the flag alone let the seeded
            // `IoError.Other` beat a user's `MyIoErr.Other` alphabetically —
            // deterministic, and the wrong side of the very preference codegen
            // documents.
            (
                self.env.enums[*n].defining_stdlib_origin || is_prelude_type_or_module_name(n),
                (*n).clone(),
            )
        });
        // The built-in owner of a colliding constructor name goes first, so a
        // user enum can never retroactively hijack a bare `None` / `Some` /
        // `Ok` / `Err`. This is the same call the Slice F rule above already
        // makes for primitive TYPE names, for the same stated reason: those
        // identifiers are overwhelmingly the built-in meaning at a use site,
        // and the user's variant stays reachable through the qualified form
        // (`Sink.None`), which `resolve_path_type` resolves before this
        // fallback ever runs.
        if let Some(owner) = Self::builtin_variant_owner(name) {
            if self.env.enums.contains_key(owner) {
                enum_names.retain(|n| n.as_str() != owner);
                // Resolved by direct lookup below, ahead of every user enum.
                enum_names.insert(0, self.env.enums.get_key_value(owner).unwrap().0);
            }
        }
        for enum_name in enum_names {
            let enum_info = &self.env.enums[enum_name];
            for (variant_name, variant_type) in &enum_info.variants {
                if variant_name == name {
                    if is_prelude_type_or_module_name(name) {
                        continue;
                    }
                    let return_args: Vec<Type> = enum_info
                        .generic_params
                        .iter()
                        .map(|p| Type::TypeParam(p.clone()))
                        .collect();
                    let return_ty = Type::Named {
                        name: enum_name.clone(),
                        args: return_args,
                    };
                    match variant_type {
                        VariantTypeInfo::Unit => return return_ty,
                        VariantTypeInfo::Tuple(fields) => {
                            return Type::Function {
                                params: fields.clone(),
                                return_type: Box::new(return_ty),
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
        // Distinct-type constructor: `UserId(value)` wraps a base value.
        // The name resolves to a one-argument constructor function
        // `fn(Base) -> UserId`, mirroring a tuple-variant constructor, so the
        // ordinary call-dispatch path checks the argument against the base
        // type and types the result as the (nominal) distinct type. The base
        // is recovered from `env.distinct_bases`. design.md § Distinct Types —
        // "Wrap: `UserId(42)` — constructor syntax".
        if let Some(base) = self.env.distinct_bases.get(name).cloned() {
            self.check_deprecated_use_at(span, name);
            self.check_unstable_use_at(span, name);
            return Type::Function {
                params: vec![base],
                return_type: Box::new(Type::Named {
                    name: name.to_string(),
                    args: Vec::new(),
                }),
            };
        }
        // NOTE (B-2026-08-11-6): the type-name-in-value-position diagnostic is
        // NOT raised here, even though this is where such a name lands. This
        // helper is also the fallback inside `resolve_path_type`, which calls it
        // on a path's FIRST segment before later machinery gets its turn —
        // resource dispatch (`RandomSource.next()`) resolves that way, and
        // erroring here rejects it. The silent `Error` is load-bearing for that
        // caller. The diagnostic belongs on the bare-identifier arm of
        // `infer_expr`, which is reached only when the identifier really is the
        // whole expression; see `type_name_in_value_position_message`.
        //
        // Fallback — likely a name the resolver already handled
        // Return Error silently (resolver already reported it)
        let _ = span;
        Type::Error
    }

    /// `Some(diagnostic)` when `name` is a TYPE used where a value belongs, and
    /// that type has no callable/constructible bare-name form. `None` for
    /// anything legal, so callers can use it as the gate as well as the message.
    ///
    /// The remedy is per-family because the right answer genuinely differs, and
    /// this is the diagnostic a user meets after following the compiler's own
    /// advice: `max(1.5, 2.5)` is rejected with "use the total-order wrapper
    /// `F64`", and `F64(1.5)` is the next thing anyone writes.
    pub(super) fn type_name_in_value_position_message(&self, name: &str) -> Option<String> {
        if !is_prelude_type_or_module_name(name) && !self.is_type_name(name) {
            return None;
        }
        // A USER-DECLARED VARIANT OF THIS NAME OUTRANKS EVERY ARM BELOW,
        // because for that author the name is not a stray type reference —
        // they declared it, and the bare form they wrote is the one the
        // variant-resolution fallback deliberately skips (see the
        // `is_prelude_type_or_module_name` `continue` in
        // `resolve_identifier_type`: the built-in meaning wins the bare name,
        // and the variant stays reachable QUALIFIED). Every arm below would
        // then answer about the type they did not mean — `Span(a, b)` was told
        // to "construct one with `Span.new(…)`", naming the `std.tracing`
        // struct, which has no `new` either, so following the advice produced a
        // SECOND error and no reachable third step. Name the qualified form
        // instead; it is the one edit that compiles.
        if let Some(owner) = self.user_enum_owning_variant(name) {
            // Since B-2026-08-17-7, unit and tuple variants RESOLVE bare at
            // the one call site that surfaces this message (`infer_expr`'s
            // bare-identifier arm checks `user_variant_value_type` first), so
            // the variant reaching this arm as a diagnostic is Struct-shaped —
            // the shape with no bare-call form anywhere. The remedy must name
            // the braced literal; the `(…)` call form it used to suggest is a
            // guaranteed second error for it. Unit/Tuple formats are kept
            // correct anyway for the `fields.rs` gate caller (which discards
            // the text) and any future caller.
            let call = match self.env.enums[&owner]
                .variants
                .iter()
                .find(|(v, _)| v == name)
                .map(|(_, t)| t)
            {
                Some(VariantTypeInfo::Unit) => format!("{owner}.{name}"),
                Some(VariantTypeInfo::Struct(fields)) => match fields.first() {
                    Some((f, _)) => format!("{owner}.{name} {{ {f}: … }}"),
                    None => format!("{owner}.{name} {{ }}"),
                },
                _ => format!("{owner}.{name}(…)"),
            };
            return Some(format!(
                "'{name}' is a type, not a function — it is also a variant of \
                 enum '{owner}', but the bare name resolves to the type, so \
                 construct the variant with `{call}`"
            ));
        }
        // ORDER MATTERS. The prelude arms come FIRST because the baked types
        // are registered in `env.structs` too — keying on that alone told the
        // author to write `F64 { … }` and `Vec { … }`, neither of which is a
        // thing. Only a genuinely user-declared struct reaches the literal arm.
        let msg = match name {
            // The remedy that motivated the row: `max(1.5, 2.5)` is rejected
            // with "use the total-order wrapper `F64`", so `F64(1.5)` is the
            // very next thing written. `F64.from` exists as of B-2026-08-11-8.
            "F64" | "F32" | "F16" | "Bf16" => {
                format!("wrap a float with `{name}.from(x)`")
            }
            // Numeric primitives only — `bool`/`char`/`String` are in
            // PRELUDE_PRIMITIVES too but have no cast from an arbitrary value,
            // so the cast advice would be wrong for them.
            "i8" | "i16" | "i32" | "i64" | "i128" | "u8" | "u16" | "u32" | "u64" | "u128"
            | "usize" | "isize" | "f16" | "bf16" | "f32" | "f64" => {
                format!("a numeric conversion is the cast `x as {name}`, not a call")
            }
            "String" => "build a string with `String.from(x)`, `x.to_string()` or an \
                         f-string"
                .to_string(),
            "bool" | "char" => {
                format!("there is no `{name}` conversion call; compare or match on the value")
            }
            _ if is_prelude_type_or_module_name(name) => {
                format!("construct one with an associated function such as `{name}.new(…)`")
            }
            _ => {
                // A user struct: name a real field, so the literal form is
                // copy-pasteable rather than a gesture.
                let field = self
                    .env
                    .structs
                    .get(name)
                    .and_then(|i| i.fields.first().map(|f| f.0.clone()));
                match field {
                    Some(f) => format!(
                        "it has named fields, so construct it with a struct literal: \
                         `{name} {{ {f}: … }}`"
                    ),
                    None => format!("construct it with a struct literal: `{name} {{ … }}`"),
                }
            }
        };
        Some(format!("'{name}' is a type, not a function — {msg}"))
    }

    /// The USER-declared enum that declares a variant called `name`, if any.
    ///
    /// Stdlib/prelude-owned enums are excluded on purpose: the point is to name
    /// the enum the AUTHOR wrote, so the remedy is an edit they recognize. A
    /// seeded `IoError.Other` is not what someone writing `Other(…)` in their
    /// own file needs pointed at.
    ///
    /// Sorted, so a variant name declared by two user enums always names the
    /// same one rather than whichever the map iterated first — the same
    /// determinism `resolve_identifier_type`'s variant fallback establishes.
    fn user_enum_owning_variant(&self, name: &str) -> Option<String> {
        let mut owners: Vec<&String> = self
            .env
            .enums
            .iter()
            .filter(|(n, info)| !info.defining_stdlib_origin && !is_prelude_type_or_module_name(n))
            .filter(|(_, info)| info.variants.iter().any(|(v, _)| v == name))
            .map(|(n, _)| n)
            .collect();
        owners.sort_unstable();
        owners.first().map(|n| (*n).clone())
    }

    /// B-2026-08-17-7 — the value/constructor type of a USER-declared enum
    /// variant whose bare name collides with a prelude type, for the ONE
    /// position where that name can only mean the variant: a bare identifier
    /// that is the whole expression (which is also how `infer_call` types a
    /// call's callee). `None` for every name the built-ins genuinely own.
    ///
    /// This deliberately does NOT live in `resolve_identifier_type`'s variant
    /// fallback, whose prelude skip is load-bearing for a different caller:
    /// that helper also resolves a path's FIRST segment, where `String` in
    /// `String.from(...)` must stay the type even when `Json.String(String)`
    /// exists — the Slice F scenario its comment documents. Here, by
    /// contrast, the identifier is the entire expression, so a type or module
    /// name has no legal meaning at all; before this fallback every one of
    /// these shapes was an error (B-2026-08-11-6's diagnostic, with
    /// B-2026-08-17-5's qualified-form remedy). Resolving them can therefore
    /// change no working program — it converts exactly the erroring set, and
    /// it converts it to the meaning PATTERN position has always given the
    /// same bare name (`match t { Span(n, w) => .. }` binds the user's
    /// variant), closing the positional asymmetry the row measured.
    ///
    /// Exclusions, each load-bearing:
    ///   - `builtin_variant_owner` names (`Some`/`None`/`Ok`/`Err`): the
    ///     built-in variant IS bare-callable, resolves fine, and never
    ///     reaches the error path — listed here for the reader, not the code.
    ///   - stdlib-origin and prelude-NAMED enums: only a variant the USER
    ///     declared can outrank a prelude name they also chose to use.
    ///   - `Struct`-shaped variants: no bare-call form exists for them
    ///     anywhere, so they keep the diagnostic (with -5's qualified-form
    ///     remedy) rather than resolving to something uncallable.
    ///
    /// Owner choice mirrors `user_enum_owning_variant` (sorted, first) so the
    /// diagnostic and the resolution can never name different enums.
    pub(super) fn user_variant_value_type(&self, name: &str) -> Option<Type> {
        if Self::builtin_variant_owner(name).is_some() {
            return None;
        }
        let owner = self.user_enum_owning_variant(name)?;
        let enum_info = &self.env.enums[&owner];
        let (_, variant_type) = enum_info.variants.iter().find(|(v, _)| v == name)?;
        let return_args: Vec<Type> = enum_info
            .generic_params
            .iter()
            .map(|p| Type::TypeParam(p.clone()))
            .collect();
        let return_ty = Type::Named {
            name: owner.clone(),
            args: return_args,
        };
        match variant_type {
            VariantTypeInfo::Unit => Some(return_ty),
            VariantTypeInfo::Tuple(fields) => Some(Type::Function {
                params: fields.clone(),
                return_type: Box::new(return_ty),
            }),
            VariantTypeInfo::Struct(_) => None,
        }
    }

    pub(super) fn resolve_path_type(&mut self, segments: &[String], span: &Span) -> Type {
        // Value-binding-rooted field path — `F.value`, `CFG.max`,
        // `OUTER.inner.field`, where the leading segment is a value binding
        // (uppercase local or module-level `let`), not a type. The parser
        // consumes an uppercase-led dotted chain greedily into a single
        // multi-segment `Path` (`src/parser/exprs.rs` — the `while
        // self.eat(&Token::Dot)` loop), so `let F: Foo = Foo { value: 5 };
        // let x: i64 = F.value;` arrives here as `Path([F, value])` rather
        // than a `FieldAccess`, and `OUTER.inner.field` as a 3-segment
        // `Path`. Sibling of the uppercase-receiver method-dispatch arm in
        // `infer_call`: that arm covers `Call(Path)` shapes
        // (`REGISTRY.insert(k, v)`), this one covers bare-Path field reads.
        // Without it the trailing segments are dropped at the
        // `resolve_identifier_type(first)` fallthrough below and the path
        // resolves to the *binding's* type instead of the *field's* type, so
        // an annotated binding fails with `expected 'i64', found 'Foo'`.
        // Codegen's `compile_path_expr` already lowers `Path([BINDING,
        // field])` correctly against `module_bindings`, so the fix is
        // typechecker-only. The predicate is the same one the method-dispatch
        // arm reuses; it excludes known type names, so the enum-variant /
        // associated-fn dispatch in the `len() == 2` block below is untouched.
        // Surfaced by slice-10 `test_e2e_modbind_struct_literal`.
        if segments.len() >= 2 && self.path_first_segment_is_value_binding(&segments[0]) {
            let mut current = self.resolve_identifier_type(&segments[0], span);
            let mut walked_all = true;
            for member in &segments[1..] {
                let Type::Named {
                    name: struct_name, ..
                } = &current
                else {
                    // Non-struct intermediate (tuple, primitive, …) — bail to
                    // the existing identifier-resolution path unchanged.
                    walked_all = false;
                    break;
                };
                let Some(struct_info) = self.env.structs.get(struct_name).cloned() else {
                    walked_all = false;
                    break;
                };
                let field = struct_info
                    .fields
                    .iter()
                    .find(|(fname, _, _)| fname == member);
                match field {
                    Some((_, ftype, is_pub)) => {
                        if !is_pub {
                            self.check_cross_module_field_access(struct_name, member, span);
                        }
                        current = ftype.clone();
                    }
                    None => {
                        // Known struct, unknown field — same diagnostic ordinary
                        // field access uses, keyed off the receiver's type (not a
                        // "type 'Foo' is not callable" misdirection).
                        let available: Vec<&str> = struct_info
                            .fields
                            .iter()
                            .map(|(n, _, _)| n.as_str())
                            .collect();
                        self.type_error(
                            format!(
                                "no field '{}' on struct '{}', available fields: {}",
                                member,
                                struct_name,
                                available.join(", ")
                            ),
                            *span,
                            TypeErrorKind::UndefinedField,
                        );
                        return Type::Error;
                    }
                }
            }
            if walked_all {
                return current;
            }
        }

        if segments.len() == 2 {
            let type_name = &segments[0];
            let member = &segments[1];

            // `ExitCode.SUCCESS` / `ExitCode.FAILURE` — paren-free
            // associated constants of the `ExitCode` distinct type
            // (Phase-8 entry-point contract Slice B). Parsed as a
            // 2-segment `Path` (not a `FieldAccess`, since `ExitCode` is
            // a known type name). Resolve to the `ExitCode` type itself
            // — NOT the bare `i32` base — so `main() -> ExitCode {
            // ExitCode.SUCCESS }` type-checks. The interpreter / codegen
            // sibling intercepts yield the matching `0` / `1`.
            if crate::prelude::lookup_exitcode_const(type_name, member).is_some() {
                return Type::Named {
                    name: type_name.clone(),
                    args: Vec::new(),
                };
            }

            // Check for enum variant. Generic enums thread their declared
            // type parameters through the return type's `args` so call-site
            // inference can solve them (see `infer_call`).
            if let Some(enum_info) = self.env.enums.get(type_name).cloned() {
                for (variant_name, variant_type) in &enum_info.variants {
                    if variant_name == member {
                        let return_args: Vec<Type> = enum_info
                            .generic_params
                            .iter()
                            .map(|p| Type::TypeParam(p.clone()))
                            .collect();
                        let return_ty = Type::Named {
                            name: type_name.clone(),
                            args: return_args,
                        };
                        match variant_type {
                            VariantTypeInfo::Unit => return return_ty,
                            VariantTypeInfo::Tuple(fields) => {
                                return Type::Function {
                                    params: fields.clone(),
                                    return_type: Box::new(return_ty),
                                };
                            }
                            _ => {}
                        }
                    }
                }
            }

            // Check for associated function (from impl). No call-site args
            // context — type_name comes from a Path expression without
            // generic args. Theme-4 conservative: only generic-on-name
            // impls participate; specialized impls (`impl Foo for
            // Bar[i32]`) need an args-aware path-expr lookup that this
            // site doesn't carry.
            for imp in &self.env.impls.clone() {
                if imp.target_type == *type_name && imp.target_args.is_empty() {
                    if let Some(sig) = imp.methods.get(member) {
                        // Phase-8 line 96 — associated-function use-site
                        // stability lint (`Server.serve_static(...)` and any
                        // other `Type.method(...)` assoc call). This path
                        // never touches `method_callee_types`, so the check
                        // keys directly off the resolved `(type_name, member)`.
                        self.check_method_stability(type_name, member, span);
                        return Type::Function {
                            params: sig.params.clone(),
                            return_type: Box::new(sig.return_type.clone()),
                        };
                    }
                }
            }

            // Module-path free functions registered as "module.fn" in the
            // function table — `process.exit`, `env.args`, `env.var`. The
            // ambient effect-resource methods (`Stdin.read_line`,
            // `FileSystem.write`, …) used to land here too, but the slice-1
            // through slice-3 migration moved every `Type.method` entry into
            // `env.impls` via baked source, so this fallback now only serves
            // module-path free functions.
            let dotted = format!("{}.{}", type_name, member);
            if let Some(sig) = self.env.functions.get(&dotted) {
                return Type::Function {
                    params: sig.params.clone(),
                    return_type: Box::new(sig.return_type.clone()),
                };
            }

            // User effect-resource method dispatch: `R.method(args)`
            // where `R` is a user-declared `effect resource`. Resolve the
            // method signature from the resource's provider trait
            // (`effect resource R: Trait;`) or, for a trait-less
            // resource, from a representative override impl recovered by
            // the env-build `with_provider` pre-scan. Returning a real
            // `Type::Function` here is what types an untyped
            // `let got = Store.lookup(1)` binding — without it the call
            // collapsed to the silent `Type::Error` fallthrough, the
            // `method_unwrap_inner_types` side-table never populated, and
            // codegen failed with "no handler for method 'is_some'"
            // (bugs.md). Unresolvable shapes (no trait, no statically
            // visible override) keep the pre-existing permissive
            // fallthrough so nothing that typechecked before is rejected.
            if let Some(bounds) = self.user_effect_resources.get(type_name).cloned() {
                if let Some((params, return_type)) =
                    self.resource_dispatch_signature(type_name, &bounds, member)
                {
                    return Type::Function {
                        params,
                        return_type: Box::new(return_type),
                    };
                }
            }

            // None of the special arms matched. If `type_name` is a known
            // type — registered enum, registered struct, prelude primitive,
            // or prelude type — emit a clean "no associated function"
            // diagnostic instead of falling through to the silent
            // identifier-resolution path below (which returns `Type::Error`
            // with no user-facing diagnostic). Without this, a call like
            // `String.from_utf8(buf)` (spec'd in design.md but not yet
            // implemented in `runtime/stdlib/`) or any typo
            // (`String.totally_made_up_method(buf)`) propagates a
            // permissive sentinel type, and the user sees the failure
            // first in *codegen* with a misleading "no handler for
            // method 'unwrap' on variable 'x'" — sending future debuggers
            // chasing a phantom heap-payload codegen bug instead of the
            // actual missing / typo'd stdlib API. Surfaced 2026-05-22
            // building the kata-91 bench mirror. Paired with the
            // `Pipeline::has_fatal_errors` extension in `src/cli.rs` —
            // without that companion change, `cmd_build` runs codegen
            // after collecting non-fatal typecheck errors and the
            // codegen failure still wins the user's stderr.
            //
            // **Ambient resource exemption.** Names in
            // `PRELUDE_EFFECT_RESOURCES` (`Clock`, `RandomSource`,
            // `FileSystem`, …) are explicitly *not* gated by this
            // check. At a `with_provider[R](provider, || …)` site (and
            // in the REPL's `:provide R = T {}` flow), the runtime
            // substitutes a user-supplied type whose method surface
            // can name *any* identifier — the typechecker has no way
            // to know which methods that provider will eventually
            // implement, so the original silent fallthrough is
            // load-bearing for this dispatch shape. Without the
            // exemption, `Clock.now()` / `RandomSource.next()` /
            // `:provide RandomSource = FakeRng {}` followed by
            // `RandomSource.next()` all break at typecheck.
            if self.is_known_type_name(type_name)
                && !crate::prelude::PRELUDE_EFFECT_RESOURCES.contains(&type_name.as_str())
                // A comptime-derived type's associated fns (e.g. a derived
                // `decode`) are synthesized after typecheck — its surface is
                // open, so don't claim the function is missing.
                && !self.type_has_comptime_derive(type_name)
            {
                self.type_error(
                    format!(
                        "no associated function '{}' on type '{}'",
                        member, type_name
                    ),
                    *span,
                    TypeErrorKind::NoMethodFound,
                );
                return Type::Error;
            }
        }
        // First segment as identifier
        if let Some(first) = segments.first() {
            return self.resolve_identifier_type(first, span);
        }
        Type::Error
    }

    /// Resolve the `(params, return_type)` signature for a user
    /// effect-resource dispatch call `R.method(args)`. Trait-ful
    /// resources (`effect resource R: Trait;`) read the trait's method
    /// declaration (user program first, then baked stdlib — via
    /// [`Self::find_trait_method`]) and lower its signature; the
    /// receiver (`self` / `ref self` / `mut ref self`) is not part of
    /// the call-site argument list, so only the declared params lower.
    /// Trait-less resources read the representative override impl
    /// recovered by the env-build `with_provider` pre-scan — its
    /// inherent-impl method signatures are already lowered in
    /// `env.impls`. All overrides of a resource share their lowered
    /// method signatures (the vtable-dispatch invariant), so the
    /// representative is authoritative. `None` when the signature
    /// can't be resolved — callers fall through to the pre-existing
    /// permissive path.
    fn resource_dispatch_signature(
        &mut self,
        resource: &str,
        bounds: &[crate::ast::ProviderBound],
        member: &str,
    ) -> Option<(Vec<Type>, Type)> {
        // A MULTI-BOUND resource (`: A + B`, design.md:7216) dispatches
        // `R.method(..)` through whichever bound declares `method` — the union
        // of the bounds' method sets is the resource's surface, since a
        // provider must implement all of them. `check_effect_resource_bounds`
        // rejects a name declared by two bounds at the DECLARATION, so at most
        // one match survives here and "first that declares it" is also "the
        // only one that declares it". B-2026-08-19-3.
        let owner = bounds
            .iter()
            .find(|b| self.find_trait_method(&b.name, member).is_some())
            .cloned();
        match owner {
            Some(bound) => {
                let trait_name = bound.name.as_str();
                // The resource's declared arguments, for the generic-bound form
                // `effect resource RequestCh: Channel[i64];` (B-2026-08-18-41).
                // Absent for every other resource, and the two branches below
                // are deliberately gated on it so a plain `: Trait` bound
                // lowers exactly as it did before this existed.
                let declared_args = bound.args.clone();
                // The TRAIT's own generic params (`trait Channel[T]` -> ["T"]).
                // These must be IN SCOPE when lowering, or `T` lowers to
                // `Type::Named { name: "T" }` and no substitution can reach it
                // -- `substitute_type_params` rewrites `Type::TypeParam` only.
                // That is why binding the args alone was not enough.
                //
                // Collected only when the resource declared arguments. Adding
                // them unconditionally would also change the UNBOUND case
                // (`effect resource RequestCh: Channel;`), turning a `T` that
                // today is a named type into a free type param -- a different
                // unification result for a program this row is not about.
                let trait_gp: Vec<String> = if declared_args.is_some() {
                    self.find_trait_def(trait_name)
                        .map(|t| Self::generic_param_names(&t.generic_params))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                // Clone the TypeExprs out first: `find_trait_method`
                // borrows `self` and `lower_type_expr` needs `&mut self`.
                let (param_tes, ret_te, mut method_gp) = {
                    let m = self.find_trait_method(trait_name, member)?;
                    (
                        m.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                        m.return_type.clone(),
                        Self::generic_param_names(&m.generic_params),
                    )
                };
                method_gp.extend(trait_gp.iter().cloned());
                let params: Vec<Type> = param_tes
                    .iter()
                    .map(|te| self.lower_type_expr(te, &method_gp))
                    .collect();
                let return_type = ret_te
                    .as_ref()
                    .map(|te| self.lower_type_expr(te, &method_gp))
                    .unwrap_or(Type::Unit);
                // Bind the TRAIT's own generic params from the resource's
                // declared arguments — `effect resource RequestCh:
                // Channel[i64];` against `trait Channel[T]` makes `T` mean
                // `i64` at every `RequestCh.send(v)` site. Without this the
                // lowered signature still mentions `T`, which nothing binds,
                // and the call fails with "expected 'T', found 'i64'" — naming
                // a type parameter the user never wrote (B-2026-08-18-41).
                // Empty for a non-generic bound, which is every other resource.
                let subs = self.provider_trait_subs(&trait_gp, declared_args);
                if subs.is_empty() {
                    return Some((params, return_type));
                }
                Some((
                    params
                        .iter()
                        .map(|t| super::inference::substitute_type_params(t, &subs))
                        .collect(),
                    super::inference::substitute_type_params(&return_type, &subs),
                ))
            }
            None => {
                // No bound declares `member`. For a BOUND resource that is a
                // genuine miss (the caller falls through to the permissive
                // path, as it did before multi-bounds existed); only a BARE
                // `effect resource R;` reads a representative override impl.
                if !bounds.is_empty() {
                    return None;
                }
                let override_ty = self.user_resource_override_types.get(resource)?;
                for imp in &self.env.impls {
                    if imp.trait_name.is_none() && imp.target_type == *override_ty {
                        if let Some(sig) = imp.methods.get(member) {
                            return Some((sig.params.clone(), sig.return_type.clone()));
                        }
                    }
                }
                None
            }
        }
    }

    /// Substitution binding a provider trait's generic parameters to the
    /// arguments the resource declared: `trait Channel[T]` +
    /// `effect resource RequestCh: Channel[i64];` yields `{T -> i64}`.
    ///
    /// Empty -- and therefore a no-op at the call site -- when the resource
    /// declared no arguments, which is every resource in the corpus today, or
    /// when the trait's definition could not be found. Both cases leave the
    /// pre-existing permissive behaviour rather than inventing a binding.
    ///
    /// ARITY IS NOT CHECKED HERE. `zip` truncates, so a wrong count would
    /// degrade to binding the prefix; the count is verified once at the
    /// declaration by [`Self::check_effect_resource_trait_arity`], which can
    /// point a diagnostic at the declaration instead of firing once per
    /// dispatch site -- and fires there even for a resource that is declared
    /// but never called.
    fn provider_trait_subs(
        &mut self,
        trait_gp: &[String],
        declared_args: Option<Vec<crate::ast::GenericArg>>,
    ) -> HashMap<String, SubstValue> {
        let Some(args) = declared_args else {
            return HashMap::new();
        };
        let mut subs = HashMap::new();
        for (name, arg) in trait_gp.iter().zip(args.iter()) {
            if let crate::ast::GenericArg::Type(te) = arg {
                let ty = self.lower_type_expr(te, &[]);
                subs.insert(name.clone(), SubstValue::Type(ty));
            }
        }
        subs
    }

    /// True when `name` denotes a known Type-class identifier — a registered
    /// enum or struct, a prelude primitive (e.g. `String`, `i32`), or a
    /// prelude type (e.g. `Option`, `Result`, `Vec`). Used by
    /// `resolve_path_type` to decide whether to surface a clean
    /// "no associated function" diagnostic when a 2-segment `Type.method`
    /// path fails to resolve all of its arms — vs. falling through to the
    /// silent identifier-resolution path used for non-type-shaped paths
    /// (e.g., `obj.field.method()` where the first segment is a value).
    pub(super) fn is_known_type_name(&self, name: &str) -> bool {
        self.env.enums.contains_key(name)
            || self.env.structs.contains_key(name)
            || crate::prelude::PRELUDE_PRIMITIVES.contains(&name)
            || crate::prelude::PRELUDE_TYPES.contains(&name)
    }

    /// True when the struct/enum `type_name` carries a `#[derive(X)]` whose
    /// `derive_<snake(X)>` is a comptime fn (in the user program or the baked
    /// stdlib) — i.e. a comptime-backed derive that synthesizes methods *after*
    /// typecheck (e.g. `#[derive(Message)]` → `encode`/`decode`/`merge`).
    ///
    /// Such a type has an **open** method set the typechecker can't enumerate
    /// (the methods don't exist yet at this phase — the comptime pass adds them
    /// later), so method / associated-function resolution must not report its
    /// members as missing. Mirrors `comptime::collect_derive_fns`'s lookup.
    /// The trade-off — a typo'd method on a comptime-derived type isn't flagged
    /// at typecheck — is the price of the post-typecheck expansion model and is
    /// caught when the generated impl fails to provide it.
    pub(super) fn type_has_comptime_derive(&self, type_name: &str) -> bool {
        let traits = match self.env.structs.get(type_name) {
            Some(s) => &s.derived_traits,
            None => match self.env.enums.get(type_name) {
                Some(e) => &e.derived_traits,
                None => return false,
            },
        };
        if traits.is_empty() {
            return false;
        }
        let is_comptime_derive_fn = |fn_name: &str| -> bool {
            let in_items = |items: &[Item]| {
                items
                    .iter()
                    .any(|it| matches!(it, Item::Function(f) if f.is_comptime && f.name == fn_name))
            };
            in_items(&self.program.items)
                || crate::prelude::STDLIB_PROGRAMS
                    .iter()
                    .any(|(_, p)| in_items(&p.items))
        };
        traits.iter().any(|t| {
            is_comptime_derive_fn(&format!("derive_{}", crate::comptime::to_snake_case(t)))
        })
    }

    /// Predicate for the uppercase-receiver method-dispatch rewrite in
    /// `infer_call`. Returns true when the first segment of a
    /// `Path([X, method])` callee resolves as a value binding rather
    /// than a Type-class root. Locals shadow types by Kara design
    /// (the resolver's scope rule), so the `local_scope` lookup wins
    /// against any same-named type unconditionally; module-level
    /// bindings and `const` declarations live in `env.constants` and
    /// participate when there is no same-named known type (the latter
    /// guard preserves the existing `Vec.new()` / `String.from(...)`
    /// associated-call dispatch). The shape `Vec[i64].new()` carries
    /// `generic_args: Some(...)` so it routes through the UFCS path,
    /// not this one; same for longer paths (`module.Sub.fn()`).
    pub(super) fn path_first_segment_is_value_binding(&self, name: &str) -> bool {
        if self.local_scope.lookup(name).is_some() {
            return true;
        }
        self.env.constants.contains_key(name) && !self.is_known_type_name(name)
    }

    // ── Binary / Unary Operators ────────────────────────────────

    /// Element-wise arithmetic on `Vector[T, N]` (design.md § Portable SIMD).
    /// Both operands must be the *same* `Vector[T, N]` type; the result is that
    /// type. Slice 1 supports `+ - * / %`; bitwise ops and comparison-producing
    /// `Mask` results are deferred to later slices. A vector-vs-scalar mix is a
    /// type error (splat-from-scalar is an explicit `Vector::splat` call, not an
    /// implicit broadcast).
    fn infer_vector_binary(
        &mut self,
        op: &BinOp,
        left_ty: &Type,
        right_ty: &Type,
        left: &Expr,
        right: &Expr,
        _span: &Span,
    ) -> Type {
        let is_arith = matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        );
        let is_bitwise = matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor);
        let is_shift = matches!(op, BinOp::Shl | BinOp::Shr);
        let is_compare = matches!(
            op,
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq
        );
        if !is_arith && !is_bitwise && !is_shift && !is_compare {
            self.type_error(
                format!(
                    "this operator is not yet supported on Vector[T, N] \
                     (element-wise + - * / % and & | ^ << >> on lanes, comparisons \
                     < <= > >= == != yielding a mask); found operands '{}' and '{}'",
                    type_display(left_ty),
                    type_display(right_ty)
                ),
                left.span,
                TypeErrorKind::InvalidBinaryOp,
            );
            return Type::Error;
        }
        match (left_ty, right_ty) {
            (
                Type::Vector {
                    element: le,
                    lanes: ll,
                },
                Type::Vector {
                    element: re,
                    lanes: rl,
                },
            ) => {
                if le != re || ll != rl {
                    self.type_error(
                        format!(
                            "element-wise vector operators require both operands to be the \
                             same Vector[T, N] type; found '{}' and '{}'",
                            type_display(left_ty),
                            type_display(right_ty)
                        ),
                        right.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                // Bitwise `& | ^` and shifts `<< >>` are integer-lane only —
                // float vectors have no meaningful bit ops. Arithmetic /
                // comparisons stay open to all numeric lanes. (Shift lowers to a
                // per-lane `shl`/`ashr`/`lshr`, so the shift-amount operand is a
                // same-width vector — splat a scalar amount to broadcast it.)
                if (is_bitwise || is_shift) && !matches!(**le, Type::Int(_) | Type::UInt(_)) {
                    self.type_error(
                        format!(
                            "bitwise / shift vector operators (& | ^ << >>) require integer \
                             lanes; Vector element is '{}'",
                            type_display(le)
                        ),
                        left.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                // Comparisons yield a per-lane mask `Vector[bool, N]` (lowers to
                // `<N x i1>`); arithmetic / bitwise return the operand type.
                if is_compare {
                    Type::Vector {
                        element: Box::new(Type::Bool),
                        lanes: ll.clone(),
                    }
                } else {
                    left_ty.clone()
                }
            }
            _ => {
                self.type_error(
                    format!(
                        "element-wise vector arithmetic requires both operands to be Vector[T, N]; \
                         found '{}' and '{}' (use Vector::splat to broadcast a scalar)",
                        type_display(left_ty),
                        type_display(right_ty)
                    ),
                    right.span,
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
        }
    }

    /// Element-wise arithmetic on `Tensor[T, Shape]`. Only `+ - * /` are
    /// defined (design.md § Numerical Types); the result is a fresh tensor.
    ///
    /// - **Tensor ⊕ Tensor:** exact shape match. Concrete-vs-concrete dim
    ///   mismatch is a static `E_SHAPE`; rank mismatch likewise. `?` dims
    ///   defer to a codegen runtime guard. Both element types must match and
    ///   be numeric. Shape mismatch points at the `broadcast_*` methods
    ///   (a future slice).
    /// - **Tensor ⊕ scalar / scalar ⊕ Tensor:** the scalar is `T` (unsuffixed
    ///   literals promote to `T` via the Q4 rule); result shape = the tensor's.
    ///
    /// Like the shape-transform family, the receiver's rank must be statically
    /// known — bare-`S` / splice shapes get a focused error.
    fn infer_tensor_binary(
        &mut self,
        op: &BinOp,
        left_ty: &Type,
        right_ty: &Type,
        left: &Expr,
        right: &Expr,
        span: &Span,
    ) -> Type {
        if !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div) {
            self.type_error(
                format!(
                    "this operator is not defined on Tensor[T, Shape] — only \
                     element-wise + - * / (and unary -) are supported; found \
                     operands '{}' and '{}'",
                    type_display(left_ty),
                    type_display(right_ty)
                ),
                *span,
                TypeErrorKind::InvalidBinaryOp,
            );
            return Type::Error;
        }

        let left_args = tensor_named_args(left_ty).map(<[Type]>::to_vec);
        let right_args = tensor_named_args(right_ty).map(<[Type]>::to_vec);

        match (left_args, right_args) {
            (Some(la), Some(ra)) => {
                let Some((le, ls)) = self.tensor_static_shape(&la, "this binary operator", span)
                else {
                    return Type::Error;
                };
                let Some((re, rs)) = self.tensor_static_shape(&ra, "this binary operator", span)
                else {
                    return Type::Error;
                };
                if !is_numeric(&le) {
                    self.type_error(
                        format!(
                            "element-wise tensor arithmetic requires a numeric element type, \
                             found '{}'",
                            type_display(&le)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                if le != re {
                    self.type_error(
                        format!(
                            "tensor operands must share an element type; found '{}' and '{}'",
                            type_display(&le),
                            type_display(&re)
                        ),
                        right.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                if ls.len() != rs.len() {
                    self.type_error(
                        format!(
                            "shape rank mismatch in element-wise tensor op: '{}' vs '{}' — \
                             tensor-tensor arithmetic requires the same rank (broadcasting is \
                             v1.5; see broadcast_add / broadcast_mul)",
                            type_display(left_ty),
                            type_display(right_ty)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                let mut merged = Vec::with_capacity(ls.len());
                for (i, (l, r)) in ls.iter().zip(rs.iter()).enumerate() {
                    match merge_tensor_dim(l, r) {
                        Ok(d) => merged.push(d),
                        Err(()) => {
                            self.type_error(
                                format!(
                                    "shape dim {} mismatch in element-wise tensor op: '{}' vs \
                                     '{}' — operands must have an identical shape (broadcasting \
                                     is v1.5; see broadcast_add / broadcast_mul)",
                                    i,
                                    type_display(left_ty),
                                    type_display(right_ty)
                                ),
                                *span,
                                TypeErrorKind::TypeMismatch,
                            );
                            return Type::Error;
                        }
                    }
                }
                Type::Named {
                    name: "Tensor".to_string(),
                    args: vec![le, Type::Shape(merged)],
                }
            }
            (Some(la), None) => {
                let Some((te, ts)) = self.tensor_static_shape(&la, "this binary operator", span)
                else {
                    return Type::Error;
                };
                if !self.check_tensor_scalar(&te, right_ty, right, span) {
                    return Type::Error;
                }
                Type::Named {
                    name: "Tensor".to_string(),
                    args: vec![te, Type::Shape(ts)],
                }
            }
            (None, Some(ra)) => {
                let Some((te, ts)) = self.tensor_static_shape(&ra, "this binary operator", span)
                else {
                    return Type::Error;
                };
                if !self.check_tensor_scalar(&te, left_ty, left, span) {
                    return Type::Error;
                }
                Type::Named {
                    name: "Tensor".to_string(),
                    args: vec![te, Type::Shape(ts)],
                }
            }
            (None, None) => {
                // The caller only routes here when at least one side is a tensor.
                unreachable!("infer_tensor_binary: neither operand is a tensor")
            }
        }
    }

    /// Extract `(elem, dims)` from a `Tensor[T, Shape]` generic-arg list,
    /// requiring a statically-known, splice-free rank. Emits a focused error
    /// and returns `None` for a bare-`S` / splice shape. `what` names the
    /// operation in the diagnostic.
    fn tensor_static_shape(
        &mut self,
        args: &[Type],
        what: &str,
        span: &Span,
    ) -> Option<(Type, Vec<DimArg>)> {
        match args {
            [elem, Type::Shape(dims)]
                if !dims
                    .iter()
                    .any(|d| matches!(d, DimArg::Splice(_) | DimArg::SpliceVar(_))) =>
            {
                Some((elem.clone(), dims.clone()))
            }
            _ => {
                self.type_error(
                    format!(
                        "{} requires the tensor's rank to be statically known; \
                         a bare-`S` or splice-bearing shape isn't supported here \
                         (rank-polymorphic tensor arithmetic is v1.5 shape arithmetic)",
                        what
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                None
            }
        }
    }

    /// Validate the scalar operand of a `Tensor[T, Shape] ⊕ scalar` op. An
    /// unsuffixed numeric literal promotes to the element type `T` (Q4 rule,
    /// re-recording the literal's span); a typed scalar must already be `T`.
    /// Returns `false` (after emitting a diagnostic) on mismatch.
    fn check_tensor_scalar(
        &mut self,
        elem: &Type,
        scalar_ty: &Type,
        scalar: &Expr,
        span: &Span,
    ) -> bool {
        if !is_numeric(elem) {
            self.type_error(
                format!(
                    "element-wise tensor arithmetic requires a numeric element type, found '{}'",
                    type_display(elem)
                ),
                *span,
                TypeErrorKind::TypeMismatch,
            );
            return false;
        }
        if let Some(is_float_const) = Self::unsuffixed_const_scalar_is_float(scalar) {
            // A float constant cannot promote to an integer element type.
            let can_promote = !(is_float_const && matches!(elem, Type::Int(_) | Type::UInt(_)));
            if can_promote {
                self.record_expr_type(&scalar.span, elem);
                return true;
            }
        }
        if Self::typed_scalar_matches_element(scalar_ty, elem) {
            return true;
        }
        self.type_error(
            format!(
                "scalar operand of element-wise tensor arithmetic must match the element \
                 type '{}', found '{}' — cast explicitly with `as {}` (an unsuffixed \
                 literal takes the element type automatically)",
                type_display(elem),
                type_display(scalar_ty),
                type_display(elem)
            ),
            scalar.span,
            TypeErrorKind::TypeMismatch,
        );
        false
    }

    /// B-2026-08-14-14 — does a TYPED scalar operand satisfy an element-wise
    /// op's element type?
    ///
    /// Both `check_tensor_scalar` and `check_column_scalar` documented the rule
    /// as "a typed scalar must already be `T`" and then implemented
    /// `scalar_ty == elem || types_compatible(scalar_ty, elem)`.
    /// `types_compatible` is permissive across the numeric types — correct
    /// while one side is still being solved, wrong once both are fixed — so
    /// every width and every domain was admitted and silently coerced to the
    /// element type. `Tensor[u8] + x` with `x: i64 = 300` typechecked, TRAPPED
    /// with "integer overflow" under `--interp`, and printed 45 from the binary.
    ///
    /// The element type is the answer's type, so a concrete numeric scalar has
    /// to BE it; design.md's literal-promotion section writes this exact
    /// program with a tensor and specifies `arr + x` (typed `i64`, `f64`
    /// element) as "compile error: expected f64, got i64 — add `x as f64`".
    /// Non-numeric or not-yet-concrete operands keep `types_compatible`, which
    /// is where its permissiveness is doing real work.
    /// B-2026-08-14-14 — is this scalar an UNSUFFIXED numeric constant
    /// expression, and is it float-domain? `Some(true)` for a float constant,
    /// `Some(false)` for an integer one, `None` when it is not a constant.
    ///
    /// The promotion this feeds used to key on a BARE literal, which meant
    /// `sv * -1.0` — a unary minus on one — took the typed path instead and was
    /// only accepted because that path fell through to `types_compatible`. Two
    /// `std.autograd` lines are written that way, so tightening the typed path
    /// without widening this one would have rejected the stdlib for a constant
    /// the compiler can see. Same argument, and the same shape, as
    /// B-2026-08-14-12's const-expression exemption: a typed variable is a leaf
    /// that returns `None`, so this cannot swallow the case the gate is for.
    ///
    /// A mixed-domain constant (`2 * 1.5`) reports float, which is what the
    /// caller needs to refuse promoting it into an integer element.
    fn unsuffixed_const_scalar_is_float(expr: &Expr) -> Option<bool> {
        match &expr.kind {
            ExprKind::Integer(_, None) => Some(false),
            ExprKind::Float(_, None) => Some(true),
            ExprKind::Unary { op, operand } => matches!(op, UnaryOp::Neg)
                .then(|| Self::unsuffixed_const_scalar_is_float(operand))
                .flatten(),
            ExprKind::Binary { op, left, right } => {
                if !matches!(
                    op,
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                ) {
                    return None;
                }
                let l = Self::unsuffixed_const_scalar_is_float(left)?;
                let r = Self::unsuffixed_const_scalar_is_float(right)?;
                Some(l || r)
            }
            _ => None,
        }
    }

    fn typed_scalar_matches_element(scalar_ty: &Type, elem: &Type) -> bool {
        if scalar_ty == elem {
            return true;
        }
        let concrete_numeric =
            |t: &Type| matches!(t, Type::Int(_) | Type::UInt(_) | Type::Float(_));
        if concrete_numeric(scalar_ty) && concrete_numeric(elem) {
            return false;
        }
        types_compatible(scalar_ty, elem)
    }

    /// Element-wise three-valued-logic arithmetic / comparison on `Column[T]`
    /// (phase-11 Arrow). `+ - * /` yield `Column[T]` (numeric element);
    /// `== != < <= > >=` yield `Column[bool]` (any matching element). Either
    /// form null-propagates at runtime. Col-col requires a shared element
    /// type (length agreement is a runtime check); col-scalar / scalar-col
    /// take a scalar of the element type (unsuffixed literals promote, Q4).
    fn infer_column_binary(
        &mut self,
        op: &BinOp,
        left_ty: &Type,
        right_ty: &Type,
        left: &Expr,
        right: &Expr,
        span: &Span,
    ) -> Type {
        let is_arith = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
        let is_cmp = matches!(
            op,
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
        );
        if !is_arith && !is_cmp {
            self.type_error(
                format!(
                    "this operator is not defined on Column[T] — only element-wise \
                     + - * / (yielding Column[T]) and comparisons == != < <= > >= \
                     (yielding Column[bool]), plus unary -, are supported; found \
                     operands '{}' and '{}'",
                    type_display(left_ty),
                    type_display(right_ty)
                ),
                *span,
                TypeErrorKind::InvalidBinaryOp,
            );
            return Type::Error;
        }
        // The result element type: T for arithmetic, bool for comparison.
        let result = |elem: Type| Type::Named {
            name: "Column".to_string(),
            args: vec![if is_arith { elem } else { Type::Bool }],
        };
        match (
            column_elem(left_ty).cloned(),
            column_elem(right_ty).cloned(),
        ) {
            (Some(le), Some(re)) => {
                if le != re {
                    self.type_error(
                        format!(
                            "column operands must share an element type; found '{}' and '{}'",
                            type_display(&le),
                            type_display(&re)
                        ),
                        right.span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                if is_arith && !is_numeric(&le) {
                    self.type_error(
                        format!(
                            "element-wise column arithmetic requires a numeric element type, \
                             found '{}'",
                            type_display(&le)
                        ),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                result(le)
            }
            (Some(le), None) => {
                if !self.check_column_scalar(&le, right_ty, right, is_arith, span) {
                    return Type::Error;
                }
                result(le)
            }
            (None, Some(re)) => {
                if !self.check_column_scalar(&re, left_ty, left, is_arith, span) {
                    return Type::Error;
                }
                result(re)
            }
            (None, None) => unreachable!("infer_column_binary: neither operand is a column"),
        }
    }

    /// Validate the scalar operand of a `Column[T] ⊕ scalar` op. For
    /// arithmetic the element must be numeric; for comparison any matching
    /// element is fine. An unsuffixed numeric literal promotes to `T` (Q4,
    /// re-recording the literal's span); otherwise the scalar must be `T`.
    fn check_column_scalar(
        &mut self,
        elem: &Type,
        scalar_ty: &Type,
        scalar: &Expr,
        require_numeric: bool,
        span: &Span,
    ) -> bool {
        if require_numeric && !is_numeric(elem) {
            self.type_error(
                format!(
                    "element-wise column arithmetic requires a numeric element type, found '{}'",
                    type_display(elem)
                ),
                *span,
                TypeErrorKind::TypeMismatch,
            );
            return false;
        }
        if let Some(is_float_const) = Self::unsuffixed_const_scalar_is_float(scalar) {
            // A float constant cannot promote to an integer element type.
            let can_promote = !(is_float_const && matches!(elem, Type::Int(_) | Type::UInt(_)));
            if can_promote && is_numeric(elem) {
                self.record_expr_type(&scalar.span, elem);
                return true;
            }
        }
        // B-2026-08-14-14 — same rule as the tensor sibling; see
        // `typed_scalar_matches_element`.
        if Self::typed_scalar_matches_element(scalar_ty, elem) {
            return true;
        }
        self.type_error(
            format!(
                "scalar operand of an element-wise column op must match the element \
                 type '{}', found '{}' — cast explicitly with `as {}` (an unsuffixed \
                 literal takes the element type automatically)",
                type_display(elem),
                type_display(scalar_ty),
                type_display(elem)
            ),
            scalar.span,
            TypeErrorKind::TypeMismatch,
        );
        false
    }

    /// True iff `ty` is a generic type parameter whose bounds (in the enclosing
    /// scope) name `trait_name` by its last path segment.
    ///
    /// An in-scope type parameter surfaces as `Type::TypeParam` when lowered in
    /// the param context, but as a bare `Type::Named { name, args: [] }` when it
    /// slips through a different lowering path — e.g. a `let x: T` annotation or
    /// a trait method's `-> T` return checked inside a default body (the
    /// Named-vs-TypeParam trap; cf. `Self` lowering). `enclosing_bounds` doubles
    /// as the authoritative in-scope type-param set (`collect_param_bounds`
    /// pre-populates every param name), so a `Named` whose name is a key there
    /// IS that parameter — accept both spellings.
    pub(super) fn type_param_has_trait_bound(&self, ty: &Type, trait_name: &str) -> bool {
        let name = match ty {
            Type::TypeParam(name) => name,
            Type::Named { name, args }
                if args.is_empty() && self.enclosing_bounds.contains_key(name) =>
            {
                name
            }
            _ => return false,
        };
        self.enclosing_bounds.get(name).is_some_and(|bounds| {
            bounds
                .iter()
                .any(|b| b.path.last().is_some_and(|t| t == trait_name))
        })
    }

    /// True iff `ty` is a generic type parameter carrying a `Numeric` bound in
    /// the enclosing scope. Lets the operator checks treat `a + b` / `-a` on a
    /// `T: Numeric` parameter as valid numeric arithmetic — the bound
    /// guarantees `T` instantiates to a primitive numeric type.
    pub(super) fn type_param_has_numeric_bound(&self, ty: &Type) -> bool {
        self.type_param_has_trait_bound(ty, "Numeric")
    }

    /// The stdlib operator trait a binary arithmetic op dispatches through
    /// (`+`→`Add`, `-`→`Sub`, `*`→`Mul`, `/`→`Div`, `%`→`Rem`), if any. Used to
    /// admit `a OP b` on a type parameter carrying that operator-trait bound
    /// (`fn f[T: Add](a: T, b: T) -> T { a + b }`): user operator-trait impls
    /// are forbidden (resolver: "operator traits are stdlib-only"), so every
    /// concrete instantiation of such a `T` is a primitive numeric / `String`
    /// (for `Add`) / distinct-numeric — all of which codegen already lowers for
    /// this operator once the generic body is monomorphized. Result type is
    /// `T`, mirroring the `Numeric`-bound arm.
    /// Warn when `f16` / `bf16` arithmetic will be promoted to `f32` (or,
    /// on wasm, routed through `__extendhfsf2` / `__truncsfhf2` libcalls)
    /// rather than executed at half width.
    ///
    /// Fires per arithmetic operation, which is what makes the cost visible
    /// where it is paid; `#[allow(f16_software_emulated)]` on the enclosing
    /// item silences the whole region through the normal cascade.
    ///
    /// THE TWO WIDTHS ARE NOT THE SAME QUESTION, and treating them as one was
    /// a false negative on every native-`f16` CPU (B-2026-08-22-30):
    ///
    /// * `f16` is a TARGET capability. `apple-m1` executes it natively (`fadd
    ///   h0, h0, h1`); an x86-64 baseline calls `__extendhfsf2`. So it asks
    ///   [`crate::target::target_has_native_f16`], and on a native host the
    ///   lint correctly stays quiet.
    /// * `bf16` is emulated on EVERY target, because *karac itself* widens it:
    ///   `Codegen::…` computes bf16 arithmetic in `f32` and rounds back with
    ///   the RNE sequence unconditionally, since LLVM 18's AArch64 ISel cannot
    ///   select scalar `bfloat` arithmetic (B-2026-07-22-1) and taking the same
    ///   path everywhere is what keeps the two architectures bit-identical.
    ///   No CPU makes that cost go away, so the CPU is not consulted.
    ///
    /// Measured on `apple-m1` before the split: a `bf16` multiply emitted the
    /// widen-compute-round sequence and produced NO warning, which is the one
    /// outcome the lint exists to prevent.
    fn warn_if_f16_is_software_emulated(&mut self, left: &Type, right: &Type, span: &Span) {
        use crate::typechecker::types::FloatSize;
        let half_size = |t: &Type| match t {
            Type::Float(s @ (FloatSize::F16 | FloatSize::BF16)) => Some(*s),
            _ => None,
        };
        let Some(size) = half_size(left).or_else(|| half_size(right)) else {
            return;
        };
        let width = if half_size(left).is_some() {
            left
        } else {
            right
        };
        let detail = match size {
            // Target-dependent: quiet on a CPU measured to run it in hardware.
            FloatSize::F16 => {
                if crate::target::target_has_native_f16() {
                    return;
                }
                // `resolved_cpu`, not `baseline_cpu_and_features`: the decision
                // just above honours `--target-cpu`, so the message has to name
                // the same CPU or it blames one it never judged.
                let cpu = crate::target::resolved_cpu();
                format!("on this target (cpu `{cpu}`): LLVM promotes each operation to `f32`")
            }
            // Target-independent: karac widens it on every backend.
            FloatSize::BF16 => "on every target: each operation is widened to `f32`".to_string(),
            _ => return,
        };
        self.type_lint_warning(
            format!(
                "`{w}` arithmetic is software-emulated {detail} and rounds back. \
                 Store in `{w}` if you need the space, but compute in `f32` to \
                 make the conversions explicit",
                w = type_display(width),
            ),
            *span,
            TypeErrorKind::TypeMismatch,
            "f16_software_emulated",
        );
    }

    pub(super) fn arithmetic_operator_trait(op: &BinOp) -> Option<&'static str> {
        match op {
            BinOp::Add => Some("Add"),
            BinOp::Sub => Some("Sub"),
            BinOp::Mul => Some("Mul"),
            BinOp::Div => Some("Div"),
            BinOp::Mod => Some("Rem"),
            _ => None,
        }
    }

    /// The arithmetic-rejection message, in the TRAIT language design.md
    /// § Operator Traits mandates: "Diagnostics for a missing impl (e.g.,
    /// `vec1 + vec2`) speak the trait language — \"type Vec[T] does not
    /// implement trait Add\" — not operator language".
    ///
    /// B-2026-08-25-30. The old wording — "arithmetic operator requires numeric
    /// type, found 'T'" — told the reader what the compiler CHECKED, never
    /// which trait was missing nor what to do instead. Both redirects are cheap
    /// because the type is in hand here:
    ///
    ///   • `Vec` / `VecDeque`: § Notably absent says the whole reason
    ///     `impl Add for Vec[T]` does not exist is to force an explicit method
    ///     name, which only works if the diagnostic says the name.
    ///   • a `distinct` type: `#[derive(Arithmetic)]` is the opt-in, and going
    ///     without it was previously unmentioned.
    ///
    /// NOTE ON `concat`: design.md's sentence offers "`vec.concat(other)` or
    /// `vec.extend(other)`", but `Vec.concat()` in this implementation is the
    /// OTHER operation — zero-argument, `Vec[String]` only, joining a Vec's own
    /// elements into one String. Naming it here would send a reader with a
    /// `Vec[i64]` to "Vec.concat() requires String elements". Only `extend` is
    /// named, and the spec/implementation divergence is filed separately.
    ///
    /// An operator with no entry in the table above keeps the old operator-
    /// language wording rather than inventing a trait name for it.
    /// The rejection message for a comparison operator whose derive-based gate
    /// failed, specialized for a type that carries a HAND-WRITTEN
    /// operator-trait impl (B-2026-08-26-10).
    ///
    /// The plain "type 'T' does not implement PartialEq" is a flat denial of an
    /// impl the author is looking at, and "add #[derive(PartialEq)]" is worse
    /// than useless there: it compiles, and then the operator uses structural
    /// comparison (or, for ordering, a DECLARATION-ORDER comparator) while the
    /// body they wrote is never called. Measured on both backends.
    ///
    /// What the two families can offer differs, so they are not merged:
    ///
    /// * EQUALITY has a real workaround. Lowering gates `==` on `impl Eq`
    ///   (`target_type_name` in `lowering.rs` looks up the trait name "Eq", not
    ///   "PartialEq"), so adding the marker `impl Eq for T {}` makes `==` lower
    ///   to the user's `eq` and dispatch correctly — verified on both backends
    ///   with an `eq` that returns `false` for identical values. That is
    ///   actionable advice, so the message gives it.
    ///
    /// * ORDERING now dispatches through `cmp`, so a complete
    ///   `impl PartialOrd + impl Ord` never reaches this message at all
    ///   (`ordering_operator_dispatches` accepts it and lowering emits
    ///   `T.cmp(a, b).is_lt()`). What is left here is the ONE ordering shape
    ///   that still cannot be lowered: an impl that supplies `partial_cmp` but
    ///   no `cmp`, because the `Option[Ordering].is_lt()` its desugaring needs
    ///   is unimplemented in codegen. That has a real workaround — write the
    ///   `impl Ord` too — so the message gives it rather than prescribing the
    ///   derive that would discard the body.
    /// Whether an ordering operator on `ty` can be lowered to a hand-written
    /// comparator body (B-2026-08-26-10).
    ///
    /// This is deliberately NOT a relaxation of `type_supports_partial_ord`.
    /// That predicate answers "does the DERIVE machinery support this type",
    /// and the bug row that opened this work measured what happens when it is
    /// made to answer yes for a hand-written impl: the operator still runs the
    /// declaration-order comparator, so a reversing `cmp` silently produced the
    /// opposite answer. The derive predicate stays exactly as it was; this is a
    /// separate question — "is there a user body for the operator to call" —
    /// asked only at the operator's own gate, and it is true only when lowering
    /// really will route to that body.
    ///
    /// It must therefore agree with `lowering::ordering_dispatch_comparator`.
    /// Accepting here without a matching desugaring there is the failure mode
    /// the row warns about — a rejection traded for a silent wrong answer — so
    /// both sides ask for exactly the same thing: an ordering impl that supplies
    /// `cmp`, or the direct `lt`/`le`/`gt`/`ge` method.
    ///
    /// `partial_cmp` is deliberately NOT enough. The desugaring it would need,
    /// `partial_cmp(a, b).is_lt()`, lands on `Option[Ordering].is_lt()`, which
    /// codegen does not implement at all; accepting on that basis would turn a
    /// typecheck rejection into a run-vs-build divergence. Since `Ord` requires
    /// `PartialOrd`, this only excludes a type that wrote `impl PartialOrd`
    /// alone — and the rejection message says so.
    pub(super) fn ordering_operator_dispatches(&self, ty: &Type) -> bool {
        let name = match ty {
            Type::Named { name, .. } | Type::Shared(name) => name.as_str(),
            _ => return false,
        };
        self.env.impls.iter().any(|imp| {
            imp.trait_name
                .as_deref()
                .is_some_and(|t| t == "PartialOrd" || t == "Ord")
                && imp.target_type == name
                && ["cmp", "lt", "le", "gt", "ge"]
                    .iter()
                    .any(|m| imp.methods.contains_key(*m))
        })
    }

    fn comparison_impl_written_but_undispatched(
        &self,
        ty: &Type,
        equality: bool,
    ) -> Option<String> {
        let name = match ty {
            Type::Named { name, .. } | Type::Shared(name) => name.as_str(),
            _ => return None,
        };
        let disp = type_display(ty);
        if equality {
            // `has_impl("Eq")` short-circuits the gate, so reaching here with
            // an `Eq` impl is impossible; only the partial one can be present.
            if self.env.has_impl("PartialEq", name, &[]) {
                return Some(format!(
                    "'==' cannot dispatch to your 'impl PartialEq for {disp}': the \
                     operator lowers through the 'Eq' marker, so add 'impl Eq for \
                     {disp} {{}}' to enable it — '#[derive(PartialEq)]' would compile \
                     but compare structurally and never call your 'eq'"
                ));
            }
            return None;
        }
        let has_po = self.env.has_impl("PartialOrd", name, &[]);
        let has_o = self.env.has_impl("Ord", name, &[]);
        if !has_po && !has_o {
            return None;
        }
        // Reaching here with a dispatchable impl is impossible — the gate
        // short-circuits on `ordering_operator_dispatches` — so the only
        // ordering impl that gets this message is one without a `cmp` body.
        let written = if has_po && has_o {
            "'impl PartialOrd' and 'impl Ord'"
        } else if has_po {
            "'impl PartialOrd'"
        } else {
            "'impl Ord'"
        };
        Some(format!(
            "ordering operators dispatch through 'cmp', which your {written} for \
             '{disp}' does not define — add 'impl Ord for {disp} {{ fn cmp(ref \
             self, other: ref {disp}) -> Ordering {{ ... }} }}' to enable '<', \
             '<=', '>', '>='; 'partial_cmp' alone cannot be lowered yet, and \
             '#[derive(PartialOrd)]' would compile but compare by field \
             DECLARATION ORDER, never calling your impl"
        ))
    }

    pub(super) fn arithmetic_rejection_message(
        &self,
        op: &BinOp,
        ty: &Type,
        right_ty: &Type,
    ) -> String {
        let Some(trait_name) = Self::arithmetic_operator_trait(op) else {
            return format!(
                "arithmetic operator requires numeric type, found '{}'",
                type_display(ty)
            );
        };
        // A `String` LEFT operand under `+` reaches this arm only because the
        // RIGHT one is not a String — `String + String` is accepted a few
        // branches above. Saying "does not implement trait Add" here would be
        // FALSE, and falsely specific in a way the old vague wording never was:
        // String does implement Add, the other operand is the fault. Report the
        // operand instead. (`String - String` still lands on the missing-impl
        // message below, which is correct — there is no `Sub` for String.)
        if matches!(op, BinOp::Add) && is_string_concat_operand(ty) {
            return format!(
                "'+' on 'String' requires a 'String' right operand, found '{}'",
                type_display(right_ty)
            );
        }
        let base = format!(
            "type '{}' does not implement trait {}",
            type_display(ty),
            trait_name
        );
        match ty {
            Type::Named { name, .. } if name == "Vec" || name == "VecDeque" => format!(
                "{base}; there is deliberately no `impl {trait_name} for {name}[T]` — \
                 use `a.extend(b)` to append b's elements to a"
            ),
            Type::Named { name, .. } if self.env.distinct_types.contains_key(name) => format!(
                "{base}; add #[derive(Arithmetic)] to '{name}' to use arithmetic \
                 operators between two '{name}' values, or unwrap explicitly"
            ),
            _ => base,
        }
    }

    /// The binary operator a compound assignment implies — `x += y` means
    /// `x = x + y`, so `+=` checks under `Add`. B-2026-08-14-29: the
    /// `CompoundAssign` arm needs this to route through [`Self::infer_binary`];
    /// codegen (`src/codegen/stmts.rs`) and the interpreter
    /// (`src/interpreter/eval_stmt.rs`) each carry the same table for their own
    /// desugaring, and all three must agree on the mapping.
    pub(super) fn compound_op_binop(op: &crate::ast::CompoundOp) -> BinOp {
        use crate::ast::CompoundOp;
        match op {
            CompoundOp::Add => BinOp::Add,
            CompoundOp::Sub => BinOp::Sub,
            CompoundOp::Mul => BinOp::Mul,
            CompoundOp::Div => BinOp::Div,
            CompoundOp::Mod => BinOp::Mod,
            CompoundOp::BitAnd => BinOp::BitAnd,
            CompoundOp::BitOr => BinOp::BitOr,
            CompoundOp::BitXor => BinOp::BitXor,
            CompoundOp::Shl => BinOp::Shl,
            CompoundOp::Shr => BinOp::Shr,
        }
    }

    pub(super) fn infer_binary(
        &mut self,
        op: &BinOp,
        left: &Expr,
        right: &Expr,
        span: &Span,
    ) -> Type {
        // Arithmetic-returns-base (design.md § Refinement Types: "Arithmetic
        // on refined types returns the base type — no automatic constraint
        // propagation"). Strip any refinement off the operand types before
        // the result-type logic below, so `Positive + Positive -> i64` and
        // comparisons / bitwise ops on refined operands operate on the base.
        // The operands' *own* recorded types (in `expr_types`) are untouched
        // — only the local types driving this binop's result are normalized.
        let left_ty = strip_refinement(&self.infer_expr(left)).clone();
        let right_ty = strip_refinement(&self.infer_expr(right)).clone();

        if left_ty == Type::Error || right_ty == Type::Error {
            return Type::Error;
        }

        // `f16_software_emulated` (design.md:2347) — the last starter-set
        // lint that was target-dependent rather than mechanical, and so
        // outlived the other seven (B-2026-08-22-7). Emitted HERE, in the
        // typechecker, and not from codegen: `#[allow(...)]` cascade support
        // lives on `effective_lint_level` and has no counterpart under
        // `src/codegen/`, and a lint that only exists in `--features llvm`
        // builds would be silently absent from a default `karac check`.
        //
        // That is only possible because the capability question is answered
        // WITHOUT the backend — see `target::target_has_native_f16`, whose
        // CPU list is measured with `llc` rather than inferred, precisely
        // because no LLVM-C entry point expands a CPU name to its resolved
        // subtarget features.
        if Self::arithmetic_operator_trait(op).is_some() {
            self.warn_if_f16_is_software_emulated(&left_ty, &right_ty, span);
        }

        // Pull-side closure-param inference (B-2026-07-12-10). A let-bound
        // closure with an un-annotated numeric param (`let f = |x| x + 1`) types
        // `x` as a fresh inference var; nothing else solves it, so without this
        // the arithmetic below rejects `?T0` as non-numeric. When exactly one
        // arithmetic operand resolves to an unsolved inference var and the other
        // to a concrete numeric type (here `1: i64`), solve the var to that type
        // — the closure then types as `Fn(i64) -> i64` and the later call
        // `f(5)` checks cleanly. This is the pull-side complement of the working
        // push-side case (`xs.iter().map(|x| x + 1)`, where the element type is
        // already concrete). After solving, re-resolve both operands so the
        // checks below see the concrete types. `resolve_type_var_top` is the
        // identity on a non-var / unsolved-var type, so nothing else is
        // affected.
        let (left_ty, right_ty) = if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) {
            let lr = resolve_type_var_top(&left_ty, &self.env.substitutions);
            let rr = resolve_type_var_top(&right_ty, &self.env.substitutions);
            match (&lr, &rr) {
                (Type::TypeVar(id), other) if is_numeric(other) => {
                    self.env.substitutions.insert(*id, other.clone());
                }
                (other, Type::TypeVar(id)) if is_numeric(other) => {
                    self.env.substitutions.insert(*id, other.clone());
                }
                _ => {}
            }
            (
                resolve_type_var_top(&left_ty, &self.env.substitutions),
                resolve_type_var_top(&right_ty, &self.env.substitutions),
            )
        } else {
            (left_ty, right_ty)
        };

        // Element-wise tensor arithmetic on `Tensor[T, Shape]` (design.md
        // § Numerical Types — "Tensor-tensor requires exact shape match";
        // scalar broadcast via the operator trait). Handled before literal
        // promotion so the tensor path owns scalar-literal promotion to the
        // element type itself. `Add`/`Sub`/`Mul`/`Div` + `Neg` only; reduces
        // and broadcasting are separate slices (phase-11 line 47).
        if is_tensor_type(&left_ty) || is_tensor_type(&right_ty) {
            return self.infer_tensor_binary(op, &left_ty, &right_ty, left, right, span);
        }

        // Element-wise three-valued-logic arithmetic / comparison on
        // `Column[T]` (phase-11 Arrow, design.md "null + x = null", "null ==
        // null = null"). `+ - * /` yield `Column[T]`; `== != < <= > >=` yield
        // `Column[bool]`; either form null-propagates (a null slot on either
        // side → null in the result, never `false`). Handled before literal
        // promotion so the Column path owns scalar-literal promotion to the
        // element type. Col-col length agreement is a runtime check (lengths
        // aren't statically known).
        if is_column_type(&left_ty) || is_column_type(&right_ty) {
            return self.infer_column_binary(op, &left_ty, &right_ty, left, right, span);
        }

        // Element-wise SIMD arithmetic on `Vector[T, N]` (design.md § Portable
        // SIMD). Handled before literal promotion — a vector never pairs with a
        // bare scalar literal in v1 (splat-from-scalar is a separate method).
        // Slice 1 covers `+ - * / %`; bitwise ops and comparison-to-`Mask` are
        // later slices (phase-7 line 289).
        if matches!(left_ty, Type::Vector { .. }) || matches!(right_ty, Type::Vector { .. }) {
            return self.infer_vector_binary(op, &left_ty, &right_ty, left, right, span);
        }

        // Auto-deref a `ref` / `mut ref` wrapper around a numeric SCALAR
        // operand for arithmetic. design.md § "Compound assignment on `mut
        // ref` lvalues" (:5306) mandates read-through: `a = a + b` on a `mut
        // ref T` lvalue desugars to `*a = *a + b`, so the RHS reads through the
        // borrow and the binop operates on the bare scalar `T` — both operand
        // types and the result type. This mirrors the ref-stripping comparison
        // already does (`strip_refs_for_compare`), keeping arithmetic
        // consistent. Placed BEFORE Q4 literal promotion so an unsuffixed
        // literal operand (`x + 1`) still promotes to the pointee type and
        // records its span; a borrow left un-stripped makes `is_numeric` false,
        // which would skip promotion and risk a codegen literal-width mismatch.
        // The tensor / Column / Vector paths above have already returned, so
        // any borrow surviving here wraps a scalar; stripping only when the
        // pointee is numeric preserves the "requires numeric type" diagnostic
        // for a non-numeric borrow.
        let (left_ty, right_ty) = if matches!(
            op,
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
        ) {
            (
                deref_numeric_scalar(left_ty),
                deref_numeric_scalar(right_ty),
            )
        } else {
            (left_ty, right_ty)
        };

        // Q4 literal promotion: for arithmetic, comparison, and equality ops,
        // when one operand is a suffix-free numeric literal and the other is a
        // concrete numeric type T, re-record the literal's span with type T so
        // the lowering pass sees a homogeneous pair. `effective_ty` tracks the
        // canonical type for the whole expression after promotion.
        let is_promotable_op = matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq
                | BinOp::Eq
                | BinOp::NotEq
        );
        // After promotion these hold the effective operand types seen by the
        // match arms below. Initialised to the inferred types; overwritten when
        // promotion fires.
        let (eff_left_ty, eff_right_ty) = if is_promotable_op {
            let left_is_unsuffixed = matches!(
                &left.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            let right_is_unsuffixed = matches!(
                &right.kind,
                ExprKind::Integer(_, None) | ExprKind::Float(_, None)
            );
            if right_is_unsuffixed && !left_is_unsuffixed && is_numeric(&left_ty) {
                // Float literal cannot be promoted to an integer type.
                let can_promote = !(matches!(&right.kind, ExprKind::Float(_, None))
                    && matches!(left_ty, Type::Int(_) | Type::UInt(_)));
                if can_promote {
                    self.record_expr_type(&right.span, &left_ty);
                    (left_ty.clone(), left_ty.clone())
                } else {
                    (left_ty.clone(), right_ty.clone())
                }
            } else if left_is_unsuffixed && !right_is_unsuffixed && is_numeric(&right_ty) {
                let can_promote = !(matches!(&left.kind, ExprKind::Float(_, None))
                    && matches!(right_ty, Type::Int(_) | Type::UInt(_)));
                if can_promote {
                    self.record_expr_type(&left.span, &right_ty);
                    (right_ty.clone(), right_ty.clone())
                } else {
                    (left_ty.clone(), right_ty.clone())
                }
            } else {
                (left_ty.clone(), right_ty.clone())
            }
        } else {
            (left_ty.clone(), right_ty.clone())
        };
        let left_ty = eff_left_ty;
        let right_ty = eff_right_ty;

        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                // String concatenation: `String + String -> String`. Only
                // `+` is defined for strings; codegen (`compile_string_binop`)
                // and the interpreter (`eval_ops`) both allocate a fresh
                // String and copy both operands. `String + <non-String>`
                // (and `String - String` etc.) fall through to the
                // numeric/distinct paths below and are rejected there.
                if matches!(op, BinOp::Add)
                    && is_string_concat_operand(&left_ty)
                    && is_string_concat_operand(&right_ty)
                {
                    Type::Str
                } else if is_numeric(&left_ty) {
                    // Integer operands must match EXACTLY (same width and
                    // signedness) — design.md § Integer overflow: "mixed-
                    // signedness across lhs and rhs is a type error … cast
                    // explicitly with `as`", and mixed *width* likewise
                    // (`n + x` where `n: i64`, `x: i32` is a compile error).
                    // Narrow integer types are real fixed-width types, so
                    // `i64 + u8` does not silently widen — it is rejected here
                    // and the programmer writes `x as i64`. Q4 literal
                    // promotion above already unified any suffix-free literal
                    // operand, so a surviving mismatch is between two concrete
                    // types. Same-domain floats keep the looser
                    // `types_compatible` check, but a cross-DOMAIN mix (one
                    // integer operand, one float) is rejected: Kāra has no
                    // implicit int→float promotion in arithmetic (the
                    // interpreter errors on `Int * Float`, and codegen would
                    // SILENTLY MISCOMPILE it — `types_compatible(Int, Float)`
                    // is `true` for assignment/other contexts, so this arm
                    // must guard the domain split itself). Cast explicitly with
                    // `as` (B-2026-07-04-11).
                    let left_is_int = matches!(left_ty, Type::Int(_) | Type::UInt(_));
                    let right_is_int = matches!(right_ty, Type::Int(_) | Type::UInt(_));
                    let both_ints = left_is_int && right_is_int;
                    if both_ints {
                        if left_ty != right_ty {
                            // B-2026-08-17-11 — the repair this message names
                            // must be the WIDENING one. The old text's example
                            // always named the LEFT type ("the operand as
                            // 'u8'" for `u8 - i64`), i.e. the narrowing
                            // direction half the time — and both repairs
                            // compile, but only widening preserves every
                            // value; the narrowing cast converts this caught
                            // compile-time error into a runtime overflow trap
                            // on exactly the inputs the subtraction was
                            // written for. When one direction is value-
                            // preserving, name it — and when the narrow
                            // operand's source extent is exactly recoverable
                            // (see `appended_cast_end_offset`), emit it as a
                            // machine-applicable ` as <wide>` insertion so
                            // `karac fix` can apply it. A signed/unsigned
                            // pair of equal width has NO value-preserving
                            // direction, so it stays advisory: naming either
                            // side would be the same wrong steer this row
                            // fixed.
                            let l_disp = type_display(&left_ty);
                            let r_disp = type_display(&right_ty);
                            let narrow_side = if int_cast_preserves_all_values(&left_ty, &right_ty)
                            {
                                Some((left, &left_ty, &right_ty))
                            } else if int_cast_preserves_all_values(&right_ty, &left_ty) {
                                Some((right, &right_ty, &left_ty))
                            } else {
                                None
                            };
                            match narrow_side {
                                Some((narrow_expr, narrow_ty, wide_ty)) => {
                                    let narrow_disp = type_display(narrow_ty);
                                    let wide_disp = type_display(wide_ty);
                                    let message = format!(
                                        "cannot mix integer types '{l_disp}' and '{r_disp}' in \
                                         arithmetic — they must match; cast the '{narrow_disp}' \
                                         operand up to '{wide_disp}' with `as` — widening \
                                         preserves every value, while the cast down to \
                                         '{narrow_disp}' can overflow at runtime"
                                    );
                                    match appended_cast_end_offset(narrow_expr) {
                                        Some(end) => self.type_error_with_fix_it(
                                            message,
                                            right.span,
                                            TypeErrorKind::TypeMismatch,
                                            FixIt {
                                                span: Span {
                                                    line: narrow_expr.span.line,
                                                    column: narrow_expr.span.column,
                                                    offset: end,
                                                    length: 0,
                                                },
                                                replacement: format!(" as {wide_disp}"),
                                            },
                                        ),
                                        None => self.type_error(
                                            message,
                                            right.span,
                                            TypeErrorKind::TypeMismatch,
                                        ),
                                    }
                                }
                                None => self.type_error(
                                    format!(
                                        "cannot mix integer types '{l_disp}' and '{r_disp}' in \
                                         arithmetic — they must match; cast explicitly with \
                                         `as`, choosing the direction deliberately: neither \
                                         type can represent every value of the other"
                                    ),
                                    right.span,
                                    TypeErrorKind::TypeMismatch,
                                ),
                            }
                        }
                    } else if (left_is_int && matches!(right_ty, Type::Float(_)))
                        || (right_is_int && matches!(left_ty, Type::Float(_)))
                    {
                        // Exactly one operand is an integer, the other a float.
                        //
                        // B-2026-07-30-13 — this arm used to be keyed on
                        // `left_is_int != right_is_int`, which does NOT mean
                        // "one int, one float": the block is entered whenever
                        // the LEFT operand is numeric, so the other side can be
                        // ANY type. `s + q.pop_front()` (an `Option[i64]`, from
                        // the near-universal habit of forgetting `.unwrap()`)
                        // was therefore reported as "cannot mix integer and
                        // floating-point operands ('i64' and 'Option[i64]')" —
                        // naming a floating-point type that is not in the
                        // program and sending the reader to look for a cast that
                        // would not help. Require the other side to actually be
                        // a float; everything else falls to the mismatch arm
                        // below, which now carries an unwrap hint.
                        let int_side = if left_is_int { &left_ty } else { &right_ty };
                        let float_side = if left_is_int { &right_ty } else { &left_ty };
                        self.type_error(
                            format!(
                                "cannot mix integer and floating-point operands ('{}' and '{}') in \
                                 arithmetic — there is no implicit promotion; cast explicitly with \
                                 `as` (e.g. `{} as {}`)",
                                type_display(&left_ty),
                                type_display(&right_ty),
                                type_display(int_side),
                                type_display(float_side),
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    } else if matches!(left_ty, Type::Float(_))
                        && matches!(right_ty, Type::Float(_))
                        && left_ty != right_ty
                    {
                        // B-2026-08-14-13 — the FLOAT sibling of the
                        // mixed-integer rejection above. Two floats of
                        // different widths used to fall through to the
                        // `types_compatible` arm below, which answers `true`
                        // for any float pair, and the result took `left_ty` —
                        // so `a * b` was `f32` and `b * a` was `f64` for the
                        // same two bindings, and on the `f64` spelling the
                        // interpreter kept the double while the binary rounded
                        // to f32. An operand's POSITION decided the precision,
                        // and then the two backends disagreed about what that
                        // precision was.
                        //
                        // design.md settles the direction in two places: the
                        // literal-promotion rule exists to buy `arr + 1`
                        // "without opening the door to implicit widening
                        // between typed variables … There is no 'numerics
                        // dialect' where typed variables silently widen", and
                        // the mixed-precision section spells the ML pattern
                        // (store `bf16`, compute `f32`, store back) with an
                        // explicit `as` in BOTH directions. Literal promotion
                        // ran above, so a surviving mismatch is between two
                        // concrete typed expressions — which is exactly the
                        // case both passages name.
                        //
                        // Arithmetic only, matching the integer rule. A
                        // COMPARISON has no result width for operand order to
                        // decide, so mixing widths there is well-defined
                        // (the narrower widens losslessly) and stays legal —
                        // as it does for integers.
                        let (wide, narrow) =
                            if float_width_rank(&left_ty) > float_width_rank(&right_ty) {
                                (&left_ty, &right_ty)
                            } else {
                                (&right_ty, &left_ty)
                            };
                        self.type_error(
                            format!(
                                "cannot mix float types '{}' and '{}' in arithmetic — they must \
                                 match; cast explicitly with `as` (e.g. the '{}' operand as '{}' \
                                 to compute at the wider width, or the '{}' operand as '{}' to \
                                 compute at the narrower)",
                                type_display(&left_ty),
                                type_display(&right_ty),
                                type_display(narrow),
                                type_display(wide),
                                type_display(wide),
                                type_display(narrow),
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    } else if !types_compatible(&left_ty, &right_ty) {
                        self.type_error(
                            format!(
                                "expected '{}', found '{}'{}",
                                type_display(&left_ty),
                                type_display(&right_ty),
                                Self::arith_wrapper_unwrap_hint(&left_ty, &right_ty)
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else if self.distinct_type_has_arithmetic(&left_ty) {
                    // Arithmetic on a distinct type: both operands must be the same type.
                    if left_ty != right_ty {
                        self.type_error(
                            format!(
                                "arithmetic on distinct type '{}' requires both operands to have \
                                 the same type, found '{}'",
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else if self.type_param_has_numeric_bound(&left_ty) {
                    // Arithmetic on a `T: Numeric` generic parameter — the bound
                    // guarantees `T` is a primitive numeric type. Both operands
                    // must be the same parameter (no mixed-`T` arithmetic).
                    if left_ty != right_ty {
                        self.type_error(
                            format!(
                                "arithmetic on a 'Numeric' type parameter requires both operands \
                                 to have the same type, found '{}' and '{}'",
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else if Self::arithmetic_operator_trait(op)
                    .is_some_and(|tr| self.type_param_has_trait_bound(&left_ty, tr))
                {
                    // Arithmetic on a `T: Add`/`Sub`/`Mul`/`Div`/`Rem` generic
                    // parameter — the bound names the stdlib operator trait for
                    // THIS operator. User operator-trait impls are forbidden, so
                    // every instantiation is a primitive numeric / `String` /
                    // distinct-numeric that codegen lowers post-monomorphization
                    // (verified: `T: Numeric` arithmetic already builds+runs).
                    // Result is `T`; both operands must be the same parameter.
                    if left_ty != right_ty {
                        self.type_error(
                            format!(
                                "arithmetic on a '{}'-bounded type parameter requires both \
                                 operands to have the same type, found '{}' and '{}'",
                                Self::arithmetic_operator_trait(op).unwrap_or(""),
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span,
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else {
                    self.type_error(
                        self.arithmetic_rejection_message(op, &left_ty, &right_ty),
                        left.span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                    Type::Error
                }
            }
            BinOp::Eq | BinOp::NotEq => {
                // Comparison auto-derefs reference operands so a value can be
                // compared against a borrow of the same type (`String ==
                // ref String`); the comparison only reads through the borrow.
                let cmp_left = strip_refs_for_compare(&left_ty);
                let cmp_right = strip_refs_for_compare(&right_ty);
                if !types_compatible(cmp_left, cmp_right) {
                    self.type_error(
                        format!(
                            "cannot compare '{}' and '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        *span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if !self.type_supports_partial_eq(cmp_left) {
                    self.type_error(
                        // B-2026-08-25-30 — name the PARTIAL trait: the
                        // desugaring runs through `PartialEq` (the guard
                        // right above is `type_supports_partial_eq`), and
                        // `#[derive(PartialEq)]` ALONE compiles. Prescribing
                        // `Eq` was not merely imprecise — for a type with an
                        // `f32`/`f64` field it named a derive the language
                        // deliberately does not offer on floats (`NaN != NaN`
                        // breaks reflexivity), sending that reader to a dead
                        // end. design.md: the `Eq` marker "is never named by
                        // the desugaring".
                        self.comparison_impl_written_but_undispatched(cmp_left, true)
                            .unwrap_or_else(|| {
                                format!(
                                    "type '{}' does not implement PartialEq; add \
                                 #[derive(PartialEq)] to use == or !=",
                                    type_display(cmp_left)
                                )
                            }),
                        *span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                // See `Eq` arm: reference operands auto-deref for comparison.
                let cmp_left = strip_refs_for_compare(&left_ty);
                let cmp_right = strip_refs_for_compare(&right_ty);
                if !types_compatible(cmp_left, cmp_right) {
                    self.type_error(
                        format!(
                            "cannot compare '{}' and '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        *span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if matches!(cmp_left, Type::Named { name, .. } if self.env.distinct_types.contains_key(name))
                    && !self.type_supports_partial_ord(cmp_left)
                    && !self.ordering_operator_dispatches(cmp_left)
                {
                    // Distinct types are opaque — ordering comparisons require
                    // an explicit `#[derive(Ord)]` (design.md § Distinct Types:
                    // "no comparison unless opted in"). Other named types keep
                    // their pre-existing comparison behavior.
                    self.type_error(
                        // B-2026-08-25-30 — the `PartialOrd` sibling of
                        // the `PartialEq` correction above: the guard is
                        // `type_supports_partial_ord` and
                        // `#[derive(PartialOrd)]` alone compiles, on a
                        // struct/enum and on a distinct type alike.
                        self.comparison_impl_written_but_undispatched(cmp_left, false)
                            .unwrap_or_else(|| {
                                format!(
                                    "type '{}' does not implement PartialOrd; add \
                                 #[derive(PartialOrd)] to use <, <=, >, or >=",
                                    type_display(cmp_left)
                                )
                            }),
                        *span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if matches!(cmp_left, Type::Named { name, .. }
                        if self.env.structs.contains_key(name) || self.env.enums.contains_key(name))
                    && !self.type_supports_partial_ord(cmp_left)
                    && !self.ordering_operator_dispatches(cmp_left)
                {
                    // A user struct / enum orders with `<`, `<=`, `>`, `>=` only
                    // when it opts in via `#[derive(Ord)]` / `#[derive(PartialOrd)]`
                    // (or a user `impl`). Pre-fix these operators were silently
                    // admitted on ANY struct/enum (returning `Type::Bool`), then
                    // the interpreter / codegen had no lowering, so the program
                    // errored at run/build with a misleading "not defined"
                    // message. Now the derive requirement is a clean type error
                    // and derived types lower through the `karac_cmp`/`value_compare`
                    // declaration-order comparator (B-2026-07-03-7).
                    self.type_error(
                        // B-2026-08-25-30 — the `PartialOrd` sibling of
                        // the `PartialEq` correction above: the guard is
                        // `type_supports_partial_ord` and
                        // `#[derive(PartialOrd)]` alone compiles, on a
                        // struct/enum and on a distinct type alike.
                        self.comparison_impl_written_but_undispatched(cmp_left, false)
                            .unwrap_or_else(|| {
                                format!(
                                    "type '{}' does not implement PartialOrd; add \
                                 #[derive(PartialOrd)] to use <, <=, >, or >=",
                                    type_display(cmp_left)
                                )
                            }),
                        *span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::And | BinOp::Or => {
                if left_ty != Type::Bool {
                    self.type_error(
                        format!(
                            "logical operator requires 'bool', found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                if right_ty != Type::Bool {
                    self.type_error(
                        format!(
                            "logical operator requires 'bool', found '{}'",
                            type_display(&right_ty)
                        ),
                        right.span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                Type::Bool
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                if !is_integer(&left_ty) {
                    self.type_error(
                        format!(
                            "bitwise operator requires integer type, found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span,
                        TypeErrorKind::InvalidBinaryOp,
                    );
                    return Type::Error;
                }
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        format!(
                            "expected '{}', found '{}'",
                            type_display(&left_ty),
                            type_display(&right_ty)
                        ),
                        right.span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                left_ty
            }
            BinOp::Range | BinOp::RangeInclusive => {
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        "range bounds must have same type".to_string(),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
                Type::Named {
                    name: "Range".to_string(),
                    args: vec![left_ty],
                }
            }
        }
    }

    /// B-2026-07-30-13 — the trailing hint on an arithmetic type mismatch whose
    /// wrong operand is an `Option`/`Result` WRAPPING a compatible payload, i.e.
    /// a forgotten `.unwrap()`. Empty for every other mismatch.
    ///
    /// `q.pop_front()` / `v.pop()` / `m.get(k)` all yield `Option[T]`, so
    /// `s = s + q.pop_front()` is the first thing most people write and the
    /// error lands on the arithmetic, several steps from the cause. Naming the
    /// wrapper and the payload turns "expected 'i64', found 'Option[i64]'" into
    /// something that says what to do about it.
    fn arith_wrapper_unwrap_hint(expected: &Type, found: &Type) -> String {
        let Type::Named { name, args } = found else {
            return String::new();
        };
        if !matches!(name.as_str(), "Option" | "Result") || args.is_empty() {
            return String::new();
        }
        if !types_compatible(expected, &args[0]) {
            return String::new();
        }
        format!(
            " — `{}` wraps the '{}'; unwrap it first \
             (`.unwrap()`, `.unwrap_or(<default>)`, or a `match`)",
            name,
            type_display(&args[0])
        )
    }

    pub(super) fn infer_unary(&mut self, op: &UnaryOp, operand: &Expr, span: &Span) -> Type {
        let ty = self.infer_expr(operand);
        if ty == Type::Error {
            return Type::Error;
        }

        match op {
            UnaryOp::Neg => {
                // Element-wise negation of a `Tensor[T, Shape]` — result is a
                // fresh tensor of the same shape (rank must be statically known,
                // like the binary path). The element type must be numeric.
                if let Some(args) = tensor_named_args(&ty) {
                    let args = args.to_vec();
                    let Some((elem, dims)) =
                        self.tensor_static_shape(&args, "unary '-' on a tensor", span)
                    else {
                        return Type::Error;
                    };
                    if !is_numeric(&elem) {
                        self.type_error(
                            format!(
                                "unary '-' on a tensor requires a numeric element type, \
                                 found '{}'",
                                type_display(&elem)
                            ),
                            *span,
                            TypeErrorKind::InvalidUnaryOp,
                        );
                        return Type::Error;
                    }
                    return Type::Named {
                        name: "Tensor".to_string(),
                        args: vec![elem, Type::Shape(dims)],
                    };
                }
                // Element-wise negation of a `Column[T]` — fresh Column[T],
                // nulls preserved. Numeric element required.
                if let Some(elem) = column_elem(&ty) {
                    let elem = elem.clone();
                    if !is_numeric(&elem) {
                        self.type_error(
                            format!(
                                "unary '-' on a column requires a numeric element type, \
                                 found '{}'",
                                type_display(&elem)
                            ),
                            *span,
                            TypeErrorKind::InvalidUnaryOp,
                        );
                        return Type::Error;
                    }
                    return Type::Named {
                        name: "Column".to_string(),
                        args: vec![elem],
                    };
                }
                if !is_numeric(&ty)
                    && !self.distinct_type_has_arithmetic(&ty)
                    && !self.type_param_has_numeric_bound(&ty)
                    && !self.type_param_has_trait_bound(&ty, "Neg")
                {
                    self.type_error(
                        format!(
                            "unary '-' requires numeric type, found '{}'",
                            type_display(&ty)
                        ),
                        *span,
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    ty
                }
            }
            UnaryOp::Not => {
                if ty != Type::Bool {
                    self.type_error(
                        // B-2026-08-25-30 — spell the operator the way the
                        // user must write it. The PARSER rejects `!` with "the
                        // `!` operator is not used in Kara; use `not` instead",
                        // and the typechecker then described the operator they
                        // DID write correctly using the spelling the parser had
                        // just refused.
                        format!("unary 'not' requires 'bool', found '{}'", type_display(&ty)),
                        *span,
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    Type::Bool
                }
            }
            UnaryOp::BitNot => {
                // Also accept an integer-lane `Vector[T, N]` — `~v` complements
                // every lane (design.md § Portable SIMD). Float lanes have no
                // bitwise complement, so they stay rejected.
                let vec_int = matches!(
                    &ty,
                    Type::Vector { element, .. }
                        if matches!(**element, Type::Int(_) | Type::UInt(_))
                );
                if !is_integer(&ty) && !vec_int {
                    self.type_error(
                        format!(
                            "unary '~' requires an integer or integer-lane Vector type, \
                             found '{}'",
                            type_display(&ty)
                        ),
                        *span,
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                } else {
                    ty
                }
            }
            UnaryOp::Deref => match ty {
                Type::Ref(inner) | Type::MutRef(inner) => *inner,
                // Raw-pointer dereference (`*const T` / `*mut T`) typechecks
                // to the pointee type. The operation itself is *unsafe* — the
                // `unsafe_op_in_unsafe_fn` lint (`src/unsafe_lint.rs`) rejects
                // it outside an `unsafe { }` block. Soundness lives at the
                // lint layer, not the type layer, so callers can still reason
                // about the deref's result type.
                Type::Pointer { inner, .. } => *inner,
                _ => {
                    self.type_error(
                        format!(
                            "unary '*' requires 'ref T', 'mut ref T', or a raw pointer \
                             ('*const T' / '*mut T'), found '{}'",
                            type_display(&ty)
                        ),
                        *span,
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                }
            },
        }
    }

    // ── Pipe Desugaring ──────────────────────────────────────────

    pub(super) fn infer_pipe(&mut self, left: &Expr, right: &Expr, span: &Span) -> Type {
        // More than one `_` per stage has no defined meaning — the piped value
        // can only land in one place. Diagnosed before desugaring, because the
        // shared rewrite would substitute into every placeholder and silently
        // duplicate the left-hand expression.
        if let ExprKind::Call { callee, args } = &right.kind {
            let placeholder_count = args
                .iter()
                .filter(|arg| matches!(arg.value.kind, ExprKind::PipePlaceholder))
                .count();
            if placeholder_count > 1 {
                self.type_error(
                    "at most one '_' placeholder allowed per pipe stage".to_string(),
                    right.span,
                    TypeErrorKind::InvalidPipePlaceholder,
                );
                self.infer_expr(callee);
                for arg in args {
                    if !matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                        self.infer_expr(&arg.value);
                    }
                }
                return Type::Error;
            }
        }

        // The rewrite itself is shared with the interpreter and codegen so the
        // three phases cannot disagree about what a pipe means (B-2026-08-17-25).
        let Some(desugared) = desugar_pipe(left, right, *span) else {
            self.type_error(
                "right-hand side of pipe must be a function name or function call".to_string(),
                right.span,
                TypeErrorKind::NotCallable,
            );
            self.infer_expr(right);
            return Type::Error;
        };

        let ExprKind::Call { callee, args } = &desugared.kind else {
            unreachable!("desugar_pipe always yields a Call")
        };
        self.infer_call(callee, args, span)
    }

    // ── ?? operator ─────────────────────────────────────────────

    /// Typecheck `left ?? right` (design.md line 782): the operand must be an
    /// `Option[T]` or a `Result[T, E]`, the fallback must be assignable to
    /// `T`, and the expression's type is `T` — the wrapper is stripped.
    ///
    /// B-2026-08-17-27 — two defects lived here. `Result` was never handled,
    /// so `parse(s) ?? -1` fell to a bare `Type::Error`; and because a
    /// *reported* error returns `Type::Error` too, that fallthrough was
    /// SILENT — `karac check` said "All checks passed" and left the two
    /// evaluators to improvise. Same shape as the index rule's silent tail
    /// (B-2026-08-17-10): the fix is to make every rejected operand say so.
    ///
    /// The evaluators lower `??` to `left.unwrap_or(right)`, which is the same
    /// operation and is already hardened. That lowering reads the payload type
    /// from `method_unwrap_inner_types`, so this records the entry under the
    /// key the desugared call will present.
    pub(super) fn infer_nil_coalesce(&mut self, left: &Expr, right: &Expr, span: &Span) -> Type {
        let l_ty = resolve_type_var_top(&self.infer_expr(left), &self.env.substitutions);

        // `Option[T]` -> (T, no error payload); `Result[T, E]` -> (T, E).
        let wrapped = match &l_ty {
            Type::Named { name, args } if name == "Option" && args.len() == 1 => {
                Some((args[0].clone(), None))
            }
            Type::Named { name, args } if name == "Result" && args.len() == 2 => {
                Some((args[0].clone(), Some(args[1].clone())))
            }
            _ => None,
        };

        let Some((payload, err_ty)) = wrapped else {
            // Still type the fallback: it may hold errors of its own, and
            // reporting only the operand would send the author back for a
            // second round on the same line.
            self.infer_expr(right);
            // An operand that is already `Error`, or still an unsolved
            // metavar, has nothing to say here — the first would double-report
            // and the second is not yet knowable.
            if l_ty != Type::Error && !matches!(l_ty, Type::TypeVar(_)) {
                self.type_error(
                    format!(
                        "'??' requires an `Option` or a `Result` on the left, found '{}'\n  \
                         '??' supplies the value to use when a wrapped value is absent, so it\n  \
                         needs a wrapper to unwrap: it yields the payload of a `Some`/`Ok` and\n  \
                         the right-hand fallback for a `None`/`Err`.\n  \
                         help: an unwrapped value needs no fallback — drop the '?? ...'",
                        type_display(&l_ty)
                    ),
                    *span,
                    TypeErrorKind::NilCoalesceNotWrapped,
                );
            }
            return Type::Error;
        };

        let r_ty = self.infer_expr(right);
        if r_ty != Type::Error {
            self.check_assignable(&payload, &r_ty, right.span);
        }

        // Feed the evaluators' `unwrap_or` lowering. The key must match what
        // `ast::desugar_nil_coalesce` builds: the `??` span as the receiver
        // span, the fallback's span standing in for the args-close span.
        let key = SpanKey::from_span(span);
        let payload_resolved = resolve_type_var_top(&payload, &self.env.substitutions);
        self.method_unwrap_inner_types
            .insert(key, Self::type_to_type_expr(&payload_resolved));
        if let Some(e) = err_ty {
            let e_resolved = resolve_type_var_top(&e, &self.env.substitutions);
            self.method_unwrap_err_types
                .insert(key, Self::type_to_type_expr(&e_resolved));
        }

        payload
    }

    // ── ?. optional chaining ────────────────────────────────────

    /// Typecheck `a?.f` / `a?.m(args)` (design.md line 782): the operand must
    /// be an `Option[T]`; the member is resolved against `T`; and the result
    /// is `Option[U]` where `U` is the member's type with any outer `Option`
    /// STRIPPED.
    ///
    /// B-2026-08-17-28 — this rule did not exist. The arm read
    /// `Type::Error // Needs advanced option handling, stubbed for now`, and
    /// because a *reported* error returns `Type::Error` too, the stub was
    /// SILENT: `karac check` passed on every `?.` program and handed an
    /// untyped expression to the evaluators, which then disagreed three
    /// different ways. Same shape as the index rule's silent tail
    /// (B-2026-08-17-10) and `??`'s missing `Result` arm (B-2026-08-17-27) —
    /// the third rule found returning `Type::Error` without saying anything.
    ///
    /// FLATTENING IS THE WHOLE POINT, and design.md's own example is what
    /// forces it: `user.address?.city?.name` can only chain if `address?.city`
    /// is `Option[City]` rather than `Option[Option[City]]`, since the next
    /// `?.` has to project from a `City`. Every language with `?.` flattens
    /// for this reason.
    ///
    /// The member is resolved by binding the payload to a scope-local
    /// synthetic name and typing the ordinary field access / method call
    /// against it. That reuses field resolution, method dispatch, generic
    /// instantiation and their diagnostics wholesale, rather than
    /// reimplementing member lookup here — and it is what makes `c?.label()`
    /// resolve exactly as `c.label()` does.
    pub(super) fn infer_optional_chain(
        &mut self,
        object: &Expr,
        member: &str,
        args: &Option<Vec<CallArg>>,
        span: &Span,
    ) -> Type {
        let obj_ty = resolve_type_var_top(&self.infer_expr(object), &self.env.substitutions);
        // A borrow projects through, exactly as field access does.
        let peeled = match &obj_ty {
            Type::Ref(inner) | Type::MutRef(inner) => (**inner).clone(),
            _ => obj_ty.clone(),
        };

        let payload = match &peeled {
            Type::Named { name, args: targs } if name == "Option" && targs.len() == 1 => {
                targs[0].clone()
            }
            _ => {
                // Still type any arguments, so their own errors are reported
                // in the same pass rather than on a second round-trip.
                if let Some(call_args) = args {
                    for a in call_args {
                        self.infer_expr(&a.value);
                    }
                }
                if peeled != Type::Error && !matches!(peeled, Type::TypeVar(_)) {
                    self.type_error(
                        format!(
                            "'?.' requires an `Option` on the left, found '{}'\n  \
                             '?.' is the short-circuiting form of member access: it yields `None`\n  \
                             when the receiver is absent, so it needs a receiver that can BE absent.\n  \
                             help: '{}' is always present — use '.' instead of '?.'",
                            type_display(&peeled),
                            type_display(&peeled)
                        ),
                        *span,
                        TypeErrorKind::OptionalChainNotOption,
                    );
                }
                return Type::Error;
            }
        };

        // Resolve the member against the PAYLOAD, through a synthetic binding
        // so the ordinary rules apply. The name cannot collide with anything
        // the author wrote — it is not lexable as a Kara identifier.
        const RECV: &str = "__optional_chain_recv";
        self.local_scope.push();
        self.local_scope.insert(RECV.to_string(), payload.clone());
        let recv_expr = Expr {
            span: object.span,
            kind: ExprKind::Identifier(RECV.to_string()),
        };
        let projected = match args {
            None => self.infer_field_access(&recv_expr, member, span),
            Some(call_args) => self.infer_method_call(&recv_expr, member, call_args, span, span),
        };
        self.local_scope.pop();

        if projected == Type::Error {
            return Type::Error;
        }

        // Flatten: an already-`Option` member IS the chain's result.
        let projected = resolve_type_var_top(&projected, &self.env.substitutions);
        let inner = match &projected {
            Type::Named { name, args: targs } if name == "Option" && targs.len() == 1 => {
                targs[0].clone()
            }
            other => other.clone(),
        };

        // Hand codegen the two facts it cannot recover from LLVM types: the
        // payload, which types the synthesized `Some(<binding>)` pattern, and
        // the member's type BEFORE flattening, which decides whether the arm
        // body wraps (`Some(x.f)`) or passes through (`x.f`). See
        // `compile_optional_chain`.
        let payload_resolved = resolve_type_var_top(&payload, &self.env.substitutions);
        // KEYED BY (span, member), not by span alone. The parser gives every
        // postfix node in a chain the RECEIVER's span, so both `?.` nodes of
        // `u.address?.city?.name` carry the same one — keying on it alone made
        // the outer record overwrite the inner, and codegen then typed the
        // inner arm's binding as `City` and refused to resolve `.city` on it.
        //
        // The member name separates them for every chain whose consecutive
        // members differ, which is all of them in practice. A repeat
        // (`x?.next?.next`) would still collide, so a conflicting re-insert
        // REMOVES the entry rather than overwriting it: codegen then declines
        // loudly ("no recorded lowering") instead of silently compiling one
        // level against the other's types.
        let key = (SpanKey::from_span(span), member.to_string());
        let facts = (
            Self::type_to_type_expr(&payload_resolved),
            Self::type_to_type_expr(&projected),
        );
        // `TypeExpr` has no `PartialEq`; compare the rendered forms, which is
        // what the ambiguity test actually needs (two chain levels agreeing on
        // both types are interchangeable for the lowering).
        let shape = |f: &(TypeExpr, TypeExpr)| format!("{:?}|{:?}", f.0.kind, f.1.kind);
        match self.optional_chain_lowering.get(&key) {
            Some(existing) if shape(existing) != shape(&facts) => {
                self.optional_chain_lowering.remove(&key);
            }
            _ => {
                self.optional_chain_lowering.insert(key, facts);
            }
        }

        Type::Named {
            name: "Option".to_string(),
            args: vec![inner],
        }
    }

    // ── ? operator ──────────────────────────────────────────────

    /// Type-check `inner?`: validate that the operand is `Result[T, E1]` or
    /// `Option[T]`, that the enclosing function returns a compatible variant,
    /// and (for Result) that error types match exactly or convert via `From`.
    /// Returns the unwrapped success type (`T`).
    pub(super) fn infer_question(&mut self, inner: &Expr, span: &Span) -> Type {
        let inner_ty = self.infer_expr(inner);
        if inner_ty == Type::Error {
            return Type::Error;
        }
        self.resolve_question(inner_ty, span)
    }

    /// The error-propagation half of the `?` operator, factored out of
    /// [`infer_question`] so a check-mode caller can feed an already-pinned
    /// operand type. Given the `?` operand's `Result`/`Option` type, validates
    /// it against the enclosing function's return type (recording any
    /// cross-error `impl From` conversion in `question_conversions`) and
    /// returns the unwrapped `Ok`/`Some` payload type. Used by `check_expr`'s
    /// fallible-constructor `?`-form arm (`let v: Vec[T] =
    /// Vec.try_with_capacity(n)?`), where inferring the operand first would
    /// mint an unpinnable fresh element typevar (phase-8-stdlib-floor item 8).
    pub(super) fn resolve_question(&mut self, inner_ty: Type, span: &Span) -> Type {
        let (inner_name, inner_args) = match &inner_ty {
            Type::Named { name, args } => (name.clone(), args.clone()),
            _ => {
                self.type_error(
                    format!(
                        "'?' operator requires `Result` or `Option`, found '{}'",
                        type_display(&inner_ty)
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };

        // Record the unwrapped Ok/Some payload type for codegen (B-2026-07-13-19).
        // Every success arm below returns `inner_args[0]` — the payload — for both
        // `Result[T, E]` and `Option[T]`, so a single recording here is the `?`
        // result type. Written under a DEDICATED key so codegen never confuses a
        // genuine nested `Option[T]`/`Result[T,E]` payload with a mistakenly
        // recorded wrapper (the span-collision `enum_inst_type_exprs` hazard).
        if !inner_args.is_empty() {
            self.question_ok_payload_types.insert(
                SpanKey::from_span(span),
                Self::type_to_type_expr(&inner_args[0]),
            );
        }

        // Closure-scoped `?` (B-2026-07-31-19): inside a closure literal
        // body, `?` on Err returns `Err(e)` FROM THE CLOSURE (the body is
        // `Fn() -> T`; the interpreter's closure boundary catches the
        // propagation), so the demand is recorded on the innermost closure
        // frame for post-body solving instead of being checked against the
        // enclosing FN's return type. Cross-error `From` conversions are
        // not applied at closure `?` sites — the solver requires exact
        // Err-type agreement.
        if let Some(frame) = self.closure_return_types.last_mut() {
            match inner_name.as_str() {
                "Result" if inner_args.len() == 2 => {
                    let e = inner_args[1].clone();
                    frame.question_errs.push(e);
                    return inner_args[0].clone();
                }
                "Option" if !inner_args.is_empty() => {
                    frame.question_option = true;
                    return inner_args[0].clone();
                }
                _ => {
                    self.type_error(
                        format!("'?' operator requires `Result` or `Option`, found '{inner_name}'"),
                        *span,
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
            }
        }

        let return_ty = match self.current_return_type.clone() {
            Some(t) => t,
            None => {
                self.type_error(
                    "'?' operator used outside a function body".to_string(),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };
        let (ret_name, ret_args) = match &return_ty {
            Type::Named { name, args } => (name.clone(), args.clone()),
            _ => {
                self.type_error(
                    format!(
                        "'?' requires the enclosing function to return `Result` or `Option`, found '{}'",
                        type_display(&return_ty)
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };

        match (inner_name.as_str(), ret_name.as_str()) {
            ("Option", "Option") if inner_args.len() == 1 && ret_args.len() == 1 => {
                inner_args[0].clone()
            }
            ("Result", "Result") if inner_args.len() == 2 && ret_args.len() == 2 => {
                let inner_err = &inner_args[1];
                let ret_err = &ret_args[1];
                if inner_err == ret_err {
                    return inner_args[0].clone();
                }
                // Cross-error type: require `impl From[InnerErr] for RetErr`.
                let target_name = match ret_err {
                    Type::Named { name, .. } => name.clone(),
                    _ => {
                        self.type_error(
                            format!(
                                "'?' cannot propagate error '{}' as '{}': target is not a named type",
                                type_display(inner_err),
                                type_display(ret_err)
                            ),
                            *span,
                            TypeErrorKind::TypeMismatch,
                        );
                        return Type::Error;
                    }
                };
                if self
                    .env
                    .find_from_impl(inner_err, &target_name, &[])
                    .is_some()
                {
                    self.question_conversions
                        .insert(SpanKey::from_span(span), target_name.clone());
                    return inner_args[0].clone();
                }
                self.type_error(
                    format!(
                        "'?' cannot convert error '{}' to '{}': no `impl From[{}] for {}` in scope",
                        type_display(inner_err),
                        type_display(ret_err),
                        type_display(inner_err),
                        target_name
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
            ("Result", "Option") | ("Option", "Result") => {
                self.type_error(
                    format!(
                        "'?' cannot mix `Result` and `Option`: operand is '{}', function returns '{}'",
                        type_display(&inner_ty),
                        type_display(&return_ty)
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
            _ => {
                self.type_error(
                    format!(
                        "'?' requires operand and return type to be `Result` or `Option`, found '{}' and '{}'",
                        type_display(&inner_ty),
                        type_display(&return_ty)
                    ),
                    *span,
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
        }
    }
}
