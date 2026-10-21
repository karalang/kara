# mini_hl7

python-hl7 0.4.5, the HL7 version 2 library for Python by John Paulett
and contributors, as the interface engine of a hospital for one morning.
HL7 v2 is still how most hospital systems talk to each other: the
patient administration system announces admissions and discharges, the
lab sends results, theatre sends schedules, and an interface engine in
the middle parses each message, routes it, and answers every one with
an ACK or a NAK over MLLP framing. The format is old and loose: the
separators are declared by the message itself in MSH.1 and MSH.2,
escapes like `\F\` and `\X41\` hide inside fields, and batches and files
wrap messages in header and trailer segments.

The port follows python-hl7's model. The parse plan reads the
separators from the message (with the defaults for short encoding
characters, and the `AssertionError` a duplicated separator trips),
and the five-level split keeps MSH's, BHS's and FHS's awkward first
fields. Containers behave as the Python list subclasses do: their repr,
`str` with each level's separator, 1-based calls (which a Segment does
not shift), slicing, and the header and trailer setters with their
`MalformedSegmentException`s. `parse_batch` and `parse_file` work over
`splitlines` with the same `ParseException`s, and `parse_hl7`,
`ishl7`, `isbatch`, `isfile` and `split_file` dispatch as they do.
Accessor keys (`PID.F3.R1.C1`) parse as `Accessor.parse_key` does, and
`extract_field` and `assign_field` keep every compatibility rule and
every `IndexError`, `TypeError` and `AttributeError` Python raises when
the tree holds a str where a list was expected. `unescape` and
`escape` take application maps; `parse_datetime` keeps the regex, the
precision rules, the float-minute time zones (so `-0030` comes out as
`+00:30`) and datetime's own checks. On the wire, `client.read_stream`
keeps its EB/FF rule, and the asyncio `HL7StreamReader` its limits,
invalid blocks, decoding errors and the incomplete read that ends the
stream.

**Inputs.** `data/` holds 38 files and `jobs.txt`: admission, lab,
discharge and custom-separator messages, edge cases (empty, bare,
lower-case, whitespace-padded and duplicate-separator messages),
batch and file variants with missing, doubled and orphaned headers
and trailers, MLLP captures (a morning's inbound feed, an FF-framed
one, broken, invalid-block and mixed streams), loose text, and lists
of datetimes and escapes. `mk.py` writes them.

**Jobs** (96 lines in `jobs.txt`):
- `tree`, `split`, `is`: parse a file each way and show the tree.
- `batchof`, `fileof`: build a batch and a file, set their headers and
  trailers, including the wrong segments.
- `get`, `accessor`, `set`, `assign`, `idx`: accessor reads and writes
  down to subcomponents, and indexing and slicing.
- `escape`, `escapeout`, `dtm`: escapes with and without application
  maps, and datetimes.
- `ack`, `mcid`: `create_ack` and `generate_message_control_id`.
- `stream`, `loose`, `blocks`: the three MLLP readers.
- `route`: the interface engine. It reads the inbound feed, ACKs, NAKs
  or rejects each message, keeps a ward census from ADT A01/A03/A08
  and lab results from ORU R01 with their flags, and writes the ACK
  stream and a results report to `data/out/`.
- `reencode`: rewrite a message with other separators and check the
  round trip.
- `audit`: count a file's batches and messages against their trailers
  and find duplicate control ids.

create_ack and generate_message_control_id read the clock and the
random module, so both programs use a clock that starts at
2026-10-21 08:00:00 UTC and ticks one second per reading, and a sample
that picks by a counter.

Not ported: python-hl7's logging (it goes to stderr), the MLLP client
and server sockets, and `hl7.mllp`'s asyncio writer.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of python-hl7 in `hl7/` (BSD licence, in
`LICENSE-hl7.txt`). `gen_kara.py` writes the data files into the Kāra
source.

## Edits from the first draft

Author errors:
- `[a, b].to_vec()`: a list literal is already a `Vec`.
- `perr` and `key_err` keep their message, so they take it as
  `own String` (a bare parameter borrows).
- The header and trailer setters matched a borrowed `Option[Cont]`, so
  the segment they store is a clone.
- `parse_batch` and `parse_file` matched on their `Option[String]`
  accumulators by value inside the loop; they match on a clone.

The oracle needed two edits of its own: a message with duplicate
separators raised out of the job runner, so every job now runs inside
a handler, and the routing engine's QRY message has no MSH.9
components, so the event is read under `except IndexError`.

After that, the output matched `expected.out` on `__mir-run` and
`__mir-llvm-run` on the first run, as did both files in `data/out/`.

Written 2026-10-21 by the real-world apps thread; karac at 54460c5918.
