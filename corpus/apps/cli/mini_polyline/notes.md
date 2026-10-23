# mini_polyline

A port of the [polyline](https://pypi.org/project/polyline/) library 2.0.4
(MIT, Frederick Jansen / Bruno M. Custódio): **Google's Encoded Polyline
Algorithm Format**, the compact ASCII encoding of a sequence of
latitude/longitude points. It is how the Google Maps Directions API, Mapbox,
OSRM and most routing engines ship a route's geometry in a URL or a small
payload instead of a list of floating-point pairs.

## What it does

`mk.py` writes `data/jobs.tsv`: one job per line, a list of integer coordinates
already scaled by 1e5 (latitude and longitude times 100000), as space-separated
`lat,lng` pairs. For each line the job encodes the coordinates and decodes the
result back, printing:

```
enc <the polyline string>
dec <lat,lng ... of the decoded coordinates>     (equals the input)
```

For example the library's own documentation route — `(38.5, -120.2)`,
`(40.7, -120.95)`, `(43.252, -126.453)` — is the scaled job
`3850000,-12020000 4070000,-12095000 4325200,-12645300`, and it encodes to the
well-known string `` _p~iF~ps|U_ulLnnqC_mqNvxq`@ ``.

`oracle.py` replays every line through the vendored `polyline` library and
writes `expected.out`; `mini_polyline.kara` reproduces it byte-for-byte on every
backend.

## How the port works

The encoding is four stages, all integer:

1. **Delta coding.** Each coordinate is stored as its difference from the
   previous one (the first is a difference from zero), so a dense route is a run
   of small numbers.
2. **Zigzag.** A signed delta is folded to an unsigned integer that keeps small
   magnitudes small: `n → 2n` for `n ≥ 0`, `n → -2n-1` for `n < 0` (the
   library's `coord <<= 1; coord = ~coord if coord < 0`).
3. **Base-32 varint.** The unsigned value is emitted five bits at a time,
   least-significant chunk first; every chunk but the last carries a `0x20`
   continuation bit.
4. **Printable offset.** `63` is added to every chunk so the output lands in the
   printable-ASCII range.

Decoding runs the same stages in reverse. Because Kāra has no bitwise operators,
each step is written in integer arithmetic: `x << 1` is `x*2`, `x >> 5` is
`x/32`, `x & 0x1f` is `x%32`, `0x20 | y` (with `y < 32`) is `32+y`, and
two's-complement `~x` is `-x-1`. There is no floating point anywhere in the port.

## Scope

- **Coordinates are integers scaled by 1e5**, exactly what the algorithm encodes
  after its own `_py2_round(coord * 1e5)` step. Feeding the library the
  reconstructed floats `scaled / 1e5` recovers those same integers, so the whole
  pipeline is defined by integer deltas and the port needs no float — which is
  also why the four backends agree by construction. The precision-6 variant
  (OpenStreetMap) and the `geojson` (lon,lat) axis swap are not exercised; both
  are the same integer machinery with a different scale factor / order.

The port was checked against the library on every backend over the job list (a
single point, zero deltas, small and large deltas that cross the 5-bit chunk
boundary, negative coordinates and multi-point routes), and separately during
development over 20,000 random coordinate lists (encode byte-exact, decode exact,
round-trip exact).

The vendored library module imports only the standard library (`io`,
`itertools`, `math`, `typing`); `vendor/` is otherwise left untouched.

## Running

```bash
karac run mini_polyline.kara             # interpret
karac build --mir mini_polyline.kara     # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
