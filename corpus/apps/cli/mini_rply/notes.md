# mini_rply

A port of [rply](https://github.com/alex/rply) 0.7.8 (BSD-3; Alex Gaynor
and contributors), the LALR(1) parser generator for Python and RPython
with a PLY-like API. It covers
`LexerGenerator` and its lexer stream, `ParserGenerator`'s grammar checks,
the table builder (LR(0) items, DeRemer-Pennello lookaheads, precedence
and conflict resolution, default reductions) and `LRParser`. Three small
programs run on top of the generated parsers: a calculator, a JSON
formatter and `SELECT` over CSV tables.

```
> build grammars/dangling_else.grammar
  warning: 1 shift/reduce conflict
  productions 4, states 9, default reductions 2
  shift/reduce on 'ELSE', resolved as shift
...
> !calc grammars/calc.grammar inputs/calc/session.txt
  > -total % 7
    = 6
  > avg(1, 2)
    error: unknown function avg/2
  > (price
    ParsingError at no position
```

## What it does

`mk.py` writes 81 files to `data/`: 22 grammars, 55 input texts, two CSV
tables, `data/files.txt` and `data/jobs.txt` (94 jobs in 8 sections).
A grammar is a small line format: `tokens`, `lex NAME pattern`,
`ignore pattern`, `prec <assoc> TOKENS`, `rule <rply production text>`
with an optional `%prec NAME`, and `onerror`. `oracle.py` feeds each one
to the vendored rply, with an action that builds a tree node per
production, and prints each result; `mini_rply.kara` runs the same jobs.

- **Build** (22 jobs): the warnings rply issues, then the number of
  productions, states and default reductions and each conflict and how
  it was resolved; or the error. Fifteen grammars build (among them a
  dangling else with and without precedence, an ambiguous expression
  grammar, a reduce/reduce conflict, an unused token and an unreachable
  production, and epsilon rules) and seven are refused: a terminal as a
  rule name, an unknown `%prec`, a token given precedence twice, an
  unknown associativity, a missing colon, a rule with no body and an
  undefined nonterminal.
- **Tokens** (17 jobs): each token with its `repr()`, line, column and
  index, up to the end or a `LexingError`.
- **Parse** (52 jobs): the parse tree or the error, over precedence and
  associativity, parse errors, the conflict grammars and JSON documents.
- **`!calc`** runs a calculator session with variables, Python's floor
  division and modulo, `**`, comparisons and `max`/`min`/`abs`.
  **`!json`** reformats JSON documents and counts their objects, arrays
  and scalars. **`!query`** runs 18 `SELECT` statements with `WHERE`,
  `ORDER BY` and `LIMIT` over two CSV tables.

## How the port works

- **The lexer patterns run on a small regex engine.** rply hands each
  pattern to Python's `re` and calls `match()` at the current position,
  taking the first rule that matches. The port compiles the patterns the
  grammars use (classes, `\d` `\w` `\s` and their negations, `\b`,
  groups, alternation, greedy and lazy quantifiers) into a backtracking
  program that tries alternatives in `re`'s order, so the same rule wins
  and takes the same span.
- **The tables are built the way rply builds them.** Python dicts keep
  insertion order, so the grammar's maps are ordered `Vec`s. The LR(0)
  construction keeps `lr0_goto`'s cache, keyed by the identity of the
  item list, because that decides which lists become states. rply's
  `digraph` shares one lookahead list between the members of a cycle and
  lets the follow sets alias the read sets; the port keeps those lists
  in an arena and shares them by index, so appending to one shows up in
  every alias, as in Python.
- **The parse tree is an enum.** A symbol is a token or a node with its
  children; each program walks the tree by child count, as the oracle's
  Python does.

rply's quirks show in the output, and the port keeps them:

- `nonassoc` does not make `1 < 2 < 3` an error, as it would in PLY.
  rply keeps the action already in the table, here the reduce, so it
  parses as `(1 < 2) < 3` and the calculator prints `1`.
- A `LexingError` reports the index of the bad character but the column
  of the last token, because skipping ignored text does not update the
  column.
- A `ParsingError` at the end of the input has no position, because the
  parser's `$end` token has none. An error handler that reads the
  position there raises `AttributeError`.
- A rule with no body raises `IndexError`, and an undefined nonterminal
  raises `KeyError` from `compute_first`, after the warnings for unused
  tokens.
- Lexer rules are tried in the order they are added, so `SELECT` without
  `\b` matches the start of `SELECTED`, and `=` added before `==` lexes
  `==` as two tokens.

## Compiler gaps

None. `karac check` asked for `mut` before a whole tuple pattern instead
of on each binding (`karac fix` made the change), and flagged that the
struct named `Stats` shadows a prelude type. After that, the interpreter
matched the oracle byte for byte on its first run, as did the LLVM, ASan
and native builds.

## Scope

- No parser cache (`cache_id`) and no parser `state` argument.
- Productions do not call user functions: every production builds a
  tree node, and the programs work on the tree.
- The regex engine covers what the grammars use. There are no anchors,
  backreferences or lookarounds.

## Running

```bash
karac run mini_rply.kara                          # interpret
karac build --mir mini_rply.kara                  # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
