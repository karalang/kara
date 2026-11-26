"""docs/library/regex.md against the Rust `regex` crate, which the page names
as the engine ("The pattern syntax is RE2's, as the Rust `regex` crate
implements it"). Random patterns over a small alphabet (ASCII, multibyte
letters, a non-ASCII digit, a newline), with groups, named groups,
alternation, classes, counted and lazy repetition, anchors, word boundaries
and the inline flags, plus invalid ones (backreference, lookaround, unclosed
group), are run through every method of the page on the interpreter, the
compiled build and (with `--lanes`) ASan, and each result is held to what
the crate returns for the same call: compile Ok or Err, is_match, find,
find_all, split, replace, replace_all (with `$1`, `${1}`, `$name`, `$$`,
`$1a` and missing groups), captures and captures_all with every group,
`get(-1)`, `get(len)` and `name` of a present and an absent name; and
`Regex.escape` of each text, which must equal the crate's and find the
text whole.

The oracle is conformance/regex_oracle (a Cargo project pinned to the
crate version Cargo.lock gives the runtime); it is built offline on first
use.

    python3 conformance/regex.py [--seed N] [--cases N] [--lanes interp,build,asan]
"""

import argparse
import random
import subprocess
from pathlib import Path

from harness import ROOT, Report, kara_str, run

ORACLE = Path(__file__).resolve().parent / "regex_oracle"

ALPHA = ["a", "b", "c", "A", "é", "ß", "1", "٣", " ", "-", "_"]
TEXT_ALPHA = ALPHA + ["\n", "B", "É", "2", "aa", "ab", "ba"] + list(".*+?()[]{}^$|\\#&~")


def atom(rng, depth, names):
    k = rng.randrange(14 if depth < 3 else 8)
    if k <= 2:
        return rng.choice(["a", "b", "c", "é", "1", "\\-", " "])
    if k == 3:
        return "."
    if k == 4:
        return rng.choice(["\\d", "\\w", "\\s", "[0-9]", "(?-u:\\d)", "\\D", "\\W"])
    if k == 5:
        return rng.choice(["[ab]", "[^a]", "[a-cé]", "[[:alpha:]]", "\\p{L}", "[\\d-]"])
    if k == 6:
        return rng.choice(["^", "$", "\\b", "\\B"])
    if k == 7:
        return rng.choice(["ab", "ba", "aé"])
    if k in (8, 9):
        return "(" + alt(rng, depth + 1, names) + ")"
    if k == 10:
        return "(?:" + alt(rng, depth + 1, names) + ")"
    if k == 11 and len(names) < 2:
        n = f"n{len(names) + 1}"
        names.append(n)
        open_ = rng.choice(["(?P<", "(?<"])
        return open_ + n + ">" + alt(rng, depth + 1, names) + ")"
    if k == 12:
        return rng.choice(["(?i)", "(?m)", "(?s)", "(?is)"]) + seq(rng, depth + 1, names)
    return "(?x: a b )" if rng.random() < 0.5 else "(?i:ab)"


def quant(rng, a):
    if a in ("^", "$", "\\b", "\\B") or a.startswith("(?i)") or a.startswith("(?m)") or a.startswith("(?s)"):
        return a
    q = rng.choice(["", "", "", "*", "+", "?", "*?", "+?", "??", "{2}", "{1,2}", "{0,}", "{2,3}?"])
    return a + q


def seq(rng, depth, names):
    return "".join(quant(rng, atom(rng, depth, names)) for _ in range(rng.randrange(1, 4)))


def alt(rng, depth, names):
    parts = [seq(rng, depth, names) for _ in range(1 if rng.random() < 0.7 else 2)]
    if rng.random() < 0.1:
        parts.append("")
    return "|".join(parts)


INVALID = ["(", "(a", "[a", "a{2,1}", "\\1", "(a)\\1", "(?=a)", "(?!a)", "(?<=a)b", "*", "a**", "\\",
           "(?P<n1>a)(?P<n1>b)", "[z-a]", "(?z)", "\\p{Nope}"]
REPS = ["", "x", "$1", "${1}", "$n1", "${n1}", "$$", "$1a", "${1}a", "<$0>", "$9", "${zz}", "$", "é$2é",
        "$n2-$n1"]

