#!/usr/bin/env bash
# Make a disk-exhausted gate SAY SO. B-2026-09-09-7.
#
# The writable disk in a cloud session is a per-session allowance of ~38 GiB,
# and one leg of `cargo test --no-run` costs 19.6 GiB in test binaries, so the
# second feature leg cannot start. That failure never names a test and never
# says "disk" in a form that survives a `grep -E 'FAILED|test result'`. The
# shapes, all measured on this repo:
#
#   1  rustc-LLVM ERROR: IO failure on output stream: No space left on device
#   2  collect2: fatal error: ld terminated with signal 7 [Bus error]
#   3  error: failed to write file ...: No space left on device (os error 28)
#   4  nothing at all — the harness tmpdir fills, the child's output is lost,
#      and only the exit code survives (reads as a hang or a flake)
#   5  NAMED TESTS with ordinary-looking assertion diffs, when the failing
#      tests are the ones that write files (B-2026-09-09-5: a `BufReader`
#      cluster; and on 2026-09-16 a `git_fetch` / `registry_proxy` cluster
#      whose count assertions read `left: 0 right: 1`). Shape 5 carries NO
#      disk message anywhere, so signature matching cannot find it — only the
#      free space at the time can, which is why `classify` reads `df` too.
#
# Shape 2 is the worst: it is the linker's mmap'd write failing on a full
# filesystem, and it prints LLVM's "PLEASE submit a bug report" banner, which
# sends you to the wrong project entirely.
#
# DESIGN: `preflight` is ADVISORY and always exits 0. A hard refusal would turn
# a tight-but-workable box red, which is a worse failure than the one this
# guards against. Its job is to put the TRUE allowance in the log, because
# `df`'s size column is the host volume (252G here) and reading it is what
# invites the wrong conclusion. `classify` is the half that returns a verdict,
# and it only ever runs on a leg that already failed.
#
# Usage:
#   scripts/disk-guard.sh allowance
#   scripts/disk-guard.sh preflight [need_gib] [label]
#   scripts/disk-guard.sh classify <logfile>     # exit 4 = disk, 0 = not disk
#   scripts/disk-guard.sh selftest
set -uo pipefail

# `used + avail`, NOT the size column — that is the host volume and is
# meaningless for a per-session allowance.
read_disk() {
    local line; line="$(df -P / | awk 'NR==2{print $3, $4}')"
    USED_KB="${line%% *}"; AVAIL_KB="${line##* }"
    USED_GIB=$(( USED_KB / 1048576 )); AVAIL_GIB=$(( AVAIL_KB / 1048576 ))
    ALLOWANCE_GIB=$(( USED_GIB + AVAIL_GIB ))
    # Shape 5's branch turns on live free space, so it is untestable without a
    # way to say "pretend the disk is full". Test-only, and named so that a
    # reader cannot mistake it for a tuning knob.
    if [[ -n "${DISK_GUARD_FAKE_AVAIL_GIB:-}" ]]; then
        AVAIL_GIB="$DISK_GUARD_FAKE_AVAIL_GIB"
        ALLOWANCE_GIB=$(( USED_GIB + AVAIL_GIB ))
    fi
}

remedies() {
    echo "   Remedies, both measured (B-2026-09-09-7):"
    echo "     cargo clean -p karac                       # ~20-24 GiB back, seconds"
    echo "     CARGO_PROFILE_TEST_DEBUG=line-tables-only   # 252 -> 97 MiB per test binary"
    echo "   NOTE: CARGO_PROFILE_DEV_DEBUG does NOT cover the test profile."
}

case "${1:-}" in
allowance)
    read_disk
    echo "DISK: used=${USED_GIB}G avail=${AVAIL_GIB}G allowance=${ALLOWANCE_GIB}G" \
         "(df's 'size' column is the host volume — ignore it)"
    ;;
preflight)
    need="${2:-8}"; label="${3:-leg}"
    read_disk
    echo ">> disk before $label: used=${USED_GIB}G avail=${AVAIL_GIB}G" \
         "allowance=${ALLOWANCE_GIB}G (host 'size' column is meaningless)"
    if (( AVAIL_GIB < need )); then
        echo ">> WARNING: ${AVAIL_GIB}G free is under the ${need}G this leg wants."
        echo "   A leg that runs out does NOT fail as a named test — see the"
        echo "   shapes in this script's header, and read \`df\` AFTER the leg,"
        echo "   because the leg's own linking is what spends the space."
        remedies
    fi
    exit 0
    ;;
