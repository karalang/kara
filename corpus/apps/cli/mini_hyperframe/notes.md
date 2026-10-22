# mini_hyperframe

The frame layer of `hyperframe` 6.1.0 — the part of the `h2` HTTP/2 stack
that turns a typed frame into the exact bytes that go on the wire and reads
them back. Where the `mini_hpack` app is HTTP/2's header *compression*, this
is HTTP/2's *framing*: the nine-byte header (24-bit length, type, flags,
31-bit stream id) and the body of each of the eleven frame types a client
and server exchange.

The port follows hyperframe's own `frame.py`. It builds and serializes
`DATA`, `HEADERS` (with its padding and priority sub-fields), `PRIORITY`,
`RST_STREAM`, `SETTINGS`, `PUSH_PROMISE`, `PING`, `GOAWAY`, `WINDOW_UPDATE`,
`CONTINUATION` and `ALTSVC`, parses a raw header into the right frame class
(or an `ExtensionFrame` for an unknown type, unless `strict` asks for the
`UnknownFrameError`), and reproduces every validation the library performs:
the stream-association rules (a `DATA` frame must have a non-zero stream, a
`SETTINGS` frame must not), the fixed-length bodies, the padding-too-long and
odd-promised-id checks, and the flow-control window bounds. The output is the
frame's `repr`, so it also reproduces hyperframe's flag set (sorted), its
`settings` dict in insertion order, the `<hex:…>` truncating data repr, and —
for `PING`, `GOAWAY` and `ALTSVC` — Python's own `bytes` repr, with the
single/double-quote selection and `\xNN` escaping.

**Jobs.** `data/jobs.tsv` holds 66 jobs, one per line, tab-separated. A
`build` job constructs a frame from a compact spec, serializes it, and
re-parses the bytes:

- `build <type> <stream_id> <flags> <type-specific fields…>`

and prints three result lines — `hex=` (the serialized bytes), `repr=` (the
frame), and `rt=` (the repr of the bytes parsed back) — or one `err=` line if
construction or serialization raises. A `parse` job reads a 9-byte header plus
body from raw hex:

- `parse <hex> <strict 0|1>`

and prints the parsed frame's repr, or the `err=` message the library raises.
Flags are a comma list or `-`; a hex field of `-` is empty.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of hyperframe in `vendor/hyperframe/` (MIT, in
`LICENSE-hyperframe.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py`
embeds it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps. karac at 502e55cb69.
