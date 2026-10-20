# mini_tokenize

Python 3.13's `tokenize` module, ported from the C code it runs: the
tokenizer in `Parser/lexer/lexer.c` with the tokenize module's extra
tokens turned on, the token building in `Python/Python-tokenize.c`, and
`detect_encoding` and the error handling from `Lib/tokenize.py`.

The port covers:
- layout: INDENT and DEDENT with tab stops of 8 checked against tab
  stops of 1 (the TabError check), blank and comment-only lines as NL,
  form feeds, backslash continuations, and the NEWLINE that a file
  without a final line break gets anyway (with empty text);
- names, all the number forms with the C tokenizer's own errors, and
  the operators (`<>` is two tokens; `$`, `?` and `` ` `` are plain OP);
- string prefixes, single and triple quotes, escaped quotes, and
  f-strings as FSTRING_START, FSTRING_MIDDLE and FSTRING_END around the
  expression tokens, with format specs, nested fields, `{{` and `}}`,
  `!r`, `=` and `\N{...}`;
- the errors the module raises, with the position each one reports:
  unterminated strings, unexpected EOF inside brackets or after a
  backslash, a bad dedent, inconsistent tabs, and the f-string errors;
- `detect_encoding`: a UTF-8 byte order mark and a coding cookie on one
  of the first two lines.

Positions come out where the C code puts them. A string token starts on
the line where it began, every token ends on the last line the
tokenizer has read, and the tokens at the end of the file go on the line
after the last one.

`mk.py` writes `data/`:
- `jobs.tsv`: about 120 snippets for `tokenize.generate_tokens`, each a
  Python str literal;
- `project/`: a small code base, with a BOM in one file and a latin-1
  file with a coding cookie;
- `broken/`: files that each stop the tokenizer somewhere.

The programs on top of the tokens are `python -m tokenize -e`, line
counts in the manner of `radon raw`, TODO/FIXME/XXX comments, a few
flake8 checks (E501, W191, E711, E722, T201, F541, E261 and E262), a
rename of a function that leaves attributes alone, and a comment
stripper.

`oracle.py` runs the same snippets and programs through Python and wrote
`expected.out`.

It matched on every backend the first time it ran; no gaps were found.
