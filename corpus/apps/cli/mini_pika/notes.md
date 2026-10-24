# mini_pika

A port of [pika](https://github.com/pika/pika) 1.3.2 (BSD-3-Clause; Tony
Garnock-Jones, Gavin M. Roy, Pivotal Software, Inc and others), the
pure-Python AMQP 0-9-1 client for RabbitMQ. It covers the parts that sit
between a socket and an application: `URLParameters`, the frame codec and
field tables of `frame.py`, `data.py` and `spec.py`, and the state machines
of `Connection`, `Channel` and `CallbackManager`, with the heartbeat checker
and the blocked-connection timer. The oracle runs the vendored pika
unchanged, as a `Connection` subclass whose transport is a recorded capture.

```
     45 app: connection open to RabbitMQ 4.0.5, frame_max 8192, channel_max 2047, heartbeat 30
     50 > <METHOD(['channel_number=1', 'frame_type=1', "method=<Basic.Qos(['global_qos=False', 'prefetch_count=20', 'prefetch_size=0'])>"])> [19 bytes crc bd288a89]
  40008 app: message 4 from 'orders' key order.bulk: b'warehouse route pallet customer batch ca'... (20000 bytes, crc 3b23e97c)
  34025 app: connection closed: AMQPHeartbeatTimeout: ('No activity or too many missed heartbeats in the last 12 seconds',)
```

## What it does

`mk.py` writes two inputs. `data/urls.txt` holds 56 connection URLs, and
`data/sessions.txt` holds 17 broker captures: the bytes a broker sends, cut
into segments at the times they arrive. The captures are built with a small
AMQP encoder of its own (`struct`, not pika), so a bug in pika's decoder
would show rather than cancel out. `oracle.py` runs both jobs on the
vendored pika, and `mini_pika.kara` runs the same jobs on the same data,
which `gen_kara.py` embeds.

- **URLParameters** prints every setting a URL produces, or the exception
  it raises. The URLs cover both schemes and their default ports, upper-case
  schemes and hosts, percent-escapes in the user, password and vhost, an
  empty or missing vhost, IPv6 hosts in brackets, bad ports, URLs with no
  scheme that `urlparse` reads as a scheme, a repeated parameter, an
  unknown one, and every numeric setting at and past its bounds, written as
  Python's `int()` and `float()` accept or reject them (signs, underscores,
  `inf`, `0x10`, `12.0`, `1__0`).
- **The sessions** each drive one `Connection` the way pika's external
  connection workflow does: the protocol header goes out, each segment is
  fed to `_on_data_available` at its time, and timers fire on the capture's
  clock. A small client program opens a channel, sets its prefetch,
  declares its queue, turns on publisher confirms, publishes its messages,
  consumes, acks each delivery (nacks a poison one) and closes after a set
  number. Every frame each way is printed with its `repr`, and for the
  frames pika sends, their size and CRC-32. The 17 sessions are a long
  happy path (a 20000-byte body split across frames, a capture cut into
  random pieces, a mandatory message returned, confirms acked singly and in
  bulk, a nack, a redelivery, `Connection.Blocked` and `Unblocked`); a broker
  `Channel.Close` that the client answers by closing the connection; a wrong
  password, a refused vhost and a broker with no PLAIN mechanism; a broker
  that answers with an AMQP 0-8 protocol header, one whose
  `Connection.Start` names another protocol version, and a garbled
  `Connection.Start`; a lost heartbeat; a blocked connection that times out;
  a bad frame end, an unknown method id, an unknown field kind, a body
  longer than its header says and an unknown frame type; a broker without
  publisher confirms; and a consumer the broker cancels.

## How the port works

