#!/usr/bin/env python3
"""Check that docs/design.md's Kāra examples agree with the compiler.

design.md is the authoritative language spec, and its ```kara blocks are the
part of it a machine can read. 12% of the bug ledger (176 rows, 62 of them
high-severity) is some form of "design.md says X, the implementation does Y" —
found, so far, one hand-read section at a time. This turns that into a sweep.

WHAT IT CHECKS. Every fenced ```kara block is fed to `karac check`. A block is
expected to be accepted unless it says otherwise; a block carrying an inline
`// compile error: …` / `// ERROR` annotation is expected to be REJECTED, and a
block accepted when the spec says it should not be is a divergence exactly as
much as the reverse.

THE WRAPPING LADDER. Most blocks are fragments — a signature, a trait, a couple
of statements — so each is tried against a ladder of framings and passes if any
of them is accepted:

    1. as written (module-level items)
    2. as written, plus a synthesized empty `fn main`
    3. wrapped in `fn main() { … }` (statement fragments)
    4. wrapped as the body of a value binding (expression fragments)

A block that fails every rung is either a real divergence or not
self-contained. Which one it is cannot be decided mechanically, so the answer
lives in a checked-in BASELINE keyed by content hash: each non-conforming block
is recorded with a one-line reason. The suite fails on any block that is not in
the baseline and does not pass — new prose, or a regression, both surface. The
baseline is meant to shrink.

Usage:
    scripts/design-conformance.py                     # report to stdout
    scripts/design-conformance.py --json out.json     # machine-readable
    scripts/design-conformance.py --update-baseline   # rewrite the baseline
    scripts/design-conformance.py --check             # CI gate: exit 1 on drift
    scripts/design-conformance.py --only <hash|substr> # one block, verbose
"""
import argparse
from collections import Counter
import hashlib
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DESIGN = ROOT / "docs" / "design.md"
BASELINE = ROOT / "docs" / "design-conformance-baseline.json"

# An inline annotation that marks the SURROUNDING BLOCK as one the spec expects
# the compiler to reject. Deliberately narrow: `// error[E0200]: …` and
# `// compile error: …` are unambiguous, whereas a bare "error" in prose is not.
ERROR_MARKER_RE = re.compile(
    r"\b(?:compile error|compile-time error|error\[[A-Za-z0-9]+\]|ERROR)\b|^\s*error:"
)
CARET_ERROR_RE = re.compile(r"^\s*//?\s*\^+\s*error", re.I)


def expects_error_annotation(code):
    """True when the block carries an annotation saying the spec expects it
    REJECTED.

    Deliberately narrow. The marker must be a TRAILING comment on a line that
    also has code, or a caret-underline line — the two conventions design.md
    actually uses:

        Map[f64, String]                 // compile error: f64 has no Hash
        let CONFIG: Config = load_config("app.toml");
        //                   ^^^^^^^^^^^^^^^^^^^^^^^ error: effectful call

    A standalone prose comment mentioning "compile error" does NOT count. An
    earlier version accepted those and mis-flagged the `#[no_effect]` block,
    whose header comment reads "Heap use anywhere below this boundary is a
    compile error" — a description of what the attribute DOES, in a block that
    contains no heap use and correctly compiles.
    """
    for line in code:
        if CARET_ERROR_RE.match(line):
            return True
        head, sep, tail = line.partition("//")
        if sep and head.strip() and ERROR_MARKER_RE.search(tail):
            return True
    return False


# "// OK" / "// ok —" marks a line the spec says is ACCEPTED. A block may carry
# both, in which case the error annotation wins: it must be rejected somewhere.
EXPECT_OK_RE = re.compile(r"//\s*OK\b")

# `...` is the spec's elision marker (`fn f() { ... }`, `let x = ...;`). A block
# carrying one is prose with holes in it, not a program, and no framing makes it
# compile. These are reported separately rather than counted as divergences —
# calling them failures would bury the real ones under noise.
ELISION_RE = re.compile(r"\.\.\.")

