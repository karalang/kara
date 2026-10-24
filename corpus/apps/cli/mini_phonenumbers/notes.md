# cli/mini_phonenumbers

phonenumbers 8.13.52, the Python port of Google's libphonenumber, ported
to Kāra with the metadata of the 48 regions (and the +800 numbers) its
jobs reach. It runs a
list of jobs through the library's API and then two small tools built on
it, and prints what `oracle.py` prints when it runs the same jobs through
the real library.

```
parse '0 15 11 96123 4567' BR: Country Code: 55 National Number: 11961234567 Country Code Source: 20 Preferred Domestic Carrier Code: 15
  e164 +5511961234567 | intl +55 11 96123-4567 | national (11) 96123-4567 | rfc3966 tel:+55-11-96123-4567
  type MOBILE, valid True, region BR, possible IS_POSSIBLE, geographical True
...
dial '011 15 2345-6789' AR: +5491123456789
  from US: 011 54 9 11 2345-6789 | mobile '+54 9 11 2345-6789' | '+5491123456789'
...
type US '16502530000': 1 | 16 | 1 65 | 1 (650 | 1 (650) 2 | 1 (650) 25 | 1 (650) 253 | 1 (650) 253-0 | 1 (650) 253-00 | 1 (650) 253-000 | 1 (650) 253-0000
...
same number: Wim Sato and Wim Sato (work) (EXACT_MATCH)
valid 67, rejected 12
...
DE 0900 1234567: dial 0900 1 234567, PREMIUM_RATE, 12 min, 18.00
947 minutes, 135.09
```

## What it does

`mk.py` writes three inputs under `data/`; `gen_kara.py` adds two more,
extracted from the vendored metadata.

- **The jobs** (`data/jobs.jsonl`, 249 of them):
  - 124 `parse` calls on the ways people write numbers, from 22 default
    regions and none. They cover national and international forms, IDD
    prefixes (`011`, `00`, `0011`, `0014`, Russia's `8 10`, New Zealand's
    `0161`, Brazil's carrier-selecting `0021`), extensions in eight
    spellings, RFC 3966 `tel:` URIs with
    `phone-context` and `isub`, letters (`1-800-FLOWERS`), fullwidth and
    Arabic-Indic digits, text around the number, and the five kinds of
    parse error. Each parsed number is shown as `str(PhoneNumber)`, then
    in the four formats, with its type, validity, region, possible-length
    verdict, area code and destination code lengths, whether it can be
    dialled from abroad and, when the raw input was kept, the original
    format.
  - 16 numbers dialled from up to 11 regions each:
    `format_out_of_country_calling_number` and
    `format_number_for_mobile_dialing`, with and without formatting.
  - The alpha-keeping formatter, the carrier-code formatters, 12
    `is_number_match` pairs and 5 `truncate_too_long_number` calls.
  - 34 key sequences through `AsYouTypeFormatter`, with the remembered
    cursor position for some, and the first four keys again after
    `clear()`.
  - 13 regions' calling code, national prefix, NANPA membership,
    portability, supported types and the regions sharing the code, and 36
    `example_number_for_type` calls.
- **The address book** (`data/contacts.csv`, 79 contacts) has numbers
  built from the metadata's example numbers, with the last two digits
  changed, one in ten a digit short or long, written with spaces, dashes,
  dots, parentheses, a national prefix, `+`, `00` or an extension, and a
  few contacts listed twice. The tool prints each number in E.164 with its
  region and type and in its original format, or why it was rejected,
  then the pairs `is_number_match` calls the same number, and counts by
  region and type.
- **The call log** (`data/calls.txt`, 44 calls from seven regions) dials
  each number the way `format_number_for_mobile_dialing` says to from the
  caller's region, and prices the call by type and distance: toll-free
  free, premium rate $1.50 a minute, domestic 1 or 3 cents, abroad 9 or 24.
  Three calls cannot be dialled from where they were made.

## How the port works

The library is regular expressions over metadata, so the port keeps its
shape function for function (`parse_full`, `format_number`,
`number_type_helper`, `Aytf.input_digit` and so on) and runs every
pattern through the port of Python's `re` from mini_arrow (lines
16-2501: the JSON reader, the code-point helpers and the engine). Text is
a `Vec[i64]` of code points, as in Python.

