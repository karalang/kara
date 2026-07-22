//! LazyFrame / LazyExpr / LazyGroupBy codegen — the compiled half of the
//! phase-11 LazyDataFrame twin (`runtime/src/lazy.rs` is the runtime half;
//! read its module doc first).
//!
//! Compiled code builds plans at RUNTIME through the `karac_lazy_*` FFI.
//! Two ABI facts anchor everything here:
//!
//! * **Handles are refcounted, borrow-everywhere.** A `LazyFrame` /
//!   `LazyExpr` value in compiled code is the Kāra POD struct
//!   `{ handle_id: i64 }` with field 0 holding the runtime `Arc` handle
//!   pointer (`ptr_to_int`). Every `karac_lazy_*` ARGUMENT borrows (the
//!   runtime clones internally); every constructor/builder returns a
//!   fresh +1 handle. Codegen's ownership model is
//!   **release-everywhere + retain-on-return**:
//!   1. every lowering site that receives a fresh handle stores it in an
//!      entry alloca and pushes `ReleaseLazyExpr` / `ReleaseLazyPlan`
//!      onto the CURRENT scope frame (`track_lazy_expr_handle` /
//!      `track_lazy_plan_handle`) — released once at the producing
//!      scope's exit;
//!   2. a user fn whose DECLARED return type is `LazyExpr` / `LazyFrame`
//!      (the `std.lazy` col/lit wrappers) emits `karac_lazy_expr_retain`
//!      / `karac_lazy_retain` on the returned handle immediately BEFORE
//!      its scope drains (`emit_lazy_retain_for_return`, hooked into all
//!      three return funnels: explicit `return` in `exprs.rs`, the
//!      non-generic tail in `functions.rs`, the mono tail in `mono.rs`);
//!   3. the CALLER of such a fn registers the escaping +1 exactly like a
//!      direct production (`register_lazy_user_call_result`, hooked at
//!      the `compile_call` / `compile_generic_call` result sites).
//!
//!   User IMPL METHODS declared to return a Lazy value are NOT wired into
//!   the caller-side registration (method dispatch has no single result
//!   funnel) — they bail loudly instead (see `try_compile_lazy_method`),
//!   and a CLOSURE returning a Lazy value is an undetected v1 limitation
//!   (the handle would be released at closure-scope exit — run those with
//!   `karac run`).
//! * **Dispatch keys off the receiver's static Lazy type**, recovered by
//!   the recursive `lazy_type_of_expr` classifier — NOT solely off the
//!   span-keyed `method_callee_types` table, because the parser aliases a
//!   chained call's `MethodCall.span` to its receiver's span, so every
//!   link of `df.lazy().filter(..).select(..).limit(2)` shares ONE table
//!   key (last insert wins; see the collision note in `method_call.rs`).
//!   The table remains a method-segment-checked fallback for receiver
//!   shapes the classifier can't name.
//!
//! v1 twin scope: `select` / `limit` / `filter` / `collect` / `explain` +
//! the full filter expression surface (col/lit, cmp, and_/or_/not_,
//! add/sub/mul/div). Everything else (sort / group_by / join /
//! with_columns / the aggregates / alias_ / desc / LazyGroupBy.agg) bails
//! loudly per method with a `karac run` pointer.

use crate::ast::*;

