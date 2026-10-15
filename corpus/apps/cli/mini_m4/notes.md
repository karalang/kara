# mini_m4

The macro processor that autoconf, sendmail.cf and many generated config
files are built on. It answers what GNU m4 1.4.19 answers, so the
program is a whole m4: an input reader that pushes every expansion back
to be read again, a tokenizer for words, quoted strings and comments,
argument collection with macros expanded inside it, and the builtins.

The run writes fourteen m4 jobs into `data/` and runs each one as an m4
command line in that directory, printing its standard output, its
standard error and its exit status:
- **basics.m4**: `define` and calls, `$0`, `$#`, `$*`, `$@` and `$10`,
  argument white space, nested quotes, `ifdef`, every shape of `ifelse`,
  `pushdef`/`popdef`, a recursive `reverse` and factorial, `indir` and
  `builtin` (including a macro whose name is not a word), `undefine` of a
  builtin, `__line__`, `dumpdef` and `defn`;
- **text.m4**: `len`, `index`, `substr`, `translit` with ranges, `incr`,
  `decr`, `format` with flags, widths and `*`, and `regexp`/`patsubst`
  with groups, the GNU manual's `capitalize`, an HTML escaper, a CSV
  quoter, an indenter, and the corners of GNU's Emacs regex syntax
  (no character classes, `+` and `?` are operators, `{` is literal, a
  leading `*` is literal, `.` stops at a newline, `^` and `$` match at
  one);
- **eval.m4**: precedence, `**`, 32-bit wraparound, shifts, octal, hex,
  binary and `0r` radix numbers, short-circuit `&&` and `||`, output
  radixes and widths, and m4 1.4.19's rejection of `?:`;
- **quotes.m4**: `changecom` and `changequote` with multi-character
  delimiters, and comments that stop expansion;
- **loops.m4**: the manual's `forloop`, `foreach` and quoting helpers,
  and an HTML table built with them;
- **divert.m4**: diversions, discarding with `divert(-1)`, `undivert`
  into another diversion, `divnum`, and `m4wrap` (last in, first out);
- **site.m4**, three times: an nginx config built from an included macro
  library, with `-D` and `-U` on the command line and `sinclude`;
- **errors.m4**, **eof.m4**, **eofarg.m4** and **exit.m4**: m4's warnings
  and errors, end of file inside a string and inside an argument list,
  and `m4exit`, which drops diversions and wrapped text;
- two jobs with two input files sharing definitions.

**Reading.** Input is a stack: the files, and above them text that
macro calls pushed back. A word that names a macro is called; if a `(`
follows at once, the arguments are collected with leading white space
skipped, macros inside them expanded, quotes removed and nested
parentheses kept. The expansion is pushed back and read again, which is
why `define(`me', `I am $0')` loops forever and needs `$0` quoted. The
builtins m4 recognizes only with arguments (`define`, `len`, `eval`,
...) are plain words without them. Warnings name the line where the
call's name was read, and a file's line count moves on when the first
character of the next line is read, as m4's does.

**Regular expressions** are the engine from mini_sed with an Emacs
syntax parser, matched leftmost-longest as GNU's matcher does, since a
backtracking matcher picks `a` where m4 picks `ab` for `a\|ab`.

**eval** is a recursive-descent parser over m4's operator table with
32-bit wrapping arithmetic. Errors in its right-hand side are ignored
when `&&` or `||` has already decided the result, as m4 does.

Not supported:
- `syscmd`, `esyscmd`, `sysval`, `mkstemp` and `maketemp`, which run
  programs or make files;
- `traceon`, `traceoff`, `debugmode`, `debugfile`, `changeword` and
  frozen state files;
- `defn` of a builtin (the builtin token), `undivert` of a file, `-I`
  search paths and the other command-line options;
- floating-point `format` conversions.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs GNU
m4 1.4.19 on each job in `data/`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_m4.orig.kara to mini_m4.kara

None. `karac check` passed the first version, and it matched
expected.out on the first run on all three MIR surfaces. An older karac
stopped it with an internal error that B-2026-10-15-22 covers, which was
already fixed on main.

Written 2026-10-15 by the real-world apps thread; karac at 35fcd8197c.