- **Metadata.** `gen_kara.py` reads each vendored `region_*.py` file with
  `ast` and writes its `PhoneMetadata(...)` call as a line of
  `data/metadata.jsonl`; `data/codes.txt` is the calling code table from
  `data/__init__.py`. `load_ctx` (L2959) reads both into `Meta` records,
  with Python's `None` as `Option` and an absent metadata as index -1.
- **Patterns** (L2502). Every pattern the library uses, its own and the
  metadata's, gets a slot in a table when the metadata is read, and is
  compiled the first time it is matched, so a job compiles only what it
  touches. `rx_sub` expands the only template form the library uses
  (`\1` to `\9`), which `gen_kara.py` checks every template in the
  metadata against.
- **The library's own patterns** (L2637) are built the way
  phonenumberutil.py builds them, extension labels included, with
  `\uXXXX` escapes where Python has the characters themselves.
- **Fidelity notes.** `_match_national_number` is only ever called without
  a prefix allowance here, so `pattern.match` plus `fullmatch` reduces to
  `fullmatch`. `unicodedata.digit` and `str.isdigit` are taken as the
  decimal digits, which is all the jobs type; the superscripts and circled
  digits Python would also accept are not handled.

`oracle.py` builds the `phonenumbers` and `phonenumbers.data` packages
itself rather than importing their `__init__.py` files, which pull in the
geocoder, carrier and time zone data, the matcher and the alternate
formats. It registers a loader for every region, loads the vendored ones,
and fails at the end if any job reached a region that is not vendored,
so the port's 49 metadata files are all the jobs can see.

## Compiler gaps

One, a new shape of an open row, B-2026-10-24-32 (from mini_arrow):
`karac check` counts `m.cc.unwrap_or(0)` as a move of `m.cc`, an
`Option[i64]` field and so `Copy`, and then refuses `metas.push(m)` in
`load_ctx` because `m` was partly moved. The MIR interpreter runs the
refused program correctly. `karac fix` inserted `.clone()` before the
`unwrap_or`. Its other five fixes were real: two `.clone()`s on vector
elements matched as a pair, one on a field matched by value, and two
`ref` bindings in `if let`. With those the port passed `karac check`, and
its first run printed the oracle's output under the MIR interpreter, the
LLVM backend, the ASan build and the native binary, with nothing on
stderr.

## Scope

The parser, the formatters (`format_number`, out-of-country, original
format, alpha-keeping, carrier codes, mobile dialing, `format_by_pattern`
as the original-format path uses it), types and validity, region
lookup, possible lengths, truncation, number matching, example numbers
and the as-you-type formatter. Not ported: `PhoneNumberMatcher` (finding
numbers in text), `shortnumberinfo`, the geocoder, carrier and time zone
lookups, the alternate formats the matcher uses,
`example_number_for_type(None, ...)` (it walks a set, whose order is
per-process), and the metadata of the regions no job reaches.

## Running

```bash
python3 -s -P -B mk.py            # data/jobs.jsonl, contacts.csv, calls.txt
python3 -I -B gen_kara.py         # data/metadata.jsonl, codes.txt, and inputs_text()
python3 -I -B oracle.py > expected.out
karac run mini_phonenumbers.kara
karac build --mir mini_phonenumbers.kara && ./mini_phonenumbers
```

About 22 s on the MIR interpreter (a debug build of `karac`), 6 s on the
LLVM backend and 0.03 s built. It writes `data/` to the working
directory and removes it at the end.

`vendor/phonenumbers` holds phonenumbers 8.13.52's core modules
(`phonenumberutil.py`, `asyoutypeformatter.py`, `phonemetadata.py`,
`phonenumber.py`, `util.py`, `re_util.py`, `unicode_util.py`),
`data/__init__.py` and 49 `data/region_*.py` files, unchanged, under the
Apache License 2.0 (`LICENSE-phonenumbers.txt`).
