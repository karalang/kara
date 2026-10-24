# mini_h2

h2 4.2.0, the sans-I/O HTTP/2 protocol stack of Python: httpx,
hypercorn, twisted and grpclib speak HTTP/2 through it. h2 owns no
socket. An application feeds it the bytes it read and gets events back,
calls methods for what it wants to send, and writes out the bytes h2
queued. The port sits on the ports of h2's two dependencies,
[mini_hpack](../mini_hpack) (header compression) and
[mini_hyperframe](../mini_hyperframe) (frames). It covers:
- `H2Connection` with its connection state machine, stream id
  allocation, the concurrency limits, and the pruning of closed
  streams;
- `H2Stream` with its stream state machine (the transition table is
  generated from h2's own), content-length tracking, and the
  `StreamClosedBy` record that decides whether a frame on a closed
  stream is answered with RST_STREAM or ends the connection;
- flow control: the connection and stream windows, `WindowManager`'s
  rule for when received data is acknowledged with WINDOW_UPDATE, and
  window changes from SETTINGS;
- SETTINGS: the local and remote settings, values that wait for an
  ACK, validation, and `RemoteSettingsChanged` / `SettingsAcknowledged`;
- `FrameBuffer`: the server's preamble check, frame size limits,
  HEADERS / PUSH_PROMISE + CONTINUATION reassembly, and the errors of a
  bad frame;
- outbound normalisation (lower-case names, stripped whitespace,
  dropped connection headers, cookie splitting, never-indexed
  credentials and short cookies) and the inbound and outbound header
  validation;
- server push, priority (sent and received, alone and inside HEADERS),
  ALTSVC, PING, RST_STREAM, GOAWAY and the h2c upgrade;
- every event and exception, with the text h2 gives it.

## What it does

The run writes `data/jobs.txt` and plays its 75 conversations. In each
one, endpoints are created (`c new client`, `s new server`) and driven
one command at a time: send headers or data, open a window, push,
ping, reset, go away, change settings, and `pipe c s`, which takes the
bytes one endpoint queued, prints them frame by frame and feeds them to
the other, which prints the events it raised. A `raw` command feeds an
endpoint hand-built frames from a misbehaving peer. Errors print as
`error <class>: <message>`.

The conversations: GET and POST exchanges, out-of-order responses, str
headers, credentials and cookies, trailers, 100-continue, HEAD and
content-length; filling and reopening windows; SETTINGS that change the
window and frame size, invalid SETTINGS, a concurrency limit; push,
priority, ALTSVC, ping, resets in both directions, GOAWAY; the calls
the state machines refuse; outbound validation and sixteen inbound
validation failures; the h2c upgrade; a bad preamble, a preamble in
pieces, CONTINUATION frames, interrupted header blocks, oversized and
badly padded frames, unknown frames and settings, stream ids out of
order, frames on closed and reset streams, bad window updates, a broken
header block, a header list over the limit; and delivery a few bytes at
a time.

`oracle.py` plays the same conversations through h2 itself (the copy
in `vendor/`, with hpack and hyperframe, unchanged) and wrote
`expected.out`. `mk.py` writes the conversations and `gen_kara.py` the
data section at the end of the program.

## How the port works

h2's header validation is a chain of generators, and the encoder pulls
headers through it one at a time. So a header that fails a check
stops the chain after the headers before it went all the way through,
and into the encoder's dynamic table: the frame is never sent, but the
encoder remembers headers the peer's decoder never saw. The port runs
the same stages one header at a time and encodes what came out of the
end before raising the error. The "outbound header validation"
conversation shows the consequence h2 has: the next request that is
sent refers to table entries the server does not have, and the
server's decoder fails with `Invalid table index 63`.

Some of the other quirks the port keeps:
- `name in b':method'` and `name in b'cookie'` are substring tests, so
  a header named `c` with a short value is sent never-indexed.
- `content-length` is read with Python's `int()`, so `+0_2` is 2 and
  `-0` is 0.
- `RemoteSettingsChanged` takes each original value from the settings
  object after the update, which still answers with the acknowledged
  value.
- A SETTINGS update that fails validation keeps the values before the
  bad one.
- The h2c upgrade builds the SETTINGS ACK for the client's settings and
  drops it.
- Reading `open_outbound_streams` or `open_inbound_streams` forgets
  every closed stream, which changes what a later frame on one of them
  gets.
- A frame that fails to parse stays in the buffer.

## Compiler gaps

One, filed as B-2026-10-24-19. A helper that wraps its argument in a
vector, `fn evs1(e: Ev) -> Vec[Ev] { ...; v.push(e); v }`, forgot the
`own` on its parameter (a bare parameter borrows). The diagnostic was
right, but it is the generic borrow mismatch with the machine fix
`.clone()`, a deep copy of every event. A `return e` in the same place
gets the "cannot give away parameter" message and the fix `own` on the
parameter, which is the edit wanted here; a `Vec.push` argument does
not get it. The two helpers take `own` by hand.

Everything else `karac check` reported was the author's: 26 missing
`mut` markers where one stream function hands its `mut ref` stream to
another, and a parameter that needed `own`, all fixed by `karac fix`.
After that the port matched h2 on the first run under the MIR
interpreter, the LLVM backend, the ASan build and the native binary,
with nothing on stderr.

## Scope

Everything in h2's `connection.py`, `stream.py`, `utilities.py`,
`settings.py`, `windows.py`, `frame_buffer.py`, `events.py`,
`exceptions.py` and `config.py` that a conversation can reach, except:
logging (`config.logger`), `header_encoding` other than UTF-8, the
`SizeLimitDict` bound on remembered closed streams (65536, which no
conversation reaches), and the order Python prints a set of two or more
pseudo-header names in (its hash order; the port prints them in the
order they were seen, and every set in the conversations has one).

Licenses: h2, hpack and hyperframe are MIT (`LICENSE-h2.txt`,
`LICENSE-hpack.txt`, `LICENSE-hyperframe.txt`).

## Running

About 12 s on the MIR interpreter and 0.02 s built. The run writes
`data/jobs.txt` to the working directory.

```bash
karac run mini_h2.kara
karac build --mir mini_h2.kara && ./mini_h2
python3 -s -P -B mk.py && python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py   # the data section
```
