# mini_coreutils

The small coreutils that shell scripts call between the text filters, as
one multi-call program. It answers what GNU coreutils 9.4 answers in the
C locale. The tools are `printf`, `expr`, `seq`, `test`, `basename`,
`dirname`, `od`, `base64`, `fold`, `expand`, `unexpand`, `factor` and
`cat`. Its sibling `mini_textutils` has the text filters (`sort`, `cut`,
`uniq`, `tr` and the rest).

The run writes the input files into `data/` and a `jobs.tsv` of 128
pipelines. Each line holds one pipeline's tab-separated arguments, with
a tab inside an argument written as a vertical tab. The stages are
separated by a group-separator byte (0x1d) argument, so that a `|` can
be an argument of `expr`. The run then runs each pipeline, feeding each
stage's output to the next, and prints what a shell prints: a `$ ...`
line, the last stage's stdout, every stage's stderr in order, and
`[exit N]` when the last stage's status is not 0.

The inputs:
- a small binary header with control bytes, a NUL and a UTF-8 `é`, for
  `od`;
- a file of tab-separated notes with one long line, for `expand`,
  `unexpand` and `fold`;
- an indented config block mixing spaces and tabs;
- a secret, its base64 encoding, a wrapped encoding and a corrupt one;
- a short story with a blank line, for `fold -s`;
- an empty file, a list of ports and a `conf.d/` directory, for `test`
  and `od -w`.

The pipelines do what such scripts do:
- `printf` with flags, widths, precisions and `*`, every integer
  conversion, `%f`, `%c`, `%s`, `%b` and `%%`; the format reused for
  extra arguments; `0x`, octal and `'c` arguments; the backslash escapes
  of the format and of `%b`, including `\c`; and errors for a partly
  numeric argument, a non-numeric one, an invalid directive and no
  format;
- `expr` arithmetic with C's truncating division, comparisons, `|` and
  `&` with their null rules, `length`, `substr`, `index`, and `:` and
  `match` with anchored basic regular expressions and a `\(...\)` group;
  and its exit status 1 for a null result and 2 for division by zero, a
  non-integer argument and two syntax errors;
- `seq` up, down, with a separator, equal widths (including a negative
  start), decimal steps and starts, `-f` with `%g` and `%f`, an empty
  sequence, and its three errors;
- `test` on files, strings and integers, with `!`, `-a`, `-o` and
  parentheses, GNU's special cases for one to four arguments, and its
  errors for an invalid integer and a missing argument;
- `basename` with a suffix, `-s` and `-a`, and `dirname` on several
  paths, including the root and doubled slashes;
- `od` with its default octal words, `-c`, `-t a`, one-, two-, four-
  and eight-byte types, `z`, `-A` in all four radixes, `-N`, `-j`, `-w`,
  `*` for repeated lines and `-v` to show them; and an invalid type;
- `base64` encoding with the default and other wrap widths, decoding
  wrapped input, decoding what stops at a bad character (GNU still
  prints the bytes before it), and a missing file;
- `fold` by columns (tabs to the next stop), by bytes and at blanks, and
  a width of 0;
- `expand` with the default stops, one size, a list of stops (a tab
  after the last one becomes a space) and `-i`; `unexpand` of leading
  blanks, with `-a`, and with `-t` (which implies `-a`), and a round trip
  through both;
- `factor` from arguments and from standard input, a twelve-digit
  composite and a ten-digit prime, and an invalid number.

**Options** follow GNU's syntax: clustered short options, a value
attached or in the next argument, and options after the operands. `seq`
takes a negative number as an operand, as GNU does.

**`printf`** reads numbers the way `strtoimax` does and reports what it
could not convert, then goes on. `%f` rounds the decimal digits half away
from zero, which matches glibc on these arguments. **`unexpand`**
follows GNU's loop, including its rule for a single blank just before a
tab stop.

Not supported:
- `printf`'s `%e`, `%g`, `%a` and `%q`;
- `seq -f` with `%e`, or `%g` of a number that is not whole;
- `expr`'s `\{m,n\}`, `\+` and `\?`, starred groups, and integers past
  64 bits (GNU uses arbitrary precision);
- `test`'s file tests other than `-e`, `-f`, `-d` and `-s`;
- `od`'s floating-point types and more than one type at a time;
- `fold`, `expand` and `unexpand` of multibyte characters (strings are
  bytes, as in the C locale).

**Oracle.** `expected.out` is GNU coreutils 9.4's output on the same
files, made by `oracle.py`, which is run from this directory after
`mk.py` writes `data/` and `jobs.tsv`. `gen_kara.py` prints the Kāra
data section from the same files.

## Edits from mini_coreutils.orig.kara to mini_coreutils.kara

Author errors:
- `const B64` is rejected: a constant needs a name with more than one
  letter, and `B64` has one. It became `B64_ALPHABET`.
- `Test.syntax(msg: String)` stored its parameter, which a bare
  parameter cannot give away. It takes `msg: own String`.
- The regular-expression struct was called `Regex`, which shadows the
  prelude type (a warning). It became `Bre`.

Compiler gaps:
- B-2026-10-18-8: the lexer rejects `\xHH` in string literals, which
  design.md lists among the text-literal escapes. The original writes
  the header file's control bytes and the stage separator as `\x01` and
  `\x1d`; `mini_coreutils.kara` writes `\u{1}` and `\u{1d}`, which are
  the same bytes.

After those edits the program matched expected.out on the first run on
both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 4c62f568f7.
