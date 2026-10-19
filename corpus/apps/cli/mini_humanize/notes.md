# mini_humanize

humanize 4.16.0, the library that turns numbers, file sizes and time spans
into the words a person reads, as CLIs, dashboards and Django templates
use it ("3.2 MB", "1.2 million", "2 hours ago", "1st"). These parts of
`humanize/number.py`, `filesize.py`, `time.py`, `lists.py` and `i18n.py`
are ported:
- `ordinal`, `intcomma`, `intword`, `apnumber`, `fractional`,
  `scientific`, `clamp` and `metric`;
- `naturalsize` in decimal, binary and GNU styles;
- `naturaldelta`, `naturaltime`, `precisedelta` (minimum units,
  suppressed units, formats), `naturalday` and `naturaldate`;
- `natural_list`;
- `activate` and `deactivate`, which load humanize's own gettext catalogs.

Python 3.13 is ported alongside, as far as humanize needs it: gettext's
`.mo` loader for both byte orders, its plural-rule compiler and fallback
chains, the locale name expansion that `gettext.translation` does,
`%`-formatting and format specs, `float()` and `int()` on strings,
`timedelta(seconds=x)`'s rounding, `divmod` on floats,
`Fraction.limit_denominator` and `strftime`.

The library's quirks are kept. Some examples:
- `ordinal(-21)` is `'-21th'`, because Python's `%` makes the last digit 9.
- `intcomma("1e5")` returns `'1e5'`. The string passes the `float()`
  check, but `int()` refuses it, and humanize then returns the string.
- `intword(999999)` is `'1.0 million'`. 999.999 thousand rounds to 1000.0,
  which equals the next power, so the unit steps up. `naturalsize(999999)`
  gives `'1.0 MB'` in the same way.
- `naturaldelta(3599)` is `'an hour'` and 729 days is `'2 years'`, because
  of the rounding steps.
- `naturaldelta(float("inf"))` raises `OverflowError`. humanize only
  catches `ValueError` and `TypeError`.
- The separators are keyed by the exact name given to `activate`.
  `activate("de")` finds the de_DE catalog through gettext's name
  expansion, but numbers keep English separators (`'3.0 Millionen'`).
- Catalogs stay cached after loading. A catalog file deleted after its
  locale was activated still works; one deleted before activation fails.

The run writes `data/jobs.jsonl` and the catalogs under `data/locale/`.
Fifteen catalogs are copied from the installed humanize. Two more are
made by hand: a big-endian `de` catalog that de_DE falls back to (de_CH
finds only it), and a corrupt `xx_XX` file. The run then does the 2961
jobs, each printed after `== ` and the job:
- `activate`, `deactivate` and `remove` jobs change the language or
  delete a catalog, then print `ok` or the exception;
- each `call` job calls one humanize function and prints the `repr()` of
  the result, or the exception it raised.

The call jobs run every function in eight languages: English, German,
Russian, Arabic, French, Polish, Japanese and Latvian. These cover seven
plural rules and four pairs of separators. Then 25 locale names are
activated, among them aliases, territories, a codeset, a modifier, `C`,
missing catalogs and the corrupt one.

`oracle.py` runs the same jobs through the real humanize
(`pip install humanize==4.16.0`, CPython 3.13) and wrote `expected.out`.
Dates passed to `naturalday` and `naturaldate` are far from today, so the
output does not depend on the day it runs. Both programs read the real
clock for "today".

Limits of the port:
- Ints in jobs stay below 2^127, so no `intword` value reaches a googol.
- The locale alias table has only the names the jobs use.
- `strftime` covers `%a %A %b %B %d %j %m %y %Y %%`.
- Aware datetimes are not handled.

Author errors:
- a `mut` inside a tuple pattern again (`let (mut years, d) = ..`; the
  `mut` goes before the whole pattern);
- unannotated arrays of string literals (`let names = [("a", "b")]`) are
  `String`s, so returning one as `(Str, Str)` is refused;
- `x.powf(y)`: Kāra's float method is `pow`.

Compiler gaps:
- `x.powf(y)` is refused with only "no method 'powf' on type 'f64'". The
  error does not point to `pow`, and docs/library does not list the float
  math methods (B-2026-10-19-3, Thread A).

The first full run matched the oracle except for one job, where my mk.py
passed an int above 2^63 that the port's JSON reader takes as i64.

Written 2026-10-19 by the real-world apps thread; karac at b6761f33ac.
