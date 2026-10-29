# mini_read_sql

A Kāra port of [pandas](https://pandas.pydata.org/) 3.0.5's SQL I/O
(BSD-3-Clause) over Python's `sqlite3` and the SQLite 3.45 engine under
it (public domain): `read_sql_query`, with `chunksize`, `get_schema`,
and `DataFrame.to_sql`. SQLite files are everywhere data sits on a
device or in an app: a browser's history, a phone's messages, a sensor
gateway's log, a desktop app's ledger, the export a vendor ships
instead of a CSV. Reading one with pandas means running SQL, so the
port is a SQLite reader and a SQL engine as well as the pandas layer.
Jobs come one per line:

```
read shop.db index_col=id parse_dates=signup :: SELECT id, name, signup, balance FROM customers WHERE country = 'DE' ORDER BY signup
read shop.db params='shipped',13 :: SELECT id, placed, total_cents FROM orders WHERE status = ? AND customer_id = ? ORDER BY id
chunks sensors.db chunksize=250 head=2 parse_dates=ts:s :: SELECT device, ts, temp_c FROM readings
tosql shop.db name=daily parse_dates=day index_label=row :: SELECT date(placed) AS day, ... ;; SELECT sql FROM sqlite_master WHERE name = 'daily'
```

| Job | The Python call |
|---|---|
| `read` | `pd.read_sql_query(sql, con, ...)` with `index_col`, `params` (positional or named), `parse_dates` (a column, a format, or a unit), `dtype` and `dtype_backend`; it prints the shape, the index, the dtypes, then `to_csv` (`head=n` prints only the first n rows) |
| `chunks` | the same with `chunksize`, one frame per chunk |
| `schema` | `pd.io.sql.get_schema(df, name, keys=..., dtype=...)` of the frame the query returns |
| `tosql` | reads the first query, writes the frame back with `df.to_sql(name, ...)` (`if_exists`, `index`, `index_label`, `dtype`) into a copy of the database held in memory, prints the row count pandas returns, then reads each later query from that copy |

## Running it

The program carries its inputs: it writes `data/` and runs
`data/jobs.txt`, 108 jobs over 8 files. `mk.py` makes the databases with
Python's `sqlite3`, so the bytes are SQLite's own:

- **shop.db** (1 KiB pages): customers with a `COLLATE NOCASE UNIQUE`
  email, products in a `WITHOUT ROWID` table with `DECIMAL` prices, 300
  orders with coupons in mixed case, order items, two indexes and a view.
- **history.db**, laid out like Chromium's: `AUTOINCREMENT` (so
  `sqlite_sequence`), a deleted row, and times in microseconds since
  1601.
- **sensors.db** (512-byte pages, so overflow and deep trees): 960
  readings from four devices and one unknown one, with missing values
  and a `-999` sentinel.
- **ledger16.db**, a personal-finance export in **UTF-16le**, with
  amounts as the text a CSV import leaves (`-12.30`, `-12`, `-1,234.00`),
  dates in two formats, and payees that differ only in case under
  `COLLATE NOCASE`.
- **mixed.db**: one column of each declared type holding the same twenty
  values, so the affinity rules show, a table loaded from a CSV with every
  column `TEXT`, and a `WITHOUT ROWID` key-value table.

The other three are the errors people meet: a file that is not a
database, an empty file, and a file that is not there. Each job prints
`$ <job>`, then what Python prints, or `! ` and the exception (15 of the
108: SQL errors with SQLite's messages, a binding count, an `index_col`
that is not a column, an integer cast over missing values, a table that
already exists, and integer overflow).

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_read_sql.kara
python3 -I -B oracle.py > expected.out
karac run mini_read_sql.kara | diff - expected.out
```

`oracle.py` is the oracle, and it is pandas 3.0.5 on Python 3.13's
`sqlite3` (SQLite 3.45.1).

## What SQLite and pandas do, and so the port does

These are their answers, kept because `expected.out` is theirs:

- **Affinity decides the stored type.** `'42'` in a `NUMERIC` or
  `INTEGER` column is stored as 42, `'3.50'` as 3.5, `' 12 '` and
  `'0x1A'` stay text; a column with no declared type keeps what it was
  given. Comparisons apply affinity too, so `t = 42` on a `TEXT` column
  is true and `units > 100` on a `TEXT` column compares text.
- **`sum` over text** reads a numeric prefix (`'-1,234.00'` sums as
  `-1`), and `sum` past `i64` is an `integer overflow` error where
  `total` gives a float. Sums of floats are Kahan-Babuska-Neumaier
  compensated, so they differ from a plain loop in the last digit.
- **Floats print as SQLite prints them**: `printf` and the text of a
  float use SQLite's own decimal conversion in x87 80-bit long double
  arithmetic, which the port carries, so `printf('%.20f', 2.675)` is
  `2.67500000000000000000` where Python prints `2.67499999999999982236`.
- **A bare column in an aggregate query** comes from the first row of
  the group, or from the row `min`/`max` picked, so `GROUP BY coupon
  COLLATE NOCASE` names the group `spring10` after its first order.
- **Windows** run their definitions last first, each a stable sort of
  the rows the previous one left, and a `ROWS` frame removes a row after
  emitting the current one.
- **Dates** follow `date.c`: `date('2026-01-31', '+1 month')` is
  `2026-03-03`, modifiers apply in order, and a time with no date is
  2000-01-01.
- **pandas infers each column from its values**: integers with a null
  become `float64`, and a mix becomes `object`, as the `BOOLEAN` column
  holding `1`, `'yes'` and nulls does; with `dtype_backend=numpy_nullable` they become `Int64`,
  `Float64` and `string`. `parse_dates` with a unit reads epoch numbers,
  and pandas 3 gives ISO text `datetime64[us]`.
- **to_sql** creates the table with pandas' own types (`INTEGER`,
  `REAL`, `TEXT`, `TIMESTAMP`), an `index` column and an `ix_` index
  unless `index=False`, writes datetimes as `isoformat(" ")` text and NaN
  as `NULL`, and fails on an existing table unless told to `append` or
  `replace`.

## Parallel work

Every job runs in its own `TaskGroup` task and the results join in
order, so the transcript is the same on every run and thread count.
The program matches all 1,586 lines of `expected.out` on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` (no reports) and a `karac build`
binary, at `KARAC_THREADS=1` and `4`, and on five runs of the auto-par
build (0.15 s real or less). `mini_read_sql.orig.kara` is the program
before the one-line workaround below.

