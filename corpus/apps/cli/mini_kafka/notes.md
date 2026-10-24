# mini_kafka

A port of the wire layer of [kafka-python](https://github.com/dpkp/kafka-python)
2.2.15 (Apache-2.0; Dana Powers, David Arthur and contributors), the
pure-Python Kafka client. It covers the record formats a broker stores and
serves (v0 and v1 message sets, v2 record batches, gzip-compressed or not),
the batch builders the producer appends to, the producer's murmur2
partitioner, the broker error table, and `KafkaProtocol`, which frames
requests and reassembles responses over the request and response schemas
kafka-python declares. The vendored copy is trimmed to the modules the
oracle imports; it loads without the package `__init__` (which pulls in the
client, consumer and producer) and without six, and the optional codec
libraries are blocked, so snappy, lz4 and zstd report themselves missing as
they do on a plain install.

```
> read v2_control.bin
  ControlRecord(offset=80, timestamp=1761300000500, timestamp_type=0, version=0, type=1 <commit>) size=16
> read v2_gzip_badcrc.bin
    error: BadGzipFile: CRC check failed 0xad2eb3ac != 0xad2eb3ad
> partition 12
  b'user-17' murmur2=1149718380 partition=0
> session client_id=c2 api_version=~
    error: CorrelationIdError: Correlation IDs do not match: sent 1, recv 9
```

## What it does

`mk.py` builds 54 binary inputs with the vendored library itself, patching
the broken ones byte by byte, and writes `data/jobs.txt`, 80 jobs.
`oracle.py` runs each job on the vendored kafka-python; `mini_kafka.kara`
runs the same jobs on the same files, which it writes to `data/` first.

- **`read`** (41 jobs) walks a record set with `MemoryRecords` and prints
  each batch, its CRC check and its records. The sets: v2 batches plain,
  idempotent (a sequence that wraps past 2^31-1), transactional, three in a
  row, with log-append time, gzip-compressed, gzip in two members whose
  headers carry FEXTRA, FNAME, FCOMMENT and FHCRC with zero padding after,
  a control batch with commit and abort markers; v0 and v1 message sets
  plain and gzip-wrapped, with broker offsets and log-append time; a set
  that mixes all three formats and ends in a cut batch; and the broken
  ones: a gzip payload with a bad magic, a bad CRC, a bad length, cut
  short, with junk after it, cut inside its header; a v2 batch with a bad
  CRC, a record count too low and too high, a record whose length is off,
  a negative header count and header key size, a ten-byte varint, the
  snappy, lz4, zstd and an unknown codec, a header cut short, a batch
  under the minimum size; a v1 message with a bad CRC, a compressed
  wrapper with no value, a double-compressed inner message, an inner set
  cut short, and snappy and lz4 wrappers.
- **`build`** (20) appends a record list through `MemoryRecordsBuilder`
  and prints each append's metadata and size estimate, the builder's
  size, whether it is full, the built bytes' CRC-32 and first 64 bytes,
  and the batch read back. They cover v2 with and without a producer id,
  transactional, a batch size that refuses records, timestamps far from
  the first (deltas that need six-byte varints), v0 and v1, headers on v1,
  the codecs that are missing, and each of the builder's assertions.
- **`partition`** (3) hashes keys with `murmur2` and picks partitions the
  way `DefaultPartitioner` does for keyed records.
- **`errors`** (1) maps broker error codes to their classes through
  `for_code`, including codes it does not know.
- **`session`** (15) drives one `KafkaProtocol`: `send` builds a request
  (ApiVersions, Metadata, Produce, Fetch, FindCoordinator, any version)
  from a Python literal, `flush` prints the framed bytes, `recv` feeds a
  response stream in chunks of a given size and prints each response's
  `repr`, and `consume` reads the record sets inside a fetch response. The
  streams include responses whose correlation id is wrong, a response with
  nothing in flight, a frame too short for its schema, the 0.8.2
  FindCoordinator quirk, a host name that is not UTF-8, a varint that runs
  too long, an ApiVersions v3 answer in the v0 format, and requests that
  fail to encode.

## How the port works

- **Record batches** follow `default_records.py` and `legacy_records.py`:
  the 61-byte v2 header, CRC-32C over everything after the attributes,
  zigzag varints (`record/util.py`, including "Out of int64 range"), lazy
  decompression on first iteration, the record and header layout and every
  `CorruptRecordError` the reader raises, with `ValueError` and
  `IndexError` wrapped as "Found invalid record structure" and the rest
  passing through; for v0 and v1, the message headers, CRC-32 over magic
  onwards, the inner message set of a compressed wrapper with relative
  offsets made absolute (v1) or kept (v0), and the wrapper's timestamp
  under log-append time. Struct errors carry `unpack_from`'s own message.
- **Gzip** is `gzip.GzipFile` reading as Python 3.13 does: members one
  after another, the optional header fields, zero padding, and its
  `BadGzipFile` and `EOFError` messages, over the raw-deflate decoder of
  the mini_png port.
- **The builders** write the same bytes: the v2 record encoding and the
  batch-size check that always lets the first record in, the header and
  its CRC written at `build()`, and the legacy messages with their CRCs.
- **The protocol** reads each class's `SCHEMA`, which `gen_kara.py` writes
  out from the vendored classes, and encodes, decodes and prints values the
  way `types.py` does: `String` takes `str()` of any value, `Bytes.repr`
  cuts at 100 bytes, `Array.repr` prints `NULL` for None, `TaggedFields`
  encodes a tag and its value but not the value's size, and a field count
  that does not match is "Item field count does not match Schema".
  `KafkaProtocol` keeps its correlation ids, its in-flight queue and its
  4-byte header buffer as `parser.py` does. After an error the header
  buffer is not reset, so the next `receive_bytes` reads the old length
  again; the sessions that continue after an error show it.
- **The schemas are kafka-python's**, mistakes included: in
  `ProduceResponse_v8` the record errors and error message sit in a tuple
  that `Schema`'s `zip()` cuts short, so the response has neither.

## Compiler gaps

None found. `karac check` stopped first on a struct I had named `KB`,
which is not PascalCase (E0001), and I renamed it. Then it raised one
error, a `let p = c.proto;` that moved a field out of a borrowed struct,
which `karac fix` turned into a borrow. With those, the MIR interpreter,
the LLVM backend, the ASan build and the native binary all matched the
oracle byte for byte on the first run, with nothing on stderr.

## Scope

- Compression on the write side is not ported: gzip would need zlib's own
  compressor to match the bytes, and the other codecs are missing on the
  oracle's side too. No job builds a compressed batch that would succeed.
- A `UnicodeDecodeError` (a header key or protocol string that is not
  UTF-8) prints a shorter message than Python's; the one job that meets
  one only prints the `KafkaProtocolError` it turns into.
- A varint whose tenth byte ends it with a value above 1 decodes to a
  number wider than 64 bits in Python; the port keeps 64. No input has one.
- Not ported: the client, consumer, producer and admin layers, SASL, the
  snappy, lz4 and zstd codecs, and the protocol versions of the other APIs.

## Running

```bash
karac run mini_kafka.kara                    # interpret
karac build --mir mini_kafka.kara            # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
