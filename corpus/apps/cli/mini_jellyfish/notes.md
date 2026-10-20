# mini_jellyfish

jellyfish 0.11.2 by James Turk, the approximate and phonetic string
matching library Python record-linkage, search and "did you mean" code
uses. The port follows its pure-Python implementation
(`jellyfish/_jellyfish.py`) line for line:
- distances: Levenshtein, Damerau-Levenshtein (with its last-row table
  of characters), Hamming, Jaro and Jaro-Winkler with and without the
  long-string tolerance;
- phonetic codes: Soundex and Metaphone (both over NFKD, so accents
  fall away), NYSIIS, and the match rating codex and comparison,
  including its `None` when the codices differ in length by three or
  more.

Strings are lists of characters, the way Python indexes a `str`, and
case mapping is Python's (`ß` upper-cases to `SS`, a final sigma lower-
cases to `ς`). NFKD comes from `nfkd_table()`, which `tables.py`
generates from Python's `unicodedata` for the Latin, Greek, Cyrillic,
letterlike, number-form, ligature and fullwidth blocks; `mk.py` checks
that NFKD of every line in `data/` is those decompositions joined.

`mk.py` writes `data/`. It holds:
- `testdata/`: jellyfish's own test vectors, one CSV per function
  (all but the Porter stemmer, which the pure-Python version lacks);
- `names.txt`, names to encode: English, Irish, German, Spanish,
  French, Polish, Nordic, Turkish, Vietnamese, Greek, Cyrillic and CJK,
  with ligatures, fullwidth letters, a Roman numeral and blanks;
- `pairs.tsv`, pairs to compare with every function;
- `customers*.csv`: customer records with typos, case changes and
  accent variants, for the deduplicator;
- `commands.txt` and `typos.txt`, git's commands and mistyped ones;
- `surnames.txt` and `queries.txt`, a genealogy index and searches;
- `products.txt` and `product_queries.txt`, a shop catalogue and
  misspelt searches;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then its output:
- `vectors`: each test vector, with what the function gives and the
  CSV's expected value;
- `encode` and `compare`: the codes of each name, and every distance,
  similarity and code comparison of each pair;
- `dedupe`: records blocked by the Soundex and Metaphone of their last
  name, each candidate pair scored on Jaro-Winkler of the names, the
  Damerau-Levenshtein distance of the e-mail local parts and the match
  rating comparison, and the duplicate groups;
- `suggest`: git's "The most similar command is" for each typo;
- `search`: the surnames sharing a query's Soundex, NYSIIS or Metaphone
  code or passing its match rating comparison, and the three closest by
  Jaro-Winkler;
- `lookup`: catalogue entries ranked by per-word and whole-string
  Jaro-Winkler.

`oracle.py` runs the jobs through `_jellyfish.py`, unchanged, and
`expected.out` is its output.

jellyfish is MIT-licensed; see `LICENSE-jellyfish.txt`. The test vectors
come from its repository under the same license.
