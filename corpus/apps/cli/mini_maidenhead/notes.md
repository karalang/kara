# mini_maidenhead

A port of [maidenhead](https://pypi.org/project/maidenhead/) 1.8.0 (MIT,
SciVision): the converter between latitude/longitude and Maidenhead locators,
the grid squares amateur-radio operators trade on the air and in contest logs.
`FN31pr` is the square around the ARRL station in Newington, Connecticut; each
extra pair of characters narrows the square.

## What it does

`mk.py` writes `data/jobs.tsv`, one library call per line. `oracle.py` replays
each call through the vendored library and `mini_maidenhead.kara` through the
port. API calls print one line:

```
maiden(41.714775, -72.727260, 3) => FN31pr
loc(FN31pr, 1) => (41.729166666666664, -72.70833333333333)
url(FN31pr, 0) => https://www.google.com/maps/@?api=1&map_action=map&center=41.708333333333336,-72.75
```

A `cli` job runs the `python -m maidenhead` command's `main()` with the
arguments its argparse would produce, and prints what the command prints:

```
cli(41.714775,-72.727260, 4, u):
  FN31pr21
  https://www.google.com/maps/@?api=1&map_action=map&center=41.7125,-72.73333333333333
```

The 66 jobs cover `to_maiden` at precisions 1 to 6, at the poles and the date
line and with longitudes past ±360; `to_location` corner and centre for 2- to
10-character locators, mixed case, surrounding spaces, and every error;
`to_location_rect`; `to_geoJSONObject` with each combination of the centre
point and the square, as `json.dumps` prints it; `google_maps`; and the command
in each of its modes (`--center`, `--url`, `--geojson`, `--nosquare`, and
latitude/longitude input).

## How the port works

- **`to_maiden`** follows the library's mixed-radix conversion: scale the
  coordinate by the product of the radices, truncate with `int()`, divide out
  the top two radices, then peel digits and letters off from the least
  significant end and interleave latitude and longitude. Python's float `%`
  (which takes the divisor's sign) is `fmod` plus CPython's sign fix-up, and
  integer `//` and `%` are floor operations.
- **`to_location_rect`** builds each axis as a numerator/denominator pair the
  way the library's `reduce` does, then divides. Python's `int / int` is
  correctly rounded, and so is an `f64` division of the same two exact
  integers, so the coordinates come out bit-identical.
- **`'%.7f'`**, which the command uses to print coordinates, is reproduced
  exactly: the float is expanded as `m / 2^k`, `m * 10^7 / 2^k` is rounded half
  to even in `i128`, and the digits are written out, which is what CPython's
  correctly rounded formatting does.
- **JSON** is written the way `json.dumps(..., indent=None)` writes this
  object: `", "` and `": "` separators, the library's key order, tuples as
  lists, floats by their shortest repr.

A restricted-op prototype used during development replaced float `%` and
`'%.7f'` with these algorithms and agreed with the library and CPython on
600,009 checks over random coordinates and precisions.

## Scope

- **The library's own quirk is kept.** `to_location_rect` checks
  `0 > v >= 24`, which no value satisfies, so letters past `X` in the second
  and later letter pairs are accepted (`FN31ZZ` decodes to a point outside the
  square it names). The port does the same.
- Locators are ASCII. `str.strip()` is applied for ASCII whitespace only, and
  JSON escaping covers `"` and `\`, which is everything an ASCII locator needs.
- The command's argparse layer is replaced by calling `main()` directly with
  the parsed arguments.

The vendored library imports only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_maidenhead.kara                # interpret
karac build --mir mini_maidenhead.kara        # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out        # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