KARA = r'''import std.regex.{Regex, Match, Captures};
import std.encoding.{hex_encode};

fn span(m: Option[Match]) -> String {
    match m {
        Some(m) => f"{m.start}-{m.end}:{hex_encode(m.text.bytes())}",
        None => f"none",
    }
}

fn caps(c: ref Captures) -> String {
    let mut out = f"{c.len()}";
    let mut i = -1;
    while i <= c.len() {
        out = f"{out} {span(c.get(i))}";
        i += 1;
    }
    out = f"{out} {span(c.name("n1"))} {span(c.name("n2"))} {span(c.name("zz"))}";
    out
}

fn case(p: Str, t: Str, r: Str) {
    let esc = Regex.escape(t);
    let whole = match Regex.compile(esc.as_str()) {
        Ok(lit) => match lit.find(t) {
            Some(m) => m.start == 0 and m.end == t.len(),
            None => false,
        },
        Err(_) => false,
    };
    println(f"X {hex_encode(esc.bytes())} {whole}");
    match Regex.compile(p) {
        Err(_) => println("C err"),
        Ok(re) => {
            println("C ok");
            println(f"M {re.is_match(t)}");
            println(f"F {span(re.find(t))}");
            let mut a = String.new();
            for m in re.find_all(t).into_iter() {
                if a.len() > 0 {
                    a.push_str(",");
                }
                a.push_str(span(Some(m)).as_str());
            }
            println(f"A {a}");
            let mut s = String.new();
            let mut first = true;
            for piece in re.split(t) {
                if not first {
                    s.push_str(",");
                }
                first = false;
                s.push_str(hex_encode(piece.bytes()).as_str());
            }
            println(f"S {s}");
            println(f"R {hex_encode(re.replace(t, r).bytes())}");
            println(f"RA {hex_encode(re.replace_all(t, r).bytes())}");
            match re.captures(t) {
                Some(c) => println(f"K {caps(c)}"),
                None => println("K none"),
            }
            for c in re.captures_all(t) {
                println(f"KA {caps(c)}");
            }
            println("E");
        }
    }
}
'''


def oracle(cases):
    exe = ORACLE / "target/release/rxo"
    if not exe.exists():
        subprocess.run(["cargo", "build", "--release", "--offline", "--quiet"], cwd=ORACLE, check=True)
    stdin = "".join(f"{p.encode().hex()}\t{t.encode().hex()}\t{r.encode().hex()}\n" for p, t, r in cases)
    out = subprocess.run([str(exe)], input=stdin, capture_output=True, text=True, check=True).stdout
    per, cur = [], []
    for line in out.splitlines():
        cur.append(line)
        if line == "E" or line == "C err":
            per.append(cur)
            cur = []
    assert len(per) == len(cases), (len(per), len(cases))
    return per


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cases", type=int, default=300)
    ap.add_argument("--lanes", default="interp,build")
    args = ap.parse_args()
    rng = random.Random(args.seed)
    rep = Report("regex")

    cases = []
    # The page's own examples, held to the crate as well.
    cases += [("a*", "baaab", "-"), ("(?P<key>\\w+)=(?P<value>\\d+)", "a=1, b=22", "$value=$key"),
              ("(a)|b", "b", "[$1]"), (".", "é", "x"), ("\\d", "٣", "x"), ("[0-9]", "٣", "x"),
              ("(?-u:\\d)", "٣3", "x"), ("(a+)+$", "a" * 30 + "b", "x"), ("", "", "x"), ("", "aé", "-"),
              ("x*", "", "-"), ("$", "ab\nc", "!"), ("(?m)$", "ab\nc", "!"), ("(?s).", "\n", "x")]
    cases += [(p, rng.choice(["", "a", "abc"]), rng.choice(REPS)) for p in INVALID]
    for _ in range(args.cases):
        names = []
        p = alt(rng, 0, names)
        t = "".join(rng.choice(TEXT_ALPHA) for _ in range(rng.randrange(0, 14)))
        cases.append((p, t, rng.choice(REPS)))

    want = oracle(cases)
    labels, wants, calls = [], [], []
    for (p, t, r), lines in zip(cases, want):
        calls.append(f"    case({kara_str(p)}, {kara_str(t)}, {kara_str(r)});")
        for line in lines:
            labels.append(f"{p!r} on {t!r} rep {r!r}: {line.split(' ')[0]}")
            wants.append(line)
    src = KARA + "\nfn main() {\n" + "\n".join(calls) + "\n}\n"
    rep.check_runs(run(src, backends=tuple(args.lanes.split(","))), labels, wants)
    rep.finish()


if __name__ == "__main__":
    main()
