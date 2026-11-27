# service/yamux_server

A yamux stream multiplexer on a loopback TCP port, with the scripted clients
that drive it. yamux is HashiCorp's protocol (its `spec.md`), the one under
Consul, Nomad, Vault's plugins and libp2p. Many streams share one
connection, and each stream's receive window holds the sender back.

- **Frames.** A frame is a 12-byte header (version 0, type, flags, stream
  id, length, big-endian), then the bytes for a data frame. The types are
  data, window update, ping and go away. The flags are SYN, ACK, FIN and RST.
  For a window update the length is the window delta. For a ping it is an
  opaque value, and for a go away an error code (0 normal, 1 protocol error).
- **Streams.** Client streams have odd ids and server streams even ones.
  - A stream opens with SYN. The SYN can ride on the first data frame, or
    come on a window update that offers more than the 256 KiB window. The
    other side answers with ACK.
  - A FIN each way closes a stream, and RST closes it at once.
  - A frame for a stream that is gone is dropped (a data frame's body is
    read and thrown away) and counted.
- **Flow control.** Each stream starts with a 256 KiB window each way.
  - A sender writes no more than its window, in frames of at most 16384
    bytes (server) or 10000 bytes (client).
  - When the window runs out, the sender stalls until a window update
    comes, and each stall is counted.
  - A receiver hands bytes back once half its window is used, as yamux
    does.
- **The engine.** The server and the clients run the same session engine.
  - It writes every frame the windows allow: control frames first, then one
    chunk per stream a round, in stream order.
  - It reads one frame only when nothing is left to write.
  - So each session's trace depends only on the order of the frames, not
    on timing, and three sessions run at once with the same output on every
    run.
- **Services.** The first line of a stream names its service:
  - `hello <name>` names the session, and the server pings the client.
  - `echo` echoes until FIN.
  - `cat <n>` streams n bytes. One client opens it with a doubled window;
    another resets it after the first frame.
  - `put` stores until FIN and answers with the length and an FNV-1a hash.
  - `push <n>` opens a server stream carrying n notes.
  - `stats` gives the server-wide counts, which the sessions share behind a
    `Mutex`.
  - Anything else is reset.
- **Protocol errors.** Each of these gets a go away with code 1 and a close:
  - version 1;
  - a data frame announcing more than the window;
  - a client stream with an even id;
  - a stream opened twice;
  - frame type 7.

  A ping followed by a half close is answered, and the session ends without
  a go away.
- **The output.** Each client prints its log, then the admin's `stats`, the
  server's log per session, and the totals.

The mirror `yamux_server.py` is the same server and clients, written with
`socket` and `threading`. It wrote `expected.out`, and a rustc build of the
draft printed the same bytes.

Written 2026-11-27 by the real-world apps thread, with explicit std imports.
karac carries the fix below.

## From yamux_server.orig.kara to yamux_server.kara

The draft is plain Rust (rustc builds it):

- `std::thread::spawn` for the server, each session and each client;
- `Arc<Mutex<Shared>>`;
- `BTreeMap` and `VecDeque`;
- a `trait App` driven by a generic `run<A: App>`;
- `std::mem::take`;
- `shutdown(Shutdown::Write)`;
- `wrapping_mul`.

One gap turned up, and it was fixed in the compiler before the fix passes
were rerun:

- B-2026-11-27-60: `tasks.push(thread::spawn(move || serve(conn, shared)))`
  inside the accept loop got no fix, since a spawn's group was only made
  for a statement of the function's own body. The group is now made before
  the statement of the body that holds the spawn, so it outlives the loop.
  Later spawns in the same body share that group, instead of each opening
  one.

After that, `karac fix` rewrote the draft in one run of passes with no hand
edits. The first run matched the mirror byte for byte on every backend.
