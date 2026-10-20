# mini_cbor

cbor2 5.6.5, the CBOR library (RFC 8949) under Python's WebAuthn
servers (python-fido2, webauthn), COSE and CWT stacks and IoT gateways.
The port follows its pure-Python decoder, encoder and command-line tool:
- the decoder: every major type, indefinite lengths, float16/32/64,
  simple values, and the semantic tags cbor2 knows (datetime strings,
  epoch times, bignums, decimal fractions, bigfloats, string references
  and namespaces, shared values, rationals, UUIDs, sets, IP addresses,
  MAC addresses and networks, epoch and ISO dates, self-describe), with
  its error messages;
- the encoder with its `canonical`, `value_sharing`,
  `string_referencing`, `datetime_as_timestamp` and `date_as_datetime`
  options;
- `python -m cbor2.tool` with `-i`, `-s`, `-d`, `-k` and `-p`: its tag
  hook, `key_to_str` and the JSON that Python's `json` module writes
  from it, up to the point where `json.dump` fails;
- Python's repr of the decoded values, plus the `Decimal`, `Fraction`,
  `datetime`, `date`, `UUID` and `ipaddress` behaviour cbor2 relies on
  (bigfloats are computed in the default 28-digit context, rounding half
  to even, and dates come from `date.fromisoformat`, ISO weeks included).

`mk.py` writes `data/`. It holds:
- CBOR files as real systems produce them: two WebAuthn attestation
  objects (`none` and `packed`) and an assertion's authenticator data,
  a COSE key and key set, a CWT (tag 61 around a COSE_Sign1), a SenML
  pack with CBOR labels, a CBOR sequence from a device log (one record
  with indefinite lengths), an inventory record that uses most semantic
  tags, the same record with timestamps, a cache's shared values and
  string references, a cycle, keys Python treats as equal, embedded CBOR,
  and bigfloats and decimal fractions;
- twelve broken files a decoder meets in practice;
- `vectors.txt`, the hex payloads from cbor2's own test suite, and
  `examples.cbor.b64`, the base64 file its tool tests read;
- `jobs.txt`, the tool command lines.

The run writes `data/`, then decodes each vector and file, prints the
value, encodes it again with each option, and reads that back; then it
runs each tool job.

`oracle.py` does the same through cbor2 itself (the copy in `cbor2/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the data
section at the end of the program.

The quirks are kept. Some examples:
- The shared-value registry slot stays open while an array's items
  decode, so each item overwrites it: a reference to a shared list can
  come back as the last string decoded inside it (`shared.cbor`, where
  the third `tags` is `"green"`), or as a dict, which is then refused as
  a key (`broken/unhashable.cbor`).
- A shared value that is `None` counts as not initialized.
- `CBORSimpleValue(20)` encodes as `0xf4`, so it reads back as `False`.
- A leap second, an empty bignum and a date like `2026-02-30` stop the
  decoder with Python's own `ValueError`s, not cbor2's.
- Under `-k`, `json.dump` writes `{` before it sorts the keys, so a dict
  with mixed key types prints `{` and then the `TypeError`.
- A `date` is not JSON serializable, while a `datetime` is.
- `-s` stops quietly at a truncated record.

Not ported: the MIME tag (36), and regular expressions beyond keeping
the pattern (tag 35 is not compiled, so an invalid pattern is not
refused). Tag 4 and 5 payloads that Python would unpack from a string,
bytes or a dict, and `Decimal`s built from floats, are refused instead.
Python's `str.isprintable()` is approximated for non-ASCII characters,
and a `frozenset` prints its items sorted rather than in hash order.
None of these occur in the data: the vectors leave out tags 35 and 36,
the one regular expression (in `examples.cbor.b64`) is valid and only
the tool prints it, and no set is encoded without `canonical`.

License: cbor2 is MIT (`LICENSE-cbor2.txt`).
