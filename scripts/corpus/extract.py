#!/usr/bin/env python3
"""Extract the program corpus (`corpus/`) from its sources.

Sources:
  fixtures  every raw string in tests/**/*.rs (families/ included) that
            contains `fn main`, read statically; the test files are not edited
  katas     every `.kara` with a `fn main` under --katas (karalang/kara-katas)
  examples  every single-file examples/*.kara with a `fn main`

Each distinct program text becomes one directory with `source.kara` and a
first `meta.toml`:

  fixtures/<suite>/<file>/<test>/   tests/codegen/strings.rs -> fixtures/codegen/strings
                                    tests/cli.rs -> fixtures/cli
                                    tests/codegen/families/b_x.rs -> fixtures/codegen/families/b_x
  katas/<range>/<kata>/<stem>/
  examples/<stem>/

Identical texts are deduplicated: the first occurrence (by path, then offset)
names the entry and the rest go in `dedup_of`. For a fixture, the string
literals of the test that holds it are kept as candidate expectations in the
work manifest (`--work`, default target/corpus-work/manifest.json);
`record.py` runs each program once on the legacy compiler and keeps a
candidate as `expected_from = "assert"` only when it matches that run.

Re-running is safe: an unchanged program keeps its recorded files, a changed
one loses them, and directories no source produces any more are reported
(removed with --prune).
"""

import argparse
import hashlib
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from rustscan import scan  # noqa: E402
from corpus_lib import ROOT, write_toml, read_toml, RECORDED  # noqa: E402

HAS_MAIN = re.compile(r"(?m)^\s*(pub\s+)?fn\s+main\s*\(")
# `&["a", "b"]`, `vec!["a", "b"]`, `["a", "b"]`: line arrays a test compares
# against `out.lines()`.
STR_ARRAY = re.compile(r'\[\s*((?:"(?:[^"\\]|\\.)*"\s*,?\s*)+)\]', re.S)
STR_LIT = re.compile(r'"((?:[^"\\]|\\.)*)"', re.S)


def slug(s: str) -> str:
    s = re.sub(r"[^A-Za-z0-9_.-]+", "_", s).strip("_")
    return s[:120] or "x"


def unescape(s: str) -> str:
    """Rust ordinary-string escapes, including `\\` + newline continuation."""
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c != "\\" or i + 1 >= len(s):
            out.append(c)
            i += 1
            continue
        d = s[i + 1]
        i += 2
        if d == "n":
            out.append("\n")
        elif d == "t":
            out.append("\t")
        elif d == "r":
            out.append("\r")
        elif d == "0":
            out.append("\0")
        elif d in "\\\"'":
            out.append(d)
        elif d == "x":
            out.append(chr(int(s[i : i + 2], 16)))
            i += 2
        elif d == "u":
            j = s.index("}", i)
            out.append(chr(int(s[i + 1 : j], 16)))
            i = j + 1
        elif d == "\n":
            while i < len(s) and s[i] in " \t\n\r":
                i += 1
        else:
            out.append("\\" + d)
    return "".join(out)


def fixture_area(path: Path, tests: Path) -> str:
    rel = path.relative_to(tests).with_suffix("")
    parts = list(rel.parts)
    if len(parts) == 1:
        return f"fixtures/{slug(parts[0])}"
    return "fixtures/" + "/".join(slug(p) for p in parts)


