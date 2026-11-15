# service/coap_server

A CoAP server (RFC 7252, the web's request/response model for constrained
devices, with block-wise transfer from RFC 7959) on loopback UDP, and the
clients that use it. It is a binary protocol over datagrams: a 4-byte header
(version, type, token length, code, message id), a token, options as
delta-encoded numbers with 4-bit nibbles and their extended forms, and a
payload after a `0xFF` marker.

- **The server.** One task answers each datagram in turn from a tree of
  resources (`/hello`, `/sensors/temp`, a 280-byte `/big`). It answers a
  confirmable request with a piggybacked ACK and a non-confirmable one with
  a NON of its own message id, replies to an empty CON (a ping) with RST,
  and keeps a cache by peer and message id so a retransmitted request gets
  the same answer without running twice. It serves `/.well-known/core` in
  link format, splits `/big` into 64-byte Block2 blocks (and smaller ones
  when the client asks), refuses an unknown critical option with 4.02,
  takes PUT with a Content-Format it knows, refuses a Size1 over 64 with
  4.13, appends to `/log`, deletes, and stops on `POST /halt`. Malformed
  datagrams (bad version, a token longer than 8, an option past the end,
  a marker with no payload) are dropped and logged; a stray ACK and a
  response sent to the server are logged too.
- **The clients.** Each sends hand-built messages and prints the reply's
  type, code, message id, token, options and payload: a ping, discovery,
  GETs (CON and NON, a missing path, a wrong method), a block-wise walk of
  `/big` reassembled to its last line, notes PUT and read back with
  formats, a duplicated `/log` POST, option edge cases, broken datagrams
  (waiting `QUIET_MS` for no reply), cleanup, and the admin's halt. The
  server's log, the log resource and the counts print last, sorted.

The mirror `coap_server.py` is the same server and clients written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-15 by the real-world apps thread; karac at 4dbfebec42,
which carries the fixes below.

## From coap_server.orig.kara to coap_server.kara

The draft was written Rust-shaped (`use` and `::` paths, `&str`, `&[u8]`,
`&mut`, `vec!`, `format!`, `b"..."`, `#[derive]`, `UdpSocket::bind`,
`thread::spawn(move || ..)`, `handle.join().unwrap()`,
`set_read_timeout(Some(Duration::from_millis(ms)))`,
`.collect::<Vec<_>>().join("")`, iterator chains returning
`Vec<&Vec<u8>>`). Six gaps turned up. Five were fixed in the compiler
before the fix passes were rerun:

- B-2026-11-15-150: `server.join().unwrap()` said only "no method
  'unwrap'". A `TaskHandle`'s `join` returns the task's result itself, and
  the fix now drops the `.unwrap()`.
- B-2026-11-15-151: `it.collect::<Vec<_>>().join("")` had no fix. It now
  becomes `Vec.from_iter(it).join("")`.
- B-2026-11-15-152: `use std::thread;` for a spawn became an import of a
  module that does not exist. The line is now deleted (a `TaskGroup` needs
  no import).
- B-2026-11-15-153: the timeout mended to `Some(Duration.ms(ms))`, then
  "expected 'i64', found 'Duration'" with no fix. Kāra's
  `set_read_timeout` takes milliseconds, and the fix now writes `Some(ms)`.
- B-2026-11-15-154: `fn option(self, num: u16) -> Vec[ref Vec[u8]]`, a
  `filter(|(n, _)| *n == num).map(..).collect()`, was refused as returning
  a borrow of `num`. A predicate returns `bool`, so the collected views
  cannot hold its capture.

The sixth is open: with the timeout mended, `Some(wait_ms)` with
`wait_ms: u64` passes an `Option[i64]` slot with no `as`, where a bare
`i64` slot refuses it, and `u64.MAX` would become -1 (B-2026-11-15-155).
The checker keeps a variable payload permissive on purpose, so the
program is left as `karac fix` wrote it; its timeouts are 300 ms.

After those, `karac fix` rewrote the draft over five passes, and one
diagnostic with no machine fix was answered by hand, as it says:

- `thread::spawn(|| serve(Server.new(), sock))` (B-2026-11-07-41: the
  rewrite needs a group): `let group = TaskGroup.new();` and
  `group.spawn(|| ..)`.

`struct Server` keeps its name, so `karac check` warns that it shadows the
prelude's HTTP `Server`, which the warning says is legal, and the clients'
discarded `c.ask(..)` results are `must_use` warnings.

The first run after that matched the mirror byte for byte.
