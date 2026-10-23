# mini_temporenc

A port of the [temporenc](https://pypi.org/project/temporenc/) library 0.1.0
(3-clause BSD, Wouter Bolsterlee): the **temporenc binary format** for dates and
times ([temporenc.org](https://temporenc.org/)). temporenc is a compact,
self-describing byte encoding that packs a date, a time, a datetime, optional
sub-second precision and an optional time zone into as few as three bytes — and,
unlike most datetime encodings, it stores *partial* values directly, so "October
2026" or "13:05 in some unknown year" round-trip without a sentinel.

## What it does

`mk.py` writes `data/jobs.tsv`: one pack/unpack job per line, as the
space-separated fields

```
TYPE year month day hour minute second subkind subval tz
```

`TYPE` is one of `D` `T` `DT` `DTZ` `DTS` `DTSZ`; a missing
year/month/day/hour/minute/second is `-1`; `subkind` is `n` (none), `ms`, `us`
or `ns` with its value in `subval`; and `tz` is `-9999` for "no time zone" or an
offset in minutes from UTC. For each line the job encodes the fields into a
temporenc byte string of the named type and decodes that byte string back,
printing:

```
pack <lowercase hex of the packed bytes>
unpack y=<v> mo=<v> d=<v> h=<v> mi=<v> s=<v> ns=<v> tz=<v>
```

where each `<v>` is a decoded component or `-` when that component is absent.
The sub-second value is reported as the canonical nanosecond count (temporenc
stores millisecond and microsecond precisions as truncated nanoseconds).

`oracle.py` replays every line through the vendored `temporenc` library and
writes `expected.out`; `mini_temporenc.kara` reproduces it byte-for-byte on
every backend.

## How the port works

temporenc is a **bit-level** format. The first byte's high bits are a tag that
names the type (and, for the sub-second types, a 2-bit precision selector); the
remaining bits carry the fields in a fixed order, zero-padded to a byte
boundary:

| type | bytes | layout |
|---|---|---|
| `D` | 3 | tag `100`, year(12) month(4) day(5) |
| `T` | 3 | tag `1010000`, hour(5) minute(6) second(6) |
| `DT` | 5 | tag `00`, date(21) time(17) |
| `DTZ` | 6 | tag `110`, date(21) time(17) tz(7) |
| `DTS` | 6–9 | tag `01`, precision(2), date(21) time(17) sub-second(0/10/20/30) |
| `DTSZ` | 7–10 | tag `111`, precision(2), date(21) time(17) sub-second tz(7) |

Each field has a fixed width; a component that is absent is stored as its
all-ones "empty" sentinel (`4095` for year, `15` for month, `31` for day, and so
on), and month and day are stored zero-based. A time-zone offset is stored as
`offset / 15 + 64` in seven bits.

Because Kāra has no bitwise operators, the whole codec runs over a `Vec` of 0/1
bits rather than over shifts and masks: a field of width `n` contributes its `n`
bits most-significant-first (`value / pow2(k) % 2`), the bit vector is packed
eight bits to a byte with the final partial byte zero-padded, and decoding reads
the same widths back in order. Representing the stream as a bit vector instead of
one integer also sidesteps the format's widest value — a `DTSZ` with nanosecond
precision is 80 bits, wider than a 64-bit integer — so the port needs no
multi-word arithmetic. There is no floating point anywhere.

## Scope

- The job always names a **type** and passes **explicit components**, which
  exercises every temporenc type and every sub-second precision directly. The
  library's two convenience layers over this core — automatic type selection
  (`type=None`, a cascade of "which components are present" checks) and building
  a value straight from a `datetime.datetime`/`date`/`time` object — are not
  exercised; they choose *which* fields to pack, not *how* the bytes are formed,
  which is the algorithm this port reproduces.
- Time-zone offsets are the multiples of 15 minutes the 7-bit field can hold
  (`-960` to `930`), which is temporenc's own supported range.
- The `Moment → datetime` conversion helpers (and their leap-second / partial
  substitution rules) are a thin stdlib adapter on top of the decoded
  components and are left to the library; the port decodes the raw components,
  which is what `expected.out` compares.

The port was checked against the library on every backend over the job list, and
separately during development over 1,252 cross-checks spanning all six types,
all four precisions, partial and empty date/time components, leap seconds and the
time-zone range boundaries (pack byte-exact, unpack exact, round-trip exact).

The vendored library module imports only the standard library (`datetime`,
`struct`, `sys`); `vendor/` is otherwise left untouched.

## Running

```bash
karac run mini_temporenc.kara             # interpret
karac build --mir mini_temporenc.kara     # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
