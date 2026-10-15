# cli/log_rotate

A log writer with size-based rotation (a `File` held in a struct and
replaced on rotation, older segments past `keep` truncated), a scan of every
segment line by line into per-level and per-component counts, a tail of the
live file by seeking back from its end in growing steps, and a CSV summary
written and read back. The mirror `log_rotate.py` wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at c8dad803. The
first draft also called `splitn`, `rfind` and `trim_end`, which are not in
docs/library/strings.md; source.orig.kara is that draft with those three
replaced by spec methods.

## Edits from source.orig.kara to source.kara

Author errors under the v2 rules:
- `IoError.Other(m) => m` moves out of a borrowed `e`: `m.clone()`.
- `let levels = ["INFO", ..]` is an array of `String` (a literal is a
  `String` by default), so `levels[i % 6]` moves out of an index: the arrays
  are annotated `Array[Str, N]`.
- `scan(p, sum)` passes a fresh binding to `mut ref Summary`: `mut sum`.
- `struct Entry` shadowed the prelude `Entry` (a warning): renamed `LogEntry`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-12-6: `i64.parse` returns `Option`.
- B-2026-10-12-7: `lines` / `split` are eager `Vec[String]`.
- B-2026-10-14-44: `File.open` / `File.create` take `ref String`, and
  `File.read` / `File.write` return `usize`, where io.md gives `Str` and `i64`.
- B-2026-10-14-45: `BufReader` is not implemented on v2 (`call of unknown
  function BufReader[File].new` on `__mir-run`, `native ... is not lowered
  yet` on `__mir-llvm-run`), so `scan` reads each segment whole with
  `fs.read_to_string`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-44 is fixed, so
`File.open` and `File.create` take the `Str` path and the counts are no
longer cast to `i64`. Each of these is the original line again, and the
app still matches on all four MIR surfaces.
