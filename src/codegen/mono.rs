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

use inkwell::basic_block::BasicBlock;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue};
use inkwell::AddressSpace;
use inkwell::IntPredicate;

use super::helpers::{
    const_value_from_literal_expr, const_value_to_mangle_str, vec_inner_type_expr,
};
use super::state::{LayoutId, MapMonoMethods, VarSlot};

/// Which LOOKUP probe loop is asking whether to fold the 7-bit hash tag into
/// its occupancy test — see [`Codegen::map_tag_compare`], which is the whole
/// documentation for why the answer differs. The distinction that matters is
/// what the tag lets the probe SKIP: an L1-resident scalar compare for a
/// primitive key, versus a `{ptr,len,cap}` load and a cold heap dereference
/// for a String one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum MapProbeKey {
    /// `emit_mono_map_get_body` / `emit_mono_set_contains_body` — i32/i64 keys
    /// by construction (`should_use_mono_map_for` / `should_use_mono_set_for`).
    Primitive,
    /// `emit_mono_map_str_get_body` — the monomorphized `String`-keyed get.
    /// NOTE this is a SECOND mono path beside the primitive one, not the
    /// erased runtime path: `should_use_mono_map_for` gates only the former,
    /// so "a String key can never reach mono.rs" is false.
    HeapString,
}

/// How a mono LOOKUP probe carries its cursor around the bucket array
/// (B-2026-08-07-16). All three forms visit the same buckets in the same
/// order; they differ only in what the loop keeps live in a register.
///
/// The two unbounded forms rest on an invariant the table maintains rather
/// than on a per-iteration test: every insert path — the runtime's
/// `karac_map_*` and the mono `fast_path` alike — evaluates
/// `(len + tombstones + 1) * 4 > capacity * 3` BEFORE claiming a bucket and
/// grows (or delegates to the runtime, which grows) when it holds. So after
/// any insert `len + tombstones <= 3/4 * capacity`, at least a quarter of the
/// buckets are EMPTY, and `capacity` is a power of two no smaller than
/// `INITIAL_CAPACITY = 16` allocated eagerly in `KaracMap::new`. A linear
/// probe therefore always reaches an EMPTY bucket and exits through
/// `not.found` — the `i >= cap` test can only fire on a table that violates
/// the invariant. Deletion preserves it (a delete trades one live entry for
/// one tombstone, leaving the sum unchanged).
///
/// THE TRADE that keeps `Bounded` the default is robustness, not speed: the
/// bound test is also what makes a CORRUPT or externally-built table
/// terminate. Unbounded, such a table spins forever instead.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapLookupProbe {
    /// `i` counts iterations and `i >= cap` ends the probe; the slot is
    /// `(start + i) & mask`. Keeps `i`, `start`, `mask` AND `cap` live.
    Bounded,
    /// As `Bounded` without the bound test — `cap` dies at the point `mask` is
    /// computed, and two instructions leave the loop body.
    Unbounded,
    /// The cursor IS the slot, advanced by `(slot + 1) & mask`. `start` is
    /// only the PHI's seed and `i` does not exist, so this retires `i`,
    /// `start` and `cap` against `Bounded` — the variant with a mechanism for
    /// actually clearing the x86 register spill this bug is about, since
    /// freeing ONE register (measured) did not.
    SlotWalk,
}

