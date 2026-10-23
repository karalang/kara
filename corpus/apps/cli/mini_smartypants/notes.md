# mini_smartypants

A port of [smartypants](https://pypi.org/project/smartypants/) 2.0.2
(BSD-3; John Gruber, Chad Miller, Yu-Jie Lin, Leo Hemsted, Justin Mayer):
the filter a static-site generator runs its HTML through to turn straight
quotes, double dashes and three dots into curly quotes, dashes and
ellipses, while leaving code, scripts and styles alone.

```
> smartypants -a 2 posts/recipe.html
  | <h1>Grandma&#8217;s &#8220;naïve&#8221; café crème</h1>
  | <p>Serves 4&#8211;6. You&#8217;ll need a 9&#8221; pan (about 23 cm) ...
```

## What it does

`mk.py` writes the posts of a small blog to `data/posts/` (week notes,
release notes with inline code, a recipe with accented text, a
Dreamweaver-style page, a page with a script and a style, a page full of
backslash escapes, broken HTML, plain text and an empty file) and a script
of 2211 steps to `data/jobs.tsv`. `oracle.py` replays the script through
the vendored library and `mini_smartypants.kara` replays it through the
port. Strings print as `repr()` shows them, rendered posts line by line.

- **The library's examples and tests**, as its docstrings and test suite
  give them.
- **Every text with each of seven attributes.** Some 900 calls on
  `smartypants()` with quotes next to punctuation, digits, tags, entities
  and non-ASCII letters, and dashes and dots of every length, under seven
  attribute values that between them use each flag.
- **Each converter on its own** (`convert_quotes`, the backtick, dash and
  ellipsis converters, `convert_entities` in its three modes,
  `process_escapes`, `_tokenize` and `_tags_to_skip_regex`).
- **Changing `Attr.default` and `tags_to_skip`**, including an empty tag,
  which then skips every tag.
- **The posts**, each rendered with the default and two other attributes.
- **The command line**, run in `data/`: `-a` with letters, numbers,
  unknown letters and `set1` (read as letters), `-s` lists, `--attr=` and
  `--skip=`, `-v`, `--`, several files, a missing file and missing or
  unknown arguments, with argparse's usage line, error messages and exit
  status.

## How the port works

- **Text is a Vec of code points**, so slicing and lookaround work on
  characters as Python's `str` does.
- **The regular expressions are the library's.** `gen_kara.py` runs each
  function once with `re` wrapped so it records every pattern and flag it
  is given, and the port compiles those patterns at start-up for a small
  backtracking matcher. The matcher has what they use: classes, groups and
  captures, alternation tried in order, greedy and lazy repetition,
  lookahead and lookbehind, `\b`, `\w`, `\d`, `\s`, `IGNORECASE`, `DOTALL`
  and `VERBOSE` (whose `#` comments make the `&#8211;` entity in one
  pattern a comment, as in Python). `_tags_to_skip_regex()` builds its
  pattern from the tag list on each call, so the port formats it the same
  way and compiles it too. A search only tries the positions where one of
  the characters a match can begin with occurs.
- **The Unicode classes come from Python.** `\w`, `\d` and `\s` and the
  printable test behind `repr()` use runs of code points read off
  `unicodedata` by `gen_kara.py`.
- **The command line is the library's `smartypants` script**, with the
  parts of `argparse` it uses: attached and separate option values, `=`
  forms, `--`, the usage line and the "expected one argument" and
  "unrecognized arguments" errors, each with exit status 2. Warnings for
  unknown attribute letters print once each, as the script prints them.

## Compiler gaps

None. `karac fix` made 3 edits, all of them the call-site `mut` marker on
an argument passed to a `mut ref` parameter, and three `.clone()`s took
strings out of arrays of string literals. The port then matched the oracle
on every backend.

## Scope

- `-h`/`--help` is not run: its text is argparse's own formatting.
- Input is UTF-8 text; the library's Python 2 paths are not used.

## Running

```bash
karac run mini_smartypants.kara                 # interpret
karac build --mir mini_smartypants.kara         # native binary
python3 -s -P -B mk.py                          # regenerate data/
python3 -I -B oracle.py > expected.out          # regenerate expected.out
python3 -I -B gen_kara.py                       # regenerate the generated tables
```
