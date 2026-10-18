# mini_fractions

Python 3.13's `fractions.Fraction` ported from `fractions.py`, and three
programs that use it: a recipe scaler that keeps exact quantities, a
bill splitter that shares the tip to the cent, and a unit converter.

The run writes a 7-line recipe and a 5-item restaurant bill into
`data/`, then runs `data/jobs.tsv`, 82 jobs with tab-separated fields.
Each job prints `== ` and its fields joined by ` | `, then its output.

The jobs cover:
- `new`: `Fraction` from strings in each form `_RATIONAL_FORMAT`
  accepts (`3/4`, `3.1415`, `-47e-2`, `1.5E3`, `  7/14  `, `1_000/3`,
  `.5`, `2.`, `+0.125`), and the ones it refuses (`10/-8`, `abc`, `1/`,
  `1 / 2`, `/2`, `1e`), with Python's `ValueError` and
  `ZeroDivisionError` messages;
- `pair`: `Fraction(n, d)`, normalised to lowest terms with a positive
  denominator, and `Fraction(3, 0)`;
- `op`: `+ - * / // % divmod **` (negative and zero exponents, and
  `0 ** -1`), and the comparisons;
- `limit`: `limit_denominator`, including pi's convergents, bounds of
  1, and the `ValueError` for a bound of 0;
- `round`: `round` half to even, with no digits, with 2, and with -2;
- `ints`: `floor`, `ceil`, `trunc`, `int()`, `is_integer` and
  `as_integer_ratio`;
- `format`: `__format__` in its general form (fill, alignment, sign,
  `#`, width and `,`/`_` grouping) and the `f` type with a precision,
  plus a bad specifier;
- `recipe`: the recipe scaled by 2, 1/2 and 2/3, printed as mixed
  numbers (`2 1/3 tsp baking powder`);
- `bill`: each person's exact share with an 18% tip, rounded to cents
  by the largest-remainder rule so the shares add up to the total;
- `convert`: lengths and volumes, exact and to four places.

**The port** follows `fractions.py`: every `Fraction` is kept in lowest
terms with a positive denominator, `+` and `-` use the `_add`/`_sub`
gcd shortcuts, `limit_denominator` is the same continued-fraction walk,
and rounding to `ndigits` rounds the scaled value half to even. Python's
`//` and `%` round toward minus infinity, so the port has `floordiv` and
`floormod`. Errors are a `FracError` enum, and the job runner turns
them into Python's messages.

Not supported: floats and `Decimal` as operands (`from_float`,
`from_decimal`), mixing with `complex`, and integers past 64 bits; the
jobs keep every value in range.

**Oracle.** `expected.out` is Python 3.13.16's output for the same jobs,
made by `oracle.py` with the real `fractions` module. Run `mk.py` first;
it writes `data/`. `gen_kara.py` prints the Kāra data section from the
same files.

## Edits from mini_fractions.orig.kara to mini_fractions.kara

Author errors:
- `fixed` took `pos_sign: String`, which borrows under the D5 rules, and
  returned it from one branch of `if neg { "-".to_string() } else
  { pos_sign }`. Check said the branches had types `String` and
  `ref String`; that branch now returns `pos_sign.clone()`.

Compiler gaps: none found.

After that edit the program matched expected.out on both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 79a6c2eb9a.
