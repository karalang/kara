# mini_baseconv

A port of [python-baseconv](https://github.com/semente/python-baseconv) 1.2.2
(PSF license, Guilherme Gondim, after Simon Willison & Drew Perttula) — the tiny
base-N encoder used for URL shorteners and id obfuscation (base58/base62 for
Bitcoin-style short ids, base64 for compact tokens).

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line as `op \t base \t value`.
`oracle.py` runs each through `vendor/baseconv.py` and prints `expected.out`.
`mini_baseconv.kara` reimplements the converter and must match byte-for-byte on
every backend.

The jobs exercise base2/16/36/56/58/62/64 plus the two custom converters from
the library's docstring (`myconv` = `BaseConverter('MyOwnAlphabet0123456')` and
`altsign` = `BaseConverter('abcd-', sign='$')`): round-trips, a negative with the
default `-` sign and with a custom `$` sign, the zero case, the big-number
base64 example, and the four invalid-digit errors.

## How the port works

The library makes one Python big integer out of the input
(`x = x * len(from_digits) + from_digits.index(digit)`) and then peels digits off
it with `%` / `//` in the target base. Kāra has no arbitrary-precision integer,
and the library's own docstring converts 10^35, so the port **never materializes
the integer**. It holds the number as an array of digit values and runs
schoolbook long division by the target base — one pass per output digit, each
pass keeping the remainder as the next (least-significant) output digit — using
only `i64` arithmetic. The working value in a division step stays below
`64 * 64 + 64`, well inside `i64`, for every base here.

`encode` converts decimal → alphabet; `decode` converts alphabet → decimal. A
literal leading `-` (on encode) or the converter's own sign character (on
decode) is split off first and re-applied afterward, exactly as the library
does — which is why `base64`'s `$` sign and `altsign`'s `$` sign behave as they
do, and why an encode always strips a literal `-` even when the converter's sign
is something else. A digit outside the source alphabet reproduces the library's
`ValueError: invalid digit "X"`.

## Scope

Values are decimal (encode) or alphabet strings (decode); the converters and
their alphabets are the fixed set above. The library's `__repr__` and its
constructor-time errors (empty alphabet, sign-in-alphabet) are not part of the
conversion surface and are left out.

## Running

```bash
karac run mini_baseconv.kara             # interpret
karac build --mir mini_baseconv.kara     # native binary
python3 -s -P -B mk.py                    # regenerate data/jobs.tsv
python3 -I oracle.py                       # regenerate expected.out
```
