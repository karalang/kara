# mini_csv

The CSV handling every data pipeline, spreadsheet export and import job
needs, as Python 3.13's `csv` module does it. The reader and writer are
ports of CPython's `_csv.c`, and DictReader, DictWriter and the Sniffer
are ports of `csv.py`.

The run writes its inputs into `data/` and prints:
- **read**: 22 reader jobs from `data/read-jobs.txt`, each a file and a
  dialect. They cover an Excel export with CRLF line ends, quoted commas,
  doubled quotes, a line break inside a field and a blank line; a TSV
  file; a semicolon file with decimal commas; `skipinitialspace`; escape
  characters with and without quoting; `QUOTE_NONNUMERIC`,
  `QUOTE_NOTNULL` and `QUOTE_STRINGS` turning fields into floats and
  `None`; no quote character at all; strict mode; a file that ends inside
  a quoted field; a carriage return in the middle of a line; and the
  field size limit. Each row is printed with the reader's `line_num`,
  and errors with `_csv`'s message;
- **write**: ten rows (numbers, `None`, delimiters, quotes, line breaks,
  backslashes, a single empty field, an empty row) written with 13
  dialects from `data/write-jobs.txt`: every quoting mode, escape
  characters, `doublequote=False`, other delimiters, quote characters
  and line terminators, and the space-delimiter rule. It shows the
  writer's errors (`need to escape, but no escapechar set`, `single empty
  field record must be quoted`);
- **dict**: DictReader with `restkey` and `restval` over ragged rows,
  with given field names, and with a duplicated column name; DictWriter
  with `restval` and both `extrasaction` settings;
- **sniff**: the Sniffer's dialect and `has_header` for 13 samples: CSV,
  semicolons with single quotes and doubled quotes, spaces after commas,
  tabs, pipes, `/etc/passwd` colons, a single column, numbers only,
  complex numbers and fixed-width codes, and a sample with no delimiter.
  Each sample is then read with the dialect found;
- **report**: an order export from a European shop system (semicolons,
  CRLF, decimal commas, a customer name with a line break in it) is
  sniffed, read with DictReader, cleaned into `data/out/orders_clean.csv`
  with `QUOTE_NONNUMERIC`, and summarized per country into
  `data/out/summary.csv` with DictWriter. Both files are printed and read
  back.

**The reader** is `_csv.c`'s state machine over each line, with the end
of each line fed in as its own event, and the input split as a file
opened with `newline=''` splits it (or, for one job, as a `StringIO` does,
which is how the carriage-return error is reached). The field size limit
counts characters.

**The writer** decides per field whether to quote or escape each
special character, as `join_append_data` does, and applies the rules for
a record that is a single empty field and for an empty field when the
delimiter is a space.

**The Sniffer** follows `csv.py` step by step. Its four quote-and-
delimiter regular expressions and the doubled-quote expression are
matched by hand, with their possessive bodies, so a match ends at the
first single quote. The fallback counts every ASCII character on every
line, takes the mode of each character's frequencies, and lowers the
consistency bar from 1.0 by 0.01 in floating point, as Python does.
`has_header` uses a port of `complex()`'s syntax to type each column.

Not supported:
- dialect names and `register_dialect`;
- `writerows`, `QUOTE_NONNUMERIC` writing of values other than numbers
  and strings, and reading from anything but a whole text.

**Oracle.** `expected.out` is the output of `oracle.py`, which builds
the same report with Python 3.13's `csv` module. Run it from this
directory after `mk.py` writes `data/`.

## Edits from mini_csv.orig.kara to mini_csv.kara

Two compiler gaps, both worked around and filed:
- `if c == EOL { /* nothing */ } else if ...` does not parse: the empty
  body after an uppercase name is read as an empty struct literal
  `EOL {}` (B-2026-10-15-46). The branch now ends with `return Ok(());`.
- `Dialect { quoting: Q, ..excel() }` is refused because the base is a
  call (B-2026-10-15-47). The three dialects now bind `let base =
  excel();` first.

The rest were author errors:
- `[..].to_vec()`: a bracketed list is already a `Vec`;
- `f64.parse` returns an `Option`, not a `Result`;
- three parameters that are stored needed `own`.

After that `karac check` passed, and the program matched expected.out
on the first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 35fcd8197c.
