# mini_latexcodec

A port of [latexcodec](https://pypi.org/project/latexcodec/) 3.0.1 (MIT,
Matthias C. M. Troffaes, after David Eppstein's table): the codec that
bibliography tools such as pybtex use to turn LaTeX markup into Unicode and
back. Once registered, it is an ordinary Python codec:

```
> enc latex strict '\xc5ngstr\xf6m'
  -> b'\\AA ngstr\\"om'
> dec latex strict b"Erd\\H{o}s, P\\'al, Fran\\c cois, Fran\\c{c}ois"
  -> 'Erd{ő}s, P\xe1l, Fran\xe7ois, Fran{\xe7}ois'
```

## What it does

`mk.py` builds a library bibliography desk's files (a BibTeX export, UTF-8,
Latin-1 and cp1252 name lists, broken files) and writes 12 jobs of 161
operations to `data/jobs.tsv`. Each job runs in a fresh working directory
holding its files. `oracle.py` replays the jobs through the vendored
library, and `mini_latexcodec.kara` replays them through the port. After
each operation, every file it created or changed is printed: as text, or as
hex when it is not UTF-8. Strings print as `ascii()` prints them, so every
character is visible.

- **The translation table.** `table` prints all 712 LaTeX-to-Unicode
  entries with their tokens, and all 401 Unicode-to-LaTeX ones, in the
  order the library registers them. Math-only entries are registered as
  `$..$` and `\(..\)`, and accent commands get their braced variants
  (`\'{e}` beside `\'e`).
- **Encoding.** `codecs.encode` and `str.encode` through `latex` and
  `ulatex`, with an input encoding (`latex+latin1`, `latex+utf8`,
  `latex+cp1252`) deciding which characters pass through. The control
  space rule: `å b` becomes `\aa\ b` and `åb` becomes `\aa b`. Each error
  mode (`strict`, `ignore`, `replace` as `{\char9731}`, `keep`, and modes
  the codec does not support), with the exact exceptions.
- **Decoding.** Accents with and without braces, dotless `\i` and `\j`,
  quotes and dashes, math in `$..$`, `$$..$$` and `\(..\)`, comments,
  paragraph breaks, control spaces, parameters and unknown commands. The
  error modes of both the lexer and the input encoding, including a
  trailing backslash and bytes the input encoding cannot read.
- **Incremental use.** Encoders and decoders fed in chunks, with `reset`.
  A token split across chunks (`\'` then `e`, `-` `-` `-`), a UTF-8
  character split across chunks, and the decoder's position quirk for a
  backslash left over from an earlier chunk.
- **The lexer.** Its tokens and state machine in one go and in chunks,
  with `getstate`, and the plain (non-translating) decoder.
- **Files.** `codecs.open` for reading and writing, and conversions
  between UTF-8, Latin-1 and LaTeX files, including a round trip of the
  BibTeX export.

An exception prints as `error <Class>: <message>`, and the job goes on.
An incremental operation stops at its first error.

## How the port works

- **The lexer's regexp is a dispatch on the next character.** The library
  joins its token patterns into one alternation; every character starts
  exactly one alternative, so `raw_match` picks the one the regexp would,
  including `---` before `--` before `-`.
- **The table is built at run time** by lexing each LaTeX string, exactly
  as `LatexUnicodeTable.register` does, so the token keys, `max_length`
  and first-entry-wins order match. The decoder keeps the library's token
  buffer and tries the longest match at its end.
- **Codec names are normalized as `codecs.lookup` does** (`latex+utf-8`
  reaches the search function as `latex_utf_8`), so `latex.utf8` is
  unknown and `latex+utf8+x` asks for an input encoding called `utf8_x`.
- **Error messages are built as CPython builds them,** down to
  `UnicodeDecodeError` reporting the byte at a character index and
  `position -1--1` for a token carried over from an earlier chunk.
- **`gen_kara.py` writes `jobs_text()` and `table_text()`** at the end of
  the `.kara` file, so the kata reads no files at run time.

## Compiler gaps

None. The port checked clean and matched the oracle on its first run on
every backend.

## Scope

- **The table's character names are resolved by `gen_kara.py`.** The
  library looks each name up with `unicodedata.lookup` when it loads
  `table.txt`; `table_text()` holds the code points instead.
- **Only the input encodings the jobs name are ported**: ASCII, Latin-1,
  UTF-8 and cp1252, with the `strict`, `ignore` and `replace` handlers.
- **`str.isprintable()` is approximated** for the characters the jobs
  hold, which decides how an error message shows the character it names.
- **The lexer module's own base encoder**, which only splits text before
  combining characters, is not exercised: the codec's encoder overrides
  it.

The vendored library is unchanged.

## Running

```bash
karac run mini_latexcodec.kara              # interpret
karac build --mir mini_latexcodec.kara      # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # regenerate jobs_text()
```