def fixture_programs(tests: Path):
    """Yield dicts: area, holder, file, line, src, template, candidates."""
    for path in sorted(tests.rglob("*.rs")):
        text = path.read_text()
        holders: list[list] = []  # [name, start, tokens]
        cur = ["(file)", 0, []]
        for tok in scan(text):
            if tok[0] == "decl":
                holders.append(cur)
                cur = [tok[3], tok[1], []]
                continue
            cur[2].append(tok)
        holders.append(cur)
        for k, (name, start, toks) in enumerate(holders):
            end = holders[k + 1][1] if k + 1 < len(holders) else len(text)
            progs = [t for t in toks if t[0] == "raw" and "fn main" in t[2]]
            if not progs:
                continue
            cands = []
            for t in toks:
                if t[0] == "raw" and "fn main" not in t[2]:
                    cands.append(t[2])
                elif t[0] == "str":
                    cands.append(unescape(t[2]))
            for m in STR_ARRAY.finditer(text, start, end):
                items = [unescape(x) for x in STR_LIT.findall(m.group(1))]
                cands.append("\n".join(items))
            for _, off, src in progs:
                before = text[max(0, off - 40) : off]
                base = {
                    "area": fixture_area(path, tests),
                    "holder": name,
                    "file": str(path.relative_to(tests.parent)),
                    "line": text.count("\n", 0, off) + 1,
                    "candidates": [c for c in dict.fromkeys(cands) if c.strip()],
                }
                # A literal the test finishes at run time is not a program:
                # `format!(r#"..."#, ..)` fills holes, and `String::from(r#"..."#)`
                # is the head of a string the test goes on to push_str onto.
                template = bool(re.search(r"(format!|String::from)\(\s*$", before))
                # `let body = r#"..KEY.."#; let x = body.replace("KEY", "..")`:
                # one program per replacement, each named after its variable.
                var = re.search(r"let\s+(\w+)\s*(?::\s*&str\s*)?=\s*$", before)
                subs = []
                if var and not template:
                    pat = re.compile(r"let\s+(\w+)\s*=\s*" + re.escape(var.group(1)) +
                                     r"\.replace\(\s*\"([^\"]+)\"\s*,\s*\"((?:[^\"\\]|\\.)*)\"\s*,?\s*\)")
                    subs = [m for m in pat.finditer(text, off, end) if m.group(2) in src]
                if subs:
                    for m in subs:
                        yield dict(base, holder=f"{name}__{m.group(1)}",
                                   src=src.replace(m.group(2), unescape(m.group(3))), template=False)
                else:
                    yield dict(base, src=src, template=template)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tests", default=str(ROOT / "tests"))
    ap.add_argument("--katas", help="path to a karalang/kara-katas checkout")
    ap.add_argument("--examples", default=str(ROOT / "examples"))
    ap.add_argument("--pins", help="core pinning programs: <DIR>/<name>/{main.kara,expected.out}")
    ap.add_argument("--no-matrix", dest="matrix", action="store_false",
                    help="leave out the drop matrix (generated by corpus/tools/drop-model/matrix.py)")
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--work", default=str(ROOT / "target/corpus-work"))
    ap.add_argument("--prune", action="store_true")
    args = ap.parse_args()

    corpus = Path(args.corpus)
    entries: dict[str, dict] = {}
    taken: set[str] = set()
    raw = {"core": 0, "drop-matrix": 0, "fixture": 0, "kata": 0, "example": 0}

    def add(rel_dir: str, name: str, source: str, src: str, kind: str, extra: dict) -> None:
        raw[kind] += 1
        h = hashlib.sha256(src.encode()).hexdigest()
        if h in entries:
            entries[h]["meta"]["dedup_of"].append(source)
            entries[h]["candidates"] += [c for c in extra.get("candidates", []) if c not in entries[h]["candidates"]]
            return
        base = slug(name)
        d, n = f"{rel_dir}/{base}", 2
        while d in taken:
            d, n = f"{rel_dir}/{base}_{n}", n + 1
        taken.add(d)
        meta = {"source": source, "dedup_of": [], "tags": list(extra.get("tags", []))}
        meta.update(extra.get("meta", {}))
        entries[h] = {"dir": d, "src": src, "meta": meta, "kind": kind, "pin": extra.get("pin_expected"),
                      "candidates": list(extra.get("candidates", []))}

    if args.pins:
        # The v2 pins: their expectation is the spec's, not a legacy run, so
        # expect / exit / expected.out are set here and record.py leaves them.
        for d in sorted(Path(args.pins).iterdir()):
            if not (d / "main.kara").exists():
                continue
            exp = (d / "expected.out").read_text() if (d / "expected.out").exists() else ""
            if d.name.startswith("err_"):
                expect = "error:E0500" if "E0500" in exp else "error"
                exit_code = 1
            elif d.name.startswith("panic_"):
                expect, exit_code = "panic:101", 101
            else:
                expect, exit_code = "stdout", 0
            meta = {"expect": expect, "exit": exit_code, "expected_from": "pin", "class": "unknown",
                    "rules": [], "backends": [], "env": {},
                    "note": "the expected.out wording of an error pin is guidance, not a byte oracle"
                    if expect.startswith("error") else ""}
            add("core", d.name, f"review/core-pins/{d.name}", (d / "main.kara").read_text(), "core",
                {"meta": meta, "pin_expected": d / "expected.out"})

    if args.matrix:
        # The drop matrix: generated programs whose expectation comes from the
        # drop reference model, never from a compiler (expected_from = spec).
        gen = Path(args.work) / "drop-matrix"
        if gen.exists():
            shutil.rmtree(gen)
        tool = ROOT / "corpus/tools/drop-model"
        subprocess.run([sys.executable, "matrix.py", str(gen)], cwd=tool, check=True, stdout=subprocess.DEVNULL)
        for d in sorted(gen.iterdir()):
            m = read_toml(d / "meta.toml")
            meta = {"expect": m["expect"], "exit": m["exit"], "expected_from": "spec", "class": "unknown",
                    "rules": [], "backends": [], "env": {}, "note": ""}
            add("drop-matrix", d.name, m["source"], (d / "main.kara").read_text(), "drop-matrix",
                {"meta": meta, "tags": ["drop"], "pin_expected": d / "expected.out"})

    for p in fixture_programs(Path(args.tests)):
        tags = ["format-template"] if p["template"] else []
        add(p["area"], p["holder"], f"{p['file']}::{p['holder']}", p["src"], "fixture",
            {"tags": tags, "candidates": p["candidates"]})

    if args.katas:
        kroot = Path(args.katas)
        for f in sorted(kroot.rglob("*.kara")):
            if "target" in f.parts or ".git" in f.parts:
                continue
            src = f.read_text()
            if not HAS_MAIN.search(src):
                continue
            relp = f.relative_to(kroot)
            parts = [slug(x) for x in relp.parent.parts]
            if parts and parts[0] == "leetcode":
                parts = parts[1:]
            tags = (["bench"] if "bench" in relp.parts else []) + (["differential"] if f.stem == "differential" else [])
            add("katas/" + "/".join(parts), f.stem, f"kara-katas/{relp}", src, "kata", {"tags": tags})

    if args.examples:
        for f in sorted(Path(args.examples).glob("*.kara")):
            src = f.read_text()
            if HAS_MAIN.search(src):
                add("examples", f.stem, f"examples/{f.name}", src, "example", {})

    states = {"new": 0, "changed": 0, "same": 0}
    manifest = []
    written = set()
    for e in entries.values():
        dest = corpus / e["dir"]
        dest.mkdir(parents=True, exist_ok=True)
        sp = dest / "source.kara"
        old = sp.read_text() if sp.exists() else None
        state = "new" if old is None else ("same" if old == e["src"] else "changed")
        states[state] += 1
        meta = e["meta"]
        if state != "same":
            sp.write_text(e["src"])
            for f in RECORDED:
                if (dest / f).exists():
                    (dest / f).unlink()
        else:
            prev = read_toml(dest / "meta.toml")
            for k, v in prev.items():
                if k not in ("source", "dedup_of"):
                    meta[k] = v if k != "tags" else sorted(set(v) | set(meta["tags"]))
        if e["pin"] is not None:
            shutil.copy(e["pin"], dest / "expected.out")
        write_toml(dest / "meta.toml", meta)
        written.add(dest)
        manifest.append({"dir": e["dir"], "kind": e["kind"], "candidates": e["candidates"]})

    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    (work / "manifest.json").write_text(json.dumps(manifest))
    stale = sorted(p.parent for p in corpus.rglob("source.kara") if p.parent not in written)
    if args.prune:
        for p in stale:
            shutil.rmtree(p)
    print("corpus-extract: raw " + " ".join(f"{k}={v}" for k, v in raw.items()))
    print(f"corpus-extract: {len(entries)} entries after dedup; "
          + " ".join(f"{k}={v}" for k, v in states.items())
          + f"; {len(stale)} stale" + (" (pruned)" if args.prune else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
