# mini_korean_lunar

A port of
[korean_lunar_calendar](https://pypi.org/project/korean-lunar-calendar/) 0.4.0
(MIT; Jinil Lee), the library behind the Korean lunar date used across the Korean
calendar ecosystem. It converts between the Gregorian (solar) calendar and the
Korean lunisolar calendar — the one that fixes Seollal (Lunar New Year) and
Chuseok — and renders the **sexagenary (gapja / 六十甲子) cycle** string for a
date, in both the Korean (한글) and Chinese (漢字) scripts. The conversion is
table-driven over a packed record per year and is valid for years **1000–2050**.

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `s2l <y> <m> <d>` — solar date → Korean lunar date, printed `YYYY-MM-DD`
  (with ` Intercalation` appended when the lunar month is a leap month), or
  `INVALID`;
- `l2s <y> <m> <d> <i>` — Korean lunar date (`i = 1` if it is the leap month) →
  solar date `YYYY-MM-DD`, or `INVALID`;
- `gap <y> <m> <d>` — solar date → Korean gapja string, or `INVALID`;
- `cgap <y> <m> <d>` — solar date → Chinese gapja string, or `INVALID`.

`oracle.py` replays each job against the vendored `korean_lunar_calendar`
library and writes `expected.out`; `mini_korean_lunar.kara` reproduces it
byte-for-byte on every backend. The gapja strings carry CJK characters, emitted
as UTF-8.

## How the port works

`mini_korean_lunar.kara` follows `korean_lunar_calendar.py` exactly. The heart
of the library is `KOREAN_LUNAR_DATA`, 1051 packed `u32` records — one per year
1000–2050 — carrying, per bit-field, the twelve lunar-month lengths (29 or 30
days), the intercalary (leap) month index and its length, the total lunar-year
day count, and a solar leap-year flag. The port embeds that table verbatim as a
space-separated hex list (`lunar_data_str`, parsed once into a `Vec[i64]`) and
reproduces the same field extraction (`>> 17 & 511` for the lunar-year length,
`>> 12 & 15` for the intercalary month, and so on).

Conversion is the library's own absolute-day arithmetic: each calendar maps a
date to a day count from the 1000-epoch, and the inverse walks the twelve months
of the candidate year to recover month and day. Every value is a bounded
integer, so the whole computation is exact `i64` — no floating point anywhere —
which is why the four backends agree bit for bit. To keep the per-date month and
year walks from re-summing the table on every call, `build_cal` precomputes
cumulative solar- and lunar-day arrays once (`cum_s`, `cum_l`); the results are
identical to the library's running sums.

The gapja string indexes three ten/twelve-element code-point tables (cheongan ×
ganji) by the absolute lunar day, month and year, and joins them with the
year/month/day unit characters, exactly as the library does; the Korean and
Chinese variants differ only in which code-point tables and intercalation marker
they use.

## Scope

The port covers the full 1000–2050 range for both conversion directions, the
leap-month forms, and the two gapja scripts, and validates out-of-range and
non-existent dates (printing `INVALID`) the way the library's `setSolarDate` /
`setLunarDate` return `False`. Two things follow the library's own boundaries:

- **Range.** The library's data table is years 1000–2050 and it rejects any date
  outside `1000-02-13`…`2050-12-31` (solar) or `1000-01-01`…`2050-11-18`
  (lunar); the port rejects the same, as `INVALID`.
- **The 1582 Gregorian-reform gap.** The library treats the dates
  `1582-10-05`…`1582-10-14` — dropped when the Gregorian calendar was adopted —
  as invalid; the port does the same.

The vendored library imports only `datetime` and `numbers` from the standard
library and the port uses none of those paths (both are only used by helpers the
port does not exercise), so `oracle.py` runs exactly the table-and-integer code
the port mirrors.

## Running

```bash
karac run mini_korean_lunar.kara              # interpret
karac build --mir mini_korean_lunar.kara      # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
