# mini_hashids

A port of [hashids](https://github.com/davidaurelio/hashids-python) 1.3.1
(MIT, Ivan Akimov & David Aurelio) — the widely-used library for turning
numeric database IDs into short, reversible, URL-safe strings (`347` →
`XVM`) so that sequential ids aren't exposed in URLs.

## What it does

`mk.py` writes `data/jobs.tsv`, one operation per line
(`op \t min_length \t payload`). For the `dec` jobs it uses the vendored
library to produce the hashid to be decoded, so the Kāra port only has to
decode a literal string. `oracle.py` runs every job through
`vendor/hashids.py` to produce `expected.out`. `mini_hashids.kara`
reimplements the algorithm and must match that output byte-for-byte on every
backend.

Covered:

- **`encode`** of one or many non-negative integers, with and without a
  `min_length` (which pads the id up to a floor using guard characters and a
  self-shuffled alphabet).
- **`decode`** back to the original integers, including hashids' own
  validity check — a decoded id is accepted only if re-encoding reproduces
  the input exactly, so a malformed string (one carrying a character outside
  the working alphabet) decodes to nothing (`[]`).

The port follows the reference implementation directly: the consistent
`reorder` shuffle, the per-value base-conversion `hash`, the lottery
character, and separator/guard handling. A fixed salt (`"this is my salt"`)
and the default 62-character alphabet make the constructor's
alphabet/separator/guard derivation deterministic, which is what lets the
oracle comparison be exact.

## Scope

All work is done on byte vectors and i64 integers; the ids a URL carries are
well within i64 range, so no bignum is needed. `encode_hex` / `decode_hex`
(which chunk an arbitrary-length hex string into integers) are out of scope
for the same reason the general base58-of-arbitrary-bytes case would be —
they need arbitrary-precision integers, not part of this port.

## Running

```bash
karac run mini_hashids.kara            # interpret
karac build --mir mini_hashids.kara    # native binary
python3 -s -P -B mk.py                 # regenerate data/jobs.tsv
python3 -I oracle.py                    # regenerate expected.out
```