# An item keyword in column 0 means the block is module-level declarations, so it
# must NOT be wrapped in `fn main`. Anything else is treated as statements.
ITEM_START_RE = re.compile(
    r"^pub\b"                      # `pub` can only introduce an item
    r"|^(fn|struct|enum|trait|impl|type|const|static|distinct|shared|layout|"
    r"effect|resource|module|use|extern|union|macro|import|host|subscript|"
    r"unsafe|marker|comptime)\b"
    r"|^#\["
)

# A fragment naming a type or function declared in some OTHER block is the
# common case in a spec, not a defect — the doc is read in order, the harness
# compiles each block alone. Those are bucketed as `unresolved` rather than
# counted as divergences. They are not discarded, though: the undefined names
# are tallied and printed, because a spec referring to something the compiler
# has never heard of is exactly the shape of B-2026-08-17-38 (`TreeMap`,
# documented 13 times, implemented zero).
UNRESOLVED_RE = re.compile(r"^undefined (name|type|module|trait|field) '([^']+)'")


class Block:
    def __init__(self, line, heading, code, info, path=()):
        self.line = line
        self.heading = heading
        self.path = tuple(path)
        self.code = code
        self.info = info
        body = "\n".join(l.rstrip() for l in code).strip()
        self.hash = hashlib.sha1(body.encode()).hexdigest()[:12]

    @property
    def text(self):
        return "\n".join(self.code)

    @property
    def expects_error(self):
        if "expect-error" in self.info:
            return True
        if "expect-ok" in self.info:
            return False
        return expects_error_annotation(self.code)

    @property
    def skipped(self):
        return "ignore" in self.info

    @property
    def deferred(self):
        """A block under design.md's own `Deferred Items` section illustrates
        syntax the compiler is NOT expected to accept yet — `r"..."` raw
        strings, effect-variable bounds, portable SIMD. Counting those as
        divergences would be reading the spec's roadmap as its contract."""
        return any("deferred" in h.lower() for h in self.path)

    @property
    def elided(self):
        return bool(ELISION_RE.search(self.text))

    @property
    def item_shaped(self):
        for line in self.code:
            t = line.strip()
            if not t or t.startswith("//"):
                continue
            return bool(ITEM_START_RE.match(line.lstrip()))
        return False


def parse_blocks(path):
    lines = path.read_text(encoding="utf-8").split("\n")
    blocks, headings, i = [], [], 0
    while i < len(lines):
        m = re.match(r"^(#{1,6})\s+(.*)$", lines[i])
        if m:
            depth = len(m.group(1))
            headings = headings[: depth - 1] + [m.group(2).strip()]
        fence = re.match(r"^```kara(.*)$", lines[i].strip())
        if fence:
            info = fence.group(1).strip()
            j = i + 1
            while j < len(lines) and lines[j].strip() != "```":
                j += 1
            blocks.append(
                Block(i + 2, " > ".join(headings[-2:]), lines[i + 1 : j], info,
                      headings)
            )
            i = j + 1
        else:
            i += 1
    return blocks


def rungs(block):
    """The framings this block is tried in, most likely first.

    Order matters for more than speed: the FIRST rung is the one whose
    diagnostics get reported when every rung fails, so it has to be the framing
    the block was actually written for. Sorting by "fewest errors" instead
    produced 88 blocks blaming `\'fn\' is a reserved keyword` — the noise of
    declarations stuffed inside a `fn main` that was never the right frame.
    """
    body = "\n".join(block.code)
    indented = "\n".join(("    " + l) if l.strip() else l for l in block.code)
    # Some blocks are a catalogue of TYPES rather than code —
    # `Tensor[f64, [3, 4, ?]]` and its two siblings, three lines that are not
    # statements in any framing. Binding each to a throwaway alias is what makes
    # them checkable at all; without this rung they read as parse failures and
    # bury a real answer (the shape syntax turns out to be accepted).
    stripped = [re.sub(r"\s*//.*$", "", l).strip().rstrip(";,")
                for l in block.code]
    aliases = "\n".join(f"type _Probe{i} = {t};"
                         for i, t in enumerate(stripped) if t) + "\n\nfn main() {}\n"
    items = ("items", body, 0)
    items_main = ("items+main", body + "\n\nfn main() {}\n", 0)
    in_main = ("in-main", "fn main() {\n" + indented + "\n}\n", -1)
    type_rung = ("type-aliases", aliases, 0)
    if block.item_shaped:
        return [items, items_main, in_main]
    return [in_main, items, items_main, type_rung]


