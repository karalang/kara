# mini_decimal

Python 3.13's `decimal` module, ported from `_pydecimal.py`, the
pure-Python implementation that the C one is tested against. Money code
uses it because `0.1 + 0.2` has to be `0.3`, a VAT line has to round half
up to the cent, and a rounding has to be the same on every machine.

A `Decimal` is a sign, a string of coefficient digits and an exponent,
or one of the specials: `Infinity`, `NaN` and the signaling `sNaN`.
Every operation rounds its result to a `Context`. The context holds the
precision, one of the eight rounding modes, the exponent limits `Emin`
and `Emax`, `capitals` and `clamp`. It also holds a flag and a trap for
each of the nine signals. A condition such as `Inexact` or
`DivisionByZero` sets its flag, and raises an exception when the context
traps it.

The port follows `_pydecimal.py`:
- construction from strings, with the module's grammar: Unicode digits,
  underscores and the `Inf`/`NaN`/`sNaN` forms. It also builds from ints,
  and from floats, exactly, with `FloatOperation` flagged.
  `create_decimal` is the strict form;
- `_fix`, the rounding to the context, with overflow, underflow,
  subnormals and clamping, and the signals in the order the spec gives;
- `+ - * / // %`, `divmod`, `remainder_near`, `fma`, and `pow` with and
  without a modulus. The exact-power search `_power_exact` is here, and
  so is `_dpower` for the inexact case;
- `sqrt`, `exp`, `ln`, `log10` and `logb`, on the integer series
  `_ilog`, `_iexp`, `_dlog`, `_dexp` and the digits of log(10). The
  digits are computed again when more than the 47 kept are wanted;
- `quantize`, `to_integral_value`, `to_integral_exact`, `normalize`,
  `round`, `floor`, `ceil` and `int`;
- the comparisons: the quiet `==` and the signaling `<`, `compare`,
  `compare_signal`, `compare_total` (the total ordering, NaNs and
  exponents included), and the `max`/`min` family;
- `next_plus`, `next_minus`, `next_toward` and `number_class`;
- `scaleb`, `rotate`, `shift`, and the logical and/or/xor/invert;
- `str` and `to_eng_string`, `as_tuple`, `as_integer_ratio`, and
  `__format__` with fill, alignment, sign, `z`, `#`, zero padding,
  grouping and the `e f g %` types;
- the contexts: `DefaultContext`, `BasicContext`, `ExtendedContext` and
  their repr. Settings are checked the way `Context.__setattr__` checks
  them, error messages included.

`mk.py` writes `data/`: the input files of the programs, and `jobs.tsv`
with 23 jobs. Each job starts from the default context. A `ctx` line sets
a new context. Every other line is one operation, which prints its
result and the flags it set. The last seven jobs are programs:
- invoices with VAT per line and per rate;
- currency conversion with the places each currency keeps;
- loan amortization schedules (the payment uses `(1 + r) ** -n`);
- a bill split that hands out the leftover cents;
- progressive income tax with the effective and marginal rates;
- savings growth, yearly and continuous, with doubling times;
- statistics of a price series: mean, median, variance, standard
  deviation and log returns.

`oracle.py` runs the same jobs through `_pydecimal` and wrote
`expected.out`. `gen_kara.py` writes the data section.

The quirks are kept. Some examples:
- `Decimal('١٢٣')` is 123, because the grammar's `\d` takes any Unicode
  digit;
- a condition raised for its flag alone has no explanation, so a trapped
  `Inexact` reads `Inexact: None`, and so do `rotate`, `shift` and the
  logical operations on a bad operand;
- `5 / Infinity` is `0E-1000026`, the smallest exponent the default
  context has, with `Clamped`;
- under `ROUND_FLOOR`, `5 + -5` is `-0`;
- `Decimal('1.00') ** -3` is `1`, but `Decimal('1.0') ** 5` is `1.00000`;
- `capitals` must be in `[0, 1]. got 2`, with no colon, while
  `prec must be in [1, inf]. got: 0` has one.

It matched on every backend with no workaround in the port. Two typecheck
gaps changed how it is written: floats have no `is_nan` or
`is_infinite`, so `from_float` reads the bits, and `String.contains`
takes no `char`, so the digit tests use a one-letter string.
