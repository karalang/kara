# mini_pprint

Python 3.13's `pprint` ported from `Lib/pprint.py`, over values read by a
port of `ast.literal_eval`'s grammar. It is the debugging dump of a
service config, an npm lock file, an HTTP response, a matrix, a word
list and a long text, at every setting `PrettyPrinter` takes.

The run writes those six files of Python literals into `data/`, then
runs `data/jobs.tsv`, 201 jobs with tab-separated fields. Each job
prints `== ` and its fields joined by ` | `, then its output.

The jobs cover:
- `pformat` over each file with 15 option sets: the default, widths
  40, 20 and 1, `indent` 0 and 4, `depth` 1 to 3, `compact`,
  `sort_dicts=False`, `underscore_numbers` and a mix;
- `saferepr` of each file, and `isreadable`/`isrecursive`;
- `pformat` of 25 inline literals (empty containers, `set()`, the
  1-tuple, nested tuples, adjacent string literals, hex and `_`
  numbers, str and bytes escapes and quote choice) at widths 80 and 10,
  with `depth=1` and with `compact`;
- the three `ValueError`s of the constructor.

**The port** follows `pprint.py`: `_format` falls back to the type's
pretty printer only when the one-line `_safe_repr` does not fit, dicts
are sorted with `_safe_tuple`, long strings are split at line breaks and
then at the `\S*\s*` words, bytes are wrapped in 4-byte steps (with
`_wrap_bytes_repr`'s quirk that the allowance only applies when the
length is not a multiple of 4), and `compact` packs items with the same
running width. Values are a `Value` enum (`Int`, `Bool`, `NoneV`, `Str`,
`Bytes`, `List`, `Tuple`, `Dict`, `Set`). A set is kept sorted, which is
the order CPython's repr gives the small-int sets in the data (mk.py
checks this).

Not supported: floats, complex, `frozenset`, recursive structures (a
literal cannot be one), dataclasses, `collections` types, and integers
past 64 bits.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `pprint` and `ast.literal_eval`. Run
`mk.py` first; it writes `data/`. `gen_kara.py` prints the Kāra data
section from the same files.

## Edits from mini_pprint.orig.kara to mini_pprint.kara

Author errors: none.

Compiler gaps:
- B-2026-10-18-33: `char.try_from(x)` with `x` from `for x in b.iter()`
  over a `Vec[i64]` passes check and runs under `karac run`, but both
  MIR surfaces refuse it: "char.try_from expects an integer codepoint,
  got `ref i64`". `bytes_repr` writes `char.try_from(*x)` in two places
  until that is fixed.

After that edit the program matched expected.out on both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at a73d3c10ad.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).

## Back to the original, 2026-10-18

B-2026-10-18-33 is fixed, so `bytes_repr` writes `char.try_from(x)` again. It matches expected.out on all four MIR backends (karac at 8d7a5ebf82).
