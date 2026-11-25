"""design.md § 17 Tooling contract, the sentences cases17.py and cases17b.py
leave open (2026-11-24 sentence-map audit). `K=<karac> python3 cases17c.py`."""
import os, sys, re; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from t14 import *

def jdoc(o):
    try: return json.loads(o)
    except Exception as e: return {"<NOT JSON>": str(e), "head": o[:300]}

def single(src, name="main.kara"):
    d = tempfile.mkdtemp(prefix="c17c-")
    open(f"{d}/{name}", "w").write(src)
    return d, f"{d}/{name}"

DESIGN = open(os.environ.get("DESIGN_MD", os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../../docs/design.md"))).read()
CONCEPTS = set(re.findall(r"^\| `([a-z-]+/[a-z-]+)` \|", DESIGN, re.M))

def check_json(src, extra=()):
    d, f = single(src)
    c, o, e, _ = run(d, ["check", "--output=json", *extra, f])
    return c, jdoc(o), e, f

def diags(doc):
    return [x for x in doc.get("diagnostics", []) if "code" in x]

def codes(doc):
    return [x.get("code") for x in diags(doc)]

def lint_names(doc):
    return [x.get("lint_name") for x in diags(doc)]

# ---- Structured compiler output -------------------------------------------

# The text renderer's header names the symbolic code.
d, f = single('fn main() {\n    let c = \'a\';\n    let b = c as u8;\n    println(f"{b}");\n}\n')
c, o, e, _ = run(d, ["check", f])
check("text header names the symbolic code", "error[E_CHAR_AS_NARROW_INT]" in (o + e), (o + e)[:300])

# A public function's incomplete effect declaration: inferred, declared,
# derivation, and a fix in the one shape.
PUBFX = '''fn log_it(s: String) { println(s); }
pub fn process(s: String) -> i64 with reads(Env) {
    log_it(s);
    1
}
fn main() { println(f"{process("x".to_string())}"); }
'''
c, doc, e, f = check_json(PUBFX)
e400 = [x for x in diags(doc) if x.get("category") == "effects"]
check("effect declaration error reported", bool(e400), codes(doc))
if e400:
    x = e400[0]
    check("effect diag: inferred_effects and declared_effects", isinstance(x.get("inferred_effects"), list) and isinstance(x.get("declared_effects"), list), x)
    der = x.get("derivation") or []
    check("effect diag: derivation of {file,line,reason}", bool(der) and all(set(["file", "line", "reason"]) <= set(y) for y in der), der)
    hs = [h for h in x.get("hints", []) if h.get("diff")]
    check("effect diag: hint diff names old and new lines", bool(hs) and all(set(["file", "line", "old", "new"]) <= set(h["diff"]) for h in hs), x.get("hints"))
    check("effect diag: byte edits beside the hint (replacement or fix_diff)", "replacement" in x or "fix_diff" in x, sorted(x))
    check("effect diag: concept in the table", x.get("concept") in CONCEPTS, x.get("concept"))

# Every batch diagnostic carries `phase`.
c, doc, e, f = check_json('fn main() {\n    let mut x = 1;\n    let y: String = 2;\n}\n')
e05 = [x for x in diags(doc) if x.get("code") == "E0200"]
check("a record with one code has no parent_code", bool(e05) and all("parent_code" not in x for x in e05), e05)
check("every batch diagnostic carries phase", diags(doc) and all("phase" in x for x in diags(doc)), diags(doc))

# A package's checks have concepts too: a manifest error.
dd = tempfile.mkdtemp(prefix="c17c-pkg-")
os.makedirs(dd + "/src")
open(dd + "/kara.toml", "w").write('[package]\nname = "pkg"\nversion = "0.1.0"\nedition = "2026"\nbogus_key = 1\n\n[dependencies]\nnosuch = { path = "../does-not-exist" }\n')
open(dd + "/src/main.kara", "w").write('fn main() {\n    let mut x = 1;\n    println(f"{x}");\n}\n')
c, o, e, _ = run(dd, ["check", "--output=json"])
pdoc = jdoc(o)
check("package check: stdout one JSON document", "<NOT JSON>" not in pdoc, o[:300])
pd = diags(pdoc)
check("package check: every diagnostic has a concept in the table", bool(pd) and all(x.get("concept") in CONCEPTS for x in pd), [(x.get("code"), x.get("concept")) for x in pd])

# A package reports a fix in the same shape as a file.
dd2 = pkg({"main.kara": 'fn main() {\n    let mut x = 1;\n    println(f"{x}");\n}\n'})
c, o, e, _ = run(dd2, ["check", "--output=json"])
pdoc = jdoc(o)
um = [x for x in diags(pdoc) if x.get("lint_name") == "unused_mut"]
check("package: unused_mut fix has replacement {offset,length,text} and a hint diff",
      bool(um) and set(["offset", "length", "text"]) <= set((um[0].get("replacement") or {})) and any(h.get("diff") for h in um[0].get("hints", [])), um)

# ---- Signal on stdout, noise on stderr --------------------------------------
d, f = single('fn main() {\n    println("hi");\n}\n')
c, o, e, _ = run(d, ["build", "--output=json", f])
check("build json: stdout one document, progress not on stdout", "<NOT JSON>" not in jdoc(o) and "Compiling" not in o, o[:200])

# ---- Streaming: which phases run after which failure -----------------------
def stream(src, cmd="check"):
    d, f = single(src)
    c, o, e, _ = run(d, [cmd, "--output=jsonl", f])
    return [json.loads(l) for l in o.splitlines() if l.strip().startswith("{")]

def phases(ev):
    done = {x["phase"]: "ran" for x in ev if x.get("type") == "phase_complete"}
    for x in ev:
        if x.get("type") == "phase_skipped":
            done[x["phase"]] = "skipped"
    return done

ev = stream('fn main() {\n    let x = undefined_name;\n}\n')
ph = phases(ev)
check("resolve error: typecheck skipped", ph.get("typecheck") == "skipped", ph)
check("resolve error: ownership skipped (typecheck did not run)", ph.get("ownership") == "skipped", ph)
check("resolve error: effect runs (it needs only parse)", ph.get("effect") == "ran", ph)
check("resolve error: lint runs", ph.get("lint") == "ran", ph)
check("resolve error: simd_check and comptime run", ph.get("simd_check") == "ran" and ph.get("comptime") == "ran", ph)

ev = stream('fn main() {\n    let x: String = 1;\n    println(x);\n}\n')
ph = phases(ev)
check("type error: ownership still runs", ph.get("ownership") == "ran", ph)

ev = stream('fn main() {\n    let x: String = 1;\n    println(x);\n}\n', "build")
ph = phases(ev)
check("build with a type error: codegen does not run", ph.get("codegen") != "ran", ph)
starts = [x["phase"] for x in ev if x.get("type") == "phase_start"]
pairs = all(p in ph for p in starts)
check("every phase_start is closed by phase_complete or phase_skipped", pairs, (starts, ph))

d, f = single('fn main() { }\n')
c, o, e, _ = run(d, ["check", "--stop-on-first-error", f])
check("there is no --stop-on-first-error flag", c != 0, (c, e[:200]))

# ---- Error return traces ----------------------------------------------------
def run_json(src, extra=(), args=None):
    d, f = single(src)
    c, o, e, _ = run(d, args or ["run", "--output=json", *extra, f])
    return c, o, e

OPT = '''fn first(v: Vec[i64]) -> Option[i64] { let x = v.get(5)?; Some(x) }
fn main() { let v: Vec[i64] = [1]; println(f"{first(v).unwrap()}"); }
'''
d, f = single(OPT)
c, o, e, _ = run(d, ["run", f])
check("Option ?: no Error return trace on the panic", c == 101 and "Error return trace" not in e, (c, e[:300]))

HANDLED = '''fn f(x: i64) -> Result[i64, String] { if x > 0 { Ok(x) } else { Err("neg") } }
fn g(x: i64) -> Result[i64, String] { let y = f(x)?; Ok(y) }
fn main() {
    match g(-1) {
        Ok(v) => println(f"{v}"),
        Err(m) => println(m),
    }
    let v: Vec[i64] = [];
    println(f"{v[3]}");
}
'''
d, f = single(HANDLED)
c, o, e, _ = run(d, ["run", f])
check("a handled error ends its trace", c == 101 and "Error return trace" not in e, (c, e[:300]))

DROPPED = '''fn f(x: i64) -> Result[i64, String] { if x > 0 { Ok(x) } else { Err("neg") } }
fn g(x: i64) -> Result[i64, String] { let y = f(x)?; Ok(y) }
fn main() {
    {
        let r = g(-1);
        println(f"{r.is_ok()}");
    }
    let v: Vec[i64] = [];
    println(f"{v[3]}");
}
'''
d, f = single(DROPPED)
c, o, e, _ = run(d, ["run", f])
check("a dropped error ends its trace", c == 101 and "Error return trace" not in e, (c, e[:300]))

UNWRAP = '''fn f(x: i64) -> Result[i64, String] { if x > 0 { Ok(x) } else { Err("neg") } }
fn g(x: i64) -> Result[i64, String] { let y = f(x)?; Ok(y) }
fn main() { println(f"{g(-1).unwrap()}"); }
'''
d, f = single(UNWRAP)
c, o, e, _ = run(d, ["run", f])
check("text mode: the trace is a note on stderr", c == 101 and "Error return trace" in e, e[:300])

MAPERR = '''fn f(x: i64) -> Result[i64, String] { if x > 0 { Ok(x) } else { Err("neg") } }
fn g(x: i64) -> Result[i64, i64] { let y = f(x).map_err(|e| e.len() as i64)?; Ok(y) }
fn main() { println(f"{g(-1).unwrap()}"); }
'''
c, o, e = run_json(MAPERR)
tr = jdoc(o).get("error_return_trace") or []
fr = tr.get("frames", []) if isinstance(tr, dict) else tr
check("map_err keeps the trace for the next ?", c == 101 and any("map_err" in (x.get("expr") or "") or x.get("line") == 2 for x in fr), (o[:400]))

DEEP = '''fn down(n: i64) -> Result[i64, String] {
    if n == 0 { return Err("bottom"); }
    let x = down(n - 1)?;
    Ok(x)
}
fn main() { println(f"{down(80).unwrap()}"); }
'''
c, o, e = run_json(DEEP)
doc = jdoc(o)
tr = doc.get("error_return_trace")
check("depth limit: a full buffer reports {frames, truncated: true}",
      isinstance(tr, dict) and tr.get("truncated") is True and len(tr.get("frames", [])) <= 64, str(tr)[:300])

d, f = single(UNWRAP)
c, o, e, _ = run(d, ["build", "--release", f, "-o", d + "/rel"])
if c == 0:
    p = subprocess.run([d + "/rel"], capture_output=True, text=True)
    check("--release: no error return trace", p.returncode == 101 and "Error return trace" not in p.stderr, p.stderr[:300])
else:
    check("--release build", False, e[:300])

# ---- Panic record -----------------------------------------------------------
KINDS = {
    "explicit": 'fn main() { panic("boom"); }',
    "index_out_of_bounds": 'fn main() { let v: Vec[i64] = [1]; let i = v.len() + 2; println(f"{v[i]}"); }',
    "overflow": 'fn main() { let mut x: i8 = 120; let n = env.args().len() as i8; x = x + 10 + n; println(f"{x}"); }',
    "div_by_zero": 'fn main() { let z = env.args().len() as i64 - 1; println(f"{10 / z}"); }',
    "unwrap": 'fn main() { let o: Option[i64] = None; println(f"{o.unwrap()}"); }',
    "assertion": 'fn main() { let n = env.args().len(); assert(n == 7); }',
    "unreachable": 'fn main() { unreachable(); }',
}
for kind, src in KINDS.items():
    c, o, e = run_json(src + "\n")
    rec = (jdoc(o).get("runtime_errors") or [{}])[0]
    check(f"panic record kind {kind}", c == 101 and rec.get("kind") == kind, (c, o[:200], e[:200]))
    loc = rec.get("location") or {}
    check(f"panic record {kind}: location file/line/col", all(k in loc for k in ("file", "line", "col")), loc)

c, o, e = run_json('fn main() { panic(); }\n')
rec = (jdoc(o).get("runtime_errors") or [{}])[0]
check("a bare panic() records the message \"explicit panic\"", rec.get("message") == "explicit panic", rec)

BAG = '''shared struct Bag { mut items: Vec[i64] }
fn add_all(src: Bag, dst: Bag) {
    for x in src.items {
        dst.items.push(x);
    }
}
fn main() {
    let b = Bag { items: Vec.new() };
    b.items.push(1);
    add_all(b, b);
}
'''
c, o, e = run_json(BAG)
rec = (jdoc(o).get("runtime_errors") or [{}])[0]
check("panic record kind borrow_conflict", c == 101 and rec.get("kind") == "borrow_conflict", rec)

c, o, e = run_json('fn main() {\n    let o: Option[i64] = None;\n    println(f"{o.unwrap()}");\n}\n')
rec = (jdoc(o).get("runtime_errors") or [{}])[0]
check("track_caller: unwrap's location is the caller's line", (rec.get("location") or {}).get("line") == 3, rec)

d, f = single('fn main() { panic("boom"); }\n')
c, o, e, _ = run(d, ["build", f, "-o", d + "/bin"])
if c == 0:
    p = subprocess.run([d + "/bin"], capture_output=True, text=True)
    check("built binary run directly: exit 101, human report, no JSON", p.returncode == 101 and "boom" in p.stderr and '"kind"' not in p.stderr + p.stdout, (p.returncode, p.stderr[:200]))
else:
    check("build for direct run", False, e[:300])

HANDLER = '''#[panic_handler]
fn on_panic(info: PanicInfo) {
    eprintln("handler ran");
}
fn main() { panic("boom"); }
'''
d, f = single(HANDLER)
c, o, e, _ = run(d, ["run", f])
check("custom panic handler runs after the record, exit 101", c == 101 and "boom" in e and "handler ran" in e and e.find("boom") < e.find("handler ran"), (c, e[:300]))

# ---- karac fix style lints --------------------------------------------------
STYLE = {
    "counter_while_loop": ('fn main() {\n    let mut i = 0;\n    while i < 3 {\n        println(f"{i}");\n        i += 1;\n    }\n}\n', "for i in 0..3"),
    "manual_compound_assign": ('fn main() {\n    let mut x = 1;\n    x = x + 2;\n    println(f"{x}");\n}\n', "x += 2"),
    "redundant_literal_method": ('fn main() {\n    let s: String = "a".to_string();\n    println(s);\n}\n', None),
    "redundant_ref_param": ('fn f(x: ref String) -> i64 { x.len() }\nfn main() {\n    let s = "a".to_string();\n    println(f"{f(s)}");\n}\n', "fn f(x: String)"),
    "qualified_prelude_variant": ('fn main() {\n    let o = Option.Some(1);\n    match o {\n        Option.Some(v) => println(f"{v}"),\n        Option.None => println("none"),\n    }\n}\n', "Some(v) =>"),
    "unused_variable": ('fn main() {\n    let x = 1;\n    println("hi");\n}\n', "let _x = 1"),
}
for lint, (src, want) in STYLE.items():
    d, f = single(src)
    c, o, e, _ = run(d, ["check", "--output=json", f])
    off = lint not in [x.get("lint_name") for x in diags(jdoc(o))]
    check(f"{lint}: off by default in check", off, o[:200])
    c, o, e, _ = run(d, ["check", "--output=json", "-W", lint, f])
    on = [x for x in diags(jdoc(o)) if x.get("lint_name") == lint]
    check(f"{lint}: -W reports it with a fix", bool(on) and ("replacement" in on[0] or "fix_diff" in on[0]), o[:300])
    c, o, e, _ = run(d, ["fix", "-W", lint, f])
    new = open(f).read()
    check(f"{lint}: fix -W applies it", new != src and (want is None or want in new), new)

UNUSED_NOT = '''struct P { a: i64, b: i64 }
enum E { A(i64), B(i64) }
fn f(param: i64) -> i64 { 1 }
fn main() {
    let mut assigned = 0;
    assigned = 3;
    let p = P { a: 1, b: 2 };
    let P { a, b: _ } = p;
    let e = E.A(1);
    match e {
        E.A(n) | E.B(n) => println("x"),
    }
    match 5 {
        whole @ 1..=9 => println("y"),
        _ => println("z"),
    }
    let c = |unused_closure_param: i64| 1;
    println(f"{f(1)} {c(2)}");
}
'''
c, doc, e, f = check_json(UNUSED_NOT, ["-W", "unused_variable"])
named = [x.get("message", "") for x in diags(doc) if x.get("lint_name") == "unused_variable"]
for nm in ["assigned", "param", "`a`", "`n`", "whole", "unused_closure_param"]:
    check(f"unused_variable not reported: {nm}", not any(nm in m for m in named), named)

# ---- explain -----------------------------------------------------------------
EX = '''struct Acc { n: i64 }
impl Acc {
    fn bump(mut ref self) { self.n += 1; println("b"); }
}
pub fn declared() with writes(Stdout) { inner(); }
fn inner() { println("i"); }
fn main() { let mut a = Acc { n: 0 }; a.bump(); declared(); }
'''
d, f = single(EX)
c, o, e, _ = run(d, ["explain", f"{f}.Acc.bump"])
check("explain <file>.Type.method: effects", c == 0 and "writes(Stdout)" in o, (c, o[:300], e[:200]))
c, o, e, _ = run(d, ["explain", f"{f}.declared"])
check("explain: a with clause ends the derivation at the clause", c == 0 and "inner" not in o.split("Effects:")[-1].split("Parameters:")[0], o[:400])

# ---- Canonical formatting ---------------------------------------------------
FMT = '''trait T {
    fn zeta(self) -> i64;
    fn alpha(self) -> i64;
    fn make() -> Self;
    type Item;
}
struct S { x: i64 }
impl S {
    /// Doc of zeta.
    #[must_use]
    fn zeta(self) -> i64 { 1 }
    fn new() -> S { S { x: 1 } }
    // A comment on alpha.
    fn alpha(self) -> i64 { 2 }
}
fn main() { }
'''
d, f = single(FMT)
c, o, e, _ = run(d, ["fmt", f])
t_body = o[o.find("trait T"):o.find("struct S")]
check("fmt trait: associated types first, then no-self fns, then methods alphabetically",
      0 <= t_body.find("type Item") < t_body.find("fn make") < t_body.find("fn alpha") < t_body.find("fn zeta"), t_body)
i_body = o[o.find("impl S"):]
check("fmt impl: doc comment and attribute travel with their item",
      i_body.find("/// Doc of zeta.") < i_body.find("#[must_use]") < i_body.find("fn zeta") and i_body.find("fn alpha") < i_body.find("/// Doc of zeta."), i_body)
check("fmt impl: a comment travels with its item", i_body.find("// A comment on alpha.") < i_body.find("fn alpha") < i_body.find("fn zeta"), i_body)

# ---- Lint levels -------------------------------------------------------------
c, doc, e, f = check_json('fn main() {\n    #[allow(unused_mut)]\n    let mut a = 1;\n    let mut b = 2;\n    println(f"{a} {b}");\n}\n')
um = [x for x in diags(doc) if x.get("lint_name") == "unused_mut"]
check("#[allow] on a statement: only that statement", len(um) == 1 and um[0].get("line") == 4, [(x.get("line")) for x in um])

c, doc, e, f = check_json('#[allow(unused_mut)]\nfn main() {\n    #[warn(unused_mut)]\n    let mut a = 1;\n    let mut b = 2;\n    println(f"{a} {b}");\n}\n')
um = [x for x in diags(doc) if x.get("lint_name") == "unused_mut"]
check("outer allow cascades; inner warn overrides for its scope", len(um) == 1 and um[0].get("line") == 4, [(x.get("line")) for x in um])

c, doc, e, f = check_json('#[allow(unused_mut, redundant_clone)]\nfn main() {\n    let mut a = 1;\n    println(f"{a}");\n}\n')
check("one attribute names several lints", "unused_mut" not in lint_names(doc) and not any(x.get("severity") == "error" for x in diags(doc)), diags(doc))

c, doc, e, f = check_json('#![must_use]\nfn main() {\n}\n')
errs = [x for x in diags(doc) if x.get("severity") == "error"]
check("inner attribute other than a lint level or rc_budget is an error", bool(errs) and c != 0, diags(doc))

c, doc, e, f = check_json('#[allow(unfulfilled_lint_expectation)]\nfn main() {\n    #[expect(unused_mut)]\n    let a = 1;\n    println(f"{a}");\n}\n')
check("#[allow(unfulfilled_lint_expectation)] suppresses it", "unfulfilled_lint_expectation" not in lint_names(doc) and "unfulfilled_lint_expectation" not in json.dumps(doc), diags(doc))

c, doc, e, f = check_json('fn main() {\n    #[expect(unused_mut)]\n    let mut a = 1;\n    println(f"{a}");\n}\n')
check("a fulfilled #[expect] reports nothing", not diags(doc), diags(doc))

c, doc, e, f = check_json('#[allow(unknown_lint)]\nfn main() {\n    #[allow(no_such_lint_name)]\n    let a = 1;\n    println(f"{a}");\n}\n')
check("#[allow(unknown_lint)] suppresses unknown_lint", not diags(doc), diags(doc))

c, doc, e, f = check_json('#[allow(unsafe_op_in_unsafe_fn)]\nfn main() {\n}\n')
check("unsafe_op_in_unsafe_fn takes no level (not a lint)", bool(diags(doc)), diags(doc))

c, o, e, _ = run(tempfile.mkdtemp(), ["lint", "--list"])
for nm in ["inferred_dependency_effects", "self_rebinding", "unused_variable"]:
    check(f"lint --list names {nm}", nm in o, "")
c2, doc2, e2, f2 = check_json('fn main() {\n    let x = 1;\n    println("hi");\n}\n', ["-W", "unused_variable"])
uv = [x for x in diags(doc2) if x.get("lint_name") == "unused_variable"]
check("unused_variable is W0324", bool(uv) and uv[0].get("code") == "W0324", diags(doc2))

c, doc, e, f = check_json('fn main() {\n    let n = 1;\n    let n = n;\n    println(f"{n}");\n}\n')
sr = [x for x in diags(doc) if x.get("lint_name") == "self_rebinding"]
check("self_rebinding: warns with code W0323 and a fix", bool(sr) and sr[0].get("code") == "W0323" and ("replacement" in sr[0] or "fix_diff" in sr[0]), diags(doc))

# ---- Attributes -------------------------------------------------------------
c, doc, e, f = check_json('#[deprecated = "old"]\nfn old() { }\nfn main() { }\n')
check("#[name = value] form is refused", c != 0, diags(doc))
c, doc, e, f = check_json('#[deprecated(since = "1.0")]\nfn old() { }\nfn main() { }\n')
check("key = value argument is refused", c != 0, diags(doc))
c, doc, e, f = check_json('#[karac::nosuch]\nfn old() { }\nfn main() { }\n')
check("an unknown karac:: name is an error", c != 0, diags(doc))
c, doc, e, f = check_json('#[cfg(any(target_os: "linux", target_os: "macos"))]\nfn a() { }\nfn main() { }\n')
check("cfg: refused as not implemented yet, naming #[target] (design: #[target] is interim until #[cfg])", c != 0 and "#[target" in json.dumps(diags(doc)), diags(doc))

c, doc, e, f = check_json('#[unstable]\npub fn shiny() { }\nfn main() { shiny(); }\n')
check("#[unstable]: a use without allow(unstable_api) is reported", bool(diags(doc)), diags(doc))
c, doc, e, f = check_json('#[unstable]\npub fn shiny() { }\n#[allow(unstable_api)]\nfn main() { shiny(); }\n')
check("#[unstable]: allow(unstable_api) permits the use", not [x for x in diags(doc) if "unstable" in json.dumps(x)], diags(doc))

ONU = '''#[diagnostic::on_unimplemented(message: "`{Self}` cannot be a key", label: "`{Self}` is not Keyish", note: "derive it")]
trait Keyish { fn k(self) -> i64; }
fn need[T: Keyish](x: T) -> i64 { x.k() }
struct P { a: i64 }
fn main() { println(f"{need(P { a: 1 })}"); }
'''
c, doc, e, f = check_json(ONU)
txt = json.dumps(diags(doc))
check("on_unimplemented: message with {Self} interpolated", "`P` cannot be a key" in txt, txt[:400])
check("on_unimplemented: label and note", "is not Keyish" in txt and "derive it" in txt, txt[:400])

c, doc, e, f = check_json(ONU.replace("{Self}` cannot", "{Nope}` cannot"))
w = [x for x in diags(doc) if x.get("severity") == "warning"]
check("on_unimplemented: unknown placeholder warns at the trait and renders literally",
      any(x.get("line") == 1 for x in w) and "{Nope}" in json.dumps(diags(doc)), diags(doc))

c, doc, e, f = check_json('#[diagnostic::on_unimplemented(mesage: "x")]\ntrait K { fn k(self) -> i64; }\nfn main() { }\n')
w = [x for x in diags(doc) if x.get("lint_name") == "malformed_diagnostic_attribute"]
check("malformed diagnostic:: attribute is the malformed_diagnostic_attribute warning", bool(w) and w[0].get("severity") == "warning" and c == 0, diags(doc))

c, doc, e, f = check_json('#[diagnostic::on_unimplemented(message: "x")]\nfn f() { }\nfn main() { }\n')
w = [x for x in diags(doc) if x.get("lint_name") == "malformed_diagnostic_attribute"]
check("on_unimplemented on a fn: malformed warning, not an error", bool(w) and c == 0, diags(doc))

TWO = '''#[diagnostic::on_unimplemented(message: "first {Self}")]
#[diagnostic::on_unimplemented(message: "second {Self}")]
trait K { fn k(self) -> i64; }
fn need[T: K](x: T) -> i64 { x.k() }
struct P { a: i64 }
fn main() { println(f"{need(P { a: 1 })}"); }
'''
c, doc, e, f = check_json(TWO)
txt = json.dumps(diags(doc))
check("two on_unimplemented: warning, and the first is used", "first P" in txt and "second P" not in txt and any(x.get("severity") == "warning" for x in diags(doc)), txt[:400])

c, doc, e, f = check_json('#[diagnostic::do_not_recommend]\nfn f() { }\nfn main() { }\n')
w = [x for x in diags(doc) if x.get("lint_name") == "malformed_diagnostic_attribute"]
check("do_not_recommend on a fn: malformed warning", bool(w) and c == 0, diags(doc))

DNR = '''trait Show { fn show(self) -> String; }
struct A { }
struct B { }
impl Show for A { fn show(self) -> String { "a" } }
#[diagnostic::do_not_recommend]
impl Show for B { fn show(self) -> String { "b" } }
fn need[T: Show](x: T) -> String { x.show() }
fn main() { println(need(5)); }
'''
c, doc, e, f = check_json(DNR)
txt = json.dumps(diags(doc))
check("do_not_recommend: the impl is left out of the suggestion list", c != 0 and "implemented by: A" in txt and "B" not in txt.split("implemented by:")[-1][:20], txt[:500])


# ---- The attribute registry -------------------------------------------------
ATTR_OK = {
    "derive": '#[derive(Clone, Debug)]\nstruct P { a: i64 }\nfn main() { let p = P { a: 1 }; let q = p.clone(); println(f"{q.a}"); }\n',
    "default": '#[derive(Default)]\nenum E { #[default] A, B }\nfn main() { let e: E = E.default(); match e { E.A => println("a"), E.B => println("b") } }\n',
    "serde": '#[derive(Serialize)]\nstruct P { #[serde(rename: "n")] a: i64 }\nfn main() { }\n',
    "target": '#[target(native)]\nfn a() { }\nfn main() { }\n',
    "repr": '#[repr(C)]\nstruct P { a: i32 }\nfn main() { }\n',
    "non_exhaustive": '#[non_exhaustive]\npub enum E { A }\nfn main() { }\n',
    "track_caller": '#[track_caller]\nfn a(x: i64) -> i64 { x }\nfn main() { println(f"{a(1)}"); }\n',
    "no_effect": '#[no_effect(writes(Stdout))]\nfn a() -> i64 { 1 }\nfn main() { println(f"{a()}"); }\n',
}
for name, src in ATTR_OK.items():
    c, doc, e, f = check_json(src)
    check(f"#[{name}] is in the registry", c == 0 and not [x for x in diags(doc) if x.get("code") == "E_UNKNOWN_ATTRIBUTE"], diags(doc))

c, doc, e, f = check_json('#[must_use]\nfn v() -> i64 { 1 }\nfn main() { v(); }\n')
check("#[must_use]: discarding the value warns", any(x.get("severity") == "warning" for x in diags(doc)), diags(doc))
c, doc, e, f = check_json('#[deprecated(since: "1.0", note: "use new")]\nfn old() { }\nfn main() { old(); }\n')
check("#[deprecated]: a use warns, key: value arguments accepted", c == 0 and "deprecated" in lint_names(doc), diags(doc))
c, doc, e, f = check_json('#[no_effect(writes(Stdout))]\nfn a() { println("x"); }\nfn main() { a(); }\n')
check("#[no_effect] forbids the effect in the body", c != 0, diags(doc))
c, doc, e, f = check_json('#[compiler_builtin]\nfn a() -> i64 { 1 }\nfn main() { }\n')
check("#[compiler_builtin] outside the standard library is refused", c != 0, diags(doc))
c, doc, e, f = check_json('#[test]\nfn t() { }\nfn main() { }\n')
check("#[test] is not an attribute (tests are test blocks)", c != 0, diags(doc))
c, doc, e, f = check_json('#[allow(unused_mut)]\n#[allow(unused_mut)]\nfn main() { let mut a = 1; println(f"{a}"); }\n')
check("the same lint named twice in one scope is E_DUPLICATE_LINT_LEVEL", "E_DUPLICATE_LINT_LEVEL" in codes(doc), codes(doc))

# ---- Debugger contract ------------------------------------------------------
DBG = '''import std.runtime;
fn work(n: i64) -> i64 { n * 2 }
fn main() {
    println(f"{runtime.has_debug_metadata()}");
    println(f"{runtime.list_par_blocks().len()}");
    println(f"{runtime.list_tasks().len()}");
    let (a, b) = par { work(1), work(2) };
    println(f"{a + b}");
}
'''
d, f = single(DBG)
c, o, e, _ = run(d, ["run", f])
check("debug metadata on in dev; empty lists outside par", c == 0 and o.splitlines()[:3] == ["true", "0", "0"], (c, o, e[:200]))
c, o, e, _ = run(d, ["build", "--release", f, "-o", d + "/rel"])
if c == 0:
    p = subprocess.run([d + "/rel"], capture_output=True, text=True)
    check("release: no debug metadata, lists empty", p.stdout.splitlines()[:3] == ["false", "0", "0"], p.stdout)
else:
    check("release build of debugger probe", False, e[:300])
dp = pkg({"main.kara": DBG}, TOML + "\n[profile.dev]\nruntime_debug_metadata = false\n")
c, o, e, _ = run(dp, ["run"])
check("[profile.dev] runtime_debug_metadata = false turns it off", c == 0 and o.splitlines()[:1] == ["false"], (c, o, e[:300]))

print(f"\n{sum(1 for _, ok in RESULTS if ok)} pass, {sum(1 for _, ok in RESULTS if not ok)} fail")
