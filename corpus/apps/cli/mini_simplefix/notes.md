# mini_simplefix

A port of [simplefix](https://pypi.org/project/simplefix/) 1.0.17 (MIT;
David Arnold), the FIX protocol library for Python. FIX is the wire
protocol that brokers, exchanges and trading desks use for orders and
executions: `tag=value` fields split by SOH bytes, with a body length and
a checksum. simplefix builds, encodes and parses those messages and
leaves the session to the application. The vendored copy in
`vendor/simplefix/` is the package, unchanged.

```
== Building a message: append_pair, the header and the values
enc m = b'8=FIX.4.2|9=70|35=F|49=SENDER|56=TARGET|29=A|11=12345|55=IBM| 56=Y|44=101.25|+7=plus|10=033|'
...
== Parsing: junk, empty values and bad tags
next f = ! EmptyValueError: 29
next h = ! TagNotNumberError: invalid literal for int() with base 10: b'1__2'
...
== A trading day: logons, orders and fills
order CLIENT1 C1-1 2 MSFT 500 2 412.50
  13:30:00.005 CLIENT1>BROKER 8=FIX.4.4|9=99|35=D|49=CLIENT1|56=BROKER|34=2|52=20261023-13:30:00.005|11=C1-1|55=MSFT|54=2|38=500|40=2|44=412.50|10=163|
  13:30:00.006 BROKER>CLIENT1 8=FIX.4.4|9=134|35=8|49=BROKER|56=CLIENT1|34=2|52=20261023-13:30:00.006|37=O1|11=C1-1|17=E1|150=0|39=0|55=MSFT|54=2|38=500|44=412.50|14=0|151=500|6=0|10=237|
...
== Lost messages: resend requests, gap fills and duplicates
  BROKER: gap: expected 13, got D 15; asking for a resend
  CLIENT1: resending 13 to 15 to BROKER
...
  fix.log read until the buffer is empty: 127 messages and 127 Nones, 16302 bytes, fnv 314540ee; by type 0:25 1:2 2:5 3:1 4:3 5:6 8:44 9:3 A:6 D:28 F:4; 0 bad checksums
```

## What it does

`mk.py` writes `data/jobs.txt`, 14 jobs of 441 steps. `oracle.py` plays
them through the vendored simplefix and `mini_simplefix.kara` through the
port; both print each query's result as a Python repr, and each error as
`! ValueError: ...`, `! TagNotNumberError: ...` and so on.

- **FixMessage**: `append_pair` with int, str and bytes tags and values,
  `None` ignored, the header insertion point, and the tags simplefix
  reads with `int()` (`035`, ` 56`, `1_0`, `+7`) so that `035=F` sets the
  message type and `1_0` counts as CheckSum; `to_string` with any
  separator and its `UnicodeDecodeError` on non-ASCII; `count`,
  indexing, `in`; `append_string(s)` with its two errors; `get` and
  `remove` of the nth repeating field; the cooked `encode`, which moves
  BeginString, BodyLength, MsgType and CheckSum into place and drops any
  given out of order, and the raw one; its two errors; `append_data`
  with raw data that holds SOH; the multiset `==`; the UTC timestamp,
  time-only and deprecated `append_time` appenders at precision 0, 3 and
  6 from datetimes and epoch floats (a leap day, 1970, 2099); and the
  time-of-day parts appenders with every range check and the time-zone
  offset string (`-30` minutes prints `+00:30`, as in the library).
- **FixParser**: whole messages, fragments of every size from 1 byte,
  the buffer between calls, junk before BeginString, empty values
  allowed and refused, tags that Python's `int()` accepts (` 12 `,
  `+12`, `-12`, `1_2`, `012`) and refuses, raw data fields with private
  tags added and removed, bad and negative raw lengths, a missing
  BeginString allowed or refused, the config error, stop tags and stop
  bytes set four ways, a stop byte inside a tag, and the CME message from
  simplefix's tests that carries an execution report in XmlData, split at
  three different places.
- **A venue**, this app's own code, the same in the oracle and the port:
  a broker with two clients on simulated links that deliver each message
  in chunks of 7, 19, 64, 3 and 200 bytes. Sessions log on, keep
  sequence numbers both ways, send heartbeats, and send a test request
  after 36 s of silence and log out when it goes unanswered. Lost
  messages are noticed at the next sequence number and answered with a
  resend request; the replay sends application messages again with
  PossDupFlag and OrigSendingTime and collapses session messages into
  SequenceReset-GapFill. A garbled checksum is caught and ignored, and a
  cut link times both ends out, after which the next logon recovers both
  directions. The broker checks NewOrderSingle (session reject for a
  missing tag, execution report rejects for an unknown symbol, a bad
  side, quantity, type or price, and a duplicate ClOrdID), matches limit
  and market orders by price then time, cancels the rest of a market
  order, and answers OrderCancelRequest with a cancel or an
  OrderCancelReject. Each client keeps a view of its orders from the
  execution reports. At the end the whole session log is written to
  `fix.log`, one message per line, read back, and parsed with
  `stop_byte=b"\n"`: the library's parser returns None at each line
  break, so a loop that stops at the first None reads one message, and
  one that runs until the buffer is empty reads all 127.

Not ported: the local-time appenders (`append_tz_timestamp`,
`append_tz_time_only`), whose output depends on the machine's time zone,
and the appenders' `timestamp=None` default, which reads the clock. The
port's `int()` stops at 18 digits; no job reaches that.

## Edits from mini_simplefix.orig.kara to mini_simplefix.kara

`karac check` raised one error at a time for the first two:
`const SYMBOLS: [Str; 2]`, which `karac fix` rewrote as
`Array[Str, 2]`, and a parameter named `char`, a type name, which is now
`byte`. Then it raised eight. `karac fix` applied five: `own` on three
parameters that are kept, `Some(ref t)` for a test request id read after
the match, and `#[derive(Clone)]` on the client view's `Row`, the first
step of its fix for a row moved inside a loop and pushed after it. That
loop is now a search for the index followed by the assignment or the
push. The other three were `[Vec.new(), Vec.new()].to_vec()` for the
venue's per-symbol books and per-client lists, refused with
`expected 'Vec[?T141]', found 'Vec[?T142]'` (B-2026-10-25-6, open); they
are `Vec[Vec.new(), Vec.new()]`, which the field's type reaches. One more
`own` came from `karac fix` after that. Then it matched the oracle on the
first run under the MIR interpreter, the LLVM backend, the ASan build and
the native binary, with nothing on stderr.

## Running

```
python3 -s -P -B mk.py            # writes data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py         # embeds data/jobs.txt in mini_simplefix.kara
karac run mini_simplefix.kara
```

About 3 s on the MIR interpreter (a debug build of `karac`) and under
0.01 s built. The run writes `fix.log` to the working directory and
removes it at the end.

## License

simplefix is under the MIT license in `vendor/LICENSE`, and its source
files carry the copyright line:

    Copyright (C) 2016-2023, David Arnold.

That covers the package in `vendor/`.
