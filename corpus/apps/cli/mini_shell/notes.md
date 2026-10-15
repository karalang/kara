# cli/mini_shell

A small POSIX-flavoured shell running a script. The tokenizer handles single
quotes, double quotes, backslash escapes and `$NAME`, `${NAME}` and `$?`;
expansion happens when each command runs, so `$?` in `a || echo $?` sees
`a`'s status. The parser handles `;`, `&&`, `||`, `|` and the redirects `<`,
`>`, `>>` and `2>`, and reports unterminated quotes and dangling operators
(status 2). Commands run as real child processes, with no `sh` in between,
and get the exported variables in their environment; `export`, `unset` and
`echo` are builtins, and a missing program is status 127. The script covers
quoting, `sort | uniq -c`, redirects into and out of files, `&&`/`||`
chains, a command that is not found, `ls` stderr sent to a file, and what a
child `sh -c` sees before and after `unset`.

The mirror `mini_shell.py` is the same tokenizer, parser and runner in
Python, running children with `subprocess` (no shell), and wrote
`expected.out`. Needs `printf`, `sort`, `uniq`, `cat`, `wc`, `tr`, `head`,
`grep`, `ls`, `true`, `false` and `sh` on PATH.

Written 2026-10-15 by the real-world apps thread; karac on origin/main of
2026-10-15 (c274de40ea).

## Edits from mini_shell.orig.kara (source.orig.kara) to mini_shell.kara

Author errors (the diagnostics were right, or the spec says so):
- `for t in toks` borrows each element (D5), so moving a word out of it
  was refused; written `toks.into_iter()`, as the diagnostic said.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-15-3: with `enum RedirKind { In, Out, Append, Err }` in scope,
  the MIR builder builds `Err("..")` as `RedirKind`'s variant 3 ("invalid
  MIR: variant 3 out of range") and refuses the `Err(e)` pattern. The
  variant is `ErrOut`.
- B-2026-10-15-4: `cmd.words.iter().map(|w| self.expand(w)).collect()` is
  refused on every MIR surface ("`self` here is not lowered yet"); it is a
  `for` loop.
- B-2026-10-15-5: `fs.write` returns `Result[Unit, IoError]`, which does
  not unify with the spec's `Result[(), IoError]` that `append_file`
  returns, so they cannot share an `if`; each is reduced to `.is_ok()`.
- B-2026-10-15-6: there is no process spec, and no way to connect one
  child's stdout to another's stdin, so pipeline stages run one after
  another with the output buffered.
- B-2026-10-14-53 (`fs.write` moves a `String` path), B-2026-10-14-43
  (literal patterns on a `Str`) and B-2026-10-14-36 (`String.clear` not
  compiled), as in the other apps.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.
