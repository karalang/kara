# service/kv_http

A key-value store served over HTTP/1.1 on a real loopback TCP socket
(`127.0.0.1:47731`), JSON bodies through `#[derive(Serialize, Deserialize)]`.
Server and client share one thread: the client writes a whole batch and shuts
its write half, the kernel buffers it, the server accepts, reads to end of
input, parses every pipelined request by `Content-Length`, answers, and
closes; the client splits the reply stream back into responses. Covers
routing, 201/200/204/400/404/405/411, a truncated head, a short body and a
garbage request line. The Python mirror `kv_http.py` does the same over real
sockets and wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread against docs/design.md and
docs/library/*, then run on the v2 pipeline (karac at c8dad803).

## Edits from source.orig.kara to source.kara

Author errors under the v2 rules (the diagnostics were right):
- `put(.., tags: Vec[String])` stores `tags`: a bare parameter borrows (D5),
  so it is `own Vec[String]`.
- `exchange(.., store, ..)` passes a fresh binding to a `mut ref Store`
  parameter: the call site needs `mut store`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-12-6: `i64.parse` returns `Option`.
- B-2026-10-12-7: `split` is an eager `Vec[String]`, so `.iter()` before
  `map`, and a piece assigned to a `Str` local is refused (it is a
  `ref String`), so `status` became a `String`.
- B-2026-10-14-44: `TcpListener.bind` / `TcpStream.connect` take `own String`
  where docs/library/io.md gives `Str`.

Not worked around: B-2026-10-12-23. `karac check` stops at
`undefined type 'DeserializeError'`, and the corpus mir-* backends run check
first, so the entry FAILS (refused) on every mir backend until that row is
fixed. Without check, `karac __mir-run`, `__mir-llvm-run`, `__mir-llvm-asan`
and `karac build --mir` all print `expected.out` exactly (measured).

The fixed port means two concurrent corpus runs on one host collide; the spec
has no `local_addr` to read back a port-0 bind.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-44 is fixed, so
`TcpListener.bind` and `TcpStream.connect` take the `Str` address and
`status` is a `Str` again. B-2026-10-12-23 is fixed too, so the note
about `DeserializeError` is gone from the header. Each of these is the
original line again, and the app still matches on all four MIR surfaces.
