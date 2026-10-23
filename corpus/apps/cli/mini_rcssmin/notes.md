# mini_rcssmin

A port of [rcssmin](https://pypi.org/project/rcssmin/) 1.1.2 (Apache-2.0;
André Malo), the CSS minifier that Python asset pipelines and static-site
builds use to shrink style sheets before serving them. It removes
comments, white space and redundant semicolons, keeps strings and
escapes, keeps `/*! ... */` comments on request, handles the Mac IE5 and
IE7 comment hacks, and strips the spaces out of `url()` values.

```
> snippet a { color : red ; } ; b { }
  'a{color:red};b{}'
> snippet /* \\*/ .mac { color : red } /* */ .all { color : blue }
  '/*\\*/.mac{color:red}/**/.all{color:blue}'
```

## What it does

`mk.py` writes 285 jobs (and 5 section comments) to `data/jobs.tsv`, the
library's own test sheets (46 of its own and 43 it took from the YUI
compressor suite) to `data/css`, two sheets for the command, a four-sheet
site with a manifest, and the minifier's regular expressions to
`data/patterns.txt`. `oracle.py` runs each job against the vendored
library's pure-Python minifier and prints the result, or
`error <class>: <message>` when it raises; `mini_rcssmin.kara` runs the
same jobs. 1 job raises.

- **`file`** minifies each test sheet, with and without
  `keep_bang_comments`, and prints its length before and after.
- **`snippet`** minifies 85 short sheets (94 jobs), each aimed at one
  rule: comments and banged comments, the Mac IE5 `/*\*/ ... /**/` hack
  and the IE7 `>/**/` hack, `:first-line` and `:first-letter`, the
  @-rules that open a group (`@media`, `@supports` and the
  vendor `@keyframes`) and those that do not, quoted strings with escaped
  newlines, unterminated strings and comments, `url()` with and without
  quotes, with escapes, and with `data:` URLs (plain, base64, upper
  case), identifiers with escapes, `calc()`, `!important`, CRLF, form
  feed and `\u2028` line ends, and non-ASCII text.
- **`bytes`** passes a sheet as bytes, which the library decodes and
  encodes as Latin-1, and prints the bytes it returns.
- **`cli`** runs `python rcssmin.py <args> < <file>` with `-b`, `-p`,
  `-bp`, `-pb` and an unknown flag, on a sheet with CRLF line ends among
  others (stdin reads them as `\n`).
- **`bundle`** is a build step: it minifies the sheets a manifest lists,
  each with its own flag, into one file, reports the bytes per sheet and
  the total saved, skips a sheet that is missing, and prints the bundle
  read back. A missing manifest raises `OSError`.

## How the port works

- **The library's patterns run on the mini_re engine.** rcssmin builds
  its main pattern (19 groups, lookbehind and lookahead, about 1800
  characters) and six helpers inside `_make_cssmin`. mk.py reads the
  compiled patterns out of the module's closures and writes their text to
  `data/patterns.txt`, and the port compiles them with the engine from
  [mini_re](../mini_re), which has Python `re` semantics.
- **`match.lastindex` picks the handler.** The library's replacement
  function looks up `lastindex` in a table, so which group closed last
  decides what a match becomes, including groups set inside a lookahead
  (a space before `:` or `;`, a `;` before `}`). The port reads the
  engine's `lastindex` the same way, and keeps the three pieces of
  closure state (inside a rule, open @-groups, an open IE5 hack) in a
  struct.
- **Each `sub` is the engine's matches stitched together.** None of the
  library's patterns can match an empty string, so a substitution is the
  text between successive matches plus each match's replacement.
- **The command reads stdin as text.** It is fed the file with `\r\n` and
  a lone `\r` turned into `\n`, as Python's text-mode stdin does, which is
  why the CRLF sheet's escaped newline inside a string comes out joined.

## Compiler gaps

None. `karac check` passed on the first assembly, and the interpreter
matched the oracle byte for byte on its first run, as did the LLVM, ASan
and native builds.

## Scope

- The C extension (`_rcssmin`) is not ported; it is a faster copy of the
  same minifier, and the oracle uses the pure-Python one.
- `bytearray` input is not exercised; it takes the same path as `bytes`.
- `.lower()` on a `data:` URL's media type is ASCII-only here; Python's
  full Unicode lowering differs only for characters such as the Kelvin
  sign, which cannot appear in a media type.

## Running

```bash
karac run mini_rcssmin.kara                  # interpret
karac build --mir mini_rcssmin.kara          # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
