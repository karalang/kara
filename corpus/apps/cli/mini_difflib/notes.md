# mini_difflib

Python 3.13's `difflib`, ported from `Lib/difflib.py`: `SequenceMatcher`
with its junk and autojunk heuristics, `get_close_matches`, the `Differ`
behind `ndiff` with its intraline `?` hints, `restore`, and the unified
and context diff formats.

The run writes text files into `data/` and runs `data/jobs.tsv`, 56 jobs
with tab-separated fields. Each job prints `== ` and its fields joined
by ` | `, then its output.

The files are two versions each of an nginx config, of a small Python
module, of a page of notes (with a tab, double spaces, an `é` and no
newline at the end of the new version), and of a 260-line access log.
The log is long enough (200 lines or more) for autojunk to drop its two
commonest lines. There are also an empty file, two identical files and a
list of 36 words.

The jobs cover:
- `opcodes`, `blocks` and `ratios` (`ratio`, `quick_ratio`,
  `real_quick_ratio`) on pairs of strings, including empty ones;
- `junk`: a matcher that treats a space as junk, and `longest`:
  `find_longest_match` with and without that junk, where a junk run
  joins the match next to it;
- `close`: `get_close_matches` with several `n` and `cutoff` values, and
  its two `ValueError`s;
- `unified` and `context` diffs with 0 to 3 lines of context, of an
  empty file, of identical files, of a file with no final newline, and
  of the access log;
- `ndiff`, with and without `IS_LINE_JUNK`, and `restore` of both sides
  (and its `ValueError`);
- `line_ratio`: the log's ratio, popular lines, blocks and opcodes with
  autojunk on and off;
- a missing file.

**The port** follows `difflib.py` function by function: `__chain_b`,
`find_longest_match` with its four extension loops, the queue in
`get_matching_blocks` and the joining of adjacent blocks,
`get_opcodes`, `get_grouped_opcodes`, the three ratios, the `nlargest`
order of `get_close_matches` (score, then word, both descending),
`_fancy_replace` with its 0.74/0.75 cutoffs and `_keep_original_ws`,
`restore`, and both range formats. The matcher is generic over its
element type: characters for strings, lines for files.

Not supported:
- `HtmlDiff`, `diff_bytes` and `SequenceMatcher`'s `set_seq1` caching;
- `IS_LINE_JUNK` takes `\s` to be what `trim` removes.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py`. Run `mk.py` first; it writes `data/`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_difflib.orig.kara to mini_difflib.kara

Author errors:
- `use std.collections.{Map, Set};` is not Kāra; `Map` and `Set` need no
  import.
- Function types were written `fn(ref T) -> bool`. Kāra writes
  `Fn(T) -> bool`.
- The methods declared their receivers as `self: ref Self`; Kāra writes
  `ref self`.
- `!` was used for "not"; Kāra writes `not`.
- `eprint` does not exist; it is `eprintln`.
- `String` has no `trim_end_matches`. The word list uses `trim_end`, and
  a job line loses its newline through a small `chomp`, since `trim_end`
  would also drop an empty last field.
- `restore` did not refuse a `which` other than 1 or 2, as Python's
  does.
- `karac fix` made `Matcher.new` and `pad_left` take `own` parameters,
  cloned an opcode's tag where a `for` loop moved it out of the borrowed
  element, and added four `mut` markers.

Compiler gaps:
- B-2026-10-12-3: there is no `Map.get_mut`, although
  `docs/library/collections.md` lists it. `__chain_b` builds its index
  lists with `entry(..).or_insert(..)`.
- B-2026-10-18-12: a generic function passed as a value is not lowered
  on the MIR surfaces. `no_junk[T]` became `no_char_junk` and
  `no_line_junk`.
- B-2026-10-18-13: in `match avail.get(e) { Some(n) => *n, None =>
  full.get(e).unwrap_or(0) }` the second arm stays a reference on the MIR
  surfaces; mir-interp stops and mir-llvm reads an address.
  `quick_ratio` matches on both lookups instead.

After those edits the program matched expected.out on both MIR
surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 83fab1e1e8.

Reverted 2026-10-18, karac at 79a6c2eb9a: B-2026-10-18-13 is fixed, so
`quick_ratio` uses `fullbcount.get(elt).unwrap_or(0)` in the match arm
again. That is the original line, and the app still matches on all four
MIR surfaces.
