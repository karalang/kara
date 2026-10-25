# mini_webvtt

A port of [webvtt-py](https://pypi.org/project/webvtt-py/) 0.5.1 (MIT,
Alejandro Mendez): the pure-Python library that video tools use to read,
edit, convert and segment WebVTT captions, and to read SubRip (`.srt`) and
YouTube (`.sbv`) captions into the same model. Its `segment` command cuts a
caption file into HLS segments with a playlist:

```
== segment long
> segment long.vtt hls 10 900000
  wrote hls/fileSequence0.webvtt (240 bytes)
    |WEBVTT
    |X-TIMESTAMP-MAP=MPEGTS:900000,LOCAL:00:00:00.000
    |
    |00:00:00.500 --> 00:00:03.000
    |Previously on The Lighthouse.
...
```

## What it does

`mk.py` builds the caption files byte by byte and writes 77 jobs to
`data/jobs.tsv`, one a line. Each job runs in a fresh working directory
(printed as `/work`) holding its files. `oracle.py` replays the jobs through
the vendored library, and `mini_webvtt.kara` replays them through the port.
A job is a list of operations. After each one, every file it created or
changed is printed: as text, or as hex when it is not UTF-8.

- **Reading.** `webvtt.read`, `from_srt`, `from_sbv`, `from_buffer` (a
  `BytesIO`) and `from_string`, with an encoding or without. `show` prints
  the whole model: each caption's `repr`, `str`, seconds, identifier,
  voice, raw lines and comments, the styles, and the header and footer
  comments.
- **Writing.** `content`, `write` as `vtt` or `srt`, `save` with an
  encoding and `add_bom`, `save_as_srt`, and `iter_slice`.
- **Editing.** Captions and styles added, deleted and indexed. Text, start
  and end set through the properties. Comments added to captions, styles,
  the header and the footer. Caption equality.
- **Timestamps.** `Timestamp.from_string`, its `str`, `repr`, seconds,
  tuple and all six comparisons.
- **`segment`**, the HLS segmenter, with its playlist.
- **`cli`** (29 jobs): the `webvtt` command line, with argparse's behaviour
  for option prefixes, `-oVALUE`, `-m=V`, negative numbers, `--`, help, and
  each error message.

The files cover:

- **Every encoding the library detects.** These are UTF-8 with and without
  a BOM, UTF-16 and UTF-32 in both byte orders, UTF-16 without a BOM, and
  Latin-1. There are CRLF, CR-only and Unicode line separators, bytes that
  are not UTF-8, and BOMs that override the encoding argument.
- **The cue grammar as the library's regexes read it.** Hours are
  optional. The cue check's `.` lets a comma or colon through, so the
  timestamp parser is the one that fails. There are NOTE and STYLE blocks
  with and without text, voice spans, cue tags, identifiers, and blank
  lines made of spaces, tabs or no-break spaces.
- **Broken files of each format.** These include bad hours, commas and
  seconds, no header, a lower-case header, empty files, captions with no
  text, and a VTT file read as SRT or SBV.
- **Behaviour that is not obvious.** The first caption's comments become
  the header comments. `save` and `save_as_srt` work out the destination
  from a file, a directory or nothing. They create the file before an
  encoding error, leaving it empty, and add a BOM only for an exact
  lower-case codec name. The segmenter raises `IndexError` when the last
  caption ends on a segment boundary or when captions are out of order, and
  `ZeroDivisionError` for 0 seconds. It writes `#EXTINF:30.00000` whatever
  the segment length.

An exception prints as `error <Class>: <message>`, and the job goes on.

## How the port works

- **A small backtracking regex engine** runs the library's own patterns
  on code points, with Python's `\d`, `\s`, `\w` and lazy quantifiers. So
  a cue that passes the cue check and fails the timestamp parser fails the
  same way.
- **The file system is a map** from path to bytes, with a set of
  directories. `save`'s `isdir` and `splitext` logic, the file created
  before encoding, and `makedirs` in `segment` all run on it.
- **The codecs are ported.** UTF-8, UTF-16, UTF-32 and Latin-1 decode and
  encode with Python's error messages and byte positions. Text mode applies
  universal newlines. A `BytesIO` buffer decodes line by line, so its error
  positions count from the start of the line.
- **The command line is argparse's behaviour** for this parser: prefix
  matching, values attached to an option, negative-number values, and the
  usage line and error message on exit code 2. The help text is the
  library's, as argparse prints it at 80 columns.
- **`gen_kara.py` writes `jobs_text()`** at the end of the `.kara` file,
  so the kata reads no files at run time.

## Compiler gaps

- **B-2026-10-23-7.** A `vec![..]` of integer literals inside a tuple is
  typed `Vec[i64]` even where `(String, Vec[u8])` is expected. The port
  binds each byte list with its type before building the tuple.
- **B-2026-10-23-8.** `karac check` accepted `*o` on a String bound by
  `for (o, a) in option_strings()`, and the MIR front ends refused it. The
  port compares `o` without the `*`, which both accept. Fixed in kara
  d5513133a7, where both spellings pass.

## Scope

- **Only the encodings the jobs use are ported.** These are the UTF
  family and Latin-1. An unknown name raises `LookupError` as Python does.
- **Python repr and regex.** `repr` of a str escapes the non-printable
  Latin-1 characters and treats every other non-ASCII character as
  printable. The regex engine supports the constructs the library's
  patterns use, and no others.
- **The help text is fixed.** It is argparse's output at 80 columns, held
  as a string, rather than produced by a formatter.

The vendored library is unchanged.

## Running

```bash
karac run mini_webvtt.kara                  # interpret
karac build --mir mini_webvtt.kara          # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate jobs_text()
```
