# mini_probables

A port of [pyprobables](https://pypi.org/project/pyprobables/) 0.7.0 (MIT; Tyler
Barrus), the probabilistic-data-structures library used for **stream analytics**
and **deduplication** (Bloom filters, Cuckoo filters, Count-Min sketches). This
port is its **Count-Min Sketch**: a `width × depth` table of counters that
estimates how many times each key has been seen, in sublinear space — the
structure behind heavy-hitter detection, approximate frequency counts and
rate tracking over large streams.

## What it does

`mk.py` writes `data/jobs.tsv`: a stream of operations on **one** shared
`CountMinSketch(width=997, depth=5)`, applied in order, each printing one line:

- `bins <key>` — the `depth` bin indices the key maps to (`(hash_i % width) +
  i*width`), space-joined;
- `add <key> <n>` — add `key` `n` times; print the post-add estimate;
- `remove <key> <n>` — remove `key` `n` times; print the post-remove estimate;
- `check <key>` — print the current estimate for `key`;
- `query <min|mean|mean-min>` — switch the estimator; echo `query=<mode>`.

`oracle.py` replays every job against the vendored `pyprobables` library and
writes `expected.out`; `mini_probables.kara` reproduces it byte-for-byte on every
backend. The same deterministic op stream is replayed under each of the three
estimators (the sketch keeps accumulating), so all three query paths are
exercised against the real library.

## How the port works

`mini_probables.kara` follows `countminsketch.py` and `hashes.py` exactly:

- **Hashing.** The library's default is a pure-integer **64-bit FNV-1a**
  (`default_fnv_1a`), and a key's `depth` hashes are FNV-1a under seeds `0 …
  depth-1`. FNV-1a multiplies by a 40-bit prime modulo 2³², so the port does that
  wraparound with **16-bit-limb multiplication** (`wrapmul64`), which keeps every
  intermediate inside `i64` and makes the 2⁶⁴ wrap exact rather than approximate.
  The offset basis `14695981039346656037` exceeds `i64::MAX`, so it is written as
  its signed two's-complement value; `^` is synthesised as `(a|b)-(a&b)`.
- **Indexing.** A hash is an unsigned 64-bit value, so `hash % width` is an
  unsigned modulo; the port computes it with a sign-bit split (`umod`) plus a
  precomputed `2⁶³ mod width`, since a hash with its top bit set is a negative
  `i64`.
- **Counters and estimators.** `add`/`remove` saturate at the `INT32` bounds
  exactly as the library does, and the `min` / `mean` / `mean-min` estimators
  follow `__min_query` / `__mean_query` / `__mean_min_query` over the sorted bin
  values — including Python's floor division (`//`), reproduced as `fdiv` so
  negative sums round the same way.

## Scope

The port implements `CountMinSketch` with an explicit width and depth (the path
that needs no floating-point error-rate sizing), its default FNV-1a hashing, the
full add/remove/check cycle with INT32 saturation, and all three estimators.
Two boundaries are the app's own:

- **ASCII keys.** `default_fnv_1a` hashes `ord()` of each *character* (not the
  UTF-8 bytes), so for non-ASCII keys the byte stream would differ; the port
  keeps keys ASCII, where the two coincide.
- **One structure.** The library also ships Bloom/Cuckoo/Quotient filters and the
  error-rate constructor (which needs `math.ceil`/`math.pow`); those are out of
  scope here. The port exercises only the integer counter-and-hash code, so
  `oracle.py` runs exactly the logic the port mirrors.

The vendored library imports only the standard library, and the Count-Min path
the port covers is pure integer arithmetic.

## Running

```bash
karac run mini_probables.kara                 # interpret
karac build --mir mini_probables.kara         # native binary
python3 -s -P -B mk.py                        # regenerate data/jobs.tsv
python3 -I oracle.py                           # regenerate expected.out
python3 -s -P -B gen_kara.py                   # regenerate the embedded jobs
```
