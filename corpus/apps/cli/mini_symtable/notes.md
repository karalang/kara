# mini_symtable

A port of CPython 3.13's symbol table, `Python/symtable.c` and the
`symtable` module over it (`Lib/symtable.py`), both under the PSF licence
(see `LICENSE-cpython.txt`). This is the resolver of the Python compiler.
For each function, class, lambda, comprehension and PEP 695 scope it
decides whether each name is local, a cell, free, or global, and it raises
the SyntaxErrors that only scoping can find. Linters, IDEs and
refactoring tools use the same module to ask what a name refers to. The
port includes its own tokenizer and recursive-descent parser for the
Python 3.13 grammar, so it reads `.py` source the way `symtable.symtable()`
does.

```
$ snip tables def counter():\n    n = 0\n    def inc():\n        nonlocal n\n        n += 1\n        return n\n    return inc
module top line 0
  counter: assigned global local namespace
  function counter line 1 optimized
    params: -
    locals: inc, n
    inc: referenced assigned local namespace
    n: assigned local
    function inc line 3 nested optimized
      params: -
      frees: n
      nonlocals: n
      n: referenced assigned free nonlocal
```

## What it does

`mk.py` writes five Python files to `data/`:

- `webapp.py`: routes, decorators, views calling `super()`, and async handlers.
- `cli.py`: closures with `nonlocal`, a generator, and `match` with every
  kind of pattern.
- `pipeline.py`: PEP 695 generics and type aliases, private-name mangling,
  a walrus in a comprehension, and comprehensions in a class body.
- `models.py`: written under `from __future__ import annotations`.
- `smells.py`: code with problems for the lint to find.

It also writes the names in `builtins` and `data/jobs.txt`, which holds
125 jobs. `oracle.py` runs the jobs through Python's own `symtable`
module. `mini_symtable.kara` runs the same jobs, one task per job, and
prints them in job order.

- **`tables`**: every table with its type, line and the `is_nested` and
  `is_optimized` marks. Each function also shows its parameters, locals,
  frees, globals and nonlocals, each class its methods, and every symbol
  the `is_*` predicates that hold for it.
- **`dump`**: the `python -m symtable` listing, with each symbol's scope
  and raw flags.
- **`lint`**: pyflakes-style checks built on the tables. They report unused
  locals and imports, undefined names, a `global` that is never assigned,
  a shadowed builtin, and a method that reads a module global where the
  author meant the class attribute.
- **`snip`** (110 jobs): one-line programs through the same modes. They
  cover free and cell variables through several levels, class scopes,
  `super` and `__class__`, comprehension inlining, generic functions,
  classes and aliases, and mangling. They also cover 45 programs that
  must fail. Each gets CPython's message and range, for example
  `name 'x' is assigned to before global declaration at 3:5-3:13`.

```
$ lint smells.py
smells.py:8: 'list' shadows a builtin in function 'load'
smells.py:17: 'LEVEL' is declared global in function 'configure' but never assigned there
smells.py:27: function 'connect' reads global 'retries', not class attribute Config.retries
...
12 problems
```

## How the port works

- **Two passes, as in CPython.** The visitor records each name's raw
  flags in the block where it occurs, such as assigned, used, parameter,
  or declared global. The analysis then walks the blocks with the sets of
  names bound around them and decides each name's scope. The functions
  keep their C names: `analyze_name`, `inline_comprehension`,
  `analyze_cells`, `update_symbols` and `extend_namedexpr_scope`.
- **Order matters where CPython's does.** Each table keeps its symbols in
  first-seen order, because the analysis visits them in that order and
  the first error wins. The listing is the exception, as noted below.
- **The 3.13 scopes are all there.** Comprehensions (other than generator
  expressions) are inlined into their parent, with their children spliced
  in. Generic functions, classes and aliases get a type-parameters
  scope, and each bound, constraint or default gets its own type-variable
  scope. `__classdict__` lets an annotation scope see the class around it.
  `from __future__ import annotations` gives each annotation a detached
  block that no table lists.
- **The AST has only what the symbol table visits.** Expressions that the
  visitor treats as "visit the children, left to right" form one `Group`
  node with a kind. The rest keep CPython's field order, because that is
  the order the visitor follows. Positions are CPython's, so error ranges
  match, including where a named expression's range leaves out its
  parentheses.

## Compiler gaps

One row, filed in the kara ledger:

- B-2026-11-01-6: a struct variant named like a library type
  (`Stmt.Match { cases }`; `Response`, `Client`, `Regex` and `Stats` too)
  passes `karac check`, but `karac run` and `karac build` refuse a use of
  its `Vec` field. The checker also reads that variant's field types from
  somewhere else, and accepts `*pos` on its `Pos` field. The app names the
  variant `MatchStmt`, with a comment naming the row.

The Mend loop otherwise: `karac fix` applied 99 changes on 92 lines.
Most of them (76 lines) drop a `*` on a copied value bound by a pattern,
such as a `Pos` or a `bool`. The others add `own` to parameters whose
argument is stored, bind 9 locals as `ref` to a table entry, add a `mut`
at one call, and write one `Some(b)` as `Some(ref b)` so an `if let` does
not move an owned `Option`. The hand edits all followed a diagnostic:

- `TK` became `Tk`, because an enum name must be PascalCase.
- `private` became `privname`, because `private` is reserved.
- `let (mut name, _)` became a `let` and a `let mut`.
- A second `Some(b)` became `Some(ref b)`, for the move check.
- Two functions return `Vec[String]`, since an array of string literals
  turned into a `Vec` holds `String`.
- `Arg` and `Args` became `Param` and `Params`, because they shadowed
  prelude types.
- One `*pos` that check let through because of the row above.

After that, the interpreter matched all 1,833 lines of the oracle on its
first run. The LLVM, ASan and native builds matched at 1 and 4 threads,
and the auto-par binary matched on five runs.

## Scope

- The parser accepts the programs the jobs use, and any valid Python 3.13
  module like them. It does not report Python's parser errors: a program
  that does not parse prints `! ParseError` with the token it stopped at.
- Each table lists its symbols in name order. CPython's own order depends
  on the hash order of a set (the free variables `update_symbols` adds),
  so it changes between runs.
- Column numbers are byte offsets, as CPython's are before it converts
  them, so the error snippets are ASCII.
- Nothing here needs the data library (docs/library/data.md).

## Running

```bash
karac run mini_symtable.kara                      # interpret
karac build mini_symtable.kara                    # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
