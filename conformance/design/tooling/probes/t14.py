"""design.md §14 probe: each case is a package; runs `karac test` on the JIT and
under --interp, parses the JSONL, and prints a compact verdict line per check."""
import json, os, subprocess, sys, tempfile, shutil, time
K = os.environ["K"]
TOML = '[package]\nname = "pkg"\nversion = "0.1.0"\nauthors = []\nedition = "2026"\n\n[dependencies]\n'
MAIN = "fn add(a: i64, b: i64) -> i64 { a + b }\n\nfn main() {\n    println(add(1, 2));\n}\n"

def pkg(files, toml=TOML):
    d = tempfile.mkdtemp(prefix="c14-")
    os.makedirs(d + "/src")
    open(d + "/kara.toml", "w").write(toml)
    files = dict(files)
    files.setdefault("main.kara", MAIN)
    for n, s in files.items():
        open(f"{d}/src/{n}", "w").write(s)
    return d

def run(d, args, env=None, timeout=120):
    e = dict(os.environ); e.pop("KARAC_TEST_JIT", None)
    for k in list(e):
        if k.startswith("KARA_RESOURCE_") or k == "KARAC_TEST_TIMEOUT_SECS": e.pop(k)
    e.update(env or {})
    t = time.time()
    p = subprocess.run([K] + args, cwd=d, capture_output=True, text=True, env=e, timeout=timeout)
    return p.returncode, p.stdout, p.stderr, time.time() - t

def events(out):
    ev = []
    for l in out.splitlines():
        try: ev.append(json.loads(l))
        except Exception: ev.append({"type": "<NOT JSON>", "line": l})
    return ev

RESULTS = []
def check(name, ok, info=""):
    RESULTS.append((name, ok))
    print(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"   [{info}]" if info and not ok else ""))

def both(d, extra=(), env=None):
    """Runs on the JIT and --interp; returns [(mode, code, events, stderr)]."""
    r = []
    for mode in (["test"], ["test", "--interp"]):
        c, o, e, _ = run(d, mode + list(extra), env)
        r.append((" ".join(mode), c, events(o), e))
    return r

def strip(ev):
    """Events with timings removed, for run-vs-interp comparison."""
    out = []
    for x in ev:
        x = {k: v for k, v in x.items() if k not in ("duration_ms", "elapsed_ms")}
        out.append(x)
    return out

def sig(ev):
    return [(x.get("type"), x.get("test"), x.get("reason")) for x in ev]
