#!/usr/bin/env python3
"""Classify the corpus against the MIR interpreter, for the M1 go/no-go gate.

    python3 scripts/corpus/classify.py                    # every entry
    python3 scripts/corpus/classify.py --filter katas/    # a subset
    python3 scripts/corpus/classify.py --report-only      # re-read meta.toml, write the report

Rules: review/corpus-classification.md §3 and its "gate rules (2026-10-08)".
For each entry, outside class d (deferred):

1. `karac check --output=json`. An error from a v2 rule (E05xx, E0288,
   E0289, and the move-from-borrow lints; see `is_v2`) sends the entry to class b: `karac fix`
   runs on a copy to a fixed point; if the copy then checks clean it becomes
   `source.kara` (the extracted text stays as `source.orig.kara`) and is judged
   below on its own output; if not, the entry is e. Any other check error on a
   program the legacy compiler accepted is e (`check-refuses:<code>`).
2. `karac __mir-run` (a release build by default), at the runner's timeout
   and `KARAC_HASH_SEED`.
   A builder refusal or a runtime error of the MIR interpreter is
   `mir-gap:<construct>`; a timeout is `timeout`.
3. The output against the expectations. `spec` is `expected.out` from a pin
   or the drop model (core/, drop-matrix/), or `model.out` recorded at the
   current model_rev:
   - spec exists, MIR == spec: `a` when spec == legacy, else `e-spec`
     (a legacy bug v2 fixes; resolved automatically, listed in the report);
   - MIR == legacy and a spec exists that differs: open e (`e-both-differ`);
   - MIR == legacy: `a`;
   - tagged drop/shared and MIR == legacy as multisets of lines: `c`
     (unreviewed);
   - anything else: open e (`e-differs`).
4. Tags: `post-m1:<feature>` for what M1 does not promise (par, net, process,
   dyn, escaping closures), `lib:<type>` and `io:<kind>` for the library and
   I/O beyond the literal M1 list, `bench` for kata bench variants.

meta.toml gets `class`, `bucket` (the rule that decided it, or the failure),
the tags, and `classified_with` (the karac commit, or `<base>+local`). `expected.out` is written
for a, b, c and e-spec (never for an open e or a gap); an e-spec entry's
`expected_from` becomes `spec`. The report,
`corpus/CLASSIFICATION.md`, holds the gate: passing / classified, over the
deduplicated entries in the slice (not post-m1, not bench, not d), with the
denominator by bucket, the same number without lib:/io:, all non-deferred
entries for context, bench's own pass/timeout line, and the list of
auto-resolved e-spec entries.
"""

import argparse
import collections
import concurrent.futures as cf
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from corpus_lib import ROOT, entries, read_toml, write_toml  # noqa: E402

PASSING = {"a", "b", "c"}
# expected.out that states the spec rather than a legacy record: a core pin,
# the drop model (drop-matrix/, or an e-spec resolution), an apps mirror.
SPEC_SOURCES = ("pin", "spec", "mirror", "decision")
SPEC_VERDICTS = {"SAME", "ORDER", "ORDER-UNSPEC", "DIFF"}
# What M1 does not promise (gate rules 2026-10-08, item 2).
POST_M1 = [
    ("par", r"\bpar\s*\{|\bpar\s+for\b|\bspawn\s*\(|\bTaskGroup\b|\basync\b|\.await\b|\bChannel\b|\bSender\b|\bReceiver\b"),
    ("net", r"\bTcp(Stream|Listener)\b|\bUdpSocket\b|\bserve(_https)?\s*\(|\bHttp[A-Z]\w*|\bRequest\b|\bResponse\b|\bWebSocket\b"),
    ("process", r"\bCommand\b|\bChild\b"),
    ("dyn", r"\bdyn\s+[A-Z]"),
    ("escaping", r"\bescaping\b"),
    # Deferred past M1 by design.md: providers to M4a services (§ Deferred),
    # SIMD, contracts and predicates on distinct types to the later tracks.
    ("provider", r"\bwith_provider\b|\bproviders\s*\{"),
    ("simd", r"\bVector\["),
    ("contract", r"(?m)\bfn\b[^\n{]*\b(?:requires|ensures)\b|^[ \t]*(?:requires|ensures)(?:\(\w+\))?[ \t]+\S"
                 # a type invariant (design.md § Keywords: deferred with contracts)
                 r"|(?:^|[{,])[ \t]*(?:impl[ \t]+)?invariant[ \t]+self\b"),
    ("refinement", r"\btype\s+[A-Z]\w*(?:\[[^\]]*\])?\s*=[^\n;]*\bwhere\b"),
]
LIB = [("map", r"\bMap\b"), ("set", r"\bSet\b"), ("sortedmap", r"\bSortedMap\b"),
       ("sortedset", r"\bSortedSet\b"), ("vecdeque", r"\bVecDeque\b"), ("heap", r"\bBinaryHeap\b")]
