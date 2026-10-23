# mini_pysubs2

A port of [pysubs2](https://pypi.org/project/pysubs2/) 1.9.0 (MIT, Tomas
Karabela): the pure-Python subtitle library that video and fansub tools use
to read, edit, retime and convert SubStation Alpha (`.ass` / `.ssa`),
SubRip, WebVTT, MicroDVD, MPL2, TMP, whisper output and pysubs2's own JSON.
Its `pysubs2` command converts and retimes files in place:

```
== cli shift
> cli --shift 0.3s srt1.srt
  return 0
  warning PossibleMissedSubtitleWarning: Possible missed subtitle start near line 20
  wrote srt1.srt (405 bytes)
    |1
    |00:00:01,300 --> 00:00:03,800
    |<i>The harbour is quiet tonight.</i>
...
```

## What it does

`mk.py` builds the subtitle files byte by byte and writes 51 jobs to
`data/jobs.tsv`, one a line. Each job runs in a fresh working directory
(printed as `/work`) holding its files. `oracle.py` replays the jobs through
the vendored library, and `mini_pysubs2.kara` replays them through the port.
A job is a list of operations. After each one, its warnings are printed,
then every file it created or changed: as text, or as hex when it is not
UTF-8.

- **Loading and showing.** `SSAFile.load`, `from_string` and the format
  readers, with an explicit format or autodetection, an encoding and an
  error handler. `show` prints the whole model: the script info and Aegisub
  sections, every style field, fonts and graphics, and every event field
  with its `plaintext`, duration and flags.
- **Saving and converting.** `save` and `to_string` to every ported
  format, with the suffix deciding the format when none is given, `fps`
  for the frame-based ones, `apply_styles`, `keep_ssa_tags` and the
  writers' header options.
- **Editing.** Events inserted, deleted, indexed, copied, compared and
  sorted. `shift` in every unit, `transform_framerate`, `rename_style`,
  `import_styles`, `remove_miscellaneous_events` and `equals`, with its
  warnings.
- **Helpers.** Timestamp parsing and printing in each format's style,
  `ms_to_times`, frames to milliseconds and back, colors in both
  SubStation encodings, alignment conversion and `parse_tags`.
- **`cli`** (71 operations): the `pysubs2` command line, with argparse's
  behaviour for option prefixes, mutually exclusive options, custom types,
  `--to` and `--output-dir`, help and version, and each error message.

The files cover:

- **SubStation as the library reads it.** There are v4.00+ and v4 styles,
  script info lines without a colon, fonts and graphics attachments,
  fields that fail to parse and fall back to defaults with a warning, a
  timestamp that does not parse, numbers written as `inf`, `nan`, `1e2`
  and `1_000.5`, and an ASS file saved as SSA.
- **The other formats' edge cases.** SRT files with missing blank lines,
  stray timestamps in the text, tags kept or dropped, and old Mac line
  endings. MicroDVD with and without an fps line. JSON with non-ASCII
  text, a character outside the BMP included.
- **Encodings.** UTF-8 with and without a BOM, UTF-16, Latin-1 and
  cp1252, bytes that do not decode under `strict` and `surrogateescape`,
  and an unknown codec name.
- **Autodetection.** Files no format claims, and files several formats
  claim.

An exception prints as `error <Class>: <message>`, and the job goes on.

## How the port works

- **A small backtracking regex engine** runs the library's own patterns,
  so a line the library's regex rejects is rejected the same way.
- **The file system is a map** from path to bytes, with a set of
  directories. `save` creates the file before encoding, as the library
  does, so an encoding error leaves it in place.
- **The codecs are ported** with Python's error messages and byte
  positions, and text mode applies universal newlines.
- **Each format is ported from its module**: the SubStation reader and
  writer with their field conversions, the SRT and WebVTT pair, MicroDVD,
  MPL2, TMP, whisper and JSON. A small JSON parser and printer stand in for
  the `json` module, with `ensure_ascii`.
- **The command line is argparse 3.13's behaviour** for this parser:
  prefix matching, mutually exclusive groups, `type=` callables and their
  error messages, and the usage line on exit code 2. The help and usage
  text are argparse's output at 80 columns.
- **`gen_kara.py` writes `jobs_text()`, `help_text()` and `usage_text()`**
  at the end of the `.kara` file, so the kata reads no files at run time.

## Compiler gaps

- **B-2026-10-23-8.** `karac check` accepted `*f` on a String bound by
  `if let Some(f) = g` inside `for g in all` over a `Vec[Option[String]]`,
  and `*t` inside `for (a, b, t) in texts`, and the MIR front ends refused
  both. The port compares without the `*`, which both accept.

The argparse model's action names are `String` rather than `Str`. That is
not a gap: a struct that can hold a view, passed as `mut ref`, may take in
the call's other reference arguments (core-semantics.md §5.3), so a `Str`
field kept the parser's buffers borrowed.

## Scope

- **SAMI and TTML are detected but not read or written.** The jobs only
  autodetect them; the port raises `NotImplementedError` for anything
  more.
- **Only the encodings the jobs use are ported.** These are UTF-8,
  `utf-8-sig`, ASCII, Latin-1, cp1252 and UTF-16, with the `strict` and
  `surrogateescape` handlers. Files decode in one piece.
- **Case-insensitive patterns fold ASCII only.**
- **JSON values are read per field** into the types the library writes.
- **The command line reads files, never standard input**, and `--verbose`
  is not exercised.

The vendored library is unchanged.

## Running

```bash
karac run mini_pysubs2.kara                 # interpret
karac build --mir mini_pysubs2.kara         # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate jobs_text()
```
