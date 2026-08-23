//! The current-function frame.
//!
//! Twelfth slice of the Phase-2 `Codegen` decomposition
//! ([`docs/spikes/state-decomposition-codegen-methodcall.md`]) — cluster
//! 14, and conceptually the cleanest win in the refactor: a real "frame"
//! object for the function being compiled.
//!
//! - `current_fn_name` and `current_fn_param_names`;
//! - the return shape — `tail_ret_inner`, `return_retargets`,
//!   `current_fn_err_payload_ty`, `current_fn_returns_ref`,
//!   `current_fn_boxes_return`, and the two target-ABI return values
//!   (`current_fn_arm64_return_coercion`, `current_fn_sret_param`) that
//!   `target_abi.rs` deliberately left behind for this cluster;
//! - `loop_stack`, the enclosing loop frames;
//! - `current_fn_caller_loc` and `current_fn_heap_closure_spans`.
//!
//! **`current_fn` itself is NOT here.** The spike classes it with the
//! legitimate LLVM substrate (`context`, `builder`, `module`) that stays
//! common, and at 524 access sites it is the handle every helper reaches
//! for rather than frame *metadata*. Moving it would be a rename of a
//! quarter of the codegen tree for no structural gain.
//!
//! These fields are saved and restored around nested function compilation
//! (closures, monomorphized instances) by hand today. Grouping them is the
//! precondition for making that a real push/pop of one value.
//!
//! Accessed as `self.fn_ctx.<name>` from the sibling `impl Codegen`
//! modules.

use std::collections::HashSet;

use inkwell::types::{BasicTypeEnum, StructType};

use super::state;
use super::state::LoopFrame;

