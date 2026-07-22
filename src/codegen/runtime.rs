//! Runtime intrinsic emission: refcounting, scope cleanup, var-tracking
//! registration, and the string-build helpers used by f-strings.
//!
//! Houses `emit_panic`, the RC/Arc alloc/inc/dec primitives
//! (`emit_rc_alloc`, `emit_rc_inc`, `emit_rc_dec`, `emit_arc_inc`,
//! `emit_arc_dec`, `emit_refcount_inc`, `emit_refcount_dec`), the
//! per-variable cleanup-registration helpers
//! (`track_rc_var`, `track_vec_var`, `track_map_var`, `track_enum_var`,
//! `track_struct_var`, `enum_name_for_binding`), the scope-cleanup
//! emission (`emit_scope_cleanup`, `drain_top_frame_with_emit`,
//! `emit_cleanup_action`), and the f-string raw-builder helpers
//! (`emit_string_append_raw`, `compile_fstr_part_to_cstr`).

use crate::ast::*;

use inkwell::types::{BasicType, BasicTypeEnum, StructType};
use inkwell::values::{BasicValueEnum, FloatValue, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, AtomicOrdering, AtomicRMWBinOp, IntPredicate};

use super::state::{CleanupAction, VarSlot};

impl<'ctx> super::Codegen<'ctx> {
    /// Allocate a new RC heap object: `malloc(sizeof(heap_type))`, store refcount = 1.
    /// Returns a pointer to the heap object.
    pub(super) fn emit_panic(&self, message: &str) {
        // OUTLINED PANIC BODIES: the printf + exit live in a per-site
        // zero-arg `internal` function (`__karac_panic_site_<n>`, marked
        // `cold` + `noinline` + `noreturn`); the panic landing pad in the
        // enclosing function is just `call @__karac_panic_site_<n>()`. Every
        // operand (format string, location, fault prefix, message) is a
        // compile-time constant baked INSIDE the outlined body, so the
        // landing pad contributes the minimum possible inline cost to the
        // enclosing function. This matters: the LLVM inline cost model
        // counts call operands, and growing the panic-site printf from 1
        // operand to 7 (fault-prefix `8183f6c7` + location `290e454c`,
        // both 2026-05-31) pushed bounds-check-bearing functions past the
        // O2 inline threshold — kata-5's `expand` helper stopped inlining
        // into its caller's hot loop and regressed 1.34× (the un-inlined
        // copy re-runs two loop-invariant guards per iteration that the
        // inlined+optimized form hoists). Verified empirically: reverting
        // the panic printf to its 1-operand form restores inlining; with
        // outlining the landing pad is cheaper still.
        let site_id = self.panic_site_counter.get();
        self.panic_site_counter.set(site_id + 1);
        // `#[track_caller]` slice 5: inside a `#[track_caller]` fn the reported
        // panic location comes from the runtime caller-location params — SSA
        // values of the *enclosing* function, which the separate outlined
        // `__karac_panic_site_<n>` body cannot reference. So when redirecting,
        // the outlined body takes the location `(file, line, col)` as three
        // params and the landing-pad call forwards the enclosing fn's received
        // values. Ordinary panics keep the zero-arg outlined body (unchanged).
        let tc_loc = self.current_fn_caller_loc;
        let panic_fn_type = if tc_loc.is_some() {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let i32_ty = self.context.i32_type();
            self.context
                .void_type()
                .fn_type(&[ptr_ty.into(), i32_ty.into(), i32_ty.into()], false)
        } else {
            self.context.void_type().fn_type(&[], false)
        };
        let panic_fn = self.module.add_function(
            &format!("__karac_panic_site_{site_id}"),
            panic_fn_type,
            Some(inkwell::module::Linkage::Internal),
        );
        for attr_name in ["cold", "noinline", "noreturn"] {
            let kind = inkwell::attributes::Attribute::get_named_enum_kind_id(attr_name);
            debug_assert!(kind != 0, "{attr_name} attribute kind-id must resolve");
            panic_fn.add_attribute(
                inkwell::attributes::AttributeLoc::Function,
                self.context.create_enum_attribute(kind, 0),
            );
        }
        let body = self.context.append_basic_block(panic_fn, "entry");
        let b = self.context.create_builder();
        b.position_at_end(body);

        // design.md § Contracts rule 2: the fault-category prefix is decided at
        // RUNTIME by `karac_runtime_panic_prefix()`, which returns
        // `"contract predicate panicked: "` while a contract predicate is on the
        // stack (a thread-local depth counter set by the enter/exit calls
        // `emit_contract_assert` brackets the predicate's evaluation with) and
        // `""` otherwise. Reading the flag at runtime — rather than baking the
        // prefix in from a compile-time flag — categorizes BOTH the inline case
        // (a bounds / div / unwrap panic lexically inside the predicate) AND the
        // cross-call case (a panic inside a function the predicate calls), which
        // a lexical flag cannot see. The format string is fixed (`panic: %s%s`),
        // so `message` is a `%s` data argument, not the format string — output
        // is byte-identical to the two historical forms `panic: <msg>` and
        // `panic: contract predicate panicked: <msg>`.
        //
        // CONTRACT-FREE FOLD: when `compile_program`'s item scan proved no
        // contract predicate can ever run in this program
        // (`runtime_panic_prefix_needed == false`), the depth counter is
        // statically 0 and the prefix is always `""` — fold it to a static
        // empty string instead of calling the runtime. That leaves
        // `karac_runtime_panic_prefix` unreferenced, so its thread-local's
        // writable 16 KiB __DATA page dead-strips from every contract-free
        // binary (+49% on the lean-binary floor when it crept in). Output is
        // byte-identical (`%s` of `""`).
        let prefix: BasicValueEnum<'ctx> = if self.runtime_panic_prefix_needed {
            b.build_call(self.karac_runtime_panic_prefix_fn, &[], "panic_prefix")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
        } else {
            b.build_global_string_ptr("\0", "panic_prefix_static")
                .unwrap()
                .as_pointer_value()
                .into()
        };
        let msg = b
            .build_global_string_ptr(&format!("{}\0", message), "panic_msg")
            .unwrap();

        // Level 2 crash diagnostics (design.md § Crash diagnostics): when a
        // source location is available, emit
        // `panic at <file>:<line>:<col> in <fn>: <msg>`. file/line/col/fn are
        // all known at COMPILE time, so they go in as constant `printf`
        // operands — there is deliberately NO runtime DWARF walk and NO
        // runtime symbolizer (that would re-add the ~57 KiB gimli/addr2line
        // tree the Phase 3 binary-size fix dead-strips from every binary; see
        // phase-7-codegen.md "Phase 3"). Span carries 1-indexed line/col
        // directly, so no source-text resolution is needed. The location is
        // gated on `source_filename` being threaded in (the CLI build/run
        // path supplies it; bare-IR tests and ad-hoc dumps don't), so callers
        // without a filename keep the original `panic: <msg>` output — the
        // same gating the sibling `?`-error-trace uses. DWARF emission for
        // gdb/lldb symbolic backtraces (the design's stated *bonus*) is a
        // separate concern handled by the DIBuilder pass.
        let i32_ty = self.context.i32_type();
        // Location operands for the `panic at <file>:<line>:<col> in <fn>` form.
        // When redirecting (`#[track_caller]`), they are the outlined body's OWN
        // three params (the caller location the landing pad forwards below);
        // otherwise they are the compile-time Level-2 span constants. The `<fn>`
        // name always identifies the actually-emitting frame. `None` → the bare
        // `panic: <msg>` form (no filename/span available, non-track_caller).
        let loc_operands: Option<(
            inkwell::values::PointerValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
            inkwell::values::IntValue<'ctx>,
        )> = if tc_loc.is_some() {
            Some((
                panic_fn.get_nth_param(0).unwrap().into_pointer_value(),
                panic_fn.get_nth_param(1).unwrap().into_int_value(),
                panic_fn.get_nth_param(2).unwrap().into_int_value(),
            ))
        } else {
            match (&self.source_filename, &self.current_span) {
                (Some(file), Some(span)) => {
                    let file_ptr = b
                        .build_global_string_ptr(&format!("{}\0", file), "panic_file")
                        .unwrap()
                        .as_pointer_value();
                    Some((
                        file_ptr,
                        i32_ty.const_int(span.line as u64, false),
                        i32_ty.const_int(span.column as u64, false),
                    ))
                }
                _ => None,
            }
        };
        match loc_operands {
            Some((file_ptr, line, col)) => {
                let fmt = b
                    .build_global_string_ptr("panic at %s:%d:%d in %s: %s%s\n\0", "panic_fmt")
                    .unwrap();
                let fn_ptr = b
                    .build_global_string_ptr(&format!("{}\0", self.current_fn_name), "panic_fn")
                    .unwrap();
                b.build_call(
                    self.printf_fn,
                    &[
                        fmt.as_pointer_value().into(),
                        file_ptr.into(),
                        line.into(),
                        col.into(),
                        fn_ptr.as_pointer_value().into(),
                        prefix.into(),
                        msg.as_pointer_value().into(),
                    ],
                    "panic_print",
                )
                .unwrap();
            }
            None => {
                let fmt = b
                    .build_global_string_ptr("panic: %s%s\n\0", "panic_fmt")
                    .unwrap();
                b.build_call(
                    self.printf_fn,
                    &[
                        fmt.as_pointer_value().into(),
                        prefix.into(),
                        msg.as_pointer_value().into(),
                    ],
                    "panic_print",
                )
                .unwrap();
            }
        }
        let exit_code = self.context.i32_type().const_int(1, false);
        b.build_call(self.exit_fn, &[exit_code.into()], "").unwrap();
        b.build_unreachable().unwrap();

        // The landing pad in the enclosing function. Normally one zero-operand
        // call; when redirecting (`#[track_caller]`), forward the enclosing fn's
        // received caller-location params so the outlined body prints them.
        // Callers of `emit_panic` terminate the block themselves (the existing
        // contract — most follow with `build_unreachable`).
        match tc_loc {
            Some((file_arg, line_arg, col_arg)) => {
                self.builder
                    .build_call(
                        panic_fn,
                        &[file_arg.into(), line_arg.into(), col_arg.into()],
                        "",
                    )
                    .unwrap();
            }
            None => {
                self.builder.build_call(panic_fn, &[], "").unwrap();
            }
        }
    }

    pub(super) fn emit_rc_alloc(&self, heap_type: StructType<'ctx>) -> PointerValue<'ctx> {
        let size = heap_type.size_of().expect("heap type must be sized");
        let call = self
            .builder
            .build_call(self.malloc_fn, &[size.into()], "rc_alloc")
            .unwrap();
        let ptr = call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Store strong refcount = 1 at field 0.
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        self.builder
            .build_store(rc_ptr, self.context.i64_type().const_int(1, false))
            .unwrap();
        // Weak-headered box `{ strong, weak, fields… }`: the strong set holds one
        // implicit weak, so the fresh box starts weak = 1 (matching the runtime
        // primitives' invariant; `docs/spikes/weak-refs.md`). Field 1 is the weak
        // count. Non-weak boxes have no such field and skip this.
        if self.heap_type_is_weak_headered(heap_type) {
            let weak_ptr = self
                .builder
                .build_struct_gep(heap_type, ptr, 1, "weak_ptr")
                .unwrap();
            self.builder
                .build_store(weak_ptr, self.context.i64_type().const_int(1, false))
                .unwrap();
        }
        ptr
    }

    /// Reverse lookup: does the box `heap_type` belong to a `weak`-targeted
    /// shared type (two-word `{ strong, weak, fields… }` control header)?
    /// Iterates `shared_types` (small map; same O(n) reverse scan `emit_rc_dec`
    /// uses). `false` for every type today — inert until the store/read slices.
    pub(super) fn heap_type_is_weak_headered(&self, heap_type: StructType<'ctx>) -> bool {
        self.shared_types
            .values()
            .any(|i| i.heap_type == heap_type && i.has_weak_header)
    }

    /// Get-or-declare a `void*(void*)` or `void(void*)` weak-primitive runtime
    /// symbol. The `weak T` codegen (store/read/drop) declares these on demand;
    /// the archive / JIT runner supply the bodies (`runtime/src/weak.rs`, kept
    /// alive via `__preserve_no_mangle_symbols`). `returns_ptr` picks the
    /// signature: `downgrade`/`upgrade` return the box pointer, `drop` is void.
    pub(super) fn weak_runtime_fn(&self, name: &str, returns_ptr: bool) -> FunctionValue<'ctx> {
        self.module.get_function(name).unwrap_or_else(|| {
            let ptr_ty = self.context.ptr_type(AddressSpace::default());
            let fn_ty = if returns_ptr {
                ptr_ty.fn_type(&[ptr_ty.into()], false)
            } else {
                self.context.void_type().fn_type(&[ptr_ty.into()], false)
            };
            self.module
                .add_function(name, fn_ty, Some(inkwell::module::Linkage::External))
        })
    }

    /// Store a `weak T` field: `field_ptr` is the single nullable weak slot,
    /// `new_box` the target's box pointer (null = store `None`). Downgrades the
    /// NEW target first (`karac_weak_downgrade`, weak += 1 — null-safe no-op),
    /// stores it, then weak-drops the OLD occupant (`karac_weak_drop`, weak -= 1,
    /// freeing the box iff strong == 0 && weak == 0). Downgrade-before-drop is
    /// the ARC-setter rule (safe under self-assignment / aliasing). No STRONG
    /// retain — a weak ref never contributes to the strong count, which is the
    /// whole point (`docs/spikes/weak-refs.md`, B-2026-07-19-8).
    pub(super) fn emit_weak_field_store(
        &self,
        field_ptr: PointerValue<'ctx>,
        new_box: PointerValue<'ctx>,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let downgrade = self.weak_runtime_fn("karac_weak_downgrade", true);
        let drop_fn = self.weak_runtime_fn("karac_weak_drop", false);
        // Downgrade the new target (weak += 1), get back the (same) pointer.
        let bumped = self
            .builder
            .build_call(downgrade, &[new_box.into()], "weak.downgrade")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Load the old occupant before the store clobbers the slot.
        let old = self
            .builder
            .build_load(ptr_ty, field_ptr, "weak.old")
            .unwrap()
            .into_pointer_value();
        self.builder.build_store(field_ptr, bumped).unwrap();
        // Weak-drop the old occupant (null-safe).
        self.builder.build_call(drop_fn, &[old.into()], "").unwrap();
    }

    /// Initialize a fresh `weak T` field (constructor site — the slot has no
    /// prior occupant to weak-drop). Downgrades the target (weak += 1) and
    /// stores it; `new_box` null stores `None`. The construction sibling of
    /// `emit_weak_field_store`.
    pub(super) fn emit_weak_field_init(
        &self,
        field_ptr: PointerValue<'ctx>,
        new_box: PointerValue<'ctx>,
    ) {
        let downgrade = self.weak_runtime_fn("karac_weak_downgrade", true);
        let bumped = self
            .builder
            .build_call(downgrade, &[new_box.into()], "weak.downgrade")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_store(field_ptr, bumped).unwrap();
    }

    /// Read a `weak T` field as a nullable box pointer for the niche
    /// `Option[T]` unpack (null = `None`, non-null = `Some`). Liveness-checks
    /// the target: a slot pointing at a box whose `strong == 0` (the target was
    /// dropped; only the control header survives for outstanding weak refs)
    /// reads `None` — never a dangling `Some` over freed payload.
    ///
    /// This is a BORROW read (no strong retain): the returned pointer is handed
    /// to the standard `Option[shared T]` machinery, whose Some-binding does its
    /// own balanced alias-acquire / scope-exit release. Doing the retain here
    /// too would double-count (a leak). The target box lives as long as this
    /// weak slot holds it (weak >= 1), so reading `strong` is always safe.
    /// (`docs/spikes/weak-refs.md`, B-2026-07-19-8.)
    pub(super) fn emit_weak_field_upgrade(
        &self,
        field_ptr: PointerValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.expect("weak read inside a function");
        let slot = self
            .builder
            .build_load(ptr_ty, field_ptr, "weak.slot")
            .unwrap()
            .into_pointer_value();
        let is_null = self.builder.build_is_null(slot, "weak.slot.null").unwrap();
        let live_bb = self.context.append_basic_block(fn_val, "weak.live.check");
        let join_bb = self.context.append_basic_block(fn_val, "weak.read.join");
        let entry_bb = self.builder.get_insert_block().unwrap();
        self.builder
            .build_conditional_branch(is_null, join_bb, live_bb)
            .unwrap();
        // Non-null slot: load the target's strong count (field 0 of its
        // `{ strong, weak, … }` box) and keep the pointer only if strong > 0.
        self.builder.position_at_end(live_bb);
        let strong = self
            .builder
            .build_load(i64_t, slot, "weak.strong")
            .unwrap()
            .into_int_value();
        let alive = self
            .builder
            .build_int_compare(IntPredicate::SGT, strong, i64_t.const_zero(), "weak.alive")
            .unwrap();
        let live_ptr = self
            .builder
            .build_select(alive, slot, ptr_ty.const_null(), "weak.live.ptr")
            .unwrap()
            .into_pointer_value();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        let live_end_bb = self.builder.get_insert_block().unwrap();
        // Join: null (dead / empty) or the live pointer.
        self.builder.position_at_end(join_bb);
        let phi = self.builder.build_phi(ptr_ty, "weak.read.ptr").unwrap();
        phi.add_incoming(&[(&ptr_ty.const_null(), entry_bb), (&live_ptr, live_end_bb)]);
        phi.as_basic_value().into_pointer_value()
    }

    /// Free a shared-struct box at `strong == 0`, choosing the weak-aware
    /// release for a two-word `{ strong, weak, … }` box. A conventional box is
    /// `free`d directly; a weak-headered box instead routes through
    /// `karac_weak_box_strong_zero_release`, which drops the implicit weak the
    /// strong set held and frees the box ONLY when no outstanding weak ref
    /// remains — so a live `weak` ref keeps the 16-byte control header alive for
    /// its `upgrade` nil-check (`docs/spikes/weak-refs.md`, B-2026-07-19-8). The
    /// caller must already have run the recursive payload drop (the header
    /// outlives the payload). Inert for all code today (no weak-headered type).
    pub(super) fn emit_shared_box_free(
        &self,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.heap_type_is_weak_headered(heap_type) {
            let release_fn = self
                .module
                .get_function("karac_weak_box_strong_zero_release")
                .unwrap_or_else(|| {
                    let void_ty = self.context.void_type();
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
                    self.module.add_function(
                        "karac_weak_box_strong_zero_release",
                        fn_ty,
                        Some(inkwell::module::Linkage::External),
                    )
                });
            self.builder
                .build_call(release_fn, &[ptr.into()], "")
                .unwrap();
        } else {
            self.builder
                .build_call(self.free_fn, &[ptr.into()], "")
                .unwrap();
        }
    }

    /// Shared-ownership inc-on-copy (B-2026-06-22-2): when a heap-env closure
    /// binding is COPIED (`let g = f`), the new owner shares the SAME RC env box
    /// `{ i64 refcount, env }`, so its refcount must be incremented — both
    /// owners then RC-drop it via `FreeClosureEnv` at scope exit and the box is
    /// reclaimed exactly once. `fat` is the `{ fn_ptr, env_ptr }` closure value
    /// being copied; field 1 is the env box (whose field 0 is the refcount). A
    /// null env (a non-capturing closure) is skipped. Mirrors the `FreeClosureEnv`
    /// cleanup's box/refcount access shape, inverted to `+1` with no free.
    pub(super) fn emit_heap_closure_env_inc(&self, fat: BasicValueEnum<'ctx>) {
        let fn_val = self
            .current_fn
            .expect("heap-closure env inc emitted inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let env_box = self
            .builder
            .build_extract_value(fat.into_struct_value(), 1, "clo.inc.env")
            .unwrap()
            .into_pointer_value();
        let null = ptr_ty.const_null();
        let live = self
            .builder
            .build_int_compare(IntPredicate::NE, env_box, null, "clo.inc.live")
            .unwrap();
        let inc_bb = self.context.append_basic_block(fn_val, "clo.inc.do");
        let join_bb = self.context.append_basic_block(fn_val, "clo.inc.join");
        self.builder
            .build_conditional_branch(live, inc_bb, join_bb)
            .unwrap();
        self.builder.position_at_end(inc_bb);
        let i64_t = self.context.i64_type();
        // The refcount is field 0 of the RC box; a `{ i64 }` GEP reaches it
        // regardless of the captured payload that follows.
        let rc_box_ty = self.context.struct_type(&[i64_t.into()], false);
        let rc_ptr = self
            .builder
            .build_struct_gep(rc_box_ty, env_box, 0, "clo.inc.rc")
            .unwrap();
        let rc = self
            .builder
            .build_load(i64_t, rc_ptr, "clo.inc.rcval")
            .unwrap()
            .into_int_value();
        let inc = self
            .builder
            .build_int_add(rc, i64_t.const_int(1, false), "clo.inc.rc1")
            .unwrap();
        self.builder.build_store(rc_ptr, inc).unwrap();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// RC-drop a heap-env closure given a loaded closure fat pointer: extract
    /// the env box (field 1), skip a null env, else decrement its refcount and
    /// `free` the box at zero. Shared by the scope-exit `FreeClosureEnv` cleanup
    /// (which loads the fat from the binding's alloca first) and the
    /// binding-reassignment drop-old path (`g = make(j)` / `g = f`), which drops
    /// `g`'s CURRENT env before overwriting the slot (B-2026-06-22-2).
    pub(super) fn emit_heap_closure_env_dec(&self, fat: BasicValueEnum<'ctx>) {
        let fn_val = self
            .current_fn
            .expect("heap-closure env dec emitted inside a function");
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let env_box = self
            .builder
            .build_extract_value(fat.into_struct_value(), 1, "clo.dec.env")
            .unwrap()
            .into_pointer_value();
        let null = ptr_ty.const_null();
        let live = self
            .builder
            .build_int_compare(IntPredicate::NE, env_box, null, "clo.dec.live")
            .unwrap();
        let dec_bb = self.context.append_basic_block(fn_val, "clo.dec.do");
        let free_bb = self.context.append_basic_block(fn_val, "clo.dec.free");
        let join_bb = self.context.append_basic_block(fn_val, "clo.dec.join");
        self.builder
            .build_conditional_branch(live, dec_bb, join_bb)
            .unwrap();
        self.builder.position_at_end(dec_bb);
        let i64_t = self.context.i64_type();
        // The refcount is field 0 of the RC box; a `{ i64 }` GEP reaches it
        // regardless of the captured payload that follows.
        let rc_box_ty = self.context.struct_type(&[i64_t.into()], false);
        let rc_ptr = self
            .builder
            .build_struct_gep(rc_box_ty, env_box, 0, "clo.dec.rc")
            .unwrap();
        let rc = self
            .builder
            .build_load(i64_t, rc_ptr, "clo.dec.rcval")
            .unwrap()
            .into_int_value();
        let dec = self
            .builder
            .build_int_sub(rc, i64_t.const_int(1, false), "clo.dec.dec1")
            .unwrap();
        self.builder.build_store(rc_ptr, dec).unwrap();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, dec, i64_t.const_zero(), "clo.dec.z")
            .unwrap();
        self.builder
            .build_conditional_branch(is_zero, free_bb, join_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        // Slice 2 (B-2026-06-22-2): before freeing the RC box, run the
        // per-closure env-drop fn (box field 1) to free any captured String/Vec
        // buffers the env owns. The box layout is `{ i64 rc, ptr env_drop, env }`;
        // field 1 is a FIXED offset regardless of the variable-size env payload,
        // so a `{ i64, ptr }` prefix GEP reaches it. A null drop slot (a POD-only
        // Slice 1 env) skips straight to the box free.
        let dropslot_prefix = self
            .context
            .struct_type(&[i64_t.into(), ptr_ty.into()], false);
        let drop_pp = self
            .builder
            .build_struct_gep(dropslot_prefix, env_box, 1, "clo.dec.dropfn.p")
            .unwrap();
        let drop_fn_ptr = self
            .builder
            .build_load(ptr_ty, drop_pp, "clo.dec.dropfn")
            .unwrap()
            .into_pointer_value();
        let has_drop = self
            .builder
            .build_int_compare(
                IntPredicate::NE,
                drop_fn_ptr,
                ptr_ty.const_null(),
                "clo.dec.hasdrop",
            )
            .unwrap();
        let call_drop_bb = self.context.append_basic_block(fn_val, "clo.dec.calldrop");
        let do_free_bb = self.context.append_basic_block(fn_val, "clo.dec.dofree");
        self.builder
            .build_conditional_branch(has_drop, call_drop_bb, do_free_bb)
            .unwrap();
        self.builder.position_at_end(call_drop_bb);
        let env_drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        self.builder
            .build_indirect_call(env_drop_fn_ty, drop_fn_ptr, &[env_box.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(do_free_bb).unwrap();
        self.builder.position_at_end(do_free_bb);
        self.builder
            .build_call(self.free_fn, &[env_box.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// Phase D: allocate a headerless cluster member — `malloc` of the
    /// twin struct's size, no rc word, no rc=1 store. Callers must hold
    /// a `shared_gep_layout` result with base 0 for the same type; the
    /// object is freed by the root's `FreeClusterWalk` (or the member
    /// orphans into it via the chain), never by any count op.
    pub(super) fn emit_headerless_alloc(&self, twin: StructType<'ctx>) -> PointerValue<'ctx> {
        let size = twin.size_of().expect("twin type must be sized");
        let call = self
            .builder
            .build_call(self.malloc_fn, &[size.into()], "hl_alloc")
            .unwrap();
        call.try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value()
    }

    /// Increment the reference count of a shared object.
    pub(super) fn emit_rc_inc(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        let rc = self
            .builder
            .build_load(self.context.i64_type(), rc_ptr, "rc")
            .unwrap()
            .into_int_value();
        let rc_inc = self
            .builder
            .build_int_add(rc, self.context.i64_type().const_int(1, false), "rc_inc")
            .unwrap();
        self.builder.build_store(rc_ptr, rc_inc).unwrap();
    }

    /// Decrement the reference count. If it reaches zero, dispatch to
    /// the per-struct recursive drop fn (`__karac_rc_drop_<Name>`)
    /// when one was lazily synthesized by `track_rc_var` for this
    /// heap type. The drop fn walks each heap-owning field (shared
    /// inner refs, `Option[shared T]` fields, Vec/String data
    /// buffers, Map/Set handles) before `free(ptr)`. Falls back to
    /// plain `free(ptr)` when the struct has no walkable fields
    /// (every field primitive) — `emit_shared_struct_rc_drop_fn`
    /// caches `None` for those, and the reverse-lookup below sees
    /// `Some(None)` and takes the legacy path.
    ///
    /// Resolving heap_type → struct name is done by iterating
    /// `shared_types` (small map; O(n) is fine — measured cost
    /// noise versus a malloc/free pair). A reverse map could be
    /// added if profiles show it.
    pub(super) fn emit_rc_dec(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "rc_ptr")
            .unwrap();
        let rc = self
            .builder
            .build_load(self.context.i64_type(), rc_ptr, "rc")
            .unwrap()
            .into_int_value();
        let rc_dec = self
            .builder
            .build_int_sub(rc, self.context.i64_type().const_int(1, false), "rc_dec")
            .unwrap();
        self.builder.build_store(rc_ptr, rc_dec).unwrap();

        let is_zero = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                rc_dec,
                self.context.i64_type().const_zero(),
                "rc_is_zero",
            )
            .unwrap();

        let current_fn = self.current_fn.unwrap();
        let free_bb = self.context.append_basic_block(current_fn, "rc_free");
        let done_bb = self.context.append_basic_block(current_fn, "rc_done");

        self.builder
            .build_conditional_branch(is_zero, free_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        // Dispatch to the per-struct recursive drop fn when one was
        // synthesized for this heap_type. Otherwise plain `free`. The
        // drop fn includes `free(ptr)` after its field walk, so we
        // don't emit a second `free` here.
        let mut dropped = false;
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                if let Some(Some(drop_fn)) = self.rc_drop_fns.get(name) {
                    self.builder
                        .build_call(*drop_fn, &[ptr.into()], "")
                        .unwrap();
                    dropped = true;
                }
                break;
            }
        }
        if !dropped {
            // RC-fallback box of an aggregate with heap fields: free the
            // boxed value's String/Vec buffers before releasing the box
            // (B-2026-06-10-8). When no such fn is registered for this box
            // type, the boxed value owns no heap and the plain free below is
            // correct. The refcount gates this whole block to `rc == 0`, so
            // the field free runs exactly once for the binding's last owner —
            // whole-binding moves (which inc/dec the box rc) never double-free.
            if let Some(&(_, value_drop_fn)) = self
                .rc_fallback_box_drop_fns
                .iter()
                .find(|(ty, _)| *ty == heap_type)
            {
                self.builder
                    .build_call(value_drop_fn, &[ptr.into()], "")
                    .unwrap();
            }
            // Weak-aware box free (inert for non-weak types): a weak-headered
            // box keeps its control header alive for outstanding weak refs.
            self.emit_shared_box_free(heap_type, ptr);
        }
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
    }

    /// Recursively test whether `agg_ty` (a tuple / struct LLVM type) holds
    /// any `{ptr,len,cap}` (String/Vec) field, directly or nested in a
    /// sub-aggregate. Drives whether an RC-fallback box needs a value-drop
    /// fn synthesized — false means the box free needs no field recursion
    /// (no IR emitted, no map entry). A String/Vec field is recognized
    /// structurally by `== vec_struct_type()`, the same signal
    /// `FreeVecBuffer`'s recursive element drop uses.
    pub(super) fn aggregate_has_heap_field(&self, agg_ty: StructType<'ctx>) -> bool {
        let vec_ty = self.vec_struct_type();
        (0..agg_ty.count_fields()).any(|i| match agg_ty.get_field_type_at_index(i) {
            Some(BasicTypeEnum::StructType(st)) if st == vec_ty => true,
            Some(BasicTypeEnum::StructType(st)) => self.aggregate_has_heap_field(st),
            _ => false,
        })
    }

    /// Emit a `cap`-guarded `free` for every String/Vec field of the
    /// aggregate at `base_ptr`, recursing into nested tuples/structs. Frees
    /// only the field buffers, never `base_ptr` itself (the box free is the
    /// caller's job). A Vec field's own *elements* are not recursed — only
    /// its outer buffer is freed, matching the one-level shape of the
    /// tuple-element drain; `Vec[heap_T]` nested inside a boxed aggregate
    /// leaks its elements (bounded remainder, never corruption).
    pub(super) fn emit_aggregate_heap_field_frees(
        &mut self,
        base_ptr: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        for i in 0..agg_ty.count_fields() {
            match agg_ty.get_field_type_at_index(i) {
                Some(BasicTypeEnum::StructType(st)) if st == vec_ty => {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(agg_ty, base_ptr, i, "rcfb.heap.f")
                        .unwrap();
                    let data_pp = self
                        .builder
                        .build_struct_gep(vec_ty, field_ptr, 0, "rcfb.data.pp")
                        .unwrap();
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_pp, "rcfb.data")
                        .unwrap()
                        .into_pointer_value();
                    let cap_pp = self
                        .builder
                        .build_struct_gep(vec_ty, field_ptr, 2, "rcfb.cap.pp")
                        .unwrap();
                    let cap = self
                        .builder
                        .build_load(i64_t, cap_pp, "rcfb.cap")
                        .unwrap()
                        .into_int_value();
                    // LLVM-type-only walker (String vs Vec erased) — 1.
                    self.emit_free_if_cap_positive(data, cap, 1);
                }
                Some(BasicTypeEnum::StructType(st)) => {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(agg_ty, base_ptr, i, "rcfb.nested.f")
                        .unwrap();
                    self.emit_aggregate_heap_field_frees(field_ptr, st);
                }
                _ => {}
            }
        }
    }

    /// Zero the `cap` of every Vec/String field of an aggregate (recursing
    /// into nested aggregates) — the move-out dual of
    /// `emit_aggregate_heap_field_frees`. After a tuple/struct VALUE is moved
    /// (`let u = t`, `return t`), the source's per-field `cap` is zeroed so its
    /// synthesized aggregate drop's `cap > 0` guards all skip, leaving the
    /// destination the sole owner (B-2026-06-11-4 part a). `&self` — pure IR
    /// emission, no state writes.
    pub(super) fn zero_aggregate_field_caps(
        &self,
        base_ptr: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        for i in 0..agg_ty.count_fields() {
            match agg_ty.get_field_type_at_index(i) {
                Some(BasicTypeEnum::StructType(st)) if st == vec_ty => {
                    if let Ok(field_ptr) =
                        self.builder
                            .build_struct_gep(agg_ty, base_ptr, i, "movecap.f")
                    {
                        if let Ok(cap_ptr) =
                            self.builder
                                .build_struct_gep(vec_ty, field_ptr, 2, "movecap.cap")
                        {
                            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
                        }
                    }
                }
                Some(BasicTypeEnum::StructType(st)) => {
                    if let Ok(field_ptr) =
                        self.builder
                            .build_struct_gep(agg_ty, base_ptr, i, "movecap.nf")
                    {
                        self.zero_aggregate_field_caps(field_ptr, st);
                    }
                }
                _ => {}
            }
        }
    }

    /// Synthesize (once per box heap type) the "free the boxed value's heap
    /// fields" fn for an RC-fallback box `{i64 rc, value}` whose `value` is
    /// an aggregate carrying String/Vec fields. Registered in
    /// `rc_fallback_box_drop_fns` and called by `emit_rc_dec` at `rc == 0`
    /// *before* the box itself is freed. No-op (nothing registered) when the
    /// boxed value owns no heap — the box free alone is then correct.
    /// Closes B-2026-06-10-8: a let-bound tuple/struct routed to RC-fallback
    /// boxing leaked its String/Vec field buffers at scope exit, because the
    /// box free (`emit_rc_dec`'s fallback `free`) never recursed into them.
    pub(super) fn register_rc_fallback_box_drop(&mut self, box_heap_type: StructType<'ctx>) {
        if self
            .rc_fallback_box_drop_fns
            .iter()
            .any(|(ty, _)| *ty == box_heap_type)
        {
            return;
        }
        let Some(BasicTypeEnum::StructType(value_ty)) = box_heap_type.get_field_type_at_index(1)
        else {
            return;
        };
        if !self.aggregate_has_heap_field(value_ty) {
            return;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_name = format!(
            "__karac_rc_fb_value_drop_{}",
            self.rc_fallback_box_drop_fns.len()
        );
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self.module.add_function(
            &fn_name,
            drop_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        // Register before emitting the body (idempotency / recursion guard).
        self.rc_fallback_box_drop_fns.push((box_heap_type, drop_fn));

        // The body uses `emit_free_if_cap_positive`, which appends basic
        // blocks to `current_fn` — point it at the drop fn during synthesis.
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let box_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let value_ptr = self
            .builder
            .build_struct_gep(box_heap_type, box_ptr, 1, "rcfb.value")
            .unwrap();
        self.emit_aggregate_heap_field_frees(value_ptr, value_ty);
        self.builder.build_return(None).unwrap();

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
    }

    /// Synthesize (once per aggregate LLVM type) a "free this aggregate's heap
    /// fields" drop fn for an ANONYMOUS aggregate — a tuple binding the
    /// named-struct `emit_struct_drop_synthesis` path can't reach (a tuple has
    /// no type name). The body is `emit_aggregate_heap_field_frees`, which
    /// recurses into nested aggregates and cap-guards each Vec/String free, so
    /// a moved binding whose field caps were zeroed drops to a no-op. Returns
    /// `None` (no fn, no cleanup) when the aggregate owns no heap. Cached in
    /// `aggregate_drop_fns`.
    pub(super) fn synthesize_aggregate_drop_fn(
        &mut self,
        agg_ty: StructType<'ctx>,
    ) -> Option<FunctionValue<'ctx>> {
        if !self.aggregate_has_heap_field(agg_ty) {
            return None;
        }
        if let Some((_, f)) = self.aggregate_drop_fns.iter().find(|(t, _)| *t == agg_ty) {
            return Some(*f);
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_name = format!("__karac_drop_tuple_{}", self.aggregate_drop_fns.len());
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self.module.add_function(
            &fn_name,
            drop_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        // Register before emitting the body (cache + recursion guard).
        self.aggregate_drop_fns.push((agg_ty, drop_fn));
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let p = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        self.emit_aggregate_heap_field_frees(p, agg_ty);
        self.builder.build_return(None).unwrap();
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(drop_fn)
    }

    /// #21 — the `TypeExpr`-driven sibling of [`Self::synthesize_aggregate_drop_fn`]:
    /// a drop fn that frees a tuple's heap via [`Self::emit_tuple_elem_drops`], so
    /// enum / nested-struct leaves are reached (the LLVM-type-driven aggregate
    /// drop above is enum-blind). Used to give a callee-owned tuple PARAM (#21
    /// entry-copy) a scope-exit drop that mirrors the owning struct's `NestedTuple`
    /// drop. Memoized by an element-type signature (NOT by `agg_ty` alone:
    /// `(Tok, i64)` and `(Other, i64)` share the LLVM type `{i64, i64}` but free
    /// different leaves). `None` when the tuple owns no drop-bearing heap.
    pub(super) fn synthesize_tuple_drop_fn_te(
        &mut self,
        agg_ty: StructType<'ctx>,
        elem_tes: &[crate::ast::TypeExpr],
    ) -> Option<FunctionValue<'ctx>> {
        if !elem_tes.iter().any(|e| self.type_expr_has_drop_heap(e)) {
            return None;
        }
        let fn_name = format!("__karac_drop_tuple_te_{}", Self::tuple_te_sig(elem_tes));
        if let Some(f) = self.module.get_function(&fn_name) {
            return Some(f);
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let drop_fn_ty = self.context.void_type().fn_type(&[ptr_ty.into()], false);
        let drop_fn = self.module.add_function(
            &fn_name,
            drop_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let p = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        self.emit_tuple_elem_drops(p, agg_ty, elem_tes);
        self.builder.build_return(None).unwrap();
        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        Some(drop_fn)
    }

    /// A stable, LLVM-name-safe signature of a tuple's element types, keying the
    /// memoization of [`Self::synthesize_tuple_drop_fn_te`].
    fn tuple_te_sig(elems: &[crate::ast::TypeExpr]) -> String {
        elems
            .iter()
            .map(Self::type_expr_sig)
            .collect::<Vec<_>>()
            .join("_")
    }

    fn type_expr_sig(te: &crate::ast::TypeExpr) -> String {
        use crate::ast::{GenericArg, TypeKind};
        match &te.kind {
            TypeKind::Path(p) => {
                let base = p
                    .segments
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "x".to_string());
                // B-2026-06-14-1: the memoization key MUST fold in the generic
                // args. `Map[i64,i64]` and `Map[String,i64]` share the base
                // segment `Map` (and the LLVM type `{i64,i64}`), but
                // `emit_tuple_elem_drops` frees them with DIFFERENT
                // `map_drop_flags` ((0,0) vs (1,0)) — keying on the base alone
                // aliased the two drop fns, so whichever map shape was
                // synthesized first silently dropped the other's heap keys/vals
                // (a scalar-first program leaked a later `Map[String,_]`'s keys;
                // a String-first program ran drop_key=1 over a scalar map — the
                // #23 garbage-free). Recurse into the args so the sig is
                // shape-exact.
                match &p.generic_args {
                    Some(args) if !args.is_empty() => {
                        let inner = args
                            .iter()
                            .map(|a| match a {
                                GenericArg::Type(t) => Self::type_expr_sig(t),
                                _ => "x".to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join("_");
                        format!("{base}_g{inner}g")
                    }
                    _ => base,
                }
            }
            TypeKind::Tuple(e) => format!("t{}t", Self::tuple_te_sig(e)),
            _ => "x".to_string(),
        }
    }

    /// Queue a scope-exit heap-field drop for an owned tuple binding
    /// (`let t = (i, f"x")`). The named-struct `track_struct_var` can't cover a
    /// tuple (no type name), so a let-bound tuple's String/Vec field had no
    /// drop and leaked (B-2026-06-11-4 part a). Synthesizes (or reuses) the
    /// aggregate drop fn and registers it via the existing `StructDrop` action
    /// — so the move-suppression (`suppress_source_vec_cleanup_for_arg`) and
    /// drain machinery treat a tuple binding exactly like a named-struct one.
    /// No-op (nothing queued) when the tuple owns no heap.
    pub(super) fn track_tuple_var(
        &mut self,
        tuple_alloca: PointerValue<'ctx>,
        agg_ty: StructType<'ctx>,
    ) {
        if let Some(drop_fn) = self.synthesize_aggregate_drop_fn(agg_ty) {
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(CleanupAction::StructDrop {
                    struct_alloca: tuple_alloca,
                    drop_fn,
                });
            }
        }
    }

    /// Atomic counterpart to `emit_rc_inc` for `arc_values`-promoted bindings.
    /// `atomicrmw add refcount, 1, seq_cst`. Mirrors the non-atomic helper's
    /// shape exactly — same `struct_gep` to land on the refcount field, same
    /// `+1`-by-i64 — only the load+arith+store sequence changes to a single
    /// `atomicrmw` op. Memory ordering is `SequentiallyConsistent` for v1
    /// (correct, conservative); relaxation to `Monotonic`+`Acquire`/`Release`
    /// per Rust's `Arc` is a future optimization tracked under "out of scope"
    /// in the slice plan. The returned old value is discarded — increments do
    /// not need to observe it (only decrements do, to detect transition to 0).
    pub(super) fn emit_arc_inc(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "arc_ptr")
            .unwrap();
        let one = self.context.i64_type().const_int(1, false);
        self.builder
            .build_atomicrmw(
                AtomicRMWBinOp::Add,
                rc_ptr,
                one,
                AtomicOrdering::SequentiallyConsistent,
            )
            .unwrap();
    }

    /// Atomic counterpart to `emit_rc_dec`. Uses `atomicrmw sub refcount, 1,
    /// seq_cst`; the returned value is the *previous* refcount, so the
    /// "drop-to-zero" check is `old == 1` (post-decrement value is 0). Same
    /// branch shape as `emit_rc_dec`: a `free_bb` that calls `free(ptr)` and
    /// a `done_bb` join.
    pub(super) fn emit_arc_dec(&self, heap_type: StructType<'ctx>, ptr: PointerValue<'ctx>) {
        let rc_ptr = self
            .builder
            .build_struct_gep(heap_type, ptr, 0, "arc_ptr")
            .unwrap();
        let one = self.context.i64_type().const_int(1, false);
        let old = self
            .builder
            .build_atomicrmw(
                AtomicRMWBinOp::Sub,
                rc_ptr,
                one,
                AtomicOrdering::SequentiallyConsistent,
            )
            .unwrap();

        let is_last = self
            .builder
            .build_int_compare(IntPredicate::EQ, old, one, "arc_is_last")
            .unwrap();

        let current_fn = self.current_fn.unwrap();
        let free_bb = self.context.append_basic_block(current_fn, "arc_free");
        let done_bb = self.context.append_basic_block(current_fn, "arc_done");

        self.builder
            .build_conditional_branch(is_last, free_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(free_bb);
        // Mirror `emit_rc_dec`'s drop-fn dispatch on the atomic
        // path. The drop fn body uses non-atomic field walks
        // internally — the last decrement happens HERE (atomicrmw
        // sub), so once we're inside `free_bb` we hold the unique
        // reference and the walk runs on a non-shared memory view.
        let mut dropped = false;
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                if let Some(Some(drop_fn)) = self.rc_drop_fns.get(name) {
                    self.builder
                        .build_call(*drop_fn, &[ptr.into()], "")
                        .unwrap();
                    dropped = true;
                }
                break;
            }
        }
        if !dropped {
            self.builder
                .build_call(self.free_fn, &[ptr.into()], "")
                .unwrap();
        }
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
    }

    /// True when `heap_type` is the heap layout of a `par struct` / `par enum`
    /// (always Arc, registered in `shared_types` with `is_par = true`). Its
    /// refcount header must be mutated atomically because `par` values cross
    /// task boundaries. Looked up by heap-type identity — each registered
    /// reference-semantic type has a unique `heap_type`.
    pub(super) fn heap_type_is_par(&self, heap_type: StructType<'ctx>) -> bool {
        self.shared_types
            .values()
            .any(|info| info.is_par && info.heap_type == heap_type)
    }

    /// Dispatch an inc on a refcount keyed purely on the heap type: atomic
    /// (`emit_arc_inc`) when `heap_type` is a `par` type, plain otherwise. Use
    /// at sites that hold a heap pointer but no source binding name (e.g. an
    /// inner handle reached through a field / `Option` / collection element) —
    /// the inner value may still be shared with another task, so a `par` inner
    /// must be incremented atomically.
    /// True when `heap_type`'s surface type uses the headerless layout in
    /// the current fn — it has NO rc word, so ANY count op would corrupt
    /// its first user field (`val` at offset 0). A universal backstop:
    /// the four `emit_refcount_*` dispatchers no-op on such types, so a
    /// count op that slipped past the cluster-role skips (e.g. a reshaper
    /// body that poisons as a cluster but whose member type is
    /// program-wide headerless) is harmless instead of a silent
    /// first-field corruption. Sound because a headerless value never has
    /// a header to inc/dec.
    pub(super) fn heap_type_is_headerless(&self, heap_type: StructType<'ctx>) -> bool {
        self.struct_name_for_heap_type(heap_type)
            .is_some_and(|n| self.headerless_here(&n))
    }

    pub(super) fn emit_refcount_inc_by_type(
        &self,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.heap_type_is_headerless(heap_type) {
            return;
        }
        if self.heap_type_is_par(heap_type) {
            self.emit_arc_inc(heap_type, ptr);
        } else {
            self.emit_rc_inc(heap_type, ptr);
        }
    }

    /// Dispatch a dec on a refcount keyed purely on the heap type: atomic
    /// (`emit_arc_dec`) when `heap_type` is a `par` type, plain otherwise. See
    /// [`Self::emit_refcount_inc_by_type`]. Critically, the drop-walk of a
    /// reference-semantic object decrements the INNER handles it owns — and a
    /// `par` inner handle may still be live in another task even when the outer
    /// object hit refcount 0, so that inner dec must be atomic.
    pub(super) fn emit_refcount_dec_by_type(
        &self,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.heap_type_is_headerless(heap_type) {
            return;
        }
        if self.heap_type_is_par(heap_type) {
            self.emit_arc_dec(heap_type, ptr);
        } else {
            self.emit_rc_dec(heap_type, ptr);
        }
    }

    /// Dispatch an inc on `name`'s refcount. The atomic path (`emit_arc_inc`)
    /// fires when the type is a `par struct` / `par enum` (always Arc) OR the
    /// binding was Arc-promoted by the ownership pass (`arc_fallback_fns` for
    /// the current function); plain non-atomic otherwise.
    pub(super) fn emit_refcount_inc(
        &self,
        name: &str,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.heap_type_is_headerless(heap_type) {
            return;
        }
        if self.heap_type_is_par(heap_type) || self.is_arc_binding(name) {
            self.emit_arc_inc(heap_type, ptr);
        } else {
            self.emit_rc_inc(heap_type, ptr);
        }
    }

    /// Dispatch a dec on `name`'s refcount. Atomic for `par` types (always Arc)
    /// or Arc-promoted bindings (`arc_fallback_fns`); plain non-atomic otherwise.
    pub(super) fn emit_refcount_dec(
        &self,
        name: &str,
        heap_type: StructType<'ctx>,
        ptr: PointerValue<'ctx>,
    ) {
        if self.heap_type_is_headerless(heap_type) {
            return;
        }
        if self.heap_type_is_par(heap_type) || self.is_arc_binding(name) {
            self.emit_arc_dec(heap_type, ptr);
        } else {
            self.emit_rc_dec(heap_type, ptr);
        }
    }

    /// Track a shared-type variable for scope-exit rc_dec.
    ///
    /// See `null_init_slot_in_entry_block` for the null-init step that
    /// has to fire AFTER the slot exists in `self.variables` (which
    /// happens at `bind_pattern` time, after this function returns in
    /// the let-stmt flow). The caller in `compile_stmt` re-fetches the
    /// slot after bind_pattern and calls `null_init_slot_in_entry_block`
    /// directly.
    /// Reverse-lookup a shared struct's surface name from its heap
    /// `StructType`. Used by `track_rc_var` / `track_rc_option_var`
    /// to drive the lazy synth of `__karac_rc_drop_<Name>`. O(n) over
    /// `shared_types`; cheap in practice (small number of shared
    /// types per program) and only runs at let-binding time, not on
    /// the hot scope-exit path.
    pub(super) fn struct_name_for_heap_type(&self, heap_type: StructType<'ctx>) -> Option<String> {
        for (name, info) in &self.shared_types {
            if info.heap_type == heap_type {
                return Some(name.clone());
            }
        }
        None
    }

    pub(super) fn track_rc_var(
        &mut self,
        name: &str,
        ptr: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        // Lazy-synth the recursive drop fn for this shared struct's
        // heap type. Without this, `emit_rc_dec`'s reverse-lookup
        // would never find a registered drop fn and the recursive
        // chain leaks (closes the LeetCode #2 kata bench). The
        // synthesis builds an idempotent fn — repeated `track_rc_var`
        // calls for the same type return the cached entry.
        if let Some(struct_name) = self.struct_name_for_heap_type(heap_type) {
            let _ = self.emit_shared_struct_rc_drop_fn(&struct_name);
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::RcDec {
                name: name.to_string(),
                ptr,
                heap_type,
            });
        }
    }

    /// Phase-B1 cluster-root sibling of `track_rc_var`: queues the
    /// link-following free-walk. The member's recursive drop fn is
    /// still lazily synthesized — fresh-node and cursor bindings keep
    /// their standard `RcDec` cleanups (B1 elides the ROOT's walk
    /// only), and displaced/orphaned nodes drop through the normal
    /// path during the build.
    pub(super) fn track_cluster_root_var(
        &mut self,
        name: &str,
        ptr: PointerValue<'ctx>,
        member_type: &str,
        link_field_index: usize,
    ) {
        let _ = self.emit_shared_struct_rc_drop_fn(member_type);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeClusterWalk {
                name: name.to_string(),
                ptr,
                member_type: member_type.to_string(),
                link_field_index,
            });
        }
    }

    /// Phase C1c adopted-root sibling of `track_rc_option_var`: queues
    /// the Option-tag-guarded link-following free-walk instead of the
    /// `RcDecOption` dec-walk. The member's recursive drop fn is still
    /// lazily synthesized for the non-niche defensive fallback (which
    /// degrades to the RcDecOption shape, behavior-preserving).
    pub(super) fn track_adopted_cluster_root_var(
        &mut self,
        name: &str,
        option_slot: PointerValue<'ctx>,
        option_ty: StructType<'ctx>,
        member_type: &str,
        link_field_index: usize,
    ) {
        let _ = self.emit_shared_struct_rc_drop_fn(member_type);
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeClusterWalkOption {
                name: name.to_string(),
                option_slot,
                option_ty,
                member_type: member_type.to_string(),
                link_field_index,
                some_tag,
            });
        }
    }

    /// RC-elided sibling of `track_rc_var` (ownership phase-A elision):
    /// queues an unconditional null-guarded `free` instead of the
    /// dec/zero-test/drop dance. No drop-fn synthesis — elision-eligible
    /// types have no heap-owning fields, so there is nothing to walk.
    pub(super) fn track_elided_shared_var(&mut self, name: &str, ptr: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeSharedElided {
                name: name.to_string(),
                ptr,
            });
        }
    }

    /// Emit a `store null, slot` at the top of the current function's
    /// entry block (after any allocas, before any body code). Used by
    /// `track_rc_var` to ensure body-local shared-struct slots whose
    /// let-binding may not execute carry a defined null sentinel by the
    /// time scope cleanup runs.
    pub(super) fn null_init_slot_in_entry_block(&self, slot: PointerValue<'ctx>) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let Some(entry) = fn_val.get_first_basic_block() else {
            return;
        };
        let b = self.context.create_builder();
        // Position at end of entry block — after any allocas, but
        // before any non-alloca instructions that compile_function
        // emits (parameter copies, RC fallback boxing, etc.). Per LLVM
        // SSA discipline allocas in the entry block precede other ops,
        // so a store at end-of-entry-block runs before the body's
        // first basic-block branch.
        match entry.get_terminator() {
            Some(term) => b.position_before(&term),
            None => b.position_at_end(entry),
        }
        let null = self.context.ptr_type(AddressSpace::default()).const_null();
        let _ = b.build_store(slot, null);
    }

    /// Track an `Option[shared T]` binding for scope-exit rc_dec of its
    /// inner pointer. Mirrors `track_rc_var` but operates on the Option
    /// struct's `{tag, w0, ...}` shape: cleanup loads the tag, branches
    /// on `Some`, and when Some recovers the inner heap pointer from
    /// `w0` (i64 → ptr) before dispatching through `emit_refcount_dec`.
    /// Closes the kata-bench leak: `let out: Option[ShareT] = call();`
    /// (and the same shape via inferred annotation) now drops the
    /// chain's head ref on scope exit. See `CleanupAction::RcDecOption`
    /// for the runtime IR shape.
    pub(super) fn track_rc_option_var(
        &mut self,
        name: &str,
        option_slot: PointerValue<'ctx>,
        option_ty: StructType<'ctx>,
        heap_type: StructType<'ctx>,
    ) {
        // Lazy-synth the recursive drop fn for the inner shared
        // struct's heap type. Same rationale as `track_rc_var`'s
        // synth call; the cleanup arm's `emit_refcount_dec` will
        // dispatch through the cached drop fn for transitive
        // refcount management.
        if let Some(struct_name) = self.struct_name_for_heap_type(heap_type) {
            let _ = self.emit_shared_struct_rc_drop_fn(&struct_name);
        }
        // Record the inner heap layout so the `Assign` arm in
        // `compile_stmt` can perform refcount-aware reassignment of
        // an `Option[shared T]` variable (dec the old inner ptr,
        // inc the new one unless the RHS is a fresh `Some(...)`).
        // Mirrors the plain shared-T Assign arm's behavior, scaled
        // up to the Option-wrapped shape. Without this, a `mut
        // Option[shared T]` binding's reassignment (`next_a =
        // n.next;` in the LeetCode #2 recursive variant) strands
        // the old ref and over-decrements at scope exit, freeing
        // an aliased chain mid-recursion.
        self.var_option_shared_heap
            .insert(name.to_string(), heap_type);
        // Resolve the Some-tag from the seeded Option layout. Defaults
        // to 1 if (impossibly) the table is missing — matches the
        // canonical `seed_builtin_enum_layouts` numbering.
        let some_tag = self
            .enum_layouts
            .get("Option")
            .and_then(|l| l.tags.get("Some").copied())
            .unwrap_or(1);
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::RcDecOption {
                name: name.to_string(),
                option_slot,
                option_ty,
                heap_type,
                some_tag,
            });
        }
    }

    /// `Result[shared T, E]` sibling of [`Self::track_rc_option_var`]
    /// (B-2026-07-12-24). The seeded generic `Result` layout carries all-`None`
    /// drop kinds, so a `Result` binding whose `Ok`/`Err` payload is a `shared`
    /// (RC) type gets no scope-exit rc-dec — a value that arrived owning a +1
    /// (a call return, a `v[i]` deep-clone, a fresh `Ok(node)`) leaks its
    /// payload node once per binding. Register a `CleanupAction::RcDecOption`
    /// (the action is tag-parameterized — the same reload-slot / tag-guard /
    /// word-1 inner-ptr / `emit_refcount_dec` shape works for Result's wider
    /// `{tag, w0..w4}` struct) for each arm that names a shared type, keyed on
    /// that arm's tag (`Ok` and/or `Err`). No-op for a non-shared `Result`
    /// (`result_arms_shared_type_for_type_expr` returns `None`) or a non-Result
    /// `te`, so callers can invoke it unconditionally alongside the inline-heap
    /// registrar.
    ///
    /// Unlike `track_rc_option_var`, this does NOT record a reassignment table
    /// (`var_option_shared_heap` has no Result analog), so a `mut` Result[shared]
    /// binding reassigned mid-scope leaks the OLD payload (the plain-store
    /// overwrite is not rc-aware) — a leak, never a double-free (the scope-exit
    /// dec releases whatever value is live in the slot at exit). Reassignment
    /// coverage is a deliberate follow-up; the common bind-once shapes (the
    /// B-24 leak class) are fully covered.
    pub(super) fn track_rc_result_var(
        &mut self,
        var_name: &str,
        result_slot: PointerValue<'ctx>,
        result_te: &TypeExpr,
    ) {
        let Some((ok_shared, err_shared)) = self.result_arms_shared_type_for_type_expr(result_te)
        else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Result") else {
            return;
        };
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        // Nested-block let: zero the slot in the entry block so a not-taken
        // path's `undef` tag can't spuriously match `Ok`/`Err` at a function-
        // level drain and dec a garbage pointer. Mirrors the Option / inline
        // Result paths.
        let is_nested = self
            .current_fn
            .and_then(|f| f.get_first_basic_block())
            .zip(self.builder.get_insert_block())
            .map(|(entry, cur)| entry != cur)
            .unwrap_or(false);
        if is_nested {
            self.zero_init_option_slot_in_entry_block(result_slot, result_ty);
        }
        // Lazy-synth each shared arm's recursive drop fn (same rationale as
        // `track_rc_option_var`) and queue one tag-guarded RcDecOption per
        // shared arm. The tag guard means only the live arm's dec fires.
        for (tag, arm) in [(ok_tag, &ok_shared), (err_tag, &err_shared)] {
            let Some((struct_name, info)) = arm else {
                continue;
            };
            let _ = self.emit_shared_struct_rc_drop_fn(struct_name);
            if let Some(frame) = self.scope_cleanup_actions.last_mut() {
                frame.push(CleanupAction::RcDecOption {
                    name: var_name.to_string(),
                    option_slot: result_slot,
                    option_ty: result_ty,
                    heap_type: info.heap_type,
                    some_tag: tag,
                });
            }
        }
    }

    /// Queue a scope-exit free of the heap box backing an enum binding
    /// whose payload `T` was too wide to inline (`Option[Wide]` /
    /// `Result[Wide, _]` — see `coerce_to_payload_words`'s boxing path).
    /// `payload_variant` is the discriminant that carries the box (`Some`
    /// / `Ok`); `inner_struct_name`, when `Some`, names the boxed struct
    /// so its `__karac_drop_struct_<T>` field cleanup runs before the box
    /// is freed (skipped when `T` is all-inline). Non-shared analogue of
    /// `track_rc_option_var`.
    pub(super) fn track_boxed_enum_var(
        &mut self,
        name: &str,
        enum_slot: PointerValue<'ctx>,
        enum_name: &str,
        payload_variant: &str,
        inner_struct_name: Option<&str>,
    ) {
        let inner_drop_fn = inner_struct_name.and_then(|n| self.emit_struct_drop_synthesis(n));
        self.track_boxed_enum_var_with_inner_drop(
            name,
            enum_slot,
            enum_name,
            payload_variant,
            inner_drop_fn,
        );
    }

    /// Peer of [`track_boxed_enum_var`] that takes the boxed payload's inner
    /// drop fn already resolved, rather than deriving it from a user-struct
    /// name. Needed when the boxed payload is itself a nested `Option[shared T]`
    /// (`Vec[Option[shared]].pop()` → `Option[Option[shared]]`,
    /// B-2026-07-12-4): the inner drop is the `Option[T]` element drop
    /// (`emit_option_drop_fn`), not a `__karac_drop_<Struct>`. Without it the
    /// box-free was shallow (freed the enum box, never dec'd the boxed node's
    /// rc) — the pop-consume leak half of B-2026-07-12-4.
    pub(super) fn track_boxed_enum_var_with_inner_drop(
        &mut self,
        name: &str,
        enum_slot: PointerValue<'ctx>,
        enum_name: &str,
        payload_variant: &str,
        inner_drop_fn: Option<FunctionValue<'ctx>>,
    ) {
        let (enum_ty, some_tag) = match self.enum_layouts.get(enum_name) {
            Some(l) => (
                l.llvm_type,
                l.tags.get(payload_variant).copied().unwrap_or(1),
            ),
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::BoxedEnumDrop {
                name: name.to_string(),
                enum_slot,
                enum_ty,
                inner_drop_fn,
                some_tag,
            });
        }
        // Track the binding so a whole-value move into a struct-literal /
        // enum-variant field can neutralize this box drop (the move target
        // becomes the box's sole owner) — see
        // `suppress_inline_option_result_binding_move`.
        self.boxed_enum_payload_vars.insert(name.to_string());
    }

    /// Zero-init an `Option[T]` slot at the top of the current
    /// function's entry block. Mirrors `null_init_slot_in_entry_block`'s
    /// shape but operates on the full Option struct (`{tag, w0, w1,
    /// w2}`) — `store zeroinitializer`, which puts tag=0 (None) in the
    /// slot. Used by the let-stmt handler for nested-block
    /// `Option[shared T]` lets whose bind_pattern store may not fire
    /// at runtime (loop body skipped, branch not taken); without this,
    /// the cleanup arm reads `undef` as the tag and may dispatch on a
    /// garbage Some-tag path.
    pub(super) fn zero_init_option_slot_in_entry_block(
        &self,
        slot: PointerValue<'ctx>,
        option_ty: StructType<'ctx>,
    ) {
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let Some(entry) = fn_val.get_first_basic_block() else {
            return;
        };
        let b = self.context.create_builder();
        match entry.get_terminator() {
            Some(term) => b.position_before(&term),
            None => b.position_at_end(entry),
        }
        let _ = b.build_store(slot, option_ty.const_zero());
    }

    /// Track a Vec/String alloca for scope-exit buffer free. Pass the
    /// element LLVM type (`vec_elem_types[var_name]`) so the cleanup loop
    /// can recursively drop nested heap-owning element types — critical
    /// for `Vec[Vec[T]]`, `Vec[String]`, `Vec[Map[K, V]]`, etc., where the
    /// outer buffer's free does not reach the inner allocations.
    pub(super) fn track_vec_var(
        &mut self,
        vec_alloca: PointerValue<'ctx>,
        elem_ty: Option<BasicTypeEnum<'ctx>>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty,
                elem_is_tensor: false,
                elem_map_drop: None,
                elem_agg_drop: None,
            });
        }
    }

    /// Track a `Vec[<user struct/enum>]` alloca for scope-exit cleanup:
    /// run each live element's synthesized `__karac_drop_<T>` (which frees
    /// every heap-bearing field — Vec/String, Map/Set, **and** enum payloads
    /// — cap-guarded) before releasing the outer buffer. The inline
    /// type-driven recursion in the `FreeVecBuffer` drain only reaches
    /// elements that are *themselves* Vec/String or that have a *direct*
    /// Vec/String field; a `Vec[Span]` where `Span` carries a `Tok` enum
    /// leaked the enum payload of every element (B-2026-06-12-6 cluster 2
    /// gap 2). Routing through the struct's own drop fn is strictly more
    /// complete, so it **supersedes** the inline paths (the drain treats
    /// `elem_agg_drop` as exclusive — running both would double-free the
    /// direct heap fields). `elem_ty` is the element's LLVM struct/enum type,
    /// carried for the per-element GEP stride. The drop fn must be threaded
    /// from a dispatch site holding the element `TypeExpr`
    /// (`vec_elem_agg_drop_for_type_expr`) — reverse-lookup by LLVM type is
    /// unsafe (anonymous by-shape struct types collide).
    pub(super) fn track_vec_of_aggs_var(
        &mut self,
        vec_alloca: PointerValue<'ctx>,
        elem_ty: BasicTypeEnum<'ctx>,
        agg_drop: inkwell::values::FunctionValue<'ctx>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty: Some(elem_ty),
                elem_is_tensor: false,
                elem_map_drop: None,
                elem_agg_drop: Some(agg_drop),
            });
        }
    }

    /// Track a `Vec[Map[K,V]]` / `Vec[Set[T]]` alloca for scope-exit
    /// cleanup: free each live element's map handle (via
    /// `emit_free_one_map_handle`, the same K/V-classified drop a standalone
    /// Map binding uses), then the outer buffer (guarded by `cap > 0` so a
    /// moved-out Vec skips both). A Map handle is a bare `ptr`; the
    /// `elem_map_drop` payload (not the LLVM type) carries the intent, exactly
    /// as `track_vec_of_tensors_var` does for tensor elements. This is what
    /// makes the Vec the OWNER of its map elements — the precondition for the
    /// move-into-Vec ownership transfer (`suppress_map_cleanup_for_tail_identifier`
    /// at the push site) to be leak-free rather than a premature-free / UAF.
    pub(super) fn track_vec_of_maps_var(
        &mut self,
        vec_alloca: PointerValue<'ctx>,
        map_elem_drop: crate::codegen::state::MapElemDrop<'ctx>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty: Some(self.context.ptr_type(AddressSpace::default()).into()),
                elem_is_tensor: false,
                elem_map_drop: Some(map_elem_drop),
                elem_agg_drop: None,
            });
        }
    }

    /// Track a `Vec[Tensor]` alloca for scope-exit cleanup: free each
    /// live element's `[rank][dims][data]` block, then the outer buffer
    /// (guarded by `cap > 0` so a moved-out Vec — `cap` zeroed by the
    /// move-suppression path — skips both). The element LLVM type is a
    /// `ptr`; the `elem_is_tensor` flag (not the type) drives the
    /// per-element free, since a `ptr` element can't be told apart from a
    /// Map handle / borrow by type alone. Used for the `iter_axis`
    /// result Vec (`src/codegen/tensor.rs`).
    pub(super) fn track_vec_of_tensors_var(&mut self, vec_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty: Some(self.context.ptr_type(AddressSpace::default()).into()),
                elem_is_tensor: true,
                elem_map_drop: None,
                elem_agg_drop: None,
            });
        }
    }

    /// Free a single live map/set handle with its K/V drop classification —
    /// the shared single-handle free shared by the `FreeMapHandle` cleanup
    /// (one map binding) and the `Vec[Map]`/`Vec[Set]` element-drop loop
    /// (`elem_map_drop`). Runs the shared-half rc_dec walks (which read live
    /// bucket bytes and so MUST precede the bucket-storage release) then
    /// routes to `karac_map_free_with_drop_vec` when either half owns
    /// Vec/String heap, else plain `karac_map_free`. May split the current
    /// block (the shared-half walk is a bucket loop); callers that emit after
    /// it should re-read the insertion block.
    pub(super) fn emit_free_one_map_handle(
        &self,
        handle: PointerValue<'ctx>,
        drop: &crate::codegen::state::MapElemDrop<'ctx>,
    ) {
        if let Some(heap_ty) = drop.val_shared_heap_type {
            self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, true);
        }
        if let Some(heap_ty) = drop.key_shared_heap_type {
            self.emit_map_shared_half_rc_dec_walk(handle, heap_ty, false);
        }
        // Slice 3r (deferred gap (d)): a synthesized per-VALUE drop fn
        // (`karac_drop_<V>(ptr)`) owns the whole value-side release —
        // route through `karac_map_free_with_val_drop_fn`, keeping the
        // key side on the flag contract. Mutually exclusive with
        // `val_is_vec` / `val_shared_heap_type` by construction
        // (`map_val_drop_fn_for_type_expr` returns None for those).
        if let Some(val_fn) = drop.val_drop_fn {
            let i32_t = self.context.i32_type();
            let key_flag = i32_t.const_int(if drop.key_is_vec { 1 } else { 0 }, false);
            let fn_ptr = val_fn.as_global_value().as_pointer_value();
            self.builder
                .build_call(
                    self.karac_map_free_with_val_drop_fn_fn,
                    &[handle.into(), key_flag.into(), fn_ptr.into()],
                    "",
                )
                .unwrap();
            return;
        }
        if drop.key_is_vec || drop.val_is_vec {
            let i32_t = self.context.i32_type();
            let key_flag = i32_t.const_int(if drop.key_is_vec { 1 } else { 0 }, false);
            let val_flag = i32_t.const_int(if drop.val_is_vec { 1 } else { 0 }, false);
            self.builder
                .build_call(
                    self.karac_map_free_with_drop_vec_fn,
                    &[handle.into(), key_flag.into(), val_flag.into()],
                    "",
                )
                .unwrap();
        } else {
            self.builder
                .build_call(self.karac_map_free_fn, &[handle.into()], "")
                .unwrap();
        }
    }

    /// General owned-temporary chokepoint (phase-6 line-489/497 unblocker —
    /// see `docs/spikes/general-owned-temp-tracking.md`). Given a freshly
    /// produced rvalue `val` and the `(offset, length)` span of the
    /// expression that produced it, queue the matching scope-exit cleanup on
    /// the **current** frame so the temporary drops when that frame drains
    /// (the same LIFO drain block locals use). Returns the temp slot when one
    /// was created, for callers that need its address (`None` for RC boxes —
    /// there is no slot — and for any value that is not a tracked owned
    /// temporary, e.g. a borrow `ptr`-ABI return or a primitive scalar).
    ///
    /// Three kinds are handled:
    /// - **Vec / String** (`{ptr, len, cap}`) — detectable from the LLVM
    ///   value type alone, so this fires even without a hint-table entry
    ///   (preserving slice-1 behavior). When `owned_temp_drops` carries the
    ///   producing expression's `TypeExpr`, the element type is recovered and
    ///   threaded to `track_vec_var` — closing the nested-heap leak slice 1's
    ///   `None` left open (`Vec[String]` / `Vec[Vec[T]]` inner buffers).
    /// - **Map / Set handle** — a plain pointer, indistinguishable from any
    ///   other heap pointer by LLVM type; recognized only via the hint
    ///   table's `Map[K, V]` / `Set[T]` `TypeExpr`, from which the per-half
    ///   Vec/shared classification is derived exactly as the let-binding path
    ///   does (`map_temp_cleanup_parts`).
    /// - **Shared-struct / shared-enum RC box** — also a plain pointer; the
    ///   hint table's `TypeExpr` head names the shared type, so its heap
    ///   layout is looked up in `shared_types` and an `rc_dec` queued.
    ///
    /// This is the single seam unnamed owned temporaries funnel through,
    /// replacing ad-hoc `track_vec_var(temp, _)` calls (e.g. the
    /// `ref_rvalue_arg` materialization in `call_dispatch.rs`, a later-slice
    /// migration candidate).
    ///
    /// Free a fresh-owned `String` temporary passed *by borrow* to a method
    /// that reads then discards it — `buffer.push_str(s.substring(a, b))`,
    /// `keyword.contains(s.substring(a, b))`, `name.starts_with(tok)`. These
    /// methods copy/scan the argument's bytes but take no ownership, so a
    /// freshly-malloc'd argument (a `substring`, a `String`-returning call)
    /// would leak its buffer once per call — unbounded in a loop. Emit a
    /// `cap > 0`-guarded `free` of the argument's buffer at the *current*
    /// insert position; the caller must first position the builder at the
    /// post-use merge block so every read of the buffer dominates the free.
    ///
    /// Gated on `expr_yields_fresh_owned_temp` (Call / MethodCall, not
    /// borrow-returning) **or** `expr_is_fresh_owned_string_slice` (a
    /// `String[a..b]` range-index slice, which `compile_string_slice` allocates
    /// fresh just like `.substring`) so a string literal, a `ref String`
    /// identifier, a place expression (`out[k]`), or a borrow-returning call is
    /// never freed — those are owned elsewhere and a free here would
    /// double-free. The `cap > 0` guard is a second backstop: a static-literal
    /// String and a borrowed (cap == 0) view own no heap. A `String` buffer is
    /// flat bytes, so a single `free` is the complete drop. Surfaced by
    /// kata-katas #722 remove-comments — the self-hosted lexer's `token_text`
    /// extraction and keyword-membership surface; the range-slice arm closes
    /// B-2026-06-12-5 (`buffer.push_str(src[a..b])` leaked the slice temp).
    pub(super) fn free_fresh_owned_str_arg(
        &mut self,
        arg: &crate::ast::Expr,
        val: BasicValueEnum<'ctx>,
    ) {
        // Three fresh-owned-String shapes flow here, all freed identically: a
        // direct `Call`/`MethodCall` result (#20), a `String[a..b]` range slice
        // (B-2026-06-12-5), and an inline-temp-Vec heap-element index
        // (`names()[0]` — B-2026-06-14-32: the deep clone
        // `compile_inline_temp_vec_index` mints has no consuming binding). The
        // `cap > 0` guard below no-ops on a borrowed (cap == 0) view, so a place
        // expression / rodata literal is never double-freed.
        // B-2026-07-21-12: a string concat left as a surface `Binary` (the
        // `String.add` desugar skips ref-typed operands) is as fresh-owned as
        // the desugared Call — admit it so consumer sites (print args, push
        // args, map keys, …) free the concat result exactly like the Call
        // route. The `llvm_ty_is_vec_struct` + `cap > 0` guards in the free
        // core make a scalar/vector `+` a no-op.
        let is_string_concat_binary = matches!(
            &arg.kind,
            ExprKind::Binary {
                op: crate::ast::BinOp::Add,
                ..
            }
        ) && self.llvm_ty_is_vec_struct(val.get_type());
        if !self.expr_yields_fresh_owned_temp(arg)
            && !self.expr_is_fresh_owned_string_slice(arg)
            && !self.expr_is_inline_temp_vec_heap_index(arg)
            && !is_string_concat_binary
        {
            return;
        }
        self.free_str_vec_buffer_if_heap(val);
    }

    /// Free a `{ptr, len, cap}` String/Vec buffer's heap allocation iff
    /// `cap > 0` (the owned-buffer marker; a borrowed view / rodata literal has
    /// `cap == 0` and is skipped), no-opping on a non-Vec/String-shaped value.
    ///
    /// The compile-time-gate-free core of [`free_fresh_owned_str_arg`]: callers
    /// that have already established the value is a fresh-owned-or-suppressed
    /// buffer (e.g. a moved-binding map key whose source `cap` was zeroed by
    /// `suppress_source_vec_cleanup_for_arg`) route here directly, since the
    /// fresh-temp expression gate would reject an `Identifier` / place-expr key.
    /// The `cap > 0` runtime guard is the sole safety net, exactly as it is for
    /// the fresh-temp path.
    pub(super) fn free_str_vec_buffer_if_heap(&mut self, val: BasicValueEnum<'ctx>) {
        if !self.llvm_ty_is_vec_struct(val.get_type()) {
            return;
        }
        let Some(fn_val) = self.current_fn else {
            return;
        };
        let i64_t = self.context.i64_type();
        let sv = val.into_struct_value();
        let ptr = self
            .builder
            .build_extract_value(sv, 0, "freearg.ptr")
            .unwrap()
            .into_pointer_value();
        let cap = self
            .builder
            .build_extract_value(sv, 2, "freearg.cap")
            .unwrap()
            .into_int_value();
        let heap = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                i64_t.const_zero(),
                "freearg.heap",
            )
            .unwrap();
        let free_bb = self.context.append_basic_block(fn_val, "freearg.free");
        let done_bb = self.context.append_basic_block(fn_val, "freearg.done");
        self.builder
            .build_conditional_branch(heap, free_bb, done_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        self.builder
            .build_call(self.free_fn, &[ptr.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();
        self.builder.position_at_end(done_bb);
    }

    /// Caller obligation: only pass values that are genuinely *fresh-owned*.
    /// A value reloaded from an existing tracked binding (a place expression)
    /// must NOT be routed here — its storage is already owned by the
    /// binding's own cleanup, so a second free/dec would double-free. The
    /// statement-discard call site enforces this with
    /// `expr_yields_fresh_owned_temp` (Call / MethodCall only).
    pub(super) fn materialize_owned_temp(
        &mut self,
        val: BasicValueEnum<'ctx>,
        span_key: (usize, usize),
    ) -> Option<PointerValue<'ctx>> {
        let cur_fn = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())?;

        // Vec / String: LLVM-type detectable on its own. The hint table only
        // *adds* the element type, so a missing entry degrades to slice-1
        // behavior (outer buffer freed, inner elements leak) — never a
        // double-free or a regression.
        if self.llvm_ty_is_vec_struct(val.get_type()) {
            let elem_ty = self
                .owned_temp_drops
                .get(&span_key)
                .cloned()
                .and_then(|te| self.extract_vec_elem_type(&te));
            let slot = self.create_entry_alloca(cur_fn, "__owned_tmp", val.get_type());
            self.builder.build_store(slot, val).unwrap();
            self.track_vec_var(slot, elem_ty);
            return Some(slot);
        }

        // Map handles and RC boxes are both plain pointers — the lowering-pass
        // hint table is the only signal. No entry → not a tracked owned temp
        // (or a kind this slice doesn't handle) → no cleanup.
        let te = self.owned_temp_drops.get(&span_key).cloned()?;
        let head = match &te.kind {
            TypeKind::Path(p) => p.segments.first().map(|s| s.as_str()).unwrap_or(""),
            _ => return None,
        };

        // Map / Set handle: store the handle pointer into an alloca and queue
        // a `FreeMapHandle`, classifying the K/V halves from the `TypeExpr`.
        if head == "Map" || head == "Set" {
            if !val.is_pointer_value() {
                return None;
            }
            let (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn) =
                self.map_temp_cleanup_parts(&te);
            let slot = self.create_entry_alloca(cur_fn, "__owned_tmp", val.get_type());
            self.builder.build_store(slot, val).unwrap();
            self.track_map_var_with_val_drop(
                slot,
                key_is_vec,
                val_is_vec,
                val_shared,
                key_shared,
                val_drop_fn,
            );
            return Some(slot);
        }

        // Shared-struct / shared-enum RC box: a discarded fresh value owns one
        // reference, so a single `rc_dec` at the `;` is the correct drop
        // (refcount → 0 frees via the lazily-synthesized recursive drop fn).
        // `track_rc_var` takes the pointer directly; the one-shot discard
        // frame drains in the same block, so the SSA pointer dominates the dec.
        if let Some(heap_type) = self.shared_types.get(head).map(|i| i.heap_type) {
            if val.is_pointer_value() {
                self.track_rc_var("__owned_tmp", val.into_pointer_value(), heap_type);
            }
            return None;
        }

        None
    }

    /// When `elem_te` is a `Map[K, V]` / `Set[T]` element TypeExpr (the
    /// element type of an enclosing `Vec`), build the per-element drop
    /// classification so the Vec's scope-exit cleanup can free each handle
    /// (`track_vec_of_maps_var`). Returns `None` for any non-map element —
    /// callers fall back to the plain `track_vec_var` path. The K/V
    /// classification is the same `map_temp_cleanup_parts` derivation a
    /// standalone Map binding uses.
    pub(super) fn vec_elem_map_drop_for_type_expr(
        &mut self,
        elem_te: &TypeExpr,
    ) -> Option<crate::codegen::state::MapElemDrop<'ctx>> {
        let head = match &elem_te.kind {
            TypeKind::Path(p) => p.segments.first().map(|s| s.as_str())?,
            _ => return None,
        };
        if head != "Map" && head != "Set" {
            return None;
        }
        let (key_is_vec, val_is_vec, key_shared_heap_type, val_shared_heap_type, val_drop_fn) =
            self.map_temp_cleanup_parts(elem_te);
        Some(crate::codegen::state::MapElemDrop {
            key_is_vec,
            val_is_vec,
            val_shared_heap_type,
            key_shared_heap_type,
            val_drop_fn,
        })
    }

    /// When `elem_te` is a *named user struct or enum* (the element type of an
    /// enclosing `Vec`), synthesize (or reuse) that type's `__karac_drop_<T>`
    /// so the Vec's scope-exit cleanup runs it per element
    /// (`track_vec_of_aggs_var`). This closes B-2026-06-12-6 cluster 2 gap 2:
    /// a `Vec[Span]` where `Span` holds a `Tok` enum field leaked each
    /// element's enum payload — the inline `FreeVecBuffer` recursion only
    /// reaches Vec/String elements or *direct* Vec/String fields, both blind
    /// to the all-i64 enum payload words. The struct/enum drop synthesizers
    /// are the same ones the `StructDrop` / `EnumDrop` actions use, and free
    /// every heap-bearing field cap-guarded.
    ///
    /// Returns `None` for anything that isn't a heap-bearing, non-shared user
    /// struct/enum — builtins (`Vec`/`Map`/`Set`/`String`), `Option`/`Result`
    /// (inline payloads dropped by the let-binding inline-drop machinery, not
    /// a drop switch — routing them here risks a double-free), shared/RC
    /// types (their own synthesizer returns `None`; RC dec is separate), and
    /// no-heap aggregates (the synthesizer returns `None`). Callers fall back
    /// to the plain `track_vec_var` path on `None`.
    pub(super) fn vec_elem_agg_drop_for_type_expr(
        &mut self,
        elem_te: &TypeExpr,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        let name = match &elem_te.kind {
            TypeKind::Path(p) => p.segments.first()?.clone(),
            _ => return None,
        };
        // An `Option[String]` / `Option[Vec[..]]` element (slice 3p): the
        // type-erased `{tag, w0, w1, w2}` layout's generic `EnumDrop` switch
        // can't free the payload (it doesn't know the payload type — it'd be
        // wrong for `Option[i64]`), and B-2026-06-10-6's concrete-typed
        // `FreeInlineOptionPayload` covers only BINDINGS, so a `Some` payload
        // inside a Vec element leaked. Route to the payload-type-aware
        // tag-guarded `karac_drop_Option_<payload>`. Gated to inline
        // {ptr,len,cap} payloads (String / Vec-of-supported) — a scalar
        // payload has no cap word to guard on, and boxed/handle payloads
        // aren't the inline overlay shape. (`Result` gets its own tag-dispatch
        // arm below — slice 3q.)
        if name == "Option" {
            if let TypeKind::Path(p) = &elem_te.kind {
                if let Some(GenericArg::Type(payload)) =
                    p.generic_args.as_ref().and_then(|a| a.first())
                {
                    // B-2026-07-11-33: an `Option[shared T]` element. As a Vec
                    // element the `Option` uses the TAGGED overlay
                    // (`{tag:i64, payload_words}`), NOT the niche pointer, so the
                    // drop must read the tag and, when `Some`, rc-dec the boxed
                    // payload. `emit_option_drop_fn` does exactly that
                    // (tag-guarded, delegating to the payload type's own drop —
                    // an rc-dec for a shared struct/enum). Without this the
                    // shared nodes inside a `Vec[Option[shared]]` leaked (the
                    // Option arm returned `None` → buffer-only Vec cleanup; the
                    // kata-23 merge-k-lists shape).
                    if self.shared_heap_type_for_type_expr(payload).is_some() {
                        return self.emit_option_drop_fn(payload);
                    }
                    if self.option_payload_inline_recursive_drop_ok(payload)
                        || self.option_payload_struct_or_enum_drop_ok(payload)
                    {
                        return self.emit_option_drop_fn(payload);
                    }
                }
            }
            return None;
        }
        // `Result[T, E]` element (slice 3q, the Option sibling): dispatch on
        // the tag and drop the live side's inline payload overlay via
        // `karac_drop_Result_<ok>_<err>`. Gated so every heap-owning side is
        // an inline String/Vec overlay shape and at least one side owns heap
        // (an all-scalar Result stays on the correct heapless fast path).
        if name == "Result" {
            if let TypeKind::Path(p) = &elem_te.kind {
                let arg = |i: usize| -> Option<&TypeExpr> {
                    match p.generic_args.as_ref()?.get(i)? {
                        GenericArg::Type(t) => Some(t),
                        _ => None,
                    }
                };
                if let (Some(ok), Some(err)) = (arg(0), arg(1)) {
                    if self.result_payload_inline_recursive_drop_ok(ok, err) {
                        return self.emit_result_drop_fn(ok, err);
                    }
                }
            }
            return None;
        }
        // B-2026-06-14-28 — a `shared` struct / enum element (`Vec[Expr]`,
        // `Expr` a shared enum — the AST-port sequence-child shape
        // `Call(args: Vec[Expr])`). The slot holds an 8-byte RC pointer, NOT
        // an inline aggregate, so the value-drop fns below are WRONG (they'd
        // walk the slot as a struct/enum value). A shared element needs an
        // rc-dec of its pointer. This check MUST precede the `enum_layouts`
        // one — a shared enum is in `enum_layouts` too, so the old code
        // routed it to `emit_enum_drop_switch` (the value drop), which never
        // decremented the refcount and leaked every element. Synthesize a
        // tiny per-element fn that loads the RC pointer from the slot,
        // null-checks, and rc-dec's via the element's heap layout.
        if let Some(heap_ty) = self.shared_heap_type_for_type_expr(elem_te) {
            return self.emit_vec_elem_rc_dec_fn(&name, heap_ty);
        }
        if self.struct_types.contains_key(&name) {
            // A non-shared struct element that transitively owns `shared` fields
            // (`Vec[CallArg]`, `CallArg` holding a shared `Expr` `value` — the
            // AST-port `Call(CallExpr { args })` shape). The plain value drop
            // skips shared fields by design (a local struct's shared fields are
            // rc-dec'd by its `let` cleanup — B-2026-06-14-28 #3), but a Vec
            // element has no let-cleanup, so the shared field's box leaks once
            // per element. Route it to the combined element drop instead.
            if self.struct_owns_shared_field(&name, &mut Vec::new()) {
                return self.emit_vec_elem_struct_with_shared_drop_fn(&name);
            }
            return self.emit_struct_drop_synthesis(&name);
        }
        if self.enum_layouts.contains_key(&name) {
            return self.emit_enum_drop_switch(&name);
        }
        // A `Vec[Inner]` ELEMENT (i.e. the container is `Vec[Vec[Inner]]` or
        // deeper) whose `Inner` itself owns heap below the buffer level
        // (`Vec[Vec[String]]`, `Vec[Vec[Vec[T]]]`, `Vec[Vec[Map[..]]]`, …). The
        // inline `FreeVecBuffer` vec-struct fast path is ONE level deep: it frees
        // each inner Vec's DATA buffer but treats that buffer's elements as
        // opaque, leaking any heap they own (the innermost String char-buffers).
        // Route to the strictly-recursive `emit_vec_drop_fn`, which drops every
        // level via the `karac_drop_Vec_<elem>` family. Gated two ways so blast
        // radius stays minimal and correctness is guaranteed: (a) `Inner` must
        // actually own heap — a `Vec[Vec[scalar]]` element is correctly handled
        // one-level by the fast path, so it keeps `None` and stays there; and
        // (b) the whole `Inner` subtree must be a shape the recursive drop family
        // fully frees (String / nested Vec / Map / Set / tuple, and — since slice
        // 3o — user struct / enum / shared, whose own drop synthesis the family's
        // named-type arm delegates to). `Option` / `Result` inners remain
        // excluded (the delegate no-ops them), so those stay on the one-level
        // fast path rather than gaining a misleading no-op drop.
        // `VecDeque` rides the same arm (slice 3v) — it shares Vec's linear
        // {ptr,len,cap} layout (`push_front` is a memmove insert at index 0,
        // not a ring buffer), so `emit_vec_drop_fn`'s 0..len walk + buffer
        // free is exact for a `Vec[VecDeque[..]]` element too.
        if name == "Vec" || name == "VecDeque" {
            if let TypeKind::Path(p) = &elem_te.kind {
                if let Some(GenericArg::Type(inner)) =
                    p.generic_args.as_ref().and_then(|a| a.first())
                {
                    if self.te_owns_heap_below_buffer(inner)
                        && self.te_recursive_drop_fully_supported(inner)
                    {
                        return Some(self.emit_vec_drop_fn(inner));
                    }
                }
            }
        }
        None
    }

    /// True iff `te` owns heap that a ONE-level `Vec` buffer free would miss —
    /// i.e. a `Vec[te]` element needs recursive per-element dropping. A bare
    /// scalar owns nothing; a `String` / collection / heap-bearing tuple does.
    /// (A user struct/enum is conservatively "owns heap" but is separately
    /// excluded by `te_recursive_drop_fully_supported`, so it never reaches the
    /// recursive path from here.)
    pub(super) fn te_owns_heap_below_buffer(&self, te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Tuple(elems) => elems.iter().any(|e| self.te_owns_heap_below_buffer(e)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                !matches!(
                    head,
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
            _ => true,
        }
    }

    /// True iff `emit_drop_fn_for_type_expr(te)` fully frees `te`'s heap — the
    /// recursive drop family (`emit_vec_drop_fn` / `emit_map_drop_fn` /
    /// `emit_string_drop_fn` / `emit_tuple_drop_fn`) bottoms out cleanly in
    /// scalar / String / collection / tuple, and — as of slice 3o —
    /// user struct / enum / shared (the family's named-type arm delegates to
    /// `vec_elem_agg_drop_for_type_expr`, which frees value heap fields and
    /// rc-decs shared fields/elements). `Option` / `Result` remain UNCOVERED
    /// (the delegate returns None for them → a no-op drop), so a
    /// `Vec[Vec[Option[String]]]` element stays false and keeps its existing
    /// (one-level fast) path rather than a wrong no-op.
    pub(super) fn te_recursive_drop_fully_supported(&self, te: &TypeExpr) -> bool {
        match &te.kind {
            TypeKind::Tuple(elems) => elems
                .iter()
                .all(|e| self.te_recursive_drop_fully_supported(e)),
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                let arg = |i: usize| -> Option<&TypeExpr> {
                    match p.generic_args.as_ref()?.get(i)? {
                        GenericArg::Type(t) => Some(t),
                        _ => None,
                    }
                };
                match head {
                    // Both String spellings: annotations write `String`; the
                    // typechecker's `type_to_type_expr(Type::Str)` (the source
                    // of every INFERRED binding's TypeExpr) renders `str`.
                    "String" | "str" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32"
                    | "u64" | "usize" | "isize" | "f32" | "f64" | "bool" | "char" => true,
                    "Vec" | "VecDeque" | "Set" | "SortedSet" => {
                        arg(0).is_some_and(|t| self.te_recursive_drop_fully_supported(t))
                    }
                    "Map" | "SortedMap" => {
                        arg(0).is_some_and(|k| self.te_recursive_drop_fully_supported(k))
                            && arg(1).is_some_and(|v| self.te_recursive_drop_fully_supported(v))
                    }
                    // `Option[String]` / `Option[Vec[..]]` (slice 3p): the
                    // tag-guarded `emit_option_drop_fn` frees the inline `Some`
                    // payload, reached via the same named-type delegation.
                    // `Option[shared T]` (B-2026-07-11-29): the same
                    // `emit_option_drop_fn` reads the tag and rc-decs the boxed
                    // shared payload — `vec_elem_agg_drop_for_type_expr` already
                    // routes an `Option[shared]` *element* there, so the
                    // recursive `Vec[Vec[Option[shared]]]` drop is exact too;
                    // without this arm the outer drop fell to the one-level
                    // buffer-only fast path and leaked every shared node inside
                    // the inner Vecs (the #95 shape-DP `shapes` table).
                    // Unsupported payloads (scalar / boxed non-shared / handle /
                    // tuple) stay false.
                    "Option" => arg(0).is_some_and(|t| {
                        self.option_payload_inline_recursive_drop_ok(t)
                            || self.option_payload_struct_or_enum_drop_ok(t)
                            || self.shared_heap_type_for_type_expr(t).is_some()
                    }),
                    // `Result[T, E]` (slice 3q): same delegation shape.
                    "Result" => match (arg(0), arg(1)) {
                        (Some(ok), Some(err)) => {
                            self.result_payload_inline_recursive_drop_ok(ok, err)
                        }
                        _ => false,
                    },
                    // A user struct / enum / shared type: its own drop synthesis
                    // (reached via the `emit_drop_fn_for_type_expr` named-type
                    // delegation) frees every heap field / variant payload, so a
                    // `Vec[..<struct>..]` element recurses correctly. Unknown
                    // names stay false.
                    _ => {
                        self.struct_types.contains_key(head)
                            || self.enum_layouts.contains_key(head)
                            || self.shared_types.contains_key(head)
                    }
                }
            }
            _ => false,
        }
    }

    /// True iff `Option[payload_te]` is a shape `emit_option_drop_fn` handles:
    /// the payload's `{ptr, len, cap}` must OVERLAY the type-erased option's
    /// words w0..w2 inline — i.e. exactly a `String` or a `Vec[..]` whose own
    /// subtree the recursive drop family fully frees. A scalar payload has no
    /// cap word (w2 would be read as garbage), a boxed/wide payload lives
    /// behind a box pointer, and Map/Set handles are single pointers — none
    /// are the inline overlay shape, so they're all excluded.
    /// `Result[T, E]` sibling of the Option gate: true iff at least one side
    /// owns heap (else there is nothing to drop — the heapless fast path is
    /// already exact) and EVERY heap-owning side is an inline
    /// `{ptr,len,cap}`-overlay shape the recursive family fully frees
    /// (String / Vec-of-supported). A heapless side (scalar / unit) is fine —
    /// its arm just emits no drop call.
    pub(super) fn result_payload_inline_recursive_drop_ok(
        &self,
        ok_te: &TypeExpr,
        err_te: &TypeExpr,
    ) -> bool {
        let side_ok = |te: &TypeExpr| {
            !self.te_owns_heap_below_buffer(te)
                || self.option_payload_inline_recursive_drop_ok(te)
                // Slice 3u: struct/enum sides (inline in the 5-word area,
                // or boxed beyond it) — the emitter's per-side branches.
                || self.option_payload_struct_or_enum_drop_ok(te)
        };
        (self.te_owns_heap_below_buffer(ok_te) || self.te_owns_heap_below_buffer(err_te))
            && side_ok(ok_te)
            && side_ok(err_te)
    }

    pub(super) fn option_payload_inline_recursive_drop_ok(&self, payload_te: &TypeExpr) -> bool {
        match &payload_te.kind {
            TypeKind::Path(p) => {
                let head = p.segments.first().map(String::as_str).unwrap_or("");
                match head {
                    // Both spellings — see `te_recursive_drop_fully_supported`.
                    "String" | "str" => true,
                    "Vec" | "VecDeque" => p
                        .generic_args
                        .as_ref()
                        .and_then(|a| a.first())
                        .is_some_and(|a| {
                            matches!(a, GenericArg::Type(t)
                                if self.te_recursive_drop_fully_supported(t))
                        }),
                    _ => false,
                }
            }
            _ => false,
        }
    }

    /// Slice 3u: an Option/Result payload that is a NON-shared user STRUCT
    /// or value ENUM the recursive drop family fully frees. Covers BOTH
    /// widths: an inline payload's i64 words overlay w0.. layout-compatibly
    /// with the type's LLVM aggregate (all 8-byte fields), so the emitters'
    /// in-place GEP path drops it directly; a WIDER-than-area payload was
    /// heap-boxed at pack time and the emitters' 3u boxed branch (load w0,
    /// null-guard, inner drop, free box) owns it. Sibling of
    /// `option_payload_inline_recursive_drop_ok` (the String/Vec overlay
    /// gate); a `false` keeps the status-quo fast path.
    pub(super) fn option_payload_struct_or_enum_drop_ok(&self, payload_te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &payload_te.kind else {
            return false;
        };
        let head = p.segments.first().map(String::as_str).unwrap_or("");
        if self.shared_types.contains_key(head) {
            return false;
        }
        (self.struct_types.contains_key(head) || self.enum_layouts.contains_key(head))
            && self.te_recursive_drop_fully_supported(payload_te)
    }

    /// True iff `field_te` is `Option[P]` with `P` a non-shared user
    /// struct/enum the recursive drop family fully frees — the shape
    /// `track_inline_option_agg_payload_var` registers a leaf drop for
    /// (B-2026-07-03-27). A pure `&self` gate for the destructure-leaf branch.
    pub(super) fn option_field_agg_drop_ok(&self, field_te: &TypeExpr) -> bool {
        let TypeKind::Path(p) = &field_te.kind else {
            return false;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return false;
        }
        matches!(
            p.generic_args.as_ref().and_then(|a| a.first()),
            Some(GenericArg::Type(payload)) if self.option_payload_struct_or_enum_drop_ok(payload)
        )
    }

    /// Synthesize (or fetch) `__karac_vec_elem_full_drop_<S>` — the per-element
    /// drop for a `Vec` whose element is a NON-shared user struct `S` that
    /// transitively owns `shared` fields (e.g. `Vec[CallArg]`, `CallArg`
    /// carrying a shared `Expr`). `emit_struct_drop_synthesis(S)` alone is
    /// INCOMPLETE for a Vec element: it frees `S`'s Vec/String/Map/Set/Option
    /// fields but, by design (B-2026-06-14-28 #3), leaves `shared` fields for
    /// the owner's `let` cleanup to rc-dec — which a Vec element does not have,
    /// so the shared box leaks on every element drop (surfaced by the
    /// self-hosted parser: each call argument's `value: Expr` box leaked).
    ///
    /// The combined drop runs two DISJOINT passes over the same element slot.
    /// Pass 1 is `__karac_drop_struct_<S>` — the value-heap free, which frees
    /// `S`'s String/Vec/Map/enum buffers AND (post-#35) DRAINS every heap-owning
    /// `Vec[T]` field's elements (rc-dec'ing a `Vec[shared]` element, running the
    /// combined/value element drop for a `Vec[struct/enum-with-shared]`), then
    /// frees the buffer. It skips only the DIRECT `shared` / `Option[shared]`
    /// SCALAR fields (classified no-cleanup — a local's shared fields are
    /// rc-dec'd by its `let` cleanup, which a Vec element lacks). Pass 2 is
    /// `emit_nested_struct_shared_rc_decs(.., owns_buffer_free=false)` — it
    /// rc-dec's exactly those direct `shared` / `Option[shared]` scalar fields
    /// pass 1 skipped (and recurses into nested structs for THEIR shared
    /// scalars). Its `Vec[T]`-element drain and buffer frees are gated OFF by
    /// `owns_buffer_free=false`, because pass 1 already did both — re-draining
    /// would double-free (B-2026-07-10-4). Disjoint field coverage ⇒ no
    /// double-free. The shared rc-dec is refcount-safe even when the element is
    /// ALSO consumed by value elsewhere (the consume site rc-incs the shared
    /// handle on its element copy, balancing this dec). Memoized by symbol name.
    fn emit_vec_elem_struct_with_shared_drop_fn(
        &mut self,
        struct_name: &str,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        let fn_name = format!("__karac_vec_elem_full_drop_{struct_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return Some(f);
        }
        // Step-1 value drop first (None when `S`'s only heap IS its shared
        // field — then there is nothing for step 1 to free).
        let value_drop = self.emit_struct_drop_synthesis(struct_name);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn =
            self.module
                .add_function(&fn_name, fn_ty, Some(inkwell::module::Linkage::Internal));
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let slot_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        if let Some(vd) = value_drop {
            self.builder.build_call(vd, &[slot_ptr.into()], "").unwrap();
        }
        self.emit_nested_struct_shared_rc_decs(slot_ptr, struct_name, drop_fn, false);
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;
        Some(drop_fn)
    }

    /// B-2026-06-14-28 — synthesize (or fetch) `__karac_vec_elem_rc_dec_<T>`,
    /// a per-element drop fn for a `Vec` whose element type is `shared T` (an
    /// inline RC pointer). The `FreeVecBuffer` drain calls it with a pointer
    /// to each live element SLOT; the fn loads the RC pointer out of the
    /// slot, null-checks, and rc-dec's via `T`'s heap layout (which fires
    /// `__karac_rc_drop_<T>` and frees the box + recurses into its children
    /// when the count reaches 0). Memoized by symbol name.
    fn emit_vec_elem_rc_dec_fn(
        &mut self,
        type_name: &str,
        heap_ty: StructType<'ctx>,
    ) -> Option<inkwell::values::FunctionValue<'ctx>> {
        let fn_name = format!("__karac_vec_elem_rc_dec_{type_name}");
        if let Some(f) = self.module.get_function(&fn_name) {
            return Some(f);
        }
        // Force-synthesize the element type's recursive RC drop fn FIRST, so
        // the `emit_refcount_dec_by_type` below resolves through
        // `emit_rc_dec`'s `rc_drop_fns` dispatch to it (and recurses into the
        // box's children) rather than falling to a plain `free` that strands
        // them. Without this, a `Vec[Expr]` element's `Add(BinOp)` payload's
        // shared children leaked even though the standalone tree drop frees
        // them (the drop fn just wasn't built yet at Vec-cleanup synth time).
        if let Some(info) = self.shared_types.get(type_name).cloned() {
            if info.is_enum {
                let _ = self.emit_shared_enum_rc_drop_fn(type_name);
            } else {
                let _ = self.emit_shared_struct_rc_drop_fn(type_name);
            }
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn =
            self.module
                .add_function(&fn_name, fn_ty, Some(inkwell::module::Linkage::Internal));
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let slot_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let inner = self
            .builder
            .build_load(ptr_ty, slot_ptr, "vecelem.rc.ptr")
            .unwrap()
            .into_pointer_value();
        let is_null = self
            .builder
            .build_is_null(inner, "vecelem.rc.isnull")
            .unwrap();
        let do_bb = self.context.append_basic_block(drop_fn, "vecelem.rc.do");
        let ret_bb = self.context.append_basic_block(drop_fn, "vecelem.rc.ret");
        self.builder
            .build_conditional_branch(is_null, ret_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.emit_refcount_dec_by_type(heap_ty, inner);
        self.builder.build_unconditional_branch(ret_bb).unwrap();
        self.builder.position_at_end(ret_bb);
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;
        Some(drop_fn)
    }

    /// Vec-store slice (B-2026-06-22-2): synthesize (or fetch) the per-element
    /// drop fn for a `Vec[Fn]` that OWNS heap-env closure environments. The
    /// `FreeVecBuffer` drain calls it once per live element with a pointer to the
    /// element SLOT (a closure fat pointer `{ fn_ptr, env_ptr }` stored inline in
    /// the buffer); the fn RC-drops that element's env — extract the env box
    /// (field 1), skip a null env (a non-capturing element), else decrement the
    /// refcount and `free` the box at zero. This is exactly the `FreeClosureEnv`
    /// cleanup logic, hoisted into a standalone fn so the existing `elem_agg_drop`
    /// `0..len` loop (`track_vec_of_aggs_var`) drives it over the dynamic length.
    /// One shared fn serves every heap-env Vec (the body is element-type-agnostic);
    /// memoized by symbol name.
    pub(super) fn emit_vec_elem_closure_env_drop_fn(
        &mut self,
    ) -> inkwell::values::FunctionValue<'ctx> {
        let fn_name = "__karac_vec_elem_closure_env_drop";
        if let Some(f) = self.module.get_function(fn_name) {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let void_ty = self.context.void_type();
        let i64_t = self.context.i64_type();
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        let fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn =
            self.module
                .add_function(fn_name, fn_ty, Some(inkwell::module::Linkage::Internal));
        self.current_fn = Some(drop_fn);
        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        // The element slot holds a closure fat pointer `{ fn_ptr, env_ptr }`.
        let elem_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();
        let fat_ty = self.closure_value_type();
        let fat = self
            .builder
            .build_load(fat_ty, elem_ptr, "vecelem.clo.fat")
            .unwrap()
            .into_struct_value();
        let env_box = self
            .builder
            .build_extract_value(fat, 1, "vecelem.clo.env")
            .unwrap()
            .into_pointer_value();
        let null = ptr_ty.const_null();
        let live = self
            .builder
            .build_int_compare(IntPredicate::NE, env_box, null, "vecelem.clo.live")
            .unwrap();
        let dec_bb = self.context.append_basic_block(drop_fn, "vecelem.clo.dec");
        let free_bb = self.context.append_basic_block(drop_fn, "vecelem.clo.free");
        let ret_bb = self.context.append_basic_block(drop_fn, "vecelem.clo.ret");
        self.builder
            .build_conditional_branch(live, dec_bb, ret_bb)
            .unwrap();
        self.builder.position_at_end(dec_bb);
        // The refcount is field 0 of the RC box; a `{ i64 }` GEP reaches it
        // regardless of the captured payload that follows.
        let rc_box_ty = self.context.struct_type(&[i64_t.into()], false);
        let rc_ptr = self
            .builder
            .build_struct_gep(rc_box_ty, env_box, 0, "vecelem.clo.rc")
            .unwrap();
        let rc = self
            .builder
            .build_load(i64_t, rc_ptr, "vecelem.clo.rcval")
            .unwrap()
            .into_int_value();
        let dec = self
            .builder
            .build_int_sub(rc, i64_t.const_int(1, false), "vecelem.clo.dec1")
            .unwrap();
        self.builder.build_store(rc_ptr, dec).unwrap();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, dec, i64_t.const_zero(), "vecelem.clo.z")
            .unwrap();
        self.builder
            .build_conditional_branch(is_zero, free_bb, ret_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        self.builder
            .build_call(self.free_fn, &[env_box.into()], "")
            .unwrap();
        self.builder.build_unconditional_branch(ret_bb).unwrap();
        self.builder.position_at_end(ret_bb);
        self.builder.build_return(None).unwrap();
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;
        drop_fn
    }

    /// Derive the four `track_map_var` classification args for a `Map[K, V]`
    /// / `Set[T]` temporary straight from its surface `TypeExpr`. Mirrors the
    /// let-binding derivation in `stmts.rs` (which reads per-binding
    /// side-tables keyed by variable name) — a temporary has no binding name
    /// and so no side-table entry, so the K/V `TypeExpr`s carried in
    /// `owned_temp_drops` are the source of truth. Returns
    /// `(key_is_vec, val_is_vec, key_shared_heap, val_shared_heap)`; a `Set`
    /// lowers to `Map[T, ()]`, so its value half is inert.
    pub(super) fn map_temp_cleanup_parts(
        &mut self,
        te: &TypeExpr,
    ) -> (
        bool,
        bool,
        Option<StructType<'ctx>>,
        Option<StructType<'ctx>>,
        Option<FunctionValue<'ctx>>,
    ) {
        fn nth(path: &PathExpr, i: usize) -> Option<&TypeExpr> {
            match path.generic_args.as_ref()?.get(i)? {
                GenericArg::Type(t) => Some(t),
                _ => None,
            }
        }
        let path = match &te.kind {
            TypeKind::Path(p) => p,
            _ => return (false, false, None, None, None),
        };
        let head = path.segments.first().map(|s| s.as_str()).unwrap_or("");
        let k = nth(path, 0);
        let key_is_vec =
            k.is_some_and(|t| self.llvm_ty_is_vec_struct(self.llvm_type_for_type_expr(t)));
        let key_shared = k.and_then(|t| self.shared_heap_type_for_type_expr(t));
        if head == "Set" {
            return (key_is_vec, false, key_shared, None, None);
        }
        let v = nth(path, 1).cloned();
        let val_is_vec = v
            .as_ref()
            .is_some_and(|t| self.llvm_ty_is_vec_struct(self.llvm_type_for_type_expr(t)));
        let val_shared = v
            .as_ref()
            .and_then(|t| self.shared_heap_type_for_type_expr(t));
        // Slice 3r (deferred gap (d)): per-VALUE drop fn for a value that
        // owns heap beyond the one-level `{ptr,len,cap}` overlay. When it
        // fires, it owns the whole value side — the flag/shared halves are
        // forced off (the helper returns None for shared / plain-overlay
        // values, so this only rewrites cases the flags mishandled).
        let val_drop_fn = v
            .as_ref()
            .and_then(|t| self.map_val_drop_fn_for_type_expr(t));
        if val_drop_fn.is_some() {
            return (key_is_vec, false, key_shared, None, val_drop_fn);
        }
        (key_is_vec, val_is_vec, key_shared, val_shared, val_drop_fn)
    }

    /// Slice 3r (deferred gap (d)) selection: the synthesized per-VALUE
    /// drop fn for a `Map[K, V]` binding/temp whose V owns heap beyond
    /// what the flag-based runtime walk releases. Returns `None` (keep the
    /// existing fast paths) for:
    /// - shared V — the codegen-side rc_dec walk owns it;
    /// - `String` / plain `Vec`/`VecDeque` with a heapless element — the
    ///   one-level `val_is_vec` overlay free is exact;
    /// - a V the recursive drop family can't fully free (boxed payloads,
    ///   unknown heads) — status-quo leak rather than a partial free that
    ///   would look done.
    ///
    /// Fires for: user structs/enums with heap fields (`Map[K, Holder]`),
    /// inner `Map`/`Set` values, `Option`/`Result` with supported inline
    /// payloads, and `Vec`-shaped values whose ELEMENT owns heap
    /// (`Map[K, Vec[String]]`, `Map[K, Vec[Vec[T]]]` — the flag free
    /// releases only the outer buffer). Delegates the actual synthesis to
    /// `emit_drop_fn_for_type_expr` / `vec_elem_agg_drop_for_type_expr`,
    /// the slice-3n/3o/3p/3q recursive drop family.
    pub(super) fn map_val_drop_fn_for_type_expr(
        &mut self,
        val_te: &TypeExpr,
    ) -> Option<FunctionValue<'ctx>> {
        let path = match &val_te.kind {
            TypeKind::Path(p) => p,
            // Tuple values: the agg-drop synthesizer handles all-heap-leaf
            // tuples; anything it declines stays on the status quo.
            TypeKind::Tuple(_) => {
                return if self.te_owns_heap_below_buffer(val_te)
                    && self.te_recursive_drop_fully_supported(val_te)
                {
                    Some(self.emit_drop_fn_for_type_expr(val_te))
                } else {
                    None
                };
            }
            _ => return None,
        };
        // Shared V: the rc_dec walk (val_shared_heap_type) owns the value
        // side; a drop fn here would double-dec.
        if self.shared_heap_type_for_type_expr(val_te).is_some() {
            return None;
        }
        let head = path.segments.first().map(|s| s.as_str()).unwrap_or("");
        let arg = |i: usize| -> Option<&TypeExpr> {
            match path.generic_args.as_ref()?.get(i)? {
                GenericArg::Type(t) => Some(t),
                _ => None,
            }
        };
        match head {
            // Exact one-level overlay — keep `val_is_vec`.
            "String" | "str" => None,
            "Vec" | "VecDeque" => {
                let elem = arg(0)?.clone();
                if self.te_owns_heap_below_buffer(&elem)
                    && self.te_recursive_drop_fully_supported(&elem)
                {
                    Some(self.emit_drop_fn_for_type_expr(val_te))
                } else {
                    // Heapless element → the flag free is exact. A
                    // heap-owning-but-unsupported element keeps the
                    // status-quo one-level free (never a double-free).
                    None
                }
            }
            "Map" | "Set" => {
                if self.te_recursive_drop_fully_supported(val_te) {
                    Some(self.emit_drop_fn_for_type_expr(val_te))
                } else {
                    None
                }
            }
            // Option/Result and named user types (struct/enum).
            _ => {
                if !self.te_owns_heap_below_buffer(val_te) {
                    return None;
                }
                // Option/Result inline-payload gates, user struct/enum
                // membership, and the both-spellings String trap all live
                // inside the central synthesizer; a None keeps the value
                // on the status-quo path.
                self.vec_elem_agg_drop_for_type_expr(val_te)
            }
        }
    }

    /// Register a SoA-laid-out Vec for scope-exit cleanup. Mirrors
    /// `track_vec_var` but emits a `FreeSoaGroups` action whose cleanup
    /// loops over every hot group pointer and (if present) the cold
    /// pointer, GEP'ing against the SoA struct type so the cap-check
    /// reads the actual cap slot (not whichever slot collides with
    /// `vec_struct_type`'s field 2). Without this, an SoA Vec routed
    /// through `track_vec_var(_, None)` leaks every group except `g0`.
    pub(super) fn track_soa_groups(
        &mut self,
        soa_alloca: PointerValue<'ctx>,
        soa_struct_ty: StructType<'ctx>,
        num_hot_groups: u32,
        has_cold: bool,
        soa_drop_fn: Option<FunctionValue<'ctx>>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeSoaGroups {
                soa_alloca,
                soa_struct_ty,
                num_hot_groups,
                has_cold,
                soa_drop_fn,
            });
        }
    }

    /// The subset of an SoA layout's group buffers (hot, then optional cold)
    /// whose element sub-struct carries at least one String/Vec (heap) field,
    /// each paired with its struct-field index in the SoA header and its
    /// element LLVM type. EMPTY for a fully-POD layout. Drives both the
    /// per-element drop synthesis and the single-element overwrite drops; an
    /// empty result means none of those are ever emitted (POD byte-identical).
    pub(super) fn soa_heap_groups(
        &self,
        soa: &crate::codegen::state::SoaLayout,
    ) -> Vec<(u32, StructType<'ctx>)> {
        let mut out = Vec::new();
        for (gi, group) in soa.groups.iter().enumerate() {
            let elem_ty = self.soa_group_elem_type(&soa.struct_name, group);
            if self.aggregate_has_heap_field(elem_ty) {
                out.push((gi as u32, elem_ty));
            }
        }
        if let Some(cold) = &soa.cold_group {
            let elem_ty = self.soa_group_elem_type(&soa.struct_name, cold);
            if self.aggregate_has_heap_field(elem_ty) {
                // The cold-group pointer sits at struct field `num_groups`,
                // right after every hot-group pointer (see `compile_soa_method`).
                out.push((soa.num_groups as u32, elem_ty));
            }
        }
        out
    }

    /// Free the heap (String/Vec) field buffers of the SoA element at `idx`
    /// across every heap-bearing group. Reads each group's buffer pointer from
    /// the header at `soa_struct_ptr`, strides to `[idx]` by the group's
    /// sub-struct, and runs `emit_aggregate_heap_field_frees` over it
    /// (cap-guarded per field, recursing nested tuples/structs). Straight-line
    /// — no loop. The caller guarantees `idx < len` and that the groups were
    /// allocated. Used as the loop body of the synthesized whole-vec drop fn
    /// and directly by the overwrite paths (whole-element / field store
    /// drop-old).
    pub(super) fn emit_soa_drop_element_at(
        &mut self,
        soa_struct_ptr: PointerValue<'ctx>,
        soa_ty: StructType<'ctx>,
        idx: IntValue<'ctx>,
        heap_groups: &[(u32, StructType<'ctx>)],
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        for &(field_idx, elem_ty) in heap_groups {
            let grp_ptr_ptr = self
                .builder
                .build_struct_gep(soa_ty, soa_struct_ptr, field_idx, "soa.drop.gptr")
                .unwrap();
            let grp_buf = self
                .builder
                .build_load(ptr_ty, grp_ptr_ptr, "soa.drop.buf")
                .unwrap()
                .into_pointer_value();
            let elem_ptr = unsafe {
                self.builder
                    .build_gep(elem_ty, grp_buf, &[idx], "soa.drop.elem")
                    .unwrap()
            };
            self.emit_aggregate_heap_field_frees(elem_ptr, elem_ty);
        }
    }

    /// Synthesize (or fetch from cache) the per-element heap-field drop fn for
    /// an SoA `layout`: `__karac_soa_drop_<layout>(*mut SoaStruct)`. Returns
    /// `None` when the element struct is fully POD (no String/Vec field in any
    /// group) — the caller then queues no drop and the cleanup arm stays
    /// byte-identical to the pre-heap-field state.
    ///
    /// The fn loops every live element `[0, len)` and frees each heap group's
    /// String/Vec buffers via `emit_soa_drop_element_at`. It is the SoA peer of
    /// `emit_struct_drop_synthesis`: the AoS path lays an element out
    /// contiguously and calls `__karac_drop_struct_<T>` per element, whereas a
    /// SoA element's fields are scattered across the per-group buffers, so the
    /// drop walks groups-then-elements instead. Same one-level discipline: a
    /// `Vec[T]` field's OUTER buffer is freed, not its elements (rejected at
    /// layout validation precisely so that remainder can't arise).
    ///
    /// Synthesis sets `current_fn` to the new fn (the cap-guard blocks
    /// `emit_aggregate_heap_field_frees` appends read it) and restores the
    /// builder's prior insert point — the same scaffolding
    /// `emit_struct_drop_synthesis` uses.
    pub(super) fn emit_soa_drop_fn(
        &mut self,
        soa: &crate::codegen::state::SoaLayout,
    ) -> Option<FunctionValue<'ctx>> {
        if let Some(f) = self.soa_drop_fns.get(&soa.name) {
            return Some(*f);
        }
        let heap_groups = self.soa_heap_groups(soa);
        if heap_groups.is_empty() {
            return None;
        }

        let has_cold = soa.cold_group.is_some();
        let soa_ty = self.soa_vec_type(soa.num_groups, has_cold);
        let len_idx = Self::soa_len_index(soa.num_groups, has_cold);

        let fn_name = format!("__karac_soa_drop_{}", soa.name);
        if let Some(f) = self.module.get_function(&fn_name) {
            self.soa_drop_fns.insert(soa.name.clone(), f);
            return Some(f);
        }

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let void_ty = self.context.void_type();

        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;

        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let drop_fn = self.module.add_function(
            &fn_name,
            drop_fn_ty,
            Some(inkwell::module::Linkage::Internal),
        );
        self.soa_drop_fns.insert(soa.name.clone(), drop_fn);
        self.current_fn = Some(drop_fn);

        let entry_bb = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry_bb);
        let p_arg = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        // Counted loop over live elements [0, len). The whole-vec drop is only
        // ever called inside a `cap > 0` guard, so `len` is the live count and
        // every group buffer is allocated.
        let len_ptr = self
            .builder
            .build_struct_gep(soa_ty, p_arg, len_idx, "soa.drop.len.ptr")
            .unwrap();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "soa.drop.len")
            .unwrap()
            .into_int_value();
        let i_slot = self.builder.build_alloca(i64_t, "soa.drop.i").unwrap();
        self.builder
            .build_store(i_slot, i64_t.const_int(0, false))
            .unwrap();
        let cond_bb = self.context.append_basic_block(drop_fn, "soa.drop.cond");
        let body_bb = self.context.append_basic_block(drop_fn, "soa.drop.body");
        let done_bb = self.context.append_basic_block(drop_fn, "soa.drop.done");
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(cond_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "soa.drop.iv")
            .unwrap()
            .into_int_value();
        let more = self
            .builder
            .build_int_compare(IntPredicate::ULT, i, len, "soa.drop.more")
            .unwrap();
        self.builder
            .build_conditional_branch(more, body_bb, done_bb)
            .unwrap();

        self.builder.position_at_end(body_bb);
        let i = self
            .builder
            .build_load(i64_t, i_slot, "soa.drop.iv2")
            .unwrap()
            .into_int_value();
        self.emit_soa_drop_element_at(p_arg, soa_ty, i, &heap_groups);
        let next = self
            .builder
            .build_int_add(i, i64_t.const_int(1, false), "soa.drop.inc")
            .unwrap();
        self.builder.build_store(i_slot, next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        self.builder.position_at_end(done_bb);
        self.builder.build_return(None).unwrap();

        // Restore the caller's insert point + current fn.
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }
        self.current_fn = saved_fn;
        Some(drop_fn)
    }

    /// Emit a runtime zero-store to a Vec/String alloca's `cap` field
    /// (slot index 2 of the `{data, len, cap}` struct). Used to suppress
    /// a queued `FreeVecBuffer` whose buffer ownership has moved to a
    /// different slot — the `cap > 0` guard in `emit_scope_cleanup`'s
    /// `FreeVecBuffer` walker turns the free into a no-op, leaving the
    /// new owner's own cleanup to run once.
    pub(super) fn zero_vec_alloca_cap(&self, vec_alloca: PointerValue<'ctx>) {
        let vec_ty = self.vec_struct_type();
        let i64_t = self.context.i64_type();
        if let Ok(cap_ptr) =
            self.builder
                .build_struct_gep(vec_ty, vec_alloca, 2, "fstr.acc.cap.suppress")
        {
            let _ = self.builder.build_store(cap_ptr, i64_t.const_int(0, false));
        }
    }

    /// Deep-copy a String / Vec value (`{data, len, cap}` struct) into a
    /// fresh heap buffer, returning the copied header. Used at retaining
    /// consume sites of owned String/Vec PARAMETERS (`Vec.push(param)`,
    /// `return param`): the call ABI passes the header by value while the
    /// caller keeps the buffer's scope-exit free, so retaining the alias
    /// would dangle once the caller's cleanup fires. The copy gives the
    /// retainer its own buffer; the caller's free stays balanced.
    ///
    /// Runtime-guarded on `cap > 0`: a `cap == 0` source (string literal
    /// over .rodata, empty vec, already-moved slot) carries no heap
    /// ownership and passes through unchanged — every downstream free is
    /// gated on `cap > 0`, so the alias is permanently safe. The copy's
    /// `new_cap = max(len, 1)` keeps the result in the owned regime even
    /// for a `len == 0, cap > 0` source (so exactly one of source/copy
    /// can't end up sharing a buffer with the other).
    ///
    /// `elem_te` (the element's surface type, from `var_elem_type_exprs`)
    /// drives the recursive case: when the element is itself heap-owning
    /// (String / Vec[...]), each copied element header is rewritten with
    /// a recursive deep copy of its own buffer — a flat memcpy would
    /// alias the inner buffers, which the source's recursive
    /// `FreeVecBuffer` drop also walks. `None` (String receivers, scalar
    /// elements) means the flat memcpy is complete.
    pub(super) fn emit_vecstr_defensive_copy(
        &mut self,
        val: BasicValueEnum<'ctx>,
        elem_ty: BasicTypeEnum<'ctx>,
        elem_te: Option<&TypeExpr>,
    ) -> BasicValueEnum<'ctx> {
        let vec_ty = self.vec_struct_type();
        if val.get_type() != vec_ty.into() {
            return val;
        }
        let sv = val.into_struct_value();
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        let data = self
            .builder
            .build_extract_value(sv, 0, "dcopy.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_extract_value(sv, 1, "dcopy.len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_extract_value(sv, 2, "dcopy.cap")
            .unwrap()
            .into_int_value();

        let entry_bb = self.builder.get_insert_block().unwrap();
        let copy_bb = self.context.append_basic_block(fn_val, "dcopy.copy");
        let done_bb = self.context.append_basic_block(fn_val, "dcopy.done");

        let owned = self
            .builder
            .build_int_compare(
                inkwell::IntPredicate::UGT,
                cap,
                i64_t.const_int(0, false),
                "dcopy.owned",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(owned, copy_bb, done_bb)
            .unwrap();

        // Copy path: bytes = len * sizeof(elem); malloc(max(bytes, 1));
        // memcpy; result {buf, len, max(len, 1)}.
        self.builder.position_at_end(copy_bb);
        let elem_size = elem_ty.size_of().unwrap();
        let bytes = self
            .builder
            .build_int_mul(len, elem_size, "dcopy.bytes")
            .unwrap();
        let one = i64_t.const_int(1, false);
        let bytes_pos = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, bytes, one, "dcopy.bytes.cmp")
            .unwrap();
        let alloc_bytes = self
            .builder
            .build_select(bytes_pos, bytes, one, "dcopy.alloc_bytes")
            .unwrap()
            .into_int_value();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "dcopy.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder.build_memcpy(buf, 1, data, 1, bytes).unwrap();

        // Recursive case: heap-owning elements get their own buffers —
        // rewrite each copied element in place.
        //   - String / Vec[...] elements are {ptr,len,cap}-shaped: recurse
        //     `emit_vecstr_defensive_copy` on each (stride = vec_ty).
        //   - Map / Set elements are opaque handles (a single `ptr`, NOT a
        //     vec struct): the outer memcpy aliased the source's handles, so
        //     both the source and this copy would free the same map
        //     (double-free). Deep-clone each handle via the synthesized
        //     `karac_clone_<Map|Set>` fn (stride = elem_ty = ptr).
        if let Some(inner_te) = elem_te {
            let inner_is_string_or_vec = self.is_string_type_expr(inner_te)
                || self.extract_vec_elem_type(inner_te).is_some();
            let inner_is_map_or_set = matches!(
                &inner_te.kind,
                TypeKind::Path(p)
                    if matches!(
                        p.segments.first().map(String::as_str),
                        Some("Map") | Some("Set")
                    )
            );
            if inner_is_string_or_vec {
                let inner_elem_ty: BasicTypeEnum<'ctx> = if self.is_string_type_expr(inner_te) {
                    self.context.i8_type().into()
                } else {
                    self.extract_vec_elem_type(inner_te).unwrap()
                };
                let inner_inner_te = crate::codegen::helpers::vec_inner_type_expr(inner_te);

                let loop_bb = self.context.append_basic_block(fn_val, "dcopy.elem.loop");
                let body_bb = self.context.append_basic_block(fn_val, "dcopy.elem.body");
                let exit_bb = self.context.append_basic_block(fn_val, "dcopy.elem.exit");
                let pre_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();

                self.builder.position_at_end(loop_bb);
                let idx_phi = self.builder.build_phi(i64_t, "dcopy.elem.i").unwrap();
                idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
                let idx = idx_phi.as_basic_value().into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, idx, len, "dcopy.elem.cmp")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, exit_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                let slot = unsafe {
                    self.builder
                        .build_gep(vec_ty, buf, &[idx], "dcopy.elem.slot")
                        .unwrap()
                };
                let elem_val = self
                    .builder
                    .build_load(vec_ty, slot, "dcopy.elem.val")
                    .unwrap();
                let copied = self.emit_vecstr_defensive_copy(
                    elem_val,
                    inner_elem_ty,
                    inner_inner_te.as_ref(),
                );
                self.builder.build_store(slot, copied).unwrap();
                // The recursive call may have moved the insertion point
                // into its own done-block — branch from wherever we are.
                let body_end = self.builder.get_insert_block().unwrap();
                let next = self
                    .builder
                    .build_int_add(idx, i64_t.const_int(1, false), "dcopy.elem.next")
                    .unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                idx_phi.add_incoming(&[(&next, body_end)]);

                self.builder.position_at_end(exit_bb);
            } else if inner_is_map_or_set {
                // The clone fn `void karac_clone_<T>(*const handle, *mut
                // handle)` loads `*src` once up front then iterates the OLD
                // map to build a fresh one, only storing the new handle to
                // `*dst` at the end — so a slot->slot clone (src == dst) is
                // sound: the alias in the copied buffer is read before it's
                // overwritten. This composes with the Vec recursion above
                // (a `Vec[Vec[Map]]` recurses to the inner `Vec[Map]`, whose
                // element is then a Map handled here).
                let clone_fn = self.emit_clone_fn_for_type_expr(inner_te);

                let loop_bb = self.context.append_basic_block(fn_val, "dcopy.map.loop");
                let body_bb = self.context.append_basic_block(fn_val, "dcopy.map.body");
                let exit_bb = self.context.append_basic_block(fn_val, "dcopy.map.exit");
                let pre_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();

                self.builder.position_at_end(loop_bb);
                let idx_phi = self.builder.build_phi(i64_t, "dcopy.map.i").unwrap();
                idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
                let idx = idx_phi.as_basic_value().into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, idx, len, "dcopy.map.cmp")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, exit_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                // Each slot holds one `elem_ty`-sized handle (`ptr`), so the
                // gep strides by `elem_ty`, not the 24-byte `vec_ty`.
                let slot = unsafe {
                    self.builder
                        .build_gep(elem_ty, buf, &[idx], "dcopy.map.slot")
                        .unwrap()
                };
                self.builder
                    .build_call(clone_fn, &[slot.into(), slot.into()], "")
                    .unwrap();
                let next = self
                    .builder
                    .build_int_add(idx, i64_t.const_int(1, false), "dcopy.map.next")
                    .unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                idx_phi.add_incoming(&[(&next, body_bb)]);

                self.builder.position_at_end(exit_bb);
            } else if self.type_expr_has_drop_heap(inner_te)
                || self.te_owns_option_heap_payload(inner_te)
            {
                // #35 copy-side peer — a struct / enum / tuple element that
                // owns heap through a NON-`{ptr,len,cap}` leaf (`Vec[Sp]`,
                // `Sp { tok: Tk }` with a heap enum `Tk`; the parser's
                // `Vec[SpannedToken]`). The flat memcpy above aliased each
                // element's inner String/enum payload with the source — both
                // the source's recursive element drop and (post-#35) this
                // copy's owning struct drop would then free it (double-free,
                // exactly what the parser's `Parser.new(toks)` entry-copy of a
                // `Vec[SpannedToken]` hit). Deep-clone each element in place via
                // its synthesized `karac_clone_<T>(*const, *mut)` — a slot->slot
                // clone is sound (the clone reads each source field's header
                // before overwriting the slot, and the heap deep-copy reads the
                // shared buffer before the new header lands). Stride by
                // `elem_ty` (the element struct/enum size), not the 24-byte
                // `vec_ty`. `type_expr_has_drop_heap` is false for shared (RC)
                // leaves and no-heap aggregates, so neither is touched here —
                // but it is ALSO (deliberately, drop-side) false for `Option`,
                // so an element struct whose only heap is an
                // `Option[String]`-class field (`AttrNode.string_value`,
                // B-2026-07-10-4 residual) skipped this leg and aliased the
                // `Some` payload; `te_owns_option_heap_payload` is the
                // copy-side companion that admits exactly that shape.
                let clone_fn = self.emit_clone_fn_for_type_expr(inner_te);

                let loop_bb = self.context.append_basic_block(fn_val, "dcopy.agg.loop");
                let body_bb = self.context.append_basic_block(fn_val, "dcopy.agg.body");
                let exit_bb = self.context.append_basic_block(fn_val, "dcopy.agg.exit");
                let pre_bb = self.builder.get_insert_block().unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();

                self.builder.position_at_end(loop_bb);
                let idx_phi = self.builder.build_phi(i64_t, "dcopy.agg.i").unwrap();
                idx_phi.add_incoming(&[(&i64_t.const_int(0, false), pre_bb)]);
                let idx = idx_phi.as_basic_value().into_int_value();
                let in_range = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, idx, len, "dcopy.agg.cmp")
                    .unwrap();
                self.builder
                    .build_conditional_branch(in_range, body_bb, exit_bb)
                    .unwrap();

                self.builder.position_at_end(body_bb);
                let slot = unsafe {
                    self.builder
                        .build_gep(elem_ty, buf, &[idx], "dcopy.agg.slot")
                        .unwrap()
                };
                self.builder
                    .build_call(clone_fn, &[slot.into(), slot.into()], "")
                    .unwrap();
                let next = self
                    .builder
                    .build_int_add(idx, i64_t.const_int(1, false), "dcopy.agg.next")
                    .unwrap();
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                idx_phi.add_incoming(&[(&next, body_bb)]);

                self.builder.position_at_end(exit_bb);
            }
        }

        let len_pos = self
            .builder
            .build_int_compare(inkwell::IntPredicate::UGT, len, one, "dcopy.len.cmp")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(len_pos, len, one, "dcopy.new_cap")
            .unwrap()
            .into_int_value();
        let mut copied = vec_ty.get_undef();
        copied = self
            .builder
            .build_insert_value(copied, buf, 0, "dcopy.out.data")
            .unwrap()
            .into_struct_value();
        copied = self
            .builder
            .build_insert_value(copied, len, 1, "dcopy.out.len")
            .unwrap()
            .into_struct_value();
        copied = self
            .builder
            .build_insert_value(copied, new_cap, 2, "dcopy.out.cap")
            .unwrap()
            .into_struct_value();
        let copy_end_bb = self.builder.get_insert_block().unwrap();
        self.builder.build_unconditional_branch(done_bb).unwrap();

        self.builder.position_at_end(done_bb);
        let phi = self.builder.build_phi(vec_ty, "dcopy.result").unwrap();
        phi.add_incoming(&[(&sv, entry_bb), (&copied, copy_end_bb)]);
        phi.as_basic_value()
    }

    /// Deep-copy a VALUE-POSITION block/branch tail when it ultimately names an
    /// owned Vec/String PARAM, so the escaping branch value owns an independent
    /// buffer. The move-suppression the branch already applies
    /// (`suppress_block_tail_cleanup` / `suppress_source_vec_cleanup_for_arg`)
    /// only zeroes the source `cap` to skip a *local* owner's free — but an
    /// owned param is CALLER-retained (the caller frees the arg buffer), so
    /// zeroing the callee's slot does nothing and the branch value still aliases
    /// the caller's buffer. Returning/binding that alias then double-frees (the
    /// caller frees the arg AND the consumer frees the result — same buffer).
    /// A deep copy gives the consumer its own buffer, exactly as the bare-tail
    /// return does (`maybe_defensive_copy_param_arg` at the function tail); this
    /// closes the branch-buried sibling that the bare-tail helper misses because
    /// there the function's `final_expr` is the whole `if`/`match`, not the leaf.
    ///
    /// Recurses through nested `{ … }` / `unsafe { … }` tails to reach the leaf
    /// identifier, mirroring `suppress_block_tail_cleanup`'s recursion. No-op for
    /// a local binding (it is not in `owned_vecstr_params`, so
    /// `maybe_defensive_copy_param_arg` returns `val` untouched and the existing
    /// move-out semantics are preserved) or any non-identifier tail. Emit-order:
    /// call AFTER the block's frame drains and BEFORE the branch's terminating
    /// jump to the merge block, so the copy lands in the branch's predecessor and
    /// the phi picks up the fresh buffer; `emit_vecstr_defensive_copy` reads the
    /// already-loaded SSA `val`, so a prior source-`cap` zeroing is irrelevant.
    pub(super) fn deepcopy_owned_param_branch_tail(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match &tail.kind {
            ExprKind::Identifier(_) => self.maybe_defensive_copy_param_arg(tail, val),
            ExprKind::Block(b) | ExprKind::Seq(b) | ExprKind::Unsafe(b) => {
                match b.final_expr.as_deref() {
                    Some(inner) => self.deepcopy_owned_param_branch_tail(inner, val),
                    None => val,
                }
            }
            _ => val,
        }
    }

    /// Defensive-copy shim for retaining consume sites: when `arg_expr`
    /// is a bare Identifier naming an owned String/Vec PARAMETER of the
    /// current function (`owned_vecstr_params`) OR a heap `for`-loop element
    /// borrow (`for_loop_borrow_vars`), return a deep copy of `val`; otherwise
    /// return `val` unchanged. Both share the same ownership rationale: the
    /// SOURCE (the caller's param buffer / the source Vec's element) retains the
    /// scope-exit free, so a retaining-consume site must own a private copy
    /// rather than alias it. See `emit_vecstr_defensive_copy`.
    pub(super) fn maybe_defensive_copy_param_arg(
        &mut self,
        arg_expr: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // B-2026-07-04-2 / B-2026-07-05-1: a heap element read by index
        // (`a[i]` where `a` is a named `Vec[String]`/`Vec[Vec[..]]`/…) and moved
        // into an OWNING sink (tuple literal, `push`, struct field, map value,
        // owned call arg, return) shallow-aliases the container's element
        // buffer. `compile_vec_index` only loads the `{ptr,len,cap}` header, so
        // both the container's element-drop AND the sink's owner free the same
        // buffer — a double-free (`(a[i], b[i])` / `d.push(a[i])`, exit 133).
        // The `let s = a[i]` binding path already deep-clones (via the same
        // helper, stmts.rs), so this closes the twin gap at the by-value consume
        // sites. `clone_owned_vec_index_element` is scoped to a named-Vec,
        // non-range, non-trivially-copyable index and leaves the source intact,
        // so the container's single drop and the sink's clone free distinct
        // buffers. No-op for POD elements and non-index args.
        if self.expr_is_heap_vec_index(arg_expr) {
            return self
                .clone_owned_vec_index_element(arg_expr, val)
                .unwrap_or(val);
        }
        // A heap FIELD read through a BORROWED receiver (`ref self` / `mut ref
        // self` / a `ref` param) and returned — `fn name(ref self) -> String {
        // self.n }`. The borrow does not own the field, so returning the loaded
        // `{ptr,len,cap}` hands the caller an ALIAS of the receiver's buffer;
        // the caller's drop of the receiver then frees it a second time (and any
        // reuse of the receiver dangles). Deep-clone the field so the returned
        // value owns an independent buffer. (An OWNED receiver's field move-out
        // is handled instead by zeroing the source cap in
        // `suppress_source_vec_cleanup_for_arg_ex` — that requires the receiver
        // to be dropped by this frame, which a borrow is not; the two cases are
        // mutually exclusive on `ref_params` membership.) Shared (RC) fields are
        // left to the refcount machinery. This runs on the return value at the
        // tail (`compile_function` / mono tail), so it fires exactly in return
        // position.
        if let ExprKind::FieldAccess { object, field } = &arg_expr.kind {
            let recv_name = match &object.kind {
                ExprKind::SelfValue => Some("self".to_string()),
                ExprKind::Identifier(n) => Some(n.clone()),
                _ => None,
            };
            if let Some(recv_name) = recv_name {
                if self.ref_params.contains_key(&recv_name) {
                    if let Some(struct_name) = self.inferred_receiver_type(object) {
                        let field_te = self
                            .struct_field_names
                            .get(&struct_name)
                            .and_then(|names| names.iter().position(|n| n == field))
                            .and_then(|idx| {
                                self.struct_field_type_exprs
                                    .get(&struct_name)
                                    .and_then(|tes| tes.get(idx))
                            })
                            .cloned();
                        if let Some(field_te) = field_te {
                            // B-2026-07-12-16 gap 2: inside a monomorph the
                            // field's declared TypeExpr is the bare generic
                            // param (`Box[T].v` → `T`), and `te_owns_heap_
                            // below_buffer(T)` is conservatively TRUE for a bare
                            // param — so this deep-clone fired even for a SCALAR
                            // field. `emit_clone_fn_for_type_expr(T)` then named
                            // + cached the helper under the param name
                            // `karac_clone_T` (last-writer-wins across every
                            // instantiation), so `Box[i32].get` reused
                            // `Box[i16].get`'s i16-width clone body and truncated
                            // the returned i32 to 16 bits (2000000000 → 37888).
                            // Resolve the param through the active
                            // `type_subst_names` to the CONCRETE type and gate on
                            // THAT: a scalar mono field (`i16`/`i32`/`bool`) owns
                            // no heap → the clone is skipped entirely → the field
                            // is returned directly at its correct width. A no-op
                            // outside a monomorph (empty subst). The EMIT below
                            // still uses the original declared `field_te`, so a
                            // HEAP field keeps its pre-existing (leak-clean,
                            // shallow) path byte-for-byte — the generic heap-field
                            // return-clone is a separate concern out of this
                            // narrow-int bug's scope.
                            let field_te_concrete = self.subst_monomorph_type_params(&field_te);
                            // `Option[shared T]` fields are excluded: this
                            // return path ALREADY incs the returned alias
                            // (the ref-rooted FieldAccess arm in
                            // `compile_tail_final_expr`), and
                            // `emit_option_value_clone_fn` now rc-incs too —
                            // cloning here would double-inc and leak the box
                            // (`asan_option_shared_method_tail_field_step_
                            // repeat`). The historical shallow handling +
                            // tail-arm inc is the balanced pair.
                            if self.te_owns_heap_below_buffer(&field_te_concrete)
                                && self
                                    .shared_heap_type_for_type_expr(&field_te_concrete)
                                    .is_none()
                                && self
                                    .option_inner_shared_type_for_type_expr(&field_te_concrete)
                                    .is_none()
                            {
                                if let Some(fn_val) = self.current_fn {
                                    let val_ty = val.get_type();
                                    // B-2026-07-15-11 — for a SINGLE-field generic
                                    // wrapper `W[T] { f: T }` whose mono drop now
                                    // frees the bare-T Vec/String field at the
                                    // receiver's scope exit, cloning with the bare
                                    // declared `T` produces the last-writer-wins
                                    // `karac_clone_T` (a shallow `{ptr,len,cap}`
                                    // copy for a heap param), so the returned alias
                                    // double-frees against that drop. Emit with the
                                    // CONCRETE resolved field type instead
                                    // (`karac_clone_str` / `karac_clone_Vec_*`), so
                                    // the caller owns an INDEPENDENT buffer. Gated
                                    // to a single-field wrapper: a multi-field
                                    // wrapper gets no mono drop (LLVM-layout
                                    // erasure limits the drop to offset 0), so a
                                    // deep clone there would leak the un-dropped
                                    // original — keep its shallow declared-`T` path.
                                    // A concrete (non-generic) struct has
                                    // `field_te == field_te_concrete`, so `emit_te`
                                    // is unchanged for it.
                                    let single_field_heap_wrapper = self
                                        .struct_field_type_exprs
                                        .get(&struct_name)
                                        .map(|v| v.len() == 1)
                                        .unwrap_or(false)
                                        && (self.is_string_type_expr(&field_te_concrete)
                                            || matches!(
                                                &field_te_concrete.kind,
                                                TypeKind::Path(p) if matches!(
                                                    p.segments.last().map(|s| s.as_str()),
                                                    Some("Vec") | Some("VecDeque")
                                                )
                                            ));
                                    let emit_te = if single_field_heap_wrapper {
                                        &field_te_concrete
                                    } else {
                                        &field_te
                                    };
                                    let clone_fn = self.emit_clone_fn_for_type_expr(emit_te);
                                    // `emit_clone_fn_*` / `create_entry_alloca`
                                    // may move the builder — re-anchor to the
                                    // tail block before emitting the copy.
                                    let cur = self.builder.get_insert_block();
                                    let src = self.create_entry_alloca(
                                        fn_val,
                                        "retfld.clone.src",
                                        val_ty,
                                    );
                                    let dst = self.create_entry_alloca(
                                        fn_val,
                                        "retfld.clone.dst",
                                        val_ty,
                                    );
                                    if let Some(bb) = cur {
                                        self.builder.position_at_end(bb);
                                    }
                                    self.builder.build_store(src, val).unwrap();
                                    self.builder
                                        .build_call(
                                            clone_fn,
                                            &[src.into(), dst.into()],
                                            "retfld.clone",
                                        )
                                        .unwrap();
                                    return self
                                        .builder
                                        .build_load(val_ty, dst, "retfld.cloned")
                                        .unwrap();
                                }
                            }
                        }
                    }
                }
            }
        }
        let name = match &arg_expr.kind {
            ExprKind::Identifier(n) => n.clone(),
            _ => return val,
        };
        if !self.owned_vecstr_params.contains(&name) && !self.for_loop_borrow_vars.contains(&name) {
            return val;
        }
        let elem_ty = match self.vec_elem_types.get(&name) {
            Some(t) => *t,
            None => return val,
        };
        let elem_te = self.var_elem_type_exprs.get(&name).cloned();
        self.emit_vecstr_defensive_copy(val, elem_ty, elem_te.as_ref())
    }

    /// Emit an eager free of a Vec/String slot's heap buffer, guarded on
    /// `cap > 0`. Used at move-overwrite sites where the slot is about to
    /// be reassigned to a new heap buffer — without this, the prior
    /// buffer leaks (the slot loses its only reference before scope-exit
    /// cleanup can reach it). Mirrors the runtime shape of `FreeVecBuffer`
    /// for the eager-free position. `cap = 0` slots (string literals,
    /// already-transferred sources) skip the free, preserving the static-
    /// vs-heap invariant the scope walker also relies on.
    ///
    /// **Outer-buffer free only** — does NOT walk inner elements when the
    /// element type is itself heap-owning. The eager-free site sits in
    /// the middle of a user's control flow, so inner heap-owning elements
    /// may already be co-owned by other live bindings (`let x = vec[i]`
    /// shapes that haven't gone out of scope yet, sibling aliases mid-
    /// loop, etc.). Walking the inner buffers here races with the per-
    /// alias scope-exit cleanup the let-binding registered at its own
    /// site — a double-free that hangs in macOS malloc. The scope-exit
    /// `FreeVecBuffer` cleanup walker IS safe to do the recursive walk
    /// because it runs at function exit when every per-alias cleanup has
    /// already drained.
    ///
    /// Result: outer-buffer leak is closed, inner heap-owned elements
    /// are still freed via their existing per-alias scope-exit cleanup
    /// (e.g., the `let prefix = out[i]` body in kata-17 frees each
    /// indexed String at end-of-iter; the leak there was the outer
    /// {ptr,len,cap} array per BFS step). Workloads that move-overwrite
    /// without per-element aliases keep their existing scope-exit
    /// recursive drop unchanged.
    /// Before an `Identifier`-target overwrite of a `Vec[shared]` /
    /// `Vec[Option[shared]]` (or any Vec whose element carries a non-trivial
    /// per-element drop) local, release the OLD value's ELEMENTS and free its
    /// buffer — the same element-releasing walk the binding's scope-exit
    /// `FreeVecBuffer` cleanup performs. `emit_free_vec_buffer_if_owned` frees
    /// ONLY the outer buffer, so a `current = next` overwrite of a
    /// `Vec[shared]`/`Vec[Option[shared]]` stranded every shared box the old Vec
    /// still held (B-2026-07-12-30, the BFS-worklist idiom). Returns `true` when
    /// it emitted the full release (the caller then SKIPS the buffer-only free);
    /// `false` when the element needs no per-element drop (scalar / String / Vec
    /// element — the caller's plain buffer free is correct and cheaper). Reuses
    /// the exact per-element drop `vec_elem_agg_drop_for_type_expr` derives for
    /// the scope-exit path, so a fixed overwrite and a scope-exit drop release a
    /// shared element identically (never twice — the moved-in source's own
    /// cleanup is cap-zeroed by the move-suppression that pairs with the store).
    pub(super) fn emit_owned_vec_element_release_on_overwrite(
        &mut self,
        slot_ptr: PointerValue<'ctx>,
        elem_te: &crate::ast::TypeExpr,
    ) -> bool {
        let agg_drop = self.vec_elem_agg_drop_for_type_expr(elem_te);
        let elem_llvm = self.llvm_type_for_type_expr(elem_te);
        // A heap-owning VALUE element — a `String` or a nested `Vec` — drains
        // through the recursive inline `elem_ty` (vec-struct) walk, the SAME
        // `FreeVecBuffer` action the scope-exit cleanup registers for a
        // `Vec[String]` / `Vec[Vec[_]]` binding. `vec_elem_agg_drop_for_type_expr`
        // returns `None` for these (it only covers named struct/enum elements),
        // so before B-2026-07-18-52 the caller fell through to
        // `emit_free_vec_buffer_if_owned` (OUTER buffer only) and a `cur = nxt`
        // move-overwrite of a `Vec[String]` stranded every element String — the
        // BFS double-buffer / worklist idiom (surfaced by kata #126 Word Ladder
        // II). Draining here is safe against a live per-element alias for the
        // SAME reason the scope-exit drain is: an index-read of a non-Copy
        // element CLONES (`let x = v[i]` owns an independent buffer), so the
        // overwritten generation's elements have no other owner. Scalar /
        // inline-tuple elements keep the cheaper outer-buffer-only free.
        let elem_is_heap_value =
            self.is_string_type_expr(elem_te) || self.llvm_ty_is_vec_struct(elem_llvm);
        if agg_drop.is_none() && !elem_is_heap_value {
            return false;
        }
        let fn_val = match self.current_fn {
            Some(f) => f,
            None => return false,
        };
        let action = crate::codegen::state::CleanupAction::FreeVecBuffer {
            vec_alloca: slot_ptr,
            elem_ty: Some(elem_llvm),
            elem_is_tensor: false,
            elem_map_drop: None,
            elem_agg_drop: agg_drop,
        };
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        self.emit_cleanup_action(&action, fn_val, vec_ty, ptr_ty, i64_t);
        true
    }

    /// Emit `karac_free_buf(data, bytes_hint)` — the recycling-aware release
    /// for an owned Vec/String DATA buffer (`runtime/src/alloc.rs`
    /// large-buffer cache). `elem_abi_size` sizes the hint as
    /// `cap * elem_abi_size`; pass `1` for String/byte buffers (cap IS the
    /// byte count) and `0` for "element size unknown", which emits hint `0`
    /// = "runtime asks the allocator". The hint is a fast-path filter only —
    /// the runtime re-derives the real size before caching — so a wrong one
    /// can cost a recycling opportunity, never correctness. Callers must
    /// already have guarded ownership (`cap > 0`); this emits inside their
    /// owned branch.
    pub(super) fn emit_free_buf_call(
        &self,
        data: PointerValue<'ctx>,
        cap: IntValue<'ctx>,
        elem_abi_size: u64,
    ) {
        let i64_t = self.context.i64_type();
        // `karac_free_buf`'s `bytes_hint` is a `usize` — the target pointer-width
        // int (i32 on wasm32, i64 on 64-bit native), matching the declaration in
        // `Codegen::new`. `cap` is an i64; on wasm32 the hint must be truncated to
        // i32 or the `build_call` type-mismatches the i32 param (and, before the
        // decl fix, the whole call mismatched wasi-libc → a trapping stub).
        let size_is_32 = crate::target::active_target_is_wasm();
        let size_ty = if size_is_32 {
            self.context.i32_type()
        } else {
            i64_t
        };
        let hint = if elem_abi_size == 0 {
            size_ty.const_zero()
        } else {
            let bytes = self
                .builder
                .build_int_mul(cap, i64_t.const_int(elem_abi_size, false), "freebuf.bytes")
                .unwrap();
            if size_is_32 {
                self.builder
                    .build_int_truncate(bytes, size_ty, "freebuf.bytes.szt")
                    .unwrap()
            } else {
                bytes
            }
        };
        self.builder
            .build_call(self.free_buf_fn, &[data.into(), hint.into()], "")
            .unwrap();
    }

    /// The `elem_abi_size` to hand `emit_free_buf_call` when freeing a
    /// Vec/String buffer whose declared FIELD/payload type is `fte` (phase-10
    /// line 282). A `String`'s element is a byte, so `1` is already exact; a
    /// `Vec[T]` returns `sizeof(T)` so a mid-size multi-byte-element buffer
    /// clears the recycling cache's 1 MiB fast-reject that a `cap × 1`
    /// under-hint wrongly tripped (e.g. a 2 MiB `Vec[Cell]` field: cap 262144,
    /// `cap × 1` < 1 MiB → never parked; `cap × sizeof(Cell)` ≥ 1 MiB → parked).
    /// Falls back to `1` for a non-Vec `fte` or when `target_data` isn't cached
    /// — a sound under-hint, never a correctness issue (the hint only gates the
    /// cache fast-reject, never sizing; the cache uses `malloc_usable_size`).
    pub(super) fn vec_field_free_hint_elem_size(&self, fte: &TypeExpr) -> u64 {
        if self.is_string_type_expr(fte) {
            return 1;
        }
        match crate::codegen::helpers::vec_inner_type_expr(fte) {
            Some(elem_te) => self
                .target_data
                .as_ref()
                .map(|td| td.get_abi_size(&self.llvm_type_for_type_expr(&elem_te)))
                .unwrap_or(1),
            None => 1,
        }
    }

    pub(super) fn emit_free_vec_buffer_if_owned(
        &mut self,
        vec_alloca: PointerValue<'ctx>,
        elem_abi_size: u64,
    ) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = match self.current_fn {
            Some(f) => f,
            None => return,
        };
        let data_ptr = match self
            .builder
            .build_struct_gep(vec_ty, vec_alloca, 0, "ov.data.pp")
        {
            Ok(p) => p,
            Err(_) => return,
        };
        let cap_ptr = match self
            .builder
            .build_struct_gep(vec_ty, vec_alloca, 2, "ov.cap.pp")
        {
            Ok(p) => p,
            Err(_) => return,
        };
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr, "ov.data")
            .unwrap()
            .into_pointer_value();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, "ov.cap")
            .unwrap()
            .into_int_value();
        // SSO forward-prep (see `sso.rs`): free only a genuinely owned
        // heap buffer; inline (cap < 0) / static (cap == 0) skip.
        let owned = self.sso_string_is_owned_heap(cap);
        let free_bb = self.context.append_basic_block(fn_val, "ov.free");
        let after_bb = self.context.append_basic_block(fn_val, "ov.after");
        self.builder
            .build_conditional_branch(owned, free_bb, after_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        self.emit_free_buf_call(data, cap, elem_abi_size);
        self.builder.build_unconditional_branch(after_bb).unwrap();
        self.builder.position_at_end(after_bb);
    }

    /// Track a Map / Set alloca for scope-exit free. `key_is_vec` /
    /// `val_is_vec` tell the cleanup whether each side follows the
    /// Vec/String `{ptr, len, cap}` layout and therefore needs per-entry
    /// buffer release before the bucket storage is deallocated. Both
    /// false → plain `karac_map_free`. Either true → routes through
    /// `karac_map_free_with_drop_vec(handle, key_is_vec, val_is_vec)`
    /// so the per-entry walk runs.
    ///
    /// `val_shared_heap_type = Some(heap_ty)` triggers the codegen-side
    /// per-bucket rc_dec walk for shared-struct / shared-enum values
    /// (the runtime helper can't decrement refcounts itself — it's
    /// type-erased and doesn't know V's heap layout). Closes the
    /// `Map[K, shared T]` leak (2026-05-16): values previously
    /// stranded their refcount when the Map went out of scope.
    /// `key_shared_heap_type` is the symmetric K-side gate — fires
    /// the same walk against the key half of each occupied bucket
    /// (`Map[shared K, V]` / `Set[shared T]`).
    pub(super) fn track_map_var(
        &mut self,
        map_alloca: PointerValue<'ctx>,
        key_is_vec: bool,
        val_is_vec: bool,
        val_shared_heap_type: Option<StructType<'ctx>>,
        key_shared_heap_type: Option<StructType<'ctx>>,
    ) {
        self.track_map_var_with_val_drop(
            map_alloca,
            key_is_vec,
            val_is_vec,
            val_shared_heap_type,
            key_shared_heap_type,
            None,
        );
    }

    /// `track_map_var` with the slice-3r per-VALUE drop fn (deferred gap
    /// (d)): a `Some(karac_drop_<V>)` routes the scope-exit free through
    /// `karac_map_free_with_val_drop_fn`, which runs the fn on every live
    /// entry's value blob in place. Callers must keep `val_is_vec = false`
    /// and `val_shared_heap_type = None` when passing a fn — the fn owns
    /// the whole value-side release (see `map_val_drop_fn_for_type_expr`).
    pub(super) fn track_map_var_with_val_drop(
        &mut self,
        map_alloca: PointerValue<'ctx>,
        key_is_vec: bool,
        val_is_vec: bool,
        val_shared_heap_type: Option<StructType<'ctx>>,
        key_shared_heap_type: Option<StructType<'ctx>>,
        val_drop_fn: Option<FunctionValue<'ctx>>,
    ) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeMapHandle {
                map_alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
                val_drop_fn,
            });
        }
    }

    /// Phase 8 `File` handle slice F4b: register a File-typed binding
    /// for scope-exit close. Pushed at the pattern-binding site in
    /// `pattern_binding.rs` when `type_name == "File"` fires the
    /// int→ptr re-typing arm. The drain emits
    /// `karac_runtime_file_close(load(file_alloca))` on exit.
    pub(super) fn track_file_var(&mut self, file_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeFileHandle { file_alloca });
        }
    }

    /// GPU-SLIP-4b: register a `GpuBuffer[S]` binding for scope-exit free. The
    /// drain frees the resident device buffers via `karac_runtime_gpu_free_soa`
    /// (idempotent — a no-op if the handle was already consumed by
    /// `gpu.download`), so no move-suppression is needed at the download site.
    pub(super) fn track_gpu_buffer_var(&mut self, buf_alloca: PointerValue<'ctx>) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeGpuBuffer { buf_alloca });
        }
    }

    /// Phase 6 "Channel AOT codegen lowering": register a channel-end
    /// (`Sender`/`Receiver`) binding for scope-exit drop. Pushed from
    /// `bind_pattern`'s `Binding` arm when the typechecker's
    /// `pattern_binding_types` records the binding's surface type as
    /// `Sender`/`Receiver`; `is_sender` selects `drop_sender` (may close) vs
    /// `drop_receiver` at the drain.
    pub(super) fn track_channel_var(&mut self, chan_alloca: PointerValue<'ctx>, is_sender: bool) {
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::DropChannelEnd {
                chan_alloca,
                is_sender,
            });
        }
    }

    /// Phase 7.2 Slice DP — resolve a let-binding's surface enum name
    /// from the let-statement's annotation and RHS shape, for the
    /// `track_enum_var` registration site. Tries in order:
    ///
    /// 1. Existing `var_type_names` entry — populated by the upstream
    ///    type-hint pass when an explicit `let e: E = ...;` annotation
    ///    is present, or when an Identifier-RHS aliases a previously-
    ///    typed binding.
    /// 2. RHS = bare `Variant(args)` (`ExprKind::Call` with an Identifier
    ///    callee whose name matches a known variant) — walk `enum_layouts`
    ///    for the enum that owns that variant. Single-variant collisions
    ///    across enums are rare in practice and are tolerated by taking
    ///    the first match.
    /// 3. RHS = qualified `Enum.Variant(args)` (`ExprKind::Call` with a
    ///    Path-based callee whose first segment matches a known enum) —
    ///    use the first-segment name directly.
    /// 4. RHS = qualified `Enum.assoc_fn(args)` returning a value of the
    ///    enum's LLVM struct type — match by LLVM-struct-identity reverse-
    ///    lookup against `enum_layouts` (the same shape the existing
    ///    user-struct fallback at the let-site uses for structs).
    ///
    /// Returns `None` when the binding's surface type isn't a known
    /// value-type enum; the cleanup hook then becomes a no-op for that
    /// binding (matches v1 conservative behavior — no spurious cleanup).
    pub(super) fn enum_name_for_binding(
        &self,
        var_name: &str,
        value: &Expr,
        ty: Option<&TypeExpr>,
    ) -> Option<String> {
        // (1) Existing var_type_names entry pointing at a known enum.
        if let Some(n) = self.var_type_names.get(var_name) {
            if self.enum_layouts.contains_key(n) {
                return Some(n.clone());
            }
        }
        // Explicit annotation.
        if let Some(t) = ty {
            if let TypeKind::Path(p) = &t.kind {
                if let Some(seg) = p.segments.last() {
                    if self.enum_layouts.contains_key(seg) {
                        return Some(seg.clone());
                    }
                }
            }
        }
        // (2) / (3) Inspect the RHS Call shape.
        if let ExprKind::Call { callee, .. } = &value.kind {
            match &callee.kind {
                ExprKind::Identifier(n) => {
                    // Bare-name variant constructor. Prefer user-declared
                    // enums over seeded built-ins (Option / Result / Json
                    // / TcpError) when the variant name collides — same
                    // disambiguation as `try_compile_enum_variant`. Without
                    // this preference, HashMap iteration order picks a
                    // seeded enum's layout non-deterministically for a
                    // user-defined variant with the same name.
                    let mut user_match: Option<String> = None;
                    let mut seed_match: Option<String> = None;
                    for (en, layout) in &self.enum_layouts {
                        if layout.tags.contains_key(n) {
                            if self.seeded_enum_names.contains(en) {
                                seed_match.get_or_insert_with(|| en.clone());
                            } else {
                                user_match.get_or_insert_with(|| en.clone());
                            }
                        }
                    }
                    if let Some(name) = user_match.or(seed_match) {
                        return Some(name);
                    }
                }
                ExprKind::Path { segments, .. } => {
                    if let Some(first) = segments.first() {
                        if self.enum_layouts.contains_key(first) {
                            return Some(first.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Phase 7.2 Slice DP — register a value-type enum alloca for
    /// scope-exit drop-function invocation. Per design lock DP1, the
    /// registration site is at let-binding time (not inside
    /// `try_compile_enum_variant` — the variant constructor returns a
    /// `BasicValueEnum` aggregate before the alloca exists; the alloca
    /// is created by `bind_pattern_values`). Per DP3, `is_shared` enums
    /// are filtered upstream — RC inc/dec via `track_rc_var` handles
    /// their cleanup through refcount semantics. Per DP4, the
    /// scope-exit drain emits a single `call drop_fn(alloca)` for the
    /// `EnumDrop` action; move-suppression for caller→callee passing
    /// is implicit in the existing convention that function parameters
    /// don't register `track_enum_var` (mirrors how Vec/String params
    /// don't register `track_vec_var` — only the let-binding site
    /// owns cleanup, so the param is a stranded view of the same
    /// payload words and no double-free can occur).
    pub(super) fn track_enum_var(&mut self, enum_name: &str, enum_alloca: PointerValue<'ctx>) {
        // DP3 carve-out: shared enums use the RC-pointer cleanup path
        // (refcount-driven free in `emit_rc_dec`). The drop-switch
        // machinery is for value-type enums only.
        let is_shared = self
            .enum_layouts
            .get(enum_name)
            .map(|l| l.is_shared)
            .unwrap_or(false);
        if is_shared {
            return;
        }
        // Skip enums with no heap-bearing payload anywhere — emitting
        // a no-op drop call would just bloat IR. The drop-fn helper
        // returns `None` when every variant's `field_drop_kinds` is
        // entirely `EnumDropKind::None`.
        let drop_fn = match self.emit_enum_drop_switch(enum_name) {
            Some(f) => f,
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::EnumDrop {
                enum_alloca,
                drop_fn,
            });
        }
    }

    /// Register a scope-exit free of an `Option[T]` binding's inline heap
    /// `Some` payload (`Option[String]` / `Option[Vec[U]]`), keyed on the
    /// CONCRETE payload type — the type-erased `Option` layout's drop
    /// switch (`track_enum_var`) is a no-op for it (it'd be wrong for
    /// `Option[i64]`), so without this the payload leaks whenever the
    /// Option is dropped without being destructured (B-2026-06-10-6).
    /// No-op when `T` is not an inline heap Vec/String. Also records the
    /// binding name so a `match`/`if let` arm that binds the payload out
    /// can zero the source `cap` (option field 3) and avoid a double-free
    /// (`suppress_inline_option_payload_cleanup`).
    pub(super) fn track_inline_option_payload_var(
        &mut self,
        var_name: &str,
        option_slot: PointerValue<'ctx>,
        option_te: &TypeExpr,
    ) {
        let Some(payload_elem_ty) = self.option_inline_payload_elem(option_te) else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Option") else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        // Nested-block let (`if c { let x = mk(); … }`): the slot's alloca
        // is hoisted to the entry block; on a not-taken path the
        // `bind_pattern` store never runs, leaving the tag `undef` — which
        // could spuriously match `Some` and free a garbage pointer at a
        // function-level drain. Zero the slot in the entry block (tag=0 =>
        // None => the action skips). Mirrors the shared-/boxed-Option paths.
        let is_nested = self
            .current_fn
            .and_then(|f| f.get_first_basic_block())
            .zip(self.builder.get_insert_block())
            .map(|(entry, cur)| entry != cur)
            .unwrap_or(false);
        if is_nested {
            self.zero_init_option_slot_in_entry_block(option_slot, option_ty);
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineOptionPayload {
                option_slot,
                option_ty,
                some_tag,
                payload_elem_ty: Some(payload_elem_ty),
            });
        }
        self.inline_option_payload_vars.insert(var_name.to_string());
    }

    /// Register a scope-exit drop of an `Option[P]` binding whose `Some`
    /// payload `P` is a NON-shared user STRUCT or value ENUM the recursive
    /// drop family fully frees (B-2026-07-03-27). The struct/enum sibling of
    /// `track_inline_option_payload_var` (which only covers the inline
    /// `String`/`Vec` `{ptr,len,cap}` overlay): `option_inline_payload_elem`
    /// returns `None` for a struct/enum payload, so those `Option` locals —
    /// e.g. a `let A { value } = a` destructure of `struct A { value:
    /// Option[Val] }`, `Val` a heap enum — got no cleanup and leaked the
    /// payload. Routes the slot through the payload-type-aware, tag-guarded
    /// `karac_drop_Option_<payload>` (`emit_option_drop_fn`, the exact fn the
    /// `Vec[Option[..]]` element path uses — it handles both the inline and the
    /// heap-BOXED wide-payload cases). No-op when the payload isn't a
    /// recursive-drop-supported struct/enum. Records the binding name so a
    /// `match`/`if let` arm that binds the `Some` payload out can zero the
    /// source tag and avoid a double-free
    /// (`suppress_inline_option_agg_payload_cleanup`).
    pub(super) fn track_inline_option_agg_payload_var(
        &mut self,
        var_name: &str,
        option_slot: PointerValue<'ctx>,
        option_te: &TypeExpr,
    ) {
        let TypeKind::Path(p) = &option_te.kind else {
            return;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return;
        }
        let Some(GenericArg::Type(payload)) = p.generic_args.as_ref().and_then(|a| a.first())
        else {
            return;
        };
        if !self.option_payload_struct_or_enum_drop_ok(payload) {
            return;
        }
        let payload = payload.clone();
        let Some(layout) = self.enum_layouts.get("Option") else {
            return;
        };
        let option_ty = layout.llvm_type;
        // Nested-block let: an untaken path leaves the tag `undef`, which could
        // spuriously match `Some`; zero the slot in the entry block (mirrors
        // `track_inline_option_payload_var`).
        let is_nested = self
            .current_fn
            .and_then(|f| f.get_first_basic_block())
            .zip(self.builder.get_insert_block())
            .map(|(entry, cur)| entry != cur)
            .unwrap_or(false);
        if is_nested {
            self.zero_init_option_slot_in_entry_block(option_slot, option_ty);
        }
        // Emit (or fetch) the tag-guarded `karac_drop_Option_<payload>` — may
        // move the builder's insert block, so resolve it before queuing.
        let Some(drop_fn) = self.emit_option_drop_fn(&payload) else {
            return;
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::EnumDrop {
                enum_alloca: option_slot,
                drop_fn,
            });
        }
        self.inline_option_agg_payload_vars
            .insert(var_name.to_string());
    }

    /// Free a discarded inline-heap `Option` temporary in statement position
    /// (`v.pop();`, `make_opt();`). Materializes the value into a slot and
    /// queues a `FreeInlineOptionPayload` keyed on the instantiated type from
    /// `enum_inst_type_exprs` (the erased `Option` drop switch can't free the
    /// concrete payload — B-2026-06-10-6). Returns `true` when it registered a
    /// free. A discarded temp has no binding / `match`, so the free is
    /// unconditional — no move-out suppression. The CALLER must exclude
    /// borrow-returning producers (`scrutinee_is_borrow_call`): `Map.get` /
    /// `Vec.get` return an `Option` whose payload ALIASES the container's
    /// storage, so freeing it would corrupt the container.
    pub(super) fn try_track_discarded_inline_option(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> bool {
        let key = (tail.span.offset, tail.span.length);
        let Some(te) = self.enum_inst_type_exprs.get(&key).cloned() else {
            return false;
        };
        let Some(payload_elem_ty) = self.option_inline_payload_elem(&te) else {
            return false;
        };
        let Some(layout) = self.enum_layouts.get("Option") else {
            return false;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_opt_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineOptionPayload {
                option_slot: slot,
                option_ty,
                some_tag,
                payload_elem_ty: Some(payload_elem_ty),
            });
            return true;
        }
        false
    }

    /// SHARED-payload sibling of `try_track_discarded_inline_option`
    /// (B-2026-07-19-16): a statement-position `Option[shared T]` temporary —
    /// canonically the discarded result of `m.remove(k)` over a
    /// `Map[K, shared V]`. `Map.remove` MOVES the value out of the bucket
    /// (the runtime tombstones the slot and frees only the key; the value's
    /// ref is handed back inside `Some(old)`), so the discarded temp OWNS
    /// that ref. The inline/boxed trackers decline a shared payload (it is
    /// rc-managed, not buffer-owned) and `materialize_owned_temp` has no
    /// `Option` arm, so the ref was never released — one leaked box per
    /// discarded remove. Queue the same tag-guarded `RcDecOption` a
    /// let-binding's scope-exit release uses, firing at the `;` via the
    /// discard frame. The span-table leg also covers other owned
    /// `Option[shared T]` producers discarded in statement position (call
    /// returns); a BOUND result was already released by the binding's own
    /// `track_rc_option_var`. Deliberately NOT extended to a displacing
    /// `m.insert(k, v2);` here without separate verification of its
    /// displaced-value balance.
    pub(super) fn try_track_discarded_shared_option(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> bool {
        // A MethodCall producer must be `Map.remove` — the only builtin
        // discard shape that hands the caller an owned ref. A displacing
        // `m.insert(k, v2)`'s old value is dec'd INLINE by the insert
        // lowering (the `map.ins.some` rc_dec), so a second dec here would
        // double-free (the insert's result te CAN land on this span via the
        // receiver-span overwrite); `get`/`first`/`last` are borrows.
        if let ExprKind::MethodCall { method, .. } = &tail.kind {
            if method != "remove" {
                return false;
            }
        }
        let key = (tail.span.offset, tail.span.length);
        // Payload `TypeExpr`: prefer the span-keyed `Option[V]` record (a
        // call-shaped producer), but a `MethodCall` node reuses its
        // receiver-side span, so `m.remove(k)` has no entry there — derive
        // `V` from the Map receiver's value-te side table instead
        // (`var_elem_type_exprs` holds the value of a Map variable).
        let payload_te = match self.enum_inst_type_exprs.get(&key) {
            Some(te) => match &te.kind {
                TypeKind::Path(p) if p.segments.last().map(|s| s.as_str()) == Some("Option") => {
                    match p.generic_args.as_ref().and_then(|a| a.first()) {
                        Some(GenericArg::Type(v)) => Some(v.clone()),
                        _ => None,
                    }
                }
                _ => None,
            },
            None => match &tail.kind {
                ExprKind::MethodCall { object, method, .. } if method == "remove" => {
                    match &object.kind {
                        ExprKind::Identifier(m) if self.map_val_types.contains_key(m.as_str()) => {
                            self.var_elem_type_exprs.get(m.as_str()).cloned()
                        }
                        _ => None,
                    }
                }
                _ => None,
            },
        };
        let Some(payload_te) = payload_te else {
            return false;
        };
        let Some(heap_type) = self.shared_heap_type_for_type_expr(&payload_te) else {
            return false;
        };
        let BasicTypeEnum::StructType(option_ty) = val.get_type() else {
            return false;
        };
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_shared_opt_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        self.track_rc_option_var("__owned_shared_opt_tmp", slot, option_ty, heap_type);
        true
    }

    /// BOXED-payload sibling of `try_track_discarded_inline_option` (slice
    /// 3r): a statement-position `Option[Wide]` temporary — the discarded
    /// result of `m.insert(k, v2)` over an existing key (the displaced old
    /// value) or `m.remove(k)` (the moved-out value) on a struct-valued map
    /// — carries a heap BOX (`coerce_to_payload_words`' boxing path fires
    /// when the payload exceeds the 3-word inline area). Nothing owned it:
    /// both the box allocation and the payload's interior heap (`Holder`'s
    /// `name`) leaked once per displaced/removed entry. Queue a
    /// `BoxedEnumDrop` with the payload struct's `__karac_drop_struct_<T>`
    /// as the inner walk — the discarded value is FULLY owned here (unlike
    /// a borrow-call `match` scrutinee, whose box-only free leg 2 pinned),
    /// so the interior walk is correct. Returns false (keep other discard
    /// paths probing) for a non-`Option` te, a payload that fits inline, or
    /// a payload that isn't a non-shared user struct.
    pub(super) fn try_track_discarded_boxed_option(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> bool {
        let key = (tail.span.offset, tail.span.length);
        let Some(te) = self.enum_inst_type_exprs.get(&key).cloned() else {
            return false;
        };
        let TypeKind::Path(p) = &te.kind else {
            return false;
        };
        if p.segments.last().map(|s| s.as_str()) != Some("Option") {
            return false;
        }
        let Some(GenericArg::Type(payload_te)) =
            p.generic_args.as_ref().and_then(|a| a.first()).cloned()
        else {
            return false;
        };
        let TypeKind::Path(pp) = &payload_te.kind else {
            return false;
        };
        let Some(struct_name) = pp.segments.last().cloned() else {
            return false;
        };
        if !self.struct_types.contains_key(struct_name.as_str())
            || self.shared_types.contains_key(struct_name.as_str())
        {
            return false;
        }
        // Inline payloads (≤ 3 words) are the inline tracker's job.
        let payload_ty = self.llvm_type_for_type_expr(&payload_te);
        if Self::llvm_type_word_count(payload_ty) <= 3 {
            return false;
        }
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_boxed_opt_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        self.track_boxed_enum_var(
            "__owned_boxed_opt_tmp",
            slot,
            "Option",
            "Some",
            Some(struct_name.as_str()),
        );
        true
    }

    /// `Result[T, E]` sibling of `track_inline_option_payload_var`. Registers
    /// a scope-exit free of a `Result` binding's inline heap `Ok`/`Err`
    /// payload keyed on the concrete per-variant element types — the erased
    /// `Result` layout's drop switch can't free them (B-2026-06-10-6). No-op
    /// when neither half is an inline heap Vec/String. Records the binding
    /// name in `inline_result_payload_vars` so a `match`/`if let` arm that
    /// binds the `Ok`/`Err` payload out can zero the source `cap` and avoid
    /// a double-free (`suppress_inline_result_payload_cleanup`).
    pub(super) fn track_inline_result_payload_var(
        &mut self,
        var_name: &str,
        result_slot: PointerValue<'ctx>,
        result_te: &TypeExpr,
    ) {
        let (ok_payload_elem_ty, err_payload_elem_ty) = self
            .result_inline_payload_elems(result_te)
            .unwrap_or((None, None));
        // Struct-with-heap payload drops (B-2026-07-12-2 gap 3) — the overlay
        // `_elems` above only covers a direct `String`/`Vec` (or transparent
        // wrapper of one); a multi-field struct-with-heap payload needs a full
        // drop. Register if either overlay OR struct-drop half has heap.
        let (ok_payload_struct_drop, err_payload_struct_drop) =
            self.result_inline_payload_struct_drops(result_te);
        if ok_payload_elem_ty.is_none()
            && err_payload_elem_ty.is_none()
            && ok_payload_struct_drop.is_none()
            && err_payload_struct_drop.is_none()
        {
            return;
        }
        let Some(layout) = self.enum_layouts.get("Result") else {
            return;
        };
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        // Nested-block let: zero the slot in the entry block so a not-taken
        // path's `undef` tag can't spuriously match `Ok`/`Err` at a function-
        // level drain. Mirrors the Option path.
        let is_nested = self
            .current_fn
            .and_then(|f| f.get_first_basic_block())
            .zip(self.builder.get_insert_block())
            .map(|(entry, cur)| entry != cur)
            .unwrap_or(false);
        if is_nested {
            self.zero_init_option_slot_in_entry_block(result_slot, result_ty);
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineResultPayload {
                result_slot,
                result_ty,
                ok_tag,
                err_tag,
                ok_payload_elem_ty,
                err_payload_elem_ty,
                ok_payload_struct_drop,
                err_payload_struct_drop,
            });
        }
        self.inline_result_payload_vars.insert(var_name.to_string());
    }

    /// `Result[T, E]` sibling of `try_track_discarded_inline_option` — frees a
    /// discarded inline-heap `Result` temporary in statement position. Same
    /// borrow-exclusion obligation on the CALLER (`scrutinee_is_borrow_call`).
    pub(super) fn try_track_discarded_inline_result(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> bool {
        let key = (tail.span.offset, tail.span.length);
        let Some(te) = self.enum_inst_type_exprs.get(&key).cloned() else {
            return false;
        };
        let (ok_payload_elem_ty, err_payload_elem_ty) = self
            .result_inline_payload_elems(&te)
            .unwrap_or((None, None));
        // Struct-with-heap payload drops (B-2026-07-12-2 gap 3): a discarded
        // `Result` temp whose payload is a multi-field struct-with-heap needs a
        // full drop the overlay `_elems` can't provide.
        let (ok_payload_struct_drop, err_payload_struct_drop) =
            self.result_inline_payload_struct_drops(&te);
        if ok_payload_elem_ty.is_none()
            && err_payload_elem_ty.is_none()
            && ok_payload_struct_drop.is_none()
            && err_payload_struct_drop.is_none()
        {
            return false;
        }
        let Some(layout) = self.enum_layouts.get("Result") else {
            return false;
        };
        let result_ty = layout.llvm_type;
        let ok_tag = layout.tags.get("Ok").copied().unwrap_or(0);
        let err_tag = layout.tags.get("Err").copied().unwrap_or(1);
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_res_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineResultPayload {
                result_slot: slot,
                result_ty,
                ok_tag,
                err_tag,
                ok_payload_elem_ty,
                err_payload_elem_ty,
                ok_payload_struct_drop,
                err_payload_struct_drop,
            });
            return true;
        }
        false
    }

    /// `Option[Map]` / `Option[Set]` sibling of
    /// `track_inline_option_payload_var`. Registers a scope-exit free of the
    /// `Some` handle payload via `FreeInlineOptionMapPayload`; no-op for any
    /// other `Option` arg. Records the binding in
    /// `inline_option_map_payload_vars` so a `match`/`if let` arm binding the
    /// `Some` payload out sets the source tag to `None`
    /// (`suppress_inline_option_map_payload_cleanup`) and the free skips.
    pub(super) fn track_inline_option_map_payload_var(
        &mut self,
        var_name: &str,
        option_slot: PointerValue<'ctx>,
        option_te: &TypeExpr,
    ) {
        let Some(map_drop) = self.option_inline_map_payload(option_te) else {
            return;
        };
        let Some(layout) = self.enum_layouts.get("Option") else {
            return;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let is_nested = self
            .current_fn
            .and_then(|f| f.get_first_basic_block())
            .zip(self.builder.get_insert_block())
            .map(|(entry, cur)| entry != cur)
            .unwrap_or(false);
        if is_nested {
            self.zero_init_option_slot_in_entry_block(option_slot, option_ty);
        }
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineOptionMapPayload {
                option_slot,
                option_ty,
                some_tag,
                map_drop,
            });
        }
        self.inline_option_map_payload_vars
            .insert(var_name.to_string());
    }

    /// `Option[Map]`/`Option[Set]` sibling of
    /// `try_track_discarded_inline_option` — frees a discarded inline-handle
    /// `Option[Map]` temp in statement position. Same caller borrow-exclusion
    /// obligation.
    pub(super) fn try_track_discarded_inline_option_map(
        &mut self,
        tail: &Expr,
        val: BasicValueEnum<'ctx>,
    ) -> bool {
        let key = (tail.span.offset, tail.span.length);
        let Some(te) = self.enum_inst_type_exprs.get(&key).cloned() else {
            return false;
        };
        let Some(map_drop) = self.option_inline_map_payload(&te) else {
            return false;
        };
        let Some(layout) = self.enum_layouts.get("Option") else {
            return false;
        };
        let option_ty = layout.llvm_type;
        let some_tag = layout.tags.get("Some").copied().unwrap_or(1);
        let Some(cur_fn) = self
            .builder
            .get_insert_block()
            .and_then(|bb| bb.get_parent())
        else {
            return false;
        };
        let slot = self.create_entry_alloca(cur_fn, "__owned_optmap_tmp", val.get_type());
        self.builder.build_store(slot, val).unwrap();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::FreeInlineOptionMapPayload {
                option_slot: slot,
                option_ty,
                some_tag,
                map_drop,
            });
            return true;
        }
        false
    }

    /// Emit the cap-guarded free of an inline `{ptr,len,cap}` heap payload
    /// that overlays the words of a tagged-union enum slot, starting at
    /// payload field index 1 (the first word past the tag). Shared by the
    /// `FreeInlineOptionPayload` (one `Some` variant) and
    /// `FreeInlineResultPayload` (two `Ok`/`Err` variants) cleanups — the
    /// caller has already tag-checked and positioned the builder at the
    /// variant-taken block; this frees that variant's payload overlay and
    /// leaves the builder positioned at its internal skip block (a no-op
    /// `cap == 0` for string-literal / empty payloads). `payload_elem_ty`
    /// drives the one-level recursive inner free for a Vec-struct element
    /// (`Option[Vec[String]]` / `Result[_, Vec[U]]`), mirroring
    /// `FreeVecBuffer`. `label` disambiguates the emitted block names so a
    /// two-variant Result emits distinct `respl.ok.*` / `respl.err.*` blocks.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_free_inline_payload_overlay(
        &self,
        enum_slot: PointerValue<'ctx>,
        enum_ty: StructType<'ctx>,
        payload_elem_ty: Option<BasicTypeEnum<'ctx>>,
        fn_val: FunctionValue<'ctx>,
        vec_ty: StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
        label: &str,
    ) {
        let zero = i64_t.const_int(0, false);
        let payload_base = self
            .builder
            .build_struct_gep(enum_ty, enum_slot, 1, &format!("{label}.payload"))
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(vec_ty, payload_base, 2, &format!("{label}.cap.ptr"))
            .unwrap();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, &format!("{label}.cap"))
            .unwrap()
            .into_int_value();
        // SSO forward-prep (see `sso.rs`): owned-heap ⇔ signed `cap > 0`.
        let is_heap = self.sso_string_is_owned_heap(cap);
        let free_bb = self
            .context
            .append_basic_block(fn_val, &format!("{label}.free"));
        let skip_bb = self
            .context
            .append_basic_block(fn_val, &format!("{label}.skip"));
        self.builder
            .build_conditional_branch(is_heap, free_bb, skip_bb)
            .unwrap();
        self.builder.position_at_end(free_bb);
        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, payload_base, 0, &format!("{label}.data.ptr"))
            .unwrap();
        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, &format!("{label}.data"))
            .unwrap()
            .into_pointer_value();
        // One-level recursive inner free for a Vec-struct payload element
        // (`Vec[String]` / `Vec[Vec[_]]`): each live element owns its own
        // data buffer. Same shape as `FreeVecBuffer`'s inner loop; `i8`
        // (String) / primitive elements skip it. Deeper nesting still leaks
        // the innermost buffers (the documented `FreeVecBuffer` limitation).
        if let Some(et) = payload_elem_ty {
            if self.llvm_ty_is_vec_struct(et) {
                let vstruct = self.vec_struct_type();
                let len_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, payload_base, 1, &format!("{label}.len.ptr"))
                    .unwrap();
                let len = self
                    .builder
                    .build_load(i64_t, len_ptr, &format!("{label}.len"))
                    .unwrap()
                    .into_int_value();
                let counter = self.create_entry_alloca(fn_val, &format!("{label}.i"), i64_t.into());
                self.builder.build_store(counter, zero).unwrap();
                let cond_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{label}.drop.cond"));
                let body_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{label}.drop.body"));
                let after_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{label}.drop.after"));
                self.builder.build_unconditional_branch(cond_bb).unwrap();
                self.builder.position_at_end(cond_bb);
                let cur = self
                    .builder
                    .build_load(i64_t, counter, &format!("{label}.drop.cur"))
                    .unwrap()
                    .into_int_value();
                let lt = self
                    .builder
                    .build_int_compare(IntPredicate::ULT, cur, len, &format!("{label}.drop.lt"))
                    .unwrap();
                self.builder
                    .build_conditional_branch(lt, body_bb, after_bb)
                    .unwrap();
                self.builder.position_at_end(body_bb);
                let inner = unsafe {
                    self.builder
                        .build_gep(vstruct, data, &[cur], &format!("{label}.drop.elem"))
                        .unwrap()
                };
                let inner_cap_ptr = self
                    .builder
                    .build_struct_gep(vstruct, inner, 2, &format!("{label}.drop.inner.cap.ptr"))
                    .unwrap();
                let inner_cap = self
                    .builder
                    .build_load(i64_t, inner_cap_ptr, &format!("{label}.drop.inner.cap"))
                    .unwrap()
                    .into_int_value();
                let inner_is_heap = self
                    .builder
                    .build_int_compare(
                        IntPredicate::UGT,
                        inner_cap,
                        zero,
                        &format!("{label}.drop.inner.is_heap"),
                    )
                    .unwrap();
                let inner_free_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{label}.drop.inner.free"));
                let inner_skip_bb = self
                    .context
                    .append_basic_block(fn_val, &format!("{label}.drop.inner.skip"));
                self.builder
                    .build_conditional_branch(inner_is_heap, inner_free_bb, inner_skip_bb)
                    .unwrap();
                self.builder.position_at_end(inner_free_bb);
                let inner_data_ptr = self
                    .builder
                    .build_struct_gep(vstruct, inner, 0, &format!("{label}.drop.inner.data.ptr"))
                    .unwrap();
                let inner_data = self
                    .builder
                    .build_load(ptr_ty, inner_data_ptr, &format!("{label}.drop.inner.data"))
                    .unwrap()
                    .into_pointer_value();
                self.builder
                    .build_call(self.free_fn, &[inner_data.into()], "")
                    .unwrap();
                self.builder
                    .build_unconditional_branch(inner_skip_bb)
                    .unwrap();
                self.builder.position_at_end(inner_skip_bb);
                let one = i64_t.const_int(1, false);
                let next = self
                    .builder
                    .build_int_add(cur, one, &format!("{label}.drop.next"))
                    .unwrap();
                self.builder.build_store(counter, next).unwrap();
                self.builder.build_unconditional_branch(cond_bb).unwrap();
                self.builder.position_at_end(after_bb);
            }
        }
        // Recycling-aware release; hint = cap × sizeof(payload element)
        // (phase-10 line 282), so a mid-size inline `Vec[T]` payload parks. The
        // `&self` context reads the cached `target_data` directly; `None`
        // element (untyped) falls back to 1.
        let overlay_elem_size = payload_elem_ty
            .and_then(|et| self.target_data.as_ref().map(|td| td.get_abi_size(&et)))
            .unwrap_or(1);
        self.emit_free_buf_call(data, cap, overlay_elem_size);
        self.builder.build_unconditional_branch(skip_bb).unwrap();
        self.builder.position_at_end(skip_bb);
    }

    /// Track a non-shared struct alloca for scope-exit drop-fn invocation.
    /// Mirrors `track_enum_var` but for struct types. The per-struct drop
    /// fn is lazily synthesized by `emit_struct_drop_synthesis`; if the
    /// struct has no heap-owning fields (every field is primitive / Slice
    /// / Ref / etc.) the synthesis returns `None` and we skip registration
    /// — there's nothing to drop. Shared structs use the RC machinery
    /// (`track_rc_var` / `emit_refcount_dec`) and are also filtered out by
    /// `emit_struct_drop_synthesis`.
    ///
    /// Closes the 2026-05-14 leak class for `struct Holder { v: Vec[i64] }`
    /// / `struct Cache { entries: Map[String, V] }` / `Vec[Container]`
    /// (slice γ of the recursive-drop work). Without this, a let-binding
    /// of a struct value never drops its Vec/Map/Set field contents on
    /// scope exit — only the struct's own inline storage (the
    /// `{ptr, len, cap}` field for a Vec field) was released, the actual
    /// heap-allocated backing buffer leaked.
    /// Build the `param -> concrete arg` substitution for a generic struct
    /// binding from its recorded instantiation TypeExpr (`S[String]` →
    /// `{T: String}`). Empty for a non-generic struct or when the instantiation
    /// doesn't name this struct. Used to thread the concrete instantiation into
    /// per-monomorph struct-drop synthesis (B-2026-07-11-35 push leg).
    pub(super) fn generic_struct_subst_from_inst(
        &self,
        struct_name: &str,
        inst: &TypeExpr,
    ) -> std::collections::HashMap<String, TypeExpr> {
        let mut subst = std::collections::HashMap::new();
        if let TypeKind::Path(p) = &inst.kind {
            if p.segments.last().map(String::as_str) == Some(struct_name) {
                if let (Some(params), Some(args)) = (
                    self.struct_generic_params.get(struct_name),
                    p.generic_args.as_ref(),
                ) {
                    for (param, arg) in params.iter().zip(args.iter()) {
                        if let GenericArg::Type(te) = arg {
                            subst.insert(param.clone(), te.clone());
                        }
                    }
                }
            }
        }
        subst
    }

    pub(super) fn track_struct_var(
        &mut self,
        struct_name: &str,
        struct_alloca: PointerValue<'ctx>,
    ) {
        self.track_struct_var_inst(struct_name, struct_alloca, None);
    }

    /// `track_struct_var` with an explicit generic instantiation (`S[String]`),
    /// so the scope-exit drop is the per-monomorph
    /// `__karac_drop_struct_S$String` that drains the concrete `Vec[String]`
    /// field's element buffers — not the name-shared `__karac_drop_struct_S`
    /// that resolves the element from bare `T` and leaks every element
    /// (B-2026-07-11-35 push leg). A `None` instantiation (or a non-generic
    /// struct) reproduces the original name-keyed behavior exactly.
    pub(super) fn track_struct_var_inst(
        &mut self,
        struct_name: &str,
        struct_alloca: PointerValue<'ctx>,
        inst: Option<TypeExpr>,
    ) {
        // B-2026-07-03-28 shared leg — a struct that transitively owns a
        // `shared` / `Option[shared]` / `Vec[shared]` field needs the COMBINED
        // drop (value-drop `__karac_drop_struct_<S>` PLUS the shared-field
        // rc-dec walker `emit_nested_struct_shared_rc_decs`), not the value
        // drop alone. The value drop SKIPS shared fields by design (they are
        // RC-machinery, not buffer-owned), so without the walker a scope-exit
        // drop of an owning struct local / callee-owned by-value param never
        // rc-decs its shared children — the direct-shared-field local leak
        // (s1/s3 probes) and the Option[shared] param leak that the
        // caller-retains entry-copy's rc-INC would otherwise strand. The
        // combined drop passes `owns_buffer_free=false` so it does NOT re-free
        // the String/Vec buffers the value drop already freed (copy-depth ==
        // drop-depth stays intact). Structs with no shared field keep the plain
        // value drop — zero behavior change for them.
        let drop_fn = if self.struct_owns_shared_field(struct_name, &mut Vec::new()) {
            match self.emit_vec_elem_struct_with_shared_drop_fn(struct_name) {
                Some(f) => f,
                None => return,
            }
        } else {
            let subst = inst
                .as_ref()
                .map(|i| self.generic_struct_subst_from_inst(struct_name, i))
                .unwrap_or_default();
            match self.emit_struct_drop_synthesis_mono(struct_name, &subst) {
                Some(f) => f,
                None => return,
            }
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::StructDrop {
                struct_alloca,
                drop_fn,
            });
        }
    }

    /// Phase 7 user-`impl Drop` dispatch Prereq.3 — track a struct
    /// alloca for scope-exit invocation of its `karac_drop_<Type>`
    /// wrapper. Used in place of `track_struct_var` when the binding's
    /// type has a user-defined `impl Drop` — the wrapper's body already
    /// invokes the existing `__karac_drop_struct_<Type>` synthesiser
    /// internally after running the user body, so registering both
    /// would double-cleanup the fields. Returns `()` either way; falls
    /// through to no-op (no action pushed) when the wrapper isn't in
    /// the cache (shouldn't happen — `emit_user_drop_wrappers` runs
    /// before the function-body compile pass).
    pub(super) fn track_user_drop_var(
        &mut self,
        type_name: &str,
        binding_name: &str,
        binding_ptr: PointerValue<'ctx>,
    ) {
        let drop_fn = match self.user_drop_wrapper_fns.get(type_name) {
            Some(f) => *f,
            None => return,
        };
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.push(CleanupAction::UserDrop {
                binding_name: binding_name.to_string(),
                binding_ptr,
                drop_fn,
                type_name: type_name.to_string(),
            });
        }
    }

    /// NLL live-range-end firing for user-`impl Drop` bindings
    /// (B-2026-07-21-1). design.md § Drop ordering: "Destructors fire at
    /// each binding's live-range end, not lexical scope end … a value whose
    /// last use is mid-scope is dropped at that use and does not appear in
    /// the end-of-scope stack at all." Codegen previously ran EVERY user
    /// drop body at scope exit — observationally divergent from the
    /// interpreter (which implements NLL placement) whenever the drop body
    /// has side effects. Called by `compile_block` after each statement
    /// with the block's precomputed last-use map (the same
    /// `compute_block_last_use` analysis the interpreter uses, so both
    /// backends agree statement-for-statement); fires every due entry in
    /// LIFO order (reverse introduction — the §867 single-stack rule) and
    /// removes it from the frame so the scope-exit drain never re-fires it.
    ///
    /// Gated to non-shared STRUCT user-drops: their let-path registration
    /// is mutually exclusive with every other cleanup action (the wrapper
    /// runs field cleanup internally), so early-firing + removal is
    /// complete. Enum user-drops (dual-registered with a complementary
    /// `EnumDrop` payload walk) and par-branch registrations (empty
    /// `type_name`) stay at scope exit — a conservative residual, never a
    /// double-fire. Memory-only drops (no user body) also stay at scope
    /// exit: with no observable side effects, scope-exit free is
    /// equivalent to NLL free.
    pub(super) fn fire_due_user_drops(
        &mut self,
        last_use: &std::collections::HashMap<String, usize>,
        stmt_idx: usize,
    ) {
        // A terminated insert block (the statement ended in return/break)
        // cannot take the drop call — and doesn't need it: the exit-path
        // cleanup drain already handles the frame's remaining entries.
        if self
            .builder
            .get_insert_block()
            .and_then(|b| b.get_terminator())
            .is_some()
        {
            return;
        }
        let due: Vec<(String, PointerValue<'ctx>, FunctionValue<'ctx>)> = {
            let Some(frame) = self.scope_cleanup_actions.last() else {
                return;
            };
            frame
                .iter()
                .rev()
                .filter_map(|a| match a {
                    CleanupAction::UserDrop {
                        binding_name,
                        binding_ptr,
                        drop_fn,
                        type_name,
                    } if last_use.get(binding_name.as_str()).copied() == Some(stmt_idx)
                        && self.struct_types.contains_key(type_name.as_str())
                        && !self.shared_types.contains_key(type_name.as_str()) =>
                    {
                        Some((binding_name.clone(), *binding_ptr, *drop_fn))
                    }
                    _ => None,
                })
                .collect()
        };
        if due.is_empty() {
            return;
        }
        for (name, ptr, drop_fn) in &due {
            self.builder
                .build_call(*drop_fn, &[(*ptr).into()], &format!("nll.drop.{name}"))
                .unwrap();
        }
        let names: std::collections::HashSet<&str> =
            due.iter().map(|(n, _, _)| n.as_str()).collect();
        if let Some(frame) = self.scope_cleanup_actions.last_mut() {
            frame.retain(|a| {
                !matches!(a, CleanupAction::UserDrop { binding_name, .. }
                    if names.contains(binding_name.as_str()))
            });
        }
    }

    /// Move-suppression for user-Drop bindings — remove the
    /// `CleanupAction::UserDrop` entry for `name` from the cleanup
    /// stack so it does NOT fire at scope exit. Used at `let g = f;`
    /// (RHS is an Identifier) when `f`'s value is moved into `g`;
    /// without suppression both bindings would drop the same logical
    /// value, double-closing fds / double-dropping resources. Walks
    /// all frames (inner-most first) so the suppression works even
    /// for moves out of nested scopes — though the v1 caller in
    /// `stmts.rs` only ever suppresses within the current frame
    /// because that's where the source binding lives.
    pub(super) fn suppress_user_drop_for_var(&mut self, name: &str) {
        for frame in self.scope_cleanup_actions.iter_mut().rev() {
            frame.retain(|action| match action {
                CleanupAction::UserDrop { binding_name, .. } => binding_name != name,
                _ => true,
            });
        }
    }

    /// Channel sibling of [`suppress_user_drop_for_var`]: drop the parent's
    /// scope-exit `DropChannelEnd` for a channel end (`Sender`/`Receiver`)
    /// `name` that was moved into a spawned task (which now owns the drop).
    /// `DropChannelEnd` keys on the binding's *alloca*, not its name, so this
    /// resolves `name` to its parent slot and matches `chan_alloca`. No-op
    /// when `name` has no live slot or no channel cleanup queued.
    pub(super) fn suppress_channel_drop_for_var(&mut self, name: &str) {
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let target = slot.ptr;
        for frame in self.scope_cleanup_actions.iter_mut().rev() {
            frame.retain(|action| {
                !matches!(
                    action,
                    CleanupAction::DropChannelEnd { chan_alloca, .. } if *chan_alloca == target
                )
            });
        }
    }

    /// **Branch-safe** channel-end move suppression for the `let keep = rx;`
    /// rebind site. [`suppress_channel_drop_for_var`] retracts the queued
    /// `DropChannelEnd` outright (compile-time) — correct at TERMINAL move sites
    /// (`return rx`, `spawn` capture) where no other path still owns the source,
    /// but WRONG for a branch-buried rebind: a `let keep = rx;` inside one arm of
    /// an `if` would unconditionally remove `rx`'s drop, so the OTHER arm (which
    /// never moved `rx`) leaks the `KaracChannel` at scope exit.
    ///
    /// Instead, KEEP the source's `DropChannelEnd` queued and neutralize it with
    /// a runtime in-slot sentinel: store a null handle into the source slot at
    /// the move site, so the (retained) drop loads null and no-ops — but only on
    /// the path that actually executed the move (the store lives in that BB).
    /// This is the channel analog of the Vec/String `cap = 0` sentinel
    /// ([`zero_vec_alloca_cap`]); it works because `karac_runtime_channel_drop_*`
    /// treat a null handle as a no-op. A channel end is affine, so the source is
    /// never read again on the move path — nulling its slot is safe.
    ///
    /// Gated to a source that actually carries a queued `DropChannelEnd` (an
    /// OWNER): a `ref Sender`/`ref Receiver` borrow has none, so this no-ops and
    /// never nulls a borrow's slot. Mirrors `suppress_channel_drop_for_var`'s
    /// "no channel cleanup queued → no-op" discipline.
    pub(super) fn neutralize_moved_channel_end_slot(&self, name: &str) {
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let target = slot.ptr;
        let has_queued_drop = self.scope_cleanup_actions.iter().any(|frame| {
            frame.iter().any(|action| {
                matches!(
                    action,
                    CleanupAction::DropChannelEnd { chan_alloca, .. } if *chan_alloca == target
                )
            })
        });
        if !has_queued_drop {
            return;
        }
        let null = self.context.ptr_type(AddressSpace::default()).const_null();
        let _ = self.builder.build_store(target, null);
    }

    /// Heap-buffer sibling of [`suppress_user_drop_for_var`] /
    /// [`suppress_channel_drop_for_var`]: drop the parent's scope-exit
    /// `FreeVecBuffer` for a `String` / `Vec[T]` binding `name` whose
    /// `{data, len, cap}` header was moved (e.g. bitwise-copied into a
    /// spawned task's capture env, which now owns and frees the buffer).
    /// `FreeVecBuffer` keys on the binding's *alloca*, not its name — and is
    /// type-agnostic, so this matches by `slot.ptr` rather than a nominal
    /// type comparison (a `String` binding's slot type is not always the
    /// canonical vec-struct type even though its layout is). No-op when
    /// `name` has no live slot or no buffer cleanup queued.
    pub(super) fn suppress_vec_buffer_drop_for_var(&mut self, name: &str) {
        let Some(slot) = self.variables.get(name) else {
            return;
        };
        let target = slot.ptr;
        for frame in self.scope_cleanup_actions.iter_mut().rev() {
            frame.retain(|action| {
                !matches!(
                    action,
                    CleanupAction::FreeVecBuffer { vec_alloca, .. } if *vec_alloca == target
                )
            });
        }
    }

    /// Emit all cleanup actions registered across all scope frames (for function exit).
    /// Iterates frames in reverse (innermost first) and within each frame in reverse
    /// push order (LIFO). LIFO is mandatory for user `defer` per design.md § Drop
    /// ordering within a branch ("last declared, first drained"); compiler-internal
    /// cleanup variants (RcDec, FreeVecBuffer, FreeMapHandle, EnumDrop, StructDrop,
    /// RcDecOption) each touch independent allocations and commute, so reversing
    /// their order is a no-op for correctness.
    ///
    /// **Normal-exit path.** `UserErrDefer` actions are skipped here — they
    /// fire only on error-exit paths (`?`-propagation, explicit `return
    /// Err(...)` / `return None`). Error-exit dispatch goes through
    /// `emit_scope_cleanup_for_error_path` instead, which runs errdefers
    /// in phase 1 before reaching this same drop+defer drain in phase 2.
    pub(super) fn emit_scope_cleanup(&mut self) {
        self.emit_scope_cleanup_from(0);
    }

    /// Free the reshaper's `dummy` sentinel as a single headerless node at
    /// the fn's scope exit — reload the ptr from its slot, null-guard,
    /// `free`, then null the slot (so any reload-based cleanup that also
    /// targets it no-ops instead of double-freeing). No-op unless `fn_key`
    /// is a recognized headerless reshaper (`headerless_reshaper_dummies`).
    /// Sound: the dummy is uniquely owned and NOT part of the returned
    /// chain (`dummy.<link>` was already loaded into the return value
    /// before this runs), so the free is disjoint from the caller's
    /// free-walk. Called AFTER `emit_scope_cleanup`, so the null-out also
    /// neutralizes a stale reload the ordinary cleanup may have left.
    pub(super) fn emit_headerless_reshaper_dummy_free(&mut self, fn_key: &str) {
        let Some(dummy) = self.headerless_reshaper_dummies.get(fn_key).cloned() else {
            return;
        };
        let Some(slot) = self.variables.get(&dummy).map(|s| s.ptr) else {
            return;
        };
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let cur = self
            .builder
            .build_load(ptr_ty, slot, &format!("{dummy}_reshaper_dummy"))
            .unwrap()
            .into_pointer_value();
        let null = ptr_ty.const_null();
        let is_null = self
            .builder
            .build_int_compare(IntPredicate::EQ, cur, null, "reshaper_dummy_is_null")
            .unwrap();
        let skip_bb = self
            .context
            .append_basic_block(fn_val, "reshaper_dummy_free_skip");
        let do_bb = self
            .context
            .append_basic_block(fn_val, "reshaper_dummy_free_do");
        let join_bb = self
            .context
            .append_basic_block(fn_val, "reshaper_dummy_free_join");
        self.builder
            .build_conditional_branch(is_null, skip_bb, do_bb)
            .unwrap();
        self.builder.position_at_end(do_bb);
        self.builder
            .build_call(self.free_fn, &[cur.into()], "")
            .unwrap();
        self.builder.build_store(slot, null).unwrap();
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(skip_bb);
        self.builder.build_unconditional_branch(join_bb).unwrap();
        self.builder.position_at_end(join_bb);
    }

    /// Emit-only drain of cleanup frames `[start_frame..]`, innermost
    /// first — the compile-time stack is left untouched (no pop), so the
    /// textual fall-through path still drains its frames at their own
    /// scope boundaries. Two callers:
    ///
    /// - `emit_scope_cleanup` (start 0): function-exit / early-`return`
    ///   parity drain of every live frame.
    /// - `compile_break` / `compile_continue` (start =
    ///   `LoopFrame::cleanup_depth`): drain only the frames INSIDE the
    ///   loop / labeled block being exited — the per-iteration frame plus
    ///   any nested block / `if let` / match-arm frames between the jump
    ///   site and the loop boundary. Frames outside the loop stay live
    ///   and drain at their own boundaries. Every action goes through
    ///   `emit_cleanup_action_at`, inheriting the reload-by-name +
    ///   null-sentinel guards, so an action whose binding didn't execute
    ///   on this path no-ops at runtime.
    ///
    /// `UserErrDefer` is skipped — `break`/`continue`/`return` are normal
    /// exits; errdefers only run on the error path
    /// (`emit_scope_cleanup_for_error_path`).
    pub(super) fn emit_scope_cleanup_from(&mut self, start_frame: usize) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        for frame_idx in (start_frame..self.scope_cleanup_actions.len()).rev() {
            let n = self.scope_cleanup_actions[frame_idx].len();
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    continue;
                }
                self.emit_cleanup_action_at(frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
            }
        }
    }

    /// Emit the full Kāra-level cleanup for a coroutine **destroy/cancel edge**
    /// (A2 slice 4 heap drops + slice 5c-4 defer-on-cancel —
    /// `docs/spikes/network-async-coroutine-transform.md` § 7). Called from
    /// `emit_coro_park_suspend`'s per-park destroy block, where the live
    /// `scope_cleanup_actions` stack is exactly the set of locals + `defer` /
    /// `errdefer` blocks live across that suspend — so a coroutine destroyed
    /// *while parked here* frees exactly the heap a mid-flight cancel would
    /// otherwise leak (Vec read buffers, String, Map/file handles, RC-fallback
    /// boxes, struct/enum drops, user `Drop` impls) **and** runs the user
    /// `defer` / `errdefer` blocks the cancel would otherwise swallow.
    ///
    /// **Cancel is an error-path exit.** This routes through the same
    /// [`Self::emit_scope_cleanup_for_error_path`] the `par {}` cooperative-
    /// cancel path uses (`emit_branch_cancel_check`, `par_blocks.rs`) and that
    /// the interpreter's `ExitPath::Cancelled` mirrors: errdefers drain in
    /// phase 1 (LIFO across frames), then drops + defers in phase 2. That
    /// satisfies design.md § *Panic During Suspend* rule 1 ("the task's `defer`
    /// blocks, `errdefer` blocks, and RC-counted drops execute in standard
    /// reverse construction order") and keeps coroutine cancellation behaviour
    /// identical to `par`-branch cancellation. As with `par`, the binding form
    /// `errdefer(e) { ... }` has no materialized `e = Cancelled` payload at a
    /// cancel exit (no `Err` value is constructed — cancel is a flag); that is
    /// the same cross-cutting design gap `par` carries, not coroutine-specific.
    ///
    /// **Recursion suppression.** A user `defer` / `errdefer` body may contain
    /// an effectful call (`defer { println(..); }`). When this coroutine is
    /// itself compiled inside a `par {}` branch, `branch_cancel_ptr` is set, so
    /// that call's `compile_call` → `emit_branch_cancel_check` re-entry would
    /// walk `scope_cleanup_actions` again and re-encounter the SAME actions
    /// (still in their frames), recursing forever at compile time. Save + null +
    /// restore `branch_cancel_ptr` across the drain — exactly as the `par`
    /// cancel-exit does — so nested cancel-checks inside cleanup bodies no-op.
    ///
    /// The frame is **not** freed here — the shared `cleanup_bb` (`coro.free`)
    /// the destroy block branches into does that; this only runs the Kāra-level
    /// cleanup. Each action goes through the same `emit_cleanup_action_at` the
    /// normal path uses, inheriting null-guards / conditional-init handling
    /// (e.g. `RcDec`'s null-sentinel skip). The completion-path cleanup and
    /// these destroy-edge actions are on mutually exclusive control-flow paths
    /// (a coroutine either runs to completion — body-end `emit_scope_cleanup`,
    /// then parks at the final suspend whose destroy edge is free-only — or is
    /// destroyed at a park, reaching this drain), so nothing runs twice.
    pub(super) fn emit_coro_destroy_edge_cleanup(&mut self) {
        let saved_cancel_ptr = self.branch_cancel_ptr.take();
        self.emit_scope_cleanup_for_error_path();
        self.branch_cancel_ptr = saved_cancel_ptr;
    }

    /// Error-exit drain. Per design.md § *Drop ordering within a branch*,
    /// when control exits a scope via an error path (the `?` operator's
    /// Err-propagation branch, an explicit `return Err(...)` or `return
    /// None`), the unified cleanup stack drains in two phases:
    ///
    /// 1. **Phase 1: errdefers.** Every `UserErrDefer` action runs first,
    ///    in reverse declaration order (LIFO), per frame innermost-first.
    /// 2. **Phase 2: drops + defers.** Every other cleanup variant (the
    ///    compiler-internal drops + `UserDefer`) drains in the same
    ///    program-order LIFO `emit_scope_cleanup` uses on normal exit.
    ///
    /// Per-frame interleave (phase 1 then phase 2 within each frame,
    /// innermost frame first) mirrors the interpreter's `run_cleanup`
    /// shape (`src/interpreter/eval_stmt.rs:364-408`): each scope drains
    /// its own errdefers before its own drops, and outer scopes drain in
    /// turn when the error bubbles out. The action stack still excludes
    /// the binding form `errdefer(e) { ... }` per slice 2 — slice 4 will
    /// lift the gate in `compile_stmt` and add the bind-payload step here.
    pub(super) fn emit_scope_cleanup_for_error_path(&mut self) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        for frame_idx in (0..self.scope_cleanup_actions.len()).rev() {
            let n = self.scope_cleanup_actions[frame_idx].len();
            // Phase 1: errdefers LIFO within this frame.
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    self.emit_cleanup_action_at(
                        frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t,
                    );
                }
            }
            // Phase 2: non-errdefer actions LIFO within this frame.
            for action_idx in (0..n).rev() {
                if matches!(
                    &self.scope_cleanup_actions[frame_idx][action_idx],
                    CleanupAction::UserErrDefer { .. }
                ) {
                    continue;
                }
                self.emit_cleanup_action_at(frame_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
            }
        }
    }

    /// Drain the topmost `scope_cleanup_actions` frame: emit cleanup IR for
    /// every action it holds (in reverse push order — LIFO), then pop the
    /// frame. Used by `compile_match` to fire match-arm-scoped cleanups
    /// (let-bindings inside the arm body, plus the match-arm pattern binding
    /// itself) at end-of-arm instead of end-of-function — without this the
    /// alloca reuse across match-arm iterations leaks all but the last bound
    /// value.
    ///
    /// Caller is responsible for ensuring the basic-block insertion point is
    /// somewhere meaningful (i.e. the arm-body's end before the merge branch).
    /// No-op if the cleanup stack is empty.
    ///
    /// **Normal-exit semantics.** `UserErrDefer` actions in the frame are
    /// skipped — this is a normal-fall-through drain, the error-path drain
    /// goes through `emit_scope_cleanup_for_error_path` instead. The skipped
    /// errdefers are dropped along with the frame on pop, so a block that
    /// registers an `errdefer` but exits normally never fires it.
    pub(super) fn drain_top_frame_with_emit(&mut self) {
        if self.scope_cleanup_actions.is_empty() {
            return;
        }
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();
        let top_idx = self.scope_cleanup_actions.len() - 1;
        let n = self.scope_cleanup_actions[top_idx].len();
        for action_idx in (0..n).rev() {
            if matches!(
                &self.scope_cleanup_actions[top_idx][action_idx],
                CleanupAction::UserErrDefer { .. }
            ) {
                continue;
            }
            self.emit_cleanup_action_at(top_idx, action_idx, fn_val, vec_ty, ptr_ty, i64_t);
        }
        self.scope_cleanup_actions.pop();
    }

    /// Dispatch one cleanup action by `(frame_idx, action_idx)` indices into
    /// `scope_cleanup_actions`. Uses indices rather than a borrowed reference
    /// so user-defer dispatch (`UserDefer(Block)` / `UserErrDefer { .. }`)
    /// can release the borrow, clone the body, and then call `compile_block`
    /// under `&mut self`. Compiler-internal variants take the existing
    /// `&self` `emit_cleanup_action` fast path.
    fn emit_cleanup_action_at(
        &mut self,
        frame_idx: usize,
        action_idx: usize,
        fn_val: FunctionValue<'ctx>,
        vec_ty: StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
    ) {
        // Slice 4 (Phase 7 § *defer / errdefer codegen*): split the
        // body extraction so the errdefer binding name can be threaded
        // through to the bind-then-emit dispatch below. `UserDefer` has
        // no binding; `UserErrDefer` carries `Option<String>` — `None`
        // is slice 2's no-binding form (no extra setup), `Some(name)`
        // is slice 4's binding form (allocate, store staged payload,
        // register in `variables`, emit, restore).
        let body_and_binding = match &self.scope_cleanup_actions[frame_idx][action_idx] {
            CleanupAction::UserDefer(block) => Some((block.clone(), None)),
            CleanupAction::UserErrDefer { binding, body } => Some((body.clone(), binding.clone())),
            _ => None,
        };
        if let Some((block, binding)) = body_and_binding {
            // Slice 4: bind the staged Err payload into the body's
            // scope when this is a binding-form errdefer. The payload
            // was staged into `self.pending_errdefer_payload` by the
            // error-exit site (`compile_question`'s `fail_bb`,
            // `ExprKind::Return(Err(...))`, or `compile_function`'s
            // tail `Err(...)` emitter) immediately before
            // `emit_scope_cleanup_for_error_path` ran. Allocate an
            // entry-block alloca of the payload's LLVM type, store
            // the staged value, save the prior `variables[name]` (if
            // any) for restoration after the body emits, then insert
            // the new slot so the body's compile_expr reads of `e`
            // resolve to a fresh load of the bound payload.
            //
            // When the binding is present but no payload is staged
            // (`pending_errdefer_payload` is `None`), the body still
            // emits — without the binding — so an `errdefer(e)` that
            // never sees a runtime error path stays consistent with
            // the no-binding form's drain semantics. In practice all
            // three error-exit sites stage before calling the
            // error-path drain, so the unstaged case is unreachable
            // from a well-formed program; the conservative branch
            // here keeps emission non-fatal.
            let saved_binding: Option<(String, Option<VarSlot<'ctx>>)> =
                if let Some(name) = &binding {
                    if let Some(payload) = self.pending_errdefer_payload {
                        let payload_ty = payload.get_type();
                        let alloca = self.create_entry_alloca(fn_val, name, payload_ty);
                        self.builder.build_store(alloca, payload).unwrap();
                        let prior = self.variables.get(name).copied();
                        self.variables.insert(
                            name.clone(),
                            VarSlot {
                                ptr: alloca,
                                ty: payload_ty,
                            },
                        );
                        Some((name.clone(), prior))
                    } else {
                        None
                    }
                } else {
                    None
                };
            // Slice 1.5: route the defer body through the frame-pushing
            // variant so a nested `defer` inside this body scopes to the
            // defer body itself (drains at end-of-defer-body) instead of
            // bubbling up to the enclosing scope's frame. Also gives the
            // defer body the same runtime-reachability shape as a naked
            // block: a `defer` inside an `if false { ... }` nested in
            // here never fires. The errdefer body (slice 2) reuses this
            // same path so a `defer` inside an errdefer body scopes the
            // same way.
            let _ = self.compile_block_with_frame(&block);
            // Restore any prior binding the errdefer's `e` shadowed.
            // Removing the slot rather than leaving it in `variables`
            // is required: the alloca is live only for the duration of
            // this body's compile, and a subsequent unrelated reference
            // to the same name (in a later errdefer body or the same
            // body re-entered) must not pick up a stale slot.
            if let Some((name, prior)) = saved_binding {
                match prior {
                    Some(slot) => {
                        self.variables.insert(name, slot);
                    }
                    None => {
                        self.variables.remove(&name);
                    }
                }
            }
            return;
        }
        let action_ref = &self.scope_cleanup_actions[frame_idx][action_idx];
        self.record_drop_obs(action_ref, fn_val);
        self.emit_cleanup_action(action_ref, fn_val, vec_ty, ptr_ty, i64_t);
    }

    /// Read-only drop-observability tap (ownership-model-mechanization Slice 4
    /// down-payment — see `src/codegen/drop_obs.rs`). Records the `(function,
    /// place)` of each *compiler-internal* heap drop this funnel emits, so the
    /// ownership oracle's drop schedule can be diffed against real lowering.
    /// A hard no-op on the production path — `drop_obs::armed()` is only ever
    /// `true` inside the differential harness, so neither the place-name
    /// extraction nor the record runs during normal `karac` / test codegen.
    ///
    /// `place` is the binding name: every alloca-carrying variant's slot is
    /// named after its binding by `create_entry_alloca`, so `get_name` recovers
    /// it; name-carrying variants (`RcDec`, `FreeSharedElided`, …) supply it
    /// directly. User `defer` / `errdefer` (drained here too) and the mutex
    /// release carry no droppable place and are skipped. Codegen-internal
    /// temporaries surface with their synthetic slot name (often empty); the
    /// differential filters to the oracle's known place set, so they are not
    /// counted as divergences.
    fn record_drop_obs(&self, action: &CleanupAction<'ctx>, fn_val: FunctionValue<'ctx>) {
        if !crate::codegen::drop_obs::armed() {
            return;
        }
        // Recover the *source binding name* for an alloca-keyed action. The
        // slot is usually named after the binding (`create_entry_alloca`), but
        // some binding kinds (Map/Set handles, pattern temporaries) allocate a
        // generically-named slot, so `get_name` alone would misattribute the
        // drop. Reverse-map through `variables` (name → slot) first — that is
        // the authoritative source binding name — and fall back to the alloca
        // name only when the slot is not a live named binding.
        let name_of = |p: PointerValue<'ctx>| -> String {
            if let Some((n, _)) = self.variables.iter().find(|(_, vs)| vs.ptr == p) {
                return n.clone();
            }
            p.get_name().to_str().unwrap_or("").to_string()
        };
        let place: Option<String> = match action {
            CleanupAction::FreeVecBuffer { vec_alloca, .. } => Some(name_of(*vec_alloca)),
            CleanupAction::StructDrop { struct_alloca, .. } => Some(name_of(*struct_alloca)),
            CleanupAction::EnumDrop { enum_alloca, .. } => Some(name_of(*enum_alloca)),
            CleanupAction::FreeMapHandle { map_alloca, .. } => Some(name_of(*map_alloca)),
            CleanupAction::FreeTensor { tensor_alloca } => Some(name_of(*tensor_alloca)),
            CleanupAction::FreeColumn { column_alloca, .. } => Some(name_of(*column_alloca)),
            CleanupAction::FreeDataFrame { df_alloca } => Some(name_of(*df_alloca)),
            CleanupAction::FreeSoaGroups { soa_alloca, .. } => Some(name_of(*soa_alloca)),
            CleanupAction::FreeFileHandle { file_alloca } => Some(name_of(*file_alloca)),
            CleanupAction::ReleaseLazyExpr { alloca }
            | CleanupAction::ReleaseLazyPlan { alloca }
            | CleanupAction::ReleaseLazyGroupBy { alloca } => Some(name_of(*alloca)),
            CleanupAction::FreeGpuBuffer { buf_alloca } => Some(name_of(*buf_alloca)),
            CleanupAction::FreeOnceHandle { once_alloca, .. } => Some(name_of(*once_alloca)),
            CleanupAction::FreeInternerHandle { interner_alloca } => {
                Some(name_of(*interner_alloca))
            }
            CleanupAction::FreeArenaHandle { arena_alloca } => Some(name_of(*arena_alloca)),
            CleanupAction::FreeClosureEnv { fat_alloca } => Some(name_of(*fat_alloca)),
            CleanupAction::DropChannelEnd { chan_alloca, .. } => Some(name_of(*chan_alloca)),
            CleanupAction::FreeInlineOptionPayload { option_slot, .. } => {
                Some(name_of(*option_slot))
            }
            CleanupAction::FreeInlineResultPayload { result_slot, .. } => {
                Some(name_of(*result_slot))
            }
            CleanupAction::FreeInlineOptionMapPayload { option_slot, .. } => {
                Some(name_of(*option_slot))
            }
            CleanupAction::RcDec { name, .. }
            | CleanupAction::RcDecOption { name, .. }
            | CleanupAction::BoxedEnumDrop { name, .. }
            | CleanupAction::FreeSharedElided { name, .. }
            | CleanupAction::FreeClusterWalk { name, .. }
            | CleanupAction::FreeClusterWalkOption { name, .. } => Some(name.clone()),
            CleanupAction::UserDrop { binding_name, .. } => Some(binding_name.clone()),
            CleanupAction::UserDefer(_)
            | CleanupAction::UserErrDefer { .. }
            | CleanupAction::ReleaseMutex { .. } => None,
        };
        if let Some(place) = place {
            let fn_name = fn_val.get_name().to_str().unwrap_or("");
            crate::codegen::drop_obs::record(fn_name, "heap", &place);
        }
    }

    /// Per-action cleanup IR emitter. Extracted from `emit_scope_cleanup` so
    /// the same code path serves both whole-stack drain (function-end /
    /// early-return cleanup) and top-frame drain (per-match-arm cleanup at
    /// `drain_top_frame_with_emit`). Signature takes pre-computed type
    /// handles so the caller hoists them out of inner loops.
    pub(super) fn emit_cleanup_action(
        &self,
        action: &CleanupAction<'ctx>,
        fn_val: FunctionValue<'ctx>,
        vec_ty: StructType<'ctx>,
        ptr_ty: inkwell::types::PointerType<'ctx>,
        i64_t: inkwell::types::IntType<'ctx>,
    ) {
        match action {
            CleanupAction::FreeClusterWalk {
                name,
                ptr,
                member_type,
                link_field_index,
            } => {
                // Pointer-type gate mirrors RcDec (B-2026-07-12-6): a
                // same-named non-pointer shadow in an inner scope must not
                // redirect this reload to a garbage slot; fall back to the
                // registration-time pointer when the current slot isn't the
                // binding's own pointer slot.
                let current_ptr = match self.variables.get(name) {
                    Some(slot) if slot.ty.is_pointer_type() => self
                        .builder
                        .build_load(ptr_ty, slot.ptr, &format!("{}_cluster_cleanup", name))
                        .unwrap()
                        .into_pointer_value(),
                    _ => *ptr,
                };
                let heap_type = self
                    .shared_types
                    .get(member_type)
                    .map(|i| i.heap_type)
                    .expect("cluster member type registered in shared_types");
                let niche = self
                    .niche_field_inner_heap_type(member_type, *link_field_index)
                    .is_some();
                if !niche {
                    // Defensive fallback: without the niche single-ptr
                    // link slot, emit the standard dec instead (same
                    // shape as the RcDec arm) — behavior-preserving.
                    let null = ptr_ty.const_null();
                    let is_null = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, current_ptr, null, "cw_fb_null")
                        .unwrap();
                    let skip_bb = self.context.append_basic_block(fn_val, "cw_fb_skip");
                    let do_bb = self.context.append_basic_block(fn_val, "cw_fb_do");
                    let join_bb = self.context.append_basic_block(fn_val, "cw_fb_join");
                    self.builder
                        .build_conditional_branch(is_null, skip_bb, do_bb)
                        .unwrap();
                    self.builder.position_at_end(do_bb);
                    self.emit_refcount_dec(name, heap_type, current_ptr);
                    self.builder.build_unconditional_branch(join_bb).unwrap();
                    self.builder.position_at_end(skip_bb);
                    self.builder.build_unconditional_branch(join_bb).unwrap();
                    self.builder.position_at_end(join_bb);
                    return;
                }
                // The free-walk:
                //   cur = root; while cur != null { n = cur-><link>;
                //   free(cur); cur = n; }
                // Phase-D layout: a headerless member's link slot GEPs
                // the twin at the un-shifted user index (the fallback
                // above is unreachable headerless — `headerless_here`
                // requires the niche link). `free` is layout-agnostic.
                let (gep_ty, base) = self.shared_gep_layout(member_type, heap_type);
                let link_heap_idx = *link_field_index as u32 + base;
                let entry_bb = self.builder.get_insert_block().unwrap();
                let loop_bb = self.context.append_basic_block(fn_val, "cw_loop");
                let body_bb = self.context.append_basic_block(fn_val, "cw_body");
                let done_bb = self.context.append_basic_block(fn_val, "cw_done");
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let phi = self.builder.build_phi(ptr_ty, "cw_cur").unwrap();
                phi.add_incoming(&[(&current_ptr, entry_bb)]);
                let cur = phi.as_basic_value().into_pointer_value();
                let is_null = self.builder.build_is_null(cur, "cw_is_null").unwrap();
                self.builder
                    .build_conditional_branch(is_null, done_bb, body_bb)
                    .unwrap();
                self.builder.position_at_end(body_bb);
                let link_ptr = self
                    .builder
                    .build_struct_gep(gep_ty, cur, link_heap_idx, "cw_link")
                    .unwrap();
                let next = self
                    .builder
                    .build_load(ptr_ty, link_ptr, "cw_next")
                    .unwrap()
                    .into_pointer_value();
                self.builder
                    .build_call(self.free_fn, &[cur.into()], "")
                    .unwrap();
                let body_end = self.builder.get_insert_block().unwrap();
                phi.add_incoming(&[(&next, body_end)]);
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
            CleanupAction::FreeClusterWalkOption {
                name,
                option_slot,
                option_ty,
                member_type,
                link_field_index,
                some_tag,
            } => {
                // Tag guard (mirror RcDecOption — w0 is garbage under
                // None), then the FreeClusterWalk loop from the
                // recovered inner pointer.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(
                        *option_ty,
                        *option_slot,
                        0,
                        &format!("{}_acw_tag_ptr", name),
                    )
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, &format!("{}_acw_tag", name))
                    .unwrap()
                    .into_int_value();
                let some_tag_const = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        some_tag_const,
                        &format!("{}_acw_is_some", name),
                    )
                    .unwrap();
                let do_bb = self.context.append_basic_block(fn_val, "acw_do");
                let join_bb = self.context.append_basic_block(fn_val, "acw_join");
                self.builder
                    .build_conditional_branch(is_some, do_bb, join_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                let w0_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 1, &format!("{}_acw_w0_ptr", name))
                    .unwrap();
                let w0 = self
                    .builder
                    .build_load(i64_t, w0_ptr, &format!("{}_acw_w0", name))
                    .unwrap()
                    .into_int_value();
                let head = self
                    .builder
                    .build_int_to_ptr(w0, ptr_ty, &format!("{}_acw_head", name))
                    .unwrap();
                let heap_type = self
                    .shared_types
                    .get(member_type)
                    .map(|i| i.heap_type)
                    .expect("adopted member type registered in shared_types");
                let niche = self
                    .niche_field_inner_heap_type(member_type, *link_field_index)
                    .is_some();
                if !niche {
                    // Defensive fallback: degrade to the RcDecOption
                    // shape (null-guarded dec of the head) — behavior-
                    // preserving; unreachable for today's all-niched
                    // `Option[shared Self]` links.
                    let null = ptr_ty.const_null();
                    let head_is_null = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, head, null, "acw_fb_null")
                        .unwrap();
                    let fb_do = self.context.append_basic_block(fn_val, "acw_fb_do");
                    let fb_skip = self.context.append_basic_block(fn_val, "acw_fb_skip");
                    self.builder
                        .build_conditional_branch(head_is_null, fb_skip, fb_do)
                        .unwrap();
                    self.builder.position_at_end(fb_do);
                    self.emit_refcount_dec(name, heap_type, head);
                    self.builder.build_unconditional_branch(fb_skip).unwrap();
                    self.builder.position_at_end(fb_skip);
                    self.builder.build_unconditional_branch(join_bb).unwrap();
                    self.builder.position_at_end(join_bb);
                    return;
                }
                // Adopted chains are always headered (never phase-D):
                // the layout helper still routes correctly because
                // `headerless_here` can't hold for a type that crosses
                // the builder's signature.
                let (gep_ty, base) = self.shared_gep_layout(member_type, heap_type);
                let link_heap_idx = *link_field_index as u32 + base;
                let entry_bb = self.builder.get_insert_block().unwrap();
                let loop_bb = self.context.append_basic_block(fn_val, "acw_loop");
                let body_bb = self.context.append_basic_block(fn_val, "acw_body");
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(loop_bb);
                let phi = self.builder.build_phi(ptr_ty, "acw_cur").unwrap();
                phi.add_incoming(&[(&head, entry_bb)]);
                let cur = phi.as_basic_value().into_pointer_value();
                let is_null = self.builder.build_is_null(cur, "acw_is_null").unwrap();
                self.builder
                    .build_conditional_branch(is_null, join_bb, body_bb)
                    .unwrap();
                self.builder.position_at_end(body_bb);
                let link_ptr = self
                    .builder
                    .build_struct_gep(gep_ty, cur, link_heap_idx, "acw_link")
                    .unwrap();
                let next = self
                    .builder
                    .build_load(ptr_ty, link_ptr, "acw_next")
                    .unwrap()
                    .into_pointer_value();
                self.builder
                    .build_call(self.free_fn, &[cur.into()], "")
                    .unwrap();
                let body_end = self.builder.get_insert_block().unwrap();
                phi.add_incoming(&[(&next, body_end)]);
                self.builder.build_unconditional_branch(loop_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::FreeSharedElided { name, ptr } => {
                // Mirror RcDec's reload + null-guard, then free directly:
                // the elision analysis proved rc can never exceed 1 and
                // the type holds no heap fields, so the whole
                // dec/zero-test/drop-fn dance collapses to `free`.
                // Pointer-type gate mirrors RcDec (B-2026-07-12-6): a
                // same-named non-pointer shadow in an inner scope must not
                // redirect this reload to a garbage slot; fall back to the
                // registration-time pointer when the current slot isn't the
                // binding's own pointer slot.
                let current_ptr = match self.variables.get(name) {
                    Some(slot) if slot.ty.is_pointer_type() => self
                        .builder
                        .build_load(ptr_ty, slot.ptr, &format!("{}_elide_cleanup", name))
                        .unwrap()
                        .into_pointer_value(),
                    _ => *ptr,
                };
                let null = ptr_ty.const_null();
                let is_null = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, current_ptr, null, "elide_is_null")
                    .unwrap();
                let skip_bb = self.context.append_basic_block(fn_val, "elide_free_skip");
                let do_bb = self.context.append_basic_block(fn_val, "elide_free_do");
                let join_bb = self.context.append_basic_block(fn_val, "elide_free_join");
                self.builder
                    .build_conditional_branch(is_null, skip_bb, do_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                self.builder
                    .build_call(self.free_fn, &[current_ptr.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::RcDec {
                name,
                ptr,
                heap_type,
            } => {
                // Reload the current pointer from the binding's slot so a
                // reassignment (`e = other_shared`) drops the live value —
                // BUT only when the slot is still the shared binding's own
                // pointer slot. A *shadowing* local of a different type in an
                // inner scope (`let mut e = 0` inside a `match e { … }` arm,
                // where `e` is a shared param) repoints `variables[name]` at an
                // unrelated non-pointer slot; loading a `ptr` from an `i64`
                // shadow reinterprets an integer as a heap pointer and the RC
                // dec walks a garbage address (B-2026-07-12-6 frame
                // corruption). A genuine shared binding — and any reassignment
                // of it — is always pointer-typed, so gate the reload on the
                // slot type and otherwise fall back to the pointer captured at
                // registration (the original binding's value, which for a
                // never-reassigned param is exactly the incoming object).
                let current_ptr = match self.variables.get(name) {
                    Some(slot) if slot.ty.is_pointer_type() => self
                        .builder
                        .build_load(ptr_ty, slot.ptr, &format!("{}_rc_cleanup", name))
                        .unwrap()
                        .into_pointer_value(),
                    _ => *ptr,
                };
                // Null-guard the dec: body-local shared-struct slots
                // whose let-binding never executed (the enclosing loop
                // body or conditional branch was skipped) carry a
                // null sentinel — `track_rc_var` emits a `store null`
                // at function entry. Without the guard, the dec
                // dereferences null (or stale memory) and hangs in
                // macOS malloc's bookkeeping pages. Skip when null;
                // otherwise dispatch through `emit_refcount_dec` as
                // before.
                let null = ptr_ty.const_null();
                let is_null = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, current_ptr, null, "rc_is_null")
                    .unwrap();
                let skip_bb = self.context.append_basic_block(fn_val, "rc_cleanup_skip");
                let do_bb = self.context.append_basic_block(fn_val, "rc_cleanup_do");
                let join_bb = self.context.append_basic_block(fn_val, "rc_cleanup_join");
                self.builder
                    .build_conditional_branch(is_null, skip_bb, do_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                self.emit_refcount_dec(name, *heap_type, current_ptr);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::FreeTensor { tensor_alloca } => {
                // Tensor binding: the slot holds one pointer to the
                // `[rank][dims][data]` block (`src/codegen/tensor.rs`).
                // Null = moved-out (the move-suppression sentinel, the
                // Tensor analog of Vec's `cap = 0`); skip the free.
                let t_ptr = self
                    .builder
                    .build_load(ptr_ty, *tensor_alloca, "cleanup.t")
                    .unwrap()
                    .into_pointer_value();
                let null = ptr_ty.const_null();
                let live = self
                    .builder
                    .build_int_compare(IntPredicate::NE, t_ptr, null, "cleanup.t.live")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "cleanup.t.free");
                let skip_bb = self.context.append_basic_block(fn_val, "cleanup.t.skip");
                self.builder
                    .build_conditional_branch(live, free_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                self.builder
                    .build_call(self.free_fn, &[t_ptr.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeColumn {
                column_alloca,
                string_elem,
            } => {
                // Column binding: the slot holds one pointer to the
                // `{ data, null_bitmap, len, capacity }` control block
                // (`src/codegen/column.rs`). Null = moved-out (the
                // move-suppression sentinel); skip the frees. Otherwise
                // free the two separate Arrow buffers (`data`,
                // `null_bitmap`) and then the control block — three
                // `free`s.
                let ctrl = self
                    .builder
                    .build_load(ptr_ty, *column_alloca, "cleanup.col")
                    .unwrap()
                    .into_pointer_value();
                let null = ptr_ty.const_null();
                let live = self
                    .builder
                    .build_int_compare(IntPredicate::NE, ctrl, null, "cleanup.col.live")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "cleanup.col.free");
                let skip_bb = self.context.append_basic_block(fn_val, "cleanup.col.skip");
                self.builder
                    .build_conditional_branch(live, free_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let st = self.column_control_struct_type();
                let data = self
                    .builder
                    .build_load(
                        ptr_ty,
                        self.builder
                            .build_struct_gep(st, ctrl, 0, "cleanup.col.data.p")
                            .unwrap(),
                        "cleanup.col.data",
                    )
                    .unwrap()
                    .into_pointer_value();
                let bitmap = self
                    .builder
                    .build_load(
                        ptr_ty,
                        self.builder
                            .build_struct_gep(st, ctrl, 1, "cleanup.col.bm.p")
                            .unwrap(),
                        "cleanup.col.bm",
                    )
                    .unwrap()
                    .into_pointer_value();
                // `Column[String]`: each valid slot owns a heap String —
                // free it (cap-guarded via the canonical String drop fn)
                // before the data buffer. Null slots hold a never-read
                // placeholder (no owned heap), so only valid slots are freed.
                if *string_elem {
                    let len = self
                        .builder
                        .build_load(
                            i64_t,
                            self.builder
                                .build_struct_gep(st, ctrl, 2, "cleanup.col.len.p")
                                .unwrap(),
                            "cleanup.col.len",
                        )
                        .unwrap()
                        .into_int_value();
                    let str_st = self.vec_struct_type();
                    // Pre-emitted in `track_column_var` (the `&self` drain
                    // can't emit); fetch it from the module immutably.
                    let drop_fn = self
                        .module
                        .get_function("karac_drop_String")
                        .expect("karac_drop_String pre-emitted by track_column_var");
                    let i_slot = self.builder.build_alloca(i64_t, "cleanup.col.s.i").unwrap();
                    self.builder
                        .build_store(i_slot, i64_t.const_zero())
                        .unwrap();
                    let head = self
                        .context
                        .append_basic_block(fn_val, "cleanup.col.s.head");
                    let body = self
                        .context
                        .append_basic_block(fn_val, "cleanup.col.s.body");
                    let free1 = self
                        .context
                        .append_basic_block(fn_val, "cleanup.col.s.free");
                    let cont = self
                        .context
                        .append_basic_block(fn_val, "cleanup.col.s.cont");
                    let done = self
                        .context
                        .append_basic_block(fn_val, "cleanup.col.s.done");
                    self.builder.build_unconditional_branch(head).unwrap();
                    self.builder.position_at_end(head);
                    let i = self
                        .builder
                        .build_load(i64_t, i_slot, "cleanup.col.s.iv")
                        .unwrap()
                        .into_int_value();
                    let more = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, i, len, "cleanup.col.s.more")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(more, body, done)
                        .unwrap();
                    self.builder.position_at_end(body);
                    let valid = self.column_load_valid_bit(bitmap, i);
                    self.builder
                        .build_conditional_branch(valid, free1, cont)
                        .unwrap();
                    self.builder.position_at_end(free1);
                    let slot = unsafe {
                        self.builder
                            .build_gep(str_st, data, &[i], "cleanup.col.s.slot")
                            .unwrap()
                    };
                    self.builder
                        .build_call(drop_fn, &[slot.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(cont).unwrap();
                    self.builder.position_at_end(cont);
                    self.builder
                        .build_store(
                            i_slot,
                            self.builder
                                .build_int_add(i, i64_t.const_int(1, false), "cleanup.col.s.next")
                                .unwrap(),
                        )
                        .unwrap();
                    self.builder.build_unconditional_branch(head).unwrap();
                    self.builder.position_at_end(done);
                }
                self.builder
                    .build_call(self.free_fn, &[data.into()], "")
                    .unwrap();
                self.builder
                    .build_call(self.free_fn, &[bitmap.into()], "")
                    .unwrap();
                self.builder
                    .build_call(self.free_fn, &[ctrl.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeDataFrame { df_alloca } => {
                self.emit_dataframe_free(fn_val, *df_alloca);
            }
            CleanupAction::FreeVecBuffer {
                vec_alloca,
                elem_ty,
                elem_is_tensor,
                elem_map_drop,
                elem_agg_drop,
            } => {
                let cap_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, *vec_alloca, 2, "cleanup.cap.ptr")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "cleanup.cap")
                    .unwrap()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                // SSO forward-prep (see `sso.rs`): owned-heap ⇔ signed
                // `cap > 0`; inline/static skip the free. No-op for `Vec`
                // (cap is a non-negative element count).
                let is_heap = self.sso_string_is_owned_heap(cap);
                let free_bb = self.context.append_basic_block(fn_val, "cleanup.free");
                let skip_bb = self.context.append_basic_block(fn_val, "cleanup.skip");
                self.builder
                    .build_conditional_branch(is_heap, free_bb, skip_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                let data_ptr_ptr = self
                    .builder
                    .build_struct_gep(vec_ty, *vec_alloca, 0, "cleanup.data.ptr")
                    .unwrap();
                let data = self
                    .builder
                    .build_load(ptr_ty, data_ptr_ptr, "cleanup.data")
                    .unwrap()
                    .into_pointer_value();

                // Recursive-drop fast path: when the element type is
                // itself a Vec/String struct, each live element owns
                // a separate data buffer. Iterate `len` elements and
                // free each one's `data` pointer before releasing
                // the outer buffer; otherwise those inner buffers
                // leak. Closes the 2026-05-13 cumulative-retention
                // bug measured on LeetCode #3629 bfs_sieve, where
                // `Vec[Vec[i64]]` leaked ~32 MB per `min_jumps`
                // call. This inline path is ONE level deep — it frees
                // each element's own buffer but treats that buffer's
                // contents as opaque, so it is exact for a
                // `Vec[Vec[scalar]]` / `Vec[String]` element (nothing
                // deeper to free) but would leak the innermost heap of
                // a `Vec[Vec[String]]` / deeper. Slice 3n closes that:
                // when the element is a `Vec[heap-inner]`,
                // `vec_elem_agg_drop_for_type_expr` returns the
                // strictly-recursive `karac_drop_Vec_<inner>` and the
                // element takes the `agg_drop` branch below instead of
                // this fast path — so this inline path now only ever
                // sees exactly the one-level-correct shapes.
                if let Some(et) = elem_ty {
                    if let Some(agg_drop) = elem_agg_drop {
                        // Named user struct/enum elements: run each live
                        // element's own `__karac_drop_<T>`, which frees every
                        // heap-bearing field cap-guarded — Vec/String, Map/Set,
                        // AND enum payloads (the all-i64 enum words the inline
                        // paths below are blind to). Strictly more complete than
                        // the vec-struct / struct-field walks, so it SUPERSEDES
                        // them (this is the `if`, they are `else if`): running
                        // both would double-free the direct heap fields.
                        // Closes B-2026-06-12-6 cluster 2 gap 2 (`Vec[Span]`,
                        // `Span` holds a `Tok` enum). Guarded by the same
                        // `cap > 0` branch, so a moved-out Vec skips per-element
                        // drops too; every slot in `[0, len)` is a live element.
                        let agg_drop = *agg_drop;
                        let elem_struct = *et;
                        let len_ptr = self
                            .builder
                            .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.adrop.len.ptr")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, "cleanup.adrop.len")
                            .unwrap()
                            .into_int_value();
                        let counter =
                            self.create_entry_alloca(fn_val, "cleanup.adrop.i", i64_t.into());
                        self.builder.build_store(counter, zero).unwrap();
                        let acond_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.adrop.cond");
                        let abody_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.adrop.body");
                        let aafter_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.adrop.after");
                        self.builder.build_unconditional_branch(acond_bb).unwrap();
                        self.builder.position_at_end(acond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "cleanup.adrop.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.adrop.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, abody_bb, aafter_bb)
                            .unwrap();
                        self.builder.position_at_end(abody_bb);
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(elem_struct, data, &[cur], "cleanup.adrop.elem")
                                .unwrap()
                        };
                        self.builder
                            .build_call(agg_drop, &[elem_ptr.into()], "")
                            .unwrap();
                        let one = i64_t.const_int(1, false);
                        let next = self
                            .builder
                            .build_int_add(cur, one, "cleanup.adrop.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder.build_unconditional_branch(acond_bb).unwrap();
                        self.builder.position_at_end(aafter_bb);
                    } else if self.llvm_ty_is_vec_struct(*et) {
                        let len_ptr = self
                            .builder
                            .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.len.ptr")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, "cleanup.len")
                            .unwrap()
                            .into_int_value();
                        let counter =
                            self.create_entry_alloca(fn_val, "cleanup.drop.i", i64_t.into());
                        self.builder.build_store(counter, zero).unwrap();
                        let drop_cond_bb =
                            self.context.append_basic_block(fn_val, "cleanup.drop.cond");
                        let drop_body_bb =
                            self.context.append_basic_block(fn_val, "cleanup.drop.body");
                        let drop_after_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.after");
                        self.builder
                            .build_unconditional_branch(drop_cond_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_cond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "cleanup.drop.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.drop.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, drop_body_bb, drop_after_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_body_bb);
                        // Each element is a Vec struct `{ptr, len,
                        // cap}` at `data + i * sizeof(VecStruct)`.
                        // Check inner cap > 0, then free inner ptr.
                        let inner_struct_ptr = unsafe {
                            self.builder
                                .build_gep(
                                    self.vec_struct_type(),
                                    data,
                                    &[cur],
                                    "cleanup.drop.elem",
                                )
                                .unwrap()
                        };
                        let inner_cap_ptr = self
                            .builder
                            .build_struct_gep(
                                self.vec_struct_type(),
                                inner_struct_ptr,
                                2,
                                "cleanup.drop.inner.cap.ptr",
                            )
                            .unwrap();
                        let inner_cap = self
                            .builder
                            .build_load(i64_t, inner_cap_ptr, "cleanup.drop.inner.cap")
                            .unwrap()
                            .into_int_value();
                        let inner_is_heap = self
                            .builder
                            .build_int_compare(
                                IntPredicate::UGT,
                                inner_cap,
                                zero,
                                "cleanup.drop.inner.is_heap",
                            )
                            .unwrap();
                        let inner_free_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.inner.free");
                        let inner_skip_bb = self
                            .context
                            .append_basic_block(fn_val, "cleanup.drop.inner.skip");
                        self.builder
                            .build_conditional_branch(inner_is_heap, inner_free_bb, inner_skip_bb)
                            .unwrap();

                        self.builder.position_at_end(inner_free_bb);
                        let inner_data_ptr = self
                            .builder
                            .build_struct_gep(
                                self.vec_struct_type(),
                                inner_struct_ptr,
                                0,
                                "cleanup.drop.inner.data.ptr",
                            )
                            .unwrap();
                        let inner_data = self
                            .builder
                            .build_load(ptr_ty, inner_data_ptr, "cleanup.drop.inner.data")
                            .unwrap()
                            .into_pointer_value();
                        // Recycling-aware release; erased inner element
                        // buffer → cap × 1 hint.
                        self.emit_free_buf_call(inner_data, inner_cap, 1);
                        self.builder
                            .build_unconditional_branch(inner_skip_bb)
                            .unwrap();

                        self.builder.position_at_end(inner_skip_bb);
                        let one = i64_t.const_int(1, false);
                        let next = self
                            .builder
                            .build_int_add(cur, one, "cleanup.drop.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder
                            .build_unconditional_branch(drop_cond_bb)
                            .unwrap();

                        self.builder.position_at_end(drop_after_bb);
                    } else if let Some(field_idxs) = self.struct_owned_vec_field_indices(*et) {
                        // Element is a tuple / struct whose fields include
                        // owned Vec/String buffers (`Vec[(i64, String)]`,
                        // B-2026-06-10-5). The vec-struct fast path above
                        // only frees an element that is ITSELF a Vec/String;
                        // a heap field nested in a tuple element leaks.
                        // Iterate `len` elements and free each live heap
                        // field's data buffer before releasing the outer
                        // buffer. One level into the element — symmetric with
                        // the one-level Vec recursion above; a heap field that
                        // is itself a tuple / Map / nested collection still
                        // leaks (same deeper-nesting limitation).
                        let elem_struct = (*et).into_struct_type();
                        let vs = self.vec_struct_type();
                        let len_ptr = self
                            .builder
                            .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.tup.len.ptr")
                            .unwrap();
                        let len = self
                            .builder
                            .build_load(i64_t, len_ptr, "cleanup.tup.len")
                            .unwrap()
                            .into_int_value();
                        let counter =
                            self.create_entry_alloca(fn_val, "cleanup.tup.i", i64_t.into());
                        self.builder.build_store(counter, zero).unwrap();
                        let cond_bb = self.context.append_basic_block(fn_val, "cleanup.tup.cond");
                        let body_bb = self.context.append_basic_block(fn_val, "cleanup.tup.body");
                        let after_bb = self.context.append_basic_block(fn_val, "cleanup.tup.after");
                        self.builder.build_unconditional_branch(cond_bb).unwrap();

                        self.builder.position_at_end(cond_bb);
                        let cur = self
                            .builder
                            .build_load(i64_t, counter, "cleanup.tup.cur")
                            .unwrap()
                            .into_int_value();
                        let lt = self
                            .builder
                            .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.tup.lt")
                            .unwrap();
                        self.builder
                            .build_conditional_branch(lt, body_bb, after_bb)
                            .unwrap();

                        self.builder.position_at_end(body_bb);
                        let elem_ptr = unsafe {
                            self.builder
                                .build_gep(elem_struct, data, &[cur], "cleanup.tup.elem")
                                .unwrap()
                        };
                        for &fidx in &field_idxs {
                            let field_ptr = self
                                .builder
                                .build_struct_gep(elem_struct, elem_ptr, fidx, "cleanup.tup.field")
                                .unwrap();
                            let fcap_ptr = self
                                .builder
                                .build_struct_gep(vs, field_ptr, 2, "cleanup.tup.field.cap.ptr")
                                .unwrap();
                            let fcap = self
                                .builder
                                .build_load(i64_t, fcap_ptr, "cleanup.tup.field.cap")
                                .unwrap()
                                .into_int_value();
                            let fheap = self
                                .builder
                                .build_int_compare(
                                    IntPredicate::UGT,
                                    fcap,
                                    zero,
                                    "cleanup.tup.field.heap",
                                )
                                .unwrap();
                            let ffree_bb = self
                                .context
                                .append_basic_block(fn_val, "cleanup.tup.field.free");
                            let fskip_bb = self
                                .context
                                .append_basic_block(fn_val, "cleanup.tup.field.skip");
                            self.builder
                                .build_conditional_branch(fheap, ffree_bb, fskip_bb)
                                .unwrap();
                            self.builder.position_at_end(ffree_bb);
                            let fdata_ptr = self
                                .builder
                                .build_struct_gep(vs, field_ptr, 0, "cleanup.tup.field.data.ptr")
                                .unwrap();
                            let fdata = self
                                .builder
                                .build_load(ptr_ty, fdata_ptr, "cleanup.tup.field.data")
                                .unwrap()
                                .into_pointer_value();
                            // Recycling-aware release; erased tuple field
                            // buffer → cap × 1 hint.
                            self.emit_free_buf_call(fdata, fcap, 1);
                            self.builder.build_unconditional_branch(fskip_bb).unwrap();
                            self.builder.position_at_end(fskip_bb);
                        }
                        let one = i64_t.const_int(1, false);
                        let next = self
                            .builder
                            .build_int_add(cur, one, "cleanup.tup.next")
                            .unwrap();
                        self.builder.build_store(counter, next).unwrap();
                        self.builder.build_unconditional_branch(cond_bb).unwrap();

                        self.builder.position_at_end(after_bb);
                    }
                }

                // Tensor-element drop: each element is a single `ptr` to a
                // `[rank][dims][data]` block (the `iter_axis` result Vec).
                // Iterate `len` elements and `free` each before releasing
                // the outer buffer. One free per element — tensors are
                // single allocations, no inner recursion. `free(null)` is a
                // no-op, so no per-element null guard is needed.
                if *elem_is_tensor {
                    let len_ptr = self
                        .builder
                        .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.tdrop.len.ptr")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_ptr, "cleanup.tdrop.len")
                        .unwrap()
                        .into_int_value();
                    let counter = self.create_entry_alloca(fn_val, "cleanup.tdrop.i", i64_t.into());
                    self.builder.build_store(counter, zero).unwrap();
                    let tcond_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.tdrop.cond");
                    let tbody_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.tdrop.body");
                    let tafter_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.tdrop.after");
                    self.builder.build_unconditional_branch(tcond_bb).unwrap();
                    self.builder.position_at_end(tcond_bb);
                    let cur = self
                        .builder
                        .build_load(i64_t, counter, "cleanup.tdrop.cur")
                        .unwrap()
                        .into_int_value();
                    let lt = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.tdrop.lt")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(lt, tbody_bb, tafter_bb)
                        .unwrap();
                    self.builder.position_at_end(tbody_bb);
                    let elem_pp = unsafe {
                        self.builder
                            .build_gep(ptr_ty, data, &[cur], "cleanup.tdrop.elem.pp")
                            .unwrap()
                    };
                    let elem_p = self
                        .builder
                        .build_load(ptr_ty, elem_pp, "cleanup.tdrop.elem")
                        .unwrap()
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[elem_p.into()], "")
                        .unwrap();
                    let one = i64_t.const_int(1, false);
                    let next = self
                        .builder
                        .build_int_add(cur, one, "cleanup.tdrop.next")
                        .unwrap();
                    self.builder.build_store(counter, next).unwrap();
                    self.builder.build_unconditional_branch(tcond_bb).unwrap();
                    self.builder.position_at_end(tafter_bb);
                }

                // Map/Set-element drop: each element is an opaque map handle
                // (a single `ptr`). Free each live element exactly as a
                // standalone Map binding would (shared-half rc_dec walks +
                // `karac_map_free[_with_drop_vec]`, via `emit_free_one_map_handle`)
                // before releasing the outer buffer. The Vec OWNS its map
                // elements — the move-into-Vec push transferred ownership by
                // suppressing the source's `FreeMapHandle`; without this free
                // they'd leak, and *with* the suppression a missing free here
                // would be a premature-free / UAF (Cluster 1). Every slot in
                // `[0, len)` holds a real handle (push stores one per element),
                // so no per-element null guard — and `karac_map_free` is not
                // null-tolerant anyway.
                if let Some(map_drop) = elem_map_drop {
                    let map_drop = map_drop.clone();
                    let len_ptr = self
                        .builder
                        .build_struct_gep(vec_ty, *vec_alloca, 1, "cleanup.mdrop.len.ptr")
                        .unwrap();
                    let len = self
                        .builder
                        .build_load(i64_t, len_ptr, "cleanup.mdrop.len")
                        .unwrap()
                        .into_int_value();
                    let counter = self.create_entry_alloca(fn_val, "cleanup.mdrop.i", i64_t.into());
                    self.builder.build_store(counter, zero).unwrap();
                    let mcond_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.mdrop.cond");
                    let mbody_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.mdrop.body");
                    let mafter_bb = self
                        .context
                        .append_basic_block(fn_val, "cleanup.mdrop.after");
                    self.builder.build_unconditional_branch(mcond_bb).unwrap();
                    self.builder.position_at_end(mcond_bb);
                    let cur = self
                        .builder
                        .build_load(i64_t, counter, "cleanup.mdrop.cur")
                        .unwrap()
                        .into_int_value();
                    let lt = self
                        .builder
                        .build_int_compare(IntPredicate::ULT, cur, len, "cleanup.mdrop.lt")
                        .unwrap();
                    self.builder
                        .build_conditional_branch(lt, mbody_bb, mafter_bb)
                        .unwrap();
                    self.builder.position_at_end(mbody_bb);
                    let elem_pp = unsafe {
                        self.builder
                            .build_gep(ptr_ty, data, &[cur], "cleanup.mdrop.elem.pp")
                            .unwrap()
                    };
                    let handle = self
                        .builder
                        .build_load(ptr_ty, elem_pp, "cleanup.mdrop.handle")
                        .unwrap()
                        .into_pointer_value();
                    self.emit_free_one_map_handle(handle, &map_drop);
                    // `emit_free_one_map_handle` may have split the block
                    // (shared-half rc_dec walk) — reload the current block as
                    // the loop back-edge source.
                    let one = i64_t.const_int(1, false);
                    let next = self
                        .builder
                        .build_int_add(cur, one, "cleanup.mdrop.next")
                        .unwrap();
                    self.builder.build_store(counter, next).unwrap();
                    self.builder.build_unconditional_branch(mcond_bb).unwrap();
                    self.builder.position_at_end(mafter_bb);
                }

                // Recycling-aware outer-buffer release (large-buffer cache):
                // hint = cap × element abi size when the element LLVM type
                // is known. A String / untyped binding passes element size 1
                // — exact for String (cap IS the byte count), a sound
                // under-hint otherwise.
                let elem_abi_size = match elem_ty {
                    Some(et) => self
                        .target_data
                        .as_ref()
                        .map(|td| td.get_abi_size(et))
                        .unwrap_or(0),
                    None => 1,
                };
                self.emit_free_buf_call(data, cap, elem_abi_size);
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeInlineOptionPayload {
                option_slot,
                option_ty,
                some_tag,
                payload_elem_ty,
            } => {
                // Tag-guard: only the `Some` discriminant carries a payload.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 0, "optpl.tag.ptr")
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, "optpl.tag")
                    .unwrap()
                    .into_int_value();
                let some_c = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, tag, some_c, "optpl.is_some")
                    .unwrap();
                let some_bb = self.context.append_basic_block(fn_val, "optpl.some");
                let done_bb = self.context.append_basic_block(fn_val, "optpl.done");
                self.builder
                    .build_conditional_branch(is_some, some_bb, done_bb)
                    .unwrap();
                self.builder.position_at_end(some_bb);
                // The `Some` payload's `{ptr,len,cap}` overlays words
                // w0/w1/w2 (option field index 1). The shared helper emits
                // the cap-guarded recursive free of that overlay and leaves
                // the builder at its internal skip block.
                self.emit_free_inline_payload_overlay(
                    *option_slot,
                    *option_ty,
                    *payload_elem_ty,
                    fn_val,
                    vec_ty,
                    ptr_ty,
                    i64_t,
                    "optpl",
                );
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
            CleanupAction::FreeInlineResultPayload {
                result_slot,
                result_ty,
                ok_tag,
                err_tag,
                ok_payload_elem_ty,
                err_payload_elem_ty,
                ok_payload_struct_drop,
                err_payload_struct_drop,
            } => {
                // `Result[T, E]` shares the tagged-union layout `{tag, w0,
                // w1, w2}` — the `Ok` and `Err` payloads OVERLAY the same
                // words, distinguished only by the tag. Free whichever
                // variant is live, keyed on its concrete payload shape (the
                // erased layout can't carry it — B-2026-06-10-6's `Result`
                // follow-on). Each side is one of THREE shapes: a scalar/
                // non-heap half (both `None` → nothing), a direct-heap overlay
                // (`elem_ty` = `Some`, a `{ptr,len,cap}` at payload offset 0),
                // or a struct-with-heap payload (`struct_drop` = `Some` — a
                // multi-field `Rec { id, name: String }` / a transparent
                // wrapper like `AlreadySetError[Rec]`, freed by running the
                // full struct drop on a pointer to the payload area,
                // B-2026-07-12-2 gap 3). The two are mutually exclusive per
                // side. A consuming match arm zeros the whole payload area
                // (`suppress_inline_result_payload_cleanup*`) so a moved-out
                // payload's overlay `cap` reads 0 AND its struct drop's heap-
                // field caps read 0 — both skip, leaving the binding sole owner.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(*result_ty, *result_slot, 0, "respl.tag.ptr")
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, "respl.tag")
                    .unwrap()
                    .into_int_value();
                let done_bb = self.context.append_basic_block(fn_val, "respl.done");
                // Pointer to the payload area (result field 1 = w0) — the
                // struct payload lays out there bit-for-bit, so the drop fn
                // reads it as the concrete struct.
                let payload_ptr = self
                    .builder
                    .build_struct_gep(*result_ty, *result_slot, 1, "respl.payload.ptr")
                    .unwrap();
                // Ok arm.
                if ok_payload_elem_ty.is_some() || ok_payload_struct_drop.is_some() {
                    let ok_c = i64_t.const_int(*ok_tag, false);
                    let is_ok = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, tag, ok_c, "respl.is_ok")
                        .unwrap();
                    let ok_bb = self.context.append_basic_block(fn_val, "respl.ok");
                    let after_ok_bb = self.context.append_basic_block(fn_val, "respl.after_ok");
                    self.builder
                        .build_conditional_branch(is_ok, ok_bb, after_ok_bb)
                        .unwrap();
                    self.builder.position_at_end(ok_bb);
                    if let Some(drop_fn) = ok_payload_struct_drop {
                        self.builder
                            .build_call(*drop_fn, &[payload_ptr.into()], "")
                            .unwrap();
                    } else {
                        self.emit_free_inline_payload_overlay(
                            *result_slot,
                            *result_ty,
                            *ok_payload_elem_ty,
                            fn_val,
                            vec_ty,
                            ptr_ty,
                            i64_t,
                            "respl.ok",
                        );
                    }
                    self.builder.build_unconditional_branch(done_bb).unwrap();
                    self.builder.position_at_end(after_ok_bb);
                }
                // Err arm.
                if err_payload_elem_ty.is_some() || err_payload_struct_drop.is_some() {
                    let err_c = i64_t.const_int(*err_tag, false);
                    let is_err = self
                        .builder
                        .build_int_compare(IntPredicate::EQ, tag, err_c, "respl.is_err")
                        .unwrap();
                    let err_bb = self.context.append_basic_block(fn_val, "respl.err");
                    let after_err_bb = self.context.append_basic_block(fn_val, "respl.after_err");
                    self.builder
                        .build_conditional_branch(is_err, err_bb, after_err_bb)
                        .unwrap();
                    self.builder.position_at_end(err_bb);
                    if let Some(drop_fn) = err_payload_struct_drop {
                        self.builder
                            .build_call(*drop_fn, &[payload_ptr.into()], "")
                            .unwrap();
                    } else {
                        self.emit_free_inline_payload_overlay(
                            *result_slot,
                            *result_ty,
                            *err_payload_elem_ty,
                            fn_val,
                            vec_ty,
                            ptr_ty,
                            i64_t,
                            "respl.err",
                        );
                    }
                    self.builder.build_unconditional_branch(done_bb).unwrap();
                    self.builder.position_at_end(after_err_bb);
                }
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
            CleanupAction::FreeInlineOptionMapPayload {
                option_slot,
                option_ty,
                some_tag,
                map_drop,
            } => {
                // Tag-guard: only `Some` carries a handle. The handle is a
                // single `ptr` at word w0 (option field index 1); free it
                // exactly as a standalone Map binding (`emit_free_one_map_handle`).
                let tag_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 0, "optmap.tag.ptr")
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, "optmap.tag")
                    .unwrap()
                    .into_int_value();
                let some_c = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(IntPredicate::EQ, tag, some_c, "optmap.is_some")
                    .unwrap();
                let some_bb = self.context.append_basic_block(fn_val, "optmap.some");
                let done_bb = self.context.append_basic_block(fn_val, "optmap.done");
                self.builder
                    .build_conditional_branch(is_some, some_bb, done_bb)
                    .unwrap();
                self.builder.position_at_end(some_bb);
                let handle_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 1, "optmap.handle.ptr")
                    .unwrap();
                let handle = self
                    .builder
                    .build_load(ptr_ty, handle_ptr, "optmap.handle")
                    .unwrap()
                    .into_pointer_value();
                self.emit_free_one_map_handle(handle, map_drop);
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
            CleanupAction::FreeSoaGroups {
                soa_alloca,
                soa_struct_ty,
                num_hot_groups,
                has_cold,
                soa_drop_fn,
            } => {
                // cap > 0 ⇒ groups were allocated. Read cap via the SoA
                // struct type so the GEP lands on the actual cap slot
                // (last field), not whichever slot collides with the
                // plain Vec `{ptr,len,cap}` layout's field 2.
                let cap_idx = *num_hot_groups + if *has_cold { 1 } else { 0 } + 1;
                let cap_ptr = self
                    .builder
                    .build_struct_gep(*soa_struct_ty, *soa_alloca, cap_idx, "soa.cleanup.cap.ptr")
                    .unwrap();
                let cap = self
                    .builder
                    .build_load(i64_t, cap_ptr, "soa.cleanup.cap")
                    .unwrap()
                    .into_int_value();
                let zero = i64_t.const_int(0, false);
                let is_heap = self
                    .builder
                    .build_int_compare(IntPredicate::UGT, cap, zero, "soa.cleanup.is_heap")
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "soa.cleanup.free");
                let skip_bb = self.context.append_basic_block(fn_val, "soa.cleanup.skip");
                self.builder
                    .build_conditional_branch(is_heap, free_bb, skip_bb)
                    .unwrap();

                self.builder.position_at_end(free_bb);
                // Per-element heap-field drop FIRST (before the buffers that
                // hold those elements are freed): for a layout whose element
                // struct carries String/Vec fields, call the synthesized
                // `__karac_soa_drop_<layout>` over the live range. `None` for a
                // POD layout, so this emits no IR there — byte-identical
                // cleanup. The fn loops `[0, len)`, so a `cap > 0` header whose
                // `len == 0` is a no-op too.
                if let Some(drop_fn) = soa_drop_fn {
                    self.builder
                        .build_call(*drop_fn, &[(*soa_alloca).into()], "")
                        .unwrap();
                }
                // Free each hot group buffer in declaration order, then the
                // cold buffer if present. Each group is its own malloc
                // (see `compile_soa_method`'s push-grow loop); a single
                // `free(g0)` leaks the rest.
                let total_ptrs = *num_hot_groups + if *has_cold { 1 } else { 0 };
                for gi in 0..total_ptrs {
                    let grp_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            *soa_struct_ty,
                            *soa_alloca,
                            gi,
                            &format!("soa.cleanup.g{}.ptr", gi),
                        )
                        .unwrap();
                    let grp_ptr = self
                        .builder
                        .build_load(ptr_ty, grp_ptr_ptr, &format!("soa.cleanup.g{}.buf", gi))
                        .unwrap()
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[grp_ptr.into()], "")
                        .unwrap();
                }
                self.builder.build_unconditional_branch(skip_bb).unwrap();
                self.builder.position_at_end(skip_bb);
            }
            CleanupAction::FreeMapHandle {
                map_alloca,
                key_is_vec,
                val_is_vec,
                val_shared_heap_type,
                key_shared_heap_type,
                val_drop_fn,
            } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *map_alloca, "cleanup.map.handle")
                    .unwrap()
                    .into_pointer_value();
                // Single-handle free shared with the `Vec[Map]`/`Vec[Set]`
                // element-drop loop. The shared-half rc_dec walks run first
                // (they read live bucket bytes, before the storage release);
                // then `karac_map_free_with_val_drop_fn` when the value has
                // a synthesized drop fn (slice 3r), else
                // `karac_map_free_with_drop_vec` when either half owns
                // Vec/String heap, else plain `karac_map_free`. Closes the
                // 2026-05-13/14/16 map leaks; see `emit_free_one_map_handle`.
                let drop = crate::codegen::state::MapElemDrop {
                    key_is_vec: *key_is_vec,
                    val_is_vec: *val_is_vec,
                    val_shared_heap_type: *val_shared_heap_type,
                    key_shared_heap_type: *key_shared_heap_type,
                    val_drop_fn: *val_drop_fn,
                };
                self.emit_free_one_map_handle(handle, &drop);
            }
            // Phase 8 `File` handle slice F4b — close the file fd at
            // scope exit. Load the handle from its alloca, hand it to
            // `karac_runtime_file_close` which reconstructs the Box
            // and drops it (releasing the OS fd via std::fs::File's
            // own Drop). Null-handle is a no-op on the runtime side.
            CleanupAction::FreeFileHandle { file_alloca } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *file_alloca, "cleanup.file.handle")
                    .unwrap()
                    .into_pointer_value();
                let close_fn = self
                    .module
                    .get_function("karac_runtime_file_close")
                    .expect("karac_runtime_file_close declared in Codegen::new");
                self.builder
                    .build_call(close_fn, &[handle.into()], "")
                    .unwrap();
            }
            // LazyFrame codegen twin — release a `LazyExpr` handle produced
            // in this scope (the release-everywhere ownership model; see
            // `runtime/src/lazy.rs`). Load the raw `Arc` pointer from its
            // alloca and drop one strong count.
            CleanupAction::ReleaseLazyExpr { alloca } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *alloca, "cleanup.lazyexpr.handle")
                    .unwrap()
                    .into_pointer_value();
                let rel_fn = self
                    .module
                    .get_function("karac_lazy_expr_release")
                    .expect("karac_lazy_expr_release declared in Codegen::new");
                self.builder
                    .build_call(rel_fn, &[handle.into()], "")
                    .unwrap();
            }
            // The plan-handle sibling: release a `LazyFrame` plan handle.
            CleanupAction::ReleaseLazyPlan { alloca } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *alloca, "cleanup.lazyplan.handle")
                    .unwrap()
                    .into_pointer_value();
                let rel_fn = self
                    .module
                    .get_function("karac_lazy_release")
                    .expect("karac_lazy_release declared in Codegen::new");
                self.builder
                    .build_call(rel_fn, &[handle.into()], "")
                    .unwrap();
            }
            // The group-by-intermediate sibling: release a `LazyGroupBy`
            // handle produced by `karac_lazy_group_by`.
            CleanupAction::ReleaseLazyGroupBy { alloca } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *alloca, "cleanup.lazygb.handle")
                    .unwrap()
                    .into_pointer_value();
                let rel_fn = self
                    .module
                    .get_function("karac_lazy_gb_release")
                    .expect("karac_lazy_gb_release declared in Codegen::new");
                self.builder
                    .build_call(rel_fn, &[handle.into()], "")
                    .unwrap();
            }
            CleanupAction::FreeGpuBuffer { buf_alloca } => {
                // Load field 0 (the i64 resident handle) of the `{handle, n}`
                // buffer value and free it. `karac_runtime_gpu_free_soa` is
                // idempotent (no-op on an already-downloaded/freed handle), so no
                // `handle != 0` guard is needed.
                let i64_t = self.context.i64_type();
                let buf_ty = self.gpu_buffer_type();
                let handle_field = self
                    .builder
                    .build_struct_gep(buf_ty, *buf_alloca, 0, "cleanup.gpu.handle.p")
                    .unwrap();
                let handle = self
                    .builder
                    .build_load(i64_t, handle_field, "cleanup.gpu.handle")
                    .unwrap()
                    .into_int_value();
                let free_fn = self.gpu_free_soa_fn();
                self.builder
                    .build_call(free_fn, &[handle.into()], "")
                    .unwrap();
            }
            CleanupAction::FreeOnceHandle {
                once_alloca,
                elem_drop,
            } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *once_alloca, "cleanup.once.handle")
                    .unwrap()
                    .into_pointer_value();
                // Heap-owning `T` (B-2026-07-12-2 gap 1): the sealed value's
                // inner heap (a `String`/`Vec` buffer moved in by `set`) is owned
                // by the cell, so run the element drop on the sealed value ptr
                // BEFORE freeing the header + control block. `karac_runtime_once_get`
                // returns a stable pointer to the sealed `T` (or null if the cell
                // was never sealed) — null-guard the drop so an unset cell is a
                // no-op. The `once_free` below then reclaims the header.
                if let Some(drop_fn) = elem_drop {
                    let get_fn = self
                        .module
                        .get_function("karac_runtime_once_get")
                        .expect("karac_runtime_once_get declared in Codegen::new");
                    let vptr = self
                        .builder
                        .build_call(get_fn, &[handle.into()], "cleanup.once.val")
                        .unwrap()
                        .try_as_basic_value()
                        .unwrap_basic()
                        .into_pointer_value();
                    let is_null = self
                        .builder
                        .build_is_null(vptr, "cleanup.once.val.null")
                        .unwrap();
                    let fn_val = self.current_fn.unwrap();
                    let drop_bb = self.context.append_basic_block(fn_val, "cleanup.once.drop");
                    let cont_bb = self.context.append_basic_block(fn_val, "cleanup.once.cont");
                    self.builder
                        .build_conditional_branch(is_null, cont_bb, drop_bb)
                        .unwrap();
                    self.builder.position_at_end(drop_bb);
                    self.builder
                        .build_call(*drop_fn, &[vptr.into()], "")
                        .unwrap();
                    self.builder.build_unconditional_branch(cont_bb).unwrap();
                    self.builder.position_at_end(cont_bb);
                }
                let free_fn = self
                    .module
                    .get_function("karac_runtime_once_free")
                    .expect("karac_runtime_once_free declared in Codegen::new");
                self.builder
                    .build_call(free_fn, &[handle.into()], "")
                    .unwrap();
            }
            CleanupAction::FreeInternerHandle { interner_alloca } => {
                // Local `Interner` binding: one call reclaims the interner
                // and every stored byte string (the runtime owns them all;
                // `resolve` borrows are `cap = 0` views that no free path
                // touches). Null-handle is a runtime no-op.
                let handle = self
                    .builder
                    .build_load(ptr_ty, *interner_alloca, "cleanup.interner.handle")
                    .unwrap()
                    .into_pointer_value();
                let free_fn = self
                    .module
                    .get_function("karac_runtime_interner_free")
                    .expect("karac_runtime_interner_free declared in Codegen::new");
                self.builder
                    .build_call(free_fn, &[handle.into()], "")
                    .unwrap();
            }
            CleanupAction::FreeArenaHandle { arena_alloca } => {
                // Local `Arena[T]` binding: one call reclaims the arena and
                // every stored blob (the runtime owns them all; `get`
                // borrows are `cap = 0` views that no free path touches).
                // Null-handle is a runtime no-op.
                let handle = self
                    .builder
                    .build_load(ptr_ty, *arena_alloca, "cleanup.arena.handle")
                    .unwrap()
                    .into_pointer_value();
                let free_fn = self
                    .module
                    .get_function("karac_runtime_arena_free")
                    .expect("karac_runtime_arena_free declared in Codegen::new");
                self.builder
                    .build_call(free_fn, &[handle.into()], "")
                    .unwrap();
            }
            CleanupAction::FreeClosureEnv { fat_alloca } => {
                // Slice 1 (B-2026-06-22-2): RC-drop a heap-env closure binding.
                // Load the fat pointer and hand it to the shared dec helper,
                // which extracts the env box (field 1), skips a null env, and
                // decrements / frees the box at zero.
                let fat_ty = self.closure_value_type();
                let fat = self
                    .builder
                    .build_load(fat_ty, *fat_alloca, "cleanup.clo.fat")
                    .unwrap();
                self.emit_heap_closure_env_dec(fat);
            }
            // Phase 6 "Channel AOT codegen lowering" — refcount-drop a
            // channel end at scope exit. Load the shared `*mut KaracChannel`
            // and hand it to `karac_runtime_channel_drop`, which decrements
            // the refcount and frees the queue at zero. Null-handle is a
            // no-op runtime-side.
            CleanupAction::DropChannelEnd {
                chan_alloca,
                is_sender,
            } => {
                let handle = self
                    .builder
                    .build_load(ptr_ty, *chan_alloca, "cleanup.chan.handle")
                    .unwrap()
                    .into_pointer_value();
                let drop_name = if *is_sender {
                    "karac_runtime_channel_drop_sender"
                } else {
                    "karac_runtime_channel_drop_receiver"
                };
                let drop_fn = self
                    .module
                    .get_function(drop_name)
                    .expect("channel drop fn declared in Codegen::new");
                self.builder
                    .build_call(drop_fn, &[handle.into()], "")
                    .unwrap();
            }
            // Phase 7.2 Slice DP — invoke the per-enum drop
            // function on the alloca. The drop fn takes a
            // pointer to the enum struct and walks the tag-
            // switch / per-variant cleanup BBs internally.
            CleanupAction::EnumDrop {
                enum_alloca,
                drop_fn,
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*enum_alloca).into()], "")
                    .unwrap();
            }
            CleanupAction::StructDrop {
                struct_alloca,
                drop_fn,
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*struct_alloca).into()], "")
                    .unwrap();
            }
            // Phase 7 user-`impl Drop` dispatch Prereq.3 — invoke the
            // per-type wrapper `karac_drop_<Type>` on the binding. The
            // wrapper internally calls the user-defined `<Type>.drop`
            // method body, then (when the type has heap-owning fields)
            // hands off to the existing `__karac_drop_struct_<Type>`
            // field cleanup synthesiser. Registration at let-binding
            // time is mutually exclusive with `StructDrop`, so this
            // path is the unique field-cleanup invocation for types
            // with a user Drop impl.
            CleanupAction::UserDrop {
                binding_ptr,
                drop_fn,
                ..
            } => {
                self.builder
                    .build_call(*drop_fn, &[(*binding_ptr).into()], "")
                    .unwrap();
            }
            // `Option[shared T]` binding — load the tag, branch on
            // Some, recover the inner pointer from word 0, dispatch
            // through `emit_refcount_dec`. None side is a no-op (no
            // inner heap allocation to release). Mirrors the `RcDec`
            // arm's reload-from-slot discipline so a reassignment of
            // the binding is observed at scope exit; mirrors the
            // null-guard shape but on the tag instead of a pointer
            // (`tag == None` is the "skip" path here).
            CleanupAction::RcDecOption {
                name,
                option_slot,
                option_ty,
                heap_type,
                some_tag,
            } => {
                // GEP to tag (field 0), load, compare with Some-tag.
                let tag_ptr = self
                    .builder
                    .build_struct_gep(
                        *option_ty,
                        *option_slot,
                        0,
                        &format!("{}_opt_tag_ptr", name),
                    )
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, &format!("{}_opt_tag", name))
                    .unwrap()
                    .into_int_value();
                let some_tag_const = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        some_tag_const,
                        &format!("{}_opt_is_some", name),
                    )
                    .unwrap();
                let do_bb = self.context.append_basic_block(fn_val, "opt_rc_cleanup_do");
                let skip_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_skip");
                let join_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_join");
                self.builder
                    .build_conditional_branch(is_some, do_bb, skip_bb)
                    .unwrap();
                // Some-side: load w0 (field 1) as i64, int_to_ptr,
                // dec. The Some-side inner pointer can itself be null
                // in malformed-IR cases — defensive null-skip mirrors
                // the `RcDec` arm so a hypothetical future codegen
                // shape that stores a sentinel-null doesn't crash the
                // dec. The common case (a real Some(ptr) payload) has
                // a non-null pointer.
                self.builder.position_at_end(do_bb);
                let w0_ptr = self
                    .builder
                    .build_struct_gep(*option_ty, *option_slot, 1, &format!("{}_opt_w0_ptr", name))
                    .unwrap();
                let w0 = self
                    .builder
                    .build_load(i64_t, w0_ptr, &format!("{}_opt_w0", name))
                    .unwrap()
                    .into_int_value();
                let inner_ptr = self
                    .builder
                    .build_int_to_ptr(w0, ptr_ty, &format!("{}_opt_inner_ptr", name))
                    .unwrap();
                let inner_null = ptr_ty.const_null();
                let inner_is_null = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        inner_ptr,
                        inner_null,
                        &format!("{}_opt_inner_is_null", name),
                    )
                    .unwrap();
                let inner_do_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_inner_do");
                let inner_skip_bb = self
                    .context
                    .append_basic_block(fn_val, "opt_rc_cleanup_inner_skip");
                self.builder
                    .build_conditional_branch(inner_is_null, inner_skip_bb, inner_do_bb)
                    .unwrap();
                self.builder.position_at_end(inner_do_bb);
                self.emit_refcount_dec(name, *heap_type, inner_ptr);
                self.builder
                    .build_unconditional_branch(inner_skip_bb)
                    .unwrap();
                self.builder.position_at_end(inner_skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(skip_bb);
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            // Oversized boxed enum payload (see `coerce_to_payload_words`):
            // free the heap box. Load the tag, branch on the payload-
            // bearing discriminant, recover the box pointer from word 0,
            // run the inner drop fn (when `T` owns heap), then `free` the
            // box. Mirrors `RcDecOption` with `free` in place of the
            // refcount dec.
            CleanupAction::BoxedEnumDrop {
                name,
                enum_slot,
                enum_ty,
                inner_drop_fn,
                some_tag,
            } => {
                let tag_ptr = self
                    .builder
                    .build_struct_gep(*enum_ty, *enum_slot, 0, &format!("{}_box_tag_ptr", name))
                    .unwrap();
                let tag = self
                    .builder
                    .build_load(i64_t, tag_ptr, &format!("{}_box_tag", name))
                    .unwrap()
                    .into_int_value();
                let some_tag_const = i64_t.const_int(*some_tag, false);
                let is_some = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        tag,
                        some_tag_const,
                        &format!("{}_box_is_some", name),
                    )
                    .unwrap();
                let do_bb = self.context.append_basic_block(fn_val, "boxdrop_do");
                let join_bb = self.context.append_basic_block(fn_val, "boxdrop_join");
                self.builder
                    .build_conditional_branch(is_some, do_bb, join_bb)
                    .unwrap();
                self.builder.position_at_end(do_bb);
                let w0_ptr = self
                    .builder
                    .build_struct_gep(*enum_ty, *enum_slot, 1, &format!("{}_box_w0_ptr", name))
                    .unwrap();
                let w0 = self
                    .builder
                    .build_load(i64_t, w0_ptr, &format!("{}_box_w0", name))
                    .unwrap()
                    .into_int_value();
                let box_ptr = self
                    .builder
                    .build_int_to_ptr(w0, ptr_ty, &format!("{}_box_ptr", name))
                    .unwrap();
                // Defensive null-guard (mirrors RcDecOption): a real
                // Some/Ok payload box is never null, but a future codegen
                // shape storing a sentinel must not crash the free.
                let is_null = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        box_ptr,
                        ptr_ty.const_null(),
                        &format!("{}_box_is_null", name),
                    )
                    .unwrap();
                let free_bb = self.context.append_basic_block(fn_val, "boxdrop_free");
                self.builder
                    .build_conditional_branch(is_null, join_bb, free_bb)
                    .unwrap();
                self.builder.position_at_end(free_bb);
                // The box points directly at `T`; run its field cleanup
                // before releasing the box (no-op when `T` is all-inline).
                if let Some(drop_fn) = inner_drop_fn {
                    self.builder
                        .build_call(*drop_fn, &[box_ptr.into()], "")
                        .unwrap();
                }
                self.builder
                    .build_call(self.free_fn, &[box_ptr.into()], "")
                    .unwrap();
                self.builder.build_unconditional_branch(join_bb).unwrap();
                self.builder.position_at_end(join_bb);
            }
            CleanupAction::UserDefer(_) => {
                // Routed through `emit_cleanup_action_at` instead — user-defer
                // bodies require `&mut self` to compile a Block, while this
                // function is `&self`. The indirection at the drain sites
                // (`emit_scope_cleanup` / `drain_top_frame_with_emit`) splits
                // the UserDefer case out before reaching this match.
                unreachable!(
                    "CleanupAction::UserDefer must be dispatched via emit_cleanup_action_at"
                );
            }
            CleanupAction::UserErrDefer { .. } => {
                // Routed through `emit_cleanup_action_at` instead — same
                // shape as UserDefer (the errdefer body needs `&mut self`
                // to compile a Block). On normal-exit drains
                // (`emit_scope_cleanup` / `drain_top_frame_with_emit`)
                // errdefers are filtered out before reaching this match;
                // on error-exit drains (`emit_scope_cleanup_for_error_path`)
                // errdefers are routed via `emit_cleanup_action_at` in
                // phase 1. Reaching this arm means the cleanup-action
                // index walked an errdefer slot on a normal-exit path,
                // which is a routing bug.
                unreachable!(
                    "CleanupAction::UserErrDefer must be dispatched via emit_cleanup_action_at on an error-exit path"
                );
            }
            CleanupAction::ReleaseMutex { flag_ptr } => {
                // Futex 3-state release (mirrors `compile_lock_block`'s acquire):
                // atomically swap the flag to 0 and read the prior state.
                //   1 = locked-uncontended → no parked waiter → inline-only, no
                //       runtime call (the fast path stays call-free).
                //   2 = locked-contended   → a waiter is parked → wake it via
                //       `karac_runtime_mutex_unlock_wake`.
                // Routing this through the cleanup frame is what makes the
                // release (and the conditional wake) fire on early-exit paths
                // too — break/continue/return all drain this action.
                let prev = self
                    .builder
                    .build_atomicrmw(
                        AtomicRMWBinOp::Xchg,
                        *flag_ptr,
                        i64_t.const_zero(),
                        AtomicOrdering::SequentiallyConsistent,
                    )
                    .expect("lock release: build_atomicrmw");
                let was_contended = self
                    .builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        prev,
                        i64_t.const_int(2, false),
                        "lock.was_contended",
                    )
                    .unwrap();
                let wake_bb = self.context.append_basic_block(fn_val, "lock.wake");
                let done_bb = self.context.append_basic_block(fn_val, "lock.release.done");
                self.builder
                    .build_conditional_branch(was_contended, wake_bb, done_bb)
                    .unwrap();
                self.builder.position_at_end(wake_bb);
                let wake_fn = self
                    .module
                    .get_function("karac_runtime_mutex_unlock_wake")
                    .expect("karac_runtime_mutex_unlock_wake declared in Codegen::new");
                self.builder
                    .build_call(wake_fn, &[(*flag_ptr).into()], "lock.wake.call")
                    .unwrap();
                self.builder.build_unconditional_branch(done_bb).unwrap();
                self.builder.position_at_end(done_bb);
            }
        }
    }

    /// Walk every live bucket of `map_handle` and emit `rc_dec` on
    /// one half of the slot — value when `is_val == true`, key when
    /// `is_val == false`. Used by `FreeMapHandle` cleanup when the
    /// corresponding side is a shared struct / shared enum — the
    /// type-erased runtime (`karac_map_free_with_drop_vec`) only
    /// knows the Vec/String `{ptr, len, cap}` layout, so per-K / per-V
    /// refcount decrements have to be open-coded at the cleanup site
    /// against the matching `SharedTypeInfo.heap_type`. Mirrors the
    /// bucket-walk shape in `karac_map_free_with_drop_vec`
    /// (`runtime/src/map.rs`): for each `slot in 0..capacity`, check
    /// `status[slot] == OCCUPIED`, then load the half's pointer from
    /// `kv[slot*stride + offset]` (`offset = 0` for key, `key_size`
    /// for val) and rc_dec it.
    ///
    /// **Layout dependence.** Reads `capacity`, `status`, `kv`,
    /// `key_size`, `val_size` from the runtime's `#[repr(C)]`
    /// `KaracMap` at the offsets pinned by the runtime-side
    /// `karac_map_field_offsets_match_codegen` unit test. `key_size`
    /// and `val_size` are loaded at runtime (not const-folded from
    /// K/V LLVM widths) so the walk stays agnostic of K's / V's
    /// exact representation — the `kv` byte array's stride is
    /// `(key_size + val_size)` bytes, with the val half starting
    /// at `+key_size` and the key half at `+0`.
    ///
    /// **Concurrency.** The walk uses `emit_rc_dec` (non-atomic)
    /// rather than `emit_arc_dec`. Maps are local to a single thread
    /// (`unsafe impl Send for KaracMap`), and the cleanup runs on
    /// the thread that owns the Map, so non-atomic is correct here.
    /// If a future change shares Maps across threads via Arc, this
    /// callsite needs the atomic dispatch — same shape as the
    /// `emit_refcount_dec` decision in `RcDec` cleanup, but the
    /// map's keys / values aren't named bindings, so the
    /// `is_arc_binding` check has no anchor; an explicit `is_arc`
    /// flag on `FreeMapHandle` would be the path then.
    pub(super) fn emit_map_shared_half_rc_dec_walk(
        &self,
        map_handle: PointerValue<'ctx>,
        heap_type: StructType<'ctx>,
        is_val: bool,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_t = self.context.i8_type();
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        // Runtime layout offsets (pinned by
        // `karac_map_field_offsets_match_codegen`):
        //   0..8   status   *u8
        //   8..16  kv       *u8
        //   16..24 capacity usize
        //   24..32 len      usize
        //   32..40 tombstones usize
        //   40..48 key_size usize
        //   48..56 val_size usize
        const STATUS_OFFSET: u64 = 0;
        const KV_OFFSET: u64 = 8;
        const CAPACITY_OFFSET: u64 = 16;
        const KEY_SIZE_OFFSET: u64 = 40;
        const VAL_SIZE_OFFSET: u64 = 48;
        const BUCKET_OCCUPIED: u64 = 1;

        // Null guard — the registration site stores a fresh
        // `karac_map_new` handle which is non-null, but defensive
        // null-skip matches the runtime helper's first check
        // (`if map.is_null() { return; }`) so the cleanup is
        // robust against any future code path that might leave
        // the alloca uninitialized.
        let is_null = self
            .builder
            .build_is_null(map_handle, "cleanup.map.shared.is_null")
            .unwrap();
        let null_skip_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.null.skip");
        let walk_entry_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.walk.entry");
        self.builder
            .build_conditional_branch(is_null, null_skip_bb, walk_entry_bb)
            .unwrap();

        // ── walk.entry: load capacity, status, kv, key_size ─────
        self.builder.position_at_end(walk_entry_bb);
        let cap_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(CAPACITY_OFFSET, false)],
                    "cleanup.map.shared.cap.p",
                )
                .unwrap()
        };
        let capacity = self
            .builder
            .build_load(i64_t, cap_p, "cleanup.map.shared.cap")
            .unwrap()
            .into_int_value();
        let status_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(STATUS_OFFSET, false)],
                    "cleanup.map.shared.status.pp",
                )
                .unwrap()
        };
        let status_ptr = self
            .builder
            .build_load(ptr_ty, status_pp, "cleanup.map.shared.status")
            .unwrap()
            .into_pointer_value();
        let kv_pp = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(KV_OFFSET, false)],
                    "cleanup.map.shared.kv.pp",
                )
                .unwrap()
        };
        let kv_ptr = self
            .builder
            .build_load(ptr_ty, kv_pp, "cleanup.map.shared.kv")
            .unwrap()
            .into_pointer_value();
        let key_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(KEY_SIZE_OFFSET, false)],
                    "cleanup.map.shared.ks.p",
                )
                .unwrap()
        };
        let key_size = self
            .builder
            .build_load(i64_t, key_size_p, "cleanup.map.shared.ks")
            .unwrap()
            .into_int_value();
        let val_size_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    map_handle,
                    &[i64_t.const_int(VAL_SIZE_OFFSET, false)],
                    "cleanup.map.shared.vs.p",
                )
                .unwrap()
        };
        let val_size = self
            .builder
            .build_load(i64_t, val_size_p, "cleanup.map.shared.vs")
            .unwrap()
            .into_int_value();
        let stride = self
            .builder
            .build_int_add(key_size, val_size, "cleanup.map.shared.stride")
            .unwrap();

        // Loop counter alloca'd in entry block.
        let counter = self.create_entry_alloca(fn_val, "cleanup.map.shared.i", i64_t.into());
        self.builder
            .build_store(counter, i64_t.const_zero())
            .unwrap();

        let cond_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.cond");
        let body_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.body");
        let occupied_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.occupied");
        let next_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.next");
        let exit_bb = self
            .context
            .append_basic_block(fn_val, "cleanup.map.shared.loop.exit");

        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // ── loop.cond: i < capacity? ──────────────────────────────
        self.builder.position_at_end(cond_bb);
        let i_val = self
            .builder
            .build_load(i64_t, counter, "cleanup.map.shared.i.cur")
            .unwrap()
            .into_int_value();
        let cont = self
            .builder
            .build_int_compare(
                IntPredicate::ULT,
                i_val,
                capacity,
                "cleanup.map.shared.cont",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(cont, body_bb, exit_bb)
            .unwrap();

        // ── loop.body: load status[i], occupied? ──────────────────
        self.builder.position_at_end(body_bb);
        let status_slot_p = unsafe {
            self.builder
                .build_in_bounds_gep(
                    i8_t,
                    status_ptr,
                    &[i_val],
                    "cleanup.map.shared.status.slot.p",
                )
                .unwrap()
        };
        let status_byte = self
            .builder
            .build_load(i8_t, status_slot_p, "cleanup.map.shared.status.byte")
            .unwrap()
            .into_int_value();
        let is_occupied = self
            .builder
            .build_int_compare(
                IntPredicate::EQ,
                status_byte,
                i8_t.const_int(BUCKET_OCCUPIED, false),
                "cleanup.map.shared.is_occupied",
            )
            .unwrap();
        self.builder
            .build_conditional_branch(is_occupied, occupied_bb, next_bb)
            .unwrap();

        // ── loop.occupied: rc_dec value pointer ───────────────────
        self.builder.position_at_end(occupied_bb);
        let slot_off = self
            .builder
            .build_int_mul(i_val, stride, "cleanup.map.shared.slot.off")
            .unwrap();
        let slot_kv_p = unsafe {
            self.builder
                .build_in_bounds_gep(i8_t, kv_ptr, &[slot_off], "cleanup.map.shared.slot.kv.p")
                .unwrap()
        };
        // Key half lives at offset 0 within the bucket (`slot_kv_p`);
        // value half lives at `+key_size`. Both are pointer-sized on
        // shared types (rc-managed heap-pointer values are 8 bytes
        // on 64-bit).
        let half_ptr_p = if is_val {
            unsafe {
                self.builder
                    .build_in_bounds_gep(
                        i8_t,
                        slot_kv_p,
                        &[key_size],
                        "cleanup.map.shared.slot.val.p",
                    )
                    .unwrap()
            }
        } else {
            slot_kv_p
        };
        let half_ptr = self
            .builder
            .build_load(
                ptr_ty,
                half_ptr_p,
                if is_val {
                    "cleanup.map.shared.val.ptr"
                } else {
                    "cleanup.map.shared.key.ptr"
                },
            )
            .unwrap()
            .into_pointer_value();
        // by-type: a `Map[K, par V]` value half holds a `par` handle that may
        // still be live in another task, so its dec must be atomic.
        self.emit_refcount_dec_by_type(heap_type, half_ptr);
        self.builder.build_unconditional_branch(next_bb).unwrap();

        // ── loop.next: i++, branch back to cond ──────────────────
        self.builder.position_at_end(next_bb);
        let i_next = self
            .builder
            .build_int_add(
                i_val,
                i64_t.const_int(1, false),
                "cleanup.map.shared.i.next",
            )
            .unwrap();
        self.builder.build_store(counter, i_next).unwrap();
        self.builder.build_unconditional_branch(cond_bb).unwrap();

        // ── loop.exit: fall through to null.skip via uncond jump ─
        self.builder.position_at_end(exit_bb);
        self.builder
            .build_unconditional_branch(null_skip_bb)
            .unwrap();

        // Continuation point — both the null-guard and the loop
        // funnel here so the caller can continue emitting the
        // `karac_map_free*` runtime call after this helper returns.
        self.builder.position_at_end(null_skip_bb);
    }

    // ── F-string helpers ──────────────────────────────────────────

    /// Append `src_len` bytes from `src_ptr` to the String (Vec<u8>) alloca at
    /// `dest_alloca`, growing the buffer if necessary.  Mirrors the inline
    /// `push_str` logic in `compile_vec_method`.
    pub(super) fn emit_string_append_raw(
        &mut self,
        dest_alloca: PointerValue<'ctx>,
        src_ptr: PointerValue<'ctx>,
        src_len: inkwell::values::IntValue<'ctx>,
    ) {
        let vec_ty = self.vec_struct_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let fn_val = self.current_fn.unwrap();

        let data_ptr_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 0, "fsa.data.pp")
            .unwrap();
        let len_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 1, "fsa.len.ptr")
            .unwrap();
        let cap_ptr = self
            .builder
            .build_struct_gep(vec_ty, dest_alloca, 2, "fsa.cap.ptr")
            .unwrap();

        let data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "fsa.data")
            .unwrap()
            .into_pointer_value();
        let len = self
            .builder
            .build_load(i64_t, len_ptr, "fsa.len")
            .unwrap()
            .into_int_value();
        let cap = self
            .builder
            .build_load(i64_t, cap_ptr, "fsa.cap")
            .unwrap()
            .into_int_value();

        let new_len = self
            .builder
            .build_int_add(len, src_len, "fsa.new_len")
            .unwrap();

        // Grow if new_len > cap.
        let grow_bb = self.context.append_basic_block(fn_val, "fsa.grow");
        let copy_bb = self.context.append_basic_block(fn_val, "fsa.copy");
        let needs_grow = self
            .builder
            .build_int_compare(IntPredicate::UGT, new_len, cap, "fsa.needs_grow")
            .unwrap();
        self.builder
            .build_conditional_branch(needs_grow, grow_bb, copy_bb)
            .unwrap();

        // Grow path: compute new_cap, malloc, memcpy old data, free old, update alloca.
        self.builder.position_at_end(grow_bb);
        let two = i64_t.const_int(2, false);
        let four = i64_t.const_int(4, false);
        let doubled = self.builder.build_int_mul(cap, two, "fsa.doubled").unwrap();
        let cmp1 = self
            .builder
            .build_int_compare(IntPredicate::UGT, doubled, four, "fsa.cmp1")
            .unwrap();
        let growth_min = self
            .builder
            .build_select(cmp1, doubled, four, "fsa.gmin")
            .unwrap()
            .into_int_value();
        let cmp2 = self
            .builder
            .build_int_compare(IntPredicate::UGT, new_len, growth_min, "fsa.cmp2")
            .unwrap();
        let new_cap = self
            .builder
            .build_select(cmp2, new_len, growth_min, "fsa.new_cap")
            .unwrap()
            .into_int_value();
        let new_buf = self
            .builder
            .build_call(self.malloc_fn, &[new_cap.into()], "fsa.new_buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Copy existing data into new buffer (memcpy with len=0 is safe per C spec).
        self.builder.build_memcpy(new_buf, 1, data, 1, len).unwrap();
        // Free old heap buffer (karac_free_buf(null) is a no-op, matching
        // free(null) per C spec). String — old cap IS the byte count.
        self.emit_free_buf_call(data, cap, 1);
        // Update data pointer and cap in the alloca.
        self.builder.build_store(data_ptr_ptr, new_buf).unwrap();
        self.builder.build_store(cap_ptr, new_cap).unwrap();
        self.builder.build_unconditional_branch(copy_bb).unwrap();

        // Copy path: reload cur data (updated by grow, or unchanged), memcpy src.
        self.builder.position_at_end(copy_bb);
        let cur_data = self
            .builder
            .build_load(ptr_ty, data_ptr_ptr, "fsa.cur_data")
            .unwrap()
            .into_pointer_value();
        let i8_ty = self.context.i8_type();
        let dest = unsafe {
            self.builder
                .build_gep(i8_ty, cur_data, &[len], "fsa.dest")
                .unwrap()
        };
        self.builder
            .build_memcpy(dest, 1, src_ptr, 1, src_len)
            .unwrap();
        self.builder.build_store(len_ptr, new_len).unwrap();
    }

    /// Render one f-string interpolation part to `(ptr, len)`. A part whose
    /// static type is a user `Display` struct is rendered via its
    /// declaration-order Display (`compile_struct_display_string`); the
    /// resulting String's buffer is already registered for scope-exit cleanup
    /// by the inner interpolation, so extracting its `(data, len)` is safe.
    /// `char` parts render as a glyph; everything else uses the primitive /
    /// String path.
    pub(super) fn fstr_render_part(
        &mut self,
        e: &Expr,
        spec: Option<&str>,
    ) -> Result<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>), String> {
        // A format specifier `{e:spec}` routes to the spec-aware scalar renderer
        // (typecheck restricts specs to int / float / string, so the user-Display
        // / struct / enum / collection early-returns below never apply). Same
        // `crate::format_spec` result as the interpreter, so run == build.
        if let Some(spec_raw) = spec {
            if let Ok(fs) = crate::format_spec::FormatSpec::parse(spec_raw) {
                return self.compile_fstr_part_spec(e, &fs);
            }
        }
        // A user `impl Display` (a compiled `<Type>.to_string`) wins over the
        // built-in renderers below: render via the user method through the
        // unified method-call path (the `to_string` arm there falls through to
        // the user fn). Store the owned result in a scope-tracked alloca so its
        // heap buffer survives the outer f-string's memcpy and is freed once at
        // scope exit (mirrors the payload-enum / collection arms). GAP-W4.
        if self.user_display_impl_type(e).is_some() {
            let sval = self
                .compile_method_call(e, "to_string", &[], &e.span, &e.span)?
                .into_struct_value();
            let acc = self.create_entry_alloca(
                self.current_fn.unwrap(),
                "fstr.ud.acc",
                sval.get_type().into(),
            );
            self.builder.build_store(acc, sval).unwrap();
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let data = self
                .builder
                .build_extract_value(sval, 0, "fstr.ud.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(sval, 1, "fstr.ud.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        if let Some(sname) = self.expr_user_struct_name(e) {
            let s = self
                .compile_struct_display_string(e, &sname)?
                .into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "fstr.s.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "fstr.s.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        // All-unit enum interpolation part → variant-name (ptr, len) directly.
        if let Some(ename) = self.expr_user_enum_name(e) {
            return self.compile_unit_enum_display(e, &ename);
        }
        // Payload-bearing user enum interpolation part → render via its
        // value-driven Display fn. Scope-track the rendered buffer so it
        // survives the outer f-string's memcpy and is freed once at scope exit
        // (mirrors the collection arm below).
        if let Some(ename) = self.expr_user_enum_name_any(e) {
            let (acc, sval) = self.render_user_enum_display(e, &ename)?;
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let s = sval.into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "fstr.e.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "fstr.e.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        // Collection (Vec/Map/Set) interpolation part → render via its Display
        // fn. Must precede the compile_fstr_part_to_cstr fallback: a Vec value
        // shares String's `{ptr,len,cap}` layout, so the fallback would
        // mis-read it as a String (the silent-empty `f"{vec}"` defect). The
        // rendered buffer is scope-tracked so it survives the outer f-string's
        // memcpy and is freed once at scope exit.
        if let Some((acc, sval)) = self.try_compile_collection_display(e)? {
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let s = sval.into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "fstr.c.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "fstr.c.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        // Option/Result *call result* interpolation (`f"{cache.get(1)}"`) — the
        // variable case is caught by `try_compile_collection_display` above; this
        // handles the no-variable-name expr via the span-keyed payload table.
        // B-2026-07-08-9 (call-result half). Same scope-tracking as collections.
        if let Some((acc, sval)) = self.try_compile_option_result_display(e)? {
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let s = sval.into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "fstr.or.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "fstr.or.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        // Whole-tuple interpolation (`f"{t}"` where `t: (i64, i64)`) → render via
        // the element-wise tuple Display fn, `(a, b)`-formatted to match the
        // interpreter. Must precede the struct-value fallback / error arms below,
        // which would otherwise mis-handle the anonymous tuple aggregate
        // (B-2026-07-18-14). The rendered buffer is scope-tracked so it survives
        // the outer f-string's memcpy and frees once at scope exit.
        if let Some((acc, sval)) = self.try_compile_tuple_display(e)? {
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let s = sval.into_struct_value();
            let data = self
                .builder
                .build_extract_value(s, 0, "fstr.tup.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(s, 1, "fstr.tup.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        let is_char = self.expr_is_char(e);
        let val = self.compile_expr(e)?;
        if is_char {
            return Ok(self.emit_codepoint_to_utf8(val.into_int_value()));
        }
        // A String-typed part that is a FRESH owned heap temp — a fn/method
        // call returning `String` (`f"{obj.describe()}"`) or a `String[a..b]`
        // slice — owns its buffer, which the f-string append COPIES into the
        // accumulator, leaving the temp's buffer unreferenced. Without
        // scope-tracking it leaks once per interpolation and scales with the
        // count (`f"{a()} {b()}"` leaks twice) — B-2026-07-15-12. Store it in a
        // tracked alloca (exactly like the user-Display / enum / collection
        // arms above) so the buffer is freed once at scope exit. A PLACE-expr
        // String (identifier / field) is owned elsewhere and must NOT be
        // tracked here — its owner frees it, and a second free double-frees —
        // so this gates strictly on the fresh-owned-temp predicates.
        if val.is_struct_value()
            && self.llvm_ty_is_vec_struct(val.into_struct_value().get_type().into())
            && (self.expr_yields_fresh_owned_temp(e) || self.expr_is_fresh_owned_string_slice(e))
        {
            let sv = val.into_struct_value();
            let acc = self.create_entry_alloca(
                self.current_fn.unwrap(),
                "fstr.str.acc",
                sv.get_type().into(),
            );
            self.builder.build_store(acc, sv).unwrap();
            let u8_ty: inkwell::types::BasicTypeEnum<'ctx> = self.context.i8_type().into();
            self.track_vec_var(acc, Some(u8_ty));
            let data = self
                .builder
                .build_extract_value(sv, 0, "fstr.str.data")
                .unwrap()
                .into_pointer_value();
            let len = self
                .builder
                .build_extract_value(sv, 1, "fstr.str.len")
                .unwrap()
                .into_int_value();
            return Ok((data, len));
        }
        // A struct value that isn't the String `{ptr,i64,i64}` layout is a
        // user struct in a non-place interpolation position (`f"{make()}"`);
        // the place-expr struct path above didn't catch it. `compile_fstr_part_to_cstr`
        // would mis-read it as a String and ICE — emit a clean error instead.
        if val.is_struct_value()
            && !self.llvm_ty_is_vec_struct(val.into_struct_value().get_type().into())
        {
            return Err(
                "Display of a struct in an f-string is supported when the interpolated \
                 expression is a variable or field access (e.g. `f\"{x}\"`); bind a struct \
                 literal or call result to a `let` first (user-struct Display, subtask-5 \
                 follow-on)"
                    .to_string(),
            );
        }
        Ok(self.compile_fstr_part_to_cstr(val, e))
    }

    /// Convert a compiled value to `(raw_ptr, byte_len)` for f-string interpolation.
    /// Dispatches on the LLVM type so callers don't need to track the Kāra type name.
    ///
    /// - `String` (3-field struct) → extract (data_ptr, len)
    /// - `bool` (i1) → global "true"/"false" literal
    /// - float (f32/f64) → snprintf "%g" into a 64-byte stack buffer
    /// - integer → snprintf "%lld" / "%llu" into a 64-byte stack buffer
    ///
    /// `source_expr` carries the originating Kāra expression so the integer
    /// arm can pick signed/unsigned widening via `expr_is_unsigned_int` —
    /// mirrors the fix in `compile_print` (2026-05-19). Pre-fix this arm
    /// passed narrow ints (e.g. `i32`) raw to `%lld`, which printf reads as
    /// 64 bits and produces the unsigned reinterpretation on negatives
    /// (`i32 -123` → `4294967173` inside an f-string).
    pub(super) fn compile_fstr_part_to_cstr(
        &mut self,
        val: BasicValueEnum<'ctx>,
        source_expr: &Expr,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let i64_t = self.context.i64_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();

        match val {
            BasicValueEnum::StructValue(sv) => {
                // Treat as String: field 0 = ptr, field 1 = len.
                let ptr = self
                    .builder
                    .build_extract_value(sv, 0, "fst.ptr")
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_extract_value(sv, 1, "fst.len")
                    .unwrap()
                    .into_int_value();
                (ptr, len)
            }
            BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 1 => {
                // bool
                let true_str = self
                    .builder
                    .build_global_string_ptr("true", "fst.true")
                    .unwrap();
                let false_str = self
                    .builder
                    .build_global_string_ptr("false", "fst.false")
                    .unwrap();
                let four = i64_t.const_int(4, false);
                let five = i64_t.const_int(5, false);
                let ptr = self
                    .builder
                    .build_select(
                        iv,
                        true_str.as_pointer_value(),
                        false_str.as_pointer_value(),
                        "fst.bptr",
                    )
                    .unwrap()
                    .into_pointer_value();
                let len = self
                    .builder
                    .build_select(iv, four, five, "fst.blen")
                    .unwrap()
                    .into_int_value();
                (ptr, len)
            }
            _ => {
                // Integer or float: use snprintf into a 64-byte stack buffer.
                // The buffer-size arg fills snprintf's `size_t n` FIXED param,
                // which is i32 on wasm32 (wasi-libc) and i64 natively — match
                // that width or the call mismatches the decl (B-2026-06-14-15).
                let buf_size = if crate::target::active_target_is_wasm() {
                    self.context.i32_type().const_int(64, false)
                } else {
                    i64_t.const_int(64, false)
                };
                let buf = self.create_entry_alloca(
                    fn_val,
                    "fst.buf",
                    self.context.i8_type().array_type(64).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_ty, "fst.buf_ptr")
                    .unwrap();
                let is_float = matches!(val, BasicValueEnum::FloatValue(_));
                // Widen narrower ints to i64 before snprintf's varargs slot —
                // sext for signed, zext for unsigned. Mirrors `compile_print`
                // (control_flow.rs ~258-285): without this, a negative i32 in
                // an f-string renders as its unsigned reinterpretation
                // (`-123` → `4294967173`) because printf reads 64 bits and
                // the high bits are LLVM's zero pad.
                let is_unsigned_int = !is_float && self.expr_is_unsigned_int(source_expr);
                let arg_val: BasicValueEnum<'ctx> = if let BasicValueEnum::IntValue(iv) = val {
                    let bits = iv.get_type().get_bit_width();
                    if bits < 64 {
                        let widened = if is_unsigned_int {
                            self.builder
                                .build_int_z_extend(iv, i64_t, "fst.zext")
                                .unwrap()
                        } else {
                            self.builder
                                .build_int_s_extend(iv, i64_t, "fst.sext")
                                .unwrap()
                        };
                        widened.into()
                    } else {
                        val
                    }
                } else {
                    val
                };
                if is_float {
                    // Shortest-round-trip via the runtime formatter (Rust `{}`),
                    // matching the interpreter — not C `%g`'s 6 significant
                    // figures. Uses its own 384-byte buffer (the 64-byte one
                    // above is for the integer path).
                    return self.format_f64_to_stack_buf(val.into_float_value());
                }
                let fmt_str = if is_unsigned_int {
                    self.builder
                        .build_global_string_ptr("%llu", "fst.fmt_u")
                        .unwrap()
                        .as_pointer_value()
                } else {
                    self.builder
                        .build_global_string_ptr("%lld", "fst.fmt_i")
                        .unwrap()
                        .as_pointer_value()
                };
                let written = self
                    .builder
                    .build_call(
                        self.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            buf_size.into(),
                            fmt_str.into(),
                            arg_val.into(),
                        ],
                        "fst.written",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let len = self
                    .builder
                    .build_int_z_extend(written, i64_t, "fst.len")
                    .unwrap();
                (buf_ptr, len)
            }
        }
    }

    /// Spec-aware sibling of [`Self::compile_fstr_part_to_cstr`]: render `e`
    /// applying the format specifier `fs`. Typecheck restricts specs to int /
    /// float / string holes, and to the printf-expressible subset, so every arm
    /// maps to a `snprintf` conversion that matches `crate::format_spec`'s
    /// `apply_*` (the interpreter path) byte-for-byte. Returns `(ptr, len)`.
    fn compile_fstr_part_spec(
        &mut self,
        e: &Expr,
        fs: &crate::format_spec::FormatSpec,
    ) -> Result<(PointerValue<'ctx>, inkwell::values::IntValue<'ctx>), String> {
        let i64_t = self.context.i64_type();
        let i32_t = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let is_wasm = crate::target::active_target_is_wasm();
        let size_of = |cg: &Self, n: u64| -> BasicValueEnum<'ctx> {
            if is_wasm {
                cg.context.i32_type().const_int(n, false).into()
            } else {
                cg.context.i64_type().const_int(n, false).into()
            }
        };

        let val = self.compile_expr(e)?;
        match val {
            // String hole → width + alignment padding only (typecheck bars
            // radix / precision / zero-pad). No width, or a value already at
            // least `width` wide, needs no work — return the source (ptr, len).
            BasicValueEnum::StructValue(sv) if self.llvm_ty_is_vec_struct(sv.get_type().into()) => {
                let sptr = self
                    .builder
                    .build_extract_value(sv, 0, "fss.ptr")
                    .unwrap()
                    .into_pointer_value();
                let slen = self
                    .builder
                    .build_extract_value(sv, 1, "fss.len")
                    .unwrap()
                    .into_int_value();
                let Some(width) = fs.width else {
                    return Ok((sptr, slen));
                };
                let wconst = i64_t.const_int(width as u64, false);
                let need_pad = self
                    .builder
                    .build_int_compare(inkwell::IntPredicate::ULT, slen, wconst, "fss.needpad")
                    .unwrap();
                let pad_bb = self.context.append_basic_block(fn_val, "fss.pad");
                let nopad_bb = self.context.append_basic_block(fn_val, "fss.nopad");
                let merge_bb = self.context.append_basic_block(fn_val, "fss.merge");
                // Buffer sized to the constant width (+1 NUL); only the pad
                // branch (len < width) writes into it.
                let buf = self.create_entry_alloca(
                    fn_val,
                    "fss.buf",
                    self.context.i8_type().array_type(width as u32 + 1).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_ty, "fss.bufp")
                    .unwrap();
                self.builder
                    .build_conditional_branch(need_pad, pad_bb, nopad_bb)
                    .unwrap();
                // pad: snprintf("%[-]W.*s", (i32)len, ptr) → exactly W bytes.
                // The `.*s` (precision = len) bounds the copy to `len` bytes so
                // the source String need not be NUL-terminated; `to_printf`
                // can't express `.*`, so build the conversion directly.
                self.builder.position_at_end(pad_bb);
                let mut fmt = String::from("%");
                if fs.align == Some(crate::format_spec::Align::Left) {
                    fmt.push('-');
                }
                fmt.push_str(&width.to_string());
                fmt.push_str(".*s");
                let fmt_g = self
                    .builder
                    .build_global_string_ptr(&fmt, "fss.fmt")
                    .unwrap()
                    .as_pointer_value();
                let len_i32 = self
                    .builder
                    .build_int_truncate(slen, i32_t, "fss.leni32")
                    .unwrap();
                self.builder
                    .build_call(
                        self.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            size_of(self, width as u64 + 1).into(),
                            fmt_g.into(),
                            len_i32.into(),
                            sptr.into(),
                        ],
                        "fss.w",
                    )
                    .unwrap();
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let pad_end = self.builder.get_insert_block().unwrap();
                self.builder.position_at_end(nopad_bb);
                self.builder.build_unconditional_branch(merge_bb).unwrap();
                let nopad_end = self.builder.get_insert_block().unwrap();
                self.builder.position_at_end(merge_bb);
                let ptr_phi = self.builder.build_phi(ptr_ty, "fss.ptr.phi").unwrap();
                ptr_phi.add_incoming(&[(&buf_ptr, pad_end), (&sptr, nopad_end)]);
                let len_phi = self.builder.build_phi(i64_t, "fss.len.phi").unwrap();
                len_phi.add_incoming(&[(&wconst, pad_end), (&slen, nopad_end)]);
                Ok((
                    ptr_phi.as_basic_value().into_pointer_value(),
                    len_phi.as_basic_value().into_int_value(),
                ))
            }
            // Numeric holes → one snprintf with the mapped conversion.
            _ => {
                let is_float = matches!(val, BasicValueEnum::FloatValue(_));
                let width = fs.width.unwrap_or(0);
                let cap = std::cmp::max(64u64, width as u64 + 2);
                let buf = self.create_entry_alloca(
                    fn_val,
                    "fss.nbuf",
                    self.context.i8_type().array_type(cap as u32).into(),
                );
                let buf_ptr = self
                    .builder
                    .build_pointer_cast(buf, ptr_ty, "fss.nbufp")
                    .unwrap();
                let (fmt, arg): (String, BasicValueEnum<'ctx>) = if is_float {
                    (fs.to_printf("", 'f', true), val)
                } else {
                    let iv = val.into_int_value();
                    let unsigned = self.expr_is_unsigned_int(e);
                    // Widen to i64 for the varargs slot (sext signed / zext
                    // unsigned) — same as the no-spec path.
                    let widened = if iv.get_type().get_bit_width() < 64 {
                        if unsigned {
                            self.builder
                                .build_int_z_extend(iv, i64_t, "fss.zx")
                                .unwrap()
                        } else {
                            self.builder
                                .build_int_s_extend(iv, i64_t, "fss.sx")
                                .unwrap()
                        }
                    } else {
                        iv
                    };
                    let conv = if fs.radix == crate::format_spec::Radix::Dec {
                        if unsigned {
                            'u'
                        } else {
                            'd'
                        }
                    } else {
                        fs.int_conv()
                    };
                    (fs.to_printf("ll", conv, true), widened.into())
                };
                let fmt_g = self
                    .builder
                    .build_global_string_ptr(&fmt, "fss.nfmt")
                    .unwrap()
                    .as_pointer_value();
                let written = self
                    .builder
                    .build_call(
                        self.snprintf_fn,
                        &[
                            buf_ptr.into(),
                            size_of(self, cap).into(),
                            fmt_g.into(),
                            arg.into(),
                        ],
                        "fss.nw",
                    )
                    .unwrap()
                    .try_as_basic_value()
                    .unwrap_basic()
                    .into_int_value();
                let len = self
                    .builder
                    .build_int_z_extend(written, i64_t, "fss.nlen")
                    .unwrap();
                Ok((buf_ptr, len))
            }
        }
    }

    /// Lazily declare `karac_runtime_f64_to_str(double, ptr, i64) -> i64` —
    /// the runtime helper that renders an `f64` with Rust's shortest-round-trip
    /// `{}` formatting (matching the interpreter), replacing C `printf`'s `%g`.
    pub(super) fn f64_to_str_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_f64_to_str") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let f64_t = self.context.f64_type();
        let fn_ty = i64_t.fn_type(&[f64_t.into(), ptr_t.into(), i64_t.into()], false);
        self.module
            .add_function("karac_runtime_f64_to_str", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_map(wgsl_ptr: ptr, wgsl_len: i64,
    /// in_ptr: ptr, n: i64, elem_size: i64) -> ptr` — the byte-oriented GPU
    /// dispatch entry point (spike slice-0c, `runtime/src/gpu.rs`). Runs the
    /// baked WGSL shader over the `n`-element input buffer (`elem_size` bytes
    /// each) and returns a fresh `malloc`'d output buffer the owned `Vec[T]`
    /// frees. Type-agnostic — `f32`/`i32`/`u32` share this path (the shader's
    /// `array<T>` declares the element type). Lives in the `gpu`-feature
    /// runtime archive only, auto-selected when this symbol is referenced.
    pub(super) fn gpu_map_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_gpu_map") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_ty = ptr_t.fn_type(
            &[
                ptr_t.into(),
                i64_t.into(),
                ptr_t.into(),
                i64_t.into(),
                i64_t.into(),
            ],
            false,
        );
        self.module
            .add_function("karac_runtime_gpu_map", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_dispatch_soa` — CG-4 / GPU-LBM-3's
    /// struct-SoA dispatch entry. Signature `(wgsl_ptr, wgsl_len, n_groups,
    /// in_ptrs, group_strides, n_fields, field_group, field_src, field_dst,
    /// field_size, aos_stride, n) -> aos_ptr`: dispatches over `n_groups`
    /// coalesced group-arrays (each element `group_strides[k]` bytes) and scatters
    /// the outputs into one interleaved AoS buffer field by field. In the
    /// `gpu`-feature archive only, auto-selected via the `karac_runtime_gpu_` prefix.
    pub(super) fn gpu_dispatch_soa_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_gpu_dispatch_soa") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_ty = ptr_t.fn_type(
            &[
                ptr_t.into(), // wgsl_ptr
                i64_t.into(), // wgsl_len
                i64_t.into(), // n_groups
                ptr_t.into(), // in_ptrs
                ptr_t.into(), // group_strides
                i64_t.into(), // n_fields
                ptr_t.into(), // field_group
                ptr_t.into(), // field_src
                ptr_t.into(), // field_dst
                i64_t.into(), // field_size
                i64_t.into(), // aos_stride
                i64_t.into(), // n
                i64_t.into(), // n_uniforms
                ptr_t.into(), // uniform_ptrs
                i64_t.into(), // uniform_size
            ],
            false,
        );
        self.module
            .add_function("karac_runtime_gpu_dispatch_soa", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_upload_soa` (GPU-SLIP-4b) — move a SoA
    /// `Vec[S]` to a resident GPU buffer. Signature `(n_groups, in_ptrs,
    /// group_strides, n) -> handle`: uploads `n_groups` coalesced group-arrays
    /// (`in_ptrs[k]`, each element `group_strides[k]` bytes) and returns an opaque
    /// `u64` handle (never 0). The `gpu.Buffer[S]` value carries this handle.
    pub(super) fn gpu_upload_soa_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_gpu_upload_soa") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_ty = i64_t.fn_type(
            &[
                i64_t.into(), // n_groups
                ptr_t.into(), // in_ptrs
                ptr_t.into(), // group_strides
                i64_t.into(), // n
            ],
            false,
        );
        self.module
            .add_function("karac_runtime_gpu_upload_soa", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_download_soa` (GPU-SLIP-4b) — move a
    /// resident handle back to host AoS + free the handle. Same field-scatter
    /// descriptor scheme as `gpu_dispatch_soa`: `(handle, n_fields, field_group,
    /// field_src, field_dst, field_size, aos_stride, n) -> aos_ptr`.
    pub(super) fn gpu_download_soa_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_gpu_download_soa") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_ty = ptr_t.fn_type(
            &[
                i64_t.into(), // handle
                i64_t.into(), // n_fields
                ptr_t.into(), // field_group
                ptr_t.into(), // field_src
                ptr_t.into(), // field_dst
                i64_t.into(), // field_size
                i64_t.into(), // aos_stride
                i64_t.into(), // n
            ],
            false,
        );
        self.module
            .add_function("karac_runtime_gpu_download_soa", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_free_soa(handle) -> void` (GPU-SLIP-4b) —
    /// the scope-exit drop-glue for a `gpu.Buffer` that leaves scope without being
    /// downloaded. Idempotent (a no-op for a freed/zero handle).
    pub(super) fn gpu_free_soa_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("karac_runtime_gpu_free_soa") {
            return f;
        }
        let i64_t = self.context.i64_type();
        let fn_ty = self.context.void_type().fn_type(&[i64_t.into()], false);
        self.module
            .add_function("karac_runtime_gpu_free_soa", fn_ty, None)
    }

    /// Lazily declare `karac_runtime_gpu_dispatch_resident` (GPU-SLIP-4b-2b) — a
    /// device→device dispatch against a resident input handle, producing a fresh
    /// resident output handle (no host round-trip). Signature `(wgsl_ptr, wgsl_len,
    /// in_handle, n_uniforms, uniform_ptrs, uniform_size) -> out_handle`. Borrows
    /// the input handle (does not free it).
    pub(super) fn gpu_dispatch_resident_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self
            .module
            .get_function("karac_runtime_gpu_dispatch_resident")
        {
            return f;
        }
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_ty = i64_t.fn_type(
            &[
                ptr_t.into(), // wgsl_ptr
                i64_t.into(), // wgsl_len
                i64_t.into(), // in_handle
                i64_t.into(), // n_uniforms
                ptr_t.into(), // uniform_ptrs
                i64_t.into(), // uniform_size
            ],
            false,
        );
        self.module
            .add_function("karac_runtime_gpu_dispatch_resident", fn_ty, None)
    }

    /// Render `fv` (widened to `f64` first — varargs/ABI parity and the
    /// formatter takes a `double`) into a fresh stack buffer via
    /// `karac_runtime_f64_to_str`; returns `(buf_ptr, len_i64)` for the
    /// `%.*s` / append-raw convention. The buffer is 384 bytes — Rust's `{}`
    /// never uses scientific notation, so an extreme `f64` (`1e308`,
    /// `5e-324`) expands to ~320 decimal digits; 384 covers the whole range
    /// without truncation (the interpreter prints the full string too).
    pub(super) fn format_f64_to_stack_buf(
        &mut self,
        fv: FloatValue<'ctx>,
    ) -> (PointerValue<'ctx>, IntValue<'ctx>) {
        let i64_t = self.context.i64_type();
        let ptr_t = self.context.ptr_type(AddressSpace::default());
        let fn_val = self.current_fn.unwrap();
        let v = if fv.get_type() != self.context.f64_type() {
            self.builder
                .build_float_ext(fv, self.context.f64_type(), "f2d")
                .unwrap()
        } else {
            fv
        };
        let buf = self.create_entry_alloca(
            fn_val,
            "fbuf",
            self.context.i8_type().array_type(384).into(),
        );
        let buf_ptr = self
            .builder
            .build_pointer_cast(buf, ptr_t, "fbufp")
            .unwrap();
        let f = self.f64_to_str_fn();
        let len = self
            .builder
            .build_call(
                f,
                &[v.into(), buf_ptr.into(), i64_t.const_int(384, false).into()],
                "f2s",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        (buf_ptr, len)
    }

    /// Build an owning `String` value (`{ data, len, cap }`) holding a fresh
    /// heap copy of `src_len` bytes at `src_ptr`. Mirrors the single-part
    /// f-string lowering: `malloc(max(len, 1))` (cap > 0 keeps the scope-exit
    /// free armed even for an empty string), `memcpy`, then pack the struct.
    /// Used by primitive `x.to_string()`, whose rendered `(ptr, len)` from
    /// `compile_fstr_part_to_cstr` points at a transient stack buffer.
    pub(super) fn build_owned_string_from_parts(
        &mut self,
        src_ptr: PointerValue<'ctx>,
        src_len: inkwell::values::IntValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let i64_t = self.context.i64_type();
        let one = i64_t.const_int(1, false);
        let is_zero = self
            .builder
            .build_int_compare(inkwell::IntPredicate::ULT, src_len, one, "ts.tot.zero")
            .unwrap();
        let alloc_bytes = self
            .builder
            .build_select(is_zero, one, src_len, "ts.alloc")
            .unwrap()
            .into_int_value();
        let buf = self
            .builder
            .build_call(self.malloc_fn, &[alloc_bytes.into()], "ts.buf")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        self.builder
            .build_memcpy(buf, 1, src_ptr, 1, src_len)
            .unwrap();
        let vec_ty = self.vec_struct_type();
        let agg = vec_ty.get_undef();
        let agg = self
            .builder
            .build_insert_value(agg, buf, 0, "ts.data")
            .unwrap();
        let agg = self
            .builder
            .build_insert_value(agg, src_len, 1, "ts.len")
            .unwrap();
        let agg = self
            .builder
            .build_insert_value(agg, alloc_bytes, 2, "ts.cap")
            .unwrap();
        agg.into_struct_value().into()
    }

    /// Encode an i32 codepoint as 1–4 UTF-8 bytes in a 4-byte stack alloca;
    /// return `(buf_ptr, byte_len_i64)`. Used by the print and f-string
    /// char-arms to render a `char` as the glyph rather than the integer
    /// codepoint. Delegates the encoding logic to the runtime helper
    /// `karac_string_encode_char` to keep the lowered IR small (one call
    /// per print, vs. the ~30-instruction inline branch ladder).
    pub(super) fn emit_codepoint_to_utf8(
        &self,
        cp: inkwell::values::IntValue<'ctx>,
    ) -> (PointerValue<'ctx>, inkwell::values::IntValue<'ctx>) {
        let fn_val = self.current_fn.unwrap();
        let i8_t = self.context.i8_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        let buf = self.create_entry_alloca(fn_val, "u8.buf", i8_t.array_type(4).into());
        let buf_ptr = self
            .builder
            .build_pointer_cast(buf, ptr_ty, "u8.buf.ptr")
            .unwrap();
        let len = self
            .builder
            .build_call(
                self.karac_string_encode_char_fn,
                &[cp.into(), buf_ptr.into()],
                "u8.enc",
            )
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        (buf_ptr, len)
    }
}
