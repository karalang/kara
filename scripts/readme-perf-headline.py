"""The README's Performance numbers, from kara-katas' bench-results.json.

Usage: readme-perf-headline.py <bench-results.json> <kara-katas/scripts>

Per program (kata, approach), Kāra's median run time over each other
language's in the sequential lane (and over rayon in the par lane), with the
equal-safety Rust twin folded by kara-katas' own `normalise` (B-2026-11-16-277).
"""
import json, statistics, sys
sys.path.insert(0, sys.argv[2])
from importlib.machinery import SourceFileLoader
inj = SourceFileLoader("inj", sys.argv[2] + "/inject-bench-readme.py").load_module()
d = json.load(open(sys.argv[1]))
seq = {}  # (kata_id, approach) -> {lang: median_ms}
par = {}
for k in d["katas"]:
    kid = k["kata"]["id"]
    for m in k.get("measurements", []):
        rt = m.get("runtime")
        if not rt:
            continue
        lang, app = inj.normalise(m)
        lane = m.get("lane") or "seq"
        tgt = seq if lane == "seq" else par if lane == "par" else None
        if tgt is None:
            continue
        tgt.setdefault((kid, app), {}).setdefault(lang, rt["median_ms"])
def ratios(table, other):
    return {p: l["kara"] / l[other] for p, l in table.items() if "kara" in l and other in l and l[other] > 0}
r = ratios(seq, "rust")
print("generated", d["generated_at"], "katas", d["kata_count"])
print("seq programs vs rust", len(r), "median %.2f" % statistics.median(r.values()))
for o in ("c", "go", "rust_ovf"):
    ro = ratios(seq, o)
    print(" vs", o, len(ro), "median %.2f" % statistics.median(ro.values()))
over = sorted(((v, p) for p, v in r.items() if v > 2), reverse=True)
print("over 2x", len(over), "worst", [(p[0], p[1], round(v, 1)) for v, p in over[:4]])
pr = ratios(par, "rust")
print("par vs rust(rayon)", len(pr), "median %.2f" % statistics.median(pr.values()), "faster", sum(v < 1 for v in pr.values()), "worst", max(((round(v, 2), p) for p, v in pr.items())))
