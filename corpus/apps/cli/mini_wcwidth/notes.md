# mini_wcwidth

How many terminal cells a string takes, as wcwidth 0.2.13 works it out.
wcwidth is the library that `prompt_toolkit`, `tabulate`, `rich`-style
table printers and many CLIs use to line up text that mixes CJK, emoji,
combining accents and controls. The port covers:
- `wcwidth()`: one character's width for each of the 19 Unicode versions
  wcwidth has tables for. Zero-width marks count 0, wide East Asian and
  emoji characters count 2, and C0 and C1 controls count -1. An empty
  string counts as NUL, and a longer string raises `TypeError`.
- `wcswidth()`: a whole string, with the optional `n`. A zero width joiner
  hides the character after it, and variation selector 16 widens the
  character before it from Unicode 9 on. It returns -1 at the first
  control, and raises `IndexError` when `n` is past the end.
- `_wcmatch_version()`: how a requested version is matched to one of the
  19. That covers `auto` and the `UNICODE_VERSION` environment variable,
  `latest`, and versions given with fewer parts. A version between two
  known ones gives the lower one. A version that is too low or does not
  parse warns, with the exact warning text.

The run writes `data/` and then works through the 88 jobs in
`data/jobs.jsonl`:
- 35 `wcwidth` jobs:
  - 40 characters whose width changes between versions, under every
    version;
  - a fixed set of hard cases under every third version;
  - jobs that pick the version through the environment and through
    strings that do not match.
- 14 jobs that match 67 version strings.
- 15 `wcswidth` jobs over 55 strings, under five versions and with `n`.
- Four reports of the app's own:
  - file names laid out in columns, the way `ls -C` does it, at six
    terminal widths;
  - a box-drawn table of users whose cells are cut to fit with `…` and
    aligned left, right or centre;
  - chat messages wrapped to a width, with a word that is too long cut
    into pieces;
  - log lines with tabs expanded to the next multiple of 8 cells, controls
    shown as `^X` or `\xNN`, and each line cut to the terminal width.

`oracle.py` runs the same jobs through the `wcwidth/` package here, which
is the 0.2.13 wheel unchanged, and wrote `expected.out`. Because wcwidth
caches what it computes and warns only once per version string, the
oracle clears both caches before each job.

The quirks are kept. Some examples:
- The version parts go through `int()`. So `" 8.0"`, `"８.0.0"` and
  `"٨.٠"` all mean 8.0.0, `"1_0.0.0"` means 10.0.0, and `"8_0.0"` is
  version 80, which gives the latest.
- `"4.1"` is lower than `(4, 1, 0)` as a tuple, so it warns that it is
  lower than any version, and then gives 4.1.0 anyway.
- `"8.-1"` gives 7.0.0, and `"-1"` gives 4.1.0.
- `UNICODE_VERSION=auto` is not special the second time round: it is read
  as a version string, does not parse, and warns.
- `"LATEST"` is not `"latest"`, so it also warns.
- VS16 widens only the last character that took a cell, and only once. So
  `"❤️️"` is 2 wide, and VS16 after a zero-width character does nothing.
- After a zero width joiner the next character is skipped whatever it is,
  so `"a‍‍b"` is 2 wide.

Python's `repr` is exact. The code points that `str.isprintable()` rejects
come from Python 3.13's `unicodedata`, which `gen_tables.py` turns into a
table along with wcwidth's own tables.

## License

wcwidth is under the MIT license in `LICENSE-wcwidth`:

    Copyright (c) 2014 Jeff Quast <contact@jeffquast.com>

with Markus Kuhn's permission notice for the original `wcwidth.c`. That
covers the package in `wcwidth/`.
