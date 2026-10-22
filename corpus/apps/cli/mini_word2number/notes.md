# mini_word2number

A port of [word2number](https://github.com/akshaynagpal/w2n) 1.1 (MIT, Akshay
Nagpal), the library that turns an English number phrase — `"two million
twenty three thousand and forty nine"` — into a number. It is used in NLP,
voice-assistant and form-parsing pipelines.

## What it does

`mk.py` writes `data/jobs.tsv`, one phrase per line. `oracle.py` runs each
through the vendored library (`vendor/word2number/`) to produce
`expected.out`. `mini_word2number.kara` reimplements `word_to_num` and must
match that output byte-for-byte on every backend.

The port follows `w2n.word_to_num` branch-for-branch, **quirks included**: the
library double-counts the tail on phrases like `"three billion two hundred
million"` (its `hundreds` fall-through re-reads the slice after `billion`),
which the oracle confirms yields `3201000200`, not `3200000000`. The grammar
is reproduced rather than "corrected", so the port matches the library, not
arithmetic.

## Scope

- The integer feature: single words, the tens/hundreds forms with `and`/`&`
  dropped, thousands/millions/billions, the bare-digit passthrough
  (`"112"` → `112`), hyphen-to-space normalisation and lower-casing.
- The three `ValueError` paths with their exact texts: no valid number words,
  a redundant `thousand`/`million`/`billion`, and a malformed ordering
  (a smaller scale word before a larger one).

The `point` / decimal path is out of scope: its result is a Python `float`
and matching `float` repr byte-for-byte is a separate concern (the same
boundary `mini_humanfriendly`'s `round()` notes). The word `point` is not
recognised, so no job uses it.

## Running

```bash
karac run mini_word2number.kara          # interpret
karac build --mir mini_word2number.kara  # native binary
python3 -s -P -B mk.py                   # regenerate data/jobs.tsv
python3 -I oracle.py                      # regenerate expected.out
```
