# mini_persiantools

A port of the [persiantools](https://pypi.org/project/persiantools/) library
(6.2.0; MIT, Majid Hajiloo): conversion between the **Gregorian** and the
**Jalali** (Persian / Shamsi) calendar, plus the **Persian-digit** transform
(`en_to_fa`). The Jalali calendar is the official civil calendar of Iran and
Afghanistan; its new year, **Nowruz**, falls on the March equinox.

## What it does

`mk.py` writes `data/jobs.tsv`: one job per line, either

```
g YYYY-MM-DD     convert a Gregorian date to Jalali
j YYYY-MM-DD     convert a Jalali date to Gregorian
```

For each line the job prints the converted date in ISO form and its
Persian-digit rendering:

```
<iso> <fa>
```

For example `g 2021-03-21` (Gregorian) prints `1400-01-01 ۱۴۰۰-۰۱-۰۱` — Nowruz
1400 — and `j 1403-12-30` (an Esfand leap day) prints `2025-03-20 ۲۰۲۵-۰۳-۲۰`.

`oracle.py` replays every line through the vendored `persiantools` library and
writes `expected.out`; `mini_persiantools.kara` reproduces it byte-for-byte on
every backend.

## How the port works

The two calendars are bridged through a **day count**:

- **The Gregorian side is a proleptic ordinal**, the same `ymd ↔ ordinal`
  arithmetic CPython's `datetime` uses: a year count over the 400/100/4-year
  leap cycle, then month and day. This is exact for every Gregorian year.
- **The Jalali side is the plain 33-year rule** the library applies in this
  range: a year is leap when `(25·y + 11) % 33 < 8`, and a fixed table gives the
  number of leap years preceding each position in the 33-year cycle, so the day
  count before any Jalali year is `365·(y−1) + 8·cycles + leaps_before[phase] −
  1`. The inverse estimates the year as `days·33 / 12053 + 1` and corrects it by
  at most one step in each direction.
- **One constant ties them together**: `EPOCH_ORDINAL = 226895`, the Gregorian
  ordinal of Jalali day zero. `g → j` is `j_from_days(gregorian_ordinal −
  EPOCH)`; `j → g` is `ord2ymd(EPOCH + jalali_day_count)`.

`en_to_fa` maps each ASCII digit `0..9` to the Persian digit `U+06F0..U+06F9`,
whose UTF-8 encoding is the two bytes `0xDB, 0xB0+d` — so the output exercises
non-ASCII UTF-8 emission — and keeps every other byte (here the `-`) unchanged.

Everything is integer arithmetic over `i64`, so the four backends agree by
construction.

## Scope

- The conversion covers the **modern range** — roughly Gregorian 1850–2100 /
  Jalali 1228–1478 — where persiantools' calendar reduces to the clean 33-year
  rule. The library additionally carries an *ancient* flip and a set of ICU4X
  correction tables for dates far outside this window (before ~1799 Gregorian);
  those tables are a documented library boundary and are not ported. Every date
  in the job list lies inside the modern range, where the port is exact against
  the library.
- Only the **date** conversion and the **digit** transform are ported;
  `persiantools`' datetime/time-of-day, timezone (`zoneinfo`) and
  Arabic-character folding surfaces are out of scope.

The port was checked against the library on every backend over the job list
(leap and common years in both calendars, Nowruz boundaries, and the 31/30/29-day
Jalali month lengths), and separately during development over every Gregorian
date in the modern range (~91k dates, each round-tripped both ways).

The vendored library is pure Python and imports only the standard library (it
lists `tzdata` only as a Windows timezone-data extra, unused here). `vendor/` is
left untouched.

## Running

```bash
karac run mini_persiantools.kara             # interpret
karac build --mir mini_persiantools.kara     # native binary
python3 -s -P -B mk.py                           # regenerate data/jobs.tsv
python3 -I oracle.py                              # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded jobs
```
