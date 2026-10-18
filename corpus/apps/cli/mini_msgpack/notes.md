# mini_msgpack

MessagePack, ported from msgpack-python 1.2.3's reference implementation
(`msgpack/fallback.py`), the serializer under Fluentd, Fluent Bit,
Celery, Salt, RQ and neovim's RPC.

- `Packer`: every int, float, str, bin, array, map and ext format, with
  `use_bin_type` and `use_single_float`, `Timestamp` (32, 64 and 96 bit)
  and `ExtType`, and its `OverflowError` and `ValueError` messages.
- `Unpacker`: one-shot `unpackb` and the streaming feed-and-iterate mode
  with `tell()`. It covers the buffer limits (`max_buffer_size`,
  `max_str_len`, `max_bin_len`, `max_array_len`, `max_map_len`,
  `max_ext_len`), `raw`, `use_list`, `timestamp` and `strict_map_key`, and
  every error it raises: `FormatError`, `ExtraData`, `BufferFull`,
  incomplete input, CPython's UTF-8 decode errors, and Python's dict
  rules for keys (`1`, `1.0` and `True` are one key; a list key is
  unhashable).
- Python's `repr` of every value that comes out.
- A Fluentd forward-protocol decoder (Message, Forward and PackedForward
  modes, with EventTime) over a captured stream.

The run writes `data/json/`, 13 documents to pack (a package.json,
GeoJSON, every integer and float boundary, awkward strings, duplicate
keys, big values). `$bin`, `$ext`, `$ts`, `$tuple` and repeat objects
spell what JSON cannot. It also writes `data/bin/`, 51 MessagePack files,
some made by the real msgpack, some by hand for unusual encodings, and
some broken on purpose. Then it runs the 97 jobs in `data/jobs.tsv`:
- `pack DOC [bin=0] [single=1]` prints the bytes and what they unpack to;
- `unpack FILE [options]` prints the value or the error;
- `stream FILE N [options]` feeds the file N bytes at a time and prints
  each object with `tell()`;
- `forward FILE N` decodes Fluentd forward messages the same way.

`oracle.py` runs the same jobs through the real msgpack's `fallback`
module, whose messages the port follows, and wrote `expected.out`. Wherever
it packs, it also checks that the C extension packs the same bytes.
