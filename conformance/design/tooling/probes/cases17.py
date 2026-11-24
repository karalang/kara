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
check("concept table parsed", len(CONCEPTS) > 40, len(CONCEPTS))

BAD = '''fn take(v: own Vec[i64]) {}
fn main() {
    let v: Vec[i64] = [1];
    take(v);
    println(f"{v.len()}");
}
'''
d, f = single(BAD)
c, o, e, _ = run(d, ["check", "--output=json", f])
doc = jdoc(o)
check("check --output=json: stdout is one JSON document", "<NOT JSON>" not in doc, doc)
check("check json: nonzero exit on error", c != 0, c)
check("check json: program_effects listed when effect analysis ran clean (an ownership error)", isinstance(doc.get("program_effects"), list), doc.get("program_effects"))
dg = doc.get("diagnostics", [])
check("check json: one diagnostic", len(dg) >= 1, dg)
REQ = {"id": str, "severity": str, "primary": bool, "code": str, "category": str, "concept": str, "file": str, "line": int, "column": int, "message": str, "phase": str}
for x in dg:
    miss = [k for k, t in REQ.items() if not isinstance(x.get(k), t)]
    check(f"diag {x.get('code')}: required fields", not miss, miss)
    check(f"diag {x.get('code')}: concept in the table", x.get("concept") in CONCEPTS, x.get("concept"))
    check(f"diag {x.get('code')}: primary true in v1, no consequence_of", x.get("primary") is True and "consequence_of" not in x, x)
    check(f"diag {x.get('code')}: message does not repeat the code", x.get("code", "") not in x.get("message", ""), x.get("message"))
    for h in x.get("hints", []):
        df = h.get("diff")
        if df is not None:
            check(f"diag {x.get('code')}: hint diff shape", isinstance(h.get("description"), str) and all(k in df for k in ("file","line","old","new")), h)
    if "replacement" in x:
        r = x["replacement"]; check(f"diag {x.get('code')}: replacement shape", all(k in r for k in ("offset","length","text")), r)
    if "fix_diff" in x:
        check(f"diag {x.get('code')}: fix_diff shape", all(all(k in r for k in ("offset","length","text")) for r in x["fix_diff"]), x["fix_diff"])
check("check json: stderr has no JSON", not e.strip().startswith("{"), e[:200])
dg0 = dg[0] if dg else {}
check("ownership diag: E0500-family move code, ownership category", dg0.get("category") == "ownership", dg0)

# passing build
GOOD = 'fn main() {\n    println("hi");\n}\n'
d, f = single(GOOD)
c, o, e, _ = run(d, ["build", "--output=json", f, "-o", d + "/out"])
doc = jdoc(o)
keys = list(doc.keys())
check("build json ok: status first", keys[:1] == ["status"] and doc.get("status") == "ok", keys)
check("build json ok: output last", keys[-1:] == ["output"], keys)
check("build json ok: diagnostics present and empty", doc.get("diagnostics") == [], doc.get("diagnostics"))
check("build json ok: program_effects listed", isinstance(doc.get("program_effects"), list), doc.get("program_effects"))
check("build json ok: exit 0", c == 0, (c, e[:200]))
c, o, e, _ = run(d, ["check", "--output=json", f])
doc = jdoc(o)
print("   info: check json ok keys", list(doc.keys()))
check("check json ok: exit 0", c == 0, c)

