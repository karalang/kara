#!/usr/bin/env python3
"""`karac fix` against kara-lsp's quick fixes (B-2026-11-27-63).

Every Kāra program in the tests/fix_*.rs cells and the Mend examples is run
two ways: `karac fix --dry-run` (its first pass), and every quick fix the
editor offers on the same text. Each karac-fix edit must be an editor edit
at the same line and column with the same text. Then the final files of
`karac fix` and of applying the editor's preferred quick fixes one at a time
are compared; those can differ by application order alone (the CLI applies
a whole pass at once), so they are listed, not failed.

    python3 scripts/fix-parity.py [--karac target/debug/karac]

Exit 1 when a karac-fix edit has no editor counterpart, 2 when nothing was
compared.
"""
import argparse, ast, collections, glob, hashlib, os, re, shutil, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def decode(s):
    s = re.sub(r'\\\n\s*', '', s)
    out, i = [], 0
    simple = {'n': '\n', 't': '\t', '"': '"', '\\': '\\', "'": "'", '0': '\0'}
    while i < len(s):
        if s[i] == '\\' and i + 1 < len(s):
            n = s[i + 1]
            if n in simple:
                out.append(simple[n]); i += 2; continue
            m = re.match(r'\\u\{([0-9a-fA-F]+)\}', s[i:])
            if m:
                out.append(chr(int(m.group(1), 16))); i += len(m.group(0)); continue
        out.append(s[i]); i += 1
    return ''.join(out)


def extract(cells):
    seen = set()
    for f in sorted(glob.glob(os.path.join(ROOT, 'tests/fix_*.rs'))):
        t = open(f).read()
        lits = [m.group(2) for m in re.finditer(r'r(#+)"(.*?)"\1', t, re.S)]
        rest = re.sub(r'r(#+)".*?"\1', '', t, flags=re.S)
        lits += [decode(m.group(1)) for m in re.finditer(r'"((?:[^"\\]|\\.)*)"', rest, re.S)]
        for lit in lits:
            # A program, not a format! template.
            if 'fn ' not in lit or '{' not in lit or '{{' in lit:
                continue
            h = hashlib.md5(lit.encode()).hexdigest()[:8]
            if h in seen:
                continue
            seen.add(h)
            name = os.path.basename(f)[:-3]
            open(os.path.join(cells, f'{name}__{h}.kara'), 'w').write(lit)
    for d in sorted(glob.glob(os.path.join(ROOT, 'examples/mend/examples/*/'))):
        sol = os.path.join(d, 'solution.kara')
        if os.path.exists(sol):
            shutil.copy(sol, os.path.join(cells, f'mend_{os.path.basename(d.rstrip("/"))}.kara'))


def probe(test, env):
    r = subprocess.run(['cargo', 'test', '-q', '-p', 'kara-lsp', '--test', 'fix_parity_probe', test,
                        '--', '--ignored', '--nocapture'], cwd=ROOT, env=env,
                       capture_output=True, text=True)
    if r.returncode != 0:
        sys.exit(f'probe {test} failed:\n{r.stdout}\n{r.stderr}')
    return r.stderr


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--karac', default=os.path.join(ROOT, 'target/debug/karac'))
    karac = os.path.abspath(ap.parse_args().karac)
    work = tempfile.mkdtemp(prefix='kara-fix-parity-')
    cells, fp = os.path.join(work, 'cells'), os.path.join(work, 'fp')
    os.makedirs(cells)
    extract(cells)
    env = dict(os.environ, KARA_PARITY_DIR=cells, KARA_PARITY_OUT=fp, KARAC=karac)
    probe('first_pass', env)
    total = missing = 0
    w = os.path.join(work, 'w')
    for name in sorted(os.listdir(cells)):
        shutil.rmtree(w, ignore_errors=True); os.makedirs(w)
        shutil.copy(os.path.join(cells, name), os.path.join(w, 'main.kara'))
        out = subprocess.run([karac, 'fix', '--dry-run', 'main.kara'], cwd=w,
                             capture_output=True, text=True).stdout
        cli = collections.Counter(
            (int(m.group(1)), int(m.group(2)), m.group(4))
            for m in re.finditer(r'^  main\.kara:(\d+):(\d+): `(.*?)` → `(.*?)`(?=\n  main\.kara:\d+:\d+: |\n*\Z)',
                                 out, re.S | re.M))
        lsp = collections.Counter()
        for line in open(os.path.join(fp, name + '.lsp')).read().splitlines():
            if line:
                l, c, _, n = line.split('\t', 3)
                lsp[(int(l), int(c), ast.literal_eval(n))] += 1
        total += sum(cli.values())
        for (l, c, n) in cli - lsp:
            missing += 1
            print(f'MISS {name}:{l}:{c}: karac fix writes {n!r}, no quick fix does')
    print(probe('parity', env).strip().splitlines()[-1] if True else '')
    print(f'fix-parity: {total} karac fix edits, {missing} with no quick fix')
    shutil.rmtree(work, ignore_errors=True)
    if total == 0:
        sys.exit(2)
    sys.exit(1 if missing else 0)


if __name__ == '__main__':
    main()