IO = [("fs", r"\bfs\.|\bFile\b"), ("env", r"\bEnv\."), ("stdin", r"\bStdin\b|\bstdin\b")]


def sh(args, cwd, env, timeout):
    try:
        return subprocess.run(args, cwd=cwd, env=env, capture_output=True, timeout=timeout,
                              stdin=subprocess.DEVNULL)
    except subprocess.TimeoutExpired:
        return None


def check_errors(karac, d, env, timeout):
    r = sh([karac, "check", "--output=json", "source.kara"], d, env, timeout)
    if r is None:
        return [{"code": "check-timeout"}]
    try:
        diags = json.loads(r.stdout).get("diagnostics", [])
    except json.JSONDecodeError:
        return [{"code": "check-unparsable"}] if r.returncode else []
    errs = [x for x in diags if x.get("severity") == "error"]
    if r.returncode and not errs:
        errs = [{"code": "check-failed"}]
    return errs


# v2 ownership rules that carry a pre-E05xx code: moving out of a type with a
# `Drop` body (E0289, core-semantics §3.7), an impl method whose parameter
# modes differ from the trait's (E0288, D5), and the move-from-borrow lints
# (E0200 with these lint names, C1/C3).
V2_CODES = {"E0288", "E0289"}
V2_LINTS = {"borrow_projection_copy", "for_element_drop_copy", "partial_move_of_drop_struct"}
# The v2 named-parameter rule (DESIGN_REVIEW §5: only a parameter after `;`
# is named and may have a default) reports under the general E0200/E0215
# codes, so it is told apart by message; `karac fix` inserts the `;` and the
# call-site labels.
V2_MESSAGES = ("only a named parameter can have a default",
               "and the parameters after it are named")


def diag_key(e):
    return e.get("lint_name") or e.get("code") or "?"


def is_v2(e):
    code = e.get("code") or ""
    return (re.fullmatch(r"E05\d\d", code) is not None or code in V2_CODES
            or e.get("lint_name") in V2_LINTS
            or any(m in (e.get("message") or "") for m in V2_MESSAGES))


def gap_construct(stderr: str) -> str:
    line = next((l for l in stderr.splitlines() if l.startswith("error[mir]")), stderr.strip()[:120])
    m = re.match(r"error\[mir\]: build: [\d:]+ in `[^`]*`: (.*)", line)
    if m:
        what = re.sub(r"\[[^\]`]*\]", "", m.group(1))  # Vec[i64].x -> Vec.x
        return re.sub(r" is not lowered yet$", "", what)[:80]
    m = re.match(r"error\[mir\]: run: (.*)", line)
    if m:
        msg = re.sub(r"`[^`]*`", "`_`", m.group(1))
        return "runtime: " + re.sub(r"\d+", "N", msg)[:70]
    m = re.match(r"error\[mir\]: (\w[\w ]*?)( [\w.\[\]]+)?: ", line)
    return (m.group(1) if m else line[:60]).strip()


