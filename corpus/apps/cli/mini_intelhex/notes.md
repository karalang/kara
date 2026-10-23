# mini_intelhex

A port of [intelhex](https://pypi.org/project/intelhex/) 2.3.0 (BSD-3-Clause,
Alexander Belchenko): the Python library for Intel HEX firmware images, used
by many flashing and bootloader tools, together with its `hexinfo`,
`hexdiff`, `hex2bin`, `bin2hex` and `hexmerge` scripts. Two images merged
into one:

```
$ hexmerge.py part_a.hex part_c.hex
:0400000500000100F6
:100000004427F051C6F50766CE238B7DE6FEEB1E36
:10001000CC3B42710F5E63AAAC031E370D521AE54A
:100100009A2801FCFBD27F5CA961FD9CDD9182995C
:00000001FF
```

## What it does

`mk.py` builds the images with the library's `Record` helpers and writes
them to `data/files/`, then writes `data/jobs.tsv`. The first 34 jobs carry
each image's bytes as hex, so the Kara port has the same files in memory.
`oracle.py` replays the other 214 jobs through the vendored library and
scripts, and `mini_intelhex.kara` replays them through the port. Each job
prints a header naming it, then its results; an exception prints as
`<ClassName>: <message>`.

- **Reading** (31 `load` jobs): an AVR program, ARM firmware at 0x08000000
  behind extended linear address records with a start address, an 8086
  image with extended segment records and a CS:IP start, CRLF and lower-case
  files with blank lines and data after the end record, old Mac `\r` line
  ends, a file with no end record, an empty file, sparse EEPROM contents
  with a record running past 64K, and 17 broken files, one for each check
  the reader makes, in the order it makes them.
- **Writing** (`hex`, `bin`, `bin2hex`, `hex2bin`): `write_hex_file` with
  1 to 255 bytes a record, CRLF, with and without the start record, across
  holes and 64K pages; `tobinstr` with every mix of start, end, size and
  padding, including the ones it refuses.
- **Access and editing** (`get`, `gets`, `getsz`, `find`, `seg`, `todict`,
  `dump`, `edit`): single bytes, strings, zero-terminated strings, searches
  within a slice, segments with gaps, the hex dump with and without padding,
  and 22 sequences of edits (bytes, slices with steps, deletes, `puts`,
  `putsz`, `fromdict`, `loadbin`, the copy constructor, odd padding values),
  each followed by a summary, a dump and the image written out.
- **Merging** (`merge`, `hexmerge`): the three overlap modes, conflicting
  start addresses, merging an object with itself, and 30 `hexmerge` command
  lines: ranges on the command line and per file, `--no-start-addr`,
  abbreviated long options, and every usage error.
- **The rest**: 16-bit word access as for Microchip images, the `Record`
  builders, `hexinfo`'s YAML summary and `hexdiff`'s unified diff.

## How the port works

- **The image is a map from address to (byte, insertion number).** The
  Python object keeps its bytes in a dict, and a dict's insertion order
  shows in `todict()` and in which overlapping address `merge` reports, so
  each byte keeps the number of its first insertion; sorted addresses are
  computed where the library sorts.
- **The writer is ported line for line**: its chain length that is off by
  one, the `bisect_right` over the sorted addresses, catching a hole to
  shorten a record, and the extended linear address record before every 64K
  page once any address is past 0xFFFF.
- **The library's quirks come along**: a slice's start of 0 means the lowest
  address, a slice drops the start address and the padding, `dump` raises
  `NameError` when the start address has unexpected keys (it formats a name
  that was never defined), values above 255 set by hand are dumped as wide
  hex but make the writer fail as `array('B')` does, `hex2bin` with an end
  and a size counts back from the end, and `IntelHex16bit` turns a padding of
  0xFF into 0xFFFF.
- **`hexmerge`'s command line is ported with getopt's GNU scanning**: options
  after file names, abbreviated long options, `--` and the exact messages,
  and `int(x, 16)` with its whitespace, sign, `0x` and underscores.
- **`hexdiff` uses the `SequenceMatcher` from mini_difflib** for difflib's
  `unified_diff`.
- **`gen_kara.py` writes two functions** at the end of the `.kara` file:
  `hexmerge.py`'s usage text, read with Python's `ast` module, and the jobs.

## Compiler gaps

None were hit.

## Scope

- **Files live in memory.** The images come in through the jobs, output to a
  file (`hexmerge -o`, `tofile`) is not exercised, and reading from standard
  input (`-`) is left out.
- **Left out:** `get_memory_size`, the deprecated `pad` arguments, the
  `hex2dump`, `hexinfo` and `hexdiff` command lines (their library calls are
  covered), and Windows drive letters in `hexmerge`'s file notation.

The vendored library is unchanged.

## Running

```bash
karac run mini_intelhex.kara                 # interpret
karac build --mir mini_intelhex.kara         # native binary
python3 -I -B mk.py                          # regenerate data/ (imports the vendored Record)
python3 -I -B oracle.py > expected.out       # regenerate expected.out
python3 -I -B gen_kara.py                    # regenerate the generated functions
```
