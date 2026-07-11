//! Generic-call monomorphization + per-K/V Map specialization.
//!
//! Houses the generic-function compilation pipeline (
//! `compile_generic_call`, `declare_mono_function`, `compile_mono_function`,
//! `infer_type_args`, `unify_type_expr`, `is_known_concrete_type`,
//! `mangle_mono_name`, `verify_bounds_at_codegen`,
//! `llvm_type_satisfies_trait`, `llvm_type_to_mangle_str`)
//! and the per-(K, V) `Map[K, V]` method monomorphization that
//! emits inlined hash / probe / load functions to short-circuit
//! the erased `karac_map_*` runtime path (`mono_map_cache_key`,
//! `should_use_mono_map_for`, `get_or_emit_map_mono_methods`,
//! `emit_mono_map_insert_old_body`, `emit_mono_map_get_body`).

use crate::ast::*;
use std::collections::HashMap;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::helpers::{const_value_from_literal_expr, const_value_to_mangle_str};
use super::state::{LayoutId, MapMonoMethods, VarSlot};

/// Snapshot of every name-keyed per-function variable side-table that
/// `register_var_from_type_expr` (plus the mono prologue's `Fn`-param /
/// owned-header registrations) can write. A mono body compiles INLINE,
/// mid-caller, so these must be swapped to a clean slate for the body and
/// restored after — the same isolation `variables` / `var_type_names` /
/// `tensor_var_infos` already had. Before this existed, only
/// `tensor_var_infos` was saved: mono #1's `c → Column[i64]` entry leaked
/// into mono #2 where `c` was a Tensor param, so the Column intercept
/// compiled a column reduce over a tensor handle (SIGSEGV; found by S6a's
/// two-instantiation `report[C: Reduce[i64]]` probe — the fallout of
/// B-2026-07-02-11's full-registration prologue). Module-binding entries
/// survive the swap via `reseed_module_binding_side_tables` in
/// `compile_mono_function`.
pub(super) struct SavedVarSideTables<'ctx> {
    column_var_infos: HashMap<String, super::state::ColumnVarInfo<'ctx>>,
    dataframe_var_infos: std::collections::HashSet<String>,
    vec_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    var_elem_type_exprs: HashMap<String, TypeExpr>,
    enum_inst_var_types: HashMap<String, TypeExpr>,
    string_vars: std::collections::HashSet<String>,
    slice_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    map_key_types: HashMap<String, BasicTypeEnum<'ctx>>,
    map_val_types: HashMap<String, BasicTypeEnum<'ctx>>,
    map_key_type_names: HashMap<String, String>,
    map_key_type_exprs: HashMap<String, TypeExpr>,
    set_elem_types: HashMap<String, BasicTypeEnum<'ctx>>,
    set_elem_type_names: HashMap<String, String>,
    set_elem_type_exprs: HashMap<String, TypeExpr>,
    atomic_var_inner_is_bool: std::collections::HashSet<String>,
    owned_vecstr_params: std::collections::HashSet<String>,
    closure_fn_types: HashMap<String, inkwell::types::FunctionType<'ctx>>,
}