def slice_tags(src: str, rel: str):
    # A program's own `enum Command` or `struct HttpRequest` is not the
    # library's: a match on a name the program declares does not count.
    own = set(re.findall(r"\b(?:struct|enum|type|trait)\s+([A-Z]\w*)", src))

    def uses(rx):
        def theirs(m):
            name = re.search(r"[A-Za-z_]\w*", m.group(0))
            return name is None or name.group(0) not in own
        return any(theirs(m) for m in re.finditer(rx, src))

    tags = [f"post-m1:{k}" for k, rx in POST_M1 if uses(rx)]
    tags += [f"lib:{k}" for k, rx in LIB if re.search(rx, src)]
    tags += [f"io:{k}" for k, rx in IO if re.search(rx, src)]
    if "/bench/" in f"/{rel}/":
        tags.append("bench")
    return tags


def model_rev():
    sys.path.insert(0, str(ROOT / "scripts/corpus"))
    import record  # noqa: E402
    return record.model_rev()


def classify(entry: Path, corpus: Path, args, rev: str, sha: str) -> dict:
    rel = str(entry.relative_to(corpus))
    meta = read_toml(entry / "meta.toml")
    tags = [t for t in meta.get("tags", []) if not t.startswith(("post-m1:", "lib:", "io:")) and t != "bench"]
    # A class-b entry this script fixed keeps the extracted text in
    # source.orig.kara; apps/ keep their blind draft there, which is not the
    # program.
    src_file = entry / ("source.orig.kara" if meta.get("fixed_by_classify") else "source.kara")
    src = src_file.read_text()
    tags += slice_tags(src, rel)
    out = {"rel": rel, "tags": tags}
    expect = meta.get("expect", "stdout")
    if meta.get("v2_reject"):
        # Legacy ran it; v2 refuses it (the reason is in `v2_reject`).
        expect = "error"
    if meta.get("class") == "d" or any(t.startswith("deferred:") for t in tags):
        return {**out, "class": "d", "bucket": "d"}
    if expect == "skip":
        return {**out, "class": "unknown", "bucket": "skip:" + (meta.get("note", "")[:40] or "expect=skip")}
    env = dict(os.environ)
    env.update({"KARAC_HASH_SEED": str(args.seed), **{k: str(v) for k, v in meta.get("env", {}).items()}})
    with tempfile.TemporaryDirectory(prefix="classify-") as tmp:
        d = Path(tmp)
        (d / "source.kara").write_text(src)
        errs = check_errors(args.karac, d, env, args.timeout)
        fixed, legacy_interp = None, None
        if expect.startswith("error"):
            want = expect.split(":", 1)[1] if ":" in expect else ""
            codes = [e.get("code") for e in errs]
            if errs and (want in ("", "codegen") or want in codes):
                return {**out, "class": "a", "bucket": "a:refused"}
            r = sh([args.karac, "__mir-run", "source.kara"], d, env, args.timeout)
            # A MIR refusal is a static check (move or borrow check), not a
            # construct the builder cannot lower or a run-time failure.
            if r is not None and r.returncode == 3 and b"error[mir]: build" not in r.stderr \
                    and b"error[mir]: run" not in r.stderr:
                return {**out, "class": "a", "bucket": "a:refused"}
            if want != "codegen":
                return {**out, "class": "e", "bucket": f"e-not-refused:{want or 'any'}"}
            # `error:codegen` records only that legacy-build's codegen declined
            # the program, which is no language rule. `karac check` accepts
            # it, so it is judged on its output, with legacy's interpreter as
            # the legacy baseline.
            li = sh([args.karac, "run", "--interp", "source.kara"], d, env, args.timeout)
            legacy_interp = (li.stdout, li.returncode) if li is not None else (None, None)
            expect = "stdout"
        if errs:
            codes = sorted({diag_key(e) for e in errs})
            if not any(is_v2(e) for e in errs):
                return {**out, "class": "e", "bucket": f"check-refuses:{codes[0]}"}
            for _ in range(20):
                before = (d / "source.kara").read_text()
                sh([args.karac, "fix", "source.kara"], d, env, args.timeout)
                if (d / "source.kara").read_text() == before:
                    break
            if check_errors(args.karac, d, env, args.timeout):
                return {**out, "class": "e", "bucket": "b-fix-incomplete:" + codes[0]}
            fixed = (d / "source.kara").read_text()
            if fixed == src:
                return {**out, "class": "e", "bucket": "b-fix-noop:" + codes[0]}
        r = sh([args.karac, "__mir-run", "source.kara"], d, env, args.timeout)
    res = {**out, "fixed": fixed}
    if r is None:
        return {**res, "class": "e", "bucket": "timeout"}
    stderr = r.stderr.decode(errors="replace")
    if r.returncode == 3 and "error[mir]" in stderr:
        return {**res, "class": "e", "bucket": "mir-gap:" + gap_construct(stderr)}
    got, code = r.stdout, r.returncode
    legacy = (entry / "legacy.out").read_bytes() if (entry / "legacy.out").exists() else None
    want_exit = int(expect.split(":", 1)[1]) if expect.startswith("panic:") else int(meta.get("exit", 0))
    legacy_exit = int(meta.get("legacy_exit", want_exit))
    if legacy_interp is not None:
        legacy, legacy_exit = legacy_interp
        res["legacy_interp"] = legacy_exit
    spec, spec_exit, as_set = None, want_exit, False
    if meta.get("expected_from") in SPEC_SOURCES and (entry / "expected.out").exists():
        spec = (entry / "expected.out").read_bytes()
        # An auto-resolved entry's expected.out is the model's output, held
        # to the model's exit, not legacy's.
        if meta.get("expected_from") == "spec" and "model_exit" in meta:
            spec_exit = int(meta["model_exit"])
            as_set = meta.get("model_verdict") == "ORDER-UNSPEC"
    elif meta.get("model_verdict") in SPEC_VERDICTS and meta.get("model_rev") == rev \
            and (entry / "model.out").exists():
        spec = (entry / "model.out").read_bytes()
        spec_exit = int(meta.get("model_exit", want_exit))
        as_set = meta.get("model_verdict") == "ORDER-UNSPEC"
    lines = lambda b: sorted(b.splitlines())  # noqa: E731
    same = lambda a, b, s=False: a is not None and (a == b or (s and lines(a) == lines(b)))  # noqa: E731
    sub = "b" if fixed is not None else None
    if spec is not None and same(got, spec, as_set) and code == spec_exit:
        if same(spec, legacy, as_set) and code == legacy_exit:
            return {**res, "class": sub or "a", "bucket": f"{sub + '->' if sub else ''}a", "expected": got}
        return {**res, "class": sub or "e", "bucket": f"{sub + '->' if sub else ''}e-spec", "expected": got}
    if legacy is not None and got == legacy and code == legacy_exit:
        if spec is not None:
            return {**res, "class": "e", "bucket": "e-both-differ-from-spec"}
        return {**res, "class": sub or "a", "bucket": f"{sub + '->' if sub else ''}a", "expected": got}
    if spec is None and legacy is not None and ({"drop", "shared"} & set(tags)) \
            and lines(got) == lines(legacy) and code == legacy_exit:
        return {**res, "class": sub or "c", "bucket": f"{sub + '->' if sub else ''}c-unreviewed", "expected": got}
    why = "exit" if legacy is not None and got == legacy else "stdout"
    return {**res, "class": "e", "bucket": f"e-differs:{why}"}


