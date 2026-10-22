# mini_ahocorasick

A port of [ahocorapy](https://pypi.org/project/ahocorapy/) 1.8.0 (MIT; Abusix,
Inc. and Frederik Petersen), a pure-Python **Aho-Corasick** keyword tree. The
Aho-Corasick automaton finds every occurrence of any of a set of keywords in a
text in a single linear pass — the algorithm behind keyword filtering, log and
intrusion-detection scanning, virus signature matching and search indexing.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of keyword trees, each built then
queried, in order:

- `tree <0|1>` — start a new tree (`0` case-sensitive, `1` case-insensitive);
- `kw <word>` — add a keyword to the current tree;
- `all <text>` — finalize (on the first query) and `search_all`; print every
  match;
- `one <text>` — `search_one`; print the first match, or `None`.

`tree` and `kw` print nothing. A match renders as `keyword@startindex`; `all`
joins its matches with spaces (a blank line when there are none), `one` prints
one match or `None`. The trees include the classic `he`/`she`/`his`/`hers`
automaton, prefix-overlapping log levels (`warn`/`warning`, `fail`/`failure`),
suffix-overlapping needles (`issi`/`ssi`/`mississippi`), a case-insensitive
language scanner, and repeated-symbol texts.

`oracle.py` replays every job against the vendored `ahocorapy` library and
writes `expected.out`; `mini_ahocorasick.kara` reproduces it byte-for-byte on
every backend.

## How the port works

`mini_ahocorasick.kara` follows `ahocorapy/keywordtree.py` exactly:

- **The trie (`add`).** Each keyword extends a trie whose nodes are *states*.
  The whole automaton is held in flat `i64` arrays indexed by state id; a
  state's transitions live in three parallel arrays `(from, symbol, to)` scanned
  linearly.
- **Failure links and the goto function (`finalize` / `search_lss`).** Each
  state's *longest strict suffix* link is computed by walking its parent's
  suffix chain, and the suffix's transitions are copied in so that `transitions`
  becomes a complete goto function (ahocorapy's DFA shortcut). The library
  drives this with a stack; the port uses plain increasing-id order, which is
  equivalent because a child's state id always exceeds its parent's (so a
  parent's link is ready before its child's is computed) — this was verified
  against the library over twelve thousand random keyword sets.
- **Precomputed outputs.** For each state, the keywords to report are
  precomputed by walking the suffix chain, in *longest match first* order, and
  stored as a slice of a flat array. The search loop reads that slice directly
  instead of walking the chain per character.
- **The search loop (`search_all` / `search_one`).** The generic DFA walk: for
  each character, follow the transition (falling back through the root), and
  emit the current state's outputs with start index `idx + 1 - len(keyword)`.
  `search_one` stops at the first match. (The library's regex skip-pattern
  optimization is omitted; it only skips work, so the matches and their order
  are identical.)

The port's output was checked against the library over the job list on every
backend, and separately (during development) over many thousands of random
keyword sets and texts, case-sensitive and case-insensitive.

## Scope

- **ASCII keywords and texts.** ahocorapy works over any sequence of hashable
  symbols and full Unicode; this port uses ASCII bytes, where a byte is a
  character and the byte offset is the character offset the library reports.
  Case-insensitive mode lowercases ASCII letters.
- **Search only.** The port covers `add` / `finalize` / `search_all` /
  `search_one`. The pickling (`__getstate__`/`__setstate__`) and the visualizer
  are out of scope. Only the three vendored lines of the library the port
  mirrors (`keywordtree.py`, `among`-free) are exercised by the oracle.

The vendored library imports only the standard library and is pure Python; the
search path the port covers is integer and byte arithmetic.

## Running

```bash
karac run mini_ahocorasick.kara               # interpret
karac build --mir mini_ahocorasick.kara       # native binary
python3 -s -P -B mk.py                         # regenerate data/jobs.tsv
python3 -I oracle.py                            # regenerate expected.out
python3 -s -P -B gen_kara.py                    # regenerate the embedded jobs
```
