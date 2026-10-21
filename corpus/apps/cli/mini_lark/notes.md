# mini_lark

Grammars to parse trees as Lark 1.2.2 does it with `parser="lalr"`.
Lark is the parsing toolkit behind many Python DSLs, config formats
and query languages. The port covers:
- the grammar loader: lark's grammar of grammars, read by an LALR
  parser built from the loader's own tables; rules and terminals with
  priorities, aliases, templates and the `?`, `!` and `_` modifiers;
  `%import` (relative, from `import_paths` and from the bundled
  grammars) with its name mangling, `%ignore`, `%declare`,
  `%override` and `%extend`;
- the grammar errors: GRAMMAR_ERRORS matched by
  `UnexpectedInput.match_examples`, and the checks for undefined,
  duplicate, recursive and empty definitions, bad templates, bad
  ranges and reduce/reduce collisions;
- compilation: literals and ranges to patterns, anonymous terminals
  and their names, EBNF to BNF with lark's helper rules (including
  `~` repeats), templates, and the regexps each terminal becomes;
- the LALR(1) table: LR(0) item sets, the DeRemer and Pennello
  lookaheads and lark's conflict rules;
- the basic and contextual lexers (with keyword `unless` callbacks
  and zero-width checks), the Indenter post-lexer, the tree builder
  (filtered tokens, `_rule` inlining, `?rule` expansion and
  `maybe_placeholders`), `parse_interactive`, `on_error` recovery and
  lark's error messages.

The run writes `data/` and works through `data/jobs.txt`:
- prints the compiled terminals, rules and state count of 7 grammars;
- evaluates a calculator, walks JSON, flattens a config file,
  summarises SQL queries and prints an indented outline, each with
  inputs that fail and the error lark reports;
- parses with the basic and the contextual lexer, with and without
  placeholders, lexes line by line and asks what can come next;
- loads 35 broken grammars and prints each error;
- parses every grammar, ours and lark's bundled ones, with lark's own
  `lark.lark`.

Terminals are matched by the regular expression engine copied from
`mini_re`. `gen_kara.py` copies the loader's tables from lark's source.

`oracle.py` runs the same jobs through lark itself and wrote
`expected.out`. It uses the Python files of the 1.2.2 wheel in
`lark/`, unchanged. Lark is MIT licensed (Erez Shinan); see
`LICENSE-lark.txt`. Every set lark builds (expected terminals,
accepts, the rules of a collision) is printed sorted, since Python's
set order changes from run to run.

The quirks are kept. Some examples:
- The contextual lexer reports a character that only another state
  could lex as an UnexpectedToken, with that token.
- `match_examples` returns the first label whose example fails in the
  same parser state, unless a later one fails on the same token.
- Lark's bundled `lark.lark` has no `%extend`, so it cannot parse a
  grammar the loader itself accepts.

Not ported: the Earley and CYK parsers, the dynamic lexers,
transformers and visitors, `propagate_positions`, the reconstructor,
the standalone generator, the grammar cache and `Lark.save`.
