#!/usr/bin/env python3
"""Extract the program corpus (`corpus/`) from its sources.

Sources:
  fixtures  every raw string in tests/**/*.rs (families/ included) that
            contains `fn main`, read statically; the test files are not edited
  katas     every `.kara` with a `fn main` under --katas (karalang/kara-katas)
  examples  every single-file examples/*.kara with a `fn main`
  apps      every kara-katas apps/<kind>/<name>/ with an app.toml, copied to
            corpus/apps/<kind>/<name>/ (see sync_apps; --katas implies it)

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
candidate as `expected_from = "assert"` only when it matches that run. A
program in tests/mir_*.rs that its test runs with `assert_runs(src,
<literal>)`, or `assert_runs(<program>, <literal>)`, is a v2 pin instead: that helper asserts exit 0 and exactly the
literal as the MIR pipeline's stdout, so the literal is written as
expected.out with `expected_from = "pin"`, whatever the legacy run does.

Re-running is safe. An unchanged program keeps its recorded files. A program
whose corpus copy is still the text extracted last time (meta.toml's
`extracted_src`) follows its test or kata when that changes. A copy that
differs from both is kept as it is, because a migration or a classify fix
edits the corpus copy on purpose; the run lists such entries in
<work>/kept.txt. With --overwrite-changed, an entry whose test
or kata text changed since it was extracted (meta.toml's `extracted_src`, an
md5 prefix of that text, differs or is missing) is replaced and loses its
recorded files. Directories no source produces any more are reported (removed
with --prune).
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
import formatfill  # noqa: E402
from rustscan import scan, unescape  # noqa: E402
from corpus_lib import ROOT, write_toml, read_toml, RECORDED  # noqa: E402

HAS_MAIN = re.compile(r"(?m)^\s*(pub\s+)?fn\s+main\s*\(")
# `&["a", "b"]`, `vec!["a", "b"]`, `["a", "b"]`: line arrays a test compares
# against `out.lines()`.
V2_PIN_NOTE = "a v2 pin: the test's assert_runs literal, the MIR pipeline's stdout at exit 0"
ASSERT_RUNS = re.compile(r"assert_runs\(\s*")
STR_ARRAY = re.compile(r'\[\s*((?:"(?:[^"\\]|\\.)*"\s*,?\s*)+)\]', re.S)
STR_LIT = re.compile(r'"((?:[^"\\]|\\.)*)"', re.S)


def slug(s: str) -> str:
    s = re.sub(r"[^A-Za-z0-9_.-]+", "_", s).strip("_")
    return s[:120] or "x"



# The test targets that go with legacy at M3 (review m3-plan item 9: they run
# only the legacy interpreter, codegen, JIT or REPL). Their ordinary-string
# programs are extracted so the behaviour they pin stays checked on v2; the
# suites that survive keep checking their own.
LEGACY_TEST_TARGETS = {
    "abi_repr_c_struct", "book_snippets", "call_site_type_args", "cli", "codegen",
    "comptime", "comptime_ast", "comptime_derive", "comptime_reflection", "contextual_keywords",
    "coro_e2e", "disjoint_differential", "drop_differential", "elision", "example_packages",
    "extern_keep_list", "gpu_e2e", "http_client_codegen", "http_server", "interpreter", "json",
    "karac_jit_runner_repl", "lljit_e2e", "lljit_prototype", "mcjit_prototype",
    "memory_sanitizer", "own_params", "par_codegen", "parallax", "parallax_bench",
    "parallax_lite", "park_and_wake", "protobuf", "protobuf_derive", "protobuf_proto",
    "relay_bench", "repl", "repl_jit", "safety_design", "safety_design_loop", "scaffold",
    "selfhost_codegen", "shared_ownership_matrix", "spawn_e2e", "stdlib_span_table_isolation",
    "tangle_corpus", "tcp_listener", "tcp_stream", "test_main_synth", "tls_codegen",
    "wasm_codegen", "ws_framing",
}


def fixture_area(path: Path, tests: Path) -> str:
    rel = path.relative_to(tests).with_suffix("")
    parts = list(rel.parts)
    if len(parts) == 1:
        return f"fixtures/{slug(parts[0])}"
    return "fixtures/" + "/".join(slug(p) for p in parts)


def fixture_programs(tests: Path, kind: str = "raw"):
    """Yield dicts: area, holder, file, line, src, template, candidates.

    `kind` is the literal the programs are read from: "raw" for raw strings,
    "str" for ordinary `"..."` strings in LEGACY_TEST_TARGETS, unescaped as rustc does (escapes and
    `\\` + newline continuations). main() reads the raw ones first and the
    ordinary ones in a second pass, so adding the second pass left every
    earlier entry's directory name as it was."""
    for path in sorted(tests.rglob("*.rs")):
        if kind == "str" and path.relative_to(tests).parts[0].removesuffix(".rs") not in LEGACY_TEST_TARGETS:
            continue
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
            progs = [(t[0], t[1], t[2] if t[0] == "raw" else unescape(t[2]))
                     for t in toks if t[0] == kind and "fn main" in t[2]]
            if not progs:
                continue
            cands = []
            for t in toks:
                if t[0] == kind and "fn main" in t[2]:
                    continue
                if t[0] == "raw":
                    cands.append(t[2])
                elif t[0] == "str":
                    cands.append(unescape(t[2]))
            for m in STR_ARRAY.finditer(text, start, end):
                items = [unescape(x) for x in STR_LIT.findall(m.group(1))]
                cands.append("\n".join(items))
            if re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
                # A `const` program is run by the tests that name it, so
                # their literals are its candidates too.
                use = re.compile(r"\b" + name + r"\b")
                for j, (_, ostart, otoks) in enumerate(holders):
                    oend = holders[j + 1][1] if j + 1 < len(holders) else len(text)
                    if j != k and use.search(text, ostart, oend):
                        cands += [t[2] if t[0] == "raw" else unescape(t[2]) for t in otoks
                                  if t[0] in ("raw", "str") and "fn main" not in t[2]]
            # A tests/mir_*.rs program run by `assert_runs(src, <literal>)`
            # is pinned by that literal: the helper asserts exit 0 and exactly
            # that stdout from the MIR pipeline, so it is a v2 pin, written
            # for v2, where the legacy run is only a record.
            pin_text = None
            if kind == "raw" and path.name.startswith("mir_") and len(progs) == 1:
                call = ASSERT_RUNS.search(text, start, end)
                lits = [t for t in toks if t[0] in ("raw", "str")]
                at = None
                if call and text.startswith("src", call.end()):
                    # `let src = r#"..."#; assert_runs(src, <want>)`
                    arg = re.compile(r"src\s*,\s*").match(text, call.end())
                    at = arg.end() if arg else None
                elif call and progs[0][1] == call.end():
                    # `assert_runs(r#"<program>"#, <want>)`: the literal after it
                    later = [t[1] for t in lits if t[1] > call.end()]
                    at = later[0] if later else None
                lit = [t for t in lits if t[1] == at]
                if lit:
                    pin_text = lit[0][2] if lit[0][0] == "raw" else unescape(lit[0][2])
            for _, off, src in progs:
                before = text[max(0, off - 40) : off]
                base = {
                    "area": fixture_area(path, tests),
                    "holder": name,
                    "file": str(path.relative_to(tests.parent)),
                    "line": text.count("\n", 0, off) + 1,
                    "candidates": [c for c in dict.fromkeys(cands) if c.strip()],
                    "pin_text": pin_text,
                }
                # A literal the test finishes at run time is not a program:
                # `format!(r#"..."#, ..)` fills holes, and `String::from(r#"..."#)`
                # is the head of a string the test goes on to push_str onto.
                template = bool(re.search(r"(format!|String::from)\(\s*$", before))
                # A `format!` whose arguments are all literals (or `const`s and
                # `let`s with literal values in scope) is a fixed program: fill it in.
                filled = None
                if template and re.search(r"format!\(\s*$", before):
                    raw_tok = next(t for t in toks if t[1] == off)
                    filled = formatfill.fill(text, off, raw_tok[2], raw_tok[0], start)
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
                    # The filled program is a further entry, under a name of
                    # its own, so the entries already in the corpus keep theirs.
                    if filled is not None:
                        yield dict(base, holder=f"{base['holder']}__fmt", src=filled, template=False, filled=True)


