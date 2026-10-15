# cli/dup_finder

A duplicate-file finder in the shape of `fdupes` / `jdupes`: build a small
tree (text files, a name with a space, one with a quote, one with a comma,
two empty files, binary blobs, a 5000-byte file in several copies, and a
same-size file one bit off), list it, stream every file through FNV-1a 64
in 64-byte reads, group by size and hash in a `SortedMap`, confirm each group
byte for byte, print the groups and the bytes they waste, remove every copy
but the first, write an RFC 4180 CSV manifest and read it back. Empty files
are counted and never grouped. The mirror `dup_finder.py` uses `os.walk`,
`os.remove` and the same hash, and wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at 84621d3858.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right):
- `struct Entry` shadows the prelude's `Entry` (a warning, not an error);
  renamed to `FileInfo` in both files.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-54: the file system library has `read_to_string`, `write`
  and `read_lines` and nothing else: no directory listing, `create_dir`,
  `remove`, `rename`, `exists` or metadata. The tree is made with
  `mkdir -p`, listed with `find` and pruned with `rm --` through `sh`, with
  paths single-quoted. Real tools need these natively.
- B-2026-10-14-55: `karac check` accepts any method on `fs` (and `env`,
  `stdout`), even `fs.bogus_op(..)`, so the missing calls above pass check
  and fail only when run.
- B-2026-10-14-44: paths are `ref String` and byte counts `usize`.
- B-2026-10-12-3: `SortedMap.get_mut` does not exist; the group is grown
  with `contains_key` and `groups[key].push(e)`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-44 is fixed, so
the paths are `Str` again and the byte counts are no longer cast to
`i64`. The `sh` script still goes to `Command.arg` as a `String`, which
is the spec. Each of these is the original line again, and the app still
matches on all four MIR surfaces.
