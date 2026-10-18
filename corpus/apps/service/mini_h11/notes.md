# mini_h11

An HTTP/1.1 protocol engine, ported from h11 0.16.0, the sans-I/O
library under httpx, httpcore and uvicorn. A connection is driven by the
program: it hands over bytes that arrived from the network and gets
events back (`Request`, `InformationalResponse`, `Response`, `Data`,
`EndOfMessage`, `ConnectionClosed`, or `NEED_DATA` and `PAUSED`), and it
hands over events to send and gets the bytes to write. Both sides of the
conversation are tracked, so the engine knows when a body ends, when the
connection can be reused and when it has to be closed.

The run writes 80 scripts to `data/scripts/` and some captured traffic
to `data/captures/`, then runs every script in name order. Each script
line is one call on a connection, printed after `> ` and followed by
what it returned and the two states:
- `role client|server [max N]` starts a connection, with N as the most
  it buffers of an incomplete event;
- `recv BYTES` passes bytes in (with `\r`, `\n`, `\t`, `\\` and `\xHH`)
  and takes events until there are none, `recvfile FILE N` does the
  same with a capture N bytes at a time, and `eof` reports the peer
  closed;
- `next` and `drain` take one event or all of them;
- `request METHOD TARGET VERSION | Name: value ...`,
  `response STATUS VERSION REASON | ...`, `info ...`, `data BYTES`,
  `eom | trailers ...` and `close` send events;
- `cycle` starts the next request/response cycle and `failed` reports
  a failed send.

The scripts cover keep-alive and a curl request a byte at a time, a
browser login POST, a chunked upload behind `Expect: 100-continue`,
three pipelined requests, HTTP/1.0 clients and servers, streamed
responses (h11 rewrites the framing headers for what the peer can read),
`Connection: close`, HEAD, WebSocket and h2c upgrades, CONNECT tunnels,
obsolete line folding, LF-only lines, chunk extensions and trailers,
chunks split across reads, a gzip body from nginx, early responses,
truncated bodies, and 1xx and no-body statuses. The 37 `err-` and `snd-`
scripts are mostly about errors: a TLS handshake on a plain port, malformed request and status
lines, missing or doubled Host, conflicting or bad Content-Length,
unsupported Transfer-Encoding, bad chunk headers and footers, an
oversized header block, data after the end, too much or too little body,
trailers with Content-Length, events in the wrong state, and header
injection. Errors print as h11 raises them, with the status code h11
suggests: `RemoteProtocolError: illegal header line: bytearray(b'Host :
evil') [400]`.

**The port** follows h11's modules: RFC 7230's grammar as h11's regexes
have it (with its errata fix, and its tolerance of control bytes in
values), the receive buffer, the readers and writers for each state and
body framing, the client and server state machines with their
event-triggered and state-triggered transitions, and the Connection
that ties them together. Events print with Python's reprs.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same scripts with the real h11 0.16.0. Run `mk.py` first; it writes
`data/`. `gen_kara.py` prints the Kāra data section (everything after
the last comment line ending in `data` in `mini_h11.kara`) from the
files `mk.py` wrote.