E_SPEC_NOTE = "e-spec: legacy differs from the drop model; v2 matches it"
C_NOTE = "class c: v2 order unreviewed against core §7"


def write_back(entry: Path, r: dict, sha: str):
    meta = read_toml(entry / "meta.toml")
    meta["tags"] = r["tags"]
    meta["class"] = r["class"]
    meta["bucket"] = r["bucket"]
    meta["classified_with"] = sha
    if r.get("fixed") is not None:
        if not meta.get("fixed_by_classify"):
            shutil.copy(entry / "source.kara", entry / "source.orig.kara")
            meta["fixed_by_classify"] = True
        (entry / "source.kara").write_text(r["fixed"])
    elif meta.get("fixed_by_classify"):
        # No longer fixed (the fix stopped converging, or is not needed):
        # back to the extracted text.
        shutil.move(entry / "source.orig.kara", entry / "source.kara")
        del meta["fixed_by_classify"]
    note = meta.get("note", "")
    for n in (C_NOTE, E_SPEC_NOTE):
        note = note.replace(n, "")
    note = " ".join(note.split())
    if "expected" in r and meta.get("expected_from") not in SPEC_SOURCES:
        (entry / "expected.out").write_bytes(r["expected"])
        if r["bucket"].endswith("e-spec"):
            meta["expected_from"] = "spec"
        elif r.get("legacy_interp") is not None:
            meta["expected_from"] = "legacy-interp"
            meta["exit"] = r["legacy_interp"]
    if r["bucket"].endswith("e-spec"):
        note += " " + E_SPEC_NOTE
    if r["bucket"].endswith("c-unreviewed"):
        note += " " + C_NOTE
    if note.strip():
        meta["note"] = note.strip()
    else:
        meta.pop("note", None)
    write_toml(entry / "meta.toml", meta)


