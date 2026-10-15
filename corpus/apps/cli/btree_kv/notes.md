# btree_kv

A page-based B+tree key-value store with a write-ahead log, over real
binary files. It is the storage engine under an embedded database.

**Format.** The file is an array of 256-byte pages, all little-endian:
- Page 0 is a header: magic `KBT1`, page count, root, key count and
  commit number.
- A leaf packs `[klen][vlen][key][value]` entries after a 7-byte header
  that links it to the next leaf.
- An inner node packs `[klen][key][child]` after its first child.

An insert, or a replace that grows a value, splits a node whose encoding
outgrows the page and promotes a separator. A root split adds a level.
Range scans walk the leaf chain.

**Durability.** Every change is first appended to `kv.wal` as
`[op][klen][vlen][key][value][crc32]` and synced. A commit serialises the
pages to `kv.db.tmp`, syncs it, renames it over `kv.db`, and empties the
WAL. Opening a store loads the database and replays the WAL up to the
first record that is incomplete or fails its checksum, so a write torn by
a crash is discarded.

**The run.**
1. 600 random operations (70% put, 20% delete, 10% get) over 400 keys,
   with a commit every 100. A `Map` model is kept alongside; it reaches
   47 pages and height 3.
2. Eleven more writes are logged, then the run "crashes" before
   committing. The last WAL record is torn by cutting 3 bytes.
3. Reopen. Ten records are replayed and the torn 23 bytes are discarded.
4. Every key is checked against the model. The invariants are checked:
   sorted keys, keys within their separators, uniform leaf depth, a leaf
   chain in tree order, and the key count.
5. A range scan.
6. A file with a bad magic and a truncated file are refused with their
   reasons. A reload re-serialises byte-identically.

Oracle: `btree_kv.py`. It is a port that writes the same page layout, and
`expected.out` is its output. The output includes the database file's
CRC-32 at each commit, so the Kāra and Python stores agree byte for byte,
not just on the printed answers.

## Edits from btree_kv.orig.kara to btree_kv.kara

Author errors (the diagnostics were right):
- `read_file(*p)` dereferenced an element of an `Array[String]`, which
  would move it. It is now `read_file(p)`.

Compiler and library gaps, worked around and listed in the source header:
- **B-2026-10-15-28 (new, library spec):** `File` opens for reading, for
  truncating writes, or for appending. Nothing writes into the middle of
  an existing file or sets its length, so a commit rewrites the whole
  database and the crash test rewrites the WAL shorter. There is also no
  byte-level `fs.read`.
- **B-2026-10-14-68:** `[DB, WAL, "kv.db.tmp"]` types the literal as
  `String` against the `Str` consts. The array is annotated
  `Array[Str, 3]`. I added this case to that row.
- **B-2026-10-14-70:** check refuses `s.tree.pages.len()` with the legacy
  chained-receiver lint. `Tree` has `npages()` and `leaf_len()`
  accessors.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.