/// Per-function compilation frame.
pub(crate) struct FnCtx<'ctx> {
    /// Nested loop stack — innermost frame is last.
    pub(crate) loop_stack: Vec<LoopFrame<'ctx>>,
    /// Heap-closure-env epic Slice 1 (B-2026-06-22-2). Spans (offset,length) of
    /// closure literals in the CURRENTLY-compiled function that ESCAPE via its
    /// return — these get a reference-counted HEAP environment (so the captured
    /// locals outlive the frame) instead of the default stack env. Recomputed
    /// per function (`compile_function`) from the same return-position analysis
    /// as the Slice 0 guard.
    pub(crate) current_fn_heap_closure_spans: std::collections::HashSet<(usize, usize)>,
    /// Names of the CURRENT function's parameters (all modes). Used by the
    /// auto-par reduction cost gate (B-2026-07-23-25): a fine-grained
    /// variable-K reduction whose trip-count bound references a function
    /// parameter is a probable hot-path helper (the `pow10(n)` /
    /// `while i < n { r = r * 10 }` shape) and must not be parallelized —
    /// the per-call dispatch overhead is unrecoverable when it's invoked
    /// millions of times. Cleared + repopulated per function.
    pub(crate) current_fn_param_names: HashSet<String>,
    /// Flow-sensitive tail-return context for `Option[shared T]` returns.
    /// `Some(inner_heap)` means "the expression about to be compiled at a
    /// block's final-expr position is in function-tail-return position, and
    /// the function returns `Option[shared T]` whose inner heap layout is
    /// this". Threaded by `compile_function` → `compile_block` (final expr) →
    /// `compile_if_let` / `compile_match` (each branch's final expr), and
    /// CLEARED while compiling block statements so a non-tail `if let` in
    /// statement position never picks it up. When a tail leaf is a bare
    /// `Option[shared]` binding (`l1` / `l2`), `compile_block` inc's its inner
    /// in that branch's own block — the per-branch compensation that lets a
    /// function MIX `Some(<alias>)` tails (which need no inc) with bare-arg
    /// returns (which do) without the over/under-count a single merge-block
    /// inc would cause. See docs/implementation_checklist/phase-7-codegen.md.
    pub(crate) tail_ret_inner: Option<StructType<'ctx>>,
    /// Active closure-scoped return targets for inline-lowered
    /// `with_provider` bodies (B-2026-07-31-16); innermost last. See
    /// [`state::ReturnRetarget`]. Entries are fn-tagged rather than
    /// saved/cleared across function boundaries — the `ExprKind::Return`
    /// arm only retargets when the top entry's `fn_val` matches
    /// `current_fn`.
    pub(crate) return_retargets: Vec<state::ReturnRetarget<'ctx>>,
    /// Phase 7 § *defer / errdefer codegen* slice 4 follow-up (a) —
    /// wider-E payload reconstruction at the `?` site (2026-05-26).
    /// Source-level LLVM type of the current function's `Result[T, E]`
    /// Err arm — recorded at `compile_function` entry by walking
    /// `func.return_type` for the `Result[T, E]` shape and lowering E
    /// via `llvm_type_for_type_expr`. Read by `compile_question`'s
    /// `fail_bb` to call `rebuild_value_from_payload_words` against
    /// the result struct's payload words (w0/w1/w2 at fields 1/2/3),
    /// staging the source-typed value rather than the i64-coerced
    /// `w0` slice 4 originally used. `None` means the current function
    /// doesn't return `Result[T, E]` (or doesn't return at all) — the
    /// `?` site falls back to staging bare `w0` as i64 in that case.
    pub(crate) current_fn_err_payload_ty: Option<inkwell::types::BasicTypeEnum<'ctx>>,
    /// The Kāra type NAME of that same `Result[T, E]` payload (E's last
    /// path segment), recorded next to its LLVM type. The LLVM type alone
    /// is not enough to install an `errdefer(e)` binding: codegen's
    /// user-struct / user-enum Display dispatch is NAME-keyed through
    /// `var_types.var_type_names`, so without this the binding lowers as an
    /// anonymous aggregate and `f"{e}"` on a struct or enum `E` fails to
    /// compile (B-2026-08-23-19). `None` when the function does not return
    /// `Result[T, E]`, or when E is not a simple path type (a tuple E is
    /// dispatched by the span-keyed table instead, so it needs no name).
    pub(crate) current_fn_err_payload_type_name: Option<String>,
    /// True while compiling a function whose declared return type is a
    /// borrow (`-> ref T` / `-> mut ref T`). The LLVM signature returns a
    /// thin `ptr`, so the tail / explicit-`return` sites must emit the
    /// ADDRESS of the borrow source (a `ref` param or a field reached
    /// through one) via `compile_ref_return_ptr`, not the materialized
    /// value — see `B-2026-06-07-5` (returned-borrow codegen). Set per
    /// function in `compile_function`.
    pub(crate) current_fn_returns_ref: bool,
    /// True while compiling a `pub extern "C" fn` whose non-transparent
    /// aggregate return (`Vec[scalar]` / `String`) is auto-boxed for the C
    /// ABI (additive-interop Slice 4 Path B). Kāra returns such a
    /// `{data,len,cap}` value in 3 registers (rax/rdx/rcx), which does NOT
    /// match the SysV struct-return ABI a C caller expects — so the export
    /// heap-boxes the value and returns an opaque *pointer* (a scalar
    /// return, trivially C-compatible; the C side reads `v->data`/`v->len`
    /// through the header's struct + frees via the auto-emitted
    /// `karac_free_<name>`). Set per function in `compile_function`; read at
    /// the tail- and explicit-`return` sites to box before `ret`.
    pub(crate) current_fn_boxes_return: bool,
    /// Name of the function currently being compiled (for rc_fallback_fns lookup).
    pub(crate) current_fn_name: String,
    /// `#[track_caller]` slice 4/5: when the function currently being compiled
    /// is `#[track_caller]`, its three hidden trailing params — the received
    /// caller location `(file_ptr, line, col)`. `emit_panic` redirects the
    /// reported panic location to these runtime values, and a nested
    /// `#[track_caller]` call forwards them (the transitivity rule). `None`
    /// inside an ordinary function.
    pub(crate) current_fn_caller_loc: Option<(
        inkwell::values::PointerValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
        inkwell::values::IntValue<'ctx>,
    )>,
    /// The current function's AArch64 struct-return coercion type, set at body
    /// entry from `arm64_coerced_struct_returns`. `Some` ⇒ every return site
    /// reinterprets its `#[repr(C)]` struct value into this type before
    /// `ret`. `None` on x86-64 and for non-coerced returns.
    pub(crate) current_fn_arm64_return_coercion: Option<BasicTypeEnum<'ctx>>,
    /// The current function's `sret` result pointer (the leading param), set at
    /// body entry from `sret_struct_returns`. `Some` ⇒ every return site stores
    /// its struct value here and returns `void`; the prologue also shifts every
    /// Kāra param index by +1 to skip this leading pointer.
    pub(crate) current_fn_sret_param: Option<inkwell::values::PointerValue<'ctx>>,
}
