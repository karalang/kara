# mini_pycparser

A port of [pycparser](https://github.com/eliben/pycparser) 3.0 (BSD-3;
Eli Bendersky, see `LICENSE-pycparser.txt`), the C99/C11 parser in pure
Python that cffi, the Python bindings generators and many static-analysis
scripts build on. It covers the lexer with its typedef-name feedback, the
hand-written recursive-descent parser, the `c_ast` node classes with their
`repr()` and `show()`, and `CGenerator`, which turns an AST back into C.
The jobs do what a binding generator or a lint script does with the AST:
list the functions a file defines, find the calls, explain each declaration
in words, and check that generated C parses back to the same tree.

```
$ explain decls.i
T: typedef int
sighandler_t: typedef pointer to function(int) returning void
signal_fn: function(int, sighandler_t) returning pointer to function(int) returning int
cbs: pointer to array[4] of pointer to function(pointer to const char, ...) returning pointer to char
names: volatile pointer to const pointer to const char
```

## What it does

`mk.py` writes ten C sources to `data/`, each written the way pycparser is
fed in practice, as preprocessor output with `# <line> "<file>"` markers
and no comments: a UART ring buffer, an open-addressing hash map, a JSON
tokenizer, a file of hard declarations (K&R definitions, abstract
declarators, `_Atomic`, `_Alignas`, bit-fields, anonymous members,
compound literals), OpenMP and `#pragma` lines, a state machine, a
60-field generated config struct, and three files that must fail. It
writes `data/jobs.txt`, 122 jobs. `oracle.py` runs them against the
installed pycparser and prints each result; `mini_pycparser.kara` runs the
same jobs, one task per job, and prints them in job order.

- **`toks`**: the lexer's token buffer, with each token's line, column,
  type and value, also after an error.
- **`show`, `showx`, `repr`**: `Node.show()` plain and with attribute names
  and coordinates, and the full `repr()` of the tree.
- **`gen`, `genr`**: `CGenerator`, plain and with `reduce_parentheses`.
- **`funcs`, `calls`**: the function definitions and the calls in a file,
  with their coordinates.
- **`explain`**: a cdecl-style reading of every top-level declaration.
- **`stats`**: the node kinds in a file and how many of each.
- **`round`**: generate C, parse it again, and compare the two `show()`
  outputs line by line.
- **`snip`** (79 jobs): one-line C programs through the same modes. They
  cover integer, float, hex-float and character constants, string
  concatenation, every operator's precedence, `_Pragma`, `_Static_assert`,
  designated initializers, and 22 errors with pycparser's exact message
  and coordinate.

## How the port works

- **Lexing happens at peek time.** pycparser's parser asks its lexer for
  tokens lazily, and the lexer decides between `ID` and `TYPEID` from the
  parser's scope stack, which the parser pushes and pops on braces. The
  port keeps that order: `peek_tok` lexes on demand into a buffer, so a
  typedef declared on one line changes how the next line lexes, and a
  local `int T` shadows a typedef `T` inside its block.
- **The lexer's regular expressions are matcher functions.** pycparser's
  master pattern takes the first alternative that matches, and a fixed
  token wins only when it is strictly longer. Each alternative is one
  function (`m_hex_float`, `m_bad_octal`, `m_cconst_char`, ...) tried in
  the same order, so `08` is "Invalid octal constant" and `'abcde'` is
  "Invalid char constant" exactly where pycparser says.
- **The AST is one `shared enum`** with 49 variants. Each node lists its
  slots in `__slots__` order, which drives `repr()`, `show()` and the
  attribute lookups; `children()` follows pycparser's generated order where
  it differs (`FuncDef` and `NamedInitializer`).
- **The declaration fix-ups are kept.** `_fix_decl_name_type`,
  `_build_declarations`, `fix_switch_cases` and `fix_atomic_specifiers`
  run as in pycparser, including the way one qualifier list is shared
  between the declarators of a declaration, so `_Atomic` reaches all of
  them.
- **Python's errors are kept.** A few inputs make pycparser fail inside
  its own code: an `AssertionError`, or an `AttributeError` naming the
  class that had no `type` attribute. The port returns the same text.

## Compiler gaps

Three rows, filed in the kara ledger for the MIR builder:

- B-2026-11-01-1: `d ?? Node.Leaf(0)` on a bare `Option[Node]` parameter of a `shared
  enum` passed `karac check` but the MIR build refused it. Fixed in kara
  f667cd24ea, so the parameter is borrowed again.
- B-2026-11-01-2: while any function's MIR lowering failed, `karac check` printed "All
  checks passed" and reported none of the program's MIR borrow errors.
  Fixed in kara 3e19124daa: check now refuses what the build refuses.
- B-2026-11-01-3: `Some(k) => kind_name(k)` on a `ref Option[Node]` scrutinee is refused
  as E0516, though `k` is a `ref` into the scrutinee. The app copies the
  name to a `String` there, with a comment naming the row.

The Mend loop otherwise: `karac fix` applied 20 changes (`own` parameters
and `.clone()` at moves out of borrows, and `let mut (a, b)` for
destructuring). Three needed a hand edit: a `.clone()` in an `if`/`else`
whose branches mix a borrowed and an owned value, where `karac fix` offers
nothing; a `.clone()` at a second use of a moved `Option[Coord]`; and
`let _ =` on calls whose `Option` result was ignored. `Slot` needed
`#[derive(Clone)]`, as the diagnostic said. After that the interpreter
matched the oracle's 7,497 lines on its first full run apart from the
child order of two node kinds, a bug in the port. The LLVM, ASan and
native builds matched at 1 and 4 threads, and the auto-par binary matched
on five runs.

## Scope

- Preprocessed input only, as with pycparser: no `#include` or macro
  expansion, and a comment is an error.
- No `c_ast.NodeVisitor`. The jobs walk the tree directly.
- The generator's output is compared through `round` and the `gen` jobs;
  the port does not format code any other way.
- Nothing here needs the data library (docs/library/data.md).

## Running

```bash
karac run mini_pycparser.kara                     # interpret
karac build --mir mini_pycparser.kara             # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