def in_slice(tags) -> bool:
    return not any(t.startswith("post-m1:") or t == "bench" for t in tags)


def report(corpus: Path) -> str:
    rows = []
    for e in entries(corpus):
        m = read_toml(e / "meta.toml")
        if m.get("bucket") or m.get("class") == "d":
            rows.append((str(e.relative_to(corpus)), m))
    passed = lambda m: m.get("class") in PASSING or m.get("bucket", "").endswith("e-spec")  # noqa: E731
    sl = [(r, m) for r, m in rows if m.get("class") not in ("d", "unknown") and in_slice(m.get("tags", []))]
    core = [(r, m) for r, m in sl if not any(t.startswith(("lib:", "io:")) for t in m.get("tags", []))]
    allc = [(r, m) for r, m in rows if m.get("class") not in ("d", "unknown")]
    bench = [(r, m) for r, m in rows if "bench" in m.get("tags", []) and m.get("class") not in ("d", "unknown")]
    pct = lambda xs: (f"{sum(passed(m) for _, m in xs)} / {len(xs)} = "  # noqa: E731
                      f"{100 * sum(passed(m) for _, m in xs) / len(xs):.1f}%") if xs else "0 / 0"
    def head(b):
        # `b->a` stays apart from `a`: the program passed after `karac fix`.
        if b.startswith("b->"):
            return "b->" + head(b[3:])
        return re.sub(r":.*", "", b) if b.startswith(("mir-gap", "e-", "check", "b-")) else b
    by = collections.Counter(head(m.get("bucket", "?")) for _, m in sl)
    gaps = collections.Counter(m["bucket"][8:] for _, m in sl if m.get("bucket", "").startswith("mir-gap:"))
    post = collections.Counter(t for _, m in allc for t in m.get("tags", []) if t.startswith("post-m1:"))
    lines = [
        "# Corpus classification for the M1 gate",
        "",
        "Generated by `scripts/corpus/classify.py`; rules in `review/corpus-classification.md` §3 and its",
        "gate rules of 2026-10-08. Each entry is one deduplicated program, run on a release-built",
        "`karac __mir-run` with a 60 s timeout.",
        "",
        f"- **Gate (the M1 slice): {pct(sl)}** passing (a, b, c, auto-resolved e-spec); bar 95%.",
        f"- Slice without `lib:`/`io:` programs: {pct(core)}.",
        f"- Every classified non-deferred program (context): {pct(allc)}.",
        f"- Bench variants (outside the gate): {pct(bench)}; timeouts "
        f"{sum(m.get('bucket') == 'timeout' for _, m in bench)}.",
        f"- Deferred (class d): {sum(m.get('class') == 'd' for _, m in rows)}; "
        f"outside the slice (`post-m1:*`): {sum(not in_slice(m.get('tags', [])) and 'bench' not in m.get('tags', []) for _, m in allc)}.",
        "",
        "## Denominator by bucket (slice)",
        "",
        "| bucket | programs |",
        "|---|---|",
    ]
    lines += [f"| {b} | {n} |" for b, n in by.most_common()]
    lines += ["", "## MIR gaps in the slice, by construct", "", "| construct | programs |", "|---|---|"]
    lines += [f"| {g} | {n} |" for g, n in gaps.most_common(60)]
    lines += ["", "## post-m1 tags (all classified)", "", "| tag | programs |", "|---|---|"]
    lines += [f"| {t} | {n} |" for t, n in post.most_common()]
    spec = sorted(r for r, m in rows if m.get("bucket", "").endswith("e-spec"))
    lines += ["", f"## Auto-resolved e-spec ({len(spec)}): MIR matches the spec's model or pin, legacy does not", ""]
    lines += [f"- `{r}`" for r in spec]
    return "\n".join(lines) + "\n"


