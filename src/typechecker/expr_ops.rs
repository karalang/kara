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

use super::inference::resolve_type_vars;
use super::types::{
    is_integer, is_numeric, is_prelude_type_or_module_name, is_string_concat_operand,
    strip_refinement, type_display, types_compatible, ConstArg, DimArg, Type, UIntSize,
    VariantTypeInfo,
};
use super::TypeErrorKind;

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
                    ty.span.clone(),
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
                    ty.span.clone(),
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
                ty.span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }
        if field_path.is_empty() {
            self.type_error(
                "error[E_OFFSET_OF_INVALID_PATH]: offset_of requires at least \
                 one field-name segment"
                    .to_string(),
                span.clone(),
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
                    ty.span.clone(),
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
                    span.clone(),
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
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
            };
        }
        usize_ty
    }

    // ── Identifier Resolution ───────────────────────────────────

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
        for (enum_name, enum_info) in &self.env.enums {
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
        // Fallback — likely a name the resolver already handled
        // Return Error silently (resolver already reported it)
        let _ = span;
        Type::Error
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
                            span.clone(),
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
            if let Some(provider_trait) = self.user_effect_resources.get(type_name).cloned() {
                if let Some((params, return_type)) =
                    self.resource_dispatch_signature(type_name, provider_trait.as_deref(), member)
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
                    span.clone(),
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
        provider_trait: Option<&str>,
        member: &str,
    ) -> Option<(Vec<Type>, Type)> {
        match provider_trait {
            Some(trait_name) => {
                // Clone the TypeExprs out first: `find_trait_method`
                // borrows `self` and `lower_type_expr` needs `&mut self`.
                let (param_tes, ret_te, method_gp) = {
                    let m = self.find_trait_method(trait_name, member)?;
                    (
                        m.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                        m.return_type.clone(),
                        Self::generic_param_names(&m.generic_params),
                    )
                };
                let params: Vec<Type> = param_tes
                    .iter()
                    .map(|te| self.lower_type_expr(te, &method_gp))
                    .collect();
                let return_type = ret_te
                    .as_ref()
                    .map(|te| self.lower_type_expr(te, &method_gp))
                    .unwrap_or(Type::Unit);
                Some((params, return_type))
            }
            None => {
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
        let is_compare = matches!(
            op,
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq
        );
        if !is_arith && !is_bitwise && !is_compare {
            self.type_error(
                format!(
                    "this operator is not yet supported on Vector[T, N] \
                     (element-wise + - * / % and & | ^ on lanes, comparisons \
                     < <= > >= == != yielding a mask); found operands '{}' and '{}'",
                    type_display(left_ty),
                    type_display(right_ty)
                ),
                left.span.clone(),
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
                        right.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                    return Type::Error;
                }
                // Bitwise `& | ^` are integer-lane only — float vectors have no
                // meaningful bit-and/or/xor. Arithmetic / comparisons stay open
                // to all numeric lanes.
                if is_bitwise && !matches!(**le, Type::Int(_) | Type::UInt(_)) {
                    self.type_error(
                        format!(
                            "bitwise vector operators (& | ^) require integer lanes; \
                             Vector element is '{}'",
                            type_display(le)
                        ),
                        left.span.clone(),
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
                    right.span.clone(),
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
                span.clone(),
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
                        span.clone(),
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
                        right.span.clone(),
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
                        span.clone(),
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
                                span.clone(),
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
                    span.clone(),
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
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return false;
        }
        let is_unsuffixed = matches!(
            &scalar.kind,
            ExprKind::Integer(_, None) | ExprKind::Float(_, None)
        );
        if is_unsuffixed {
            // A float literal cannot promote to an integer element type.
            let can_promote = !(matches!(&scalar.kind, ExprKind::Float(_, None))
                && matches!(elem, Type::Int(_) | Type::UInt(_)));
            if can_promote {
                self.record_expr_type(&scalar.span, elem);
                return true;
            }
        }
        if scalar_ty == elem || types_compatible(scalar_ty, elem) {
            return true;
        }
        self.type_error(
            format!(
                "scalar operand of element-wise tensor arithmetic must match the element \
                 type '{}', found '{}'",
                type_display(elem),
                type_display(scalar_ty)
            ),
            scalar.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
        false
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
                span.clone(),
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
                        right.span.clone(),
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
                        span.clone(),
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
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return false;
        }
        let is_unsuffixed = matches!(
            &scalar.kind,
            ExprKind::Integer(_, None) | ExprKind::Float(_, None)
        );
        if is_unsuffixed {
            // A float literal cannot promote to an integer element type.
            let can_promote = !(matches!(&scalar.kind, ExprKind::Float(_, None))
                && matches!(elem, Type::Int(_) | Type::UInt(_)));
            if can_promote && is_numeric(elem) {
                self.record_expr_type(&scalar.span, elem);
                return true;
            }
        }
        if scalar_ty == elem || types_compatible(scalar_ty, elem) {
            return true;
        }
        self.type_error(
            format!(
                "scalar operand of an element-wise column op must match the element \
                 type '{}', found '{}'",
                type_display(elem),
                type_display(scalar_ty)
            ),
            scalar.span.clone(),
            TypeErrorKind::TypeMismatch,
        );
        false
    }

    /// True iff `ty` is a generic type parameter carrying a `Numeric` bound in
    /// the enclosing scope. Lets the operator checks treat `a + b` / `-a` on a
    /// `T: Numeric` parameter as valid numeric arithmetic — the bound
    /// guarantees `T` instantiates to a primitive numeric type.
    pub(super) fn type_param_has_numeric_bound(&self, ty: &Type) -> bool {
        let Type::TypeParam(name) = ty else {
            return false;
        };
        self.enclosing_bounds.get(name).is_some_and(|bounds| {
            bounds
                .iter()
                .any(|b| b.path.last().is_some_and(|t| t == "Numeric"))
        })
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
                    // types. Floats keep the looser `types_compatible` check.
                    let both_ints = matches!(left_ty, Type::Int(_) | Type::UInt(_))
                        && matches!(right_ty, Type::Int(_) | Type::UInt(_));
                    if both_ints {
                        if left_ty != right_ty {
                            self.type_error(
                                format!(
                                    "cannot mix integer types '{}' and '{}' in arithmetic — they \
                                     must match; cast explicitly with `as` (e.g. the operand as '{}')",
                                    type_display(&left_ty),
                                    type_display(&right_ty),
                                    type_display(&left_ty)
                                ),
                                right.span.clone(),
                                TypeErrorKind::TypeMismatch,
                            );
                        }
                    } else if !types_compatible(&left_ty, &right_ty) {
                        self.type_error(
                            format!(
                                "expected '{}', found '{}'",
                                type_display(&left_ty),
                                type_display(&right_ty)
                            ),
                            right.span.clone(),
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
                            right.span.clone(),
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
                            right.span.clone(),
                            TypeErrorKind::TypeMismatch,
                        );
                    }
                    left_ty
                } else {
                    self.type_error(
                        format!(
                            "arithmetic operator requires numeric type, found '{}'",
                            type_display(&left_ty)
                        ),
                        left.span.clone(),
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
                        span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if !self.type_supports_partial_eq(cmp_left) {
                    self.type_error(
                        format!(
                            "type '{}' does not implement Eq; add #[derive(Eq)] to use == or !=",
                            type_display(cmp_left)
                        ),
                        span.clone(),
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
                        span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                } else if matches!(cmp_left, Type::Named { name, .. } if self.env.distinct_types.contains_key(name))
                    && !self.type_supports_partial_ord(cmp_left)
                {
                    // Distinct types are opaque — ordering comparisons require
                    // an explicit `#[derive(Ord)]` (design.md § Distinct Types:
                    // "no comparison unless opted in"). Other named types keep
                    // their pre-existing comparison behavior.
                    self.type_error(
                        format!(
                            "type '{}' does not implement Ord; add #[derive(Ord)] to use \
                             <, <=, >, or >=",
                            type_display(cmp_left)
                        ),
                        span.clone(),
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
                        left.span.clone(),
                        TypeErrorKind::InvalidBinaryOp,
                    );
                }
                if right_ty != Type::Bool {
                    self.type_error(
                        format!(
                            "logical operator requires 'bool', found '{}'",
                            type_display(&right_ty)
                        ),
                        right.span.clone(),
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
                        left.span.clone(),
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
                        right.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
                left_ty
            }
            BinOp::Range | BinOp::RangeInclusive => {
                if !types_compatible(&left_ty, &right_ty) {
                    self.type_error(
                        "range bounds must have same type".to_string(),
                        span.clone(),
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
                            span.clone(),
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
                            span.clone(),
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
                {
                    self.type_error(
                        format!(
                            "unary '-' requires numeric type, found '{}'",
                            type_display(&ty)
                        ),
                        span.clone(),
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
                        format!("unary '!' requires 'bool', found '{}'", type_display(&ty)),
                        span.clone(),
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
                        span.clone(),
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
                        span.clone(),
                        TypeErrorKind::InvalidUnaryOp,
                    );
                    Type::Error
                }
            },
        }
    }

    // ── Pipe Desugaring ──────────────────────────────────────────

    pub(super) fn infer_pipe(&mut self, left: &Expr, right: &Expr, span: &Span) -> Type {
        match &right.kind {
            // a |> f => f(a)
            ExprKind::Identifier(_) | ExprKind::Path { .. } => {
                let synthetic_arg = CallArg {
                    label: None,
                    mut_marker: false,
                    value: left.clone(),
                    span: left.span.clone(),
                };
                self.infer_call(right, &[synthetic_arg], span)
            }

            // a |> f(args...) => f(a, args...) or f(args with _ replaced)
            ExprKind::Call { callee, args } => {
                // Count _ placeholders in args
                let placeholder_count = args
                    .iter()
                    .filter(|arg| matches!(arg.value.kind, ExprKind::PipePlaceholder))
                    .count();

                if placeholder_count > 1 {
                    self.type_error(
                        "at most one '_' placeholder allowed per pipe stage".to_string(),
                        right.span.clone(),
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

                // Build the desugared argument list
                let desugared_args: Vec<CallArg> = if placeholder_count == 1 {
                    // Replace _ with the left-hand value
                    args.iter()
                        .map(|arg| {
                            if matches!(arg.value.kind, ExprKind::PipePlaceholder) {
                                CallArg {
                                    label: arg.label.clone(),
                                    mut_marker: arg.mut_marker,
                                    value: left.clone(),
                                    span: left.span.clone(),
                                }
                            } else {
                                arg.clone()
                            }
                        })
                        .collect()
                } else {
                    // No placeholder — prepend left as first argument
                    let mut new_args = vec![CallArg {
                        label: None,
                        mut_marker: false,
                        value: left.clone(),
                        span: left.span.clone(),
                    }];
                    new_args.extend(args.iter().cloned());
                    new_args
                };

                self.infer_call(callee, &desugared_args, span)
            }

            _ => {
                self.type_error(
                    "right-hand side of pipe must be a function name or function call".to_string(),
                    right.span.clone(),
                    TypeErrorKind::NotCallable,
                );
                self.infer_expr(right);
                Type::Error
            }
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
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                return Type::Error;
            }
        };

        let return_ty = match self.current_return_type.clone() {
            Some(t) => t,
            None => {
                self.type_error(
                    "'?' operator used outside a function body".to_string(),
                    span.clone(),
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
                    span.clone(),
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
                            span.clone(),
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
                    span.clone(),
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
                    span.clone(),
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
                    span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
                Type::Error
            }
        }
    }
}
