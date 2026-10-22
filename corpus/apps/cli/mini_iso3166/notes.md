# mini_iso3166

A port of [iso3166](https://github.com/deactivated/python-iso3166) 3.0.0 (MIT,
Mike Spindel), the self-contained ISO 3166-1 country database — the 250 country
records with lookup by alpha-2, alpha-3, numeric code, name and
apolitical_name. It is used wherever a service needs to normalise or validate a
country reference without a network call.

## What it does

`mk.py` writes `data/jobs.tsv`, one lookup per line. `oracle.py` runs each
through the vendored library (`vendor/iso3166/`) to produce `expected.out`.
`mini_iso3166.kara` reimplements the `countries` lookup object and must match
that output byte-for-byte on every backend.

The port reproduces `_CountryLookup.get`'s dispatch exactly, quirks included: a
two-character key is always alpha-2; a three-character all-digit key other than
`"000"` resolves as numeric *before* alpha-3 is tried; an `int` key is
zero-padded to three digits (`f"{n:03d}"`); a name miss falls through to
`apolitical_name`; and a final miss raises `KeyError` with the original key.
`get`, `get(int)`, `in` (`__contains__`) and `len` are all covered, and the
`Country` NamedTuple repr is emitted field for field.

The 250 records come from the library's own `_records`, packed one per row and
split at startup. Each row carries the display name together with Python's
`str.upper()` of the name and apolitical_name — the keys the library's indexes
use — so the port never has to case-fold stored data at run time.

## Scope

Lookup keys are kept ASCII (codes are ASCII by definition; the name lookups
chosen are ASCII and apostrophe-free), which is what lets the port's ASCII
upper-casing match Python's `str.upper()` and the `Country` repr use plain
single-quote delimiters with no escaping. The six non-ASCII countries (AX, CI,
CW, RE, BL, TR) and the three apostrophe names (CI, KP, LA) are exercised
through their ASCII codes, which exposes their stored names verbatim in output;
a cased lookup on a non-ASCII name (which would need full Unicode case mapping)
is out of scope.

## Running

```bash
karac run mini_iso3166.kara          # interpret
karac build --mir mini_iso3166.kara  # native binary
python3 -s -P -B mk.py               # regenerate data/jobs.tsv
python3 -I oracle.py                  # regenerate expected.out
```
