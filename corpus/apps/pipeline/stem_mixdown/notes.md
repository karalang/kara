# pipeline/stem_mixdown

An audio pipeline, the shape of a podcast mastering job. It synthesizes
four two-second stems at 16 kHz, writes each as a mono 16-bit PCM WAV
file, reads the files back and decodes them, cleans each stem up, runs
five reports over the cleaned stems, and mixes them onto a master bus that
it writes as a WAV file and checks by decoding it again.

- The stems: a voice (a 140 Hz sawtooth with a 5 Hz vibrato through a
  one-pole low-pass, gated into syllables, with a pause from 1.2 s to
  1.5 s), a music bed (an A major chord with a 3 Hz tremolo), drums (a kick
  every half second whose pitch falls from 150 Hz to 50 Hz, and a hi-hat of
  high-passed noise between the kicks) and room tone (low-passed noise and
  a 60 Hz hum). The oscillators read a 1024-step Q15 sine table built from
  Bhaskara I's approximation; the noise is a 31-bit linear congruential
  generator.
- The decoder walks the RIFF chunks and checks the format code, the
  channel count, the sample width and the rate, and that the data chunk
  fits in the file. A copy cut to 1000 bytes shows that error, and a copy
  of the master with its format code changed shows another.
- The clean-up is a DC-blocking high-pass, a 4:1 compressor above 12000
  driven by a peak envelope, and normalization to a peak of 24000.
- The reports: levels (peak, RMS, crest factor), the zero-crossing rate,
  Goertzel magnitudes at eight frequencies, the runs of silence, and an
  ASCII waveform.
- The mixdown adds each stem at its own gain and saturates at full scale,
  counting the samples that clip; then it prints the master's RMS per
  250 ms, its levels and its waveform.

Everything is integer arithmetic, so the backends and the Python mirror
`stem_mixdown.py` agree by construction; the mirror wrote `expected.out`.
Kāra's `/` truncates toward zero, so the mirror divides through `tdiv()`.

stmt-par: six groups of statements that do not depend on each other, in
`run()` (`stem_mixdown.orig.kara` lines):
- L487-L490 four syntheses, each from nothing but the sine table. This is
  where most of the time goes;
- L493-L496 four `fs.write_bytes` calls, each with the `encode_wav` call
  that builds its bytes;
- L508-L511 four `fs.read_bytes` calls;
- L520-L523 four `decode_wav` calls. Each can fail with `?`, so a parallel
  run has to report the first failure in source order;
- L530-L533 four `clean` calls;
- L536-L555 twenty reports, five per stem, that only read the cleaned
  stems.

The four `mix_into` calls (L565-L568) are the case that must stay in
order: each takes `mut ref bus`, and because every addition saturates, the
master and the clip counts depend on the order the stems go in.

## Edits from stem_mixdown.orig.kara to stem_mixdown.kara

Author errors, all machine-fixed by `karac fix` (two passes, 14 fixes):
- `let (mut p1, mut p2, ..) = ..` is Rust syntax; Kāra puts the `mut`
  before the pattern, `let mut (p1, p2, ..) = ..`. Three such lets; the
  parser's error says so and the fix rewrites them.
- `put_u32` passes its `mut ref` parameter on to `put_u16` without the
  call-site `mut` marker. The parse errors stopped the first pass before
  the typechecker saw these, so a second `karac fix` added the two
  markers.

Compiler gaps: none found. After those fixes it matched the mirror on the
first run under the MIR interpreter, the LLVM backend, the ASan build and
the native binary, with nothing on stderr.

## Running

```bash
karac run stem_mixdown.kara
karac build --mir stem_mixdown.kara && ./stem_mixdown
python3 -I -B stem_mixdown.py > expected.out
```

About 8 s on the MIR interpreter and 0.01 s built. It writes its six files
to the working directory and removes them at the end.
