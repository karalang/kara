# mini_pyluach

A port of [pyluach](https://pypi.org/project/pyluach/) 2.3.0 (MIT; MS List), the
Hebrew-calendar library used by Jewish-date applications and the `holidays`
ecosystem. It converts between the proleptic **Gregorian** calendar and the
**Hebrew lunisolar** calendar — the 19-year Metonic cycle whose year lengths are
fixed by the molad (lunar conjunction) rules — reports the day of the week, and
renders a Hebrew date as a **gematria** (Hebrew-numeral) string.

## What it does

`mk.py` writes `data/jobs.tsv`: one tab-separated job per line, one of

- `g2h <y> <m> <d>` — Gregorian date → Hebrew date, printed `HYEAR-MM-DD` (the
  Hebrew year, then the month 1–13 and day zero-padded), or `INVALID`;
- `h2g <y> <m> <d>` — Hebrew date (`m` is 1 = Nissan … 7 = Tishrei … 13 = Adar 2)
  → Gregorian date `YYYY-MM-DD`, or `INVALID`;
- `wd <y> <m> <d>` — day of the week of a Gregorian date, `1` (Sunday) through
  `7` (Shabbos), or `INVALID`;
- `heb <y> <m> <d>` — Gregorian date → the converted Hebrew date as a gematria
  string `<day> <month> <year>` (Hebrew numerals, the Hebrew month name, and the
  Hebrew year with its thousands place), or `INVALID`.

`oracle.py` replays each job against the vendored `pyluach` library and writes
`expected.out`; `mini_pyluach.kara` reproduces it byte-for-byte on every backend.
The `heb` strings carry Hebrew characters, emitted as UTF-8.

## How the port works

`mini_pyluach.kara` follows `dates.py`, `utils.py` and `gematria.py` exactly. The
heart of the Hebrew calendar is `_elapsed_days(year)` — the molad arithmetic that
places Rosh Hashana, with the four postponement (dechiya) rules — and the
month-length rules (the fixed months, the leap-year Adar, and the variable
Cheshvan/Kislev that depend on the year's total length). Both directions bridge
through the **Julian day**: a Gregorian date maps to a JD, a JD maps to a Hebrew
date by walking the year's months, and the inverse directions reverse each step.

pyluach's `GregorianDate.jd` is the one place the library uses floating point —
`int(365.25*year) + int(30.6001*month) + …` — so the port does exactly those two
truncations in `f64` (IEEE double, the same representation Python uses) and every
other step in `i64`, which is what makes the four backends agree bit for bit. The
Hebrew→Gregorian direction uses the Fliegel–Van Flandern algorithm, all integer.
The gematria renderer mirrors `gematria.py` letter for letter: the units/tens/
hundreds lookup, the `400`-repetition for large hundreds, the `15→טו` / `16→טז`
substitutions that avoid spelling a divine name, and the geresh/gershayim
punctuation.

## Scope

The port covers both conversion directions, the weekday, and the gematria
rendering, over the proleptic range and the leap-year (13-month) forms, and it
validates Hebrew dates exactly as `HebrewDate` does (month 1–13, Adar 2 only in a
leap year, the day within the month's real length), printing `INVALID`
otherwise. One boundary is the app's own, not the library's:

- **Gregorian input domain.** `GregorianDate` performs *no* input validation — it
  will happily build `GregorianDate(2000, 13, 40)` and convert it — and its BC
  path (`year < 1`) uses Python's floor division, which differs from this port's
  integer arithmetic. So the app fixes a clean proleptic-Gregorian domain
  (`1 ≤ year ≤ 9999`, a real month and day) and rejects anything outside it as
  `INVALID`; `oracle.py` applies the identical rule, so the comparison stays
  honest.

The vendored library imports only `datetime`, `numbers`, `abc`, `enum`,
`functools`, `itertools` and `collections` from the standard library, and the
port exercises only the pure integer/float calendar and gematria code, so
`oracle.py` runs exactly the logic the port mirrors.

## Running

```bash
karac run mini_pyluach.kara                   # interpret
karac build --mir mini_pyluach.kara           # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