# kara-katas apps/<kind>/<name>/ -> corpus/apps/<kind>/<name>/. The app is
# authored in kara-katas; corpus/apps keeps the copy the corpus gates run.
APP_FILES = (("{name}.orig.kara", "source.orig.kara"), ("{name}.fixed.kara", "source.fixed.kara"),
             ("{name}.py", "{name}.py"), ("expected.out", "expected.out"), ("README.md", "notes.md"))
# app.toml holds the authoring keys; every other meta.toml key (class,
# bucket, legacy_*, model_*, note, ...) is written by record/classify in the
# corpus and is kept. Tags with these prefixes are classify's, also kept.
APP_KEYS = ("expect", "exit", "tags", "expected_from", "rules", "backends", "env")
CLASSIFY_TAG = ("lib:", "io:", "post-m1:")


def is_app_file(f: Path, root: Path) -> bool:
    return any((d / "app.toml").exists() for d in f.parents if d != root and root in d.parents)


def sync_apps(aroot: Path, dest_root: Path, overwrite_changed: bool, prune: bool) -> dict:
    """Copy each app in `aroot` (a dir holding app.toml and <name>.kara) into
    `dest_root`. Supporting files follow kara-katas. source.kara follows it
    too unless the corpus copy was edited since the last copy (meta.toml's
    `extracted_src` no longer matches it, as when classify fixes it): that copy
    is kept and listed, and --overwrite-changed replaces it."""
    st = {"new": 0, "changed": 0, "same": 0, "kept": [], "orphan": []}
    seen = set()
    for app in sorted(aroot.glob("*/*/app.toml")):
        d = app.parent
        kind, name = d.parent.name, d.name
        main_src = d / f"{name}.kara"
        if not main_src.exists():
            print(f"corpus-extract: {d} has app.toml but no {name}.kara; skipped", file=sys.stderr)
            continue
        out = dest_root / kind / name
        seen.add(out)
        out.mkdir(parents=True, exist_ok=True)
        src = main_src.read_text()
        h = hashlib.md5(src.encode()).hexdigest()[:8]
        prev = read_toml(out / "meta.toml")
        sp = out / "source.kara"
        old = sp.read_text() if sp.exists() else None
        if old is None:
            state = "new"
        elif old == src:
            state = "same"
        elif overwrite_changed or (prev.get("extracted_src")
                                   and hashlib.md5(old.encode()).hexdigest()[:8] == prev["extracted_src"]):
            state = "changed"
        else:
            state = "kept"
        if state == "kept":
            st["kept"].append(str(out.relative_to(dest_root.parent)))
        else:
            st[state] += 1
            if state != "same":
                sp.write_text(src)
        for a, b in APP_FILES:
            a, b = a.format(name=name), b.format(name=name)
            if (d / a).exists():
                if not (out / b).exists() or (out / b).read_bytes() != (d / a).read_bytes():
                    shutil.copy(d / a, out / b)
            elif (out / b).exists():
                (out / b).unlink()
        conf = read_toml(app)
        meta = dict(prev)
        meta["source"] = f"kara-katas/apps/{kind}/{name}/{name}.kara"
        meta.setdefault("dedup_of", [])
        for k in APP_KEYS:
            if k in conf:
                meta[k] = conf[k]
        meta["tags"] = list(conf.get("tags", [])) + [t for t in prev.get("tags", [])
                                                     if t.startswith(CLASSIFY_TAG) and t not in conf.get("tags", [])]
        if state != "kept":
            meta["extracted_src"] = h
        write_toml(out / "meta.toml", meta)
    if dest_root.exists():
        st["orphan"] = sorted(str(p.parent.relative_to(dest_root.parent)) for p in dest_root.glob("*/*/source.kara")
                              if p.parent not in seen)
        if prune:
            for o in st["orphan"]:
                shutil.rmtree(dest_root.parent / o)
    return st


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tests", default=str(ROOT / "tests"))
    ap.add_argument("--katas", help="path to a karalang/kara-katas checkout")
    ap.add_argument("--apps", help="kara-katas apps/ dir to copy into corpus/apps/ (default: <--katas>/apps)")
    ap.add_argument("--apps-only", action="store_true",
                    help="only copy the apps (needs --katas or --apps); fixtures, katas, examples untouched")
    ap.add_argument("--examples", default=str(ROOT / "examples"))
    ap.add_argument("--pins", help="core pinning programs: <DIR>/<name>/{main.kara,expected.out}")
    ap.add_argument("--no-matrix", dest="matrix", action="store_false",
                    help="leave out the drop matrix (generated by corpus/tools/drop-model/matrix.py)")
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--work", default=str(ROOT / "target/corpus-work"))
    ap.add_argument("--prune", action="store_true")
    ap.add_argument("--overwrite-changed", action="store_true",
                    help="replace a corpus source whose test or kata text changed since it was "
                         "extracted, dropping its recorded files (default: keep it and list it)")
    args = ap.parse_args()

    corpus = Path(args.corpus)
    apps_dir = Path(args.apps) if args.apps else (Path(args.katas) / "apps" if args.katas else None)
    if args.apps_only:
        if apps_dir is None:
            ap.error("--apps-only needs --katas or --apps")
        return report_apps(sync_apps(apps_dir, corpus / "apps", args.overwrite_changed, args.prune), args.prune)
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
                      "pin_text": extra.get("pin_text"),
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

    for kind in ("raw", "str"):
        for p in fixture_programs(Path(args.tests), kind):
            tags = ["format-template"] if p["template"] else ["format-filled"] if p.get("filled") else []
            if Path(p["file"]).parts[1].removesuffix(".rs") in LEGACY_TEST_TARGETS:
                tags.append("src:legacy-tests")  # outside the M1 gate (classify.py)
            add(p["area"], p["holder"], f"{p['file']}::{p['holder']}", p["src"], "fixture",
                {"tags": tags, "candidates": p["candidates"], "pin_text": p["pin_text"]})

    if args.katas:
        kroot = Path(args.katas)
        for f in sorted(kroot.rglob("*.kara")):
            if "target" in f.parts or ".git" in f.parts or is_app_file(f, kroot):
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

    states = {"new": 0, "changed": 0, "same": 0, "kept": 0}
    kept = []
    manifest = []
    written = set()
    for e in entries.values():
        dest = corpus / e["dir"]
        dest.mkdir(parents=True, exist_ok=True)
        sp = dest / "source.kara"
        old = sp.read_text() if sp.exists() else None
        orig = (dest / "source.orig.kara").read_text() if (dest / "source.orig.kara").exists() else None
        src_hash = hashlib.md5(e["src"].encode()).hexdigest()[:8]
        prev = read_toml(dest / "meta.toml") if (dest / "meta.toml").exists() else {}
        if old is None:
            state = "new"
        elif e["src"] in (old, orig):
            state = "same"
        elif (prev.get("extracted_src") and orig is None
              and hashlib.md5(old.encode()).hexdigest()[:8] == prev["extracted_src"]):
            # The corpus copy is still the text extracted last time, so no
            # one edited it; the test or kata moved on, and the copy follows.
            state = "changed"
        elif prev.get("extracted_src") == src_hash or not args.overwrite_changed:
            # The source in the corpus differs from the one the test or kata
            # holds, but the test text is what was extracted last time (or
            # there is no record of it): the corpus copy was edited on
            # purpose (a migration, a `karac fix` that classify keeps), so it
            # stays. Only --overwrite-changed replaces such a copy.
            state = "kept"
            kept.append(e["dir"])
        else:
            state = "changed"
        states[state] += 1
        meta = e["meta"]
        if state != "kept":
            meta["extracted_src"] = src_hash
        elif "extracted_src" in prev:
            meta["extracted_src"] = prev["extracted_src"]
        if state in ("new", "changed"):
            sp.write_text(e["src"])
            for f in RECORDED:
                if (dest / f).exists():
                    (dest / f).unlink()
        else:
            for k, v in prev.items():
                if k not in ("source", "dedup_of", "extracted_src"):
                    # src:legacy-tests marks only entries first extracted
                    # for M3 item 9; the gate's existing entries keep theirs.
                    meta[k] = v if k != "tags" else list(v) + sorted(set(meta["tags"]) - set(v) - {"src:legacy-tests"})
        if meta.get("note") == "":
            del meta["note"]
        if e["pin"] is not None:
            shutil.copy(e["pin"], dest / "expected.out")
        if e.get("pin_text") is not None:
            (dest / "expected.out").write_text(e["pin_text"])
            meta.update({"expect": "stdout", "exit": 0, "expected_from": "pin"})
            # classify.py appends its own notes to this one; keep them.
            if not meta.get("note", "").startswith(V2_PIN_NOTE):
                meta["note"] = V2_PIN_NOTE
        write_toml(dest / "meta.toml", meta)
        written.add(dest)
        manifest.append({"dir": e["dir"], "kind": e["kind"], "candidates": e["candidates"]})

    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    (work / "manifest.json").write_text(json.dumps(manifest))
    # corpus/apps/ is copied from kara-katas apps/ by sync_apps, not this walk.
    stale = sorted(p.parent for p in corpus.rglob("source.kara")
                   if p.parent not in written and not p.parent.relative_to(corpus).parts[0] == "apps")
    if args.prune:
        for p in stale:
            shutil.rmtree(p)
    print("corpus-extract: raw " + " ".join(f"{k}={v}" for k, v in raw.items()))
    print(f"corpus-extract: {len(entries)} entries after dedup; "
          + " ".join(f"{k}={v}" for k, v in states.items())
          + f"; {len(stale)} stale" + (" (pruned)" if args.prune else ""))
    if apps_dir is not None and apps_dir.exists():
        report_apps(sync_apps(apps_dir, corpus / "apps", args.overwrite_changed, args.prune), args.prune)
    if kept:
        (work / "kept.txt").write_text("\n".join(kept) + "\n")
        print(f"corpus-extract: {len(kept)} corpus sources differ from their test or kata text and were "
              f"kept (listed in {work / 'kept.txt'}); --overwrite-changed replaces them")
    return 0


def report_apps(st: dict, prune: bool) -> int:
    print(f"corpus-extract: apps new={st['new']} changed={st['changed']} same={st['same']} kept={len(st['kept'])}"
          f"; {len(st['orphan'])} corpus app(s) with no kara-katas source" + (" (pruned)" if prune and st["orphan"] else ""))
    for k in st["kept"]:
        print(f"  kept (corpus source.kara edited since the last copy; --overwrite-changed replaces it): {k}")
    for o in st["orphan"]:
        print(f"  no kara-katas source: {o}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
