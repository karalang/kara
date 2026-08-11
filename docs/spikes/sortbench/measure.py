#!/usr/bin/env python3
"""Slope-based pure-sort timing, per docs/spikes/sort-algorithm-gap.md § Method.

    per_round(mode) = (t[R rounds] - t[1 round]) / (R - 1)
    pure_sort       = per_round(sort) - per_round(cloneonly)

The slope removes process startup AND the one-time input build; subtracting the
clone-only slope removes the per-round base.clone(). Reports the best of N
repeats per point, since this host is a shared container (see Caveats).

usage: measure.py [bindir] [repeats]
"""
import subprocess, sys, pathlib, time

BIN = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'bin')
REPS = int(sys.argv[2]) if len(sys.argv) > 2 else 7
R = 25
PATTERNS = ['random', 'few_unique', 'sawtooth', 'sorted', 'reverse', 'nearly_sorted']


def best(path):
    t = []
    for _ in range(REPS):
        s = time.perf_counter()
        r = subprocess.run([str(path)], capture_output=True, text=True)
        t.append(time.perf_counter() - s)
        if r.returncode != 0:
            raise SystemExit(f'{path} exited {r.returncode}: {r.stderr[:200]}')
    return min(t)


print(f'{"pattern":<15}{"sort/round":>12}{"clone/round":>13}{"PURE SORT":>12}')
print('-' * 52)
out = {}
for p in PATTERNS:
    per = {}
    for mode in ('sort', 'cloneonly'):
        t1 = best(BIN / f'{p}_1_{mode}')
        t25 = best(BIN / f'{p}_25_{mode}')
        per[mode] = (t25 - t1) / (R - 1)
    pure = per['sort'] - per['cloneonly']
    out[p] = pure
    print(f'{p:<15}{per["sort"]*1000:>10.2f}ms{per["cloneonly"]*1000:>11.2f}ms{pure*1000:>10.2f}ms')
print()
for p in PATTERNS:
    print(f'{p}={out[p]*1000:.3f}')
