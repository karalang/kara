//! Runtime lifecycle entry points for producer-mode library artifacts
//! (additive-interop Slice 2; design.md § Exported C ABI). A C / Rust
//! host that links a Kāra `.a` / `.so` calls `karac_runtime_init()` once
//! before the first exported call and `karac_runtime_shutdown()` at
//! teardown — the two prototypes the C-header emitter (`src/cheader.rs`)
//! always surfaces.
//!
//! At v1 these are no-ops beyond documenting the contract: the allocator
//! (`alloc.rs`) is backed by the process global allocator (no explicit
//! arming), and the scheduler initializes lazily on first `spawn` / `par`.
//! They exist so the emitted header's lifecycle prototypes resolve at
//! link time, and so the contract has a stable ABI hook to grow into once
//! the scheduler needs explicit host-thread setup / teardown (at which
//! point the bodies fill in without a header or source-level break for
//! existing callers).

/// Initialize the Kāra runtime. Idempotent; safe to call once at host
/// startup. No-op at v1 (allocator is global, scheduler is lazy).
#[no_mangle]
pub extern "C" fn karac_runtime_init() {}

/// Shut the Kāra runtime down, draining any runtime-owned tasks. Call at
/// host teardown. No-op at v1.
#[no_mangle]
pub extern "C" fn karac_runtime_shutdown() {}
