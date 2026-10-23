# mini_pythonosc

A port of [python-osc](https://pypi.org/project/python-osc/) 1.10.2
(Unlicense, attwad): the Open Sound Control library used to
drive lighting desks, QLab cue machines, TouchOSC tablets and digital mixers
from Python. A mixer fader move, built and then answered by a server:

```
== build fader x2f6d697865722f63682f332f6661646572 f0.75
args [('f', 0.75)]
dgram 2f6d697865722f63682f332f66616465720000002c6600003f400000
msg '/mixer/ch/3/fader' [0.75] size 28
str /mixer/ch/3/fader 0.75
```

## What it does

`mk.py` writes `data/jobs.tsv`, the OSC traffic of a small theatre rig with
one job a line. `oracle.py` replays the jobs through the vendored library,
and `mini_pythonosc.kara` replays them through the port. Each job prints a
header naming it, then what the library returns; an exception prints as
`error <module.Class>: <message>`.

- **Building messages** (56 `build` and 13 `bmsg` jobs): every argument type,
  inferred and explicit, including arrays, typed lists zipped against their
  values, MIDI tuples and bytes, RGBA colours, float32 rounding and overflow,
  UTF-8 addresses and strings, and each way a wrong value fails: struct range
  errors, `TypeError`s from `len()` and from concatenation, an empty blob,
  unsupported inferred types and invalid type codes.
- **Parsing** (35 `parse` jobs): every type tag, the short float that
  Reaktor sends, negative blob sizes that turn into Python negative slices,
  unhandled tags logged as warnings, missing and extra brackets, and
  truncated datagrams.
- **Bundles** (12 `bundle`, 11 `bparse` and 5 `packet` jobs): nested
  bundles, IMMEDIATELY, NTP timestamps at both ends of the range, content that
  is neither a bundle nor a message, and how errors are wrapped as they move
  up from the type codecs through OscMessage and OscBundle to OscPacket.
- **NTP and SLIP** (22 `ntp` and 31 `slip` jobs): conversions both ways, and
  SLIP encoding, decoding and validation with every escape.
- **Address patterns** (41 `match` jobs): `*`, `?`, `[...]` with `!` and
  ranges, `{a,b}`, regex metacharacters in addresses, broken patterns, and
  mapped addresses with `*` that the dispatcher also reads as regexes.
- **Servers** (13 `serve` jobs): a dispatcher set up with handlers, reply
  addresses, unmaps and a default handler, then fed UDP datagrams, TCP 1.0
  length-prefixed frames or TCP 1.1 SLIP frames. Bundles in the future make
  the server sleep, so the clock is a fake that only moves when it does.

## How the port works

- **Values are Python values.** A `Val` enum covers int, float, str, bytes,
  bool, None, tuple, list and the `(datetime, fraction)` time tag, with
  Python's `repr` and `str` for each. The errors the library raises depend on
  the type of a wrong value, so the port carries values as Python has them
  rather than as OSC types.
- **Slices follow Python.** A negative blob size makes the parser index from
  the end of the datagram, and a short float is padded before it is read, so
  every read goes through a helper with Python's slice rules.
- **Errors keep their wrapping.** Each exception carries its Python class.
  OscMessage wraps a codec error in a two-argument `ParseError`, OscBundle
  and OscPacket wrap it again, and some errors are not caught at all. The port
  catches exactly where the library does.
- **The dispatcher's regexes run on a small regex engine.** The dispatcher
  turns an OSC pattern into a Python regex, and reads a mapped address with
  `*` as a regex too. The port parses that subset of Python's `re`
  (literals, escapes, `.`, classes, groups with `|`, `*` `+` `?` and `^` `$`),
  including the errors that make a broken pattern match nothing, and matches
  it with a Pike VM.
- **The servers are their handlers' loops.** UDP takes one datagram at a
  time and recovers from errors, TCP 1.0 reads length prefixes with
  `recvall`, and TCP 1.1 reads SLIP frames and splits them on `END END`.
  The socket is a buffer that returns the captured bytes.
- **`gen_kara.py` writes `jobs_text()`** at the end of the `.kara` file, so
  the kata reads no files at run time.

## Compiler gaps

None were hit.

## Scope

- **The network is replaced by captured traffic.** The servers and clients
  (`osc_server`, `osc_tcp_server`, `udp_client`, `tcp_client`) are not
  ported; their handler loops are, over byte streams, and their asyncio
  versions are left out.
- **The six callbacks are fixed:** `log`, `echo`, `ack`, `bad`, `empty` and
  `who`.
- **Two boundaries:** a string that is not valid UTF-8 raises
  `UnicodeDecodeError` with a shorter message than Python's, and `repr`
  treats every non-ASCII character as printable. The regex engine supports
  only the subset above, and a mapped address that is not a valid regex
  raises `PatternError` without Python's position. No job reaches any of
  these.

The vendored library is unchanged.

## Running

```bash
karac run mini_pythonosc.kara                # interpret
karac build --mir mini_pythonosc.kara        # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate jobs_text()
```
