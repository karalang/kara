# mini_bson

A port of the `bson` package from
[PyMongo](https://github.com/mongodb/mongo-python-driver) 4.10.1
(Apache-2.0; MongoDB, Inc.): the BSON encoder and decoder, MongoDB Extended
JSON (`json_util.loads`, and `dumps` in canonical, relaxed and legacy mode),
`ObjectId`, `Decimal128`, `DBRef`, `Binary`, `Code`, `Regex`, `Timestamp`
and the decode options for datetimes and UUIDs. The package is vendored
without its C extension, so the oracle runs the pure-Python code. Two
programs run on top: a `bsondump` that prints a dump file as relaxed
Extended JSON, and a `mongoimport`-style loader that turns a JSON Lines file
into a BSON file and reads it back.

```
== json {"d": {"$date": "2020-01-02T03:04:05.250-0800"}}
  repr: {'d': datetime.datetime(2020, 1, 2, 11, 4, 5, 250000)}
  bson: 100000000964008241ed656f01000000
  canonical: {"d": {"$date": {"$numberLong": "1577963045250"}}}
  relaxed: {"d": {"$date": "2020-01-02T11:04:05.250Z"}}
  legacy: {"d": {"$date": 1577963045250}}
== hex 0f00000002730003000000e2820000
  decode: InvalidBSON: 'utf-8' codec can't decode bytes in position 0-1: unexpected end of data
  valid: False
== dec 1E+6120
  bid: 00ca9a3b00000000000000000000fe5f
  str: 1.000000000E+6120
```

## What it does

`mk.py` writes `data/jobs.txt` (372 jobs) and six files listed in
`data/files.txt`: five BSON dumps (people, orders, a truncated one, one with
an unknown element type, an empty one) and `import/events.jsonl`.
`oracle.py` runs each job on the vendored package and prints the result;
`mini_bson.kara` runs the same jobs.

- **`json`** (173 jobs) loads Extended JSON, prints the `repr`, encodes to
  BSON, decodes it again and dumps it in all three modes. The jobs cover
  int32/int64 boundaries and overflow, floats (`-0.0`, NaN, infinities,
  subnormals), strings with escapes and non-printable characters, every
  `$`-wrapper the loader knows (`$oid`, `$date` in ISO form with offsets
  and fractions and as numbers, `$numberInt`, `$numberLong`,
  `$numberDouble`, `$numberDecimal`, `$binary` in both forms, `$uuid`,
  `$code` and `$scope`, `$regularExpression` and `$regex`, `$timestamp`,
  `$minKey`, `$maxKey`, `$undefined`, `$symbol`, `$ref`, `$dbPointer`) and
  each one's error for a wrong type or an extra field, plus documents that
  are not mappings.
- **`keys`** encodes with `check_keys=True`.
- **`hex`** (90 jobs) decodes raw BSON: each element type, then malformed
  input that reaches every error in the decoder (lengths, `eoo` bytes,
  strings, invalid UTF-8 at each kind of position, binary lengths, arrays,
  code with scope, out-of-range dates, unknown types, short reads).
- **`opts`** decodes with `datetime_conversion` set to auto, clamp or ms,
  and with each `uuid_representation`.
- **`all`** runs `decode_all` and `decode_iter` over concatenated
  documents, some of them broken.
- **`oid`**, **`oidtime`**, **`dec`** and **`bid`** call `ObjectId`,
  `ObjectId.from_datetime`, `Decimal128(str)` and `Decimal128.from_bid`
  directly.

## How the port works

- **Values.** A Python value is a `Val`, a recursive enum with one case per
  type the package hands back; a `dict` is a Vec of (key, value) pairs,
  since Python's `dict` keeps insertion order. Exceptions are an `Exc`
  holding the class name and `str(exception)`, carried in `Result`.
- **The decoder** follows `bson/__init__.py` function for function,
  including how it indexes `bytes`: `data[i]` raises `IndexError` and wraps
  for a negative index, slices clamp, `data.index` raises "subsection not
  found", and `struct.unpack_from` gives CPython's bounds message. Every
  exception other than `InvalidBSON` is rewrapped as `InvalidBSON`, except
  the size read in `decode_iter`, which sits outside the handler and
  escapes as a bare `struct.error`.
- **UTF-8** is decoded by hand with CPython's strict rules, so a bad string
  fails with the same message: invalid start byte, invalid continuation
  byte over one to three bytes, or unexpected end of data over the rest.
- **Extended JSON.** The JSON parser calls the same `object_hook`: the
  first key that names a wrapper picks the parser. The error text that
  embeds a document uses Python's `repr` of it, so the port has `repr` for
  every type, `str` quoting and escaping rules included, and the
  `SON([...])` form the `$dbPointer` errors print.
- **Python's built-ins** that the package leans on are emulated where the
  jobs reach them: `int(s, base)` (whitespace, signs, `0x`, underscores),
  `float(s)`, non-strict `base64.b64decode` (characters outside the
  alphabet are skipped), `uuid.UUID(hex)`, `bytes.fromhex`, and
  `datetime.strptime` for `%Y-%m-%dT%H:%M:%S` as the backtracking regex
  `_strptime` builds, with `datetime`'s and `timedelta`'s range errors.
- **Datetimes** are microseconds since the epoch, with civil-date
  arithmetic for `repr`, ISO strings and the year 1 to 9999 range.
- **Decimal128** parses and prints as `decimal` does in the package's
  context (34 digits, exponents -6176 to 6111, clamping, and traps on
  Inexact and Overflow), and packs the BID layout the same way, including
  the two-high-bits form `from_bid` reads.

## Compiler gaps

- `c as i128` on a `char` is refused with a bare "cannot cast 'char' to
  'i128'", although `c as i64` is accepted and the rule in the typechecker
  admits any integer of 32 bits or more: its width table has no 128-bit
  entries. Filed as B-2026-10-24-7. The port writes `c as u32 as i128`.

The rest were my mistakes, and the diagnostics caught them. `karac fix`
applied 207 machine fixes: 203 turned `&&`, `||` and `!` into `and`, `or`
and `not`, one respelled `[Str; 18]` as `Array[Str, 18]`, and one moved a
`mut` out of a tuple pattern. By hand: a `mut` binding inside a `Some(..)`
pattern, nine parameters that had to be `own` because the function keeps
the value (a bare parameter borrows), and `trim_start_matches` and
`trim_matches`, which Kāra's `String` does not have. After that the interpreter's output differed from the oracle on one
line: I had made NaN as `inf - inf`, which on x86-64 has the sign bit set,
and the BSON bytes showed it. Python's NaN is `0x7ff8000000000000`. With
that fixed, the interpreter, the LLVM backend, the ASan build and the
native binary all matched the oracle byte for byte.

## Scope

- Only what the jobs reach of the Python built-ins is emulated.
  `str.isprintable` treats unassigned code points as printable, Python's
  arbitrary-size ints saturate past 10^38, and JSON parse errors carry
  `json`'s messages only roughly. No job depends on any of these.
- Not ported: `RawBSONDocument`, the type registry and custom codecs,
  timezone-aware decoding (`tz_aware`, `tzinfo`), `SON` as a document
  class, `ObjectId()` generation (it reads the clock and `os.urandom`),
  `decode_file_iter`, and `json_util`'s size helpers.

## Running

```bash
karac run mini_bson.kara                  # interpret
karac build --mir mini_bson.kara          # native binary
python3 -s -P -B mk.py                    # regenerate data/
python3 -I -B oracle.py > expected.out    # regenerate expected.out
python3 -I -B gen_kara.py                 # regenerate the embedded data
```
