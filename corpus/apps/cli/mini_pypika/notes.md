# mini_pypika

A port of [PyPika](https://pypi.org/project/PyPika/) 0.48.9 (Apache-2.0;
Timothy Heys and KAYAK): the SQL query builder that services, ETL jobs
and report tools use to build `SELECT`, `INSERT`, `UPDATE`, `DELETE` and
DDL statements from Python objects instead of pasting strings together,
in ten SQL dialects.

```
> program report reports.tsv
  wrote out/reports/mysql.sql (18 lines)
  | -- paid_orders
  | SELECT `o`.`id`,`o`.`customer_id`,`o`.`total` FROM `orders` `o` WHERE `o`.`status`='paid' AND `o`.`total`>=100 ORDER BY `o`.`total` DESC,`o`.`id` ASC LIMIT 20;
```

## What it does

`mk.py` writes a schema, report definitions and two CSV files to `data/`,
and a script of 1,265 steps to `data/jobs.tsv`. Each step is a Python
expression over the library's API. `oracle.py` evaluates the script
against the vendored library, and `mini_pypika.kara` evaluates it against
the port. A `let` step binds names and a `show` step prints `str()` of
the value. An exception prints as `error <class>: <message>`; there are
63 of them.

- **The tutorial** from the library's README, then **values and quoting**
  (every `get_sql` option: quote characters, aliases, namespaces,
  `as_keyword`), **arithmetic** with Python's precedence and brackets, and
  **criteria** (comparisons, `isin`, `between` and slices, `like` and the
  other matchers, `&`, `|`, `^`, `~`, `Criterion.all`/`any`).
- **Aggregates and `is_aggregate`**, which is three-valued (`True`,
  `False` or `None`), over 31 expressions.
- **Functions** (49 of the `fn` module, including `Cast`, `Extract`,
  `DateAdd`, `ApproximatePercentile` and `CustomFunction` with its
  argument check) and **analytic functions** with `over`, `orderby`, frames
  (`rows`, `range`, `Preceding`, `Following`) and `ignore_nulls`.
- **`Case`, tuples, arrays, parameters and intervals** (each parameter
  style; intervals in every unit, negative, mixed and per dialect).
- **Select lists and stars, group by, having and rollup, joins** (each
  join type, `on`, `on_field`, `using`, `cross`, self-joins and the
  library's checks on join criteria), **subqueries, `WITH` and set
  operations**, and **insert, update and delete**.
- **The dialects**: MySQL (`ON DUPLICATE KEY UPDATE`, modifiers,
  `FOR UPDATE` options), PostgreSQL (`ON CONFLICT`, `RETURNING`,
  `DISTINCT ON`), MSSQL `TOP`, Oracle, Snowflake, SQLite
  `INSERT OR REPLACE`, ClickHouse and Vertica.
- **`CREATE TABLE`** (columns, keys, `AS SELECT`, periods, system
  versioning) and **`DROP`** for databases, tables, users and views.
- **`replace_table` and the builder's copy rules**: which calls return a
  copy, and which change objects another query still holds.
- **Three programs** that write SQL files under `data/out/` and print
  them back. A migrator turns the schema into `CREATE TABLE` and
  `DROP TABLE` statements for three dialects. The second foreign key of
  a table fails there with the library's own error. A report compiler
  builds nine reports (joins, filters, grouping, ordering, paging) for
  five dialects. A bulk loader turns the CSV files into batched
  upserts for PostgreSQL, MySQL and SQLite.

## How the port works

- **The objects live in an arena.** The library's `@builder` decorator
  copies a query with `copy.copy()` before each call, and only some of the
  query's lists are copied too. A copied query therefore shares its other
  lists with the original: `distinct_on`, `using`, MySQL modifiers and
  conflict fields, and every list of a term, a `CREATE` or a `DROP`.
  Appending to one of those lists changes both objects. Each object is an
  index into one arena with its attributes in order, and the port copies
  exactly the lists the library copies.
- **Keyword arguments are an ordered dictionary.** `get_sql(**kwargs)`
  passes its keywords down the tree, and each class pops, sets defaults
  for or passes on the ones it knows. That decides which quote character
  each piece sees, and why a `Criterion` fails with
  `unexpected keyword argument 'quote_char'`. The port models the keyword
  set as a small dictionary and does the same pops and defaults.
- **Python's own rules are modelled where the library relies on them.**
  This covers binary operators (the method, then the reflected one, then
  CPython's `TypeError`), list containment, which calls `__eq__` and so
  is true for any term because `==` on a term builds a criterion, sets
  keyed by each class's `__hash__`, and `str()` of floats, dates and
  enums.
- **The jobs are evaluated as Python.** A small tokenizer and
  recursive-descent evaluator handle the script's subset: attribute
  access, calls with `*args` and keywords, slices, tuple and list
  displays, adjacent string literals, `is`, and Python's operator
  precedence.

## Compiler gaps

None found. `karac fix` made 19 edits (call-site `mut` markers and
`.clone()` on loop elements). Two errors were language rules rather than
gaps: a struct name must be PascalCase (`KA` became `Karg`), and `+` on a
`String` takes only a `String`, so a `Str` parameter is joined with an
f-string. After one fix in the port's own enum table (the empty `JoinType`
value had used the marker that `Arithmetic.sub` needs), the output
matched the oracle on every backend.

## Scope

- The library's classes the script uses. Not covered: `JSON` terms,
  `AtTimezone`, `PseudoColumn`, `MySQLQuery.load`, `VerticaQuery.from_file`
  and `ClickHouse` dictionaries and quotas.
- Python's syntax is the subset the script uses. There are no `and`,
  `or`, `not`, comprehensions or dict displays.
- `repr()` of objects that print a memory address is never shown.

## Running

```bash
karac run mini_pypika.kara                 # interpret
karac build --mir mini_pypika.kara         # native binary
python3 -s -P -B mk.py                     # regenerate data/
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate the embedded inputs
```
