# mini_ast

Python 3.13's `ast` module, ported from the code it runs:
- `ast.parse`: the PEG parser's rules from `Grammar/python.gram`, with
  the positions each node gets and the errors CPython reports, on top of
  the tokenizer from `mini_tokenize` run in parser mode;
- `ast.dump`, with and without positions;
- `ast.unparse`, after `Lib/ast.py`'s `_Unparser`: precedences decide the
  parentheses, string literals pick the quotes that need the fewest
  escapes, and f-strings negotiate one quote for all their parts;
- `ast.literal_eval` with its error messages, and `ast.get_docstring`
  with `inspect.cleandoc`.

The parser covers the statements and expressions up to 3.13 except
`match`, `type` and type parameters: decorators, `async`, `with` in
parentheses, starred targets and subscripts, the walrus, lambdas with
every kind of parameter, comprehensions, and f-strings with nested
fields, `=`, conversions and format specs. Errors are the generic
`invalid syntax` at the furthest token, the specific messages CPython's
second pass raises (`Maybe you meant '==' or ':='`, `expected ':'`,
`cannot assign to ...`, `expected an indented block after ...`,
`positional argument follows keyword argument` and the rest), and the
tokenizer's errors, including the rule that a tokenizer error further on
replaces the parser's error.

`mk.py` writes `data/`:
- `jobs.tsv`: 254 snippets to parse (dump and unparse, or the error), 6
  to dump with positions, and 25 for `literal_eval`, each a Python str
  literal;
- `project/`: a small shop package.

The programs over the package unparse each file and check that the
result parses to the same tree, print an outline of classes and
functions with their signatures and docstrings, an import graph with
relative imports resolved, unused imports in the manner of pyflakes,
cyclomatic complexity in the manner of radon, and node counts.

`oracle.py` runs the same snippets and programs through Python and wrote
`expected.out`.

It matched on every backend the first time it ran; no gaps were found.
