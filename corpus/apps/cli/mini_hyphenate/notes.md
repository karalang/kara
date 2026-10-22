# mini_hyphenate

A port of [Hyphenate](https://pypi.org/project/hyphenate/) 1.1.0 (public domain;
Ned Batchelder, from Frank Liang's algorithm), which **hyphenates English
words** — it returns the points at which a word may be broken across a line
(`com-put-er`). This is the logic behind TeX/LaTeX line breaking and the
hyphenation used by typesetting and text-layout tools.

## What it does

`mk.py` writes `data/jobs.tsv`: one word per line. The job hyphenates the word
and prints the pieces joined by `-`.

```
computer      -> com-put-er
hyphenation   -> hy-phen-ation
beautiful     -> beau-ti-ful
cat           -> cat            # words of 4 letters or fewer are never split
associate     -> as-so-ciate    # a built-in exception
present       -> present        # an exception that is deliberately not split
```

`oracle.py` replays every word through the vendored `hyphenate` library and
writes `expected.out`; `mini_hyphenate.kara` reproduces it byte-for-byte on
every backend.

## How the port works

Liang's algorithm is a pattern trie plus an integer point-array scan, which
ports directly:

- **Patterns.** The ~4900 Knuth/Liang TeX patterns (each like `.ach4` or
  `a1bc3d4`) carry small odd/even numbers between letters. They are parsed into
  a **flat 27-symbol trie** — symbol `.` = 0 and `a`..`z` = 1..26, with each
  node's 27 child slots held in one `Vec[i64]` (the same flat-trie shape as
  mini_ahocorasick and mini_pygtrie). A node that ends a pattern stores that
  pattern's point list.
- **Scanning.** For a word longer than four letters, the work string
  `.word.` is lowercased and **every suffix** is walked through the trie; each
  matched pattern raises the points between letters to the max seen. The points
  just inside each end are forced to zero (no break in the first two or last two
  letters). An **odd** point marks a legal break.
- **Exceptions.** A short list (`as-so-ciate`, `ta-ble`, …) carries a hand-set
  point array used instead of the computed one; entries with no dash
  (`present`, `project`) are the library's way of suppressing hyphenation.

No floats and no bitwise operators are involved — the whole algorithm is integer
points and trie indexing, so the backends agree by construction. The pattern and
exception data is the library's own, embedded verbatim and parsed into the trie
at run time (the constructor stays linear in the data size).

## Scope

- **ASCII English.** The patterns only match `a`..`z`; as in the library, any
  other character never matches a pattern (it ends the walk), so a word with an
  apostrophe or hyphen is split only at the letter runs around it, exactly as
  `hyphenate` does.

The port was checked against the library over the job list on every backend, and
separately (during development) over 100,000 random letter strings plus a
curated word list and all 14 exceptions.

The vendored library is pure Python and imports only the standard library.

## Running

```bash
karac run mini_hyphenate.kara             # interpret
karac build --mir mini_hyphenate.kara     # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