def run_karac(karac, source, extra_timeout=60):
    with tempfile.NamedTemporaryFile(
        "w", suffix=".kara", delete=False, encoding="utf-8"
    ) as f:
        f.write(source)
        tmp = f.name
    try:
        p = subprocess.run(
            [karac, "check", "--output=json", tmp],
            capture_output=True,
            text=True,
            timeout=extra_timeout,
        )
        try:
            payload = json.loads(p.stdout or "{}")
        except json.JSONDecodeError:
            return None, [{"message": (p.stderr or p.stdout or "").strip()[:400],
                           "phase": "harness", "line": 0}]
        # A program carrying `#[target(...)]` is checked once per target, and
        # `karac check --output=json` then returns `{"targets": [{..., "diagnostics":
        # [...]}, ...]}` instead of a top-level `diagnostics` array. Reading only
        # the top level made those blocks report as REJECTED WITH NO DIAGNOSTICS —
        # a failure with nothing to explain it, which is the least useful thing a
        # checker can say.
        raw = payload.get("diagnostics")
        if raw is None and isinstance(payload.get("targets"), list):
            raw = [d for t in payload["targets"] for d in t.get("diagnostics", [])]
        diags = [d for d in (raw or []) if d.get("severity") == "error"]
        return p.returncode, diags
    finally:
        Path(tmp).unlink(missing_ok=True)


