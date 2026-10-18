# mini_find

`find` walks a directory tree and prints, deletes or formats the files
that match an expression. The program ports GNU find 4.9: its link
modes, its walk, its tests and actions, `-printf`, and its error
messages. It prints what `mini_find.py`, the same program in Python,
prints, and `real.py` checks that mirror against the real `find`.

The run writes `data/manifest.txt` and `data/jobs.txt`, builds
`data/proj` from the manifest (directories, files of given sizes,
symbolic links including a dangling one and one to a directory), and
sets every mode, including setuid, setgid and sticky bits, and every
modification time. Then it runs the jobs.

**Walk.** Start paths come before the expression, with `-H`, `-L` or
`-P` before them. The walk is pre-order, or post-order with `-depth`
or `-delete`, and honours `-maxdepth` and `-mindepth`. A start path is
printed as given, trailing or doubled slashes included. `-L` follows
links, so it descends `docs-link` and reports only the dangling link as
type `l`; `-H` follows only a start path.

**Tests.** `-name`, `-iname`, `-path`, `-ipath`, `-wholename`,
`-lname` and `-ilname` use find's globbing, where `*` matches a leading
dot and a `/`, with `[...]`, `[!...]`, `[^...]` and backslash escapes.
`-type` and `-xtype` take lists such as `f,l`. `-size` rounds up to its
unit, so `-size -1k` matches only empty files. `-perm` takes an exact,
`-all` or `/any` mode, octal or symbolic. `-mtime` counts whole days
from "now" (2026-10-15 12:00:00 UTC), or from the end of today with
`-daystart`; `-mmin` counts minutes without rounding. `-newer` and
`-newermt` compare modification times.

**Expression.** `( )`, `!` and `-not`, `-a` and juxtaposition, `-o` and
the comma, with GNU's precedence. When the expression has no action,
`-print` is added to the whole of it. `-prune`, `-quit`, `-delete` and
`-printf` behave as in GNU find; `-printf` has `%p %P %f %h %d %y %Y %l
%m %M %s %t`, the `%T` time fields, widths, `-` and precision, and the
escapes `\n \t \\ \ooo \c`.

**Errors.** Unknown predicates, missing arguments, words after the
expression, bad `-type`, `-size`, `-perm`, depth and time arguments,
unbalanced or empty parentheses, dangling operators, `-delete` with
`-prune`, a format ending in `%`, and unknown escapes and directives
(as warnings) give find's messages and exit status.

**Divergence.** GNU find lists a directory in readdir order. Kāra's
`fs.read_dir` returns names sorted, so mini_find, and its mirror, list
in sorted order. `real.py` compares each job's lines as a sorted list
(the first match of `-quit` aside), and its errors and exit status
exactly. `-printf %s` is not used on directories, whose size depends on
the file system, nor time fields on links, whose times cannot be set.

Run `python3 real.py` to check the mirror against the real find. It
moves the tree to the machine's clock, so the time tests line up.

## Kāra notes

Author errors in the first draft (`mini_find.orig.kara`): two
parameters that are stored had to be `own`, as did the path handed to
an `Item`; and structs named `Parser` and `Entry` shadow prelude types
(a warning), so they are `Cmd` and `Item`.

Compiler gaps: none found. The port matched the mirror on every backend
at the first run.
