# mini_datetime

Timestamps the way Python 3.13's `datetime` module handles them in the
C locale, and a log normalizer built on top. Services, ETL jobs and
incident tools all read timestamps in a dozen formats, convert them to
UTC and line them up, and most of the bugs live in the corners: a
strptime pattern that backtracks, a `%z` with seconds, a naive time
that needs an assumed offset, a week-based date. The program ports
`_strptime`'s regular expressions, the C `fromisoformat` parser,
`timedelta` arithmetic and the glibc `strftime` that `datetime.strftime`
calls.

The run has five parts:
- **strptime**: 62 input and format pairs from `data/strptime.txt`.
  Each prints the parsed datetime and its zone name, or the
  `ValueError` Python gives: no match, unconverted data, a bad
  directive, a stray `%`, a field out of range, an inconsistent `%z`,
  an offset of 24 hours or more, and the ISO week and year rules. `%U`
  and `%W` weeks, `%j`, `%G-W%V-%u`, `%c`, `%x`, `%X`, `%r` and the
  empty `%O` patterns of the C locale are covered.
- **strftime**: 21 formats from `data/strftime.txt` on five sample
  datetimes, naive and aware, including year 5. Python fills in `%z`,
  `%:z`, `%Z` and `%f`; the rest is glibc's: the `_`, `-`, `0`, `^`
  and `#` flags, widths, `E` and `O` modifiers, `%s`, and unknown
  directives copied as they are.
- **fromisoformat**: 45 strings for `datetime`, `date` and `time`:
  basic and extended forms, ISO weeks, ordinal dates (refused, as the C
  parser refuses them), fractions with a comma or more than six
  digits, every offset form, and the malformed strings it rejects.
- **timedelta**: 29 lines of construction, `str` and `repr`, adding to
  a datetime, subtracting aware and naive datetimes, multiplication,
  floor division and modulo, negation and `abs`, with the overflow and
  division errors.
- **normalize**: nine log sources in `data/sources.txt` (nginx, an
  app log, syslog, Apache, a Windows CSV export, mail, an edge proxy
  with epoch times, Tomcat and a factory line with ISO week dates).
  Each source says where its timestamp is and how to parse it, and the
  offset to assume for naive times. The program reports failed lines
  and lines that go back in time, then prints all 29 events on one UTC
  timeline with the gaps between them, a per-minute histogram and the
  span.

Not supported: locales other than C, `tzinfo` classes other than fixed
offsets (no `zoneinfo`, no DST), `date` and `time` arithmetic beyond
what the log normalizer needs, and `timedelta` with float fields.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with the real `datetime`, run under `LC_ALL=C` and
`TZ=UTC` from this directory after `mk.py` writes `data/`.

## Edits from mini_datetime.orig.kara to mini_datetime.kara

Compiler gaps:
- B-2026-10-06-107: `MONTHS[m].to_string()` on a `const` array at a
  non-literal index fails with `no method 'to_string' on MONTHS[…]`.
  The port binds the element to a local first.

Author errors:
- The structs were named `TD` and `DT`; struct names must be
  PascalCase, so they are `Delta` and `DateTime`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
