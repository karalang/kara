#!/usr/bin/env python3
"""Record the legacy compiler's behaviour for every corpus entry, once.

For each entry it runs `karac check --output=json` (for tags and error codes)
and one legacy run, then writes:

  legacy.out     the legacy run's stdout (its exit code goes in meta.toml as legacy_exit)
  meta.toml      expect, exit, tags, class = "unknown", expected_from, legacy_backend

expected.out is not written: it stays absent until a program is classified,
and the runner falls back to legacy.out. Core pins already carry the spec's
expected.out and keep their expect / exit; only the legacy record is added.

The legacy backend is the one today's suite used for the program:
`legacy-interp` for tests/interpreter fixtures, `legacy-build` (AOT, auto-par
at its default, which is off) for everything else.

expected_from:
  assert      one of the holding test's own string literals (or a `[..]` line
              array) matches the legacy stdout, compared as the suites compare
              (exact, or trimmed, or as lines)
  legacy-run  no literal matched: an IR-shape test that asserts nothing about
              stdout, a tolerant or partial assertion, a kata, an example

expect: stdout | panic:<exit> | error:<code> | skip (deferred feature,
format! template, or the legacy run timed out).

Raw logs (check JSON, stdout, stderr) go to --work/logs/<entry>/.
Entries that already have legacy.out are skipped unless --redo.

--model runs the source-level drop model (corpus/tools/drop-model/kmodel.py)
over every recorded entry outside core/ and drop-matrix/ and writes:

  model.out      the model's stdout, for model_verdict SAME, ORDER,
                 ORDER-UNSPEC and DIFF; the MIR backends are held to it
  meta.toml      model_verdict, model_exit, model_rev (the last commit touching
                 the model), and model_note for a V2-REJECT (the rule it broke)

An entry the model has no verdict for (UNSUP, PARSE, CRASH, LEGACY-REJECT)
loses all of these. Entries already at the current model_rev are kept
unless --redo.

--recheck runs every already-recorded entry (narrow it with --filter) once
more on its legacy backend. One whose stdout or exit code differs from its
record prints something that changes between runs (a bound port, thread
scheduling, cancellation timing): it is tagged `nondeterministic` and set to
expect = "skip", since no expected.out can hold it.
"""

import argparse
import concurrent.futures as cf
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import ROOT, entries, read_toml, run_program, write_toml  # noqa: E402

DEFERRED = [
    ("gpu", re.compile(r"\bgpu\.")),
    ("tensor", re.compile(r"\bTensor\b")),
    ("dataframe", re.compile(r"\b(Column|DataFrame)\b|to_arrow_ipc")),
    ("comptime", re.compile(r"\bcomptime\b")),
    ("dyn", re.compile(r"\bdyn ")),
    ("layout", re.compile(r"(?m)^\s*layout\b")),
    ("catch_panic", re.compile(r"\bcatch_panic\b")),
    ("regex", re.compile(r"\bRegex\b")),
    ("normalize", re.compile(r"\.normalize\(")),
]
SYNTAX_TAGS = [
    ("drop", re.compile(r"\bimpl(\[[^\]]*\])?\s+Drop\s+for\b")),
    ("shared", re.compile(r"\b(shared|par)\s+(struct|enum)\b|\bfrozen\b|\bWeak\b|\bweak\b")),
    ("par", re.compile(r"\bpar\s*\{|\bTaskGroup\b|\bspawn\(")),
    ("closure", re.compile(r"(\bmove\s*)?\|[^|\n]*\|\s*[\w({\[!&*-]|\|\|\s*[\w({\[]")),
    ("ref-return", re.compile(r"->\s*(ref\b|Option\[\s*ref\b)")),
]
MOVE_FROM_BORROW = ("borrow_projection_copy", "for_element_drop_copy")


def tags_for(src: str, diags: list[dict], legacy_exit) -> list[str]:
    tags = [name for name, rx in SYNTAX_TAGS if rx.search(src)]
    codes = {d.get("code") for d in diags}
    if legacy_exit == 101:
        tags.append("panic")
    if codes & {"E0500", "N0503"}:
        tags.append("uses-moved")
    blob = json.dumps(diags)
    if any(l in blob for l in MOVE_FROM_BORROW):
        tags.append("move-from-borrow")
    tags += [f"deferred:{name}" for name, rx in DEFERRED if rx.search(src)]
    return tags


