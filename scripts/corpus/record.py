#!/usr/bin/env python3
"""Tag every corpus entry from `karac check` and its archived legacy record.

THE LEGACY RECORD IS ARCHIVED. Each entry's legacy.out (and `legacy_exit`,
`legacy_backend` in meta.toml) was written by one run of the legacy compiler,
which went at M3. Nothing here runs it any more: an entry that already has
legacy.out keeps it, and a new entry has none, so it needs an expected.out of
its own (`expected_from` = pin, spec or mirror) before the runner can judge it.

What a record held when the legacy run still existed:

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
Entries that already have legacy.out are skipped unless --redo, which re-runs
`karac check` and re-derives the tags against the archived legacy.out.

--model runs the source-level drop model (corpus/tools/drop-model/kmodel.py)
over every recorded entry outside core/ and drop-matrix/ and writes:

  model.out      the model's stdout, for model_verdict SAME, ORDER,
                 ORDER-UNSPEC and DIFF; the MIR backends are held to it
  meta.toml      model_verdict, model_exit, model_rev (an md5 prefix over the
                 model's .py files), and model_note for a V2-REJECT (the rule it broke)

An entry the model has no verdict for (UNSUP, PARSE, CRASH, LEGACY-REJECT)
loses all of these. Entries already at the current model_rev are kept
unless --redo.

--recheck re-ran a recorded entry on its legacy backend to find output that
changes between runs. It went with the legacy pipeline: the entries it tagged
`nondeterministic` keep the tag, and the option now refuses.
"""

import argparse
import concurrent.futures as cf
import json
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import ROOT, entries, matches_filter, read_toml, write_toml  # noqa: E402

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


def code_only(src: str) -> str:
    """The source with string literals emptied and line comments removed,
    so that a word in a title or a message ("Same Row or Column") does not
    read as a use of the feature it names."""
    src = re.sub(r'"(\\.|[^"\\])*"', '""', src)
    return re.sub(r"//[^\n]*", "", src)


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
    code = code_only(src)
    tags += [f"deferred:{name}" for name, rx in DEFERRED if rx.search(code)]
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


def derive(meta: dict, rel: str, cands: dict, out: str) -> dict:
    """Tags and class that follow from what was recorded, without a run.

    literal-unmatched  the holding test has string literals and none of them is
                       the legacy stdout: an IR-shape test, a partial or tolerant
                       assertion, or a legacy bug the suite pins; the first
                       literal goes in `note` for the classifier to read
    needs-driver       the legacy run timed out: almost always a server the Rust
                       test drives with a client, untestable as a plain program
    legacy-timeout     the same for a leetcode kata, which is never a server:
                       the legacy run was only slow
    class d            any deferred:* tag (classification rules, section 3)
    """
    tags = set(meta.get("tags", []))
    expect = str(meta.get("expect", ""))
    note = meta.get("note", "")
    if "timed out" in note:
        kata = re.match(r"katas/\d+-\d+/", rel) is not None
        tags.add("legacy-timeout" if kata else "needs-driver")
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
    if not (entry / "legacy.out").exists() or meta.get("expected_from") in ("pin", "spec", "mirror"):
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


MODEL_KEYS = ("model_verdict", "model_exit", "model_rev", "model_note", "model_src")
MODEL_OUT_VERDICTS = ("SAME", "ORDER", "ORDER-UNSPEC", "DIFF")


def model_rev() -> str:
    """md5 prefix over the model's Python sources: a content hash rather than a
    commit, so it is set before the model is committed and survives a rebase."""
    import hashlib
    h = hashlib.md5()
    for f in sorted((ROOT / "corpus/tools/drop-model").glob("*.py")):
        h.update(f.name.encode() + b"\0" + f.read_bytes())
    return h.hexdigest()[:8]


def _model_init() -> None:
    import os
    import signal
    # corpus_run's 20 s default is per program run alone; under --jobs load
    # several katas that take 11-19 s alone time out and lose model.out.
    os.environ.setdefault("MODEL_TIMEOUT", "90")
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
    # The verdict is for one text: classify's `karac fix` rewrites source.kara
    # after the model has run, so a changed source needs its own verdict.
    # Entries recorded before `model_src` existed count as stale only if fixed.
    import hashlib
    src = hashlib.md5((entry / "source.kara").read_bytes()).hexdigest()[:8]
    same_src = meta.get("model_src", None if meta.get("fixed_by_classify") else src) == src
    if meta.get("model_rev") == rev and same_src and not redo:
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
        meta.update({"model_verdict": v, "model_exit": seen["r"][1], "model_rev": rev, "model_src": src})
    else:
        if mo.exists():
            mo.unlink()
        if v == "V2-REJECT":
            meta.update({"model_verdict": v, "model_rev": rev, "model_src": src,
                         "model_note": " ".join(why.split())[:200]})
        else:
            meta.update({"model_rev": rev, "model_src": src})  # no verdict, but this rev has looked at it
    write_toml(entry / "meta.toml", meta)
    return v


def record_one(entry: Path, corpus: Path, cands: dict, args) -> str:
    """Tags from `karac check` plus the archived legacy record; no legacy run."""
    rel = str(entry.relative_to(corpus))
    has_record = (entry / "legacy.out").exists()
    if has_record and not args.redo:
        return "kept"
    meta = read_toml(entry / "meta.toml")
    if not has_record and meta.get("expected_from") not in ("pin", "spec", "mirror"):
        # The legacy pipeline that wrote legacy.out went at M3.
        return "no-legacy-record"
    src = (entry / "source.kara").read_text()
    logs = Path(args.work) / "logs" / rel
    logs.mkdir(parents=True, exist_ok=True)
    diags, raw = check_json(entry, args.karac, args.timeout)
    (logs / "check.json").write_text(raw)
    tags = set(meta.get("tags", [])) | set(tags_for(src, diags, meta.get("legacy_exit")))
    out = (entry / "legacy.out").read_bytes().decode(errors="replace") if has_record else ""
    meta["tags"] = sorted(tags)
    if meta.get("expected_from") not in ("pin", "spec", "mirror"):
        meta = derive(meta, rel, cands, out)
    write_toml(entry / "meta.toml", meta)
    return "retagged"


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
                    help="retired with the legacy pipeline at M3: refuses")
    args = ap.parse_args()
    corpus = Path(args.corpus)
    manifest = json.loads((Path(args.work) / "manifest.json").read_text())
    cands = {m["dir"]: m["candidates"] for m in manifest}
    todo = [e for e in entries(corpus) if not args.filter or matches_filter(str(e.relative_to(corpus)), args.filter)]
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
        print("corpus-record: --recheck needs a legacy run, and the legacy pipeline went at M3",
              file=sys.stderr)
        return 2
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
