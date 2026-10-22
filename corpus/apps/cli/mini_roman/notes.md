# mini_roman

A port of [roman](https://github.com/zopefoundation/roman) 5.2 (ZPL-2.1, Mark
Pilgrim) — the small, widely-used library that converts between integers and
Roman numerals (docutils and Sphinx use it for numbered lists and section
labels).

## What it does

`mk.py` writes `data/jobs.tsv`, one conversion per line. `oracle.py` runs each
through the vendored library (`vendor/roman/`) to produce `expected.out`.
`mini_roman.kara` reimplements `toRoman` and `fromRoman` and must match that
output byte-for-byte on every backend.

The port follows the library exactly: the subtractive-notation digit map,
`toRoman`'s greedy build and its `N`-for-zero special case, `fromRoman`'s
case-insensitive input and greedy decode, the `special_case` flag, and the
three error classes with their exact messages — `OutOfRangeError`
(`number out of range (must be 0..4999)`), `NotIntegerError`
(`decimals cannot be converted`) and `InvalidRomanNumeralError`
(`Input cannot be blank` / `Invalid Roman numeral: <uppercased input>`).

`fromRoman`'s validity test is the library's VERBOSE regex
`^M{0,4}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3})$`. The alternatives in
each subtractive group share no common prefix, so the match is deterministic;
the port recognises it with a structural matcher (`group` / `valid_roman`)
rather than a regex engine, which is exact for this grammar.

## Running

```bash
karac run mini_roman.kara          # interpret
karac build --mir mini_roman.kara  # native binary
python3 -s -P -B mk.py             # regenerate data/jobs.tsv
python3 -I oracle.py                # regenerate expected.out
```
