# mini_rapidfuzz

A port of [rapidfuzz](https://github.com/rapidfuzz/RapidFuzz) 3.14.6 (MIT;
Max Bachmann, Adam Cohen), the fuzzy string matcher that deduplication
jobs, command-line "did you mean" prompts and product search use to tell
how alike two strings are. It has the `fuzz` ratios, ten edit distances,
the edit operations between two strings, and `process.extract` to find
the best matches for a query among many choices.

```
> !suggest data/commands.txt data/typos.txt
  comit: did you mean commit (1)?
  pul: did you mean pull (1), push (2)?
  statsu: did you mean stash (2), status (2)?
  xyzzy: unknown command
```

## What it does

`mk.py` writes 415 jobs and 3 programs to `data/jobs.jsonl`, one JSON
array a line, in 10 sections, and the programs' five input files to
`data/`. `oracle.py` runs each job against the vendored library and prints
every result, or `error <class>: <message>` when the library raises (17
times); `mini_rapidfuzz.kara` runs the same jobs.

- **The ten `fuzz` scorers** (`ratio`, `partial_ratio`, the four token
  ratios and their partial forms, `WRatio`, `QRatio`) run on 50 pairs in
  74 jobs:
  the documentation's examples, company names, addresses, accented
  names, repeated and reordered tokens, empty and blank strings, with
  `default_process` and with `score_cutoff` from 0 to 101.
- **`partial_ratio_alignment`** shows where the shorter string sits in
  the longer one for 19 pairs, and **`default_process`** cleans 17
  strings, including Greek, Cyrillic, Japanese and superscript digits.
- **The distance metrics** (Levenshtein, Indel, LCSseq, Hamming, Jaro,
  JaroWinkler, OSA, DamerauLevenshtein, Prefix, Postfix) each give
  `distance`, `similarity` and both normalized forms on 17 pairs, then
  with weights, cutoffs, `pad` and `prefix_weight`, including the
  invalid ones the library refuses.
- **Edit operations** for Levenshtein, Indel, LCSseq and Hamming: the
  `Editops`, the `Opcodes`, both `apply()`s, the matching blocks, the
  inverse and the round trip back to `Editops`.
- **`process.extract`, `extractOne` and `extract_iter`** run 43 jobs
  with 18 scorers, limits, cutoffs, `None` choices and dict choices.
- **`dedupe`** groups 30 vendor names from invoices into 17 vendors with
  `WRatio` at 88. **`suggest`** answers 15 mistyped `git` commands with
  the nearest ones by edit distance, falling back to Jaro-Winkler.
  **`reconcile`** matches 12 free-text order lines to a 12-item catalog,
  keyed by SKU, and shows the second-best match when it is close.

## How the port works

- **The pure-Python implementation is the reference.** The oracle sets
  `RAPIDFUZZ_IMPLEMENTATION=python` and checks that the scorers come
  from `fuzz_py` and `process_py`; the compiled extension is not
  vendored.
- **Its bit vectors become tables.** The Python code runs Hyyrö's
  bit-parallel algorithms on Python integers. The port fills the
  ordinary Levenshtein, LCS and OSA tables instead and reads the same
  answers from them: bit `col-1` of a row's `VP` is set exactly when
  the table grows by one going down that column, `VN` when it shrinks
  by one, and LCS's `S` when the subsequence does not grow. The
  `editops` walks then take the same path through the same cells. The two
  forms were checked against each other on 4000 random pairs before the
  port was written.
- **Scores keep their Python type.** The library returns ints on some
  paths (an edit count, 0 below a cutoff, 100 for an exact partial
  match or a shared token) and floats on others, and they print
  differently, so a score is an int or a float. `max()` keeps the first
  of equal values, which decides between `100` and `100.0`.
- **`process` follows the scorer's flags**: a distance's worst score is
  `2**63 - 1` and its best 0, so a lower score wins. `extractOne`
  raises its cutoff to each new best and stops at a perfect score, and
  `extract` sorts stably, so ties keep the order of the choices.

## Compiler gaps

None. `karac check` passed on the first try, and the interpreter matched
the oracle byte for byte on its first run, as did the LLVM, ASan and
native builds.

## Scope

- Strings only. The library also takes lists of hashables, which it
  turns into Python `hash()` values.
- `\w` is a letter, a digit or `_`, as Python's `re` and Rust's
  `char::is_alphanumeric` agree on everything the data holds. They
  differ on combining marks, so `mk.py` refuses data with any.
- `cdist` and `cpdist` need NumPy and are left out.

## Running

```bash
karac run mini_rapidfuzz.kara                     # interpret
karac build --mir mini_rapidfuzz.kara             # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
