# mini_pygments

A docs-site highlighter ported from Pygments 2.17.2 (BSD).
- `RegexLexerMeta`'s processing of token definitions: `include`,
  `inherit` merged through the class hierarchy, `default`, `combined`
  states, `words()` through `regex_opt`, `bygroups` and `using(this)` /
  `using(OtherLexer, state=...)`;
- the `RegexLexer` state machine with tuple, `#pop`, `#push` and numeric
  transitions, and its error recovery (a newline resets to `root`,
  anything else is one `Error` token);
- 20 lexers run from their own tables: Python, Bash, INI, Diff, TOML,
  nginx, CSS, SQL, Docker, Go, Rust, JavaScript, systemd, Properties,
  XML, HTML, Python tracebacks, C (with its `Name` to `Keyword.Type`
  switch and the stdlib / C99 / C11 / Linux type options), JSON (its
  hand-written character machine) and plain text;
- lexer preprocessing: BOM, `\r\n`, `stripall`, `stripnl`, `tabsize`
  and `ensurenl`, with the option parsers' errors;
- the `codetagify`, `keywordcase`, `highlight`, `raiseonerror`,
  `whitespace`, `gobble` and `tokenmerge` filters;
- `StyleMeta` inheritance (`noinherit`, `bg:`, `border:`, ANSI names)
  and `style_for_token`;
- `HtmlFormatter` (table and inline line numbers, `hl_lines`,
  `lineanchors`, `linespans`, `wrapcode`, `nowrap`, `noclasses`, `full`
  documents and `get_style_defs`), `TerminalFormatter`, the raw token,
  null and testcase formatters;
- lookup by alias, mimetype and file name over the whole lexer map,
  with `fnmatch` globs and the priority rules.

The site builder turns `data/docs/*.md` into HTML, highlighting each
fenced block with its options, falling back to plain text for a fence
Pygments does not know, and writes `pygments.css`.

`mk.py` writes `data/`: 25 source files, 81 lexing jobs, 8 style jobs and
2 docs pages. `oracle.py` runs Pygments itself and wrote `expected.out`
(`python3 -s -P oracle.py`). `gen_tables.py` prints the generated tables
at the end of `mini_pygments.kara` (lexer map, token definitions, Unicode
identifier classes, C type lists, standard types and six styles), and
`gen_kara.py` prints the data section after them.

The lexer loop skips a rule whose program must start with a character
other than the one at the current position. Pygments has no such check
(Python's `re` does the equivalent inside each match); it keeps the
interpreter under the corpus time limit and does not change any token.
