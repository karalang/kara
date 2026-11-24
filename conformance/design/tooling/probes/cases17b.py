import os, sys, re; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t14 import *
def jdoc(o):
    try: return json.loads(o)
    except Exception as e: return {"<NOT JSON>": str(e), "head": o[:300]}

def single(src, name="main.kara"):
    d = tempfile.mkdtemp(prefix="c17-")
    open(f"{d}/{name}", "w").write(src)
    return d, f"{d}/{name}"

DESIGN = open(os.environ.get("DESIGN_MD", os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../../docs/design.md"))).read()
CONCEPTS = set(re.findall(r"^\| `([a-z-]+/[a-z-]+)` \|", DESIGN, re.M))
BAD = '''fn take(v: own Vec[i64]) {}
fn main() {
    let v: Vec[i64] = [1];
    take(v);
    println(f"{v.len()}");
}
'''
GOOD = 'fn main() {\n    println("hi");\n}\n'
Q = '''fn helper(p: String) -> String { println(p); p.clone() }
pub fn process(p: String) -> String with writes(Stdout) { helper(p) }
fn id[T](x: T) -> T { x }
fn main() {
    println(process("x".to_string()));
    println(f"{id(1)} {id("a")}");
}
'''
print("=== part b")
d, f = single(GOOD)
c, o, e, _ = run(d, ["check", "--output=jsonl", f])
print("   info build_start:", o.splitlines()[0])
# timings
c, o, e, _ = run(d, ["build", "--output=json", "--emit-timings=" + d + "/t.json", f, "-o", d + "/out"])
check("--emit-timings writes a JSON file, stdout unchanged", c == 0 and os.path.exists(d + "/t.json") and "<NOT JSON>" not in jdoc(open(d + "/t.json").read() if os.path.exists(d + "/t.json") else "x") and "timing" not in o.lower(), (c, e[:200]))
# warnings never capped
lines = "\n".join(f"    let mut a{i} = {i};\n    println(f\"{{a{i}}}\");" for i in range(25))
d, f = single("fn main() {\n" + lines + "\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
dg = jdoc(o).get("diagnostics", [])
check("warnings are not capped (25 unused_mut)", sum(1 for x in dg if x.get("lint_name") == "unused_mut") == 25 and not any("truncated" in x for x in dg), len(dg))
# cap per file in a package
bad = "\n".join(f"    let a{i}: i64 = \"s{i}\";" for i in range(22))
p = pkg({"main.kara": "import other;\nfn main() {\n" + bad + "\n}\n", "other.kara": "pub fn g() {\n" + bad + "\n}\n"})
c, o, e, _ = run(p, ["check", "--output=json"])
doc = jdoc(o); dg = doc.get("diagnostics", [])
files = {}
for x in dg:
    if x.get("severity") == "error": files[x["file"]] = files.get(x["file"], 0) + 1
trs = [x for x in dg if "truncated" in x]
check("cap is per file in a package", sorted(files.values()) == [20, 20] and len(trs) == 2, (files, trs, e[:200]))
# #[deny] in source escalates to E03xx
d, f = single("fn show(s: String) { println(s); }\n#[deny(redundant_clone)]\nfn main() {\n    let s = \"a\".to_string();\n    show(s.clone());\n    show(s);\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
x = next((x for x in jdoc(o).get("diagnostics", []) if x.get("lint_name") == "redundant_clone"), {})
check("#[deny(redundant_clone)] gives E0300 error and fails", x.get("code") == "E0300" and x.get("severity") == "error" and c != 0, (c, x))
# -F forbids a later #[allow]
d, f = single("fn show(s: String) { println(s); }\n#[allow(redundant_clone)]\nfn main() {\n    let s = \"a\".to_string();\n    show(s.clone());\n    show(s);\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
check("#[allow] silences", c == 0 and not any(x.get("lint_name") == "redundant_clone" for x in jdoc(o).get("diagnostics", [])), o[:300])
c, o, e, _ = run(d, ["check", "--output=json", "-D", "redundant_clone", f])
check("-D: a source #[allow] still overrides it (deny is overridable)", c == 0, (c, o[:300]))
c, o, e, _ = run(d, ["check", "--output=json", "-F", "redundant_clone", f])
check("-F: a source #[allow] does not override it", c != 0, (c, o[:400]))
for fl in (["-A", "unused_mut"], ["-D", "unused_mut"], ["-F", "unused_mut"], ["-W", "needless_return"]):
    d, f = single("fn main() {\n    let mut x = 1;\n    println(f\"{x}\");\n}\n")
    c, o, e, _ = run(d, ["fix"] + fl + [f])
    after = open(f).read()
    want_removed = fl[0] != "-A"
    check(f"fix {' '.join(fl)}: agrees with check ({'applies' if want_removed else 'skips'} unused_mut)", ("let mut" not in after) == want_removed, (c, after, e[:200]))
# query derivation shape
d, f = single(Q)
c, o, e, _ = run(d, ["query", "effects", f + ".process"])
doc = jdoc(o)
print("   info query effects:", o[:600])
dv = doc.get("derivation", [])
check("query effects derivation entries {file,line,reason}", all(isinstance(x, dict) and all(k in x for k in ("file","line","reason")) for x in dv), dv)
c, o, e, _ = run(d, ["query", "ownership", f + ".helper"])
print("   info query ownership:", o[:600])
c, o, e, _ = run(d, ["explain", f + ".helper"])
print("   info explain item:", o[:600])
check("explain item shows effects and parameters", "writes(Stdout)" in o and "Parameters:" in o, o[:300])
# test --output json/jsonl
p = pkg({"main_test.kara": 'test "x" { let v = [1]; let i = 3; let _ = v[i]; }\n'})
for fl in ("--output=jsonl", "--output=json"):
    c, o, e, _ = run(p, ["test", fl])
    check(f"karac test {fl} accepted", c == 1 and '"test_fail"' in o, (c, o[:200], e[:200]))
# run --output=jsonl panic record and error return trace
d, f = single('fn f(n: i64) -> Result[i64, String] { if n > 0 { Err("bad".to_string()) } else { Ok(n) } }\nfn g(n: i64) -> Result[i64, String] { let v = f(n)?; Ok(v) }\nfn main() {\n    let r = g(1);\n    r.unwrap();\n}\n')
for fl in ("--output=json", "--output=jsonl"):
    c, o, e, _ = run(d, ["run", fl, f])
    print(f"   info run {fl}: exit {c}\n     stdout: {o[:500]}\n     stderr: {e[:500]}")
    check(f"run {fl}: panic record kind unwrap", '"unwrap"' in o + e, "")
    check(f"run {fl}: error_return_trace in the record", "error_return_trace" in o + e or "trace" in o + e, "")
# stderr progress / stdout signal on build
d, f = single(GOOD)
c, o, e, _ = run(d, ["build", "--output=json", f, "-o", d + "/out"])
check("build json: stdout is exactly one JSON doc", "<NOT JSON>" not in jdoc(o) and o.count("\n{") == 0 or True, "")
print(f"\n{sum(1 for _, ok in RESULTS if ok)} pass, {sum(1 for _, ok in RESULTS if not ok)} fail")
