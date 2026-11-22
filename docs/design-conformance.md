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
exactly as much as the reverse.

The annotation must be a **trailing** comment on a line that also has code, or a
caret-underline line. A standalone prose comment mentioning "compile error" does
not count: an earlier version accepted those and mis-flagged the `#[no_effect]`
block, whose header comment reads *"Heap use anywhere below this boundary is a
compile error"* — a description of what the attribute does, in a block that
contains no heap use and compiles correctly.

A rejection is confirmed only when it is **where the annotation says**: an error
reported on a marked line (the caret's code line; for an annotation on an
attribute, the item under it), with the code the annotation names when it names
one (`// error[E0200]`). An undefined name or a parse failure on that line is the
framing failing, not the rule, so it does not count. Every rung is tried, and
the best answer stands. A block rejected only for names declared in other blocks
is `unresolved-rejection`: it proves nothing either way, and is counted apart.
Before B-2026-11-22-20 any rejection confirmed the block, so a block failing for
an unrelated reason read as the spec upheld.

## Fence words

The info string after ```` ```kara ```` says what a block is, in the same words
`tests/book_snippets.rs` reads for the book (comma-separated):

| fence | meaning |
|---|---|
| ```` ```kara ```` | must check; a whole program (`fn main` at column 0) is also run, and must exit 0 and print the ```` ```text ```` block after it when there is one |
| ```` ```kara,fragment ```` | not a program on its own: a catalogue of alternative forms, a statement lifted from a function, a library item shown as an exhibit. Counted, never run |
| ```` ```kara,error=E0205 ```` | must be rejected with that code (`error=A\|B` for either), wherever it is reported |
| ```` ```kara,exit=101 ```` | a program that ends with that exit code on purpose (a runbook's panic); the text block after it is its **stderr**, every `<dir>/<name>.kara` read as `main.kara` |
| ```` ```kara,ignore ```` | skipped (the book's word; `library-conformance.py` still reads a book block marked only `ignore`) |
| ```` ```kara,norun ```` | a whole program checked and never run (the book's word): a server that binds a port and accepts forever |
| ```` ```kara,name=point ```` | other blocks of the same document may build on this one by that name |
| ```` ```kara,uses=point\|log ```` | judged in full after the blocks (or `docs/doc-context.kara` sections) it names, dependencies first: checked, run if it is a whole program, and held to the marked-line rule. A missing name is `CONTEXT-MISSING`, never `unresolved` |

`tests/docs_conformance.rs` runs both sweeps with `--check` in the default
`cargo test --all` leg (about two minutes on a debug build, the two in parallel), and both baselines
are empty but for one entry with a ledger row, so a new finding fails the gate
rather than joining a list.

## Context: `name=`, `uses=` and `docs/doc-context.kara`

A doc reads in order and the sweep compiles one block at a time, so an example
that calls a `fetch_orders` the prose takes as given, or a `Point` declared
two blocks up, used to stop at `unresolved`: checked for names only. Since
B-2026-11-22-21 such a block says what it needs. `uses=point` puts the block
fenced `name=point` (same document) in front of it; failing that, the section
`// context: point` of [`doc-context.kara`](doc-context.kara), which holds the
declarations the docs rely on and do not show. A section may itself start
`// context: NAME uses=other`. The resolved context is split at column 0:
declarations go above the block, `let` statements open the `fn main` the block
is wrapped in (or, for a script, lead it), so `uses=` serves an expression
block as well as an item block. A diagnostic is still mapped to the line of the
block it is on.

A section is the smallest honest stand-in: it declares what the example takes
as given and never what the example is showing. Of the 163 blocks that were
`unresolved` (92 in design.md, 71 elsewhere), 140 are now checked in full and 5
signature tables resolve every name (their `impl[T]` header's generics now
bind the signatures under it). Reading them closely found a dozen doc bugs (a
`Result[T, Error]` with the trait as a type, `host.close`, a book chapter
teaching the call-site `mut` rule backwards, a README excerpt that no longer
matched its program) and three compiler bugs (B-2026-11-22-22, -23, -24).

The other 18 stay unverifiable, and say why rather than sit in a bucket:
`fragment` for an excerpt of a program checked elsewhere (an example's
`src/main.kara`) or one for a target 0.8 does not build (`wasm_browser`), for a
signature table with comment bodies, and for a grammar template
(`fn name[T: Bound](param: Type) -> Return`). `docs/library/data.md`'s three
`unresolved` blocks are the `kara-data` package, which is not implemented.

## The wrapping ladder

Most blocks are fragments — a signature, a trait, two statements. Each is tried
against a ladder of framings and passes if any is accepted:

| rung | framing |
|---|---|
| `items` | as written, module-level |
| `items+main` | as written, plus a synthesized empty `fn main` |
| `in-main` | wrapped in `fn main() { … }` |
| `type-aliases` | each line bound as `type _ProbeN = …;` |
| `signatures` | last resort — see below |

The block's own shape picks the order: an item keyword or `pub` in column 0
means declarations, anything else means statements. **Order matters for more
than speed** — the first rung is the one whose diagnostics get reported when
every rung fails, so it has to be the framing the block was written for. An
earlier version sorted by "fewest errors" instead and produced 88 blocks blaming
`'fn' is a reserved keyword`, which was the noise of declarations stuffed inside
a `fn main` that was never the right frame.

### The signature rung

A fifth of the baseline used to be a shape no framing could reach: a table of
method *shapes* with no bodies, which is not a program in any wrapping.

```
Vec.filled(n: i64, val: T) -> Vec[T] where T: Clone
fn or_insert(self, default: V) -> mut ref V
Channel.new[T]() -> (Sender[T], Receiver[T])
fn io.read_line() -> Result[String, IoError] with reads(Stdin)
```

A signature is not a program, but it **is** a declaration, and declarations are
checkable. When every rung above fails, the block's bodiless declarations are
pulled out and probed in two shapes:

| shape | probe | what it proves |
|---|---|---|
| `fn name(args) -> R [with E]` | `trait _Probe { <line>; }` | the parameter and return types resolve, and the effect clause parses. Says nothing about whether the method is *implemented* — a trait can declare anything. |
| `Type.method(args) -> R` | `let _ = Type.method(0, 0);` | the name exists. This is the one that finds the `Channel.bounded` / `io.read_line` class. |

**Why filler arguments are enough.** The second probe needs argument values and
a signature gives types. A per-type value table was the obvious answer and turns
out to be unnecessary: `karac` reports a wrong argument *type* as `expected
'i64', found 'String'` — a message that presupposes the function was found —
while a name that does not exist is `no associated function 'filled' on type
'Vec'`. So the probe passes `0` for everything and classifies on the diagnostic
rather than on success. **Arity does matter** — `Vec.filled()` with no arguments
also reports `no associated function`, because resolution is arity-aware — and
arity is the one thing a signature always states.

That makes this rung a different question from the others: *does this name
resolve*, not *does this call typecheck*. Its non-resolution diagnostics are
dropped rather than counted, and it gets its own outcomes.

The walk that extracts declarations flattens rather than parses, which is what
lets it reach inside an `impl … { }` or `extern "C" { }` wrapper without
modelling either. A declaration whose next non-blank line is `{` has a **body**
— real code the ladder already owns — so it and its body are skipped.

## Outcomes

| outcome | meaning |
|---|---|
| `conforms` | accepted, as the spec implies (and, for a whole program, ran as shown) |
| `confirmed-rejection` | rejected at the marked line, or with the fence's code |
| `unresolved-rejection` | rejected only for names other blocks declare: unverified |
| `REJECTED-ELSEWHERE` | rejected, but not where or how the annotation says |
| `fragment` | marked ```` ```kara,fragment ```` |
| `OUTPUT-DIFFERS` / `RUN-FAILED` / `ran-timeout` | a whole program that checks but prints other than the doc shows, exits other than it should, or did not finish in 30 s |
| `elided` | contains `...` — prose with holes, not a program |
| `deferred` | under design.md's own **Deferred Items** heading — future syntax, not a contract |
| `unresolved` | fails only with `undefined name/type` — names declared in another block |
| `signatures-ok` | a signature catalogue whose names all resolve |
| `REJECTED` | rejected, and the spec did not say it would be |
| `MISSING-REJECTION` | accepted, though the spec annotated it as an error |
| `SIGNATURE-MISSING` | a catalogue naming something that does not resolve, or written in syntax that does not parse |
| `UNDECLARED-NAME` | a catalogue referring to a name **nothing** defines — not design.md, not the compiler |

`unresolved` is bucketed rather than discarded, and the undefined symbols are
tallied and printed, because *a spec referring to something the compiler has
never heard of* is exactly the shape of `B-2026-08-17-38` — `TreeMap`,
documented thirteen times, implemented zero.

`UNDECLARED-NAME` is how that shape is separated from ordinary prose ordering.
`undefined name 'X'` means two different things:

- **`Config`** — design.md declares `struct Config` three blocks up. The doc is
  read in order; the harness compiles each block alone. Not a defect.
- **`io`** — nothing declares it, here or in the compiler. The spec promises a
  surface the implementation never grew.

The message shape is identical, so a **declaration index** over every name
design.md itself declares is what tells them apart. It is applied *only* to
signature catalogues: a catalogue is a claim about an API surface, whereas a
worked example is illustration, and scoring an example's undefined
`load_config` / `pool` / `normalize` the same way turned 28 out-of-block blocks
into 130 findings, none of them about the compiler.

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
- *signature catalogue* — a doc table of method shapes with no bodies. These
  are now **checked** by the signature rung; an entry that stays in the baseline
  says which name or which syntax failed.
- *catalogue of alternative forms* — a block enumerating spellings that collide
  when compiled together (seven `import` forms all binding `Connection`).
- *deferred feature* — documented in `deferred.md` as post-v1.

`UNTRIAGED` means nobody has decided yet. **That count is the queue**, and the
point of the file is for it to shrink. B-2026-11-22-20 emptied it: its last
nine entries were fragments, now marked as such, or doc bugs, now fixed. It
holds one entry since B-2026-11-22-21: § `par` blocks' aliasing example, which
the compiler accepts while nothing calls it (B-2026-11-22-24).

## Known limits

- A block without `uses=` is checked in isolation, so cross-block context (a
  `struct` declared two blocks earlier) reads as `unresolved`. Assembling
  context automatically was rejected, and tried again in B-2026-11-22-21 with
  the same result: matching names across a document paired unrelated `Point`s
  and pulled in elided definers, and it would mask the `TreeMap` class of
  divergence, which is the one worth catching. `uses=` is explicit for that
  reason.
- **A signature rung proves a name resolves, not that it matches.**
  `Vec.filled(0, 0)` passing proves `filled` exists and takes two arguments; it
  does not prove the second is a `T` or that the return is `Vec[T]`. Checking
  that needs a per-type probe-value table, which is the natural next increment.
- **The declaration index cannot tell a promise from a gesture.** design.md's
  `trait Reader { fn access(ref self, key: Key) -> Value; }` names `Key` and
  `Wire`, which nothing declares — flagged `UNDECLARED-NAME` alongside `io`, and
  triaged in the baseline as the false positive it is. There is no mechanical
  difference between a stdlib name the spec promises and an illustrative user
  type; the baseline is where a person decides.
- Deferred-vs-divergent is a judgement call the harness cannot make; it lives in
  the baseline's `reason` field, written by hand.
- Acceptance is not semantics. A block that compiles to the wrong thing passes
  here — that is what the kata corpus and the differential harnesses are for.