- **Values** are a `Val` enum shaped like the Python values pika's tables
  hold: `None`, `bool`, `int`, pika's `long` (which prints with an `L`),
  `Decimal`, a naive `datetime`, `str`, `bytes`, lists and dicts that keep
  insertion order. `repr` and `str` follow Python for each, so a frame's
  `repr` (AMQPObject's sorted `k=v` list, with `%s` values inside a
  `repr`'d list) comes out the same, nested quotes and all.
- **Field tables** follow `data.py` both ways, quirks included: `b` decodes
  unsigned and `B` signed, `f` and `d` decode to `long(int(x))`, `l` and `L`
  swap their signedness, a decimal is `Decimal(raw) * 10**-d`, and a `S`
  that is not UTF-8 stays bytes. Encoding picks `I` for an int that fits
  32 bits and `l` or `L` otherwise, normalizes a decimal, and turns a
  `datetime` into `T` through `timegm`.
- **Methods** come from a table `gen_kara.py` reads off the vendored
  `spec.py` with `ast`: each method's index, name and the fields its
  `decode()` reads, with their kinds. One codec over that table encodes,
  decodes and prints all 66 methods, with bits packed into octets the way
  `spec.py` packs them. `BasicProperties` has its own codec, flag words
  with their continuation bit.
- **The connection** keeps pika's states and its callback table. A callback
  is an enum value naming the bound method it stands for, so `add()`
  dedupes, a one-shot entry is used up before any callback runs, and
  `remove()` matches arguments the way pika's does. Channel RPCs block and
  queue as `_rpc` does, `_on_synchronous_complete` runs before the user's
  callback, closing cancels consumers without waiting, and the content
  assembler raises where pika raises (an `AttributeError` for a body with no
  header, `BodyTooLongError`, an unexpected frame only logged). An
  exception out of a data event aborts the transport with
  `StreamLostError('Stream connection lost: ...')`, and a connection that
  never opened turns a lost stream into `IncompatibleProtocolError`,
  `ProbableAuthenticationError` or `ProbableAccessDeniedError` by the state
  it was in.
- **URLs** go through Python 3.13's `urlparse` and `urlsplit` (the bracket
  and port checks), `parse_qs` and `unquote`, then pika's setters with
  their messages, so a bad value reports the same `ValueError` text.

## Compiler gaps

One, filed as B-2026-10-24-37. `obj_repr` took its list of strings as a
bare parameter (which borrows) and sorted a local copy:
`let mut sorted = items; sorted.sort();`. `karac check` refused the sort
with E0517, calling the local `sorted` "a `ref` parameter", and its fix-it
inserted `mut ` before the parameter's type, giving `items: mut
Vec[String]`, which does not parse, so `karac fix` skipped it. The same
fix-it comes for a plain `items.sort()` on a bare parameter. The fix that
works is `items: own Vec[String]`; `mini_pika.orig.kara` keeps the first
version.

The rest of what `karac check` raised were real: four `own` parameters
and three `.clone()`s, which `karac fix` wrote; a `match cb { Some(f) =>
... }` inside a loop that moved `cb`, rewritten as `Some(ref f)`; and three
type names (`Url`, `Arg`, `Entry`) that shadow prelude types, renamed. With
those, the MIR interpreter, the LLVM backend, the ASan build and the native
binary all matched the oracle byte for byte on the first run, with nothing
on stderr.

## Scope

- No sockets: the transport is the capture, the clock is the capture's,
  and the adapters (asyncio, select, Twisted, Tornado, gevent, blocking)
  are not ported. `BlockingConnection` is not ported.
- URL parameters whose values pika reads with `ast.literal_eval`
  (`ssl_options`, `client_properties`, `tcp_options`) fail with a
  `ValueError` that says the port does not support them. No URL uses them.
- The client program uses one channel and one consumer. The channel methods
  it does not call (`basic_get`, exchange and queue binding, `flow`,
  transactions) are not ported, though their methods encode and decode.
- pika logs through `logging`; the port drops the log lines, which no job
  prints.

## Running

```bash
karac run mini_pika.kara                    # interpret
karac build --mir mini_pika.kara            # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded data
```
