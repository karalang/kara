//! `Symbol` + `Interner` method codegen — `intern` / `resolve` / `len`.
//!
//! `Interner` lowers to the opaque `*mut KaracInterner` handle returned by
//! `Interner.new()` (see `assoc_call.rs`); the byte-string table lives in
//! `runtime/src/interner.rs`. `Symbol` is a `distinct type Symbol = i64`, so
//! it erases to a bare `i64` through the existing distinct-type lowering —
//! no per-`Symbol` codegen exists at all: the interner methods traffic in
//! plain `i64` ids.
//!
//! v1 floor (mirrors the interpreter, `src/interpreter/method_call_interner.rs`):
//! - `intern(s)` passes the argument `String`'s `(ptr, len)` to the runtime,
//!   which copies the bytes on a fresh mint (the argument is only borrowed —
//!   `fn intern(ref self, s: ref String)`), and returns the `i64` id.
//! - `resolve(sym)` returns a **borrowed** `String` view of the interned
//!   bytes: `{ptr, len, cap = 0}` — the `cap = 0` static-buffer convention
//!   means no scope-exit free ever touches it, exactly like a string
//!   literal. The pointed-at bytes are stable for the interner's lifetime
//!   (individually-boxed buffers, append-only table).
//! - `len()` returns the distinct-string count as `i64`.
//!
//! Receiver scope matches the `OnceLock`/`OnceCell` lowering: a LOCAL
//! binding whose initializer is `Interner.new()` (or annotated `Interner`).
//! Passing an interner to another function stays interpreter-only for now —
//! the receiver gate (`interner_vars`) only ever contains local bindings, so
//! such programs fail loudly at the user-impl fallthrough rather than
//! miscompile.

use crate::ast::*;

use inkwell::values::BasicValueEnum;

impl<'ctx> super::Codegen<'ctx> {
    /// Lower an `Interner` method call on a local binding `recv`. Dispatched
    /// from `compile_method_call` gated on `interner_vars` membership.
    pub(super) fn compile_interner_method(
        &mut self,
        recv: &str,
        method: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match method {
            "intern" => self.compile_interner_intern(recv, args),
            "resolve" => self.compile_interner_resolve(recv, args),
            "len" => self.compile_interner_len(recv),
            _ => Err(format!(
                "codegen: unsupported Interner method `{method}` (only \
                 intern/resolve/len are lowered)"
            )),
        }
    }

    /// Load the opaque `*mut KaracInterner` handle from the binding's slot.
    fn load_interner_handle(
        &mut self,
        recv: &str,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let slot = self
            .get_data_ptr(recv)
            .ok_or_else(|| format!("unknown Interner binding '{recv}'"))?;
        Ok(self
            .builder
            .build_load(ptr_ty, slot, "interner.handle")
            .unwrap()
            .into_pointer_value())
    }

    /// `interner.intern(s) -> Symbol`. Compiles the argument to its
    /// `{ptr, len, cap}` String value, passes `(ptr, len)` to the runtime
    /// (which copies on a fresh mint — the argument is borrowed), and rides
    /// the returned `i64` id as the `Symbol` value. A fresh-owned temp
    /// argument (`interner.intern(a + b)`) is materialized into a
    /// caller-scope slot so its buffer is freed at scope exit — the runtime
    /// copied the bytes, so nothing else owns the temp.
    fn compile_interner_intern(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let arg = args
            .first()
            .ok_or_else(|| "Interner.intern expects a string argument".to_string())?;
        let handle = self.load_interner_handle(recv)?;
        let sval = self.compile_expr(&arg.value)?;
        if !self.llvm_ty_is_vec_struct(sval.get_type()) {
            return Err("codegen: Interner.intern argument must be a String value".to_string());
        }
        let sstruct = sval.into_struct_value();
        let data_ptr = self
            .builder
            .build_extract_value(sstruct, 0, "intern.arg.ptr")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sstruct, 1, "intern.arg.len")
            .unwrap()
            .into_int_value();
        let f = self
            .module
            .get_function("karac_runtime_interner_intern")
            .expect("karac_runtime_interner_intern declared in Codegen::new");
        let id = self
            .builder
            .build_call(
                f,
                &[handle.into(), data_ptr.into(), len.into()],
                "intern.id",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic();
        // The runtime copied the bytes; a fresh-owned temp argument
        // (`intern(a + b)`, `intern(f())`) would otherwise orphan its
        // buffer. A named binding keeps its own let-drop and is excluded
        // by `expr_yields_fresh_owned_temp`.
        let arg_key = (arg.value.span.offset, arg.value.span.length);
        if self.expr_yields_fresh_owned_temp(&arg.value) && !self.rhs_stages_fstr_acc(&arg.value) {
            self.materialize_owned_temp(sval, arg_key);
        }
        Ok(id)
    }

