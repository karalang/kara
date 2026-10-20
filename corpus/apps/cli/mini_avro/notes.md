# mini_avro

Apache Avro 1.12.0's Python library (`avro`), the reference
implementation of the serialization format Kafka, Hadoop and data
lakes use to store and ship records. The port follows:
- `avro.schema`: parsing a schema from JSON with Avro's checks and
  messages (names and namespaces, duplicate fields and symbols,
  defaults, unions, the logical types `decimal`, `uuid`, `date`,
  `time-*` and `timestamp-*`), `str()` of a schema, its canonical form
  and its CRC-64, MD5 and SHA-256 fingerprints;
- `avro.io`: `validate`, the binary encoder and decoder, every logical
  type, and schema resolution between a writer's and a reader's schema
  (promotions, field defaults, renamed fields, union branches);
- `avro.datafile`: object container files with the `null` and `deflate`
  codecs, read block by block, and written the way `DataFileWriter`
  buffers them;
- `python -m avro`, its `cat` and `write` commands and their options;
- Python's repr and str of the values a datum reads back as.

`mk.py` writes `data/`. It holds:
- schemas as services keep them: a Kafka signup event, an order with
  money as fixed decimals, Hadoop's `Weather`, the spec's `LongList`, a
  metrics sample, Avro's interop and handshake schemas, and versions of
  them evolved for resolution;
- 34 broken schemas a schema registry refuses;
- `vectors.txt`, the schema examples from Avro's own tests;
- datums to encode, one JSON value per line, where a one-key object
  stands for what JSON has no form for: `{"$bytes": hex}`,
  `{"$decimal": text}`, `{"$date": ...}`, `{"$time": ...}`,
  `{"$datetime": ...}` and `{"$naive": ...}`;
- raw encodings, and container files, among them broken ones (a bad
  magic, a snappy codec, a file cut short, a bad sync marker, a block
  count past the data, no schema);
- CSV and JSON rows for `avro write`;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then its output:
- `schema F` prints the schema's JSON, canonical form and fingerprints;
- `vectors` does the same for each line of `vectors.txt`;
- `encode S D` encodes each datum, then decodes it back;
- `resolve W R D` encodes with W and decodes with W as the writer's
  schema and R as the reader's;
- `decode S B` decodes a raw encoding;
- `read F [S]` reads a container file, optionally with a reader schema;
- `write S D` writes a container file and reads it back;
- `cat ...` and `tool-write ...` run `python -m avro cat` and
  `python -m avro write`, printing each line of stdout after `| `, then
  how the command ended. A written file is read back.

`oracle.py` runs the jobs through Avro itself (the copy in `avro/`,
unchanged) and wrote `expected.out`. It fixes Avro's random sync marker
so the output stays the same. An error that names a Python object
prints `<avro.schema.X object>` where Python prints the object's
address, and a carriage return prints as `\r`. `gen_kara.py` writes the
data section.

The quirks are kept. Some examples:
- A `bytes` decimal is written from the digits as given, not rescaled,
  so `Decimal('-3.5')` at scale 2 reads back as `Decimal('-0.35')`.
- A datum that fails part way through writing (a decimal with too many
  places) leaves its bytes in the container's block, so the records
  after it read back wrong: "Requested -28 bytes to read".
- A union is written with the last branch the datum validates against.
- `cat` shares one `DatumReader` across files, so the first file's
  schema is the reader's schema for all of them; `--header` is refused
  outside CSV; `--fields` drops names the record lacks.
- `avro write` guesses the input type from the first suffix, so
  `weather.jsonl` is refused; a refused write leaves an empty file.

Avro is under the Apache License 2.0 (`LICENSE-avro.txt`,
`NOTICE-avro.txt`).
