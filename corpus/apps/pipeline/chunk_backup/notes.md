# pipeline/chunk_backup

A deduplicating backup, the way restic or borg store data. Four snapshots
of a generated source tree (60 files, about 580 KB each) are cut into
content-defined chunks, the chunks go into one store that keeps each
distinct chunk once, and the program reports on the store and then
restores every file of every snapshot from it to check the bytes.

- Snapshot k is its own edit of the same base tree: 40% of files stay as
  they are, 40% get one to four line edits (replace, insert or delete),
  the rest grow by 20 lines, one new file appears, and on even k one file
  is gone.
- Cut points come from a gear rolling hash: `h = (h << 1) + gear[byte]`
  in 32 bits, a cut where the low 10 bits are zero once a chunk has 256
  bytes, and a forced cut at 4096. Because the hash only sees the last
  few bytes, an edit changes the chunks around it and leaves the rest of
  the file's chunks as they were, which is what makes the store dedupe.
- A chunk is named by its FNV-1a and djb2 hashes (both 32-bit) and its
  length.
- The reports: per snapshot files, bytes, chunks and how many chunks it
  added to the store; the dedupe ratio; what changed between consecutive
  snapshots; a histogram of chunk sizes; the three most shared chunks.

Every report sorts or keys its output, so nothing depends on hash order
(the store's `Map` is only looked up). The Python mirror `chunk_backup.py`
uses the same generator and wrote `expected.out`.

stmt-par: four groups of statements that do not depend on each other, in
`main()` (`chunk_backup.orig.kara` lines):
- L369-L372 four `make_snapshot` calls, each reading the shared base;
- L375-L378 four `chunk_snapshot` calls, each reading one snapshot and the
  gear table. This is where most of the time goes;
- L390-L396 seven reports that only read the store and the chunk lists;
- L406-L409 four `verify` calls, each rebuilding one snapshot from the
  store.

The four `add_snapshot` calls between them (L383-L386) are the opposite
case: each takes `mut ref store`, and a chunk belongs to the first
snapshot that adds it, so they have to run in order.

## Edits from chunk_backup.orig.kara to chunk_backup.kara

Author errors (the diagnostics were right, and `karac fix` applied the
machine-applicable ones):
- `const M32` is a one-letter name, so it is Type class, not Const class
  (design.md, the naming rules around CN-7). `karac fix` renamed it
  `MASK32`.
- A `mut ref` parameter needs `mut` at the call: `make_line(mut r, ...)`,
  `add_snapshot(mut store, ...)`.
- `let data = snap[i].data;` moves out of a borrowed element. `karac fix`
  offered `.clone()`; the slice is read in place instead
  (`snap[i].data[pos..pos + size].to_vec()`).
- A struct named `File` shadows the prelude's `File` (a warning); it is
  `SnapFile` now.

Compiler gaps: none found.

## Running

```bash
karac run chunk_backup.kara
karac build chunk_backup.kara && ./chunk_backup
python3 -I -B chunk_backup.py > expected.out
```

About 18 s on the MIR interpreter and 0.03 s built.
