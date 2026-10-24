# mini_parsimonious

A port of [parsimonious](https://github.com/erikrose/parsimonious) 0.10.0
(MIT; Erik Rose and contributors), the PEG parser library for Python: you
write a grammar as text, it builds a packrat parser from it, and a
`NodeVisitor` turns the parse tree into values. Four programs run on top: a
calculator, a JSON reader, an INI reader and a query filter over records.

```
> tree grammars/greeting.peg inputs/greet_ok.txt
  <Node called "greeting" matching "Hello World!">
      <RegexNode called "hello" matching "Hello">
      <Node called "_" matching " ">
          <Node matching " ">
      <RegexNode called "name" matching "World">
      ...
> !query inputs/people.csv inputs/queries.txt
  age > 30: Asha, Chidi, Dana, Eitan, Goran (5)
  active and (city = "Pune" or age < 25): Asha, Farah (2)
```

## What it does

`mk.py` writes 41 files to `data/` (listed in `data/files.txt`): 18
grammars and 23 inputs, and `data/jobs.txt` (47 jobs). `oracle.py` runs
each job on the vendored parsimonious and prints the result;
`mini_parsimonious.kara` runs the same jobs.

- **`rules`** prints `str(rule_grammar)`, parsimonious's own grammar of
  grammars.
- **`grammar`** (18 jobs) builds a `Grammar` and prints its rules, or the
  error: the four app grammars, quantifiers, lookaheads, string literal
  prefixes and escapes, regex flags, an alias, left recursion, an empty
  grammar, an alternative written without parentheses, and six broken
  ones (an undefined label, a circular reference,
  mixed `str` and `bytes` literals, a missing `=`, a syntax error and a bad
  regex).
- **`tree`** (15 jobs) and **`match`** (3) parse an input, with the default
  rule or a named one, and print `Node.prettily()`, or the `ParseError`,
  `IncompleteParseError`, `LeftRecursionError` or `RuntimeError`.
- **`!calc`** evaluates 16 integer expressions with `+ - * / %`, unary
  minus, parentheses and `max`, `min`, `abs` and `pow`.
- **`!json`** (5 jobs) reads JSON and prints it back with `json.dumps`,
  then one line per leaf.
- **`!ini`** (3 jobs) reads INI files into sections, with ints, booleans and
  empty values.
- **`!query`** runs 16 filters (comparisons, `~` for substring, flags,
  `and`, `or`, `not`, parentheses) over 8 records.

## How the port works

- **Expressions** live in one arena, so a rule's members can point at other
  rules, cycles included, the way Python objects do. `match_core` keeps the
  packrat cache keyed by expression and position, raises
  `LeftRecursionError` on a re-entry, and moves the error position the same
  way parsimonious does.
- **The grammar of grammars** is bootstrapped as in `grammar.py`: the
  hand-built `BootstrappingGrammar` reads `rule_syntax`, and the grammar it
  produces reads `rule_syntax` again to give `rule_grammar`. `gen_kara.py`
  copies `rule_syntax` from the vendored `grammar.py`.
- **`RuleVisitor`** turns the tree into expressions, with lazy references
  resolved at the end, `ast.literal_eval` for string literals, and
  `BadGrammar`, `UndefinedLabel` and `VisitationError` as the library
  raises them.
- **`NodeVisitor.visit`** visits the children, then the node's method, and
  wraps an exception in a `VisitationError` that shows the subtree it was
  raised at.
- **Regexes** run on the engine of `mini_re`, a port of Python's `sre`.

parsimonious's behaviour shows in the output, and the port keeps it:

- `a b / c` is not an alternative of a sequence: the grammar reads
  `a b` and stops at `/`. Alternatives of sequences need parentheses.
- With `greeting = salutation`, both names point to the same expression,
  named `salutation`, so `str(grammar)` prints one rule and the default
  rule is called `salutation`.
- A line missing its `=` joins the rule above it, so the error is an
  `UndefinedLabel`, not a syntax error.
- `{2}` prints as `{2,2}`. Every `str` regex prints with a `u` flag, and
  the `a` flag never prints, because `_regex_flags_from_bits` puts it at
  bit 7.
- An empty `*` or `?` visits to its `Node`, not to a list, so the visitors
  check for that.
- `LeftRecursionError` reports position -1: the window is the text's last
  character and the column is 0.

## Compiler gaps

None. `karac check` refused moves out of an index (`px.ex[e]`), `.clone()`
on an enum without `#[derive(Clone)]` and two `Str` arguments where a
`String` was needed. After those changes the interpreter matched the oracle
byte for byte, as did the LLVM, ASan and native builds.

## Scope

- parsimonious imports the `regex` module as `re`. `oracle.py` puts the
  standard `re` in its place, and the grammars use only syntax both treat
  alike. So bad patterns report `re.PatternError`, as the oracle sees them.
- Not ported: `TokenGrammar`, custom rules (Python callables in a
  grammar), the `@rule` decorator and `Grammar.default()`.

## Running

```bash
karac run mini_parsimonious.kara                  # interpret
karac build --mir mini_parsimonious.kara          # native binary
python3 -s -P -B mk.py                            # regenerate data/
python3 -I -B oracle.py > expected.out            # regenerate expected.out
python3 -I -B gen_kara.py                         # regenerate the embedded data
```
