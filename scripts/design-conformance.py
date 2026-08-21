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

# A fragment naming a type, function, or EFFECT RESOURCE declared in some OTHER
# block is the common case in a spec, not a defect — the doc is read in order, the harness
# compiles each block alone. Those are bucketed as `unresolved` rather than
# counted as divergences. They are not discarded, though: the undefined names
# are tallied and printed, because a spec referring to something the compiler
# has never heard of is exactly the shape of B-2026-08-17-38 (`TreeMap`,
# documented 13 times, implemented zero).
#
# `undefined effect resource 'X'` belongs in the same bucket: design.md declares
# `effect resource Heap;` (and `Console`, `Log`, …) in the section that
# introduces them and then writes `with allocates(Heap)` for pages afterward.
# `Heap` in particular is NOT a prelude resource — `src/prelude.rs`'s
# `PRELUDE_EFFECT_RESOURCES` comment says the primitives are "registered
# incrementally as their method surfaces land" — so every signature carrying it
# trips resolution. That is an out-of-block reference, not a divergence.
UNRESOLVED_RE = re.compile(
    r"^undefined (?:effect )?(name|type|module|trait|field|resource) '([^']+)'"
)


# Every name design.md itself declares, anywhere in any block. This is what
# separates the two things `undefined name 'X'` can mean:
#
#   `undefined name 'Config'`  — design.md declares `struct Config` three blocks
#                                up. The doc is read in order; the harness
#                                compiles each block alone. Not a defect.
#   `undefined name 'io'`      — NOTHING declares it, here or in the compiler.
#                                The spec expects the implementation to provide
#                                it and the implementation does not.
#
# The message shape is identical, so without this index the second hides inside
# the first — which is how design.md's whole `io.` I/O surface sat in the
# "out-of-block references" bucket looking like ordinary prose ordering.
DECLARES_RE = re.compile(
    r"^\s*(?:pub\s+)?(?:unsafe\s+)?(?:shared\s+)?"
    r"(?:struct|enum|trait|type|const|static|distinct|layout|module|union|macro|"
    r"marker|fn)\s+([A-Za-z_]\w*)(?!\.)"
    r"|^\s*effect\s+resource\s+([A-Za-z_]\w*)"
    r"|^\s*(?:pub\s+)?effect\s+([A-Za-z_]\w*)"
)
# A bare type parameter is not a missing name — it is the signature's own
# variable, unbound because the enclosing `impl[K, V]` lives in another block.
GENERIC_NAME_RE = re.compile(r"^[A-Z][0-9]?$|^(?:Eff|Self)$")


def collect_declared_names(blocks):
    names = set()
    for b in blocks:
        for line in b.code:
            m = DECLARES_RE.match(line)
            if m:
                names.add(next(g for g in m.groups() if g))
            # `[T, U]` / `[K: Hash, V]` — every generic parameter introduced
            # anywhere counts, since a catalogue's `T` is bound by an `impl`
            # header the harness never sees.
            for grp in re.findall(r"\[([^\]\[]*)\]", line):
                for part in grp.split(","):
                    n = part.split(":")[0].strip().removeprefix("with ").strip()
                    if re.fullmatch(r"[A-Za-z_]\w*", n):
                        names.add(n)
    return names


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


