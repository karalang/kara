# mini_flashtext

A port of [flashtext](https://pypi.org/project/flashtext/) 2.7 (MIT, Vikash
Singh): the **trie-based keyword extractor and replacer** that, for a large set
of keywords, is far faster than a regex alternation (the algorithm is loosely
Aho–Corasick, and the library was written to replace a slow regex over millions
of documents). It is a staple of NLP preprocessing — tagging documents, swapping
synonyms or clean names, redaction — and the same longest-match-with-word-
boundaries scan underlies entity dictionaries and bulk find/replace.

## What it does

`mk.py` writes `data/jobs.tsv`: a command stream, one command per line, that
drives a single `KeywordProcessor`. `oracle.py` replays it through the library;
`mini_flashtext.kara` replays it through the port. Each command prints one line,
`<command> => <result>`:

```
case <0|1>          start a fresh processor (1 = case-sensitive)
nwb <chars>         set the non-word-boundary set to exactly these chars
add <kw>[|<clean>]  add a keyword (clean name defaults to the keyword) -> status
addlist <a,b,c>     add several keywords (clean name = keyword)
rm <kw>             remove a keyword                 -> True/False
has <word>          membership test                  -> True/False
get <word>          clean name for an exact term     -> name or None
len                 number of distinct terms         -> count
extract <text>      keywords found, in text order (joined by " | ")
span <text>         the same, with clean:start:end spans
replace <text>      text with keywords swapped for their clean names
```

For example, with `Big Apple` → `New York` and `Bay Area` added,
`extract I love Big Apple and Bay Area.` gives `New York | Bay Area`, and
`replace` rewrites the sentence with those clean names.

`mini_flashtext.kara` reproduces the stream byte-for-byte on every backend.

## How the port works

The library stores keywords in a trie of nested dictionaries; the port
reproduces that trie as an **arena of parallel `Vec`s** — per node: the child
byte keys, the child node ids, an is-keyword flag and the clean name. The two
scanning routines, `extract_keywords` and `replace_keywords`, are transcribed
statement for statement, including the parts that make the result exact:

- **Longest match.** From a word start the scan follows the trie as far as it
  can and records the longest keyword that ends on a word boundary, so `Big`
  and `Big Apple` both being keywords yields `Big Apple` on the longer run.
- **Word boundaries.** A match only counts when the characters on either side
  are outside the *non-word-boundary* set (letters, digits, `_` by default), so
  `end` is found in `the end` but not inside `endless`. The set is
  configurable (`nwb`), which is how `c++` becomes matchable.
- **Case folding.** Case-insensitive mode lowercases for lookup but emits the
  original text around replacements.
- The control flow uses Python's `for/while`-`else` and `break`; those are
  rewritten with explicit flags, the one construct Kāra lacks, with the
  behaviour preserved exactly (verified below).

Every trie access is a **lookup by key**, never an iteration over a node's
children, so the output does not depend on map order — the one precondition for
`run == build`. (`get_all_keywords`, the one library method that *does* walk the
trie in dict order, is not part of this port.) The whole thing is byte- and
integer-level, so the four backends agree by construction.

## Scope

- Input is **ASCII**: the trie keys, the word-boundary set and case folding are
  all byte-level, which is what every command here needs. The library accepts
  arbitrary Unicode and folds with `str.lower()`; the non-ASCII case is the
  documented boundary.
- The commands exercise `add`/`remove`/`contains`/`get`/`len`, both
  `extract_keywords` forms (plain and `span_info`) and `replace_keywords`,
  case-sensitive and insensitive processors, a custom non-word-boundary set, and
  the longest-match, word-boundary and end-of-sentence edges. The file/dict
  loaders and `get_all_keywords` are out of scope.

The port was checked against the library on every backend over the command
stream, and separately during development the whole stream was compared
line-for-line against `flashtext.KeywordProcessor`.

The vendored library package imports only the standard library; `vendor/` is
otherwise left untouched.

## Running

```bash
karac run mini_flashtext.kara              # interpret
karac build --mir mini_flashtext.kara      # native binary
python3 -s -P -B mk.py                       # regenerate data/jobs.tsv
python3 -I oracle.py                          # regenerate expected.out
python3 -s -P -B gen_kara.py                  # regenerate the embedded jobs
```
