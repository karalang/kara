# mini_cmd

Python's `cmd.Cmd`: the framework behind line-oriented consoles such as
pdb and many admin shells and test harnesses, with an inventory console
built on it. The prompt-and-read loop, how a line becomes a command word
and an argument, `?` and `!`, an empty line repeating the last command,
the end of input as the `EOF` command, the help listing with its
docstrings and columns, and what Tab would complete.

The program writes its own data (`data/sessions/*.txt`), runs a console
over each session in name order and prints the transcript. `oracle.py`
runs the same sessions through the vendored `cmd.py` from CPython 3.13
(`vendor/`, PSF licence) and is the reference; `mk.py` writes the data
and `gen_kara.py` embeds it in the program.

## The console loop (the module's)

- Each session prints `==== <file>`, then the intro line, then reads its
  file as stdin. Before each read the prompt `(inv) ` is written with no
  newline (the input is not echoed, so the next output follows it on the
  same line). A line loses every trailing `\r` and `\n`; at the end of
  the file the line is `EOF`. Each line is recorded in the history (the
  console's `precmd`) and counted (its `postcmd`); the command's result
  says whether to stop. At the end the console prints
  `[<n> lines handled]`. The stock and the history carry over from one
  session to the next; the last command does not.
- A line is stripped of surrounding whitespace. Empty, it repeats the last
  command (nothing when there is none). A leading `?` becomes `help `, a
  leading `!` becomes `shell `. The command word is the leading run of
  ASCII letters, digits and `_`, so `set-price` is the command `set` with
  the argument `-price ...`; the argument is the rest, stripped. A line
  whose word is empty, or names no command, prints
  `*** Unknown syntax: <line>`. Every line with a word becomes the last
  command, except `EOF`, which clears it.
- `EOF` prints an empty line and ends the session; so does the end of the
  file. `quit` prints `bye` and ends it. Lines after the end are not read.

## Help (the module's)

- `help NAME` calls the `help_NAME` topic when there is one (`EOF`, `add`,
  `list`, `pricing`); otherwise prints the command's docstring cleaned as
  `inspect.cleandoc` does (tabs expanded, the common margin of the lines
  after the first removed, the first line's leading spaces removed, blank
  lines at either end dropped), unless the command has none or it cleans
  to nothing; then `*** No help on NAME`. The docstrings are what Python
  3.13 compiles them to, which already strips their indentation and
  expands a tab in `set`'s; `ping`'s is a newline and spaces, so it is
  listed as documented and prints a line of spaces.
- `help` alone prints the doc leader, then three sections: documented
  commands (those with a help topic or a docstring), the help topics that
  are not commands, and undocumented commands, each in `dir()` order
  (`EOF` sorts before the lower-case names). A section with entries is its
  header, a line of `-` as long as it, the entries in columns of width 79,
  and an empty line.
- Columns (`Cmd.columnize`): no entries print `<empty>`, one prints
  itself; otherwise the fewest rows, from 1 up, whose columns (filled top
  to bottom, each as wide as its widest entry, two spaces apart) fit the
  width; when no row count below the number of entries fits, one entry
  per row. Trailing empty cells are dropped and each cell is padded to
  its column's width, counted in characters.

## The inventory (the port's own)

- Prices are cents, written `D`, `D.C` or `D.CC` with one to seven ASCII
  digits before the point; printed `D.CC`. Quantities are one to seven
  ASCII digits.
- `add NAME QTY [PRICE]` adds QTY to the item (a new one costs 0.00) and
  replaces its price when one is given, printing `<name>: <qty> at
  <price>`; else `usage: add NAME QTY [PRICE]`, `bad quantity: <qty>` or
  `bad price: <price>`.
- `set NAME qty|price VALUE` changes one field and prints as `add` does;
  else `cannot set: <repr of the argument>`, `no item <repr>`, or the
  bad quantity or price message.
- `remove NAME` prints `removed <name>` or `no item <repr>`. `show NAME`
  prints `<repr>: qty=<q> price=<p>`, zeros for a missing item.
- `list [PREFIX]` prints the items whose names start with PREFIX, in name
  order, as the name left-aligned in 12 characters, the quantity
  right-aligned in 7 and the price right-aligned in 10; `(nothing)` when
  none does. `total` prints `total <value> over <n> items`.
- `history` prints every line read so far as its number right-aligned in
  3, a space and the line's Python `repr` (a tab as `\t`).
- `ping [TEXT]` prints `pong`, then a space and TEXT when there is one.
  `shell TEXT` (and `!TEXT`) prints `(not running) <text>`.
- `columns WIDTH WORD...` lays the words out at that width as help does;
  `usage: columns WIDTH WORD...` when WIDTH is not digits.
- `complete LINE` prints what Tab would offer at the end of LINE (the
  argument, already stripped): the text being completed is what follows
  the last space. At the start of the line it completes command names; in
  a `help` argument, command names and help topics; anywhere else
  nothing. It prints `<repr of the text>: <matches>`, sorted and
  space-separated, or `(none)`.
