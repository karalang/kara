# mini_jsonlogic

A JsonLogic rules engine: the JSON rule format that feature-flag services,
form validators and pricing rules share between a server and a browser.
The operations of panzi-json-logic's Python port, with the JavaScript-style
coercions it emulates (loose `==`, `+` on text, `"%.15g"` number text,
`[object Object]`), the short-circuit forms, the list forms, and `var`
paths. The program decides a small web shop's feature flags for four
users, then runs single cases through every operation.

The program writes its own data (`data/flags.json`, `data/users.json` and
`data/cases.jsonl`), runs it and prints a report. `oracle.py` runs the same
data through `json_logic/apply.py` and `builtins.py` from panzi-json-logic
1.0.1 (`vendor/`, MIT) and is the reference; `mk.py` writes the data and
`gen_kara.py` embeds it in the program.

## Values

- JSON is read as Python's `json.loads` reads it: a number with no point
  or exponent is an `int`, any other a `float`, and an object keeps its
  keys in order. The data has no `-0` (an `int` in Python, a float in
  Kāra's reader), no NaN or infinity literal, and no integer beyond i64.
- A Python `bool` is an `int`, and the module's type tests see it so:
  `true` is a number to `+`, `<` and `var`, and `to_string(true)` is
  `True`. `==` between two lists or two objects asks whether they are the
  same object; every list and object in the cases is a different one, so
  it is false.

## Coercions (the module's)

- `to_number`: numbers (booleans included) as they are; `null` is 0; a
  list is 0 when empty, its item's number when it has one, else NaN; an
  object is NaN; text is NaN when it is `inf`, `+inf` or `-inf` stripped
  and lower-cased, else Python's `float()` of it (surrounding whitespace,
  `_` between digits, `infinity` and `nan` in any case), NaN when that
  fails.
- `to_string`: text as it is; a float as `"%.15g"` (fifteen significant
  digits, trailing zeros dropped, an exponent below 1e-4 or from 1e15);
  an int in decimal; `null`, `True`, `False`; a list as its items' text
  joined with `,`; an object as `[object Object]`.
- Truth: `null`, `false`, 0, NaN, `""` and `[]` are false; objects are
  true.

## Operations (the module's)

- A rule that is a list is the list of its items' results. A rule that is
  not an object with exactly one key is a literal. Otherwise the key is the
  operation and its value the arguments (one argument when it is not a
  list).
- `if` / `?:` take condition, result pairs and an optional last result
  (`null` when none). `and` returns the first false argument or the last;
  `or` the first true one or the last; both `null` with no arguments.
- `map`, `filter`, `all`, `some` and `none` run their second argument
  with each item of the first as the data; `reduce` with the data
  `{"accumulator": …, "current": …}`, starting from its third argument as
  written (not evaluated). Too few arguments, or a first argument that is
  not a list, give `[]`, `[]`, false, false, true and the start value.
  `all` of an empty list is false.
- The rest evaluate their arguments first, then: `==` and `!=` (loose:
  numbers against the other side's number, text against a list or object
  as text, `null` equal only to `null`), `===` and `!==` (Python's `==`),
  `<`, `<=`, `>`, `>=` (a number against the other side's number, else
  text against the other side's text, else both as numbers; a third
  argument makes it a between test), `!`, `!!`, `+` and `*` (folding from
  0 and 1), `-` (negation when the second argument is `null` or missing),
  `/` (always a float), `%` (Python's modulo, with the sign of the
  divisor), `min` and `max` (NaN with no arguments; the first of equal
  ones), `in` (an item of a list by Python's `==`, or text inside text),
  `cat`, `substr` (in characters; a negative start counts from the end,
  a negative length stops that far from the end), `merge` (one level),
  `missing` and `missing_some`, `var` and `log` (writes the value as
  `json.dumps` does, on its own line, and returns it).
- `var` with `null` or `""` is the whole data. A number indexes a list or
  text when it is a whole number in range. A path is split at `.`; each
  part indexes a list or text when it is a plain decimal number in range
  (`length` is the length), or looks up an object's key. Anything else, or
  a `null` at the end, is the default (the second argument).
- Errors are Python's: `ZeroDivisionError` (`division by zero`, `float
  division by zero`, `integer modulo by zero`, `float modulo by zero`),
  `ReferenceError: Unrecognized operation: '<op>'` (for a dotted name, its
  first part), `TypeError: 'function' object is not subscriptable` for a
  dotted name whose first part is an operation, and
  `TypeError: op_var() takes from 0 to 3 positional arguments but <n>
  were given` for `var` with three or more arguments (the data is the
  first).

## The report

- `== flags`, then for each user `user <name>` and each flag, in file
  order, `  <flag>: <result>`.
- `== cases`, then for each case `<name>: <result>`.
- A result is `json.dumps` of the value (`, ` and `: ` separators, text
  with every character outside space to `~` escaped as `\uXXXX`, floats
  as Python's `repr`, `NaN`, `Infinity`, `-Infinity`), or
  `error <Type>: <message>`.
