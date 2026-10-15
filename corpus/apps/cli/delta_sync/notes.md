# delta_sync

An rsync-style sync of one directory onto another. `dst/` holds an older
copy of the tree in `src/`:
- `notes.txt` lacks a paragraph near the top and has one paragraph edited;
- `data.bin` has three changed bytes and is missing 100 bytes;
- `log.txt` is a prefix of today's log;
- `config.ini` is unchanged, `README` is new, and `stale.tmp` has since
  been removed from the source.

**The algorithm.** For each source file whose MD5 differs from the
destination's:
1. The destination file is signed: a rolling weak checksum (rsync's
   16+16-bit sums) and an MD5 for every 256-byte block.
2. The source is scanned with the rolling checksum. A weak hit is
   confirmed by MD5, so the run also counts false hits. A matched block
   becomes a COPY and everything else a literal run. The short last block
   can match the tail.
3. The delta is written to `deltas/<name>.delta`, read back, and applied
   to the old file. The result must match the MD5 carried in the delta,
   and is written to `.part` and renamed into place.

Files that are no longer in the source are deleted. A second round
finds every file up to date and sends nothing. Then four damaged deltas
are refused: a flipped literal byte (checksum mismatch), a truncated
file, a bad magic, and a copy past the end of the old file.

**Delta format.** `KDL1`, then a list of ops: `1` COPY u32 block, `2`
LIT u32 length plus the bytes, and `0` END followed by the result's
16-byte MD5. Integers are little-endian.

**Oracle.** `delta_sync.py` is a port, and `expected.out` is its output.
Each delta's MD5 is printed, and the two runs write byte-identical
`src/`, `dst/` and `deltas/` trees. `check_hashlib.py` checks the
mirror's hand-written pieces: its MD5 matches `hashlib` on 19 inputs,
and its rolling checksum matches a direct sum at all 17,734 windows.

## Edits from delta_sync.orig.kara to delta_sync.kara

Author errors (the diagnostics were right, except the last, which the
oracle caught):
- `flush(out, lit, st)` and `put32(out, ..)` needed `mut` on the local
  buffers they fill.
- `'2' as u8` is refused; the byte literal `b'2'` is the right spelling.
- I renamed `struct Stats` to `SyncStats` after the prelude-shadow
  warning.
- I edited paragraph 7 of the old notes where the mirror edits paragraph
  6. The printed statistics happened to agree, but the delta files
  differed. The run now prints each delta's MD5, so the output checks
  them.

The app hit no compiler gaps.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.
