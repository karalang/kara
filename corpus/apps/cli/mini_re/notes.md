# mini_re

Python 3.13's `re` module: the pattern parser ported from
`Lib/re/_parser.py` with its error messages and positions, compiled to
a program for a backtracking matcher that follows `Modules/_sre/sre_lib.h`,
and the module functions on top. Every log parser, config reader, router
and tokenizer in Python leans on it.

The run writes five files into `data/` and then runs `data/cases.tsv`,
213 cases with tab-separated fields `op pattern flags subject [extra]`.
Each prints `== ` and its fields joined by ` | `, then:
- `search`, `match` and `fullmatch` print the span and text, the groups,
  their spans and the named groups;
- `findall` prints the list, `finditer` each match's span and groups;
- `sub` prints `subn`'s result for a replacement template and a count;
- `split` prints the list for a `maxsplit`;
- `tokens` runs the tokenizer recipe from the `re` documentation,
  printing each token's line, `lastgroup` and `lastindex`;
- `compile` prints the group count and `groupindex`, or the error.

The flags field is `-` or letters from `imsxa`. In the subject, `\n`,
`\t` and `\\` stand for a newline, a tab and a backslash, and `@name`
means the file `data/name`.

The cases cover literals, sets (ranges, `]` and `-` placement, escapes,
categories), anchors in both line modes, `\b` and `\B`, greedy, lazy and
possessive repeats, `{m,n}` and the brace forms that are literals, atomic
groups, capturing, named and non-capturing groups, groups inside repeats
(including the empty-iteration cases where sre's zero-width protection
decides the result), backreferences by number and name, conditionals,
lookahead and lookbehind with captures, scoped and global inline flags,
verbose patterns, alternation priority, the empty-match rules of
`findall`, `sub` and `split`, every replacement-template form and error,
and 33 pattern errors. The real-world cases parse a combined access log,
a Java-style application log and an INI file with named groups, redact
emails and card numbers, pull out host:port pairs and URLs, match quoted
strings with escapes, and tokenize a small language.

**The port.** The parser keeps `_parser.py`'s structure: the tokenizer
with its one-token lookahead, `_parse_sub`, `_parse`, `_parse_flags`,
`_escape` and `_class_escape`, group widths for the lookbehind check, and
the deferred check of conditional group numbers. The compiler emits
marks, splits for alternation, a register per repeat, and sub-programs
for lookaround and atomic groups. The matcher keeps a trail of every
mark and register it changes, so backtracking restores captures exactly
as sre's mark stack does, and each repeat's boundary op is sre's
`MAX_UNTIL`/`MIN_UNTIL`, including the rule that an iteration matching
nothing ends the loop and keeps its captures. Searches carry sre's
`must_advance`, so an empty match is never reported twice at one place.

Text is handled as code points, so positions count characters. `\d` uses
Unicode's decimal digits and `\s` Python's whitespace, both from tables;
`\w` is `char.is_alphanumeric()` or `_`, which agrees with Python's
`isalnum` on letters, digits and CJK but not on every combining mark.
Case folding is the simple lowercase mapping, without sre's extra folds
for characters such as the Kelvin sign. Not supported: `\N{...}`, bytes
patterns, the LOCALE flag, and the line and column sre adds to an error
in a pattern with a newline.

**Oracle.** `expected.out` is Python 3.13.16's output for the same
cases, made by `oracle.py` with the real `re`. Run `mk.py` first; it
writes `data/`. `gen_kara.py` prints the Kāra data section (everything
after the last comment line ending in `data` in `mini_re.kara`).
