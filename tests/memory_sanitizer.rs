//! This file was split on 2026-09-21. The fixtures now live in per-area
//! files under `tests/memory_sanitizer/`, because this file had grown into an
//! append target nobody could read, review or blame. The TEST TARGET is
//! unchanged -- `cargo test --features llvm --test memory_sanitizer` still runs
//! every one of them, so CI needs no edit. A separate test target per
//! area was measured at ~118 MiB of link output each (~250 MiB at the
//! default debug level), which the session disk allowance cannot carry;
//! modules cost nothing.
//!
//! Run one area alone:  cargo test --features llvm --test memory_sanitizer <area>::

//! Memory-behavior E2E tests under AddressSanitizer.
//!
//! Compiles representative Kāra programs, links them with `-fsanitize=address`,
//! runs the resulting binary, and asserts a clean ASAN exit. Catches leaks,
//! double free and invalid free from codegen-emitted heap operations
//! (`emit_rc_dec`, `emit_scope_vec_cleanup`, `scope_cleanup_actions`).
//!
//! WHAT THE DEFAULT LEG DOES NOT CATCH, and how to catch it (B-2026-09-07-40).
//! `-fsanitize=address` is passed to `cc` at the LINK step, which gives the
//! ASAN RUNTIME — allocator interposition, i.e. LeakSanitizer, double free,
//! invalid free, allocator-side overflow. ASAN's memory-ACCESS checking is a
//! COMPILER pass, and an object nobody instrumented carries no shadow-memory
//! checks, so a use-after-free READ or WRITE that never reaches `free` passes
//! here. Set `KARAC_SANITIZE_ADDRESS=1` to run the `asan` pass over the emitted
//! module; `scripts/asan-instrumented-leg.sh` is that whole-suite leg, with its
//! own quarantine ratchet. `asan_instrumentation_tracks_the_sanitize_address_knob`
//! is the mechanism's positive control and runs on every leg.
//!
//! Necessary-but-not-sufficient even so: ASAN is blind to drop *ordering* and
//! to "freed late" bugs (frees that happen at process exit rather than scope
//! exit). See `Drop-order E2E tests` and the `scope_cleanup_actions` testing
//! note in `docs/implementation_checklist/` for those gaps.
//!
//! The tests skip gracefully if the host lacks ASAN runtime support (probed
//! once on first invocation) or if `KARAC_SKIP_ASAN_TESTS=1` is set in the
//! environment.

mod common;

#[cfg(feature = "llvm")]
#[path = "memory_sanitizer/mod.rs"]
mod memory_sanitizer_tests;
