# cli/mini_grep

A grep with its own regular-expression engine: a parser to an NFA program
(literals, `.`, classes and `\d \w \s`, greedy and lazy `* + ?`, `{m,n}`,
alternation, groups, anchors) run by a Pike VM with leftmost-first
priority, so it reports the same match a backtracking engine does. The
front end handles `-i -v -c -l -n -o` and `-C N` context with GNU's `--`
separators and `path-N-` context lines, over a source tree and log files
written to disk, with grep's exit statuses (0 match, 1 none, 2 error).
The mirror `mini_grep.py` uses Python's `re` (ASCII classes) as an
independent oracle for every match, and wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at 0b14473a2e.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right, or the spec says so):
- `enum Node { .. Repeat(Node, ..) }` contains itself; design.md says a
  recursive enum is a `shared enum`. Changed in both files.
- `for _ in 0..lo` with `lo` bound by matching through a `ref`: the bounds
  are `ref i64`; written `*lo`.
- `let a = args[i];` moves out of an index (D5); `ref args[i]`.
- `find .. | sort` in `sh` reports `sort`'s exit status, so a missing path
  looked like an empty directory; the program now sorts in Kāra and checks
  `find`'s own status.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-63: `Regex` (legacy has `compile`, `is_match`, `find`,
  `find_all`, `replace_all`) passes `karac check` but has no v2 natives
  (`call of unknown function Regex.compile` on the interpreter, `native
  Regex.is_match is not lowered yet` compiled), and docs/library has no
  regex section. The engine here is part of the program.
- B-2026-10-14-54: no directory listing.
- B-2026-10-12-7 and B-2026-10-14-44 as in the other apps.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.
