# mini_tinydb

A port of [TinyDB](https://pypi.org/project/tinydb/) 4.8.2 (MIT; Markus
Siemens): the document database that scripts, bots, desktop tools and
small services embed when they want to keep JSON records on disk without
running a database server.

```
> program report queries.tsv db/contacts.json out/report.txt
  out/report.txt (17 lines)
  | berliners: 3 (Ann Lee, Cy Ng, Ida Berg)
  | adults_in_paris: 1 (Bo Brown)
```

## What it does

`mk.py` writes a contacts CSV file, sixteen saved queries and a batch of
updates to `data/`, and a script of 526 steps to `data/jobs.tsv`. Each
step is a Python expression over the library's API. `oracle.py` evaluates
the script against the vendored library, and `mini_tinydb.kara` evaluates
it against the port. A `let` step binds a name or assigns one item, a
`show` step prints `str()` of the value, and a `cat` step prints a file
under `data/` as it is on disk. An exception prints as
`error <class>: <message>`; there are 33 of them.

- **The tutorial** from the library's README, over a JSON file.
- **Queries**: comparisons and paths (`q.a.b`, `q['a']`, `where()`),
  nested documents, `&`, `|` and `~`, `noop()`, `matches()` and
  `search()` with flags, `test()` with arguments, `map()`, and the list
  tests `any()`, `all()`, `one_of()` and `fragment()`, each with a list
  and with a query.
- **Query errors**, which follow Python: a `KeyError` or `TypeError` on
  the way down a path means no match, while one raised by the test itself
  propagates (`'<' not supported between instances of 'str' and 'int'`,
  `argument of type 'NoneType' is not iterable`).
- **Query equality, hashing and `repr`**: `1`, `1.0` and `True` give
  equal queries, `&` and `|` compare as sets, and `map()` makes a query
  uncacheable.
- **Tables, document IDs, `get` and `contains`, `update` with every
  operation, `update_multiple`, `upsert`, `remove` and `truncate`**,
  including how the next ID is cached between calls and found again by a
  new database on the same file.
- **The query cache** (which results it keeps, why a cached result can
  disagree with the table, and its LRU order at a capacity of two),
  **`LRUCache` and `freeze`** on their own.
- **The storages**: `MemoryStorage` keeps the very objects it is given,
  `JSONStorage` with `indent`, `sort_keys`, `separators`,
  `create_dirs`, read-only mode and its file errors, and
  `CachingMiddleware` with its write count, `flush()` and `close()`.
- **Three programs**. An importer loads the CSV file into a pretty-printed
  database with the file's own IDs. A report runner turns each saved
  query (`city == Paris & age >= 25`, `email ~ .*@example\.com$`,
  `tags has ops`, ...) into a query and writes counts and names to
  `data/out/`. A batch updater applies increments, sets and deletes to the
  matching documents. The report runs again after the updates, and each
  program prints the file it wrote.

## How the port works

- **Every list and dict lives in an arena.** TinyDB's behaviour depends on
  which objects are shared. `insert()` stores a shallow copy, so a later
  change to the caller's nested list shows up in a `MemoryStorage`
  database and a change to a top-level key does not. `search()` hands out
  the documents it keeps in its cache, so changing a result changes what
  the next identical search returns. An `update()` that fails halfway has
  already changed the documents before it, because it works on the stored
  objects. A Python reference is an index into the arena, so the port
  shares exactly what the library shares.
- **Python's rules are modelled where the library relies on them.** This
  covers `==` across `int`, `float` and `bool`, `in` on strings, lists and
  dicts with CPython's error texts, ordering comparisons and their
  `TypeError`, truthiness (the LRU cache replaces an entry in place only
  when the old value is truthy), and `repr()` of strings, tuples and
  `FrozenDict`.
- **`json.dumps` and `json.loads` are written out.** The port writes the
  files byte for byte as the library does, with both separator defaults,
  indentation, key sorting and `ensure_ascii` escapes, and reads them back
  as fresh objects on every access, which is why a `JSONStorage` database
  never shows the sharing a `MemoryStorage` one does.
- **The regular expressions are matched by a small backtracking engine.**
  It covers classes, `\w`, `\s`, `\d`, the quantifiers, anchors, groups,
  alternation and `re.IGNORECASE`, which is the syntax the script uses.
- **The jobs are evaluated as Python.** A small tokenizer and
  recursive-descent evaluator handle attribute access, calls with keyword
  arguments, subscripts, list, tuple and dict displays, string escapes,
  `is`, and Python's operator precedence.

## Compiler gaps

One, already filed. A top-level function named `ref_eq` is checked
against the prelude's `ref_eq` intrinsic instead of being called, which
is B-2026-10-23-10 (found with `todo` in mini_parsedatetime); the row now
names `ref_eq` too. The port's structural equality is called `obj_eq`,
with a comment citing the row. `karac fix` made 2 edits, and the output
matched the oracle on the first run on every backend.

## Scope

- The library's public API and the attributes the script reads
  (`_query_cache`, `_cache_modified_count`, `storage.memory`). Not
  covered: custom `Table` or `Document` classes, the mypy plugin, and
  `repr()` of a database, a table or a storage, which prints a memory
  address.
- Python's syntax is the subset the script uses. There are no
  comprehensions, slices or `and`/`or`/`not`.
- The regular-expression engine answers only whether there is a match,
  which is all `Query.matches()` and `Query.search()` ask.

## Running

```bash
karac run mini_tinydb.kara                 # interpret
karac build --mir mini_tinydb.kara         # native binary
python3 -s -P -B mk.py                     # regenerate data/
python3 -I -B oracle.py > expected.out     # regenerate expected.out
python3 -I -B gen_kara.py                  # regenerate the embedded inputs
```