## The code, ported

About 9,200 lines before the data section, about 1,300 of them reused:
from **mini_sqlite**, the page, cell, overflow and record reader; from
**mini_read_xml**, the text helpers, Python's `repr` of floats and bytes,
the civil-date arithmetic and pandas' `to_csv` and frame printing.

The new code is the rest of SQLite and the pandas SQL layer:

- the schema (`sqlite_master`, `CREATE TABLE` column types, constraints
  and defaults, `WITHOUT ROWID` key order, views), UTF-16 databases, and
  `PRAGMA table_info`;
- the tokenizer and a parser for `SELECT` with joins, subqueries, CTEs,
  compound selects, `CASE`, `CAST`, `COLLATE`, `IN`, `EXISTS`, `LIKE`,
  `GLOB`, window definitions and bound parameters, with SQLite's error
  messages;
- name resolution, affinity and collation rules, and SQLite's evaluator:
  three-valued logic, integer overflow into floats, the scalar functions
  of `func.c`, `printf`, `date.c`, aggregates with `DISTINCT` and
  `FILTER`, `GROUP BY`, `HAVING`, windows with frames, `ORDER BY`,
  `LIMIT` and `OFFSET`;
- SQLite's float and text conversions in emulated x87 long double
  arithmetic;
- pandas' column inference, `dtype` casts and their errors,
  `parse_dates`, `index_col`, chunks, `get_schema` and `to_sql`.

## The Mend loop

`karac check` first stopped at parse errors, two habits of mine from
Rust: `mut t` in an `if let` pattern, which Kāra does not have, and `!x`
on an integer, where Kāra's bitwise not is `~x`. I fixed those by hand;
`karac fix` would have too, rebinding with `let mut t = t;` and writing
`~` for an integer operand, though the message and the JSON
`replacement` of `karac check --output=json` still say `not`
(B-2026-10-29-29). `karac fix` then applied 11 machine-applicable fixes (`own`
parameters and `.clone()` calls); for one parameter it chose
`mut ref Vec[Cte]` where the function keeps the vector, and `own` was
the right call. The interpreter run then failed with invalid MIR
(B-2026-10-29-28, below); after the workaround, one job differed from
`expected.out`, the bare-column rule above, which was my port's
mistake.

## Gaps found

- **B-2026-10-29-28** (open): `r | (255 << b)` with `r: u64` and
  `b: i64` passes `karac check` and then fails as invalid MIR on every
  backend ("binary operands of different types u64 and i64"). The
  unsuffixed literal on the left of a shift by a variable is typed `i64`
  before the context reaches it, so `r + (255 << b)` is refused as well.
  Worked around in `bit_op` by naming the mask as a `u64`.
- **B-2026-10-29-29** (open): for `!x` on an integer, the message and
  the JSON replacement of `karac check` say `not`, which then fails
  typecheck; `karac fix` itself writes the right `~x`.

## What reading SQL would need (notes for M4c)

Compared with `docs/library/data.md` in the kara repo, which reads CSV
and Arrow IPC:

- **A database reader**: `read_sql(conn, query; params)` beside
  `read_csv`, and a `Connection` for SQLite at least. SQLite files are
  the most common database on a device, and the data an app exports is
  often one.
- **Types come from the values, not the schema.** SQLite's declared
  types are hints, so a column can hold integers and text. The page
  should say what a reader does with a mixed column: an error, a
  `String` column, or a per-column type the caller gives. pandas turns
  it into `object`, which `data.md` has no element type for.
- **Nulls are native here.** An `INTEGER` column with a null is a null
  `i64` slot with `data.md`'s validity bitmap, not the `float64` pandas
  makes it; `numpy_nullable` is the behaviour to copy.
- **Dates.** Decision Q1 reads dates as `String`, but every database
  here stores them as text, epoch seconds or microseconds since 1601. A
  reader needs a date and timestamp type, with `parse_dates`'s format and
  unit forms.
- **Bound parameters**, positional and named, so a query is never built
  by pasting values into SQL.
- **Chunks**: a `chunksize`, or a `scan_sql` into a `LazyFrame`, for
  tables larger than memory.
- **A writer**: `write_sql(df, conn, table; if_exists, index, types)`,
  with the `CREATE TABLE` it issues documented, since pandas' choice of
  `TIMESTAMP` text and `REAL` for nullable integers surprises people.
- **Errors**: SQLite's message, the statement and the binding count
  belong in one `DataError` variant the page could name.
