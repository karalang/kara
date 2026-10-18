# mini_zoneinfo

Time zones as Python 3.13's `zoneinfo` handles them, read from real
TZif files. Calendars, schedulers, booking systems, log tools and
anything that shows a time to a person in their own zone read the
system's tzdata and convert between UTC and wall time. The program
ports `zoneinfo`'s loader and its conversion rules (the pure-Python
implementation, `zoneinfo._zoneinfo`), and builds a transition report
and a meeting planner on top.

The run has five parts:
- **zones**: 25 TZif files in `data/zones`. Eleven are copied from the
  system's tzdata: New York, London, Lord Howe (a 30-minute DST),
  Kolkata, São Paulo (DST abolished in 2019), Apia (skipped a day in
  2011), Chatham (+12:45), UTC, Casablanca, St John's and Troll (a
  two-hour DST). The other fourteen were made by `mk.py`:
  - a version 1 file and a version 3 file;
  - a zone with only a footer, and a zone with negative DST;
  - zones with Julian and plain day rules and with indicator bytes;
  - a DST type whose offset has to be guessed;
  - one zone for each error the loader reports.

  For each file the program prints the types with the DST offsets
  `zoneinfo` infers, the first and last transitions, the type used
  before the first transition, and the rule used after the last one.
- **tz strings**: 30 POSIX TZ strings with `M`, `J` and plain day rules,
  negative and over-24-hour transition times, `<...>` names and seconds
  in offsets. For each one the program prints the parsed rule and its
  2024 and 2025 transitions, or `_parse_tz_str`'s `ValueError`.
- **conversions**: 56 conversions:
  - from UTC to local time with `fromutc`'s fold detection;
  - from wall times with fold 0 and 1 to UTC, in gaps and overlaps,
    before the first transition and far past the last.

  When `fromutc` and `utcoffset` disagree, as they do just after a
  zone's last transition, the program prints what Python prints.
- **transitions**: each zone's transitions in chosen years. They are
  found by stepping `fromutc` and bisecting to the second, and each one
  is printed with the wall clock before and after.
- **meetings**: four weekly meetings converted from the organizer's zone
  to the attendees'. Each occurrence that falls in a DST gap is moved
  and flagged, and each that happens twice is flagged. One series
  crosses Apia's skipped day.

Not supported: `ZoneInfo(key)` lookup on `TZPATH` and the cache (the
program loads every file from `data/zones` by path), leap-second
records, `datetime.time` objects, and pickling. The report and the
planner are the program's own and are the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `zoneinfo._zoneinfo` and `datetime`. Run it from
this directory after `mk.py` writes `data/`. `mk.py` copies its eleven
real zones from `/usr/share/zoneinfo`, so it reproduces `data/` exactly
only with the tzdata the files were taken from (2026c, Ubuntu 24.04).

## Edits from mini_zoneinfo.orig.kara to mini_zoneinfo.kara

The `std_of` helper that worked around B-2026-10-14-70 was dropped after
the fix in 0bacd9912b; the port calls `z.after.std.clone()` directly, as
the original did.

Author errors:
- A closure was written with a return type, `|a, b| -> i64 { .. }`,
  which is Rust syntax. `karac check` said so and named the fix.
- `load_data` and `fixed_after` store their parameters, so the
  parameters have to be `own`.
- The days before a year were counted from ordinal 719162 instead of
  `EPOCHORDINAL`, 719163, which put every rule-based transition a day
  late.
- The TZ-string parser checked an offset's hours before the whole
  string had matched, so `EST1234EDT` gave "Invalid STD offset" instead
  of "not a valid TZ string".
- The local time's offset, DST and name were taken from `fromutc`'s
  result. Python takes them from `utcoffset()` on the wall time it
  produced, and the two differ just after a zone's last transition
  (`Test/Guess`).

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
