//! Pass-2 item checking: walk the program with the populated TypeEnv
//! and check function bodies, trait declarations, impl blocks, const
//! declarations, plus the visibility audit on public signatures.
//!
//! Houses `check_items` (the driver), `check_trait_def`, the
//! visibility-audit triad (`collect_type_visibility`,
//! `check_type_expr_visibility`, `check_signature_visibility`),
//! `check_function`, `check_impl_block`, `check_const_decl`, and the
//! statement/block-level inference primitives (`check_block_against`,
//! `infer_block`, `check_stmt`, `check_unsolved_type_param`).

use crate::ast::*;
use crate::resolver::SpanKey;
use crate::token::{IntSuffix, Span};
use std::collections::{HashMap, HashSet};

use super::const_eval::{
    apply_binary, apply_unary, const_value_type, infer_operand_target_ty, integer_to_const_value,
};
use super::inference::{find_unbound_const_param, find_unbound_type_param};
use super::types::{
    type_display, type_is_fully_concrete, IntSize, ScrutineeMode, Type, UIntSize, VariantTypeInfo,
};
use super::{ConstEvalError, LocalTypeScope, TypeErrorKind};

impl<'a> super::TypeChecker<'a> {
    pub(super) fn check_items(&mut self) {
        let items: Vec<Item> = self.program.items.clone();
        for item in &items {
            match item {
                Item::Function(f) => {
                    // `#[compiler_builtin]` declarations carry a placeholder
                    // body that is replaced by Rust dispatch at runtime
                    // (CR-202 slice 2). The signature is the contract callers
                    // are checked against; the body itself is irrelevant, so
                    // skip body-checking entirely. This lets stdlib source
                    // pair an attribute with whatever body keeps the parser
                    // happy without that body being held to type-correctness.
                    if self.env.compiler_builtins.contains(&f.name) {
                        continue;
                    }
                    self.check_function(f, None, &[]);
                    self.check_wasm_export_boundary(f);
                }
                Item::ImplBlock(imp) => self.check_impl_block(imp),
                Item::TraitDef(t) => self.check_trait_def(t),
                Item::ConstDecl(c) => self.check_const_decl(c),
                Item::ModuleBinding(b) => self.check_module_binding(b),
                Item::StructDef(s) => {
                    let gp = Self::generic_param_names(&s.generic_params);
                    self.validate_all_bounds(&s.generic_params, &s.where_clause, &gp);
                    // Variance declarations (design.md § Variance):
                    // user-side `+`/`-` rejection, stdlib-side
                    // explicit-marker lint + structural verifier.
                    self.check_struct_variance(s);
                    self.check_struct_invariants(s, &gp);
                    self.check_repr_transparent_struct(s, &gp);
                    if s.is_par {
                        self.check_par_field_constraints("struct", &s.name, &s.fields);
                    }
                }
                Item::EnumDef(e) => {
                    let gp = Self::generic_param_names(&e.generic_params);
                    self.validate_all_bounds(&e.generic_params, &e.where_clause, &gp);
                    self.check_enum_variance(e);
                    self.check_repr_transparent_enum(e);
                    self.check_enum_discriminants(e);
                    if e.is_par {
                        for v in &e.variants {
                            if let VariantKind::Struct(fields) = &v.kind {
                                self.check_par_field_constraints("enum", &e.name, fields);
                            }
                        }
                    }
                }
                Item::UnionDef(u) => self.check_repr_transparent_union(u),
                Item::DistinctType(d) => self.check_repr_transparent_distinct(d),
                // Variance markers are legal only on stdlib struct/enum
                // declarations (design.md § Variance) — never on type
                // aliases. The alias's other checks (bound enforcement,
                // phase-8 "type alias bounds" entry) live elsewhere.
                Item::TypeAlias(t) => {
                    self.reject_user_variance_markers(&t.generic_params, false);
                }
                // `host fn` boundary-type restrictions (phase-10,
                // design.md § Host Functions > Parameter and return
                // types). Extern-block fns are exempt — `extern "C"`
                // is the raw C-ABI door and keeps its own rules.
                Item::ExternFunction(e) if e.abi == "host" => {
                    self.check_host_fn_boundary(e);
                }
                _ => {}
            }
        }
    }