classify)
    log="${2:-}"
    [[ -r "$log" ]] || { echo "disk-guard: cannot read log '$log'" >&2; exit 2; }
    read_disk
    hits=0
    for pat in 'No space left on device' 'os error 28' \
               'ld terminated with signal 7' 'IO failure on output stream'; do
        n="$(grep -c "$pat" "$log" 2>/dev/null || true)"
        if [[ "${n:-0}" -gt 0 ]]; then
            echo "!! DISK SIGNATURE: '$pat' x$n"
            hits=$(( hits + n ))
        fi
    done
    if (( hits > 0 )); then
        echo "!! This leg died of a FULL DISK, not of the change under test."
        echo "   avail=${AVAIL_GIB}G of a ${ALLOWANCE_GIB}G allowance."
        remedies
        exit 4
    fi
    # Shape 5: no disk message anywhere, but the box is out of room and tests
    # failed. Cannot be concluded from the log alone, so this SUSPECTS rather
    # than asserts.
    if (( AVAIL_GIB < 2 )); then
        echo "!! NO disk signature in the log, but only ${AVAIL_GIB}G is free of a"
        echo "   ${ALLOWANCE_GIB}G allowance. Tests that WRITE FILES fail first on a"
        echo "   full disk, with ordinary-looking assertion diffs and no disk"
        echo "   message at all (shape 5 in this script's header). Before"
        echo "   attributing these failures to the change under test: free space"
        echo "   and re-run, and check whether the failing tests are the"
        echo "   file-writing ones."
        remedies
        exit 4
    fi
    echo ">> no disk signature and ${AVAIL_GIB}G free — this failure is NOT disk."
    exit 0
    ;;
selftest)
    # Each shape must be detected, and a clean log must NOT be flagged. Run
    # after touching the patterns above.
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT; rc=0
    mk() { printf '%s\n' "$2" > "$tmp/$1"; }
    mk shape1 'rustc-LLVM ERROR: IO failure on output stream: No space left on device'
    mk shape2 'collect2: fatal error: ld terminated with signal 7 [Bus error]'
    mk shape3 'error: failed to write file /x/dep-graph.part.bin: No space left on device (os error 28)'
    mk clean  'test result: FAILED. 100 passed; 1 failed'
    for s in shape1 shape2 shape3; do
        out="$("$0" classify "$tmp/$s" 2>&1 || true)"
        code=0; "$0" classify "$tmp/$s" >/dev/null 2>&1 || code=$?
        if [[ "$out" == *"DISK SIGNATURE"* && "$code" == "4" ]]; then
            echo "selftest ok: $s detected (exit 4)"
        else
            echo "selftest FAIL: $s not detected (exit $code)"; rc=1
        fi
    done
    # The clean log's verdict depends on free space, which is the point of
    # shape 5 — so only assert it is not flagged by a SIGNATURE.
    if "$0" classify "$tmp/clean" 2>&1 | grep -q 'DISK SIGNATURE'; then
        echo "selftest FAIL: clean log matched a signature"; rc=1
    else
        echo "selftest ok: clean log matches no signature"
    fi
    # Shape 5: named failures, no disk message, disk nearly full -> SUSPECT.
    # Capture and then match: piping into `grep -q` under `set -o pipefail`
    # yields classify's OWN exit (4 on a disk verdict) rather than grep's, so
    # the pipeline form reports a failure that did not happen. Caught by this
    # selftest on its first run.
    out="$(DISK_GUARD_FAKE_AVAIL_GIB=1 "$0" classify "$tmp/clean" 2>&1 || true)"
    if [[ "$out" == *"NO disk signature in the log"* ]]; then
        echo "selftest ok: shape 5 suspected when space is gone"
    else
        echo "selftest FAIL: shape 5 not suspected at 1G free"; rc=1
    fi
    # ... and the same log with room must come back clean, so the branch is
    # keyed on space rather than firing on every red.
    out="$(DISK_GUARD_FAKE_AVAIL_GIB=20 "$0" classify "$tmp/clean" 2>&1 || true)"
    if [[ "$out" == *"is NOT disk"* ]]; then
        echo "selftest ok: same log with room reads as NOT disk"
    else
        echo "selftest FAIL: shape 5 branch fired with 20G free"; rc=1
    fi
    exit $rc
    ;;
*)
    sed -n '/^# Usage:/,/^set /p' "$0" | sed '$d'
    exit 2
    ;;
esac
