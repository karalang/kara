# mini_rjsmin

A port of [rJSmin](https://pypi.org/project/rjsmin/) 1.2.4 (Apache-2.0;
André Malo), the JavaScript minifier that django-compressor, webassets
and other Python asset pipelines call to shrink scripts before serving
them. It follows Douglas Crockford's jsmin.c: comments and space go,
string, template and regular-expression literals are kept, and a space
or newline is kept only where the script would otherwise change meaning.

```
> snippet a = b + + c ; d = e - - f ; g = h + ++i ; j = k - --l
  'a=b+ +c;d=e- -f;g=h+ ++i;j=k- --l'
> snippet x = ( a ) /b/.source
  'x=(a)/b/.source'
```

## What it does

`mk.py` writes 141 jobs (and 6 section comments) to `data/jobs.tsv`, the
library's 14 test scripts to `data/js`, two real scripts from its
benchmark set to `data/bench` (Django admin's `DateTimeShortcuts.js` and
Google's `markermanager.js`, each keeping its own licence header), a
script with CRLF line ends for the command, a three-script site with a
manifest, and the minifier's regular expressions to `data/patterns.txt`.
`oracle.py` runs each job against the vendored library's pure-Python
minifier and prints the result, or `error <class>: <message>` when it
raises; `mini_rjsmin.kara` runs the same jobs. 1 job raises.

- **`file`** minifies each script, with and without
  `keep_bang_comments`, and prints its length before and after.
- **`snippet`** minifies 80 short scripts (89 jobs), each aimed at one
  rule: where a `/` starts a regular expression (after `)` followed by a
  method call, after an operator, after `return` on the same line or the
  next) and where it divides; `+ +`, `- -` and `++`/`--` next to them;
  the newlines kept before `!`, `(`, `[`, `{`, `+`, `-`, `++` and `--`;
  strings, template literals and line continuations; unterminated
  strings, regular expressions and comments; bang comments in each
  position; CRLF, lone CR, form feed, vertical tab and ` `; and
  non-ASCII identifiers and strings.
- **`bytes`** passes a script as bytes, which the library decodes and
  encodes as Latin-1, and prints the bytes it returns.
- **`posers`** runs `jsmin_for_posers`, the library's digest of the same
  minifier as two literal patterns, on four scripts and checks it agrees
  with `jsmin`.
- **`cli`** runs `python rjsmin.py <args> < <file>` with `-b`, `-p`,
  `-bp`, `-pb` and an unknown flag (stdin reads `\r\n` as `\n`).
- **`bundle`** is a build step: it minifies the scripts a manifest lists,
  each with its own flag, into one file, reports the bytes per script and
  the total saved, skips a script that is missing, and prints the bundle
  read back. A missing manifest raises `OSError`.

## How the port works

- **The library's patterns run on the mini_re engine.** `_make_jsmin`
  builds two large substitution patterns (about 2600 characters each,
  with lookbehind and lookahead) and a `keep` pattern for bang comments.
  mk.py reads the compiled patterns out of the module's closures, and the
  three literal patterns out of `jsmin_for_posers`, and writes their text
  to `data/patterns.txt`; the port compiles them with the engine from
  [mini_re](../mini_re), which has Python `re` semantics.
- **The subbers test groups the way Python does.** The replacement
  functions pick a branch by which group is truthy (took part and is not
  empty), so the port tests exactly that, in the library's order.
  `jsmin_for_posers` writes the same choices as one `or` chain; each
  branch yields non-empty text whenever its test passes, so the same
  functions serve both.
- **The script is wrapped and stripped as in the library.** A newline is
  added at each end before the substitution and the result is stripped
  of Python whitespace (which includes `\x1c`-`\x1f`, `\x85`, `\xa0` and
  the Unicode spaces).

## Compiler gaps

None. `karac check` passed on the first assembly, and the interpreter
matched the oracle byte for byte on its first run, as did the LLVM, ASan
and native builds.

## Scope

- The C extension (`_rjsmin`) is not ported; it is a faster copy of the
  same minifier, and the oracle uses the pure-Python one.
- `bytearray` input is not exercised; it takes the same path as `bytes`.
- The larger benchmark scripts (jQuery, Bootstrap, Knockout, apiviewer)
  are left out to keep the interpreter run short.

## Running

```bash
karac run mini_rjsmin.kara                   # interpret
karac build --mir mini_rjsmin.kara           # native binary
python3 -s -P -B mk.py                       # regenerate data/
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the embedded data
```
