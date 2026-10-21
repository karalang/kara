# mini_mido

MIDI as mido 1.3.3 handles it. mido is the pure-Python MIDI library
behind most Python music tools: sequencers, patch librarians, MIDI thru
boxes and file converters. The port covers:
- `Message` with every type: its checks, `repr`, `str`, `bytes()`,
  `hex()`, `bin()`, `dict()`, `is_cc`, `from_bytes`, `from_hex`,
  `from_str`, `copy`, setting attributes, equality, freezing and
  thawing;
- `MetaMessage` with every meta type and its checks, encoding and
  decoding (key signatures, SMPTE offsets, time signatures, text in a
  charset), and `UnknownMetaMessage`;
- the `Parser` and `Tokenizer`, `parse`, `parse_all` and
  `parse_string_stream`;
- `tick2second`, `second2tick`, `bpm2tempo`, `tempo2bpm`, variable-length
  ints and `signed`/`unsigned`;
- `MidiTrack`: names, slices, sums, `fix_end_of_track`, `merge_tracks`;
- `MidiFile`: reading with running status, `clip`, a charset and the
  `debug` dump; every way the broken files in `data/songs/` fail;
  saving, `length`, playback and `print_tracks`;
- SysEx files in both formats (`read_syx_file`, `write_syx_file`) and
  the `EchoPort`, with `reset`, `panic` and `autoreset`;
- five programs:
  - a report on every song: its tracks, notes, tempo map and length;
  - a transposer: the minuet in G moved to C, faster, saved and
    compared with the original;
  - a SysEx librarian: patch files sorted, Roland checksums checked, the
    good messages written to one bank in both formats;
  - a composer: text event logs in `data/events/` turned into MIDI files
    at 100 bpm;
  - a MIDI thru box: the minuet played into an echo port through a
    filter, then the port closed with `autoreset` on.

The run writes `data/`, works through the 87 jobs in `data/jobs.jsonl`,
and then runs the five programs. `mk.py` wrote `data/`.

`oracle.py` runs the same jobs and programs through mido itself and
wrote `expected.out`. It uses the `mido/` package from the 1.3.3 wheel,
unchanged. mido needs `packaging` to import; its version string is not
printed.

The quirks are kept. Some examples:
- Bools pass the checks as ints, so `note_on` takes `channel=True`.
- `copy(data=n)` goes through `bytearray`: an int gives `n` zero bytes
  and a negative one fails with "negative count".
- `from_str` parses `data=x1,2y` as `(1, 2)` and refuses `data=()`; a
  value is tried as `int()` and then `float()`, so `velocity=1_0` and
  `time=inf` are read.
- An unknown message type raises `LookupError`, which
  `parse_string_stream` does not catch, so the stream stops there.
- `MetaMessage.from_bytes` reads the length as a variable int but scans
  the bytes from the wrong place, so `FF 01 81 00 ...` gives a 129-char
  text that starts with `'\x00'`.
- A key signature error says "flats" whenever the key is below 7; a bad
  SMPTE frame rate fails with "sequence item 0: expected str instance,
  int found".
- `MidiFile(type=3)` says "invalid format <built-in function format>".
- The header's shorts are read signed, so an SMPTE division is
  `ticks_per_beat=-6360`, and a save with `ticks_per_beat=40000` fails
  part way, leaving the header only.
- An unknown meta message loses its delta time when it is read.
- Closing an echo port with `autoreset` sends it 32 reset messages,
  which are then waiting on the closed port.
- `MidiFile.length` is a `sum()`, which since Python 3.12 adds floats
  with compensation, so it can differ in the last digit from adding the
  times one by one (the thru program's clock).

## License

mido is under the MIT License, in `LICENSE-mido.txt`.
