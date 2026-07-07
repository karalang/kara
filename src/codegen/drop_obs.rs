//! Read-only codegen drop-observability recorder.
//!
//! This is the Slice-4 down-payment of the ownership-model-mechanization spike
//! (`docs/spikes/ownership-model-mechanization.md`). Slice 3 delivered the
//! executable ownership oracle (`src/ownership_oracle.rs`): a standalone pass
//! that computes, per function, the *drop schedule* (which owned heap places
//! must be freed at each scope exit) purely from the AST. Slice 4's endgame is
//! codegen consuming that schedule instead of re-deriving it. The bounded,
//! zero-risk first step toward that — and the piece the spike flagged as the
//! remaining half of Slice 3's "differential vs codegen" — is *observing* what
//! drops codegen actually emits, so the oracle's schedule can be diffed against
//! real lowering. Divergences are the remaining bugs, now attributable to the
//! lowering (a missing drop → leak, an extra drop → double-free) rather than
//! the model.
//!
//! **Design: a thread-local string sink, off by default.** Recording is a
//! deliberate no-op unless a differential harness calls [`begin`] first, so the
//! normal `karac` / test / REPL codegen path pays nothing (one relaxed
//! `is_some` check per emitted cleanup action). The recorder is inkwell-free —
//! it takes already-extracted `(function, category, place)` strings, so the
//! LLVM-typed extraction (reading an alloca's name) stays at the single call
//! site in `runtime.rs`'s `emit_cleanup_action_at`, the sole funnel every
//! actually-emitted drop passes through.
//!
//! The comparison is keyed on *place names*: `create_entry_alloca` names each
//! binding's slot after the binding, so an emitted `FreeVecBuffer` /
//! `StructDrop` / `FreeMapHandle` on that slot recovers the place name via
//! `PointerValue::get_name`. Name-carrying variants (`RcDec`, `FreeSharedElided`,
//! …) supply the name directly. Codegen also drops *temporaries* the oracle
//! never names; the differential ignores any recorded place outside the
//! oracle's vocabulary (see `drop_fuzz`'s `--differential` mode), so temporary
//! drops are not false divergences.

use std::cell::RefCell;
use std::sync::OnceLock;

/// One observed codegen drop: the function it was emitted in, a coarse category
/// (currently always `"heap"` — every compiler-internal cleanup variant frees
/// heap), and the *place* it targets (the binding name recovered from the
/// alloca, or the variant's `name` field). The same logical drop is emitted on
/// every exit path (fall-through, early `return`, error path), so a place can
/// be recorded many times; consumers dedup by `(function, place)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropRecord {
    pub function: String,
    pub category: String,
    pub place: String,
}

thread_local! {
    /// `None` = recording off (the default). `Some(vec)` after [`begin`].
    static SINK: RefCell<Option<Vec<DropRecord>>> = const { RefCell::new(None) };
}

/// Start (or restart) recording on this thread, clearing any prior buffer.
/// Call immediately before an in-process `compile_to_ir` whose emitted drops
/// you want to capture.
pub fn begin() {
    SINK.with(|s| *s.borrow_mut() = Some(Vec::new()));
}

/// Whether recording is armed on this thread. Lets the (LLVM-typed) call site
/// skip place-name extraction entirely on the production path, where the answer
/// is always `false`.
pub fn armed() -> bool {
    SINK.with(|s| s.borrow().is_some())
}

/// Fault-injection knob (non-vacuity proof for the differential, mirroring the
/// Slice-1 fuzzer's reverted fault-injection). When `KARAC_DROPOBS_SILENCE=1`,
/// [`record`] no-ops even while armed — simulating codegen *forgetting* every
/// drop — so `drop_fuzz --differential` must then report the oracle's whole
/// schedule as missing. A green differential with the knob *off* and a red one
/// with it *on* together prove the gate actually observes codegen's drops
/// rather than passing vacuously. Read once (env is fixed for a process).
fn silenced() -> bool {
    static SILENCED: OnceLock<bool> = OnceLock::new();
    *SILENCED.get_or_init(|| std::env::var("KARAC_DROPOBS_SILENCE").as_deref() == Ok("1"))
}

/// Record one emitted drop. A no-op unless [`begin`] armed the sink — so the
/// production codegen path is unaffected. `place` empty (an unnamed temporary
/// slot whose alloca carried no name) is still recorded verbatim; the
/// differential filters by the oracle's known place set, so nameless temporary
/// drops fall out there rather than here.
pub fn record(function: &str, category: &str, place: &str) {
    if silenced() {
        return;
    }
    SINK.with(|s| {
        if let Some(buf) = s.borrow_mut().as_mut() {
            buf.push(DropRecord {
                function: function.to_string(),
                category: category.to_string(),
                place: place.to_string(),
            });
        }
    });
}

/// Stop recording and return everything captured since [`begin`], resetting the
/// sink to the off state. Returns an empty vec if recording was never armed.
pub fn take() -> Vec<DropRecord> {
    SINK.with(|s| s.borrow_mut().take().unwrap_or_default())
}
