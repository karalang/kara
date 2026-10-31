#!/usr/bin/env python3
"""Check the library docs and the examples against the compiler.

scripts/design-conformance.py sweeps docs/design.md. The other code a reader
copies first had no sweep: the ```kara blocks of docs/library/*.md, README.md
and the examples' READMEs, and the example programs themselves. This runs
them, reusing design-conformance's block parser, its wrapping ladder and its
classification, so a block here means what it means there.

WHAT IT CHECKS.
  - Every ```kara block of the markdown files goes through `karac check` up
    the same ladder (as items, items plus `fn main`, inside `fn main`, as
    type aliases, as a signature catalogue).
  - A block that is a whole program (a `fn main` at column 0) and checks is
    also run with `karac __mir-run`. When the doc shows its output, the run
    is compared against it. Two conventions are read: a ```text / ```output /
    ```console block right after the program, and a shell line
    `karac run <file>  # <output>` in a ```bash / ```sh block right after it.
  - examples/*.kara are checked and run, and compared with the
    examples/<name>.expected beside them when there is one.
  - examples/*/kara.toml packages are checked with `karac check` in the
    package directory.

A finding is either a known one, recorded in the BASELINE
(docs/library-conformance-baseline.json) with a reason, or new. `--check`
fails on new findings, as design-conformance does.

Usage:
    scripts/library-conformance.py                    # report
    scripts/library-conformance.py --json out.json    # machine-readable
    scripts/library-conformance.py --update-baseline  # rewrite the baseline
    scripts/library-conformance.py --check            # exit 1 on drift
    scripts/library-conformance.py --only <hash|path|substr>
                                                      # narrow to matching
                                                      # entries; --check then
                                                      # judges only those
"""
import argparse
import importlib.util
import json
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "docs" / "library-conformance-baseline.json"

_spec = importlib.util.spec_from_file_location(
    "design_conformance", ROOT / "scripts" / "design-conformance.py")
dc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dc)


def markdown_files():
    files = sorted((ROOT / "docs" / "library").glob("*.md"))
    files.append(ROOT / "README.md")
    files += sorted(p for p in (ROOT / "examples").rglob("README.md")
                    if "node_modules" not in p.parts)
    return [f for f in files if "```kara" in f.read_text(encoding="utf-8")]


def is_program(block):
    return any(re.match(r"^fn main\s*\(", l) for l in block.code)


def documented_output(lines, end):
    """The output the doc shows for the program block ending at line index
    `end` (its closing fence), or None. See the module docstring."""
    k = end + 1
    while k < len(lines) and not lines[k].strip():
        k += 1
    if k >= len(lines) or k - end > 3:
        return None
    fence = re.match(r"^```(\w*)\s*$", lines[k].strip())
    if not fence:
        return None
    lang = fence.group(1)
    j = k + 1
    while j < len(lines) and lines[j].strip() != "```":
        j += 1
    body = lines[k + 1 : j]
    if lang in ("text", "output", "console", ""):
        return "\n".join(body).rstrip("\n") + "\n" if body else None
    if lang in ("bash", "sh", "shell"):
        for line in body:
            m = re.match(r"^\$?\s*karac run \S+\s+#\s?(.*)$", line)
            if m:
                return m.group(1).rstrip() + "\n"
    return None


def program_blocks(path):
    """parse_blocks, plus each program block's documented output."""
    lines = path.read_text(encoding="utf-8").split("\n")
    blocks = dc.parse_blocks(path)
    for b in blocks:
        b.file = path
        # parse_blocks records the first code line (fence + 1, 1-based).
        start = b.line - 2
        end = start + len(b.code) + 1
        b.expected = documented_output(lines, end) if is_program(b) else None
    return blocks


def mir_run(karac, source, cwd=None, timeout=30):
    with tempfile.TemporaryDirectory() as d:
        f = Path(d) / "main.kara"
        f.write_text(source, encoding="utf-8")
        try:
            p = subprocess.run([karac, "__mir-run", str(f)], capture_output=True,
                               text=True, timeout=timeout, stdin=subprocess.DEVNULL,
                               cwd=cwd or d, env={"KARAC_HASH_SEED": "1",
                                                  "PATH": "/usr/bin:/bin"})
        except subprocess.TimeoutExpired:
            return None, "", "timeout"
        return p.returncode, p.stdout, p.stderr


