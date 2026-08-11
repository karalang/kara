#!/usr/bin/env python3
"""Generate + build one self-contained .kara per (pattern, rounds, mode).

usage: gen.py <karac-binary> [outdir-suffix]

Kept out of sed's way deliberately: the comparator contains `|x, y|`, which
collides with any `s|...|...|` delimiter.
"""
import os, subprocess, sys, pathlib

K = sys.argv[1] if len(sys.argv) > 1 else '/home/user/kara/target/debug/karac'
SUFFIX = sys.argv[2] if len(sys.argv) > 2 else ''
HERE = pathlib.Path(__file__).parent
os.chdir(HERE)

PATTERNS = {
    'random':        'k = r;',
    'few_unique':    'k = r % 8;',
    'sawtooth':      'k = i % 1000;',
    'sorted':        'k = i;',
    'reverse':       'k = n - i;',
    'nearly_sorted': 'if r % 100 == 0 { k = r; } else { k = i; }',
}
SORTLINE = 'work.sort_by(|x, y| x.0.cmp(y.0));'

tmpl = (HERE / 'bench.tmpl.kara').read_text()
gen = HERE / ('gen' + SUFFIX)
binr = HERE / ('bin' + SUFFIX)
gen.mkdir(exist_ok=True)
binr.mkdir(exist_ok=True)

ok = fail = 0
for p, psrc in PATTERNS.items():
    for rounds in (1, 25):
        for mode in ('sort', 'cloneonly'):
            src = (tmpl.replace('@PATTERN@', psrc)
                       .replace('@ROUNDS@', str(rounds))
                       .replace('@SORT@', SORTLINE if mode == 'sort' else '// clone only'))
            name = f'{p}_{rounds}_{mode}'
            f = gen / f'{name}.kara'
            f.write_text(src)
            r = subprocess.run([K, 'build', str(f), '-o', str(binr / name)],
                               capture_output=True, text=True)
            if r.returncode == 0:
                ok += 1
            else:
                fail += 1
                print(f'FAIL {name}: {(r.stderr or r.stdout).splitlines()[:1]}')
print(f'built {ok} ok, {fail} failed -> {binr}')
