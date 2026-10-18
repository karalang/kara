# mini_struct

Python 3.13's `struct` module, ported from `Modules/_struct.c`, and run
over a session of jobs the way Python's prompt runs them.

The run writes five binary files into `data/` and runs `data/jobs.txt`,
275 jobs with comment lines between them. Each job is one Python
expression, or `name = expression`. For each job the program prints
`>>> job`, then the value's repr, or `Kind: message` when the job raises
(`error` is `struct.error`).

The files:
- `tone.wav`: a 16-bit stereo WAV with 24 frames, written by Python's
  `wave` module;
- `notes.txt.gz`: a gzip member with a file name and a fixed mtime;
- `pair.zip`: a zip with two stored members and fixed timestamps;
- `sensors.bin`: five `<IhhfB3x` sensor records;
- `ifaces.bin`: a big-endian header followed by length-prefixed names.

The jobs cover:
- `calcsize` in each byte order, native sizes and alignment (including
  `0q` to align without adding), whitespace between codes, and the format
  errors: a bad character, `n`/`N`/`P` outside native mode, a count with
  no code, an embedded null, a count too large, and a format that is not
  a str or bytes;
- `pack` of every integer code at both ends of its range and one past
  them in native and standard sizes, `?` on any object, `e`/`f`/`d`
  floats with rounding, subnormals, infinities and overflow, `c`, `s`
  and `p` with padding and truncation, and argument count and type
  errors;
- `unpack`, `unpack_from` with positive and negative offsets,
  `iter_unpack`, and their buffer-size errors;
- `pack_into` on bytearrays, with its offset and size errors, and a
  bytes object where a writable buffer is needed;
- `Struct` objects: `size`, `format` and their methods;
- the binary files, read through three helpers the session defines:
  `unpack_file(fmt, name, offset)`, `records(fmt, name)` (every record,
  with `iter_unpack`) and `names(name)` (walks `ifaces.bin`'s names), as
  well as a missing file.

**The port** follows `_struct.c`: `prepare_s` for the format (the byte
order prefix, repeat counts with their overflow check, native alignment,
and one item per repeated code but one for an `s` or `p` field), the
native table of x86-64 Linux and the standard table, the integer packers
with 3.13's range messages, `PyFloat_Pack2` and `PyFloat_Unpack2` for
halves (rounding half to even), the `f` overflow check, and the offset
rules of `unpack_from` and `pack_into`. `pack_into` zeroes its target
range first and writes items in order, as `s_pack_internal` does. The
session's expressions are read by a small lexer and parser: literals
(ints, floats, str, bytes with Python's escapes, lists, `True`, `False`,
`None`), calls, names and attribute access.

Not supported:
- integers beyond the range of `i128`;
- the `e` format's handling of a NaN's payload (no job packs a NaN);
- any Python beyond the expressions above.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py`. Run `mk.py` first; it writes `data/`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_struct.orig.kara to mini_struct.kara

Author errors:
- The methods declared their receivers as `self: ref Self`. Kāra writes
  `ref self` and `mut ref self`.
- `struct_error`, `type_error`, `parse_fmt` and `Session.assign` stored
  their parameters, which a bare parameter cannot give away. They take
  `own`.
- `Val` and `Tok` were cloned without `#[derive(Clone)]`.
- `list(x)` matched on `args[0]` and moved its payload out of the
  vector; it matches on a clone. A `Struct` method moved the format out
  of a borrowed `Val`; it clones it.
- Four calls passed a local `Vec[u8]` to a `mut ref` parameter without
  the `mut` marker. `karac fix` added them, and made `float(x)` match on
  a clone of its argument as well, for the same reason as `list(x)`.

Compiler gaps:
- B-2026-10-18-11: `i128.parse` is not lowered on the compiled MIR
  surfaces, so the lexer sums an integer's digits itself.

After those edits the program matched expected.out on the first run on
both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 83fab1e1e8.
