# mini_protobuf

Protocol Buffers without generated code, the way a gRPC gateway, a
message-queue inspector or a `protoc --decode` debugging session uses
them. It reads `.proto` files at run time and converts messages between
the binary wire format, the text format and JSON, keeping the fields it
does not know.

The run writes 73 files into `data/`, then runs `data/jobs.tsv`, 97 jobs
with tab-separated fields. Each job prints `== ` and its fields joined by
spaces, then its output:
- `encode P M F` reads `F` in text format as message `M` of `P` and
  prints the deterministic binary encoding as a hex dump;
- `text P M F` reads `F` in text format and prints it back;
- `decode P M F` decodes the binary file `F` and prints it in text format;
- `json P M F [names] [defaults] [ints]` decodes `F` and prints it as JSON,
  with keys sorted, optionally with the proto field names, the fields
  that are at their defaults, and enums as numbers;
- `fromjson P M F` reads JSON and prints the message in text format;
- `roundtrip P M F` decodes and re-encodes `F`, and says whether the bytes
  came back the same;
- `raw F` dumps a message it has no schema for, field number by field
  number, as `protoc --decode_raw` does.

Errors print as the library's exception: `ParseError` with the line and
column (and the line itself, where the tokenizer gives it), `DecodeError`,
or `EncodeError` naming the missing required fields.

The four schemas are the protobuf tutorial's address book, a proto3
telemetry schema with every scalar type, packed and unpacked repeated
fields, maps with string and integer keys, a oneof, proto3 `optional`,
`json_name` and reserved ranges, a proto2 order schema with required
fields, defaults, a closed enum and `[packed = true]`, and an empty
message for the raw dumps. The text files use the format's many
spellings: `<>` for braces, `:` before a message, single quotes, adjacent
strings, hex, octal and underscored integers, `inf` and `nan`, short
repeated lists, `#` comments and `;` or `,` separators. There are 21 text
files that fail, 17 JSON files that fail, and 9 corrupt binary files (a
wrong wire type, truncation, an overlong varint, a short length, groups
that do not close or close the wrong group, and bad UTF-8). The other
binary files carry unknown fields before and after the known ones,
non-canonical encodings (unpacked where packed is the default and the
reverse, overlong varints, a split sub-message, a oneof set twice, an
unknown value of a closed enum), and groups and nested messages for the
raw dumps.

**The port** reads `.proto` files with a recursive-descent parser that
resolves type names from the innermost scope out, as `protoc` does. The
wire codec accepts and rejects what upb does: a varint of up to ten bytes,
a field with the wrong wire type kept as unknown, the last value of a
singular field and merged sub-messages, UTF-8 checked only in proto3
strings. The encoder writes fields in number order, unknown fields last,
and map entries in upb's deterministic order (string keys ascending,
except that a key comes before its own prefixes; integer keys descending
by their bits). The text printer and parser follow `text_format.py` with
its tokenizer, so error positions and messages match, and print 32-bit
floats with `ToShortestFloat`, which the port does on the float's exact
decimal expansion. JSON follows `json_format.py` over a port of CPython's
JSON scanner, with its error messages and paths, and the raw dump uses
Python's `_DecodeUnknownFieldSet` to decide whether a length-delimited
field is a nested message or a string.

Not supported: imports, extensions, `Any` and the other well-known types,
groups declared in a schema, services, and options other than `packed`,
`default` and `json_name`.

**Oracle.** `expected.out` is the output of `oracle.py`, which builds
descriptors from the same `.proto` files with a small parser and runs the
jobs with the real protobuf library (`text_format`, `json_format` and the
upb wire codec). Run `mk.py` first; it writes `data/`, with the library
encoding the binary files made from the text files. `gen_kara.py` prints
the Kāra data section (everything after the last comment line ending in
`data` in `mini_protobuf.kara`) from the same files, binary files as hex
and the rest as text.

## Back to the original, 2026-10-18

B-2026-10-18-45 is fixed, so the schema struct is named `Field` again, in both `mini_protobuf.kara` and `mini_protobuf.orig.kara`. It matches expected.out on all four MIR backends (karac at 8d7a5ebf82).
