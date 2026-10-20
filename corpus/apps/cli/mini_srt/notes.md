# mini_srt

srt 3.5.3 by Christopher Down, the SubRip subtitle library media tools
and subtitle editors in Python use, with the `srt_tools` scripts that
come with it. The port follows the library and the scripts:
- `srt.parse`: SubRip blocks matched by srt's own `SRT_REGEX`, with its
  lookaheads, which accept a missing index, a fractional index, `.` and
  fullwidth delimiters in timestamps, a missing millisecond field,
  `- >` and spaced arrows, position metadata after the timestamps, CRLF
  line ends, blank lines inside the content and a missing blank line at
  the end; the contiguity check, its `SRTParseError` and, with
  `ignore_errors`, the warning that skips the junk instead;
- `Subtitle` and its `repr`, the `timedelta` values it holds and their
  `repr`, ordering by start, end and index;
- `compose`, `sort_and_reindex` with its three skip rules, `to_srt`,
  `make_legal_content`, `srt_timestamp_to_timedelta` and
  `timedelta_to_srt_timestamp`, and the log lines they write;
- the scripts `srt normalise`, `fixed-timeshift`, `linear-timeshift`,
  `deduplicate`, `mux`, `lines-matching` and `process`, with their
  options, `-i`/`-o`/`-p`, `--no-strict`, `-c` and `-e` with the
  `utf-8-sig` default and `latin-1`.

`SRT_REGEX` needs lookaheads and a lazy `.*?`, so the port has a small
backtracking regex engine with Python's leftmost, priority-ordered
semantics, working on code points as Python's `str` does. The patterns
are built from the same pieces srt builds them from.

`lines-matching` and `process` take a Python lambda; the port knows the
ones the jobs use.

`mk.py` writes `data/`. It holds:
- subtitle files: a clean one out of order, a messy one with every quirk
  above and a BOM, one with junk between blocks, one with a WebVTT
  header, one with leading blank lines, one with subtitles the
  reindexing skips, one with times past an hour, a day and a hundred
  hours, English and Chinese files to mux, a mixed-language file, one
  with repeated lines, a Latin-1 file, one with tags, an empty file, a
  blank one and a long generated one;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then what it printed: a
subtitle's `repr`, a log line (`! LEVEL:logger:message`), an error, or
the file a script wrote. The last job runs `normalise -p` on a file and
the next one parses what it left.

`oracle.py` runs the jobs through srt itself (`srt.py` and the scripts
in `srt_tools/`, unchanged; each script runs its own `main()` with its
own argument parser) and wrote `expected.out`. `gen_kara.py` writes the
data section; files that are not plain UTF-8 are stored as hex.

The quirks are kept. Some examples:
- `00:00:01.5` is one second and five milliseconds, not 500;
- a negative time is written with Python's `divmod` arithmetic, so
  -3725.5 seconds from zero comes out as `-2:57:54,500`;
- `srt mux --no-time-matching` turns time matching on, and leaves it off
  only with `-t`;
- `deduplicate` deletes by position after earlier deletions, in the
  order the duplicates sort in;
- with `-e utf-8` a BOM is not dropped by the codec, and the parser lets
  it through as leading junk.

srt is under the MIT License (`LICENSE-srt.txt`).
