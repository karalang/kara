# mini_nmea

pynmea2 1.19.0, the NMEA 0183 parser for Python by Tom Flanagan and
contributors, on the instrument log of a small yacht sailing the Solent.
Chart plotters, AIS gateways, fleet trackers and boat data loggers all
read NMEA: comma-separated sentences from GPS receivers, depth sounders,
wind instruments and autopilots, multiplexed onto one serial line. They
have to check the checksum, tell a talker sentence from a query and a
manufacturer's proprietary one, turn `4916.45,N` into degrees, and keep
going when a line arrives broken.

The port follows pynmea2's model. `NMEASentence.parse` runs the same
sentence regex (optional `$`, the proprietary, query and talker forms
tried in that order, an optional checksum in either case) and raises
the same `ParseError`, `ChecksumError` and `SentenceTypeError` with
their tuple arguments. Proprietary sentences dispatch through each
manufacturer's `__new__` (Ashtech, Garmin, Kenwood, Magellan, Nortek,
RD Instruments, SiRF, Sxn, Trimble, u-blox and Vectronix), including the
ones that raise `IndexError` on a short sentence. Attribute lookup takes
properties first, then instance attributes, then fields, with a
property's `AttributeError` falling through to `__getattr__` as Python
does. Fields convert with pynmea2's converters: `timestamp`,
`datestamp` (strptime with its backtracking), `int`, `float`,
`Decimal` and `utcfromtimestamp`, and values print as Python reprs.

**Inputs.** `data/` holds a trip log (283 CRLF lines), an instruments
file, a device file with every manufacturer's sentences, an edge-case
file of broken and odd lines, a file without a final newline, one with
bare `\r` line ends, and `jobs.txt`. `mk.py` writes them.

**Jobs** (56 lines in `jobs.txt`):
- `parse` / `strict`: each line's repr and properties (lat/lon in
  degrees, minutes and seconds, `datetime`, `localdatetime`, `is_valid`,
  waypoint and satellite lists, transducers, SeaTalk command names), or
  the error.
- `attrs`: named attributes across a file.
- `stream` / `streamfile`: `NMEAStreamReader` in `raise`, `yield` and
  `ignore` modes, fed in fixed chunks or line by line.
- `file`, `fileread`, `filelines`, `filenext`: `NMEAFile`.
- `edit`: `setattr` on fields and properties, and the rendered sentence.
- `build` / `buildp` / `query`: sentences built from parts, rendered.
- `render`: `render()` with every checksum, dollar and newline option.
- `report`, `gpx`, `mux`, `alarms`: a voyage report, a GPX track in
  `data/out/`, a multiplexed log, and an anchor/depth/wind alarm watch.

Not ported: SiRF 103's `checksum` field (its repr carries an object
address, and no data uses it), non-ASCII letters in `\w`, and `NMEAFile` in write
mode.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of pynmea2 in `pynmea2/` (MIT licence, in
`LICENSE-pynmea2.txt`). `gen_kara.py` writes the sentence classes (base,
flags, properties and fields with their converters), the SeaTalk
command table and the data files into the Kāra source.

## Edits from the first draft

Author errors:
- `fn f(mut self, ..)` is not Kāra; a mutating method takes
  `mut ref self`.
- `s.bytes().count()` is `s.len()`.
- `.clone()` needs `#[derive(Clone)]` on the five value types.
- A match on an owned `Val` binds by value, so `*n` was wrong.
- `let f = c.fields[i]` moves out of a Vec (`E_INDEX_MOVE_NON_COPY`);
  the port clones the one member it needs.

The first data draft had `V` (void) in every RMC status, so the report
had no fixes; `mk.py` now varies it.

After that, the output matched `expected.out` on `__mir-run` and
`__mir-llvm-run` on the first run.

Written 2026-10-21 by the real-world apps thread; karac at e14c800554.
