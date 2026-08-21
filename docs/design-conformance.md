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

Current weak spot: the suite checks *that* such a block was rejected, not that it
was rejected **for the annotated reason**. Matching the message is the next
increment.

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
| `conforms` | accepted, as the spec implies |
| `confirmed-rejection` | rejected, as the spec's annotation says it should be |
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
point of the file is for it to shrink. It is currently **zero**: all 53 entries
carry a reason, filed across `B-2026-08-21-9` through `-12` and `-29` through
`-32`.

## Known limits

- Fragments are checked in isolation, so cross-block context (a `struct`
  declared two blocks earlier) reads as `unresolved`. Assembling context
  automatically was rejected: it would mask the `TreeMap` class of divergence,
  which is the one worth catching.
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