# parent_code and lint codes
d, f = single("fn main() {\n    let c = 'a';\n    let b = c as u8;\n    println(f\"{b}\");\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
dg = jdoc(o).get("diagnostics", [])
x = next((x for x in dg if x.get("code") == "E_CHAR_AS_NARROW_INT"), {})
check("E_CHAR_AS_NARROW_INT has parent_code E0210", x.get("parent_code") == "E0210", dg)
d, f = single("fn show(s: String) { println(s); }\nfn main() {\n    let s = \"a\".to_string();\n    show(s.clone());\n    show(s);\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
dg = jdoc(o).get("diagnostics", [])
x = next((x for x in dg if x.get("lint_name") == "redundant_clone"), {})
check("redundant_clone is W0300, parent W0295", x.get("code") == "W0300" and x.get("parent_code") == "W0295" and x.get("severity") == "warning", dg)
check("a warning carries concept", x.get("concept") in CONCEPTS, x.get("concept"))
c, o, e, _ = run(d, ["check", "--output=json", "-D", "redundant_clone", f])
dg = jdoc(o).get("diagnostics", [])
x = next((x for x in dg if x.get("lint_name") == "redundant_clone"), {})
check("-D redundant_clone gives E0300 parent E0295, exit nonzero", x.get("code") == "E0300" and x.get("parent_code") == "E0295" and c != 0, (c, dg))
c, o, e, _ = run(d, ["check", "--output=json", "-D", "warnings", f])
check("-D warnings fails a warning", c != 0, c)
c, o, e, _ = run(d, ["check", "--output=json", "-A", "redundant_clone", f])
check("-A redundant_clone silences it", not any(x.get("lint_name") == "redundant_clone" for x in jdoc(o).get("diagnostics", [])), o[:300])

# cascade cap
lines = "\n".join(f"    let a{i}: i64 = \"s{i}\";" for i in range(25))
d, f = single("fn main() {\n" + lines + "\n}\n")
c, o, e, _ = run(d, ["check", "--output=json", f])
doc = jdoc(o); dg = doc.get("diagnostics", [])
errs = [x for x in dg if x.get("severity") == "error"]
tr = [x for x in dg if "truncated" in x] + ([doc["truncated"]] if "truncated" in doc else [])
check("cascade cap: 20 errors", len(errs) == 20, len(errs))
check("cascade cap: truncated marker with phase and count 5", len(tr) == 1 and tr[0].get("phase") == "typecheck" and tr[0].get("truncated", {}).get("suppressed_primary_errors") == 5 and tr[0]["truncated"].get("file"), tr)
c2, o2, e2, _ = run(d, ["check", "--output=jsonl", f])
ev = events(o2)
trs = [x for x in ev if x.get("type") == "diagnostic" and "truncated" in x]
check("cascade cap: same marker in the stream", len(trs) == 1 and trs[0].get("truncated") == (tr[0].get("truncated") if tr else None), trs)

# jsonl stream
PARSE_BAD = "fn main() {\n    let x = ;\n}\n"
d, f = single(PARSE_BAD)
c, o, e, _ = run(d, ["check", "--output=jsonl", f])
ev = events(o)
ty = [x.get("type") for x in ev]
check("jsonl: every line JSON with type", "<NOT JSON>" not in ty and all(ty), ty)
check("jsonl: lines end in LF, not CRLF", "\r" not in o)
bs = ev[0] if ev else {}
check("jsonl: build_start first with build_id, timestamp, args", bs.get("type") == "build_start" and isinstance(bs.get("build_id"), str) and isinstance(bs.get("timestamp"), str) and ("args" in bs or "arguments" in bs or "cli_args" in bs), bs)
bc = ev[-1] if ev else {}
check("jsonl: build_complete last", bc.get("type") == "build_complete" and all(k in bc for k in ("build_id","success","total_errors","total_warnings","program_effects")), bc)
check("jsonl: build_id matches", bc.get("build_id") == bs.get("build_id"))
sk = [x for x in ev if x.get("type") == "phase_skipped"]
dids = [x.get("id") for x in ev if x.get("type") == "diagnostic"]
check("jsonl: parse error skips resolve, typecheck, effect, ownership", {"resolve","typecheck","effect","ownership"} <= {x.get("phase") for x in sk}, [x.get("phase") for x in sk])
check("jsonl: phase_skipped has reason and blocking ids", sk and all(isinstance(x.get("reason"), str) and x.get("blocking") and set(x["blocking"]) <= set(dids) for x in sk), sk[:2])
pc = [x for x in ev if x.get("type") == "phase_complete"]
check("jsonl: total_errors == sum of phase_complete errors", bc.get("total_errors") == sum(x.get("errors", 0) for x in pc), (bc.get("total_errors"), [(x["phase"], x.get("errors")) for x in pc]))
check("jsonl: phase_complete fields", all(all(k in x for k in ("phase","errors","warnings","notes")) for x in pc), pc[:2])
check("jsonl: diagnostic events carry phase", all(x.get("phase") for x in ev if x.get("type") == "diagnostic"))
starts = [x.get("phase") for x in ev if x.get("type") == "phase_start"]
check("jsonl: lex and parse start", starts[:2] == ["lex", "parse"], starts)
c3, o3, e3, _ = run(d, ["check", "--output=json", f])
bd = jdoc(o3).get("diagnostics", [])
sd = [{k: v for k, v in x.items() if k != "type"} for x in ev if x.get("type") == "diagnostic"]
check("jsonl reconstructs batch diagnostics", bd == sd, (bd[:1], sd[:1]))
check("parse error: program_effects null", jdoc(o3).get("program_effects", 0) is None, jdoc(o3).get("program_effects", 0))
check("jsonl program_effects == batch", bc.get("program_effects") == jdoc(o3).get("program_effects"))
for src, nm in [(BAD, "ownership"), (GOOD, "ok")]:
    d, f = single(src)
    c, o, e, _ = run(d, ["check", "--output=jsonl", f])
    ev = events(o); bc = ev[-1] if ev else {}
    pc = [x for x in ev if x.get("type") == "phase_complete"]
    check(f"jsonl ({nm}): total_errors == sum", bc.get("total_errors") == sum(x.get("errors", 0) for x in pc), (bc, [(x['phase'], x['errors']) for x in pc]))
    c3, o3, e3, _ = run(d, ["check", "--output=json", f])
    bd = jdoc(o3).get("diagnostics", [])
    sd = [{k: v for k, v in x.items() if k != "type"} for x in ev if x.get("type") == "diagnostic"]
    check(f"jsonl ({nm}) reconstructs batch", bd == sd, (bd[:1], sd[:1]))
    check(f"jsonl ({nm}) exit == batch exit", c == c3, (c, c3))
d, f = single(GOOD)
c, o, e, _ = run(d, ["build", "--output=jsonl", f, "-o", d + "/out"])
ev = events(o)
check("build jsonl: codegen phase", any(x.get("type") == "phase_complete" and x.get("phase") == "codegen" for x in ev), [x.get("phase") for x in ev])
check("build jsonl: build_complete has output", ev and ev[-1].get("type") == "build_complete" and ev[-1].get("output"), ev[-1:] )

# query
Q = '''fn helper(p: String) -> String { println(p); p.clone() }
pub fn process(p: String) -> String with writes(Stdout) { helper(p) }
fn id[T](x: T) -> T { x }
fn main() {
    println(process("x".to_string()));
    println(f"{id(1)} {id("a")}");
}
'''
d, f = single(Q)
for qa in (["effects", f + ".process"], ["effects", f + ".helper"], ["ownership", f + ".helper"], ["monomorphization", f], ["queries", f]):
    c, o, e, _ = run(d, ["query"] + qa)
    doc = jdoc(o)
    check(f"query {qa[0]} {os.path.basename(qa[-1])}: JSON, exit 0", c == 0 and "<NOT JSON>" not in doc, (c, o[:200], e[:200]))
    if qa[0] == "effects":
        check(f"query effects {qa[1].rsplit('.',1)[1]}: derivation", isinstance(doc.get("derivation"), list), list(doc.keys()) if isinstance(doc, dict) else doc)
    if qa[0] == "ownership":
        check("query ownership: derivation", isinstance(doc.get("derivation"), list) or any("derivation" in str(v) for v in (doc.values() if isinstance(doc, dict) else [])), str(doc)[:300])
    if qa[0] == "monomorphization":
        bg = doc.get("by_generic", [])
        idg = next((g for g in bg if g.get("generic", "").endswith("id")), {})
        check("query monomorphization: shape", doc.get("scope") is not None and isinstance(bg, list) and "totals" in doc and all(k in doc["totals"] for k in ("generic_count","instance_count")), list(doc.keys()))
        check("query monomorphization: id has 2 instances with types and site", idg.get("instance_count") == 2 and all("types" in i and "site" in i for i in idg.get("instances", [])), idg)

# explain
for a in ["E0500", "W0300", "W0295", "redundant_clone", "ownership/move-semantics", f + ".process", "E_CHAR_AS_NARROW_INT"]:
    c, o, e, _ = run(d, ["explain", a])
    check(f"explain {os.path.basename(a)}: exit 0, text", c == 0 and len(o.strip()) > 20, (c, o[:150], e[:150]))
c, o, e, _ = run(d, ["explain", "ownership/move-semantics"])
check("explain concept names its codes", "E05" in o, o[:300])
c, o, e, _ = run(d, ["explain", "E9999"])
check("explain unknown code: nonzero", c != 0, (c, o[:150]))

# fix
FX = '''fn show(s: String) { println(s); }
fn f(x: i64) -> i64 {
    return x + 1;
}
fn main() {
    let mut n = 1;
    let s = "a".to_string();
    show(s.clone());
    show(s);
    println(f"{f(n)}");
}
'''
d, f = single(FX)
c, o, e, _ = run(d, ["fix", f])
a1 = open(f).read()
check("fix: applied unused_mut and redundant_clone", "let mut" not in a1 and ".clone()" not in a1, a1)
check("fix: style lint off by default (needless_return kept)", "return x + 1;" in a1, a1)
c, o, e, _ = run(d, ["fix", f]); a2 = open(f).read()
check("fix: idempotent", a1 == a2, (a1, a2))
c, o, e, _ = run(d, ["check", "--output=json", f])
check("check after fix: no machine-applicable left", not any(x.get("replacement") or x.get("fix_diff") for x in jdoc(o).get("diagnostics", [])), o[:300])
c, o, e, _ = run(d, ["fix", "-W", "needless_return", f]); a3 = open(f).read()
check("fix -W needless_return applies it", "return x + 1;" not in a3 and "x + 1" in a3, a3)
c, o, e, _ = run(d, ["fix", "-W", "needless_return", f]); a4 = open(f).read()
check("fix -W idempotent", a3 == a4)
d, f = single(FX)
c, o, e, _ = run(d, ["check", "--output=json", "-W", "needless_return", f])
nfix = sum(1 for x in jdoc(o).get("diagnostics", []) if x.get("replacement") or x.get("fix_diff"))
check("check -W needless_return reports it with a fix", any(x.get("lint_name") == "needless_return" and (x.get("replacement") or x.get("fix_diff")) for x in jdoc(o).get("diagnostics", [])), o[:400])

# fmt
UGLY = "fn main(){\nlet x=1;\n    println(f\"{x}\");}\n"
d, f = single(UGLY)
c, o, e, _ = run(d, ["fmt", f])
check("fmt file: prints to stdout, file unchanged", c == 0 and open(f).read() == UGLY and o.strip() and o != UGLY, (c, o[:200]))
c2, o2, e2, _ = run(d, ["fmt", "--write", f])
check("fmt --write writes the same text", open(f).read() == o, (open(f).read(), o))
c3, o3, e3, _ = run(d, ["fmt", f])
check("fmt idempotent", o3 == o)
p = pkg({"main.kara": UGLY, "b.kara": "fn b()->i64{1}\n"})
c, o, e, _ = run(p, ["fmt", "--check"])
check("fmt --check in a package: exit 1, lists both files, writes nothing", c == 1 and "main.kara" in o + e and "b.kara" in o + e and open(p + "/src/main.kara").read() == UGLY, (c, o, e))
c, o, e, _ = run(p, ["fmt"])
check("fmt in a package: formats in place", c == 0 and open(p + "/src/main.kara").read() != UGLY, (c, o, e))
c, o, e, _ = run(p, ["fmt", "--check"])
check("fmt --check clean: exit 0", c == 0, (c, o, e))
p2 = pkg({"main.kara": UGLY})
c, o, e, _ = run(p2, ["fmt", "src"])
check("fmt on a directory: in place", c == 0 and open(p2 + "/src/main.kara").read() != UGLY, (c, o, e))
d, f = single("struct S {}\nimpl S {\n    fn b(ref self) {}\n    fn new() -> S { S {} }\n    fn a(ref self) {}\n}\nfn main() {}\n")
c, o, e, _ = run(d, ["fmt", f])
check("fmt orders impl: constructors then methods alphabetically", o.find("fn new") < o.find("fn a(") < o.find("fn b("), o)
d, f = single("import std.time.{sleep};\nimport std.fs;\nfn main() { let a = 1 + 2; let b = 2 + 1; println(f\"{a}{b}\"); }\n")
c, o, e, _ = run(d, ["fmt", f])
check("fmt sorts imports, keeps a + b / b + a", o.find("import std.fs") < o.find("import std.time") and "1 + 2" in o and "2 + 1" in o, o)

# lint list, unknown lint, unknown attribute, expect
c, o, e, _ = run(d, ["lint", "--list"])
for n in ["deprecated","mutual_recursion_note","redundant_suffix","float_in_serialized_type","undocumented_unsafe","unfulfilled_lint_expectation","unknown_lint","unused_effect","lossless_cast","unused_mut","redundant_clone","vec_per_element","counter_while_loop","manual_compound_assign","needless_return","redundant_literal_method","redundant_ref_param","qualified_prelude_variant"]:
    check(f"lint --list names {n}", n in o, (c, o[:100]))
for src, want, sev in [
  ("#[allow(no_such_lint)]\nfn main() {}\n", "unknown_lint", "warning"),
  ("#[no_such_attr]\nfn main() {}\n", "E_UNKNOWN_ATTRIBUTE", "error"),
  ("#[allow(unused_mut, unused_mut)]\nfn main() {}\n", "E_DUPLICATE_LINT_LEVEL", "error"),
  ("fn main() {\n    #[expect(unused_mut)]\n    let x = 1;\n    println(f\"{x}\");\n}\n", "unfulfilled_lint_expectation", "warning"),
  ("#[expect(unfulfilled_lint_expectation)]\nfn main() {}\n", "E_EXPECT_ON_UNFULFILLED", "error"),
  ("#[diagnostic::polish_me]\nfn main() {}\n", None, None),
  ("#[tool::name]\nfn main() {}\n", "error", "error"),
  ("#![allow(unused_mut)]\nfn main() {\n    let mut x = 1;\n    println(f\"{x}\");\n}\n", None, None),
]:
    d, f = single(src)
    c, o, e, _ = run(d, ["check", "--output=json", f])
    dg = jdoc(o).get("diagnostics", [])
    if want is None:
        check(f"accepted silently: {src.splitlines()[0]}", c == 0 and not dg, (c, dg))
    elif want == "error":
        check(f"refused: {src.splitlines()[0]}", c != 0 and any(x.get("severity") == "error" for x in dg), (c, dg))
    else:
        x = next((x for x in dg if want in (x.get("code",""), x.get("lint_name",""))), None)
        check(f"{want}: reported as {sev}", x is not None and x.get("severity") == sev, dg)

# panic record from run --output=json
d, f = single('fn main() {\n    let v = [1];\n    let i = 3;\n    println(f"{v[i]}");\n}\n')
c, o, e, _ = run(d, ["run", "--output=json", f])
check("run panic: exit 101", c == 101, (c, e[:200]))
blob = o + e
check("run --output=json carries a panic record with kind and location", "index_out_of_bounds" in blob and '"kind"' in blob, (o[:300], e[:300]))
c, o, e, _ = run(d, ["run", f])
check("run text mode: no JSON panic record", '"kind"' not in e, e[:300])

# env vars
HS = 'fn main() {\n    let mut m: Map[String, i64] = Map.new();\n    for k in ["a", "b", "c", "d", "e", "f"] { m.insert(k.to_string(), 1); }\n    let mut s = "".to_string();\n    for (k, _) in m { s.push_str(k); }\n    println(s);\n}\n'
d, f = single(HS)
c, o1, e, _ = run(d, ["run", f], {"KARAC_HASH_SEED": "7"})
c, o2, e, _ = run(d, ["run", f], {"KARAC_HASH_SEED": "7"})
c, o3, e, _ = run(d, ["build", f, "-o", d + "/hs"])
o4 = subprocess.run([d + "/hs"], capture_output=True, text=True, env={**os.environ, "KARAC_HASH_SEED": "7"}).stdout
o5 = subprocess.run([d + "/hs"], capture_output=True, text=True, env={**os.environ, "KARAC_HASH_SEED": "7"}).stdout
check("KARAC_HASH_SEED pins run order", o1 == o2 and o1.strip(), (o1, o2))
check("KARAC_HASH_SEED: build == run", o4 == o1 and o5 == o4, (o1, o4))
outs = {subprocess.run([d + "/hs"], capture_output=True, text=True).stdout for _ in range(8)}
check("unpinned order varies", len(outs) > 1, outs)
p = subprocess.run([d + "/hs"], capture_output=True, text=True, env={**os.environ, "KARAC_HASH_SEED": "abc"})
check("malformed KARAC_HASH_SEED warns on stderr", "KARAC_HASH_SEED" in p.stderr, p.stderr[:200])
p = subprocess.run([d + "/hs"], capture_output=True, text=True, env={**os.environ, "KARAC_THREADS": "0"})
check("KARAC_THREADS=0 warns on stderr", "KARAC_THREADS" in p.stderr, p.stderr[:200])
c, o, e, _ = run(d, ["run", f], {"KARAC_HASH_SEED": "abc"})
check("run: malformed KARAC_HASH_SEED warns", "KARAC_HASH_SEED" in e, e[:200])

print(f"\n{sum(1 for _, ok in RESULTS if ok)} pass, {sum(1 for _, ok in RESULTS if not ok)} fail")
