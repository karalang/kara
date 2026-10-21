# mini_git: a small git that real git can read

Writes a real `.git` directory inside `proj/`. Objects use git's format:
`<type> <len>\0<body>`, named by SHA-1 and zlib-compressed. The SHA-1 is
`std.crypto.legacy.sha1` (until B-2026-10-21-6 it was this program's own);
the zlib inflater is the program's own. The compressor writes stored
deflate blocks. The inflater handles all three block types, and is checked
on two objects that real git compressed (one dynamic Huffman, one fixed).
Trees use git's binary format and its ordering, where a directory sorts as
if its name ended in `/` (so `src.txt` sorts before `src/`).

The run covers init, add, commit, status (staged, unstaged and untracked),
a line diff, rm, a second commit, log, `ls-tree -r`, checkout of the first
commit and back, and fsck. Then one object is rewritten with the wrong
content and fsck catches it.

The index is the program's own text file (`.git/kara-index`), not git's
binary one.

Oracles:

- `mini_git.py` is the same program using `hashlib` and `zlib`.
  `expected.out` is its output.
- Real git agrees. `git commit` on the same files, author and timestamps
  (with `commit.gpgsign=false`) gives the same commit ids, 50d2533 and
  df2c4cb. `git log` reads the repository this program writes, and
  `git fsck --full` reports the same corrupted blob.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-15:** the string library has no reverse search: no `rfind`,
  `rsplit_once` or path helpers. `parent_dir` scans the bytes by hand.
- **B-2026-10-15-16:** `let v = m.get(k).unwrap_or(String.new());` borrows a
  temporary (core-semantics.md §5.1). `karac check` passes it. Every MIR
  surface refuses it with `borrow check run: bb444[term]: drop of _243 while
  it is borrowed`, which names no source line, so the whole program fails to
  compile with no pointer to the cause. The app uses `.unwrap_or("").clone()`.
- `f()?.field` needed karac at or after 980724f583 (B-2026-10-14-50, fixed).

## Author errors

- `!x` for bitwise not; Kāra writes `~x`.
- The first version ran in the current directory, so the files it was
  started next to (the source itself) became part of the work tree. All paths
  now go through `ROOT`.

`mini_git.orig.kara` is the first draft.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: `text_of(obj.body).lines()[0]` collects the lines first (B-2026-10-18-35). The app matches expected.out on all four MIR backends again (karac at a4b0967495).
