"""Check every `format!` fill against rustc.

For each template `formatfill.fill` fills in, compile the `format!` call
itself with rustc, under the `const`s, `static`s and `let`s that `fill` took
its values from (their source text, not `fill`'s reading of it), and compare
the output with `fill`'s byte for byte. A mismatch, or a call rustc refuses,
is printed with its line and the head of its template.

    python3 scripts/corpus/formatfill_check.py      # needs rustc on PATH
"""
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import extract  # noqa: E402
import formatfill as ff  # noqa: E402
from corpus_lib import ROOT  # noqa: E402

calls = []
orig = ff.fill
def rec(text, start, content, kind, holder_start):
    r = orig(text, start, content, kind, holder_start)
    if r is not None:
        calls.append((text, start, content, kind, holder_start, r))
    return r
ff.fill = rec
tests = ROOT / "tests"
for kind in ("raw", "str"):
    for _ in extract.fixture_programs(tests, kind):
        pass
print("fills", len(calls), file=sys.stderr)

def module(i, text, start, content, kind, holder_start):
    end = ff._lit_end(text, start, content, kind)
    j = end
    while text[j] in " \t\r\n": j += 1
    if text[j] == ")":
        call_end = j + 1
    else:
        _, call_end = ff._split_args(text, j + 1)
    call = "format!(" + text[start:call_end]
    f = ff._FILES[id(text)][1]
    env = ff._env(text, holder_start, start)
    consts, lets, best = [], [], {}
    for (p, kind, name, src), blk in zip(f.defs, f.def_block):
        if blk is not None and not (blk[0] < start < blk[1]):
            continue
        stmt = text[p : text.index(src, p) + len(src)] + ";"
        if kind == "let":
            if p < start and name in env:
                try:
                    ff._value(src, env); lets.append(stmt)
                except ff.Unfillable:
                    pass
        elif name in env:
            d = -1 if blk is None else blk[0]
            if name not in best or best[name][0] < d:
                best[name] = (d, stmt)
    consts = [v[1] for v in best.values()]
    body = "\n".join(lets)
    return (f"#[allow(unused, non_upper_case_globals)] mod m{i} {{\n" + "\n".join(consts) +
            f"\npub fn run() -> String {{\n{body}\n{call}\n}}\n}}\n")

def run(idx):
    src = "".join(module(i, *calls[i][:5]) for i in idx)
    src += "fn main() {\n" + "".join(f'print!("\\x01{i}\\x02{{}}", m{i}::run());\n' for i in idx) + "}\n"
    d = WORK
    Path(d + "/a.rs").write_text(src)
    p = subprocess.run(["rustc", "--edition", "2021", "-A", "warnings", "-o", d + "/a", d + "/a.rs"], capture_output=True, text=True)
    if p.returncode != 0:
        return None, p.stderr
    out = subprocess.run([d + "/a"], capture_output=True, text=True).stdout
    res = {}
    for part in out.split("\x01")[1:]:
        k, v = part.split("\x02", 1); res[int(k)] = v
    return res, None

WORK = tempfile.mkdtemp(prefix="formatfill-check-")
ok = bad = err = 0
report = []
def check(idx):
    global ok, bad, err
    res, e = run(idx)
    if res is None:
        if len(idx) == 1:
            err += 1; report.append(("rustc-error", idx[0], e[:600])); return
        h = len(idx) // 2; check(idx[:h]); check(idx[h:]); return
    for i in idx:
        if res[i] == calls[i][5]: ok += 1
        else:
            bad += 1; report.append(("MISMATCH", i, json.dumps([res[i][:300], calls[i][5][:300]])))
n = len(calls)
for b in range(0, n, 60):
    check(list(range(b, min(n, b + 60))))
print(f"ok {ok} mismatch {bad} rustc-error {err}")
for r in report:
    t, i, msg = r
    text, start = calls[i][0], calls[i][1]
    print("==", t, "line", text.count("\n", 0, start) + 1, repr(calls[i][2][:80]), msg)
sys.exit(1 if bad or err else 0)
