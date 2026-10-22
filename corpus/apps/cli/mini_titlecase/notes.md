# mini_titlecase

A port of [titlecase](https://github.com/ppannuto/python-titlecase) 2.4.1
(MIT, Stuart Colville & Patrick Pannuto, after John Gruber's Perl original) —
the English title-caser CMS and publishing pipelines use to render headlines
in New York Times style.

## What it does

`mk.py` writes `data/jobs.tsv`, one phrase per line. `oracle.py` runs each
through `vendor/titlecase_lib.py` and prints `expected.out`. `mini_titlecase.kara`
reimplements the algorithm and must match byte-for-byte on every backend.

The library drives everything with a dozen regexes; the port reimplements each
as an explicit byte scan and follows the same per-word decision order:

1. In an all-caps line, dotted initials (`U.N.C.L.E.`) are kept as-is.
2. The `O'`/`d'`/`l'` apostrophe form capitalizes the letter after the
   apostrophe (`O'Brien`).
3. An `Mc`/`MC` prefix keeps the prefix and recursively title-cases the rest
   (`mcdonald's` → `McDonald's`).
4. `Mr`/`Mrs`/`Ms`/`Dr` get a capital first letter.
5. A word with an inline period (`www.example.com`) or mixed-case elsewhere
   (`iPhone`, `AT&T`) is left untouched.
6. A small word (the NYT list: a, an, and, as, at, but, by, en, for, if, in,
   of, on, or, the, to, v, via, vs) is lowercased mid-title.
7. `/`- and `-`-separated words are split and each part recursed into
   (`well-known` → `Well-Known`); a `//` disables the slash split.
8. An all-caps word folds down; an all-consonant token longer than two chars
   becomes an acronym; otherwise the first letter is capitalized.

Afterwards the first and last words have their small words capitalized
**case-insensitively** — which is why `AT&T`, whose leading `AT` reads as the
small word `at`, becomes `At&T` — and a small word opening a subphrase after
`: . ; ? ! -` is capitalized (`...Rings: the...` → `...Rings: The...`).

## Scope

Inputs are ASCII, which is where the library's `re`-fallback and its
`regex`-module (Unicode-category) branches agree. The Unicode punctuation and
curly-quote variants, and the callback/abbreviation-list hook, are the
boundaries left out.

## Running

```bash
karac run mini_titlecase.kara            # interpret
karac build --mir mini_titlecase.kara    # native binary
python3 -s -P -B mk.py                   # regenerate data/jobs.tsv
python3 -I oracle.py                     # regenerate expected.out
```
