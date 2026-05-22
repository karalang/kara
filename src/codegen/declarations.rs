//! Pre-compilation declaration passes.
//!
//! Houses the per-item declaration passes that run before
//! `compile_program` proper: struct LLVM-type construction
//! (`declare_structs`), SOA layout collection (`collect_soa_layouts`)
//! plus its supporting `soa_*` helpers and `aligned_alloc_fn`,
//! enum tagged-union layout construction (`declare_enums`) plus
//! `payload_word_count_for_type_expr`, `llvm_type_word_count`,
//! `seed_builtin_enum_layouts` and `enum_drop_kind_for_type_expr`,
//! and extern-function declarations
//! (`declare_extern_functions`, `declare_one_extern_function`).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::ast::*;

use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum, StructType};
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};
use inkwell::AddressSpace;

use super::state::{EnumDropKind, EnumLayout, SharedTypeInfo, SoaGroup, SoaLayout};

/// Decide whether a shared-struct field's source type is `Option[T]` where
/// `T` is itself a (known) shared struct — the precondition for niche-opt
/// storage of the field as a single nullable pointer instead of the
/// conventional 4-i64 Option enum. Pre-computed at `declare_structs` time
/// from a name-set so self-referential / forward-reference shapes don't
/// trip on declaration order. Returns the inner shared-struct name when
/// eligible, `None` otherwise.
fn niche_inner_name_if_eligible(
    ty: &TypeExpr,
    shared_struct_names: &std::collections::HashSet<String>,
) -> Option<String> {
    let outer_path = match &ty.kind {
        TypeKind::Path(p) => p,
        _ => return None,
    };
    if outer_path.segments.last().map(|s| s.as_str()) != Some("Option") {
        return None;
    }
    let inner_te = outer_path
        .generic_args
        .as_ref()?
        .iter()
        .find_map(|a| match a {
            GenericArg::Type(t) => Some(t),
            _ => None,
        })?;
    let inner_path = match &inner_te.kind {
        TypeKind::Path(p) => p,
        _ => return None,
    };
    let inner_name = inner_path.segments.last()?;
    if shared_struct_names.contains(inner_name) {
        Some(inner_name.clone())
    } else {
        None
    }
}

/// Body-splitting statement classification used by `emit_state_machine_poll_fns`.
///
/// Slice 8h queued only arg-less void free-fn names per arm; slice 8j
/// added self-and-identifier-receiver method calls; slice 8k extends
/// the free-fn variant with a recognised-arg list so calls like
/// `helper(42)` and `helper(x)` (where `x` is a captured local) emit.
/// Each variant carries the minimal data needed to re-emit the call
/// inside the per-arm body.
enum BodySplitStmt {
    /// Slice 8h + 8k: `name(args...)` with each arg in a recognised
    /// shape (slice 8k v1: integer literal or captured-local
    /// identifier). Callee declared as void.
    FreeFnCall { name: String, args: Vec<BodyArg> },
    /// Slice 8j + 8l: `<receiver>.<method>(args...)` with each arg in a
    /// recognised shape (same `BodyArg` set as `FreeFnCall`). Callee
    /// declared as void. `receiver_field` is the state-struct layout
    /// field name to load (`"self"` for impl methods invoked on self,
    /// otherwise the source binding name). `callee_key` is the
    /// `Type.method` symbol name as emitted by the impl-block
    /// declaration pass.
    MethodCall {
        receiver_field: String,
        callee_key: String,
        args: Vec<BodyArg>,
    },
    /// Slice 8m: `let name = <recognised-rhs>` introduced inside an arm
    /// body. The walker accepts simple `PatternKind::Binding(name)`
    /// lets whose RHS is in the `BodyArg` recognised set; emission
    /// allocas a local slot, computes the RHS, stores it, and registers
    /// the binding into the per-arm slot map so subsequent calls in
    /// the same arm can reference it. v1 lowers the slot as `i64` (the
    /// state-struct primitive fallback) — wider / pointer / aggregate
    /// types stay deferred until typed-aware lowering threads
    /// param/let-type information into the walker.
    Let { name: String, rhs: BodyArg },
    /// Slice 8p: `name = <recognised-value>` assignment to an in-scope
    /// binding (captured-local OR arm-local let). The walker accepts
    /// `StmtKind::Assign { target, value }` where `target` is an
    /// `ExprKind::Identifier(name)` with `name` in `current_names` and
    /// `value` matches `BodyArg`. Emission compiles the value and
    /// stores it into the binding's existing slot — does NOT alloca a
    /// new slot. Composes with slice 8n writeback: an assignment to a
    /// captured local in a non-terminal arm is written back to the
    /// state-struct field before the yield, so the post-yield reload
    /// sees the updated value.
    Assign { name: String, value: BodyArg },
}

/// Slice 8k: per-arg shape recognised by the body-splitting walker.
///
/// Each user-source call arg gets classified into one of these shapes,
/// or the whole call is skipped if any arg falls outside the recognised
/// set (method-call args, field accesses, struct literals, etc.). The
/// per-arm emission compiles each variant into a `BasicMetadataValueEnum`
/// for the `build_call` invocation.
enum BodyArg {
    /// Integer literal — emitted as an `i64` constant in v1, matching the
    /// state-struct primitive fallback. Wider integer suffixes / signed-
    /// unsigned distinctions stay on a v1 conservative i64 lowering until
    /// the typechecker's recorded callee param type starts flowing into
    /// the body-splitting walker.
    IntLit(i64),
    /// Captured-local reference — name resolves to a layout field, and
    /// the per-arm slot map has the alloca `PointerValue` + element
    /// `BasicTypeEnum`. Emission performs a `build_load(slot_ty, slot_ptr)`
    /// to pass by value.
    Slot(String),
    /// Slice 8q: arithmetic binary expression where each operand is
    /// itself a recognised `BodyArg`. v1 recognises only the five integer
    /// arithmetic ops (`+` / `-` / `*` / `/` / `%`) over i64 operands,
    /// produced via `build_int_add` / `build_int_sub` / `build_int_mul` /
    /// `build_int_signed_div` / `build_int_signed_rem`. Comparison /
    /// logical / bitwise / float / mixed-width arithmetic stays outside
    /// the recognised set (returns `None` from `recognize_body_arg`) —
    /// follow-on slices widen the recognition as the typed-aware
    /// lowering work proceeds. Unblocks compound-assign (`+=` / `-=`
    /// / `*=` / …) which lowers as `Assign { name, value: Binary { op,
    /// lhs: Slot(name), rhs: <recognised> } }` once the parser surface
    /// for compound-assign reaches the body-splitting walker.
    Binary {
        op: BinaryArithOp,
        lhs: Box<BodyArg>,
        rhs: Box<BodyArg>,
    },
}

/// Slice 8q: the closed integer-arithmetic op set the body-splitting
/// walker accepts inside a `BodyArg::Binary` node. Mirrors the five
/// arms of `BinOp` that lower to LLVM `build_int_*` calls under v1's
/// i64-only assumption. Kept separate from the AST's `BinOp` so the
/// recognition path stays bounded — adding a new arm here forces an
/// explicit recognition+emission pair rather than silently picking up
/// future AST extensions.
#[derive(Copy, Clone)]
enum BinaryArithOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl<'ctx> super::Codegen<'ctx> {
    // ── Struct declaration pass ───────────────────────────────────

