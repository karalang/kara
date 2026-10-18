# mini_bc

The arbitrary-precision calculator that shell scripts call when the
shell's own integers are not enough: money with exact cents, big
integers, constants to fifty digits, base conversion. It answers what GNU
bc 1.07.1 answers without `-l`, so the program is a whole bc: a lexer, a
parser with bc's error recovery, an interpreter, and decimal arithmetic
on numbers of any size.

The run writes eight bc scripts into `data/` and a `jobs.txt` naming
them. It feeds each one to a fresh calculator, as `bc < script` does, and
prints the `$ bc < name` line, the standard output and the standard
error. The scripts are:
- **loan.bc**: a mortgage payment with a rounding function, three months
  of an amortization table, and `^` with a negative exponent;
- **bigint.bc**: factorials, Fibonacci numbers, gcd, lcm, modular powers,
  digit sums, `length`, `2^256`, and a prime sieve with `break`;
- **series.bc**: pi by Machin's formula and e by its series, at scale
  60, and `sqrt` at scales 70, 20 and 3;
- **scale.bc**: how scale decides the digits of each result for `+ - * /
  % ^`, `length` and `scale`, comparisons and booleans, `++`/`--`, every
  `op=`, `last` and `.`;
- **bases.bc**: `obase` 16, 2, 8, 60, 100, 1000 and 7, with fractions,
  and `ibase` 16 and 2;
- **functions.bc**: dynamic scope, arrays by value and by reference
  (`*v[]`), `void` functions, Ackermann, auto arrays, `while`/`for`/`if`
  with `break` and `continue`;
- **report.bc**: an invoice made with `print` escapes and string
  statements, and output long enough to wrap;
- **errors.bc**: syntax errors, an illegal character, and a `quit` that
  bc obeys inside `if (0)`.

**Numbers** are decimal digits with a scale, and each operator gives its
result the digits bc's rules give it:
- `+` and `-` keep the larger scale;
- `*` keeps min(sa + sb, max(scale, sa, sb)) digits;
- `/` keeps exactly `scale` digits;
- `%` is a - (a / b) * b with the quotient at `scale` digits;
- `^` takes an integer exponent and keeps min(sa * e, max(scale, sa))
  digits, or works out 1 / x^-e at `scale` digits for a negative one;
- `sqrt` keeps max(scale, sa) digits.

Digits that do not fit are cut off, not rounded. `Num` has `Add`, `Sub`
and `Neg` impls; the operators that need `scale` are functions.

**Output** follows bc's rules too:
- every line wraps at 70 columns with a backslash, strings included;
- zero is `0` whatever its scale, and `.5` has no leading zero;
- an `obase` above 16 prints each digit as a zero-padded decimal number
  after a space;
- fraction digits in another base come from repeated multiplication,
  while the base's power has no more digits than the scale.

**Constants** are read in the current `ibase` when they run, not when
they are read. A one-digit constant is its own value in any base, so
`ibase = A` always sets base ten.

**The grammar** is bc's:
- assignment takes the name on its left, so `a = 3 < 4` is
  `(a = 3) < 4` and `x + y = 5` is `x + (y = 5)`;
- unary minus binds tighter than `^`, so `-2 ^ 2` is 4;
- `!` takes everything up to a `&&` or `||`;
- an expression statement prints its value unless it is an
  assignment or a `void` call.

**Errors** are bc's too. A syntax error prints `(standard_in) N: syntax
error` and the whole line is dropped. N is the line the lexer is on, so
an error found at a newline names the next line. An illegal character
is reported when the lexer reaches it. `quit` exits as soon as it is
read.

Not supported:
- the `-l` math library, `read()`, `limits`, `warranty` and the
  command line;
- runtime errors and warnings (divide by zero, a fractional exponent,
  `ibase` or `obase` out of range), and the line numbers bc gives an
  error inside a function definition that spans lines;
- POSIX mode and the `BC_*` environment variables.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs GNU
bc 1.07.1 on each script with the `BC_*` variables cleared. Run it from
this directory after `mk.py` writes `data/`.

## Edits from mini_bc.orig.kara to mini_bc.kara

- `run_script` passes its fresh `State` to `exec_list` as `mut st`. This
  was an author error, and `karac fix` applied it.

There were no compiler gaps. The program matched expected.out on the
first run on all three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
