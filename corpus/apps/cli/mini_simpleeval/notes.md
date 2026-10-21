# mini_simpleeval

simpleeval 1.0.3 by Daniel Fairhead and others: the "safe `eval`" that
web apps, rule engines and config tools hand user-written formulas to
(pricing rules, alert conditions, notification templates, spreadsheet
cells). It parses with Python's `ast` and walks the tree itself, with
an allow-list of node types, operators, functions and names. The port
covers:
- `SimpleEval` and `EvalWithCompoundTypes`: names (a dict, or a
  function), functions, every operator in `DEFAULT_OPERATORS`,
  `and`/`or` returning the deciding value, chained comparisons,
  `if`/`else`, calls with keywords, subscripts and slices, attributes
  with the fallback to `x["attr"]`, f-strings with format specs, and
  list, tuple, dict, set and comprehension literals (with `*x` and
  `**x`);
- its limits (`MAX_STRING_LENGTH`, `MAX_COMPREHENSION_LENGTH`,
  `MAX_POWER`, `MAX_SHIFT`) and its exceptions (`NameNotDefined`,
  `FunctionNotDefined`, `AttributeDoesNotExist`, `OperatorNotDefined`,
  `FeatureNotAvailable`, `NumberTooHigh`, `IterableTooLong`,
  `InvalidExpression`), and the `AssignmentAttempted` and
  `MultipleExpressions` warnings;
- the Python underneath: ints, floats (`repr`, `//` and `%` as
  `floatobject.c` does them, `**` with its errors), strings and their
  common methods, `str % args`, `format()`'s mini-language, dict views,
  `int()`, `float()`, `round()` (correctly rounded), `sum()` (with 3.12's
  compensated float sum), `sorted()` (CPython's run detection and binary
  insertion, so a failing comparison is the same pair), `min`, `max`,
  `any`, `all`, `range` and the containers, with the TypeErrors,
  ValueErrors and KeyErrors they raise.

The parser is mini_ast's port of CPython 3.13's `ast.parse`, so a
syntax error has CPython's message and line.

`mk.py` writes `data/`:
- `docs/`: a signed-in user, a shop order, a host's metrics and an HTTP
  request, as JSON;
- `jobs.tsv`: 719 expressions, each run on a fresh evaluator: the
  defaults, `EvalWithCompoundTypes`, or either with a document as names
  and builtins such as `len`, `round`, `sum` and `sorted` added;
- `shop/`, `ops/`, `mail/` and `sheet.tsv`: pricing rules over five
  orders, alert rules over five hosts, notification templates over
  three users, and a spreadsheet whose names function evaluates the
  cell a name refers to on the same evaluator.

Each job prints `-- `, its evaluator and the expression, any warning,
and then `repr` of the result, or `! `, the exception's class and its
message.

`oracle.py` runs the jobs and programs through simpleeval itself (the
copy in `simpleeval.py`, unchanged) and wrote `expected.out`.
`gen_kara.py` writes the generated section at the end of
`mini_simpleeval.kara`: `data/`.

The quirks are kept. Some examples:
- the keywords of a call are evaluated before its positional
  arguments, so `int(nope1, base=nope2)` reports `nope2`;
- a dict's own methods win over its keys, so `order.items` is the
  bound method `items`, not the order's items;
- `'ab' * 'cd'` fails comparing `'cdcd' > 100000`, inside the length
  check;
- an f-string's length check never adds the parts up, so two parts of
  60000 and 50000 characters pass;
- a generator expression is evaluated to a list;
- a comprehension target that is neither a name nor a tuple fails with
  `'Attribute' object has no attribute 'elts'`, and `for a, b in`
  over three values drops the third;
- `{**[(1, 2)]}` is a dict, because `**x` becomes `dict.update(x)`;
- an error inside a cell names the expression of the last cell
  evaluated, since a nested eval replaces `self.expr`.

Ints are 128 bits; no job goes past that. Sets print in CPython's
order for small non-negative ints (the only ones the jobs print), and
`rand()` and `randint()` draw from a fixed-seed generator (no job
prints a draw).

simpleeval is under the MIT license in `LICENSE-simpleeval.txt`.
