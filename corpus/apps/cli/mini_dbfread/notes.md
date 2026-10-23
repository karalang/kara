# mini_dbfread

A port of [dbfread](https://pypi.org/project/dbfread/) 2.0.7 (MIT; Ole
Martin Bjørndalen, UiT): the reader that data-migration scripts, GIS
tools and ETL jobs use to pull records out of dBase, FoxPro and Visual
FoxPro `.dbf` files and their memo files.

```
> ledger data/legacy/orders.dbf data/out/ledger.txt
  wrote data/out/ledger.txt
> cat data/out/ledger.txt
  data/out/ledger.txt (5 lines)
  | customer     orders            total         unpaid  last order
  | Acme              3         124.9321         0.4321  2024-03-02 01:00
  | Bolt & Co         2       12355.6788         9.9999  2024-02-29 17:00
  | Cirrus            2             3000           3000  2024-03-03 12:34
  | all               7       15480.6109
```

## What it does

`mk.py` writes 45 legacy database files to `data/legacy/` and a script of
417 steps to `data/jobs.tsv`. `oracle.py` replays the script against the
vendored library and `mini_dbfread.kara` replays it against the port. An
exception prints as `error <class>: <message>`; 79 steps end in one.

- **Tables from every era**: dBase III with and without a `.dbt` memo,
  dBase IV memos, FoxPro and Visual FoxPro with `.fpt` memos and
  autoincrement, a VFP table with `Varchar` fields, and tables in
  cp437, cp850, cp865, cp866, cp1251, cp1252, Mac Roman, ASCII and UTF-8,
  chosen from the language driver byte or given as `encoding=`.
- **Opening**: the header and field descriptors, `lowernames`, two-digit
  years, case-insensitive lookup of the table and its memo file (`ifind`,
  with its `[Pp][Ee]...` patterns), `DBFNotFound`, `MissingMemoFile` and
  `ignore_missing_memofile`, and which memo file wins when a table has
  both.
- **Records**: `records`, `deleted`, `len()`, `load()` and `unload()`,
  `raw=True`, `char_decode_errors` (strict, replace, ignore,
  backslashreplace) and `recfactory` (`OrderedDict` by default, `dict`,
  and `None` for a list of pairs).
- **Every field parser** (`C V D F N I + L M O T @ Y B G P 0`), each fed
  good and bad bytes through `parse`: Python's `int()` and `float()`
  grammar for numbers, Julian day numbers for timestamps with their
  overflow errors, `Decimal` currency, and memo blocks.
- **Broken files**: an unknown field type, wrong field lengths, a header
  cut short, no field terminator, no end-of-file marker, padded records,
  a truncated memo file, an undecodable field name and bad values in
  every type. A parse error leaves the records read before it.
- **Parser subclasses**: one that keeps bad values as `InvalidValue`,
  one that reverses strings, and one that adds a hex field type `X`,
  as in the library's examples.
- **Programs** after the library's examples (`dbfinfo.py`,
  `print_invalid_values.py`, `print_csv.py`, `dbf2sqlite`), plus a JSON
  export and a ledger that sums orders by customer with `Decimal`. The
  SQL is written as text, and each program prints the file it wrote.

## How the port works

- **Bytes are read with the library's cursor.** Records are found the
  way `_iter_records` finds them, one record length at a time from the
  header length, so a table whose header and field lengths disagree
  drifts exactly as it does in Python, and `len()` counts the same way.
- **The codecs are written out.** The charmap tables come from Python's
  own codecs, and UTF-8 decoding reports CPython's error ranges and
  messages, so `replace` and `backslashreplace` give the same text.
- **Values are a small enum** covering `None`, `bool`, `int`, `float`,
  `str`, `bytes`, `date`, `datetime` and `Decimal`, with Python's
  `repr()` and `str()` for each, so a record prints as the library's
  `OrderedDict` would.
- **Python's text reading is modelled where the script relies on it.**
  `cat` reads with universal newlines, so a memo's `\r\n` comes back as
  `\n` in the CSV and SQL files it shows.

## Compiler gaps

None. `karac check` asked for two `.clone()` calls where the port matched
on an `Option[String]` field of a borrowed table, which is the language's
ownership rule rather than a gap. The one output difference on the first
run was a port bug (universal newlines in `cat`), and after that fix the
output matched the oracle on every backend.

## Scope

- Reading only, as in the library. The `dbf2sqlite` program writes SQL
  text instead of a database file.
- Not covered: the `namedtuples`, `pandas` and `dataset` examples.
  `str` printability follows `str.isprintable` for the characters the
  files contain.

## Running

```bash
karac run mini_dbfread.kara                 # interpret
karac build --mir mini_dbfread.kara         # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded inputs
```