/// The loop-carried cursor of a mono LOOKUP probe, returned by
/// [`Codegen::emit_lookup_probe_cursor`] and advanced by every back-edge.
pub(super) struct LookupProbeCursor<'ctx> {
    phi: inkwell::values::PhiValue<'ctx>,
    form: MapLookupProbe,
    mask: IntValue<'ctx>,
}

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
    array_elem_type_exprs: HashMap<String, TypeExpr>,
    closure_ret_vec_te: HashMap<String, TypeExpr>,
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
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Frozen(inner) => {
                    inner.as_ref()
                }
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
            if let Some(ci) = self.accel.column_typed_exprs.get(&key) {
                out.push((
                    param_name.to_string(),
                    super::state::MonoHandleArgInfo::Column(ci.clone()),
                ));
            } else if let Some(ti) = self.accel.tensor_typed_exprs.get(&key) {
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
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) | TypeKind::Frozen(inner) => {
                    inner.as_ref()
                }
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
            if self.accel.column_typed_exprs.contains_key(&key)
                || self.accel.tensor_typed_exprs.contains_key(&key)
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
    /// The SCALAR twin of [`Self::augment_subst_from_arg_elem_types`] below:
    /// bind a bare generic type param to the concrete type NAME the enclosing
    /// monomorph already knows for the argument (B-2026-08-31-11).
    ///
    /// A nested generic call whose callee's type param has the SAME NAME as the
    /// caller's — `fn wrap[T](x: T) { show(x) }` calling `fn show[T](x: T)` —
    /// resolves `show`'s `T` to the caller's `T`, and the typechecker drops
    /// that as a self-referential binding (`infer_call`'s
    /// `if !matches!(&resolved, Type::TypeParam(n) if n == name)`, kept so the
    /// interpreter's substitution stack never sees a `T -> T` entry shadowing
    /// the outer frame that does know `T`). So `call_type_subs` has no entry
    /// here and `subst_names` stays empty, leaving the mangle to fall back on
    /// `llvm_type_to_mangle_str` — which carries a WIDTH but no SIGNEDNESS.
    ///
    /// Measured: `show`'s `u32` instantiation reused `show$i32`, and
    /// `u32::MAX` printed as `-1` on BOTH compiled backends at every unsigned
    /// width (u8/u16/u32/u64/u128), transitively at any depth, while `--interp`
    /// and the DIRECT `show(x)` at the same width were correct. Renaming the
    /// callee's param to `U` made the identical program correct — which is what
    /// localizes this to the NAME COLLISION rather than to nesting — and the
    /// arithmetic half went the same way (`u64::MAX / 2` gave 0, `u64::MAX < 2`
    /// gave true).
    ///
    /// The enclosing mono knows each argument's concrete name already:
    /// `compile_mono_function`'s param prologue records it in `var_type_names`,
    /// resolved through `type_subst_names`. Read it back here.
    ///
    /// Only fills a GAP — a param the typechecker DID record keeps its binding,
    /// so nothing that works today changes. Restricted to the scalar-primitive
    /// names because those are exactly what the LLVM token erases; a
    /// struct/enum type argument is disambiguated by the `token == "struct"`
    /// branch of the mangle and is not part of the measured defect.
    fn augment_subst_names_from_arg_type_names(
        &self,
        func: &Function,
        args: &[CallArg],
        subst_names: &mut HashMap<String, String>,
        subst: &mut HashMap<String, BasicTypeEnum<'ctx>>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            // A BARE type param only: `Vec[T]` and friends are the container
            // twin's business, and a concrete `x: i64` needs no binding.
            if path.segments.len() != 1 || path.generic_args.is_some() {
                continue;
            }
            let pname = &path.segments[0];
            if !is_param(pname) || subst_names.contains_key(pname) {
                continue;
            }
            // First choice: the ARGUMENT's own registered concrete name. This is
            // direct evidence and cannot be wrong. Covers every binding-rooted
            // argument, since a mono registers its params AND its locals here —
            // measured: a plain identifier, a `let`-bound call result and a
            // `let`-bound arithmetic result all resolve through this arm.
            let from_arg = match &arg.value.kind {
                ExprKind::Identifier(arg_name) => self
                    .var_types
                    .var_type_names
                    .get(arg_name.as_str())
                    .cloned(),
                _ => None,
            };
            // Fallback for an INLINE expression argument (`show(add1(x, o))`,
            // `show(x + o)`), which names no binding and so has no entry above.
            // The binding was dropped for exactly one reason — the callee's
            // param resolved to the CALLER's param OF THE SAME NAME — so the
            // enclosing mono's own binding for that name IS the answer.
            //
            // Guarded by the scalar shape of `subst`, which `infer_type_args`
            // already filled from the argument's LLVM type. The other way a
            // binding can be absent is a solution `type_to_concrete_or_param_-
            // name` cannot spell (a tuple, a function type); those lower to an
            // aggregate, so requiring an int/float here excludes them. Without
            // the guard, `show(mkPair(x))` would bind the callee's `T` to the
            // enclosing `u64` and mangle a tuple instantiation as `$u64`,
            // colliding with a real `u64` one in the same program.
            let from_enclosing = || {
                let scalar_slot = match subst.get(pname) {
                    None => true,
                    Some(t) => t.is_int_type() || t.is_float_type(),
                };
                if !scalar_slot {
                    return None;
                }
                self.mono_state.type_subst_names.get(pname).cloned()
            };
            let Some(concrete) = from_arg.or_else(from_enclosing) else {
                continue;
            };
            if !Self::is_scalar_primitive_mangle_name(&concrete) {
                continue;
            }
            let llvm = self.llvm_type_for_name(&concrete);
            subst_names.insert(pname.clone(), concrete);
            subst.entry(pname.clone()).or_insert(llvm);
        }
    }

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
            // B-2026-07-29-32: the argument is no longer required to be a plain
            // identifier. A `Slice[T]` param can now receive a FRESH Vec rvalue
            // (`f(vs.clone())`, `f(make())`), which reaches codegen only because
            // -32 taught `coerce_to_slice` to coerce it; the arms that genuinely
            // need a variable NAME to index a side table still demand one below.
            // Leaving the blanket gate here bound `T`'s NAME (via
            // `resolve_container_param_elem_substs`) without binding its LLVM
            // TYPE, so the mono returned `i64` while its body cloned a String —
            // `Function return type does not match operand type of return inst`.
            let arg_ident: Option<&str> = match &arg.value.kind {
                ExprKind::Identifier(n) => Some(n.as_str()),
                _ => None,
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
                    // B-2026-08-27-50: the name-keyed lookup misses a
                    // STRUCT-FIELD argument, which has no binding to index by.
                    // The fallback fires only when the identifier arm found
                    // nothing, so an identifier argument keeps its exact
                    // previous binding.
                    if let (Some(pn), Some(elem)) = (
                        param_at(0),
                        arg_ident
                            .and_then(|n| self.var_types.vec_elem_types.get(n))
                            .copied()
                            .or_else(|| self.field_arg_container_elem_llvm(&arg.value)),
                    ) {
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
                    // B-2026-07-29-32 adds the fresh-rvalue fallback. The
                    // shared `infer_elem_from_source` stays identifier-only
                    // deliberately — it has three other callers whose behaviour
                    // should not shift for this fix.
                    // B-2026-08-27-50 adds the struct-field fallback, on the
                    // same footing as the fresh-rvalue one beside it.
                    if let (Some(pn), Some(elem)) = (
                        param_at(0),
                        self.infer_elem_from_source(&arg.value)
                            .or_else(|| self.fresh_container_arg_elem_llvm(&arg.value))
                            .or_else(|| self.field_arg_container_elem_llvm(&arg.value)),
                    ) {
                        subst.entry(pn).or_insert(elem);
                    }
                }
                "Set" => {
                    if let (Some(pn), Some(&elem)) = (
                        param_at(0),
                        arg_ident.and_then(|n| self.mapset.set_elem_types.get(n)),
                    ) {
                        subst.entry(pn).or_insert(elem);
                    }
                }
                "Map" => {
                    if let (Some(kn), Some(&kty)) = (
                        param_at(0),
                        arg_ident.and_then(|n| self.mapset.map_key_types.get(n)),
                    ) {
                        subst.entry(kn).or_insert(kty);
                    }
                    if let (Some(vn), Some(&vty)) = (
                        param_at(1),
                        arg_ident.and_then(|n| self.mapset.map_val_types.get(n)),
                    ) {
                        subst.entry(vn).or_insert(vty);
                    }
                }
                _ => {}
            }
        }
    }

    /// A bare-`T` argument's concrete collection identity in the CURRENT
    /// (caller / enclosing-mono) scope: `("String", None)` for a String var,
    /// `("Vec"|"VecDeque", Some(elem_te))` for a Vec/VecDeque var. Read from the
    /// caller's live var side-tables (`string_vars` / `var_elem_type_exprs` /
    /// `var_type_names`). `None` for a scalar / struct / handle arg. MUST be
    /// called before the mono's `take_var_side_tables` clears these maps.
    fn arg_collection_head_elem(&self, arg_name: &str) -> Option<(String, Option<TypeExpr>)> {
        if self.var_types.string_vars.contains(arg_name) {
            return Some(("String".to_string(), None));
        }
        // B-2026-08-13-9 — a MAP / SET / SLICE argument, before the Vec arm.
        //
        // That arm reads `var_elem_type_exprs` and, finding an entry, calls the
        // container a `Vec` — defaulting the head to "Vec" when the recorded
        // name is anything else. But that table doubles as the MAP's value slot
        // and carries a `Set`'s element too, so a `Map[String, i64]` argument
        // bound to a bare `T` param resolved to the type-expr `Vec[i64]`: the
        // monomorph then registered its receiver as a Vec and every method on it
        // went to the Vec/String path ("Vec/String method 'describe' is not yet
        // supported in codegen" for a user trait impl). The head name survived
        // only because the typechecker had already filled it and this resolver
        // uses `or_insert`, which is why the two subst tables disagreed —
        // `{"T": "Map"}` beside a `Vec[i64]` type-expr.
        //
        // Each of these returns its OWN head with its own element-aware
        // type-expr, so the registration matches the container the caller
        // actually passed. `Map` carries two args and cannot be expressed as a
        // one-element head, so this returns the FULL container type-expr and the
        // caller inserts it verbatim.
        if let Some(head) = self
            .var_types
            .var_type_names
            .get(arg_name)
            .filter(|h| matches!(h.as_str(), "Map" | "SortedMap"))
        {
            let (Some(k), Some(v)) = (
                self.mapset.map_key_type_exprs.get(arg_name),
                self.var_types.var_elem_type_exprs.get(arg_name),
            ) else {
                return None;
            };
            return Some((
                head.clone(),
                Some(Self::path_type_expr(head, &[k.clone(), v.clone()])),
            ));
        }
        if let Some(head) = self
            .var_types
            .var_type_names
            .get(arg_name)
            .filter(|h| matches!(h.as_str(), "Set" | "SortedSet"))
        {
            let elem = self
                .mapset
                .set_elem_type_exprs
                .get(arg_name)
                .or_else(|| self.var_types.var_elem_type_exprs.get(arg_name))?;
            return Some((
                head.clone(),
                Some(Self::path_type_expr(head, std::slice::from_ref(elem))),
            ));
        }
        if self.var_types.slice_elem_types.contains_key(arg_name) {
            // The element type-expr is recorded only when the binding was
            // registered FROM a declared `Slice[E]`. An INFERRED slice binding
            // (`let sl = v[0..2]`) has none, and records the ELEMENT's name in
            // `var_type_names` instead — the same convention that makes
            // `inferred_receiver_type` need its own slice fallback. Rebuild the
            // element from that name when the type-expr is absent, so both
            // spellings of a slice binding resolve identically.
            let elem = self
                .var_types
                .var_elem_type_exprs
                .get(arg_name)
                .cloned()
                .or_else(|| {
                    self.var_types
                        .var_type_names
                        .get(arg_name)
                        .map(|n| Self::path_type_expr(n, &[]))
                })?;
            return Some((
                "Slice".to_string(),
                Some(Self::path_type_expr("Slice", std::slice::from_ref(&elem))),
            ));
        }
        if let Some(elem) = self.var_types.var_elem_type_exprs.get(arg_name) {
            let head = self
                .var_types
                .var_type_names
                .get(arg_name)
                .filter(|h| h.as_str() == "Vec" || h.as_str() == "VecDeque")
                .cloned()
                .unwrap_or_else(|| "Vec".to_string());
            return Some((
                head.clone(),
                Some(Self::path_type_expr(&head, std::slice::from_ref(elem))),
            ));
        }
        None
    }

    /// B-2026-08-13-9 — build `Head[A, B, …]` as a `TypeExpr`, so a resolver can
    /// hand back a whole container type rather than an element the caller has to
    /// re-wrap (which is what limited the shape above to one generic arg).
    fn path_type_expr(head: &str, args: &[TypeExpr]) -> TypeExpr {
        let span = args.first().map(|a| a.span).unwrap_or(crate::token::Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        });
        TypeExpr {
            kind: TypeKind::Path(PathExpr {
                segments: vec![head.to_string()],
                // No args means a BARE path (`i64`), not `i64[]` — an empty
                // `generic_args` list is a different type expression and every
                // consumer that peels one arg would see a malformed head.
                generic_args: if args.is_empty() {
                    None
                } else {
                    Some(args.iter().cloned().map(GenericArg::Type).collect())
                },
                span,
            }),
            span,
        }
    }

    /// Resolve each bare generic type param `x: T` bound WHOLE to a builtin
    /// collection ARGUMENT (String / Vec / VecDeque identifier) to its concrete
    /// identity, filling BOTH the head-name `subst_names` (B-2026-07-13-2 leg A —
    /// the nested-generic-call case the typechecker drops as a self-referential
    /// `T -> T` binding, leaving `subst_names` empty so the mangle stayed the
    /// element-erased `$struct` and the body registered no element) AND the
    /// element-aware `type_subst_type_exprs` (leg B — `Vec`/`VecDeque` element
    /// that the head-only name loses; `String` carries none). Only INSERTS a
    /// `subst_names` entry when absent, so a concrete typechecker binding is
    /// never overwritten; always records the Vec/VecDeque type-expr (the direct
    /// call needs the element too). Reads the caller's live var side-tables via
    /// `arg_collection_head_elem`, so it MUST run before `take_var_side_tables`.
    /// Composes through nesting once the outer mono registers its own collection
    /// param element-aware. Identifier collection args only.
    fn resolve_collection_param_substs(
        &self,
        func: &Function,
        args: &[CallArg],
        subst_names: &mut HashMap<String, String>,
    ) -> HashMap<String, TypeExpr> {
        let mut out: HashMap<String, TypeExpr> = HashMap::new();
        let Some(gp) = &func.generic_params else {
            return out;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            // A BARE generic type param: single segment, no generic args of its
            // own, declared in `func`'s generic list. A container param like
            // `Vec[T]` is NOT this — `augment_subst_from_arg_elem_types` binds
            // ITS element instead.
            if path.segments.len() != 1 || path.generic_args.is_some() {
                continue;
            }
            let pname = &path.segments[0];
            if !is_param(pname) {
                continue;
            }
            // A RANGE INDEX (`show(v[0..2])`) is a slice VALUE with no binding
            // to look up, so the identifier path below cannot see it — it was
            // the one spelling of a slice argument still dead after the binding
            // form worked (B-2026-08-13-9). Resolve it from the container it
            // slices: the head is `Slice`, the element is the container's own.
            let resolved = match &arg.value.kind {
                ExprKind::Identifier(arg_name) => self.arg_collection_head_elem(arg_name.as_str()),
                ExprKind::Index { object, index }
                    if matches!(&index.kind, ExprKind::Range { .. }) =>
                {
                    match &object.kind {
                        ExprKind::Identifier(base) => self
                            .var_types
                            .var_elem_type_exprs
                            .get(base.as_str())
                            .map(|elem| Self::path_type_expr("Slice", std::slice::from_ref(elem)))
                            .map(|te| ("Slice".to_string(), Some(te))),
                        _ => None,
                    }
                }
                _ => None,
            };
            let Some((head, elem)) = resolved else {
                continue;
            };
            // Fill the head name only if the typechecker didn't (leg A).
            subst_names.entry(pname.clone()).or_insert(head.clone());
            // Record the element-aware full type (leg B). `String` carries
            // none; every other head hands back its whole container type-expr
            // (B-2026-08-13-9), so a two-parameter `Map[K, V]` survives the
            // round trip that a head-plus-one-element shape could not express.
            if let Some(container_te) = elem {
                out.insert(pname.clone(), container_te);
            }
        }
        out
    }

    /// B-2026-07-18-45: bind a generic type param nested inside a user
    /// generic-STRUCT param (`get[T](b: Box[T])`) to its concrete element-aware
    /// `TypeExpr` when the arg is bound to a whole collection (`b: Box[Vec[i64]]`).
    /// `infer_type_args` can't recover the element (`Box[Vec[i64]]` and
    /// `Box[String]` share the erased `{ {ptr,len,cap} }` LLVM shape), and
    /// `resolve_collection_param_substs` only handles a param that IS a bare `T`.
    /// Unify the DECLARED struct arg (`Box[T]`) against the arg identifier's
    /// recorded concrete instantiation (`enum_inst_var_types["b"] = Box[Vec[i64]]`,
    /// set at the let/param binding), and for each position where the declared
    /// arg is a bare type param of `func` and the concrete arg is a heap
    /// collection (`Vec`/`VecDeque`/`String`), record the element-aware
    /// B-2026-07-29-35: bind a CONTAINER param's element type param by NAME and
    /// `TypeExpr` — `fn f[T](s: Slice[T])` called with a `Vec[String]`.
    ///
    /// `augment_subst_from_arg_elem_types` already binds the LLVM *type* for this
    /// exact shape (B-2026-07-03-22), which is why the mono's signature and
    /// return type are correct. The NAME maps were left unbound, so every
    /// consumer that reasons about `T` symbolically fell back to the literal
    /// string `"T"`. The visible cost was the per-type clone helper: `s[0]`
    /// emitted `call @karac_clone_T` — a clone fn for a type that does not exist
    /// — instead of `@karac_clone_String`, so a heap element was never
    /// deep-cloned and the returned value shallow-aliased the container's
    /// buffer. An inline `f"{first(vs)}"` therefore printed EMPTY under
    /// `karac build` while the interpreter was correct, with no diagnostic.
    ///
    /// A `Vec` of string LITERALS masked it for a long time: those elements are
    /// static globals with `cap 0`, so the container's drop frees nothing and the
    /// shallow alias stays readable by luck. Only genuinely heap-OWNED elements
    /// (`push("a" + "b")`, a `.clone()`) expose it.
    ///
    /// `or_insert` throughout, so a binding the typechecker or an earlier
    /// resolver already recorded is never overwritten. Reads live var
    /// side-tables, so it MUST run before `take_var_side_tables`.
    fn resolve_container_param_elem_substs(
        &self,
        func: &Function,
        args: &[CallArg],
        subst_names: &mut HashMap<String, String>,
        subst_type_exprs: &mut HashMap<String, TypeExpr>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(path) = &peeled.kind else {
                continue;
            };
            if !matches!(
                path.segments.last().map(|s| s.as_str()),
                Some("Slice") | Some("Vec") | Some("VecDeque")
            ) {
                continue;
            }
            // The element must be written as a BARE type param of `func` —
            // `Slice[T]`, not `Slice[String]` (nothing to bind) and not
            // `Slice[Vec[T]]` (the nested case is not this resolver's business).
            let Some(gargs) = path.generic_args.as_ref() else {
                continue;
            };
            let Some(GenericArg::Type(te)) = gargs.first() else {
                continue;
            };
            let TypeKind::Path(ep) = &te.kind else {
                continue;
            };
            if !(ep.segments.len() == 1 && ep.generic_args.is_none() && is_param(&ep.segments[0])) {
                continue;
            }
            let pname = &ep.segments[0];
            let Some(elem_te) = self.arg_container_elem_type_expr(&arg.value) else {
                continue;
            };
            let TypeKind::Path(elem_path) = &elem_te.kind else {
                continue;
            };
            let Some(head) = elem_path.segments.last().cloned() else {
                continue;
            };
            subst_names.entry(pname.clone()).or_insert(head);
            subst_type_exprs.entry(pname.clone()).or_insert(elem_te);
        }
    }

    /// The element LLVM type of a FRESH container rvalue argument — the
    /// type-side twin of [`Self::arg_container_elem_type_expr`], for the `Slice`
    /// arm of `augment_subst_from_arg_elem_types` (B-2026-07-29-32).
    ///
    /// Kept separate from the shared `infer_elem_from_source` on purpose: that
    /// helper has three other callers, and widening it would shift their
    /// behaviour for a fix that only needs this one call site. Covers the same
    /// two shapes `arg_container_elem_type_expr` does — `<vec>.clone()` and a
    /// named `Vec`-returning call — so the NAME and TYPE bindings for `T` can
    /// never disagree. They must move together: binding one without the other
    /// produced either `Function return type does not match operand type of
    /// return inst` (name bound, type still defaulted to `i64`) or a raw pointer
    /// printed as an integer (type bound, name still the literal `"T"`).
    fn fresh_container_arg_elem_llvm(&self, arg: &Expr) -> Option<BasicTypeEnum<'ctx>> {
        // A plain identifier is already served by `infer_elem_from_source`,
        // which is consulted first; this is only the fresh-rvalue fallback.
        //
        // A struct FIELD is excluded for a different reason: it is a PLACE, not
        // a fresh rvalue, and answering for one here would quietly widen what
        // this helper claims. B-2026-08-27-50 gave `arg_container_elem_type_expr`
        // a `FieldAccess` arm, and without this guard that arm would leak
        // through and make a borrowed field report as a fresh temporary. Today
        // the single caller reads only the TYPE, so the leak would be harmless;
        // it is excluded anyway, because the next caller to ask this helper the
        // OWNERSHIP question it is named for would get a wrong answer, and the
        // wrong answer's direction is a caller-side free of a buffer the struct
        // still owns. `field_arg_container_elem_llvm`, one link further down the
        // same `or_else` chain, answers for a field deliberately.
        if matches!(
            &arg.kind,
            ExprKind::Identifier(_) | ExprKind::FieldAccess { .. }
        ) {
            return None;
        }
        let te = self.arg_container_elem_type_expr(arg)?;
        Some(self.llvm_type_for_type_expr(&te))
    }

    /// The element `TypeExpr` of a container ARGUMENT, for
    /// `resolve_container_param_elem_substs`.
    ///
    /// Three shapes, all of which can reach a `Slice[T]` parameter:
    ///
    /// * a named local — read its registered element (the same source
    ///   `vec_index_elem_type_expr` uses);
    /// * `<vec>.clone()` — the element is the RECEIVER's element;
    /// * a call to a named `Vec`-returning fn — peel the declared return type.
    ///
    /// The last two were added with B-2026-07-29-32. Before it, a fresh Vec
    /// rvalue could not reach a `Slice[T]` param at all (codegen aborted in the
    /// LLVM verifier), so `Identifier` was the only shape that ever got here.
    /// Once -32 made those arguments compile, they arrived with `T` unbound by
    /// name and hit exactly the `karac_clone_T` path B-2026-07-29-35 had just
    /// closed for named locals — silent wrong output again. So -32 could not land
    /// without widening this.
    ///
    /// An `Array` local is covered too, via its own `array_elem_type_exprs`
    /// (B-2026-07-30-3). It needs a separate table because `Array` has no entry
    /// in `var_elem_type_exprs` — the reason this arm used to return `None` for
    /// one. The consequence was NOT that `T` stayed wholly unbound: the `Slice`
    /// arm of `augment_subst_from_arg_elem_types` binds an Array argument's
    /// element LLVM TYPE through `infer_elem_from_source` (which reads the array
    /// slot), so `T` got a type but no NAME. That is precisely the split -32
    /// established must never happen: the name drives the per-type clone helper,
    /// so the mono emitted a shared `karac_clone_T` specialized to whichever
    /// instantiation was lowered FIRST. Two monos of one generic (`first(ns)`
    /// at `i64` then `first(ss)` at `String`) then ran the String through an
    /// 8-byte i64 clone, leaving `len`/`cap` as uninitialized alloca garbage —
    /// order-dependent: reversing the calls made the i64 mono copy 24 bytes out
    /// of an 8-byte alloca instead, which happened to print correctly.
    fn arg_container_elem_type_expr(&self, arg: &Expr) -> Option<TypeExpr> {
        match &arg.kind {
            ExprKind::Identifier(name) => self
                .var_types
                .var_elem_type_exprs
                .get(name.as_str())
                .or_else(|| self.var_types.array_elem_type_exprs.get(name.as_str()))
                .cloned(),
            // `<vec>.clone()` yields a Vec with the receiver's element type.
            ExprKind::MethodCall { object, method, .. } if method == "clone" => {
                self.arg_container_elem_type_expr(object)
            }
            // A named fn returning `Vec[E]` — peel one Vec layer off the
            // DECLARED return type to get `E`.
            ExprKind::Call { callee, .. } => {
                let ExprKind::Identifier(fname) = &callee.kind else {
                    return None;
                };
                let ret = self.fn_sig.fn_return_type_exprs.get(fname.as_str())?;
                vec_inner_type_expr(ret)
            }
            // B-2026-08-27-50 — a STRUCT-FIELD container argument
            // (`swap01(mut self.xs)`, `head(b.items)`). Every arm above keys on
            // a BINDING NAME to index a var side-table, and a field access has
            // none, so all of them declined and `T` fell to the `i64`
            // unknown-name default: an 8-byte swap over a 16-byte tuple element
            // (silent wrong answer) and a segfault at a String one.
            //
            // The element is recoverable without a name.
            // `vec_index_elem_type_expr` already resolves exactly this shape for
            // an INDEX read (`self.xs[i]`) — receiver's struct type, the field's
            // declared `Vec[E]`, the receiver's recorded instantiation for a
            // generic field, then one Vec (or Array) peel. `self.xs` as an
            // ARGUMENT wants the identical answer, so this delegates rather than
            // restating the resolution and risking the two drifting apart.
            ExprKind::FieldAccess { .. } => self.vec_index_elem_type_expr(arg),
            _ => None,
        }
    }

    /// The element LLVM type of a STRUCT-FIELD container argument — the
    /// type-side twin of the `FieldAccess` arm of
    /// [`Self::arg_container_elem_type_expr`] (B-2026-08-27-50).
    ///
    /// Deliberately resolved THROUGH that helper rather than from a parallel
    /// lookup, so the NAME and the TYPE this argument binds for `T` come from
    /// one source and cannot disagree. That split is the documented failure mode
    /// of this whole family: name bound with the type still defaulted gives
    /// `Function return type does not match operand type of return inst`, and
    /// type bound with the name still the literal `"T"` gives a shared
    /// `karac_clone_T` specialized to whichever mono was lowered first.
    fn field_arg_container_elem_llvm(&self, arg: &Expr) -> Option<BasicTypeEnum<'ctx>> {
        if !matches!(arg.kind, ExprKind::FieldAccess { .. }) {
            return None;
        }
        let elem_te = self.arg_container_elem_type_expr(arg)?;
        Some(self.llvm_type_for_type_expr(&elem_te))
    }

    /// `subst_type_exprs` (+ head-name `subst_names`). Without this the mono
    /// entry-copy (`deep_copy_struct_heap_fields_in_place_mono`) sees a bare
    /// `Vec` with no element, skips the field copy, and the struct's Vec field
    /// aliases the caller's buffer — both free it (double-free). `or_insert` so a
    /// binding the typechecker already recorded is never overwritten. Reads live
    /// var side-tables, so it MUST run before `take_var_side_tables`.
    fn resolve_generic_struct_param_substs(
        &self,
        func: &Function,
        args: &[CallArg],
        subst_names: &mut HashMap<String, String>,
        subst_type_exprs: &mut HashMap<String, TypeExpr>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
        for (param, arg) in func.params.iter().zip(args.iter()) {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            let TypeKind::Path(decl_path) = &peeled.kind else {
                continue;
            };
            let struct_name = decl_path.segments.last().map(|s| s.as_str()).unwrap_or("");
            // A user generic struct with declared params AND generic args written
            // here (`Box[T]`, `Pair[A, B]`) — not a bare `T` (handled elsewhere).
            if self
                .type_decls
                .struct_generic_params
                .get(struct_name)
                .is_none_or(|p| p.is_empty())
            {
                continue;
            }
            let Some(decl_args) = decl_path.generic_args.as_ref() else {
                continue;
            };
            let ExprKind::Identifier(arg_name) = &arg.value.kind else {
                continue;
            };
            let Some(inst) = self.type_decls.enum_inst_var_types.get(arg_name.as_str()) else {
                continue;
            };
            let TypeKind::Path(inst_path) = &inst.kind else {
                continue;
            };
            if inst_path.segments.last().map(|s| s.as_str()) != Some(struct_name) {
                continue;
            }
            let Some(inst_args) = inst_path.generic_args.as_ref() else {
                continue;
            };
            for (d, c) in decl_args.iter().zip(inst_args.iter()) {
                let (GenericArg::Type(dte), GenericArg::Type(cte)) = (d, c) else {
                    continue;
                };
                let TypeKind::Path(dp) = &dte.kind else {
                    continue;
                };
                if !(dp.segments.len() == 1
                    && dp.generic_args.is_none()
                    && is_param(&dp.segments[0]))
                {
                    continue;
                }
                let pname = &dp.segments[0];
                let chead = match &cte.kind {
                    TypeKind::Path(cp) => cp.segments.last().map(|s| s.as_str()).unwrap_or(""),
                    _ => "",
                };
                if matches!(chead, "Vec" | "VecDeque" | "String") {
                    subst_names
                        .entry(pname.clone())
                        .or_insert_with(|| chead.to_string());
                    subst_type_exprs
                        .entry(pname.clone())
                        .or_insert_with(|| cte.clone());
                }
            }
        }
    }

    /// B-2026-08-27-40, free-function leg — bind a bare-`T` param to a TUPLE
    /// argument's `TypeExpr`.
    ///
    /// The explicit-args loop in `compile_generic_call` covers an IMPL METHOD,
    /// whose type args arrive from the receiver's recorded instantiation. A
    /// free generic function has no receiver: `pick[T](a: T, b: T)` binds `T`
    /// from `infer_type_args`, which sees only the LLVM shape. Every tuple
    /// lowers to the opaque `"struct"` token there, so `pick((1, 2), …)` and
    /// `pick((1.5, 2.5), …)` both mangled `pick$struct` and the second call
    /// failed module verification against the first's signature — repro B of
    /// this row, one call shape over.
    ///
    /// The argument's type comes from `seq_eq_operand_types`, the span-keyed
    /// table lowering fills from `tc.expr_types` for EVERY typed expression, so
    /// a call argument is on record exactly like a comparison operand is. Only
    /// a param SPELLED as a bare type param is considered — a container param
    /// (`v: Vec[T]`) binds its element through the resolvers above, and reading
    /// the whole-argument type here would bind `T` to the container.
    fn resolve_structural_param_substs(
        &self,
        func: &Function,
        args: &[CallArg],
        structural: &mut Vec<(String, TypeExpr)>,
    ) {
        let Some(gp) = &func.generic_params else {
            return;
        };
        let is_param = |n: &str| gp.params.iter().any(|p| !p.is_const && p.name == n);
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
            let pname = &path.segments[0];
            if !is_param(pname) {
                continue;
            }
            let Some(te) = self.tuple_te_of_operand(&arg.value) else {
                continue;
            };
            if Self::type_expr_is_structural_type_arg(&te) {
                structural.push((pname.clone(), te));
            }
        }
    }

    /// Element-aware mangle token for a generic param whose concrete binding is a
    /// builtin collection, mirroring the typechecker's `type_to_mono_mangle_token`
    /// (`String`, `Vec_i64`, `Vec_String`, `VecDeque_i64`, …). Built from the
    /// resolved `type_subst_type_exprs` (Vec/VecDeque, element-aware) or the
    /// head-only `subst_names` (String). `None` for a non-collection param. Used
    /// to disambiguate a nested-generic-call mono symbol when the typechecker
    /// recorded no per-call token (the dropped self-referential binding).
    fn collection_param_mangle_token(
        &self,
        pname: &str,
        subst_names: &HashMap<String, String>,
        subst_type_exprs: &HashMap<String, TypeExpr>,
    ) -> Option<String> {
        if let Some(te) = subst_type_exprs.get(pname) {
            return Some(Self::mono_mangle_token_for_type_expr(te));
        }
        match subst_names.get(pname).map(String::as_str) {
            Some("String") => Some("String".to_string()),
            _ => None,
        }
    }

    /// Codegen-side sibling of the typechecker's `type_to_mono_mangle_token`,
    /// over a `TypeExpr` instead of a `Type`. Recurses into generic args so
    /// `Vec[i64]` → `Vec_i64`, `Vec[String]` → `Vec_String`, `Vec[Vec[i64]]` →
    /// `Vec_Vec_i64`. A bare path segment maps to its name (`String`, `i64`).
    fn mono_mangle_token_for_type_expr(te: &TypeExpr) -> String {
        match &te.kind {
            TypeKind::Path(p) => {
                let head = p
                    .segments
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "e".to_string());
                match &p.generic_args {
                    Some(gargs) if !gargs.is_empty() => {
                        let parts: Vec<String> = gargs
                            .iter()
                            .map(|g| match g {
                                GenericArg::Type(t) => Self::mono_mangle_token_for_type_expr(t),
                                _ => "x".to_string(),
                            })
                            .collect();
                        format!("{head}_{}", parts.join("_"))
                    }
                    _ => head,
                }
            }
            TypeKind::Ref(inner) | TypeKind::MutRef(inner) => {
                Self::mono_mangle_token_for_type_expr(inner)
            }
            // B-2026-08-27-40 — a tuple has no head name, so without this arm
            // every tuple collapsed to the same `"e"` token and two tuple
            // instantiations of one generic shared a symbol. Spelled to match
            // the typechecker's `type_to_mono_mangle_token`, which has carried
            // the identical `tup_<a>_<b>` rendering all along — the two are
            // twins over `Type` and `TypeExpr` and must agree.
            TypeKind::Tuple(elems) => {
                let parts: Vec<String> = elems
                    .iter()
                    .map(Self::mono_mangle_token_for_type_expr)
                    .collect();
                format!("tup_{}", parts.join("_"))
            }
            _ => "e".to_string(),
        }
    }

    /// Is `te` a type argument the NAME channel structurally cannot carry?
    ///
    /// The substitution channel that flows a generic binding from a call site
    /// into a monomorph body is keyed by type NAME
    /// (`mono_state.type_subst_names`, built from `TypeKind::Path` segments).
    /// That is total over named types and empty over the rest, and a TUPLE is
    /// the shape where the difference is observable: `Bag[(i64, i64)]` binds
    /// `T`'s LLVM type correctly at the OUTER call (the explicit-args loop
    /// lowers any `TypeExpr`) but records no name, so a monomorph body that
    /// calls another method on `self` re-derives the receiver's instantiation
    /// through the name map, finds nothing, and emits the callee with an EMPTY
    /// substitution — `T` then falls to the `i64` unknown-name default and a
    /// 16-byte element is swapped 8 bytes at a time (B-2026-08-27-40).
    ///
    /// Deliberately narrow: `Tuple` only, not the whole non-`Path` complement.
    /// `Array` is the near neighbour and is NOT included — it reaches codegen
    /// through the const-generic size axis as well, so admitting it here would
    /// need its own measurement rather than an argument by analogy (its own
    /// gap is tracked as B-2026-08-27-42). `FnType` / `Pointer` / `Frozen` /
    /// `ImplTrait` have no monomorph-key convention at all today.
    fn type_expr_is_structural_type_arg(te: &TypeExpr) -> bool {
        matches!(te.kind, TypeKind::Tuple(_))
    }

    /// Fold nameless type arguments (see
    /// [`Self::type_expr_is_structural_type_arg`]) into the element-aware
    /// substitution map, which — unlike the name map — can express them.
    ///
    /// `subst_type_exprs` becomes `mono_state.type_subst_type_exprs` for the
    /// body, and both `concrete_generic_struct_inst` (which re-renders a
    /// generic-struct param's instantiation for the `enum_inst_var_types`
    /// record a nested method call reads) and `subst_monomorph_type_params`
    /// (which resolves a generic FIELD's type inside the body) already consult
    /// it in preference to the head-only name map. So one insert here is what
    /// carries the tuple through both, and no consumer needed widening.
    ///
    /// `or_insert` so a binding an earlier resolver recorded always wins and
    /// every pre-existing instantiation keeps the entry it had.
    fn merge_structural_type_arg_substs(
        structural: Vec<(String, TypeExpr)>,
        subst_type_exprs: &mut HashMap<String, TypeExpr>,
    ) {
        for (pname, te) in structural {
            subst_type_exprs.entry(pname).or_insert(te);
        }
    }

    /// Append the NAMELESS-type-argument axis to a mangled mono name:
    /// `$<param>_st_<token>` for a generic param bound to a tuple.
    ///
    /// `mangle_mono_name` disambiguates a type argument either by the LLVM
    /// token or, when that token is the opaque `"struct"`, by the concrete NAME
    /// from `subst_names`. A tuple lowers to `"struct"` and has no name, so
    /// both fell through and every tuple instantiation of one generic landed on
    /// the same symbol. That is not a silent collision like the collection one
    /// it sits beside — the second instantiation fails module verification
    /// (`Call parameter type does not match function signature`, repro B of
    /// B-2026-08-27-40) — but it is the same defect, and the same axis shape
    /// fixes it.
    ///
    /// TWO SOURCES, in order. The `TypeExpr` is preferred because it is exact
    /// and readable (`tup_i64_i64`); the LLVM SHAPE is the fallback for a
    /// binding that reached `subst` without ever passing through a `TypeExpr` —
    /// a tuple FORWARDED between two generic functions (`outer[U](a: U)` whose
    /// body calls `pick(a, b)`). There the argument's static type is the
    /// caller's own type PARAM, so no resolver can name it and the tuple only
    /// exists as `{i64, i64}` in the subst; `infer_type_args` gets the body
    /// right and only the SYMBOL collides, which is why the fallback needs to
    /// mangle and nothing more. The named-type control (`outer("aa", "bb")`
    /// beside `outer(1, 2)`) has always worked — `subst_names` carries `String`
    /// and `i64` through the forward — so this is the one shape the name
    /// channel cannot reach.
    ///
    /// The fallback is strictly weaker than the `TypeExpr` source and known to
    /// be: `(i64, i64)` and `(u64, u64)` lower to the same `{i64, i64}` and
    /// still share a symbol under it. That is the same erasure
    /// `llvm_type_to_mangle_str` has everywhere else, it is only reachable on
    /// the forwarding path (a direct call takes the exact `TypeExpr` source),
    /// and it is a strict improvement on the collapse it replaces, where EVERY
    /// tuple shared one symbol.
    ///
    /// Gated so no pre-existing symbol changes: the `TypeExpr` source requires
    /// a structural entry, and all three resolvers that write that map require
    /// a `TypeKind::Path` on the value they record, so none of them can produce
    /// a tuple; the shape source additionally requires NO `subst_names` entry,
    /// and every named type has one.
    fn append_structural_type_param_mangle(
        &self,
        mut mangled: String,
        func: &Function,
        subst: &HashMap<String, BasicTypeEnum<'ctx>>,
        subst_names: &HashMap<String, String>,
        subst_type_exprs: &HashMap<String, TypeExpr>,
    ) -> String {
        use std::fmt::Write as _;
        let Some(gp) = &func.generic_params else {
            return mangled;
        };
        for param in &gp.params {
            if param.is_const {
                continue;
            }
            let token = match subst_type_exprs.get(&param.name) {
                Some(te) if Self::type_expr_is_structural_type_arg(te) => {
                    Self::mono_mangle_token_for_type_expr(te)
                }
                // Fallback: a STRUCT-shaped binding whose `subst_names` entry
                // does not name a real type. "Absent" is not the right test and
                // measuring it is what showed why: on the forwarding path the
                // name channel records the CALLER's own type-param spelling
                // (`T -> "U"`), because flattening `U` through an empty
                // `type_subst_names` leaves it as itself. That is a placeholder,
                // not a type — `mangle_mono_name` ignores it and emits the bare
                // `$struct` token — so treating it as a name kept the fallback
                // from firing on the one shape it exists for. Requiring the name
                // to RESOLVE is what keeps user structs, enums, builtin
                // containers and scalars on the paths that already disambiguate
                // them while letting a placeholder fall through.
                _ if subst_names
                    .get(&param.name)
                    .is_none_or(|n| !self.mangle_name_is_resolved(n)) =>
                {
                    match subst.get(&param.name) {
                        Some(BasicTypeEnum::StructType(st)) => self.llvm_struct_shape_token(*st),
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let _ = write!(mangled, "${}_st_{}", param.name, token);
        }
        mangled
    }

    /// Does `name` name a real type, as opposed to an unresolved generic-param
    /// placeholder left in `subst_names`?
    ///
    /// Built on [`Self::is_known_concrete_type`] — the scalar spellings plus the
    /// declared struct/enum tables — widened by the two BUILTIN CONTAINER
    /// families it does not cover: the handle-shaped ones
    /// ([`Self::is_builtin_container_mangle_name`]) and the `{ptr,i64,i64}` ones
    /// (`Vec` / `VecDeque`; `String` is already concrete). Both are named types
    /// that an EXISTING mangle axis disambiguates, so the structural axis must
    /// leave them alone.
    ///
    /// THE `{ptr,i64,i64}` FAMILY IS THE HALF THAT BITES, and it did:
    /// `append_collection_type_param_mangle` handles those element-awarely, so a
    /// structural token on top is a SECOND copy of the same identity
    /// (`driver$T_ct_String$T_st_sh3_ptr_i64_i64`) — which broke the same five
    /// per-mono destructor tests that read the emitted symbol, the very tests
    /// `is_builtin_container_mangle_name`'s own doc records breaking under
    /// B-2026-08-13-9 for the identical double-append. Being the second author
    /// to trip that wire is why this predicate names the family explicitly
    /// rather than reaching for "anything not a user type".
    fn mangle_name_is_resolved(&self, name: &str) -> bool {
        self.is_known_concrete_type(name)
            || Self::is_builtin_container_mangle_name(name)
            || matches!(name, "Vec" | "VecDeque")
    }

    /// A shape token for an anonymous LLVM struct — `sh2_i64_i64`, leading with
    /// the field COUNT so that a nested aggregate cannot alias a flat one of
    /// the same leaf sequence (`{ {i64, i64}, i64 }` vs `{ i64, i64, i64 }`,
    /// which is exactly the arity collision this row's fixtures cover). Fields
    /// recurse; anything that is not an int/float/pointer/struct maps to the
    /// same `opaque` spelling `llvm_type_to_mangle_str` uses.
    fn llvm_struct_shape_token(&self, st: inkwell::types::StructType<'ctx>) -> String {
        let parts: Vec<String> = st
            .get_field_types()
            .into_iter()
            .map(|f| match f {
                BasicTypeEnum::StructType(inner) => self.llvm_struct_shape_token(inner),
                other => self.llvm_type_to_mangle_str(other),
            })
            .collect();
        format!("sh{}_{}", parts.len(), parts.join("_"))
    }

    /// Append the builtin-collection element-disambiguation axis to a mangled
    /// mono name (B-2026-07-11-35). For every generic param whose concrete
    /// binding is a `{ptr,i64,i64}`-shaped builtin collection (`String` / `Vec`
    /// / `VecDeque`) — the head recorded in `subst_names` — append
    /// `$<param>_ct_<token>`, where `<token>` is the ELEMENT-AWARE mono-mangle
    /// token from the typechecker (`call_type_subs_mangle`): `String`,
    /// `Vec_i64`, `Vec_String`, `Vec_Vec_i64`, … . Without this all three shapes
    /// mangled to the same `$struct` token in `mangle_mono_name` (which
    /// disambiguates only USER struct/enum names by concrete name; builtin
    /// collections deliberately keep the opaque token), so distinct
    /// instantiations shared one element-erased body. Only the collision class
    /// (String/Vec/VecDeque) is touched — Map/Set (single-`ptr` handle) and
    /// scalars mangle distinctly already, and a user struct/enum keeps the
    /// concrete-name path in `mangle_mono_name`. A no-op when no token is on
    /// record (non-generic layout monos, or a param whose binding isn't in the
    /// collision class), so existing symbols are unchanged outside this class.
    fn append_collection_type_param_mangle(
        &self,
        mut mangled: String,
        func: &Function,
        subst_names: &HashMap<String, String>,
        subst_type_exprs: &HashMap<String, TypeExpr>,
        call_span: &crate::token::Span,
    ) -> String {
        use std::fmt::Write as _;
        let Some(gp) = &func.generic_params else {
            return mangled;
        };
        // Typechecker-recorded per-call tokens (direct calls). Absent for a
        // nested generic call whose self-referential `T -> T` binding the
        // typechecker dropped (B-2026-07-13-2 leg A) — that param falls back to
        // `collection_param_mangle_token`, derived codegen-side from the resolved
        // head/element, so the nested call reaches the SAME element-aware symbol
        // as a direct call (correctly-strided body) instead of the erased one.
        let tokens = self
            .span_tables
            .call_type_subs_mangle
            .get(&(call_span.offset, call_span.length));
        for param in &gp.params {
            if param.is_const {
                continue;
            }
            // Gate on the concrete HEAD name (exact — so a user `struct Vector`
            // isn't caught by a "Vec" prefix): only the `{ptr,i64,i64}` builtin
            // collections collide on `$struct`. A user type of the same head is
            // already disambiguated by `mangle_mono_name`'s concrete-name path.
            let head = subst_names.get(&param.name).map(|h| h.as_str());
            let is_collision_class = match head {
                Some(h) => {
                    matches!(h, "String" | "Vec" | "VecDeque")
                        && !self.type_decls.struct_types.contains_key(h)
                        && !self.type_decls.enum_layouts.contains_key(h)
                }
                None => tokens.is_some_and(|t| t.contains_key(&param.name)),
            };
            if !is_collision_class {
                continue;
            }
            let token = tokens
                .and_then(|t| t.get(&param.name).cloned())
                .or_else(|| {
                    self.collection_param_mangle_token(&param.name, subst_names, subst_type_exprs)
                });
            if let Some(token) = token {
                let _ = write!(mangled, "${}_ct_{}", param.name, token);
            }
        }
        mangled
    }

    /// Append the NESTED-INSTANTIATION axis to a mangled mono name:
    /// `$<param>_gi_<token>`, where `<token>` is the FULL recursive spelling
    /// (`Box_i64`, `Box_Box_Wide`, …). B-2026-08-06-25.
    ///
    /// `mangle_mono_name` disambiguates a user struct/enum type argument by its
    /// concrete NAME, which is the HEAD segment only. Exact while the argument
    /// is non-generic — `Box[Wide]` / `Box[i64]` / `Box[String]` give `Wide` /
    /// `i64` / `String`, all distinct, which is why the whole single-level
    /// generic surface works — but every `Box[Box[..]]` names `Box` regardless
    /// of the inner box. So `Box[Box[i64]]`, `Box[Box[String]]` and
    /// `Box[Box[Wide]]` all collided on `Box.take$Box`: the first emitted
    /// defined the signature and the rest were type-checked against it, failing
    /// module verification with `Call parameter type does not match function
    /// signature`.
    ///
    /// THE INSTANTIATION COMES FROM THE RECEIVER, which is why this is a
    /// separate axis rather than a change to the two existing ones. Both of
    /// those read the typechecker's per-call tables, and for exactly the
    /// colliding calls BOTH are empty — instrumented, not assumed:
    /// `call_type_subs_mangle` has no entry for the call span and
    /// `subst_type_exprs` is empty, so neither could supply the nested
    /// spelling. What IS on record is the receiver binding's own
    /// instantiation (`enum_inst_type_of_expr`, the channel B-2026-08-06-19 and
    /// -22 made reliable): `c2` carries `Box[Box[i64]]` in full. An impl
    /// method's type params map POSITIONALLY onto the receiver instantiation's
    /// generic args — `impl[T] Box[T]` called on `Box[Box[i64]]` binds
    /// `T = Box[i64]` — so the token is read from there.
    ///
    /// GATED so every existing symbol stays byte-identical: it fires only when
    /// the bound argument is ITSELF a generic instantiation, which is precisely
    /// the row's measured trigger. A non-generic argument appends nothing, so
    /// the entire pre-existing mono surface is untouched.
    ///
    /// `mono_mangle_token_for_type_expr` already recurses (the collection axis
    /// uses it), so `Box[Box[Wide]]` and `Box[Box[Box[Wide]]]` differ as the
    /// row requires. It is a pure function of the TypeExpr — no hashing, no
    /// iteration-order input — which answers the row's stability care point;
    /// symbol length is bounded by source nesting depth and left unhashed,
    /// because a readable symbol is worth more than the bytes and a hash would
    /// reintroduce the determinism question the row flags.
    fn append_nested_instantiation_mangle(
        &self,
        mut mangled: String,
        func: &Function,
        args: &[CallArg],
    ) -> String {
        use std::fmt::Write as _;
        let Some(gp) = &func.generic_params else {
            return mangled;
        };
        // The receiver is arg 0 for an impl-method mono; its recorded
        // instantiation carries the concrete args the params bind to.
        let Some(recv) = args.first() else {
            return mangled;
        };
        let Some(inst) = self.enum_inst_type_of_expr(&recv.value) else {
            return mangled;
        };
        let TypeKind::Path(ip) = &inst.kind else {
            return mangled;
        };
        let Some(iargs) = ip.generic_args.as_ref() else {
            return mangled;
        };
        let type_params: Vec<&String> = gp
            .params
            .iter()
            .filter(|p| !p.is_const)
            .map(|p| &p.name)
            .collect();
        if type_params.is_empty() || type_params.len() != iargs.len() {
            return mangled;
        }
        for (pname, garg) in type_params.iter().zip(iargs.iter()) {
            let GenericArg::Type(te) = garg else {
                continue;
            };
            // Only a NESTED instantiation can collide; a plain type argument
            // already mangles distinctly by its head name.
            let TypeKind::Path(p) = &te.kind else {
                continue;
            };
            if p.generic_args.as_ref().is_none_or(|a| a.is_empty()) {
                continue;
            }
            // And only a USER struct/enum head, which is the class
            // `mangle_mono_name` disambiguates BY NAME and where the head-only
            // collision therefore lives. A builtin collection head
            // (`Box[Vec[i64]]` vs `Box[Vec[String]]`) is already separated by
            // `append_collection_type_param_mangle`'s `$<param>_ct_<token>`
            // axis, so suffixing it here would be redundant AND would rename
            // symbols outside this bug — measured: without this gate a
            // `mut Slice[T]` mono at `T = Vec[i64]` was renamed from
            // `kara.state.driver` to `kara.state.driver$T_gi_Vec_i64`,
            // breaking `test_slice_8af_mut_slice_t_composite_element_type`.
            // That was the whole cost of the first, looser gate, and it is why
            // "no existing symbol changes" is asserted by the suite rather than
            // by inspection.
            let Some(head) = p.segments.last() else {
                continue;
            };
            if !self.type_decls.struct_types.contains_key(head.as_str())
                && !self.type_decls.enum_layouts.contains_key(head.as_str())
            {
                continue;
            }
            let token = Self::mono_mangle_token_for_type_expr(te);
            let _ = write!(mangled, "${pname}_gi_{token}");
        }
        mangled
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
            column_var_infos: std::mem::take(&mut self.accel.column_var_infos),
            dataframe_var_infos: std::mem::take(&mut self.accel.dataframe_var_infos),
            vec_elem_types: std::mem::take(&mut self.var_types.vec_elem_types),
            var_elem_type_exprs: std::mem::take(&mut self.var_types.var_elem_type_exprs),
            array_elem_type_exprs: std::mem::take(&mut self.var_types.array_elem_type_exprs),
            closure_ret_vec_te: std::mem::take(&mut self.var_types.closure_ret_vec_te),
            enum_inst_var_types: std::mem::take(&mut self.type_decls.enum_inst_var_types),
            string_vars: std::mem::take(&mut self.var_types.string_vars),
            slice_elem_types: std::mem::take(&mut self.var_types.slice_elem_types),
            map_key_types: std::mem::take(&mut self.mapset.map_key_types),
            map_val_types: std::mem::take(&mut self.mapset.map_val_types),
            map_key_type_names: std::mem::take(&mut self.mapset.map_key_type_names),
            map_key_type_exprs: std::mem::take(&mut self.mapset.map_key_type_exprs),
            set_elem_types: std::mem::take(&mut self.mapset.set_elem_types),
            set_elem_type_names: std::mem::take(&mut self.mapset.set_elem_type_names),
            set_elem_type_exprs: std::mem::take(&mut self.mapset.set_elem_type_exprs),
            atomic_var_inner_is_bool: std::mem::take(&mut self.atomic_var_inner_is_bool),
            owned_vecstr_params: std::mem::take(&mut self.borrow_vars.owned_vecstr_params),
            closure_fn_types: std::mem::take(&mut self.closure_state.closure_fn_types),
        }
    }

    /// Restore the caller's side-tables after a nested mono compile.
    pub(super) fn restore_var_side_tables(&mut self, saved: SavedVarSideTables<'ctx>) {
        self.accel.column_var_infos = saved.column_var_infos;
        self.accel.dataframe_var_infos = saved.dataframe_var_infos;
        self.var_types.vec_elem_types = saved.vec_elem_types;
        self.var_types.var_elem_type_exprs = saved.var_elem_type_exprs;
        self.var_types.array_elem_type_exprs = saved.array_elem_type_exprs;
        self.var_types.closure_ret_vec_te = saved.closure_ret_vec_te;
        self.type_decls.enum_inst_var_types = saved.enum_inst_var_types;
        self.var_types.string_vars = saved.string_vars;
        self.var_types.slice_elem_types = saved.slice_elem_types;
        self.mapset.map_key_types = saved.map_key_types;
        self.mapset.map_val_types = saved.map_val_types;
        self.mapset.map_key_type_names = saved.map_key_type_names;
        self.mapset.map_key_type_exprs = saved.map_key_type_exprs;
        self.mapset.set_elem_types = saved.set_elem_types;
        self.mapset.set_elem_type_names = saved.set_elem_type_names;
        self.mapset.set_elem_type_exprs = saved.set_elem_type_exprs;
        self.atomic_var_inner_is_bool = saved.atomic_var_inner_is_bool;
        self.borrow_vars.owned_vecstr_params = saved.owned_vecstr_params;
        self.closure_state.closure_fn_types = saved.closure_fn_types;
    }

    /// B-2026-08-15-7 — the CONTAINER sibling of
    /// [`Self::generic_param_is_bare_type_param`]: a by-value param whose type
    /// is written out (`v: Vec[T]`, `v: Vec[i64]`) rather than as a bare type
    /// parameter.
    ///
    /// The two are disjoint by construction (this one rejects exactly what that
    /// one accepts), so the arm built on it cannot disturb the shapes
    /// B-2026-08-11-3 tuned. They are nonetheless the same ownership case: the
    /// mono prologue enters EVERY bare non-borrow param that lands in
    /// `vec_elem_types` into `owned_vecstr_params` (see `compile_mono_function`),
    /// and how the param was SPELLED is not one of its inputs. So `v: Vec[T]` is
    /// caller-retains just like `v: T` is, and a fresh temporary argument has no
    /// owner on either side — which is why `take(nums.clone())` leaked one buffer
    /// per call while the non-generic twin `take_i(v: Vec[i64])` was clean: the
    /// ordinary call path materializes every fresh heap Vec temp with no
    /// callee-shape test at all.
    ///
    /// The return-type exclusion is NOT inherited, and that is measured rather
    /// than assumed. It exists on the bare-`T` arm because a forwarding tail
    /// (`fn pick[T](a: T, b: T) -> T { id(a) }`) hands the caller's own buffer
    /// back out, and a caller-side free is then the second one. The container
    /// spelling cannot do that: an owned `Vec` param is deep-copied at every
    /// retaining consume site INCLUDING the return, so the value the caller binds
    /// is always an independent buffer. The non-generic twins are the control —
    /// `fn passthru(v: Vec[i64]) -> Vec[i64] { return v; }` and a
    /// forwarding-tail sibling are both single-free under LSan with the very same
    /// unconditional materialization this arm performs.
    ///
    /// NOTE the negation is against the SPELLING test alone
    /// ([`Self::generic_param_is_bare_type_param_spelling`]), never against
    /// `generic_param_is_bare_type_param` — that one folds the return-type
    /// exclusion in, so negating it would read "container spelling OR a bare
    /// type param that IS returned" and hand this arm precisely the forwarding
    /// tail the exclusion exists to keep out. Measured, not hypothetical: an
    /// earlier draft negated the combined predicate and turned
    /// `fn pick[T](a: T, b: T) -> T { id(a) }` into an ASAN double-free abort —
    /// the same shape, and the same failure, that B-2026-08-11-3 had already
    /// recorded once.
    fn generic_param_is_owned_container(generic_fn: &Function, idx: usize) -> bool {
        let Some(param) = generic_fn.params.get(idx) else {
            return false;
        };
        // A borrow never transfers ownership: `ref` / `mut ref` / `mut Slice`
        // params are excluded from `owned_vecstr_params` by the same test in the
        // mono prologue, and the caller's own binding still owns the buffer.
        if matches!(
            param.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        ) {
            return false;
        }
        !Self::generic_param_is_bare_type_param_spelling(generic_fn, idx)
    }

    /// The SPELLING half of [`Self::generic_param_is_bare_type_param`]: is the
    /// param written as a single-segment path naming one of the callee's own
    /// generic params (`v: T`), by value? Split out so the container arm can ask
    /// about spelling alone — see the note on
    /// [`Self::generic_param_is_owned_container`] for why the combined predicate
    /// is the wrong thing to negate.
    fn generic_param_is_bare_type_param_spelling(generic_fn: &Function, idx: usize) -> bool {
        let Some(param) = generic_fn.params.get(idx) else {
            return false;
        };
        if matches!(
            param.ty.kind,
            TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
        ) {
            return false;
        }
        let TypeKind::Path(path) = &param.ty.kind else {
            return false;
        };
        if path.generic_args.is_some() || path.segments.len() != 1 {
            return false;
        }
        let tp = &path.segments[0];
        generic_fn
            .generic_params
            .as_ref()
            .is_some_and(|gp| gp.params.iter().any(|p| &p.name == tp))
    }

    /// Is `tp` the WHOLE declared type of some parameter of `generic_fn` —
    /// `x: T`, `x: ref T`, `x: mut ref T` — as opposed to appearing nested
    /// inside one (`x: Option[T]`, `x: Vec[T]`)?
    ///
    /// This is the gate on the exact-`TypeExpr` substitution channel
    /// (B-2026-08-31-39), and it is a DELIBERATE, MEASURED narrowing rather
    /// than caution. A bare-`T` by-value param whose concrete binding is a heap
    /// collection lands in `owned_vecstr_params` — but only if the mono
    /// prologue can see its ELEMENT, because that is what puts it in
    /// `vec_elem_types`. For an argument the side-table resolvers cannot read
    /// (anything but a plain identifier — `x.clone()`, a literal, a call
    /// result) the element was invisible, so the param stayed OUT of
    /// `owned_vecstr_params` and the body MOVED the buffer through instead of
    /// deep-copying it. The caller-side materialization gate above is tuned
    /// against exactly that state.
    ///
    /// Feeding the exact channel to those params flips them into
    /// `owned_vecstr_params` and the two sides stop agreeing: measured,
    /// `fn echo[T](x: T) -> T { x }` called with `[r, r + 1, r + 2]` leaked one
    /// buffer per call (the body returns a deep copy, and nothing owns the
    /// original), while widening the materialization gate to compensate turned
    /// `fn takeout[T](b: T) -> T { match b { v => return v } }` into an ASAN
    /// DOUBLE-FREE — because a match-arm return still moves rather than copies,
    /// so the deep-copy is not uniform across return shapes and no single
    /// caller-side rule is right for both.
    ///
    /// So the channel stops at the boundary where it is needed and no further.
    /// Every shape it exists for — a type param NESTED in a param's type, which
    /// is where the head-only name map loses the element and a nameless
    /// aggregate loses everything — is untouched by this test. The whole-param
    /// case keeps the resolution it has, and the underlying inconsistency (an
    /// ownership convention that depends on the element being UNKNOWN) is filed
    /// rather than fixed here.
    fn type_param_is_a_whole_param_type(generic_fn: &Function, tp: &str) -> bool {
        generic_fn.params.iter().any(|param| {
            let peeled = match &param.ty.kind {
                TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                _ => &param.ty,
            };
            matches!(&peeled.kind, TypeKind::Path(p)
                if p.generic_args.is_none() && p.segments.len() == 1 && p.segments[0] == tp)
        })
    }

    /// Is the callee's parameter at `idx` written as a BARE type parameter
    /// (`v: T`), by value, AND not handed back out? That is the exact shape
    /// B-2026-07-11-35 routes through `owned_vecstr_params` — the mono resolves
    /// `T` to its concrete `Vec`/`String` and every retaining consume site in
    /// the body deep-copies it, so the caller keeps ownership of the argument
    /// and must free a fresh temporary itself (B-2026-08-11-3).
    ///
    /// Borrow forms (`ref T` / `mut ref T` / `mut Slice[T]`) are excluded: they
    /// never enter `owned_vecstr_params`, so the callee neither copies nor
    /// consumes and the caller's own binding still owns the buffer. A param
    /// spelled with the type param NESTED (`v: Vec[T]`) is excluded too — only
    /// a single path segment naming one of the callee's own generic params
    /// matches. That nested spelling is not unowned, merely not THIS predicate's
    /// business: [`Self::generic_param_is_owned_container`] claims it, and the
    /// call site admits either.
    ///
    /// A callee that can hand the parameter BACK OUT is rejected, and the test
    /// is syntactic rather than a body walk: if the declared return type
    /// mentions the same type param anywhere (`-> T`, `-> Vec[T]`,
    /// `-> Option[T]`), the caller's consumer of the RESULT may own the very
    /// buffer being passed in, and a caller-side free would be the second one.
    /// This is strictly stronger than `fn_returns_param`, which only recognizes
    /// a param reaching a return site bare or inside an aggregate literal and so
    /// answers false for a FORWARDING tail (`fn pick[T](a: T, b: T) -> T {
    /// id(a) }`) — measured as a real double-free abort, not a hypothetical.
    /// The leak this fix targets is on a `push`-shaped sink (return type unit,
    /// or anything not mentioning `T`), which the rule admits.
    ///
    /// The exclusion is why this predicate must not be negated to obtain the
    /// container case — see the note on
    /// [`Self::generic_param_is_owned_container`].
    fn generic_param_is_bare_type_param(generic_fn: &Function, idx: usize) -> bool {
        if !Self::generic_param_is_bare_type_param_spelling(generic_fn, idx) {
            return false;
        }
        let param = &generic_fn.params[idx];
        let TypeKind::Path(path) = &param.ty.kind else {
            return false;
        };
        let tp = &path.segments[0];
        !generic_fn
            .return_type
            .as_ref()
            .is_some_and(|rt| Self::type_expr_mentions(rt, tp))
    }

    /// Does `te` name `tp` anywhere in its tree? Used by
    /// [`Self::generic_param_is_bare_type_param`] to reject a callee whose
    /// return type could carry a by-value type-param argument back out to the
    /// caller.
    fn type_expr_mentions(te: &TypeExpr, tp: &str) -> bool {
        match &te.kind {
            TypeKind::Path(p) => {
                p.segments.iter().any(|s| s == tp)
                    || p.generic_args.as_ref().is_some_and(|args| {
                        args.iter().any(|a| match a {
                            GenericArg::Type(t) => Self::type_expr_mentions(t, tp),
                            _ => false,
                        })
                    })
            }
            TypeKind::Ref(inner)
            | TypeKind::MutRef(inner)
            | TypeKind::MutSlice(inner)
            | TypeKind::Array { element: inner, .. } => Self::type_expr_mentions(inner, tp),
            TypeKind::Tuple(elems) => elems.iter().any(|t| Self::type_expr_mentions(t, tp)),
            _ => true,
        }
    }

    pub(super) fn compile_generic_call(
        &mut self,
        name: &str,
        args: &[CallArg],
        explicit_generic_args: Option<&[GenericArg]>,
        call_span: &crate::token::Span,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let generic_fn = self.mono_state.generic_fns[name].clone();

        // Compile argument values so we can infer concrete types.
        // B-2026-07-02-13: cleared pending-let hint for the arg compiles —
        // argument literals pack at their own span-recorded (callee-declared)
        // width, not the let binding's. Mirrors the `compile_call` user-fn
        // arg loop; see `literal_span_elem_hint` for the precedence story.
        let saved_pending_elem = self.var_types.pending_let_elem_type.take();
        let saved_pending_elem_te = self.var_types.pending_let_elem_type_expr.take();
        let arg_vals: Result<Vec<BasicValueEnum<'ctx>>, String> =
            args.iter().map(|a| self.compile_expr(&a.value)).collect();
        self.var_types.pending_let_elem_type = saved_pending_elem;
        self.var_types.pending_let_elem_type_expr = saved_pending_elem_te;
        let arg_vals: Vec<BasicValueEnum<'ctx>> = arg_vals?;

        // Body walk for B-2026-08-15-9's gate, computed at most once per call
        // and only when a bare-`T` param the return-type test rejected actually
        // turns up — most generic calls never touch it.
        let mut unused_generic_params: Option<std::collections::HashSet<String>> = None;
        for (i, a) in args.iter().enumerate() {
            let val = arg_vals[i];
            // B-2026-07-14-12: a fresh-heap `String` TEMP arg to a generic fn
            // (`dup(mk())`, `passthru(mk())`, where `mk() -> String`) is
            // orphaned — the mono body CLONES a `String` generic param (both into
            // an owned copy AND when forwarding it to the return; the non-generic
            // path move-consumes instead), so the caller's temp buffer has no
            // owner and leaks. `track_inline_owned_aggregate_arg` early-returns for
            // the Vec/String struct shape, so the fresh-heap temp needs its own
            // caller-scope materialization here — the same one `compile_call`
            // emits. NOT under the `flows_into_return` passthrough guard: the mono
            // clones rather than moves a `String` param, so the temp is orphaned
            // even on the passthrough path, and the returned value is an
            // independent copy the result-consumer frees separately (verified
            // double-free-clean under valgrind). Gated to `String` (via
            // `string_typed_exprs`): a `Vec[T]` param temp instead MOVES/aliases
            // into the callee's sink (a distinct, pre-existing double-free shape),
            // so materializing it would add a spurious second free. An
            // `Identifier` arg (a named binding whose buffer its own let-drop
            // retains) is excluded by `expr_yields_fresh_owned_temp`.
            let arg_key = (a.value.span.offset, a.value.span.length);
            let is_fresh_string_temp = self.expr_yields_fresh_owned_temp(&a.value)
                && self.llvm_ty_is_vec_struct(val.get_type())
                && self.span_tables.string_typed_exprs.contains(&arg_key)
                && !self.rhs_stages_fstr_acc(&a.value);
            // B-2026-08-11-3 — the `Vec` sibling of the arm above, and the
            // third leg B-2026-07-11-35 (mono.rs, the `owned_vecstr_params`
            // registrar) names but never landed: "paired with per-monomorph
            // struct-drop synthesis … and caller-side fresh-owned-temp
            // cleanup". The first two legs shipped; this is the third.
            //
            // A param declared as the BARE TYPE PARAMETER (`fn push(mut ref
            // self, v: T)`) resolves through `subst_monomorph_type_params` to
            // its concrete `Vec[..]`, so the mono prologue enters it in
            // `owned_vecstr_params` — which makes every retaining consume site
            // in the body DEEP-COPY it (`self.items.push(v)` emits the `dcopy`
            // malloc+memcpy). That is the caller-retains convention: the callee
            // owns an independent buffer and never frees the caller's, so a
            // fresh TEMP argument has no owner anywhere and leaks one buffer
            // per call. A NAMED binding is clean only because its own let-drop
            // still owns the original — which is why this reads as an
            // argument-form bug rather than a drop-synthesis one (the
            // container's per-element drain is correct and does fire; it just
            // frees the callee's copy).
            //
            // Deliberately narrower than `compile_call`'s #20 arm, which
            // materializes every fresh heap Vec temp: this fires ONLY when the
            // callee param is written as a bare type param. That is exactly the
            // set B-2026-07-11-35 converted to deep-copy, so it cannot disturb
            // the `Vec[T]`-declared param the String arm's note warns about
            // (that shape MOVES/aliases into the callee's sink, where a
            // caller-side free would be a second free). `Identifier` args are
            // excluded by the fresh-temp predicate; collection literals are
            // admitted explicitly because `expr_yields_fresh_owned_temp` is
            // Call/MethodCall-only and the headline reproducer passes a literal.
            let is_collection_literal_arg = matches!(
                &a.value.kind,
                ExprKind::ArrayLiteral(_)
                    | ExprKind::PrefixCollectionLiteral { .. }
                    | ExprKind::RepeatLiteral { .. }
            );
            // B-2026-08-15-9 — the third admitting predicate, for a bare-`T`
            // param the return-type test excluded even though THIS param cannot
            // be what the callee returns.
            //
            // That test asks whether the declared return type mentions the
            // param's type param NAME, and the name is shared by every param
            // written with it. So in `fn pick[T](a: T, b: T) -> T { id(a) }`,
            // `b` is rejected for `a`'s reason: the return can only ever be one
            // of them, but the gate cannot tell which, so it declines both. The
            // row this closes was filed on the belief that the RETURNED param
            // leaked; it does not — `echo[T](x: T) -> T { x }` and the
            // forwarding `nest[T](x: T) -> T { id(id(x)) }` are both single-free
            // already, because the buffer moves out and the caller's result
            // binding frees it exactly once. The leak was always the SIBLING,
            // and asymmetric argument sizes are what showed it: a 128-byte `b`
            // against a 64-byte `a` leaks 128 per call.
            //
            // `unused_param_names` — not the `nonescaping_param_names` next to
            // it — is the predicate, and the difference is load-bearing rather
            // than cautious. That sibling also admits a param used only as a
            // direct `match` scrutinee, which is safe for the in-place RC
            // release it was built for but not here: `match b { v => return v }`
            // type-checks, counts `b` as a scrutinee use, and hands the caller's
            // own buffer back as the result — freeing it here would be the
            // second free. A param the body never names at all cannot reach any
            // position, returns included.
            let param_cannot_reach_return = Self::generic_param_is_bare_type_param_spelling(
                &generic_fn,
                i,
            ) && matches!(
                generic_fn.params.get(i).map(|p| &p.pattern.kind),
                Some(crate::ast::PatternKind::Binding(n))
                    if unused_generic_params
                        .get_or_insert_with(|| crate::result_escape::unused_param_names(&generic_fn))
                        .contains(n.as_str())
            );
            let is_fresh_vec_temp_for_owned_param = (self.expr_yields_fresh_owned_temp(&a.value)
                || is_collection_literal_arg)
                && self.llvm_ty_is_vec_struct(val.get_type())
                && !self.span_tables.string_typed_exprs.contains(&arg_key)
                && !self.rhs_stages_fstr_acc(&a.value)
                && (Self::generic_param_is_bare_type_param(&generic_fn, i)
                    || Self::generic_param_is_owned_container(&generic_fn, i)
                    || param_cannot_reach_return);
            if is_fresh_string_temp || is_fresh_vec_temp_for_owned_param {
                self.materialize_owned_temp(val, arg_key);
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
            .span_tables
            .call_type_subs
            .get(&(call_span.offset, call_span.length))
            .cloned()
        {
            for (param_name, concrete_name) in frame {
                // Flatten through the caller's active name-subst: a recorded
                // name that is itself an outer generic param resolves to the
                // outer param's concrete binding.
                let resolved = self
                    .mono_state
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
        // B-2026-08-31-11 — the SCALAR gap the container fallback below does not
        // cover: a same-named type param in a nested generic call, whose
        // binding the typechecker drops as self-referential. Runs BEFORE the
        // container pass and fills only params still unbound, so a recorded
        // binding always wins. See the helper for the measurement.
        self.augment_subst_names_from_arg_type_names(
            &generic_fn,
            args,
            &mut subst_names,
            &mut subst,
        );
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
        // B-2026-08-27-40 — the STRUCTURAL half of the loop below. A type
        // argument that is a tuple has no NAME, so the `TypeKind::Path` arm
        // cannot record it and the whole name channel skips it; collected here
        // and merged into the element-aware `subst_type_exprs` once that map
        // exists (it is built further down, by resolvers that must read live
        // var side-tables first). See `merge_structural_type_arg_substs`.
        let mut structural_type_args: Vec<(String, TypeExpr)> = Vec::new();
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
                                    .mono_state
                                    .type_subst_names
                                    .get(seg)
                                    .cloned()
                                    .unwrap_or_else(|| seg.clone());
                                subst_names.insert(param.name.clone(), resolved);
                            }
                        } else if Self::type_expr_is_structural_type_arg(t) {
                            structural_type_args.push((param.name.clone(), t.clone()));
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

        // B-2026-07-13-2/-3: resolve each bare-`T` param bound WHOLE to a
        // collection argument to its concrete head (fills `subst_names` for the
        // nested-generic-call case the typechecker drops — leg A) and its
        // element-aware `TypeExpr` (`Vec`/`VecDeque` — leg B). Reads the
        // caller's LIVE var side-tables, so it MUST run before the mangle (which
        // needs the head + token) AND before `take_var_side_tables` clears them.
        let mut subst_type_exprs =
            self.resolve_collection_param_substs(&generic_fn, args, &mut subst_names);
        // B-2026-07-18-45: a generic-STRUCT param whose type param is bound to a
        // whole collection (`get[T](b: Box[T])` called with `b: Box[Vec[i64]]`).
        // `infer_type_args` can't recover `T`'s element (Box[Vec[i64]] and
        // Box[String] share the erased `{ {ptr,len,cap} }` LLVM shape), and the
        // collection-param resolver above only handles a param that IS a bare
        // `T` — not one nested inside a user struct. Unify the declared
        // `Box[T]` against the arg's recorded concrete instantiation
        // (`enum_inst_var_types`) to bind `T -> Vec[i64]` in the element-aware
        // subst, so the mono entry-copy can deep-copy the Vec field (else it
        // aliases the caller's buffer and both free it — a double-free).
        self.resolve_generic_struct_param_substs(
            &generic_fn,
            args,
            &mut subst_names,
            &mut subst_type_exprs,
        );
        // B-2026-07-29-35: the two resolvers above cover a param that IS a bare
        // `T` and one nested inside a user generic struct. Neither covers the
        // plainest shape of all — a CONTAINER param whose element is the type
        // param, `fn f[T](s: Slice[T])`. Its LLVM type was already bound by
        // `augment_subst_from_arg_elem_types`, so only the name/TypeExpr maps
        // were missing, and the gap surfaced as `karac_clone_T`.
        self.resolve_container_param_elem_substs(
            &generic_fn,
            args,
            &mut subst_names,
            &mut subst_type_exprs,
        );

        // B-2026-08-27-40, free-function leg: an impl method's tuple type args
        // arrive through the explicit-args loop above; a free generic fn has no
        // receiver to read them from, so recover them from the ARGUMENTS.
        // Pushed after the explicit ones so those still win the `or_insert`.
        self.resolve_structural_param_substs(&generic_fn, args, &mut structural_type_args);

        // B-2026-08-27-40: fold in the NAMELESS type arguments captured from the
        // explicit-args loop above. Runs after the three resolvers so a binding
        // any of them recorded still wins (`or_insert`), which keeps every
        // existing instantiation on exactly the entry it had.
        Self::merge_structural_type_arg_substs(structural_type_args, &mut subst_type_exprs);

        // B-2026-08-31-39: the typechecker's EXACT per-type-arg `TypeExpr` for
        // this call span. The three resolvers above each cover one param SHAPE
        // (a bare `T`, a `T` inside a user generic struct, a `Container[T]`);
        // this covers every shape at once, because it comes from the solver's
        // own solution rather than from re-deriving the binding out of the
        // argument. It is what lets a `T` nested in an `Option[T]` / `Result[T,
        // E]` param, or bound to a NAMELESS aggregate the name channel cannot
        // spell, resolve inside the body at all.
        //
        // Flattened through the CALLER's active substitution before it is
        // stored, so a recorded `Vec[T]` inside a monomorph resolves against the
        // outer binding — the same flattening `subst_names` gets above. That
        // must happen HERE, while the caller's maps are still live.
        let subst_call_te: HashMap<String, TypeExpr> = self
            .span_tables
            .call_type_subs_te
            .get(&(call_span.offset, call_span.length))
            .map(|frame| {
                frame
                    .iter()
                    .filter(|(k, _)| {
                        !Self::type_param_is_a_whole_param_type(&generic_fn, k.as_str())
                    })
                    .map(|(k, te)| (k.clone(), self.subst_monomorph_type_params(te)))
                    .collect()
            })
            .unwrap_or_default();

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
            self.mono_state
                .mono_handle_param_infos
                .insert(mangled.clone(), handle_params);
        }
        // B-2026-07-11-35 (return-owned-param leg) — disambiguate a generic
        // param bound to a builtin COLLECTION (String / Vec / VecDeque) by its
        // element-aware token. Those three all lower to the opaque
        // `{ptr,i64,i64}` shape, so `mangle_mono_name` emitted the same `$struct`
        // token for every one and the second instantiation silently reused the
        // first's element-erased body — benign while the body only MOVED the
        // value, but a miscompile once the tail-return deep-copies with the
        // wrong element stride (`echo[String]` then `echo[Vec[i64]]`: the Vec
        // copy ran String's i8 stride → 3-byte under-copy → UB read). Append the
        // full mono-mangle token (`Vec_i64` / `Vec_String` / `String`) so each is
        // a distinct symbol with its own correctly-strided body.
        let mangled = self.append_collection_type_param_mangle(
            mangled,
            &generic_fn,
            &subst_names,
            &subst_type_exprs,
            call_span,
        );
        // B-2026-08-27-40 — the NAMELESS-type-argument axis. A tuple lowers to
        // the opaque `$struct` token and has no `subst_names` entry to
        // disambiguate it, so `W[(i64, i64)]` and `W[(String, i64)]` collided on
        // one `W.add$struct` symbol.
        let mangled = self.append_structural_type_param_mangle(
            mangled,
            &generic_fn,
            &subst,
            &subst_names,
            &subst_type_exprs,
        );
        // B-2026-08-06-25 — a type argument that is ITSELF a generic
        // instantiation (`Box[Box[i64]]` vs `Box[Box[String]]`) mangles only
        // its HEAD above, so every `Box[Box[..]]` collided on one symbol.
        let mangled = self.append_nested_instantiation_mangle(mangled, &generic_fn, args);
        // Bind handle-backed-container type params (`C` bound to a Column/Tensor
        // arg) to `ptr` so a bare-`C` RETURN (`map`/`zip_with` → `Self`) or a
        // `let d: C` local lowers to the pointer shape, not the `i64` default
        // (the "return type does not match operand type" verifier error). Done
        // AFTER `mangle_mono_name` above so the mangled name is byte-identical
        // to before — the injection changes only the `type_subst` the body /
        // return lowering consults, never the mono cache key.
        self.augment_subst_from_handle_params(&generic_fn, args, &mut subst);

        // B-2026-07-08-6 (generic/mono leg) — caller-side arg-temp drop, the
        // twin of the mono param entry-copy. The monomorph body ENTRY-COPIES an
        // owned heap struct/enum param and returns an INDEPENDENT copy, so —
        // exactly as in the non-generic `compile_call` path — the caller must
        // drop the ORIGINAL moved-in arg buffer of an inline aggregate temp
        // (struct/tuple literal, enum-variant ctor), else it is orphaned. Same
        // gate as `compile_call`: skip only when the callee FORWARDS the arg
        // (`call_arg_flows_into_return`) AND does not entry-copy it — an
        // entry-copied heap struct arg is registered even on the
        // return-passthrough path. `track_inline_owned_aggregate_arg`
        // self-restricts to inline temps (identifier args keep their binding's
        // drop; the fstr→struct move is already suppressed inside
        // `compile_expr`), so nothing is double-registered. Runs in the CALLER's
        // context — before the mono body is compiled inline below, which swaps
        // `scope_cleanup_actions`.
        //
        // B-2026-08-06-2 defect (B) — it sits HERE, after the substitution is
        // built, rather than beside the arg compiles above, because deciding
        // ownership of a GENERIC struct temp requires the CALLEE's view. A
        // struct whose heap sits behind a bare `T` (`Box[T] { v: T }`) has no
        // name-keyed drop, so `track_struct_var` silently registered nothing and
        // the temp leaked one buffer per call. Supplying the instantiation fixes
        // that — but only where the callee actually entry-copies: the same
        // struct arrives as OWN-BY-TRANSFER both through a concrete fn
        // (`fn take(b: Box[String])`) and through a monomorph whose fn-level
        // param is named differently from the struct's (`fn take[U](b: Box[U])`,
        // where `mono_struct_type_from_active_subst` finds no `T` binding and
        // falls back to the base layout). There the callee TOOK the buffer and a
        // caller drop is a double free. Installing the callee's subst and asking
        // the callee's own predicate is what makes the two agree by
        // construction; a caller-side look-alike cannot see that distinction.
        //
        // The predicate's raw answer rides alongside the instantiation rather
        // than being recovered from it (B-2026-08-07-17). `enum_inst_type_from_-
        // span` can miss where the predicate holds, and the two callers want
        // opposite defaults on that gap: the drop registration needs the
        // instantiation or it cannot key a drop at all, while the own-by-
        // transfer suppression must NOT fire for an entry-copying callee whose
        // span carries no annotation — that would trade B-2026-08-07-15's
        // corruption for a leak. Collapsing them into `Option::is_some` reads
        // the second question off an answer to the first.
        // B-2026-08-07-18 — the IDENTIFIER-argument companion, decided in the
        // same subst-installed block for the same reason: only the callee's
        // view can tell the two arms apart.
        //
        // The own-by-transfer arm's safety argument is a LOCKSTEP — the callee
        // takes the caller's buffers, so the caller must give up its drop — and
        // `compile_call` honours it on the concrete path via
        // `move_declined_copy_struct_arg`. This path never did: everything
        // below reasons about struct LITERALS, so `t(b)` for a let-bound `b`
        // left the binding and the callee both owning the same buffers.
        //
        // It stayed invisible because the callee's drop was not real. Keyed by
        // bare name in a monomorph it was synthesized against the ERASED layout
        // and freed a length word (the -18 corruption) or, for an all-bare-`T`
        // struct with no concrete field, was never emitted at all (the -18
        // leak). Giving that drop the instantiation is what turned the missing
        // retraction into an observable double free.
        let mut mono_agg: Vec<(bool, Option<TypeExpr>)> = Vec::new();
        let mut transfer_ident: Vec<bool> = Vec::new();
        {
            let saved_names =
                std::mem::replace(&mut self.mono_state.type_subst_names, subst_names.clone());
            let saved_type_exprs = std::mem::replace(
                &mut self.mono_state.type_subst_type_exprs,
                subst_type_exprs.clone(),
            );
            let saved_call_te = std::mem::replace(
                &mut self.mono_state.type_subst_call_te,
                subst_call_te.clone(),
            );
            for a in args.iter() {
                mono_agg.push(match &a.value.kind {
                    ExprKind::StructLiteral { path, .. } => match path.last() {
                        Some(sname) if self.mono_entry_copies_aggregate_param(sname) => {
                            (true, self.enum_inst_type_from_span(&a.value))
                        }
                        _ => (false, None),
                    },
                    _ => (false, None),
                });
                transfer_ident.push(match &a.value.kind {
                    ExprKind::Identifier(var) => self
                        .var_types
                        .var_type_names
                        .get(var.as_str())
                        .cloned()
                        .is_some_and(|sname| {
                            let entry_copies = self.mono_entry_copies_aggregate_param(&sname);
                            self.struct_param_owned_by_transfer(&sname, entry_copies)
                        }),
                    _ => false,
                });
            }
            self.mono_state.type_subst_names = saved_names;
            self.mono_state.type_subst_type_exprs = saved_type_exprs;
            self.mono_state.type_subst_call_te = saved_call_te;
        }
        for (i, a) in args.iter().enumerate() {
            let val = arg_vals[i];
            // B-2026-08-29-3 — a METHOD reaches this loop under a plain
            // `Type.method` key, which `call_arg_flows_into_return`'s
            // `Item::Function`-only scan answers `false` for. So a generic
            // method that hands an argument back left the caller registering a
            // body the RESULT binding also owns: `impl G1 { fn gm_ret[T](ref
            // self, r: R, t: T) -> R { r } }` ran R's `Drop` body TWICE on all
            // three compiled backends against once in the interpreter, and once
            // for both the non-generic and free-function twins.
            //
            // The same two predicates the non-generic method-argument site asks
            // (`method_call.rs`). Both legs hold for a generic callee since
            // B-2026-08-28-71: `fn_always_returns_param` never needed callee
            // cooperation, and the conditional leg's callee-side flip now exists
            // in `compile_mono_function` too. THIS is the site that decides the
            // generic METHOD case — a generic FREE function stands down one line
            // below through `call_arg_flows_into_return`'s union, which is
            // `Item::Function`-only and so answers `false` for a `Type.method`
            // key. Measured with this leg still gated on `generic_params
            // .is_none()` and every other half of that row's fix in place:
            // `impl T { fn pick[U](ref self, r: R, k: bool, u: U) -> R }` ran
            // R's body TWICE on all three compiled backends, against once in the
            // interpreter, while the free-function twin was already correct.
            //
            // THE INDEX HERE IS RECEIVER-INCLUSIVE, the opposite of the sibling
            // site: this loop's `args` carry the receiver, so a two-parameter
            // method iterates i = 0, 1, 2 while `find_function_ast` hands back
            // the raw AST method whose `params` exclude it. Established by
            // instrumentation before writing this, not inferred — the
            // conventions genuinely differ per site here, which is what
            // B-2026-08-28-70 had to verify the same way.
            let handed_off = self
                .program_snapshot
                .as_deref()
                .and_then(|p| super::declarations::find_function_ast(p, name))
                .is_some_and(|f| {
                    let ast_i = if f.self_param.is_some() {
                        match i.checked_sub(1) {
                            Some(x) => x,
                            None => return false,
                        }
                    } else {
                        i
                    };
                    crate::ast::fn_always_returns_param(f, ast_i)
                        || crate::ast::fn_conditionally_returns_param_bare(f, ast_i)
                });
            let flows_into_return = self.call_arg_flows_into_return(name, i) || handed_off;
            // B-2026-08-26-9 — the monomorphized leg of the same union the
            // free-fn arg loop computes: a callee that STORES this argument into
            // `self` or into a `ref`/`mut ref` param leaves it alive in the
            // caller, so its Drop body belongs to the value's new home while the
            // orphaned original's memory stays ours. This is the leg
            // `PriorityQueue[Item].push` actually takes.
            let escapes_frame =
                flows_into_return || self.call_arg_moves_into_outliving_place(name, i, true);
            // B-2026-08-27-44 — the monomorph leg of the same admission test.
            // The tuple sibling is needed here for the same reason as on the
            // free-fn path: a generic `passthru[T](p: (Bag[T], i64)) -> ..`
            // entry-copies its tuple param (B-2026-08-27-37 gave the mono path
            // that copy), so on the escape path the caller's original is
            // orphaned unless it is admitted to the registrar.
            //
            // NOTE the ENUM sibling (`arg_is_entry_copied_heap_enum`) is absent
            // here and stays absent: B-2026-08-01-14 added it only to the
            // free-fn gate, so whether a generic enum arg leaks the same way is
            // an unmeasured question, and guessing a caller-side free onto an
            // unmeasured path is how a leak fix becomes a double free.
            if !flows_into_return
                || self.arg_is_entry_copied_heap_struct(&a.value)
                || self.arg_is_entry_copied_heap_tuple(&a.value, name, i)
            {
                let escaping_parts = self.callee_returned_param_parts(name, i);
                self.track_inline_owned_aggregate_arg_inst(
                    val,
                    &a.value,
                    escapes_frame,
                    mono_agg[i].1.clone(),
                    mono_agg[i].0,
                    &escaping_parts,
                );
            }
        }
        // The retraction itself runs with the CALLER's substitution restored —
        // it edits caller-scope cleanup frames — while the decision above was
        // made under the callee's. See `struct_param_owned_by_transfer`.
        for (i, a) in args.iter().enumerate() {
            if transfer_ident[i] {
                self.move_declined_copy_struct_arg(&a.value);
            }
        }

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
        if !self.mono_state.generated_monos.contains(&mangled) {
            // Mark as in-progress before recursing to avoid infinite loops.
            self.mono_state.generated_monos.insert(mangled.clone());

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
            let saved_fn_name = std::mem::take(&mut self.fn_ctx.current_fn_name);
            let saved_vars = std::mem::take(&mut self.variables);
            let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
            // The mono body is compiled INLINE, mid-caller — so its tensor
            // param registrations (added by `compile_mono_function`) must not
            // leak into the caller's `tensor_var_infos`, which is keyed by
            // bare var name and would otherwise have a caller-side `a` / `b`
            // overwritten by the callee's same-named tensor param. Swap to a
            // clean slate for the body (module-level tensor bindings are
            // re-seeded inside `compile_mono_function`) and restore below —
            // parallel to `variables` / `var_type_names`.
            let saved_tensor_infos = std::mem::take(&mut self.accel.tensor_var_infos);
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
            let saved_cleanup = std::mem::take(&mut self.drop_rc.scope_cleanup_actions);
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
            let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
            let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
            let saved_subst = std::mem::replace(&mut self.mono_state.type_subst, subst.clone());
            // Name-level twin of `type_subst` (B-2026-07-03-11): thread the
            // concrete-type-name subst so the mono param prologue can register a
            // bound-generic receiver under its concrete type for trait dispatch.
            let saved_subst_names =
                std::mem::replace(&mut self.mono_state.type_subst_names, subst_names.clone());
            // Element-aware twin (B-2026-07-13-2/-3): thread each Vec/VecDeque
            // whole-collection param's FULL concrete `TypeExpr` so the body
            // registers its element (see `type_subst_type_exprs`). Built above,
            // before the side-table swap cleared the caller's element map.
            let saved_subst_type_exprs =
                std::mem::replace(&mut self.mono_state.type_subst_type_exprs, subst_type_exprs);
            // Third channel (B-2026-08-31-39): the typechecker's exact
            // per-type-arg `TypeExpr`, so a bare `T` in the body resolves to
            // the type this call instantiated it at even when it is nested in a
            // non-container param or names no type at all.
            let saved_subst_call_te =
                std::mem::replace(&mut self.mono_state.type_subst_call_te, subst_call_te);
            // Const generics slice 4: thread the const-arg substitution
            // into the body-lowering pass so `compile_expr Identifier`
            // can resolve const-param refs against it. Parallel to
            // `type_subst`'s save/restore.
            let saved_const_subst =
                std::mem::replace(&mut self.mono_state.const_subst, const_subst.clone());
            // Per-layout-monomorphization axis: thread the per-call layout
            // substitution into the body-lowering pass. Parallel to
            // `type_subst` / `const_subst`. Slice 1 always carries `Aos`
            // entries, so body lowering (which doesn't yet consult this map)
            // is unchanged; slice 2 reads it to select the SoA access paths.
            let saved_layout_subst =
                std::mem::replace(&mut self.mono_state.layout_subst, layout_subst.clone());
            // Slice 4: `compile_mono_function`'s prologue may register SoA
            // borrow params in `ref_params` (a generic fn with a `ref Vec[E]`
            // param whose binding-site layout is SoA). Swap it out for the mono
            // body and restore below, like `variables` — see the matching note
            // in `ensure_layout_mono_generated`.
            let saved_ref_params = std::mem::take(&mut self.borrow_vars.ref_params);
            let saved_signature_ref_params =
                std::mem::take(&mut self.borrow_vars.signature_ref_params);
            // Slice 5: per-binding layout carrier — the mono body seeds its own
            // locals at their `let` sites; swap out the caller's map and restore
            // below, parallel to `variables` / `ref_params`.
            let saved_binding_layouts = std::mem::take(&mut self.var_types.binding_layouts);
            // Same isolation for the entry-slot-ref locals (the two-step
            // `let r = m.entry(k).or_insert(d)` binding tag): a nested mono
            // body must not see/clobber the outer function's tags.
            let saved_entry_slot_ref_vars =
                std::mem::take(&mut self.borrow_vars.entry_slot_ref_vars);
            let saved_soa_return_locals = std::mem::take(&mut self.accel.soa_return_locals);
            // B-2026-08-28-71 — the mono's conditional-move flags are allocas in
            // the mono's own function, so they must not be visible to (or
            // survive into) the enclosing body. Mirrors `saved_cleanup`.
            let saved_cond_move_flags = std::mem::take(&mut self.drop_rc.cond_move_drop_flags);
            let saved_optres_bodies_flags =
                std::mem::take(&mut self.drop_rc.optres_payload_bodies_flags);
            let saved_cond_store_params = std::mem::take(&mut self.drop_rc.cond_store_flag_params);
            let saved_field_view_flags = std::mem::take(&mut self.drop_rc.field_view_flags);
            let saved_decl_anchors = std::mem::take(&mut self.drop_rc.loop_decl_rearm_anchors);

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
            self.drop_rc.cond_move_drop_flags = saved_cond_move_flags;
            self.drop_rc.optres_payload_bodies_flags = saved_optres_bodies_flags;
            self.drop_rc.cond_store_flag_params = saved_cond_store_params;
            self.drop_rc.field_view_flags = saved_field_view_flags;
            self.drop_rc.loop_decl_rearm_anchors = saved_decl_anchors;
            self.accel.soa_return_locals = saved_soa_return_locals;
            self.var_types.binding_layouts = saved_binding_layouts;
            self.borrow_vars.ref_params = saved_ref_params;
            self.borrow_vars.signature_ref_params = saved_signature_ref_params;
            self.borrow_vars.entry_slot_ref_vars = saved_entry_slot_ref_vars;
            self.mono_state.layout_subst = saved_layout_subst;
            self.mono_state.const_subst = saved_const_subst;
            self.mono_state.type_subst = saved_subst;
            self.mono_state.type_subst_names = saved_subst_names;
            self.mono_state.type_subst_type_exprs = saved_subst_type_exprs;
            self.mono_state.type_subst_call_te = saved_subst_call_te;
            self.fn_ctx.loop_stack = saved_loop_stack;
            self.conc.branch_cancel_ptr = saved_cancel_ptr;
            self.drop_rc.scope_cleanup_actions = saved_cleanup;
            self.restore_var_side_tables(saved_side_tables);
            self.accel.tensor_var_infos = saved_tensor_infos;
            self.var_types.var_type_names = saved_var_types;
            self.variables = saved_vars;
            self.current_fn = saved_fn;
            self.fn_ctx.current_fn_name = saved_fn_name;
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
            self.conc
                .state_machine_state_constructors
                .get(&mangled)
                .copied()
        } else {
            None
        };
        if let Some(ctor_fn) = ctor_fn_opt {
            let poll_fn = self
                .conc
                .state_machine_poll_fns
                .get(&mangled)
                .copied()
                .expect("poll-fn co-emitted with state-machine constructor");
            let state_struct = self
                .conc
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
            let ref_flags = self
                .fn_sig
                .fn_param_ref
                .get(&mangled)
                .cloned()
                .unwrap_or_default();
            let slice_elems = self
                .fn_sig
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
                .build_call(self.runtime_fns.sched_yield_fn, &[], "kara.yield_result")
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
                if let Some(ret_ty) = self.conc.state_machine_return_types.get(&mangled).copied() {
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
                .build_call(self.runtime_fns.free_fn, &[state_ptr.into()], "")
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
        let ref_flags = self
            .fn_sig
            .fn_param_ref
            .get(&mangled)
            .cloned()
            .unwrap_or_default();
        // B-2026-08-05-37 — the mutate-through subset, for the place-argument
        // arm below. The non-generic direct-call path (call_dispatch.rs) gained
        // the same arm; a generic callee has exactly the same ABI obligation,
        // and without this `bump[T](mut g.v, 8i64)` silently discarded its
        // write under AOT while the interpreter (which keys on the same flags)
        // performed it — a run/build divergence the mono path introduces on its
        // own.
        let mut_ref_flags = self
            .fn_sig
            .fn_param_mut_ref
            .get(&mangled)
            .cloned()
            .unwrap_or_default();
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
            .fn_sig
            .fn_param_slice_elem
            .get(&mangled)
            .cloned()
            .unwrap_or_default();
        let compiled_args: Vec<BasicMetadataValueEnum<'ctx>> = arg_vals
            .iter()
            .enumerate()
            .map(|(i, v)| -> Result<BasicMetadataValueEnum<'ctx>, String> {
                if ref_flags.get(i).copied().unwrap_or(false) {
                    // B-2026-07-30-3: a GENERIC `ref Slice[T]` param fed an
                    // `Array[T, N]`. This is the hole B-2026-06-19-1 closed on
                    // the NON-generic path (the Array->header synthesis at
                    // call_dispatch.rs) which this monomorphized path never
                    // got. An Array binding's storage is its raw elements with
                    // no `{ptr,len}` header, so the `get_data_ptr` fast-path
                    // below hands the callee `&array[0]` and it reads
                    // `ptr = elem0, len = elem1` — a bogus slice. Observed
                    // directly: `s.len()` over `[100,7,3,4]` returned 7 (elem1)
                    // instead of 4, and using elem0 as a pointer trapped
                    // (SIGTRAP for String elements, SIGSEGV for scalars), while
                    // the interpreter was correct. Synthesize the header and
                    // pass a pointer to it, exactly as the non-generic path
                    // does. Array sources ONLY, mirroring that gate: a `Vec`
                    // binding's storage starts with `{ptr,len}` (a header
                    // superset) and a `Slice` / `ref Slice` binding's
                    // `get_data_ptr` already yields a header pointer, so both
                    // forward correctly below — intercepting them would
                    // re-coerce a ref-slice binding and corrupt the forward.
                    // `coerce_to_slice`'s Array fast-path derives ptr and len
                    // from the binding's own alloca and `ArrayType::len()`, so
                    // it does not depend on `elem_ty` — which is why no
                    // `type_subst` element binding is needed here.
                    if let Some(Some(elem_ty)) = slice_elems.get(i).cloned() {
                        if self.arg_is_array_source(&args[i].value) {
                            if let Some(slice_val) =
                                self.coerce_to_slice(&args[i].value, elem_ty)?
                            {
                                let ptr = self.materialize_rvalue_for_ref_arg(slice_val, i);
                                return Ok(BasicMetadataValueEnum::from(ptr));
                            }
                        }
                    }
                    // B-2026-08-05-37: a `mut ref` param given a PLACE
                    // argument must receive a pointer to the place, not to the
                    // rvalue copy `materialize_rvalue_for_ref_arg` mints — the
                    // callee's write would land on that copy and vanish.
                    let mut_ref_place = if mut_ref_flags.get(i).copied().unwrap_or(false) {
                        self.mut_ref_place_arg_ptr(&args[i].value)
                    } else {
                        None
                    };
                    // B-2026-08-25-5: `self` parses as `SelfValue`, NOT
                    // `Identifier("self")`, so a sibling call on the receiver
                    // (`self.one()` inside another method of the same generic
                    // impl) matched NONE of the pointer-producing arms below —
                    // not this one, not the index-borrow arm, and not
                    // `mut_ref_place_arg_ptr` (which handles only FieldAccess /
                    // TupleIndex, on the stated assumption that "a bare
                    // identifier already took the `get_data_ptr` fast path").
                    // It fell through to `materialize_rvalue_for_ref_arg`, so
                    // the callee received a pointer to a COPY of the receiver
                    // and every mutation it made through `mut ref self` was
                    // discarded. Normalising the receiver to the name `self`
                    // routes it through `get_data_ptr`, which loads the stored
                    // pointer for a `ref`/`mut ref` param exactly as it does
                    // for any other borrow. Same normalisation several other
                    // sites already spell out (`control_flow_for.rs`,
                    // `calls.rs`, `call_dispatch.rs`).
                    let ident_name: Option<&str> = match &args[i].value.kind {
                        ExprKind::Identifier(n) => Some(n.as_str()),
                        ExprKind::SelfValue => Some("self"),
                        _ => None,
                    };
                    // B-2026-08-27-52 — the read-only `ref` sibling of the
                    // `mut_ref_place` arm above, and the MONO half of
                    // B-2026-07-12-1. That row gave the non-generic call path an
                    // in-place borrow for a struct-FIELD argument to a `ref`
                    // param, for exactly the reason it matters here: the
                    // fall-through `materialize_rvalue_for_ref_arg` shallow-copies
                    // the field's `{ptr,len,cap}` header into a temp and queues a
                    // scope-exit FREE of that buffer, but the field is still owned
                    // by the receiver, so the owner's own field drop doubles it.
                    // The generic path never got that arm: `mut_ref_place` is
                    // computed only when `mut_ref_flags[i]` is set, so
                    // `vlen(self.xs)` against `v: ref Vec[T]` aborted with glibc
                    // `free(): double free detected` under AOT and the JIT while
                    // the interpreter was correct — a silent run/build divergence,
                    // and the `mut ref` spelling of the same call was already
                    // clean because it took the arm above.
                    //
                    // Gated on the compiled value's LLVM shape rather than on a
                    // re-lowering of the field: `*v` is the field header already
                    // in hand, so `{ptr,len,cap}` IS the confirmed double-free
                    // class (`Vec` / `String`). That mirrors the non-generic
                    // arm's `field_ty == vec_struct_type()` restriction, which
                    // exists so an `Option[shared T]` field still reaches its
                    // RC-inc and a niche/enum field keeps the copy path.
                    let ref_place =
                        if mut_ref_place.is_none() && self.llvm_ty_is_vec_struct(v.get_type()) {
                            self.mut_ref_place_arg_ptr(&args[i].value)
                        } else {
                            None
                        };
                    let ptr: BasicValueEnum<'ctx> = if let Some(var_name) = ident_name {
                        if let Some(ptr) = self.get_data_ptr(var_name) {
                            ptr.into()
                        } else {
                            self.materialize_rvalue_for_ref_arg(*v, i)
                        }
                    } else if let Some(elem_ptr) = self.ref_arg_index_borrow_ptr(&args[i].value)? {
                        elem_ptr.into()
                    } else if let Some(place_ptr) = mut_ref_place.or(ref_place) {
                        place_ptr.into()
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

        // GAP 1 (B-2026-07-12-16): a mono'd generic method with a by-value
        // narrow-int type param (`fn set[T](v: T)` at `T=i32`) declares an
        // `i32` param, but Kāra's narrow-ints-live-in-i64-slots convention
        // hands the argument over as i64 — so the by-value scalar pushes above
        // pass an i64 against an i32 param and LLVM module verification hard-
        // fails ("Call parameter type does not match function signature") even
        // for a single instantiation. The non-generic direct-call path narrows
        // via `coerce_args_to_fn_params`; the generic mono-call path skipped it.
        // Coerce here (int/float scalars only; ref pointers / aggregates are
        // left untouched by the helper) so the mono param widths are honoured.
        let mut compiled_args = compiled_args;
        self.coerce_args_to_fn_params(func, &mut compiled_args);

        let call = self
            .builder
            .build_call(func, &compiled_args, "call")
            .unwrap();

        let basic_val = call.try_as_basic_value();
        if basic_val.is_instruction() {
            Ok(self.context.i64_type().const_int(0, false).into())
        } else {
            let v = basic_val.unwrap_basic();
            // LazyFrame codegen twin — rule 3, the generic-call twin of the
            // `compile_call` hook: a generic fn DECLARED to return LazyExpr/
            // LazyFrame (`std.lazy`'s `lit[T]`) hands back an escaping +1;
            // register the matching release in the CALLER's scope (the mono
            // body's frame stack was already swapped back by this point).
            self.register_lazy_user_call_result(name, v);
            Ok(v)
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
        if self.mono_state.generated_monos.contains(mangled) {
            return Ok(());
        }
        self.mono_state.generated_monos.insert(mangled.to_string());

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let saved_vars = std::mem::take(&mut self.variables);
        let saved_var_types = std::mem::take(&mut self.var_types.var_type_names);
        let saved_tensor_infos = std::mem::take(&mut self.accel.tensor_var_infos);
        // Full var-side-table isolation — see `SavedVarSideTables`.
        let saved_side_tables = self.take_var_side_tables();
        let saved_cleanup = std::mem::take(&mut self.drop_rc.scope_cleanup_actions);
        let saved_cancel_ptr = self.conc.branch_cancel_ptr.take();
        let saved_loop_stack = std::mem::take(&mut self.fn_ctx.loop_stack);
        let saved_subst = std::mem::take(&mut self.mono_state.type_subst);
        // Name-subst twin (B-2026-07-03-11): isolate the layout-mono body from a
        // stale outer name-subst, mirroring `type_subst`.
        let saved_subst_names = std::mem::take(&mut self.mono_state.type_subst_names);
        // Element-aware twin (B-2026-07-13-2/-3): a layout mono is non-generic,
        // so clear any stale outer type-expr subst too; restored below.
        let saved_subst_type_exprs = std::mem::take(&mut self.mono_state.type_subst_type_exprs);
        let saved_subst_call_te = std::mem::take(&mut self.mono_state.type_subst_call_te);
        let saved_const_subst = std::mem::take(&mut self.mono_state.const_subst);
        let saved_layout_subst = std::mem::replace(&mut self.mono_state.layout_subst, layout_subst);
        let saved_return_layout = std::mem::replace(&mut self.return_layout, return_layout);
        // Slice 4: the mono prologue now registers SoA `ref`/`mut ref Vec[E]`
        // params in `ref_params` (so the access paths deref the slot once).
        // `ref_params` is per-function state the caller doesn't otherwise swap
        // out, so take it for the mono body (empty → the prologue rebuilds it
        // for this mono's own params) and restore the caller's map after —
        // mirroring the `variables` save/restore above. Without this a mono's
        // ref param would mark a same-named caller binding as a borrow.
        let saved_ref_params = std::mem::take(&mut self.borrow_vars.ref_params);
        let saved_signature_ref_params = std::mem::take(&mut self.borrow_vars.signature_ref_params);
        // Slice 5: the mono body seeds its own locals' layouts in
        // `binding_layouts` at their `let` sites. Take the caller's carrier for
        // the duration (the body starts empty, like `variables`) and restore it
        // after, so a mono's local can't leak its SoA-ness back to a same-named
        // caller binding.
        let saved_binding_layouts = std::mem::take(&mut self.var_types.binding_layouts);
        let saved_entry_slot_ref_vars = std::mem::take(&mut self.borrow_vars.entry_slot_ref_vars);
        // Returned-local set is per-function; `compile_mono_function` repopulates
        // it from this mono's body. Save/restore so it can't leak across the
        // nested compile (mirrors `binding_layouts`).
        let saved_soa_return_locals = std::mem::take(&mut self.accel.soa_return_locals);
        // B-2026-08-28-71 — see the twin in `compile_generic_call`.
        let saved_cond_move_flags = std::mem::take(&mut self.drop_rc.cond_move_drop_flags);
        let saved_optres_bodies_flags =
            std::mem::take(&mut self.drop_rc.optres_payload_bodies_flags);
        let saved_cond_store_params = std::mem::take(&mut self.drop_rc.cond_store_flag_params);
        let saved_field_view_flags = std::mem::take(&mut self.drop_rc.field_view_flags);
        let saved_decl_anchors = std::mem::take(&mut self.drop_rc.loop_decl_rearm_anchors);

        let result = self
            .declare_mono_function(func, mangled)
            .and_then(|_| self.compile_mono_function(func, mangled));

        self.drop_rc.cond_move_drop_flags = saved_cond_move_flags;
        self.drop_rc.optres_payload_bodies_flags = saved_optres_bodies_flags;
        self.drop_rc.cond_store_flag_params = saved_cond_store_params;
        self.drop_rc.field_view_flags = saved_field_view_flags;
        self.drop_rc.loop_decl_rearm_anchors = saved_decl_anchors;
        self.accel.soa_return_locals = saved_soa_return_locals;
        self.var_types.binding_layouts = saved_binding_layouts;
        self.borrow_vars.ref_params = saved_ref_params;
        self.borrow_vars.signature_ref_params = saved_signature_ref_params;
        self.borrow_vars.entry_slot_ref_vars = saved_entry_slot_ref_vars;
        self.return_layout = saved_return_layout;
        self.mono_state.layout_subst = saved_layout_subst;
        self.mono_state.const_subst = saved_const_subst;
        self.mono_state.type_subst = saved_subst;
        self.mono_state.type_subst_names = saved_subst_names;
        self.mono_state.type_subst_type_exprs = saved_subst_type_exprs;
        self.mono_state.type_subst_call_te = saved_subst_call_te;
        self.fn_ctx.loop_stack = saved_loop_stack;
        self.conc.branch_cancel_ptr = saved_cancel_ptr;
        self.drop_rc.scope_cleanup_actions = saved_cleanup;
        self.restore_var_side_tables(saved_side_tables);
        self.accel.tensor_var_infos = saved_tensor_infos;
        self.var_types.var_type_names = saved_var_types;
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
            LayoutId::Soa(block) => self.accel.soa_layouts.get(block).cloned(),
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
        self.fn_sig
            .fn_param_ref
            .insert(mangled.to_string(), ref_flags);
        // B-2026-08-05-37 — the mutate-through subset. Same shape as the
        // non-generic `declare_function`; see `fn_param_mut_ref`.
        let mut_ref_flags: Vec<bool> = func
            .params
            .iter()
            .map(|p| matches!(&p.ty.kind, TypeKind::MutRef(_) | TypeKind::MutSlice(_)))
            .collect();
        self.fn_sig
            .fn_param_mut_ref
            .insert(mangled.to_string(), mut_ref_flags);
        let slice_elems: Vec<Option<BasicTypeEnum<'ctx>>> = func
            .params
            .iter()
            .map(|p| self.extract_slice_elem_type(&p.ty))
            .collect();
        self.fn_sig
            .fn_param_slice_elem
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
        // A generic `#[target_feature]` / `#[multiversion]` variant carries its
        // `target-features` onto every specialization — the non-generic
        // declaration path does this via `emit_codegen_hint_attrs`, but the mono
        // path is separate, so re-emit here or a generic hot kernel would compile
        // each instance against the bare module baseline (phase-11 multiversion
        // generics follow-on).
        self.apply_target_feature_attr(fn_val, func);
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
        self.fn_ctx.current_fn_name = func.name.clone();
        self.variables.clear();
        self.var_types.var_type_names.clear();
        // Per-binding layout carrier (slice 5): the caller's map was swapped out
        // (`mem::take`) at the mono entry point, so this fresh body starts empty
        // and seeds its own locals; `let`-site registrations land here.
        self.var_types.binding_layouts.clear();
        // This mono's returned local(s) — so the origin name-match in
        // `seed_binding_site_layout` is suppressed for them (their layout comes
        // from `return_layout` / `layout_subst`, seeded just below). The caller's
        // set was swapped out at the mono entry point.
        self.accel.soa_return_locals = self
            .soa_return_local_names(&func.body)
            .into_iter()
            .collect();
        self.payload_vars.inline_option_payload_vars.clear();
        self.payload_vars.boxed_enum_payload_vars.clear();
        self.payload_vars.boxed_optres_payload_view_vars.clear();
        self.payload_vars.deboxed_payload_box_ptrs.clear();
        self.payload_vars.inline_result_payload_vars.clear();
        self.payload_vars.inline_option_map_payload_vars.clear();
        self.payload_vars.inline_option_agg_payload_vars.clear();
        // Function-level scope-cleanup frame for owned locals (`Tensor` /
        // `Vec` / `String` / `Map` lets needing drop), mirroring
        // `compile_function`. The caller's frame stack was swapped out in
        // `compile_generic_call`, so this is the body's sole, fresh stack;
        // let-site registrations land here and drain at the tail return
        // below. Without it, a mono body's `let out = Tensor.zeros(…)`
        // FreeTensor cleanup leaked into the caller's frame and was emitted
        // where the callee's alloca didn't dominate ("Instruction does not
        // dominate all uses").
        self.drop_rc.scope_cleanup_actions.clear();
        self.drop_rc.scope_cleanup_actions.push(Vec::new());
        // B-2026-08-28-71 — the two halves of B-2026-08-28-51's per-path
        // conditional-move machinery that `compile_function` sets up and this
        // prologue omitted. Both are needed before a mono body can own a
        // conditionally-returned param's `Drop` body (the param-loop
        // registration below).
        //
        //  * The FLAGS are ALLOCAS, so they are function-scoped exactly as the
        //    `compile_function` comment says. A mono is compiled NESTED inside
        //    its caller's body, so without this the mono's guard would load the
        //    CALLER's stack slot for a same-named binding. Both call sites
        //    (`compile_generic_call`, the layout-mono path) save and restore the
        //    caller's map around the nested compile, mirroring
        //    `scope_cleanup_actions`.
        //  * The BODY TAIL is the first of the three escaping sites, and the
        //    only one seeded per FUNCTION rather than per statement (`let`
        //    initializers and `return` operands are seeded in `compile_stmt`,
        //    which a mono body shares). A generic function's body is never
        //    compiled by `compile_function`, so nothing had ever seeded it —
        //    `arm_conditional_move_tail_flag` then early-returns on the
        //    returning arm and the guard never clears. Measured, with the
        //    param registration in place but this line absent: the escaping
        //    call `genf(R { id: 1, name: f"i1" }, true, 7)` ran the body TWICE,
        //    once inside the callee on the path that returned the value.
        //    `cond_move_escaping_sites` is span-keyed and shared across
        //    monomorphizations, so re-seeding per mono is idempotent.
        self.drop_rc.cond_move_drop_flags.clear();
        self.drop_rc.optres_payload_bodies_flags.clear();
        self.drop_rc.cond_store_flag_params.clear();
        self.drop_rc.field_view_flags.clear();
        self.drop_rc.loop_decl_rearm_anchors.clear();
        if let Some(tail) = func.body.final_expr.as_deref() {
            self.note_escaping_site(tail);
        }
        // Slice 10: reseed module-binding side-tables in monomorphised
        // bodies too (same reason as the `compile_function` path —
        // `var_type_names` is cleared per function).
        self.reseed_module_binding_side_tables();

        let entry = self.context.append_basic_block(fn_val, "entry");
        self.builder.position_at_end(entry);

        // Params of THIS mono body that never escape (used only as a `match`
        // scrutinee, or unused) — gates the owned boxed-enum param drop below.
        let nonescaping_params = crate::result_escape::nonescaping_param_names(func);

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
                self.borrow_vars
                    .ref_params
                    .insert(param_name.clone(), inner_ty);
                self.borrow_vars
                    .signature_ref_params
                    .insert(param_name.clone());
            }
            // B-2026-08-05-7 — the monomorph's own copy of the owned-param box
            // drop. This is THE site that matters for the reported shape: the
            // erasure that causes the boxing lives in the generic declaration
            // (`o: Opt[T]`), so the boxed value is nearly always a temp handed
            // straight into a generic callee (`get(Opt.Yes(f"…"), d)`) with no
            // binding anywhere to hang a drop on. `compile_function`'s sibling
            // registration never runs for a mono body — mono has its own param
            // loop — so without this the envelope leaked once per call.
            //
            // Resolve through the active subst first: the declared `Opt[T]` is
            // the erased form, and the predicate must see `Opt[String]` to know
            // it boxes. BOX-ONLY, as at the other two sites.
            //
            // Gated on the same escape walk as the `compile_function` sibling —
            // a param that is forwarded or returned hands the box on, and freeing
            // it here is a double-free (see that site for the two measured
            // shapes). Computed locally rather than through
            // `result_shared_nonescaping_param_names`: a mono body is compiled
            // INLINE inside its caller's, so that field still holds the caller's
            // set and writing to it would corrupt the caller's `Result[shared]`
            // arm.
            if !self.borrow_vars.ref_params.contains_key(&param_name)
                && nonescaping_params.contains(&param_name)
            {
                let mono_ty = self.subst_monomorph_type_params(&param.ty);
                for (enum_name, variant) in self.user_enum_boxed_payload_variants(&mono_ty) {
                    self.track_boxed_enum_var_with_inner_drop(
                        &param_name,
                        alloca,
                        &enum_name,
                        &variant,
                        None,
                    );
                }
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
                            .mono_state
                            .type_subst_names
                            .get(type_name)
                            .cloned()
                            .unwrap_or_else(|| type_name.clone());
                        self.var_types
                            .var_type_names
                            .insert(param_name.clone(), concrete);
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
            // B-2026-08-07-18 — the param's CONCRETE instantiation, resolved
            // HERE rather than at the recording site below, because the
            // ownership call needs it and used to run first.
            //
            // Only the own-by-transfer arm's DROP reads it: `inst` reaches
            // `track_struct_var_inst`, which binds the STRUCT's own params
            // POSITIONALLY from it (`Mix[U]` under `U -> String` gives
            // `Mix[String]`, hence `T -> String`). Every arm-selection
            // predicate in `make_aggregate_param_callee_owned_inst` is
            // name-keyed and cannot see this, which is the point: the erased
            // drop is corrected WITHOUT re-deciding who owns the param, so the
            // caller-side predicates gated on that decision
            // (`arg_is_entry_copied_heap_struct`, the flows-into-return gate)
            // keep answering exactly as before.
            //
            // Without it the mono path passed `None` and the own-by-transfer
            // drop was synthesized against the ERASED base layout — reading a
            // heap field at the base offset, which for `Mix[T] { v: T, s:
            // String }` is field 0's LENGTH word, and freeing it.
            let param_inst = {
                let peeled = match &param.ty.kind {
                    TypeKind::Ref(inner) | TypeKind::MutRef(inner) => inner.as_ref(),
                    _ => &param.ty,
                };
                self.concrete_generic_struct_inst(peeled)
            };
            if matches!(&param.ty.kind, TypeKind::Path(_)) {
                if let Some(concrete) = self.var_types.var_type_names.get(&param_name).cloned() {
                    self.make_aggregate_param_callee_owned_inst(
                        &concrete,
                        alloca,
                        param_inst.clone(),
                    );
                }
            }
            // B-2026-08-27-37 — the TUPLE leg of the pairing rule above. That
            // arm is gated `TypeKind::Path(_)`, so a by-value TUPLE param fell
            // through it and the mono got NO entry-copy and NO scope-exit drop,
            // while `compile_function` gives a non-generic callee both (#21,
            // `make_tuple_param_callee_owned`). The caller's tuple-literal arm
            // is not gated on genericity and registers its temp drop either way,
            // on the stated assumption that "the callee now entry-copies a
            // heap-bearing tuple param, so this caller temp is an INDEPENDENT
            // buffer". For a mono that assumption was false: caller temp and
            // callee param aliased one buffer, so `let (b, _n) = p; b.xs` handed
            // the SAME Vec out to the result AND left the caller's temp drop to
            // free it — a double free at `T = i64` as well as `T = String`,
            // with the interpreter correct. Same "the two MUST stay paired"
            // rule as B-2026-07-08-6 above, one param shape further.
            //
            // The element types must be resolved through the active monomorph's
            // substitution first: `type_expr_has_drop_heap(Bag[T])` reads the
            // GENERIC form as heapless, so passing the declared elements would
            // bail at the first gate and change nothing.
            if let TypeKind::Tuple(elems) = &param.ty.kind {
                if let BasicTypeEnum::StructType(agg_ty) = param_val.get_type() {
                    let concrete: Vec<crate::ast::TypeExpr> = elems
                        .iter()
                        .map(|e| {
                            self.concrete_generic_struct_inst(e)
                                .unwrap_or_else(|| e.clone())
                        })
                        .collect();
                    self.make_tuple_param_callee_owned(&concrete, agg_ty, alloca);
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
            // struct-literal span record. Computed above (B-2026-08-07-18
            // moved it ahead of the ownership call, which needs the same
            // value); this is the recording half, unchanged.
            if let Some(inst) = param_inst {
                self.type_decls
                    .enum_inst_var_types
                    .insert(param_name.clone(), inst);
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
                // B-2026-07-11-35 (push leg) — resolve a bare type-param param
                // (`x: T`) to its CONCRETE monomorph type before registration.
                // Without this the registrar sees `Path("T")` (not String/Vec),
                // so a `x: T` (T=String) param never enters `vec_elem_types` /
                // `owned_vecstr_params`, and the retaining `self.xs.push(x)` MOVES
                // the caller's buffer instead of deep-copying it — the element
                // then reads as garbage from the caller (a `Box[String].add(...)`
                // push corrupted every element). The active `type_subst_names`
                // (set by `compile_generic_call`) drives the resolution; a no-op
                // outside a monomorph. Paired with per-monomorph struct-drop
                // synthesis (so the now-deep-copied element buffers are freed at
                // the container's drop) and caller-side fresh-owned-temp cleanup.
                let resolved_registration_te = self.subst_monomorph_type_params(registration_te);
                let registration_te = &resolved_registration_te;
                self.register_var_from_type_expr(&param_name, registration_te);
                // A bare-type-param param bound to a handle-backed builtin
                // (Column/Tensor) registers from the call site's recorded
                // arg type — the declared te is just `C`, which the
                // registrar above can't act on (S6a; see
                // `mono_handle_param_infos`).
                let handle_info = self
                    .mono_state
                    .mono_handle_param_infos
                    .get(mangled)
                    .and_then(|entries| entries.iter().find(|(n, _)| n == &param_name))
                    .map(|(_, info)| info.clone());
                match handle_info {
                    Some(super::state::MonoHandleArgInfo::Column(ci)) => {
                        let info = self.column_var_info_from_table(&ci);
                        self.accel.column_var_infos.insert(param_name.clone(), info);
                    }
                    Some(super::state::MonoHandleArgInfo::Tensor(ti)) => {
                        let info = self.tensor_var_info_from_table(&ti);
                        self.accel.tensor_var_infos.insert(param_name.clone(), info);
                    }
                    None => {}
                }
                // Owned (bare, non-ref) String/Vec params: retaining consume
                // sites must deep-copy — the same owned-header set
                // `compile_function` records (see `owned_vecstr_params`).
                if !matches!(
                    param.ty.kind,
                    TypeKind::Ref(_) | TypeKind::MutRef(_) | TypeKind::MutSlice(_)
                ) && self.var_types.vec_elem_types.contains_key(&param_name)
                {
                    self.borrow_vars
                        .owned_vecstr_params
                        .insert(param_name.clone());
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
                self.closure_state
                    .closure_fn_types
                    .insert(param_name.clone(), fn_type);
            }
            // B-2026-08-28-22's conditionally-returned owned param, MONO leg
            // (B-2026-08-28-71). `compile_function`'s sibling registration
            // never runs for a mono body — mono has its own param loop — so a
            // GENERIC callee kept that row's original defect: the caller stands
            // down over the UNION of return sites while only one path returns
            // the value, and whichever value died inside the call lost its user
            // `Drop` body. Measured before this: `genf(R { id: 1, name: f"i1" },
            // false, 7)` printed `96` / `drop n96` with `drop i1` missing.
            //
            // Every safety property is the non-generic site's, unchanged —
            // BODIES ONLY (the caller still owns the memory, so the binding's
            // own wrapper would double-free a heap field) and GUARDED PER PATH
            // by the flag whose two missing halves the prologue above now sets
            // up. See `compile_function` for the full argument.
            //
            // The row this closes predicted the mono param slot holds a POINTER
            // rather than the struct, and that is REFUTED: instrumentation at
            // this site prints `param_val_ty = { i64, { ptr, i64, i64 } }` — the
            // struct by value, the same shape `compile_function` sees — with the
            // alloca an ordinary opaque `ptr`. The corruption the row measured
            // came from the unseeded escaping site, not from the slot.
            if !self.is_coroutine_compiled(&func.name)
                && crate::ast::fn_conditionally_returns_param_bare(func, i)
                && !crate::ast::fn_moves_param_into_outliving_place(func, i)
            {
                // B-2026-09-02-3 — the SUBSTITUTED parameter type, not the raw
                // one. `param.ty` here is the GENERIC AST, so a param declared
                // `a: T` reads as the literal path `T`, and the user-`Drop`
                // lookup below asks `drop_method_keys` for a type named "T" and
                // is told no. The registration then silently did not happen,
                // and the value that died inside the call ran no body on any
                // compiled lane while `--interp` ran it.
                //
                // That is why B-2026-08-28-71 looked complete: its measurement
                // was `genf(R { .. }, false, 7)`, whose conditionally-returned
                // param is declared with the CONCRETE type `R`, so the raw path
                // already named a real struct. The gap is only for a param
                // whose declared type IS the type parameter.
                //
                // `subst_monomorph_type_params` is the existing resolver and it
                // already answers `R` at this exact point (instrumented:
                // `raw=Path(["T"]) substituted=Path(["R"])` with
                // `type_subst_names = {"T": "R"}`), so this needs no new
                // channel — only the resolved type instead of the declared one.
                let param_ty_resolved = self.subst_monomorph_type_params(&param.ty);
                if let TypeKind::Path(path) = &param_ty_resolved.kind {
                    if let Some(struct_name) = path.segments.first() {
                        let has_user_drop = self
                            .program_snapshot
                            .as_deref()
                            .map(|p| p.drop_method_keys.contains_key(struct_name))
                            .unwrap_or(false);
                        if has_user_drop
                            && !self
                                .type_decls
                                .shared_types
                                .contains_key(struct_name.as_str())
                        {
                            if let Some(bodies) =
                                self.emit_struct_user_drop_bodies_only_fn(struct_name)
                            {
                                self.track_user_drop_var_with_fn(
                                    "",
                                    &param_name,
                                    alloca,
                                    bodies,
                                    crate::codegen::state::UserDropKind::StructFieldBodies,
                                );
                            }
                        }
                    }
                }
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
                self.mono_state
                    .layout_subst
                    .insert(name, LayoutId::Soa(block.clone()));
            }
        }

        // Slice-parameter scoped-alias metadata (alias-metadata slice 4). After
        // the mono param registration above, before the body — same ordering as
        // the non-generic `compile_function` path.
        self.build_slice_alias_scopes(func);

        // Contract frame for THIS mono (B-2026-08-21-21). The monomorphized
        // path had none at all, so `requires` / `ensures` / `invariant` on a
        // generic function were silently dropped by both compiled backends
        // while the interpreter enforced them — design.md § Contracts leads
        // with a generic `binary_search[T: Ord]`, so the documented shape was
        // the unchecked one.
        //
        // Saved and restored around the body because a mono is compiled INLINE
        // inside its caller (`compile_generic_call` reaches here mid-body), so
        // the caller's own `ensures` clauses and `old(...)` snapshots are live
        // across this call — the same reason `variables`, `ref_params` and the
        // layout carriers are swapped at the mono entry point.
        let saved_contract_frame = self.take_contract_frame();
        self.install_contract_frame(func)?;

        let mut result = self.compile_block(&func.body)?;

        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            // B-2026-07-11-35 (return-owned-`T`-param leg) — deep-copy an owned
            // String/Vec PARAM returned in tail position, mirroring
            // `compile_function` (functions.rs). The by-value header ABI leaves the
            // buffer's free with the CALLER that passed the arg (owned String/Vec
            // params are caller-retains — they land in `owned_vecstr_params`, never
            // a callee-side `track_vec_var`), and the caller receiving THIS return
            // binds-and-frees it too — so handing back the moved-in buffer directly
            // double-frees (`fn echo[T](x: T) -> T { x }` over a fresh String arg:
            // main frees it once as `a` and once as the f-string temp). Copy so each
            // side owns an independent buffer. The non-generic `compile_function`
            // already does this at its tail; the mono path had only the Identifier-
            // tail MOVE suppression below (`suppress_cleanup_for_tail_return`), which
            // zeros the LOCAL param cap but leaves the returned value aliasing the
            // caller's buffer. MUST run BEFORE that suppression — the defensive copy
            // reads the param's `cap` to decide whether to duplicate, and the
            // suppression zeros it. A no-op for a non-owned-vecstr tail (the copy
            // self-gates on `owned_vecstr_params` membership + a recorded elem type).
            if let (Some(final_expr), Some(v)) = (func.body.final_expr.as_deref(), result) {
                result = Some(self.maybe_defensive_copy_return_value(final_expr, v));
            }
            // LazyFrame codegen twin — retain-on-return (rule 2), the mono
            // twin of the `compile_function` hook: a generic fn DECLARED to
            // return LazyExpr/LazyFrame (`std.lazy`'s `lit[T]`) hands its
            // caller an escaping +1 that must survive the release drain
            // below; `compile_generic_call` registers the matching release.
            if let (Some(kind), Some(val)) = (
                func.return_type
                    .as_ref()
                    .and_then(Self::lazy_kind_of_type_expr),
                result,
            ) {
                self.emit_lazy_retain_for_return(kind, val);
            }
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
            // Contract `ensures` / `invariant` checks at the tail return, with
            // `result` bound to the tail value — BEFORE scope cleanup, so the
            // postcondition sees live params and result. Same ordering and same
            // exit point as `compile_function` (B-2026-08-21-21). An explicit
            // `return` inside the body already fires them through the shared
            // Return arm in `compile_expr`, which reads the frame installed
            // above; only this tail site is per-path.
            self.emit_ensures_checks(result)?;
            self.emit_invariant_checks(result)?;
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
                None if !fn_returns_void => {
                    // No tail value in a NON-void mono: the body's tail was a
                    // DIVERGENT expression (`loop { return .. }` / `loop {}`,
                    // type `Never`), so this fall-through block is unreachable —
                    // the typechecker already proved every real exit returns a
                    // value. Emitting `ret void` here (the old `_` arm) fails
                    // module verification in a non-void fn ("Function return
                    // type does not match operand type of return inst", B-2026-
                    // 07-12-8). Emit `unreachable`, mirroring the non-generic
                    // `compile_function` tail (functions.rs). The non-generic
                    // sibling and a generic `while`/`loop{..break;}`+tail-return
                    // already compiled; only the generic divergent-`loop` tail
                    // hit this arm.
                    self.builder.build_unreachable().unwrap();
                }
                _ => {
                    self.builder.build_return(None).unwrap();
                }
            }
        }
        // Hand the enclosing function back its contract frame — this mono's
        // is finished at the returns above (B-2026-08-21-21).
        self.restore_contract_frame(saved_contract_frame);
        // Leave the frame stack as the caller swapped it in
        // (`compile_generic_call` restores its own); clearing keeps the
        // post-body state tidy and matches `compile_function`'s exit.
        self.drop_rc.scope_cleanup_actions.clear();

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
            .type_decls
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
                            // B-2026-08-25-11 — prefer the ELEMENT-AWARE full
                            // `TypeExpr` before the head-only name map, the same
                            // precedence `subst_monomorph_type_params` already
                            // uses. `type_subst_names` is name→name, so a param
                            // bound to a collection (`T = Vec[i64]`) resolves to
                            // the bare head `"Vec"` and the element is silently
                            // dropped — this function then recorded
                            // `enum_inst_var_types["self"] = Heap[Vec]`.
                            //
                            // That record is what the receiver's scope-exit drop
                            // is selected from, so the elementless instantiation
                            // picked `__karac_drop_struct_Heap$Vec`, whose
                            // per-element drop is `karac_drop_Vec` — an EMPTY
                            // STUB (`ret void`), because a `Vec` with no element
                            // gives the drop synthesizer nothing to free. The
                            // caller's own drop of the same struct is correctly
                            // mangled `$Vec_i64`, so one program emitted both and
                            // only the monomorph's elements leaked.
                            //
                            // Before B-2026-08-25-10 the callee's entry-copy
                            // aliased the caller's element buffers, so the
                            // caller's drop freed them and the stub's no-op-ness
                            // was invisible — that aliasing WAS that double free.
                            // Making the copy element-deep left the callee owning
                            // real buffers its own drop did not free.
                            if let Some(full) =
                                self.mono_state.type_subst_type_exprs.get(&p.segments[0])
                            {
                                any_bound = true;
                                return GenericArg::Type(full.clone());
                            }
                            if let Some(concrete) =
                                self.mono_state.type_subst_names.get(&p.segments[0])
                            {
                                any_bound = true;
                                return GenericArg::Type(TypeExpr {
                                    kind: TypeKind::Path(PathExpr {
                                        segments: vec![concrete.clone()],
                                        generic_args: None,
                                        span: t.span,
                                    }),
                                    span: t.span,
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
                span: te.span,
            }),
            span: te.span,
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
        ) || self.type_decls.struct_types.contains_key(name)
            || self.type_decls.enum_layouts.contains_key(name)
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
                // `i128` / `u128` are here for a THIRD instance of the same
                // erasure, and the sharpest one: LLVM's `IntType` carries a
                // width but no SIGNEDNESS, so `llvm_type_to_mangle_str` renders
                // both as `i128` and structurally cannot separate them. Without
                // the name channel a program instantiating a generic at both
                // widths gave them one body, and whichever was emitted first
                // decided the signedness for both — `show[T](x) { f"{x}" }`
                // printed `u128::MAX` as `-1` beside a correct `i128 -7`, while
                // the u128-ONLY program was correct. That asymmetry is what
                // shows the 128-bit code generation itself is fine and only the
                // symbol was shared (B-2026-08-30-45).
                | "i128"
                | "u128"
                // f16 / bf16 are here for the same reason the narrow ints are:
                // `llvm_type_to_mangle_str` cannot tell them apart from `f64`
                // (it only special-cases `f32`), so without the NAME channel
                // every `half` and `bfloat` instantiation mangled to `$f64` and
                // collided — with each other and with a genuine `f64` one. The
                // second instantiation then reused the first's body and codegen
                // inserted `fpext`/`fptrunc` at the call to force the arguments
                // into the winner's width, so `g(a, b)` at `bf16` silently
                // computed at `f16` (B-2026-08-30-36).
                | "f16"
                | "bf16"
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
                            if self.type_decls.struct_types.contains_key(name)
                                || self.type_decls.enum_layouts.contains_key(name)
                            {
                                mangled.push_str(name);
                                continue;
                            }
                        }
                    }
                    // B-2026-08-13-9 — the same collapse, one type family over.
                    // A BUILTIN CONTAINER lowers to `"struct"` (String/Vec) or
                    // `"ptr"` (Map/Set/Slice and the handle-backed builtins), so
                    // `show(map)` and `show(set)` both mangled to `show$ptr`: one
                    // compiled body served both instantiations and dispatched
                    // every receiver to whichever impl it was built against —
                    // measured as `m s m m` against the interpreter's `m s m s`.
                    //
                    // The arm above says a builtin "keeps the `$struct` token so
                    // its existing per-mono symbols are unchanged (its method
                    // dispatch never keys on `var_type_names`, so the opaque
                    // token is still sound)". That premise expired: user trait
                    // impls on `Map`/`Set` (ec1d19c) and on `Slice[T]` (b0ef988)
                    // dispatch BY RECEIVER NAME, so the identity the token erases
                    // is now load-bearing. Disambiguating by name is what the
                    // primitive arm (B-2026-07-03-24) and the user-struct arm
                    // (B-2026-07-03-11) each already do for their own erasure —
                    // this is the third instance of one rule, not a new one.
                    if matches!(token.as_str(), "ptr" | "struct") {
                        if let Some(name) = subst_names.get(&param.name) {
                            if Self::is_builtin_container_mangle_name(name) {
                                mangled.push_str(name);
                                continue;
                            }
                        }
                    }
                    mangled.push_str(&token);
                } else if let Some(name) = subst_names.get(&param.name) {
                    // B-2026-08-13-9 — a NESTED generic call inside a monomorph
                    // (`outer[T](x) { inner(x) }`) reaches here with NO LLVM-type
                    // binding: `infer_type_args` leaves the inner `subst` empty
                    // because the typechecker drops the self-referential `T -> T`
                    // it sees inside a generic body. The arms above are all keyed
                    // on that binding, so the inner call appended NOTHING and every
                    // instantiation shared one symbol — `outer(map)` and
                    // `outer(set)` got distinct outer monos that both called the
                    // same `inner`, which answered `m! m!` where the interpreter
                    // said `m! s!`.
                    //
                    // The NAME is known here even when the LLVM type is not
                    // (`resolve_collection_param_substs` fills it from the caller's
                    // live side-tables), and it is the same identity the arms above
                    // reach for. Gated to names that ARE a concrete type — a bare
                    // type param that resolved to nothing keeps today's suffix-less
                    // symbol rather than putting `T` in the symbol table.
                    if Self::is_builtin_container_mangle_name(name)
                        || Self::is_scalar_primitive_mangle_name(name)
                        || self.type_decls.struct_types.contains_key(name)
                        || self.type_decls.enum_layouts.contains_key(name)
                    {
                        mangled.push('$');
                        mangled.push_str(name);
                    }
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
    /// (slice 2) to body lowering via `self.mono_state.layout_subst`.
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
        if let Some(layout) = self.mono_state.layout_subst.get(binding_name) {
            return layout.clone();
        }
        if let Some(layout) = self.var_types.binding_layouts.get(binding_name) {
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
        let layout = if let Some(layout) = self.mono_state.layout_subst.get(binding_name) {
            // A returned local seeded by a return-SoA mono, or a name the
            // dispatch already laid out. Honored even for a returned local —
            // this IS the return-mono's SoA seeding.
            layout.clone()
        } else if self.accel.soa_layouts.contains_key(binding_name)
            && !self.accel.soa_return_locals.contains(binding_name)
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
                self.var_types
                    .binding_layouts
                    .insert(binding_name.to_string(), LayoutId::Soa(block.clone()));
                self.accel.soa_layouts.get(&block).cloned()
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
            LayoutId::Soa(block) => self.accel.soa_layouts.get(&block).cloned(),
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
        match self.mono_state.layout_subst.get(name) {
            Some(LayoutId::Soa(block)) => self.accel.soa_layouts.get(block).cloned(),
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
        self.fn_sig
            .fn_asts
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

    /// B-2026-08-13-9 — is `name` a BUILTIN container head whose LLVM lowering
    /// erases its identity in the mangle token?
    ///
    /// The HANDLE-shaped ones only. `String` / `Vec` / `VecDeque` lower to
    /// `{ptr,i64,i64}` and are already disambiguated — element-awarely, which is
    /// strictly better — by `append_collection_type_param_mangle`'s `$<p>_ct_<t>`
    /// axis; adding them here appended a SECOND copy of the same identity
    /// (`driver$String$T_ct_String`) and broke five per-mono destructor tests
    /// that read the emitted symbol. What that axis skips is exactly this list:
    /// its comment says "Map/Set (single-`ptr` handle) … mangle distinctly
    /// already", which was true until a user `impl` could attach methods to
    /// them — a `ptr` is a `ptr`, so `Map` and `Set` collided.
    ///
    /// Deliberately a CLOSED list rather than "anything not a user type": a name
    /// absent from it keeps its existing token, so no symbol outside this family
    /// changes.
    fn is_builtin_container_mangle_name(name: &str) -> bool {
        matches!(name, "Slice" | "Map" | "Set" | "SortedMap" | "SortedSet")
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
                // Every float width that is not `f64` must name itself, or two
                // instantiations share a symbol and the second silently reuses
                // the first's body. `f16` and `bf16` were both answering "f64"
                // here (B-2026-08-30-36); they are distinct LLVM types (`half`
                // vs `bfloat`) with distinct arithmetic, so they are compared
                // against the canonical types exactly as `f32` already was.
                if t == self.context.f16_type() {
                    "f16".to_string()
                } else if t == self.context.bf16_type() {
                    "bf16".to_string()
                } else if t == self.context.f32_type() {
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
    /// `val_size` field (offset 48): after status, kv, capacity, len,
    /// tombstones, key_size (5*8 + 2*8 = 48). Read by the Set contains probe to
    /// recover the true bucket stride `key_size + val_size` — a `Set.new()` map
    /// has `val_size = 0`, but a *cloned* Set has `val_size = 8` (its unit value
    /// type lowers to an i64-sized slot in `emit_map_clone_fn`), so the stride
    /// is not constant across Set instances and must be read at runtime.
    const KARAC_MAP_VAL_SIZE_OFFSET: u64 = 48;
    /// `key_size` field (offset 40). Read by the monomorphized String-key Map
    /// probe to recover the bucket stride and the value's in-bucket offset
    /// without hardcoding `sizeof(String) == 24`.
    const KARAC_MAP_KEY_SIZE_OFFSET: u64 = 40;
    /// `eq_fn` pointer field (offset 64), immediately after `hash_fn`. Read by
    /// the String-key Map probe for the same reason it reads `hash_fn`: the
    /// comparison must be the one the buckets were actually filled with.
    const KARAC_MAP_EQ_FN_OFFSET: u64 = 64;
    /// `hash_fn` pointer field: after status, kv (2 ptrs) + capacity, len,
    /// tombstones, key_size, val_size (5 usizes) = 8*2 + 8*5 = 56. Used by
    /// [`emit_mono_set_contains_body`] to call the Set's *actual* stored hash
    /// rather than a hardcoded one — different Set creation paths (`Set.new`
    /// vs `clone`/set-ops) may store different-but-self-consistent hashes, so
    /// the mono probe must hash the same way the buckets were filled.
    const KARAC_MAP_HASH_FN_OFFSET: u64 = 56;
    /// Bucket control-byte encoding for the monomorphized probe loops. Must
    /// match `runtime/src/map.rs` — see its module header for the layout and
    /// for why disagreement here is a silent wrong answer rather than a crash.
    ///
    ///   `0x00`        EMPTY
    ///   `0x01`        TOMBSTONE
    ///   `0x80 | tag7` OCCUPIED, `tag7` = bits 57..63 of the key's hash
    ///
    /// B-2026-07-26-2 replaced a bare `OCCUPIED = 1` flag with the tagged form.
    /// The probes exploit it in two different ways depending on shape: a LOOKUP
    /// probe (`get` / `contains`) compares the byte against the searched key's
    /// own control byte, which tests occupancy and tag in ONE instruction and
    /// skips the key dereference entirely on a mismatch; an INSERT probe still
    /// reaches its occupied arm by elimination (not EMPTY, not TOMBSTONE),
    /// which stays correct verbatim under the new encoding.
    const BUCKET_EMPTY: u64 = 0;
    const BUCKET_TOMBSTONE: u64 = 1;
    const BUCKET_OCCUPIED_BIT: u64 = 0x80;

    /// Whether a LOOKUP probe folds the 7-bit hash tag into its occupancy test
    /// (`status == ctrl`) or tests occupancy alone (`status >= 0x80`), for the
    /// probe shape named by `key`. B-2026-08-05-5.
    ///
    /// The tag's BENEFIT is skipping the key load+compare on a bucket it
    /// rejects, so it is worth exactly what that skipped compare would have
    /// cost. Its PRICE is a serialized compare against a computed operand, and
    /// that price is heavily architecture-dependent. Hence a policy per (probe
    /// shape x target arch) rather than one switch.
    ///
    /// MEASURED, Apple M5 Pro / arm64, `KARAC_MAP_TAG` A/B on one karac
    /// (exact `scripts/pmc.c` counters, `hyperfine` both orders, sinks
    /// verified, `KARAC_AUTO_PAR=0`). Negative = the tag costs:
    ///
    ///   kata:170 `Map[i64,i64]` (168 keys, `keys()` walk)  tag 1.13-1.17x SLOWER
    ///   kata:219 `Map[i64,i64]` (sliding window)           tag 1.03x SLOWER
    ///   kata:1   `Map[i64,i64]` (get-heavy, scaled)        tag 1.02-1.03x SLOWER
    ///   kata:146 `Map[i64,i64]` (LRU, tombstone churn)     neutral
    ///   kata:128 `Set[i64]`     (20k keys)                 neutral
    ///   kata:217 `Set[i64]`     (800-key windows)          neutral
    ///   kata:127 `Map[String,i64]`                         tag 1.07x FASTER
    ///
    /// So on arm64 the tag NEVER pays for a primitive key across six workloads
    /// and costs up to 1.17x, while it pays for a String key — whose skipped
    /// compare is a `{ptr,len,cap}` load plus a cold heap dereference, not an
    /// L1 hit.
    ///
    /// MEASURED, x86_64 (Intel Xeon @2.80GHz, 4 cores, Linux container), same
    /// `KARAC_MAP_TAG` A/B on one karac, strictly interleaved A/B/A/B, min and
    /// median of N, `KARAC_AUTO_PAR=0`, sinks verified, and an A/A control
    /// (one binary against a copy of itself) run FIRST at each workload length
    /// — 0.2%/0.02% at 3.9 s, 1.5%/0.8% at 3.6 s, 0.0%/0.4% at 6.8 s. Sign is
    /// stated as the cost of DROPPING the tag, i.e. positive = the tag pays:
    ///
    ///   kata:170 `Map[i64,i64]` (168 keys, `keys()` walk, 3.9 s)  +12.2%/+13.2%
    ///   kata:128 `Set[i64]`     (20k keys, scaled to 3.6 s)       +5.4%/+5.4%
    ///   kata:146 `Map[i64,i64]` (LRU, tombstone churn, 0.6 s)     +2.8%/+2.4%
    ///   kata:217 `Set[i64]`     (800-key windows, 6.8 s)          neutral
    ///   kata:219 `Map[i64,i64]` (sliding window, 0.8 s)           neutral
    ///
    /// So x86 is the MIRROR of arm64 on the same primitive-keyed sites: the tag
    /// never costs beyond the A/A control and pays up to 1.12x. On kata:170 the
    /// two distributions are disjoint (tag-on max 3971 ms < tag-off min 4331
    /// ms over 10 interleaved reps). B-2026-08-06-33 settled this; that is why
    /// this is arch-conditional and not a plain key-type gate, and deleting
    /// `!self.target_abi.target_is_aarch64` would cost x86 up to 12%.
    ///
    /// kata:170 is the outlier in MAGNITUDE on BOTH architectures, and on both
    /// its INSTRUCTION COUNT points away from the winner — in opposite
    /// directions, which is why counting instructions cannot decide this:
    ///
    ///   arm64  tag-on executes 12.8% FEWER instructions and still burns 18.5%
    ///          MORE cycles (IPC 3.04 -> 2.24). The tag LOSES.
    ///   x86    tag-on executes 15.1% MORE instructions (cachegrind, exact and
    ///          deterministic: 15.081 G I-refs vs 13.106 G) and still wins by
    ///          12%. D1 and LLd misses differ by ONE out of ~3,100 and ~2,360
    ///          either way — the ~168-entry table is L1-resident, so there is
    ///          no cache pressure for the tag to relieve.
    ///
    /// The x86 extra instructions are a SPILL, visible in the cachegrind D-ref
    /// split: over 201.6 M key-probes, tag-on does exactly 2.00 more writes per
    /// probe (403.2 M vs 42.8 K total) while tag-off does 1.24 more reads per
    /// probe — the kv load the tag would have skipped. So x86 pays two stores
    /// per probe for the tag and still comes out 12% ahead, because what the
    /// tag removes is a data-dependent load on the probe's CRITICAL PATH: the
    /// status byte alone decides whether to advance, so the loop never waits on
    /// the kv line. What the tag costs is a serialized compare against a
    /// computed operand. x86 values the shortened chain more; arm64 values the
    /// avoided compare more. Any future attempt to re-derive this policy from
    /// I-refs, or from a commit pair rather than from the flag, will get the
    /// sign wrong — both have already done so once (B-2026-08-06-33).
    ///
    /// Loose thread, not blocking: that 2-stores-per-probe spill is pure
    /// overhead on the arm that already wins. A spill-free tag would widen
    /// x86's margin, and might flip arm64's sign if part of what the tag costs
    /// there is the same spill rather than the compare.
    pub(super) fn map_tag_compare(&self, key: MapProbeKey) -> bool {
        if let Some(forced) = self.mapset.map_tag_override {
            return forced;
        }
        match key {
            MapProbeKey::Primitive => !self.target_abi.target_is_aarch64,
            MapProbeKey::HeapString => true,
        }
    }

    /// Mirror of the runtime's `ctrl_of`: the control byte for a bucket holding
    /// a key with this hash. Takes the TOP 7 bits because the bucket index
    /// consumes the low ones — a tag sharing them would be constant along a
    /// probe chain and would reject nothing.
    pub(super) fn emit_map_ctrl_of(&self, hash: IntValue<'ctx>) -> IntValue<'ctx> {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let shifted = self
            .builder
            .build_right_shift(hash, i64_t.const_int(57, false), false, "ctrl.sh")
            .unwrap();
        let narrowed = self
            .builder
            .build_int_truncate(shifted, i8_t, "ctrl.tr")
            .unwrap();
        let tag = self
            .builder
            .build_and(narrowed, i8_t.const_int(0x7f, false), "ctrl.tag")
            .unwrap();
        self.builder
            .build_or(
                tag,
                i8_t.const_int(Self::BUCKET_OCCUPIED_BIT, false),
                "ctrl",
            )
            .unwrap()
    }

    /// Mirror of the runtime's `is_occupied`: true for any control byte that is
    /// not EMPTY or TOMBSTONE. Both sentinels are below `0x80` and every
    /// occupied byte is at or above it, so this is one unsigned compare.
    pub(super) fn emit_map_is_occupied(
        &self,
        status_byte: IntValue<'ctx>,
        name: &str,
    ) -> IntValue<'ctx> {
        let i8_t = self.context.i8_type();
        self.builder
            .build_int_compare(
                IntPredicate::UGE,
                status_byte,
                i8_t.const_int(Self::BUCKET_OCCUPIED_BIT, false),
                name,
            )
            .unwrap()
    }

    /// Emit a LOOKUP probe's `probe.cond` — the cursor PHI plus, in
    /// [`MapLookupProbe::Bounded`], the `i >= cap` test — and leave the builder
    /// positioned at the head of `probe.body` with this iteration's bucket
    /// index computed. Returns the cursor every back-edge then advances with
    /// [`Codegen::advance_lookup_probe`].
    ///
    /// The three LOOKUP sites (`emit_mono_map_get_body`,
    /// `emit_mono_set_contains_body`, `emit_mono_map_str_get_body`) share this
    /// so the probe form is one edit rather than three kept in sync by hand.
    /// The two INSERT probes deliberately do NOT use it: they carry
    /// `first_tomb` / `ft_set` alongside the counter, and they must keep their
    /// bound because an insert probe walks to claim a bucket rather than to
    /// find an EMPTY one — the termination argument in [`MapLookupProbe`] is a
    /// lookup-only property.
    pub(super) fn emit_lookup_probe_cursor(
        &mut self,
        blocks: (BasicBlock<'ctx>, BasicBlock<'ctx>, BasicBlock<'ctx>),
        not_found_bb: BasicBlock<'ctx>,
        cap: IntValue<'ctx>,
        start: IntValue<'ctx>,
        mask: IntValue<'ctx>,
    ) -> (LookupProbeCursor<'ctx>, IntValue<'ctx>) {
        let (entry_bb, probe_cond_bb, probe_body_bb) = blocks;
        let i64_t = self.context.i64_type();
        let form = self.mapset.map_lookup_probe;

        self.builder.position_at_end(probe_cond_bb);
        let (phi, seed) = match form {
            // The cursor IS the slot, so it starts AT `start` rather than at
            // the zeroth step away from it.
            MapLookupProbe::SlotWalk => (self.builder.build_phi(i64_t, "slot.cur").unwrap(), start),
            _ => (
                self.builder.build_phi(i64_t, "i").unwrap(),
                i64_t.const_zero(),
            ),
        };
        phi.add_incoming(&[(&seed, entry_bb)]);
        let cur = phi.as_basic_value().into_int_value();

        match form {
            MapLookupProbe::Bounded => {
                let bound_done = self
                    .builder
                    .build_int_compare(IntPredicate::UGE, cur, cap, "bound.done")
                    .unwrap();
                self.builder
                    .build_conditional_branch(bound_done, not_found_bb, probe_body_bb)
                    .unwrap();
            }
            // No bound test. `not_found_bb` stays reachable — `probe.body`
            // branches to it on the EMPTY bucket the load factor guarantees,
            // which is the exit every terminating probe already took.
            MapLookupProbe::Unbounded | MapLookupProbe::SlotWalk => {
                self.builder
                    .build_unconditional_branch(probe_body_bb)
                    .unwrap();
            }
        }

        self.builder.position_at_end(probe_body_bb);
        let slot = match form {
            MapLookupProbe::SlotWalk => cur,
            _ => {
                let sum_si = self.builder.build_int_add(start, cur, "sum.si").unwrap();
                self.builder.build_and(sum_si, mask, "slot").unwrap()
            }
        };
        (LookupProbeCursor { phi, form, mask }, slot)
    }

    /// Register a back-edge into a LOOKUP probe: advance the cursor one bucket
    /// and feed it to the PHI as arriving from `from_bb`. Builds into whatever
    /// block the builder is currently positioned at, so call it where the old
    /// `i + 1` was built — before that block's terminator.
    pub(super) fn advance_lookup_probe(
        &mut self,
        cursor: &LookupProbeCursor<'ctx>,
        from_bb: BasicBlock<'ctx>,
        name: &str,
    ) {
        let i64_t = self.context.i64_type();
        let cur = cursor.phi.as_basic_value().into_int_value();
        let bumped = self
            .builder
            .build_int_add(cur, i64_t.const_int(1, false), name)
            .unwrap();
        let next = match cursor.form {
            // Wrap on the edge rather than at the use site, so the PHI always
            // holds a valid bucket index and `probe.body` can index with it
            // directly.
            MapLookupProbe::SlotWalk => self
                .builder
                .build_and(bumped, cursor.mask, "slot.next")
                .unwrap(),
            _ => bumped,
        };
        cursor.phi.add_incoming(&[(&next, from_bb)]);
    }

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
        if let Some(entry) = self.mapset.map_mono_methods.get(&cache_key) {
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
        self.mapset.map_mono_methods.insert(cache_key, methods);
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
                self.runtime_fns.karac_map_insert_old_fn,
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

        // B-2026-08-22-27 — load the Map's STORED `hash_fn` and call it
        // indirectly, exactly as the Set monos and the String-key Map get
        // already do. This body used to call a `karac_hash_<K>` symbol baked in
        // at emission time, on the reasoning (still in the git history) that it
        // "matches the existing erased path's hash exactly". That was true while
        // exactly one hasher existed; `Map[K, V, H]` falsified it silently.
        //
        // WHAT WENT WRONG. The hasher is a CONSTRUCTION-time decision: `Map.new`
        // synthesizes the hash fn for the declared `H` and stores the pointer in
        // the control block, and every erased-path operation — `contains_key`,
        // `get`, and crucially `resize`/`rehash_from` — calls through it. This
        // fast path called the DEFAULT hash instead, so a non-default map's
        // fast-path inserts filed keys under one hash while every lookup probed
        // under another.
        //
        // The failure had a distinctive shape worth recording, because it looks
        // like a resize bug and is not one: each resize takes the SLOW path,
        // which rehashes the whole table through the stored fn and so REPAIRS
        // every previously-misfiled key. Only the keys inserted since the last
        // resize stay lost — a contiguous tail beginning at exactly 3/4 of the
        // stalled capacity (196609 at N=200000, 393217 at 400000, 786433 at
        // 800000). `len` counts them, `contains_key` cannot find them.
        //
        // The hash is now the only indirect call in the probe; the eq and the
        // bucket walk stay inlined, which is where this family's win actually
        // comes from (the erased path pays an FFI boundary per operation).
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_HASH_FN_OFFSET, false)],
                    "hash.fn.pp",
                )
                .unwrap()
        };
        let hash_fn_ptr = self
            .builder
            .build_load(ptr_ty, hash_fn_pp, "hash.fn")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_indirect_call(hash_fn_ty, hash_fn_ptr, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        // The searched key's control byte (B-2026-07-26-2). A LOOKUP probe
        // compares the bucket byte against this directly — occupancy and hash
        // tag in one instruction; an INSERT probe stores it when claiming a
        // bucket. Loop-invariant, so it is hoisted here with `start`.
        let ctrl = self.emit_map_ctrl_of(hash);
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
        self.builder.build_store(target_status_p, ctrl).unwrap();
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
                self.runtime_fns.karac_map_insert_old_fn,
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
        // B-2026-08-22-27 — the read-side twin of the insert body's stored-hash
        // load. A `get` that probed with a different hash than `insert` filed
        // under would miss every key; both now read the one pointer the map was
        // constructed with.
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let hash_fn_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_HASH_FN_OFFSET, false)],
                    "hash.fn.pp",
                )
                .unwrap()
        };
        let hash_fn_ptr = self
            .builder
            .build_load(ptr_ty, hash_fn_pp, "hash.fn")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_indirect_call(hash_fn_ty, hash_fn_ptr, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        // The searched key's control byte (B-2026-07-26-2). A LOOKUP probe
        // compares the bucket byte against this directly — occupancy and hash
        // tag in one instruction; an INSERT probe stores it when claiming a
        // bucket. Loop-invariant, so it is hoisted here with `start`.
        // `None` when this probe tests occupancy ALONE (B-2026-08-05-5), so the
        // tag arithmetic is never emitted rather than emitted and DCE'd.
        let ctrl = self
            .map_tag_compare(MapProbeKey::Primitive)
            .then(|| self.emit_map_ctrl_of(hash));
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond + head of probe.body: cursor, then the slot ─
        // Both the cursor form and the presence of a bound test are
        // `KARAC_MAP_PROBE`'s call — see `MapLookupProbe` (B-2026-08-07-16).
        let (cursor, slot) = self.emit_lookup_probe_cursor(
            (entry_bb, probe_cond_bb, probe_body_bb),
            not_found_bb,
            cap,
            start,
            mask,
        );
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
        // Occupied AND the hash tag matches, in ONE compare. `ctrl >= 0x80`
        // and both sentinels are below it, so a tombstone or empty bucket can
        // never compare equal — and a bucket whose tag differs is rejected
        // WITHOUT touching its key, which is where the win is (a `String` key
        // costs a `{ptr,len,cap}` load plus a cold heap dereference). The
        // false-positive rate is ~1/128, so `eq` runs on real hits.
        //
        // `ctrl` is `None` when this probe's policy tests occupancy ALONE
        // (B-2026-08-05-5) — `map_tag_compare` carries the per-site
        // measurements. Correctness is unaffected either way: the OCCUPIED bit
        // is still what admits a bucket to `eq.check`, and dropping the tag
        // only lets more buckets through to a key compare that then rejects
        // them. The stored ENCODING never changes — insert probes write
        // `0x80 | tag7` on every host — so this stays layout-compatible with
        // `runtime/src/map.rs` and with archives built either way.
        let is_occupied = if let Some(ctrl) = ctrl {
            self.builder
                .build_int_compare(IntPredicate::EQ, status_byte, ctrl, "ctrl.match")
                .unwrap()
        } else {
            self.emit_map_is_occupied(status_byte, "ctrl.match")
        };
        // Tombstone path: advance the cursor, branch to probe.cond.
        self.advance_lookup_probe(&cursor, check_occupied_bb, "i.next.tomb");
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
        self.advance_lookup_probe(&cursor, eq_check_bb, "i.next.nomatch");
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

    /// Sibling of [`should_use_mono_map_for`] for Set membership: a Set lowers
    /// to a `val_size = 0` key-only map, so there is no value type to gate on —
    /// only the key must be an `i32`/`i64` scalar (identical hashing across the
    /// erased-insert and mono-contains paths, the same guarantee the Map mono
    /// path relies on).
    pub(super) fn should_use_mono_set_for(&self, key_ty: BasicTypeEnum<'ctx>) -> bool {
        let i32_t = self.context.i32_type();
        let i64_t = self.context.i64_type();
        matches!(key_ty, BasicTypeEnum::IntType(t) if t == i32_t || t == i64_t)
    }

    /// Set membership fast path: emit (or reuse) a monomorphized
    /// `karac_set_<K>_contains(map, key) -> bool` that inlines the linear probe
    /// and the key `icmp eq` directly against the runtime `KaracMap`, skipping
    /// the erased runtime's FFI boundary and its indirect per-probe `eq_fn`
    /// call. Semantically `karac_map_get(...).is_some()` with no value slot.
    /// Only valid for [`should_use_mono_set_for`] keys. The hash and the bucket
    /// stride are BOTH read from the map struct at runtime (the stored `hash_fn`
    /// pointer and the `val_size` field) rather than hardcoded, because Set
    /// instances are not uniform: `Set.new()` and `clone`/set-ops can register
    /// different hashes and different `val_size`s, and the probe must match how
    /// the buckets were actually filled.
    pub(super) fn get_or_emit_set_mono_contains(
        &mut self,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let mangle = self.llvm_type_to_mangle_str(key_ty);
        let fn_name = format!("karac_set_{mangle}_contains");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let saved_bb = self.builder.get_insert_block();
        let fn_ty = bool_t.fn_type(&[ptr_ty.into(), key_ty.into()], false);
        let f = self
            .module
            .add_function(&fn_name, fn_ty, Some(Linkage::LinkOnceODR));
        self.emit_mono_set_contains_body(f, key_ty);
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Body for [`get_or_emit_set_mono_contains`]. Mirrors
    /// [`emit_mono_map_get_body`] with two changes: no `out_val` out-param
    /// (Sets carry no value), and the bucket stride is `key_size` (`val_size =
    /// 0`). A match returns `true` without loading a value; a miss returns
    /// `false`.
    fn emit_mono_set_contains_body(&mut self, f: FunctionValue<'ctx>, key_ty: BasicTypeEnum<'ctx>) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let key_int_ty = key_ty.into_int_type();
        // Key occupies the first `key_size` bytes of each bucket. The bucket
        // STRIDE, however, is `key_size + val_size`, and `val_size` is NOT
        // constant across Set instances: `Set.new()` uses 0, but a cloned Set
        // carries an 8-byte unit value slot (see `emit_map_clone_fn`). So the
        // stride is read from the map's `val_size` field at runtime below.
        let key_size_bytes = (key_int_ty.get_bit_width() as u64).div_ceil(8);

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Signature of the runtime's stored `hash_fn`: `fn(*const key) -> u64`.
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);

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
        // Runtime bucket stride = key_size + val_size (val_size read from the
        // struct — 0 for a fresh Set, 8 for a cloned one).
        let val_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_VAL_SIZE_OFFSET, false)],
                    "val.size.p",
                )
                .unwrap()
        };
        let val_size = self
            .builder
            .build_load(i64_t, val_size_p, "val.size")
            .unwrap()
            .into_int_value();
        let stride = self
            .builder
            .build_int_add(
                i64_t.const_int(key_size_bytes, false),
                val_size,
                "bucket.stride",
            )
            .unwrap();
        // Load the Set's stored `hash_fn` pointer and call it indirectly — the
        // buckets were filled with this exact function (`Set.new` and
        // `clone`/set-ops may register different-but-self-consistent hashes), so
        // hardcoding one here would probe the wrong bucket for a Set whose
        // stored hash differs. The probe + key eq below stay inlined; only the
        // hash is an indirect call (as on the erased path too), and the win
        // comes from dropping the erased-runtime FFI boundary + inlining eq.
        let hash_fn_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_HASH_FN_OFFSET, false)],
                    "hash.fn.pp",
                )
                .unwrap()
        };
        let hash_fn_ptr = self
            .builder
            .build_load(ptr_ty, hash_fn_pp, "hash.fn")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_indirect_call(hash_fn_ty, hash_fn_ptr, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        // The searched key's control byte (B-2026-07-26-2). A LOOKUP probe
        // compares the bucket byte against this directly — occupancy and hash
        // tag in one instruction; an INSERT probe stores it when claiming a
        // bucket. Loop-invariant, so it is hoisted here with `start`.
        // `None` when this probe tests occupancy ALONE (B-2026-08-05-5), so the
        // tag arithmetic is never emitted rather than emitted and DCE'd.
        let ctrl = self
            .map_tag_compare(MapProbeKey::Primitive)
            .then(|| self.emit_map_ctrl_of(hash));
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond + head of probe.body: cursor, then the slot ─
        // Both the cursor form and the presence of a bound test are
        // `KARAC_MAP_PROBE`'s call — see `MapLookupProbe` (B-2026-08-07-16).
        let (cursor, slot) = self.emit_lookup_probe_cursor(
            (entry_bb, probe_cond_bb, probe_body_bb),
            not_found_bb,
            cap,
            start,
            mask,
        );
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
        // Occupied AND the hash tag matches, in ONE compare. `ctrl >= 0x80`
        // and both sentinels are below it, so a tombstone or empty bucket can
        // never compare equal — and a bucket whose tag differs is rejected
        // WITHOUT touching its key, which is where the win is (a `String` key
        // costs a `{ptr,len,cap}` load plus a cold heap dereference). The
        // false-positive rate is ~1/128, so `eq` runs on real hits.
        //
        // `ctrl` is `None` when this probe's policy tests occupancy ALONE
        // (B-2026-08-05-5) — `map_tag_compare` carries the per-site
        // measurements. Correctness is unaffected either way: the OCCUPIED bit
        // is still what admits a bucket to `eq.check`, and dropping the tag
        // only lets more buckets through to a key compare that then rejects
        // them. The stored ENCODING never changes — insert probes write
        // `0x80 | tag7` on every host — so this stays layout-compatible with
        // `runtime/src/map.rs` and with archives built either way.
        let is_occupied = if let Some(ctrl) = ctrl {
            self.builder
                .build_int_compare(IntPredicate::EQ, status_byte, ctrl, "ctrl.match")
                .unwrap()
        } else {
            self.emit_map_is_occupied(status_byte, "ctrl.match")
        };
        // Tombstone path: advance the cursor, branch to probe.cond.
        self.advance_lookup_probe(&cursor, check_occupied_bb, "i.next.tomb");
        self.builder
            .build_conditional_branch(is_occupied, eq_check_bb, probe_cond_bb)
            .unwrap();

        // ── eq.check: inline icmp eq on K key ────────────────────
        self.builder.position_at_end(eq_check_bb);
        let slot_off = self
            .builder
            .build_int_mul(slot, stride, "slot.off")
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
        self.advance_lookup_probe(&cursor, eq_check_bb, "i.next.nomatch");
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: return true (no value to load) ──────────
        self.builder.position_at_end(match_found_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── not.found: return false ──────────────────────────────
        self.builder.position_at_end(not_found_bb);
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();
    }

    /// Set insert fast path: emit (or reuse) a monomorphized
    /// `karac_set_<K>_insert(map, key) -> bool` (returns whether the key already
    /// existed, matching `karac_map_insert_old`'s flag). Mirrors
    /// [`emit_mono_map_insert_old_body`] with the value half removed: the bucket
    /// stores only the key, and the load-factor / exhausted slow paths forward
    /// to the erased `karac_map_insert_old` (with throwaway value / out slots,
    /// since the runtime copies `val_size` — 0 or 8 — bytes there). Like the Set
    /// contains path, the hash and the bucket stride are read from the map
    /// struct at runtime (stored `hash_fn`, `val_size`) so a cloned Set — which
    /// carries an 8-byte unit value slot — is probed with the right stride.
    /// Only valid for [`should_use_mono_set_for`] keys.
    pub(super) fn get_or_emit_set_mono_insert(
        &mut self,
        key_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let mangle = self.llvm_type_to_mangle_str(key_ty);
        let fn_name = format!("karac_set_{mangle}_insert");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let saved_bb = self.builder.get_insert_block();
        let fn_ty = bool_t.fn_type(&[ptr_ty.into(), key_ty.into()], false);
        let f = self
            .module
            .add_function(&fn_name, fn_ty, Some(Linkage::LinkOnceODR));
        self.emit_mono_set_insert_body(f, key_ty);
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Body for [`get_or_emit_set_mono_insert`].
    fn emit_mono_set_insert_body(&mut self, f: FunctionValue<'ctx>, key_ty: BasicTypeEnum<'ctx>) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let key_int_ty = key_ty.into_int_type();
        let key_size_bytes = (key_int_ty.get_bit_width() as u64).div_ceil(8);

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_int_value();
        // Runtime `hash_fn` signature: `fn(*const key) -> u64`.
        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);

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

        // Helper to forward one insert to the erased runtime (slow + exhausted
        // paths). Throwaway 8-byte val/out slots cover val_size ∈ {0, 8}.
        let erased_insert = |cg: &mut Self| {
            let key_slot = cg.builder.build_alloca(key_ty, "erased.key.slot").unwrap();
            let dummy_val = cg.builder.build_alloca(i64_t, "erased.val.slot").unwrap();
            let dummy_out = cg.builder.build_alloca(i64_t, "erased.out.slot").unwrap();
            cg.builder.build_store(key_slot, key_arg).unwrap();
            let existed = cg
                .builder
                .build_call(
                    cg.runtime_fns.karac_map_insert_old_fn,
                    &[
                        map_arg.into(),
                        key_slot.into(),
                        dummy_val.into(),
                        dummy_out.into(),
                    ],
                    "erased.existed",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic();
            cg.builder.build_return(Some(&existed)).unwrap();
        };

        // ── entry: load len/tombs/cap, load-factor check ──────────
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
        // (len + tombs + 1) * 4 > cap * 3 → resize (delegate to runtime).
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
        erased_insert(self);

        // ── fast_path: load ptrs, stride, hash, start ────────────
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
            .build_load(ptr_ty, status_pp, "status")
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
            .build_load(ptr_ty, kv_pp, "kv")
            .unwrap()
            .into_pointer_value();
        // stride = key_size + val_size (val_size read at runtime — 0 fresh, 8 cloned).
        let val_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_VAL_SIZE_OFFSET, false)],
                    "val.size.p",
                )
                .unwrap()
        };
        let val_size = self
            .builder
            .build_load(i64_t, val_size_p, "val.size")
            .unwrap()
            .into_int_value();
        let stride = self
            .builder
            .build_int_add(i64_t.const_int(key_size_bytes, false), val_size, "stride")
            .unwrap();
        // hash via the Set's stored hash_fn (see the contains path for why).
        let hash_fn_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_arg,
                    &[i64_t.const_int(Self::KARAC_MAP_HASH_FN_OFFSET, false)],
                    "hash.fn.pp",
                )
                .unwrap()
        };
        let hash_fn_ptr = self
            .builder
            .build_load(ptr_ty, hash_fn_pp, "hash.fn")
            .unwrap()
            .into_pointer_value();
        let hash_key_slot = self.builder.build_alloca(key_ty, "hash.key.slot").unwrap();
        self.builder.build_store(hash_key_slot, key_arg).unwrap();
        let hash = self
            .builder
            .build_indirect_call(hash_fn_ty, hash_fn_ptr, &[hash_key_slot.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        // The searched key's control byte (B-2026-07-26-2). A LOOKUP probe
        // compares the bucket byte against this directly — occupancy and hash
        // tag in one instruction; an INSERT probe stores it when claiming a
        // bucket. Loop-invariant, so it is hoisted here with `start`.
        let ctrl = self.emit_map_ctrl_of(hash);
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond: 3-PHI (i, first_tomb, ft_set), bound check ─
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

        // ── probe.body: slot, status, switch ──────────────────────
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

        // ── case.check_tomb ───────────────────────────────────────
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

        // ── case.empty: write key (no value), reuse earlier tomb ──
        self.builder.position_at_end(case_empty_bb);
        let target_slot = self
            .builder
            .build_select(ft_set_val, ft_val, slot, "target.slot")
            .unwrap()
            .into_int_value();
        let target_off = self
            .builder
            .build_int_mul(target_slot, stride, "target.off")
            .unwrap();
        let target_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[target_off], "target.kv.p")
                .unwrap()
        };
        self.builder.build_store(target_kv_p, key_arg).unwrap();
        let target_status_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, status_ptr, &[target_slot], "target.status.p")
                .unwrap()
        };
        self.builder.build_store(target_status_p, ctrl).unwrap();
        let new_len = self
            .builder
            .build_int_add(len, i64_t.const_int(1, false), "len.new")
            .unwrap();
        self.builder.build_store(len_p, new_len).unwrap();
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
        // Fresh insert → key did NOT already exist.
        self.builder
            .build_return(Some(&bool_t.const_zero()))
            .unwrap();

        // ── case.tomb: remember first tombstone, keep probing ─────
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

        // ── case.occupied: eq-check, found (dup) vs continue ─────
        self.builder.position_at_end(case_occupied_bb);
        let slot_off = self
            .builder
            .build_int_mul(slot, stride, "slot.off")
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
        i_phi.add_incoming(&[(&occ_i_next, case_occupied_bb)]);
        ft_phi.add_incoming(&[(&ft_val, case_occupied_bb)]);
        ft_set_phi.add_incoming(&[(&ft_set_val, case_occupied_bb)]);
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: key already present, nothing to write ───
        self.builder.position_at_end(match_found_bb);
        self.builder
            .build_return(Some(&bool_t.const_int(1, false)))
            .unwrap();

        // ── exhausted: unreachable under resize policy; erased safety
        self.builder.position_at_end(exhausted_bb);
        erased_insert(self);
    }

    /// Gate for the monomorphized **String-key** `Map` lookup
    /// (B-2026-07-26-2). Sibling of [`should_use_mono_map_for`], which covers
    /// scalar keys only, so a `Map[String, _]` used to fall all the way through
    /// to the erased `karac_map_get` FFI call.
    ///
    /// Why it is worth a separate path, measured rather than assumed. On a
    /// 3,125-key lookup loop run 60 times:
    ///
    /// * `Map[i64, i64]`, which already takes the scalar mono path, runs 2.2ms
    ///   against Rust's 2.1ms — **parity**. The mono probe is not the problem.
    /// * `Map[String, i64]`, which did not, runs 4.1ms net against Rust's
    ///   1.9ms — **2.16x**.
    /// * The difference is NOT the erased runtime's indirect `hash_fn` /
    ///   `eq_fn` calls, which is what the original bug report guessed.
    ///   Re-implementing `KaracMap`'s exact probe in Rust twice — once through
    ///   stored fn pointers, once with direct calls — measures 3.6ms vs 3.7ms,
    ///   a wash: the call target is identical every iteration, so it predicts
    ///   perfectly. What costs is the **FFI boundary itself** — an opaque call
    ///   per lookup that blocks inlining, plus the out-param protocol (store
    ///   the key into an alloca, hand over its address, reload the value).
    ///
    /// So this path inlines the probe loop but deliberately keeps calling the
    /// map's **stored** `hash_fn` / `eq_fn` rather than the ones this call site
    /// would synthesize. That mirrors [`emit_mono_set_contains_body`]'s
    /// existing rule and is the correctness invariant: `Map.new` and
    /// `clone`/map-ops may register different-but-self-consistent hashes for
    /// the same key type, and a probe that hashes differently from how the
    /// buckets were filled is a silent wrong-answer bug, not a slow one. Since
    /// the indirect call measured free, there is nothing to trade away.
    ///
    /// Gated on a scalar value type so the found-value load is one typed load;
    /// a compound V keeps the erased path.
    pub(super) fn should_use_mono_str_map_get(
        &self,
        key_te: &TypeExpr,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> bool {
        let key_name = Self::mangled_type_name(key_te);
        if key_name != "String" && key_name != "str" {
            return false;
        }
        matches!(
            val_ty,
            BasicTypeEnum::IntType(_) | BasicTypeEnum::FloatType(_) | BasicTypeEnum::PointerType(_)
        )
    }

    /// Emit (or reuse) `karac_map_str_<val>_get(map, key_ptr, out_val) -> bool`
    /// — the monomorphized String-key lookup gated by
    /// [`should_use_mono_str_map_get`]. `LinkOnceODR` so duplicates across
    /// translation units collapse at link time, matching the scalar family.
    ///
    /// Calling convention differs from the scalar mono `get` in one way: the
    /// key is passed **by pointer**, not by value. A `String` is a
    /// `{ptr, len, cap}` aggregate that the stored `hash_fn` / `eq_fn` already
    /// expect to receive by address, so passing the caller's existing key slot
    /// straight through avoids a copy.
    pub(super) fn get_or_emit_map_str_mono_get(
        &mut self,
        val_ty: BasicTypeEnum<'ctx>,
    ) -> FunctionValue<'ctx> {
        let val_mangle = self.llvm_type_to_mangle_str(val_ty);
        let fn_name = format!("karac_map_str_{val_mangle}_get");
        if let Some(f) = self.module.get_function(&fn_name) {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let bool_t = self.context.bool_type();
        let saved_bb = self.builder.get_insert_block();
        let fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into(), ptr_ty.into()], false);
        let f = self
            .module
            .add_function(&fn_name, fn_ty, Some(Linkage::LinkOnceODR));
        self.emit_mono_map_str_get_body(f, val_ty);
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        f
    }

    /// Body for [`get_or_emit_map_str_mono_get`]. Structurally the same probe
    /// as [`emit_mono_map_get_body`] — bound by `capacity`, stop on EMPTY, skip
    /// TOMBSTONE, compare on OCCUPIED — with three changes for a heap key:
    ///
    /// * `key_size` / `val_size` (and therefore the bucket stride and the
    ///   value's in-bucket offset) are **read from the map struct at runtime**
    ///   instead of being folded in as constants. The scalar path can fold them
    ///   because its K and V are fixed-width by construction; here it costs two
    ///   loads hoisted out of the loop and buys immunity to any creation path
    ///   that lays buckets out differently.
    /// * The key comparison is an indirect call to the map's stored `eq_fn`
    ///   rather than an inline `icmp eq` — a `String` compare is a length test
    ///   plus a `memcmp`, not a register compare.
    /// * The hash likewise comes from the stored `hash_fn`.
    ///
    /// See [`should_use_mono_str_map_get`] for why keeping both calls indirect
    /// costs nothing and why hashing any other way would be a correctness bug.
    fn emit_mono_map_str_get_body(&mut self, f: FunctionValue<'ctx>, val_ty: BasicTypeEnum<'ctx>) {
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let bool_t = self.context.bool_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let map_arg = f.get_nth_param(0).unwrap().into_pointer_value();
        let key_arg = f.get_nth_param(1).unwrap().into_pointer_value();
        let out_val_arg = f.get_nth_param(2).unwrap().into_pointer_value();

        let entry_bb = self.context.append_basic_block(f, "entry");
        let probe_cond_bb = self.context.append_basic_block(f, "probe.cond");
        let probe_body_bb = self.context.append_basic_block(f, "probe.body");
        let check_occupied_bb = self.context.append_basic_block(f, "check.occupied");
        let eq_check_bb = self.context.append_basic_block(f, "eq.check");
        let match_found_bb = self.context.append_basic_block(f, "match.found");
        let not_found_bb = self.context.append_basic_block(f, "not.found");

        // ── entry: hoist every map field the loop needs ──────────
        self.builder.position_at_end(entry_bb);
        let load_i64_field = |s: &Self, off: u64, name: &str| {
            let p = unsafe {
                s.builder
                    .build_in_bounds_gep(i8_t, map_arg, &[i64_t.const_int(off, false)], name)
                    .unwrap()
            };
            s.builder
                .build_load(i64_t, p, name)
                .unwrap()
                .into_int_value()
        };
        let load_ptr_field = |s: &Self, off: u64, name: &str| {
            let p = unsafe {
                s.builder
                    .build_in_bounds_gep(i8_t, map_arg, &[i64_t.const_int(off, false)], name)
                    .unwrap()
            };
            s.builder
                .build_load(ptr_ty, p, name)
                .unwrap()
                .into_pointer_value()
        };

        let cap = load_i64_field(self, Self::KARAC_MAP_CAPACITY_OFFSET, "cap");
        let status_ptr = load_ptr_field(self, Self::KARAC_MAP_STATUS_OFFSET, "status");
        let kv_ptr = load_ptr_field(self, Self::KARAC_MAP_KV_OFFSET, "kv");
        let key_size = load_i64_field(self, Self::KARAC_MAP_KEY_SIZE_OFFSET, "key.size");
        let val_size = load_i64_field(self, Self::KARAC_MAP_VAL_SIZE_OFFSET, "val.size");
        let hash_fn_ptr = load_ptr_field(self, Self::KARAC_MAP_HASH_FN_OFFSET, "hash.fn");
        let eq_fn_ptr = load_ptr_field(self, Self::KARAC_MAP_EQ_FN_OFFSET, "eq.fn");
        let kv_size = self
            .builder
            .build_int_add(key_size, val_size, "kv.size")
            .unwrap();

        let hash_fn_ty = i64_t.fn_type(&[ptr_ty.into()], false);
        let eq_fn_ty = bool_t.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);

        let hash = self
            .builder
            .build_indirect_call(hash_fn_ty, hash_fn_ptr, &[key_arg.into()], "hash")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        let mask = self
            .builder
            .build_int_sub(cap, i64_t.const_int(1, false), "mask")
            .unwrap();
        let start = self.builder.build_and(hash, mask, "start").unwrap();
        // The searched key's control byte (B-2026-07-26-2). A LOOKUP probe
        // compares the bucket byte against this directly — occupancy and hash
        // tag in one instruction; an INSERT probe stores it when claiming a
        // bucket. Loop-invariant, so it is hoisted here with `start`.
        // `Some` on every host here (B-2026-08-05-5): the tag is worth MORE for
        // a String key, not less — it skips a `{ptr,len,cap}` load and a cold
        // heap dereference, measured 1.07x on arm64 and +11.1% on x86. Only the
        // blunt `KARAC_MAP_TAG` A/B override can turn this one off.
        let ctrl = self
            .map_tag_compare(MapProbeKey::HeapString)
            .then(|| self.emit_map_ctrl_of(hash));
        self.builder
            .build_unconditional_branch(probe_cond_bb)
            .unwrap();

        // ── probe.cond + head of probe.body: cursor, then the slot ─
        // Both the cursor form and the presence of a bound test are
        // `KARAC_MAP_PROBE`'s call — see `MapLookupProbe` (B-2026-08-07-16).
        let (cursor, slot) = self.emit_lookup_probe_cursor(
            (entry_bb, probe_cond_bb, probe_body_bb),
            not_found_bb,
            cap,
            start,
            mask,
        );
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
        // Occupied AND the hash tag matches, in ONE compare. `ctrl >= 0x80`
        // and both sentinels are below it, so a tombstone or empty bucket can
        // never compare equal — and a bucket whose tag differs is rejected
        // WITHOUT touching its key, which is where the win is (a `String` key
        // costs a `{ptr,len,cap}` load plus a cold heap dereference). The
        // false-positive rate is ~1/128, so `eq` runs on real hits.
        //
        // `ctrl` is `None` when this probe's policy tests occupancy ALONE
        // (B-2026-08-05-5) — `map_tag_compare` carries the per-site
        // measurements. Correctness is unaffected either way: the OCCUPIED bit
        // is still what admits a bucket to `eq.check`, and dropping the tag
        // only lets more buckets through to a key compare that then rejects
        // them. The stored ENCODING never changes — insert probes write
        // `0x80 | tag7` on every host — so this stays layout-compatible with
        // `runtime/src/map.rs` and with archives built either way.
        let is_occupied = if let Some(ctrl) = ctrl {
            self.builder
                .build_int_compare(IntPredicate::EQ, status_byte, ctrl, "ctrl.match")
                .unwrap()
        } else {
            self.emit_map_is_occupied(status_byte, "ctrl.match")
        };
        // Tombstone path: advance the cursor, branch to probe.cond.
        self.advance_lookup_probe(&cursor, check_occupied_bb, "i.next.tomb");
        self.builder
            .build_conditional_branch(is_occupied, eq_check_bb, probe_cond_bb)
            .unwrap();

        // ── eq.check: stored eq_fn against the bucket's key half ──
        self.builder.position_at_end(eq_check_bb);
        let slot_off = self
            .builder
            .build_int_mul(slot, kv_size, "slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "slot.kv.p")
                .unwrap()
        };
        let key_match = self
            .builder
            .build_indirect_call(
                eq_fn_ty,
                eq_fn_ptr,
                &[slot_kv_p.into(), key_arg.into()],
                "key.match",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.advance_lookup_probe(&cursor, eq_check_bb, "i.next.nomatch");
        self.builder
            .build_conditional_branch(key_match, match_found_bb, probe_cond_bb)
            .unwrap();

        // ── match.found: load val, write out, return true ────────
        self.builder.position_at_end(match_found_bb);
        let slot_val_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, slot_kv_p, &[key_size], "slot.val.p")
                .unwrap()
        };
        let val = self.builder.build_load(val_ty, slot_val_p, "val").unwrap();
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