impl<'ctx> super::Codegen<'ctx> {
    /// For each param of a generic fn whose declared type (ref-peeled) is
    /// a BARE type param of that fn, look up the matching call arg's span
    /// in the Column/Tensor typed-expr side-tables. Such an argument's
    /// LLVM value type is an opaque `ptr` — `infer_type_args` can neither
    /// tell Column from Tensor nor recover the element type — so this is
    /// the only channel that lets the mono register the param for the
    /// builtin method intercepts (and lets the mangle distinguish the
    /// instantiations). See `state::MonoHandleArgInfo`.
    fn collect_mono_handle_params(
        &self,
        func: &Function,
        args: &[CallArg],
    ) -> Vec<(String, super::state::MonoHandleArgInfo)> {
        let Some(gp) = &func.generic_params else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            if path.segments.len() != 1 || path.generic_args.is_some() {
                continue;
            }
            let name = &path.segments[0];
            if !gp.params.iter().any(|p| !p.is_const && &p.name == name) {
                continue;
            }
            let Some(param_name) = param.name() else {
                continue;
            };
            let key = (arg.value.span.offset, arg.value.span.length);
            if let Some(ci) = self.column_typed_exprs.get(&key) {
                out.push((
                    param_name.to_string(),
                    super::state::MonoHandleArgInfo::Column(ci.clone()),
                ));
            } else if let Some(ti) = self.tensor_typed_exprs.get(&key) {
                out.push((
                    param_name.to_string(),
                    super::state::MonoHandleArgInfo::Tensor(ti.clone()),
                ));
            }
        }
        out
    }

    /// Bind a handle-backed-container type param (`C` under `c: ref C` where
    /// the arg is a `Column`/`Tensor`) to its `ptr` LLVM shape in the mono
    /// subst. `infer_type_args` can't recover this — the element is erased and
    /// the arg value is a bare `ptr` — so a bare-type-param appearing in the
    /// RETURN position (`fn f[C: ElementwiseMap[i64]](c: ref C) -> C`, i.e.
    /// `map`/`zip_with` returning `Self`) or in a `let d: C` local would fall
    /// through `llvm_type_for_name`'s `i64` default and mis-declare the mono's
    /// return type ("Function return type does not match operand type of return
    /// inst" — a `ret ptr` in an `i64`-returning fn). Column-vs-Tensor
    /// discrimination stays in `mono_handle_param_infos`; the LLVM SHAPE is
    /// `ptr` for both, so binding the shape here is unambiguous. `entry().
    /// or_insert` so a genuine `infer_type_args` binding is never overwritten.
    fn augment_subst_from_handle_params(
        &self,
        func: &Function,
        args: &[CallArg],
        subst: &mut HashMap<String, BasicTypeEnum<'ctx>>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default()).into();
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            if path.segments.len() != 1 || path.generic_args.is_some() {
                continue;
            }
            let name = &path.segments[0];
            if !gp.params.iter().any(|p| !p.is_const && &p.name == name) {
                continue;
            }
            let key = (arg.value.span.offset, arg.value.span.length);
            if self.column_typed_exprs.contains_key(&key)
                || self.tensor_typed_exprs.contains_key(&key)
            {
                // OVERWRITE, not `or_insert`: `infer_type_args` already bound
                // this handle param to the `i64` default (a `Column`/`Tensor`
                // arg is a bare `ptr` it can't resolve), and `ptr` is the one
                // correct LLVM shape for a handle-backed container.
                subst.insert(name.clone(), ptr_ty);
            }
        }
    }

    /// Bind container-element type params from an identifier arg's
    /// registered element type (`vec_elem_types` / `slice_elem_types` /
    /// `set_elem_types` / `map_key_types` / `map_val_types`). Complements
    /// `call_type_subs` for the nested-call case the typechecker drops as a
    /// self-referential `T -> T` binding. Only fills gaps `infer_type_args`
    /// / the `call_type_subs` augmentation left, and only when the param's
    /// (ref-peeled) declared element is a bare type param of `func` — so a
    /// concrete `Vec[i64]` param binds nothing. No-op unless the arg is a
    /// plain identifier with a registered element type.
    fn augment_subst_from_arg_elem_types(
        &self,
        func: &Function,
        args: &[CallArg],
        subst: &mut HashMap<String, BasicTypeEnum<'ctx>>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let ExprKind::Identifier(arg_name) = &arg.value.kind else {
                continue;
            };
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            let head = path.segments.last().map(|s| s.as_str()).unwrap_or("");
            let gargs = match &path.generic_args {
                Some(g) => g,
                None => continue,
            };
            // Name of the element/key/value type param at a given generic-arg
            // position, if it is a bare type param of `func`.
            let param_at = |idx: usize| -> Option<String> {
                match gargs.get(idx)? {
                    GenericArg::Type(te) => {
                        if let TypeKind::Path(p) = &te.kind {
                            if p.segments.len() == 1 && p.generic_args.is_none() {
                                let n = p.segments[0].clone();
                                if is_param(&n) {
                                    return Some(n);
                                }
                            }
                        }
                        None
                    }
                    _ => None,
                }
            };
            match head {
                "Vec" | "VecDeque" => {
                    if let (Some(pn), Some(&elem)) =
                        (param_at(0), self.vec_elem_types.get(arg_name.as_str()))
                    {
                        subst.entry(pn).or_insert(elem);
                    }
                }
                "Slice" => {
                    // A `Slice[T]` param accepts a `Vec` / `Array` / `Slice`
                    // arg (the by-value coercion of B-2026-07-03-9), so the
                    // element type may live in `vec_elem_types` (a `Vec` arg)
                    // or the `variables` array slot (an `Array` arg), not just
                    // `slice_elem_types`. `infer_elem_from_source` unifies all
                    // three. Keying only on `slice_elem_types` left `T` unbound
                    // for the common `gsum[T](s: Slice[T])` called with a
                    // `Vec[String]` — `T` then defaulted to `i64`, so `s[0]`
                    // read the String's 8-byte ptr field as an integer (the
                    // returned value printed as a raw pointer). The i64/Array
                    // cases masked it: an unbound `T` defaults to `i64`, which
                    // matched those element types by luck (B-2026-07-03-22).
                    if let (Some(pn), Some(elem)) =
                        (param_at(0), self.infer_elem_from_source(&arg.value))
                    {
                        subst.entry(pn).or_insert(elem);
                    }
                }
                "Set" => {
                    if let (Some(pn), Some(&elem)) =
                        (param_at(0), self.set_elem_types.get(arg_name.as_str()))
                    {
                        subst.entry(pn).or_insert(elem);
                    }
                }
                "Map" => {
                    if let (Some(kn), Some(&kty)) =
                        (param_at(0), self.map_key_types.get(arg_name.as_str()))
                    {
                        subst.entry(kn).or_insert(kty);
                    }
                    if let (Some(vn), Some(&vty)) =
                        (param_at(1), self.map_val_types.get(arg_name.as_str()))
                    {
                        subst.entry(vn).or_insert(vty);
                    }
                }
                _ => {}
            }
        }
    }

    /// Append the handle-arg axis to a mangled mono name:
    /// `$<param>_col_<elem>` / `$<param>_ten_<elem>_<d0>_...` (dynamic
    /// dims mangle as `x`). Without this, `report[C](c: ref C)` called
    /// with a `Column[i64]` and then a `Tensor[i64, [4]]` mangles both
    /// instantiations to the same symbol (both args are `ptr`), so the
    /// second call reuses the first body and miscompiles.
    fn append_handle_mangle(
        &self,
        mut mangled: String,
        handle_params: &[(String, super::state::MonoHandleArgInfo)],
    ) -> String {
        use std::fmt::Write as _;
        for (pname, info) in handle_params {
            match info {
                super::state::MonoHandleArgInfo::Column(ci) => {
                    let elem = Self::type_expr_mangle_seg(&ci.elem);
                    let _ = write!(mangled, "${pname}_col_{elem}");
                }
                super::state::MonoHandleArgInfo::Tensor(ti) => {
                    let elem = Self::type_expr_mangle_seg(&ti.elem);
                    let _ = write!(mangled, "${pname}_ten_{elem}");
                    for d in &ti.dims {
                        match d {
                            Some(n) => {
                                let _ = write!(mangled, "_{n}");
                            }
                            None => mangled.push_str("_x"),
                        }
                    }
                }
            }
        }
        mangled
    }

    /// Last path segment of a (concrete, primitive-element) TypeExpr for
    /// mangling — `i64`, `f64`, `bool`, `u32`, ….
    fn type_expr_mangle_seg(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Path(p) => p
                .segments
                .last()
                .cloned()
                .unwrap_or_else(|| "e".to_string()),
            _ => "e".to_string(),
        }
    }

    /// Swap out the name-keyed variable side-tables for a nested mono
    /// compile. Pair with [`Self::restore_var_side_tables`].
    pub(super) fn take_var_side_tables(&mut self) -> SavedVarSideTables<'ctx> {
        SavedVarSideTables {
            column_var_infos: std::mem::take(&mut self.column_var_infos),
            dataframe_var_infos: std::mem::take(&mut self.dataframe_var_infos),
            vec_elem_types: std::mem::take(&mut self.vec_elem_types),
            var_elem_type_exprs: std::mem::take(&mut self.var_elem_type_exprs),
            enum_inst_var_types: std::mem::take(&mut self.enum_inst_var_types),
            string_vars: std::mem::take(&mut self.string_vars),
            slice_elem_types: std::mem::take(&mut self.slice_elem_types),
            map_key_types: std::mem::take(&mut self.map_key_types),
            map_val_types: std::mem::take(&mut self.map_val_types),
            map_key_type_names: std::mem::take(&mut self.map_key_type_names),
            map_key_type_exprs: std::mem::take(&mut self.map_key_type_exprs),
            set_elem_types: std::mem::take(&mut self.set_elem_types),
            set_elem_type_names: std::mem::take(&mut self.set_elem_type_names),
            set_elem_type_exprs: std::mem::take(&mut self.set_elem_type_exprs),
            atomic_var_inner_is_bool: std::mem::take(&mut self.atomic_var_inner_is_bool),
            owned_vecstr_params: std::mem::take(&mut self.owned_vecstr_params),
            closure_fn_types: std::mem::take(&mut self.closure_fn_types),
        }
    }

    /// Restore the caller's side-tables after a nested mono compile.
    pub(super) fn restore_var_side_tables(&mut self, saved: SavedVarSideTables<'ctx>) {
        self.column_var_infos = saved.column_var_infos;
        self.dataframe_var_infos = saved.dataframe_var_infos;
        self.vec_elem_types = saved.vec_elem_types;
        self.var_elem_type_exprs = saved.var_elem_type_exprs;
        self.enum_inst_var_types = saved.enum_inst_var_types;
        self.string_vars = saved.string_vars;
        self.slice_elem_types = saved.slice_elem_types;
        self.map_key_types = saved.map_key_types;
        self.map_val_types = saved.map_val_types;
        self.map_key_type_names = saved.map_key_type_names;
        self.map_key_type_exprs = saved.map_key_type_exprs;
        self.set_elem_types = saved.set_elem_types;
        self.set_elem_type_names = saved.set_elem_type_names;
        self.set_elem_type_exprs = saved.set_elem_type_exprs;
        self.atomic_var_inner_is_bool = saved.atomic_var_inner_is_bool;
        self.owned_vecstr_params = saved.owned_vecstr_params;
        self.closure_fn_types = saved.closure_fn_types;
    }

    pub(super) fn compile_generic_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        explicit_generic_args: Option<&[GenericArg]>,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let generic_fn = self.generic_fns[name].clone();

        // Compile argument values so we can infer concrete types.
        // B-2026-07-02-13: cleared pending-let hint for the arg compiles —
        // argument literals pack at their own span-recorded (callee-declared)
        // width, not the let binding's. Mirrors the `compile_call` user-fn
        // arg loop; see `literal_span_elem_hint` for the precedence story.
        let saved_pending_elem = self.pending_let_elem_type.take();
        let saved_pending_elem_te = self.pending_let_elem_type_expr.take();
        let arg_vals: Result<Vec<BasicValueEnum<'ctx>>, String> =
            args.iter().map(|a| self.compile_expr(&a.value)).collect();
        self.pending_let_elem_type = saved_pending_elem;
        self.pending_let_elem_type_expr = saved_pending_elem_te;
        let arg_vals: Vec<BasicValueEnum<'ctx>> = arg_vals?;

        // B-2026-07-08-6 (generic/mono leg) — caller-side arg-temp drop, the
        // twin of the mono param entry-copy above. The monomorph body now
        // ENTRY-COPIES an owned heap struct/enum param and returns an
        // INDEPENDENT copy, so — exactly as in the non-generic `compile_call`
        // path — the caller must drop the ORIGINAL moved-in arg buffer of an
        // inline aggregate temp (struct/tuple literal, enum-variant ctor), else
        // it is orphaned. Same gate as `compile_call`: skip only when the
        // callee FORWARDS the arg (`call_arg_flows_into_return`) AND does not
        // entry-copy it — an entry-copied heap struct arg is registered even on
        // the return-passthrough path. `track_inline_owned_aggregate_arg`
        // self-restricts to inline temps (identifier args keep their binding's
        // drop; the fstr→struct move is already suppressed inside
        // `compile_expr`), so nothing is double-registered. Runs here in the
        // CALLER's context (before the mono body is compiled inline below,
        // which swaps `scope_cleanup_actions`).
        for (i, a) in args.iter().enumerate() {
            let val = arg_vals[i];
            if !self.call_arg_flows_into_return(name, i)
                || self.arg_is_entry_copied_heap_struct(&a.value)
            {
                self.track_inline_owned_aggregate_arg(val, &a.value);
            }
        }

        // Infer type arguments from the argument value types.
        let mut subst = self.infer_type_args(&generic_fn, &arg_vals);

        // B-2026-07-02-41: augment the LLVM-type-based subst with the
        // typechecker's recorded per-call type args. `infer_type_args`
        // binds only bare-`T` params — a container's `{ptr,len,cap}` LLVM
        // shape is element-erased, so a `ref Vec[T]` / `Column[T]` param
        // leaves `T` unbound, and two element-type instantiations then
        // mangle identically and share one (wrong) monomorph (the second
        // call reads the first's element width). The recorded frame names
        // the concrete element type; resolve it through the active
        // `type_subst` (so a nested generic call inside a mono flattens the
        // outer `T`) via `llvm_type_for_name`, filling only the gaps
        // `infer_type_args` couldn't bind — the explicit-generic-args pass
        // below still overrides. Also feeds the mangle (via `subst`), so
        // the two instantiations become distinct symbols.
        // Name-level twin of `subst` (B-2026-07-03-11): the concrete type
        // *name* each generic param resolves to, so the mono param prologue can
        // register a bare-type-param receiver (`x: X`) under its concrete type
        // (`var_type_names["x"] = "C"`) and dispatch a trait method called
        // through the bound (`x.tag()` → `C.tag`). Built from the same
        // `call_type_subs` frame, resolving each recorded name through the
        // caller's active `type_subst_names` so a nested generic call flattens
        // an outer param (mirrors the LLVM `type_subst` resolution just below).
        let mut subst_names: HashMap<String, String> = HashMap::new();
        if let Some(frame) = self
            .call_type_subs
            .get(&(call_span.offset, call_span.length))
            .cloned()
        {
            for (param_name, concrete_name) in frame {
                // Flatten through the caller's active name-subst: a recorded
                // name that is itself an outer generic param resolves to the
                // outer param's concrete binding.
                let resolved = self
                    .type_subst_names
                    .get(&concrete_name)
                    .cloned()
                    .unwrap_or(concrete_name);
                subst_names.insert(param_name.clone(), resolved.clone());
                if let std::collections::hash_map::Entry::Vacant(e) = subst.entry(param_name) {
                    let llvm = self.llvm_type_for_name(&resolved);
                    e.insert(llvm);
                }
            }
        }
        // Container-element fallback for the nested-call case the
        // typechecker can't record: inside a mono `wrap[T](v: ref Vec[T])`
        // the inner call `first(v)` resolves first's `T` to the OUTER `T`,
        // which `record_call_type_subs` deliberately drops as a
        // self-referential binding — so `call_type_subs` is empty there.
        // But codegen already knows `v`'s concrete element type from the
        // enclosing mono's param registration (`vec_elem_types` etc.), so
        // bind any container-element type param straight from the arg's
        // registered element. Also covers the top-level case (a let-bound
        // `a: Vec[i64]` is registered the same way), making the two
        // element instantiations distinct monos regardless of nesting.
        self.augment_subst_from_arg_elem_types(&generic_fn, args, &mut subst);

        // Const generics slice 1b: process explicit generic args. For
        // each formal param the user supplied an explicit arg for,
        // override the inferred type subst (for type params) or
        // populate a parallel const_subst (for const params). The
        // const_subst flows to `mangle_mono_name` so each distinct
        // const-arg tuple produces a distinct mono symbol. Slice 4
        // will collapse this into a single `SubstValue<'ctx>` shape
        // (fork F2) once codegen body lowering needs const-param
        // identifier resolution.
        let mut const_subst: HashMap<String, crate::prelude::ConstValue> = HashMap::new();
        if let (Some(explicit), Some(gp)) = (explicit_generic_args, &generic_fn.generic_params) {
            for (param, arg) in gp.params.iter().zip(explicit.iter()) {
                match arg {
                    GenericArg::Type(t) => {
                        let llvm_ty = self.llvm_type_for_type_expr(t);
                        subst.insert(param.name.clone(), llvm_ty);
                        // Keep the name-subst twin in step (B-2026-07-03-11) so an
                        // explicit `f[C](x)` also registers the receiver's concrete
                        // type name for bound-trait-method dispatch.
                        if let TypeKind::Path(path) = &t.kind {
                            if let Some(seg) = path.segments.first() {
                                let resolved = self
                                    .type_subst_names
                                    .get(seg)
                                    .cloned()
                                    .unwrap_or_else(|| seg.clone());
                                subst_names.insert(param.name.clone(), resolved);
                            }
                        }
                    }
                    GenericArg::Const(e) => {
                        if let Some(cv) = const_value_from_literal_expr(e) {
                            const_subst.insert(param.name.clone(), cv);
                        }
                    }
                    // Shape args never reach mono — the typechecker's
                    // v1 stub rejects shape-kinded generics before
                    // codegen runs. Benign skip rather than unreachable!
                    // so a bypassed-typecheck path cannot panic here.
                    GenericArg::Shape(_) => {}
                }
            }
        }

        // Slice 0.a sub-step 2 — codegen monomorphization-request bound
        // enforcement (defense-in-depth). The typechecker discharges
        // bounds at every call site (`discharge_type_bounds` /
        // `normalize_bounds_into_where_clause`); this hook fires only
        // for paths that reach codegen with a still-unsatisfied bound
        // (a future cross-module path, or a typechecker-internal call
        // that bypassed the discharge). Covers built-in trait names
        // against primitive LLVM types only — user-trait-on-user-type
        // requires an impl-table threading slice that isn't built yet.
        self.verify_bounds_at_codegen(&generic_fn, &subst)?;

        // Cross-argument `?`-dim equality asserts at the call boundary
        // (design.md § Runtime equality check). For a callee that shares a
        // named `Dim` parameter across two `Tensor` params (the `K` in
        // `matmul(a: [M, K], b: [K, N])`), insert a runtime check that the
        // bound argument dims agree — the type system can't prove two `?`
        // dims equal statically. Emitted here, before the specialization is
        // generated and called, so the trap fires ahead of the operation
        // (and ahead of any tensor read the callee would do out of bounds).
        // The `arg_vals` were just compiled above; a tensor value is a
        // single pointer, so this consults no variable slots.
        self.emit_tensor_crossarg_dim_asserts(&generic_fn, args, &arg_vals)?;

        // Per-layout-monomorphization axis — forward layout-flow inference
        // (`docs/spikes/per-layout-monomorphization.md`). The layout half of
        // the monomorph key: each layout-carrying param's active `LayoutId`,
        // keyed by param name. Slice 1 resolves every entry to `Aos`, so the
        // mangled name below is unchanged and the monomorph is byte-identical
        // to the name-keyed model.
        let layout_subst = self.compute_call_layout_subst(&generic_fn, args);

        // Mangle a unique name for this specialization (e.g. `max$i64`).
        // A generic call carries no backward (return) layout inference yet —
        // that path is the non-generic `ensure_layout_mono_generated` entry —
        // so the return axis is `Aos` here.
        let mangled = self.mangle_mono_name(
            name,
            &generic_fn,
            &subst,
            &subst_names,
            &const_subst,
            &layout_subst,
            &LayoutId::Aos,
        );
        // Handle-backed builtin (Column/Tensor) args bound to bare type
        // params: a distinct mangle axis + a prologue-registration record
        // — the LLVM-shape subst above sees only `ptr` for these (S6a).
        let handle_params = self.collect_mono_handle_params(&generic_fn, args);
        let mangled = self.append_handle_mangle(mangled, &handle_params);
        if !handle_params.is_empty() {
            self.mono_handle_param_infos
                .insert(mangled.clone(), handle_params);
        }
        // Bind handle-backed-container type params (`C` bound to a Column/Tensor
        // arg) to `ptr` so a bare-`C` RETURN (`map`/`zip_with` → `Self`) or a
        // `let d: C` local lowers to the pointer shape, not the `i64` default
        // (the "return type does not match operand type" verifier error). Done
        // AFTER `mangle_mono_name` above so the mangled name is byte-identical
        // to before — the injection changes only the `type_subst` the body /
        // return lowering consults, never the mono cache key.
        self.augment_subst_from_handle_params(&generic_fn, args, &mut subst);

        // Slice 8y: per-call-site decision on whether the caller
        // takes the state-machine intercept path or falls through to
        // a direct call. `true` (state-machine) is the conservative
        // default — it kicks in when the callee has static
        // network-yield effects, when the callee is non-pure-polymorphic,
        // or when no `call_effect_subs` resolution is available. The
        // optimization fires only for callees declared with a
        // purely-polymorphic effect surface (`with E` or `with _`,
        // no fixed portion) whose per-call `E` bindings resolve to
        // an effect set free of `sends(Network)` / `receives(Network)`.
        //
        // Per-mono state-machine helpers stay emitted unconditionally
        // (the four helpers are idempotent across call sites and a
        // future call site of the same mono whose `E` resolves to
        // network-yield will need them). Only the intercept site
        // below consults this flag — direct call when `false`,
        // state-machine invocation when `true`.
        let use_state_machine = self.call_uses_state_machine(call_span, name);

        // Generate the specialization if we haven't done so yet.
        if !self.generated_monos.contains(&mangled) {
            // Mark as in-progress before recursing to avoid infinite loops.
            self.generated_monos.insert(mangled.clone());

            // Save all per-function codegen state — we're about to compile a
            // different function inline.
            let saved_bb = self.builder.get_insert_block();
            let saved_fn = self.current_fn;
            // The mono body is compiled INLINE mid-caller; `compile_mono_function`
            // sets `current_fn_name` to the mono's name so a valueless `return;`
            // in a void mono emits `ret void` — not the caller's identity. Without
            // this, a mono compiled inside `main` inherited `current_fn_name ==
            // "main"` and a bare `return;` mis-emitted `ret i32 0` (main's exit-code
            // signature) into the void mono (B-2026-07-11-28).
            let saved_fn_name = std::mem::take(&mut self.current_fn_name);
            let saved_vars = std::mem::take(&mut self.variables);
            let saved_var_types = std::mem::take(&mut self.var_type_names);
            // The mono body is compiled INLINE, mid-caller — so its tensor
            // param registrations (added by `compile_mono_function`) must not
            // leak into the caller's `tensor_var_infos`, which is keyed by
            // bare var name and would otherwise have a caller-side `a` / `b`
            // overwritten by the callee's same-named tensor param. Swap to a
            // clean slate for the body (module-level tensor bindings are
            // re-seeded inside `compile_mono_function`) and restore below —
            // parallel to `variables` / `var_type_names`.
            let saved_tensor_infos = std::mem::take(&mut self.tensor_var_infos);
            // Same isolation for every other name-keyed var side-table the
            // full-registration prologue (B-2026-07-02-11) can now write —
            // see `SavedVarSideTables` for the leak this fixes.
            let saved_side_tables = self.take_var_side_tables();
            // The mono body manages its OWN scope-cleanup frame stack
            // (pushed/drained in `compile_mono_function`, mirroring
            // `compile_function`). Because the body compiles inline,
            // mid-caller, its frames must not be appended to — or drained
            // out of — the caller's live stack: a callee `let out` cleanup
            // landing on the caller's frame would be emitted in the caller's
            // scope where the callee's alloca doesn't dominate ("Instruction
            // does not dominate all uses"). Swap to an empty stack for the
            // body and restore the caller's below — parallel to `variables`.
            let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
            // A mono body is a top-level function, not a par branch — it must
            // compile with `branch_cancel_ptr = None` so `compile_call`'s
            // cooperative cancel check stays a no-op (the ptr names a par
            // branch fn's cancel param, valid only inside that branch). The
            // body compiles INLINE, so without this an auto-par branch
            // emitted while lowering an EARLIER mono (whose loops
            // parallelized) leaves `branch_cancel_ptr` set, and the NEXT
            // mono's first call emits a cancel check against that stale ptr
            // → "Referring to an argument in another function" + a `ret void`
            // in a value-returning fn. Reset for the body, restore the
            // caller's value below (re-entrant, like `variables`).
            let saved_cancel_ptr = self.branch_cancel_ptr.take();
            let saved_loop_stack = std::mem::take(&mut self.loop_stack);
            let saved_subst = std::mem::replace(&mut self.type_subst, subst.clone());
            // Name-level twin of `type_subst` (B-2026-07-03-11): thread the
            // concrete-type-name subst so the mono param prologue can register a
            // bound-generic receiver under its concrete type for trait dispatch.
            let saved_subst_names =
                std::mem::replace(&mut self.type_subst_names, subst_names.clone());
            // Const generics slice 4: thread the const-arg substitution
            // into the body-lowering pass so `compile_expr Identifier`
            // can resolve const-param refs against it. Parallel to
            // `type_subst`'s save/restore.
            let saved_const_subst = std::mem::replace(&mut self.const_subst, const_subst.clone());
            // Per-layout-monomorphization axis: thread the per-call layout
            // substitution into the body-lowering pass. Parallel to
            // `type_subst` / `const_subst`. Slice 1 always carries `Aos`
            // entries, so body lowering (which doesn't yet consult this map)
            // is unchanged; slice 2 reads it to select the SoA access paths.
            let saved_layout_subst =
                std::mem::replace(&mut self.layout_subst, layout_subst.clone());
            // Slice 4: `compile_mono_function`'s prologue may register SoA
            // borrow params in `ref_params` (a generic fn with a `ref Vec[E]`
            // param whose binding-site layout is SoA). Swap it out for the mono
            // body and restore below, like `variables` — see the matching note
            // in `ensure_layout_mono_generated`.
            let saved_ref_params = std::mem::take(&mut self.ref_params);
            // Slice 5: per-binding layout carrier — the mono body seeds its own
            // locals at their `let` sites; swap out the caller's map and restore
            // below, parallel to `variables` / `ref_params`.
            let saved_binding_layouts = std::mem::take(&mut self.binding_layouts);
            // Same isolation for the entry-slot-ref locals (the two-step
            // `let r = m.entry(k).or_insert(d)` binding tag): a nested mono
            // body must not see/clobber the outer function's tags.
            let saved_entry_slot_ref_vars = std::mem::take(&mut self.entry_slot_ref_vars);
            let saved_soa_return_locals = std::mem::take(&mut self.soa_return_locals);

            // Declare then compile the specialization.
            self.declare_mono_function(&generic_fn, &mangled)?;
            self.compile_mono_function(&generic_fn, &mangled)?;

            // Slice 8v Phase 2: when the polymorphic source is a
            // network-yielding fn (entry in `program.state_struct_layouts`
            // under its base name), emit per-mono state-machine helpers
            // (state-struct LLVM type + poll-fn + constructor +
            // destructor) under the mangled key. `type_subst` is STILL
            // ACTIVE here — the restore steps run after this — so
            // `llvm_type_for_name("T")` inside the helpers resolves
            // correctly to the per-mono concrete LLVM type. The
            // orchestrator no-ops when the base key isn't in
            // `state_struct_layouts` (non-yielding generic fn — the
            // common case), so the cost for the common path is one
            // HashMap lookup per generic-call mono.
            self.emit_state_machine_helpers_for_mono(name, &mangled);

            // Restore state.
            self.soa_return_locals = saved_soa_return_locals;
            self.binding_layouts = saved_binding_layouts;
            self.ref_params = saved_ref_params;
            self.entry_slot_ref_vars = saved_entry_slot_ref_vars;
            self.layout_subst = saved_layout_subst;
            self.const_subst = saved_const_subst;
            self.type_subst = saved_subst;
            self.type_subst_names = saved_subst_names;
            self.loop_stack = saved_loop_stack;
            self.branch_cancel_ptr = saved_cancel_ptr;
            self.scope_cleanup_actions = saved_cleanup;
            self.restore_var_side_tables(saved_side_tables);
            self.tensor_var_infos = saved_tensor_infos;
            self.var_type_names = saved_var_types;
            self.variables = saved_vars;
            self.current_fn = saved_fn;
            self.current_fn_name = saved_fn_name;
            if let Some(bb) = saved_bb {
                self.builder.position_at_end(bb);
            }
        }

        // Slice 8v Phase 2: per-mono caller-side intercept. When
        // the polymorphic source is a network-yielding fn, the
        // per-mono state-machine helpers were emitted at the mangled
        // key by `emit_state_machine_helpers_for_mono` above. Replace
        // the direct `call @<mangled>(args)` with the state-machine
        // invocation shape — mirrors slice 8d's caller-side intercept
        // (in `src/codegen/call_dispatch.rs`) keyed on the mangled
        // name instead of the source-level callee name:
        //
        //   %state  = call ptr @__kara_state_new_<mangled>()
        //   store args into state struct captured-local fields
        //   br label %kara.poll_loop
        // kara.poll_loop:
        //   %result = call i8 @__kara_poll_<mangled>(ptr %state, ptr null)
        //   %pending = icmp eq i8 %result, 0
        //   br i1 %pending, label %kara.poll_yield, label %kara.poll_done
        // kara.poll_yield:
        //   call i32 @sched_yield()
        //   br label %kara.poll_loop
        // kara.poll_done:
        //   load terminal return value (if non-unit)
        //   call void @free(ptr %state)
        //
        // Slice 8d's incomplete state-struct destructor invocation
        // (the slice ships the destructor but doesn't yet call it
        // from any use site) carries over here — destructor wiring
        // for both the slice 8d and this per-mono intercept is a
        // separate follow-on slice. Cooperative yield (`sched_yield`)
        // matches the slice 8e shape so the parent task doesn't
        // busy-spin between poll-fn invocations.
        //
        // Slice 8y: gate the intercept on the per-call
        // `use_state_machine` decision. When `false`, take the
        // direct-call path even if the per-mono state-machine helpers
        // were emitted earlier (by this or an earlier call site of
        // the same mono).
        let ctor_fn_opt = if use_state_machine {
            self.state_machine_state_constructors.get(&mangled).copied()
        } else {
            None
        };
        if let Some(ctor_fn) = ctor_fn_opt {
            let poll_fn = self
                .state_machine_poll_fns
                .get(&mangled)
                .copied()
                .expect("poll-fn co-emitted with state-machine constructor");
            let state_struct = self
                .state_struct_types
                .get(&mangled)
                .copied()
                .expect("state struct type co-emitted with constructor");
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i8_ty = self.context.i8_type();
            let cur_fn = self
                .builder
                .get_insert_block()
                .and_then(|bb| bb.get_parent())
                .expect("compile_generic_call inside a function context");

            // Allocate the state struct via the constructor helper.
            let state_call = self
                .builder
                .build_call(ctor_fn, &[], "kara.state")
                .expect("call per-mono state-struct constructor");
            let state_ptr = state_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value();

            // Thread arg values into the state struct's captured-local
            // slots — mirrors slice 8f's discipline. State-struct
            // layout positions parameters first (1..=K after the tag
            // at 0), so arg `i` goes into field `i + 1`. Per-mono
            // emission used the active `type_subst` so the field
            // types match `arg_vals[i].get_type()` for owned-value
            // params.
            //
            // Slice 8z: extend the store discipline to `ref T` /
            // `mut ref T` / `mut Slice[T]` param shapes — without
            // this, the intercept stored a loaded value (Vec struct,
            // i64, etc.) into a ptr- or Slice-struct-shaped state-
            // struct field and produced ill-typed IR that the LLVM
            // verifier rejects. Mirrors slice 8d's non-generic
            // intercept: ref param → `get_data_ptr(var_name)` for
            // Identifier args; ref param → materialize into stack
            // temp for rvalue args (`val` from `arg_vals[i]` is the
            // already-compiled value, alloca + store + optional
            // `track_vec_var` for Vec-struct-shaped rvalues so the
            // heap buffer's scope-exit cleanup queues correctly);
            // `mut Slice[T]` param → `coerce_to_slice(arg, elem_ty)`
            // synthesizes the `{ptr, i64}` slice header at the call
            // site. The tables `fn_param_ref` and
            // `fn_param_slice_elem` are populated by
            // `declare_mono_function` against the mangled key (slice
            // 8z extension) so the lookups resolve to per-mono
            // results that honor the active `type_subst`.
            let ref_flags = self.fn_param_ref.get(&mangled).cloned().unwrap_or_default();
            let slice_elems = self
                .fn_param_slice_elem
                .get(&mangled)
                .cloned()
                .unwrap_or_default();
            for (i, val) in arg_vals.iter().enumerate() {
                let field_idx = (i + 1) as u32;
                let field_ptr = self
                    .builder
                    .build_struct_gep(
                        state_struct,
                        state_ptr,
                        field_idx,
                        &format!("kara.arg{i}.field_ptr"),
                    )
                    .expect("GEP per-mono state struct field for arg");

                let is_ref = ref_flags.get(i).copied().unwrap_or(false);
                let slice_elem = slice_elems.get(i).copied().flatten();

                let to_store: BasicValueEnum<'ctx> = if is_ref {
                    // Ref param: pass a pointer to the caller-side
                    // data, not the loaded value. Identifier args
                    // resolve through `get_data_ptr`; rvalue args
                    // (literals, function returns, arithmetic) get
                    // materialized into an entry-block alloca whose
                    // pointer is stored into the field.
                    if let ExprKind::Identifier(var_name) = &args[i].value.kind {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            self.materialize_rvalue_for_ref_arg(*val, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&args[i].value)? {
                        // `vec[idx]` borrow — element pointer in place
                        // (no shallow-copy + drop double-free). The
                        // pre-compiled `*val` load is left dead (DCE'd).
                        elem_ptr.into()
                    } else {
                        self.materialize_rvalue_for_ref_arg(*val, i)
                    }
                } else if let Some(elem_ty) = slice_elem {
                    // `mut Slice[T]` param: synthesize the slice
                    // header (`{ptr, i64}`) from the arg. Falls
                    // through to the loaded value for shapes the
                    // coercion doesn't recognize (matches the
                    // non-generic intercept's discipline).
                    match self.coerce_to_slice(&args[i].value, elem_ty)? {
                        Some(slice_val) => slice_val,
                        None => *val,
                    }
                } else {
                    *val
                };

                self.builder
                    .build_store(field_ptr, to_store)
                    .expect("store arg into per-mono state struct field");
            }

            let loop_bb = self.context.append_basic_block(cur_fn, "kara.poll_loop");
            let yield_bb = self.context.append_basic_block(cur_fn, "kara.poll_yield");
            let done_bb = self.context.append_basic_block(cur_fn, "kara.poll_done");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br to per-mono poll loop");
            self.builder.position_at_end(loop_bb);
            let null_cancel = ptr_ty.const_null();
            let poll_call = self
                .builder
                .build_call(
                    poll_fn,
                    &[state_ptr.into(), null_cancel.into()],
                    "kara.poll_result",
                )
                .expect("call per-mono poll-fn");
            let poll_result = poll_call
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let is_pending = self
                .builder
                .build_int_compare(
                    IntPredicate::EQ,
                    poll_result,
                    i8_ty.const_int(0, false),
                    "kara.is_pending",
                )
                .expect("icmp eq i8 result, 0 for per-mono");
            self.builder
                .build_conditional_branch(is_pending, yield_bb, done_bb)
                .expect("br on per-mono poll discriminant");

            self.builder.position_at_end(yield_bb);
            self.builder
                .build_call(self.sched_yield_fn, &[], "kara.yield_result")
                .expect("call sched_yield for per-mono cooperative yield");
            self.builder
                .build_unconditional_branch(loop_bb)
                .expect("br back to per-mono poll loop after yield");

            self.builder.position_at_end(done_bb);
            // Slice 8i shape: when the mono's return type is non-unit
            // (recorded under the mangled key by
            // `emit_state_struct_type_for_key` when the polymorphic
            // source had a non-unit return type and active `type_subst`
            // resolved to a `state_machine_return_types`-eligible
            // type), load the terminal field BEFORE freeing.
            let call_result =
                if let Some(ret_ty) = self.state_machine_return_types.get(&mangled).copied() {
                    let n_fields = state_struct.count_fields();
                    let terminal_idx = n_fields - 1;
                    let terminal_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            terminal_idx,
                            "kara.return.field_ptr",
                        )
                        .expect("GEP per-mono terminal return-value field on caller side");
                    self.builder
                        .build_load(ret_ty, terminal_ptr, "kara.return.value")
                        .expect("load per-mono callee return value from terminal field")
                } else {
                    self.context.i64_type().const_int(0, false).into()
                };
            self.builder
                .build_call(self.free_fn, &[state_ptr.into()], "")
                .expect("call free on per-mono state struct");
            return Ok(call_result);
        }

        // Non-yielding generic call: emit the direct call to the
        // mono'd specialization. This is the common case for
        // generic functions — most user generics aren't network-
        // yielding (only those reachable to `sends(Network)` /
        // `receives(Network)` end up in `state_struct_layouts`).
        let func = match self.module.get_function(&mangled) {
            Some(f) => f,
            None => return Ok(self.context.i64_type().const_int(0, false).into()),
        };

        // Ref-mode params take a POINTER to the caller-side data, matching
        // the pointer ABI `declare_mono_function` gives them — the same
        // discipline the state-machine intercept above applies when storing
        // args into state-struct fields. Identifier args resolve through
        // `get_data_ptr`; rvalue args are materialized into an entry alloca.
        // Without this the direct call passed the loaded value against a
        // `ptr` signature slot and module verification failed ("Call
        // parameter type does not match function signature") the moment a
        // mono body actually used a `ref Vec[E]` param (B-2026-07-02-11
        // registration made such bodies compile; before it they errored at
        // the first collection-method touch).
        let ref_flags = self.fn_param_ref.get(&mangled).cloned().unwrap_or_default();
        // By-value `Slice[T]` params: the caller must synthesize the `{ptr,i64}`
        // slice header from a Vec / Array / slice argument, exactly as the
        // non-generic direct-call path does (call_dispatch.rs). Without this the
        // raw arg value (`{ptr,i64,i64}` for a Vec) was passed against the mono's
        // `{ptr,i64}` Slice-typed param and module verification rejected the
        // mismatch (B-2026-07-03-9). `declare_mono_function` populated
        // `fn_param_slice_elem[mangled]` via `extract_slice_elem_type` (which
        // resolves the element `T` through the active `type_subst`), so the
        // element type is already the concrete per-mono width. `mut Slice[T]`
        // was already handled on the state-machine path; this closes the
        // by-value form on the common direct-call path.
        let slice_elems = self
            .fn_param_slice_elem
            .get(&mangled)
            .cloned()
            .unwrap_or_default();
        let compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = arg_vals
            .iter()
            .enumerate()
            .map(|(i, v)| -> Result<BasicMetadataValueEnum<'ctx>, String> {
                if ref_flags.get(i).copied().unwrap_or(false) {
                    let ptr: BasicValueEnum<'ctx> = if let ExprKind::Identifier(var_name) =
                        &args[i].value.kind
                    {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            self.materialize_rvalue_for_ref_arg(*v, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&args[i].value)? {
                        elem_ptr.into()
                    } else {
                        self.materialize_rvalue_for_ref_arg(*v, i)
                    };
                    Ok(BasicMetadataValueEnum::from(ptr))
                } else if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                    match self.coerce_to_slice(&args[i].value, elem_ty)? {
                        Some(slice_val) => Ok(BasicMetadataValueEnum::from(slice_val)),
                        None => Ok(BasicMetadataValueEnum::from(*v)),
                    }
                } else {
                    Ok(BasicMetadataValueEnum::from(*v))
                }
            })
            .collect::<Result<_, _>>()?;

        let call = self
            .builder
            .build_call(func, &compiled_args, "call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            Ok(basic_val.unwrap_basic())
        }
    }

    /// Phase 6 line 26 slice 8y: decide whether a generic call site
    /// should take the per-mono state-machine intercept path or fall
    /// through to a direct call.
    ///
    /// Returns `true` (state-machine intercept) when EITHER:
    ///   - the callee is NOT in `state_struct_layouts` — but the
    ///     intercept gate below additionally requires the per-mono
    ///     helpers to exist, so this branch is moot for callees that
    ///     wouldn't take the intercept anyway. We return `false`
    ///     in this case so the predicate stays parsimonious.
    ///   - the callee IS in `state_struct_layouts` AND is NOT in
    ///     `callee_purely_polymorphic_effects` — callees with static
    ///     fixed effects (`Explicit` or `PolymorphicWithFixed`) may
    ///     carry `sends(Network)` / `receives(Network)` in the static
    ///     portion regardless of any `with E` resolution, so the
    ///     intercept must fire to drive their internal yields.
    ///   - the callee IS purely polymorphic AND `call_effect_subs[span]`
    ///     records at least one effect-variable binding to a
    ///     network-yield verb (`sends(Network)` / `receives(Network)`):
    ///     state-machine path needed.
    ///
    /// Returns `false` (direct call) when the callee is purely
    /// polymorphic AND all of its `call_effect_subs[span]` bindings
    /// resolve to a non-network effect set, or when no entry is
    /// present at all (the callee has no effect-variable parameters
    /// at all, which today indicates a `with _` anonymous polymorphic
    /// surface — conservative `true` keeps the intercept in that
    /// case).
    ///
    /// **Soundness caveat:** for a private fn whose body contains
    /// static yield points (e.g. `fn op[T, with E](cb: Fn() with E)
    /// with E { fetch(); cb(); }`), the callee's body parks at
    /// `fetch()` regardless of `E`. The current architecture's
    /// `state_struct_layouts` population coupling — only populated
    /// when the body contains static yield points — means the
    /// optimization's only reachable scenario co-occurs with body
    /// yields, and the skip is technically unsound in production
    /// (the direct-call path would block at the body's internal
    /// fetch). The v1 test-harness `fetch` stubs are empty-bodied
    /// so the skip is harmless in tests; production correctness
    /// awaits a follow-on slice that decouples `state_struct_layouts`
    /// population from the body-yield-points requirement (broadens
    /// the candidate pool to purely-polymorphic-no-body-yield
    /// callees, after which the slice 8y gate fires soundly).
    pub(super) fn call_uses_state_machine(
        &self,
        call_span: &crate::token::Span,
        base_key: &str,
    ) -> bool {
        let snap = match self.program_snapshot.as_ref() {
            Some(s) => s,
            None => return false,
        };
        if !snap.state_struct_layouts.contains_key(base_key) {
            return false;
        }
        if !snap.callee_purely_polymorphic_effects.contains(base_key) {
            return true;
        }
        let key = (call_span.offset, call_span.length);
        let bindings = match snap.call_effect_subs.get(&key) {
            Some(b) => b,
            None => return true,
        };
        bindings.values().any(|effects| {
            effects
                .iter()
                .any(|e| (e.verb == "sends" || e.verb == "receives") && e.resource == "Network")
        })
    }

    /// Declare the LLVM function for a monomorphized specialization.
    /// `type_subst` must already be populated before calling this.
    /// Slice 8z: materialize a non-place rvalue arg into an entry-block
    /// alloca so the `ref T` per-mono caller-side intercept can store
    /// the resulting `ptr` into the state struct's field. Mirrors
    /// slice 8d's identical mechanic in `compile_call` — a literal /
    /// arithmetic / function-return arg bound to a `ref T` param has
    /// no addressable storage, so codegen mints one. Vec-struct-shaped
    /// values (Vec / VecDeque / String) get queued for scope-exit
    /// `FreeVecBuffer` via `track_vec_var` so the heap buffer's
    /// cleanup runs at the caller's scope boundary; primitives and
    /// pointer-shaped temporaries (string literals, etc.) need no
    /// such tracking. Slice 8ad widened visibility to `pub(super)` so
    /// the non-generic state-machine intercept in `call_dispatch.rs`
    /// can call this same helper for its `ref T` rvalue path.
    pub(super) fn materialize_rvalue_for_ref_arg(
        &mut self,
        val: BasicValueEnum<'ctx>,
        arg_idx: usize,
    ) -> BasicValueEnum<'ctx> {
        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
            .expect("compile_generic_call or compile_call inside a function context");
        let temp = self.create_entry_alloca(
            cur_fn,
            &format!("kara.arg{arg_idx}.ref_rvalue"),
            val.get_type(),
        );
        self.builder
            .build_store(temp, val)
            .expect("store rvalue value into ref-arg materialization slot");
        if self.llvm_ty_is_vec_struct(val.get_type()) {
            self.track_vec_var(temp, None);
        }
        temp.into()
    }

    /// Generate (declare + compile) a per-layout monomorph of a *non-generic*
    /// function under `mangled`, with `layout_subst` active so its `Vec[E]`
    /// params lower SoA against the caller's argument layout (slice 2) and
    /// `return_layout` active so a non-`Aos` return lowers the LLVM return type
    /// to the SoA struct and the returned local(s) build SoA (slice 3). The
    /// non-specialized (all-`Aos`) body was already compiled in the normal
    /// module pass; this adds the SoA variant as a distinct symbol. Idempotent
    /// via `generated_monos`. Mirrors `compile_generic_call`'s mono-entry
    /// save/restore, with empty type/const substs (a non-generic callee has no
    /// type/const params) — and restores even on error so a failed body can't
    /// leave a half-swapped builder/var state behind.
    pub(super) fn ensure_layout_mono_generated(
        &mut self,
        func: &Function,
        mangled: &str,
        layout_subst: HashMap<String, LayoutId>,
        return_layout: LayoutId,
    ) -> Result<(), String> {
        if self.generated_monos.contains(mangled) {
            return Ok(());
        }
        self.generated_monos.insert(mangled.to_string());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_type_names);
        let saved_tensor_infos = std::mem::take(&mut self.tensor_var_infos);
        // Full var-side-table isolation — see `SavedVarSideTables`.
        let saved_side_tables = self.take_var_side_tables();
        let saved_cleanup = std::mem::take(&mut self.scope_cleanup_actions);
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        let saved_loop_stack = std::mem::take(&mut self.loop_stack);
        let saved_subst = std::mem::take(&mut self.type_subst);
        // Name-subst twin (B-2026-07-03-11): isolate the layout-mono body from a
        // stale outer name-subst, mirroring `type_subst`.
        let saved_subst_names = std::mem::take(&mut self.type_subst_names);
        let saved_const_subst = std::mem::take(&mut self.const_subst);
        let saved_layout_subst = std::mem::replace(&mut self.layout_subst, layout_subst);
        let saved_return_layout = std::mem::replace(&mut self.return_layout, return_layout);
        // Slice 4: the mono prologue now registers SoA `ref`/`mut ref Vec[E]`
        // params in `ref_params` (so the access paths deref the slot once).
        // `ref_params` is per-function state the caller doesn't otherwise swap
        // out, so take it for the mono body (empty → the prologue rebuilds it
        // for this mono's own params) and restore the caller's map after —
        // mirroring the `variables` save/restore above. Without this a mono's
        // ref param would mark a same-named caller binding as a borrow.
        let saved_ref_params = std::mem::take(&mut self.ref_params);
        // Slice 5: the mono body seeds its own locals' layouts in
        // `binding_layouts` at their `let` sites. Take the caller's carrier for
        // the duration (the body starts empty, like `variables`) and restore it
        // after, so a mono's local can't leak its SoA-ness back to a same-named
        // caller binding.
        let saved_binding_layouts = std::mem::take(&mut self.binding_layouts);
        let saved_entry_slot_ref_vars = std::mem::take(&mut self.entry_slot_ref_vars);
        // Returned-local set is per-function; `compile_mono_function` repopulates
        // it from this mono's body. Save/restore so it can't leak across the
        // nested compile (mirrors `binding_layouts`).
        let saved_soa_return_locals = std::mem::take(&mut self.soa_return_locals);

        let result = self
            .declare_mono_function(func, mangled)
            .and_then(|_| self.compile_mono_function(func, mangled));

        self.soa_return_locals = saved_soa_return_locals;
        self.binding_layouts = saved_binding_layouts;
        self.ref_params = saved_ref_params;
        self.entry_slot_ref_vars = saved_entry_slot_ref_vars;
        self.return_layout = saved_return_layout;
        self.layout_subst = saved_layout_subst;
        self.const_subst = saved_const_subst;
        self.type_subst = saved_subst;
        self.type_subst_names = saved_subst_names;
        self.loop_stack = saved_loop_stack;
        self.branch_cancel_ptr = saved_cancel_ptr;
        self.scope_cleanup_actions = saved_cleanup;
        self.restore_var_side_tables(saved_side_tables);
        self.tensor_var_infos = saved_tensor_infos;
        self.var_type_names = saved_var_types;
        self.variables = saved_vars;
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        result
    }

    pub(super) fn declare_mono_function(
        &mut self,
        func: &Function,
        mangled: &str,
    ) -> Result<FunctionValue<'ctx>, String> {
        let mut param_types: Vec<BasicMetadataTypeEnum<'ctx>> = func
            .params
            .iter()
            .map(|p| self.llvm_param_type(p))
            .collect();
        // Per-layout-monomorphization (slice 2): a `Vec[E]` param whose active
        // `LayoutId` (in the current monomorph's `layout_subst`) is `Soa` is
        // passed as the 4-field SoA struct, not the AoS `{ptr,len,cap}` Vec —
        // the caller holds that SoA struct for the argument binding. Mirrors
        // the name-keyed by-value signature patch (functions.rs); keyed on the
        // layout subst, not the param name, so it crosses call boundaries
        // regardless of binding name. No-op outside a layout-monomorph.
        for (i, p) in func.params.iter().enumerate() {
            if let Some(soa) = self.active_param_soa_layout(p) {
                let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
                param_types[i] = soa_ty.into();
            }
        }

        // Per-layout-monomorphization backward axis (slice 3): a non-`Aos`
        // return layout lowers the LLVM return type to the 4-field SoA struct
        // (`soa_vec_type`), not the AoS `{ptr,len,cap}` the declared `Vec[E]`
        // would give. The caller binds the result into its SoA slot; the body
        // builds + returns the SoA struct. No-op outside a return-SoA mono.
        let soa_return = match &self.return_layout {
            LayoutId::Soa(block) => self.soa_layouts.get(block).cloned(),
            LayoutId::Aos => None,
        };
        let fn_type = if let Some(soa) = soa_return {
            let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
            soa_ty.fn_type(&param_types, false)
        } else {
            match self.llvm_return_type(&func.return_type) {
                Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_types, false),
                Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                    self.context.void_type().fn_type(&param_types, false)
                }
            }
        };

        // Slice 8z: mirror the non-generic `declare_one_function` ref /
        // slice-elem table population for the mangled per-mono key.
        // Without this, slice 8d's caller-side arg-passing rules (ref →
        // pass pointer, mut Slice → coerce to slice header) are
        // unreachable from `compile_generic_call`'s per-mono state-
        // machine intercept — the intercept's arg-store loop falls
        // through to "store loaded value" for ref / slice params and
        // mints stores of the wrong LLVM type into the ptr / Slice-
        // struct-shaped state-struct field. Type-parameter-typed ref
        // (`ref T`) keeps `ref_flag: true` regardless of T's
        // resolution; `mut Slice[T]`'s element type resolves through
        // `extract_slice_elem_type` → `llvm_type_for_type_expr`, which
        // honors the active `type_subst`.
        let ref_flags: Vec<bool> = func
            .params
            .iter()
            .map(|p| matches!(&p.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)))
            .collect();
        self.fn_param_ref.insert(mangled.to_string(), ref_flags);
        let slice_elems: Vec<Option<BasicTypeEnum<'ctx>>> = func
            .params
            .iter()
            .map(|p| self.extract_slice_elem_type(&p.ty))
            .collect();
        self.fn_param_slice_elem
            .insert(mangled.to_string(), slice_elems);

        let fn_val = self.module.add_function(mangled, fn_type, None);
        // Ownership-derived `noalias` (owned-params slice 2): same treatment as
        // the non-generic `declare_function`. On this path a bare generic param
        // (`fn f[T](x: T)`) is resolved through the active `type_subst_names`
        // inside the helper, so a specialization with a value-semantics `ptr`
        // type (or a shared type, for the `mut ref` carve-out) is classified
        // correctly. Monos are never `sret`/coroutine ramps, so the param-index
        // math needs no shift.
        self.emit_param_alias_attrs(fn_val, func);
        Ok(fn_val)
    }

    /// Compile the body of a monomorphized specialization.
    /// `type_subst` must already be populated and per-function state must be fresh.
    pub(super) fn compile_mono_function(
        &mut self,
        func: &Function,
        mangled: &str,
    ) -> Result<(), String> {
        let fn_val = self
            .module
            .get_function(mangled)
            .ok_or_else(|| format!("Mono '{}' not declared", mangled))?;

        self.current_fn = Some(fn_val);
        // Identify the mono by its own name (mirrors `compile_function`), so a
        // valueless `return;` in a void mono checks against the right identity —
        // the caller saved/restores this around the inline body (B-2026-07-11-28).
        self.current_fn_name = func.name.clone();
        self.variables.clear();
        self.var_type_names.clear();
        // Per-binding layout carrier (slice 5): the caller's map was swapped out
        // (`mem::take`) at the mono entry point, so this fresh body starts empty
        // and seeds its own locals; `let`-site registrations land here.
        self.binding_layouts.clear();
        // This mono's returned local(s) — so the origin name-match in
        // `seed_binding_site_layout` is suppressed for them (their layout comes
        // from `return_layout` / `layout_subst`, seeded just below). The caller's
        // set was swapped out at the mono entry point.
        self.soa_return_locals = self
            .soa_return_local_names(&func.body)
            .into_iter()
            .collect();
        self.inline_option_payload_vars.clear();
        self.boxed_enum_payload_vars.clear();
        self.inline_result_payload_vars.clear();
        self.inline_option_map_payload_vars.clear();
        self.inline_option_agg_payload_vars.clear();
        // Function-level scope-cleanup frame for owned locals (`Tensor` /
        // `Vec` / `String` / `Map` lets needing drop), mirroring
        // `compile_function`. The caller's frame stack was swapped out in
        // `compile_generic_call`, so this is the body's sole, fresh stack;
        // let-site registrations land here and drain at the tail return
        // below. Without it, a mono body's `let out = Tensor.zeros(…)`
        // FreeTensor cleanup leaked into the caller's frame and was emitted
        // where the callee's alloca didn't dominate ("Instruction does not
        // dominate all uses").
        self.scope_cleanup_actions.clear();
        self.scope_cleanup_actions.push(Vec::new());
        // Slice 10: reseed module-binding side-tables in monomorphised
        // bodies too (same reason as the `compile_function` path —
        // `var_type_names` is cleared per function).
        self.reseed_module_binding_side_tables();

        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);

        for (i, param) in func.params.iter().enumerate() {
            let param_name = self.param_name(param);
            let param_val = fn_val.get_nth_param(i as u32).unwrap();
            // Per-layout-monomorphization (slice 2): a `Vec[E]` param whose
            // active `LayoutId` is `Soa` arrives as the 4-field SoA struct
            // (the signature was patched in `declare_mono_function`). Spill it
            // to a slot typed as the SoA struct and register the binding so the
            // body's access paths (`active_soa_layout`) lower SoA against it.
            // Ownership is CALLER-RETAINS, mirroring the name-keyed by-value
            // path (functions.rs): the callee borrows the moved-in 4-field
            // header sharing the caller's group buffers, so NO `FreeSoaGroups`
            // cleanup here — the caller's binding frees them exactly once.
            if let Some(soa) = self.active_param_soa_layout(param) {
                let soa_ty = self.soa_vec_type(soa.num_groups, soa.cold_group.is_some());
                let alloca = self.create_entry_alloca(fn_val, &param_name, soa_ty.into());
                self.builder.build_store(alloca, param_val).unwrap();
                self.variables.insert(
                    param_name.clone(),
                    VarSlot {
                        ptr: alloca,
                        ty: soa_ty.into(),
                    },
                );
                continue;
            }
            let alloca = self.create_entry_alloca(fn_val, &param_name, param_val.get_type());
            self.builder.build_store(alloca, param_val).unwrap();
            // Track ref params: the alloca holds a pointer-to-data, so body
            // reads deref the slot once — the by-ref-reads discipline
            // `compile_function` applies. Originally SoA-gated (slice 4: a
            // SoA-carrying `ref Vec[E]` param must deref before GEPing
            // groups/len, else the access path reads the pointer bytes as
            // the SoA struct → garbage len → SIGTRAP); generalized to every
            // ref param by the B-2026-07-02-11 mono-param registration so a
            // `ref Vec[E]` / `ref String` param's collection dispatch derefs
            // correctly inside mono bodies too.
            if let Some(inner_ty) = self.inner_type_of_ref(&param.ty) {
                self.ref_params.insert(param_name.clone(), inner_ty);
            }
            // Track declared type name for struct/enum field resolution.
            // B-2026-07-03-11: if the declared type is a generic type parameter
            // bound in this monomorph (`x: X`), register the CONCRETE type name
            // (`C`) — resolved through the name-level `type_subst_names` — so a
            // trait method called through the bound (`x.tag()`) dispatches to
            // `C.tag` via `inferred_receiver_type`. Non-generic Path params
            // (`x: C`) fall through to the declared segment unchanged.
            // B-2026-07-06-2: peel a leading `ref`/`mut ref` first, so a
            // `c: ref C` bound-generic receiver ALSO registers its concrete
            // name. Without the peel, `TypeKind::Ref(..)` never matched the
            // `Path` arm, so `inferred_receiver_type(c)` returned `None` inside
            // the mono and `c.method()` on a USER-TYPE implementor fell through
            // to the "no handler" codegen error (containers were unaffected —
            // their `column_var_infos`/kernel intercept fires without needing
            // `var_type_names`). The receiver ABI (ptr-self for `ref self`) is
            // already handled downstream, so recording the name is sufficient.
            {
                let name_ty = match &param.ty.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                    _ => &param.ty,
                };
                if let TypeKind::Path(path) = &name_ty.kind {
                    if let Some(type_name) = path.segments.first() {
                        let concrete = self
                            .type_subst_names
                            .get(type_name)
                            .cloned()
                            .unwrap_or_else(|| type_name.clone());
                        self.var_type_names.insert(param_name.clone(), concrete);
                    }
                }
            }
            // B-2026-07-08-6 (generic/mono leg) — mirror `compile_function`'s
            // #14 owned-aggregate entry-copy for a monomorph's bare (non-ref)
            // owned heap struct/enum param: deep-copy its heap fields at entry
            // and register the scope-exit drop, at the param's CONCRETE
            // monomorph type (already recorded in `var_type_names`). Without
            // it `compile_mono_function` registered NO owned-aggregate param
            // drop (unlike `compile_function`), so a non-returned owned
            // heap-struct param leaked — e.g. `std.cmp` `min`/`max`/`clamp`
            // over a `String`-field `Ord` type: `match a.cmp(b) { Greater => b,
            // _ => a }` returns one param and the OTHER was never freed. The
            // RETURNED param is move-suppressed by `suppress_cleanup_for_tail_-
            // return` (below), so its entry-copy is not double-freed. The
            // caller side (`compile_generic_call`) registers the drop of the
            // ORIGINAL moved-in arg buffer, mirroring `compile_call` — the two
            // MUST stay paired (the callee returns an INDEPENDENT copy, so the
            // caller's original is orphaned without it). `ref`/`mut ref` params
            // (borrows, no ownership) are excluded by the `Path(_)`-only gate.
            if matches!(&param.ty.kind, TypeKind::Path(_)) {
                if let Some(concrete) = self.var_type_names.get(&param_name).cloned() {
                    self.make_aggregate_param_callee_owned(&concrete, alloca);
                }
            }
            // B-2026-07-03-23 layer 4: record the CONCRETE generic instantiation
            // of a generic-struct param (`self: (ref) Box[T]` with
            // `type_subst_names["T"] = "f64"` → `Box[f64]`) into the name-keyed
            // `enum_inst_var_types`, so a nested method call ON that param
            // (`self.hi()` inside `gap`) can recover the receiver's args and
            // route the inner call through the mono pipeline at the same
            // instantiation. Without this, `enum_inst_type_of_expr(self)` is
            // empty inside the mono body, the inner call binds no `T`, and the
            // inner mono/return mis-resolves (a `double` subtraction returned
            // through an `i64`-typed `gap` → module-verifier reject). Only
            // records when at least one generic arg is a bound type param — a
            // fully-concrete param instantiation is already covered by the
            // struct-literal span record.
            {
                let peeled = match &param.ty.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                    _ => &param.ty,
                };
                if let Some(inst) = self.concrete_generic_struct_inst(peeled) {
                    self.enum_inst_var_types.insert(param_name.clone(), inst);
                }
            }
            // B-2026-07-02-11: register the collection / String / struct
            // side-tables for the parameter via the same registrar
            // `compile_function` uses. This subsumes the older tensor-only
            // registration (shape-generic bodies indexing `Tensor` params)
            // and extends it to the whole collection surface: without it, a
            // `for x in xs` over a `Vec` param inside a mono SILENTLY
            // compiled to nothing (the for lowering's unknown-iterable
            // fallback skips the body), and any collection method
            // (`xs.len()`, `xs[i]`) failed loudly with "no handler for
            // method". The active `type_subst` (set by `compile_generic_call`
            // around this call) resolves generic element types (`Vec[T]`).
            // A `ref`-mode param registers off its inner type, pairing with
            // its `ref_params` entry above. SoA-active params keep the
            // minimal binding — their access paths lower through
            // `layout_subst` / `ref_params`, and the AoS vec side-tables
            // would shadow that.
            if self.active_soa_layout(&param_name).is_none() {
                let registration_te = match &param.ty.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                    _ => &param.ty,
                };
                self.register_var_from_type_expr(&param_name, registration_te);
                // A bare-type-param param bound to a handle-backed builtin
                // (Column/Tensor) registers from the call site's recorded
                // arg type — the declared te is just `C`, which the
                // registrar above can't act on (S6a; see
                // `mono_handle_param_infos`).
                let handle_info = self
                    .mono_handle_param_infos
                    .get(mangled)
                    .and_then(|entries| entries.iter().find(|(n, _)| n == &param_name))
                    .map(|(_, info)| info.clone());
                match handle_info {
                    Some(super::state::MonoHandleArgInfo::Column(ci)) => {
                        let info = self.column_var_info_from_table(&ci);
                        self.column_var_infos.insert(param_name.clone(), info);
                    }
                    Some(super::state::MonoHandleArgInfo::Tensor(ti)) => {
                        let info = self.tensor_var_info_from_table(&ti);
                        self.tensor_var_infos.insert(param_name.clone(), info);
                    }
                    None => {}
                }
                // Owned (bare, non-ref) String/Vec params: retaining consume
                // sites must deep-copy — the same owned-header set
                // `compile_function` records (see `owned_vecstr_params`).
                if !matches!(
                    param.ty.kind,
                    TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
                ) && self.vec_elem_types.contains_key(&param_name)
                {
                    self.owned_vecstr_params.insert(param_name.clone());
                }
            }
            // B-2026-07-02-11: a `Fn(...)`-typed param is a closure fat
            // pointer; register its env-first closure-call ABI fn type so a
            // body call `f(x)` routes through `compile_closure_call` —
            // mirroring `compile_function`'s registration (functions.rs,
            // B-2026-06-20-1), which this prologue omitted. Without it the
            // call fell through to the unknown-callee const-0 placeholder, so
            // `fn apply[T](x: T, f: Fn(T) -> T) -> T { f(x) }` silently
            // returned 0 under `karac build` (correct under `karac run`).
            // The active `type_subst` resolves generic refs (`T`) inside the
            // `Fn` shape to this mono's concrete LLVM types.
            if let TypeKind::FnType {
                params,
                return_type,
                ..
            } = &param.ty.kind
            {
                let fn_type = self.closure_abi_fn_type(params, return_type.as_deref());
                self.closure_fn_types.insert(param_name.clone(), fn_type);
            }
            self.variables.insert(
                param_name,
                VarSlot {
                    ptr: alloca,
                    ty: param_val.get_type(),
                },
            );
        }

        // Per-layout-monomorphization backward axis (slice 3): in a return-SoA
        // mono, seed the local(s) that flow to the return value with the
        // receiving binding's layout, so the body's construction
        // (`let out = Vec.new()`), mutation (`out.push(…)`), and tail
        // (`out`) all lower SoA via `active_soa_layout` — and the returned
        // value is the 4-field SoA struct the patched signature
        // (`declare_mono_function`) returns. Seeding happens AFTER the param
        // prologue so a returned local never shadows a SoA param's slot.
        // No-op outside a return-SoA mono.
        let ret_block = match &self.return_layout {
            LayoutId::Soa(block) => Some(block.clone()),
            LayoutId::Aos => None,
        };
        if let Some(block) = ret_block {
            for name in self.soa_return_local_names(&func.body) {
                self.layout_subst.insert(name, LayoutId::Soa(block.clone()));
            }
        }

        // Slice-parameter scoped-alias metadata (alias-metadata slice 4). After
        // the mono param registration above, before the body — same ordering as
        // the non-generic `compile_function` path.
        self.build_slice_alias_scopes(func);

        let result = self.compile_block(&func.body)?;

        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // Drain the function-level cleanup frame at the tail return,
            // mirroring `compile_function`. Move-aware suppression first:
            // when the body's tail is a bare Identifier naming an owned
            // local that is moved out as the return value (`matmul`'s
            // `out`), null its slot / flip its sentinel so the
            // `FreeTensor` / `FreeVecBuffer` walk skips it — the caller now
            // owns the value. (Early `return` statements drain via their
            // own path in `compile_expr`; that path is reached only when
            // the block left a terminator, so it's excluded here.)
            self.suppress_cleanup_for_tail_return(&func.body);
            // InterpolatedStringLit-tail suppression — the mono twin of
            // `compile_function`'s block (functions.rs). When a generic fn's
            // final expression is a bare `f"…"`, the loaded {data, len, cap} is
            // the return value, but the f-string accumulator's queued
            // `FreeVecBuffer` would free `data` between the return-value load
            // and `ret` — handing the caller a dangling pointer that its own
            // binding then frees again (double-free; the `describe[T:
            // Display](x) { f"..{x}.." }` shape). `suppress_cleanup_for_tail_-
            // return` only covers Identifier-tail moves, so without this a mono
            // tail f-string leaked the suppression the non-generic path already
            // had. Zero the acc's `cap` so its cleanup no-ops; the caller owns
            // the buffer. Guarded on the syntactic f-string tail exactly like
            // `compile_function`.
            if matches!(
                func.body.final_expr.as_deref().map(|e| &e.kind),
                Some(ExprKind::InterpolatedStringLit(_))
            ) {
                if let Some(acc) = self.last_fstr_acc.take() {
                    self.zero_vec_alloca_cap(acc);
                }
            }
            self.emit_scope_cleanup();
            // A VOID monomorph whose body tail still compiled to a value must
            // `ret void`, mirroring the non-generic `compile_function` guard
            // (functions.rs, `fn_returns_void`). A statement-position tail `if`
            // yields a default `i64 0` from `compile_block`, so without this
            // guard `fn f[T](x: T) { if true { } }` monomorphized to
            // `ret i64 0` in a void function and failed module verification
            // ("non-void return in Function of void return type", B-2026-07-11-28).
            // `compile_mono_function` uses the conventional by-value return ABI
            // (no sret / niche / box paths here), so a void LLVM return type is
            // always a true unit-returning fn — no result to store elsewhere.
            let fn_returns_void = self
                .current_fn
                .and_then(|f| f.get_type().get_return_type())
                .is_none();
            match result {
                Some(val) if !fn_returns_void => {
                    // Scalar width coercion at the tail-ret boundary, mirroring
                    // the non-generic path. A mono declared `-> u8` whose body
                    // tail is the i64 literal `255` would otherwise emit
                    // `ret i64 255` into an `i8`-returning fn and fail module
                    // verification; `coerce_to_current_ret_type` truncates to the
                    // declared narrow return width (no-op for matching /
                    // non-scalar returns). Narrow-width return from a generic
                    // mono (B-2026-07-03-N).
                    let val = self.coerce_to_current_ret_type(val);
                    self.builder.build_return(Some(&val)).unwrap();
                }
                _ => {
                    self.builder.build_return(None).unwrap();
                }
            }
        }
        // Leave the frame stack as the caller swapped it in
        // (`compile_generic_call` restores its own); clearing keeps the
        // post-body state tidy and matches `compile_function`'s exit.
        self.scope_cleanup_actions.clear();

        Ok(())
    }

    /// The local binding name(s) that flow to this function's return value as
    /// a bare `Vec[E]` identifier — used by the return-SoA monomorph path
    /// (slice 3) to seed them with the receiving binding's layout so the body
    /// builds + returns the SoA struct. Seeding and the matching move-out
    /// suppression must agree on the same name set, or a returned local would
    /// build SoA without its `FreeSoaGroups` suppressed (leak / UAF) or be
    /// suppressed without building SoA (type mismatch).
    ///
    /// Collects EVERY bare-identifier return site, not just the single tail
    /// (the branch-leaf / multi-`return` follow-on): every explicit
    /// `return <id>;` reachable in the body (in any branch / loop / nested
    /// block, but NOT inside a closure — its `return` exits the closure, not
    /// this function) AND every tail leaf of a branch-bearing tail expression
    /// (`if c { a } else { b }` contributes both `a` and `b`). Without the
    /// extra sites a guard-clause helper (`if empty { return fallback; } …;
    /// result`) lowered only `result` SoA, leaving the early `return fallback`
    /// returning the AoS `{ptr,len,cap}` against the SoA-patched return
    /// signature — an LLVM "return type does not match" verify failure.
    pub(super) fn soa_return_local_names(&self, body: &Block) -> Vec<String> {
        let mut names = Vec::new();
        self.collect_soa_return_idents_block(body, true, &mut names);
        names.sort();
        names.dedup();
        names
    }

    /// Walk a block for return-position bare identifiers. `in_tail` marks
    /// whether the block's *value* position is itself the function's return
    /// value (so its tail leaf is a return site). Every statement is still
    /// scanned for explicit `return <id>;` regardless of `in_tail`.
    fn collect_soa_return_idents_block(
        &self,
        block: &Block,
        in_tail: bool,
        names: &mut Vec<String>,
    ) {
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            if let StmtKind::Expr(e) = &stmt.kind {
                // The block's value is the last statement iff there is no
                // `final_expr`; that position inherits `in_tail`. Every other
                // statement is non-tail (scanned only for explicit returns).
                let stmt_in_tail = in_tail && block.final_expr.is_none() && i + 1 == n;
                self.collect_soa_return_idents_expr(e, stmt_in_tail, names);
            }
        }
        if let Some(fe) = &block.final_expr {
            self.collect_soa_return_idents_expr(fe, in_tail, names);
        }
    }

    /// Walk an expression for return-position bare identifiers. `in_tail` ⇒
    /// this expression is in the function's return/tail position, so a bare
    /// `Identifier` here is a returned local. An explicit `return E` puts `E`
    /// in return position regardless of `in_tail`. Branch-bearing forms recurse
    /// with `in_tail` preserved on their value leaves; loops recurse with
    /// `in_tail = false` (their value is `Unit`). Closures are a boundary —
    /// their `return` exits the closure, not this function.
    fn collect_soa_return_idents_expr(&self, expr: &Expr, in_tail: bool, names: &mut Vec<String>) {
        match &expr.kind {
            ExprKind::Identifier(name) if in_tail => {
                names.push(name.clone());
            }
            ExprKind::Return(Some(boxed)) => {
                self.collect_soa_return_idents_expr(boxed, true, names);
            }
            ExprKind::Return(None) => {}
            ExprKind::Closure { .. } => {}
            ExprKind::Block(b)
            | ExprKind::LabeledBlock { body: b, .. }
            | ExprKind::Unsafe(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b)
            | ExprKind::Try(b)
            | ExprKind::Lock { body: b, .. }
            | ExprKind::Providers { body: b, .. } => {
                self.collect_soa_return_idents_block(b, in_tail, names);
            }
            ExprKind::If {
                then_block,
                else_branch,
                ..
            }
            | ExprKind::IfLet {
                then_block,
                else_branch,
                ..
            } => {
                self.collect_soa_return_idents_block(then_block, in_tail, names);
                if let Some(eb) = else_branch {
                    self.collect_soa_return_idents_expr(eb, in_tail, names);
                }
            }
            ExprKind::Match { arms, .. } => {
                for arm in arms {
                    self.collect_soa_return_idents_expr(&arm.body, in_tail, names);
                }
            }
            ExprKind::While { body, .. }
            | ExprKind::WhileLet { body, .. }
            | ExprKind::For { body, .. }
            | ExprKind::Loop { body, .. } => {
                self.collect_soa_return_idents_block(body, false, names);
            }
            _ => {}
        }
    }

    /// Infer the type-parameter substitution for a generic function call by
    /// matching each parameter's declared type against the concrete argument type.
    pub(super) fn infer_type_args(
        &self,
        func: &Function,
        arg_vals: &[BasicValueEnum<'ctx>],
    ) -> HashMap<String, BasicTypeEnum<'ctx>> {
        let mut subst = HashMap::new();
        for (param, val) in func.params.iter().zip(arg_vals.iter()) {
            self.unify_type_expr(&param.ty, val.get_type(), &mut subst);
        }
        subst
    }

    /// If `te` is a generic user-struct instantiation (`Box[T]`, `Pair[T]`)
    /// carrying at least one generic arg that is a bare type param bound in the
    /// active monomorph (`type_subst_names`), return the CONCRETE instantiation
    /// with those params substituted (`Box[f64]`). `None` for a non-struct, a
    /// non-generic struct, a struct whose args resolve to nothing bound, or any
    /// shape without a recorded struct-generic-param list. Used to seed
    /// `enum_inst_var_types` for a generic-struct method param so nested
    /// self-method calls re-enter the mono pipeline at the same instantiation
    /// (B-2026-07-03-23 layer 4).
    pub(super) fn concrete_generic_struct_inst(&self, te: &TypeExpr) -> Option<TypeExpr> {
        let TypeKind::Path(path) = &te.kind else {
            return None;
        };
        let name = path.segments.last()?;
        // Must be a struct with declared generic params (Box, Pair, …).
        if self
            .struct_generic_params
            .get(name)
            .is_none_or(|p| p.is_empty())
        {
            return None;
        }
        let args = path.generic_args.as_ref()?;
        let mut any_bound = false;
        let new_args: Vec<GenericArg> = args
            .iter()
            .map(|a| match a {
                GenericArg::Type(t) => {
                    // A bare-type-param arg resolves through the active
                    // name-subst to its concrete type name; wrap it back into a
                    // Path TypeExpr. Already-concrete args pass through.
                    if let TypeKind::Path(p) = &t.kind {
                        if p.segments.len() == 1 && p.generic_args.is_none() {
                            if let Some(concrete) = self.type_subst_names.get(&p.segments[0]) {
                                any_bound = true;
                                return GenericArg::Type(TypeExpr {
                                    kind: TypeKind::Path(PathExpr {
                                        segments: vec![concrete.clone()],
                                        generic_args: None,
                                        span: t.span.clone(),
                                    }),
                                    span: t.span.clone(),
                                });
                            }
                        }
                    }
                    GenericArg::Type(t.clone())
                }
                other => other.clone(),
            })
            .collect();
        if !any_bound {
            return None;
        }
        Some(TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![name.clone()],
                generic_args: Some(new_args),
                span: te.span.clone(),
            }),
            span: te.span.clone(),
        })
    }

    /// Recursively match a declared type expression against a concrete LLVM type,
    /// recording bindings for any unbound type parameters found.
    pub(super) fn unify_type_expr(
        &self,
        ty: &TypeExpr,
        concrete: BasicTypeEnum<'ctx>,
        subst: &mut HashMap<String, BasicTypeEnum<'ctx>>,
    ) {
        if let TypeKind::Path(path) = &ty.kind {
            if path.segments.len() == 1 && path.generic_args.is_none() {
                let name = &path.segments[0];
                // Treat as a type parameter if it's not a known concrete type.
                if !self.is_known_concrete_type(name) {
                    subst.entry(name.clone()).or_insert(concrete);
                }
            }
            // TODO: unify generic args (e.g. `Vec[T]`) when container types are codegen'd.
        }
    }

    /// Returns true if `name` is a built-in concrete type or a declared struct/enum.
    pub(super) fn is_known_concrete_type(&self, name: &str) -> bool {
        matches!(
            name,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "isize"
                | "usize"
                | "f32"
                | "f64"
                | "bool"
                | "str"
                | "String"
                | "char"
        ) || self.struct_types.contains_key(name)
            || self.enum_layouts.contains_key(name)
    }

    /// A scalar-primitive type name whose mangle token would be lossy: narrow
    /// ints widen to `i64` (losing width AND signedness), so the concrete name
    /// must be threaded into the mono mangle to keep per-width instantiations
    /// distinct (B-2026-07-03-24). `i64`/`f32`/`f64`/`bool`/`char` are included
    /// too — appending them is a no-op vs their existing token, so the symbol is
    /// unchanged for those.
    fn is_scalar_primitive_mangle_name(name: &str) -> bool {
        matches!(
            name,
            "i8" | "i16"
                | "i32"
                | "i64"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "usize"
                | "isize"
                | "f32"
                | "f64"
                | "bool"
                | "char"
        )
    }

    /// Build a mangled name for a specialization, e.g. `max$i64` or `zip$i64$f64`.
    ///
    /// `layout_subst` adds the per-layout-monomorphization axis: a layout
    /// suffix (`$soa_<name>`) for any layout-carrying value param whose active
    /// `LayoutId` is non-`Aos`, so each layout variant is a distinct LLVM
    /// symbol (`docs/spikes/per-layout-monomorphization.md` §4.3). `Aos`
    /// contributes no suffix, so an all-`Aos` call keeps the existing symbol.
    /// `return_layout` adds the backward-inference axis (slice 3): a non-`Aos`
    /// *return* layout appends a `$ret_soa_<name>` suffix, so a helper called
    /// to return one layout vs. another (or vs. plain AoS) is a distinct symbol.
    // Each argument is a distinct monomorphization axis (type / name / const /
    // layout / return-layout); collapsing them into a struct would only move the
    // arity into a builder with no readability gain.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn mangle_mono_name(
        &self,
        base: &str,
        func: &Function,
        subst: &HashMap<String, BasicTypeEnum<'ctx>>,
        subst_names: &HashMap<String, String>,
        const_subst: &HashMap<String, crate::prelude::ConstValue>,
        layout_subst: &HashMap<String, LayoutId>,
        return_layout: &LayoutId,
    ) -> String {
        let mut mangled = base.to_string();
        // Type / const generic axes (only for a generic function — a
        // non-generic layout-monomorph has no `generic_params`).
        if let Some(gp) = &func.generic_params {
            for param in &gp.params {
                // Const generics slice 1b: const params take priority over
                // type subst when both maps are populated (the const_subst
                // is keyed by formal name, the type subst doesn't carry
                // const params).
                if param.is_const {
                    if let Some(cv) = const_subst.get(&param.name) {
                        mangled.push('$');
                        mangled.push_str(&const_value_to_mangle_str(cv));
                    }
                } else if let Some(ty) = subst.get(&param.name) {
                    mangled.push('$');
                    let token = self.llvm_type_to_mangle_str(*ty);
                    // Prefer the concrete NAME from `subst_names` when it names a
                    // scalar primitive: narrow ints (i8/i16/i32/u8/u16/u32) are
                    // WIDENED to i64 before the call, so `token` is "i64" for
                    // every narrow width — two distinct instantiations
                    // (`tag_it$i64` for both an i8 and an i32 call) would collide
                    // and the second reuse the first's body, dispatching a
                    // bound-trait method (`x.tag()`) to the wrong width's impl,
                    // or losing u8-vs-i8 comparison signedness. The `token` also
                    // erases every unsigned width to its signed spelling. Append
                    // the exact declared name instead (same spelling for a
                    // non-widened `i64`/`f64`, so those symbols are unchanged).
                    // This is the primitive analog of the struct/enum name-append
                    // just below (B-2026-07-03-11); B-2026-07-03-24.
                    if let Some(name) = subst_names.get(&param.name) {
                        if Self::is_scalar_primitive_mangle_name(name) {
                            mangled.push_str(name);
                            continue;
                        }
                    }
                    // Every user struct/enum lowers to the opaque `"struct"`
                    // token, so two same-shape-but-distinct instantiations
                    // (`use_it$A` vs `use_it$B`, both `{i64}`) would collide and
                    // the second silently reuse the first's body — miscompiling
                    // any name-dependent behavior (field access, bound-trait
                    // method dispatch, B-2026-07-03-11). Disambiguate by the
                    // concrete type NAME, but ONLY for a USER struct/enum — a
                    // builtin whose layout is `"struct"` (String, Vec, Map, …)
                    // keeps the `$struct` token so its existing per-mono symbols
                    // are unchanged (its method dispatch never keys on
                    // `var_type_names`, so the opaque token is still sound).
                    if token == "struct" {
                        if let Some(name) = subst_names.get(&param.name) {
                            if self.struct_types.contains_key(name)
                                || self.enum_layouts.contains_key(name)
                            {
                                mangled.push_str(name);
                                continue;
                            }
                        }
                    }
                    mangled.push_str(&token);
                }
            }
        }
        // Per-layout-monomorphization axis: append a per-param layout suffix
        // for any value param carrying a non-`Aos` layout. Applies to generic
        // and non-generic functions alike (slice 2 monomorphizes plain `Vec[E]`
        // helpers per the caller's arg layout). The param NAME is part of the
        // suffix (`$<param>_soa_<layout>`) so that two different layout
        // assignments over the same params can't collide — e.g. `f(grid,plain)`
        // (`$a_soa_grid`) vs `f(plain,grid)` (`$b_soa_grid`) are distinct
        // monomorphs. An all-`Aos` call adds no suffix, so the symbol is
        // unchanged for non-SoA code.
        for param in &func.params {
            if let Some(name) = param.name() {
                if let Some(suffix) = layout_subst.get(name).and_then(LayoutId::mangle_suffix) {
                    mangled.push('$');
                    mangled.push_str(name);
                    mangled.push('_');
                    mangled.push_str(&suffix);
                }
            }
        }
        // Per-layout-monomorphization backward axis (slice 3): a non-`Aos`
        // return layout appends `$ret_soa_<name>`. Disjoint from the per-param
        // `$<param>_soa_<name>` suffixes (the `ret` keyword can't be a param
        // name), so a fn that both takes and returns SoA gets both.
        if let Some(suffix) = return_layout.mangle_suffix() {
            mangled.push_str("$ret_");
            mangled.push_str(&suffix);
        }
        mangled
    }

    /// Forward layout-flow inference for a call
    /// (`docs/spikes/per-layout-monomorphization.md` §4.2): the `LayoutId` of
    /// each layout-carrying (`Vec[E]`) value param, keyed by param name. This
    /// is the layout half of the monomorph key fed to `mangle_mono_name` and
    /// (slice 2) to body lowering via `self.layout_subst`.
    ///
    /// **Forward (arguments):** a param's `LayoutId` is the binding-site layout
    /// of the matching argument's *root* — but only when the argument is a bare
    /// binding (a whole `Vec[E]`). A projection (`grid[i]`, `g.field`) yields a
    /// materialized AoS element/field, so it is `Aos`; nested layout-through-
    /// aggregate flow is deferred (spike §8). When the matching argument's root
    /// is a `layout`-declared / SoA-forwarded binding, the param is `Soa(name)`,
    /// monomorphizing the callee against the caller's physical layout.
    pub(super) fn compute_call_layout_subst(
        &self,
        func: &Function,
        args: &[CallArg],
    ) -> HashMap<String, LayoutId> {
        let mut layout_subst = HashMap::new();
        for (i, param) in func.params.iter().enumerate() {
            if !Self::param_is_layout_carrying(param) {
                continue;
            }
            let Some(name) = param.name() else { continue };
            let layout = args
                .get(i)
                .map(|a| self.arg_root_layout_id(&a.value))
                .unwrap_or(LayoutId::Aos);
            layout_subst.insert(name.to_string(), layout);
        }
        layout_subst
    }

    /// The `LayoutId` an argument expression contributes to forward inference.
    /// Only a bare binding (whole `Vec[E]`) carries its binding-site layout; any
    /// other shape (projection, call result, literal) is `Aos` for the first
    /// slices (top-level `Vec[E]` only — spike §8).
    fn arg_root_layout_id(&self, expr: &Expr) -> LayoutId {
        match &expr.kind {
            ExprKind::Identifier(name) => self.active_layout_id(name),
            _ => LayoutId::Aos,
        }
    }

    /// The active physical layout of a binding at a *use site* in the current
    /// codegen context, read purely from the value carriers (slice 5 — no
    /// name-keyed `soa_layouts` lookup): the per-call layout subst (a
    /// SoA-forwarded param/return in the active monomorph) takes precedence,
    /// then the per-binding `binding_layouts` carrier (an in-function local
    /// seeded at its binding site by `seed_binding_site_layout`), else `Aos`.
    /// This is design.md Feature 1's "the value carrier is a `LayoutId`
    /// attached to bindings, not the binding name": a binding reads as SoA iff
    /// it was *made* SoA — by the call dispatch (`layout_subst`) or at its `let`
    /// (`binding_layouts`) — so a base-symbol param that merely shares a name
    /// with a `layout` block no longer lowers SoA by coincidence.
    pub(super) fn active_layout_id(&self, binding_name: &str) -> LayoutId {
        if let Some(layout) = self.layout_subst.get(binding_name) {
            return layout.clone();
        }
        if let Some(layout) = self.binding_layouts.get(binding_name) {
            return layout.clone();
        }
        LayoutId::Aos
    }

    /// Resolve a `let` binding's layout from its binding *site* and, if SoA,
    /// seed the per-binding `binding_layouts` carrier so every downstream use
    /// reads it via `active_layout_id` (no further name-keyed lookups). This is
    /// the **one sanctioned origin name-match** (design.md Feature 1: "layout
    /// binds to the binding site"): the binding's layout is the active
    /// `layout_subst` entry if present — a returned local seeded by a return-SoA
    /// mono (slice 3), or a name the dispatch already laid out — otherwise the
    /// `layout <name>` origin map keyed by the binding's own name. Returns the
    /// resolved `SoaLayout` (and records the carrier) for a SoA binding, or
    /// `None` for an `Aos` one. Called only from the `let` arm; use sites read
    /// `active_soa_layout`, which never touches the origin map.
    pub(super) fn seed_binding_site_layout(
        &mut self,
        binding_name: &str,
    ) -> Option<super::state::SoaLayout> {
        let layout = if let Some(layout) = self.layout_subst.get(binding_name) {
            // A returned local seeded by a return-SoA mono, or a name the
            // dispatch already laid out. Honored even for a returned local —
            // this IS the return-mono's SoA seeding.
            layout.clone()
        } else if self.soa_layouts.contains_key(binding_name)
            && !self.soa_return_locals.contains(binding_name)
        {
            // Origin name-match — but NOT for a returned local. A returned
            // local's layout is dictated by the function's `return_layout`
            // (handled by the `layout_subst` arm above in a return-SoA mono);
            // matching it by name here would lower the body SoA in the AoS base
            // symbol / a forward-only mono, clashing with the AoS return type.
            LayoutId::Soa(binding_name.to_string())
        } else {
            LayoutId::Aos
        };
        match layout {
            LayoutId::Soa(block) => {
                self.binding_layouts
                    .insert(binding_name.to_string(), LayoutId::Soa(block.clone()));
                self.soa_layouts.get(&block).cloned()
            }
            LayoutId::Aos => None,
        }
    }

    /// The `SoaLayout` metadata for a binding whose active layout is `Soa`, or
    /// `None` when it is `Aos`. Resolves the `Soa(<block-name>)` id through the
    /// `soa_layouts` origin map. The single body-lowering trigger that replaces
    /// the raw `soa_layouts.get(name)` / `.contains_key(name)` access checks, so
    /// a mono SoA param (not itself a `layout`-block name) lowers SoA.
    pub(super) fn active_soa_layout(&self, binding_name: &str) -> Option<super::state::SoaLayout> {
        match self.active_layout_id(binding_name) {
            LayoutId::Soa(block) => self.soa_layouts.get(&block).cloned(),
            LayoutId::Aos => None,
        }
    }

    /// The `SoaLayout` for a value param whose active `LayoutId` (in the current
    /// monomorph's `layout_subst`) is `Soa` — drives the SoA param signature
    /// and prologue in the mono path. Returns `None` outside a layout-monomorph
    /// (empty `layout_subst`), so the normal `compile_function` pass is
    /// unaffected and the name-keyed declaring-fn path still applies.
    pub(super) fn active_param_soa_layout(&self, param: &Param) -> Option<super::state::SoaLayout> {
        // By-value only (slice 4): a `ref`/`mut ref Vec[E]` SoA param keeps its
        // pointer ABI — the caller passes `&struct` and the mono body derefs
        // once through `ref_params` — so its *signature* is NOT patched to the
        // SoA struct by value. Only an owned by-value `Vec[E]` param's
        // signature becomes the 4-field SoA struct. (The param still carries a
        // `Soa` entry in `layout_subst`, which drives the body's access paths
        // via `active_soa_layout`; this guard only suppresses the signature
        // rewrite for the borrow forms.)
        if matches!(&param.ty.kind, TypeKind::Ref(_) | TypeKind::MutRef(_)) {
            return None;
        }
        let name = param.name()?;
        match self.layout_subst.get(name) {
            Some(LayoutId::Soa(block)) => self.soa_layouts.get(block).cloned(),
            _ => None,
        }
    }

    /// Whether a value-or-borrow param's declared type is a layout-carrying
    /// collection — a `Vec[E]` (owned `Vec[E]`, `ref Vec[E]`, or
    /// `mut ref Vec[E]`) whose physical layout the per-layout-monomorphization
    /// axis can vary (`Aos` vs an SoA grouping). Peels one `ref`/`mut ref` so
    /// borrow forms also gate the dispatch + populate `layout_subst` (slice 4:
    /// a SoA buffer through a shared by-ref helper monomorphizes per the
    /// caller's buffer layout, regardless of the param name). The *signature*
    /// difference between owned and borrow forms is handled downstream by
    /// `active_param_soa_layout` (by-value gets the SoA struct; borrow keeps
    /// the pointer ABI and derefs in the body).
    pub(super) fn param_is_layout_carrying(param: &Param) -> bool {
        let underlying = match &param.ty.kind {
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => &inner.kind,
            other => other,
        };
        matches!(
            underlying,
            TypeKind::Path(path) if path.segments.last().map(String::as_str) == Some("Vec")
        )
    }

    /// Whether a function's declared return type is a layout-carrying `Vec[E]`
    /// — the backward-inference (slice 3) analog of `param_is_layout_carrying`.
    /// Gates the return-SoA monomorph: only a function that returns a whole
    /// `Vec[E]` can be specialized to return an SoA struct.
    pub(super) fn return_is_layout_carrying(func: &Function) -> bool {
        matches!(
            func.return_type.as_ref().map(|t| &t.kind),
            Some(TypeKind::Path(path)) if path.segments.last().map(String::as_str) == Some("Vec")
        )
    }

    /// Whether a `let`-binding RHS is a direct call to a known user function
    /// whose return type is a layout-carrying `Vec[E]` — the gate for the
    /// backward-inference SoA-let path (slice 3). Matches `compile_call`'s
    /// callee-name extraction (bare identifier / single-segment path), so the
    /// callee resolved here is exactly the one the dispatch monomorphizes.
    /// Excludes `Vec.new()` (a 2-segment `Vec::new` path handled by
    /// `compile_soa_new`) and any non-`fn_asts` callee (intrinsics, generics),
    /// keeping the SoA-let path in lockstep with the backward dispatch — so the
    /// bound call result is always the SoA struct the slot expects.
    pub(super) fn let_rhs_calls_layout_returning_fn(&self, value: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &value.kind else {
            return false;
        };
        let name = match &callee.kind {
            ExprKind::Identifier(n) => n.as_str(),
            ExprKind::Path {
                segments,
                generic_args: None,
            } if segments.len() == 1 => segments[0].as_str(),
            _ => return false,
        };
        self.fn_asts
            .get(name)
            .is_some_and(Self::return_is_layout_carrying)
    }

    /// Slice 0.a sub-step 2 — codegen monomorphization-request bound
    /// enforcement.
    ///
    /// Walks both inline-form (`fn f[T: Bound]`) and where-clause
    /// (`fn f[T] where T: Bound`) bounds against the concrete LLVM
    /// substitution. Returns `Err` when a primitive LLVM type
    /// demonstrably fails to satisfy a built-in trait bound (e.g.
    /// `f64` for `Hash` / `Eq` / `Ord`), matching the typechecker's
    /// `type_supports_*` shape on primitives.
    ///
    /// **Scope is intentionally narrow.** The typechecker discharges
    /// bound violations at every call site (`discharge_type_bounds`),
    /// so this hook is purely defense-in-depth for paths that reach
    /// codegen without a typechecker pass (no such path exists in the
    /// single-CU compiler today, but cross-module compilation would
    /// open one). Coverage:
    /// - Built-in traits (`Hash` / `Eq` / `PartialEq` / `Ord` /
    ///   `PartialOrd` / `Display` / `Clone` / `Copy`) checked against
    ///   primitive LLVM types via `llvm_type_satisfies_trait`.
    /// - Non-primitive LLVM types (pointers, structs) and unknown
    ///   trait names fall through permissively — verifying those
    ///   requires plumbing the typechecker's impl table into codegen
    ///   (deferred; tracked as a hard-stop trigger in
    ///   `phase-7-codegen.md § Trait-bounds-at-codegen enforcement`).
    pub(super) fn verify_bounds_at_codegen(
        &self,
        generic_fn: &Function,
        subst: &HashMap<String, BasicTypeEnum<'ctx>>,
    ) -> Result<(), String> {
        if let Some(gp) = &generic_fn.generic_params {
            for param in &gp.params {
                if param.bounds.is_empty() {
                    continue;
                }
                let Some(concrete) = subst.get(&param.name) else {
                    continue;
                };
                for bound in &param.bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if !self.llvm_type_satisfies_trait(*concrete, trait_name) {
                        return Err(format!(
                            "trait bound `{}: {}` is not satisfied at monomorphization site for `{}` \
                             (concrete type `{}` does not implement `{}`)",
                            param.name,
                            trait_name,
                            generic_fn.name,
                            self.llvm_type_to_mangle_str(*concrete),
                            trait_name,
                        ));
                    }
                }
            }
        }

        if let Some(wc) = &generic_fn.where_clause {
            for constraint in &wc.constraints {
                let WhereConstraint::TypeBound {
                    type_name, bounds, ..
                } = constraint
                else {
                    continue;
                };
                let Some(concrete) = subst.get(type_name) else {
                    continue;
                };
                for bound in bounds {
                    let Some(trait_name) = bound.path.last() else {
                        continue;
                    };
                    if !self.llvm_type_satisfies_trait(*concrete, trait_name) {
                        return Err(format!(
                            "trait bound `{}: {}` is not satisfied at monomorphization site for `{}` \
                             (concrete type `{}` does not implement `{}`)",
                            type_name,
                            trait_name,
                            generic_fn.name,
                            self.llvm_type_to_mangle_str(*concrete),
                            trait_name,
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    /// Conservative LLVM-type-vs-built-in-trait predicate used by
    /// `verify_bounds_at_codegen`. Mirrors the typechecker's
    /// `type_supports_*` helpers but operates on `BasicTypeEnum`
    /// instead of `Type`. Permissive on non-primitive shapes
    /// (`PointerType`, `StructType`) and unknown trait names — those
    /// cases are the typechecker's responsibility today; the codegen
    /// hook only catches the unambiguous primitive violations
    /// (f32/f64 failing `Hash` / `Eq` / `Ord`).
    pub(super) fn llvm_type_satisfies_trait(
        &self,
        ty: BasicTypeEnum<'ctx>,
        trait_name: &str,
    ) -> bool {
        match trait_name {
            "Hash" | "Eq" | "Ord" => !matches!(ty, BasicTypeEnum::FloatType(_)),
            "PartialEq" | "PartialOrd" | "Display" | "Clone" | "Copy" => true,
            _ => true,
        }
    }

    /// Produce a stable string token for an LLVM type suitable for name mangling.
    pub(super) fn llvm_type_to_mangle_str(&self, ty: BasicTypeEnum<'ctx>) -> String {
        match ty {
            BasicTypeEnum::IntType(t) => match t.get_bit_width() {
                1 => "bool".to_string(),
                8 => "i8".to_string(),
                16 => "i16".to_string(),
                32 => "i32".to_string(),
                64 => "i64".to_string(),
                w => format!("i{}", w),
            },
            BasicTypeEnum::FloatType(t) => {
                // Distinguish f32 from f64 by comparing with context-canonical types.
                if t == self.context.f32_type() {
                    "f32".to_string()
                } else {
                    "f64".to_string()
                }
            }
            BasicTypeEnum::PointerType(_) => "ptr".to_string(),
            BasicTypeEnum::StructType(_) => "struct".to_string(),
            _ => "opaque".to_string(),
        }
    }

    // ── Monomorphized Map[K, V] symbol emission (Slice 1) ───────

    /// Byte offsets into the runtime's `#[repr(C)]` `KaracMap`
    /// layout (`runtime/src/map.rs`). Codegen-emitted monomorphized
    /// `Map[K, V]` method symbols load these fields by direct GEP +
    /// load against a `*mut KaracMap` opaque pointer rather than
    /// calling through the type-erased `karac_map_*` runtime
    /// functions. Pinned by the runtime-side unit test
    /// `karac_map_field_offsets_match_codegen` — any drift trips
    /// the runtime test before the binary can diverge.
    const KARAC_MAP_STATUS_OFFSET: u64 = 0;
    const KARAC_MAP_KV_OFFSET: u64 = 8;
    const KARAC_MAP_CAPACITY_OFFSET: u64 = 16;
    const KARAC_MAP_LEN_OFFSET: u64 = 24;
    const KARAC_MAP_TOMBSTONES_OFFSET: u64 = 32;
    /// Bucket status-byte sentinels for the monomorphized probe
    /// loop. Must match the runtime's `BUCKET_EMPTY` /
    /// `BUCKET_OCCUPIED` / `BUCKET_TOMBSTONE` constants in
    /// `runtime/src/map.rs`.
    const BUCKET_EMPTY: u64 = 0;
    const BUCKET_OCCUPIED: u64 = 1;
    const BUCKET_TOMBSTONE: u64 = 2;

    /// Cache key for the monomorphized Map[K, V] symbol family —
    /// `"{key_mangle}_{val_mangle}"` (e.g. `"i64_i64"`). Mirrors the
    /// content-addressed scheme used by `mangle_mono_name` for user
    /// generic fns, expressed in terms of `llvm_type_to_mangle_str`'s
    /// stable token set so distinct K/V tuples never collide.
    pub(super) fn mono_map_cache_key(
        &self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> String {
        format!(
            "{}_{}",
            self.llvm_type_to_mangle_str(key_ty),
            self.llvm_type_to_mangle_str(val_ty),
        )
    }

    /// Gate predicate: does this K/V tuple route through the
    /// monomorphized Map path? Every tuple that returns `false`
    /// falls through to the erased `karac_map_*` runtime per § 3.6
    /// coexist-during-migration. Slice 5 deletes the erased
    /// fallback entirely.
    ///
    /// Slice 1 shipped `Map[i64, i64]`. Slice 2 adds the `i32`
    /// key family — that covers `Map[char, i64]` (the LeetCode #3
    /// kata's K/V tuple, since `char` lowers to LLVM `i32` per
    /// Slice 2.0) and `Map[i32, i64]` if anyone instantiates it.
    /// Both mangle identically (`i32_i64`) and share a single
    /// mono symbol — the K/V slot layout and FNV-1a-over-4-bytes
    /// hash are byte-identical regardless of which surface name
    /// the user wrote.
    pub(super) fn should_use_mono_map_for(
        &self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> bool {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        let key_ok = matches!(key_ty, BasicTypeEnum::IntType(t) if t == i32_t || t == i64_t);
        let val_ok = matches!(val_ty, BasicTypeEnum::IntType(t) if t == i64_t);
        key_ok && val_ok
    }

    /// Lazily emit the monomorphized `Map[K, V]` method-symbol family
    /// for a given K/V tuple and return the cached handles. Each
    /// per-method `FunctionValue` is emitted with `LinkOnceODR`
    /// linkage so cross-crate / cross-TU duplicates collapse at link
    /// time (locked design § 3.2).
    ///
    /// Slice 1a ships **wrapper bodies only**: each mono method
    /// forwards to the corresponding erased `karac_map_*` runtime
    /// function 1:1. The wrapper exists at this slice to validate
    /// emission, mangling, dispatch wiring, and `linkonce_odr`
    /// linkage — `nm | grep karac_map_i64_i64_len | wc -l == 1`
    /// after the slice lands. Slice 1b replaces hot-path bodies
    /// (`insert_old`, `get`) with fully-inlined LLVM (direct i64
    /// hash + icmp eq), unlocking the bench gain.
    pub(super) fn get_or_emit_map_mono_methods(
        &mut self,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> MapMonoMethods<'ctx> {
        let cache_key = self.mono_map_cache_key(key_ty, val_ty);
        if let Some(entry) = self.map_mono_methods.get(&cache_key) {
            return *entry;
        }

        let saved_bb = self.builder.get_insert_block();

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();

        // len: direct GEP + load against the runtime's `#[repr(C)]`
        // `KaracMap.len` field. Drops the function-pointer indirection
        // and the extern call overhead the erased fallback's
        // `karac_map_len` carried. Offset pinned by the runtime-side
        // `karac_map_field_offsets_match_codegen` unit test.
        let len_name = format!("karac_map_{cache_key}_len");
        let len_fn = match self.module.get_function(&len_name) {
            Some(f) => f,
            None => {
                let len_ty = i64_t.fn_type(&[ptr_ty.into()], false);
                let f = self
                    .module
                    .add_function(&len_name, len_ty, Some(Linkage::LinkOnceODR));
                let entry = self.context.append_basic_block(f, "entry");
                self.builder.position_at_end(entry);
                let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
                let i8_t = self.context.i8_type();
                let offset = i64_t.const_int(Self::KARAC_MAP_LEN_OFFSET, false);
                let len_field_ptr = unsafe {
                    self.builder
                        .build_in_bounds_gep(i8_t, map_arg, &[offset], "mono.len.field.ptr")
                        .unwrap()
                };
                let len = self
                    .builder
                    .build_load(i64_t, len_field_ptr, "mono.len")
                    .unwrap();
                self.builder.build_return(Some(&len)).unwrap();
                f
            }
        };

        // insert_old: fast path inlines load-factor check, FNV-1a
        // hash (via direct call to the existing `karac_hash_<K>`
        // helper — same hash as the erased fallback so cross-path
        // consistency holds while coexist is in effect), linear
        // probe with empty / tombstone / occupied switch, and
        // inline K-typed icmp eq. Slow path (resize-needed branch
        // and safety fallback for the impossible exhausted-probe
        // case) forwards to `karac_map_insert_old` extern.
        let insert_name = format!("karac_map_{cache_key}_insert_old");
        let insert_old_fn = match self.module.get_function(&insert_name) {
            Some(f) => f,
            None => {
                let bool_t = self.context.bool_type();
                let insert_ty = bool_t.fn_type(
                    &[ptr_ty.into(), key_ty.into(), val_ty.into(), ptr_ty.into()],
                    false,
                );
                let f =
                    self.module
                        .add_function(&insert_name, insert_ty, Some(Linkage::LinkOnceODR));
                self.emit_mono_map_insert_old_body(f, key_ty, val_ty);
                f
            }
        };

        // get: same shape as insert_old's fast path but read-only.
        // No load-factor branch (get never resizes), no tombstone
        // tracking, no fresh-slot writes. Probe loop terminates on
        // EMPTY (return false) or OCCUPIED-with-matching-key (load
        // val, store to out_val, return true). On exhausted probe
        // (would be unreachable under valid resize policy, but
        // guarded for safety) returns false.
        let get_name = format!("karac_map_{cache_key}_get");
        let get_fn = match self.module.get_function(&get_name) {
            Some(f) => f,
            None => {
                let bool_t = self.context.bool_type();
                let get_ty = bool_t.fn_type(&[ptr_ty.into(), key_ty.into(), ptr_ty.into()], false);
                let f = self
                    .module
                    .add_function(&get_name, get_ty, Some(Linkage::LinkOnceODR));
                self.emit_mono_map_get_body(f, key_ty, val_ty);
                f
            }
        };

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        let methods = MapMonoMethods {
            len_fn,
            insert_old_fn,
            get_fn,
        };
        self.map_mono_methods.insert(cache_key, methods);
        methods
    }

    /// Emit the fast-path-inlined body of the monomorphized
    /// `karac_map_<K>_<V>_insert_old` function. The shape mirrors
    /// the runtime's `KaracMap::insert` algorithm
    /// (`runtime/src/map.rs:166`) — load-factor branch first,
    /// then linear probe — but inlines the hash (via direct call
    /// to `karac_hash_<K>`, the same FNV-1a helper the erased
    /// fallback's function-pointer hash dispatches to) and the eq
    /// (direct icmp on the K LLVM type), dropping the function-
    /// pointer indirection that defines the erasure tax.
    ///
    /// Slice 1b emitted this for (i64, i64) only; Slice 2 generalizes
    /// to any (i32 / i64 key) × (i64 val) pair so `Map[char, i64]`
    /// can share the shape — char lowers to LLVM i32 (Slice 2.0).
    ///
    /// On entry the function has signature `i1 (ptr map, K key,
    /// V val, ptr out_old_val)`. On exit, every path terminates
    /// with `ret i1` (the existed bit).
    pub(super) fn emit_mono_map_insert_old_body(
        &mut self,
        f: FunctionValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let key_int_ty = key_ty.into_int_type();
        let val_int_ty = val_ty.into_int_type();
        let key_size = (key_int_ty.get_bit_width() as u64).div_ceil(8);
        let val_size = (val_int_ty.get_bit_width() as u64).div_ceil(8);
        let kv_size_bytes = key_size + val_size;

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        let val_arg = f.get_nth_param(2).unwrap().into_int_value();
        let out_old_arg = f.get_nth_param(3).unwrap().into_pointer_value();

        // Match the mangle-token used by `mono_map_cache_key` so the
        // helper name aligns with the symbol family. Both `char` (4-
        // byte) and `i32` keys hash via `karac_hash_i32` here even
        // though the erased fallback's stored function-pointer might
        // be `karac_hash_char` — both are FNV-1a over 4 bytes and
        // produce identical output for identical input, so cross-
        // path consistency holds.
        let hash_name = self.llvm_type_to_mangle_str(key_ty);
        let hash_fn = self.emit_hash_fn_for_type(&hash_name, key_ty);

        let entry_bb = self.context.append_basic_block(f, "entry");
        let slow_bb = self.context.append_basic_block(f, "slow_path");
        let fast_bb = self.context.append_basic_block(f, "fast_path");
        let probe_cond_bb = self.context.append_basic_block(f, "probe.cond");
        let probe_body_bb = self.context.append_basic_block(f, "probe.body");
        let case_empty_bb = self.context.append_basic_block(f, "case.empty");
        let case_tomb_check_bb = self.context.append_basic_block(f, "case.check_tomb");
        let case_tomb_bb = self.context.append_basic_block(f, "case.tomb");
        let case_occupied_bb = self.context.append_basic_block(f, "case.occupied");
        let match_found_bb = self.context.append_basic_block(f, "match.found");
        let exhausted_bb = self.context.append_basic_block(f, "exhausted");

        // ── entry: field loads + load-factor check ────────────────
        self.builder.position_at_end(entry_bb);
        let len_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_LEN_OFFSET, false)],
                    "len.p",
                )
                .unwrap()
        };
        let len = self
            .builder
            .build_load(i64_t, len_p, "len")
            .unwrap()
            .into_int_value();
        let tomb_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_TOMBSTONES_OFFSET, false)],
                    "tomb.p",
                )
                .unwrap()
        };
        let tombs = self
            .builder
            .build_load(i64_t, tomb_p, "tombs")
            .unwrap()
            .into_int_value();
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_CAPACITY_OFFSET, false)],
                    "cap.p",
                )
                .unwrap()
        };
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();

        // Load factor: (len + tombs + 1) * 4 > cap * 3 → resize
        let sum = self.builder.build_int_add(len, tombs, "len+tombs").unwrap();
        let sum1 = self
            .builder
            .build_int_add(sum, i64_t.const_int(1, false), "lt+1")
            .unwrap();
        let lhs = self
            .builder
            .build_int_mul(sum1, i64_t.const_int(4, false), "lhs")
            .unwrap();
        let rhs = self
            .builder
            .build_int_mul(cap, i64_t.const_int(3, false), "rhs")
            .unwrap();
        let need_resize = self
            .builder
            .build_int_compare(IntPredicate::UGT, lhs, rhs, "need_resize")
            .unwrap();
        self.builder
            .build_conditional_branch(need_resize, slow_bb, fast_bb)
            .unwrap();

        // ── slow_path: forward to erased karac_map_insert_old ─────
        self.builder.position_at_end(slow_bb);
        let slow_key_slot = self.builder.build_alloca(key_ty, "slow.key.slot").unwrap();
        let slow_val_slot = self.builder.build_alloca(val_ty, "slow.val.slot").unwrap();
        self.builder.build_store(slow_key_slot, key_arg).unwrap();
        self.builder.build_store(slow_val_slot, val_arg).unwrap();
        let slow_existed = self
            .builder
            .build_call(
                self.karac_map_insert_old_fn,
                &[
                    map_arg.into(),
                    slow_key_slot.into(),
                    slow_val_slot.into(),
                    out_old_arg.into(),
                ],
                "slow.existed",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&slow_existed)).unwrap();

        // ── fast_path: load status/kv ptrs, inline hash ───────────
        self.builder.position_at_end(fast_bb);
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_STATUS_OFFSET, false)],
                    "status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                status_pp,
                "status",
            )
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_KV_OFFSET, false)],
                    "kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(self.context.ptr_type(AddressSpace::default()), kv_pp, "kv")
            .unwrap()
            .into_pointer_value();

        // Compute hash via direct call to karac_hash_<K>. Stack-
        // alloca + store + call matches the existing erased path's
        // hash exactly (same FNV-1a basis + prime, same byte order).
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_call(hash_fn, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond: 3-PHI'd state, bound check on i ───────────
        self.builder.position_at_end(probe_cond_bb);
        let i_phi = self.builder.build_phi(i64_t, "i").unwrap();
        let ft_phi = self.builder.build_phi(i64_t, "ft").unwrap();
        let ft_set_phi = self.builder.build_phi(bool_t, "ft_set").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), fast_bb)]);
        ft_phi.add_incoming(&[(&i64_t.const_zero(), fast_bb)]);
        ft_set_phi.add_incoming(&[(&bool_t.const_zero(), fast_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let ft_val = ft_phi.as_basic_value().into_int_value();
        let ft_set_val = ft_set_phi.as_basic_value().into_int_value();
        let bound_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, i_val, cap, "bound.done")
            .unwrap();
        self.builder
            .build_conditional_branch(bound_done, exhausted_bb, probe_body_bb)
            .unwrap();

        // ── probe.body: compute slot, load status, switch ─────────
        self.builder.position_at_end(probe_body_bb);
        let sum_si = self.builder.build_int_add(start, i_val, "sum.si").unwrap();
        let slot = self.builder.build_and(sum_si, mask, "slot").unwrap();
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[slot], "status.slot.p")
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "status.byte")
            .unwrap()
            .into_int_value();
        let is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_EMPTY, false),
                "is.empty",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, case_empty_bb, case_tomb_check_bb)
            .unwrap();

        // ── case.check_tomb: branch tomb vs occupied ──────────────
        self.builder.position_at_end(case_tomb_check_bb);
        let is_tomb = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_TOMBSTONE, false),
                "is.tomb",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_tomb, case_tomb_bb, case_occupied_bb)
            .unwrap();

        // ── case.empty: write fresh entry, possibly at earlier tomb
        self.builder.position_at_end(case_empty_bb);
        let target_slot = self
            .builder
            .build_select(ft_set_val, ft_val, slot, "target.slot")
            .unwrap()
            .into_int_value();
        let kv_size = i64_t.const_int(kv_size_bytes, false);
        let target_off = self
            .builder
            .build_int_mul(target_slot, kv_size, "target.off")
            .unwrap();
        let target_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[target_off], "target.kv.p")
                .unwrap()
        };
        self.builder.build_store(target_kv_p, key_arg).unwrap();
        let target_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    target_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "target.val.p",
                )
                .unwrap()
        };
        self.builder.build_store(target_val_p, val_arg).unwrap();
        let target_status_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[target_slot], "target.status.p")
                .unwrap()
        };
        self.builder
            .build_store(
                target_status_p,
                i8_t.const_int(Self::BUCKET_OCCUPIED, false),
            )
            .unwrap();
        // len += 1
        let new_len = self
            .builder
            .build_int_add(len, i64_t.const_int(1, false), "len.new")
            .unwrap();
        self.builder.build_store(len_p, new_len).unwrap();
        // if ft_set, tombs -= 1
        let tombs_dec = self
            .builder
            .build_int_sub(tombs, i64_t.const_int(1, false), "tombs.dec")
            .unwrap();
        let new_tombs = self
            .builder
            .build_select(ft_set_val, tombs_dec, tombs, "tombs.new")
            .unwrap()
            .into_int_value();
        self.builder.build_store(tomb_p, new_tombs).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();

        // ── case.tomb: remember first tomb, continue probing ─────
        self.builder.position_at_end(case_tomb_bb);
        let new_ft = self
            .builder
            .build_select(ft_set_val, ft_val, slot, "ft.new")
            .unwrap()
            .into_int_value();
        let tomb_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.tomb")
            .unwrap();
        i_phi.add_incoming(&[(&tomb_i_next, case_tomb_bb)]);
        ft_phi.add_incoming(&[(&new_ft, case_tomb_bb)]);
        ft_set_phi.add_incoming(&[(&bool_t.const_int(1, false), case_tomb_bb)]);
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── case.occupied: eq-check, found vs continue ───────────
        self.builder.position_at_end(case_occupied_bb);
        let slot_off = self
            .builder
            .build_int_mul(slot, kv_size, "slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "slot.kv.p")
                .unwrap()
        };
        let slot_key = self
            .builder
            .build_load(key_int_ty, slot_kv_p, "slot.key")
            .unwrap()
            .into_int_value();
        let key_match = self
            .builder
            .build_int_compare(IntPredicate::EQ, slot_key, key_arg, "key.match")
            .unwrap();
        let occ_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.occ")
            .unwrap();
        // Pre-build the no-match phi inputs.
        i_phi.add_incoming(&[(&occ_i_next, case_occupied_bb)]);
        ft_phi.add_incoming(&[(&ft_val, case_occupied_bb)]);
        ft_set_phi.add_incoming(&[(&ft_set_val, case_occupied_bb)]);
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: copy old val out, write new val ─────────
        self.builder.position_at_end(match_found_bb);
        let slot_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    slot_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "slot.val.p",
                )
                .unwrap()
        };
        let old_val = self
            .builder
            .build_load(val_int_ty, slot_val_p, "old.val")
            .unwrap()
            .into_int_value();
        self.builder.build_store(out_old_arg, old_val).unwrap();
        self.builder.build_store(slot_val_p, val_arg).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── exhausted: unreachable under correct resize policy,
        //               fall back to erased extern for safety ──────
        self.builder.position_at_end(exhausted_bb);
        let safe_key_slot = self.builder.build_alloca(key_ty, "safe.key.slot").unwrap();
        let safe_val_slot = self.builder.build_alloca(val_ty, "safe.val.slot").unwrap();
        self.builder.build_store(safe_key_slot, key_arg).unwrap();
        self.builder.build_store(safe_val_slot, val_arg).unwrap();
        let safe_existed = self
            .builder
            .build_call(
                self.karac_map_insert_old_fn,
                &[
                    map_arg.into(),
                    safe_key_slot.into(),
                    safe_val_slot.into(),
                    out_old_arg.into(),
                ],
                "safe.existed",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        self.builder.build_return(Some(&safe_existed)).unwrap();
    }

    /// Emit the fast-path-inlined body of the monomorphized
    /// `karac_map_<K>_<V>_get` function. Mirrors `KaracMap::lookup` and
    /// `KaracMap::get` from `runtime/src/map.rs:120` — but inlines hash,
    /// probe, K-typed eq, and the val load on match. No load-factor /
    /// resize branch (get never resizes); no tombstone-tracking PHI
    /// (get doesn't write).
    ///
    /// Slice 1b emitted this for (i64, i64) only; Slice 2 generalizes
    /// to any (i32 / i64 key) × (i64 val) pair so `Map[char, i64]`
    /// shares the shape.
    ///
    /// On entry the function has signature `i1 (ptr map, K key,
    /// ptr out_val)`. Returns true and writes the value through
    /// `out_val` on match; returns false otherwise, leaving
    /// `out_val` untouched.
    pub(super) fn emit_mono_map_get_body(
        &mut self,
        f: FunctionValue<'ctx>,
        key_ty: BasicTypeEnum<'ctx>,
        val_ty: BasicTypeEnum<'ctx>,
    ) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let key_int_ty = key_ty.into_int_type();
        let val_int_ty = val_ty.into_int_type();
        let key_size = (key_int_ty.get_bit_width() as u64).div_ceil(8);
        let val_size = (val_int_ty.get_bit_width() as u64).div_ceil(8);
        let kv_size_bytes = key_size + val_size;

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        let out_val_arg = f.get_nth_param(2).unwrap().into_pointer_value();

        let hash_name = self.llvm_type_to_mangle_str(key_ty);
        let hash_fn = self.emit_hash_fn_for_type(&hash_name, key_ty);

        let entry_bb = self.context.append_basic_block(f, "entry");
        let probe_cond_bb = self.context.append_basic_block(f, "probe.cond");
        let probe_body_bb = self.context.append_basic_block(f, "probe.body");
        let check_occupied_bb = self.context.append_basic_block(f, "check.occupied");
        let eq_check_bb = self.context.append_basic_block(f, "eq.check");
        let match_found_bb = self.context.append_basic_block(f, "match.found");
        let not_found_bb = self.context.append_basic_block(f, "not.found");

        // ── entry: load cap / status / kv, compute hash and start ─
        self.builder.position_at_end(entry_bb);
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_CAPACITY_OFFSET, false)],
                    "cap.p",
                )
                .unwrap()
        };
        let cap = self
            .builder
            .build_load(i64_t, cap_p, "cap")
            .unwrap()
            .into_int_value();
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_STATUS_OFFSET, false)],
                    "status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(
                self.context.ptr_type(AddressSpace::default()),
                status_pp,
                "status",
            )
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_KV_OFFSET, false)],
                    "kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(self.context.ptr_type(AddressSpace::default()), kv_pp, "kv")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_call(hash_fn, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond: PHI for i; bound-check vs cap ─────────────
        self.builder.position_at_end(probe_cond_bb);
        let i_phi = self.builder.build_phi(i64_t, "i").unwrap();
        i_phi.add_incoming(&[(&i64_t.const_zero(), entry_bb)]);
        let i_val = i_phi.as_basic_value().into_int_value();
        let bound_done = self
            .builder
            .build_int_compare(IntPredicate::UGE, i_val, cap, "bound.done")
            .unwrap();
        self.builder
            .build_conditional_branch(bound_done, not_found_bb, probe_body_bb)
            .unwrap();

        // ── probe.body: load status, branch on empty ──────────────
        self.builder.position_at_end(probe_body_bb);
        let sum_si = self.builder.build_int_add(start, i_val, "sum.si").unwrap();
        let slot = self.builder.build_and(sum_si, mask, "slot").unwrap();
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[slot], "status.slot.p")
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "status.byte")
            .unwrap()
            .into_int_value();
        let is_empty = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_EMPTY, false),
                "is.empty",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_empty, not_found_bb, check_occupied_bb)
            .unwrap();

        // ── check.occupied: tombstone → continue, occupied → eq ──
        self.builder.position_at_end(check_occupied_bb);
        let is_occupied = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(Self::BUCKET_OCCUPIED, false),
                "is.occupied",
            )
            .unwrap();
        let tomb_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.tomb")
            .unwrap();
        // Tombstone path: advance i, branch to probe.cond.
        i_phi.add_incoming(&[(&tomb_i_next, check_occupied_bb)]);
        self.builder
            .build_conditional_branch(is_occupied, eq_check_bb, probe_cond_bb)
            .unwrap();

        // ── eq.check: inline icmp eq on K key ────────────────────
        self.builder.position_at_end(eq_check_bb);
        let kv_size = i64_t.const_int(kv_size_bytes, false);
        let slot_off = self
            .builder
            .build_int_mul(slot, kv_size, "slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "slot.kv.p")
                .unwrap()
        };
        let slot_key = self
            .builder
            .build_load(key_int_ty, slot_kv_p, "slot.key")
            .unwrap()
            .into_int_value();
        let key_match = self
            .builder
            .build_int_compare(IntPredicate::EQ, slot_key, key_arg, "key.match")
            .unwrap();
        let nomatch_i_next = self
            .builder
            .build_int_add(i_val, i64_t.const_int(1, false), "i.next.nomatch")
            .unwrap();
        i_phi.add_incoming(&[(&nomatch_i_next, eq_check_bb)]);
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: load val, write out, return true ────────
        self.builder.position_at_end(match_found_bb);
        let slot_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    slot_kv_p,
                    &[i64_t.const_int(key_size, false)],
                    "slot.val.p",
                )
                .unwrap()
        };
        let val = self
            .builder
            .build_load(val_int_ty, slot_val_p, "val")
            .unwrap()
            .into_int_value();
        self.builder.build_store(out_val_arg, val).unwrap();
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── not.found: return false, out_val untouched ───────────
        self.builder.position_at_end(not_found_bb);
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();
    }
}
