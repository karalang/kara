# mini_priceparser

The number-parsing core of `price-parser` 0.5.1 (BSD-3, Scrapinghub) — the
library Scrapy-based scrapers use to turn a scraped price string into a clean
`Decimal`.

The port follows `price_parser.parser` for the two functions that do the
numeric work:

- **`get_decimal_separator`** — given a bare number string, decide which of
  `.`, `,` or `€` is the decimal separator (or none). The library's rule,
  reproduced here, is the first separator whose run of digits to the end of the
  string numbers **one, two, or four-or-more**; a run of exactly three digits
  reads as a thousands group, so `12.999` and `1,234,567` have no decimal
  separator while `12.99`, `3,0000` and `1,235€99` do.
- **`parse_number`** — strip whitespace, take the decimal separator (the forced
  one if given, otherwise the guess above), and do the library's
  mode-dependent replacements: with no separator every `.`/`,` is dropped; with
  `.` the commas are dropped; with `,` the dots are dropped and the comma
  becomes the point; with `€` the dots and commas are dropped and the euro sign
  becomes the point. The cleaned string is then a `Decimal`.

The result is rendered exactly as CPython's `str(Decimal(...))` — leading zeros
dropped, a bare trailing dot dropped, fractional zeros kept (`140.000` under a
`,` separator is `140000`, but forced `.` keeps it `140.000`) — computed with
integers and string work, so no float is ever formed. A string that is not a
valid number returns `None`.

**Scope.** price-parser's other two layers — guessing the currency symbol from
a 600-entry table and pulling the price substring out of surrounding prose with
a pair of backtracking regexes — are deliberately **not** part of this port;
this app is the separator-guessing number parser, which is the piece with the
interesting arithmetic and the one that is Decimal-exact.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: the number
string and the forced decimal separator (`-` for none / guess). The set covers
the 1/2/≥4-digit rule, all four separator modes, the euro-as-decimal case
(`12€34`, `1,235€99`), European `1.234.567,89` grouping, forced separators and
the invalid inputs that return `None`.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `price_parser` in `vendor/price_parser/` (BSD-3, in
`LICENSE-price_parser.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
