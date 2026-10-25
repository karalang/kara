# mini_esprima

esprima-python 4.0.1 as Python 3.13 runs it. esprima is the ECMAScript
2017 parser behind many JavaScript tools: linters, minifiers, code
browsers and bundlers that need a syntax tree. The port covers:
- The scanner: identifiers with escapes, every punctuator, numeric
  literals in four bases with legacy octals, strings and templates with
  their escapes, regular expression literals, and line, block and HTML
  comments.
- The parser, with its cover grammars for arrows, destructuring and
  parenthesised expressions, script and module goals, strict mode,
  directive prologues, classes (with `classProperties` fields), async
  functions and generators, and imports and exports.
- Tolerant mode, where many errors are recorded and parsing goes on, and
  the error messages and positions esprima reports.
- `range`, `loc` (with `source`), `tokens` and `comment`, and the comment
  handler that attaches leading, trailing and inner comments to nodes
  (`attachComment`).
- The standalone tokenizer, which guesses whether a `/` starts a regular
  expression from the tokens before it.

esprima-python compiles each regular expression literal with Python's
`re` and leaves the compiled pattern in the tree. Those checks and
patterns come from the parser and compiler of `mini_re` (Python's `re`,
ported from `_parser.py` and sre), which the program includes.

The JavaScript is in `data/src/`, which `mk.py` writes: a small web
service (`app/`, five modules), a library (`lib/`, five modules), a
report script (`tools/`), two legacy scripts and 134 snippets, 147 files
in all. The run writes `data/` and then works through the 189 jobs in
`data/jobs.tsv`. A job prints the tree as JSON, a digest of it, the
tokens, an outline of the declarations, statistics, or the import graph
of several modules:

```
== job 162: outline app/router.js module
L5 class NotFound extends Error
  L6 constructor(what)
L14 class Router
  L15 constructor({…}=)
  L22 static compile(path)
  L33 get size()
  L37 get hits()
  L41 set hits(value)
  L46 add(method, path, handler)
```

`oracle.py` runs the same jobs through the package in `esprima/`, which
is the 4.0.1 sources unchanged, and wrote `expected.out`. Its output is
the same over three runs.

esprima's quirks are kept. Some examples:
- A regular expression literal's `value` is the compiled `re.Pattern`,
  written as its repr: `re.compile('[A-Z]', re.IGNORECASE)`. Before
  compiling, esprima replaces astral characters and surrogate pairs with
  U+FFFF, so `/\u{1F600}+/u` compiles to `re.compile('\uffff+')`.
- A decimal literal goes through `float` and then `int` when it is
  whole, so `123456789012345678901234567890` has the value
  `123456789012345677877719597056`, and `1e21` is written out in full.
- `''` is not a directive, and it ends the directive prologue, so in
  `''; 'x'` the `'x'` is a plain expression statement too.
- Attaching comments to `if (a) { /* c */ } // d` followed by `{ }`
  raises `IndexError` in `insertInnerComments`, which clears the same
  index in two lists of different lengths.
- The tokenizer's reader indexes its token list the way Python does, so
  `a b } /x/` and `) /x/` raise `IndexError`.
- `tolerateError` takes its line from the scanner and its column from
  the last token, where `throwError` takes both from the last token.
- `export default from`, and an `import` or `export` that needs `from`
  and lacks it, give `Unexpected token` with the next token's value, or
  with no value when that is empty.

Not ported: JSX (`jsx_parser.py`), the `delegate` callback, and the
ReprVisitor. Identifier characters outside ASCII use Rust's
`is_alphabetic` and `is_alphanumeric` with the Other_ID_Start and
Other_ID_Continue lists, which agree with Python's tables on the letters
the data uses.

## Edits from mini_esprima.orig.kara to mini_esprima.kara

`karac check` raised 35 errors and `karac fix` applied all 35. 34 were
missing `mut` markers on `mut ref` arguments, and one was a move out of
a borrowed `for` element that needed `.clone()`. The check then raised
seven E0516 errors with no fix: each passed `self.lookahead` to a
method of `self` that writes it, as
`self.tolerate_unexpected(self.lookahead, ...)`. The fix was
`self.lookahead.clone()`. The diagnostic names the call itself as the
place of the shared borrow, not the argument. Two types, `Entry` and
`Parser`, were renamed for the prelude-shadow warning.

On its first run the output differed from `expected.out` on one line:
`mini_re`'s `repr` printed U+FFFF itself, where Python escapes it as a
noncharacter. With noncharacters made unprintable it matched under the
MIR interpreter, the LLVM backend, the ASan build and the native binary,
with nothing on stderr. All of these were mistakes in the port.

## Running

```bash
python3 -s -P -B mk.py                      # writes data/
python3 -I -B oracle.py > expected.out
karac run mini_esprima.kara                 # writes data/ itself, removes it at the end
karac build --mir mini_esprima.kara && ./mini_esprima
```

## License

esprima-python is under the BSD 2-clause license in `LICENSE-BSD`:

    Copyright JS Foundation and other contributors, https://js.foundation/

That covers the package in `esprima/`.
