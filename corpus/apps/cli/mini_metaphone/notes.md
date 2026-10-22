# mini_metaphone

A port of [Metaphone](https://github.com/oubiwann/metaphone) 0.6 (BSD), the
**Double Metaphone** phonetic encoding algorithm (Lawrence Philips, 2000). Double
Metaphone maps a word to one or two short consonant codes that approximate how
it *sounds*, so that differently-spelled homophones collide: `Smith` → `SM0`,
`Schmidt` → `XMT`, both sharing `XMT`. It is the engine behind fuzzy name
matching, spell-check "did you mean" suggestions, and de-duplicating records
that differ only in spelling — a direct successor to Soundex, but far more
accurate, with ~100 letter-context rules accounting for Slavic, Germanic,
Celtic, Greek, French, Italian, Spanish and other influences on English.

## What it does

`mk.py` writes `data/jobs.tsv`: one `dm <word>` job per line (tab-separated).
Each asks for the Double Metaphone codes of an ASCII word. `oracle.py` replays
the list against the vendored Metaphone library and writes `expected.out`, one
`primary|secondary` line per job (the secondary half is empty when it equals the
primary); `mini_metaphone.kara` reproduces it byte-for-byte on every backend.

The word list is drawn from the library's own test suite plus a spread chosen to
reach the many branches the algorithm takes — the `C`/`CH`/`CC`/`CZ`/`CIA`/`SCH`
contexts, `GH`/`GN`, `TH`/`TION`, the silent `GN`/`KN`/`PN`/`WR`/`PS`
word-starts, the initial-`X` and `SUGAR`/`ISLAND` special cases, Slavo-Germanic
detection, and the Spanish `SAN `/`JOSE` space handling. Every output is fully
determined by the input word, so it is a clean A/B surface.

## How the port works

`double_metaphone` follows `metaphone.py` rule for rule. The word is
ASCII-uppercased and bracketed into a buffer `"--" + WORD + "------"` so the
rules can index a few characters before and after the current letter without
bounds checks; a `position` cursor walks it. Each letter computes a `next`
action — `Nxt { np, ns, adv, arity }` — where arity 2 adds one code to *both* the
primary and secondary codes and arity 3 adds a different code to each, and `adv`
is how far to advance. Buffer reads go through `sl`/`ch` helpers that reproduce
Python's slice semantics exactly (negative indices wrap, out-of-range slices
clamp to empty), which is what lets the rules read past the ends into the pad.

One faithfully-reproduced quirk: two branches of the `G` rule leave `next`
*unchanged* from the previous letter rather than assigning it, so the port
threads the previous `Nxt` into `proc_g` and returns it there — matching the
library (and its reference C++ implementation) byte-for-byte.

## Scope

Inputs are ASCII. The library additionally NFD-normalises accented letters
(stripping combining marks, e.g. `Bartoš` → `BARTOS`) and maps `Ç`/`ç` to `s`
before encoding; that Unicode normalisation is the documented boundary here, as
elsewhere in this corpus. The vendored `metaphone/` package is pure Python
importing only the standard library, so `oracle.py` runs exactly the code the
port mirrors.

## Running

```bash
karac run mini_metaphone.kara                  # interpret
karac build --mir mini_metaphone.kara          # native binary
python3 -s -P -B mk.py                              # regenerate data/jobs.tsv
python3 -I oracle.py                                 # regenerate expected.out
python3 -s -P -B gen_kara.py                         # regenerate the embedded jobs
```
