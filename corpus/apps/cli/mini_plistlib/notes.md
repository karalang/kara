# mini_plistlib

Python 3.13's `plistlib` ported from `Lib/plistlib.py`: Apple's property
lists in both formats, the XML one and the binary `bplist00` one. It is
what reads and writes an app's `Info.plist`, a launchd job, the binary
preferences macOS keeps in `~/Library/Preferences`, and NSKeyedArchiver
files.

The run writes 36 files into `data/`, then runs `data/jobs.tsv`, 98 jobs
with tab-separated fields. Each job prints `== ` and its fields joined
by ` | `, then its output:
- `load F` prints the format and the `repr` of what `loads` returns;
- `xml F [sort_keys=0] [skipkeys=1]` prints `dumps(value)` line by line;
- `bin F [...]` prints the length and a hex dump of
  `dumps(value, fmt=FMT_BINARY)`;
- `roundtrip F` writes the value in each format, reads it back and
  compares, and checks the XML of the binary copy against the first XML;
- `get F K...` walks keys and list indexes, as PlistBuddy's `Print` does.

The files are an app's `Info.plist` (URL types, document types, ATS
exceptions, `©` and `&`), a launchd job with a comment, an editor's
preferences written by plistlib in binary with `sort_keys=False` (dates
with and without microseconds, a 2^63 + 5 and a -2^63, `-0.0` beside
`0.0`, a repeated string, an emoji path) and the same in XML with
four-space indents, an `Info.plist` converted to binary, an
NSKeyedArchiver archive with UIDs, a file with a UTF-8 BOM, and an
edge-case file (entities and character references, CDATA, `\r\n` and
`\r` line ends, every integer and real spelling `int()` and `float()`
take, partial dates, base64 with spaces, duplicate keys, an element
plistlib ignores). mk.py also builds binary files plistlib's own writer
never makes: two keys naming one array, a dict with an int key, `None`,
a 4-byte real, a 16-byte integer, 2-byte references, a UTF-16 string
with a surrogate pair. The broken files cover each `InvalidFileException`
path, the parser's `ValueError`s with their line numbers, an entity
declaration, bad numbers and both base64 errors.

**The port** follows `plistlib.py`. The XML reader stands in for expat
only as far as plist files need: it normalizes line ends first, skips
the declaration, comments and the doctype (and refuses an entity
declaration inside it), and feeds start tags, end tags and character
data to handlers that are `_PlistParser`'s, including its quirk that
every start tag clears the collected text. The binary reader and writer
follow `_BinaryPlistParser` and `_BinaryPlistWriter`: the trailer, the
offset table, sizes past 14 items, the int widths, UTF-16 for any
non-ASCII string, and the writer's object table, which writes equal
scalars once (so `-0.0` comes out as `0.0`) but every container once per
object. Containers live in a heap and are named by index, so a binary
file's shared array stays one object through `load` and `dumps`, as it
does in Python. `repr` follows Python's for str, bytes, float, datetime
and UID; `==` is Python's, so `1 == 1.0 == True`. Dates read from binary
files go through `timedelta`'s rounding to the microsecond.

Not supported: `aware_datetime`, `dict_type`, expat errors for XML that
is not well formed, and recursive structures.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `plistlib`. Run `mk.py` first; it
writes `data/`. `gen_kara.py` prints the Kāra data section from the same
files, text files as strings and the rest as hex.

## Edits from mini_plistlib.orig.kara to mini_plistlib.kara

Author errors: `String.bytes()` gives a `Slice[u8]`, so the helpers that
take it (`parse_digits`, `two_digits`, `be_uint`, `codepoints`,
`starts_bytes`, `is_fmt_xml`, `loads` and the two loaders) take
`ref Slice[u8]` rather than `ref Vec[u8]`, and `parse_real` copies the
sign with `.to_vec()`.

Compiler gaps:
- B-2026-10-18-42: `out.push(if b { 0x09 } else { 0x08 })` on a
  `Vec[u8]` is refused by check ("implicit coercion from 'i64' to 'u8'
  would narrow"), though `let t: u8 = if b { 0x09 } else { 0x08 };`
  passes. The binary writer binds the byte first.

After those edits the program matched expected.out on the first run on
all four MIR backends.

Written 2026-10-18 by the real-world apps thread; karac at 4ea06e27b9.
