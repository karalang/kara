# mini_wsproto

The WebSocket frame protocol of `wsproto` 1.3.2 (`frame_protocol.py`) — the
sans-I/O core that `hypercorn` and `uvicorn` drive to speak RFC 6455. It is
the fourth sans-I/O port here, after `mini_h11` (HTTP/1.1) and the HTTP/2
pieces `mini_hpack`, `mini_hyperframe` and `mini_priority`.

The port follows wsproto's own module. On the decode side it runs the
`FrameDecoder` / `MessageDecoder` / `FrameProtocol` pipeline: parse the
two-byte frame header with its FIN/RSV/opcode and the one/two/eight-byte
payload length, reject a control frame that is fragmented or over 125 bytes,
a 2- or 8-byte length that a shorter form would have carried, a reserved bit
with no extension, and a frame masked the wrong way for its role; unmask with
`XorMaskerSimple`, whose key **rotates by `len % 4` after each chunk** so a
single frame split across several `receive_bytes` calls unmasks correctly;
reassemble a fragmented message, validating a text message's UTF-8
**incrementally** (a codepoint split across two frames is buffered until the
next frame completes it); and parse a CLOSE frame's status code, enforcing the
code ranges, the local-only codes a peer must not send, and the UTF-8 of its
reason. On the serialize side it builds DATA (text or binary, with
continuation for `fin=0`), PING, PONG and CLOSE frames, truncating a close
reason to 123 bytes on a codepoint boundary.

Every `ParseFailed` is reproduced with wsproto's exact message and close code,
and a UTF-8 failure carries CPython's own `UnicodeDecodeError` text (byte,
position and reason), shared with the `mini_codecs` port.

**Masking and determinism.** A client masks each frame with a 32-bit key
"chosen at random"; there is nothing to reproduce byte-for-byte in a random
key, so `os.urandom` is pinned to one fixed key (`37fa1c8e`) in both the
oracle and the port, and the README says so. The decode path still exercises
`XorMaskerSimple` with real multi-chunk key rotation against masked client
frames, so the masker is covered in both directions.

**Jobs.** `data/jobs.tsv` holds 53 jobs, one per line, tab-separated. Field 0
names the kind:

- `recv <role> <hexchunk> [<hexchunk> ...]` — make a FrameProtocol for that
  role (`client`/`server`), feed each hex chunk with its own `receive_bytes`
  call (so a frame split across reads exercises the buffer and the masker's
  rotation), and drain `received_frames()`. Prints one line per frame
  (`op=… ff=… mf=…` with `text=`/`bin=`/`code= reason=`), or `err=<msg>
  code=<n>` when the protocol raises.
- `send <role> <op>|<op>|…` — make a FrameProtocol and run each op on it in
  turn (ops share the protocol, so a `data` op with `fin=0` opens a message
  the next op continues). Each op prints `bytes=<hex>`. An op is
  space-separated: `data <t|b> <0|1> <payloadhex>`, `ping <payloadhex>`,
  `pong <payloadhex>`, `close <code|-> <reasonhex|->`.

The jobs cover single and fragmented text and binary, interleaved control
frames, a frame split three ways across reads, text split mid-codepoint, all
five close-code outcomes, and every error path (bad opcode, wrong-way mask,
over-long control frame, invalid/local-only/reserved/one-byte close codes,
invalid UTF-8 in a text frame and in a close reason, a fragmented control
frame, an orphan continuation, a non-minimal 2- and 8-byte length, a reserved
bit), plus server and client serialization of every frame kind including a
two-byte-length binary and a truncated close reason.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `wsproto` in `vendor/wsproto/` (MIT, in
`LICENSE-wsproto.txt`). `mk.py` writes `data/jobs.tsv` (building each valid
frame with the library so the hex is correct); `gen_kara.py` embeds it into
the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with
no new compiler gaps. karac at 502e55cb69.
