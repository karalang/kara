# mini_sqlite

A reader for SQLite database files, written from SQLite's file-format
document and checked against SQLite itself. Backup tools, forensic and
recovery tools, data exporters and sync agents read `.db` files without
going through the SQLite library, and they have to get the format's
details right: varints, overflow chains, the REAL-affinity rule, columns
added with `ALTER TABLE`, UTF-16 databases, freelists and pointer maps.

The program opens three files made by Python's `sqlite3` (SQLite 3.45):
- **data/shop.db**: 1 KiB pages, 133 of them. A `customers` table with
  an `INTEGER PRIMARY KEY`, a `UNIQUE` email, long notes that spill
  onto overflow chains, Unicode names, and three columns added later
  with `ALTER TABLE ... DEFAULT`, so older rows are shorter than the
  table. An `orders` table with every integer size from 0 and 1 to
  `-2**63`, blobs, NULLs and a rowid of `2**40`, plus an index on
  `(customer_id, placed)`. A `WITHOUT ROWID` key-value table that a
  trigger fills, a view, and deleted rows that leave free pages.
- **data/notes16.db**: 512-byte pages, UTF-16le text with emoji, and
  `auto_vacuum = FULL`, so it has a pointer-map page.
- **data/bad.db**: a CSV file, which is not a database.

For each one it prints:
- the header: page size, page count, encoding, auto-vacuum mode,
  schema and user versions, application id, writer version and the
  number of free pages;
- the schema table;
- every page, as SQLite's `dbstat` reports it: b-tree pages with their
  name, path, cell count, payload bytes, unused bytes and largest
  payload, overflow pages with the same, and the free and pointer-map
  pages;
- the pointer map, entry by entry, for the auto-vacuum database;
- the rows of every table, decoded from their records;
- the entries of every index, including the automatic one SQLite makes
  for the `UNIQUE` column;
- for shop.db, lookups that descend the b-trees rather than scanning
  them: customers by email through the `UNIQUE` index, each customer's
  orders through the two-column index, and rowid ranges; then a report
  of orders per country, joined in the program.

Not supported: WAL files and hot journals, the `sqlite_stat` tables,
collations other than `BINARY`, generated columns, and incremental
vacuum beyond reading its flag. The reader trusts the file it reads;
it does not check a damaged database the way `PRAGMA integrity_check`
does.

**Oracle.** `expected.out` is the output of `oracle.py`, which asks
Python's `sqlite3` the same questions: `PRAGMA`s for the header,
`SELECT`s for the rows, index entries and lookups, and the `dbstat`
virtual table for the pages. It derives each pointer-map entry from
`dbstat`'s page paths. Run it from this directory after `mk.py`
writes `data/`.

## Edits from mini_sqlite.orig.kara to mini_sqlite.kara

Compiler gaps:
- B-2026-10-15-57: the MIR borrow checker's memory use grows faster
  than quadratically with the length of a function. The data generator
  first wrote each database as a function of 48-byte `push_str` calls,
  about 2,800 of them for shop.db, and `__mir-run` reached 12 GB before
  it was stopped. The generator now writes each file as one block of
  hex lines, which `unhex` reads past the line breaks.
- B-2026-10-15-58: `f64.from_bits(bits)` is refused, and nothing points
  to the library's `bits.bits_as_f64()` on an `i64`, which the port now
  uses for SQLite's 8-byte REAL.

Author errors:
- `let b = db.data;` in three functions moved the file's bytes out of
  a borrowed `Db`; they are `let b = ref db.data;`.
- `f64.parse` returns an `Option`, not a `Result`.
- `for g in mut groups` is not Kāra; the loop indexes the `Vec`.
- A bare `for` element was pushed into a `Vec` without `.clone()`.
- The struct named `Column` shadowed the prelude type (a warning); it
  is `ColumnDef`.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.

Reverted 2026-10-15, karac at 3385ce51b1: B-2026-10-15-57 is fixed, so
the test databases are the original functions of 48-byte `push_str`
calls again, with the original `unhex`. `__mir-run` now runs the program
in 5 s and 212 MB, and the app still matches on all four MIR surfaces.
`karac check` on it takes about 42 s and 1.8 GB, which the drop schedule
thread puts in the frozen legacy auto-par conflict graph.
