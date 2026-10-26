# cli/mini_cantools

A port of [cantools](https://github.com/cantools/cantools) 39.4.5 (Erik
Moqvist, MIT; `LICENSE-cantools.txt`), the Python CAN bus toolkit, with
the parts of [textparser](https://github.com/eerimoq/textparser) 0.24.0
and [bitstruct](https://github.com/eerimoq/bitstruct) 8.19.0 (both MIT,
same author; `LICENSE-textparser.txt`, `LICENSE-bitstruct.txt`) that its
DBC path runs on, used the way a vehicle team uses it.

## What it does

`data/vehicle.dbc` describes the powertrain bus of an electric car: six
nodes, eight messages (one with an extended id, one multiplexed into four
pages of cell data), comments, attribute definitions and values, value
tables and choices. The program writes it out, reads it back, loads it
and prints what cantools sees in it:

```
message BMS_Cells 0x201 standard, 8 bytes, senders ['BMS'], cycle 250, send type 'Cyclic'
  tree [{'CellGroup': {0: ['CellVoltA', 'CellVoltB', 'CellVoltC'], 1: [...], 2: [...], 3: ['BalanceMask', 'CellTempAvg']}}, 'RollingCount']
  PackCurrent: 23|16 big signed, range [-3276.8, 3276.7], unit 'A', receivers ['VCU'], initial None
    LinearConversion(scale=0.1, offset=0, is_float=False), scale 0.1, offset 0
```

Then:

- eleven broken DBC files are loaded, strictly and then not: overlapping
  signals (Intel, Motorola, and two under the same multiplexer page),
  signals that stick out of their message, a zero-length signal, frame ids
  too long for a standard or an extended frame, a syntax error, an
  unterminated string and a stray character, each with cantools's own
  message (`DBC: "Invalid syntax at line 11, column 16: " SG_ Right : 8 >>!<<8@1+ ..."`);
- 87 codec jobs (`data/jobs.txt`) encode and decode messages with and
  without scaling, padding, strict checks, choice decoding and truncated
  frames, including every kind of error the encoder and decoder raise
  (a missing or unknown signal, a value out of range, a choice name that
  does not exist, a multiplexer page that does not exist, a raw value too
  wide for its bits, a short frame);
- `data/drive.log`, a candump `-L` log of a one-minute drive and a charge
  (2,781 lines, 2,779 frames), is decoded frame by frame: per-message
  counts, mean period against the DBC's cycle time and the longest gap;
  every signal's range, choice counts and last value; decode errors (a
  frame cut short, a cell page the DBC does not know); every frame
  re-encoded and compared with the bytes on the bus (a stray bit in an
  unused field, a "not available" 65535 the DBC's maximum refuses, a
  sensor spike); the VCU checksum and rolling counter; state changes of
  the gear, the pack, the motor fault and the charger; and the trip's
  distance, pack energy, peak and regenerative power, and cell spread.

```
re-encode errors: 48
  x47 from 0.495 OBC_Charge: EncodeError: Expected signal "TimeToFull" value smaller than or equal to 65534 in message "OBC_Charge", but got 65535.
  x1 from 31.000 MCU_Motor: EncodeError: Expected signal "MotorTemp" value smaller than or equal to 180 in message "MCU_Motor", but got 190.
...
distance 0.5842 km, pack energy 38.448 Wh, peak 72.62 kW, regen -42.27 kW, derated in 50 motor frames
```

## How the port works

- **textparser.** The DBC grammar is cantools's own, built from the same
  combinators: Sequence, Choice, ChoiceDict (which `choice()` picks when
  every alternative starts with a distinct token, as textparser does),
  ZeroOrMore, OneOrMoreDict, DelimitedList, Optional, Any and AnyUntil,
  over a token cursor with textparser's save/restore stack and its
  furthest-failure mark, so a syntax error is reported at the same token.
  The tokenizer follows the regex alternation in order, including the
  lazy string pattern and its backtracking onto an escaped quote.
- **dbc.py.** The loaders for comments (bus comments concatenate),
  attribute definitions and defaults (`int(Decimal(...))`, ENUM indices
  resolved for the send type), attributes by scope, value tables and
  choices (OrderedDicts, so a repeated number keeps its place), extra
  senders, multiplexer flags (`M`, `m<n>`), `[0|0]` meaning no range,
  `Vector__XXX` meaning nobody, and `num()`'s int-or-float.
- **conversion.py.** `BaseConversion.factory` with Python's int/float
  rules: `(1,0)` is an identity, `(2.0,0)` a LinearIntegerConversion with
  scale 2, anything else a LinearConversion whose results are floats, and
  any signal with choices a NamedSignalConversion over one of those.
  Encoding uses `divmod` first for integer scales and `round()` (half to
  even) for the rest.
- **message.py.** Signals sorted by start bit (a stable sort), the codec
  tree with a node per multiplexer page, the signal tree, and with strict
  loading the fit and overlap checks laid out bit by bit as
  `_check_signal` does; `gather_signals`, the "not required" check and the
  range checks (with their `abs(scale) * 1e-6` slack), and decoding with
  `allow_truncated` / `allow_excess`.
- **bitstruct.** Intel signals take bits from the start bit up, Motorola
  signals from the network bit of their most significant bit down; the
  pack order (big-endian signals in network order, then little-endian
  ones last to first) decides which overflow is reported.

Numbers carry Python's int/float distinction through decoding, scaling
and printing, and floats print as Python's `repr`, so the output matches
cantools exactly. `oracle.py` is the same program written against
cantools itself (with stand-ins for python-can and diskcache, which only
other parts import); it wrote `expected.out`. `mk.py` wrote the data.

## Compiler gaps

One, filed as `B-2026-10-24-28` (false positive): the codec builder is
called for the root as `create_codec(sorted, None, None)`, its parent a
`ref Option[String]` and its page an owned `Option[i64]`, and
`karac check`, `karac build` and `karac run` refused it with "cannot move
`None` while a borrow into it ... is still live", taking the two literal
`None`s for one place. `__mir-run` ran it. The port named the first one
(`let root: Option[String] = None;`). Everything else `karac check`
flagged, 30 parameter and index moves, `karac fix` fixed; the port then
matched the oracle on its first run under the MIR interpreter, the LLVM
backend, the ASan build and the native binary, with nothing on stderr.

Kara fixed B-2026-10-24-28 in f7bccfcbbb, so the call is back to
`create_codec(sorted, None, None)`. It still matches the oracle on all
four backends with nothing on stderr (`karac` at 946c6e5575).

## Scope

DBC only (no ARXML, KCD, SYM or CDD), classic CAN frames of up to 8
bytes, integer signals (no `SIG_VALTYPE_` floats), one level of plain
multiplexing (no `SG_MUL_VAL_` extended multiplexing), no container
messages, no long-name attributes, no environment-variable tables, no
J1939 or CAN FD frame formats, and no DBC writer. Multiplexer ids are
kept in ascending order, which is the order a Python set of small ints
iterates in. Where cantools prints a set of several unknown signal
names, in no fixed order, the port sorts them; the jobs only ever pass
one.

## Running

```bash
karac run mini_cantools.kara
karac build --mir mini_cantools.kara && ./mini_cantools
python3 -s -P -B mk.py
CANTOOLS_PATH=<cantools>/src:<textparser>:<bitstruct>/src python3 -I -B oracle.py > expected.out
```

About 18 s on the MIR interpreter (a debug build of `karac`), 4.5 s on
the LLVM backend and 0.05 s built. It writes its files under `data/` and removes them at the end.
