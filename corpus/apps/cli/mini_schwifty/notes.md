# mini_schwifty

A port of the IBAN core of [schwifty](https://github.com/mdomke/schwifty)
2026.10.0 (MIT, Martin Domke) — the IBAN/BIC library that payments, banking and
fintech code validates account numbers with. This kata ports the
`schwifty.IBAN` validator: checking an IBAN against the ISO 13616 registry,
formatting it in blocks of four, and splitting the BBAN into its bank / branch /
account components.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    <op> \t <iban>

- `valid <iban>` → `VALID <compact>` for a well-formed IBAN, otherwise the exact
  schwifty error message;
- `fmt <iban>` → the compact IBAN in blocks of four (`IBAN.formatted`);
- `parts <iban>` → `bank=<> branch=<> account=<>` from the BBAN components.

Valid IBANs are built with `IBAN.from_bban`, which computes the correct mod-97
check digits, across countries whose BBAN specs differ (all-numeric, an
alphabetic bank code like GB, an alphanumeric `c` field like AD, with and
without a branch code); a few are written spaced or lower-cased to exercise the
input cleaning. The invalid cases walk each error path. `oracle.py` runs the
jobs through the vendored `schwifty`; `mini_schwifty.kara` reproduces
`expected.out` byte-for-byte on every backend.

## How the port works

The input is first cleaned (whitespace stripped, upper-cased), as schwifty's
`common.clean` does. `validate()` then runs the same checks in the same order,
and the first failure decides the message:

1. **characters** — the whole value must match `[A-Z]{2}\d{2}[A-Z0-9]+`;
2. **country** — the two-letter code must be in the registry (this lookup
   happens inside the length step, so an unknown country is reported before a
   wrong length — matching schwifty's precedence exactly);
3. **length** — must equal the country's `iban_length`;
4. **BBAN structure** — the characters after position 4 must match the
   country's `bban_spec`, each `N!t` segment being `n` a digit, `a` a letter,
   `c` an alphanumeric;
5. **checksum** — the ISO 7064 mod-97-10 check digits must match.

The checksum avoids big integers. schwifty numerifies `BBAN + country` (each
letter `A`–`Z` → `10`..`35`, i.e. two decimal digits) and takes that number
`* 100` modulo 97, then `98 - r` is the check. Since that number is ~40 digits,
the port streams the decimal digits through a running `(acc*10 + d) % 97`
accumulator instead of building it.

The effective ISO 13616 registry (127 countries — `generated.json` merged with
`overwrite.json`) is dumped from the vendored library into `iban_registry.tsv`
and embedded by `gen_kara.py` as `registry_text()`, parsed at run time so the
embedding stays a flat string rather than a large table constructor.

## Scope

IBAN validation, formatting and component extraction. The national
(country-specific) BBAN checksum (`validate_bban=True`), BIC lookup, the bank
registry (trimmed from the vendored copy — it is never touched by validation),
and IBAN generation are out of scope. The vendored `schwifty/` is pure-Python;
its `__init__` version lookup is wrapped so the vendored copy imports without
installed dist metadata (a packaging shim, no behavior change). Regenerating
`expected.out` needs schwifty's own dependencies (`pycountry`, `rstr`) on the
`PYTHONPATH`, as the library imports them at load.

## Running

```bash
karac run mini_schwifty.kara                  # interpret
karac build --mir mini_schwifty.kara          # native binary
python3 -s -P -B mk.py                           # regenerate data/jobs.tsv
python3 -I oracle.py                              # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded blocks
```
