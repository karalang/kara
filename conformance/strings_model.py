"""docs/library/strings.md § Reading and § Building, against Rust's `str` and
`String`, which the page names as the answer for every backend. Random
texts over ASCII, multibyte letters, the whitespace the page lists (space,
tab, line ends, U+00A0, U+0085, U+3000), a lone `\\r`, case pairs whose
mapping changes length (`ß`, `İ`, `ﬁ`) and an emoji, with patterns drawn
from the text itself, from the alphabet, and empty, go through every
reading method (`len`, `chars`, `char_indices`, `char_at`, `contains`,
`starts_with`, `ends_with`, `eq_ignore_ascii_case`, `find`, `rfind`,
`match_indices`, `matches`, `split_once`, `rsplit_once`, `split_at`,
`strip_prefix`, `strip_suffix`, `split`, `splitn`, `lines`,
`split_whitespace`, the three trims, `replace`, the four case maps,
`repeat`, `substring` with saturating bounds) and the building ones
(`push`, `push_str`, `pop`, `truncate`, `retain`, `extend`, `+`). Results
are printed as hex bytes on both sides; the Rust side is one program
`rustc` builds.

    python3 conformance/strings_model.py [--seed N] [--cases N] [--lanes interp,build,asan]
"""

import argparse
import os
import random
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from harness import Report, kara_str, run

ALPHA = list("abcAB, -") + ["é", "É", "ß", "İ", "ﬁ", "€", "😀", "\t", "\n", "\r", "\r\n", " ", "\u0085",
                           "　", "ab", "a,"]


def rand_text(rng, n=None):
    return "".join(rng.choice(ALPHA) for _ in range(rng.randrange(0, 10) if n is None else n))


def rand_pat(rng, s):
    k = rng.randrange(5)
    if k == 0:
        return ""
    if k in (1, 2) and s:
        cs = list(s)
        i = rng.randrange(len(cs))
        return "".join(cs[i:i + rng.randrange(1, 4)])
    return rand_text(rng, rng.randrange(1, 3))


def rust_str(s):
    return '"' + "".join(f"\\u{{{ord(c):x}}}" for c in s) + '"'


def boundaries(s):
    out, b = [0], 0
    for c in s:
        b += len(c.encode())
        out.append(b)
    return out


KARA_HEAD = """import std.encoding.{hex_encode};

fn hx(s: Str) -> String {
    let h = hex_encode(s.bytes());
    f"[{h}]"
}

fn hl(v: Vec[Str]) -> String {
    let mut out = String.new();
    for p in v {
        out.push_str(hx(p));
    }
    out
}

fn ho(o: Option[Str]) -> String {
    match o {
        Some(t) => hx(t),
        None => "N".to_string(),
    }
}

fn hp(o: Option[(Str, Str)]) -> String {
    match o {
        Some((a, b)) => f"{hx(a)}{hx(b)}",
        None => "N".to_string(),
    }
}

fn hi(o: Option[i64]) -> String {
    match o {
        Some(i) => f"{i}",
        None => "N".to_string(),
    }
}

fn keep(c: char) -> bool {
    c != 'a' and c != ' ' and c != 'é'
}

"""

RUST_HEAD = """#![allow(unused)]
fn hx(s: &str) -> String {
    let mut h = String::from("[");
    for b in s.bytes() { h.push_str(&format!("{:02x}", b)); }
    h.push(']');
    h
}
fn hl<'a>(v: impl Iterator<Item = &'a str>) -> String { v.map(hx).collect() }
fn ho(o: Option<&str>) -> String { o.map(hx).unwrap_or("N".to_string()) }
fn hp(o: Option<(&str, &str)>) -> String { o.map(|(a, b)| format!("{}{}", hx(a), hx(b))).unwrap_or("N".to_string()) }
fn hi(o: Option<usize>) -> String { o.map(|i| i.to_string()).unwrap_or("N".to_string()) }
fn keep(c: char) -> bool { c != 'a' && c != ' ' && c != 'é' }
fn sub(s: &str, a: i64, b: i64) -> String {
    let n = s.len() as i64;
    let a = a.clamp(0, n) as usize;
    let b = b.clamp(0, n) as usize;
    if b <= a { String::new() } else { s[a..b].to_string() }
}
fn splitn(s: &str, n: i64, p: &str) -> String { if n <= 0 { String::new() } else { hl(s.splitn(n as usize, p)) } }
fn rep(s: &str, n: i64) -> String { if n <= 0 { String::new() } else { s.repeat(n as usize) } }
fn trunc(s: &mut String, n: i64) { if n < 0 { s.clear() } else if (n as usize) < s.len() { s.truncate(n as usize) } }
"""


