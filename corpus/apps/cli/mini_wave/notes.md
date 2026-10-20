# mini_wave

Python 3.13's `wave` module, the standard library's WAV reader and
writer, which speech, podcast and audio tools reach for first. The port
follows `wave.py`:
- `_Chunk` with word alignment, chunks read through the RIFF chunk (so a
  RIFF size too small hides the data chunk and one too big is ignored),
  and `skip()`, whose seek past the RIFF end is a bare `RuntimeError`;
- `Wave_read`: the RIFF and WAVE checks, the fmt chunk for PCM and
  WAVE_FORMAT_EXTENSIBLE with its subformat GUID (printed as a UUID),
  the sample width rounded up from the bit depth, `readframes`,
  `setpos`, `rewind`, `tell` and `getparams`;
- `Wave_write`: every setter with its checks, `round()` of the frame
  rate, the header written on the first write and patched afterwards,
  `writeframesraw` and `writeframes`, and `close`.

`mk.py` writes `data/`, a podcast's upload folder built byte by byte:
- recordings in 8, 16 (and 12) and 24-bit, mono and stereo, at 8000 and
  11025 Hz, one with a LIST INFO chunk and one WAVE_FORMAT_EXTENSIBLE;
- the broken and the wrongly named: float WAVs, an MP3 and an AVI renamed
  `.wav`, an upload cut off mid-data, missing and out-of-order chunks, a
  short fmt chunk, zero channels or bits, a LIST chunk claiming a
  megabyte, RIFF sizes too small and too big, an empty file;
- two playlists and `jobs.tsv`, the 57 jobs.

Five programs are built on the module: `inspect` (format, length, peak
per channel and the quiet lead-in and tail of every file in a folder),
`mixdown` to mono, `normalize` to a peak, `trim` of silence, `convert`
between sample widths, and `concat` of a playlist, which skips clips in
another format. Each writes its file through `Wave_write` and reads it
back. The jobs also drive the reader and the writer call by call.

`oracle.py` runs the jobs through Python's own `wave` and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- an error in the fmt chunk's first 16 bytes is an `EOFError` with no
  message, which `Wave_read` lets through;
- `getparams()` on a writer whose compression type was never set is an
  `AttributeError` for `_comptype`, and writing after `close()` is one
  for `'NoneType' object has no attribute 'write'`;
- the parameters can still be changed after an empty first write, though
  the header has already gone out with the old ones;
- an odd number of data bytes is written with no pad byte;
- `getparams()` keeps the frame count from the first write after later
  writes patch the header.

`getmarkers()` warns that it goes away in Python 3.15; the oracle
silences the warning.

It matched on every backend and found no gaps.