def evaluate(block, karac):
    """Return (status, rung, diagnostics). status is pass or rejected."""
    attempts = []
    for name, source, offset in rungs(block):
        rc, diags = run_karac(karac, source)
        if rc == 0 and not diags:
            return ("pass", name, [])
        attempts.append((name, diags, offset))
    name, diags, offset = attempts[0]
    for d in diags:
        d["design_line"] = block.line + max(0, d.get("line", 1) - 1) + offset
    return ("rejected", name, diags)


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

    blocks = parse_blocks(DESIGN)
    if args.only:
        blocks = [b for b in blocks if args.only in b.hash or args.only in b.text]
        if not blocks:
            sys.exit(f"no block matches {args.only!r}")

    baseline = {}
    if BASELINE.exists():
        baseline = json.loads(BASELINE.read_text(encoding="utf-8")).get("blocks", {})

    results = []
    for b in blocks:
        if b.skipped or b.elided or b.deferred:
            outcome = ("skipped" if b.skipped
                       else "deferred" if b.deferred else "elided")
            results.append({"hash": b.hash, "line": b.line, "heading": b.heading,
                            "outcome": outcome, "rung": None, "diagnostics": []})
            continue
        status, rung, diags = evaluate(b, args.karac)
        # An expect-error block that WAS rejected is confirmed, whatever phase
        # rejected it — the unresolved bucket below must not swallow it. (The
        # weak spot: we check that it was rejected, not that it was rejected for
        # the annotated reason. Matching the message is the next increment.)
        if b.expects_error and status == "rejected":
            results.append({"hash": b.hash, "line": b.line, "heading": b.heading,
                            "outcome": "confirmed-rejection", "rung": rung,
                            "expects_error": True, "diagnostics": diags[:4]})
            continue
        if (status == "rejected" and diags
                and all(d.get("phase") == "resolve"
                        and UNRESOLVED_RE.match(d.get("message", "")) for d in diags)):
            missing = [UNRESOLVED_RE.match(d["message"]).group(2) for d in diags]
            results.append({"hash": b.hash, "line": b.line, "heading": b.heading,
                            "outcome": "unresolved", "rung": rung,
                            "expects_error": b.expects_error,
                            "missing": sorted(set(missing)),
                            "diagnostics": diags[:4]})
            continue
        if b.expects_error:
            outcome = "confirmed-rejection" if status == "rejected" else "MISSING-REJECTION"
        else:
            outcome = "conforms" if status == "pass" else "REJECTED"
        results.append({"hash": b.hash, "line": b.line, "heading": b.heading,
                        "outcome": outcome, "rung": rung,
                        "expects_error": b.expects_error,
                        "diagnostics": diags[:4]})
        if args.only:
            print(f"# block {b.hash} @ design.md:{b.line}  [{b.heading}]")
            print(f"# expects_error={b.expects_error} outcome={outcome} rung={rung}")
            print(b.text)
            for d in diags:
                print(f"  -> {d.get('phase')}: {d.get('message')} "
                      f"(design.md:{d.get('design_line')})")

    good = {"conforms", "confirmed-rejection", "skipped", "elided", "unresolved",
            "deferred"}
    bad = [r for r in results if r["outcome"] not in good]

    if args.update_baseline:
        entries = {}
        for r in bad:
            prev = baseline.get(r["hash"], {})
            entries[r["hash"]] = {
                "line": r["line"],
                "heading": r["heading"],
                "outcome": r["outcome"],
                "reason": prev.get("reason", "UNTRIAGED — classify me"),
                "first_diagnostic": (r["diagnostics"][0].get("message")
                                     if r["diagnostics"] else None),
            }
        BASELINE.write_text(
            json.dumps({"note": "Non-conforming design.md blocks. Shrink me. "
                                "`reason` says why a block is here; UNTRIAGED "
                                "means nobody has decided yet.",
                        "blocks": dict(sorted(entries.items(),
                                              key=lambda kv: kv[1]["line"]))},
                       indent=2, ensure_ascii=False) + "\n",
            encoding="utf-8")
        print(f"baseline updated: {len(entries)} non-conforming blocks")

    if args.json_out:
        Path(args.json_out).write_text(json.dumps(results, indent=2), encoding="utf-8")

    total = len(results)
    conf = sum(1 for r in results if r["outcome"] == "conforms")
    rej = sum(1 for r in results if r["outcome"] == "confirmed-rejection")
    eli = sum(1 for r in results if r["outcome"] == "elided")
    dfr = sum(1 for r in results if r["outcome"] == "deferred")
    unres = [r for r in results if r["outcome"] == "unresolved"]
    print(f"\ndesign.md conformance: {total} blocks — {conf} accepted, "
          f"{rej} rejected-as-specified, {eli} elided (prose with `...`), "
          f"{dfr} under Deferred Items, "
          f"{len(unres)} referencing out-of-block declarations, "
          f"{len(bad)} non-conforming")
    if unres:
        names = Counter(n for r in unres for n in r["missing"])
        top = ", ".join(f"{n}({c})" for n, c in names.most_common(12))
        print(f"  most-referenced undefined symbols: {top}")

    if args.check:
        new = [r for r in bad if r["hash"] not in baseline]
        fixed = [h for h in baseline if h not in {r["hash"] for r in bad}
                 and any(b.hash == h for b in blocks)]
        for r in new:
            print(f"  NEW  design.md:{r['line']} [{r['heading']}] {r['outcome']}")
            for d in r["diagnostics"][:2]:
                print(f"       {d.get('phase')}: {d.get('message')}")
        for h in fixed:
            print(f"  FIXED {baseline[h]['line']} [{baseline[h]['heading']}] "
                  f"— remove from the baseline")
        if new:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
