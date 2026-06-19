//! Type / path / pattern / effect / trait-bound resolution.
//!
//! Houses the inner-resolution helpers called from every per-item
//! resolver to chase references inside type expressions, patterns,
//! and effect lists:
//!
//! - `resolve_type_expr` — recursive descent over a `TypeExpr`
//!   (`Path`, `Ref`, `MutRef`, function types, tuples, etc.)
//! - `resolve_path_expr` — single path-expression resolution
//! - `resolve_trait_bound` — trait-bound path resolution
//! - `define_generic_params` — push generic-param TypeParam/ConstParam
//!   symbols + record their bounds
//! - `resolve_where_clause` — `where T: Bound + Bound2` traversal
//! - `resolve_pattern` — read-only walk that resolves variant /
//!   struct references inside a pattern
//! - `define_pattern_bindings` — write-side: push binding symbols
//!   for fresh names introduced by the pattern
//! - `resolve_effect_list` / `resolve_effect_verb` — effect path
//!   resolution + per-verb resource argument resolution
//!
//! Lives in a sibling `impl<'a> super::Resolver<'a>` block.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

use super::{ResolveError, ResolveErrorKind, SymbolId, SymbolKind};

impl<'a> super::Resolver<'a> {
    // ── Type resolution ─────────────────────────────────────────

    pub(crate) fn resolve_type_expr(&mut self, ty: &TypeExpr) {
        match &ty.kind {
            TypeKind::Path(path) => {
                self.resolve_path_expr(path);
            }
            TypeKind::Tuple(types) => {
                for t in types {
                    self.resolve_type_expr(t);
                }
            }
            TypeKind::Array { element, size } => {
                self.resolve_type_expr(element);
                self.resolve_expr(size);
            }
            TypeKind::Pointer { inner, .. } => {
                self.resolve_type_expr(inner);
            }
            TypeKind::FnType {
                params,
                return_type,
                effect_spec,
                is_once: _,
            } => {
                for p in params {
                    self.resolve_type_expr(p);
                }
                if let Some(ref ret) = return_type {
                    self.resolve_type_expr(ret);
                }
                if let Some(ref spec) = effect_spec {
                    match spec {
                        EffectSpec::Specific(list) => self.resolve_effect_list(list),
                        EffectSpec::Polymorphic => {}
                    }
                }
            }
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Weak(inner) => {
                self.resolve_type_expr(inner);
            }
            TypeKind::MutSlice(element) => {
                self.resolve_type_expr(element);
            }
            // `impl Trait` slice 1 stub: resolve the trait path and any
            // generic args / nested `use_effects` clauses analogously to
            // a `Path` type plus an effect-list. The argument-position
            // desugar into an anonymous generic parameter ships in
            // slice 2 (see phase-5-diagnostics.md line 395); until then
            // the resolver records the trait path so downstream
            // typechecker diagnostics can name the trait.
            TypeKind::ImplTrait {
                trait_path,
                args,
                use_effects,
                ..
            } => {
                self.resolve_path_expr(trait_path);
                for arg in args {
                    match arg {
                        GenericArg::Type(ty) => self.resolve_type_expr(ty),
                        GenericArg::Const(expr) => self.resolve_expr(expr),
                        GenericArg::Shape(_) => {
                            // Shape-literal dims resolve under the Dim/Shape kind
                            // system (Phase 11 Q1) — identifiers inside a shape literal
                            // name Dim-kinded / variadic shape params the resolver
                            // cannot see until that lands; deliberately not walked.
                        }
                    }
                }
                if let Some(list) = use_effects {
                    self.resolve_effect_list(list);
                }
            }
            // `dyn Trait` slice 5: resolve the trait path + nested
            // generic args so downstream typechecker diagnostics (the
            // RPITIT-conflict check and the P1-deferred stub) can name
            // the trait and any malformed nested types are surfaced.
            TypeKind::Dyn {
                trait_path, args, ..
            } => {
                self.resolve_path_expr(trait_path);
                for arg in args {
                    match arg {
                        GenericArg::Type(ty) => self.resolve_type_expr(ty),
                        GenericArg::Const(expr) => self.resolve_expr(expr),
                        GenericArg::Shape(_) => {
                            // Shape-literal dims resolve under the Dim/Shape kind
                            // system (Phase 11 Q1) — identifiers inside a shape literal
                            // name Dim-kinded / variadic shape params the resolver
                            // cannot see until that lands; deliberately not walked.
                        }
                    }
                }
            }
            TypeKind::Unit | TypeKind::Error => {}
        }
    }

