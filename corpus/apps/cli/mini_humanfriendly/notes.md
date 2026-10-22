# mini_humanfriendly

The parse-and-format core of `humanfriendly` 10.0 (MIT) — the library a great
many CLI tools lean on to turn byte counts, distances, durations and plain
numbers into text a person reads, and to parse the same forms back. This port
follows the library's own `humanfriendly/__init__.py`, function for function.

Nine functions are covered, both directions where the library has both:

- **`format_size` / `parse_size`** — a byte count to `1.5 GB` / `1 KiB` /
  `6 bytes` and back, in SI (decimal, powers of 1000) or binary (powers of
  1024) units, with `keep_width` padding the singular unit. `parse_size`
  tokenizes the input, matches the unit against the decimal and binary tables
  (bare `KB`/`kilobyte`, the `KiB`/`mebibyte` binary spellings), and raises
  `InvalidSize` with the library's exact text — including the tokenizer's own
  rendering of the offending input — for a number with no unit it can place.
- **`format_length` / `parse_length`** — metres to `1 km` / `5 mm` / `15.3 cm`
  and back, over the nm…km prefix table.
- **`format_timespan` / `parse_timespan`** — seconds to
  `1 minute and 30 seconds` / `1 year, 2 days and 4 minutes`, with `detailed`
  and `max_units`, and the inverse over the `s`/`m`/`h`/`d`/`w`/`y` units and
  their long spellings.
- **`format_number`** — thousands grouping with a chosen number of decimals
  (`6,000,000`, `-6,000,000`).
- **`round_number`** — a float to its shortest exact decimal, dropping a
  trailing `.0` unless `keep_width`.
- **`coerce_boolean`** — the `yes`/`no`/`true`/`false`/`on`/`off`/`1`/`0`
  vocabulary to a bool, raising `ValueError` on anything else, with the
  library's message.

**Formatting without a float formatter.** Kāra's format spec has no `f`
conversion, so the port reproduces the library's `round(x, n)` with its own
round-half-to-even fixed-point (`round_scaled` / `fmt_fixed`) and presents a
parsed length or timespan through `num_str` — an integer when the value is
integral, otherwise a six-decimal form stripped of trailing zeros — exactly as
`oracle.py` does on the Python side. That presentation is the harness's; every
number it prints is the library's own computation. The jobs are chosen so each
formatted value has a short exact decimal, so the two sides agree byte for
byte.

**Jobs.** `data/jobs.tsv` holds 88 jobs, one per line, tab-separated. Field 0
names the kind (`size_fmt`, `size_parse`, `len_fmt`, `len_parse`, `time_fmt`,
`time_parse`, `num`, `round`, `bool`); the remaining fields are its arguments.
Each job prints a `== ` header of its fields and a two-space result line: the
formatted string, the parsed number, `True`/`False`, or `err=<message>` with
the library's exact exception text. The set covers every unit table in both
directions, the `keep_width` / `binary` / `detailed` / `max_units` flags, the
thousands-grouping and sign paths, half-to-even rounding ties, and every error
path (`InvalidSize`, `InvalidLength`, `InvalidTimespan`, `ValueError`).

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `humanfriendly` in `vendor/humanfriendly/` (MIT, in
`LICENSE-humanfriendly.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
