# mini_braceexpand

A port of [braceexpand](https://github.com/trendels/braceexpand) 0.1.7
(MIT, Stanis Trendelenburg) — Bash-style brace expansion
(`item{1..3}` → `item1 item2 item3`), the string-generation step that
snakemake, glob helpers and shell-like tools reuse.

## What it does

`mk.py` writes `data/jobs.tsv`, one brace pattern per line. `oracle.py` runs
every pattern through `vendor/braceexpand.py` and prints `expected.out`
(a header `== <pattern>  (<count>)` and one indented line per expansion, or
`!ERROR <message>` for an unbalanced pattern). `mini_braceexpand.kara`
reimplements the algorithm and must match that output byte-for-byte on every
backend.

Covered:

- **Integer ranges** `{1..3}`, with zero-padding inferred from the operands
  (`{07..10}`), an optional step (`{10..4..3}`), both directions
  (`{4..1}`), and negatives (`{2..-1}`).
- **Character ranges** `{a..c}`, with a step (`{a..g..2}`) and the
  library's documented mixed-case behaviour (`{Z..a}` skips `[]^_\``).
- **Comma sequences** `{x,y,z}`, including an empty element
  (`index.html{,.backup}`).
- **Nesting** `python{2.{5..7},3.{2,3}}` and the cartesian product of
  several groups (`a{b,c}d{e,f}`).
- **Literal-brace fallback**: `{}` and `{a}` are not ranges or sequences, so
  they pass through unchanged, exactly as the library's recursive
  `None`-handling does.
- **Backslash escapes** (`{1\{2,3}`) — an escaped brace is not special, and
  the escape is stripped from the output after expansion.
- **The unbalanced-braces error** (`{1{2,3}`), raised with the library's
  exact message.

The reference uses `itertools.product` / `chain` lazily; this port computes
the same lists eagerly — `parse_pattern` returns a `Vec[String]` that is the
cartesian product of the pattern's parts — and the three parsers recurse
through each other just as the library's do. `escape=True` (the library
default) throughout; `escape=False` is the one documented mode left out.

## Running

```bash
karac run mini_braceexpand.kara            # interpret
karac build --mir mini_braceexpand.kara    # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I oracle.py                         # regenerate expected.out
```