def check_json(entry: Path, karac: str, timeout: float) -> tuple[list[dict], str]:
    try:
        r = subprocess.run([karac, "check", "--output=json", "source.kara"], cwd=entry,
                           capture_output=True, timeout=timeout, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return [], "timeout"
    raw = r.stdout.decode(errors="replace")
    try:
        return json.loads(raw).get("diagnostics", []), raw
    except json.JSONDecodeError:
        return [], raw


def norm_lines(s: str) -> list[str]:
    return s.strip().splitlines()


def matches(cand: str, out: str) -> bool:
    return cand == out or cand.strip() == out.strip() or norm_lines(cand) == norm_lines(out)


def derive(meta: dict, rel: str, cands: dict, out: str) -> dict:
    """Tags and class that follow from what was recorded, without a run.

    literal-unmatched  the holding test has string literals and none of them is
                       the legacy stdout: an IR-shape test, a partial or tolerant
                       assertion, or a legacy bug the suite pins; the first
                       literal goes in `note` for the classifier to read
    needs-driver       the legacy run timed out: almost always a server the Rust
                       test drives with a client, untestable as a plain program
    class d            any deferred:* tag (classification rules, section 3)
    """
    tags = set(meta.get("tags", []))
    expect = str(meta.get("expect", ""))
    note = meta.get("note", "")
    if "timed out" in note:
        tags.add("needs-driver")
    ran = expect == "stdout" or expect.startswith("panic:") or \
        (expect == "skip" and not note and "format-template" not in tags)
    if meta.get("expected_from") == "legacy-run" and cands.get(rel) and ran and "literal-unmatched" not in tags:
        tags.add("literal-unmatched")
        first = " ".join(cands[rel][0].split())[:120]
        note = (note + " " if note else "") + f"first literal of the holding test: {first!r}"
    if any(t.startswith("deferred:") for t in tags) and meta.get("class", "unknown") == "unknown":
        meta["class"] = "d"
    meta["tags"] = sorted(tags)
    meta["note"] = note
    return meta


def retag_one(entry: Path, corpus: Path, cands: dict) -> str:
    meta = read_toml(entry / "meta.toml")
    if not (entry / "legacy.out").exists() or meta.get("expected_from") in ("pin", "spec"):
        return "untouched"
    before = dict(meta, tags=list(meta.get("tags", [])))
    meta = derive(meta, str(entry.relative_to(corpus)), cands, "")
    if meta == before:
        return "same"
    write_toml(entry / "meta.toml", meta)
    return "retagged"


def mirror_one(entry: Path, args) -> str:
    """A kata whose directory holds a Python mirror with the same stem: run it
    and tag the entry `mirror-agrees` or `mirror-differs` against legacy.out.
    The mirrors implement the same algorithm (kara-katas CLAUDE.md), so
    agreement makes legacy's output an independently checked expectation."""
    meta = read_toml(entry / "meta.toml")
    src = str(meta.get("source", ""))
    if not src.startswith("kara-katas/") or not (entry / "legacy.out").exists():
        return "not-a-kata"
    py = Path(args.katas) / Path(src[len("kara-katas/"):]).with_suffix(".py")
    if not py.exists():
        return "no-mirror"
    try:
        r = subprocess.run([sys.executable, py.name], cwd=py.parent, capture_output=True,
                           timeout=args.timeout, stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return "mirror-timeout"
    if r.returncode != 0:
        return "mirror-failed"
    tag = "mirror-agrees" if r.stdout == (entry / "legacy.out").read_bytes() else "mirror-differs"
    tags = set(meta.get("tags", [])) - {"mirror-agrees", "mirror-differs"}
    meta["tags"] = sorted(tags | {tag})
    write_toml(entry / "meta.toml", meta)
    return tag


MODEL_KEYS = ("model_verdict", "model_exit", "model_rev", "model_note")
MODEL_OUT_VERDICTS = ("SAME", "ORDER", "ORDER-UNSPEC", "DIFF")


def model_rev() -> str:
    return subprocess.run(["git", "log", "-1", "--format=%h", "--", "corpus/tools/drop-model/"],
                          cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()


def _model_init() -> None:
    import signal
    sys.path.insert(0, str(ROOT / "corpus/tools/drop-model"))
    import corpus_run
    signal.signal(signal.SIGALRM, corpus_run._alarm)


def model_one(job) -> str:
    """One entry through the drop model. Runs in a worker process: the model's
    timeout is SIGALRM, which only a process's main thread can take."""
    entry, rev, redo = job
    import corpus_run
    meta = read_toml(entry / "meta.toml")
    if not (entry / "legacy.out").exists() or meta.get("expect") == "skip":
        return "unrecorded-or-skip"
    if meta.get("model_rev") == rev and not redo:
        return "kept"
    seen = {}
    real = corpus_run.run_source

    def capture(src):
        seen["r"] = real(src)
        return seen["r"]

    corpus_run.run_source = capture
    try:
        v, why, out = corpus_run.verdict(entry, meta)
    finally:
        corpus_run.run_source = real
    for k in MODEL_KEYS:
        meta.pop(k, None)
    mo = entry / "model.out"
    if v in MODEL_OUT_VERDICTS:
        mo.write_text(out)
        meta.update({"model_verdict": v, "model_exit": seen["r"][1], "model_rev": rev})
    else:
        if mo.exists():
            mo.unlink()
        if v == "V2-REJECT":
            meta.update({"model_verdict": v, "model_rev": rev, "model_note": " ".join(why.split())[:200]})
        else:
            meta["model_rev"] = rev  # no verdict, but this rev has looked at it
    write_toml(entry / "meta.toml", meta)
    return v


def recheck_one(entry: Path, args) -> str:
    meta = read_toml(entry / "meta.toml")
    if not (entry / "legacy.out").exists() or meta.get("expect") == "skip":
        return "unrecorded-or-skip"
    if not (meta.get("expect") == "stdout" or str(meta.get("expect", "")).startswith("panic:")):
        return "not-a-run"
    res = run_program(entry, meta.get("legacy_backend", "legacy-build"), args.karac, args.timeout,
                      {"KARAC_HASH_SEED": "1", **meta.get("env", {})})
    if res["stdout"] == (entry / "legacy.out").read_bytes() and res["exit"] == meta.get("legacy_exit"):
        return "same"
    meta["tags"] = sorted(set(meta.get("tags", [])) | {"nondeterministic"})
    meta["expect"] = "skip"
    meta["note"] = (meta.get("note", "") + " " if meta.get("note") else "") + \
        "output differs between two legacy runs (a bound port, scheduling or timing)"
    write_toml(entry / "meta.toml", meta)
    print(f"nondeterministic: {entry}", flush=True)
    return "nondeterministic"


def record_one(entry: Path, corpus: Path, cands: dict, args) -> str:
    rel = str(entry.relative_to(corpus))
    if (entry / "legacy.out").exists() and not args.redo:
        return "kept"
    meta = read_toml(entry / "meta.toml")
    src = (entry / "source.kara").read_text()
    # drop-matrix: the model was compared against the interpreter when it was built
    backend = "legacy-interp" if rel.startswith(("fixtures/interpreter/", "drop-matrix/")) else "legacy-build"
    logs = Path(args.work) / "logs" / rel
    logs.mkdir(parents=True, exist_ok=True)

    diags, raw = check_json(entry, args.karac, args.timeout)
    (logs / "check.json").write_text(raw)
    tags = set(meta.get("tags", []))
    note = meta.get("note", "")
    res = None
    if "format-template" not in tags:
        res = run_program(entry, backend, args.karac, args.timeout, {"KARAC_HASH_SEED": "1", **meta.get("env", {})})
        (logs / "stdout").write_bytes(res["stdout"])
        (logs / "stderr").write_bytes(res["stderr"])
    tags |= set(tags_for(src, diags, res and res["exit"]))

    out = res["stdout"].decode(errors="replace") if res else ""
    if res is None:
        expect, exit_code, note = "skip", 0, note or "a format! template; the test fills in its holes"
    elif res["status"] == "timeout":
        expect, exit_code, note = "skip", 0, note or f"legacy run timed out after {args.timeout:.0f}s"
    elif res["status"] == "refused":
        errs = [d.get("code") or d.get("phase") for d in diags if d.get("severity") == "error"]
        expect, exit_code = f"error:{errs[0] if errs else 'codegen'}", res["exit"]
    elif res["exit"] == 0:
        expect, exit_code = "stdout", 0
    else:
        expect, exit_code = f"panic:{res['exit']}", res["exit"]
    if any(t.startswith("deferred:") for t in tags) and args.skip_deferred:
        expect = "skip"

    expected_from = "legacy-run"
    if res is not None and res["status"] == "ran":
        if any(matches(c, out) for c in cands.get(rel, [])):
            expected_from = "assert"
    (entry / "legacy.out").write_bytes(res["stdout"] if res else b"")
    legacy_exit = -1 if res is None or res["exit"] is None else res["exit"]
    if meta.get("expected_from") in ("pin", "spec"):
        # A v2 pin or drop-matrix program keeps the spec's expectation; only
        # the legacy record is added.
        meta.update({"tags": sorted(tags), "legacy_exit": legacy_exit, "legacy_backend": backend})
    else:
        meta.update({"expect": expect, "exit": exit_code, "tags": sorted(tags),
                     "class": meta.get("class", "unknown"), "expected_from": expected_from,
                     "legacy_exit": legacy_exit, "legacy_backend": backend})
        meta.setdefault("rules", [])
        meta.setdefault("backends", [])
        meta.setdefault("env", {})
        meta["note"] = note
        meta = derive(meta, rel, cands, out)
    write_toml(entry / "meta.toml", meta)
    return expect.split(":")[0]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--karac", default=str(ROOT / "target/debug/karac"))
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--work", default=str(ROOT / "target/corpus-work"))
    ap.add_argument("--filter", action="append", default=[])
    ap.add_argument("--jobs", type=int, default=8)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--redo", action="store_true")
    ap.add_argument("--model", action="store_true",
                    help="record the drop model's output and verdict per entry (model.out, model_verdict)")
    ap.add_argument("--mirror", action="store_true",
                    help="run each kata's Python mirror and tag mirror-agrees / mirror-differs")
    ap.add_argument("--katas", default=str(ROOT.parent / "kara-katas"), help="kara-katas checkout, for --mirror")
    ap.add_argument("--retag", action="store_true",
                    help="re-derive tags and class d from what is already recorded, without running anything")
    ap.add_argument("--recheck", action="store_true",
                    help="re-run recorded entries and mark the ones whose output changes as nondeterministic")
    ap.add_argument("--keep-deferred", dest="skip_deferred", action="store_false",
                    help="keep expect=stdout for deferred-feature programs (default: expect=skip, "
                         "since several need opt-in runtime archives and v2 defers them)")
    args = ap.parse_args()
    corpus = Path(args.corpus)
    manifest = json.loads((Path(args.work) / "manifest.json").read_text())
    cands = {m["dir"]: m["candidates"] for m in manifest}
    todo = [e for e in entries(corpus) if not args.filter or any(f in str(e.relative_to(corpus)) for f in args.filter)]
    counts: dict[str, int] = {}
    done = 0
    if args.model:
        rev = model_rev()
        todo = [e for e in todo if not str(e.relative_to(corpus)).startswith(("core/", "drop-matrix/"))]
        counts: dict[str, int] = {}
        with cf.ProcessPoolExecutor(max_workers=args.jobs, initializer=_model_init) as pool:
            for v in pool.map(model_one, [(e, rev, args.redo) for e in todo], chunksize=16):
                counts[v] = counts.get(v, 0) + 1
        print(f"corpus-record: model_rev={rev} " + " ".join(f"{k}={v}" for k, v in sorted(counts.items()))
              + f" of {len(todo)}")
        return 0
    if args.mirror:
        step = lambda e: mirror_one(e, args)  # noqa: E731
    elif args.retag:
        step = lambda e: retag_one(e, corpus, cands)  # noqa: E731
    elif args.recheck:
        step = lambda e: recheck_one(e, args)  # noqa: E731
    else:
        step = lambda e: record_one(e, corpus, cands, args)  # noqa: E731
    with cf.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for v in pool.map(step, todo):
            counts[v] = counts.get(v, 0) + 1
            done += 1
            if done % 500 == 0:
                print(f"corpus-record: {done}/{len(todo)}", flush=True)
    print("corpus-record: " + " ".join(f"{k}={v}" for k, v in sorted(counts.items())) + f" of {len(todo)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
