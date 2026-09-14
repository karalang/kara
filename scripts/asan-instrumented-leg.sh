#!/usr/bin/env bash
# Run the memory_sanitizer suite with the karac-emitted object INSTRUMENTED by
# AddressSanitizer, and gate on the result against its own expected-failures
# list. B-2026-09-07-40.
#
# WHY A THIRD LEG. `tests/memory_sanitizer.rs` links `-fsanitize=address` but
# the flag is passed at the LINK step only, and ASAN's memory-ACCESS checking is
# a COMPILER pass. So the default suite is an ALLOCATOR gate — LeakSanitizer,
# double free, invalid free, allocator-side overflow — and is structurally blind
# to an invalid read or write. Measured on B-2026-09-07-33: a callee that wrote
# three zero words into a 56-byte block it had just freed passed this suite at
# both opt levels, while valgrind reported `Invalid write of size 8` at offsets
# 0/40/48. `KARAC_SANITIZE_ADDRESS=1` runs LLVM's `asan` module pass over the
# emitted module (`codegen::driver::apply_address_sanitizer`), which is what
# adds the shadow-memory checks that catch that class.
#
# WHY IT RUNS AT -O0. The instrumentation checks the accesses that SURVIVE
# optimization, and at -O2 a store into freed memory whose result is never read
# is dead and gets deleted — exactly the case above, which passes at -O2 for a
# reason that has nothing to do with correctness. -O0 keeps the allocation and
# the store real, the same argument `asan-o0-leg.sh` makes for the allocator
# gate. Override with ASAN_LEG_OPT_LEVEL if a level-specific question comes up.
#
# WHY A SEPARATE QUARANTINE LIST. The two legs see DIFFERENT error classes, so
# their failure sets cannot share a list — merging them would let an
# access-checking regression hide behind an allocator-class quarantine entry.
# Same ratchet in both directions: an unlisted failure is red, and a listed
# fixture that starts PASSING is red too. Every entry names the bug row that
# owns it; the list only shrinks.
#
# NOT A SUPERSET, AND NOT A LEAK GATE (B-2026-09-08-12, measured 2026-09-09).
# This header used to claim the leg sees "a strictly LARGER error class than the
# -O0 leg, so its failure set is a superset". That is FALSE, in both directions,
# and the correction matters because the natural next move — "the strict leg is
# green, so the leak must be gone" — is wrong.
#
# Reproduced by reverse-applying 730ae1828 (the B-2026-09-08-7 weak-slot fix) to
# resurrect its 24-byte `Vec[Vec[weak N]]` leak, then running all three legs:
#
#     fixture                       default   -O0    instrumented
#     nested `Vec[Vec[weak N]]`      FAIL     pass       pass
#     `Map[K, Vec[weak V]]` stash    FAIL     FAIL       pass
#
# MECHANISM, which that row left unchased. It is not codegen perturbation — the
# leak still happens under instrumentation; only the REPORT is lost. It is
# LeakSanitizer's conservative STACK root scan: ASAN's stack instrumentation
# leaves a copy of the pointer in a frame that is still live at exit (`main`),
# so LSan reaches the block and classifies it reachable rather than lost.
# Single-variable A/B on the second fixture above:
#
#     LSAN_OPTIONS=(default)      pass    — leak hidden
#     LSAN_OPTIONS=use_stacks=0   FAIL    — leak reported
#     LSAN_OPTIONS=use_registers=0 pass   — stack specifically, not registers
#
# DO NOT "FIX" THIS WITH use_stacks=0. Measured over the full suite: the current
# default is 1564 passed / 0 failed, and `use_stacks=0` is 1530 / 34 failed.
# Those 34 are FALSE POSITIVES, not hidden leaks — under valgrind, matched to
# this harness's auto-par-ON build, every one spot-checked reports `definitely
# lost: 0 bytes` with 1,216-1,444 bytes only `possibly lost`, i.e. reachable
# through an interior pointer. The shared ~1,216-byte floor is the auto-par
# runtime's own state. Dropping stack roots reclassifies that as leaked.
#
# SO: THIS LEG IS AN ACCESS GATE, NOT A LEAK GATE. It exists for the invalid
# read/write class the allocator-only legs are structurally blind to, which is
# what the paragraphs above describe. A GREEN RUN HERE IS NOT EVIDENCE THAT A
# LEAK IS GONE — the DEFAULT and -O0 legs own that question.
#
# The mechanism has its own positive control INSIDE the suite:
# `asan_instrumentation_tracks_the_sanitize_address_knob` asserts the emitted
# object carries an `__asan_report*` reference exactly when the knob is set. It
# is not skippable, so a knob that silently stopped instrumenting fails this leg
# rather than turning it green over an uninstrumented run.
#
# WHAT CAN AND CANNOT BE QUARANTINED (B-2026-09-08-12). A fixture for an open
# defect can only live in the tree if it fails ONLY this leg and/or the -O0 leg,
# because those are the two legs with an expected-failures list. One that fails
# the DEFAULT `--features llvm` leg cannot be quarantined at all — that leg has
# no list — so it is simply red CI and has to be removed. That asymmetry is why
# a known-broken shape sometimes has NO fixture at all, and it is worth knowing
# before writing one: check the default leg FIRST, or the fixture may have to be
# deleted after it is written.
#
# Usage:
#   scripts/asan-instrumented-leg.sh                  # full leg
#   scripts/asan-instrumented-leg.sh --update         # rewrite the list from this run
#   ASAN_O0_TEST_THREADS=8 scripts/asan-instrumented-leg.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export KARAC_SANITIZE_ADDRESS=1

