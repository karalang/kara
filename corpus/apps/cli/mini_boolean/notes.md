# mini_boolean

A port of [boolean.py](https://pypi.org/project/boolean.py/) 4.0
(BSD-2-Clause; Sebastian Kraemer and contributors): the Boolean-algebra
library that parses expressions such as `a and (b or not c)`, simplifies
them and puts them in conjunctive or disjunctive normal form.
license-expression, and through it ScanCode's SPDX licence handling, is
built on it, and access rules and feature flags are written in it.

```
> licences licences/packages.txt licences/allowed.txt
  cryptolib: Apache-2.0&(MIT|BSD-3-Clause)&ISC
    simplified: Apache-2.0&ISC&(BSD-3-Clause|MIT)
    choice Apache-2.0 + BSD-3-Clause + ISC -> allowed
    choice Apache-2.0 + ISC + MIT -> allowed
```

## What it does

`mk.py` writes a lock file's package licences and an allow-list, access
rules and users to `data/`, and a script of 713 steps to `data/jobs.tsv`.
`oracle.py` replays the script through the vendored library and
`mini_boolean.kara` replays it through the port. Expressions print as
`repr()` and `str()` show them, and an exception as
`error <class>: <message>`.

- **The documentation's examples**, then **the grammar**: every spelling
  of each operator, brackets, `TRUE`/`FALSE`/`None` and `1`/`0`, dotted
  and coloned names, whitespace, Latin-1 letters and precedence.
- **Broken expressions** (42), each with the library's own error. Several
  are not `ParseError`: `()` is an `IndexError`, `(~)` a `TypeError`, and
  `( & a)` an `AssertionError` that shows the parser's list holding the
  number 4.
- **The tokenizer**, called directly, with its positions (a word's
  position is that of its last character).
- **Simplifying** (associativity, annihilation, idempotence, identity,
  complementation, elimination, absorption, sorting) and **normal forms**
  (`cnf`, `dnf`), including the library's habit of rewriting the
  arguments of the object it normalizes, which shows when that object is
  printed again.
- **Each method alone** (`literalize`, `cancel`, `demorgan`, `flatten`,
  `distributive`, `subtract`), **pretty printing** with and without debug
  details, **symbols and literals**, **equality and order** (155 steps,
  including the comparisons that fall back to `NotImplemented`),
  **membership**, **building** with `&`, `|` and `~`, **evaluating**
  (with the errors a `TRUE` inside an expression causes) and
  **substitution** with a default and with simplifying.
- **Three programs**: a licence checker that reads SPDX expressions
  through its own tokenizer (joining `X WITH Y` into one symbol), lists
  each expression's alternatives from its DNF and picks the first one the
  allow-list covers; an access matrix that simplifies each rule, prints
  its CNF and DNF and evaluates it for every user; and a truth-table
  checker that compares an expression with its simplified and normal
  forms on every assignment. The first two write reports under
  `data/out/` and print them back.

## How the port works

- **Expressions live in an arena.** The library compares objects by
  identity in places (`literalize` keeps an object whose arguments come
  back as the same objects), and `normalize` assigns to `expr.args` of an
  object that other expressions may hold, so every object is an index into
  one arena and is mutated where the library mutates it.
- **The parser's lists are modelled as lists.** `parse()` builds nested
  Python lists of `[parent, operation, arguments...]`, and some inputs pop
  the operation itself out of one of them; the port keeps those lists as
  vectors of items, so the same inputs fail with the same errors.
- **Order is Python's.** `<` follows CPython's rich comparison: the
  method, then the reflected `__gt__`, which the library defines as
  `not other < self`, so a `NotImplemented` there reads as `False`. The
  arguments of a simplified expression are sorted with a stable sort; a
  check over every sort the jobs make confirmed that the library's order
  is a strict weak order on them, so any stable sort agrees with
  CPython's.
- **Evaluation keeps Python's values.** A symbol gives a `bool`, `TRUE()`
  returns the `TRUE` object, and `TRUE & FALSE` builds an `AND`, so
  `1 & 0` evaluates to `AND(TRUE, FALSE)` as it does in the library.

## Compiler gaps

None found. `karac fix` made 95 edits (call-site `mut` markers and a
`.clone()`) and the port matched the oracle on the first run on every
backend.

## Scope

- One algebra with the standard classes; no subclassed algebras.
- `isalnum()` is exact for ASCII and Latin-1, which is all the inputs use.
- A `TRUE` or `FALSE` made by `cnf()`/`dnf()` is a new object with no dual,
  as in the library; the jobs never negate one and simplify it, where the
  library would return `None`.
- Sets (`literals`, `symbols`, `objects`) print sorted, since their order
  in Python depends on string hashing.

## Running

```bash
karac run mini_boolean.kara                 # interpret
karac build --mir mini_boolean.kara         # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate the embedded inputs
```