use inkwell::values::{BasicValueEnum, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

/// Tracker pointer shared by every loud Lazy bail.
const LAZY_TRACKER: &str = "phase-11-stdlib-longtail.md \u{a7} LazyDataFrame";

/// The loud per-method bail for a Lazy method the v1 twin does not lower.
fn lazy_unsupported(recv: &str, method: &str) -> String {
    format!(
        "{recv}.{method} is not yet lowered by the v1 codegen twin — run it with \
         `karac run` (tracker: {LAZY_TRACKER})"
    )
}

impl<'ctx> super::Codegen<'ctx> {
    // ── Type classification ─────────────────────────────────────

    /// The Lazy kind (`"LazyFrame"` / `"LazyExpr"` / `"LazyGroupBy"`) a
    /// declared type expression names, if any. Used for retain-on-return
    /// (rule 2 above) against a fn's declared return type.
    pub(super) fn lazy_kind_of_type_expr(te: &TypeExpr) -> Option<&'static str> {
        if let TypeKind::Path(p) = &te.kind {
            return match p.segments.first().map(|s| s.as_str()) {
                Some("LazyFrame") => Some("LazyFrame"),
                Some("LazyExpr") => Some("LazyExpr"),
                _ => None,
            };
        }
        None
    }

    /// The Lazy kind a NAMED fn is declared to return: `fn_return_type_names`
    /// for concrete fns / impl methods, `generic_fns` for generic fns (their
    /// monos keep the source name as `current_fn_name`). `None` for
    /// everything else — including closures (v1 limitation, see module doc).
    pub(super) fn declared_lazy_return_of_fn(&self, name: &str) -> Option<&'static str> {
        if let Some(ret) = self.fn_return_type_names.get(name) {
            return match ret.as_str() {
                "LazyFrame" => Some("LazyFrame"),
                "LazyExpr" => Some("LazyExpr"),
                _ => None,
            };
        }
        self.generic_fns
            .get(name)
            .and_then(|f| f.return_type.as_ref())
            .and_then(Self::lazy_kind_of_type_expr)
    }

    /// The Lazy kind the CURRENT function is declared to return, keyed by
    /// `current_fn_name` (the source name for both concrete fns and monos).
    pub(super) fn current_fn_lazy_return_kind(&self) -> Option<&'static str> {
        self.declared_lazy_return_of_fn(&self.current_fn_name)
    }

    /// Recursive static classifier: the Lazy kind `e` evaluates to, or
    /// `None` for a non-Lazy expression. This is the dispatch key for
    /// `try_compile_lazy_method` — span-collision-immune (see module doc).
    pub(super) fn lazy_type_of_expr(&self, e: &Expr) -> Option<&'static str> {
        match &e.kind {
            ExprKind::Identifier(_) | ExprKind::SelfValue | ExprKind::FieldAccess { .. } => {
                match self.type_name_of_expr(e)?.as_str() {
                    "LazyFrame" => Some("LazyFrame"),
                    "LazyExpr" => Some("LazyExpr"),
                    "LazyGroupBy" => Some("LazyGroupBy"),
                    _ => None,
                }
            }
            ExprKind::MethodCall { object, method, .. } => {
                // `df.lazy()` — the single entry point from the eager world.
                if method == "lazy" {
                    let is_df = match &object.kind {
                        ExprKind::Identifier(n) => self.dataframe_var_infos.contains(n.as_str()),
                        _ => self.type_name_of_expr(object).as_deref() == Some("DataFrame"),
                    };
                    if is_df {
                        return Some("LazyFrame");
                    }
                }
                match self.lazy_type_of_expr(object)? {
                    "LazyFrame" => match method.as_str() {
                        "select" | "limit" | "filter" | "sort" | "join" | "with_columns" => {
                            Some("LazyFrame")
                        }
                        "group_by" => Some("LazyGroupBy"),
                        _ => None,
                    },
                    "LazyExpr" => match method.as_str() {
                        "gt" | "ge" | "lt" | "le" | "eq" | "ne" | "and_" | "or_" | "not_"
                        | "add" | "sub" | "mul" | "div" | "count" | "sum" | "mean" | "min"
                        | "max" | "alias_" | "desc" => Some("LazyExpr"),
                        _ => None,
                    },
                    "LazyGroupBy" => (method == "agg").then_some("LazyFrame"),
                    _ => None,
                }
            }
            ExprKind::Call { callee, .. } => match &callee.kind {
                // `LazyExpr.col(..)` / `LazyExpr.lit(..)` path constructors.
                ExprKind::Path { segments, .. }
                    if segments.len() == 2
                        && segments[0] == "LazyExpr"
                        && matches!(segments[1].as_str(), "col" | "lit") =>
                {
                    Some("LazyExpr")
                }
                // A user free fn declared to return a Lazy value — the
                // `std.lazy` col/lit wrappers and user helpers.
                ExprKind::Identifier(f) => self.declared_lazy_return_of_fn(f),
                _ => None,
            },
            _ => None,
        }
    }

    // ── Handle plumbing ─────────────────────────────────────────

    /// The LLVM struct type of a Lazy value: the seeded `{ i64 }` POD
    /// layout (`declarations.rs` seeds all three names; the literal
    /// fallback is structurally identical, so mixing is safe).
    fn lazy_struct_type(&self, name: &str) -> inkwell::types::StructType<'ctx> {
        self.struct_types.get(name).copied().unwrap_or_else(|| {
            self.context
                .struct_type(&[self.context.i64_type().into()], false)
        })
    }

    /// Extract the runtime handle pointer from a compiled Lazy value
    /// (field 0 of the `{ i64 }` struct, `int_to_ptr`).
    pub(super) fn lazy_handle_of_value(
        &self,
        v: BasicValueEnum<'ctx>,
        what: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let BasicValueEnum::StructValue(sv) = v else {
            return Err(format!(
                "codegen: {what} did not lower to the {{ handle_id: i64 }} Lazy value shape \
                 (got {:?}) — this is a codegen bug in the LazyFrame twin",
                v.get_type()
            ));
        };
        let word = self
            .builder
            .build_extract_value(sv, 0, "lz.handle.word")
            .unwrap()
            .into_int_value();
        Ok(self
            .builder
            .build_int_to_ptr(
                word,
                self.context.ptr_type(AddressSpace::default()),
                "lz.handle",
            )
            .unwrap())
    }

    /// Wrap a runtime handle pointer back into the Kāra `{ i64 }` POD value.
    pub(super) fn build_lazy_struct_value(
        &self,
        type_name: &str,
        handle: PointerValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let word = self
            .builder
            .build_ptr_to_int(handle, self.context.i64_type(), "lz.handle.int")
            .unwrap();
        let st = self.lazy_struct_type(type_name);
        self.builder
            .build_insert_value(st.get_undef(), word, 0, "lz.value")
            .unwrap()
            .into_struct_value()
            .into()
    }

    /// Rule-1 registration: store a fresh +1 `LazyExpr` handle in an entry
    /// alloca and queue its release on the CURRENT scope frame. Mirrors
    /// `track_file_var` / `track_dataframe_var`.
    pub(super) fn track_lazy_expr_handle(&mut self, handle: PointerValue<'ctx>) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let alloca = self.create_entry_alloca(fn_val, "lz.expr.slot", ptr_ty.into());
        self.builder.build_store(alloca, handle).unwrap();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::ReleaseLazyExpr { alloca });
        }
    }

    /// The plan-handle sibling of [`Self::track_lazy_expr_handle`].
    pub(super) fn track_lazy_plan_handle(&mut self, handle: PointerValue<'ctx>) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let alloca = self.create_entry_alloca(fn_val, "lz.plan.slot", ptr_ty.into());
        self.builder.build_store(alloca, handle).unwrap();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(super::state::CleanupAction::ReleaseLazyPlan { alloca });
        }
    }

    /// Rule-2 retain-on-return: bump the returned handle's count so it
    /// survives the producing scope's release drain. Emitted immediately
    /// BEFORE the return-path scope cleanup at all three return funnels.
    pub(super) fn emit_lazy_retain_for_return(
        &mut self,
        kind: &'static str,
        val: BasicValueEnum<'ctx>,
    ) {
        // A non-struct value here means the body produced something the
        // twin doesn't model (e.g. a diverging tail) — skip quietly; the
        // typechecker has already validated the return type.
        let Ok(handle) = self.lazy_handle_of_value(val, "return value") else {
            return;
        };
        let fname = if kind == "LazyExpr" {
            "karac_lazy_expr_retain"
        } else {
            "karac_lazy_retain"
        };
        let f = self
            .module
            .get_function(fname)
            .unwrap_or_else(|| panic!("{fname} declared in Codegen::new"));
        self.builder.build_call(f, &[handle.into()], "").unwrap();
    }

    /// Rule-3 caller-side registration: when `callee_name` is declared to
    /// return `LazyExpr` / `LazyFrame`, the call result carries an escaping
    /// +1 (retained in the callee) — queue its release in THIS scope.
    /// Hooked at the `compile_call` / `compile_generic_call` result sites;
    /// a no-op for every other callee.
    pub(super) fn register_lazy_user_call_result(
        &mut self,
        callee_name: &str,
        val: BasicValueEnum<'ctx>,
    ) {
        let Some(kind) = self.declared_lazy_return_of_fn(callee_name) else {
            return;
        };
        let Ok(handle) = self.lazy_handle_of_value(val, "call result") else {
            return;
        };
        if kind == "LazyExpr" {
            self.track_lazy_expr_handle(handle);
        } else {
            self.track_lazy_plan_handle(handle);
        }
    }

    // ── Expression-argument lowering ────────────────────────────

    /// Lower a scalar (i64 / f64 / String / bool) to a fresh, tracked
    /// `karac_lazy_expr_lit_*` handle — the wrap for `LazyExpr.lit(..)`
    /// and for bare-scalar comparison/arithmetic RHS positions.
    fn lazy_lit_handle_for_value(
        &mut self,
        v: BasicValueEnum<'ctx>,
        what: &str,
    ) -> Result<PointerValue<'ctx>, String> {
        let i64_t = self.context.i64_type();
        let handle = match v {
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => {
                // bool → lit_bool(i8 0/1).
                let b = self
                    .builder
                    .build_int_z_extend(iv, self.context.i8_type(), "lz.lit.b")
                    .unwrap();
                let f = self
                    .module
                    .get_function("karac_lazy_expr_lit_bool")
                    .expect("karac_lazy_expr_lit_bool declared in Codegen::new");
                self.builder
                    .build_call(f, &[b.into()], "lz.lit.bool")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value()
            }
            BasicValueEnum::IntValue(iv) => {
                let wide = if iv.get_type().get_bit_width() < 64 {
                    self.builder
                        .build_int_s_extend(iv, i64_t, "lz.lit.i")
                        .unwrap()
                } else {
                    iv
                };
                let f = self
                    .module
                    .get_function("karac_lazy_expr_lit_int")
                    .expect("karac_lazy_expr_lit_int declared in Codegen::new");
                self.builder
                    .build_call(f, &[wide.into()], "lz.lit.int")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value()
            }
            BasicValueEnum::FloatValue(fv) => {
                let f64_t = self.context.f64_type();
                let wide = if fv.get_type() != f64_t {
                    self.builder.build_float_ext(fv, f64_t, "lz.lit.f").unwrap()
                } else {
                    fv
                };
                let f = self
                    .module
                    .get_function("karac_lazy_expr_lit_float")
                    .expect("karac_lazy_expr_lit_float declared in Codegen::new");
                self.builder
                    .build_call(f, &[wide.into()], "lz.lit.float")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value()
            }
            BasicValueEnum::StructValue(sv) if self.llvm_ty_is_vec_struct(v.get_type()) => {
                // String `{ptr, len, cap}` — the runtime copies the bytes.
                let (data, len) = self.str_data_len(sv);
                let f = self
                    .module
                    .get_function("karac_lazy_expr_lit_str")
                    .expect("karac_lazy_expr_lit_str declared in Codegen::new");
                self.builder
                    .build_call(f, &[data.into(), len.into()], "lz.lit.str")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value()
            }
            _ => {
                return Err(format!(
                    "{what} accepts i64 / f64 / String / bool (or another LazyExpr in \
                     comparison/arithmetic position); got a value of LLVM type {:?} \
                     (tracker: {LAZY_TRACKER})",
                    v.get_type()
                ));
            }
        };
        // The wrap is a +1 production too — release it at this scope's exit.
        self.track_lazy_expr_handle(handle);
        Ok(handle)
    }

    /// Lower a comparison / boolean / arithmetic RHS argument to a BORROWED
    /// expr handle: an expression that statically types as `LazyExpr`
    /// compiles to its (already-tracked) handle; anything else compiles as
    /// a scalar and wraps through a tracked `lit_*` handle.
    fn lazy_rhs_handle(&mut self, arg: &Expr, what: &str) -> Result<PointerValue<'ctx>, String> {
        let is_expr = self.lazy_type_of_expr(arg) == Some("LazyExpr")
            || self.type_name_of_expr(arg).as_deref() == Some("LazyExpr");
        let v = self.compile_expr(arg)?;
        if is_expr {
            return self.lazy_handle_of_value(v, what);
        }
        // A `{ i64 }` one-field struct that the classifier could not name is
        // still a Lazy value in every shape the typechecker admits here
        // (match/block-tail LazyExpr results); treat it as a handle rather
        // than erroring on the wrap path.
        if let BasicValueEnum::StructValue(sv) = v {
            let st = sv.get_type();
            if st.count_fields() == 1
                && st
                    .get_field_type_at_index(0)
                    .is_some_and(|t| t.is_int_type())
                && !self.llvm_ty_is_vec_struct(v.get_type())
            {
                return self.lazy_handle_of_value(v, what);
            }
        }
        self.lazy_lit_handle_for_value(v, what)
    }

    // ── Path constructors (`assoc_call.rs` delegates here) ─────

    /// `LazyExpr.col(name)` — build a tracked column-reference handle and
    /// return it as a `LazyExpr` value.
    pub(super) fn compile_lazy_expr_col(
        &mut self,
        arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let v = self.compile_expr(arg)?;
        let BasicValueEnum::StructValue(sv) = v else {
            return Err("LazyExpr.col expects a String column name".to_string());
        };
        let (data, len) = self.str_data_len(sv);
        let f = self
            .module
            .get_function("karac_lazy_expr_col")
            .expect("karac_lazy_expr_col declared in Codegen::new");
        let handle = self
            .builder
            .build_call(f, &[data.into(), len.into()], "lz.col")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.track_lazy_expr_handle(handle);
        Ok(self.build_lazy_struct_value("LazyExpr", handle))
    }

    /// `LazyExpr.lit(v)` — classify the argument (int / float / String /
    /// bool) and build the matching tracked literal handle.
    pub(super) fn compile_lazy_expr_lit(
        &mut self,
        arg: &Expr,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let v = self.compile_expr(arg)?;
        let handle = self.lazy_lit_handle_for_value(v, "LazyExpr.lit")?;
        Ok(self.build_lazy_struct_value("LazyExpr", handle))
    }

    // ── Method dispatch ─────────────────────────────────────────

    /// Early `compile_method_call` hook: lower a method whose receiver is a
    /// Lazy value. `Ok(None)` for non-Lazy receivers (normal dispatch
    /// continues); loud `Err` for Lazy methods outside the v1 twin and for
    /// user methods declared to return a Lazy value (no caller-side
    /// release registration exists for method calls — see module doc).
    pub(super) fn try_compile_lazy_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
        call_span: &crate::token::Span,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let span_key = (call_span.offset, call_span.length);
        // Span-keyed fallback, trusted only when the entry's method segment
        // matches THIS call (chained calls share one aliased key — see the
        // collision note in `method_call.rs`).
        let table_kind = || -> Option<&'static str> {
            let k = self.method_callee_types.get(&span_key)?;
            let (ty, m) = k.rsplit_once('.')?;
            if m != method {
                return None;
            }
            match ty {
                "LazyFrame" => Some("LazyFrame"),
                "LazyExpr" => Some("LazyExpr"),
                "LazyGroupBy" => Some("LazyGroupBy"),
                _ => None,
            }
        };
        let Some(kind) = self.lazy_type_of_expr(object).or_else(table_kind) else {
            // Not a Lazy receiver. One loud guard before falling through:
            // a USER method declared to return a Lazy value has no
            // caller-side release registration (rule 3 only covers free-fn
            // calls), so the escaping +1 would leak silently — bail.
            if let Some(k) = self.method_callee_types.get(&span_key) {
                let matches_this_call = k.rsplit_once('.').is_some_and(|(_, m)| m == method);
                if matches_this_call && self.declared_lazy_return_of_fn(k).is_some() {
                    return Err(format!(
                        "returning LazyExpr/LazyFrame from user methods (`{k}`) is not yet \
                         lowered by the v1 codegen twin — run it with `karac run` \
                         (tracker: {LAZY_TRACKER})"
                    ));
                }
            }
            return Ok(None);
        };

        match kind {
            "LazyFrame" => self.compile_lazy_frame_method(object, method, args),
            "LazyExpr" => self.compile_lazy_expr_method(object, method, args),
            "LazyGroupBy" => {
                if method == "agg" {
                    return Err(lazy_unsupported("LazyGroupBy", "agg"));
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Lower the supported `LazyFrame` plan methods; loud bail for the rest.
    fn compile_lazy_frame_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        match method {
            "select" | "limit" | "filter" | "collect" | "explain" => {}
            "sort" | "group_by" | "join" | "with_columns" => {
                return Err(lazy_unsupported("LazyFrame", method));
            }
            _ => {
                // A user-extension method on LazyFrame: reject a Lazy-returning
                // one (leak — see module doc); let the rest dispatch normally.
                let qualified = format!("LazyFrame.{method}");
                if self.declared_lazy_return_of_fn(&qualified).is_some() {
                    return Err(format!(
                        "returning LazyExpr/LazyFrame from user methods (`{qualified}`) is \
                         not yet lowered by the v1 codegen twin — run it with `karac run` \
                         (tracker: {LAZY_TRACKER})"
                    ));
                }
                return Ok(None);
            }
        }

        let recv_v = self.compile_expr(object)?;
        let recv = self.lazy_handle_of_value(recv_v, "LazyFrame receiver")?;
        let i64_t = self.context.i64_type();

        match method {
            "select" => {
                let arg = args
                    .first()
                    .ok_or("LazyFrame.select expects one Vec[String] argument")?;
                let cols_val = self.compile_expr(&arg.value)?;
                // A fresh owned `Vec[String]` arg (a vec literal / fresh call
                // result) has no consuming binding — route it through the
                // owned-temp chokepoint so buffer + element Strings free
                // exactly once at scope exit (mirrors
                // `compile_dataframe_select`; the runtime copies the names
                // during the call, before that free runs).
                let cols_is_fresh = self.expr_yields_fresh_owned_temp(&arg.value)
                    || matches!(&arg.value.kind, ExprKind::PrefixCollectionLiteral { .. });
                if cols_is_fresh && self.llvm_ty_is_vec_struct(cols_val.get_type()) {
                    self.materialize_owned_temp(
                        cols_val,
                        (arg.value.span.offset, arg.value.span.length),
                    );
                }
                // Read data ptr (field 0) + len (field 1) via scalar
                // `struct_gep` loads off a spill alloca — NOT `extractvalue`
                // on the 24-byte aggregate load, which mis-lowers the pointer
                // field to null under ASan on arm64-Linux (see the proven
                // pattern + rationale in `stats.rs`).
                let sv = cols_val.into_struct_value();
                let vec_ty = sv.get_type();
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let spill = self.builder.build_alloca(vec_ty, "lz.sel.arg").unwrap();
                self.builder.build_store(spill, sv).unwrap();
                let data_field = self
                    .builder
                    .build_struct_gep(vec_ty, spill, 0, "lz.sel.data.p")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_field, "lz.sel.data")
                    .unwrap()
                    .into_pointer_value();
                let len_field = self
                    .builder
                    .build_struct_gep(vec_ty, spill, 1, "lz.sel.len.p")
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_field, "lz.sel.len")
                    .unwrap()
                    .into_int_value();
                let f = self
                    .module
                    .get_function("karac_lazy_select")
                    .expect("karac_lazy_select declared in Codegen::new");
                let out = self
                    .builder
                    .build_call(f, &[recv.into(), data.into(), len.into()], "lz.select")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.track_lazy_plan_handle(out);
                Ok(Some(self.build_lazy_struct_value("LazyFrame", out)))
            }
            "limit" => {
                let arg = args
                    .first()
                    .ok_or("LazyFrame.limit expects one i64 argument")?;
                let mut n = self.compile_expr(&arg.value)?.into_int_value();
                if n.get_type().get_bit_width() < 64 {
                    n = self
                        .builder
                        .build_int_s_extend(n, i64_t, "lz.limit.n")
                        .unwrap();
                }
                let f = self
                    .module
                    .get_function("karac_lazy_limit")
                    .expect("karac_lazy_limit declared in Codegen::new");
                let out = self
                    .builder
                    .build_call(f, &[recv.into(), n.into()], "lz.limit")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.track_lazy_plan_handle(out);
                Ok(Some(self.build_lazy_struct_value("LazyFrame", out)))
            }
            "filter" => {
                let arg = args
                    .first()
                    .ok_or("LazyFrame.filter expects one LazyExpr predicate argument")?;
                let pred_v = self.compile_expr(&arg.value)?;
                let pred = self.lazy_handle_of_value(pred_v, "LazyFrame.filter predicate")?;
                let f = self
                    .module
                    .get_function("karac_lazy_filter")
                    .expect("karac_lazy_filter declared in Codegen::new");
                let out = self
                    .builder
                    .build_call(f, &[recv.into(), pred.into()], "lz.filter")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                self.track_lazy_plan_handle(out);
                Ok(Some(self.build_lazy_struct_value("LazyFrame", out)))
            }
            "explain" => {
                // Malloc'd-buffer String adoption, byte-for-byte the
                // `karac_regex_replace_all` convention (`method_call.rs`):
                // out-len slot, call, `cap = max(len, 1)` (the runtime always
                // allocated `max(len, 1)` bytes), adopt as an owned String.
                let fn_val = self.current_fn.unwrap();
                let len_slot = self.create_entry_alloca(fn_val, "lz.explain.len", i64_t.into());
                self.builder
                    .build_store(len_slot, i64_t.const_zero())
                    .unwrap();
                let f = self
                    .module
                    .get_function("karac_lazy_explain")
                    .expect("karac_lazy_explain declared in Codegen::new");
                let ptr = self
                    .builder
                    .build_call(f, &[recv.into(), len_slot.into()], "lz.explain")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_load(i64_t, len_slot, "lz.explain.len.v")
                    .unwrap()
                    .into_int_value();
                let one = i64_t.const_int(1, false);
                let len_pos = self
                    .builder
                    .build_int_compare(IntPredicate::UGT, len, i64_t.const_zero(), "lz.ex.pos")
                    .unwrap();
                let cap = self
                    .builder
                    .build_select(len_pos, len, one, "lz.ex.cap")
                    .unwrap()
                    .into_int_value();
                Ok(Some(self.build_vec_value(ptr, len, cap)))
            }
            "collect" => {
                // A fresh malloc-compatible DataFrame control block — the
                // ordinary `FreeDataFrame` binding path owns it (the let-stmt
                // tracker keys off the typechecker's `DataFrame` binding
                // type and the pointer-typed slot, exactly like the eager
                // DataFrame-returning methods).
                let f = self
                    .module
                    .get_function("karac_lazy_collect")
                    .expect("karac_lazy_collect declared in Codegen::new");
                let out = self
                    .builder
                    .build_call(f, &[recv.into()], "lz.collect")
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_pointer_value();
                Ok(Some(out.into()))
            }
            _ => unreachable!("gated above"),
        }
    }

    /// Lower the supported `LazyExpr` builder methods; loud bail for the
    /// aggregate / sort-marker family.
    fn compile_lazy_expr_method(
        &mut self,
        object: &Expr,
        method: &str,
        args: &[CallArg],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let cmp_op: Option<u64> = match method {
            "gt" => Some(0),
            "ge" => Some(1),
            "lt" => Some(2),
            "le" => Some(3),
            "eq" => Some(4),
            "ne" => Some(5),
            _ => None,
        };
        let bool_op: Option<u64> = match method {
            "and_" => Some(0),
            "or_" => Some(1),
            _ => None,
        };
        let arith_op: Option<u64> = match method {
            "add" => Some(0),
            "sub" => Some(1),
            "mul" => Some(2),
            "div" => Some(3),
            _ => None,
        };
        if cmp_op.is_none() && bool_op.is_none() && arith_op.is_none() && method != "not_" {
            return match method {
                "count" | "sum" | "mean" | "min" | "max" | "alias_" | "desc" => {
                    Err(lazy_unsupported("LazyExpr", method))
                }
                _ => {
                    let qualified = format!("LazyExpr.{method}");
                    if self.declared_lazy_return_of_fn(&qualified).is_some() {
                        return Err(format!(
                            "returning LazyExpr/LazyFrame from user methods (`{qualified}`) \
                             is not yet lowered by the v1 codegen twin — run it with \
                             `karac run` (tracker: {LAZY_TRACKER})"
                        ));
                    }
                    Ok(None)
                }
            };
        }

        let recv_v = self.compile_expr(object)?;
        let recv = self.lazy_handle_of_value(recv_v, "LazyExpr receiver")?;
        let i64_t = self.context.i64_type();

        let handle = if method == "not_" {
            let f = self
                .module
                .get_function("karac_lazy_expr_not")
                .expect("karac_lazy_expr_not declared in Codegen::new");
            self.builder
                .build_call(f, &[recv.into()], "lz.not")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value()
        } else {
            let arg = args
                .first()
                .ok_or_else(|| format!("LazyExpr.{method} expects one argument"))?;
            let what = format!("LazyExpr.{method} argument");
            let rhs = self.lazy_rhs_handle(&arg.value, &what)?;
            let (fname, op) = if let Some(op) = cmp_op {
                ("karac_lazy_expr_cmp", op)
            } else if let Some(op) = bool_op {
                ("karac_lazy_expr_bool", op)
            } else {
                ("karac_lazy_expr_arith", arith_op.unwrap())
            };
            let f = self
                .module
                .get_function(fname)
                .unwrap_or_else(|| panic!("{fname} declared in Codegen::new"));
            self.builder
                .build_call(
                    f,
                    &[i64_t.const_int(op, false).into(), recv.into(), rhs.into()],
                    "lz.expr",
                )
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_pointer_value()
        };
        self.track_lazy_expr_handle(handle);
        Ok(Some(self.build_lazy_struct_value("LazyExpr", handle)))
    }
}