    /// `interner.resolve(sym) -> ref String`. Calls the runtime with an
    /// out-param length slot and assembles a BORROWED `{ptr, len, cap = 0}`
    /// String view — `cap = 0` is the static-buffer convention, so no free
    /// path ever reclaims it (the interner owns the bytes). A foreign /
    /// out-of-range id degrades to the empty string in the runtime.
    fn compile_interner_resolve(
        &mut self,
        recv: &str,
        args: &[CallArg],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let arg = args
            .first()
            .ok_or_else(|| "Interner.resolve expects a Symbol argument".to_string())?;
        let handle = self.load_interner_handle(recv)?;
        let id = self.compile_expr(&arg.value)?;
        if !id.is_int_value() {
            return Err("codegen: Interner.resolve argument must be a Symbol".to_string());
        }
        let i64_t = self.context.i64_type();
        let cur_fn = self.current_fn.unwrap();
        let len_slot = self.create_entry_alloca(cur_fn, "resolve.len.slot", i64_t.into());
        let f = self
            .module
            .get_function("karac_runtime_interner_resolve")
            .expect("karac_runtime_interner_resolve declared in Codegen::new");
        let data_ptr = self
            .builder
            .build_call(
                f,
                &[handle.into(), id.into_int_value().into(), len_slot.into()],
                "resolve.ptr",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_slot, "resolve.len")
            .unwrap()
            .into_int_value();
        let str_ty = self.vec_struct_type();
        let with_ptr = self
            .builder
            .build_insert_value(str_ty.get_undef(), data_ptr, 0, "resolve.s0")
            .unwrap();
        let with_len = self
            .builder
            .build_insert_value(with_ptr, len, 1, "resolve.s1")
            .unwrap();
        let view = self
            .builder
            .build_insert_value(with_len, i64_t.const_zero(), 2, "resolve.s2")
            .unwrap()
            .into_struct_value();
        Ok(view.into())
    }

    /// `interner.len() -> i64` — number of distinct strings interned.
    fn compile_interner_len(&mut self, recv: &str) -> Result<BasicValueEnum<'ctx>, String> {
        let handle = self.load_interner_handle(recv)?;
        let f = self
            .module
            .get_function("karac_runtime_interner_len")
            .expect("karac_runtime_interner_len declared in Codegen::new");
        Ok(self
            .builder
            .build_call(f, &[handle.into()], "interner.len")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic())
    }
}

/// `true` when the expression is a zero-arg `Interner.new()` associated
/// call — the local-binding initializer shape that turns a `let` binding
/// into a tracked interner (`interner_vars` + scope-exit
/// `FreeInternerHandle`). Mirrors `module_binding_is_once_new`'s shape test.
pub(super) fn expr_is_interner_new(expr: &Expr) -> bool {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return false;
    };
    if !args.is_empty() {
        return false;
    }
    let ExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    segments.len() == 2 && segments[0] == "Interner" && segments[1] == "new"
}
