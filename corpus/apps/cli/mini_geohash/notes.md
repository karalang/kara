# mini_geohash

A port of [python-geohash](https://pypi.org/project/python-geohash/) 0.9.2
(Hiroaki Kawai; licensed at the user's choice under Apache-2.0, MIT or New BSD,
used here under MIT): the geohash codec, which names a latitude/longitude cell
with a base-32 string whose prefixes are the enclosing cells (`ezs42` is the
4.9 x 4.9 km cell around 42.6 N, 5.6 W), plus the two Japanese grid-code
codecs that ship in the same package: `jpgrid` (JIS X 0410 "standard mesh"
codes, the grid Japan's census and land statistics are published on) and
`jpiarea` (NTT DoCoMo's Open iArea codes).

The package's speed comes from an optional Rust extension; without it,
`geohash.py` runs its own pure-Python path, and that path is what is ported
and what the oracle runs (it asserts the extension is not loaded).

## What it does

`mk.py` writes `data/jobs.tsv`, one library call per line. `oracle.py` replays
each call through the vendored modules and `mini_geohash.kara` through the
port. Each job prints one line, the `repr()` of the result:

```
enc(42.6, -5.6, 5) => 'ezs42'
bbox(ezs42) => {'s': 42.5830078125, 'w': -5.625, 'n': 42.626953125, 'e': -5.5810546875}
nb(ezs42) => ['ezefr', 'ezs43', 'ezefx', 'ezs48', 'ezs49', 'ezefp', 'ezs40', 'ezs41']
eu64(13835058055282163712, 3) => [(2305843009213693952, 4611686018427387904), (6917529027641081856, None)]
jpenc(35.6812, 139.7671, 40) => '533946005'
iaenc(6.5, 140.0) => Exception: Unsupported location
```

An exception prints as `<type>: <message>`, and a call that warns (geohash
moves latitude 90.0 to the adjacent cell, with a `UserWarning`) gets
` [UserWarning]` appended.

The 275 jobs cover geohash `encode` at precisions 0 to 24, at both poles, at
the date line, with longitudes several turns past ±180 and with coordinates
as small as 1e-300; `decode`, `decode_exactly`, `bbox`, `neighbors` and
`expand` on codes from 0 to 24 characters, including the corner cells where
neighbors wrap or are dropped, and invalid characters (`KeyError`); the uint64
forms (`encode_uint64`, `decode_uint64`, and `expand_uint64` at precisions 1
to 64, with its range merging); `jpgrid` encode at every mesh level (1st, 2nd
and 3rd, half, quarter and eighth, Uni5 and Uni2), decode, `decode_sw` with
and without deltas, `bbox`, `neighbors` and `expand`; and the same for
`jpiarea`, with its unsupported-location error.

## How the port works

- **geohash `encode`** gets its cell integers through `float.hex()`: for
  `f = latitude/90` the library computes `floor((1 + f) * 2^(L-1))` exactly
  for the cell bit length `L`. The port gets the same integer without big
  ints by writing `f` as `m / 2^k` (doubling until it is integral) and shifting
  `m`, rounding the shift toward minus infinity the way Python's `>>` does.
- **geohash `decode` and `bbox`** rebuild the float with `float.fromhex()`,
  which is `(i - 2^(l-1)) / 2^(l-1)` correctly rounded. An `i64` to `f64`
  conversion rounds to nearest, ties to even, exactly as `fromhex` does, and
  dividing by a power of two is exact.
- **Negative cell numbers**: `neighbors` steps one column west of column 0 and
  relies on Python's `&` and `>>` on negative ints wrapping around the world.
  Those are `i64` two's-complement `&` and arithmetic `>>`.
- **The uint64 forms** interleave two 32-bit integers into a 64-bit one and
  add range widths past `2^64`, so they run in `i128`. `expand_uint64`'s
  `ranges.sort()` (tuples, by start then end) is an insertion sort.
- **`jpgrid` and `jpiarea`** are integer codecs: Python's `%` is floor modulo,
  `int(x / n)` on these non-negative values is truncating division, and
  `int(float)` is truncation. Both libraries' `bbox` label the south-west
  corner's latitude `'w'` and its longitude `'s'`; the port keeps the labels.

A restricted-op prototype used during development replaced `float.hex()`,
`float.fromhex()` and big ints with these algorithms and agreed with the
library on 1,566,058 checks over random coordinates, precisions and codes.

## Scope

- geohash precision is 0 to 24 and codes are at most 24 characters, so every
  cell integer fits in an `i64`. `latitude/90` and `longitude/180` must not be
  subnormal (the library itself fails an assertion on those).
- `jpgrid` and `jpiarea` codes are defined with two-digit latitude and
  longitude fields, which holds for latitude 6.67 to 66.6 and longitude 110 to
  199. Every code the jobs decode is inside that domain. One encode job sits
  on its edge, `iaenc(7.0, 100.0)`, where the library writes a one-digit
  longitude field (`'1004000000'`), and the port writes the same.
- `quadtree.py`, which the package also ships, is not ported.

The vendored modules import only the standard library; `vendor/` is otherwise
left untouched.

## Running

```bash
karac run mini_geohash.kara                   # interpret
karac build --mir mini_geohash.kara           # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out        # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