def compiler_commit() -> str:
    """HEAD, or `<base>+local` when HEAD is not on origin/main yet (a rebase
    would rename it), so the recorded commit always exists upstream."""
    git = lambda *a: subprocess.run(["git", *a], cwd=ROOT, capture_output=True, text=True)  # noqa: E731
    head = git("rev-parse", "--short=9", "HEAD").stdout.strip()
    if git("merge-base", "--is-ancestor", "HEAD", "origin/main").returncode == 0:
        return head
    base = git("merge-base", "HEAD", "origin/main").stdout.strip()[:9]
    return f"{base}+local" if base else head


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    # A release karac: the gate measures what MIR does, and a timeout should
    # not depend on how fast a debug-built interpreter runs.
    ap.add_argument("--karac", default=str(ROOT / "target/release/karac"))
    ap.add_argument("--corpus", default=str(ROOT / "corpus"))
    ap.add_argument("--filter", action="append", default=[])
    ap.add_argument("--bucket", action="append", default=[],
                    help="re-classify only entries whose recorded bucket starts with this")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 4)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--report-only", action="store_true")
    ap.add_argument("--retag", action="store_true",
                    help="recompute the slice tags from each source without running anything, then report")
    ap.add_argument("--resume", action="store_true",
                    help="skip entries already classified with this compiler commit")
    args = ap.parse_args()
    corpus = Path(args.corpus)
    if args.retag:
        for e in entries(corpus):
            meta = read_toml(e / "meta.toml")
            src = (e / ("source.orig.kara" if meta.get("fixed_by_classify") else "source.kara")).read_text()
            tags = [t for t in meta.get("tags", []) if not t.startswith(("post-m1:", "lib:", "io:")) and t != "bench"]
            tags += slice_tags(src, str(e.relative_to(corpus)))
            if tags != meta.get("tags", []):
                meta["tags"] = tags
                write_toml(e / "meta.toml", meta)
    elif not args.report_only:
        sha = compiler_commit()
        rev = model_rev()
        todo = [e for e in entries(corpus)
                if (not args.filter or any(f in str(e.relative_to(corpus)) for f in args.filter))
                and (not args.bucket
                     or any(str(read_toml(e / "meta.toml").get("bucket", "")).startswith(b) for b in args.bucket))
                and not (args.resume and read_toml(e / "meta.toml").get("classified_with") == sha)]
        n = 0
        with cf.ThreadPoolExecutor(args.jobs) as ex:
            futs = {ex.submit(classify, e, corpus, args, rev, sha): e for e in todo}
            for f in cf.as_completed(futs):
                e = futs[f]
                try:
                    r = f.result()
                except Exception as x:  # noqa: BLE001
                    print(f"CRASH {e.relative_to(corpus)}: {x}", file=sys.stderr)
                    continue
                write_back(e, r, sha)
                n += 1
                if n % 500 == 0:
                    print(f"classified {n}/{len(todo)}", file=sys.stderr)
    text = report(corpus)
    (corpus / "CLASSIFICATION.md").write_text(text)
    print("\n".join(text.splitlines()[5:11]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