# ---------------------------------------------------------------------------
# Signature catalogues
#
# A fifth of the baseline is a shape no framing above can reach: a table of
# method SHAPES with no bodies, which is not a program in any wrapping.
#
#     Vec.filled(n: i64, val: T) -> Vec[T] where T: Clone
#     fn or_insert(self, default: V) -> mut ref V
#     Channel.new[T]() -> (Sender[T], Receiver[T])
#     fn io.read_line() -> Result[String, IoError] with reads(Stdin)
#
# Setting them aside was the suite's biggest blind spot (B-2026-08-21-28): 21
# blocks carrying 60 signature lines, unchecked — and the class the suite is
# strongest at. B-2026-08-21-10 was four documented stdlib entry points that did
# not exist, and every one was found by a human reading the doc rather than by
# the sweep, because they live in exactly this shape.
#
# A signature is not a program, but it IS a declaration, and declarations are
# checkable. Two probes cover the 60:
#
#   (a) `fn name(args) -> R [with E]`  ->  `trait _Probe { <line>; }`
#       A bodiless method declaration is legal in a trait, so this typechecks
#       the parameter and return types and the effect clause. It catches a
#       signature naming a type that does not exist. It says NOTHING about
#       whether the method is implemented — a trait can declare anything.
#
#   (b) `Type.method(args) -> R`  ->  `let _ = Type.method(<fillers>);`
#       This is the probe that answers "does it exist", and it is where the
#       `Channel.bounded` / `io.read_line` class lives.
#
# WHY FILLER ARGUMENTS ARE ENOUGH. (b) needs argument values, and the signature
# gives types, not values. A per-type value table was the obvious answer and is
# not needed: measured, `karac` reports a WRONG ARGUMENT TYPE as `expected
# 'i64', found 'String'` — a message that presupposes the function was found —
# while a name that does not exist is `no associated function 'filled' on type
# 'Vec'`. So the probe passes `0` for every argument and classifies on the
# diagnostic rather than on success. ARITY does matter (a call with the wrong
# argument count reports as `no associated function`, since resolution is
# arity-aware), and arity is the one thing a signature always states.
#
# That makes (b) a different question from every other rung — "does this name
# resolve", not "does this call typecheck" — so its non-resolution diagnostics
# are dropped rather than counted, and it gets its own outcome rather than
# being folded into `conforms`.

SIG_FN_RE = re.compile(
    r"^(?:pub\s+)?(?:unsafe\s+)?fn\s+"
    r"(?P<name>[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)?)\s*"
    r"(?P<generics>\[[^\]]*\])?\s*\("
)
# `Vec.filled(...)` / `Channel.new[T](...)` — an associated function written the
# way a doc table writes one, with no `fn`.
SIG_ASSOC_RE = re.compile(
    r"^(?P<ty>[A-Z]\w*)\.(?P<method>[a-z_]\w*)\s*(?P<generics>\[[^\]]*\])?\s*\("
)
# A free function written without its `fn` — the § Provider-Rooted Resources
# table spells `with_provider[R: effect resource, …](…) -> T with E` this way.
SIG_BARE_FN_RE = re.compile(
    r"^(?P<name>[a-z_]\w*)\s*(?P<generics>\[[^\]]*\])\s*\("
)
# `<field-path>` and friends are doc placeholders standing in for syntax the
# spec has not settled, not Kāra. A signature carrying one is prose with a hole
# in it — the same thing `...` marks — so it is skipped rather than failed.
SIG_PLACEHOLDER_RE = re.compile(r"<[a-z-]+>")
# A line that continues the signature above it rather than starting a new one.
# `->` ends in a non-word character, so a trailing `\b` never fires after it —
# which silently truncated every signature whose return type sat on its own
# line, and let the harness walk on into the function BODY below it.
SIG_CONTINUATION_RE = re.compile(r"^(?:(?:with|requires|ensures|where)\b|->)")


def _depth(line):
    return (line.count("(") + line.count("[") + line.count("{")
            - line.count(")") - line.count("]") - line.count("}"))


def split_signature_declarations(code):
    """Pull the bodiless declarations out of a block, one per returned string.

    Flattens rather than parses: a declaration starts at any line matching
    SIG_FN_RE / SIG_ASSOC_RE and runs until its brackets balance and no
    continuation clause follows. Walking the lines this way reaches inside an
    `impl … { }` or `extern "C" { }` wrapper without modelling either, and
    steps over the wrapper braces, struct bodies and worked call examples that
    share these blocks. A declaration whose balanced form ends in `{` has a
    BODY — that is real code the ladder above already owns — so it is dropped.
    """
    lines = [re.sub(r"//.*$", "", l).rstrip() for l in code]
    decls, i = [], 0
    while i < len(lines):
        stripped = lines[i].strip()
        if not _starts_signature(stripped):
            i += 1
            continue
        parts, depth = [], 0
        while i < len(lines):
            parts.append(lines[i].strip())
            depth += _depth(lines[i])
            i += 1
            if depth > 0:
                continue
            nxt = next((l.strip() for l in lines[i:] if l.strip()), "")
            if not SIG_CONTINUATION_RE.match(nxt):
                break
        decl = " ".join(x for x in parts if x).strip().rstrip(";")
        # A `{` on the next line means this declaration has a BODY: it is a real
        # function, not a catalogue entry, and the ladder above already owns it.
        # Skip past the body so its statements are not mistaken for more
        # signatures — `File.open(path)?.lines()` inside one read as a
        # `Type.method(…)` catalogue line and got probed, which is how a worked
        # example turns into a phantom finding.
        nxt = next((l.strip() for l in lines[i:] if l.strip()), "")
        if nxt.startswith("{") or "{" in decl:
            i = _skip_body(lines, i)
            continue
        if SIG_PLACEHOLDER_RE.search(decl):
            continue
        decls.append(decl)
    return decls