    /// Enforce the binding-surface restriction on a discovered WASM
    /// export (phase-10 "WASM entry-point discovery", design.md § Entry
    /// point discovery): a `pub fn` positively tagged
    /// `#[target(wasm_browser)]` / `#[target(wasm_wasi)]` may only take /
    /// return types expressible across the chosen boundary.
    ///
    /// Both `#[target(wasm_browser)]` and `#[target(wasm_wasi)]` exports
    /// marshal the rich owned surface — records / `option` / `result` /
    /// `string` / `list` — via the canonical-ABI trampolines (the browser
    /// glue marshals JS objects against the same layout the component WIT
    /// describes). So the only hard rejection is a borrow (`ref` /
    /// `mut ref`), which has no by-value export form. Owned types the
    /// codegen trampoline does not yet lower (nested aggregates, variant
    /// params) are not hard errors: they are omitted from the typed
    /// surface with a build-time note (`cli::warn_unlowered_exports`) and
    /// remain raw core exports.
    fn check_wasm_export_boundary(&mut self, f: &Function) {
        let Some(spec) = crate::target::target_spec_of(&f.attributes) else {
            return;
        };
        if spec.negated {
            return;
        }
        if !f.is_pub || f.self_param.is_some() || f.name == "main" {
            return;
        }
        let is_wasm_export = spec
            .names
            .iter()
            .any(|n| n == "wasm_browser" || n == "wasm_wasi");
        if !is_wasm_export {
            return;
        }
        for p in &f.params {
            if let Some(msg) = Self::wasm_export_boundary_violation(&p.ty, "parameter") {
                self.type_error(
                    format!("wasm export '{}': {msg}", f.name),
                    p.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        if let Some(ref rt) = f.return_type {
            if let Some(msg) = Self::wasm_export_boundary_violation(rt, "return") {
                self.type_error(
                    format!("wasm export '{}': {msg}", f.name),
                    rt.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// WASM export boundary: the canonical ABI / browser glue express
    /// owned records / variants / `string` / `list`, so the only hard
    /// rejection is a borrow (`ref` / `mut ref`), which has no by-value
    /// export form. Everything else is accepted; whether codegen can lower
    /// it yet is a separate, non-fatal concern (omitted-with-a-note, see
    /// `check_wasm_export_boundary`).
    fn wasm_export_boundary_violation(ty: &TypeExpr, position: &str) -> Option<String> {
        match &ty.kind {
            TypeKind::Ref(_) => Some(format!(
                "`ref` {position}s cannot cross the wasm export boundary — pass an owned value \
                 (neither the Component Model canonical ABI nor the browser glue has a borrow \
                 form for exported functions)",
            )),
            TypeKind::MutRef(_) => Some(format!(
                "`mut ref` {position}s cannot cross the wasm export boundary — pass an owned \
                 value (neither the Component Model canonical ABI nor the browser glue has a \
                 borrow form for exported functions)",
            )),
            _ => None,
        }
    }

    /// Enforce the `host fn` parameter/return restriction: primitives,
    /// `Copy`-satisfying types, and opaque-handle newtypes (single
    /// primitive-field structs) only. Owned non-`Copy` and `ref` /
    /// `mut ref` get targeted diagnostics; generics are structurally
    /// impossible (the grammar has no generic-param list).
    fn check_host_fn_boundary(&mut self, e: &ExternFunction) {
        for p in &e.params {
            if let Some(msg) = self.host_boundary_violation(&p.ty, "parameter") {
                self.type_error(
                    format!("host fn '{}': {msg}", e.name),
                    p.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
        if let Some(ref rt) = e.return_type {
            if let Some(msg) = self.host_boundary_violation(rt, "return") {
                self.type_error(
                    format!("host fn '{}': {msg}", e.name),
                    rt.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
        }
    }

    /// `None` when `ty` may cross the host boundary; `Some(message)`
    /// naming the violation otherwise. `position` is "parameter" or
    /// "return type" for message text.
    fn host_boundary_violation(&mut self, ty: &TypeExpr, position: &str) -> Option<String> {
        match &ty.kind {
            // Borrow aliasing rules are a Kāra-compiler property; the
            // host cannot be asked to honor them.
            TypeKind::Ref(_) => Some(format!(
                "`ref` {position}s cannot cross the host boundary — the host \
                 cannot honor Kāra's borrow rules; pass an opaque handle or a \
                 (pointer, length) pair instead (design.md § Host Functions)",
            )),
            TypeKind::MutRef(_) => Some(format!(
                "`mut ref` {position}s cannot cross the host boundary — the \
                 host cannot honor Kāra's borrow rules; pass an opaque handle \
                 or a (pointer, length) pair instead (design.md § Host Functions)",
            )),
            // Raw pointers are primitive at the boundary (the unsafe
            // contract is the programmer's, same as extern "C").
            TypeKind::Pointer { .. } => None,
            _ => {
                let lowered = self.lower_type_expr(ty, &[]);
                if self.host_boundary_type_ok(&lowered) {
                    None
                } else {
                    Some(format!(
                        "{position} type `{}` cannot cross the host boundary — \
                         only primitives, `Copy`-satisfying types, and \
                         opaque-handle newtypes (single primitive-field \
                         structs) are permitted; owned non-`Copy` values \
                         raise ownership-transfer questions the host cannot \
                         answer (design.md § Host Functions)",
                        type_display(&lowered),
                    ))
                }
            }
        }
    }

    /// Lowered-type leg of the host-boundary check: primitives,
    /// `Copy`-satisfying types, opaque-handle newtypes.
    fn host_boundary_type_ok(&self, ty: &Type) -> bool {
        if matches!(ty, Type::Pointer { .. }) {
            return true;
        }
        if self.is_copy_type_during_check(ty) {
            return true;
        }
        // Opaque-handle newtype: user struct with exactly one
        // primitive-typed field. The host identity is the scalar; the
        // wrapper exists purely for stronger typing — `Copy` derive is
        // not required (handles often deliberately aren't Copy so a
        // Drop impl can release the host resource).
        if let Type::Named { name, args } = ty {
            if args.is_empty() {
                if let Some(info) = self.env.structs.get(name) {
                    if info.fields.len() == 1 {
                        let (_, field_ty, _) = &info.fields[0];
                        return matches!(
                            field_ty,
                            Type::Int(_)
                                | Type::UInt(_)
                                | Type::Float(_)
                                | Type::Bool
                                | Type::Char
                                | Type::Pointer { .. }
                        );
                    }
                }
            }
        }
        false
    }

    /// Type-check default method bodies inside a trait declaration.
    /// `Self` is treated as an abstract type parameter (`Type::TypeParam("Self")`)
    /// so signature and body references to `Self`/`self` resolve consistently.
    fn check_trait_def(&mut self, t: &TraitDef) {
        // Variance markers are legal only on stdlib struct/enum
        // declarations (design.md § Variance) — never on traits.
        self.reject_user_variance_markers(&t.generic_params, false);

        let mut enclosing = vec!["Self".to_string()];
        if let Some(ref generics) = t.generic_params {
            for p in &generics.params {
                enclosing.push(p.name.clone());
            }
        }

        // Validate inline bounds and where clause on the trait itself
        self.validate_all_bounds(&t.generic_params, &t.where_clause, &enclosing);

        // Save outer bounds. Trait-level generics' bounds + supertraits-as-Self
        // are visible to default method bodies. Restored after the trait's
        // items are checked.
        let saved_bounds = self.enclosing_bounds.clone();
        for (name, bounds) in Self::collect_param_bounds(&t.generic_params, &t.where_clause) {
            self.enclosing_bounds.insert(name, bounds);
        }
        if !t.supertraits.is_empty() {
            self.enclosing_bounds
                .entry("Self".to_string())
                .or_default()
                .extend(t.supertraits.iter().cloned());
        }

        // Slice 3.5 of the method-resolution CR: track the enclosing trait so
        // `self.method()` in a default body dispatches through the trait's
        // own methods + supertrait closure rather than silently falling
        // through.
        let saved_enclosing_trait = self.enclosing_trait.take();

        // Lint-level slice 4b — push the trait declaration's lint
        // overrides for the duration of its default-body checking.
        self.lint_override_stack.push(t.lint_overrides.clone());
        self.enclosing_trait = Some(t.name.clone());

        let self_type = Type::TypeParam("Self".to_string());
        for item in &t.items {
            if let TraitItem::Method(method) = item {
                if let Some(ref body) = method.body {
                    let synthesized = Function {
                        span: method.span.clone(),
                        attributes: Vec::new(),
                        doc_comment: None,
                        is_pub: false,
                        is_private: false,
                        is_unsafe: false,
                        is_comptime: false,
                        name: method.name.clone(),
                        generic_params: method.generic_params.clone(),
                        params: method.params.clone(),
                        self_param: method.self_param.clone(),
                        return_type: method.return_type.clone(),
                        effects: method.effects.clone(),
                        requires: method.requires.clone(),
                        ensures: method.ensures.clone(),
                        where_clause: method.where_clause.clone(),
                        body: body.clone(),
                        stdlib_origin: t.stdlib_origin,
                        deprecation: None,
                        unstable: None,
                        is_track_caller: false,
                        is_gpu: false,
                        inline_hint: None,
                        is_cold: false,
                        lint_overrides: Vec::new(),
                        profile_compat: Vec::new(),
                        abi: None,
                    };
                    self.check_function(&synthesized, Some(&self_type), &enclosing);
                }
            }
        }

        self.enclosing_bounds = saved_bounds;
        self.enclosing_trait = saved_enclosing_trait;
        self.lint_override_stack.pop();
    }

    /// Build a map of user-defined type names → `is_pub`. Types absent from the
    /// map are treated as public (builtins, primitives, stdlib-registered types
    /// like `Option` / `Result` / `F32` live outside the user AST).
    ///
    /// CR-24 slice 6b: imported types are folded in under their local name
    /// (alias-aware) with the *origin* module's visibility. An imported type
    /// whose origin is `Default` or `Private` behaves identically to a
    /// locally-declared non-`pub` type when it appears in a `pub` signature
    /// — the type is not part of the current package's public API, so
    /// leaking it through one trips `E0221 PrivateTypeInPublicSignature`.
    fn collect_type_visibility(&self) -> HashMap<String, bool> {
        let mut map: HashMap<String, bool> = HashMap::new();
        for item in &self.program.items {
            match item {
                Item::StructDef(s) => {
                    map.insert(s.name.clone(), s.is_pub);
                }
                Item::EnumDef(e) => {
                    map.insert(e.name.clone(), e.is_pub);
                }
                Item::TraitDef(t) => {
                    map.insert(t.name.clone(), t.is_pub);
                }
                Item::TypeAlias(t) => {
                    map.insert(t.name.clone(), t.is_pub);
                }
                Item::DistinctType(d) => {
                    map.insert(d.name.clone(), d.is_pub);
                }
                _ => {}
            }
        }
        for (name, (_origin_path, _origin_name, vis)) in &self.type_origins {
            // Only overwrite when we don't already have a local entry for
            // this name; a local declaration shadows an import for purposes
            // of the signature check.
            map.entry(name.clone()).or_insert_with(|| vis.is_pub());
        }
        map
    }

    /// Walk a `TypeExpr` and emit `PrivateTypeInPublicSignature` for every
    /// reference to a non-`pub` user-defined type. `generic_scope` suppresses
    /// single-segment paths that name an in-scope generic parameter (e.g. `T`
    /// in `fn foo[T](x: T)`).
    ///
    /// Note on scope: the check fires on name-visible leaks only. Cross-module
    /// private-field access (`user.password_hash` from outside the defining
    /// module) is part of CR-18 but gated on the module system (CR-24) — with
    /// a single-module compilation unit, every access is "same project" per
    /// design.md § Three-level visibility, so the field rule has no firing
    /// sites today.
    fn check_type_expr_visibility(
        &mut self,
        ty: &TypeExpr,
        generic_scope: &[String],
        type_vis: &HashMap<String, bool>,
        context: &str,
        owner: &str,
    ) {
        match &ty.kind {
            TypeKind::Path(p) => {
                if let Some(ref args) = p.generic_args {
                    for a in args {
                        if let GenericArg::Type(t) = a {
                            self.check_type_expr_visibility(
                                t,
                                generic_scope,
                                type_vis,
                                context,
                                owner,
                            );
                        }
                    }
                }
                let last = match p.segments.last() {
                    Some(s) => s.clone(),
                    None => return,
                };
                if p.segments.len() == 1 && generic_scope.iter().any(|g| g == &last) {
                    return;
                }
                if let Some(false) = type_vis.get(&last).copied() {
                    self.type_error(
                        format!(
                            "private type '{}' leaks through {} of '{}'; mark the type `pub` or remove it from the public surface",
                            last, context, owner
                        ),
                        ty.span.clone(),
                        TypeErrorKind::PrivateTypeInPublicSignature,
                    );
                }
            }
            TypeKind::Tuple(ts) => {
                for t in ts {
                    self.check_type_expr_visibility(t, generic_scope, type_vis, context, owner);
                }
            }
            TypeKind::Array { element, .. } => {
                self.check_type_expr_visibility(element, generic_scope, type_vis, context, owner);
            }
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => {
                self.check_type_expr_visibility(inner, generic_scope, type_vis, context, owner);
            }
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                for p in params {
                    self.check_type_expr_visibility(p, generic_scope, type_vis, context, owner);
                }
                if let Some(ref rt) = return_type {
                    self.check_type_expr_visibility(rt, generic_scope, type_vis, context, owner);
                }
            }
            // `impl Trait` slice 1 stub: walk the trait-path's last
            // segment + generic-arg types under the same
            // private-type-leak rule as `TypeKind::Path`. Full
            // typechecker semantics for `impl Trait` land in slice 3
            // (see phase-5-diagnostics.md line 397); the visibility
            // check is independent of those semantics — a private
            // trait name in an `impl T` public-signature is just as
            // much a leak as in a `T` public-signature.
            TypeKind::ImplTrait {
                trait_path, args, ..
            } => {
                for a in args {
                    if let GenericArg::Type(t) = a {
                        self.check_type_expr_visibility(t, generic_scope, type_vis, context, owner);
                    }
                }
                if let Some(last) = trait_path.segments.last() {
                    if !(trait_path.segments.len() == 1 && generic_scope.iter().any(|g| g == last))
                    {
                        if let Some(false) = type_vis.get(last).copied() {
                            self.type_error(
                                format!(
                                    "private type '{}' leaks through {} of '{}'; mark the type `pub` or remove it from the public surface",
                                    last, context, owner
                                ),
                                ty.span.clone(),
                                TypeErrorKind::PrivateTypeInPublicSignature,
                            );
                        }
                    }
                }
            }
            // `dyn Trait` slice 5: same private-type-leak rule as
            // `Path` / `ImplTrait` — a private trait name surfacing
            // through a `pub` signature via `dyn Trait` is a leak.
            TypeKind::Dyn {
                trait_path, args, ..
            } => {
                for a in args {
                    if let GenericArg::Type(t) = a {
                        self.check_type_expr_visibility(t, generic_scope, type_vis, context, owner);
                    }
                }
                if let Some(last) = trait_path.segments.last() {
                    if !(trait_path.segments.len() == 1 && generic_scope.iter().any(|g| g == last))
                    {
                        if let Some(false) = type_vis.get(last).copied() {
                            self.type_error(
                                format!(
                                    "private type '{}' leaks through {} of '{}'; mark the type `pub` or remove it from the public surface",
                                    last, context, owner
                                ),
                                ty.span.clone(),
                                TypeErrorKind::PrivateTypeInPublicSignature,
                            );
                        }
                    }
                }
            }
            TypeKind::Unit | TypeKind::Error => {}
        }
    }

    /// Flag non-`pub` types appearing in `pub` signature positions across
    /// functions, methods, extern functions, struct fields, enum variant
    /// payloads, type aliases, and constants. See CR-18.
    pub(super) fn check_signature_visibility(&mut self) {
        let type_vis = self.collect_type_visibility();
        let items = self.program.items.clone();
        for item in &items {
            match item {
                Item::Function(f) if f.is_pub => {
                    let scope = Self::generic_param_names(&f.generic_params);
                    for p in &f.params {
                        self.check_type_expr_visibility(
                            &p.ty,
                            &scope,
                            &type_vis,
                            "parameter",
                            &f.name,
                        );
                    }
                    if let Some(ref rt) = f.return_type {
                        self.check_type_expr_visibility(
                            rt,
                            &scope,
                            &type_vis,
                            "return type",
                            &f.name,
                        );
                    }
                }
                Item::ExternFunction(e) if e.is_pub => {
                    for p in &e.params {
                        self.check_type_expr_visibility(
                            &p.ty,
                            &[],
                            &type_vis,
                            "extern parameter",
                            &e.name,
                        );
                    }
                    if let Some(ref rt) = e.return_type {
                        self.check_type_expr_visibility(
                            rt,
                            &[],
                            &type_vis,
                            "extern return type",
                            &e.name,
                        );
                    }
                }
                Item::ExternBlock(b) => {
                    for it in &b.items {
                        match it {
                            ExternItem::Function(e) if e.is_pub => {
                                for p in &e.params {
                                    self.check_type_expr_visibility(
                                        &p.ty,
                                        &[],
                                        &type_vis,
                                        "extern parameter",
                                        &e.name,
                                    );
                                }
                                if let Some(ref rt) = e.return_type {
                                    self.check_type_expr_visibility(
                                        rt,
                                        &[],
                                        &type_vis,
                                        "extern return type",
                                        &e.name,
                                    );
                                }
                            }
                            ExternItem::Function(_) => {}
                            // Opaque foreign type declarations have no
                            // type-expression surface to visibility-check
                            // — the declaration *is* the type.
                            ExternItem::OpaqueType(_) => {}
                        }
                    }
                }
                Item::StructDef(s) if s.is_pub => {
                    let scope = Self::generic_param_names(&s.generic_params);
                    for f in &s.fields {
                        if f.is_pub {
                            let owner = format!("{}.{}", s.name, f.name);
                            self.check_type_expr_visibility(
                                &f.ty,
                                &scope,
                                &type_vis,
                                "struct field",
                                &owner,
                            );
                        }
                    }
                }
                Item::EnumDef(e) if e.is_pub => {
                    let scope = Self::generic_param_names(&e.generic_params);
                    for v in &e.variants {
                        match &v.kind {
                            VariantKind::Unit => {}
                            VariantKind::Tuple(ts) => {
                                let owner = format!("{}.{}", e.name, v.name);
                                for t in ts {
                                    self.check_type_expr_visibility(
                                        t,
                                        &scope,
                                        &type_vis,
                                        "enum variant payload",
                                        &owner,
                                    );
                                }
                            }
                            VariantKind::Struct(fs) => {
                                for f in fs {
                                    let owner = format!("{}.{}.{}", e.name, v.name, f.name);
                                    self.check_type_expr_visibility(
                                        &f.ty,
                                        &scope,
                                        &type_vis,
                                        "enum variant field",
                                        &owner,
                                    );
                                }
                            }
                        }
                    }
                }
                Item::TypeAlias(t) if t.is_pub => {
                    let scope = Self::generic_param_names(&t.generic_params);
                    self.check_type_expr_visibility(
                        &t.ty,
                        &scope,
                        &type_vis,
                        "type alias",
                        &t.name,
                    );
                }
                Item::DistinctType(d) if d.is_pub => {
                    let scope = Self::generic_param_names(&d.generic_params);
                    self.check_type_expr_visibility(
                        &d.base_type,
                        &scope,
                        &type_vis,
                        "distinct type base",
                        &d.name,
                    );
                }
                Item::ConstDecl(c) if c.is_pub => {
                    self.check_type_expr_visibility(&c.ty, &[], &type_vis, "constant", &c.name);
                }
                Item::ImplBlock(imp) => {
                    let impl_scope = Self::generic_param_names(&imp.generic_params);
                    for ii in &imp.items {
                        if let ImplItem::Method(m) = ii {
                            if m.is_pub {
                                let mut scope = impl_scope.clone();
                                scope.extend(Self::generic_param_names(&m.generic_params));
                                for p in &m.params {
                                    self.check_type_expr_visibility(
                                        &p.ty,
                                        &scope,
                                        &type_vis,
                                        "method parameter",
                                        &m.name,
                                    );
                                }
                                if let Some(ref rt) = m.return_type {
                                    self.check_type_expr_visibility(
                                        rt,
                                        &scope,
                                        &type_vis,
                                        "method return type",
                                        &m.name,
                                    );
                                }
                            }
                        }
                    }
                }
                Item::TraitDef(t) if t.is_pub => {
                    let trait_scope = Self::generic_param_names(&t.generic_params);
                    for ti in &t.items {
                        if let TraitItem::Method(m) = ti {
                            let mut scope = trait_scope.clone();
                            scope.extend(Self::generic_param_names(&m.generic_params));
                            for p in &m.params {
                                self.check_type_expr_visibility(
                                    &p.ty,
                                    &scope,
                                    &type_vis,
                                    "trait method parameter",
                                    &m.name,
                                );
                            }
                            if let Some(ref rt) = m.return_type {
                                self.check_type_expr_visibility(
                                    rt,
                                    &scope,
                                    &type_vis,
                                    "trait method return type",
                                    &m.name,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // ── Phase 6 line 218 slice 2: ScopeLocal escape walker ──────────
    //
    // design.md § ScopeLocal: types implementing the sealed marker
    // trait `ScopeLocal` cannot escape the scope that created them.
    // The typechecker rejects them in three positions:
    //   (a) function return type — any fn, including non-`pub`
    //   (b) struct field type / enum variant payload — any field
    //   (c) channel `Sender.send(arg)` argument (handled in
    //       `src/typechecker/stdlib_io.rs::infer_channel_method` at
    //       the call site where the channel element type is known)
    //
    // The walker mirrors `check_signature_visibility`'s structure but
    // (i) runs regardless of `is_pub` (escape is escape — a private
    // fn returning a TaskHandle is still leaking it past the spawning
    // scope), (ii) keys off a `scope_local_types: HashSet<String>`
    // collected from `impl ScopeLocal for T {}` blocks across both
    // user `program.items` AND every entry in `STDLIB_PROGRAMS` (the
    // baked stdlib's impl blocks don't get spliced into
    // `program.items` — `synthetic_prelude_items` clones the
    // StructDef only, so the walker reaches into STDLIB_PROGRAMS
    // explicitly to pick up `impl[T] ScopeLocal for TaskHandle[T]`
    // from `runtime/stdlib/task_group.kara`). Same precedent as
    // `raii_check::collect_cancel_unsafe_annotations`.

    /// Collect type names with an `impl ScopeLocal for T {}` opt-in,
    /// across the user `program.items` AND the baked stdlib.
    fn collect_scope_local_types(&self) -> HashSet<String> {
        let mut out: HashSet<String> = HashSet::new();
        let mut record = |item: &Item| {
            let Item::ImplBlock(imp) = item else { return };
            let Some(ref trait_path) = imp.trait_name else {
                return;
            };
            if trait_path.segments.last().map(String::as_str) != Some("ScopeLocal") {
                return;
            }
            let TypeKind::Path(ref target_path) = imp.target_type.kind else {
                return;
            };
            if target_path.segments.len() != 1 {
                return;
            }
            out.insert(target_path.segments[0].clone());
        };
        for item in &self.program.items {
            record(item);
        }
        for (_, prog) in crate::prelude::STDLIB_PROGRAMS.iter() {
            for item in &prog.items {
                record(item);
            }
        }
        out
    }

    /// Walk a `TypeExpr` and emit `ScopeLocalEscape` for every
    /// reference to a `ScopeLocal`-marked type. The walker mirrors
    /// `check_type_expr_visibility` — same recursion shape over
    /// `TypeKind::Path` / `Tuple` / `Array` / `Pointer` / `Ref` /
    /// `MutRef` / `MutSlice` / `Weak` / `FnType` / `ImplTrait` /
    /// `Dyn` — but keys off the ScopeLocal type set instead of the
    /// visibility map. Generic-scope identifiers (`T` inside
    /// `fn foo[T](...)`) are passed through unchanged because the
    /// rule applies to the outermost named type, not type
    /// parameters.
    fn check_type_expr_scope_local(
        &mut self,
        ty: &TypeExpr,
        generic_scope: &[String],
        scope_local_types: &HashSet<String>,
        context: &str,
        owner: &str,
    ) {
        match &ty.kind {
            TypeKind::Path(p) => {
                if let Some(ref args) = p.generic_args {
                    for a in args {
                        if let GenericArg::Type(t) = a {
                            self.check_type_expr_scope_local(
                                t,
                                generic_scope,
                                scope_local_types,
                                context,
                                owner,
                            );
                        }
                    }
                }
                let last = match p.segments.last() {
                    Some(s) => s.clone(),
                    None => return,
                };
                if p.segments.len() == 1 && generic_scope.iter().any(|g| g == &last) {
                    return;
                }
                if scope_local_types.contains(&last) {
                    self.type_error(
                        format!(
                            "ScopeLocal type '{}' cannot appear in {} of '{}'; the value is bound \
                             to the scope that created it and cannot escape via return, \
                             field storage, or channel send",
                            last, context, owner
                        ),
                        ty.span.clone(),
                        TypeErrorKind::ScopeLocalEscape,
                    );
                }
            }
            TypeKind::Tuple(ts) => {
                for t in ts {
                    self.check_type_expr_scope_local(
                        t,
                        generic_scope,
                        scope_local_types,
                        context,
                        owner,
                    );
                }
            }
            TypeKind::Array { element, .. } => {
                self.check_type_expr_scope_local(
                    element,
                    generic_scope,
                    scope_local_types,
                    context,
                    owner,
                );
            }
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => {
                self.check_type_expr_scope_local(
                    inner,
                    generic_scope,
                    scope_local_types,
                    context,
                    owner,
                );
            }
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                for p in params {
                    self.check_type_expr_scope_local(
                        p,
                        generic_scope,
                        scope_local_types,
                        context,
                        owner,
                    );
                }
                if let Some(ref rt) = return_type {
                    self.check_type_expr_scope_local(
                        rt,
                        generic_scope,
                        scope_local_types,
                        context,
                        owner,
                    );
                }
            }
            // `impl Trait` / `dyn Trait` carry generic args we still
            // descend into (same shape as the visibility walker).
            TypeKind::ImplTrait { args, .. } | TypeKind::Dyn { args, .. } => {
                for a in args {
                    if let GenericArg::Type(t) = a {
                        self.check_type_expr_scope_local(
                            t,
                            generic_scope,
                            scope_local_types,
                            context,
                            owner,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    /// Walk every function, struct, and enum in the program, emitting
    /// `ScopeLocalEscape` for each return-type / field / payload that
    /// references a `ScopeLocal`-marked type. Runs regardless of
    /// visibility — escape is escape. Channel-send check lives in
    /// `infer_channel_method`'s Sender.send arm where the channel
    /// element type is known.
    pub(super) fn check_scope_local_escape(&mut self) {
        let scope_local_types = self.collect_scope_local_types();
        if scope_local_types.is_empty() {
            return;
        }
        let items = self.program.items.clone();
        for item in &items {
            match item {
                Item::Function(f) => {
                    let scope = Self::generic_param_names(&f.generic_params);
                    if let Some(ref rt) = f.return_type {
                        self.check_type_expr_scope_local(
                            rt,
                            &scope,
                            &scope_local_types,
                            "return type",
                            &f.name,
                        );
                    }
                }
                Item::StructDef(s) => {
                    let scope = Self::generic_param_names(&s.generic_params);
                    for fld in &s.fields {
                        let owner = format!("{}.{}", s.name, fld.name);
                        self.check_type_expr_scope_local(
                            &fld.ty,
                            &scope,
                            &scope_local_types,
                            "struct field",
                            &owner,
                        );
                    }
                }
                Item::EnumDef(e) => {
                    let scope = Self::generic_param_names(&e.generic_params);
                    for v in &e.variants {
                        match &v.kind {
                            VariantKind::Unit => {}
                            VariantKind::Tuple(ts) => {
                                let owner = format!("{}.{}", e.name, v.name);
                                for t in ts {
                                    self.check_type_expr_scope_local(
                                        t,
                                        &scope,
                                        &scope_local_types,
                                        "enum variant payload",
                                        &owner,
                                    );
                                }
                            }
                            VariantKind::Struct(fs) => {
                                for fld in fs {
                                    let owner = format!("{}.{}.{}", e.name, v.name, fld.name);
                                    self.check_type_expr_scope_local(
                                        &fld.ty,
                                        &scope,
                                        &scope_local_types,
                                        "enum variant field",
                                        &owner,
                                    );
                                }
                            }
                        }
                    }
                }
                Item::ImplBlock(imp) => {
                    // Methods inside `impl` blocks — return-type
                    // check fires regardless of visibility. Parameter
                    // types are intentionally NOT checked: passing a
                    // ScopeLocal into a same-scope helper is the
                    // normal usage pattern (e.g.
                    // `tg_spawn_helper(mut tg, conn)`).
                    let impl_scope = Self::generic_param_names(&imp.generic_params);
                    for ii in &imp.items {
                        if let ImplItem::Method(m) = ii {
                            let mut scope = impl_scope.clone();
                            scope.extend(Self::generic_param_names(&m.generic_params));
                            if let Some(ref rt) = m.return_type {
                                self.check_type_expr_scope_local(
                                    rt,
                                    &scope,
                                    &scope_local_types,
                                    "method return type",
                                    &m.name,
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Const-expression evaluator (const generics slice 2). Walks `expr`
    /// against a target `Type`, returning either a resolved `ConstValue`
    /// or a `ConstEvalError`.
    ///
    /// Operand-type propagation: arithmetic / bitwise / shift ops propagate
    /// `target_ty` to both operands (recursing with the same target ensures
    /// `2 + 3` against `target_ty=i8` walks both literals as i8). Comparison
    /// and logical ops infer their operand target type from the operand
    /// expressions themselves (a comparison's result is `Bool`; the operand
    /// type comes from their literal suffixes / `ConstDecl` types). For
    /// comparisons, both sides must produce `ConstValue` variants from the
    /// same comparable family (int/int, bool/bool, char/char,
    /// enum-variant/enum-variant from the same enum).
    ///
    /// Identifier resolution at slice 2: tries `ConstDecl` lookup in
    /// `program.items`. Const-generic parameters via slice 1's `SubstValue`
    /// substitution context are not threaded here yet (slice 3 wires the
    /// inference solver to pass `SubstValue` through to the evaluator).
    pub(crate) fn eval_const_expr(
        &mut self,
        expr: &Expr,
        target_ty: &Type,
    ) -> Result<crate::prelude::ConstValue, ConstEvalError> {
        self.eval_const_expr_with_chain(expr, target_ty, &mut Vec::new())
    }

    fn eval_const_expr_with_chain(
        &mut self,
        expr: &Expr,
        target_ty: &Type,
        chain: &mut Vec<String>,
    ) -> Result<crate::prelude::ConstValue, ConstEvalError> {
        use crate::prelude::ConstValue;
        match &expr.kind {
            ExprKind::Integer(n, sfx) => {
                let ty = match sfx {
                    Some(IntSuffix::I8) => Type::Int(IntSize::I8),
                    Some(IntSuffix::I16) => Type::Int(IntSize::I16),
                    Some(IntSuffix::I32) => Type::Int(IntSize::I32),
                    Some(IntSuffix::I64) => Type::Int(IntSize::I64),
                    Some(IntSuffix::U8) => Type::UInt(UIntSize::U8),
                    Some(IntSuffix::U16) => Type::UInt(UIntSize::U16),
                    Some(IntSuffix::U32) => Type::UInt(UIntSize::U32),
                    Some(IntSuffix::U64) => Type::UInt(UIntSize::U64),
                    Some(IntSuffix::I128) => Type::Int(IntSize::I128),
                    Some(IntSuffix::U128) => Type::UInt(UIntSize::U128),
                    None => {
                        if matches!(target_ty, Type::Int(_) | Type::UInt(_)) {
                            target_ty.clone()
                        } else {
                            Type::Int(IntSize::I64)
                        }
                    }
                };
                integer_to_const_value(*n, &ty, &expr.span)
            }
            ExprKind::Bool(b) => Ok(ConstValue::Bool(*b)),
            ExprKind::CharLit(c) => Ok(ConstValue::Char(*c)),
            ExprKind::ByteLit(b) => Ok(ConstValue::U8(*b)),
            ExprKind::Identifier(name) => {
                if chain.iter().any(|n| n == name) {
                    let mut chain_with_self = chain.clone();
                    chain_with_self.push(name.clone());
                    return Err(ConstEvalError::CyclicConstDef {
                        chain: chain_with_self,
                        span: expr.span.clone(),
                    });
                }
                for item in &self.program.items {
                    if let Item::ConstDecl(c) = item {
                        if c.name == *name {
                            // Evaluate the const's value against the
                            // surrounding context's target type rather
                            // than the const's own declared type so
                            // `const TEN: i64 = 10` used in an Array
                            // size position (target = usize) flows as
                            // Usize(10), not I64(10) — preventing a
                            // spurious cross-width mismatch in the
                            // surrounding binary op (`TEN + 1`). The
                            // const's declared-type vs use-site
                            // compatibility is enforced by the regular
                            // typechecker elsewhere; here we just
                            // produce the value at the use site's
                            // width.
                            chain.push(name.clone());
                            let res = self.eval_const_expr_with_chain(&c.value, target_ty, chain);
                            chain.pop();
                            return res;
                        }
                    }
                }
                Err(ConstEvalError::UndefinedConst {
                    name: name.clone(),
                    span: expr.span.clone(),
                })
            }
            ExprKind::Path { segments, .. } if segments.len() == 2 => {
                let enum_name = &segments[0];
                let variant_name = &segments[1];
                if let Some(info) = self.env.enums.get(enum_name) {
                    for (discriminant, (vname, vkind)) in info.variants.iter().enumerate() {
                        if vname == variant_name {
                            if !matches!(vkind, VariantTypeInfo::Unit) {
                                return Err(ConstEvalError::NonConstShape(expr.span.clone()));
                            }
                            return Ok(ConstValue::EnumVariant {
                                enum_name: enum_name.clone(),
                                variant_name: variant_name.clone(),
                                discriminant: discriminant as i64,
                            });
                        }
                    }
                }
                Err(ConstEvalError::UndefinedConst {
                    name: format!("{}.{}", enum_name, variant_name),
                    span: expr.span.clone(),
                })
            }
            ExprKind::Unary { op, operand } => {
                let val = self.eval_const_expr_with_chain(operand, target_ty, chain)?;
                apply_unary(op.clone(), val, &expr.span)
            }
            ExprKind::Binary { op, left, right } => {
                let operand_target = match op {
                    BinOp::And | BinOp::Or => Type::Bool,
                    BinOp::Eq
                    | BinOp::NotEq
                    | BinOp::Lt
                    | BinOp::LtEq
                    | BinOp::Gt
                    | BinOp::GtEq => {
                        infer_operand_target_ty(left, right).unwrap_or(Type::Int(IntSize::I64))
                    }
                    _ => target_ty.clone(),
                };
                // Short-circuit at evaluator level for And/Or so
                // `false && (1 / 0 == 1)` doesn't fire DivByZero.
                if matches!(op, BinOp::And | BinOp::Or) {
                    let lhs = self.eval_const_expr_with_chain(left, &operand_target, chain)?;
                    match (&lhs, op) {
                        (ConstValue::Bool(false), BinOp::And) => {
                            return Ok(ConstValue::Bool(false))
                        }
                        (ConstValue::Bool(true), BinOp::Or) => return Ok(ConstValue::Bool(true)),
                        (ConstValue::Bool(_), _) => {}
                        _ => {
                            return Err(ConstEvalError::LogicalOnNonBool {
                                ty: const_value_type(&lhs),
                                op: op.clone(),
                                span: left.span.clone(),
                            });
                        }
                    }
                    let rhs = self.eval_const_expr_with_chain(right, &operand_target, chain)?;
                    return apply_binary(op.clone(), lhs, rhs, &expr.span);
                }
                let lhs = self.eval_const_expr_with_chain(left, &operand_target, chain)?;
                let rhs = self.eval_const_expr_with_chain(right, &operand_target, chain)?;
                apply_binary(op.clone(), lhs, rhs, &expr.span)
            }
            _ => Err(ConstEvalError::NonConstShape(expr.span.clone())),
        }
    }

    /// Returns true if `expr` contains a bare `Identifier` node with exactly
    /// `name`. Used to detect cross-parameter references in default values.
    pub(super) fn expr_references_name(expr: &Expr, name: &str) -> bool {
        match &expr.kind {
            ExprKind::Identifier(n) => n == name,
            ExprKind::Unary { operand: inner, .. } => Self::expr_references_name(inner, name),
            ExprKind::Binary { left, right, .. } => {
                Self::expr_references_name(left, name) || Self::expr_references_name(right, name)
            }
            ExprKind::Tuple(elems) | ExprKind::ArrayLiteral(elems) => {
                elems.iter().any(|e| Self::expr_references_name(e, name))
            }
            _ => false,
        }
    }

    /// Per-position "is this param a borrow slot?" lookup for the
    /// `Call` arm of the once-callability walker. Returns
    /// `Some(Vec<bool>)` where each `true` means "borrow position
    /// (`ref T` / `mut ref T` / `mut Slice[T]`), so the arg is read,
    /// not consumed". `None` when the callee's signature is unknown
    /// (function-pointer call, type-param method, builtin without an
    /// `env.functions` entry) — the caller falls back to per-arg
    /// defaults (Consuming). Mirrors `ownership::param_modes_from_signature`
    /// without depending on it, by reading directly from `self.env`.
    pub(super) fn callee_borrow_positions(&self, callee: &Expr) -> Option<Vec<bool>> {
        let key = match &callee.kind {
            ExprKind::Identifier(name) => name.clone(),
            ExprKind::Path { segments, .. } => segments.join("."),
            _ => return None,
        };
        if let Some(sig) = self.env.functions.get(&key) {
            return Some(sig.params.iter().map(Self::is_borrow_param_type).collect());
        }
        if let Some((target, method)) = key.split_once('.') {
            for imp in &self.env.impls {
                // No call-site args context here — borrow-position lookup
                // works off the syntactic `Type.method` key. Conservative
                // post-Theme-4: only generic-on-name impls participate;
                // specialized impls would need an args-aware lookup that
                // this site doesn't carry. Slice-scope deviation (no
                // currently-realistic specialized-impl case for borrow
                // positions).
                if imp.target_type == target && imp.target_args.is_empty() {
                    if let Some(sig) = imp.methods.get(method) {
                        return Some(sig.params.iter().map(Self::is_borrow_param_type).collect());
                    }
                }
            }
        }
        None
    }

    fn is_borrow_param_type(t: &Type) -> bool {
        matches!(
            t,
            Type::Ref(_) | Type::MutRef(_) | Type::Slice { mutable: true, .. }
        )
    }

    /// Whether `ty` is the unit type for entry-point purposes — the `()`
    /// spelling (`Type::Unit`) or the bare `Unit` identifier spelling
    /// (`Type::Named { name: "Unit" }`), per the Slice C note that "`Unit`
    /// spelled `Unit` must count as `()`".
    fn is_entry_unit(ty: &Type) -> bool {
        matches!(ty, Type::Unit)
            || matches!(ty, Type::Named { name, args } if name == "Unit" && args.is_empty())
    }

    /// Phase-8 entry-point contract Slice C (design.md § Entry Point): the
    /// program's `main()` must return exactly one of `()` / `Unit`,
    /// `Result[(), E]` with `E: Display`, or `ExitCode`. Emits
    /// `E_MAIN_RETURN_TYPE` for any other shape and `E_MAIN_ERR_NOT_DISPLAY`
    /// for a conforming `Result` whose error type lacks `Display` (the
    /// runtime renders `Err(e)` as `Error: {e}` via that `Display`).
    fn check_main_entry_return_type(&mut self, return_type: &Type, f: &Function) {
        // Anchor diagnostics at the return-type annotation when present,
        // else at the function signature.
        let span = f
            .return_type
            .as_ref()
            .map(|t| t.span.clone())
            .unwrap_or_else(|| f.span.clone());

        // A return type that already carries an error elsewhere (e.g. an
        // undefined error-type name the resolver already flagged) must not
        // pile a second diagnostic on the same signature.
        if matches!(return_type, Type::Error) {
            return;
        }

        // `()` / `Unit`.
        if Self::is_entry_unit(return_type) {
            return;
        }
        // `ExitCode`.
        if matches!(return_type, Type::Named { name, args } if name == "ExitCode" && args.is_empty())
        {
            return;
        }
        // `Result[(), E]` — Ok arm must be unit; E must be `Display`.
        if let Type::Named { name, args } = return_type {
            if name == "Result" && args.len() == 2 && Self::is_entry_unit(&args[0]) {
                let err_ty = &args[1];
                // Don't double-report when the error type is itself an
                // already-flagged error.
                if !matches!(err_ty, Type::Error) && !self.type_supports_display(err_ty) {
                    let err_name = match err_ty {
                        Type::Named { name, .. } => name.clone(),
                        other => format!("{other:?}"),
                    };
                    self.type_error(
                        format!(
                            "error[E_MAIN_ERR_NOT_DISPLAY]: `main()` returns \
                             `Result[(), {err_name}]`, but `{err_name}` does not \
                             implement `Display` — the runtime prints a returned \
                             `Err(e)` as `Error: {{e}}` using its `Display` impl. \
                             help: add `#[derive(Display)]` to `{err_name}` (or \
                             write `impl Display for {err_name}`)"
                        ),
                        span,
                        TypeErrorKind::MainErrNotDisplay,
                    );
                }
                return;
            }
        }

        // Anything else is not a legal entry-point return type.
        self.type_error(
            "error[E_MAIN_RETURN_TYPE]: `main()` must return `()`, \
             `Result[(), E]` (with `E: Display`), or `ExitCode` \
             (design.md § Entry Point)"
                .to_string(),
            span,
            TypeErrorKind::MainReturnType,
        );
    }

    /// Is `t` the abstract `Self` type, in either spelling it can take after
    /// lowering — `TypeParam("Self")` (when "Self" is an in-scope generic
    /// name) or the bare `Named { name: "Self", args: [] }` a `Self` type
    /// annotation lowers to?
    pub(super) fn is_self_type(t: &Type) -> bool {
        matches!(t, Type::TypeParam(n) if n == "Self")
            || matches!(t, Type::Named { name, args } if name == "Self" && args.is_empty())
    }

    /// Replace every `Self` leaf in `ty` with the concrete impl-target type
    /// `concrete`, recursing through the compound-type forms that can appear
    /// in a return position (`Option[Self]`, `Vec[Self]`, `(Self, Self)`,
    /// `ref Self`, `fn() -> Self`, …). Used to resolve a method's `-> Self`
    /// return type against the body's concrete tail expression. Exotic
    /// nestings not enumerated here pass through unchanged — the transform
    /// only ever *adds* resolution, so an unhandled shape degrades to the
    /// prior "leave `Self` abstract" behavior rather than miscompiling.
    pub(super) fn resolve_self_in_type(ty: Type, concrete: &Type) -> Type {
        if Self::is_self_type(&ty) {
            return concrete.clone();
        }
        let rec = |t: Type| Self::resolve_self_in_type(t, concrete);
        match ty {
            Type::Named { name, args } => Type::Named {
                name,
                args: args.into_iter().map(rec).collect(),
            },
            Type::Tuple(elems) => Type::Tuple(elems.into_iter().map(rec).collect()),
            Type::Slice { element, mutable } => Type::Slice {
                element: Box::new(rec(*element)),
                mutable,
            },
            Type::Array { element, size } => Type::Array {
                element: Box::new(rec(*element)),
                size,
            },
            Type::Ref(inner) => Type::Ref(Box::new(rec(*inner))),
            Type::MutRef(inner) => Type::MutRef(Box::new(rec(*inner))),
            Type::Weak(inner) => Type::Weak(Box::new(rec(*inner))),
            Type::Rc(inner) => Type::Rc(Box::new(rec(*inner))),
            Type::Arc(inner) => Type::Arc(Box::new(rec(*inner))),
            Type::Pointer { is_mut, inner } => Type::Pointer {
                is_mut,
                inner: Box::new(rec(*inner)),
            },
            Type::Function {
                params,
                return_type,
            } => Type::Function {
                params: params.into_iter().map(rec).collect(),
                return_type: Box::new(rec(*return_type)),
            },
            Type::OnceFunction {
                params,
                return_type,
            } => Type::OnceFunction {
                params: params.into_iter().map(rec).collect(),
                return_type: Box::new(rec(*return_type)),
            },
            other => other,
        }
    }

    /// Collect the names in `generics` that appear in a SHAPE position
    /// (`[D]` / `[...S]`) anywhere inside `ty` — the dim params a body-level
    /// annotation must resolve (B-2026-07-13-5 leg B). Recursive walk over the
    /// annotation-nesting `TypeKind`s; `GenericArg::Shape` dims are the only
    /// shape carriers. A type param is never collected (it sits in a
    /// `GenericArg::Type` position, not a shape one), so its `Named` body
    /// spelling is preserved and the Named-vs-TypeParam trap is avoided.
    fn collect_shape_dim_generic_names(ty: &TypeExpr, generics: &[String], out: &mut Vec<String>) {
        fn note(name: &str, generics: &[String], out: &mut Vec<String>) {
            if generics.iter().any(|g| g == name) && !out.iter().any(|o| o == name) {
                out.push(name.to_string());
            }
        }
        if let TypeKind::Path(p) = &ty.kind {
            if let Some(args) = &p.generic_args {
                for arg in args {
                    match arg {
                        GenericArg::Type(t) => {
                            Self::collect_shape_dim_generic_names(t, generics, out)
                        }
                        GenericArg::Shape(shape) => {
                            for dim in &shape.dims {
                                match dim {
                                    ShapeDim::Const(e) => {
                                        if let ExprKind::Identifier(n) = &e.kind {
                                            note(n, generics, out);
                                        }
                                    }
                                    ShapeDim::Splice { name, .. } => note(name, generics, out),
                                    ShapeDim::Dynamic { .. } => {}
                                }
                            }
                        }
                        GenericArg::Const(_) => {}
                    }
                }
            }
        }
        match &ty.kind {
            TypeKind::Tuple(ts) => {
                for t in ts {
                    Self::collect_shape_dim_generic_names(t, generics, out);
                }
            }
            TypeKind::Array { element, .. }
            | TypeKind::Pointer { inner: element, .. }
            | TypeKind::Ref(element)
            | TypeKind::MutRef(element)
            | TypeKind::MutSlice(element)
            | TypeKind::Weak(element) => {
                Self::collect_shape_dim_generic_names(element, generics, out)
            }
            _ => {}
        }
    }

    fn check_function(
        &mut self,
        f: &Function,
        self_type: Option<&Type>,
        enclosing_generics: &[String],
    ) {
        self.local_scope = LocalTypeScope::new();

        // Variance markers are a property of (stdlib) nominal type
        // declarations — never legal on function generic params
        // (design.md § Variance).
        self.reject_user_variance_markers(&f.generic_params, false);

        let mut gp = enclosing_generics.to_vec();
        gp.extend(Self::generic_param_names(&f.generic_params));

        // Save outer bounds, merge in function-level bounds. Restored after
        // the body is checked so sibling functions don't see this fn's
        // generics. `merge` semantics: function-level entries shadow outer
        // entries with the same name (innermost wins, mirroring scope).
        let saved_bounds = self.enclosing_bounds.clone();
        for (name, bounds) in Self::collect_param_bounds(&f.generic_params, &f.where_clause) {
            self.enclosing_bounds.insert(name, bounds);
        }

        // Collect the DIM params used in shape positions in the signature (and
        // any `const` / `...S` params) so body-level annotations can resolve
        // them — `let p: Tensor[f32, [D]]` (B-2026-07-13-5 leg B). Type params
        // are excluded (they keep the `Named` body spelling). Saved/restored
        // around the body like `enclosing_bounds`.
        let mut body_dim_scope: Vec<String> = Vec::new();
        if let Some(gps) = &f.generic_params {
            for p in &gps.params {
                if (p.is_const || p.is_variadic_shape) && gp.iter().any(|g| g == &p.name) {
                    body_dim_scope.push(p.name.clone());
                }
            }
        }
        for param in &f.params {
            Self::collect_shape_dim_generic_names(&param.ty, &gp, &mut body_dim_scope);
        }
        if let Some(ret) = &f.return_type {
            Self::collect_shape_dim_generic_names(ret, &gp, &mut body_dim_scope);
        }
        let saved_body_dim_scope =
            std::mem::replace(&mut self.current_body_dim_scope, body_dim_scope);

        // Validate default parameter values
        self.validate_default_params(&f.params, &gp);

        // Validate and bind parameters
        for param in &f.params {
            let ty = self.lower_type_expr(&param.ty, &gp);
            // `E_TYPE_VALUE_AT_RUNTIME` (substrate 2): a `Type` value is
            // first-class only at comptime, so a runtime function may not
            // declare a `Type` parameter. Legal only when the parameter is
            // `comptime`-prefixed (`comptime T: Type`) or the whole function
            // is a `comptime fn`. Spec: deferred.md § Comptime — Types as
            // first-class values.
            if !param.is_comptime
                && !f.is_comptime
                && matches!(&ty, Type::Named { name, args } if name == "Type" && args.is_empty())
            {
                self.type_error(
                    "error[E_TYPE_VALUE_AT_RUNTIME]: a runtime function may not take a `Type` \
                     parameter; `Type` values are first-class only at compile time — mark the \
                     parameter `comptime` or the function `comptime fn` (deferred.md § Comptime \
                     — Types as first-class values)"
                        .to_string(),
                    param.ty.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
            self.check_param_irrefutable(param, &ty);
            self.bind_pattern_types(&param.pattern, &ty);
        }

        // Validate inline bounds and where clause (merged — both apply)
        self.validate_all_bounds(&f.generic_params, &f.where_clause, &gp);

        // Bind self
        if f.self_param.is_some() {
            if let Some(st) = self_type {
                self.local_scope.insert("self".to_string(), st.clone());
                self.current_self_type = Some(st.clone());
            }
        }

        let mut return_type = f
            .return_type
            .as_ref()
            .map(|t| self.lower_type_expr(t, &gp))
            .unwrap_or(Type::Unit);
        // Resolve `Self` in the declared return type to the concrete impl
        // target so the body's tail expression (which has the concrete type,
        // e.g. `W` / `u8`) checks against `-> Self`. Inside a concrete `impl`,
        // `Self` names the target type; `lower_type_expr` leaves the annotation
        // as `Type::Named { name: "Self" }` (and, when "Self" is an in-scope
        // generic name, `Type::TypeParam("Self")`), neither of which the body's
        // concrete tail type is assignable to — so `fn m(self) -> Self { W { .. } }`
        // otherwise fails with "expected 'Self', found 'W'". For a trait's
        // *default* body the passed `self_type` is itself the abstract
        // `TypeParam("Self")`, so we leave `Self` abstract there.
        if let Some(st) = self_type {
            if !Self::is_self_type(st) {
                return_type = Self::resolve_self_in_type(return_type, st);
            }
        }
        self.current_return_type = Some(return_type.clone());

        // Return-position `impl Trait` single-witness pinning (design.md §
        // `impl Trait`: "one concrete return per monomorphization"). Arm the
        // per-function witness collector when the declared return is an
        // existential; the end-of-body `check_impl_trait_single_witness`
        // rejects a body that yields two or more distinct concrete witnesses.
        // Saved/restored so a nested item (impl method, trait default body)
        // doesn't leak its parent's collector.
        let saved_impl_trait = self.current_return_impl_trait.take();
        let saved_impl_trait_witnesses = std::mem::take(&mut self.return_impl_trait_witnesses);
        self.current_return_impl_trait = match &return_type {
            Type::Existential {
                origin, trait_name, ..
            } => Some((*origin, trait_name.clone())),
            _ => None,
        };

        // FE-2 — `GpuSafe` structural check. A `#[gpu]` function may use
        // only the GPU-compatible type subset; reject heap / RC types (and
        // aggregates containing them) in its parameter and return types.
        // Local-binding types are checked from the body walk below. See
        // `gpu_safe.rs` and design.md § GPU Subset Constraints.
        if f.is_gpu {
            self.check_gpu_safe_signature(f, &gp);
        }

        // Phase-8 entry-point contract Slice C: the top-level `fn main`
        // must return exactly `()` / `Unit`, `Result[(), E: Display]`, or
        // `ExitCode` (design.md § Entry Point). Gated on a free function
        // named `main` — `self_type` / `self_param` exclude methods named
        // `main`, which are not entry points.
        if f.name == "main" && self_type.is_none() && f.self_param.is_none() {
            self.check_main_entry_return_type(&return_type, f);
        }

        // Contract clauses (design.md § Contracts): `requires` / `ensures`
        // predicates must be `bool`; an `ensures(result) …` binding types
        // `result` as the function's return type. Checked with the params in
        // scope (`requires` references params; `ensures` references `result`
        // and non-consumed params).
        let self_consumed = matches!(f.self_param, Some(SelfParam::Owned));
        self.check_contract_clauses(&f.requires, &f.ensures, &return_type, self_consumed);

        // `#[non_exhaustive]` slice 4 — track the current function's
        // origin so struct-literal sites can detect the cross-package
        // case (stdlib-defined non-exhaustive struct constructed from
        // user-origin code). Save / restore so nested item walks
        // (impl methods, trait default bodies) propagate the inner
        // function's origin while their bodies are checked.
        let saved_fn_stdlib_origin = self.current_fn_stdlib_origin;
        self.current_fn_stdlib_origin = f.stdlib_origin;

        // Lint-level slice 4b — push this function's lint overrides
        // as the innermost cascade frame. Popped on exit so sibling
        // / outer items don't inherit it.
        self.lint_override_stack.push(f.lint_overrides.clone());

        // `impl Trait` slice 4 — compute the capture set for every
        // return-position existential declared in this function's
        // signature. Done after lowering so we know which `impl Trait`
        // occurrences actually survived to the typed level; the AST
        // walk inspects the source `TypeExpr` directly because the
        // lowered `Type::Existential` carries only the trait surface,
        // not the structural shape needed to apply the elision rule.
        if let Some(ref ret_ty) = f.return_type {
            self.record_impl_trait_captures(ret_ty, f, &gp);
        }

        // Type-check body — thread the expected return type through so that
        // a `.into()` in tail position can resolve against it.
        //
        // A `comptime fn` body is a comptime context (substrate 2): `Type`
        // pseudovalues and the reflection API are legal inside it, so bump
        // `comptime_depth` for the body check.
        let comptime_fn = f.is_comptime;
        if comptime_fn {
            self.comptime_depth += 1;
        }
        // FE-3c — flag the body as a `#[gpu]` context so the closure-capture
        // hook rejects host-capturing closures. Saved/restored so a non-gpu
        // sibling item is unaffected.
        let saved_fn_is_gpu = self.current_fn_is_gpu;
        self.current_fn_is_gpu = f.is_gpu;
        if f.body.final_expr.is_some() {
            self.check_block_against(&f.body, &return_type);
        } else {
            self.infer_block(&f.body);
        }
        self.current_fn_is_gpu = saved_fn_is_gpu;
        if comptime_fn {
            self.comptime_depth -= 1;
        }

        // FE-2b — `GpuSafe` local-binding check. Now that the body is checked
        // (so `expr_types` carries each binding's value type), reject any
        // `let` binding in a `#[gpu]` function whose type is GPU-incompatible.
        // Complements the FE-2 signature check above and FE-4's effect gate.
        if f.is_gpu {
            self.check_gpu_safe_bindings(&f.body, &gp);
        }

        // Reject a return-position `impl Trait` whose body yielded 2+ distinct
        // concrete witnesses (must run while `current_return_impl_trait` is
        // still armed, before the restore below).
        self.check_impl_trait_single_witness();
        self.current_return_impl_trait = saved_impl_trait;
        self.return_impl_trait_witnesses = saved_impl_trait_witnesses;

        self.current_return_type = None;
        self.current_self_type = None;
        self.enclosing_bounds = saved_bounds;
        self.current_body_dim_scope = saved_body_dim_scope;
        self.current_fn_stdlib_origin = saved_fn_stdlib_origin;
        self.lint_override_stack.pop();
    }

    // ── `#[repr(transparent)]` carrier-shape validation ──────────────────
    // design.md § `#[repr(transparent)]` for distinct-type FFI. The wrapper
    // must be a single-data-field newtype so its ABI shape IS its inner
    // field's. Each rule is a focused `E_REPR_TRANSPARENT_*` diagnostic.

    /// Shared prologue: `true` iff `#[repr(transparent)]` is active, emitting
    /// `E_REPR_TRANSPARENT_EXCLUSIVE` when it is combined with any other repr.
    /// When it returns `false`, the carrier-specific checks are skipped.
    fn repr_transparent_active(&mut self, attributes: &[Attribute], span: &Span) -> bool {
        let names = super::repr_arg_head_names(attributes);
        if !names.iter().any(|n| n == "transparent") {
            return false;
        }
        if names.iter().any(|n| n != "transparent") {
            self.type_error(
                "error[E_REPR_TRANSPARENT_EXCLUSIVE]: `#[repr(transparent)]` cannot be \
                 combined with any other `#[repr(...)]` (`C` / `packed` / `align(N)` / \
                 `intN`) — `transparent` IS the layout claim, so another repr would \
                 either contradict it or duplicate the inner type's claim"
                    .to_string(),
                span.clone(),
                TypeErrorKind::ReprTransparentInvalid,
            );
        }
        true
    }

    /// A zero-sized, alignment-one companion field permitted next to the single
    /// data field of a `transparent` struct (design.md: `PhantomData[T]`, `()`,
    /// `[T; 0]`, an empty unit struct). Structural for the first three; the
    /// empty-unit-struct case consults `struct_info` for a zero-field struct.
    fn is_zero_sized_companion(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            // `()` — the unit type (parsed as `TypeKind::Unit`; a zero-element
            // tuple also collapses here defensively).
            TypeKind::Unit => true,
            TypeKind::Tuple(elems) => elems.is_empty(),
            // A zero-length array `[T; 0]` (when represented as `TypeKind::Array`).
            TypeKind::Array { size, .. } => {
                matches!(&size.kind, ExprKind::Integer(0, _))
            }
            TypeKind::Path(p) => {
                let head = p.segments.last().map(|s| s.as_str()).unwrap_or("");
                // `PhantomData[T]` carries no bytes by convention.
                if head == "PhantomData" {
                    return true;
                }
                // An empty unit struct (`struct Empty {}`) — zero fields.
                self.env
                    .structs
                    .get(head)
                    .map(|info| info.fields.is_empty())
                    .unwrap_or(false)
            }
            _ => false,
        }
    }

    /// `true` iff `ty` is a bare generic type parameter of the carrier (a
    /// single-segment path naming a `gp` entry, no generic args). Such an inner
    /// makes the wrapper's size depend on `T`; without a `Sized` bound (which
    /// Kāra has no surface for) the layout is indeterminate — the
    /// `E_REPR_TRANSPARENT_REQUIRES_SIZED` case.
    fn is_bare_type_param(ty: &TypeExpr, gp: &[String]) -> bool {
        if let TypeKind::Path(p) = &ty.kind {
            return p.generic_args.is_none()
                && p.segments.len() == 1
                && gp.iter().any(|g| g == &p.segments[0]);
        }
        false
    }

    fn check_repr_transparent_struct(&mut self, s: &StructDef, gp: &[String]) {
        if !self.repr_transparent_active(&s.attributes, &s.span) {
            return;
        }
        let data_fields: Vec<&StructField> = s
            .fields
            .iter()
            .filter(|f| !self.is_zero_sized_companion(&f.ty))
            .collect();
        match data_fields.len() {
            0 => self.type_error(
                "error[E_REPR_TRANSPARENT_REQUIRES_FIELD]: a `#[repr(transparent)]` \
                 struct must have exactly one non-zero-sized field whose layout becomes \
                 the whole struct's layout; this struct has none"
                    .to_string(),
                s.span.clone(),
                TypeErrorKind::ReprTransparentInvalid,
            ),
            1 => {
                if Self::is_bare_type_param(&data_fields[0].ty, gp) {
                    self.type_error(
                        format!(
                            "error[E_REPR_TRANSPARENT_REQUIRES_SIZED]: the single field of a \
                             `#[repr(transparent)]` struct is the bare type parameter \
                             '{}', so the wrapper's size depends on it and its layout is \
                             indeterminate; wrap a concrete or `Sized`-known type instead",
                            data_fields[0].name
                        ),
                        data_fields[0].span.clone(),
                        TypeErrorKind::ReprTransparentInvalid,
                    );
                }
            }
            _ => self.type_error(
                "error[E_REPR_TRANSPARENT_REQUIRES_SINGLE_FIELD]: a `#[repr(transparent)]` \
                 struct may have at most one non-zero-sized field (the rest must be \
                 zero-sized, alignment-one companions like `PhantomData[T]` / `()` / \
                 `[T; 0]`); this struct has more than one data field"
                    .to_string(),
                s.span.clone(),
                TypeErrorKind::ReprTransparentInvalid,
            ),
        }
    }

    fn check_repr_transparent_enum(&mut self, e: &EnumDef) {
        if !self.repr_transparent_active(&e.attributes, &e.span) {
            return;
        }
        // Exactly one variant carrying exactly one field.
        let ok = e.variants.len() == 1
            && match &e.variants[0].kind {
                VariantKind::Tuple(tys) => tys.len() == 1,
                VariantKind::Struct(fields) => fields.len() == 1,
                VariantKind::Unit => false,
            };
        if !ok {
            self.type_error(
                "error[E_REPR_TRANSPARENT_ENUM_NOT_SINGLE_VARIANT]: a `#[repr(transparent)]` \
                 enum must have exactly one variant carrying exactly one field (the \
                 discriminant-position marker with no runtime tag); multi-variant or \
                 payload-less enums are rejected"
                    .to_string(),
                e.span.clone(),
                TypeErrorKind::ReprTransparentInvalid,
            );
        }
    }

    /// The inclusive `[min, max]` value range of a `#[repr(intN)]` head name,
    /// or `None` when `name` is not an integer repr. `u64`'s true upper bound
    /// exceeds `i64::MAX`; since discriminants are folded to `i64`, its range is
    /// capped at `i64::MAX` (a `u64` literal that big cannot survive the `i64`
    /// literal parse anyway).
    fn int_repr_range(name: &str) -> Option<(i64, i64)> {
        Some(match name {
            "i8" => (i8::MIN as i64, i8::MAX as i64),
            "i16" => (i16::MIN as i64, i16::MAX as i64),
            "i32" => (i32::MIN as i64, i32::MAX as i64),
            "i64" => (i64::MIN, i64::MAX),
            "u8" => (0, u8::MAX as i64),
            "u16" => (0, u16::MAX as i64),
            "u32" => (0, u32::MAX as i64),
            "u64" => (0, i64::MAX),
            _ => return None,
        })
    }

    /// Fold an explicit-discriminant expression to an `i64` — the v1 const
    /// surface: integer literals, unary negation, `+`/`-`/`*`/`/`/`%` + bitwise
    /// / shift arithmetic over them, and **references to module-level integer
    /// constants** (an immutable `let` binding or a `const` decl, resolved
    /// through `consts`). Overflow, a non-integer reference, an unknown name, or
    /// a reference cycle yields `None` (reported as `E_NON_CONSTANT_DISCRIMINANT`
    /// at the call site). `visiting` is the in-progress reference chain, so a
    /// cyclic `let A = B; let B = A` alias folds to `None` rather than looping.
    fn fold_int_const<'e>(
        expr: &'e Expr,
        consts: &HashMap<&'e str, &'e Expr>,
        visiting: &mut Vec<&'e str>,
    ) -> Option<i64> {
        match &expr.kind {
            ExprKind::Integer(v, _) => Some(*v),
            ExprKind::Unary {
                op: UnaryOp::Neg,
                operand,
            } => Self::fold_int_const(operand, consts, visiting)?.checked_neg(),
            ExprKind::Binary { op, left, right } => {
                let l = Self::fold_int_const(left, consts, visiting)?;
                let r = Self::fold_int_const(right, consts, visiting)?;
                match op {
                    BinOp::Add => l.checked_add(r),
                    BinOp::Sub => l.checked_sub(r),
                    BinOp::Mul => l.checked_mul(r),
                    BinOp::Div => l.checked_div(r),
                    BinOp::Mod => l.checked_rem(r),
                    BinOp::BitAnd => Some(l & r),
                    BinOp::BitOr => Some(l | r),
                    BinOp::BitXor => Some(l ^ r),
                    BinOp::Shl => u32::try_from(r).ok().and_then(|s| l.checked_shl(s)),
                    BinOp::Shr => u32::try_from(r).ok().and_then(|s| l.checked_shr(s)),
                    _ => None,
                }
            }
            // A bare reference to a module-level constant — `= SOME_CONST`. It
            // parses as an `Identifier` or a single-segment `Path`. Resolve it
            // through `consts` and fold the referent (which may itself reference
            // another constant, so recurse under a cycle guard).
            ExprKind::Identifier(name) => Self::fold_const_ref(name, consts, visiting),
            ExprKind::Path {
                segments,
                generic_args: None,
            } if segments.len() == 1 => Self::fold_const_ref(&segments[0], consts, visiting),
            _ => None,
        }
    }

    /// Resolve a module-constant name to its folded `i64` value, guarding
    /// against a reference cycle (a name already on the `visiting` chain folds
    /// to `None`). Helper for [`Self::fold_int_const`].
    fn fold_const_ref<'e>(
        name: &'e str,
        consts: &HashMap<&'e str, &'e Expr>,
        visiting: &mut Vec<&'e str>,
    ) -> Option<i64> {
        let value = consts.get(name)?;
        if visiting.contains(&name) {
            return None; // cyclic alias — treat as non-constant
        }
        visiting.push(name);
        let folded = Self::fold_int_const(value, consts, visiting);
        visiting.pop();
        folded
    }

    /// Explicit discriminants on enum variants (design.md § Explicit
    /// Discriminants on Payload Variants). After folding each `= CONST_EXPR` to
    /// an `i64`, run four checks at the enum-decl site: (a) all-or-nothing, (b)
    /// range per `#[repr(intN)]` / `c_int`, (c) duplicate values, (d) `#[repr]`
    /// requirement on payload variants. The values are pure *declarations* —
    /// codegen does not treat them as layout commitments at v1. All findings use
    /// `E0804` (`DiscriminantInvalid`), disambiguated by the symbolic code in
    /// the message.
    fn check_enum_discriminants(&mut self, e: &EnumDef) {
        // Nothing to check unless at least one variant declares `= value`.
        if e.variants.iter().all(|v| v.discriminant.is_none()) {
            return;
        }

        // Fold each variant's discriminant into owned data first, so the error
        // emission below doesn't hold an immutable borrow of `e` across the
        // `&mut self` `type_error` calls.
        struct Folded {
            name: String,
            value: Option<i64>,
            has_payload: bool,
            explicit: bool,
            span: Span,
        }
        // Module-level integer constants a discriminant may reference:
        // immutable `let` bindings and `const` decls. `let mut` is excluded
        // (reassignable → not a constant). Built from a copied-out `&Program`
        // reference so the map's borrows are the program's lifetime, not tied to
        // the later `&mut self` `type_error` calls. (No name collisions: the
        // resolver already rejects duplicate module-item names.)
        let program: &Program = self.program;
        let consts: HashMap<&str, &Expr> = program
            .items
            .iter()
            .filter_map(|item| match item {
                Item::ConstDecl(c) => Some((c.name.as_str(), &c.value)),
                Item::ModuleBinding(b) if !b.is_mut => Some((b.name.as_str(), &b.value)),
                _ => None,
            })
            .collect();

        let mut folded: Vec<Folded> = Vec::with_capacity(e.variants.len());
        let mut nonconst: Vec<Span> = Vec::new();
        for v in &e.variants {
            let has_payload = matches!(v.kind, VariantKind::Struct(_) | VariantKind::Tuple(_));
            match &v.discriminant {
                None => folded.push(Folded {
                    name: v.name.clone(),
                    value: None,
                    has_payload,
                    explicit: false,
                    span: v.span.clone(),
                }),
                Some(expr) => {
                    let value = Self::fold_int_const(expr, &consts, &mut Vec::new());
                    if value.is_none() {
                        nonconst.push(expr.span.clone());
                    }
                    folded.push(Folded {
                        name: v.name.clone(),
                        value,
                        has_payload,
                        explicit: true,
                        span: expr.span.clone(),
                    });
                }
            }
        }

        for span in nonconst {
            self.type_error(
                "error[E_NON_CONSTANT_DISCRIMINANT]: an enum discriminant must be a constant \
                 integer expression — an integer literal, arithmetic over such, or a reference to \
                 an immutable module-level integer constant (`let`/`const`); a `let mut` binding, \
                 a non-integer or unknown name, or a reference cycle is not a constant here"
                    .to_string(),
                span,
                TypeErrorKind::DiscriminantInvalid,
            );
        }

        // (a) All-or-nothing: every variant declares a value, or none does.
        let first_explicit = folded.iter().find(|f| f.explicit);
        let first_implicit = folded.iter().find(|f| !f.explicit);
        if let (Some(fe), Some(fi)) = (first_explicit, first_implicit) {
            self.type_error(
                format!(
                    "error[E_PARTIAL_EXPLICIT_DISCRIMINANTS]: enum '{}' mixes explicit and \
                     implicit discriminants; declare a value on every variant or remove the \
                     explicit values and rely on declaration order (variant '{}' has one, '{}' \
                     does not)",
                    e.name, fe.name, fi.name
                ),
                fe.span.clone(),
                TypeErrorKind::DiscriminantInvalid,
            );
        }

        // (d) `#[repr]` requirement on payload variants.
        let head_names = super::repr_arg_head_names(&e.attributes);
        let int_repr = head_names
            .iter()
            .find_map(|n| Self::int_repr_range(n).map(|r| (n.clone(), r)));
        let has_repr_c = head_names.iter().any(|n| n == "C");
        let commits_discriminant = int_repr.is_some() || has_repr_c;
        if !commits_discriminant {
            if let Some(f) = folded.iter().find(|f| f.has_payload && f.explicit) {
                self.type_error(
                    format!(
                        "error[E_PAYLOAD_DISCRIMINANT_REQUIRES_REPR]: explicit discriminants on \
                         payload variants require '#[repr(intN)]' or '#[repr(C)]'; without one, \
                         the discriminant location is unspecified and the value commitment is \
                         unreachable (variant '{}')",
                        f.name
                    ),
                    f.span.clone(),
                    TypeErrorKind::DiscriminantInvalid,
                );
            }
        }

        // (b) Range check. `#[repr(intN)]` fixes the range; a bare `#[repr(C)]`
        // (no int companion) uses the platform `c_int` — signed `i32` on every
        // v1 target. Without any commitment repr there is no range constraint (a
        // field-less C-like enum's values just need to be constant + distinct).
        let range = int_repr.as_ref().map(|(n, r)| (n.clone(), *r)).or_else(|| {
            has_repr_c.then(|| ("c_int".to_string(), (i32::MIN as i64, i32::MAX as i64)))
        });
        if let Some((range_name, (lo, hi))) = range {
            for f in &folded {
                if let Some(val) = f.value {
                    if val < lo || val > hi {
                        self.type_error(
                            format!(
                                "error[E_DISCRIMINANT_OUT_OF_RANGE]: discriminant '{val}' on \
                                 variant '{}' does not fit in '{range_name}' (range '[{lo}, {hi}]')",
                                f.name
                            ),
                            f.span.clone(),
                            TypeErrorKind::DiscriminantInvalid,
                        );
                    }
                }
            }
        }

        // (c) Duplicate-value rejection over the *resolved* integer values (so
        // two variants folding to the same value collide even if written
        // differently).
        let mut seen: HashMap<i64, String> = HashMap::new();
        for f in &folded {
            if let Some(val) = f.value {
                if let Some(prev) = seen.get(&val) {
                    self.type_error(
                        format!(
                            "error[E_DUPLICATE_DISCRIMINANT]: variant '{}' has the same \
                             discriminant value '{val}' as variant '{}'",
                            f.name, prev
                        ),
                        f.span.clone(),
                        TypeErrorKind::DiscriminantInvalid,
                    );
                } else {
                    seen.insert(val, f.name.clone());
                }
            }
        }
    }

    fn check_repr_transparent_union(&mut self, u: &UnionDef) {
        if !self.repr_transparent_active(&u.attributes, &u.span) {
            return;
        }
        self.type_error(
            "error[E_REPR_TRANSPARENT_UNION_FORBIDDEN]: `#[repr(transparent)]` is not \
             permitted on a `union` — unions already give untagged byte reuse, so \
             layering `transparent` on top is incoherent"
                .to_string(),
            u.span.clone(),
            TypeErrorKind::ReprTransparentInvalid,
        );
    }

    fn check_repr_transparent_distinct(&mut self, d: &DistinctTypeDef) {
        if !self.repr_transparent_active(&d.attributes, &d.span) {
            return;
        }
        // A `distinct type Foo = Inner` is a single-field newtype by
        // construction, so the only shape rule is the unsized-generic guard.
        let gp = Self::generic_param_names(&d.generic_params);
        if Self::is_bare_type_param(&d.base_type, &gp) {
            self.type_error(
                format!(
                    "error[E_REPR_TRANSPARENT_REQUIRES_SIZED]: the base of a \
                     `#[repr(transparent)]` distinct type is the bare type parameter \
                     '{}', so the wrapper's size depends on it and its layout is \
                     indeterminate; wrap a concrete or `Sized`-known type instead",
                    match &d.base_type.kind {
                        TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                        _ => String::new(),
                    }
                ),
                d.base_type.span.clone(),
                TypeErrorKind::ReprTransparentInvalid,
            );
        }
    }

    /// Type-check a struct's `invariant` predicates (design.md § Contracts).
    /// Each invariant must be `bool`, evaluated with `self` bound to the
    /// struct's own type so `self.field` references resolve. No-op for a
    /// struct without invariants.
    fn check_struct_invariants(&mut self, s: &StructDef, gp: &[String]) {
        if s.invariants.is_empty() && s.impl_invariants.is_empty() {
            return;
        }
        let self_ty = Type::Named {
            name: s.name.clone(),
            args: gp.iter().map(|p| Type::TypeParam(p.clone())).collect(),
        };
        self.local_scope = LocalTypeScope::new();
        self.local_scope.insert("self".to_string(), self_ty.clone());
        let saved_self = self.current_self_type.take();
        self.current_self_type = Some(self_ty);
        for inv in s.invariants.iter().chain(s.impl_invariants.iter()) {
            self.check_expr(inv, &Type::Bool);
            self.reject_old_calls(inv, "invariant");
        }
        self.current_self_type = saved_self;
    }

    /// `par struct` / `par enum` definition-site field-constraint check
    /// (design.md § "Part 5b: Concurrent Shared Types" > Field constraints).
    ///
    /// A `par` type enforces concurrent safety structurally: immutable fields
    /// (`field: T`) are freely readable across tasks, but every `mut` field
    /// must be a concurrency primitive — `Atomic[T]` (lock-free) or `Mutex[T]`
    /// (locked, for compound mutation). A bare `mut val: i64` is a compile
    /// error here, at the definition site, unconditionally — the guarantee
    /// does not depend on any usage site or parallel-region boundary.
    ///
    /// `kind` is `"struct"` / `"enum"`; for `par enum` this runs once per
    /// struct-shaped variant. Plain / `shared` types are skipped by the
    /// caller (it only invokes this for `is_par` definitions).
    fn check_par_field_constraints(&mut self, kind: &str, type_name: &str, fields: &[StructField]) {
        for f in fields {
            // `OnceCell[T]` is single-task; a `par struct`/`par enum` is
            // visible to every task, so a field whose type mentions
            // `OnceCell` anywhere in its tree breaks the structural
            // safety guarantee. Reject it (`OnceLock[T]` is the
            // cross-task-safe replacement). Checked ahead of the
            // `mut`-must-be-concurrent rule so an immutable `OnceCell`
            // field is caught too (it would otherwise pass the `!f.is_mut`
            // skip). `shared struct` fields accept `OnceCell` and never
            // reach here — the caller only invokes this for `is_par`.
            if Self::type_expr_mentions_type(&f.ty, "OnceCell") {
                self.type_error(
                    format!(
                        "error[E_ONCE_CELL_IN_PAR_TYPE]: field `{field}` of \
                         `par {kind} {type_name}` has type `{ty}`, but `OnceCell[T]` \
                         is single-task; fields of a `par {kind}` are visible to \
                         every task and must use the cross-task-safe `OnceLock[T]` \
                         instead. Replace `OnceCell` with `OnceLock` in the field \
                         type",
                        field = f.name,
                        ty = crate::formatter::render_type_expr(&f.ty),
                    ),
                    f.ty.span.clone(),
                    TypeErrorKind::ParFieldNotConcurrent,
                );
                continue;
            }
            if !f.is_mut || Self::is_concurrent_field_type(&f.ty) {
                continue;
            }
            // Anchor at the `mut` keyword when the parser captured its span
            // (always present for `mut` fields), else the field type.
            let span = f
                .mut_keyword_span
                .clone()
                .unwrap_or_else(|| f.ty.span.clone());
            self.type_error(
                format!(
                    "`mut` field `{field}` of `par {kind} {type_name}` must be \
                     `Atomic[T]` or `Mutex[T]`: a `par {kind}`'s concurrent-safety \
                     guarantee is structural, so plain mutable fields are not \
                     permitted. Wrap the field type in `Atomic[...]` for lock-free \
                     access or `Mutex[...]` for locked compound mutation, or drop \
                     `mut` to make the field immutable (immutable fields are freely \
                     readable across tasks)",
                    field = f.name,
                ),
                span,
                TypeErrorKind::ParFieldNotConcurrent,
            );
        }
    }

    /// True when `ty` is a `par`-field-legal concurrency primitive —
    /// `Atomic[...]` or `Mutex[...]` (matched on the final path segment, so
    /// `core.sync.Atomic[T]` qualifies too). Generic args are not inspected:
    /// the *wrapper* is what provides the synchronization, regardless of the
    /// inner type.
    fn is_concurrent_field_type(ty: &TypeExpr) -> bool {
        matches!(
            &ty.kind,
            TypeKind::Path(p)
                if matches!(
                    p.segments.last().map(String::as_str),
                    Some("Atomic") | Some("Mutex")
                )
        )
    }

    /// True when `target` appears as the final path segment of any type
    /// node anywhere in `ty`'s tree — `OnceCell[i64]`, `Vec[OnceCell[i64]]`,
    /// `(i64, OnceCell[T])`, `ref OnceCell[T]`, etc. Used by the
    /// single-task structural rules (`OnceCell` at module scope / in a
    /// `par`-type field) to reject a disallowed type wherever it is nested.
    /// Operates on the surface `TypeExpr` (pre-lowering), matching the
    /// last-segment convention of `is_concurrent_field_type`.
    fn type_expr_mentions_type(ty: &TypeExpr, target: &str) -> bool {
        match &ty.kind {
            TypeKind::Path(p) => {
                if p.segments.last().map(String::as_str) == Some(target) {
                    return true;
                }
                p.generic_args
                    .as_ref()
                    .is_some_and(|ga| ga.iter().any(|a| Self::generic_arg_mentions(a, target)))
            }
            TypeKind::Tuple(elems) => elems
                .iter()
                .any(|e| Self::type_expr_mentions_type(e, target)),
            TypeKind::Array { element, .. } => Self::type_expr_mentions_type(element, target),
            TypeKind::Pointer { inner, .. }
            | TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Weak(inner) => Self::type_expr_mentions_type(inner, target),
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                params
                    .iter()
                    .any(|p| Self::type_expr_mentions_type(p, target))
                    || return_type
                        .as_ref()
                        .is_some_and(|rt| Self::type_expr_mentions_type(rt, target))
            }
            TypeKind::ImplTrait { args, .. } | TypeKind::Dyn { args, .. } => {
                args.iter().any(|a| Self::generic_arg_mentions(a, target))
            }
            TypeKind::Unit | TypeKind::Error => false,
        }
    }

    /// Recurse into a `GenericArg`'s type payload for
    /// [`Self::type_expr_mentions_type`]. Const / shape args carry no type
    /// tree to inspect.
    fn generic_arg_mentions(arg: &GenericArg, target: &str) -> bool {
        match arg {
            GenericArg::Type(t) => Self::type_expr_mentions_type(t, target),
            GenericArg::Const(_) | GenericArg::Shape(_) => false,
        }
    }

    /// Type-check a function's contract clauses (design.md § Contracts).
    /// Each `requires` predicate and each `ensures` body must have type
    /// `bool`; an `ensures(result) …` clause binds `result` to the return
    /// type for the duration of its body check. Callers invoke this with the
    /// function's parameters already bound in `local_scope`.
    fn check_contract_clauses(
        &mut self,
        requires: &[Expr],
        ensures: &[EnsuresClause],
        return_type: &Type,
        self_consumed: bool,
    ) {
        for req in requires {
            self.check_expr(req, &Type::Bool);
            // `old(...)` is only valid in `ensures` (design.md § Contracts
            // rule 4 — preconditions already run at entry, so pre-state is
            // just "state").
            self.reject_old_calls(req, "requires");
        }
        for ens in ensures {
            self.local_scope.push();
            if let Some(param) = &ens.param {
                self.local_scope.insert(param.clone(), return_type.clone());
            }
            self.check_expr(&ens.body, &Type::Bool);
            // Validate `old(...)` occurrences: `old(result)` is rejected
            // (result does not exist at entry) and the captured expression
            // must be `Clone` (design.md § Contracts rule 4).
            let mut olds = Vec::new();
            collect_old_calls(&ens.body, &mut olds);
            for (old_span, arg) in olds {
                if let (Some(param), ExprKind::Identifier(n)) = (&ens.param, &arg.kind) {
                    if n == param {
                        self.type_error(
                            "error[E_OLD_RESULT]: `old(result)` is invalid — `result` does \
                             not exist at function entry; `old(...)` captures pre-state only"
                                .to_string(),
                            old_span,
                            TypeErrorKind::TypeMismatch,
                        );
                        continue;
                    }
                }
                let arg_ty = self.infer_expr(&arg);
                if !matches!(arg_ty, Type::Error) && !self.type_supports_clone(&arg_ty) {
                    self.type_error(
                        format!(
                            "error[E_OLD_NOT_CLONE]: `old(...)` requires the captured \
                             expression to be Clone, but `{}` is not — capture a narrower \
                             Clone field (e.g. `old(self.field)`) instead",
                            type_display(&arg_ty)
                        ),
                        old_span,
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            // Consumed-parameter check (design.md § Contracts rule 4): a
            // bare-`self` (owned/consuming) receiver is moved by the time the
            // postcondition runs, so referencing `self` directly — outside an
            // `old(...)` capture — is an error.
            if self_consumed && references_self_outside_old(&ens.body) {
                self.type_error(
                    "error[E_CONSUMED_SELF_IN_ENSURES]: cannot reference consumed parameter \
                     `self` in an `ensures` clause — an owned (`self`) receiver is moved by the \
                     time the postcondition runs; capture its pre-state with `old(self)` or \
                     `old(self.field)`"
                        .to_string(),
                    ens.body.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
            self.local_scope.pop();
        }
    }

    /// Emit an error for every `old(...)` occurrence inside a contract
    /// expression where `old` is not permitted (`requires` / `invariant`).
    fn reject_old_calls(&mut self, expr: &Expr, ctx: &str) {
        let mut olds = Vec::new();
        collect_old_calls(expr, &mut olds);
        for (old_span, _) in olds {
            self.type_error(
                format!(
                    "error[E_OLD_OUTSIDE_ENSURES]: `old(...)` is only valid inside an \
                     `ensures` clause, not in a {ctx} contract"
                ),
                old_span,
                TypeErrorKind::TypeMismatch,
            );
        }
    }

    /// Like `infer_block`, but type-checks the block's final expression
    /// against an expected type so expected-type threading (e.g. `.into()`)
    /// sees the target.
    pub(super) fn check_block_against(&mut self, block: &Block, expected: &Type) -> Type {
        self.local_scope.push();
        let mut diverged = false;
        for stmt in &block.stmts {
            diverged |= self.check_stmt(stmt) == Type::Never;
        }
        let ty = if let Some(ref expr) = block.final_expr {
            self.check_expr(expr, expected)
        } else if diverged {
            // Tail-less but diverging — bottom, not unit (see #12).
            Type::Never
        } else {
            Type::Unit
        };
        self.local_scope.pop();
        ty
    }

    fn check_impl_block(&mut self, imp: &ImplBlock) {
        // Variance markers are legal only on stdlib struct/enum
        // declarations (design.md § Variance) — never on impl blocks.
        self.reject_user_variance_markers(&imp.generic_params, false);

        let type_name = match &imp.target_type.kind {
            TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
            _ => return,
        };
        // Slice 1b: `env_add_impl` already emitted
        // `E_OPAQUE_TYPE_NO_INHERENT_OR_TRAIT_IMPLS` for impls on opaque
        // foreign types and skipped registration. Silently skip method-body
        // checking here too so the user sees one focused diagnostic, not a
        // cascade of `self`-argument REQUIRES_INDIRECTION + missing-supertrait
        // noise from the unregistered impl.
        if self.env.opaque_foreign_types.contains(&type_name) {
            return;
        }
        // Validate inline bounds and where clause on the impl block itself
        let gp = Self::generic_param_names(&imp.generic_params);

        // `impl Tr for u8 { fn m(self) -> Self { self + self } }` — a trait
        // impl on a PRIMITIVE target. Hand-building `Named { name: "u8" }`
        // leaves `self` non-numeric inside the body, so `self + self` errors
        // "arithmetic operator requires numeric type, found 'u8'". Lower the
        // target type instead: for a scalar primitive that yields
        // `Type::UInt(U8)` / `Type::Int(I64)` / `Type::Float(_)` / … so the
        // body's `self` is recognized as numeric. Struct / enum / generic
        // targets keep the by-name `Named { type_name }` shape they relied on
        // (lowering a generic target would fold in `target_args` and shift
        // existing behavior). B-2026-07-03-5.
        //
        // S6c-12: a CONCRETE handle-backed container target (`impl Trait for
        // Column[i64]` / `Tensor[i64, [n]]`) is the one exception that KEEPS
        // its element args. The Column/Tensor reduction intercepts in
        // `infer_method_call` key on `args.len() == 1` to compute a builtin
        // method's return type; with `self` erased to `Column[]` a body call
        // like `self.sum()` misses the intercept and stays the abstract trait
        // return `T`, so `self.sum() + self.sum()` errors "found 'T'". Scoped
        // to fully-concrete Column/Tensor so user-struct/enum/generic impls
        // (whose empty-args shape the comment above protects) are untouched.
        let self_type = {
            let lowered = self.lower_type_expr(&imp.target_type, &gp);
            match &lowered {
                Type::Int(_) | Type::UInt(_) | Type::Float(_) | Type::Bool | Type::Char => lowered,
                Type::Named { name, args }
                    if (name == "Column" || name == "Tensor")
                        && !args.is_empty()
                        && args.iter().all(type_is_fully_concrete) =>
                {
                    lowered.clone()
                }
                // S6c blanket-Vec: `impl Trait for Vec[i64]` must keep its
                // concrete element arg so `self` is `Vec[i64]` (not `Vec[]`)
                // and `for x in self` types the element `i64` — otherwise the
                // body's element stays the abstract trait param `T` and a
                // `self.sum()`-style fold errors "cannot mix i64 and T".
                Type::Named { name, args }
                    if (name == "Vec" || name == "VecDeque")
                        && !args.is_empty()
                        && args.iter().all(type_is_fully_concrete) =>
                {
                    lowered.clone()
                }
                _ => Type::Named {
                    name: type_name.clone(),
                    args: Vec::new(),
                },
            }
        };

        self.validate_all_bounds(&imp.generic_params, &imp.where_clause, &gp);

        // Check that trait impls provide all required associated types,
        // and that all supertrait impls exist for the same target type.
        if let Some(ref trait_path) = imp.trait_name {
            let trait_name = trait_path.segments.last().cloned().unwrap_or_default();
            // `impl MarkerTrait for T { fn ... }` — the body of an impl
            // for a marker trait must be empty. Per design.md § Marker
            // Traits.
            if self.env.marker_traits.contains(&trait_name) {
                let has_items = imp
                    .items
                    .iter()
                    .any(|item| matches!(item, ImplItem::Method(_) | ImplItem::AssocType(_)));
                if has_items {
                    self.type_error(
                        format!(
                            "error[E_MARKER_IMPL_HAS_METHOD]: impl of marker trait \
                             '{trait_name}' cannot contain methods or items; \
                             the body must be empty"
                        ),
                        imp.span.clone(),
                        TypeErrorKind::TypeMismatch,
                    );
                }
            }
            // `impl TraitAlias for T` is rejected at v1: trait aliases are
            // not implementable directly. Per design.md § Trait Aliases —
            // implement each component trait separately. The bound list is
            // copy-pasted into the diagnostic so the user can apply the
            // workaround inline.
            if self.is_trait_alias(&trait_name) {
                let bound_list = self
                    .trait_alias_bound_list(&trait_name)
                    .unwrap_or_else(|| "<bounds>".to_string());
                self.type_error(
                    format!(
                        "error[E_IMPL_TRAIT_ALIAS]: cannot implement trait alias \
                         '{trait_name}'; implement each component trait \
                         separately: `{bound_list}`"
                    ),
                    imp.span.clone(),
                    TypeErrorKind::TypeMismatch,
                );
            }
            if let Some(trait_info) = self.env.traits.get(&trait_name).cloned() {
                let provided: HashSet<String> = imp
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        ImplItem::AssocType(binding) => Some(binding.name.clone()),
                        _ => None,
                    })
                    .collect();
                for required in &trait_info.assoc_types {
                    if !provided.contains(required) {
                        self.type_error(
                            format!(
                                "impl of trait '{}' is missing associated type '{}'",
                                trait_name, required
                            ),
                            imp.span.clone(),
                            TypeErrorKind::MissingField,
                        );
                    }
                }
                // Supertrait constraint: every supertrait of `trait_name` must
                // have an impl for the same target type. Theme-4 deviation:
                // when `imp` is specialized (`impl Foo for Bar[i32]`), the
                // ideal supertrait check would require `impl SuperFoo for
                // Bar[i32]` specifically; currently we accept either a
                // matching specialized supertrait OR a generic-on-name
                // supertrait. Tightening is out of scope until a real
                // specialized-with-supertrait case appears.
                for supertrait in &trait_info.supertraits {
                    let has_impl = self.env.impls.iter().any(|info| {
                        info.trait_name.as_deref() == Some(supertrait.as_str())
                            && info.target_type == type_name
                    });
                    if !has_impl {
                        self.type_error(
                            format!(
                                "impl {} for {} requires impl {} for {}",
                                trait_name, type_name, supertrait, type_name
                            ),
                            imp.span.clone(),
                            TypeErrorKind::MissingSupertrait,
                        );
                    }
                }
            }
        }

        // Store assoc type bindings so `resolve_assoc_projections` can look
        // them up when substituting `T.Item` after `T` is solved to this type.
        let gp = Self::generic_param_names(&imp.generic_params);

        // Save outer bounds, merge in impl-level bounds. Method bodies see
        // both the impl's generic params and their own; `check_function`
        // further merges method-level bounds and restores after each method.
        let saved_bounds = self.enclosing_bounds.clone();
        for (name, bounds) in Self::collect_param_bounds(&imp.generic_params, &imp.where_clause) {
            self.enclosing_bounds.insert(name, bounds);
        }

        // Lint-level slice 4b — push the impl block's lint overrides
        // so per-method `check_function` calls inherit the impl's
        // overrides via the cascade. Popped at the end of the impl.
        self.lint_override_stack.push(imp.lint_overrides.clone());

        // GAT slice 7: cache the trait's AssocTypeDecl bounds keyed by
        // assoc-type name so the binding loop can enforce them at impl
        // site without re-walking program.items per binding. Empty when
        // the impl is inherent (no trait) or the trait isn't found in
        // the current program's items (e.g., baked stdlib traits, where
        // the bound enforcement is a no-op — slice 7 v1 scope is the
        // user-program surface where program.items carries the decl).
        //
        // GAT slice 8b carry-forwards (b) + (c): also cache the GAT
        // decl's per-param inline-bound list and the GAT decl's
        // where-clause so `resolve_assoc_projections` can discharge
        // them at projection-resolution time.
        let trait_assoc_decls: HashMap<String, &AssocTypeDecl> = imp
            .trait_name
            .as_ref()
            .and_then(|tp| tp.segments.last())
            .and_then(|tn| {
                self.program.items.iter().find_map(|it| match it {
                    Item::TraitDef(t) if t.name == *tn => Some(t),
                    _ => None,
                })
            })
            .map(|trait_def| {
                trait_def
                    .items
                    .iter()
                    .filter_map(|it| match it {
                        TraitItem::AssocType(decl) => Some((decl.name.clone(), decl.as_ref())),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let trait_assoc_bounds: HashMap<String, Vec<TraitBound>> = trait_assoc_decls
            .iter()
            .map(|(name, decl)| (name.clone(), decl.bounds.clone()))
            .collect();

        // `par struct` / `par enum` methods may not declare a `mut ref self`
        // receiver (design.md § Part 5b > "`ref self` receivers only"): `par`
        // values are always Arc with potential multiple holders, so the
        // exclusive mutable borrow `mut ref self` is never available.
        // Consuming `self` (drop one Arc handle) and `ref self` (shared read)
        // remain legal; exclusive mutation goes through `lock` blocks on
        // `Mutex[T]` fields. Inherent impls only — a trait impl's receiver
        // shape is fixed by the trait declaration, so rejecting it here would
        // misattribute the error to the impl site.
        let target_is_par = imp.trait_name.is_none()
            && (self.env.structs.get(&type_name).is_some_and(|i| i.is_par)
                || self.env.enums.get(&type_name).is_some_and(|i| i.is_par));

        for item in &imp.items {
            match item {
                ImplItem::Method(method) => {
                    if target_is_par && method.self_param == Some(SelfParam::MutRef) {
                        self.type_error(
                            format!(
                                "method `{m}` on `par {tn}` cannot take a `mut ref self` \
                                 receiver: a `par` value is always Arc-allocated and may \
                                 have multiple holders, so exclusive mutable access is \
                                 never available. Use `ref self` and mutate `Atomic[T]` \
                                 fields via their atomic operations or `Mutex[T]` fields \
                                 inside a `lock` block",
                                m = method.name,
                                tn = type_name,
                            ),
                            method.span.clone(),
                            TypeErrorKind::ParMutSelfReceiver,
                        );
                    }
                    self.check_function(method, Some(&self_type), &[]);
                }
                ImplItem::AssocType(binding) => {
                    // GAT slice 5: extend the generic scope with the GAT's
                    // own params so the binding RHS like `Wrapper[U]` lowers
                    // `U` as `Type::TypeParam("U")` instead of falling
                    // through as `Named { name: "U", args: [] }`. The
                    // template now references both impl-side params
                    // (from `gp`) and GAT-side params (from
                    // `binding.generic_params`) uniformly; the resolver
                    // distinguishes the two via the `gat_params` list
                    // stored alongside the template.
                    let gat_params = Self::generic_param_names(&binding.generic_params);
                    let mut combined_scope = gp.clone();
                    combined_scope.extend(gat_params.iter().cloned());
                    let bound_ty = self.lower_type_expr(&binding.ty, &combined_scope);

                    // GAT slice 7: impl-site bound enforcement.
                    // The trait's GAT declaration may carry bounds
                    // (`type Mapped[U]: Trait`). At every impl site,
                    // the binding's lowered RHS must satisfy each
                    // declared bound. Per design.md the proof is
                    // structural: the RHS is provable to satisfy
                    // `Trait` for arbitrary GAT-param instantiation
                    // when the RHS's head type carries a generic-on-
                    // name impl of the bound trait (e.g.,
                    // `Vec[U]: Clone` via `impl[T] Clone for Vec[T]`).
                    // The TypeParam-RHS shape (`type Mapped = T`)
                    // proves via the impl's own `enclosing_bounds`
                    // on T.
                    if let Some(bounds) = trait_assoc_bounds.get(&binding.name) {
                        for bound in bounds {
                            let bound_trait = bound.path.last().cloned().unwrap_or_default();
                            if !self.gat_rhs_satisfies_bound(&bound_ty, &bound_trait) {
                                self.type_error(
                                    format!(
                                        "error[E_GAT_BOUND_NOT_SATISFIED]: \
                                         binding `type {} = {}` does not satisfy \
                                         declared GAT bound `{}: {}`",
                                        binding.name,
                                        type_display(&bound_ty),
                                        binding.name,
                                        bound_trait,
                                    ),
                                    binding.span.clone(),
                                    TypeErrorKind::TypeMismatch,
                                );
                            }
                        }
                    }

                    // GAT slice 8b: capture the GAT decl's per-param
                    // inline bounds + where-clause for projection-time
                    // discharge.
                    let trait_decl = trait_assoc_decls.get(&binding.name);
                    let param_bound_traits: Vec<Vec<String>> =
                        match trait_decl.and_then(|d| d.generic_params.as_ref()) {
                            Some(gp) => gp
                                .params
                                .iter()
                                .map(|p| {
                                    p.bounds
                                        .iter()
                                        .filter_map(|tb| tb.path.last().cloned())
                                        .collect()
                                })
                                .collect(),
                            None => Vec::new(),
                        };
                    let where_clause_clone = trait_decl.and_then(|d| d.where_clause.clone());

                    self.env.impl_assoc_types.insert(
                        (type_name.clone(), binding.name.clone()),
                        crate::typechecker::env::ImplAssocTypeEntry {
                            ty: bound_ty,
                            gat_params,
                            param_bound_traits,
                            where_clause: where_clause_clone,
                        },
                    );
                }
            }
        }

        self.enclosing_bounds = saved_bounds;
        self.lint_override_stack.pop();
    }

    fn check_const_decl(&mut self, c: &ConstDecl) {
        let declared_ty = self.lower_type_expr(&c.ty, &[]);
        let value_ty = self.infer_expr(&c.value);
        self.check_assignable(&declared_ty, &value_ty, c.value.span.clone());
    }

    /// Slice 4 + 5 of design.md § Module-Level Bindings (§1280-1297,
    /// §1284). Walks the binding's initializer expression and rejects
    /// any sub-shape that requires runtime evaluation (slice 4); when
    /// the binding carries an explicit `: TYPE` annotation, checks the
    /// initializer's type against it and rejects bare `String` per
    /// §1297; when no annotation is present, infers the binding's type
    /// from the initializer and stashes it in `env.constants` for
    /// use-site resolution (slice 5).
    fn check_module_binding(&mut self, b: &ModuleBinding) {
        self.check_module_binding_init(&b.value, &b.name);

        if let Some(ref ty_expr) = b.ty {
            // `OnceCell[T]` is single-task; a module-level binding is
            // visible to every task, so it must be the cross-task-safe
            // `OnceLock[T]` instead. Checked on the surface type tree so a
            // nested occurrence (`Map[String, OnceCell[i64]]`) is caught
            // too. Reuses the cross-task-unsafe diagnostic family — this is
            // the same single-task-leaks-across-tasks violation the par /
            // spawn / channel escape check reports, just at declaration
            // scope.
            if Self::type_expr_mentions_type(ty_expr, "OnceCell") {
                self.type_error(
                    format!(
                        "error[E_ONCE_CELL_AT_MODULE_SCOPE]: module-level binding '{}' \
                         is declared with type '{}', but `OnceCell[T]` is single-task; \
                         module-level bindings are visible to every task and must use \
                         the cross-task-safe `OnceLock[T]` instead. Replace `OnceCell` \
                         with `OnceLock` in the binding's type",
                        b.name,
                        crate::formatter::render_type_expr(ty_expr),
                    ),
                    ty_expr.span.clone(),
                    TypeErrorKind::CrossTaskUnsafeCapture,
                );
                return;
            }
            let declared = self.lower_type_expr(ty_expr, &[]);
            if matches!(declared, Type::Str) {
                self.type_error(
                    format!(
                        "error[E_MODULE_BINDING_HEAP_TYPE]: module-level binding '{}' \
                         is declared with type 'String' which is heap-allocated; \
                         module bindings live in the binary as constant data — \
                         use 'StringSlice' for static string data",
                        b.name,
                    ),
                    ty_expr.span.clone(),
                    TypeErrorKind::ModuleBindingHeapType,
                );
                return;
            }
            // §1284: at module scope, string literals have type
            // `StringSlice` rather than the function-body default of
            // `String`. The generic `check_expr` against a
            // `StringSlice`-typed expected does not coerce a
            // `StringLit` (which always infers as `String`), so a
            // bare `let X: StringSlice = "lit";` would falsely
            // mismatch. The literal-coercion carve-out is contained
            // here: when the declared type is `StringSlice` and the
            // init is structurally a bare string literal, accept
            // without invoking the generic check. Composite forms
            // (e.g. `[("a", "b"); 4]`) are slice-5-out-of-scope and
            // continue to go through `check_expr` — the kata-level
            // need is the bare-literal case.
            if Self::module_binding_is_string_slice_named(&declared)
                && matches!(b.value.kind, ExprKind::StringLit(_))
            {
                // Skip the check_expr — the literal coerces by §1284.
            } else {
                self.check_expr(&b.value, &declared);
            }
        } else {
            // No annotation — infer from the initializer. The const-init
            // walker above has already verified the value's shape, so any
            // type the inferer returns is rooted in a permitted form.
            let inferred = self.infer_expr(&b.value);
            // Same single-task rule as the annotated branch, for the
            // direct `let CFG = OnceCell.new()` form (a nested unannotated
            // occurrence isn't constructible as a const-init initializer).
            if matches!(&inferred, Type::Named { name, .. } if name == "OnceCell") {
                self.type_error(
                    format!(
                        "error[E_ONCE_CELL_AT_MODULE_SCOPE]: module-level binding '{}' \
                         was inferred as `OnceCell[T]`, which is single-task; \
                         module-level bindings are visible to every task and must use \
                         the cross-task-safe `OnceLock[T]` instead — annotate the \
                         binding as `: OnceLock[...]` and construct it with \
                         `OnceLock.new()`",
                        b.name,
                    ),
                    b.value.span.clone(),
                    TypeErrorKind::CrossTaskUnsafeCapture,
                );
                return;
            }
            // §1297: a heap-allocated `String` cannot live at module
            // scope. The §1284 sibling rule (string literals default to
            // `StringSlice` at module scope) is not yet automatic —
            // direct the programmer to the explicit annotation instead.
            if matches!(inferred, Type::Str) {
                self.type_error(
                    format!(
                        "error[E_MODULE_BINDING_HEAP_TYPE]: module-level binding '{}' \
                         was inferred as 'String' which is heap-allocated; module \
                         bindings live in the binary as constant data — annotate \
                         the binding as `: StringSlice` for static string data",
                        b.name,
                    ),
                    b.value.span.clone(),
                    TypeErrorKind::ModuleBindingHeapType,
                );
                return;
            }
            if !matches!(inferred, Type::Error) {
                self.env.constants.insert(b.name.clone(), inferred);
            }
        }
    }

    /// `Type::Named { name: "StringSlice", args: [] }` predicate used
    /// by the §1284 literal-coercion carve-out in `check_module_binding`.
    fn module_binding_is_string_slice_named(ty: &Type) -> bool {
        matches!(ty, Type::Named { name, args } if name == "StringSlice" && args.is_empty())
    }

    /// Slice 5 of design.md § Module-Level Bindings. The assignment-LHS
    /// mutability check for module-level `let` / `let mut`: when the
    /// assignment target is an identifier that resolves to a
    /// module-level binding (looked up by scanning `self.program.items`
    /// — the resolver's symbol table classifies these as `Constant`-class
    /// per slice 3, so this is the authoritative source for the
    /// is_mut flag), an immutable binding rejects with
    /// `E_REASSIGN_TO_IMMUTABLE_MODULE_BINDING`. Mirrors the ownership
    /// checker's local-binding `ReassignToImmutable` rule but lives at
    /// the typechecker layer so use sites of module bindings are
    /// caught regardless of whether the assignment ever reaches the
    /// ownership pass (e.g. when earlier type errors cause the
    /// pipeline to short-circuit).
    pub(super) fn check_module_binding_assignment(&mut self, target: &Expr) {
        let ExprKind::Identifier(name) = &target.kind else {
            return;
        };
        // Local bindings shadow module bindings — if the name is in
        // the local scope, the module-binding check doesn't fire.
        if self.local_scope.lookup(name).is_some() {
            return;
        }
        let mut decl_span: Option<Span> = None;
        let mut is_mut = false;
        let mut found = false;
        for item in &self.program.items {
            if let Item::ModuleBinding(b) = item {
                if b.name == *name {
                    found = true;
                    is_mut = b.is_mut;
                    decl_span = Some(b.span.clone());
                    break;
                }
            }
        }
        if !found || is_mut {
            return;
        }
        let decl_hint = match decl_span {
            Some(s) => format!(" (declared at line {}:{})", s.line, s.column),
            None => String::new(),
        };
        self.type_error(
            format!(
                "error[E_REASSIGN_TO_IMMUTABLE_MODULE_BINDING]: cannot assign to \
                 module-level binding '{}' — declared without `mut`{}. Change the \
                 declaration to `let mut {}: ...` if mutation is required.",
                name, decl_hint, name,
            ),
            target.span.clone(),
            TypeErrorKind::ReassignToImmutableModuleBinding,
        );
    }

    /// Recursive const-init structural walk for slice 4. Permits
    /// literals, references to other bindings (any `Path`/`Identifier`
    /// — the resolver already validated the reference resolves), and
    /// composite forms built from permitted sub-expressions; rejects
    /// every shape requiring runtime evaluation.
    fn check_module_binding_init(&mut self, e: &Expr, binding_name: &str) {
        match &e.kind {
            // ── Always-permitted scalar literals ────────────────────
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_) => {}

            // f"..." builds a String at runtime — heap-allocating and
            // therefore rejected.
            ExprKind::InterpolatedStringLit(_) => {
                self.reject_module_binding_init(e, binding_name, "string interpolation");
            }

            // ── References ──────────────────────────────────────────
            // The resolver has already verified that the path/ident
            // resolves to something visible; whether *that* something
            // is itself a compile-time constant is checked at the
            // resolved-symbol level by the rest of the typechecker
            // (and ultimately at codegen, where the const-init lowering
            // will fail loudly if a ref to a runtime entity slipped
            // through). Paths to free functions are technically
            // permitted as "function-pointer values" — the spec doesn't
            // exclude that and it composes naturally with the
            // `LazyLock.new(|| ...)` capture rule (slice 10).
            ExprKind::Identifier(_) | ExprKind::Path { .. } => {}
            ExprKind::SelfValue | ExprKind::SelfType => {
                self.reject_module_binding_init(e, binding_name, "`self` / `Self` reference");
            }

            // ── Operators over permitted forms ──────────────────────
            ExprKind::Binary { left, right, .. } => {
                self.check_module_binding_init(left, binding_name);
                self.check_module_binding_init(right, binding_name);
            }
            ExprKind::Unary { operand, .. } => {
                self.check_module_binding_init(operand, binding_name);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.check_module_binding_init(left, binding_name);
                self.check_module_binding_init(right, binding_name);
            }

            // ── Composite literals ──────────────────────────────────
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for it in items {
                    self.check_module_binding_init(it, binding_name);
                }
            }
            ExprKind::PrefixCollectionLiteral { type_name, items } => {
                // Only `Array[...]` is permitted at module scope; `Vec`,
                // `Set`, `Map` allocate on the heap.
                if type_name == "Array" {
                    for it in items {
                        self.check_module_binding_init(it, binding_name);
                    }
                } else {
                    self.reject_module_binding_init(
                        e,
                        binding_name,
                        &format!(
                            "'{}[...]' collection literal (heap-allocated; \
                             use 'Array[...]' for fixed-size data)",
                            type_name,
                        ),
                    );
                }
            }
            ExprKind::RepeatLiteral {
                type_name,
                value,
                count,
            } => match type_name.as_deref() {
                // Bare `[v; n]` is permitted (the binding's declared
                // type coerces it to `Array[T, N]` per §1288; if the
                // declared type forces a `Vec`, slice 5 / the
                // surrounding type-check will catch the heap form).
                // Explicit `Array[v; n]` is also fine; explicit
                // `Vec[v; n]` allocates on the heap and is rejected.
                None | Some("Array") => {
                    self.check_module_binding_init(value, binding_name);
                    self.check_module_binding_init(count, binding_name);
                }
                _ => {
                    self.reject_module_binding_init(
                        e,
                        binding_name,
                        "repeat literal (only 'Array[v; n]' or bare '[v; n]' \
                         in an 'Array'-typed binding is permitted at module scope)",
                    );
                }
            },
            ExprKind::MapLiteral(_) => {
                self.reject_module_binding_init(e, binding_name, "Map literal (heap-allocated)");
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.check_module_binding_init(&f.value, binding_name);
                }
                if let Some(s) = spread {
                    self.check_module_binding_init(s, binding_name);
                }
            }

            // ── Calls: enum-variant constructors + recognized
            // special forms only ────────────────────────────────────
            ExprKind::Call { callee, args } => {
                if self.module_binding_call_is_enum_variant(callee) {
                    for a in args {
                        self.check_module_binding_init(&a.value, binding_name);
                    }
                } else if !self.module_binding_call_is_special_form(callee, args, binding_name) {
                    self.reject_module_binding_init(e, binding_name, "function call");
                }
            }

            // ── Member access / index — permitted only insofar as the
            // base is itself a permitted form ───────────────────────
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.check_module_binding_init(object, binding_name);
            }
            ExprKind::Index { object, index } => {
                self.check_module_binding_init(object, binding_name);
                self.check_module_binding_init(index, binding_name);
            }
            ExprKind::Cast { expr, .. } => {
                self.check_module_binding_init(expr, binding_name);
            }

            // ── Always-rejected runtime-only shapes ─────────────────
            ExprKind::MethodCall { .. } => {
                self.reject_module_binding_init(e, binding_name, "method call");
            }
            ExprKind::OptionalChain { .. } => {
                self.reject_module_binding_init(e, binding_name, "optional-chain call");
            }
            ExprKind::Closure { .. } => {
                self.reject_module_binding_init(e, binding_name, "closure");
            }
            ExprKind::Question(_) => {
                self.reject_module_binding_init(e, binding_name, "'?'-propagation");
            }
            ExprKind::Pipe { .. } | ExprKind::PipePlaceholder => {
                self.reject_module_binding_init(e, binding_name, "pipe expression");
            }
            ExprKind::Block(_)
            | ExprKind::Comptime(_)
            | ExprKind::If { .. }
            | ExprKind::IfLet { .. }
            | ExprKind::Match { .. }
            | ExprKind::While { .. }
            | ExprKind::WhileLet { .. }
            | ExprKind::For { .. }
            | ExprKind::Loop { .. }
            | ExprKind::LabeledBlock { .. }
            | ExprKind::Return(_)
            | ExprKind::Break { .. }
            | ExprKind::Continue { .. } => {
                self.reject_module_binding_init(
                    e,
                    binding_name,
                    "block or control-flow expression",
                );
            }
            ExprKind::Range { .. } => {
                self.reject_module_binding_init(e, binding_name, "range expression");
            }
            ExprKind::Unsafe(_) => {
                self.reject_module_binding_init(e, binding_name, "'unsafe' block");
            }
            ExprKind::Try(_) => {
                self.reject_module_binding_init(e, binding_name, "'try' block");
            }
            ExprKind::Seq(_) | ExprKind::Par(_) => {
                self.reject_module_binding_init(e, binding_name, "'seq' / 'par' block");
            }
            ExprKind::Lock { .. } => {
                self.reject_module_binding_init(e, binding_name, "'lock' block");
            }
            ExprKind::Providers { .. } => {
                self.reject_module_binding_init(e, binding_name, "'providers' block");
            }
            // Compile-time intrinsic — evaluates to a `usize` at compile
            // time, so it's permissible.
            ExprKind::OffsetOf { .. } => {}
            // Parser already emitted an error for this node; suppress
            // the duplicate const-init rejection.
            ExprKind::Error => {}
        }
    }

    /// Recognize `EnumName.Variant(args...)` calls in init position.
    /// Looks up the leading segment in the env's enum table to decide
    /// — purely structural recognition isn't safe because the same
    /// shape is also used by associated-fn calls like `Foo.bar()`.
    fn module_binding_call_is_enum_variant(&self, callee: &Expr) -> bool {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return false;
        };
        if segments.len() != 2 {
            return false;
        }
        let enum_name = &segments[0];
        let variant_name = &segments[1];
        let Some(info) = self.env.enums.get(enum_name) else {
            return false;
        };
        info.variants.iter().any(|(vname, _)| vname == variant_name)
    }

    /// Recognize the compiler-recognized constant-init special forms
    /// per design.md §1280-1297. Returns true if the call shape matches;
    /// in that case sub-expression walks (for `Atomic.new(LIT)` /
    /// `Mutex.new(LIT)`) have already been performed.
    fn module_binding_call_is_special_form(
        &mut self,
        callee: &Expr,
        args: &[CallArg],
        binding_name: &str,
    ) -> bool {
        let ExprKind::Path { segments, .. } = &callee.kind else {
            return false;
        };
        if segments.len() != 2 || segments[1] != "new" {
            return false;
        }

        match segments[0].as_str() {
            // The closure body is permitted to be arbitrary — it runs
            // at first access, not at compile time. Slice 10 of the
            // module-binding work will gate captures to other
            // compile-time bindings only.
            "LazyLock" => args.len() == 1 && matches!(args[0].value.kind, ExprKind::Closure { .. }),
            "OnceLock" | "OnceCell" => args.is_empty(),
            // `Vec.new()` / `VecDeque.new()` lower to the canonical empty
            // `{ptr=null, len=0, cap=0}` aggregate at codegen — see
            // `assoc_call.rs`'s shared `Vec/VecDeque && method == "new"` arm,
            // and `module_bindings.rs`'s matching const-init lowering. The
            // null-ptr-cap-0 representation is the runtime invariant for
            // empty Vec, so no heap allocation is needed and the value is
            // a true compile-time constant.
            "Vec" | "VecDeque" => args.is_empty(),
            // `Map.new()` / `Set.new()` are NOT a zero-shaped aggregate
            // (unlike Vec): `runtime/src/map.rs::karac_map_new` installs
            // per-instance hash seeds + a vtable before any op can run.
            // The const-init walker therefore admits them only as a
            // structural permission; codegen emits a placeholder `null`
            // `ptr` global and fills it from a `__karac_static_init`
            // prologue (`karac_map_new(...)`) that runs before `main`'s
            // body — see `declare_module_bindings` /
            // `finalize_module_binding_static_init` in
            // `src/codegen/module_bindings.rs`. `Set[T]` reuses the same
            // runtime via `karac_map_new` with `val_size = 0`.
            "Map" | "Set" => args.is_empty(),
            // Atomic.new / Mutex.new take a single argument that must
            // itself be a permitted constant-init form.
            "Atomic" | "Mutex" => {
                if args.len() != 1 {
                    return false;
                }
                self.check_module_binding_init(&args[0].value, binding_name);
                true
            }
            _ => false,
        }
    }

    fn reject_module_binding_init(&mut self, e: &Expr, binding_name: &str, what: &str) {
        self.type_error(
            format!(
                "error[E_MODULE_BINDING_EFFECTFUL_INIT]: module-level binding '{}' \
                 initializer must be a compile-time constant expression; {} requires \
                 runtime evaluation. Permitted forms: literals, references to other \
                 module-level bindings, arithmetic / boolean / comparison operations \
                 over permitted forms, struct / enum-variant constructors over \
                 permitted forms, tuple and fixed-size 'Array' literals, and the \
                 recognized special forms 'LazyLock.new(|| ...)', 'OnceLock.new()', \
                 'OnceCell.new()', 'Atomic.new(LITERAL)', 'Mutex.new(LITERAL)', \
                 'Vec.new()', 'VecDeque.new()', 'Map.new()', 'Set.new()'",
                binding_name, what,
            ),
            e.span.clone(),
            TypeErrorKind::ModuleBindingEffectfulInit,
        );
    }

    // ── Block & Statement ───────────────────────────────────────

    pub(super) fn infer_block(&mut self, block: &Block) -> Type {
        self.local_scope.push();
        let mut diverged = false;
        for stmt in &block.stmts {
            diverged |= self.check_stmt(stmt) == Type::Never;
        }
        let ty = if let Some(ref expr) = block.final_expr {
            self.infer_expr(expr)
        } else if diverged {
            // A tail-less block whose body diverges (e.g. `{ return e; }`)
            // never falls through — it is bottom, not unit. See #12.
            Type::Never
        } else {
            Type::Unit
        };
        self.local_scope.pop();
        ty
    }

    /// Diagnose unsolved generic type parameters in a synthesis-mode
    /// inferred type. Currently called from `let x = e;` and
    /// `let pat = e else …` when the user supplied no type annotation:
    /// without a check-mode expected type to pin them, any `TypeParam(T)`
    /// in `inferred` that isn't an enclosing function/impl generic is
    /// unsolvable at this site. Item 131 sub-step 2a.
    fn check_unsolved_type_param(&mut self, inferred: &Type, span: &Span) {
        if matches!(inferred, Type::Error) {
            return;
        }
        // A raw pointer whose pointee is unresolved is a LEGAL value —
        // construction is safe (design.md § Raw Pointer Construction: "Building a
        // raw pointer ... does not require unsafe; the construction itself has no
        // UB risk"). `let p = ptr.null()` with an un-pinned `T` must NOT error at
        // the binding; the pointee only has to be known where it is USED through a
        // size-dependent method (`read`/`write`/`offset`/…), which the
        // `E_RAW_POINTER_UNRESOLVED_POINTEE` check at the method-call site reports
        // (design.md § "Method dispatch on raw pointers requires a known pointee":
        // the diagnostic underlines the read site, not the construction). Deferring
        // here is what makes that focused, single diagnostic possible.
        if matches!(inferred, Type::Pointer { .. }) {
            return;
        }
        let unbound_type: Option<String> = {
            let in_scope: HashSet<&str> =
                self.enclosing_bounds.keys().map(|s| s.as_str()).collect();
            find_unbound_type_param(inferred, &in_scope).map(|s| s.to_string())
        };
        if let Some(name) = unbound_type {
            self.type_error(
                format!(
                    "cannot infer type parameter '{}'; add a type annotation to this binding",
                    name
                ),
                span.clone(),
                TypeErrorKind::CannotInferTypeParam,
            );
        }
        // Const generics slice 3b sub-step (h): const-param analog.
        // Surfaces `cannot infer const parameter 'N'` for return-only
        // / bounds-only const-params that the call-site solver
        // couldn't pin from arguments (e.g.
        // `fn f[const N: i64]() -> Array[i64, N]` called as `let x = f();`
        // without an annotation).
        let unbound_const: Option<String> = {
            let in_scope: HashSet<&str> =
                self.enclosing_bounds.keys().map(|s| s.as_str()).collect();
            find_unbound_const_param(inferred, &in_scope).map(|s| s.to_string())
        };
        if let Some(name) = unbound_const {
            self.type_error(
                format!(
                    "cannot infer const parameter '{}'; provide explicit generic args \
                     (e.g. `f[..., 8](...)`) or add a type annotation to this binding",
                    name
                ),
                span.clone(),
                TypeErrorKind::CannotInferTypeParam,
            );
        }
    }

    /// Returns the statement's *flow type*: `Type::Never` when the
    /// statement diverges (a trailing `return x;` / `break;` /
    /// `continue;`, or a call to a `-> !` function like `panic(..)` /
    /// `process.exit(..)` in statement position), else `Type::Unit`.
    /// `infer_block` / `check_block_against` consume this so a tail-less
    /// block whose last statement diverges types as `Never` rather than
    /// `Unit` — letting `let x = if c { v } else { return e; };` and the
    /// `match`-arm-block analog typecheck via the never-as-bottom
    /// coercion (phase-12 #12). Only `StmtKind::Expr` can carry
    /// divergence; every other statement form completes normally.
    pub(super) fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        if let StmtKind::Expr(expr) = &stmt.kind {
            return self.infer_expr(expr);
        }
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let {
                is_mut: _,
                pattern,
                ty,
                value,
            } => {
                let expected_ty = if let Some(ty_expr) = ty {
                    // Lower with the enclosing fn's DIM params in scope so a
                    // shape param `let p: Tensor[f32, [D]]` resolves `D`; type
                    // params stay `Named` (B-2026-07-13-5 leg B).
                    let scope = self.current_body_dim_scope.clone();
                    let declared = self.lower_type_expr(ty_expr, &scope);
                    self.check_expr(value, &declared);
                    declared
                } else {
                    let inferred = self.infer_expr(value);
                    self.check_unsolved_type_param(&inferred, &value.span);
                    inferred
                };
                // Per design.md: `let PAT = expr;` requires `PAT` to be
                // irrefutable (the binding has no else-arm; a missed
                // pattern would have nowhere to dispatch). Refutable
                // patterns must use `let ... else { … }` (which has its
                // own check at `StmtKind::LetElse`) or `if let` /
                // `while let`. The check inherits through `@` bindings
                // — `let x @ Option.Some(y) = opt` is rejected because
                // the inner `Option.Some(y)` is refutable.
                if !self.is_irrefutable_pattern(pattern, &expected_ty) {
                    self.type_error(
                        "refutable pattern in `let` binding; use `let ... else { ... }`, \
                         `if let`, or `match` for patterns that may not match"
                            .to_string(),
                        pattern.span.clone(),
                        TypeErrorKind::RefutablePattern,
                    );
                }
                self.bind_pattern_types(pattern, &expected_ty);
                // `@`-bearing let patterns additionally route through
                // `check_pattern_against` (the `if let` / `let-else`
                // path): it owns the cannot-double-consume rule
                // (`let x @ Foo { a } = foo` is the let-form of the
                // match-arm conflict) and records the `Ref` borrow
                // modes codegen's ref-shims need for `let ref x @ …`.
                // Gated on `contains_at_binding` so ordinary lets keep
                // the `bind_pattern_types`-only path unchanged.
                if pattern.contains_at_binding() {
                    let (mode, dispatch_ty) = ScrutineeMode::classify(&expected_ty);
                    let dispatch_ty = dispatch_ty.clone();
                    self.check_pattern_against(pattern, &dispatch_ty, mode);
                }
            }
            StmtKind::LetUninit {
                is_mut: _,
                name,
                name_span,
                ty,
            } => {
                let declared = self.lower_type_expr(ty, &[]);
                // Expose the declared type at the binding's name span so later
                // phases (ownership) can recover it without reaching into
                // `local_scope`. The Let arm above stores via bind_pattern_types;
                // LetUninit has no RHS so we record directly.
                self.expr_types
                    .insert(SpanKey::from_span(name_span), declared.clone());
                self.local_scope.insert(name.clone(), declared);
            }
            StmtKind::LetElse {
                pattern,
                ty,
                value,
                else_block,
            } => {
                let expected_ty = if let Some(ty_expr) = ty {
                    // Lower with the enclosing fn's DIM params in scope so a
                    // shape param `let p: Tensor[f32, [D]]` resolves `D`; type
                    // params stay `Named` (B-2026-07-13-5 leg B).
                    let scope = self.current_body_dim_scope.clone();
                    let declared = self.lower_type_expr(ty_expr, &scope);
                    self.check_expr(value, &declared);
                    declared
                } else {
                    let inferred = self.infer_expr(value);
                    self.check_unsolved_type_param(&inferred, &value.span);
                    inferred
                };
                // The else block runs on the NON-matching edge, so the
                // pattern's bindings are NOT in scope there — infer it first,
                // before binding the pattern. It must diverge.
                let else_ty = self.infer_block(else_block);
                if else_ty != Type::Never && else_ty != Type::Error {
                    self.type_error(
                        "let...else block must diverge (return, break, continue, or panic)"
                            .to_string(),
                        else_block.span.clone(),
                        TypeErrorKind::BranchTypeMismatch,
                    );
                }
                // Bind the pattern into the CURRENT scope (not a child scope) so
                // the bindings are live for the rest of the enclosing block.
                // Route through `check_pattern_against` — the same path `if let`
                // uses — so a variant pattern (`Some(x)`) extracts the payload
                // type for `x` and records `pattern_binding_types` for codegen.
                // (`bind_pattern_types`, the prior call, binds variant payloads
                // to `Type::Error`, which left `x` untyped.)
                let (mode, dispatch_ty) = ScrutineeMode::classify(&expected_ty);
                let dispatch_ty = dispatch_ty.clone();
                self.check_pattern_against(pattern, &dispatch_ty, mode);
            }
            StmtKind::Defer { body } => {
                let prev = self.in_defer;
                self.in_defer = true;
                self.infer_block(body);
                self.in_defer = prev;
            }
            StmtKind::ErrDefer { binding, body } => {
                let prev = self.in_defer;
                self.in_defer = true;
                // If errdefer(e), bind `e` in a new scope — typed as the Err
                // variant of the enclosing function's return type (stubbed as
                // Error for now since Result type is not yet fully implemented).
                if let Some(name) = binding {
                    self.local_scope.push();
                    self.local_scope.insert(name.clone(), Type::Error);
                }
                self.infer_block(body);
                if binding.is_some() {
                    self.local_scope.pop();
                }
                self.in_defer = prev;
            }
            StmtKind::Assign { target, value } => {
                // Reject `*r = v` when `r: ref T` — shared borrow is read-only.
                if let ExprKind::Unary {
                    op: UnaryOp::Deref,
                    operand,
                } = &target.kind
                {
                    let ref_ty = self.infer_expr(operand);
                    if matches!(ref_ty, Type::Ref(_)) {
                        self.type_error(
                            "cannot assign through a shared reference ('ref T'); use 'mut ref T'"
                                .to_string(),
                            target.span.clone(),
                            TypeErrorKind::InvalidUnaryOp,
                        );
                    }
                }
                // Slice 5 of design.md § Module-Level Bindings — reject
                // assignment to a module-level immutable `let`. The
                // ownership checker's local-binding mutability rule does
                // not cover module bindings; this check runs at the
                // typechecker layer so the error surfaces even if the
                // pipeline never reaches the ownership pass.
                self.check_module_binding_assignment(target);
                // Flag the immediate LHS as a place expression so the
                // union field-read gate (line 549 slice 2a) doesn't fire
                // on `u.field = ...`. `infer_field_access` captures this
                // on entry and resets to false for nested reads.
                let saved = self.assigning_lhs;
                self.assigning_lhs = true;
                let target_ty = self.infer_expr(target);
                self.assigning_lhs = saved;
                self.check_expr(value, &target_ty);
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.infer_expr(target);
                self.infer_expr(value);
            }
            // Handled by the early return above; kept for exhaustiveness.
            StmtKind::Expr(_) => {}
        }
        Type::Unit
    }

    /// `impl Trait` slice 4 — walk `return_ty` for every `TypeKind::ImplTrait`
    /// occurrence and record its capture set into
    /// `self.impl_trait_captures` keyed by the impl-trait node's
    /// `SpanKey` (the same key used by `Type::Existential::origin`).
    ///
    /// Capture-set rule per design.md § "Capture set — what the
    /// existential carries from the surrounding signature":
    /// 1. **Type-parameter captures** — every generic-param name in `gp`
    ///    that textually appears inside the existential's trait args
    ///    (e.g., `impl Iterator[Item = T]` captures `T`).
    /// 2. **Input-borrow captures** — when the existential's trait args
    ///    contain a `Ref`/`MutRef` whose source elides to function inputs,
    ///    every `ref`/`mut ref` input parameter is captured. Kāra's
    ///    `-> ref T` elision over-approximates to "all ref inputs" in the
    ///    multi-input case (see safety_design.rs § multi-source comment);
    ///    slice 4 mirrors that conservatism for existentials so the
    ///    borrow-checker integration reuses the existing "drop of
    ///    borrowed source" diagnostic at every captured input. `ref self`
    ///    / `mut ref self` count as a ref input under the name `self`.
    fn record_impl_trait_captures(&mut self, return_ty: &TypeExpr, f: &Function, gp: &[String]) {
        // Collect ref-input param names. A name-less destructuring
        // pattern can't be cited at a call-site capture diagnostic, so
        // we skip such params (they would not be reachable as a borrow
        // source in any case — the destructuring binds fresh sub-names).
        let mut ref_inputs: Vec<String> = Vec::new();
        for param in &f.params {
            if matches!(&param.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
                if let Some(name) = param.name() {
                    ref_inputs.push(name.to_string());
                }
            }
        }
        if matches!(f.self_param, Some(SelfParam::Ref) | Some(SelfParam::MutRef)) {
            ref_inputs.push("self".to_string());
        }
        let generic_param_names: std::collections::HashSet<String> = gp.iter().cloned().collect();

        Self::walk_for_impl_trait(return_ty, &mut |impl_trait_span, args| {
            let mut type_params: Vec<String> = Vec::new();
            let mut found_ref_in_args = false;
            for arg in args {
                if let GenericArg::Type(t) = arg {
                    Self::collect_capture_signals(
                        t,
                        &generic_param_names,
                        &mut type_params,
                        &mut found_ref_in_args,
                    );
                }
            }
            type_params.sort();
            type_params.dedup();
            let input_borrows = if found_ref_in_args {
                ref_inputs.clone()
            } else {
                Vec::new()
            };
            self.impl_trait_captures.insert(
                SpanKey::from_span(impl_trait_span),
                crate::typechecker::ImplTraitCaptures {
                    type_params,
                    input_borrows,
                },
            );
        });
    }

    /// Visit every `TypeKind::ImplTrait` node nested inside `ty`,
    /// invoking the callback with the impl-trait's span + its trait
    /// args. Argument-position `impl Trait` was already desugared away
    /// by slice 2, so the only occurrences here are return-position /
    /// RPITIT-return / TAIT-RHS / structurally-similar shapes.
    fn walk_for_impl_trait<F: FnMut(&Span, &[GenericArg])>(ty: &TypeExpr, f: &mut F) {
        match &ty.kind {
            TypeKind::ImplTrait {
                args,
                span: it_span,
                ..
            } => {
                f(it_span, args);
                for arg in args {
                    if let GenericArg::Type(t) = arg {
                        Self::walk_for_impl_trait(t, f);
                    }
                }
            }
            TypeKind::Tuple(types) => {
                for t in types {
                    Self::walk_for_impl_trait(t, f);
                }
            }
            TypeKind::Array { element, .. } => Self::walk_for_impl_trait(element, f),
            TypeKind::Pointer { inner, .. } => Self::walk_for_impl_trait(inner, f),
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Weak(inner) => {
                Self::walk_for_impl_trait(inner, f)
            }
            TypeKind::MutSlice(element) => Self::walk_for_impl_trait(element, f),
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                for p in params {
                    Self::walk_for_impl_trait(p, f);
                }
                if let Some(ret) = return_type {
                    Self::walk_for_impl_trait(ret, f);
                }
            }
            TypeKind::Path(p) => {
                if let Some(ref args) = p.generic_args {
                    for arg in args {
                        if let GenericArg::Type(t) = arg {
                            Self::walk_for_impl_trait(t, f);
                        }
                    }
                }
            }
            // `dyn Trait` slice 5: `dyn` is the complement of `impl` —
            // walk generic args for any nested `impl Trait` occurrences
            // (defensive — current slice 5 surface forbids `impl Trait`
            // nested under `dyn Trait` via the slice-1 NestedGenericArg
            // block, but the walk stays uniform with `Path`).
            TypeKind::Dyn { args, .. } => {
                for arg in args {
                    if let GenericArg::Type(t) = arg {
                        Self::walk_for_impl_trait(t, f);
                    }
                }
            }
            TypeKind::Unit | TypeKind::Error => {}
        }
    }

    /// Walk a single trait-arg type-expression collecting (a) the
    /// generic-param names that appear textually (added to
    /// `type_params`) and (b) whether any `Ref`/`MutRef` occurs (sets
    /// `found_ref`). The recursion descends through every kind that
    /// can carry nested type expressions.
    fn collect_capture_signals(
        ty: &TypeExpr,
        generic_param_names: &std::collections::HashSet<String>,
        type_params: &mut Vec<String>,
        found_ref: &mut bool,
    ) {
        match &ty.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
                *found_ref = true;
                Self::collect_capture_signals(inner, generic_param_names, type_params, found_ref);
            }
            TypeKind::MutSlice(inner) | TypeKind::Weak(inner) => {
                Self::collect_capture_signals(inner, generic_param_names, type_params, found_ref);
            }
            TypeKind::Path(p) => {
                if p.segments.len() == 1 && generic_param_names.contains(&p.segments[0]) {
                    type_params.push(p.segments[0].clone());
                }
                if let Some(ref args) = p.generic_args {
                    for arg in args {
                        if let GenericArg::Type(t) = arg {
                            Self::collect_capture_signals(
                                t,
                                generic_param_names,
                                type_params,
                                found_ref,
                            );
                        }
                    }
                }
            }
            TypeKind::Tuple(types) => {
                for t in types {
                    Self::collect_capture_signals(t, generic_param_names, type_params, found_ref);
                }
            }
            TypeKind::Array { element, .. } => {
                Self::collect_capture_signals(element, generic_param_names, type_params, found_ref);
            }
            TypeKind::Pointer { inner, .. } => {
                Self::collect_capture_signals(inner, generic_param_names, type_params, found_ref);
            }
            TypeKind::FnType {
                params,
                return_type,
                ..
            } => {
                for p in params {
                    Self::collect_capture_signals(p, generic_param_names, type_params, found_ref);
                }
                if let Some(ret) = return_type {
                    Self::collect_capture_signals(ret, generic_param_names, type_params, found_ref);
                }
            }
            TypeKind::ImplTrait { args, .. } => {
                for arg in args {
                    if let GenericArg::Type(t) = arg {
                        Self::collect_capture_signals(
                            t,
                            generic_param_names,
                            type_params,
                            found_ref,
                        );
                    }
                }
            }
            // `dyn Trait` slice 5: walk generic args for type-param /
            // ref-flow signals nested under the `dyn` surface so the
            // capture-set rule applies uniformly even though slice 5
            // rejects every `dyn Trait` use site.
            TypeKind::Dyn { args, .. } => {
                for arg in args {
                    if let GenericArg::Type(t) = arg {
                        Self::collect_capture_signals(
                            t,
                            generic_param_names,
                            type_params,
                            found_ref,
                        );
                    }
                }
            }
            TypeKind::Unit | TypeKind::Error => {}
        }
    }
}

/// Collect every `old(arg)` occurrence in a contract expression, returning
/// `(old_call_span, arg_expr)` for each. Walks the contract-expression
/// grammar; the arg of an `old(...)` is captured but not recursed into.
/// Used to validate `old(...)` placement and Clone-ability (design.md
/// § Contracts rule 4).
fn collect_old_calls(expr: &Expr, out: &mut Vec<(Span, Expr)>) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Identifier(n) = &callee.kind {
                if n == "old" && args.len() == 1 {
                    out.push((expr.span.clone(), args[0].value.clone()));
                    return;
                }
            }
            collect_old_calls(callee, out);
            for a in args {
                collect_old_calls(&a.value, out);
            }
        }
        ExprKind::Binary { left, right, .. } => {
            collect_old_calls(left, out);
            collect_old_calls(right, out);
        }
        ExprKind::Unary { operand, .. } => collect_old_calls(operand, out),
        ExprKind::FieldAccess { object, .. } => collect_old_calls(object, out),
        ExprKind::MethodCall { object, args, .. } => {
            collect_old_calls(object, out);
            for a in args {
                collect_old_calls(&a.value, out);
            }
        }
        ExprKind::Index { object, index } => {
            collect_old_calls(object, out);
            collect_old_calls(index, out);
        }
        _ => {}
    }
}

/// Returns `true` if `expr` references `self` (the `ExprKind::SelfValue`
/// keyword) anywhere *outside* an `old(...)` capture. Used to enforce the
/// consumed-parameter rule (design.md § Contracts rule 4): a bare-`self`
/// receiver is moved by the postcondition point, so the postcondition must
/// route any `self` reference through `old(...)`.
fn references_self_outside_old(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::SelfValue => true,
        ExprKind::Call { callee, args } => {
            // `old(self.field)` is the sanctioned form — its arg is fine.
            if let ExprKind::Identifier(n) = &callee.kind {
                if n == "old" && args.len() == 1 {
                    return false;
                }
            }
            references_self_outside_old(callee)
                || args.iter().any(|a| references_self_outside_old(&a.value))
        }
        ExprKind::Binary { left, right, .. } => {
            references_self_outside_old(left) || references_self_outside_old(right)
        }
        ExprKind::Unary { operand, .. } => references_self_outside_old(operand),
        ExprKind::FieldAccess { object, .. } => references_self_outside_old(object),
        ExprKind::MethodCall { object, args, .. } => {
            references_self_outside_old(object)
                || args.iter().any(|a| references_self_outside_old(&a.value))
        }
        ExprKind::Index { object, index } => {
            references_self_outside_old(object) || references_self_outside_old(index)
        }
        _ => false,
    }
}
