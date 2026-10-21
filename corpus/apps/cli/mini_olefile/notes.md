# mini_olefile

olefile 0.47, Philippe Lagadec's pure-Python reader of OLE2 compound
files, as the records office of a small law firm working through one
matter's intake folder. Compound files (Microsoft's Compound File
Binary format) are what Word 97-2003 `.doc`, Excel `.xls` and Outlook
`.msg` files are, and they are still everywhere in old archives and
mail. Each one is a small file system: a header, a FAT of sector
chains (with a DIFAT once the FAT outgrows the header), a MiniFAT and
mini stream for small streams, and a directory kept as a red-black
tree. Damaged copies, crashed saves and hostile senders break each of
these parts, and olefile's job is to say how.

The port follows olefile's model. `OleFileIO.open()` makes every
header check, loads the FAT and DIFAT, reads the directory along its
chain and walks its tree as `append_kids` does, with the same kids
lists when one entry is reached twice, and every defect goes through
`_raise_defect`: it raises at or above the `raise_defects` level and
lands in `parsing_issues` below it. Streams are read as `OleStream`
reads them, so a chain that loops, leaves the FAT, ends early or runs
past the end of the file gives the same bytes and the same defects.
`listdir`, `exists`, `get_type`, `get_size`, `getclsid`, `getmtime`,
`getctime` and `openstream` keep the case-insensitive lookup.
`getproperties` and `get_userdefined_properties` parse every property
type olefile parses, with its vectors, its misreadings of padded
strings and of `VT_LPWSTR`, and its errors (`struct.error`,
`AssertionError`, `IndexError`, `OverflowError`, and the `TypeError`
that `UnicodeDecodeError(message)` itself raises). `OleMetadata` reads
both summary streams.

**Inputs.** `data/` holds 29 compound files and `jobs.txt`. Six are
good: a board report with an embedded object, a budget workbook, an
invoice with a VBA macro, an Outlook message with two attachments, an
encrypted Word file and a file with 4096-byte sectors. The other 23
are those files damaged one way each: not OLE at all, cut short,
header oddities, chains that loop or leave the FAT, sizes that lie,
six kinds of directory damage, no directory, no MiniFAT, times past
year 9999, five DIFAT cases, and property streams broken in every way
the parser notices. `mk.py` writes them with its own small CFB writer.

**Jobs** (147 lines in `jobs.txt`):
- `open`, at each defect level: the header, FAT and directory, or the
  exception.
- `tree`, `list`: the directory tree and `listdir` with streams,
  storages or both.
- `entry`: `exists`, `get_type`, `get_size`, `getclsid`, `getmtime`
  and `getctime` of one path.
- `stream`, `extract`: a stream's bytes, checked by Adler-32 or
  written to `data/out/`.
- `props`, `userprops`, `meta`: the property sets.
- `report`: the intake report. It reads each file's metadata or, for
  a message, its subject, sender and attachments, flags macros,
  encryption, embedded objects and defects, retries a strict open at
  `DEFECT_INCORRECT`, and decides archive, review, quarantine or
  rejected. Then it writes `data/out/manifest.csv`.

Not ported: writing (`write_stream`, `write_sect`), file-like objects
and bytes input, `path_encoding`, logging, and the debug dumps.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of olefile in `olefile.py` (BSD licence, in
`LICENSE-olefile.txt`). `gen_kara.py` writes the data files into the
Kāra source.

## Edits from the first draft

Five compile fixes: `!` for bitwise not became `^` on the known bit,
an `IoError` is printed with `{e}`, an int parse was written out by
hand, a match on an `Option[String]` that moved it became a helper
taking a reference, and the directory entry struct was renamed so it
does not shadow the prelude's `Entry`. `karac fix` added the call-site
`mut` markers. After that, the output matched `expected.out` on
`__mir-run` and `__mir-llvm-run` on the first run, as did the six
files written to `data/out/`.

Written 2026-10-21 by the real-world apps thread; karac at 0c775342dc.
