# mini_sqids

A port of [sqids](https://pypi.org/project/sqids/) 0.5.2 (MIT, the Sqids
maintainers), the successor to Hashids. Sqids turns a list of non-negative
integers into a short, URL-safe id that decodes back to the same list:
`[1, 2, 3]` becomes `86Rf07`. Services use it to show database keys in URLs
without exposing sequential row numbers, and its built-in blocklist keeps an id
from ever spelling a rude word.

## What it does

`mk.py` writes `data/jobs.tsv`, a script of constructor calls and the encodes
and decodes made with each one, and `data/blocklist.txt`, the library's default
blocklist read as a literal out of `vendor/sqids/constants.py`. `oracle.py`
replays the script through the vendored library and `mini_sqids.kara` through
the port. Both print one line per job:

```
new(-, 10, -) => ok
encode(1, 2, 3) => [86Rf07xd4z]
decode(86Rf07xd4z) => [1, 2, 3]
new(ab, 0, -) => ValueError: Alphabet length must be at least 3
```

`-` stands for the library default (the 62-character alphabet, or the default
blocklist). A custom blocklist is comma-separated; an empty field is an empty
blocklist.

The 77 jobs cover the default Sqids (single numbers, lists, zeros, the largest
supported number `sys.maxsize`, empty input, ids with characters outside the
alphabet, negative numbers), `min_length` padding up to the 255 maximum,
custom alphabets (hex, three letters, upper-case only) with the default
blocklist filtered against them, custom and empty blocklists including every
case in the library's blocklist tests, the re-generation limit, and each of the
constructor's errors.

## How the port works

- **Constructor.** The alphabet checks run in the library's order (multibyte,
  length, uniqueness, then `min_length`), with the same messages. The blocklist
  is filtered the library's way: words shorter than three characters, or with a
  letter the lower-cased alphabet lacks, are dropped; three-letter words must
  match the whole id, words containing a digit must match at either end, and
  the rest may match anywhere.
- **Encode.** The offset is the library's sum over the numbers, the alphabet is
  rotated by it and reversed, each number is written in the alphabet minus its
  first character, a separator and a reshuffle come between numbers, and short
  ids are padded from successive shuffles. A blocked id is regenerated with the
  next increment until the library's limit, after which it raises
  `Reached max attempts to re-generate the ID`.
- **Decode** splits on each step's separator and reads every chunk back as a
  number, stopping at an empty chunk exactly as the library does.

The alphabet is a byte vector, since the library only accepts ASCII. The
library stores the filtered blocklist in sets; the port uses vectors. The only
question asked of them is "does any word match", whose answer does not depend
on order, so the output is deterministic and `run == build` holds.

## Scope

- The library's default constructor uses copies of the three blocklist sets
  that `assets/filter_blocklist.py` filtered ahead of time. The port filters
  `DEFAULT_BLOCKLIST` when it is constructed instead. Both give the same three
  sets, which was checked against `sqids.constants` during development.
- Decoded numbers fit in `i64` for every id in the job set. The library returns
  arbitrarily large Python integers for a long hand-made id, which the port
  does not try to reproduce.
- The library's `TypeError` for a non-integer `min_length` cannot happen in a
  typed port.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_sqids.kara                # interpret
karac build --mir mini_sqids.kara        # native binary
python3 -s -P -B mk.py                   # regenerate data/
python3 -I -B oracle.py > expected.out   # regenerate expected.out
python3 -s -P -B gen_kara.py             # regenerate the embedded jobs and blocklist
```
