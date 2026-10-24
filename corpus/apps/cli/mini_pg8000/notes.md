# mini_pg8000

A port of [pg8000](https://github.com/tlocke/pg8000) 1.31.2 (BSD-3-Clause;
Tony Locke, Mathieu Fenniak and contributors), the pure-Python PostgreSQL
driver, together with the parts of its two dependencies it runs through:
the SCRAM client of [scramp](https://github.com/tlocke/scramp) 1.4.5
(MIT-0; Tony Locke) and the timestamp parsing of
[python-dateutil](https://github.com/dateutil/dateutil) 2.9.0.post0
(Apache-2.0 / BSD-3-Clause dual licence). It covers the frontend/backend
wire protocol from the startup message to Terminate, every authentication
method pg8000 knows (trust, cleartext, MD5, SCRAM-SHA-1 and SCRAM-SHA-256)
and the ones it refuses, the simple and extended query protocols, prepared
statements, COPY in and out, notices and notifications, the out-converters
that turn Python values into parameters and literals, the in-converters
for every type oid pg8000 maps, and the paramstyle converter.

```
> session s04 user='dave' ssl=False
    > startup b'\x00\x03\x00\x00user\x00dave\x00\x00'
    > X b''
    error InterfaceError: server requesting MD5 password authentication, but no password was provided
> session s06 user='frank' password='correct horse battery staple' ssl=False
    > startup b'\x00\x03\x00\x00user\x00frank\x00\x00'
    > p b'SCRAM-SHA-256\x00\x00\x00\x00-n,,n=frank,r=5f3a91c2d8e64b07a1c39e2f4d6b8a10'
    > p b'c=biws,r=5f3a91c2d8e64b07a1c39e2f4d6b8a103rfcNHYJY1ZVvWVs7j,p=LkOuVcEmJz8a38/h1E4hPBs/EPfv8QxmPKccUCvTfng='
    connected
> style qmark a "SELECT '?' , ?"
    ("SELECT '?' , $1", ('a',))
> in 114 '[1, 2'
    error JSONDecodeError: Expecting ',' delimiter: line 1 column 6 (char 5)
```

## What it does

`mk.py` writes `data/jobs.txt` (392 jobs) and 31 recorded server streams,
`data/s01.bin` .. `data/s31.bin`. It plays the server side itself,
including the SCRAM server signatures for the client nonces `pgstub.py`
hands out in order. `oracle.py` runs each job on the vendored pg8000;
`mini_pg8000.kara` runs the same jobs, writing the same files to `data/`
first.

- **`session`** (31 jobs) connects over a fake socket that replays one
  stream and prints every message pg8000 flushes, then runs the job's
  `op` lines (61) and closes at `end`. The streams: trust auth with the
  startup parameters; cleartext and MD5 passwords; SCRAM-SHA-256 in full,
  with both mechanisms offered (no TLS, so the -PLUS ones are skipped),
  SCRAM-SHA-1 only, and the SCRAM failures (only -PLUS offered, a bad
  server signature, a bad server nonce, bad attributes, an error in the
  server-final message, a password with a tab for saslprep); a server
  that refuses the password, asks for GSS or for a method code pg8000
  does not know, and the bad connect arguments (`user=None`, a non-string
  database); TLS requests answered `N`; one result set through every in-converter;
  the server switching `client_encoding` to latin1, cp1252 and ascii;
  errors, a failed transaction, notices and notifications; prepared
  statements; COPY TO STDOUT into bytes and text streams and COPY FROM
  STDIN out of lines, bytes and text; the extended protocol with every
  parameter type, the `types=` argument and placeholder errors; and
  streams that desynchronise, cut off or send an unknown message code.
- **`stmt`** (24) runs `to_statement`, the `:name` placeholder rewriter of
  the native interface, over quoting, casts, comments and dollar quotes.
- **`style`** (18) runs the DB-API paramstyle converter in each of the
  five styles (qmark, numeric, named, format, pyformat) and an unknown
  one, with the errors it raises.
- **`out`** (59) runs `make_param` and the array, composite and range
  out-converters; **`literal`** (22) and **`ident`** (13) run the native
  interface's `literal()` and `identifier()`.
- **`in`** (133) runs the in-converter for an oid over a text value:
  booleans, bytea, numerics, dates and times with every timezone form,
  timestamps BC (dateutil's warning is printed where it is raised),
  intervals in the iso, postgres and sql_standard styles, JSON with its
  decode errors, UUIDs, inet and cidr, ranges, multiranges, arrays and
  records.

## How the port works

Python values are one `V` enum, from `None` and `Int(i128)` to `Range`,
`Interval` (pg8000's `PGInterval`, eleven fields) and the `Bio` and `Sio`
streams; `repr` and `pstr` print them as Python's `repr` and `str` do,
including `datetime`, `timedelta`, `Decimal`, `UUID` and the `ipaddress`
types. The jobs give their arguments as Python literals with constructors,
read by a small expression parser, `Px`. The connection is a `Conn` over
the replayed bytes, with `handle` dispatching on the message code as
pg8000's `handle_messages` does, and `Ctx` carrying a statement's rows,
columns, stream and error. SCRAM uses the hash, HMAC and base64 code from
mini_bson and mini_kafka, with SHA-1 and SHA-256. `strptime` is emulated
with the backtracking `_strptime` does over its alternatives, and
dateutil's `parse` is cut to the shapes pg8000 hands it. JSON comes from
mini_json's scanner.

## Compiler gaps

- **`B-2026-10-24-15`** (filed with this app): a binding of a non-Copy
  payload from a `match` on a field of a `mut ref` parameter,
  `match ctx.stream { V.Bio(buf) => append(mut buf, data) }` with
  `ctx: mut ref Ctx`, passes `karac check`, as core-semantics §4.6 says it
  should, but the MIR typecheck types `buf` as `ref Vec[u8]` and refuses
  the `mut` argument. A method call through the same binding is accepted,
  so the COPY_DATA arm pushes the bytes one at a time with
  `buf.push(*x)`.
- None besides that one: `karac fix` supplied the `mut` call-site
  markers and the give-away repairs the hand-written port left out.

## Scope

- saslprep is ASCII-only, the SCRAM stage-order checks are not ported, and
  only SHA-1 and SHA-256 are offered.
- No TLS: a session that asks for it is answered `N` or fails as pg8000
  does without an SSL context.
- dateutil is cut to `Y-m-d H:M:S[.f]` with a numeric offset or a zone
  name, which is what pg8000's BC-timestamp path gives it; an unknown name
  warns and leaves the value naive, as dateutil does.
- The "replace" decode of an invalid UTF-8 byte gives U+FFFD per
  non-ASCII byte rather than per invalid sequence, and `str.isalpha`
  (in `identifier`) is ASCII-only. No job reaches either difference.

## Running

```bash
python3 -s -P -B mk.py                  # data/
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py               # appends the jobs and streams to mini_pg8000.kara
karac __mir-run mini_pg8000.kara | diff - expected.out
```
