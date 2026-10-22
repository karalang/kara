# mini_jdcal

A port of [jdcal](https://pypi.org/project/jdcal/) 1.4.1 (BSD; Prasanth Nair),
the small library for converting between **calendar dates and Julian dates**. A
Julian date counts days from a fixed epoch; it is the lingua franca of astronomy,
ephemeris tables and any code that has to do arithmetic across the
Julian/Gregorian calendar boundary. jdcal provides the four proleptic
conversions (both directions, both calendars) plus a Gregorian leap-year test.

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `g <y> <m> <d>` — Gregorian calendar date → Modified Julian Date (an integer);
- `j <y> <m> <d>` — Julian calendar date → Modified Julian Date;
- `rg <mjd>` — MJD → proleptic Gregorian date, printed `y|m|d`;
- `rj <mjd>` — MJD → proleptic Julian date, printed `y|m|d`;
- `leap <y>` — `is_leap(y)` in the Gregorian calendar → `1` or `0`.

`oracle.py` replays each job against the vendored `jdcal` library and writes
`expected.out`; `mini_jdcal.kara` reproduces it byte-for-byte on every backend.

## How the port works

`mini_jdcal.kara` follows `jdcal.py`'s formulae exactly — the same
Fliegel–Van Flandern-style integer polynomials for `gcal2jd`/`jcal2jd` and their
inverses `jd2gcal`/`jd2jcal`, and the same leap-year rule. The library carries the
Julian date as two doubles (`MJD_0` plus the MJD) and truncates each division with
`math.modf` (its `ipart`). Because every value in a date-only conversion is an
exact integer far inside a double's 53-bit range, the port does the whole
computation in `i64` with an explicit **truncate-toward-zero** division (`tdiv`),
which is what `math.modf(a/b)` computes, so the two backends agree bit for bit —
including for BC years (given as `-(BC-1)`) and the carry-over forms the library
documents, where an out-of-range month or day rolls into the neighbouring
month or year (`gcal2jd(2000, 2, 30)` == `gcal2jd(2000, 3, 1)`).

## Scope

The port covers all five public functions over the full proleptic range, negative
(BC) years, and the negative/zero/overflowing month and day forms `gcal2jd`
accepts. One thing is deliberately out of scope:

- **Time of day.** `jd2gcal`/`jd2jcal` return a fourth value, the fractional part
  of the day, and `gcal2jd` lets you add a time as a fraction. These conversions
  are date-only (midnight), so that fraction is always `0.0`; reproducing the
  floating-point fractional-day arithmetic is the one float surface jdcal has and
  is not part of the calendar math itself. The port prints `y|m|d` and the MJD as
  integers.

The vendored `jdcal` imports only `math`, and the port uses none of its
floating-point paths, so `oracle.py` runs exactly the integer code the port
mirrors.

## Running

```bash
karac run mini_jdcal.kara                   # interpret
karac build --mir mini_jdcal.kara           # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
