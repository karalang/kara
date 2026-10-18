# mini_make: a small make

This app reads a Makefile, builds the dependency graph, and runs the recipes
of out-of-date targets through `sh`, comparing file modification times read
with `fs.metadata`. It supports:

- recursive (`=`), simple (`:=`) and appending (`+=`) variables, `$(VAR)`,
  `${VAR}` and computed names such as `$($*)` (the `print-%` idiom);
- the automatic variables `$@`, `$<`, `$^` (deduplicated) and `$*`;
- the functions `wildcard` (through `fs.read_dir`), `patsubst`, `notdir` and
  `words`;
- pattern rules, order-only prerequisites (`| build`), `.PHONY`, and inline
  recipes after `;`;
- the recipe prefixes `@` (silent) and `-` (ignore errors), `-n` (dry run)
  and `-f FILE`;
- GNU make's messages for a circular dependency, a missing rule (with and
  without the target that needed it), a failing recipe, an ignored failure,
  "Nothing to be done" and "is up to date".

The scenario builds a project, rebuilds nothing, edits one source (one
object and the link rebuild), asks for an up-to-date object, edits the
shared header (every object rebuilds), runs `check` with an ignored
failure, prints two variables, asks for a missing target, runs three
targets of a broken Makefile, deletes an object and dry-runs, cleans, and
dry-runs from scratch.

Oracle: `mini_make.py`, a port of the Kara. The independent check is GNU
Make 4.3: run on the same files and steps, its combined output is
byte-identical to `expected.out`.

## Edits from mini_make.orig.kara to mini_make.kara

No author errors. Compiler gaps, worked around and listed in the source
header:

- **B-2026-10-15-23:** `karac check` refuses two programs that every MIR
  surface runs, calling an argument moved. `line.find(op)`, with `line` a
  `Str` parameter, moves `op`, so the operator loop reads `op.len()` first
  and compares operators by index. `p.replacen("%", stem, 1)` moves the
  String `stem`, so the app passes `stem.clone()`.

Written 2026-10-15 by the real-world apps thread; karac at f32a0eebf2.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
