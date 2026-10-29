# mini_xsv

A Kāra port of [xsv](https://github.com/BurntSushi/xsv) 0.13.0 by Andrew
Gallant (MIT or Unlicense), the Rust command line toolkit for CSV data.
Fourteen commands are ported from xsv's source, together with the
behaviour of the crates it builds on: `csv` (reading, quoting, length
checks and their error messages), `streaming-stats` (running mean and
variance, median, mode, cardinality, frequency tables, and how partial
results merge) and `tabwriter` (elastic tabstops for `headers`, `flatten`
and `table`).

| Command | Ported options |
|---|---|
| `count` | `-n` |
| `headers` | `-j`, `--intersect`, several inputs |
| `select` | the whole selection syntax: names, 1-based indexes, ranges either way, open ranges, `name[k]` for duplicate names, quoted names, `!` to invert |
| `slice` | `-s`, `-e`, `-l`, `-i`, `-n` |
| `search` | `-i`, `-s`, `-v`, `-n` (patterns run on `std.regex`) |
| `sort` | `-s`, `-N`, `-R`, `-n` |
| `frequency` | `-s`, `-l`, `--asc`, `--no-nulls`, `-j`, `-n` |
| `stats` | `--everything`, `--median`, `--mode`, `--cardinality`, `--nulls`, `-s`, `-j`, `-n` |
| `join` | inner, `--left`, `--right`, `--full`, `--cross`, `--no-case`, `--nulls` |
| `fixlengths` | `-l` |
| `flatten` | `-c`, `-s`, `-n` |
| `table` | `-w`, `-p`, `-c` |
| `fmt` | `-t`, `--crlf`, `--ascii`, `--quote`, `--quote-always`, `--escape` |
| `cat` | `rows`, `columns`, `--pad`, `-n` |

`-d` and the `.tsv` extension choose the input delimiter everywhere.

## Running it

The program carries its inputs: it writes `data/` (an order export, a
customer list, a TSV of regions, sensor readings that reach the corners
of xsv's number parsing, quoted and CRLF text, duplicate header names, a
ragged file, an empty file and a header-only file) and runs
`data/jobs.txt`, 87 xsv command lines. Each job prints `$ xsv ...`, then
what the command wrote. An error prints after the output the command had
already written, as xsv's buffered CSV writer flushes it on the way out,
each line after `! `, then `[exit 1]`; output that ends without a newline
(the record a writer refused is already in its buffer) is followed by
`\ no newline at end of output`. A carriage return in the output (`fmt --crlf`) is
shown as `^M`, as `cat -v` shows it, so the transcript stays plain text.

```
python3 -s -P -B mk.py          # writes data/ and data/jobs.txt
python3 -I -B gen_kara.py       # copies data/ into the end of mini_xsv.kara
python3 -I -B oracle.py > expected.out
karac run mini_xsv.kara | diff - expected.out
```

`oracle.py` is the oracle: a Python mirror of the same commands written
from xsv's Rust source, including Rust's integer and float grammars
(`+7`, `1.`, `.5`, `2.5E-2` are numbers; ` 12` is not) and Rust's float
`Display` (no exponent, no `.0`), which the stats columns print.

## Parallel work

`stats -j N` and `frequency -j N` cut the records into xsv's chunks
(`n / N` records each) and compute each chunk in its own `TaskGroup` task;
`stats` also renders each column's line in its own task, as xsv does with
its thread pool. The chunks merge in file order, so the output is the same
on every run and every thread count. The program was checked at
`KARAC_THREADS=1` and `4` with auto-par on.

## Where it differs from xsv, on purpose

- **Frequency ties.** xsv sorts counts out of a hash map, so equal counts
  come out in hash order; here they come out by value.
- **`-j` without an index.** xsv parallelises only an indexed file (`xsv
  index`, not ported) and merges chunks in the order they finish; here
  `-j N` acts as if the file were indexed and merges in file order. The
  merge is xsv's: a chunk with no numbers in a column merges as `0/0`, so
  a column whose first chunk is all empty reports `NaN` for its mean, as
  xsv's would.
- **Arguments** are parsed by a small table of each command's flags, not
  docopt; no job relies on docopt's own error messages.

## xsv's own quirks, kept

- `frequency -s product,customer` labels the tables in selection order but
  counts them in column order, so the two labels swap.
- `headers --intersect` keeps the first of each name: a union.
- `sort -N` stops comparing at the first field that is not a number, and
  puts it first.
- `stats --mode` reports `N/A` for a column whose values are all distinct,
  and the type of a column is decided sample by sample, so a sum or a
  minimum can depend on which type the column had when a value arrived.

## The Mend loop

The first `karac check` stopped at a parse error: a closure written with a
Rust return type (`|k: Str| -> Result[..] { .. }`). `karac fix` removed it;
a second `karac fix` applied the five fixes the full check then offered
(`.clone()` on two field reads and one record, `.into_iter()` on two loops
that join task handles, and a `*` on a copied count). Nothing was fixed by
hand. `mini_xsv.orig.kara` is the program as first written.

## Gaps found

- **B-2026-10-28-23** (fixed in ed940b7730): the closure the fix left
  behind passed `karac check` but did not build on MIR, because nothing
  fixed the error type its `?` converts into. `slice` read its flags
  through a named method until the fix; it is back to the closure.

## What a dataframe API would need (notes for M4c)

What this program had to build by hand, each a candidate for a data
library: selecting columns by name, index and range (with duplicate
names); a per-column type inferred from text; a mergeable column summary
(count, sum, min, max, lengths, mean, standard deviation, median, mode,
cardinality) that splits across tasks and merges; a group-by count;
joins of four kinds on trimmed or case-folded keys; a stable sort on
several columns, by text or by number; filtering rows by a regex over
chosen columns; and concatenating frames by rows or by columns, padded.