def case(rng, c):
    """(kara statements, rust statements): both print the same lines."""
    s = rand_text(rng)
    p = rand_pat(rng, s)
    q = rand_text(rng, rng.randrange(0, 3))
    n = rng.choice([-1, 0, 1, 2, 3, 5])
    k = rng.choice([-1, 0, 1, 2, 3, 9])
    bs = boundaries(s)
    mid = rng.choice(bs)
    a, b = rng.choice(bs + [-3, 999]), rng.choice(bs + [-2, 999])
    tl = rng.choice(bs + [-1, 999])
    ks, kp, kq = kara_str(s), kara_str(p), kara_str(q)
    rs, rp, rq = rust_str(s), rust_str(p), rust_str(q)
    pc = rng.choice(["a", "é", "😀", " ", "ß"])
    lines = [
        ("len", "{s.len()}", "s.len()"),
        ("count", "{s.chars().count()}", "s.chars().count()"),
        ("rev", "{hx(rv.as_str())}", "hx(&s.chars().rev().collect::<String>())"),
        ("ci", "{cix}", "s.char_indices().map(|(i, _)| i.to_string()).collect::<Vec<_>>().join(\",\")"),
        ("char_at", f"{{ca}}", f"({k}i64).try_into().ok().and_then(|k: usize| s.chars().nth(k)).map(|c| hx(&c.to_string())).unwrap_or(\"N\".to_string())"),
        ("contains", "{s.contains(p)}", "s.contains(p)"),
        ("starts", "{s.starts_with(p)} {s.ends_with(p)}", "format!(\"{} {}\", s.starts_with(p), s.ends_with(p))"),
        ("eqic", "{s.eq_ignore_ascii_case(p)}", "s.eq_ignore_ascii_case(p)"),
        ("find", "{hi(s.find(p))} {hi(s.rfind(p))}", "format!(\"{} {}\", hi(s.find(p)), hi(s.rfind(p)))"),
        ("mi", "{mix}", "s.match_indices(p).map(|(i, m)| format!(\"{}{}\", i, hx(m))).collect::<Vec<_>>().join(\",\")"),
        ("matches", "{s.matches(p).count()}", "s.matches(p).count()"),
        ("once", "{hp(s.split_once(p))} {hp(s.rsplit_once(p))}", "format!(\"{} {}\", hp(s.split_once(p)), hp(s.rsplit_once(p)))"),
        ("split_at", f"{{sa}}", f"{{ let (x, y) = s.split_at({mid}); format!(\"{{}}{{}}\", hx(x), hx(y)) }}"),
        ("strip", "{ho(s.strip_prefix(p))} {ho(s.strip_suffix(p))}", "format!(\"{} {}\", ho(s.strip_prefix(p)), ho(s.strip_suffix(p)))"),
        ("split", "{hl(sp)}", "hl(s.split(p))"),
        ("splitn", "{hl(spn)}", f"splitn(s, {n}, p)"),
        ("lines", "{hl(ln)}", "hl(s.lines())"),
        ("ws", "{hl(ws)}", "hl(s.split_whitespace())"),
        ("trim", "{hx(s.trim())} {hx(s.trim_start())} {hx(s.trim_end())}", "format!(\"{} {} {}\", hx(s.trim()), hx(s.trim_start()), hx(s.trim_end()))"),
        ("replace", "{hx(s.replace(p, q))}", "hx(&s.replace(p, q))"),
        ("case", "{hx(s.to_uppercase())} {hx(s.to_lowercase())}", "format!(\"{} {}\", hx(&s.to_uppercase()), hx(&s.to_lowercase()))"),
        ("acase", "{hx(s.to_ascii_uppercase())} {hx(s.to_ascii_lowercase())}", "format!(\"{} {}\", hx(&s.to_ascii_uppercase()), hx(&s.to_ascii_lowercase()))"),
        ("repeat", f"{{hx(s.repeat({n}))}}", f"hx(&rep(s, {n}))"),
        ("substring", f"{{hx(s.substring({a}, {b}))}} {{hx(s.substring({a}))}}", f"format!(\"{{}} {{}}\", hx(&sub(s, {a}, {b})), hx(&sub(s, {a}, i64::MAX)))"),
        ("build", "{hx(t.as_str())} {popped} {hx(u.as_str())} {hx(w.as_str())}",
         "format!(\"{} {} {} {}\", hx(&t), popped, hx(&u), hx(&w))"),
        ("plus", "{hx((s.to_string() + q.to_string()).as_str())}", "hx(&(s.to_string() + q))"),
    ]
    kara = [f"    {{\n        let s: Str = {ks};\n        let p: Str = {kp};\n        let q: Str = {kq};",
            "        let rv: String = s.chars().rev().collect();",
            "        let mut cix = String.new();",
            "        for (i, _) in s.char_indices() {\n            if not cix.is_empty() {\n                cix.push(',');\n            }\n            cix.push_str(f\"{i}\");\n        }",
            f"        let ca = match s.char_at({k}) {{\n            Some(ch) => {{\n                let mut one = String.new();\n                one.push(ch);\n                hx(one.as_str())\n            }}\n            None => \"N\".to_string(),\n        }};",
            "        let mut mix = String.new();",
            "        for (i, m) in s.match_indices(p) {\n            if not mix.is_empty() {\n                mix.push(',');\n            }\n            mix.push_str(f\"{i}{hx(m)}\");\n        }",
            f"        let (x, y) = s.split_at({mid});\n        let sa = f\"{{hx(x)}}{{hx(y)}}\";",
            "        let sp: Vec[Str] = s.split(p).collect();",
            f"        let spn: Vec[Str] = s.splitn({n}, p).collect();",
            "        let ln: Vec[Str] = s.lines().collect();",
            "        let ws: Vec[Str] = s.split_whitespace().collect();",
            # Building: push, push_str, pop, truncate, retain, extend.
            f"        let mut t = s.to_string();\n        t.push('{pc}');\n        t.push_str(q);",
            "        let popped = match t.pop() {\n            Some(ch) => {\n                let mut one = String.new();\n                one.push(ch);\n                hx(one.as_str())\n            }\n            None => \"N\".to_string(),\n        };",
            f"        let mut u = s.to_string();\n        u.truncate({tl});",
            "        let mut w = s.to_string();\n        w.retain(|ch| keep(ch));\n        w.extend(q.chars());"]
    # truncate inside a char panics: keep tl on a boundary or out of range.
    rust = [f"    {{\n        let s: &str = {rs};\n        let p: &str = {rp};\n        let q: &str = {rq};",
            f"        let mut t = s.to_string(); t.push('{pc}'); t.push_str(q);",
            "        let popped = t.pop().map(|c| hx(&c.to_string())).unwrap_or(\"N\".to_string());",
            f"        let mut u = s.to_string(); trunc(&mut u, {tl});",
            "        let mut w = s.to_string(); w.retain(keep); w.extend(q.chars());"]
    for name, kf, rf in lines:
        kara.append(f'        println(f"{c} {name} {kf}");')
        rust.append(f'        println!("{c} {name} {{}}", {rf});')
    kara.append("    }")
    rust.append("    }")
    return "\n".join(kara), "\n".join(rust)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    # One block per case in one function: B-2026-11-27-71 makes checking it
    # grow as the cube of the case count.
    ap.add_argument("--cases", type=int, default=40)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("strings_model")
    rustc = os.environ.get("RUSTC") or shutil.which("rustc")
    if not rustc:
        sys.exit("strings_model: no rustc")
    ks, rs = [], []
    for c in range(args.cases):
        k, r = case(rng, c)
        ks.append(k)
        rs.append(r)
    with tempfile.TemporaryDirectory(prefix="strings-rs-") as d:
        (Path(d) / "m.rs").write_text(RUST_HEAD + "fn main() {\n" + "\n".join(rs) + "\n}\n")
        b = subprocess.run([rustc, "-O", "--edition", "2021", "-o", str(Path(d) / "m"), str(Path(d) / "m.rs")],
                           capture_output=True, text=True)
        if b.returncode != 0:
            sys.exit("rustc failed:\n" + b.stderr[-4000:])
        want = subprocess.run([str(Path(d) / "m")], capture_output=True, text=True, check=True).stdout.splitlines()
    src = KARA_HEAD + "fn main() {\n" + "\n".join(ks) + "\n}\n"
    res = run(src, backends=tuple(args.lanes.split(",")))
    rep.check_runs(res, [" ".join(w.split(" ", 2)[:2]) for w in want], want)
    rep.finish()


if __name__ == "__main__":
    main()
