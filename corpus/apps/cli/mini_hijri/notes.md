# mini_hijri

The converter core of `hijri-converter` 2.3.2 (MIT, Mohammed Alshehri) — the
library that converts dates between the Gregorian calendar and the Umm al-Qura
lunar Hijri calendar used across the Islamic world (and that backs
date pickers, prayer-time apps and government systems in the region).

The port follows the library function for function:

- **`Gregorian.to_hijri`** — range-check the Gregorian date, take its proleptic
  ordinal, convert to a Reduced Julian Day, `bisect` into the Umm al-Qura
  `MONTH_STARTS` table, and read off the Hijri year, month, day and month
  length.
- **`Hijri.to_gregorian`** — validate the Hijri date (year range, month `1..12`,
  day `1..month_length`), index `MONTH_STARTS` directly, and convert the
  Reduced Julian Day back to a Gregorian ordinal and date.
- **`helpers`** — the JDN / ordinal / Reduced-Julian-Day conversions.
- **`weekday` / `isoweekday`**, **`isoformat`** and **`dmyformat`** (default and
  a custom separator with padding off), and the localized **month** and **day
  names** in English, Arabic and Bengali, plus the `AH` / `CE` era notations.

The proleptic-Gregorian ordinal is CPython's own `date.toordinal` /
`fromordinal` algorithm ported directly, so the day a Hijri date lands on — and
its weekday — is identical to Python's. Every `OverflowError` ("date out of
range") and `ValueError` ("month must be in 1..12", "day must be in
1..N for month") carries the library's exact text. The whole thing is integer
and string arithmetic; **no float is ever formed**.

`MONTH_STARTS` (1,897 month-boundary Julian days) is stored compactly as one
digit per month-length delta (each month is 28–31 days, encoded as `delta-28`)
and rebuilt at run time; the three name tables are the library's verbatim.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: a direction
(`g2h` Gregorian→Hijri, `h2g` Hijri→Gregorian) and an ISO date in the source
calendar. The set spans the supported range (Gregorian 1924-08-01 .. 2077-11-16,
Hijri 1343-01-01 .. 1500-12-30), both endpoints, and the out-of-range and
invalid-field cases that raise.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `hijri_converter` in `vendor/hijri_converter/` (MIT, in
`LICENSE-hijri_converter.txt`). `mk.py` writes `data/jobs.tsv`; `gen_data.py`
emits the Kāra `MONTH_STARTS` and name-table functions and `gen_kara.py` embeds
the jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
