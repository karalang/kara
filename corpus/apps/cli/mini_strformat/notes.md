# mini_strformat

Python 3.13's `str.format` and `string.Template`, ported from CPython's C
and Python sources, and two programs that use them: an invoice laid out
by format strings read from a file, and a mail merge.

The run writes an items CSV, a layout file of format strings and two mail
templates into `data/`, then runs `data/jobs.tsv`, 116 jobs with
tab-separated fields. Each job prints `== ` and its fields joined by
` | `, then its output.

The jobs cover:
- `format` (102 jobs): a format string applied to three positional
  arguments (`'ab'`, `-42`, `'Zoë'`) and the keyword values of `env.py`
  (strings, ints, a `User` object, a dict, a list):
  - literal text, `{{`/`}}`, automatic and manual numbering and the
    error for mixing them;
  - field names with `.attr` and `[key]`/`[index]` lookups, and their
    `KeyError`, `IndexError`, `TypeError` and `AttributeError`;
  - the `!r`, `!s` and `!a` conversions (quote choice and escapes);
  - format specs with nested `{}` fields, and the recursion limit;
  - the str, int and int-through-float presentation types: fill,
    alignment, sign, `z`, `#`, the `0` flag, width, `,`/`_` grouping
    (by 4 for `b`/`o`/`x`), precision, `b c d o x X n e E f F g G %`;
  - every parse error message, in CPython's order of checks;
- `template` (10 jobs): `is_valid`, `get_identifiers`, `substitute`
  and `safe_substitute`, with `$$`, `$name`, `${name}`, a missing key,
  and invalid placeholders reported by line and column;
- `invoice` (2 jobs): five items formatted through `data/layout.tsv`
  with VAT at 19% and 0%, money as `{:,}.{:02}`;
- `mail` (2 jobs): a mail template filled from the job's fields and the
  invoice total, and a template with an invalid placeholder.

**The port** follows `Objects/stringlib/unicode_format.h`
(`MarkupIterator_next`, `parse_field`, `field_name_split`,
`FieldNameIterator`, `get_field_object`, `do_conversion`) and
`Python/formatter_unicode.c` (`parse_internal_render_format_spec`,
`format_string_internal`, `format_long_internal`,
`format_float_internal`, `calc_number_spec` and
`_PyUnicode_InsertThousandsGrouping` with its zero-fill minimum width).
`string.Template` follows `Lib/string.py`, with its regular expression
matched by hand. Values are a `Value` enum (`Int`, `Str`, `List`,
`Dict`, `Obj(User)`), and errors are a `PyError` with Python's
exception name and message.

The float presentation types are only applied to ints, as
`format(int, 'e')` does in Python. An int converts to a float exactly
in this range, so `float_text` works on the decimal digits of the int
and rounds half to even, as `PyOS_double_to_string` does for an exact
tie (`{125:.2g}` is `1.2e+02`, `{135:.2g}` is `1.4e+02`).

Not supported: float and complex values, `format_map`, `__format__` on
other user types, and attributes other than a `User`'s fields and an
int's `real`/`imag`/`numerator`/`denominator`. Integers stay within 64
bits.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `str.format` and `string.Template`.
Run `mk.py` first; it writes `data/`. `gen_kara.py` prints the Kāra
data section from the same files.

## Edits from mini_strformat.orig.kara to mini_strformat.kara

Author errors:
- A closure was written `|k: ref String| -> String { .. }`. Check said
  the return type is inferred, and `karac fix` removed `-> String`.
- `User` and `Value` had no `#[derive(Clone)]`, and the program clones
  values out of lists, dicts and the keyword map. Check said "no method
  'clone' on type 'Value'" with no hint that a derive was missing; that
  is B-2026-10-18-32.

Compiler gaps: B-2026-10-18-32 (the missing hint) only.

After those edits the program matched expected.out on both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at a73d3c10ad.