    pub(crate) fn resolve_path_expr(&mut self, path: &PathExpr) {
        // Resolve the first segment as a type name
        if let Some(first) = path.segments.first() {
            if let Some(sym) = self.table.lookup(first) {
                let id = sym.id;
                self.record_resolution(&path.span, id);
            } else {
                self.error_undefined_type(first, path.span.clone());
            }
        }
        // Resolve generic args
        if let Some(ref args) = path.generic_args {
            for arg in args {
                match arg {
                    GenericArg::Type(ty) => self.resolve_type_expr(ty),
                    GenericArg::Const(expr) => self.resolve_expr(expr),
                    GenericArg::Shape(_) => {
                        // Shape-literal dims resolve under the Dim/Shape kind
                        // system (Phase 11 Q1) — identifiers inside a shape literal
                        // name Dim-kinded / variadic shape params the resolver
                        // cannot see until that lands; deliberately not walked.
                    }
                }
            }
        }
    }

    /// Resolve the trait name and any generic args inside a `TraitBound`.
    /// Records a resolution for the trait path when found. Undefined trait
    /// names are *not* reported here — the typechecker emits a more specific
    /// "unknown trait" diagnostic during bound validation, and double-erroring
    /// would be noise.
    pub(crate) fn resolve_trait_bound(&mut self, bound: &TraitBound) {
        if let Some(first) = bound.path.first() {
            if let Some(sym) = self.table.lookup(first) {
                let id = sym.id;
                self.record_resolution(&bound.span, id);
            }
        }
        if let Some(ref args) = bound.generic_args {
            for arg in args {
                match arg {
                    GenericArg::Type(ty) => self.resolve_type_expr(ty),
                    GenericArg::Const(expr) => self.resolve_expr(expr),
                    GenericArg::Shape(_) => {
                        // Shape-literal dims resolve under the Dim/Shape kind
                        // system (Phase 11 Q1) — identifiers inside a shape literal
                        // name Dim-kinded / variadic shape params the resolver
                        // cannot see until that lands; deliberately not walked.
                    }
                }
            }
        }
    }

    /// Define each generic param as a `TypeParam` symbol and record its inline
    /// bounds. Trait paths in bounds are resolved so they appear in the
    /// resolution map. Returns the mapping from param name to defined SymbolId
    /// (used by where-clause resolution to merge clause-level bounds in).
    pub(crate) fn define_generic_params(
        &mut self,
        generics: &GenericParams,
    ) -> HashMap<String, SymbolId> {
        let mut by_name = HashMap::new();
        for param in &generics.params {
            let kind = if param.is_const {
                SymbolKind::ConstParam
            } else {
                SymbolKind::TypeParam
            };
            match self
                .table
                .define(param.name.clone(), kind, param.span.clone(), false)
            {
                Ok(id) => {
                    self.table.record_generic_bounds(id, &param.bounds);
                    by_name.insert(param.name.clone(), id);
                }
                Err(e) => self.errors.push(e),
            }
            for bound in &param.bounds {
                self.resolve_trait_bound(bound);
            }
            // Const params reference their declared type via the source AST;
            // resolve the type expression so its references appear in the
            // resolution map alongside other resolved type expressions.
            if let Some(ty) = &param.const_type {
                self.resolve_type_expr(ty);
            }
        }
        by_name
    }

