# design.md conformance suite

`scripts/design-conformance.py` feeds every ```kara block in
[`design.md`](design.md) to `karac check` and reports where the spec and the
implementation disagree.

## Why

12% of `bug-ledger.jsonl` — 176 rows, **62 of them high-severity** — is some
form of *"design.md says X, the implementation does Y"*. Until now every one of
those was found by a person reading a section and noticing. Probes find them at
five times the rate katas do, which says the yield is in **reading the spec
adversarially**, not in writing more programs. This turns that reading into a
sweep that runs on every push.

It is deliberately narrow. It checks the machine-readable part of a prose
specification — whether the compiler accepts what the spec writes, and rejects
what the spec says is rejected. It says nothing about whether accepted code
*means* the right thing.

## Running it

```bash
cargo build --bin karac                       # front end only; no LLVM needed
python3 scripts/design-conformance.py         # report
python3 scripts/design-conformance.py --check # CI gate: exit 1 on drift
python3 scripts/design-conformance.py --only 'Iterator[Item ='   # one block, verbose
python3 scripts/design-conformance.py --update-baseline
```

## What a block is checked against

A block is expected to be **accepted**, unless it carries an inline annotation
saying the spec expects it rejected — `// compile error: …`, `// error[E0200]:
…`, `// ERROR`, or the caret-underline convention:

```
let CONFIG: Config = load_config("app.toml");
//                   ^^^^^^^^^^^^^^^^^^^^^^^ error: effectful call at module scope
```

A block the spec says must be rejected and the compiler accepts is a divergence
exactly as much as the reverse. (Current weak spot: the suite checks *that* such
a block was rejected, not that it was rejected **for the annotated reason**.
Matching the message is the next increment.)

## The wrapping ladder

Most blocks are fragments — a signature, a trait, two statements. Each is tried
against a ladder of framings and passes if any is accepted:

| rung | framing |
|---|---|
| `items` | as written, module-level |
| `items+main` | as written, plus a synthesized empty `fn main` |
| `in-main` | wrapped in `fn main() { … }` |

The block's own shape picks the order: an item keyword or `pub` in column 0
means declarations, anything else means statements. **Order matters for more
than speed** — the first rung is the one whose diagnostics get reported when
every rung fails, so it has to be the framing the block was written for. An
earlier version sorted by "fewest errors" instead and produced 88 blocks blaming
`'fn' is a reserved keyword`, which was the noise of declarations stuffed inside
a `fn main` that was never the right frame.

## Outcomes

| outcome | meaning |
|---|---|
| `conforms` | accepted, as the spec implies |
| `confirmed-rejection` | rejected, as the spec's annotation says it should be |
| `elided` | contains `...` — prose with holes, not a program |
| `unresolved` | fails only with `undefined name/type` — names declared in another block |
| `REJECTED` | rejected, and the spec did not say it would be |
| `MISSING-REJECTION` | accepted, though the spec annotated it as an error |

`unresolved` is bucketed rather than discarded, and the undefined symbols are
tallied and printed, because *a spec referring to something the compiler has
never heard of* is exactly the shape of `B-2026-08-17-38` — `TreeMap`,
documented thirteen times, implemented zero.

## The baseline is the work queue

`docs/design-conformance-baseline.json` records every non-conforming block by
**content hash**, with a one-line `reason`. The gate fails on any block that is
non-conforming and *not* in the baseline — new prose that the compiler cannot
honour, and regressions in the compiler, both surface. Blocks that start
conforming are reported as `FIXED`; drop them from the baseline in the same
change.

Reasons fall into five kinds, and only two of them are defects:

- **SPEC GAP** — a real divergence. Has, or should have, a ledger row.
- **DOC BUG** — design.md contradicting `syntax.md` or itself.
- *signature catalogue* — a doc table of method shapes with no bodies. Not
  compilable by construction; the harness cannot frame it.
- *catalogue of alternative forms* — a block enumerating spellings that collide
  when compiled together (seven `import` forms all binding `Connection`).
- *deferred feature* — documented in `deferred.md` as post-v1.

`UNTRIAGED` means nobody has decided yet. **That count is the queue**, and the
point of the file is for it to shrink.

## Known limits

- Fragments are checked in isolation, so cross-block context (a `struct`
  declared two blocks earlier) reads as `unresolved`. Assembling context
  automatically was rejected: it would mask the `TreeMap` class of divergence,
  which is the one worth catching.
- Deferred-vs-divergent is a judgement call the harness cannot make; it lives in
  the baseline's `reason` field, written by hand.
- Acceptance is not semantics. A block that compiles to the wrong thing passes
  here — that is what the kata corpus and the differential harnesses are for.
