# cli/archive

A tiny archiver with a binary format (magic, little-endian u16/u32 lengths,
CRC-32 per member): pack five members (text, an empty file, a UTF-8 name, a
300-byte binary blob) into one file, read it back with seek and read, list it
with width and hex format specs, extract every member to its own file, and
detect a flipped byte, a truncated archive, trailing bytes and a wrong magic.
The mirror `archive.py` uses `struct` and `zlib.crc32` and wrote
`expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at c8dad803.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right):
- `{crc32(table, \"123456789\".bytes()):08x}` escaped the quotes of a string
  literal inside an f-string interpolation; the parser's message says to use
  plain quotes there.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-44: `File.open` / `File.create` take `ref String`, and
  `File.read` / `File.write` return `usize`.
- B-2026-10-14-50: `c.take(size)?.iter()` is parsed as optional chaining
  (`'?.' requires an Option on the left`), where design.md says there is no
  `?.` and `f()?.x` means `(f()?).x`; the `?` is parenthesized.
- B-2026-10-14-51: `karac check` refuses `let s = self.bytes[a..b]; ... Ok(s)`
  in a method ("slice from temporary value escapes the enclosing statement"),
  which every MIR surface accepts; `Cursor.take` returns the view without
  binding it.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-44 and
B-2026-10-14-50 are fixed, so the paths are `Str` again, the read and
write counts are no longer cast to `i64`, and `c.take(size)?.iter()` has
no parentheses. Each of these is the original line again, and the app
still matches on all four MIR surfaces.
