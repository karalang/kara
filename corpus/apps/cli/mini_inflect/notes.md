# mini_inflect

English plurals, articles, ordinals and numbers in words as inflect
7.5.0 does them. inflect is what Python programs reach for when a
message has to read "1 file" and "3 files", "an hour" and "a unicorn",
or "two thousand and five". The port covers:
- plurals of nouns, verbs and adjectives, with inflect's word lists,
  its patterns for Latin and Greek endings and its rules for compounds
  and prepositional phrases such as "mother-in-law";
- `singular_noun`, with the gender used for pronouns and the count;
- classical modes, one at a time and all together;
- `compare` and the plural and singular tests behind it;
- the indefinite article, `a` or `an`, by spelling and by sound;
- user definitions of nouns, verbs, adjectives and articles, written
  as words or as patterns with groups;
- `no`, `present_participle` and `ordinal`;
- `number_to_words` with its grouping, decimal point, `andword`,
  `zero`, `one`, threshold and comma options, and `join`;
- `inflect()`, which reads calls such as `plural('cat', 2)` and
  `num(3, False)` inside a string and replaces them;
- seven programs:
  - a repository digest that counts each repository's open items;
  - a cheque writer;
  - a leaderboard with ordinal ranks and ties;
  - ORM table names from model class names, and back;
  - a stock report from a warehouse export;
  - notification templates written in inflect's call syntax;
  - a deploy tool's dry-run plan in present participles.

Patterns are matched with the regular expression engine of `mini_re`.

The run writes `data/` and then works through the 17 jobs in
`data/jobs.jsonl`, 2152 operations in all:
- 296 noun plurals, 99 `plural_noun` calls, 191 verb plurals and 56
  adjective plurals;
- 424 singular nouns and 65 more with a gender and a count;
- 435 calls under classical modes;
- 84 comparisons and 119 indefinite articles;
- 40 user definitions with the calls that use them;
- 33 `no`, 51 present participles and 54 ordinals;
- 149 numbers in words, 18 joins and 31 `inflect()` strings;
- the seven programs over the files in `data/`.

`oracle.py` runs the same jobs through inflect itself and wrote
`expected.out`. It uses the Python files of the 7.5.0 wheel in
`inflect/`, unchanged. inflect imports two packages that the oracle
stands in for: typeguard, whose `@typechecked` only checks argument
types, and more_itertools, for `windowed_complete`.

The quirks are kept. Some examples:
- In a user definition written as a pattern, every `$1`, `$2` and so on
  in the replacement becomes the first group.
- `_plequal` sets classical mode for its comparisons and does not put
  it back when it returns early.
- A compound with a preposition is joined back with spaces whatever
  separator it had, so `jack-in-the-box` becomes `jacks-in-the box`.
- `ordinal(-1)` raises a TypeError.
- Numbers too large for inflect's names raise its NumOutOfRangeError,
  or an IndexError for some sizes.
- `decimal=""` also turns the commas off.
- The TypeError for a bad piece in `inflect()` counts only the
  non-empty pieces in its "sequence item N".

Not ported:
- inflect's type checks on arguments, which the jobs never trip.

## License

inflect is under the MIT license in `LICENSE-inflect.txt`. The license
file has no copyright line; the module says:

    Copyright (C) 2010 Paul Dyson

That covers the files in `inflect/`.