    /// Walk a where clause and merge `where T: Bound` constraints into the
    /// existing generic-param bound map. `params_by_name` lets the helper map
    /// the textual `T` to the freshly-defined param SymbolId without searching
    /// scopes (which could match an unrelated outer `T` shadowed by ours).
    /// Trait paths in bounds and equality RHS types are resolved so references
    /// land in the resolution map.
    pub(crate) fn resolve_where_clause(
        &mut self,
        where_clause: &WhereClause,
        params_by_name: &HashMap<String, SymbolId>,
    ) {
        for constraint in &where_clause.constraints {
            match constraint {
                WhereConstraint::TypeBound {
                    type_name, bounds, ..
                } => {
                    if let Some(&param_id) = params_by_name.get(type_name) {
                        self.table.record_generic_bounds(param_id, bounds);
                    }
                    for bound in bounds {
                        self.resolve_trait_bound(bound);
                    }
                }
                WhereConstraint::AssocTypeEq { ty, .. } => {
                    self.resolve_type_expr(ty);
                }
                WhereConstraint::ProjectionBound {
                    projection, bounds, ..
                } => {
                    // Resolve the projection's receiver-and-assoc path so
                    // the receiver type-param lands in the resolution map.
                    // GAT slice 8a: bounds carry the trait-bound paths
                    // (e.g., `FromIterator[i64]`); they also resolve so
                    // the trait reference is recorded.
                    self.resolve_type_expr(projection);
                    for bound in bounds {
                        self.resolve_trait_bound(bound);
                    }
                }
                WhereConstraint::ConstPredicate { expr, .. } => {
                    self.resolve_expr(expr);
                }
            }
        }
    }

    // ── Pattern resolution ──────────────────────────────────────

