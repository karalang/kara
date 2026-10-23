# mini_thrift

A port of [Apache Thrift](https://pypi.org/project/thrift/)'s Python
library, 0.21.0 (Apache-2.0; the Apache Software Foundation): the RPC and
serialization framework behind many service meshes and log pipelines. An
IDL file declares structs, exceptions, enums, constants and services; the
library writes and reads them in its binary, compact and JSON wire formats
over plain, framed or buffered transports, and runs a service's generated
processor against a handler.

```
> call compact framed - calculate 4 Work(num1=1, num2=2, op=9)
  request 000000188221000963616c63756c61746515081c1502150415120000
  server: calculate(4, Work(num1=1, num2=2, op=9, comment=None))
  response 000000258241000963616c63756c6174651c15121811496e76616c6964206f7065726174696f6e0000
  error app.ttypes.InvalidOperation: InvalidOperation(whatOp=9, why='Invalid operation')
> readframes compact data/out/points.c Point
  Point(x=1, y=2)
  Point(x=None, y=None)
  Point(x=None, y=None)
  Point(x=None, y=None)
  4 records
  error EOFError: 
```

## What it does

`mk.py` writes the IDL (`data/app.thrift`: the Thrift tutorial's
Calculator service with `shared.thrift` folded in, plus a shop's records),
an event log's rows, a user table and a script of 612 jobs to
`data/jobs.tsv`. `oracle.py` compiles the IDL the way `thrift --gen
py:dynamic` would and replays the script against the vendored library;
`mini_thrift.kara` compiles the same IDL and replays the same script. A job
that raises prints `error <class>: <message>`; 215 do.

- **The IDL**: typedefs, constants (with maps and lists), enums with
  implicit and explicit values, defaults, optional fields, fields listed
  out of id order, gaps in field ids, recursive structs, exceptions, and a
  service that extends another. `spec` prints each generated class's
  `__slots__` and `thrift_spec`, `const` and `enum` the compiled values.
- **Three wire formats**: the binary protocol (strict and loose message
  headers, string and container length limits), the compact protocol
  (zigzag varints, field id deltas, packed bools, the state machine whose
  assertions fire on out-of-order calls) and the JSON protocol (its
  context stack, base64 binaries, and `nan` and `inf` written bare, which
  its own reader then rejects), plus the
  write-only SimpleJSON protocol.
- **Python's edges**: values are Python values, so a struct field holding
  the wrong type fails the way `struct.pack`, `'{0}'.format` or
  `makeZigZag` fail; a set or map key that is a list is `unhashable`; a
  field whose wire type is not the spec's is skipped; a negative field id
  counts from the end of `thrift_spec`; in JSON a surrogate pair is one
  character, a lone low surrogate is an error and a high surrogate at the
  end of a string is dropped.
- **Records**: `enc` / `dec` / `rt` round trips with `==`, `sizes` across
  the four protocols, a schema-less `dump` of any payload, `skip` of one
  value, and framed record files written, patched, truncated and read back.
- **RPC**: a client call (`send_<method>` then `recv_<method>`) through a
  request file, one server turn of the generated processor or the
  `TMultiplexedProcessor`, and a response file, over each protocol and
  transport. Unknown methods, declared exceptions, oneway calls, handler
  bugs that become `INTERNAL_ERROR`, results the client cannot read, and
  hand-written requests the server rejects.
- **Five programs**: an event log writer and a per-kind latency report
  (read again as a newer schema with defaults for the new fields), a legacy
  user writer, a migration from `UserV1` frames to `UserV2` that grants
  roles, and an export to SimpleJSON lines.

## How the port works

- **Values are a Python value enum.** `None`, bools, ints, floats, str,
  bytes, lists, sets and dicts (both kept in insertion order, as CPython's
  dict is), and generated instances as a class id plus slot values. `repr`,
  `==` and hashability follow Python, and a set prints sorted so the output
  does not depend on CPython's string hashing.
- **The IDL compiler builds `thrift_spec`.** Each struct, exception,
  `<method>_args` and `<method>_result` becomes a class table in the order
  the generator emits them, with `thrift_spec` indexed by field id; reads
  and writes walk it exactly as `TBase.read` / `TBase.write` do.
- **One protocol record, four protocols.** A `Proto` carries the binary
  protocol's strictness and limits, the compact protocol's state, last
  field id and pending bool, and the JSON protocol's context stack and
  one-byte lookahead; each `write_*` / `read_*` dispatches on the kind.
- **Transports keep what reached the file.** The framed and buffered
  transports hold writes until `flush()`, so a write that fails mid-record
  leaves the file as the library leaves it, and a framed reader reads
  across frames.
- **Number formatting is CPython's.** `repr` of floats, `'{0:.17g}'` for
  JSON doubles and `%8.2f` in the report come from exact decimal expansion
  of the double, and base64 decoding follows `binascii.a2b_base64`'s
  non-strict mode.

## Compiler gaps

None. `karac fix` made 577 fixes, call-site `mut` markers and `ref` on
three bindings of a `Vec` element, all language rules. The one difference on the
first run was NaN's sign: `0.0 / 0.0` computed at run time is the
hardware's default NaN, which has the sign bit set on x86, while Python's
`float("nan")` does not. The port now uses `f64.NAN`, as design.md does.

## Scope

- The pure Python protocols only: the C accelerator, `THeaderProtocol`,
  the zlib and HTTP transports and the socket servers are not ported.
- The IDL compiler reads the IDL forms `app.thrift` uses: no `include`,
  unions, `senum` or annotations.
- Job literals are parsed as the Python literals the script uses, not
  every form `ast.literal_eval` reads.

## Running

```bash
karac run mini_thrift.kara                   # interpret
karac build --mir mini_thrift.kara           # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded inputs
```
