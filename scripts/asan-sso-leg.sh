#!/usr/bin/env bash
# Run the memory_sanitizer suite with Small-String Optimization TURNED ON
# (`KARAC_SSO=1`) and gate the result against its own expected-failures list.
# B-2026-09-16-35.
#
# WHY A FOURTH LEG. `KARAC_SSO` is read once per process through a `OnceLock`
# (`src/codegen/sso.rs::sso_enabled`) that defaults to OFF, and
# `tests/memory_sanitizer.rs` compiles every fixture IN-PROCESS. So no fixture
# in that file can turn SSO on for itself: the first reader wins, and a fixture
# that set the variable would change the setting for every other fixture
# compiling concurrently. Every one of the ~1660 ASAN cells therefore exercises
# the non-SSO String paths only, and the entire inline-String surface — which
# since B-2026-09-16-1 includes a heap allocation codegen OWNS while the free
# stays in the runtime — has never been under a sanitizer.
#
# MEASURED, on the row that filed this. While adding ownership cells for
# B-2026-09-16-1 the heap route was deliberately broken (the result aggregate
# made to point into the SOURCE buffer — a guaranteed double free). A direct
# `KARAC_SSO=1` build of the same program aborts immediately; the new ASAN
# fixture stayed GREEN with that fault in place, as did three `tests/codegen.rs`
# fixtures written for the same change.
#
# WHY A WRAPPER RATHER THAN HARNESS WORK. The filing row guessed the fix would
# have to be "a shelled-out lane", because `run_under_asan` builds and links
# in-process. It does not: the `OnceLock` is per PROCESS, and the process that
# compiles is the test binary itself, so exporting the variable for the whole
# `cargo test` run flips every fixture at once. That is exactly the shape
# `asan-o0-leg.sh`'s header describes — "any other leg's environment is
# inherited by the cargo run, so a new leg is a wrapper, not a fork" — and it is
# how `asan-instrumented-leg.sh` is built. Nothing in `tests/` changes.
#
# WHY IT RUNS AT -O0. Same argument as `asan-o0-leg.sh`: at -O2 LLVM deletes an
# allocation nothing observes, so an -O2-only zero is evidence of nothing, and
# the allocation this leg exists for is precisely the one the de-inlining path
# makes. Override with ASAN_LEG_OPT_LEVEL if a level-specific question comes up.
#
# WHY A SEPARATE QUARANTINE LIST. The SSO and non-SSO trees are DIFFERENT
# emitted code for the same fixture — inline descriptors, tag-aware reads, a
# de-inlining allocation that does not exist at SSO=0 — so the two failure sets
# are not comparable in either direction. Sharing `asan-o0-known-failures.txt`
# would let an SSO-surface regression hide behind an allocator-class quarantine
# entry naming the same fixture, and would equally make an SSO-only failure look
# like an -O0 regression. Same ratchet in both directions as the other lists: an
# unlisted failure is red, and a listed fixture that starts PASSING is red too.
#
# WHAT A RED RUN HERE MEANS, and it is not the same as the other legs. SSO is
# still STAGED — off by default while the tag-aware read surface is swept into
# ~430 field-0/field-1 sites. A failure here is therefore a defect in the
# unfinished SSO surface, not a regression on the shipping default, and the
# owning row is usually an SSO-campaign row rather than the fixture's own. That
# is why this leg is NOT a pre-push gate for ordinary work: run it when touching
# `src/codegen/sso.rs`, the String read surface, or anything that allocates a
# String buffer. The default flip is what turns it into one.
#
# Usage:
#   scripts/asan-sso-leg.sh                   # full leg
#   scripts/asan-sso-leg.sh --update          # rewrite the list from this run
#   ASAN_O0_TEST_THREADS=8 scripts/asan-sso-leg.sh
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export KARAC_SSO=1

export ASAN_LEG_NAME="SSO -O0"
export ASAN_LEG_OPT_LEVEL="${ASAN_LEG_OPT_LEVEL:-0}"
export ASAN_LEG_EXPECTED="${ASAN_LEG_EXPECTED:-$HERE/../tests/asan-sso-known-failures.txt}"

exec "$HERE/asan-o0-leg.sh" "$@"
