//! Field access and struct-literal inference.
//!
//! Houses `infer_field_access` (with primitive-type associated-constant
//! lookup), `check_cross_module_field_access` (CR-24 slice 6b
//! private-field visibility check), `infer_imported_field_access`
//! (imported-struct field-access surface), and `infer_struct_literal`
//! (positional/named field literal inference, shared-struct
//! recognition).

use crate::ast::*;
use crate::token::Span;
use std::collections::HashSet;

use super::const_eval::primitive_const_type;
use super::env::StructInfo;
use super::types::Type;
use super::{extract_derived_traits, extract_must_use_message, find_struct_def, TypeErrorKind};

impl<'a> super::TypeChecker<'a> {
    pub(super) fn infer_field_access(&mut self, object: &Expr, field: &str, span: &Span) -> Type {
        // Primitive-type associated constants — `i64.MAX`, `f64.INFINITY`,
        // `usize.MAX`, etc. The parser emits these as
        // `FieldAccess { object: Identifier("<primitive>"), field: "<NAME>" }`.
        // Intercept here before `infer_expr(object)` would silently return
        // `Type::Error` for the bare primitive identifier. The lookup
        // returns the const's typed surface so downstream code (`let x =
        // i64.MAX;`) sees the right `Type::Int(I64)` etc.
        if let ExprKind::Identifier(name) = &object.kind {
            if let Some(cv) = crate::prelude::lookup_primitive_const(name, field) {
                return primitive_const_type(cv);
            }
            // `ExitCode.SUCCESS` / `ExitCode.FAILURE` — paren-free
            // associated constants of the `ExitCode` distinct type
            // (Phase-8 entry-point contract Slice B). These resolve to
            // the `ExitCode` type itself (NOT the bare `i32` base) so
            // `main() -> ExitCode { ExitCode.SUCCESS }` type-checks; the
            // interpreter / codegen sibling intercepts yield the matching
            // `0` / `1` value / constant.
            if crate::prelude::lookup_exitcode_const(name, field).is_some() {
                return Type::Named {
                    name: name.clone(),
                    args: Vec::new(),
                };
            }
        }
        let obj_ty = self.infer_expr(object);
        if obj_ty == Type::Error {
            return Type::Error;
        }

        // Slice 1b: opaque foreign types (`unsafe extern { type Foo; }`)
        // have no fields visible to Kāra — the C side owns the layout, so
        // even `r.field` through a `ref Foo` / `mut ref Foo` has no
        // meaningful resolution. The bare `Type::Named` arm is a defensive
        // belt for typecheck-error-recovery flows; the by-value binding
        // itself would already have fired `E_OPAQUE_TYPE_REQUIRES_INDIRECTION`
        // upstream.
        let opaque_receiver_name = match &obj_ty {
            Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                Type::Named { name, .. } if self.env.opaque_foreign_types.contains(name) => {
                    Some(name.clone())
                }
                _ => None,
            },
            Type::Named { name, .. } if self.env.opaque_foreign_types.contains(name) => {
                Some(name.clone())
            }
            _ => None,
        };
        if let Some(name) = opaque_receiver_name {
            self.type_error(
                format!(
                    "error[E_OPAQUE_TYPE_NO_FIELDS]: opaque foreign type '{name}' \
                     has no fields visible to Kāra; the C side owns the layout. \
                     Field access through `ref {name}` / `mut ref {name}` is not \
                     supported — pass the reference to a foreign function declared \
                     in the same `unsafe extern {{ }}` block instead"
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
            return Type::Error;
        }

        let type_name = match &obj_ty {
            Type::Named { name, .. } => name.clone(),
            // Shared-struct receivers (`Type::Shared(name)` — a `shared
            // struct N { ... }`'s value type) carry the same struct
            // definition lookup as a bare `Type::Named { name, args: [] }`.
            // Without this arm, `node.field` on a pattern-bound shared
            // handle falls through to `Type::Error` and silently breaks
            // every downstream consumer (match scrutinee inference,
            // method dispatch, pattern-binding type recording).
            Type::Shared(name) => name.clone(),
            // FFI union receivers reached through a `ref U` / `mut ref U`
            // borrow (line 549 slice 2a) — `r.field` where `r: ref Foo`
            // is identical to `u.field` where `u: Foo` from the
            // typechecker's perspective. Peeled here so the union arm
            // below sees the underlying name. Non-union targets behind
            // `ref` keep falling through to `Type::Error` so the
            // struct-field-access-through-ref path (which doesn't exist
            // yet for arbitrary structs) doesn't accidentally light up.
            Type::Ref(inner) | Type::MutRef(inner) => match inner.as_ref() {
                Type::Named { name, .. } if self.env.unions.contains_key(name) => name.clone(),
                _ => return Type::Error,
            },
            _ => return Type::Error,
        };

        // Line 549 slice 2a: union receivers route here through the same
        // `Type::Named { name, ... }` shape that structs do (unions live
        // in their own `env.unions` map rather than `env.structs`). On
        // a successful field lookup, fire `E_UNION_READ_REQUIRES_UNSAFE`
        // unless we're inside an `unsafe { ... }` block OR the access
        // is the immediate LHS of a `StmtKind::Assign` (field assignment
        // is unconditionally safe per design.md § FFI Unions). Capture
        // `is_lhs` at entry and reset it for nested reads — `a.b.c = x`
        // where `a.b` reads union `a`'s field `b` must still fire.
        //
        // Slice 2b layers borrow-context routing on top of 2a: when the
        // typechecker entered the field access through a call-arg
        // position whose parameter is `ref T` / `mut ref T`,
        // `borrow_context` is `Some(_)` and the borrow-flavored
        // `E_UNION_BORROW_REQUIRES_UNSAFE` fires instead of the
        // read-flavored 2a code. The context is taken (cleared) on the
        // outermost union access so nested non-borrow union reads
        // inside the same arg expression still route through 2a.
        let is_lhs = self.assigning_lhs;
        self.assigning_lhs = false;
        let borrow_kind = self.borrow_context.take();
        let union_fields = self.env.unions.get(&type_name).map(|u| u.fields.clone());
        if let Some(fields) = union_fields {
            if !is_lhs && self.unsafe_depth == 0 {
                if let Some(kind) = borrow_kind {
                    self.type_error(
                        format!(
                            "error[E_UNION_BORROW_REQUIRES_UNSAFE]: borrowing \
                             field '{field}' of union '{type_name}' as \
                             `{kind} T` must be wrapped in an `unsafe \
                             {{ ... }}` block — the active variant of a \
                             union is not tracked by the type system, so the \
                             borrower is asserting they know which \
                             interpretation of the bytes is valid. Field \
                             *assignment* (`u.{field} = ...`) is \
                             unconditionally safe and does not require \
                             unsafe."
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                } else {
                    self.type_error(
                        format!(
                            "error[E_UNION_READ_REQUIRES_UNSAFE]: reading field \
                             '{field}' of union '{type_name}' must be wrapped in an \
                             `unsafe {{ ... }}` block — the active variant of a \
                             union is not tracked by the type system, so the reader \
                             is asserting they know which interpretation of the \
                             bytes is valid. Field *assignment* (`u.{field} = ...`) \
                             is unconditionally safe and does not require unsafe."
                        ),
                        span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            for (fname, ftype, is_pub) in &fields {
                if fname == field {
                    if !*is_pub {
                        self.check_cross_module_field_access(&type_name, field, span);
                    }
                    return ftype.clone();
                }
            }
            let available: Vec<&str> = fields.iter().map(|(n, _, _)| n.as_str()).collect();
            self.type_error(
                format!(
                    "no field '{}' on union '{}', available fields: {}",
                    field,
                    type_name,
                    available.join(", ")
                ),
                span.clone(),
                TypeErrorKind::UndefinedField,
            );
            return Type::Error;
        }

        if let Some(struct_info) = self.env.structs.get(&type_name) {
            let struct_info = struct_info.clone();
            for (fname, ftype, is_pub) in &struct_info.fields {
                if fname == field {
                    // CR-18 field-access half: reject non-`pub` field access
                    // on an imported struct from outside the defining module.
                    if !is_pub {
                        self.check_cross_module_field_access(&type_name, field, span);
                    }
                    return ftype.clone();
                }
            }
            let available: Vec<&str> = struct_info
                .fields
                .iter()
                .map(|(n, _, _)| n.as_str())
                .collect();
            self.type_error(
                format!(
                    "no field '{}' on struct '{}', available fields: {}",
                    field,
                    type_name,
                    available.join(", ")
                ),
                span.clone(),
                TypeErrorKind::UndefinedField,
            );
            Type::Error
        } else {
            // Not in the local env, but may be an imported struct — probe the
            // origin module directly so cross-module field access can still
            // be validated for CR-18.
            self.infer_imported_field_access(&type_name, field, span)
        }
    }

    /// Emit `E0221 PrivateTypeInPublicSignature` when a non-`pub` field is
    /// accessed on an imported struct from outside its defining module. For
    /// local structs (and when no `ProgramTree` is attached), silently
    /// accepts the access — slice 6b treats same-module field access as
    /// always allowed.
    pub(super) fn check_cross_module_field_access(
        &mut self,
        struct_name: &str,
        field: &str,
        span: &Span,
    ) {
        let Some(tree) = self.tree else { return };
        let Some(current_id) = self.current_module else {
            return;
        };
        let current_path = tree.module(current_id).path.clone();

        // Find the defining module. For a local struct, origin == current.
        let origin_path: Vec<String> = match self.type_origins.get(struct_name) {
            Some((path, _, _)) => path.clone(),
            None => current_path.clone(),
        };
        if origin_path == current_path {
            // Same-module access — non-pub fields are always reachable to
            // sibling code.
            return;
        }
        self.type_error(
            format!(
                "private field '{}' of struct '{}' is not visible outside its defining module",
                field, struct_name,
            ),
            span.clone(),
            TypeErrorKind::PrivateTypeInPublicSignature,
        );
    }

    /// Access a field on a struct that isn't registered in the local env —
    /// typically an imported struct from another module. Consults the
    /// `ProgramTree` so we can (a) return the field type and (b) enforce
    /// the cross-module field-visibility rule.
    fn infer_imported_field_access(&mut self, struct_name: &str, field: &str, span: &Span) -> Type {
        let Some(tree) = self.tree else {
            return Type::Error;
        };
        let Some((origin_path, canonical_name, _vis)) = self.type_origins.get(struct_name).cloned()
        else {
            return Type::Error;
        };
        let Some(&origin_id) = tree.graph.by_path.get::<[String]>(&origin_path) else {
            return Type::Error;
        };
        let origin_module = tree.module(origin_id);
        // Look up by the canonical name — `struct_name` here may be an
        // import alias (`import db.Connection as Conn` binds `Conn` but the
        // struct is defined as `Connection`). The canonical name survives
        // the chain walked in `collect_import_origins`.
        let Some(struct_def) = find_struct_def(origin_module, &canonical_name) else {
            return Type::Error;
        };
        let field_def = match struct_def.fields.iter().find(|f| f.name == field) {
            Some(f) => f,
            None => {
                let available: Vec<&str> =
                    struct_def.fields.iter().map(|f| f.name.as_str()).collect();
                self.type_error(
                    format!(
                        "no field '{}' on struct '{}', available fields: {}",
                        field,
                        struct_name,
                        available.join(", ")
                    ),
                    span.clone(),
                    TypeErrorKind::UndefinedField,
                );
                return Type::Error;
            }
        };

        if !field_def.is_pub {
            // `origin_path` is guaranteed to differ from `current_module`'s
            // path because `type_origins` only holds cross-module entries.
            self.type_error(
                format!(
                    "private field '{}' of struct '{}' is not visible outside its defining module",
                    field, struct_name,
                ),
                span.clone(),
                TypeErrorKind::PrivateTypeInPublicSignature,
            );
        }

        // Return the field's declared type. We lower the TypeExpr with an
        // empty generic scope — the origin module's generics are not in
        // scope here, and that's OK for slice-6b's coarse cross-module type
        // surface.
        self.lower_type_expr(&field_def.ty, &[])
    }

    // ── FFI Union Literals (line 549 slice 2c) ──────────────────

    /// Construct a value of an FFI union type via the
    /// `Name { field: value }` literal shape. The exactly-one-field
    /// rule is what makes union construction *safe* (design.md §
    /// FFI Unions): the bytes are written with a single named
    /// interpretation, so no reinterpretation is happening at the
    /// construction site, so no `unsafe { ... }` block is required.
    ///
    /// Diagnostic-shape contract:
    /// - Zero fields (`Foo {}`) and multi-field (`Foo { a: x, b: y }`)
    ///   both fire `E_UNION_LITERAL_REQUIRES_ONE_FIELD` at the literal
    ///   span; the message lists the available field names so the
    ///   recovery shape is obvious.
    /// - A spread base (`Foo { ..base }`) is rejected with the same
    ///   code — "copy remaining fields from base" is meaningless when
    ///   only one field is active at a time. The spread expression is
    ///   still inferred so its own diagnostics surface.
    /// - An unknown field name fires the standard undefined-field
    ///   diagnostic (mirroring slice 2a's union-field-access path)
    ///   naming the union and the available fields.
    /// - In every error case the field values are still typechecked
    ///   against the declared field type when known, falling back to
    ///   synth otherwise, so cascading diagnostics don't fire.
    pub(super) fn infer_union_literal(
        &mut self,
        union_name: &str,
        fields: &[FieldInit],
        spread: Option<&Expr>,
        span: &Span,
    ) -> Type {
        let union_fields = self
            .env
            .unions
            .get(union_name)
            .expect("caller verified union_name is in env.unions")
            .fields
            .clone();

        self.check_deprecated_use_at(span, union_name);
        self.check_unstable_use_at(span, union_name);

        if let Some(spread_expr) = spread {
            self.infer_expr(spread_expr);
            self.type_error(
                format!(
                    "error[E_UNION_LITERAL_REQUIRES_ONE_FIELD]: union '{union_name}' \
                     literal does not support spread base (`..base`) — a union's \
                     storage is shared by every field, so 'copy the remaining \
                     fields from base' is meaningless. Write `{union_name} \
                     {{ <field>: <expr> }}` with exactly one field instead."
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
        }

        if fields.len() != 1 {
            let avail: Vec<String> = union_fields
                .iter()
                .map(|(n, _, _)| format!("'{n}'"))
                .collect();
            self.type_error(
                format!(
                    "error[E_UNION_LITERAL_REQUIRES_ONE_FIELD]: union \
                     '{union_name}' literal must name exactly one field — \
                     got {n}. A union's storage is shared by every field, so \
                     construction commits to one named interpretation. Write \
                     `{union_name} {{ <field>: <expr> }}` with one of: {avail}.",
                    n = fields.len(),
                    avail = avail.join(", "),
                ),
                span.clone(),
                TypeErrorKind::TypeMismatch,
            );
        }

        for f in fields {
            let matched = union_fields
                .iter()
                .find(|(n, _, _)| n == &f.name)
                .map(|(_, ty, is_pub)| (ty.clone(), *is_pub));
            match matched {
                Some((expected_ty, is_pub)) => {
                    if !is_pub {
                        self.check_cross_module_field_access(union_name, &f.name, &f.span);
                    }
                    self.check_expr(&f.value, &expected_ty);
                }
                None => {
                    let avail: Vec<&str> =
                        union_fields.iter().map(|(n, _, _)| n.as_str()).collect();
                    self.type_error(
                        format!(
                            "no field '{}' on union '{}', available fields: {}",
                            f.name,
                            union_name,
                            avail.join(", ")
                        ),
                        f.span.clone(),
                        TypeErrorKind::UndefinedField,
                    );
                    self.infer_expr(&f.value);
                }
            }
        }

        Type::Named {
            name: union_name.to_string(),
            args: Vec::new(),
        }
    }

    // ── Struct Literals ─────────────────────────────────────────

    /// Resolve a module-qualified struct path (`module.Type`, `a.b.Type`) to a
    /// `StructInfo` by locating the defining module in the program tree and
    /// lowering its field types. Returns `None` for a bare (single-segment)
    /// path or when the addressed module has no such struct. Generics get an
    /// empty scope — cross-module generic construction is a coarse surface,
    /// matching `infer_imported_field_access`.
    fn resolve_qualified_struct_info(&mut self, path: &[String]) -> Option<StructInfo> {
        if path.len() < 2 {
            return None;
        }
        let tree = self.tree?;
        let name = path.last()?;
        let module_path = &path[..path.len() - 1];
        let &mid = tree.graph.by_path.get::<[String]>(module_path)?;
        let module = tree.module(mid);
        let sdef = find_struct_def(module, name)?;
        let gp = Self::generic_param_names(&sdef.generic_params);
        let fields: Vec<(String, Type, bool)> = sdef
            .fields
            .iter()
            .map(|f| (f.name.clone(), self.lower_type_expr(&f.ty, &gp), f.is_pub))
            .collect();
        let field_attrs: std::collections::HashMap<String, Vec<String>> = sdef
            .fields
            .iter()
            .filter(|f| !f.attributes.is_empty())
            .map(|f| {
                (
                    f.name.clone(),
                    f.attributes
                        .iter()
                        .map(crate::typechecker::render_attribute)
                        .collect(),
                )
            })
            .collect();
        Some(StructInfo {
            generic_params: gp,
            fields,
            field_attrs,
            derived_traits: extract_derived_traits(&sdef.attributes),
            no_rc: sdef.no_rc,
            is_shared: sdef.is_shared,
            is_par: sdef.is_par,
            must_use_message: extract_must_use_message(&sdef.attributes),
            is_non_exhaustive: sdef.is_non_exhaustive,
            defining_stdlib_origin: sdef.stdlib_origin,
        })
    }

    pub(super) fn infer_struct_literal(
        &mut self,
        path: &[String],
        fields: &[FieldInit],
        span: &Span,
    ) -> Type {
        let struct_name = path.last().cloned().unwrap_or_default();

        let struct_info = match self.env.structs.get(&struct_name) {
            Some(info) => info.clone(),
            None => match self.resolve_qualified_struct_info(path) {
                // Module-qualified construction `module.Type { .. }`: the type
                // is not bound by bare name in this module (only the module is
                // imported), so resolve its definition from the defining module
                // via the program tree. Mirrors how `module.Type` type
                // annotations and `module.fn()` calls already resolve.
                Some(info) => info,
                None => {
                    // Type-check field values anyway
                    for f in fields {
                        self.infer_expr(&f.value);
                    }
                    self.type_error(
                        format!("'{}' is not a struct", struct_name),
                        span.clone(),
                        TypeErrorKind::NotAStruct,
                    );
                    return Type::Error;
                }
            },
        };

        // `#[deprecated]` slice 4 — emit the deprecation warning when
        // the struct's defining decl carries a `Deprecation` payload.
        // Surface here is struct-literal construction (`Foo { ... }`);
        // the type-position reference (`var: Foo`) is covered by the
        // `lower_path_type` site, and the pattern reference is covered
        // by `check_pattern_against`'s struct-pattern arm.
        self.check_deprecated_use_at(span, &struct_name);
        self.check_unstable_use_at(span, &struct_name);

        // `#[non_exhaustive]` slice 4 — cross-package struct-literal
        // enforcement. A `pub struct` marked `#[non_exhaustive]` may
        // grow additional fields without breaking source compatibility;
        // outside-package consumers therefore cannot enumerate the
        // current field set in a literal (an added field would silently
        // become a missing-field error at the consumer). The defining
        // package retains exhaustive literal use because it owns the
        // shape. Today the only inter-package boundary the compiler
        // tracks is stdlib-vs-user (`stdlib_origin`); when richer
        // per-package boundaries land, the comparison below shifts at
        // this site without touching the `is_non_exhaustive` plumbing.
        if struct_info.is_non_exhaustive
            && struct_info.defining_stdlib_origin
            && !self.current_fn_stdlib_origin
        {
            self.type_error(
                format!(
                    "error[E_NON_EXHAUSTIVE_CROSS_PACKAGE_LITERAL]: \
                     cannot construct `{name}` with a struct literal — \
                     `{name}` is `#[non_exhaustive]` and defined in another \
                     package, so its field set may grow. Construct through \
                     the type's public constructor (commonly `{name}.new(...)`) \
                     instead. See design.md § `#[non_exhaustive]` for \
                     Evolvable Public Types.",
                    name = struct_name
                ),
                span.clone(),
                TypeErrorKind::NonExhaustiveCrossPackageLiteral,
            );
        }

        let expected_fields: HashSet<&str> = struct_info
            .fields
            .iter()
            .map(|(n, _, _)| n.as_str())
            .collect();
        let provided_fields: HashSet<&str> = fields.iter().map(|f| f.name.as_str()).collect();

        // Check for missing fields
        for (fname, _, _) in &struct_info.fields {
            if !provided_fields.contains(fname.as_str()) {
                self.type_error(
                    format!("missing field '{}' in struct '{}'", fname, struct_name),
                    span.clone(),
                    TypeErrorKind::MissingField,
                );
            }
        }

        // Check for extra fields
        for f in fields {
            if !expected_fields.contains(f.name.as_str()) {
                self.type_error(
                    format!("unknown field '{}' in struct '{}'", f.name, struct_name),
                    f.span.clone(),
                    TypeErrorKind::ExtraField,
                );
            }
        }

        // Type-check field values. Use `check_expr` against the field's
        // declared type when known so check-mode coercions (empty
        // `Vec[]` / `Set[]` / `Array[]`, `Into` / `TryInto`, closure
        // pushdown, etc.) fire at struct-field initializer positions.
        // Fall back to synthesis when the field is not declared on the
        // struct (already diagnosed above as an extra field).
        for f in fields {
            if let Some((_, expected_ty, _)) =
                struct_info.fields.iter().find(|(n, _, _)| n == &f.name)
            {
                self.check_expr(&f.value, &expected_ty.clone());
            } else {
                self.infer_expr(&f.value);
            }
        }

        // Shared-struct literals lower to Type::Shared so the literal's
        // type matches an annotated `let s: S = S { ... }` shape and the
        // method-resolution deref step (sub-item 3a) sees a consistent
        // receiver type. Sub-item 2's `lower_path_type` intercept handles
        // the annotation side; this is its construction-site twin.
        //
        // `par struct` literals lower to `Type::Shared` for the same reason:
        // both `shared` and `par` are reference-semantics handle types (no
        // exclusive ownership — passing clones the handle). The Rc-vs-Arc and
        // cross-task distinctions are made via `StructInfo.is_par` in later
        // phases (codegen, cross-task-safe), not at the bare-`Type` level — so
        // no `Type::Par` variant is needed. See design.md § Part 5b ("Passing
        // to functions"). The cross-task-safe pass keys off `Type::Shared` to
        // reject; Slice B teaches it to exempt `is_par` types.
        if struct_info.is_shared || struct_info.is_par {
            Type::Shared(struct_name)
        } else {
            Type::Named {
                name: struct_name,
                args: Vec::new(),
            }
        }
    }

    // ── Enum struct-variant literals ────────────────────────────
    //
    // `Enum.Variant { field: value, ... }` — qualified construction of an
    // enum's struct-shaped variant. The parser produces the same
    // `StructLiteral` node a struct literal does, with `path = [.., Enum,
    // Variant]`; the dispatcher routes here when the leading segment names a
    // known enum whose `Variant` is a `VariantTypeInfo::Struct`. Without this
    // path the node falls to `infer_struct_literal`, which looks up the last
    // segment (`Variant`) as a *struct* and rejects it as "not a struct".

    /// If `Enum.Variant` names a struct-shaped variant of a known enum,
    /// return its declared `(field, type)` list (cloned). `None` otherwise
    /// (unit/tuple variant, unknown enum, or a plain struct path).
    pub(super) fn enum_struct_variant_fields(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Option<Vec<(String, Type)>> {
        let info = self.env.enums.get(enum_name)?;
        let (_, vinfo) = info.variants.iter().find(|(n, _)| n == variant)?;
        match vinfo {
            super::types::VariantTypeInfo::Struct(fields) => Some(fields.clone()),
            _ => None,
        }
    }

    /// Resolve an *unqualified* struct-variant construction `Variant { ... }`
    /// to its `(enum_name, declared_fields)` via the resolver. The resolver
    /// already binds the bare `Variant` segment of a `StructLiteral` to its
    /// `EnumVariant` symbol (handling scoping, imports, and ambiguity); we read
    /// that resolution back here so the typechecker can route to
    /// `infer_enum_struct_variant_literal` instead of rejecting `Variant` as a
    /// missing struct. The resolution is keyed by the literal's own span (see
    /// `resolve_block.rs`'s `StructLiteral` arm, which records the head segment
    /// against `expr.span`). Returns `None` when the span resolves to anything
    /// but a struct-shaped enum variant (a plain struct path, a unit/tuple
    /// variant, or an unresolved name). The qualified form `Enum.Variant { ... }`
    /// is handled separately by the `path[len-2]` dispatch and never reaches here.
    pub(super) fn unqualified_enum_struct_variant(
        &self,
        literal_span: &Span,
        variant: &str,
    ) -> Option<(String, Vec<(String, Type)>)> {
        use crate::resolver::{SpanKey, SymbolKind};
        let sym_id = self
            .resolve_result
            .resolutions
            .get(&SpanKey::from_span(literal_span))
            .copied()?;
        let sym = self.resolve_result.symbol_table.get_symbol(sym_id);
        let SymbolKind::EnumVariant { parent_enum, .. } = sym.kind else {
            return None;
        };
        let enum_name = self
            .resolve_result
            .symbol_table
            .get_symbol(parent_enum)
            .name
            .clone();
        let fields = self.enum_struct_variant_fields(&enum_name, variant)?;
        Some((enum_name, fields))
    }

    /// Type-check an `Enum.Variant { ... }` literal against the variant's
    /// declared struct fields and return the enum type. Mirrors
    /// `infer_struct_literal`'s missing/extra/field-type checks; like it,
    /// generic args are left empty (`Type::Named { args: [] }`) — context
    /// (`let e: Enum[T] = ...`) drives instantiation, not the literal.
    pub(super) fn infer_enum_struct_variant_literal(
        &mut self,
        enum_name: &str,
        variant: &str,
        declared_fields: &[(String, Type)],
        fields: &[FieldInit],
        span: &Span,
    ) -> Type {
        let provided: HashSet<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        for (fname, _) in declared_fields {
            if !provided.contains(fname.as_str()) {
                self.type_error(
                    format!("missing field '{fname}' in enum variant '{enum_name}.{variant}'"),
                    span.clone(),
                    TypeErrorKind::MissingField,
                );
            }
        }
        for f in fields {
            if let Some((_, expected_ty)) = declared_fields.iter().find(|(n, _)| n == &f.name) {
                self.check_expr(&f.value, &expected_ty.clone());
            } else {
                self.type_error(
                    format!(
                        "unknown field '{}' in enum variant '{enum_name}.{variant}'",
                        f.name
                    ),
                    f.span.clone(),
                    TypeErrorKind::ExtraField,
                );
                self.infer_expr(&f.value);
            }
        }
        Type::Named {
            name: enum_name.to_string(),
            args: Vec::new(),
        }
    }
}
