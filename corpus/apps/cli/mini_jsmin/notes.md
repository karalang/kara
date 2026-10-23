# mini_jsmin

A port of [jsmin](https://pypi.org/project/jsmin/) 3.0.1 (MIT, Dave St.Germain —
the Python translation of Douglas Crockford's original jsmin): a **JavaScript
minifier**. It strips comments and collapses insignificant whitespace while
preserving the program's meaning — the kind of entropy stage a web-asset build
pipeline runs before shipping `.js`.

## What it does

`mk.py` writes `data/jobs.tsv`: one lowercase hex string per line, the hex of an
ASCII JavaScript source snippet. For each line the job minifies the decoded
source and prints the hex of the minified bytes:

```
min <hex of the minified source>
```

For example `function f(a, b) {\n    return a + b;\n}\n` minifies to
`function f(a,b){return a+b;}`, and `/*! keep me */\nvar w = 4;` minifies to
`/*! keep me */\nvar w=4;` (a `/*!` comment is preserved).

`oracle.py` replays every line through the vendored `jsmin` library and writes
`expected.out`; `mini_jsmin.kara` reproduces it byte-for-byte on every backend.

## How the port works

jsmin is a **character state machine** over an input stream. The port keeps that
shape exactly, reading through an index cursor over a char array (chars as `i64`
code points, `-1` for end of input) in a `Jsmin` struct that also holds the
output buffer and the two pieces of carried state (`is_return`, and the
`return`-tracking buffer). Every subtle rule is carried over, because each is
what makes the output correct:

- **Literal regular expressions.** A `/` is the start of a regex (not a divide)
  when the previous non-space char is one of `{ ( , = : [ ? ! & | ;` or the code
  just wrote a bare `return`. The regex scanner copies through to the closing
  `/`, honouring `\`-escapes and `[...]` character classes (so the `/` inside
  `/[/]/` does not end it). The `return /re/` case even re-inserts the space the
  whitespace rule would otherwise drop.
- **Comments.** Line comments (`//`) are consumed as a newline; block comments
  (`/* ... */`) are dropped — *unless* they begin `/*!`, the convention for a
  license banner, which is emitted verbatim.
- **Strings.** Single- and double-quoted strings are copied verbatim, with
  backslash-escape counting so an escaped quote (`'it\'s'`) does not end the
  string.
- **Whitespace.** Insignificant spaces and newlines collapse, but a space is
  kept between two identifier characters, and specifically between `+ +` / `- -`
  so that `a + + b` does not become the different program `a++b`.

The minified bytes come out byte-identical to the library's.

## Scope

- The input is treated as a **byte / char stream of ASCII JavaScript**, so a
  byte stream and a Unicode code-point stream coincide and jsmin's non-ASCII
  (`> '~'`) branches never fire. Non-ASCII inside strings and comments would be
  copied through unchanged regardless; non-ASCII *identifiers* are the documented
  boundary.
- An **unterminated** block comment, which sends the library into an infinite
  loop, is guarded (the scan stops at end of input) rather than reproduced.
- The port is the `jsmin()` string-to-string transform; the library's
  `JavascriptMinify` stream wiring, its CLI and the `quote_chars` option are not
  exposed.

The port was checked against the library on every backend over the job list
(comments, the `/*!` rule, quoted strings with escapes, regex literals with
character classes, the `+`/`-`/newline spacing rules and nested structures), and
separately during development against the library over a battery of snippets.

The vendored library is pure Python and imports only the standard library;
`vendor/` is left untouched.

## Running

```bash
karac run mini_jsmin.kara             # interpret
karac build --mir mini_jsmin.kara     # native binary
python3 -s -P -B mk.py                    # regenerate data/jobs.tsv
python3 -I oracle.py                       # regenerate expected.out
python3 -s -P -B gen_kara.py               # regenerate the embedded jobs
```
