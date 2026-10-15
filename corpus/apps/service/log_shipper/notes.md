# log_shipper

A log shipper in the shape of Vector or Fluent Bit. It tails real log
files and posts NDJSON batches to an HTTP collector on a real loopback
socket (port 47746).

**Inputs and checkpoints.** A file is identified the way Vector's
checksum strategy does it: by the CRC-32 of its first line, not by its
name. A checkpoint is (fingerprint, byte offset). It is written
atomically, to a temp file that is then renamed, after every acknowledged
batch. On start, three cases are handled:
- The fingerprint matches: reading resumes at the offset, or the file is
  reported up to date.
- The fingerprint changed and `<path>.1` carries the old one: the file
  was rotated by rename. The rotated file is drained from the saved
  offset first, then the new file is read from 0.
- Otherwise the file was replaced or copytruncated, and it is read from 0.

A trailing line without a newline is held back until it is complete. Read
offsets are in bytes, through `File.seek`.

**Processing.** Each line goes through a parser chosen per input: JSON
lines (escapes including `\u`, numbers, booleans, null, with
column-numbered errors) or nginx's combined format. The parser is chosen
through a generic `LineParser` bound, because `dyn` is deferred. Records
then pass through a list of closure transforms:
- drop health checks;
- normalise levels (`WARNING` becomes `warn`);
- redact e-mail addresses and the last octet of client IPs;
- add the host name.

Records render as JSON with sorted keys and carry `_fp` and `_off`, which
the collector uses as an idempotency key.

**Delivery.** Batches hold up to 4 records. The collector is scripted to
answer 503 with and without `Retry-After`, and answers 413 when a
record's `msg` is too long. The shipper backs off exponentially on a
virtual clock, or waits as long as `Retry-After` says. It gives up after
4 attempts. It splits a 413 batch in half until the bad record is alone.
Anything it gives up on goes to `dead/letters.ndjson`.

**Scenario.** It runs four times:
1. Run 1 crashes after a batch is acknowledged but before its checkpoint
   is written.
2. Run 2 comes after app.log was rotated and access.log's partial line
   was completed. The collector ignores the three re-sent records.
3. Run 3 has nothing new.
4. Run 4 sees access.log copytruncated and a 503 storm that ends in
   dead-lettering.

The collector's store, the checkpoints and the dead letters are printed.

Oracle: `log_shipper.py`. It is a port, run over a real socket.
`expected.out` is its output.

## Edits from log_shipper.orig.kara to log_shipper.kara

There were no author errors.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-15-25 (new):** `|r: own Rec| ...` does not parse, though
  design.md gives `|x: own T| body` as how to set a closure parameter's
  mode. The transforms take their mode from the binding's
  `Fn(own Rec, Str)` type instead.
- **B-2026-10-15-27 (new):** a closure that captures `host` and is
  returned inside a `Vec` of structs captures it by reference on v2,
  though core-semantics.md §9.3 says an escaping closure captures by
  move. mir-interp reports a dangling reference and ASan a
  heap-use-after-free. `karac check` refuses the program with the legacy
  backend's `escaping_closure` deferral. The transform takes the host as
  an argument stored next to it instead.
- **B-2026-10-15-19:** passing a `String` to `parser.parse`, a trait
  method taking `Str`, through the generic bound is refused. The call
  passes `line.as_str()`.
- **B-2026-10-15-23:** check treats `find`'s argument as moved.
  `field_of` reads the key's length before the find.

While narrowing -27 I also found **B-2026-10-15-26 (new)**: calling
`v[0](7)` on a `Vec[Fn]` filled with `push` fails on every v2 surface.
`vec![..]` and struct fields work. The app keeps its closures in a
struct field, so it does not hit this one.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-15-19 is fixed, so
`parser.parse(line)` passes the `String` without `.as_str()`. Each of
these is the original line again, and the app still matches on all four
MIR surfaces.