def _starts_signature(stripped):
    return bool(SIG_FN_RE.match(stripped)
                or SIG_ASSOC_RE.match(stripped)
                or SIG_BARE_FN_RE.match(stripped))


def _skip_body(lines, i):
    """Advance past a brace-delimited body starting at or after line `i`."""
    depth = 0
    while i < len(lines):
        if "{" in lines[i] or depth:
            depth += _depth(lines[i])
            i += 1
            if depth <= 0:
                return i
        else:
            i += 1
    return i


def _call_probe(decl, match):
    """`Type.method[T](a, b) -> R` -> `Type.method[i64](0, 0)`.

    Probed AS WRITTEN, type arguments included: the documented spelling is what
    a reader would type, so a form that only works with the type argument
    dropped is a finding, not something to paper over.

    `match` is a SIG_ASSOC_RE / SIG_FN_RE match on `decl`; both patterns end at
    the parameter list's open paren, so `match.end() - 1` locates it without
    re-scanning for a `(` that might belong to a type instead.
    """
    ty, _, method = _probe_name(match)
    open_paren = match.end() - 1
    args, depth = [], 0
    cur = ""
    for ch in decl[open_paren + 1:]:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            if depth == 0:
                break
            depth -= 1
        if ch == "," and depth == 0:
            args.append(cur)
            cur = ""
        else:
            cur += ch
    if cur.strip():
        args.append(cur)
    targs = ""
    generics = match.groupdict().get("generics")
    if generics:
        names = [g.split(":")[0].strip() for g in generics[1:-1].split(",") if g.strip()]
        targs = "[" + ", ".join("i64" for _ in names) + "]" if names else ""
    return f"{ty}.{method}{targs}({', '.join('0' for _ in args)})"


def _probe_name(match):
    """(receiver, '.', method) for either signature shape."""
    g = match.groupdict()
    if "ty" in g and g.get("ty"):
        return g["ty"], ".", g["method"]
    return g["name"].partition(".")


def signature_probes(decls):
    """(trait-declaration source, [(probe expression, declaration)]) for a block."""
    items, calls = [], []
    for n, decl in enumerate(decls):
        if SIG_BARE_FN_RE.match(decl) and not SIG_FN_RE.match(decl):
            decl = "fn " + decl               # the doc omits it; the grammar does not
        m_fn = SIG_FN_RE.match(decl)
        if m_fn and "." not in m_fn.group("name"):
            body = re.sub(r"^(?:pub\s+)", "", decl)   # `pub` is not legal on a trait method
            items.append(f"trait _Probe{n} {{ {body}; }}")
            continue
        if m_fn:                                       # `fn io.read_line() -> …`
            calls.append((_call_probe(decl, m_fn), decl))
            continue
        m_assoc = SIG_ASSOC_RE.match(decl)
        if m_assoc:
            calls.append((_call_probe(decl, m_assoc), decl))
    return items, calls


# `type FILE;` inside an `extern "C" { }` block, `struct Entry[K, V] { … }`
# above an `impl` — a catalogue's own supporting declarations, which the
# flattening walk steps over on its way to the signatures.
LOCAL_TYPE_RE = re.compile(r"^\s*(?:pub\s+)?type\s+([A-Za-z_]\w*)\s*;")


def local_type_stubs(code):
    """Opaque-type declarations the block makes for its own signatures to use."""
    return [f"struct {m.group(1)} {{}}"
            for m in (LOCAL_TYPE_RE.match(l) for l in code) if m]


