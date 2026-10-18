# toml_check

A TOML 1.0 parser and validator. It writes three real config files to
`conf/` (a Cargo manifest, a pyproject and the TOML tour) plus 27
malformed documents, then reads the directory back in sorted order. For
each good file it prints every leaf with its path and type. For each bad
one it prints the line and the reason it was rejected.

**The parser.**
- The document is an arena. Tables are `(keys, values)` in insertion order
  and arrays are lists, both addressed by index; a value is a `TVal`.
- Strings: basic, literal, and both multi-line forms, including the
  line-ending backslash and the rule that allows up to two extra quotes
  before the closing delimiter. Escapes include `\uXXXX` and `\UXXXXXXXX`,
  and surrogates are refused.
- Numbers: decimal, hex, octal and binary, with the underscore and
  leading-zero rules. Integers are range-checked to i64. `inf` and `nan`
  are accepted.
- Dates and times: all four kinds. Month lengths and leap years are
  checked, and the value is normalised the way Python's `isoformat`
  prints it (`T` separator, `Z` as `+00:00`, the fraction cut to six
  digits).
- Redefinition rules follow tomllib. A header or dotted key marks its
  path explicit, and an inline table or array freezes its subtree. The
  tables a dotted key creates become explicit at the next header, and an
  array-of-tables header clears the flags under its path.

**Oracle.** `toml_check.py` is a port, and `expected.out` is its output.
`check_tomllib.py` checks that port against Python's own `tomllib`:
- tomllib's parse of the three good documents, rendered with the port's
  renderer, gives the same leaves;
- tomllib rejects 26 of the 27 bad documents. The exception is the hex
  overflow, because tomllib reads integers of any size, where TOML 1.0
  requires an error outside i64.

## Edits from toml_check.orig.kara to toml_check.kara

There were no author errors. The original runs correctly on every MIR
surface; only `karac check` refuses it.

The `Flags.defer` workaround for B-2026-10-14-70 was dropped after the fix
in 0bacd9912b; the parser calls `p.flags.pending.push(..)` again.

Compiler gaps, worked around and listed in the source header:
- **B-2026-10-15-20:** check says a binding declared inside the loop body
  "is moved inside a loop and not assigned again", when it is read in an
  early-return branch and then moved into a method's `own` parameter. The
  same body calling a free function passes. `cont` is cloned at the move,
  and the duplicate-key message reads the key from the path again. I
  added this case to that row.

Written 2026-10-15 by the real-world apps thread; karac at fee58fc7e2.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).

## Back to the original, 2026-10-18

The rows behind the edits above are fixed, so `toml_check.kara` is now exactly what `karac fix` makes of `toml_check.orig.kara`. It matches expected.out on all four MIR backends (karac at 4ea06e27b9).
