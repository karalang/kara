#!/usr/bin/env sh
# B-2026-08-28-76 (2026-09-10): reproduce the auto-par collapse on HOMOGENEOUS
# Linux cores by taking away the per-thread allocation caching Kara's runtime
# borrows from the host allocator.
#
#   $1  auto-par binary   (karac build <kata>.kara -o u_par)
#   $2  sequential twin   (KARAC_AUTO_PAR=0 karac build <kata>.kara -o u_seq)
#
# `timeit.py` stands in for hyperfine (medians, wait4 rusage, child stdout
# to /dev/null). Interleave the passes -- a single non-interleaved cell
# produced a 2x outlier that three interleaved passes did not reproduce.
set -eu
PAR="$1"; SEQ="$2"; HERE=$(dirname "$0")
T() { python3 "$HERE/timeit.py" --runs 15 --warmup 3 "$@"; }
NT=glibc.malloc.tcache_count=0

for pass in 1 2 3; do
    echo "-- pass $pass --"
    T --label "seq"            "$SEQ"
    T --label "par default"    --env KARAC_PAR_WORKERS=4 "$PAR"
    T --label "par arena1"     --env KARAC_PAR_WORKERS=4 --env MALLOC_ARENA_MAX=1 "$PAR"
    T --label "par tcache0"    --env KARAC_PAR_WORKERS=4 --env GLIBC_TUNABLES=$NT "$PAR"
    # The control that makes it a contention result rather than a slow
    # allocator: MALLOC_ARENA_MAX is a concurrency knob and must not move seq.
    T --label "seq arena1"     --env MALLOC_ARENA_MAX=1 "$SEQ"
done

echo "-- worker sweep, both arena settings --"
for n in 1 2 3 4; do
    T --label "par N=$n default" --env KARAC_PAR_WORKERS=$n "$PAR"
    T --label "par N=$n arena1"  --env KARAC_PAR_WORKERS=$n --env MALLOC_ARENA_MAX=1 "$PAR"
done