    pub(super) fn declare_structs(&mut self, program: &Program) {
        // Pre-pass: collect names of all `shared struct`s in the program.
        // Needed before per-field niche-eligibility check because
        // self-referential / forward-reference shapes (`shared struct Node {
        // next: Option[Node] }`, `shared struct A { ref_b: Option[B] }`
        // where B is declared later) wouldn't find their target in
        // `shared_types` yet — that map is populated in the same loop that
        // emits heap layouts. Niche eligibility is purely a name check,
        // so a name-set built first is sufficient.
        let shared_struct_names: std::collections::HashSet<String> = program
            .items
            .iter()
            .filter_map(|it| match it {
                Item::StructDef(s) if s.is_shared => Some(s.name.clone()),
                _ => None,
            })
            .collect();

        for item in &program.items {
            if let Item::StructDef(s) = item {
                // Per-field niche-eligibility decision (only meaningful for
                // shared structs — owned structs don't get an RC header so
                // there's no heap-layout to compact). A field is niche if
                // its source type is `Option[T]` with T a known shared
                // struct (`shared_struct_names`), in which case the heap
                // slot is a single `ptr` (null = None, non-null = Some)
                // instead of the conventional 4-i64 Option layout. Saves
                // 24 bytes/field — biggest win on small reference-
                // semantics shapes like the ListNode kata.
                let niche_option_fields: Vec<Option<String>> = if s.is_shared {
                    s.fields
                        .iter()
                        .map(|f| niche_inner_name_if_eligible(&f.ty, &shared_struct_names))
                        .collect()
                } else {
                    vec![None; s.fields.len()]
                };
                let ptr_ty = self.context.ptr_type(AddressSpace::default());
                let field_types: Vec<BasicTypeEnum<'ctx>> = s
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        if s.is_shared && niche_option_fields[i].is_some() {
                            // Niche-optimized Option[shared T] — store as ptr.
                            ptr_ty.into()
                        } else {
                            self.llvm_type_for_type_expr(&f.ty)
                        }
                    })
                    .collect();
                let names: Vec<String> = s.fields.iter().map(|f| f.name.clone()).collect();
                // Per-field user-type name (last path segment if the
                // declared type is a `Path`; `None` otherwise). Lets
                // chained field-access lowering resolve the inner type
                // of `o.inner` so `o.inner.name` walks past the first
                // hop into the nested struct's field registry. See
                // `field_index_for` / `type_name_of_expr`.
                let field_type_names: Vec<Option<String>> = s
                    .fields
                    .iter()
                    .map(|f| match &f.ty.kind {
                        TypeKind::Path(p) => p.segments.last().cloned(),
                        _ => None,
                    })
                    .collect();
                self.struct_field_type_names
                    .insert(s.name.clone(), field_type_names);
                // Full per-field TypeExpr for the field-receiver method
                // dispatch path — generic args (`Vec[Node]`) are needed to
                // populate the synth's element-type side tables via
                // `register_var_from_type_expr`.
                let field_type_exprs: Vec<TypeExpr> =
                    s.fields.iter().map(|f| f.ty.clone()).collect();
                self.struct_field_type_exprs
                    .insert(s.name.clone(), field_type_exprs);

                if s.is_shared {
                    // Shared struct: heap layout is { i64 refcount, field0, field1, … }
                    let mut heap_fields: Vec<BasicTypeEnum<'ctx>> =
                        vec![self.context.i64_type().into()]; // refcount
                    heap_fields.extend_from_slice(&field_types);
                    let heap_type = self.context.struct_type(&heap_fields, false);

                    self.shared_types.insert(
                        s.name.clone(),
                        SharedTypeInfo {
                            heap_type,
                            field_names: names.clone(),
                            is_enum: false,
                            niche_option_fields,
                        },
                    );
                    // Also register field names for field-index lookups.
                    self.struct_field_names.insert(s.name.clone(), names);
                } else {
                    let st = self.context.struct_type(&field_types, false);
                    self.struct_types.insert(s.name.clone(), st);
                    self.struct_field_names.insert(s.name.clone(), names);
                }
            }
        }
    }

    // ── Union declaration pass (phase 5 line 569 slice 4) ──────────

    /// FFI union LLVM-type construction. For each `#[repr(C)] union
    /// Foo { f0: T0, f1: T1, ... }` declaration the slice spec (phase
    /// 5 line 569 slice 4) requires the storage type to have
    /// `size = max(field_sizes)` and `align = max(field_aligns)`.
    /// LLVM has no native union type, so we encode it as a struct
    /// holding the field-with-max-alignment (tie-break: largest size)
    /// as its first member, followed by a `[k x i8]` padding tail
    /// when that field's own size is smaller than the union's total
    /// size. The resulting struct's alignment is driven by the
    /// alignment of its first member, which by construction is
    /// `max(field_aligns)`; its `size_of` is `max(field_sizes)`
    /// rounded up to that alignment — exactly the C-union rule.
    ///
    /// Type-checking (slices 1d / 2 / 3) already rejects unions that
    /// lack `#[repr(C)]`, carry non-`Copy` fields, or sit inside an
    /// `impl Drop` block, so this pass operates on a guaranteed-valid
    /// `UnionDef`. Field-access (`u.field` outside `unsafe { }`) and
    /// literal-shape (`Foo { multi: ... }`) violations are filtered
    /// upstream as well; codegen is purely a lowering step.
    pub(super) fn declare_unions(&mut self, program: &Program) {
        for item in &program.items {
            let Item::UnionDef(u) = item else { continue };
            let llvm_fields: Vec<(String, BasicTypeEnum<'ctx>)> = u
                .fields
                .iter()
                .map(|f| (f.name.clone(), self.llvm_type_for_type_expr(&f.ty)))
                .collect();
            // Empty union bodies are rejected at parse time
            // (`E_EMPTY_UNION`, slice 1b); guard defensively so a
            // future relaxation doesn't crash the codegen pass.
            if llvm_fields.is_empty() {
                continue;
            }
            // Scoped target-data borrow: extract per-field (align, size)
            // up front, then drop the borrow so `self.context` /
            // `self.union_types` are usable again below. Without the
            // explicit scope the returned `&TargetData` would live
            // through `self.context.struct_type(...)` and conflict with
            // the disjoint-field borrow checker.
            let metrics: Vec<(u32, u64)> = {
                let target_data = match self.ensure_target_data() {
                    Ok(td) => td,
                    Err(_) => continue,
                };
                llvm_fields
                    .iter()
                    .map(|(_, ty)| {
                        (
                            target_data.get_abi_alignment(ty),
                            target_data.get_abi_size(ty),
                        )
                    })
                    .collect()
            };
            // Pick the field whose alignment is largest (tie-break:
            // largest size, then earliest source position). That
            // field becomes the storage struct's first member so
            // LLVM derives the correct alignment for the aggregate.
            let mut primary_idx = 0usize;
            let mut primary_align = metrics[0].0;
            let mut primary_size = metrics[0].1;
            let mut max_size = primary_size;
            for (i, &(a, s)) in metrics.iter().enumerate().skip(1) {
                if s > max_size {
                    max_size = s;
                }
                let better = a > primary_align || (a == primary_align && s > primary_size);
                if better {
                    primary_idx = i;
                    primary_align = a;
                    primary_size = s;
                }
            }
            let primary_ty = llvm_fields[primary_idx].1;
            let pad_bytes = max_size.saturating_sub(primary_size);
            let storage_ty = if pad_bytes == 0 {
                self.context.struct_type(&[primary_ty], false)
            } else {
                // u64 → u32 narrowing for the array length is safe in
                // practice (no FFI union spans 4GB), and saturates
                // defensively if a pathological size ever appears.
                let pad_len = u32::try_from(pad_bytes).unwrap_or(u32::MAX);
                let pad_ty = self.context.i8_type().array_type(pad_len);
                self.context
                    .struct_type(&[primary_ty, pad_ty.into()], false)
            };
            self.union_types.insert(u.name.clone(), storage_ty);
            self.union_field_types.insert(u.name.clone(), llvm_fields);
        }
    }

    // ── State-struct type emission (phase 6 line 26 slice 5) ──────────

    /// Emit one `%kara.state.<fn_key>` LLVM struct type per entry in
    /// `program.state_struct_layouts` (populated by `Pipeline::effectcheck`
    /// from slice 4's `build_state_struct_layouts`). The struct shape is:
    ///
    /// ```text
    /// %kara.state.<fn_key> = type { i32, <field 0 LLVM type>, <field 1 LLVM type>, ... }
    ///                                ^^^                       ^^^
    ///                                tag = yield-point index   captured local
    /// ```
    ///
    /// Field 0 is always an `i32` tag carrying the yield-point index the
    /// state machine resumes against. Fields 1..=N correspond 1:1 to the
    /// `StateStructLayout.fields` in source-introduction order; each is
    /// sized via `llvm_type_for_name(type_name)` when the typechecker
    /// recorded a surface type name for the binding's pattern span, and
    /// falls back to `i64` for primitive / unrecorded bindings (the same
    /// over-approximation `llvm_type_for_name`'s default arm uses for
    /// unknown type names). Vec / String surface names expand to the
    /// existing `{ptr, i64, i64}` 3-word struct; user-named `shared
    /// struct`s collapse to a pointer-sized handle.
    ///
    /// Must run after `declare_structs` (so user-named struct types are
    /// resolvable through `struct_types`) and after `declare_enums` (for
    /// future entries that may resolve to enum-typed slots). Must run
    /// before any function-body lowering so the slice-6+ state-machine
    /// transform passes can look up the struct type at body-rewrite time.
    pub(super) fn emit_state_struct_types(&mut self, program: &Program) {
        for (fn_key, layout) in &program.state_struct_layouts {
            // Slice 8v: generic (un-monomorphized) functions don't get
            // a base-name state-struct — their field types include
            // unsubstituted type parameters that `llvm_type_for_name`
            // would silently fall through to the i64 default for.
            // Per-mono emission (under the mangled key) lands at
            // `compile_generic_call` time once `type_subst` is active.
            if is_generic_fn_key(program, fn_key) {
                continue;
            }
            self.emit_state_struct_type_for_key(program, fn_key, fn_key, layout);
        }
    }

    /// Slice 8v Phase 2: per-key state-struct LLVM type emission. The
    /// base-name pass (above) iterates `state_struct_layouts` and calls
    /// this with `ast_key == emit_key`. The per-mono path
    /// (`emit_state_machine_helpers_for_mono`) passes the polymorphic
    /// base name as `ast_key` (used for `find_function_ast` to resolve
    /// the return type) and the mangled name as `emit_key` (used in the
    /// LLVM symbol names + the `state_struct_types` / `state_machine_return_types`
    /// map keys). Field types resolve through `llvm_type_for_name` which
    /// honors the active `self.type_subst`, so per-mono callers populate
    /// the subst before calling this helper.
    pub(super) fn emit_state_struct_type_for_key(
        &mut self,
        program: &Program,
        ast_key: &str,
        emit_key: &str,
        layout: &crate::ast::StateStructLayout,
    ) {
        let mut fields: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(1 + layout.fields.len());
        // Tag is i32 — yield-point indices for v1 fit comfortably in
        // 31 bits (designers expect single-digit yields per
        // network-boundary function; the headroom matches the
        // `karac explain` predictability claim from the design spec).
        fields.push(self.context.i32_type().into());
        // Slice 8v Phase 2: for per-mono emission, the polymorphic
        // source's `T`-typed params have `type_name: None` in the
        // layout (the typechecker's `pattern_binding_types`
        // recorder doesn't emit a surface name for type-parameter
        // references). Look up each captured-local field's
        // parameter declaration in `fn_ast.params` and use its
        // `TypeExpr` through `llvm_type_for_type_expr` — which
        // honors the active `type_subst` (e.g. `T → i32` for the
        // i32 mono).
        //
        // Slice 8x extends the `None`-fallback chain with
        // `lookup_let_type_expr`, which walks `fn_ast.body`
        // recursively for a body-level `let`-binding whose pattern
        // binds the requested name. Recovers the let's explicit
        // type annotation when present, falling back to the RHS
        // expression's recoverable type when annotation is absent
        // (v1: bare-identifier RHS resolved through the param /
        // let chains). Closes the gap where `let copy = item;`
        // inside a `T`-typed yielding fn body whose binding is
        // captured across a later yield would have lowered to i64
        // when `T` is non-i64. Resolution order: params first
        // (short-circuits before any body walk), then lets, then
        // the i64 default.
        let fn_ast = find_function_ast(program, ast_key);
        for field in &layout.fields {
            let ty: BasicTypeEnum<'ctx> = match &field.type_name {
                Some(name) => self.llvm_type_for_name(name),
                None => lookup_param_type_expr(fn_ast, &field.name)
                    .or_else(|| lookup_let_type_expr(fn_ast, &field.name))
                    .map(|te| self.llvm_type_for_type_expr(&te))
                    .unwrap_or_else(|| self.context.i64_type().into()),
            };
            fields.push(ty);
        }
        // Phase 6 line 26 slice 8i: append a terminal return-value
        // field when the function's return type is non-unit. v1
        // records `i64` returns only — other types (Vec, struct,
        // user-named, etc.) skip the terminal field and continue
        // to use the unit-return path. The terminal arm of the
        // poll-fn writes a placeholder into this field before
        // Ready; caller-side intercepts load it as the call's
        // return value.
        if let Some(fn_ast) = fn_ast {
            if let Some(ret_te) = &fn_ast.return_type {
                if is_i64_return_type(ret_te) {
                    let i64_ty: BasicTypeEnum<'ctx> = self.context.i64_type().into();
                    fields.push(i64_ty);
                    self.state_machine_return_types
                        .insert(emit_key.to_string(), i64_ty);
                }
            }
        }
        let st = self
            .context
            .opaque_struct_type(&format!("kara.state.{}", emit_key));
        st.set_body(&fields, false);
        // LLVM `print_to_string` elides named types that no module
        // entity references; without an anchor, the slice-5 type
        // would exist in the context but not appear in the IR
        // dump that codegen tests grep against. Emit a private
        // zero-initialized global per state struct to keep the
        // type referenced. Slice 6 (the poll-function body rewrite)
        // will reference the same type from function signatures
        // directly, at which point this anchor can be removed.
        let anchor_name = format!("__kara_state_type_anchor_{}", emit_key);
        let anchor = self.module.add_global(st, None, &anchor_name);
        anchor.set_linkage(Linkage::Private);
        anchor.set_initializer(&st.const_zero());
        self.state_struct_types.insert(emit_key.to_string(), st);
    }

    // ── State-machine poll-function emission (line 26 slice 6) ────────

    /// Emit one stub poll function per entry in
    /// `program.state_struct_layouts` (slice 4 output, slice 5 emitted
    /// the state struct type itself). Each poll function carries the ABI
    /// from line-17 sub-item-2 `KaracParkedTask.poll_fn` — `i8 fn(ptr
    /// state, ptr cancel)` returning the `KaracPollResult` discriminant
    /// (`0=Pending`, `1=Ready`, `2=Err`) — so caller-side allocate-
    /// state-struct-then-invoke-poll work in slice 7+ can wire against
    /// a stable signature without waiting for the full switch-on-tag
    /// transform to land.
    ///
    /// Slice 6's body is a **stub**: loads the yield-point tag from
    /// `state[0]` via a typed GEP into `state_struct_types[fn_key]`
    /// (which keeps the named state-struct type referenced from a real
    /// instruction — the slice-5 anchor global stays in place as
    /// belt-and-suspenders for now), then unconditionally returns
    /// Pending. Subsequent sub-slices replace the unconditional return
    /// with the dispatch switch (one arm per yield point + the entry
    /// state for the first poll), the per-yield-arm captured-locals
    /// reload + actual user-code resume, and the Ready/Err return
    /// paths.
    ///
    /// Must run after `emit_state_struct_types` (the GEP type operand
    /// requires the state struct type to exist). Runs before
    /// `collect_soa_layouts` to slot alongside the other line-26
    /// codegen pieces, though the ordering doesn't matter — the SOA
    /// pass doesn't touch state structs.
    pub(super) fn emit_state_machine_poll_fns(&mut self, program: &Program) {
        // Sort the keys for deterministic emission order — HashMap
        // iteration order is randomized, and we want the IR text to be
        // stable across runs so test grep is reproducible (the existing
        // per-fn IR-grep tests don't depend on ordering, but ASAN /
        // FileCheck-style invariants would).
        let mut keys: Vec<&String> = program.state_struct_layouts.keys().collect();
        keys.sort();
        for fn_key in keys {
            // Slice 8v: skip generic functions — per-mono poll-fn
            // emission at `compile_generic_call` time emits under
            // the mangled key with `type_subst` active.
            if is_generic_fn_key(program, fn_key) {
                continue;
            }
            self.emit_state_machine_poll_fn_for_key(program, fn_key, fn_key);
        }
    }

    /// Slice 8v Phase 2: per-key poll-fn emission. The base-name pass
    /// iterates `state_struct_layouts` and calls this with `ast_key ==
    /// emit_key`. The per-mono path passes the polymorphic base name
    /// as `ast_key` (used to look up the layout, yield-points table,
    /// and function AST for the body-splitting walker) and the mangled
    /// name as `emit_key` (used for the `__kara_poll_<emit_key>`
    /// LLVM symbol name + the state-struct + return-type table
    /// lookups). The body-splitting walker invokes
    /// `self.materialize_body_arg` / `llvm_type_for_name` inside the
    /// per-arm emission; those routines honor `self.type_subst`, so
    /// per-mono callers populate the subst before calling this helper
    /// to get the right concrete LLVM types for `T`-typed locals.
    pub(super) fn emit_state_machine_poll_fn_for_key(
        &mut self,
        program: &Program,
        ast_key: &str,
        emit_key: &str,
    ) {
        let i8_ty = self.context.i8_type();
        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Poll-fn ABI: `i8 fn(ptr state, ptr cancel)`.
        let fn_type = i8_ty.fn_type(
            &[
                BasicMetadataTypeEnum::from(ptr_ty),
                BasicMetadataTypeEnum::from(ptr_ty),
            ],
            false,
        );
        let state_struct = match self.state_struct_types.get(emit_key) {
            Some(st) => *st,
            // Defensive: layout entry without a corresponding LLVM
            // struct type means slice-5 emit didn't run or the key
            // shapes diverged. Skip rather than crash — the test
            // suite will surface the divergence before users do.
            None => return,
        };
        let layout = match program.state_struct_layouts.get(ast_key) {
            Some(l) => l,
            None => return,
        };

        // Slice 8h/8j: build per-arm segments of user-code statements
        // between yield-point spans. For each statement in the user
        // function's body, classify it as either:
        // - a yield-point Call/MethodCall (advances the current
        //   segment index, statement itself isn't emitted — the
        //   state-transition lowering handles it via tag-store +
        //   Pending return),
        // - an emittable void-call statement: slice 8h covers
        //   `name()` (free-fn, no args, void return); slice 8j adds
        //   `<self|name>.method()` (impl method, no args, void
        //   return) where the receiver is `self` or a captured
        //   layout field already reloaded into a slot by slice 8a,
        // - any other shape (let bindings, control flow, non-void
        //   calls, args-bearing calls, non-captured receivers) →
        //   ignored at v1; future slices extend the supported set.
        let yield_points = program
            .yield_points
            .get(ast_key)
            .cloned()
            .unwrap_or_default();
        let layout_names: std::collections::HashSet<String> =
            layout.fields.iter().map(|f| f.name.clone()).collect();
        // Slice 8m: `current_names` extends `layout_names` with
        // arm-local let-introduced bindings as the walker
        // encounters them. Resets to `layout_names` at every yield
        // (each new arm starts with only the slice-8a reload
        // prologue contents visible — arm-local lets from prior
        // arms aren't carried forward without state-struct write-
        // back, which is a follow-on slice).
        let mut current_names = layout_names.clone();
        let mut per_arm_stmts: Vec<Vec<BodySplitStmt>> =
            (0..yield_points.len() + 1).map(|_| Vec::new()).collect();
        // Slice 8o: capture the user's final-expression value when
        // it's a recognised `BodyArg` shape. The terminal-arm
        // emission consults this instead of slice 8i's `i64 0`
        // placeholder, threading the user's actual return value
        // through the state-struct terminal field. Walker fills
        // this AFTER the per-statement loop so the recognition
        // uses the terminal arm's `current_names` (including any
        // arm-local lets introduced in the terminal arm body).
        let mut terminal_return: Option<BodyArg> = None;
        if let Some(fn_ast) = find_function_ast(program, ast_key) {
            let mut cur_arm = 0usize;
            for stmt in &fn_ast.body.stmts {
                // Slice 8m: handle simple `let name = <recognised>`
                // statements between yields. The let is queued for
                // emission (alloca + RHS compute + store) and the
                // binding name is added to `current_names` so
                // subsequent calls in the same arm can reference
                // it. Non-binding-pattern shapes (destructuring,
                // struct patterns, etc.) and unrecognised RHS
                // shapes are silently skipped — same conservative
                // rule as the call-classification arms.
                if let StmtKind::Let { value, pattern, .. } = &stmt.kind {
                    if let PatternKind::Binding(name) = &pattern.kind {
                        if let Some(rhs) = recognize_body_arg(value, &current_names) {
                            per_arm_stmts[cur_arm].push(BodySplitStmt::Let {
                                name: name.clone(),
                                rhs,
                            });
                            current_names.insert(name.clone());
                        }
                    }
                    // Slice 8t: a let RHS may contain nested yield-
                    // point calls (e.g. `let x = some_async();`).
                    // Advance cur_arm by the count so subsequent
                    // stmts land in the correct arm.
                    let nested = count_yields_inside_span(&stmt.span, &yield_points, cur_arm);
                    for _ in 0..nested {
                        cur_arm += 1;
                        current_names = layout_names.clone();
                    }
                    continue;
                }
                // Slice 8p: `name = value` assignment. Walker
                // accepts targets that are bare identifiers
                // already in `current_names` (i.e. captured-local
                // params OR arm-local lets); value must match the
                // recognised `BodyArg` set. Non-identifier targets
                // (field assignments, index assignments) and
                // unrecognised values are silently skipped — same
                // conservative rule.
                if let StmtKind::Assign { target, value } = &stmt.kind {
                    if let ExprKind::Identifier(name) = &target.kind {
                        if current_names.contains(name) {
                            if let Some(body_value) = recognize_body_arg(value, &current_names) {
                                per_arm_stmts[cur_arm].push(BodySplitStmt::Assign {
                                    name: name.clone(),
                                    value: body_value,
                                });
                            }
                        }
                    }
                    // Slice 8t: same nested-yield descent as the
                    // Let branch; an Assign RHS may contain CF or
                    // yield calls.
                    let nested = count_yields_inside_span(&stmt.span, &yield_points, cur_arm);
                    for _ in 0..nested {
                        cur_arm += 1;
                        current_names = layout_names.clone();
                    }
                    continue;
                }
                // Slice 8r: `name OP= value` compound assignment.
                // Desugars to `name = name OP value` so the
                // existing slice-8p Assign emission + slice-8q
                // Binary materialisation handle the actual codegen
                // unchanged. Walker accepts identifier targets in
                // `current_names` (captured-local OR arm-local
                // let) and value shapes in the slice-8q recognised
                // set. CompoundOp arms supported by slice 8r: Add
                // / Sub / Mul / Div / Mod — match the v1
                // arithmetic ops `BinaryArithOp` already lowers to
                // LLVM `build_int_*` calls. Bitwise / shift
                // compound ops (`&=` / `|=` / `^=` / `<<=` /
                // `>>=`) silently drop pending the same
                // recognition extension on the `Binary` side.
                if let StmtKind::CompoundAssign { target, op, value } = &stmt.kind {
                    if let ExprKind::Identifier(name) = &target.kind {
                        if current_names.contains(name) {
                            let arith_op = match op {
                                CompoundOp::Add => Some(BinaryArithOp::Add),
                                CompoundOp::Sub => Some(BinaryArithOp::Sub),
                                CompoundOp::Mul => Some(BinaryArithOp::Mul),
                                CompoundOp::Div => Some(BinaryArithOp::Div),
                                CompoundOp::Mod => Some(BinaryArithOp::Mod),
                                CompoundOp::BitAnd
                                | CompoundOp::BitOr
                                | CompoundOp::BitXor
                                | CompoundOp::Shl
                                | CompoundOp::Shr => None,
                            };
                            if let (Some(arith_op), Some(rhs)) =
                                (arith_op, recognize_body_arg(value, &current_names))
                            {
                                let lhs = BodyArg::Slot(name.clone());
                                per_arm_stmts[cur_arm].push(BodySplitStmt::Assign {
                                    name: name.clone(),
                                    value: BodyArg::Binary {
                                        op: arith_op,
                                        lhs: Box::new(lhs),
                                        rhs: Box::new(rhs),
                                    },
                                });
                            }
                        }
                    }
                    // Slice 8t: nested-yield descent for the
                    // compound-assign RHS too.
                    let nested = count_yields_inside_span(&stmt.span, &yield_points, cur_arm);
                    for _ in 0..nested {
                        cur_arm += 1;
                        current_names = layout_names.clone();
                    }
                    continue;
                }
                let StmtKind::Expr(expr) = &stmt.kind else {
                    continue;
                };
                // Is this stmt-expr the yield-point call for the
                // next yield? Compare offsets — yield_points are
                // recorded in source order by slice 2's walker.
                if cur_arm < yield_points.len() {
                    let yp_span = &yield_points[cur_arm].span;
                    if expr.span.offset == yp_span.offset && expr.span.length == yp_span.length {
                        cur_arm += 1;
                        // Slice 8m: reset arm-local lets at yield
                        // boundary. layout-captured bindings stay
                        // available via slice-8a's reload prologue;
                        // arm-local lets from the prior arm don't
                        // survive without state-struct write-back.
                        current_names = layout_names.clone();
                        continue;
                    }
                }
                match &expr.kind {
                    // Slice 8h + 8k shape: bare-identifier free-fn
                    // call with zero-or-more args, each arg in a
                    // recognised shape (integer literal or
                    // captured-local identifier reference). Calls
                    // whose args fall outside the recognised set
                    // are silently skipped — the walker's coverage
                    // grows incrementally as arg shapes get
                    // threaded through.
                    ExprKind::Call { callee, args } => {
                        if let ExprKind::Identifier(name) = &callee.kind {
                            let body_args: Option<Vec<BodyArg>> = args
                                .iter()
                                .map(|a| recognize_body_arg(&a.value, &current_names))
                                .collect();
                            if let Some(body_args) = body_args {
                                per_arm_stmts[cur_arm].push(BodySplitStmt::FreeFnCall {
                                    name: name.clone(),
                                    args: body_args,
                                });
                            }
                        }
                    }
                    // Slice 8j + 8l shape: `<recv>.method(args...)`
                    // with zero-or-more recognised args. Receiver
                    // must resolve to a layout field (so slice 8a's
                    // reload prologue has already alloca'd a slot
                    // for it); callee must resolve through
                    // `method_callee_types` to a stable
                    // `Type.method` symbol; each arg must match the
                    // slice-8k `BodyArg` recognised set (any
                    // unrecognised arg → whole call skipped).
                    ExprKind::MethodCall { object, args, .. } => {
                        let receiver_field = match &object.kind {
                            ExprKind::SelfValue => Some("self".to_string()),
                            ExprKind::Identifier(name) if current_names.contains(name.as_str()) => {
                                Some(name.clone())
                            }
                            _ => None,
                        };
                        let callee_key = program
                            .method_callee_types
                            .get(&(expr.span.offset, expr.span.length))
                            .cloned();
                        let body_args: Option<Vec<BodyArg>> = args
                            .iter()
                            .map(|a| recognize_body_arg(&a.value, &current_names))
                            .collect();
                        if let (Some(receiver_field), Some(callee_key), Some(body_args)) =
                            (receiver_field, callee_key, body_args)
                        {
                            per_arm_stmts[cur_arm].push(BodySplitStmt::MethodCall {
                                receiver_field,
                                callee_key,
                                args: body_args,
                            });
                        }
                    }
                    // Slice 8t: stmt-expr is control flow (`if cond
                    // { fetch(); }`, `while !done { fetch(); }`,
                    // `for x in xs { fetch(); }`, `match cond { ...
                    // }`) or another unrecognised shape. Slice 2's
                    // walker already recorded any nested yield-
                    // points inside the CF body; this descent
                    // advances `cur_arm` for each, so post-CF
                    // statements get queued into the correct (post-
                    // yield) arm. The CF expression itself is
                    // dropped from the IR — a follow-on slice
                    // rebuilds it as branching codegen.
                    _ => {
                        let nested = count_yields_inside_span(&stmt.span, &yield_points, cur_arm);
                        for _ in 0..nested {
                            cur_arm += 1;
                            current_names = layout_names.clone();
                        }
                    }
                }
            }
            // Slice 8o: capture the block's trailing expression (if
            // any) as the terminal-arm return value, provided it
            // matches the `BodyArg` recognised set. The walker
            // reaches here after processing all `stmts`, with
            // `current_names` reflecting the terminal arm's scope
            // (layout-captured locals + any terminal-arm let
            // bindings). Non-i64-returning functions still record
            // the value here, but the terminal-arm emission only
            // consults `terminal_return` when
            // `state_machine_return_types` has an entry — so non-
            // i64 returns stay on the unit path until follow-on
            // slices widen the supported set.
            if let Some(final_expr) = fn_ast.body.final_expr.as_deref() {
                terminal_return = recognize_body_arg(final_expr, &current_names);
            }
        }
        let poll_name = format!("__kara_poll_{emit_key}");
        let poll_fn = self.module.add_function(&poll_name, fn_type, None);
        // `Internal` rather than `Private`: both restrict visibility
        // to the current module, but `Internal` is the conventional
        // LLVM choice for codegen-synthesized helpers (the function
        // appears as `define internal i8 @__kara_poll_<fn_key>`),
        // while `Private` is reserved for symbols the linker should
        // strip outright. Caller-side wiring in slice 7+ will load
        // the FunctionValue through the side-table; the symbol need
        // not be link-visible.
        poll_fn.set_linkage(Linkage::Internal);

        // Save outer builder position — slice 6 is invoked before
        // function-body lowering runs, so there's no insert block
        // to save, but the save/restore is cheap and future-proofs
        // against re-ordering.
        let saved_bb = self.builder.get_insert_block();

        let entry = self.context.append_basic_block(poll_fn, "entry");
        self.builder.position_at_end(entry);

        // Typed GEP into the state struct's field 0 (the i32 tag).
        // Keeps the named `%kara.state.<fn_key>` type referenced from
        // a real instruction so LLVM's `print_to_string` retains the
        // type-definition line.
        let state_ptr = poll_fn.get_nth_param(0).unwrap().into_pointer_value();
        let tag_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 0, "tag_ptr")
            .expect("state struct field 0 (tag) GEP must succeed");
        // Load the tag — drives the slice-7 switch dispatch. The
        // GEP + load are the prologue every subsequent sub-slice
        // keeps; slice 7 adds the switch arms; slice 8 fills in
        // per-yield-arm captured-locals reload + user-code resume.
        let tag = self
            .builder
            .build_load(i32_ty, tag_ptr, "tag")
            .expect("load tag from state struct")
            .into_int_value();

        // Slice 7: switch the tag against one arm per (initial-call
        // state + per-yield post-resume state). For N yield points
        // recorded on the function, emit N+1 arms — state 0 is the
        // initial call (before any yield); states 1..=N are the
        // post-yield resume points. The default arm is `unreachable`
        // since the runtime never invokes the poll-fn with an
        // out-of-range tag (the state-struct initializer pins tag=0
        // at allocation time, and tag transitions are codegen-
        // controlled). Slice 6's stub returned Pending
        // unconditionally; slice 7 keeps each arm at Pending so the
        // observable per-arm behavior is unchanged — slice 8 fills
        // the arms with actual resume logic.
        let yield_count = program
            .yield_points
            .get(ast_key)
            .map(|v| v.len())
            .unwrap_or(0);
        let arm_count = yield_count + 1;
        let default_block = self.context.append_basic_block(poll_fn, "tag_unreachable");
        let arm_blocks: Vec<_> = (0..arm_count)
            .map(|i| {
                self.context
                    .append_basic_block(poll_fn, &format!("state_{i}"))
            })
            .collect();
        let cases: Vec<_> = arm_blocks
            .iter()
            .enumerate()
            .map(|(i, bb)| (i32_ty.const_int(i as u64, false), *bb))
            .collect();
        self.builder
            .build_switch(tag, default_block, &cases)
            .expect("build switch on state tag");

        // Slice-8a per-arm body: each state arm emits a uniform
        // reload prologue — for every captured local in the
        // layout, GEP into the corresponding state-struct field
        // (`field_idx + 1` to skip the tag at field 0), load the
        // value, alloca a slot for it, and store the loaded value
        // into the slot. Slice 8b's body-splitting walks these
        // allocas via the existing `variables` registry so the
        // resumed user code references the reloaded values through
        // the same alloca-load machinery as ordinary stack-bound
        // locals.
        //
        // The reload runs uniformly across all state arms (state_0
        // through state_N). For state_0 (initial call) some fields
        // are uninitialized — only the locals live at the entry
        // point (function parameters) carry meaningful data, the
        // rest are zero from the caller-side state-struct
        // allocator. Slice 8b's body-splitting won't reference the
        // not-yet-bound locals at state_0, so the load-of-zero is
        // harmless; uniform per-arm shape simplifies the codegen
        // and matches the over-approximation from slice 4.
        //
        // Each arm still terminates with `ret i8 0` (Pending stub).
        // Slice 8c+ replaces the unconditional return with the
        // actual per-arm logic (run user code until next yield,
        // store captured locals back, return Pending; or at the
        // terminal arm return Ready with the result).
        for (arm_idx, arm_bb) in arm_blocks.iter().enumerate() {
            self.builder.position_at_end(*arm_bb);
            // Slice-8a reload prologue + 8j slot map: walk every
            // captured local and GEP/load/alloca/store it into a
            // local slot. Stash each slot's pointer + element type
            // by field name so slice-8j method-call emission below
            // can re-load the receiver value (or pass the slot
            // pointer directly for ref-self methods).
            let mut slot_map: HashMap<String, (BasicTypeEnum<'ctx>, PointerValue<'ctx>)> =
                HashMap::new();
            for (field_idx, field) in layout.fields.iter().enumerate() {
                let struct_field_idx = (field_idx + 1) as u32;
                let field_ty = state_struct
                    .get_field_type_at_index(struct_field_idx)
                    .expect("state struct field type at captured-local index");
                let field_ptr_name = format!("{}.field_ptr", field.name);
                let field_ptr = self
                    .builder
                    .build_struct_gep(state_struct, state_ptr, struct_field_idx, &field_ptr_name)
                    .expect("GEP captured-local field in state struct");
                let reload_name = format!("{}.reload", field.name);
                let loaded = self
                    .builder
                    .build_load(field_ty, field_ptr, &reload_name)
                    .expect("load captured-local value from state struct");
                let slot_name = format!("{}.slot", field.name);
                let slot = self
                    .builder
                    .build_alloca(field_ty, &slot_name)
                    .expect("alloca for reloaded captured-local slot");
                self.builder
                    .build_store(slot, loaded)
                    .expect("store reloaded captured-local into slot");
                slot_map.insert(field.name.clone(), (field_ty, slot));
            }
            // Slice 8h/8j body-splitting: emit each user-code
            // statement queued for this arm. Slice 8h handles
            // `name()` (free-fn, no args, void return); slice 8j
            // handles `<recv>.method()` (self or captured-receiver,
            // no args, void return) — looks up the Type.method
            // symbol declared by the impl-block pass and threads
            // the reloaded receiver through. Lookups use
            // `module.get_function` against the user-level `@<sym>`
            // shape. Non-void returns are skipped at v1 (the call
            // would need a name binding which adds complexity we'll
            // thread through a later slice).
            if let Some(arm_stmts) = per_arm_stmts.get(arm_idx) {
                for stmt in arm_stmts {
                    match stmt {
                        BodySplitStmt::FreeFnCall { name, args } => {
                            let Some(callee_fn) = self.module.get_function(name) else {
                                continue;
                            };
                            if callee_fn.get_type().get_return_type().is_some() {
                                continue;
                            }
                            // Slice 8k + 8q: compile each recognised
                            // arg into a BasicMetadataValueEnum via
                            // the shared `materialize_body_arg`
                            // helper. Skip the whole call if any arg
                            // can't be materialised (e.g. a slot
                            // lookup failed, or a binary operand
                            // didn't lower to an int value).
                            let mut compiled: Vec<BasicMetadataValueEnum<'ctx>> =
                                Vec::with_capacity(args.len());
                            let mut arg_ok = true;
                            for arg in args {
                                let Some(val) = self.materialize_body_arg(arg, &slot_map, ".arg")
                                else {
                                    arg_ok = false;
                                    break;
                                };
                                compiled.push(val.into());
                            }
                            if !arg_ok {
                                continue;
                            }
                            self.builder
                                .build_call(callee_fn, &compiled, "")
                                .expect("emit slice-8h/8k void user call");
                        }
                        BodySplitStmt::MethodCall {
                            receiver_field,
                            callee_key,
                            args,
                        } => {
                            let Some(callee_fn) = self.module.get_function(callee_key) else {
                                continue;
                            };
                            if callee_fn.get_type().get_return_type().is_some() {
                                continue;
                            }
                            let Some((slot_ty, slot_ptr)) = slot_map.get(receiver_field).copied()
                            else {
                                continue;
                            };
                            // Receiver ABI: mirror compile_method_call's
                            // discipline. If the first param of the
                            // resolved method is pointer-typed, the
                            // method takes `ref self` / `mut ref self` —
                            // pass the slot pointer directly. Otherwise
                            // the method takes owned self — load the
                            // slot's stored value and pass by value.
                            let first_param_is_ptr = callee_fn
                                .get_type()
                                .get_param_types()
                                .first()
                                .map(|t| matches!(t, BasicMetadataTypeEnum::PointerType(_)))
                                .unwrap_or(false);
                            let recv_arg: BasicMetadataValueEnum<'ctx> = if first_param_is_ptr {
                                slot_ptr.into()
                            } else {
                                let loaded = self
                                    .builder
                                    .build_load(
                                        slot_ty,
                                        slot_ptr,
                                        &format!("{receiver_field}.recv"),
                                    )
                                    .expect("load receiver from reloaded slot");
                                loaded.into()
                            };
                            // Slice 8l + 8q: compile method args via
                            // the shared `materialize_body_arg`
                            // helper. The receiver claims arg
                            // position 0; method args follow at
                            // 1..=N matching the source order.
                            let mut compiled: Vec<BasicMetadataValueEnum<'ctx>> =
                                Vec::with_capacity(args.len() + 1);
                            compiled.push(recv_arg);
                            let mut arg_ok = true;
                            for arg in args {
                                let Some(val) = self.materialize_body_arg(arg, &slot_map, ".marg")
                                else {
                                    arg_ok = false;
                                    break;
                                };
                                compiled.push(val.into());
                            }
                            if !arg_ok {
                                continue;
                            }
                            self.builder
                                .build_call(callee_fn, &compiled, "")
                                .expect("emit slice-8j/8l void method call");
                        }
                        BodySplitStmt::Let { name, rhs } => {
                            // Slice 8m + 8q + 8s: arm-local let
                            // binding — materialise the RHS via the
                            // shared helper, alloca a slot whose
                            // type matches the materialised value
                            // (slice 8s: was hardcoded i64 prior;
                            // now derives from `value.get_type()` so
                            // `let v = items` where items is a Vec
                            // captured local alloca's the inline
                            // `{ptr, i64, i64}` shape, not an 8-byte
                            // i64 region that would silently store
                            // 24 bytes of Vec data and produce a
                            // stack-smashing miscompile). Store the
                            // value, then register (slot type, slot
                            // ptr) into slot_map so subsequent
                            // statements in the SAME arm load the
                            // right width when the binding is read.
                            // Across yields the arm-local binding
                            // is not preserved without state-struct
                            // write-back — a follow-on slice for
                            // the capture-then-yield case.
                            let Some(value) = self.materialize_body_arg(rhs, &slot_map, ".let_rhs")
                            else {
                                continue;
                            };
                            let slot_ty = value.get_type();
                            let slot_name = format!("{name}.slot");
                            let slot = self
                                .builder
                                .build_alloca(slot_ty, &slot_name)
                                .expect("alloca for arm-local let slot");
                            self.builder
                                .build_store(slot, value)
                                .expect("store let RHS into slot");
                            slot_map.insert(name.clone(), (slot_ty, slot));
                        }
                        BodySplitStmt::Assign { name, value } => {
                            // Slice 8p + 8q: assignment to an
                            // existing in-scope binding (captured-
                            // local OR arm-local let). Look up the
                            // slot in `slot_map` (DON'T alloca a
                            // new one), materialise the value via
                            // the shared helper, and store. Across
                            // yields: when the target is a captured
                            // local, slice 8n's writeback before
                            // the next yield picks up the new value
                            // and writes it to the state-struct
                            // field so the post-yield reload sees
                            // it.
                            let Some((slot_ty, slot_ptr)) = slot_map.get(name).copied() else {
                                continue;
                            };
                            let Some(new_val) =
                                self.materialize_body_arg(value, &slot_map, ".assign_rhs")
                            else {
                                continue;
                            };
                            self.builder
                                .build_store(slot_ptr, new_val)
                                .expect("store assignment value into slot");
                            // Silence unused — slot_ty currently
                            // not consulted; future typed-aware
                            // lowering will use it to coerce the
                            // const width.
                            let _ = slot_ty;
                        }
                    }
                }
            }
            // Slice-8b state transition: non-terminal arms (the
            // first `yield_count` arms — state_0..state_<N-1>) write
            // the next tag value `arm_idx + 1` into the state
            // struct's field 0 ahead of returning Pending, so the
            // next poll-fn invocation dispatches to the correct
            // resume arm. The terminal arm (state_<N>) returns Ready
            // (`i8 1`) — the function has completed and the caller
            // can observe the result. Slice 8c+ replaces the bare
            // tag-store + Pending sequence with the full yield-site
            // mechanic (suspend the parent task via the scheduler,
            // store back any not-yet-saved captured locals); slice
            // 8d+ replaces the bare Ready return with the actual
            // function-return-value plumbing through the state
            // struct (final field carries the result; caller reads
            // it after observing Ready).
            if arm_idx < yield_count {
                // Slice 8n: write-back captured-locals to state-
                // struct fields before the yield. After slice 8m,
                // a `let x = ...` inside the arm body can shadow a
                // captured-local slot pointer in `slot_map` with a
                // new arm-local alloca; without write-back, the
                // post-yield reload reads the stale state-struct
                // field. Iterating `layout.fields` (the slice-4
                // capture set) and storing the current `slot_map`
                // value into the corresponding state-struct field
                // covers both shadowing and any other in-arm
                // mutation path. For captured locals untouched
                // inside the arm, the write-back is a value-
                // equivalent no-op (slice 8a's reload loaded the
                // field; write-back stores the same value back) —
                // the LLVM optimizer can elide the redundant store.
                for (field_idx, field) in layout.fields.iter().enumerate() {
                    let struct_field_idx = (field_idx + 1) as u32;
                    let Some((slot_ty, slot_ptr)) = slot_map.get(&field.name).copied() else {
                        continue;
                    };
                    let val = self
                        .builder
                        .build_load(slot_ty, slot_ptr, &format!("{}.writeback", field.name))
                        .expect("load slot for state-struct writeback");
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            struct_field_idx,
                            &format!("{}.writeback_field_ptr", field.name),
                        )
                        .expect("GEP state-struct field for writeback");
                    self.builder
                        .build_store(field_ptr, val)
                        .expect("store slot back to state-struct field");
                }
                let next_tag_ptr = self
                    .builder
                    .build_struct_gep(
                        state_struct,
                        state_ptr,
                        0,
                        &format!("state_{arm_idx}.next_tag_ptr"),
                    )
                    .expect("GEP tag field for state transition");
                let next_tag = i32_ty.const_int((arm_idx + 1) as u64, false);
                self.builder
                    .build_store(next_tag_ptr, next_tag)
                    .expect("store next tag for state transition");
                self.builder
                    .build_return(Some(&i8_ty.const_int(0, false)))
                    .expect("return Pending from non-terminal arm");
            } else {
                // Phase 6 line 26 slice 8i: when the network-
                // boundary function has a non-unit return type
                // (recorded in `state_machine_return_types`), the
                // state struct has a terminal field appended after
                // the captured-local fields. Write a placeholder
                // value into the terminal field before the Ready
                // return — body-splitting in a future slice will
                // replace the placeholder with the user's actual
                // return-expression value. The terminal field's
                // index in the state struct is `1 + N` where N is
                // the captured-local count (tag at 0, captures at
                // 1..=N, terminal at N+1).
                if self.state_machine_return_types.contains_key(emit_key) {
                    let terminal_idx = (layout.fields.len() + 1) as u32;
                    let terminal_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            terminal_idx,
                            "kara.return.field_ptr",
                        )
                        .expect("GEP terminal return-value field");
                    // Slice 8o + 8q: when the walker captured the
                    // user's final-expression value via
                    // `terminal_return`, materialise it via the
                    // shared helper (`IntLit` → i64 const, `Slot`
                    // → load from per-arm slot map, `Binary` →
                    // recursive int arithmetic). If
                    // `terminal_return` is `None` (final expr not
                    // recognised, or no trailing expr) or the
                    // helper bails (slot lookup miss, non-IntValue
                    // binary operand), fall back to the slice-8i
                    // `i64 0` placeholder.
                    let return_val: inkwell::values::BasicValueEnum<'ctx> = terminal_return
                        .as_ref()
                        .and_then(|arg| self.materialize_body_arg(arg, &slot_map, ".return"))
                        .unwrap_or_else(|| self.context.i64_type().const_int(0, false).into());
                    self.builder
                        .build_store(terminal_ptr, return_val)
                        .expect("store terminal return value");
                }
                self.builder
                    .build_return(Some(&i8_ty.const_int(1, false)))
                    .expect("return Ready from terminal arm");
            }
        }
        // Default block — unreachable, since the runtime never
        // produces out-of-range tags. The `unreachable` instruction
        // signals to LLVM that this path is impossible, enabling
        // downstream optimizations to drop the default case.
        self.builder.position_at_end(default_block);
        self.builder
            .build_unreachable()
            .expect("unreachable tag default");

        // Restore the outer builder state.
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        self.state_machine_poll_fns
            .insert(emit_key.to_string(), poll_fn);
    }

    // ── State-struct constructor helper (line 26 slice 8c) ─────────────

    /// Emit one constructor helper per network-boundary function:
    /// `define internal ptr @__kara_state_new_<fn_key>()` — a no-arg
    /// function that `malloc`s a fresh state struct of the right size,
    /// initializes the i32 yield-point tag at field 0 to 0 (so the
    /// next poll-fn invocation routes to the entry arm `state_0`),
    /// and returns the heap pointer. Captured-local fields (state
    /// struct fields 1..N) are left uninitialized — slice 8a's reload
    /// prologue will load them, but slice 8b's terminal-vs-non-
    /// terminal arm logic doesn't reference the loaded values, so the
    /// loads of `poison` / `undef` from uninitialized memory are
    /// harmless at this slice. A future slice that adds user-code
    /// lowering between reload and tag-store (slice 8d / 8e) will need
    /// to ensure either:
    /// - the caller initializes the captured-local fields with the
    ///   function's parameters before invoking the poll-fn, or
    /// - the constructor zero-initializes the whole struct via memset.
    ///
    /// Must run after `emit_state_machine_poll_fns` — keeps the
    /// emission of all state-machine helpers grouped, and matches the
    /// alphabetical-by-purpose ordering of slice 6 then 8c.
    pub(super) fn emit_state_machine_state_constructors(&mut self, program: &Program) {
        let mut keys: Vec<&String> = program.state_struct_layouts.keys().collect();
        keys.sort();
        for fn_key in keys {
            // Slice 8v: skip generic functions — per-mono constructor
            // emission lands at `compile_generic_call` time.
            if is_generic_fn_key(program, fn_key) {
                continue;
            }
            self.emit_state_machine_state_constructor_for_key(fn_key);
        }
    }

    /// Slice 8v Phase 2: per-key state-struct constructor emission. The
    /// base-name pass iterates and calls this; the per-mono path passes
    /// the mangled key. Constructor body is fully type-agnostic (only
    /// references the state struct by `state_struct_types[emit_key]`),
    /// so no `ast_key` parameter is needed.
    pub(super) fn emit_state_machine_state_constructor_for_key(&mut self, emit_key: &str) {
        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let ctor_fn_type = ptr_ty.fn_type(&[], false);
        let state_struct = match self.state_struct_types.get(emit_key) {
            Some(st) => *st,
            None => return,
        };
        let ctor_name = format!("__kara_state_new_{emit_key}");
        let ctor_fn = self.module.add_function(&ctor_name, ctor_fn_type, None);
        ctor_fn.set_linkage(Linkage::Internal);

        let saved_bb = self.builder.get_insert_block();
        let entry = self.context.append_basic_block(ctor_fn, "entry");
        self.builder.position_at_end(entry);

        // Compute the size of the state struct via inkwell's
        // `size_of()` helper (which materializes a `ptrtoint` on
        // a constant GEP — the standard LLVM idiom for `sizeof`).
        let size = state_struct
            .size_of()
            .expect("state struct size_of always succeeds for sized types");

        // Call malloc(size) — returns ptr to the fresh heap allocation.
        let malloc_call = self
            .builder
            .build_call(self.malloc_fn, &[size.into()], "state.alloc")
            .expect("call malloc for state struct");
        let state_ptr = malloc_call
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();

        // Initialize the tag (field 0) to 0 — entry state for the
        // poll-fn's switch dispatch. The captured-local fields
        // (1..N) are left uninitialized at this slice.
        let tag_ptr = self
            .builder
            .build_struct_gep(state_struct, state_ptr, 0, "tag_init_ptr")
            .expect("GEP tag field for init");
        self.builder
            .build_store(tag_ptr, i32_ty.const_int(0, false))
            .expect("store tag = 0 init");

        self.builder
            .build_return(Some(&state_ptr))
            .expect("return state pointer from constructor");

        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        self.state_machine_state_constructors
            .insert(emit_key.to_string(), ctor_fn);
    }

    // ── State-struct destructor helper (line 26 slice 8u) ──────────────

    /// Emit one destructor helper per network-boundary function whose
    /// state-struct holds at least one heap-bearing captured-local
    /// field: `define internal void @__kara_state_drop_<fn_key>(ptr
    /// %state)`. The destructor walks the captured-local fields in
    /// source-introduction order (matching `StateStructLayout.fields`)
    /// and emits the per-field drop pattern:
    ///
    /// - `Vec` / `String` / `VecDeque` / `str` field: the `cap > 0 ?
    ///   free(data)` pattern — GEP into the field's `{ptr, len, cap}`
    ///   inline shape, load cap (struct-field index 2), branch on
    ///   `cap > 0`, on the free branch load the data pointer (index 0)
    ///   and call `free(data)`. Identical IR shape to
    ///   `emit_struct_drop_synthesis`'s `FieldDrop::VecOrString` arm
    ///   and `CleanupAction::FreeVecBuffer`'s scope-cleanup walker,
    ///   so optimizer recognition / coverage parity holds across all
    ///   three drop surfaces.
    ///
    /// - User-named `shared struct` field (the field's `type_name`
    ///   resolves through `shared_types`): the field holds a
    ///   pointer-sized handle to the heap-allocated rc-headed
    ///   payload. Load the ptr; null-guard (slice-8a's reload over
    ///   uninitialized state-struct fields may surface zero/poison —
    ///   guard prevents a dec-on-null trap before the use sites are
    ///   wired); on non-null dispatch to `emit_rc_dec` against the
    ///   shared type's `heap_type`. The dec re-uses the existing
    ///   recursive-drop chain (`rc_drop_fns`) so transitively-owned
    ///   refs get released the same way they do for ordinary
    ///   scope-exit cleanup.
    ///
    /// - Any other field (`Option[shared T]`, value-type struct with
    ///   heap fields, enum with heap payload, `Map[K, V]` / `Set[T]`,
    ///   primitives, unrecorded `None` type_name): silently skipped
    ///   at v1. Mirrors the body-splitting walker's conservative
    ///   coverage rule — the destructor's recognized set grows
    ///   incrementally as the analyzer-side machinery threads more
    ///   per-field type info through. Field positions still GEP'd in
    ///   strict source order so a future slice extending the
    ///   recognized set can drop in additional emission arms without
    ///   re-ordering anything.
    ///
    /// Skips emission entirely when none of the captured-local fields
    /// has a heap-bearing type — the destructor would have an
    /// `entry: ret void` body that IR consumers would treat as dead
    /// code anyway. The skip keeps the `state_machine_state_destructors`
    /// map sparse so the future invocation use sites can check
    /// presence as a fast "is there cleanup to do?" predicate without
    /// calling an empty function.
    ///
    /// The state struct's own heap allocation is the **caller's**
    /// responsibility to `free` after invoking the destructor. This
    /// matches the constructor's caller-allocates / caller-frees
    /// discipline (slice 8c): the runtime parked-task release path
    /// already has the state pointer at hand and pairs the destructor
    /// call with the free in one place, rather than baking the free
    /// into every destructor body and forcing the caller to remember
    /// not to free again.
    ///
    /// Cross-coordination with slice 1a (phase 7 line 67 — Result-slot
    /// ABI + Err-triggers-cancel): the future post-yield-arm Err-route
    /// and the per-arm `*cancel == true` check will both branch to an
    /// `err_unwind` BB that invokes this destructor, writes the
    /// terminal-result field's Err sentinel, and returns `i8 2`
    /// (`KaracPollResult::Err`). Slice 8u lands the destructor as the
    /// shared primitive; the two use sites land as follow-on slices
    /// once the yield-call result-slot mechanism / cancel-flag check
    /// are threaded through the body-splitting walker.
    ///
    /// Runs immediately after `emit_state_machine_state_constructors`
    /// so the three state-machine helpers (poll-fn, constructor,
    /// destructor) all land in one orchestration block. Independent
    /// of the user-function declarations — only consults
    /// `state_struct_layouts`, `state_struct_types`, `shared_types`,
    /// and the runtime intrinsic `free_fn` / RC helpers.
    pub(super) fn emit_state_machine_state_destructors(&mut self, program: &Program) {
        let mut keys: Vec<&String> = program.state_struct_layouts.keys().collect();
        keys.sort();
        for fn_key in keys {
            // Slice 8v: skip generic functions — per-mono destructor
            // emission lands at `compile_generic_call` time, with
            // field-type classification using `llvm_type_for_name`
            // against the active `type_subst`.
            if is_generic_fn_key(program, fn_key) {
                continue;
            }
            let layout = program
                .state_struct_layouts
                .get(fn_key)
                .expect("layout exists for sorted key");
            self.emit_state_machine_state_destructor_for_key(program, fn_key, fn_key, layout);
        }
    }

    /// Slice 8v Phase 2 + slice 8w: per-key state-struct destructor
    /// emission. The base-name pass iterates and calls this with
    /// `ast_key == emit_key`; the per-mono path passes the polymorphic
    /// base name as `ast_key` (so the parameter-AST lookup recovers
    /// the polymorphic `TypeExpr` for type-parameter-typed fields)
    /// and the mangled name as `emit_key` (for the LLVM symbol name +
    /// `state_machine_state_destructors` map insertion).
    ///
    /// Field-type classification dispatches on `field.type_name`:
    /// - `Some("Vec" | "VecDeque" | "String" | "str")` →
    ///   `FieldDrop::VecOrString` — emit the `cap > 0 ? free(data)`
    ///   pattern. This is the v1 fast path for fields whose container
    ///   type is recorded directly in `pattern_binding_types` (e.g.
    ///   `let s: String = …;` records `Some("String")`).
    /// - `Some(name)` where `shared_types.contains_key(name)` →
    ///   `FieldDrop::Shared` — emit handle null-guard +
    ///   `emit_refcount_dec` against the shared type's `heap_type`.
    /// - `None` → slice 8w param-type resolution: look up the
    ///   parameter's declared `TypeExpr` via `lookup_param_type_expr`
    ///   (recovered from the polymorphic `fn_ast.params` by binding
    ///   name) and resolve it through `llvm_type_for_type_expr`
    ///   (which honors the active `self.type_subst` so a `T`-typed
    ///   parameter resolves to the concrete LLVM type for the
    ///   current monomorphization). If the resolved LLVM type is the
    ///   Vec struct shape (`{ ptr, i64, i64 }` — same lowering used
    ///   by `String` / `Vec[U]` / `VecDeque[U]`), classify as
    ///   `FieldDrop::VecOrString`; otherwise fall through to
    ///   `FieldDrop::Skip`. The Vec-struct check uses
    ///   `llvm_ty_is_vec_struct` which does a context-uniqued struct
    ///   identity comparison. **Shared-`T` (where `T` resolves to a
    ///   `shared struct N`'s pointer handle) stays as `Skip` in slice
    ///   8w** — `infer_type_args` loses the surface name when it
    ///   binds `T → ptr_type`, so the reverse-lookup needed to recover
    ///   the `shared_types[N].heap_type` for `emit_refcount_dec` would
    ///   need a parallel name-tracking table; deferred as a follow-on
    ///   slice once that infrastructure lands.
    /// - Otherwise → `FieldDrop::Skip` (primitives, ptr, unknown).
    pub(super) fn emit_state_machine_state_destructor_for_key(
        &mut self,
        program: &Program,
        ast_key: &str,
        emit_key: &str,
        layout: &crate::ast::StateStructLayout,
    ) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_t = self.context.i64_type();
        let void_ty = self.context.void_type();
        let vec_ty = self.vec_struct_type();
        let drop_fn_ty = void_ty.fn_type(&[ptr_ty.into()], false);
        let state_struct = match self.state_struct_types.get(emit_key) {
            Some(st) => *st,
            None => return,
        };
        // Classify each captured-local field. The classification is
        // hoisted out of the emission loop so we can decide whether
        // to emit the destructor at all (skip when every field is
        // `Skip`) before mutating the module.
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum FieldDrop {
            Skip,
            VecOrString,
            Shared,
        }
        let fn_ast = find_function_ast(program, ast_key);
        let kinds: Vec<FieldDrop> = layout
            .fields
            .iter()
            .map(|f| match f.type_name.as_deref() {
                Some("Vec") | Some("VecDeque") | Some("String") | Some("str") => {
                    FieldDrop::VecOrString
                }
                Some(name) if self.shared_types.contains_key(name) => FieldDrop::Shared,
                None => {
                    // Slice 8w: type-parameter-typed param recovery.
                    // Resolve the parameter's `TypeExpr` against the
                    // active `type_subst` and classify by LLVM type
                    // identity. Vec-struct shape ({ ptr, i64, i64 })
                    // → VecOrString. Other shapes (incl. ptr-shaped
                    // shared-struct handles) stay Skip in v1.
                    //
                    // Slice 8x: extend the same `None`-fallback chain
                    // with `lookup_let_type_expr` so body-level
                    // generic-typed let-bindings (e.g. `let copy =
                    // item;` inside `fn driver[T](item: T)`) classify
                    // alongside the parameter path — without this the
                    // destructor would leak the `copy` field's heap
                    // buffer on cancel/Err unwinding when `T` resolves
                    // to a Vec-struct-shaped type.
                    lookup_param_type_expr(fn_ast, &f.name)
                        .or_else(|| lookup_let_type_expr(fn_ast, &f.name))
                        .map(|te| self.llvm_type_for_type_expr(&te))
                        .filter(|ty| self.llvm_ty_is_vec_struct(*ty))
                        .map(|_| FieldDrop::VecOrString)
                        .unwrap_or(FieldDrop::Skip)
                }
                Some(name) => {
                    // Slice 8x: an explicit annotation `let copy: T =
                    // item;` causes the typechecker to record `"T"` in
                    // `pattern_binding_types` (the let's
                    // `lower_type_expr(ty_expr, &[])` call passes an
                    // empty generic scope, so `T` is lowered as
                    // `Type::Named { name: "T" }` rather than
                    // `Type::TypeParam`). The shape-bucket arms above
                    // miss this case and the `shared_types` lookup also
                    // miss (T isn't a shared struct), so without this
                    // arm classification would fall through to Skip and
                    // the annotated-let surface would leak heap on
                    // cancel/Err unwinding. Resolve the recorded name
                    // through `llvm_type_for_name` (which already
                    // honors the active `type_subst`); if it resolves
                    // to a Vec-struct shape (`{ ptr, i64, i64 }`),
                    // classify as `VecOrString` — same LLVM-type
                    // identity check the `None` arm uses for the
                    // recovered-`TypeExpr` path. Non-Vec-shaped
                    // resolutions (primitives, pointers, etc.) stay
                    // `Skip`.
                    if self.llvm_ty_is_vec_struct(self.llvm_type_for_name(name)) {
                        FieldDrop::VecOrString
                    } else {
                        FieldDrop::Skip
                    }
                }
            })
            .collect();
        if kinds.iter().all(|k| *k == FieldDrop::Skip) {
            return;
        }

        let drop_name = format!("__kara_state_drop_{emit_key}");
        let drop_fn = self.module.add_function(&drop_name, drop_fn_ty, None);
        drop_fn.set_linkage(Linkage::Internal);

        // `emit_refcount_dec` and friends create basic blocks
        // against `self.current_fn.unwrap()`. Swap to the
        // destructor while emitting; restore at the end so the
        // outer caller's builder state is untouched. Matches
        // `emit_struct_drop_synthesis`'s save / restore
        // discipline.
        let saved_bb = self.builder.get_insert_block();
        let saved_fn = self.current_fn;
        self.current_fn = Some(drop_fn);

        let entry = self.context.append_basic_block(drop_fn, "entry");
        self.builder.position_at_end(entry);
        let state_ptr = drop_fn.get_nth_param(0).unwrap().into_pointer_value();

        for (field_idx, (field, kind)) in layout.fields.iter().zip(kinds.iter()).enumerate() {
            // State-struct field index is `captured_idx + 1` (tag at
            // 0). The strict source-order walk keeps the destructor's
            // field traversal aligned with the layout — useful for
            // both IR-grep tests and the future user-Drop hook that
            // wants source-declaration order for reverse-construction
            // discipline.
            let struct_field_idx = (field_idx + 1) as u32;
            match kind {
                FieldDrop::Skip => {}
                FieldDrop::VecOrString => {
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            struct_field_idx,
                            &format!("{}.drop.field_ptr", field.name),
                        )
                        .expect("GEP captured-local field for destructor");
                    let cap_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            2,
                            &format!("{}.drop.cap_ptr", field.name),
                        )
                        .expect("GEP Vec cap field for destructor");
                    let cap = self
                        .builder
                        .build_load(i64_t, cap_ptr, &format!("{}.drop.cap", field.name))
                        .expect("load Vec cap for destructor")
                        .into_int_value();
                    let zero = i64_t.const_int(0, false);
                    let is_heap = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::UGT,
                            cap,
                            zero,
                            &format!("{}.drop.is_heap", field.name),
                        )
                        .expect("compare cap > 0 for destructor");
                    let free_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{}.drop.free", field.name));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{}.drop.skip", field.name));
                    self.builder
                        .build_conditional_branch(is_heap, free_bb, skip_bb)
                        .expect("branch on cap > 0 for destructor");
                    self.builder.position_at_end(free_bb);
                    let data_ptr_ptr = self
                        .builder
                        .build_struct_gep(
                            vec_ty,
                            field_ptr,
                            0,
                            &format!("{}.drop.data_ptr_ptr", field.name),
                        )
                        .expect("GEP Vec data-ptr field for destructor");
                    let data = self
                        .builder
                        .build_load(ptr_ty, data_ptr_ptr, &format!("{}.drop.data", field.name))
                        .expect("load Vec data ptr for destructor")
                        .into_pointer_value();
                    self.builder
                        .build_call(self.free_fn, &[data.into()], "")
                        .expect("call free for captured-local Vec data");
                    self.builder
                        .build_unconditional_branch(skip_bb)
                        .expect("branch to skip after free");
                    self.builder.position_at_end(skip_bb);
                }
                FieldDrop::Shared => {
                    // The shared-struct field stores a heap handle
                    // (ptr to the rc-headed object). Look up the
                    // heap_type from `shared_types`; load the ptr;
                    // null-guard (slice-8a reload may surface
                    // poison on uninitialized state-struct fields
                    // before the use sites land) then dispatch
                    // through `emit_refcount_dec` so the recursive
                    // drop chain (`rc_drop_fns`) fires on
                    // transitively-owned shared refs the same way
                    // ordinary scope-exit cleanup does. The
                    // shared type name is the field's recorded
                    // `type_name` (guaranteed `Some` here by the
                    // `FieldDrop::Shared` classification arm).
                    let type_name = field
                        .type_name
                        .as_deref()
                        .expect("FieldDrop::Shared implies recorded type_name");
                    let heap_type = self
                        .shared_types
                        .get(type_name)
                        .expect("FieldDrop::Shared implies shared_types entry")
                        .heap_type;
                    let field_ptr = self
                        .builder
                        .build_struct_gep(
                            state_struct,
                            state_ptr,
                            struct_field_idx,
                            &format!("{}.drop.field_ptr", field.name),
                        )
                        .expect("GEP shared captured-local field for destructor");
                    let handle = self
                        .builder
                        .build_load(ptr_ty, field_ptr, &format!("{}.drop.handle", field.name))
                        .expect("load shared captured-local handle for destructor")
                        .into_pointer_value();
                    let null = ptr_ty.const_null();
                    let is_null = self
                        .builder
                        .build_int_compare(
                            inkwell::IntPredicate::EQ,
                            handle,
                            null,
                            &format!("{}.drop.is_null", field.name),
                        )
                        .expect("compare shared handle vs null for destructor");
                    let dec_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{}.drop.dec", field.name));
                    let skip_bb = self
                        .context
                        .append_basic_block(drop_fn, &format!("{}.drop.skip", field.name));
                    self.builder
                        .build_conditional_branch(is_null, skip_bb, dec_bb)
                        .expect("branch on shared handle null-check for destructor");
                    self.builder.position_at_end(dec_bb);
                    self.emit_refcount_dec(&field.name, heap_type, handle);
                    self.builder
                        .build_unconditional_branch(skip_bb)
                        .expect("branch to skip after rc dec");
                    self.builder.position_at_end(skip_bb);
                }
            }
        }

        self.builder
            .build_return(None)
            .expect("ret void from state-struct destructor");

        self.current_fn = saved_fn;
        if let Some(bb) = saved_bb {
            self.builder.position_at_end(bb);
        }

        self.state_machine_state_destructors
            .insert(emit_key.to_string(), drop_fn);
    }

    // ── Slice 8v Phase 2: per-mono state-machine orchestrator ──────────

    /// Emit the four state-machine helpers — state-struct LLVM type,
    /// constructor, destructor, poll-fn — for a polymorphic yielding
    /// function's monomorphization. The polymorphic function's
    /// analysis-side metadata (state-struct layout, yield points, AST
    /// body) lives in `program.state_struct_layouts[base_key]` /
    /// `program.yield_points[base_key]` / `find_function_ast(program,
    /// base_key)`; we access it through the
    /// `Codegen.program_snapshot: Option<Rc<Program>>` cached at the
    /// top of `compile_program`. The emit_key is the mangled
    /// monomorphization name (e.g. `driver$i64`); `compile_generic_call`
    /// passes both names after `compile_mono_function` ran with the
    /// active `type_subst` (so `llvm_type_for_name("T")` inside the
    /// per-key helpers resolves to the concrete LLVM type for this
    /// mono).
    ///
    /// Idempotent: a second call for the same `mono_key` short-
    /// circuits if `state_machine_poll_fns[mono_key]` already exists.
    /// Both callers in `compile_generic_call` guard on
    /// `generated_monos` set membership before invoking this, so
    /// idempotence is belt-and-suspenders, but the check keeps the
    /// helper safe to invoke from future paths that may not gate the
    /// same way.
    ///
    /// No-op when `base_key` isn't in `state_struct_layouts` (the
    /// polymorphic function isn't a network-yield candidate, so no
    /// state-machine machinery is needed for any of its monos). Also
    /// no-op when the program snapshot is missing — defensive against
    /// the helper being invoked outside `compile_program`'s scope.
    pub(super) fn emit_state_machine_helpers_for_mono(&mut self, base_key: &str, mono_key: &str) {
        // Idempotence guard: skip if the per-mono poll-fn (the last
        // helper to be inserted in `emit_state_machine_poll_fn_for_key`)
        // already exists for this `mono_key`. The four helpers all
        // emit-and-insert atomically per call, so the poll-fn's
        // presence implies all four are emitted.
        if self.state_machine_poll_fns.contains_key(mono_key) {
            return;
        }
        let program = match self.program_snapshot.as_ref() {
            Some(p) => Rc::clone(p),
            None => return,
        };
        let layout = match program.state_struct_layouts.get(base_key) {
            Some(l) => l.clone(),
            None => return,
        };
        // Emit the four helpers in declaration-dependency order:
        // (1) state-struct LLVM type (others reference it via
        // `state_struct_types[mono_key]`); (2) constructor +
        // destructor + poll-fn (any order; all read the LLVM type).
        self.emit_state_struct_type_for_key(&program, base_key, mono_key, &layout);
        self.emit_state_machine_state_constructor_for_key(mono_key);
        self.emit_state_machine_state_destructor_for_key(&program, base_key, mono_key, &layout);
        self.emit_state_machine_poll_fn_for_key(&program, base_key, mono_key);
    }

    /// Phase 6 line 26 slice 8q: materialize a `BodyArg` into a
    /// `BasicValueEnum` at the current builder position. v1 lowers
    /// integer arithmetic only: `IntLit` → `i64` constant; `Slot` →
    /// `build_load` from the per-arm slot map; `Binary` → recursive
    /// materialization of both sides + `build_int_*` op. `name_hint`
    /// is the LLVM-name suffix the caller wants on emitted instructions
    /// — slot loads name as `"{slot_name}{name_hint}"` (preserves the
    /// existing `.arg` / `.marg` / `.let_rhs` / `.assign_rhs` / `.return`
    /// shapes from slices 8h-8p), binary results name as
    /// `"binop{name_hint}"`. Returns `None` if any nested `Slot` lookup
    /// fails OR a `Binary` operand resolves to a non-IntValue — the
    /// caller treats `None` as "skip this statement" matching the
    /// conservative-skip discipline of the prior slices.
    fn materialize_body_arg(
        &self,
        arg: &BodyArg,
        slot_map: &HashMap<String, (BasicTypeEnum<'ctx>, PointerValue<'ctx>)>,
        name_hint: &str,
    ) -> Option<inkwell::values::BasicValueEnum<'ctx>> {
        match arg {
            BodyArg::IntLit(v) => Some(self.context.i64_type().const_int(*v as u64, true).into()),
            BodyArg::Slot(slot_name) => {
                let (slot_ty, slot_ptr) = slot_map.get(slot_name).copied()?;
                let load_name = format!("{slot_name}{name_hint}");
                let loaded = self
                    .builder
                    .build_load(slot_ty, slot_ptr, &load_name)
                    .expect("load slot for body-arg materialization");
                Some(loaded)
            }
            BodyArg::Binary { op, lhs, rhs } => {
                let lhs_val = self.materialize_body_arg(lhs, slot_map, name_hint)?;
                let rhs_val = self.materialize_body_arg(rhs, slot_map, name_hint)?;
                // v1 assumes i64 operands — IntLit lowers to i64; Slot
                // loads carry their slot's element type (currently i64
                // across the board since `pattern_binding_types` doesn't
                // record primitives at slice 8q). Non-IntValue operands
                // (e.g. a future Vec / String slot) fall through to
                // `None` so the call site skips emission rather than
                // emitting ill-typed IR.
                let inkwell::values::BasicValueEnum::IntValue(lhs_int) = lhs_val else {
                    return None;
                };
                let inkwell::values::BasicValueEnum::IntValue(rhs_int) = rhs_val else {
                    return None;
                };
                let result_name = format!("binop{name_hint}");
                let result = match op {
                    BinaryArithOp::Add => self
                        .builder
                        .build_int_add(lhs_int, rhs_int, &result_name)
                        .expect("build_int_add for body-arg binary"),
                    BinaryArithOp::Sub => self
                        .builder
                        .build_int_sub(lhs_int, rhs_int, &result_name)
                        .expect("build_int_sub for body-arg binary"),
                    BinaryArithOp::Mul => self
                        .builder
                        .build_int_mul(lhs_int, rhs_int, &result_name)
                        .expect("build_int_mul for body-arg binary"),
                    BinaryArithOp::Div => self
                        .builder
                        .build_int_signed_div(lhs_int, rhs_int, &result_name)
                        .expect("build_int_signed_div for body-arg binary"),
                    BinaryArithOp::Mod => self
                        .builder
                        .build_int_signed_rem(lhs_int, rhs_int, &result_name)
                        .expect("build_int_signed_rem for body-arg binary"),
                };
                Some(result.into())
            }
        }
    }

    pub(super) fn collect_soa_layouts(&mut self, program: &Program) {
        for item in &program.items {
            if let Item::LayoutDef(layout) = item {
                // Extract element struct name from collection type.
                let struct_name = if let TypeKind::Path(path) = &layout.collection_type.kind {
                    if let Some(args) = &path.generic_args {
                        if let Some(GenericArg::Type(te)) = args.first() {
                            if let TypeKind::Path(inner) = &te.kind {
                                inner.segments.first().cloned()
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                let struct_name = match struct_name {
                    Some(n) => n,
                    None => continue,
                };

                // Look up struct field names.
                let all_fields = match self.struct_field_names.get(&struct_name) {
                    Some(f) => f.clone(),
                    None => continue,
                };

                // Build groups from layout items.
                let mut groups = Vec::new();
                let mut cold_group: Option<SoaGroup> = None;
                let mut assigned: HashSet<String> = HashSet::new();

                for li in &layout.items {
                    match li {
                        LayoutItem::Group {
                            name,
                            fields,
                            align,
                            ..
                        } => {
                            let field_indices: Vec<usize> = fields
                                .iter()
                                .filter_map(|f| all_fields.iter().position(|af| af == f))
                                .collect();
                            for f in fields {
                                assigned.insert(f.clone());
                            }
                            groups.push(SoaGroup {
                                name: name.clone(),
                                fields: fields.clone(),
                                field_indices,
                                elem_type: None,
                                align: *align,
                                is_cold: false,
                            });
                        }
                        LayoutItem::Cold { fields, .. } => {
                            let field_indices: Vec<usize> = fields
                                .iter()
                                .filter_map(|f| all_fields.iter().position(|af| af == f))
                                .collect();
                            for f in fields {
                                assigned.insert(f.clone());
                            }
                            cold_group = Some(SoaGroup {
                                name: "__cold".to_string(),
                                fields: fields.clone(),
                                field_indices,
                                elem_type: None,
                                align: None,
                                is_cold: true,
                            });
                        }
                        LayoutItem::SplitByVariant(_) => {}
                    }
                }

                // Implicit trailing hot group for unassigned fields (excludes cold fields).
                let unassigned: Vec<String> = all_fields
                    .iter()
                    .filter(|f| !assigned.contains(*f))
                    .cloned()
                    .collect();
                if !unassigned.is_empty() {
                    let field_indices: Vec<usize> = unassigned
                        .iter()
                        .filter_map(|f| all_fields.iter().position(|af| af == f))
                        .collect();
                    groups.push(SoaGroup {
                        name: "__unassigned".to_string(),
                        fields: unassigned,
                        field_indices,
                        elem_type: None,
                        align: None,
                        is_cold: false,
                    });
                }

                let num_groups = groups.len();
                self.soa_layouts.insert(
                    layout.name.clone(),
                    SoaLayout {
                        name: layout.name.clone(),
                        struct_name,
                        groups,
                        cold_group,
                        num_groups,
                    },
                );
            }
        }
    }

    /// Returns (or lazily declares) `aligned_alloc(i64 alignment, i64 size) -> ptr`.
    /// Used for SoA group allocations with an `align(N)` modifier.
    pub(super) fn aligned_alloc_fn(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("aligned_alloc") {
            return f;
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let fn_ty = ptr_ty.fn_type(&[i64_ty.into(), i64_ty.into()], false);
        self.module
            .add_function("aligned_alloc", fn_ty, Some(Linkage::External))
    }

    /// Build the LLVM struct type for a SoA-laid-out Vec.
    /// Layout: `{ ptr_g0, ..., ptr_gN, [ptr_cold,] i64 len, i64 cap }`.
    /// The cold pointer (if `has_cold` is true) comes after all hot group pointers and before len/cap.
    pub(super) fn soa_vec_type(&self, num_groups: usize, has_cold: bool) -> StructType<'ctx> {
        let ptr_ty: BasicTypeEnum<'ctx> = self.context.ptr_type(AddressSpace::default()).into();
        let i64_ty: BasicTypeEnum<'ctx> = self.context.i64_type().into();
        let num_ptrs = num_groups + if has_cold { 1 } else { 0 };
        let mut fields: Vec<BasicTypeEnum<'ctx>> = vec![ptr_ty; num_ptrs];
        fields.push(i64_ty); // len
        fields.push(i64_ty); // cap
        self.context.struct_type(&fields, false)
    }

    /// Returns the struct field index of the cold pointer within a SoA vec struct.
    /// `num_hot_groups` is the count of hot groups (excluding cold).
    pub(super) fn soa_cold_ptr_index(num_hot_groups: usize) -> u32 {
        num_hot_groups as u32
    }

    /// Returns the struct field index of `len` within a SoA vec struct.
    pub(super) fn soa_len_index(num_hot_groups: usize, has_cold: bool) -> u32 {
        num_hot_groups as u32 + if has_cold { 1 } else { 0 }
    }

    /// Returns the struct field index of `cap` within a SoA vec struct.
    pub(super) fn soa_cap_index(num_hot_groups: usize, has_cold: bool) -> u32 {
        Self::soa_len_index(num_hot_groups, has_cold) + 1
    }

    /// Build the LLVM struct type for one element of a SoA group.
    /// E.g., if group "physics" has fields { position: f64, velocity: f64 },
    /// the group element type is `{ f64, f64 }`.
    pub(super) fn soa_group_elem_type(
        &self,
        struct_name: &str,
        group: &SoaGroup,
    ) -> StructType<'ctx> {
        let struct_field_types: Vec<BasicTypeEnum<'ctx>> =
            if let Some(&st) = self.struct_types.get(struct_name) {
                (0..st.count_fields())
                    .map(|i| st.get_field_type_at_index(i).unwrap())
                    .collect()
            } else {
                Vec::new()
            };

        let group_field_types: Vec<BasicTypeEnum<'ctx>> = group
            .field_indices
            .iter()
            .filter_map(|&idx| struct_field_types.get(idx).copied())
            .collect();

        self.context.struct_type(&group_field_types, false)
    }

    pub(super) fn declare_enums(&mut self, program: &Program) {
        // Phase 1: register names. Sub-step (b) typeof-recursion in
        // `payload_word_count_for_type_expr` looks up nested user types,
        // so we need every enum/struct name registered before we can size
        // any variant. We can't compute layouts in a single pass over
        // `program.items` because variant payload types may reference
        // sibling enums declared further down.
        for item in &program.items {
            if let Item::EnumDef(e) = item {
                let _ = e; // names already collected via declare_structs and the seed pass
            }
        }

        for item in &program.items {
            if let Item::EnumDef(e) = item {
                // CP4 / CP5: compute per-variant per-field word offsets,
                // sized via the recursive helper. The variant's total
                // payload-word count is the last entry's `start + num_words`
                // (or 0 for unit variants); the enum-wide payload-area
                // width is `max(variant_totals)`.
                let mut field_word_offsets: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
                let mut field_drop_kinds: HashMap<String, Vec<EnumDropKind>> = HashMap::new();
                let mut variant_totals: Vec<usize> = Vec::with_capacity(e.variants.len());
                for v in &e.variants {
                    let mut offsets: Vec<(usize, usize)> = Vec::new();
                    let mut drop_kinds: Vec<EnumDropKind> = Vec::new();
                    let mut running: usize = 0;
                    let field_tys: Vec<&TypeExpr> = match &v.kind {
                        VariantKind::Unit => Vec::new(),
                        VariantKind::Tuple(tys) => tys.iter().collect(),
                        VariantKind::Struct(fields) => fields.iter().map(|f| &f.ty).collect(),
                    };
                    for ty in field_tys {
                        let n = self.payload_word_count_for_type_expr(ty, &e.name, &v.name);
                        offsets.push((running, n));
                        drop_kinds.push(self.enum_drop_kind_for_type_expr(ty));
                        running += n;
                    }
                    variant_totals.push(running);
                    field_word_offsets.insert(v.name.clone(), offsets);
                    field_drop_kinds.insert(v.name.clone(), drop_kinds);
                }
                let max_words = variant_totals.iter().copied().max().unwrap_or(0);

                // Build the unified LLVM type: { i64 tag, i64 w0, ..., i64 wN }
                let i64_t: BasicTypeEnum<'ctx> = self.context.i64_type().into();
                let mut field_types: Vec<BasicTypeEnum<'ctx>> = vec![i64_t]; // tag
                for _ in 0..max_words {
                    field_types.push(i64_t);
                }
                let llvm_type = self.context.struct_type(&field_types, false);

                let mut tags = HashMap::new();
                let mut field_counts = HashMap::new();
                for (idx, v) in e.variants.iter().enumerate() {
                    tags.insert(v.name.clone(), idx as u64);
                    let fc = match &v.kind {
                        VariantKind::Unit => 0,
                        VariantKind::Tuple(tys) => tys.len(),
                        VariantKind::Struct(fields) => fields.len(),
                    };
                    field_counts.insert(v.name.clone(), fc);
                }

                if e.is_shared {
                    // Shared enum: heap layout is { i64 refcount, i64 tag, i64 w0, … }
                    let mut heap_fields: Vec<BasicTypeEnum<'ctx>> = vec![i64_t]; // refcount
                    heap_fields.extend_from_slice(&field_types); // tag + payload words
                    let heap_type = self.context.struct_type(&heap_fields, false);

                    self.shared_types.insert(
                        e.name.clone(),
                        SharedTypeInfo {
                            heap_type,
                            field_names: vec![],
                            is_enum: true,
                            // Shared enums don't carry user-named fields the
                            // way shared structs do — the heap layout is
                            // `{ rc, tag, w0, w1, ... }`, not per-field-
                            // typed. Niche-opt applies to shared-struct
                            // Option fields only; leave empty for enums.
                            niche_option_fields: Vec::new(),
                        },
                    );
                }

                // Always register in enum_layouts for tag/variant resolution.
                self.enum_layouts.insert(
                    e.name.clone(),
                    EnumLayout {
                        llvm_type,
                        tags,
                        field_counts,
                        field_word_offsets,
                        field_drop_kinds,
                        is_shared: e.is_shared,
                    },
                );
            }
        }
    }

    /// Compound-payload enum codegen (CP5) — recursive per-field word-count
    /// computation. Returns the number of i64 payload words required to
    /// store a value of `ty` in a variant's payload area.
    ///
    /// Word counts:
    /// - primitives (i8..i64, u8..u64, usize, f32, f64, bool, char, unit): 1
    /// - `String`, `Vec[T]`: 3 (data + len + cap)
    /// - `Slice[T]` / `mut Slice[T]`: 2 (data + len)
    /// - tuple `(T1, T2, …)`: sum of components
    /// - user struct: sum over fields (recursive)
    /// - user enum (nested in another enum's payload): rejected (CP5 carve-out)
    /// - everything else: 1 (conservative; matches `coerce_to_i64` fallback)
    ///
    /// `outer_enum` / `outer_variant` are passed for diagnostic context
    /// when nested enum payloads are rejected.
    pub(super) fn payload_word_count_for_type_expr(
        &self,
        ty: &TypeExpr,
        outer_enum: &str,
        outer_variant: &str,
    ) -> usize {
        match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                match name {
                    // 3-word aggregates: { ptr, i64 len, i64 cap }
                    "String" | "Vec" => 3,
                    // 2-word aggregates: { ptr, i64 len }
                    "Slice" => 2,
                    // Heap-pointer handles; one word.
                    "Map" | "Set" | "SortedSet" => 1,
                    // Single-word primitives.
                    "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
                    | "isize" | "f32" | "f64" | "bool" | "char" | "Unit" => 1,
                    // Other path types: dispatch based on whether it's a
                    // user-defined struct / enum / shared type already
                    // registered. Order matters: shared types (RC pointers)
                    // are 1 word; structs recurse; enum-in-enum payload is
                    // the v1 carve-out and emits an error.
                    _ => {
                        let _ = (outer_enum, outer_variant); // diagnostic context — emitted by typechecker
                        if self.shared_types.contains_key(name) {
                            // RC pointer — single word.
                            1
                        } else if self.enum_layouts.contains_key(name) {
                            // Direct enum-in-enum payload — rejected at v1
                            // (CP5 carve-out) by the typechecker's
                            // E_ENUM_NESTED_ENUM_PAYLOAD diagnostic. If we
                            // reach here, the typecheck stage didn't fail
                            // (or this is an stdlib-baked enum the
                            // typechecker can't see); fall back to a
                            // single i64-payload word so codegen produces
                            // *something* runnable rather than crashing
                            // out of the layout pass.
                            1
                        } else if let Some(struct_ty) = self.struct_types.get(name).copied() {
                            // User struct — sum of LLVM field widths in i64 words.
                            // We can't recurse through TypeExpr here (we lost
                            // it after declare_structs); fall back to LLVM
                            // field count, which is accurate for primitive-
                            // and pointer-typed fields. Aggregate-typed
                            // fields (a struct field that is itself a
                            // String/Vec) inflate by their LLVM struct
                            // arity automatically.
                            Self::llvm_type_word_count(struct_ty.into())
                        } else {
                            // Unknown name (generic type param, builtin not yet
                            // registered, …) — conservative 1 word.
                            1
                        }
                    }
                }
            }
            TypeKind::Tuple(elems) if elems.is_empty() => 1, // unit
            TypeKind::Tuple(elems) => elems
                .iter()
                .map(|t| self.payload_word_count_for_type_expr(t, outer_enum, outer_variant))
                .sum(),
            TypeKind::Ref(_) | TypeKind::MutRef(_) => 1, // pointer
            TypeKind::MutSlice(_) => 2,                  // { ptr, len }
            _ => 1,
        }
    }

    /// Compute the i64-word count of an LLVM aggregate type. Used by
    /// `payload_word_count_for_type_expr` for user structs whose source
    /// `TypeExpr` isn't directly available (we only kept the resolved
    /// LLVM `StructType`). Recursive: nested aggregates inflate by their
    /// own field count.
    pub(super) fn llvm_type_word_count(ty: BasicTypeEnum<'ctx>) -> usize {
        match ty {
            BasicTypeEnum::IntType(_)
            | BasicTypeEnum::FloatType(_)
            | BasicTypeEnum::PointerType(_) => 1,
            BasicTypeEnum::StructType(st) => (0..st.count_fields())
                .map(|i| Self::llvm_type_word_count(st.get_field_type_at_index(i).unwrap()))
                .sum(),
            BasicTypeEnum::ArrayType(at) => {
                Self::llvm_type_word_count(at.get_element_type()) * (at.len() as usize)
            }
            _ => 1,
        }
    }

    /// Seed enum layouts for stdlib types that are not declared as `enum` in
    /// the prelude AST (e.g. Option[T]) so that variant construction/matching
    /// and methods like `first`/`last`/`get` can produce properly typed LLVM.
    pub(super) fn seed_builtin_enum_layouts(&mut self) {
        let i64_t: BasicTypeEnum<'ctx> = self.context.i64_type().into();
        // Option[T]: { i64 tag, i64 w0, i64 w1, i64 w2 } — payload widened
        // to 3 i64 words (from the original 1) so tuple `(i64, i64)`
        // payloads (the kata's `VecDeque[(i64,i64)].pop_front()` element
        // shape) and 3-word aggregates (`Vec[T]` / `String` ABI =
        // `{ptr, len, cap}`) fit. Backwards-compatible with the legacy
        // single-word consumers (`Vec.first` / `Vec.last` / `Map.get` /
        // `Map.insert` / etc.) — they `build_insert_value` only at
        // indices 0 (tag) and 1 (w0); trailing fields default to undef.
        // Match destructure pulls per-binding word count via
        // `pattern_payload_word_count` (see `reconstruct_payload_value`)
        // — single-word bindings still extract only w0, not all 3.
        let enum_type = self
            .context
            .struct_type(&[i64_t, i64_t, i64_t, i64_t], false);
        let option_payload_words = 3usize;

        // Option[T]:
        //   None(tag=0)
        //   Some(tag=1, w0..w(N-1)=payload words; N varies per use site
        //   via `coerce_to_payload_words` at construction)
        if !self.enum_layouts.contains_key("Option") {
            let mut tags = HashMap::new();
            tags.insert("None".to_string(), 0u64);
            tags.insert("Some".to_string(), 1u64);
            let mut field_counts = HashMap::new();
            field_counts.insert("None".to_string(), 0usize);
            field_counts.insert("Some".to_string(), 1usize);
            let mut field_word_offsets = HashMap::new();
            field_word_offsets.insert("None".to_string(), Vec::new());
            // Some's single source field spans the full payload area.
            // `reconstruct_payload_value` slices this to the binding's
            // natural width (1 for primitives, 2 for Slice, 3 for
            // Vec/String, sum for tuples).
            field_word_offsets.insert("Some".to_string(), vec![(0, option_payload_words)]);
            // DP slice: Option[T] is generic; the seeded shape can't
            // synthesize per-monomorphization drop kinds, so uniformly
            // None — the drop function (if synthesized) is a pure
            // tag-switch with default `ret`. User-declared enums with
            // explicit String/Vec payloads go through `declare_enums`.
            let mut field_drop_kinds = HashMap::new();
            field_drop_kinds.insert("None".to_string(), Vec::new());
            field_drop_kinds.insert(
                "Some".to_string(),
                std::iter::repeat_n(EnumDropKind::None, option_payload_words).collect(),
            );
            self.enum_layouts.insert(
                "Option".to_string(),
                EnumLayout {
                    llvm_type: enum_type,
                    tags,
                    field_counts,
                    field_word_offsets,
                    field_drop_kinds,
                    is_shared: false,
                },
            );
        }

        // std.json `Json` enum — baked stdlib in `runtime/stdlib/json.kara`
        // reaches the typechecker via `STDLIB_PROGRAMS` but does NOT reach
        // codegen's `declare_enums` pass (codegen reads only the user's
        // `program.items`). Without an explicit layout seed here, the
        // codegen-side `Json.X(...)` construction in `try_compile_enum_variant`
        // falls through to the default `i64 0` placeholder, breaking
        // `Json.stringify()` dispatch and every kata that emits JSON via
        // compiled binaries (phase-8 line 435).
        //
        // Variant layout (4 i64 words total — matches Option's seeded shape):
        //   Null    (tag=0) — 0 payload words
        //   Bool    (tag=1) — 1 payload word  (bool stored at w0)
        //   Number  (tag=2) — 1 payload word  (f64 bitpattern at w0)
        //   String  (tag=3) — 3 payload words ({data ptr, len, cap})
        //   Array   (tag=4) — 3 payload words (Vec[Json] = {data, len, cap})
        //   Object  (tag=5) — 3 payload words (Vec[(String, Json)] = {data, len, cap})
        //
        // String/Array/Object's data buffer should be dropped at scope
        // exit. EnumDropKind::VecOrString does the standard `cap > 0 ?
        // free(data)` walk in `emit_enum_drop_switch` — but Array/Object
        // need recursive child cleanup (each element is itself a Json or
        // a (String, Json) tuple). The runtime-side
        // `karac_runtime_json_free_value` handles the recursive case via
        // tag-keyed dispatch; for codegen-built Kāra `Json` values stored
        // in local bindings, the value lives entirely in registers /
        // stack until the user calls `.stringify()`, which immediately
        // frees the intermediate FFI tree it materialized — so v1's
        // EnumDropKind::None on Array/Object is correct (no Kāra-side
        // heap-owning state at the variant-layout level beyond what the
        // Vec-typed payload's own drop path would handle). When the
        // higher-level Vec[Json] / Vec[(String, Json)] drop landing
        // arrives, switch these to VecOrString.
        if !self.enum_layouts.contains_key("Json") {
            let json_enum_type = self
                .context
                .struct_type(&[i64_t, i64_t, i64_t, i64_t], false);
            let mut tags = HashMap::new();
            tags.insert("Null".to_string(), 0u64);
            tags.insert("Bool".to_string(), 1u64);
            tags.insert("Number".to_string(), 2u64);
            tags.insert("String".to_string(), 3u64);
            tags.insert("Array".to_string(), 4u64);
            tags.insert("Object".to_string(), 5u64);
            let mut field_counts = HashMap::new();
            field_counts.insert("Null".to_string(), 0usize);
            field_counts.insert("Bool".to_string(), 1usize);
            field_counts.insert("Number".to_string(), 1usize);
            field_counts.insert("String".to_string(), 1usize);
            field_counts.insert("Array".to_string(), 1usize);
            field_counts.insert("Object".to_string(), 1usize);
            let mut field_word_offsets = HashMap::new();
            field_word_offsets.insert("Null".to_string(), Vec::new());
            field_word_offsets.insert("Bool".to_string(), vec![(0, 1)]);
            field_word_offsets.insert("Number".to_string(), vec![(0, 1)]);
            field_word_offsets.insert("String".to_string(), vec![(0, 3)]);
            field_word_offsets.insert("Array".to_string(), vec![(0, 3)]);
            field_word_offsets.insert("Object".to_string(), vec![(0, 3)]);
            let mut field_drop_kinds = HashMap::new();
            field_drop_kinds.insert("Null".to_string(), Vec::new());
            field_drop_kinds.insert("Bool".to_string(), vec![EnumDropKind::None]);
            field_drop_kinds.insert("Number".to_string(), vec![EnumDropKind::None]);
            field_drop_kinds.insert("String".to_string(), vec![EnumDropKind::VecOrString]);
            field_drop_kinds.insert("Array".to_string(), vec![EnumDropKind::VecOrString]);
            field_drop_kinds.insert("Object".to_string(), vec![EnumDropKind::VecOrString]);
            self.enum_layouts.insert(
                "Json".to_string(),
                EnumLayout {
                    llvm_type: json_enum_type,
                    tags,
                    field_counts,
                    field_word_offsets,
                    field_drop_kinds,
                    is_shared: false,
                },
            );
        }

        // Result[T, E]: { i64 tag, i64 w0, i64 w1, i64 w2, i64 w3 }
        // — Err(tag=0) | Ok(tag=1), payload occupies w0..w3.
        //
        // Phase-8 line 435 slice 2 widening (2026-05-21): bumped from the
        // legacy `{i64, i64}` (single payload word) to four payload words
        // so `Result[Json, JsonError]` — the return type of
        // `Json.parse(s)` — can carry the four-word `Json` enum value
        // verbatim in the Ok arm. Backwards-compatible by construction:
        //   - Construction (`try_compile_enum_variant`) extracts the
        //     value's natural LLVM-field count via `coerce_to_payload_words`
        //     and writes only those many words; trailing slots stay undef.
        //   - Destructure (`reconstruct_payload_value`) reads only as many
        //     words as the binding's natural width (`pattern_payload_word_count`),
        //     so single-word bindings still extract w0 alone.
        //   - The `?` operator (`compile_question`) pulls `enum_ty`
        //     dynamically from the enclosing function's return type.
        //   - Par-block surfaces (`par_blocks::compile_par_block`) read
        //     the Result struct type from the slot-layout descriptor, not
        //     a hardcoded shape.
        // The Err arm of `Result[Json, JsonError]` truncates `JsonError`
        // (5 words: u32 line + u32 column + String (ptr,len,cap)) to four
        // — `cap` is lost, so the message String's scope-exit free becomes
        // a no-op (acceptable v1 behavior; the message bytes leak but
        // remain valid until process exit, and the kata's `/echo` arm
        // never reads them anyway). A future Result-payload-width sweep
        // can widen further once we have a need for it.
        let result_enum_type = self
            .context
            .struct_type(&[i64_t, i64_t, i64_t, i64_t, i64_t], false);
        let result_payload_words = 4usize;
        if !self.enum_layouts.contains_key("Result") {
            let mut tags = HashMap::new();
            tags.insert("Err".to_string(), 0u64);
            tags.insert("Ok".to_string(), 1u64);
            let mut field_counts = HashMap::new();
            field_counts.insert("Err".to_string(), 1usize);
            field_counts.insert("Ok".to_string(), 1usize);
            let mut field_word_offsets = HashMap::new();
            field_word_offsets.insert("Err".to_string(), vec![(0, result_payload_words)]);
            field_word_offsets.insert("Ok".to_string(), vec![(0, result_payload_words)]);
            let mut field_drop_kinds = HashMap::new();
            field_drop_kinds.insert(
                "Err".to_string(),
                std::iter::repeat_n(EnumDropKind::None, result_payload_words).collect(),
            );
            field_drop_kinds.insert(
                "Ok".to_string(),
                std::iter::repeat_n(EnumDropKind::None, result_payload_words).collect(),
            );
            self.enum_layouts.insert(
                "Result".to_string(),
                EnumLayout {
                    llvm_type: result_enum_type,
                    tags,
                    field_counts,
                    field_word_offsets,
                    field_drop_kinds,
                    is_shared: false,
                },
            );
        }
    }

    /// DP slice helper — classify a payload field's TypeExpr into an
    /// `EnumDropKind`. Mirrors `payload_word_count_for_type_expr`'s shape
    /// detection: only top-level `String` / `Vec[T]` get the
    /// `VecOrString` 3-word destructor; `Slice[T]` (2 words, borrowed),
    /// primitives, RC pointers, and v1-carved-out nested user-struct
    /// payloads (their per-field drop is the optional test-7 territory)
    /// all return `None`. Tuples and nested user enums are also `None`
    /// at v1 — the DP1–DP5 design locks scope cleanup to top-level
    /// String/Vec payloads, which is what the regression gates exercise.
    pub(super) fn enum_drop_kind_for_type_expr(&self, ty: &TypeExpr) -> EnumDropKind {
        match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                match name {
                    "String" | "Vec" => EnumDropKind::VecOrString,
                    _ => EnumDropKind::None,
                }
            }
            _ => EnumDropKind::None,
        }
    }

    // ── FFI: extern function declarations ──────────────────────────

    pub(super) fn declare_extern_functions(&mut self, program: &Program) -> Result<(), String> {
        for item in &program.items {
            match item {
                Item::ExternFunction(ext) => self.declare_one_extern_function(ext, &[]),
                Item::ExternBlock(b) => {
                    for it in &b.items {
                        match it {
                            ExternItem::Function(ext) => {
                                self.declare_one_extern_function(ext, &b.attributes)
                            }
                            // Opaque foreign types lower naturally — the
                            // type's name is never used as a value (only
                            // as the element of `ref Foo` / `mut ref Foo`,
                            // which lower as sized pointers via existing
                            // reference-type machinery). No LLVM emission
                            // needed at the declaration site.
                            ExternItem::OpaqueType(_) => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub(super) fn declare_one_extern_function(
        &mut self,
        ext: &ExternFunction,
        block_attrs: &[Attribute],
    ) {
        let param_types: Vec<BasicMetadataTypeEnum<'ctx>> = ext
            .params
            .iter()
            .map(|p| BasicMetadataTypeEnum::from(self.llvm_type_for_type_expr(&p.ty)))
            .collect();

        let fn_type = match ext.return_type.as_ref().and_then(|ty| match &ty.kind {
            TypeKind::Path(path) => {
                let name = path.segments.first().map(|s| s.as_str()).unwrap_or("");
                if name.is_empty() {
                    None
                } else {
                    Some(self.llvm_type_for_name(name))
                }
            }
            TypeKind::Tuple(elems) if elems.is_empty() => None,
            _ => Some(self.llvm_type_for_type_expr(ty)),
        }) {
            Some(BasicTypeEnum::IntType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::FloatType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::PointerType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::StructType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::ArrayType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::VectorType(t)) => t.fn_type(&param_types, false),
            Some(BasicTypeEnum::ScalableVectorType(_)) | None => {
                self.context.void_type().fn_type(&param_types, false)
            }
        };

        let fn_val = self
            .module
            .add_function(&ext.name, fn_type, Some(Linkage::External));
        // `#[link_section]`, `#[no_mangle]`, `#[used]` attached to an
        // `extern` declaration apply to the symbol as imported. Block-
        // level attributes (when the extern is inside an
        // `unsafe extern { ... }` block) apply to every item; per-item
        // attributes win on conflict via order (block first, item last).
        self.apply_linker_attrs(fn_val, block_attrs);
        self.apply_linker_attrs(fn_val, &ext.attributes);
    }
}

/// Detect whether a `TypeExpr` is the `i64` primitive — slice 8i's v1
/// scope for non-unit returns through the state-struct terminal field.
/// Other primitive widths (`i32`, `u64`, `bool`) and complex types
/// (`Vec[T]`, user-named structs, etc.) are deferred to a follow-on
/// slice that widens the supported return-type set.
pub(super) fn is_i64_return_type(ty: &TypeExpr) -> bool {
    matches!(
        &ty.kind,
        TypeKind::Path(p) if p.segments.len() == 1 && p.segments[0] == "i64"
    )
}

/// Locate the user-level `Function` AST node corresponding to a state-
/// machine function key. For free functions the key is the bare name
/// (`"driver"`); for impl methods the key is `"Type.method"` and we
/// match the impl block's target-type name's last segment against
/// the key's prefix. Returns `None` when no matching item is found
/// (e.g. the key refers to a generic or trait-method that doesn't
/// have a concrete free-fn / impl-method AST node yet).
///
/// Used by phase 6 line 26 slice 8h's body-splitting walk to find
/// the user's statements to emit per state arm.
/// Slice 8v Phase 2: look up the declared `TypeExpr` for a function
/// parameter by binding name. Returns `None` when `fn_ast` is `None`
/// or when no parameter binds the requested name (e.g. the name
/// refers to a body-level let-binding, not a parameter; or to a
/// destructuring binding whose introducing pattern's surface name
/// doesn't match `fn_ast.params[i].pattern`). Used by
/// `emit_state_struct_type_for_key` per-mono path to recover the
/// generic-typed parameter's `TypeExpr` so `llvm_type_for_type_expr`
/// can resolve it through the active `type_subst`.
pub(super) fn lookup_param_type_expr(
    fn_ast: Option<&Function>,
    field_name: &str,
) -> Option<TypeExpr> {
    let func = fn_ast?;
    for param in &func.params {
        for name in param.pattern.binding_names() {
            if name == field_name {
                return Some(param.ty.clone());
            }
        }
    }
    None
}

/// Phase 6 line 26 slice 8x: look up the declared `TypeExpr` for a
/// body-level `let`-binding by binding name. Walks `fn_ast.body`
/// recursively (descending into nested `Block`-bearing expressions
/// — `if` / `while` / `for` / `loop` / `match` arm bodies / bare
/// blocks — per the body-splitting walker's discipline) for the
/// first `StmtKind::Let` / `StmtKind::LetElse` / `StmtKind::LetUninit`
/// whose pattern binds the requested name. Returns the explicit
/// type annotation when present, falling back to the RHS expression's
/// recoverable type when annotation is absent — v1 recognises the
/// `let <name> = <identifier>;` shape and recursively resolves the
/// referenced identifier through `lookup_param_type_expr` first
/// (params take priority) then this helper (chained let-bindings).
/// Other RHS shapes (calls, method calls, literals, expressions)
/// return `None` and the caller falls through to the i64 default,
/// matching the conservative `None`-fallback rule the per-key state
/// struct emission has established.
///
/// Slice 8x ships the parameter-only path's sibling for the
/// generic-typed arm-local let-binding surface — `fn driver[T](item:
/// T) { fetch(); let copy = item; fetch(); }` with `T = Vec[i64]`
/// previously fell through to i64 for the `copy` field (the
/// typechecker's `pattern_binding_types` recorder emits `type_name:
/// None` for type-parameter-typed bindings whose RHS is itself
/// a type-parameter-typed identifier). With slice 8x, the per-mono
/// state struct lowers `copy` to the Vec struct shape `{ ptr, i64,
/// i64 }` and the per-mono destructor's None-fallback chain (also
/// extended to consult this helper after `lookup_param_type_expr`)
/// emits `cap > 0 ? free` for both `item` and `copy` in source
/// order. The shape-coverage gap remaining after slice 8x — RHS
/// expressions other than bare-identifier (calls, method calls,
/// struct literals, etc.) — needs the typechecker's
/// `expr_types` sibling table threaded through to recover the
/// recorded expression type; deferred until that infrastructure
/// lands or a use case forces it.
pub(super) fn lookup_let_type_expr(
    fn_ast: Option<&Function>,
    field_name: &str,
) -> Option<TypeExpr> {
    let func = fn_ast?;
    let (ty, value) = find_let_binding_in_block(&func.body, field_name)?;
    if let Some(te) = ty {
        return Some(te.clone());
    }
    // Annotation absent — fall back to the RHS's recoverable type.
    // v1 only recognises bare-identifier RHS (`let copy = item;`):
    // resolve through `lookup_param_type_expr` first (params take
    // priority and short-circuit the recursion); fall back to
    // chained let-binding lookup (`let a = item; let b = a;`).
    let value = value?;
    if let ExprKind::Identifier(name) = &value.kind {
        return lookup_param_type_expr(fn_ast, name).or_else(|| lookup_let_type_expr(fn_ast, name));
    }
    None
}

/// Walk a block (and any nested block-bearing exprs reachable from
/// its stmts / final-expr) looking for a `let`-style binding whose
/// pattern binds `field_name`. Returns the explicit type annotation
/// (if any) paired with the RHS expression (the RHS is `None` for
/// `LetUninit` whose type is always annotated). First match wins —
/// matches the body-splitting walker's source-order traversal.
fn find_let_binding_in_block<'a>(
    block: &'a Block,
    field_name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    for stmt in &block.stmts {
        if let Some(hit) = find_let_binding_in_stmt(stmt, field_name) {
            return Some(hit);
        }
    }
    if let Some(fe) = &block.final_expr {
        if let Some(hit) = find_let_binding_in_expr(fe, field_name) {
            return Some(hit);
        }
    }
    None
}

fn find_let_binding_in_stmt<'a>(
    stmt: &'a Stmt,
    field_name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    match &stmt.kind {
        StmtKind::Let {
            pattern, ty, value, ..
        } => {
            if pattern.binding_names().iter().any(|n| n == field_name) {
                Some((ty.as_ref(), Some(value)))
            } else {
                None
            }
        }
        StmtKind::LetElse {
            pattern, ty, value, ..
        } => {
            if pattern.binding_names().iter().any(|n| n == field_name) {
                Some((ty.as_ref(), Some(value)))
            } else {
                None
            }
        }
        StmtKind::LetUninit { name, ty, .. } => {
            if name == field_name {
                Some((Some(ty), None))
            } else {
                None
            }
        }
        StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
            find_let_binding_in_block(body, field_name)
        }
        StmtKind::Expr(e)
        | StmtKind::Assign { value: e, .. }
        | StmtKind::CompoundAssign { value: e, .. } => find_let_binding_in_expr(e, field_name),
    }
}

fn find_let_binding_in_expr<'a>(
    expr: &'a Expr,
    field_name: &str,
) -> Option<(Option<&'a TypeExpr>, Option<&'a Expr>)> {
    match &expr.kind {
        ExprKind::Block(b) => find_let_binding_in_block(b, field_name),
        ExprKind::If {
            then_block,
            else_branch,
            ..
        }
        | ExprKind::IfLet {
            then_block,
            else_branch,
            ..
        } => find_let_binding_in_block(then_block, field_name).or_else(|| {
            else_branch
                .as_ref()
                .and_then(|e| find_let_binding_in_expr(e, field_name))
        }),
        ExprKind::Match { arms, .. } => arms
            .iter()
            .find_map(|arm| find_let_binding_in_expr(&arm.body, field_name)),
        ExprKind::While { body, .. }
        | ExprKind::WhileLet { body, .. }
        | ExprKind::For { body, .. }
        | ExprKind::Loop { body, .. } => find_let_binding_in_block(body, field_name),
        _ => None,
    }
}

/// Phase 6 line 26 slice 8v: does `fn_key` refer to a generic
/// (un-monomorphized) function or method? The base-name passes
/// (slice 5 state-struct LLVM type, slice 6 poll-fn, slice 8c
/// constructor, slice 8u destructor) skip generic entries because
/// their captured-local field types reference type parameters
/// (`T`, `U`, ...) that `llvm_type_for_name` would silently fall
/// through to the i64 default for — producing dead-and-broken IR
/// the slice 8d caller-side intercept can't reach anyway (the
/// `compile_call` dispatch returns early through
/// `compile_generic_call` for generics, bypassing the
/// state-machine intercept on the polymorphic name). Per-mono
/// emission then re-emits each helper at the mangled key with
/// `type_subst` populated, so `llvm_type_for_name(T)` resolves to
/// the concrete LLVM type for that monomorphization. Same
/// fn_key shape as `find_function_ast`: `"name"` for free fns,
/// `"Type.method"` for impl methods.
pub(super) fn is_generic_fn_key(program: &Program, fn_key: &str) -> bool {
    match find_function_ast(program, fn_key) {
        Some(f) => f.generic_params.is_some(),
        None => false,
    }
}

pub(super) fn find_function_ast<'p>(program: &'p Program, fn_key: &str) -> Option<&'p Function> {
    for item in &program.items {
        match item {
            Item::Function(f) if f.name == fn_key => return Some(f),
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    TypeKind::Path(p) => match p.segments.last() {
                        Some(s) => s.as_str(),
                        None => continue,
                    },
                    _ => continue,
                };
                let expected_prefix = format!("{type_name}.");
                if !fn_key.starts_with(&expected_prefix) {
                    continue;
                }
                let method_name = &fn_key[expected_prefix.len()..];
                for ii in &imp.items {
                    if let ImplItem::Method(m) = ii {
                        if m.name == method_name {
                            return Some(m);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Phase 6 line 26 slice 8t: count yield-point spans whose source
/// range is contained inside the given statement's span. Used by the
/// body-splitting walker when a stmt isn't classified as a top-level
/// yield or a top-level Call/MethodCall enqueue — typically when the
/// stmt is control flow (`if cond { fetch(); }`, `while not_done {
/// fetch(); }`, etc.) whose nested yields slice 2's enumeration has
/// already recorded but slice 8's body splitter would otherwise miss.
///
/// Span-containment is a sound count because slice 2's
/// `YieldPointWalker` records yield-points in source order, so once
/// a yield's offset is past the statement's end, all subsequent
/// yields are also past it (the `take_while` short-circuits there).
/// The `filter` defensively guards against a yield ordered before the
/// statement-start (a slice-2 ordering bug if it ever happens).
///
/// **v1 limitation:** advancing `cur_arm` by the count makes post-CF
/// statements correctly classified into the post-yield arm, but the
/// CF expression itself is dropped from the IR. The conditional /
/// looping structure isn't preserved — the function suspends-then-
/// resumes uniformly. A follow-on slice rebuilds the CF in IR by
/// branching into state-machine arm blocks at yield points.
fn count_yields_inside_span(
    stmt_span: &crate::token::Span,
    yield_points: &[YieldPoint],
    start_idx: usize,
) -> usize {
    let stmt_end = stmt_span.offset + stmt_span.length;
    yield_points
        .iter()
        .skip(start_idx)
        .take_while(|yp| yp.span.offset < stmt_end)
        .filter(|yp| {
            yp.span.offset >= stmt_span.offset && yp.span.offset + yp.span.length <= stmt_end
        })
        .count()
}

/// Phase 6 line 26 slice 8k: classify a call arg into the recognised
/// `BodyArg` set. Returns `None` for any shape outside v1's coverage
/// (method-call args, field accesses, struct literals, comparison /
/// logical / bitwise / float ops, etc.) — the body-splitting walker
/// treats the whole call as ineligible when any arg returns `None`,
/// mirroring the conservative skip behaviour of slice 8h's
/// "non-emittable-shape silently dropped" rule.
///
/// Slice 8q: arithmetic binary expressions (`+` / `-` / `*` / `/` / `%`)
/// reach this point in their *lowered* form. The lowering pass
/// (`src/lowering.rs`) rewrites every `Binary { op, left, right }` over
/// a primitive type into a `Call { callee: Path { segments: ["<int_ty>",
/// "<method>"] }, args: [lhs, rhs] }` so codegen sees the same shape
/// for every arithmetic site. The recognition path here matches on that
/// post-lowering form: callee path is `[<i*|u*|usize|isize>, <add|sub|
/// mul|div|rem>]`, args are unwrapped from their `CallArg` envelope,
/// and both operands recurse through `recognize_body_arg`. Comparison
/// (`eq`/`ne`/`lt`/`le`/`gt`/`ge`), logical (`bitand`/`bitor`/`bitxor`/
/// `shl`/`shr`), and float arithmetic stay outside the recognised set
/// — slice 8q's scope is integer arithmetic that unblocks compound-
/// assign.
fn recognize_body_arg(expr: &Expr, in_scope_names: &HashSet<String>) -> Option<BodyArg> {
    match &expr.kind {
        ExprKind::Integer(n, _) => Some(BodyArg::IntLit(*n)),
        ExprKind::Identifier(name) if in_scope_names.contains(name) => {
            Some(BodyArg::Slot(name.clone()))
        }
        ExprKind::Call { callee, args } => {
            // Lowered binary arithmetic surface — see
            // `src/lowering.rs::rewrite_binary`. Match the
            // two-segment `Path` callee shape, restrict to integer
            // primitive types, and accept the five arithmetic
            // method names that map onto v1's LLVM int-arith ops.
            let ExprKind::Path { segments, .. } = &callee.kind else {
                return None;
            };
            if segments.len() != 2 {
                return None;
            }
            let int_primitive = matches!(
                segments[0].as_str(),
                "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize"
            );
            if !int_primitive {
                return None;
            }
            let arith_op = match segments[1].as_str() {
                "add" => BinaryArithOp::Add,
                "sub" => BinaryArithOp::Sub,
                "mul" => BinaryArithOp::Mul,
                "div" => BinaryArithOp::Div,
                "rem" => BinaryArithOp::Mod,
                _ => return None,
            };
            if args.len() != 2 {
                return None;
            }
            let lhs = recognize_body_arg(&args[0].value, in_scope_names)?;
            let rhs = recognize_body_arg(&args[1].value, in_scope_names)?;
            Some(BodyArg::Binary {
                op: arith_op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            })
        }
        _ => None,
    }
}
