# mini_snowball

A port of [snowballstemmer](https://pypi.org/project/snowballstemmer/) 3.1.1
(BSD-3-Clause; Dr Martin Porter and the Snowball project), the stemming library
used across search indexing and NLP to collapse inflected words to a common
root — so that *connection*, *connected* and *connecting* all index under
*connect*. This port is its English **Porter** stemmer, the algorithm Snowball
generates from `porter.sbl`.

## What it does

`mk.py` writes `data/jobs.tsv`: a list of `stem <word>` jobs. Each job runs the
Porter stemmer on one word and prints its stem. The word list is chosen to
exercise every Porter step — the Step 1a plural rules, the Step 1b past/gerund
rules with their `e`-insert and consonant-undouble sub-cases, the Step 1c
`y`→`i` rule, the Step 2/3/4 suffix derivations across the Among tables, the
Step 5 final-`e` and double-`l` rules, and the word-initial / post-vowel
`y`→`Y` consonant mapping.

`oracle.py` replays every job against the vendored `snowballstemmer` library and
writes `expected.out`; `mini_snowball.kara` reproduces it byte-for-byte on every
backend.

## How the port works

`mini_snowball.kara` follows `basestemmer.py` and `porter_stemmer.py` exactly:

- **The snowball string cursor.** A `St` struct holds the working byte buffer
  with the snowball registers — `cursor`, `limit`, `limit_backward`, `bra`,
  `ket` — and the splice operations (`replace_s`, `slice_from`, `slice_del`,
  `insert`) rebuild the buffer and shift those registers exactly as the runtime
  does. Backward processing runs the cursor from `limit` down to
  `limit_backward`.
- **Among-table suffix lookup.** `find_among_b` finds the longest table suffix
  ending at the cursor; because every one of the six Porter tables has
  `method=None`, that reduces to a plain longest-matching-suffix scan, which the
  port writes as a length-descending chain of `take_b` tests per table. (The
  ordering is load-bearing: `ization` must be tried before `ation`, `ement`
  before `ment` before `ent`.)
- **The eight step functions.** `r_Step_1a` … `r_Step_5b` are ported one-for-one,
  including the `R1`/`R2` region tests, the short-vowel check, and the
  `e`-insertion and consonant-undoubling of Step 1b. The snowball `lab` gotos —
  which encode "try this, else that" and "scan until" — are rewritten as plain
  flags and `if`/`else`, and the forward `y`→`Y` consonant mapping and its final
  `Y`→`y` restore are written as direct scans over the buffer.

The result is checked against the vendored library over the job list on every
backend, and separately (during development) against the library over more than
100,000 words.

## Scope

The port implements the **Porter** algorithm (`snowballstemmer.stemmer("porter")`).
Two boundaries are the app's own:

- **ASCII lowercase words.** The Porter algorithm, like the library, operates on
  lowercase ASCII letters; the port and oracle both feed lowercase ASCII, where
  the byte buffer and the library's character handling coincide.
- **One algorithm.** snowballstemmer ships 38 language stemmers; only the Porter
  path (three files: `among.py`, `basestemmer.py`, `porter_stemmer.py`) is
  vendored here, unmodified, under a minimal package `__init__` that exposes
  only `stemmer("porter")`. So `oracle.py` runs exactly the code the port
  mirrors, with no other-language machinery pulled in.

The vendored Porter path imports only the standard library and is pure Python
(no C extension), so the oracle runs the same visible algorithm the port
reproduces.

## Running

```bash
karac run mini_snowball.kara                 # interpret
karac build --mir mini_snowball.kara         # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
