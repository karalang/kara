# mini_natsort

Natural sorting as natsort 8.4.0 does it. Python programs use natsort
to put file names, versions and labels in the order a person expects,
so that `file2` comes before `file10`. The port covers:
- `natsorted`, `realsorted`, `index_natsorted`, `index_realsorted` and
  `order_by_index`, with `reverse`, a key function and `PRESORT`.
- The key natsort builds for a string:
  - the string normalized to NFD, or to NFKD with
    `COMPATIBILITYNORMALIZE`;
  - split around its numbers by natsort's regular expressions;
  - the numbers converted as fastnumbers would convert them (natsort's
    pure-Python stand-in for it, which is what runs without
    fastnumbers);
  - separators put in so that numbers and text never meet at the same
    position.
- The number forms: int, float, signed and without exponents, with
  Unicode digits, superscripts, fractions and Roman numerals, and `inf`
  and `nan`.
- `IGNORECASE`, `LOWERCASEFIRST`, `GROUPLETTERS`, `NANLAST` and
  `NUMAFTER`.
- `PATH`, which splits a path into its parts and the file name into its
  base and up to two suffixes.
- Keys for numbers, `None` and nested lists. Ints and floats compare
  exactly, as Python compares them, however long the int is.
- The `natsort` command line: `-p`, `-f`, `-F`, `-e`, `-r`, `-t`, `-s`,
  `--nosign` and `--noexp`.

Python's `float()` is ported as well, since natsort calls it on every
number it finds. That means Unicode digits and spaces, underscores, and
correct rounding however many digits there are, down to the smallest
subnormal.

The character data is in `data/unicode.txt`, which `mk.py` takes from
Python's `unicodedata` (Unicode 15.1 in Python 3.13): decompositions,
casefold and swapcase, combining classes, whether a character is
printable or white space, and its numeric values. It covers Latin,
Greek, Cyrillic, punctuation, every character in natsort's own table of
numbers, and everything those map to.

The run writes `data/` and then works through the 34 jobs in
`data/jobs.jsonl`, 278 operations in all:
- photo names, versions, rotated logs, chapters, addresses and sizes,
  with each option that changes how they sort;
- signs and exponents, including `inf`, `nan`, overflow, underflow,
  subnormals and numbers longer than a float holds;
- accented, cased, Greek and Cyrillic words, and `ß`, `ﬁ` and wide
  spaces;
- numbers in other scripts and the numeric symbols;
- paths, among them `Folder` before `Folder (1)`, `.tar.gz` suffixes,
  dot files, roots and empty paths;
- lists of numbers and `None`, dicts sorted by a field, nested lists,
  `PRESORT` and the index functions;
- 20 command lines, including filters, exclusions and their errors;
- 26 jobs drawn at random from a fixed seed: file names built from
  parts, long decimals for `float()`, and big ints mixed with floats.

`oracle.py` runs the same jobs through natsort itself and wrote
`expected.out`. It uses the Python files of the 8.4.0 wheel in
`natsort/`, unchanged. fastnumbers is not installed, so natsort uses its
stand-in, as the port does.

The quirks are kept. Some examples:
- Without `PATH`, `Folder (1)/a` sorts before `Folder/a`, because ` ` is
  less than `/`.
- With `PATH`, every copy of the suffixes is cut out of the base, so
  `b.tar.gz.tar.gz` sorts as `b` with `.tar` and `.gz`.
- `IGNORECASE` folds case but keeps accents, so `à la carte` still sorts
  after `apple`.
- `nan` as text becomes negative infinity, so it ties with `-inf` and
  keeps its place beside it. `None` sorts just after both.
- In the command line, a filter over a superscript stops with
  `could not convert string to float: '²'`, since natsort finds the
  number but `float()` will not read it.

Not ported:
- `LOCALE`, `humansorted` and `os_sorted`, which depend on the system
  locale or on PyICU;
- bytes;
- `alg` values that are not flags, beyond the error natsort raises for
  a string.

## License

natsort is under the MIT license in `LICENSE-natsort`:

    Copyright (c) 2012-2023 Seth M. Morton

That covers the files in `natsort/`.