def check_package(karac, pkg):
    p = subprocess.run([karac, "check", "--output=json"], cwd=pkg,
                       capture_output=True, text=True, timeout=300,
                       stdin=subprocess.DEVNULL)
    try:
        payload = json.loads(p.stdout or "{}")
    except json.JSONDecodeError:
        return p.returncode, [{"message": (p.stderr or p.stdout).strip()[:400]}]
    raw = payload.get("diagnostics")
    if raw is None and isinstance(payload.get("targets"), list):
        raw = [d for t in payload["targets"] for d in t.get("diagnostics", [])]
    return p.returncode, [d for d in (raw or []) if d.get("severity") == "error"]


def rel(p):
    return str(Path(p).resolve().relative_to(ROOT))


def classify_block(b, karac, declared):
    """design-conformance's outcome for one block, plus the run."""
    if b.skipped or b.elided:
        return {"outcome": "skipped" if b.skipped else "elided", "rung": None,
                "diagnostics": []}
    status, rung, diags = dc.evaluate(b, karac)
    if b.expects_error:
        return {"outcome": "confirmed-rejection" if status == "rejected"
                else "MISSING-REJECTION", "rung": rung, "diagnostics": diags[:4]}
    if (diags and all(dc.UNRESOLVED_RE.match(d.get("message", "")) for d in diags)
            and (status == "rejected" or rung == "signatures")):
        missing = sorted({dc.UNRESOLVED_RE.match(d["message"]).group(2)
                          for d in diags})
        orphan = [n for n in missing if rung == "signatures"
                  and n not in declared and not dc.GENERIC_NAME_RE.match(n)]
        return {"outcome": "UNDECLARED-NAME" if orphan else "unresolved",
                "rung": rung, "missing": missing, "undeclared": orphan,
                "diagnostics": diags[:4]}
    if rung == "signatures":
        return {"outcome": "signatures-ok" if status == "pass"
                else "SIGNATURE-MISSING", "rung": rung, "diagnostics": diags[:4]}
    if status != "pass":
        return {"outcome": "REJECTED", "rung": rung, "diagnostics": diags[:4]}
    out = {"outcome": "conforms", "rung": rung, "diagnostics": []}
    if is_program(b) and rung == "items":
        rc, stdout, stderr = mir_run(karac, b.text + "\n")
        out["run"] = {"exit": rc, "stdout": stdout[-2000:], "stderr": stderr[-1000:]}
        if rc is None:
            out["outcome"] = "ran-timeout"
        elif b.expected is not None:
            out["expected"] = b.expected
            if stdout != b.expected:
                out["outcome"] = "OUTPUT-DIFFERS"
        elif rc != 0:
            out["outcome"] = "RUN-FAILED"
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--karac", default=str(ROOT / "target" / "debug" / "karac"))
    ap.add_argument("--json", dest="json_out")
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--only")
    args = ap.parse_args()
    if not Path(args.karac).exists():
        sys.exit(f"karac not found at {args.karac} — build it first (cargo build)")

    blocks = [b for f in markdown_files() for b in program_blocks(f)]
    declared = dc.collect_declared_names(blocks)
    def selected(key, where, text=""):
        # `--only` matches an entry's hash, its `path:line`, or its text.
        return not args.only or any(args.only in s for s in (key, where, text))

    results = []
    for b in blocks:
        where = f"{rel(b.file)}:{b.line}"
        if not selected(b.hash, where, b.text):
            continue
        r = classify_block(b, args.karac, declared)
        r.update({"hash": b.hash, "where": where, "heading": b.heading})
        results.append(r)
        if args.only:
            print(f"# {r['where']} [{b.heading}] {r['outcome']} rung={r['rung']}")
            print(b.text)
            for d in r["diagnostics"]:
                print(f"  -> {d.get('phase')}: {d.get('message')} (line {d.get('line')})")
            if "run" in r:
                print("  run:", json.dumps(r["run"]))

    for src in sorted((ROOT / "examples").glob("*.kara")):
        if not selected("example:" + src.name, rel(src)):
            continue
        text = src.read_text(encoding="utf-8")
        rc, diags = dc.run_karac(args.karac, text)
        r = {"hash": "example:" + src.name, "where": rel(src),
             "heading": "example program", "rung": "file", "diagnostics": diags[:4]}
        if rc != 0 or diags:
            r["outcome"] = "REJECTED"
        else:
            rc, stdout, stderr = mir_run(args.karac, text)
            r["run"] = {"exit": rc, "stdout": stdout[-2000:], "stderr": stderr[-1000:]}
            exp = src.with_suffix(".expected")
            if rc is None:
                r["outcome"] = "ran-timeout"
            elif exp.exists():
                r["outcome"] = ("conforms" if stdout == exp.read_text(encoding="utf-8")
                                else "OUTPUT-DIFFERS")
            else:
                r["outcome"] = "conforms" if rc == 0 else "RUN-FAILED"
        results.append(r)
    for manifest in sorted((ROOT / "examples").glob("*/kara.toml")):
        pkg = manifest.parent
        if not selected("package:" + pkg.name, rel(manifest)):
            continue
        if not any((pkg / "src" / e).exists() for e in ("main.kara", "lib.kara")):
            # A directory of standalone programs under a manifest: each
            # file is checked on its own.
            for src in sorted((pkg / "src").glob("*.kara")):
                rc, diags = dc.run_karac(args.karac, src.read_text(encoding="utf-8"))
                results.append({"hash": f"file:{pkg.name}/{src.name}",
                                "where": rel(src), "heading": "example file",
                                "rung": "file",
                                "outcome": "conforms" if rc == 0 and not diags
                                else "REJECTED", "diagnostics": diags[:4]})
            continue
        rc, diags = check_package(args.karac, pkg)
        results.append({"hash": "package:" + pkg.name, "where": rel(manifest),
                        "heading": "example package", "rung": "package",
                        "outcome": "conforms" if rc == 0 and not diags else "REJECTED",
                        "diagnostics": diags[:4]})

    good = {"conforms", "confirmed-rejection", "skipped", "elided", "unresolved",
            "signatures-ok", "ran-timeout"}
    bad = [r for r in results if r["outcome"] not in good]
    baseline = {}
    if BASELINE.exists():
        baseline = json.loads(BASELINE.read_text(encoding="utf-8")).get("blocks", {})

    # The baseline entries this run judged: all of them, or under `--only`
    # the ones it ran. An entry outside that is neither fixed nor dropped. (A
    # full run judges an entry whose block is gone, since an edit rehashes it.)
    judged = {r["hash"] for r in results} if args.only else set(baseline)

    if args.update_baseline:
        entries = {h: e for h, e in baseline.items() if h not in judged}
        for r in bad:
            prev = baseline.get(r["hash"], {})
            entries[r["hash"]] = {
                "where": r["where"], "heading": r["heading"], "outcome": r["outcome"],
                "reason": prev.get("reason", "UNTRIAGED — classify me"),
                "first_diagnostic": (r["diagnostics"][0].get("message")
                                     if r["diagnostics"] else None),
            }
        BASELINE.write_text(json.dumps(
            {"note": "Non-conforming library-doc blocks and examples. Shrink me. "
                     "`reason` says why each is here; UNTRIAGED means nobody has "
                     "decided yet.",
             "blocks": dict(sorted(entries.items(), key=lambda kv: kv[1]["where"]))},
            indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(f"baseline updated: {len(entries)} non-conforming entries")

    if args.json_out:
        Path(args.json_out).write_text(json.dumps(results, indent=2), encoding="utf-8")

    counts = Counter(r["outcome"] for r in results)
    print("library conformance: " + ", ".join(f"{n} {o}" for o, n in counts.most_common()))
    for r in bad:
        tag = "known" if r["hash"] in baseline else "NEW"
        print(f"  {tag:5} {r['where']} [{r['heading']}] {r['outcome']}")
        for d in r["diagnostics"][:2]:
            print(f"        {d.get('phase')}: {d.get('message')}")
    if args.check:
        new = [r for r in bad if r["hash"] not in baseline]
        bad_hashes = {r["hash"] for r in bad}
        fixed = [h for h in baseline if h in judged and h not in bad_hashes]
        for h in fixed:
            print(f"  FIXED {baseline[h]['where']} — remove it from the baseline")
        if new:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