    pub(crate) fn resolve_pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Wildcard => {}
            PatternKind::Binding(name) => {
                let _ = self.table.define(
                    name.clone(),
                    SymbolKind::Variable { is_mut: false },
                    pattern.span.clone(),
                    false,
                );
            }
            PatternKind::Literal(_) => {}
            PatternKind::Struct {
                path,
                fields,
                has_rest: _, // `..` rest binds nothing — the resolver only
                             // needs to walk named-field sub-patterns.
            } => {
                // Resolve the struct/variant path
                if let Some(first) = path.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&pattern.span, id);
                    } else {
                        self.error_undefined_name(first, pattern.span.clone());
                    }
                }
                // Define field bindings
                for field in fields {
                    if let Some(ref sub_pattern) = field.pattern {
                        self.resolve_pattern(sub_pattern);
                    } else {
                        // Shorthand: field name becomes binding
                        let _ = self.table.define(
                            field.name.clone(),
                            SymbolKind::Variable { is_mut: false },
                            field.span.clone(),
                            false,
                        );
                    }
                }
            }
            PatternKind::TupleVariant { path, patterns } => {
                // Resolve the variant path
                if let Some(first) = path.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&pattern.span, id);
                    } else {
                        self.error_undefined_name(first, pattern.span.clone());
                    }
                }
                for p in patterns {
                    self.resolve_pattern(p);
                }
            }
            PatternKind::Tuple(patterns) => {
                for p in patterns {
                    self.resolve_pattern(p);
                }
            }
            PatternKind::Or(alternatives) => {
                for alt in alternatives {
                    self.resolve_pattern(alt);
                }
            }
            PatternKind::RangePattern { .. } => {
                // No bindings to define
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                let _ = self.table.define(
                    name.clone(),
                    SymbolKind::Variable { is_mut: false },
                    pattern.span.clone(),
                    false,
                );
                self.resolve_pattern(pattern);
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix {
                    self.resolve_pattern(p);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    let _ = self.table.define(
                        name.clone(),
                        SymbolKind::Variable { is_mut: false },
                        pattern.span.clone(),
                        false,
                    );
                }
                for p in suffix {
                    self.resolve_pattern(p);
                }
            }
        }
    }

    /// Define bindings from a let-pattern (used for `let` statements).
    /// Define the bindings introduced by a non-`let` pattern (function
    /// params, closure params, `for`-loop variables). No top-level shadowing:
    /// a name already bound in the current scope is a duplicate-definition
    /// error (e.g. `fn f(x: i64, x: i64)`).
    pub(crate) fn define_pattern_bindings(&mut self, pattern: &Pattern, is_mut: bool) {
        let mut bound = std::collections::HashSet::new();
        self.define_pattern_bindings_inner(pattern, is_mut, false, &mut bound);
    }

    /// Define the bindings of a `let`/`let mut` pattern. A *top-level*
    /// re-binding of a name already present in the current scope **shadows**
    /// it (creates a fresh binding) rather than erroring — design.md
    /// § Variables > Shadowing. Duplicate binders *within the same pattern*
    /// (e.g. `let (a, a) = ...`) are still rejected, tracked via `bound`.
    pub(crate) fn define_let_bindings(&mut self, pattern: &Pattern, is_mut: bool) {
        let mut bound = std::collections::HashSet::new();
        self.define_pattern_bindings_inner(pattern, is_mut, true, &mut bound);
    }

    /// Define a single leaf binder. When `allow_shadow` is set, an existing
    /// same-scope binding is shadowed; otherwise it is a duplicate error.
    /// In both modes, a name that already appears *in this pattern* (tracked
    /// in `bound`) is a duplicate-binder error.
    fn define_binding_leaf(
        &mut self,
        name: &str,
        is_mut: bool,
        span: Span,
        allow_shadow: bool,
        bound: &mut std::collections::HashSet<String>,
    ) {
        if !bound.insert(name.to_string()) {
            self.errors.push(ResolveError {
                message: format!("'{}' is bound more than once in the same pattern", name),
                span,
                kind: ResolveErrorKind::DuplicateDefinition,
                suggestion: None,
                replacement: None,
                stub_hint: None,
            });
            return;
        }
        let result = if allow_shadow {
            self.table.define_shadowable(
                name.to_string(),
                SymbolKind::Variable { is_mut },
                span,
                false,
            )
        } else {
            self.table.define(
                name.to_string(),
                SymbolKind::Variable { is_mut },
                span,
                false,
            )
        };
        if let Err(e) = result {
            self.errors.push(e);
        }
    }

    fn define_pattern_bindings_inner(
        &mut self,
        pattern: &Pattern,
        is_mut: bool,
        allow_shadow: bool,
        bound: &mut std::collections::HashSet<String>,
    ) {
        match &pattern.kind {
            PatternKind::Binding(name) => {
                self.define_binding_leaf(name, is_mut, pattern.span.clone(), allow_shadow, bound);
            }
            PatternKind::Struct {
                path,
                fields,
                has_rest: _,
            } => {
                // Resolve the struct name
                if let Some(first) = path.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&pattern.span, id);
                    } else {
                        self.error_undefined_name(first, pattern.span.clone());
                    }
                }
                for field in fields {
                    if let Some(ref sub_pattern) = field.pattern {
                        self.define_pattern_bindings_inner(
                            sub_pattern,
                            is_mut,
                            allow_shadow,
                            bound,
                        );
                    } else {
                        self.define_binding_leaf(
                            &field.name,
                            is_mut,
                            field.span.clone(),
                            allow_shadow,
                            bound,
                        );
                    }
                }
            }
            PatternKind::TupleVariant { path, patterns } => {
                if let Some(first) = path.first() {
                    if let Some(sym) = self.table.lookup(first) {
                        let id = sym.id;
                        self.record_resolution(&pattern.span, id);
                    } else {
                        self.error_undefined_name(first, pattern.span.clone());
                    }
                }
                for p in patterns {
                    self.define_pattern_bindings_inner(p, is_mut, allow_shadow, bound);
                }
            }
            PatternKind::Tuple(patterns) => {
                for p in patterns {
                    self.define_pattern_bindings_inner(p, is_mut, allow_shadow, bound);
                }
            }
            PatternKind::Wildcard | PatternKind::Literal(_) | PatternKind::RangePattern { .. } => {}
            PatternKind::Or(alternatives) => {
                // Bindings from first alternative (all alts should bind same names)
                if let Some(first) = alternatives.first() {
                    self.define_pattern_bindings_inner(first, is_mut, allow_shadow, bound);
                }
            }
            PatternKind::AtBinding { name, pattern, .. } => {
                self.define_binding_leaf(name, is_mut, pattern.span.clone(), allow_shadow, bound);
                self.define_pattern_bindings_inner(pattern, is_mut, allow_shadow, bound);
            }
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix {
                    self.define_pattern_bindings_inner(p, is_mut, allow_shadow, bound);
                }
                if let Some(RestPattern::Bound(name)) = rest {
                    self.define_binding_leaf(
                        name,
                        is_mut,
                        pattern.span.clone(),
                        allow_shadow,
                        bound,
                    );
                }
                for p in suffix {
                    self.define_pattern_bindings_inner(p, is_mut, allow_shadow, bound);
                }
            }
        }
    }

    // ── Effect resolution ───────────────────────────────────────

    pub(crate) fn resolve_effect_list(&mut self, effects: &EffectList) {
        for item in &effects.items {
            match item {
                EffectItem::Verb(verb) => {
                    self.resolve_effect_verb(verb);
                }
                EffectItem::Group(name) => {
                    if let Some(sym) = self.table.lookup(name) {
                        let id = sym.id;
                        self.record_resolution(&effects.span, id);
                    } else {
                        self.error_undefined_name(name, effects.span.clone());
                    }
                }
                EffectItem::Polymorphic => {}
                EffectItem::Variable(_) => {} // declared in [with E]; no resolution needed
            }
        }
    }

    pub(crate) fn resolve_effect_verb(&mut self, verb: &EffectVerb) {
        for resource in &verb.resources {
            let name = resource.path.join(".");
            let first = resource.path.first().map(|s| s.as_str()).unwrap_or("");
            if let Some(sym) = self.table.lookup(first) {
                // Phase-10 (`std.web` gating): the symbol must actually be
                // resource-shaped. Without this check any in-scope name
                // satisfies a verb clause — `writes(Display)` in native
                // code silently resolved against the prelude `Display`
                // (fmt) TRAIT instead of erroring until `std.web.Display`
                // is imported, making the module gate hollow for every
                // colliding name. `Import` is accepted as-is: in
                // single-file mode there is no tree to chase the target's
                // kind through, and in tree mode the import-site
                // validation already confirmed the item exists.
                let kind_label = match &sym.kind {
                    SymbolKind::EffectResource | SymbolKind::Import { .. } => None,
                    // Dotted resource paths (`reads(net.Conn)`) resolve
                    // their first segment to a module binding.
                    SymbolKind::Module if resource.path.len() > 1 => None,
                    SymbolKind::Trait { .. } | SymbolKind::TraitAlias => Some("a trait"),
                    SymbolKind::Struct { .. } => Some("a struct"),
                    SymbolKind::Enum { .. } => Some("an enum"),
                    SymbolKind::Union { .. } => Some("a union"),
                    SymbolKind::Function { .. } | SymbolKind::ExternFunction => {
                        Some("a function")
                    }
                    // Scope-0 registers every prelude type AND trait as
                    // `Primitive` (`register_prelude_symbols`) — this is
                    // the arm the `Display`-collision case lands in.
                    SymbolKind::Primitive => Some("a prelude type or trait"),
                    SymbolKind::Variable { .. } => Some("a variable"),
                    SymbolKind::EffectGroup => Some(
                        "an effect group — groups appear bare in a `with` clause, not inside a verb",
                    ),
                    _ => Some("not a resource declaration"),
                };
                if let Some(kind_label) = kind_label {
                    // Guidance lives in the message — `suggestion` renders
                    // as a `did you mean \`X\`?` name replacement, which
                    // has no sensible value here.
                    self.errors.push(ResolveError {
                        message: format!(
                            "'{}' is not an effect resource (it is {}); declare `effect resource {};` or import one (e.g. `import std.web.{};`)",
                            name, kind_label, first, first
                        ),
                        span: resource.span.clone(),
                        kind: ResolveErrorKind::UndefinedName,
                        suggestion: None,
                        replacement: None,
                        stub_hint: None,
                    });
                    continue;
                }
                let id = sym.id;
                self.record_resolution(&resource.span, id);
            } else {
                self.errors.push(ResolveError {
                    message: format!("undefined effect resource '{}'", name),
                    span: resource.span.clone(),
                    kind: ResolveErrorKind::UndefinedName,
                    suggestion: None,
                    replacement: None,
                    stub_hint: None,
                });
            }
            // Resolve parameterized resource expression
            if let Some(ref param_expr) = resource.param {
                self.resolve_expr(param_expr);
            }
        }
    }
}
