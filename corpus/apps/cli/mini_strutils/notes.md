# mini_strutils

A port of a subset of [boltons](https://github.com/mahmoud/boltons)'
`strutils` 26.2.0 (BSD, Mahmoud Hashemi). boltons is a widely-used pure-Python
"batteries" collection; `strutils` is its string toolkit. This ports the
deterministic, ASCII string transforms and the integer-range-string family that
printer dialogs and page selectors use (`"1,3,5-8,10-11,15"`).

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line (tab-separated):

    c2u  <s>                 camel2under:  BasicParseTest -> basic_parse_test
    u2c  <s>                 under2camel:  complex_tokenizer -> ComplexTokenizer
    ord  <numstr>            ordinalize:   1 -> 1st, 13 -> 13th, hi -> hi
    a10  <s>                 a10n:         internationalization -> i18n
    slug <s>                 slugify:      "First post! Hi" -> first_post_hi
    pil  <range_string>      parse_int_list -> the sorted list, as [1, 3, 5]
    fil  <space-sep ints>    format_int_list -> the collapsed range string
    cil  <rs> <start> <end>  complement_int_list ( end = * means None )
    irl  <range_string>      int_ranges_from_int_list -> ((1, 1), (5, 8))
    hrl  <item>|<item>|...   human_readable_list -> "a, b, and c"

`oracle.py` runs the jobs through the vendored `boltons.strutils`;
`mini_strutils.kara` reproduces `expected.out` byte-for-byte on every backend.
The cases walk the boundaries each function branches on: camel/underscore
transitions and acronym runs (`HTTPResponse`, `getHTTPResponseCode`), the
ordinal teens (11th/12th/13th vs 21st/22nd/23rd), range compaction with its
de-dup / repeated / reversed-bound paths, the complement with and without an
explicit end, and the 0/1/2/3+-item human-readable list.

## How the port works

Each transform follows the library statement for statement. Two of them lean on
a regex in the original, reimplemented here as an explicit byte scan over the
same character class (exact for ASCII input — the same approach mini_pyhumps and
mini_titlecase take):

- **camel2under** inserts `_` before an uppercase letter that is either preceded
  by a lowercase/digit or (not at the start and) followed by a lowercase, then
  lowercases the result — the library's `((?<=[a-z0-9])[A-Z]|(?!^)[A-Z](?=[a-z]))`
  boundary.
- **slugify** splits on runs of `string.punctuation + string.whitespace` (the
  byte ranges `0x21–0x2F`, `0x3A–0x40`, `0x5B–0x60`, `0x7B–0x7E`, plus space and
  `0x09–0x0D`), joins the tokens with `_`, and lowercases; an all-punctuation
  input yields the bare delimiter.

The integer-range family is ported directly: `parse_int_list` expands `a-b`
spans and sorts (keeping duplicates); `format_int_list` runs the library's
`deque`-based contiguous-run collapse; `complement_int_list` takes the set
difference against `range(start)`/`range(end)`; and `int_ranges_from_int_list`
normalizes through the two and emits Python's tuple-of-tuples repr, trailing
comma on a one-tuple included. All arithmetic is small positive `i64`.

## Scope

The ASCII, integer, deterministic surface. strutils' Unicode-aware helpers
(`pluralize`/`singularize`, which use `unicodedata`; `asciify`), the
float-formatting `bytes2human`, and the `zlib` / `html` / `uuid` helpers are out
of scope. The vendored `boltons/strutils.py` is Python 3 and imports only the
standard library, so `oracle.py` runs the real module unchanged.

## Running

```bash
karac run mini_strutils.kara                  # interpret
karac build --mir mini_strutils.kara          # native binary
python3 -s -P -B mk.py                            # regenerate data/jobs.tsv
python3 -I oracle.py                               # regenerate expected.out
python3 -s -P -B gen_kara.py                       # regenerate the embedded jobs
```
