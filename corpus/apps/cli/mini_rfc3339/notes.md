# mini_rfc3339

A port of [rfc3339-validator](https://github.com/naimetti/rfc3339-validator)
0.1.4 (MIT, Nicolas Aimetti) — the RFC 3339 date-time validator `jsonschema`
calls for `format: date-time` when checking OpenAPI and JSON Schema documents.

## What it does

`mk.py` writes `data/jobs.tsv`, one timestamp per line. `oracle.py` runs each
through `vendor/rfc3339_validator.py` and prints `expected.out`: `true` or
`false` per line. `mini_rfc3339.kara` reimplements the check and must match
byte-for-byte on every backend.

The library matches one `re.VERBOSE | re.ASCII` regex and then applies two
post-checks (year 0 is rejected; the day must not exceed the month's real
length). The port runs that regex as an explicit fixed-position byte scan —
the format is rigid, so every field sits at a known offset — and then applies
the same post-checks:

- **Year** `\d{4}`, rejected when 0.
- **Month** `0[1-9]|1[0-2]`, **hours** `[01]\d|2[0123]`, **minutes** and
  **seconds** `[0-5]\d` — each alternation collapses to a plain integer-range
  test, because every in-range value has exactly one two-digit spelling.
- **Day** `\d{2}`, then range-checked `1 <= day <= days_in_month(year, month)`
  with a proper leap-year February (so `2020-02-29` passes, `2021-02-29`,
  `1900-02-29` and `2020-04-31` fail).
- Optional fractional seconds `(?:\.\d+)?`, then a required `Z` **or** a
  numeric offset `[+-](?:[01]\d|2[0123]):[0-5]\d`.

## Scope

As in the library, only uppercase `T` and `Z` are accepted (a lowercase
`t`/`z` is rejected), and leap seconds are not supported (`:60` fails). Inputs
are ASCII, which is the alphabet the library's `re.ASCII` `\d` already fixes.

## Running

```bash
karac run mini_rfc3339.kara            # interpret
karac build --mir mini_rfc3339.kara    # native binary
python3 -s -P -B mk.py                 # regenerate data/jobs.tsv
python3 -I oracle.py                   # regenerate expected.out
```