# B-2026-09-09-15 — WITHOUT THIS THE LEG CANNOT LINK ON macOS, and it fails in
# the shape most likely to be misread. The `asan` pass stamps a call to
# `__asan_version_mismatch_check_v<N>` naming the compiler-rt ABI karac's
# bundled LLVM expects; the runtime actually linked is whatever
# `cc -fsanitize=address` picks, which on macOS is Xcode's
# `libclang_rt.asan_osx_dynamic.dylib` and does not export that symbol. Every
# fixture then dies at the LINK step:
#
#     Undefined symbols for architecture arm64:
#       "___asan_version_mismatch_check_v8", referenced from:
#           _asan.module_ctor in karac_asan_*.o
#
# Measured before this line existed: 12 passed / 1555 failed in 41 s, against
# the -O0 leg's 1567 / 0 in 396 s. A near-total failure that finishes an order
# of magnitude FASTER than the passing leg is a build failure, not a
# memory-error storm — the fixtures never ran — but the ratchet's own diagnostic
# says "these are real, fix the codegen defect or quarantine them", so it reads
# as a catastrophic regression.
#
# WHAT TURNING IT OFF GIVES UP, stated because it is a real check and not
# ceremony: the guard exists to catch a pass/runtime version SKEW, and skew is
# exactly what this configuration has. It is disabled here rather than in
# `apply_address_sanitizer` for that reason — a leg that knowingly mixes karac's
# pass with the host's runtime opts out for itself, and an ordinary
# `KARAC_SANITIZE_ADDRESS=1` build still gets the check. What keeps this honest
# is that a run which instrumented nothing cannot report green.
#
# B-2026-09-13-8 — THAT USED TO REST ON THE RATCHET and no longer can. The
# argument was "it fails if a QUARANTINED fixture starts passing", which holds
# only while the quarantine list has entries: both lists are now fully drained
# (0 live entries, 2026-09-14), so an empty `got` against an empty `expected`
# matches exactly and the leg exits 0. Measured on a tree with the runtime
# archives moved aside: `test result: ok. 1613 passed` in 60 s, "matches the
# quarantine list exactly", rc 0 — a vacuous green from the authoritative
# pre-push gate, which is the shape that let `6ea22e3b4` land four red fixtures
# on `main`. The property is now carried by `asan-o0-leg.sh` requiring the
# runtime archives instead, which does not decay as the lists shrink.
#
# Appended rather than assigned, so a caller's own -mllvm flags survive.
export KARAC_LLVM_ARGS="${KARAC_LLVM_ARGS:+$KARAC_LLVM_ARGS }-asan-guard-against-version-mismatch=0"

export ASAN_LEG_NAME="instrumented -O0"
export ASAN_LEG_OPT_LEVEL="${ASAN_LEG_OPT_LEVEL:-0}"
export ASAN_LEG_EXPECTED="${ASAN_LEG_EXPECTED:-$HERE/../tests/asan-instrumented-known-failures.txt}"

exec "$HERE/asan-o0-leg.sh" "$@"
