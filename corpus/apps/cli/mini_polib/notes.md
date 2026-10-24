# mini_polib

A port of [polib](https://github.com/izimobil/polib) 1.2.0 (MIT; David Jean
Louis), the pure-Python library for gettext catalogs: the PO parser and
writer, the MO reader and compiler, entry lookup, fuzzy flags, merging a
catalog against its template, and the helpers (`escape`, `unescape`,
`natural_sort`, `detect_encoding`). `polib.py` is vendored under `vendor/`
with its licence, and the oracle runs it unchanged.

```
== stats | data/de.po
88% translated; translated 24, untranslated 1, fuzzy 2, obsolete 3; 30 entries; header flags [], encoding UTF-8
== find | data/de.po | msgid | Open | verb
ctx='verb' id='Open' str='Öffnen' occ=1 first=('templates/catalog/detail.html', '40') line 132
== po | data/bad_quote.po
OSError: Syntax error in po file data/bad_quote.po (line 6): unescaped double quote found
== mo | data/rev2.mo
OSError: Invalid mo file, unexpected major revision number
```

## What it does

`mk.py` writes the catalogs of Larkfield, an invented library-loans web
application: a template (`messages.pot`, with a fuzzy header and
`charset=CHARSET`), German, French, Japanese (saved with a byte-order mark)
and Brazilian Portuguese translations at different stages, an older French
catalog in ISO-8859-1, a big-endian MO file from an older release, three
MO files with a wrong magic number or revision, and seven broken PO files.
It also writes `data/jobs.txt` (76 jobs). `oracle.py` runs each job on
polib and prints the result; `mini_polib.kara` carries the same files as
hex, writes them under `data/`, and runs the same jobs.

- **`detect`** runs `detect_encoding` on PO and MO files.
- **`stats`**, **`metadata`**, **`entries`** and **`po`** parse a catalog
  and print the counts and percentage, the ordered metadata, or every
  entry with its context, plural forms, flags, occurrences, comments,
  previous (`#|`) fields, obsolete mark and line number. On the broken
  files `po` prints polib's `OSError`, line number included.
- **`find`**, **`find-all`** and **`contains`** look entries up by
  `msgid`, `msgstr` or `msgid_plural`, with and without a `msgctxt`.
- **`roundtrip`** writes the catalog back and compares it with the file;
  polib rewraps long lines and reorders metadata, so three of the five
  differ, and the job prints the first line that does.
- **`print`** writes the catalog at wrap widths 0, 40, 60, 78 and 120.
- **`fuzzy`** turns an entry's fuzzy flag on or off.
- **`merge`** merges a translation against the template the way msgmerge
  does, saves it, and lists what became obsolete.
- **`sort`** orders entries by polib's `__cmp__`.
- **`compile`**, **`mo`**, **`mo-to-po`** and **`mo-save`** compile a
  catalog to MO, read MO files of either byte order, and convert back.
- **`escape`**, **`unescape`** and **`natural-sort`** call the helpers.

## How the port works

- **The parser** is polib's `_POFileParser` state machine: the same
  transition table, one handler per symbol, the same rules for when an
  entry closes, `#|` previous fields, `#~` obsolete entries, and the
  syntax errors with their messages and line numbers. After parsing, the
  header entry is found with `find('')`, removed, and turned into
  metadata and the header's flags.
- **Writing** follows `_BaseEntry.__unicode__` and `POEntry.__unicode__`.
  Long strings are wrapped with Python's `textwrap` rules (the
  `mini_textwrap` port is included, with `break_long_words=False` and
  `drop_whitespace=False` as polib calls it), comments and occurrences are
  wrapped too, and metadata comes out in polib's fixed order followed by
  the rest in natural sort order.
- **MO files.** `to_binary` sorts the translated entries by the UTF-8
  bytes of `msgctxt\x04msgid` and writes the little-endian layout with no
  hash table. The reader accepts both magic numbers and refuses a major
  revision other than 0 or 1.
- **Encodings.** `detect_encoding` reads the `charset=` with a hand-written
  version of polib's regex, and the codec lookup knows the aliases the
  catalogs use (`utf8`, `UTF-8`, `ISO-8859-1`, `latin-1`, `ascii`).
  Latin-1 is decoded and encoded by hand.
- **Python's `str`.** Slices count code points, `repr` follows CPython's
  quoting and escape rules, and `splitlines`, `strip` and `split()` use
  Python's definitions of whitespace and line breaks. An exception is a
  `PoErr` holding the class name and message, carried in `Result`.

## Compiler gaps

None. The port passed `karac check` after the ordinary fixes the
diagnostics asked for (iterating owned temporaries with `.into_iter()`,
`own` on a parameter that is stored, `.to_vec()` on a `bytes()` slice),
and printed the same output under all four backends on its first run.

## Scope

The port covers what the jobs exercise. Not ported: `pofile` and `mofile`
on a string instead of a path, the `check_for_duplicates` option, the
`klass` hook, a hash table in compiled MO files (polib never writes one
either), and the codecs polib could reach through Python that these
catalogs never name.

## Running

```
python3 -s -P -B mk.py                   # writes data/
python3 -I -B oracle.py > expected.out   # the oracle, on vendor/polib.py
python3 -I -B gen_kara.py                # refreshes data_files() in the port
karac run mini_polib.kara                # the port; prints the same
```