def evaluate_signatures(block, karac):
    """Check a signature catalogue. Returns (status, diagnostics) or None.

    None means the block yielded no signatures, so this rung does not apply and
    the caller should report the ladder's verdict instead.
    """
    decls = split_signature_declarations(block.code)
    items, calls = signature_probes(decls)
    if not items and not calls:
        return None
    outside = []
    diags = []
    if items:
        preamble = "\n".join(local_type_stubs(block.code))
        _, d = run_karac(karac,
                         (preamble + "\n" if preamble else "")
                         + "\n".join(items) + "\n\nfn main() {}\n")
        # A catalogue is out-of-block references by construction — its `T` is
        # bound by an `impl` header in another block, its `Heap` by an `effect
        # resource` declaration pages away. Judging the rung on those would fail
        # every catalogue for the one thing that is not a divergence. They are
        # returned alongside so the caller can still bucket them.
        diags += [x for x in d if not UNRESOLVED_RE.match(x.get("message", ""))]
        outside += [x for x in d if UNRESOLVED_RE.match(x.get("message", ""))]
    for probe, decl in calls:
        _, d = run_karac(karac, "fn main() {\n    let _ = %s;\n}\n" % probe)
        # Resolution only: the arguments are fillers, so a type complaint about
        # one of them is the probe's own doing and, more to the point, is proof
        # the name resolved.
        for x in d:
            if RESOLUTION_ERROR_RE.match(x.get("message", "")):
                x = dict(x, message=f"{x['message']}  [probe: {probe}]",
                         signature=decl)
                diags.append(x)
    return ("rejected" if diags else "pass"), (diags or outside)


# The diagnostics that mean A NAME DOES NOT EXIST, as opposed to one that exists
# and was called wrongly. Only these decide a `Type.method(...)` probe.
RESOLUTION_ERROR_RE = re.compile(
    r"^no (?:associated function|method|associated type|associated const|"
    r"variant|field) '"
    r"|^undefined (?:effect )?(?:name|type|module|trait|field|resource) '"
    r"|^unknown module"
)


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
    # Nothing in the ladder compiled. Before calling that a divergence, ask the
    # one remaining question a bodiless signature table can answer.
    sig = evaluate_signatures(block, karac)
    if sig is not None:
        status, diags = sig
        return (status, "signatures", diags)
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
    declared = collect_declared_names(blocks)
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
        if (diags and all(UNRESOLVED_RE.match(d.get("message", "")) for d in diags)
                and (status == "rejected" or rung == "signatures")):
            missing = sorted({UNRESOLVED_RE.match(d["message"]).group(2)
                              for d in diags})
            # Split the two meanings of `undefined name 'X'`. A name design.md
            # declares elsewhere is prose ordering; a name NOTHING declares is
            # the `TreeMap` / `io.` shape — the spec promising a surface the
            # implementation never grew.
            #
            # Only a SIGNATURE CATALOGUE gets this treatment. A catalogue is a
            # claim about the API surface, so an undefined name in one is a
            # promise nothing keeps. A worked example is illustration, and its
            # undefined `load_config` / `pool` / `normalize` are stage
            # furniture — scoring those the same way turned 28 out-of-block
            # blocks into 130 findings, none of them about the compiler.
            orphan = [n for n in missing
                      if rung == "signatures"
                      and n not in declared and not GENERIC_NAME_RE.match(n)]
            results.append({"hash": b.hash, "line": b.line, "heading": b.heading,
                            "outcome": "UNDECLARED-NAME" if orphan else "unresolved",
                            "rung": rung, "expects_error": b.expects_error,
                            "missing": missing, "undeclared": orphan,
                            "diagnostics": diags[:4]})
            continue
        if b.expects_error:
            outcome = "confirmed-rejection" if status == "rejected" else "MISSING-REJECTION"
        elif rung == "signatures":
            outcome = "signatures-ok" if status == "pass" else "SIGNATURE-MISSING"
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
            "deferred", "signatures-ok"}
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
    sigok = sum(1 for r in results if r["outcome"] == "signatures-ok")
    orphans = Counter(n for r in results for n in r.get("undeclared", ()))
    if orphans:
        top = ", ".join(f"{n}({c})" for n, c in orphans.most_common(12))
        print(f"  names NOTHING defines — not design.md, not the compiler: {top}")
    print(f"\ndesign.md conformance: {total} blocks — {conf} accepted, "
          f"{rej} rejected-as-specified, {eli} elided (prose with `...`), "
          f"{dfr} under Deferred Items, "
          f"{len(unres)} referencing out-of-block declarations, "
          f"{sigok} signature catalogues whose names all resolve, "
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
