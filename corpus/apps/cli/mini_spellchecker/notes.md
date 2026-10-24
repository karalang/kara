# mini_spellchecker

[pyspellchecker](https://github.com/barrust/pyspellchecker) 0.8.1 (MIT),
Tyler Barrus's spell checker after Peter Norvig's "How to Write a
Spelling Corrector", ported to Kāra and used the way a documentation team
would use it: a dictionary built from the team's own docs, the documents
in the review queue checked against it, and the dictionary edited,
exported to JSON and opened again. The port runs 175 commands and prints
what the library returns for each.

```
docs stats: 5979 total, 249 unique, longest 11, letters ['012345689_abcdefghijklmnopqrstuvwxyz]; top the 710, a 249, and 229, to 151, is 110, one 80
docs split: 13 [Don't|panic|it's|quoted|a''b|x_y|3|14|rock'n'roll|users|end|_x_|tis]
docs unknown: {0x10, 1_, 1__0, 1e, _1, ab, nan, servise}
docs candidates ro: {do, to}
docs ed2 job: 77575 edits, 9 known: {do, for, how, job, log, on, to, too, you}
docs check: 23 words, 3 unknown: ofwen -> often (1); rublic -> public (1); thalt -> that (1); fnv 6cc9cb8c
names candidates THe: {THE, The}
folded freq skiff: 1
```

## What it does

`mk.py` writes two files. `data/corpus.txt` is 90 paragraphs of
documentation for a deployment tool, sentences from twenty templates
("Don't delete the {A} {N} until the {N} {V}.") filled from lists of
nouns, verbs, adjectives and numbers, with contractions, possessives,
`max_retries`, port numbers and quoted phrases in them. `data/jobs.txt`
holds the commands:

- `new` makes a checker (`SpellChecker(language=None, distance=...,
  case_sensitive=...)`), and `load_text_file`, `load_text`, `load_words`
  and `add` fill its word frequency table; `pop`, `remove`,
  `remove_words` and `remove_threshold` take words out again; `stats`
  prints the totals, the letter set, the longest word and the six most
  frequent words.
- `freq`, `contains`, `split`, `known` and `unknown` ask about words and
  text: case folding, the tokenizer's handling of apostrophes and
  underscores, and the rule that a number, or anything else Python's
  `float()` accepts (`1_000`, `1e5`, `Infinity`, but not `1__0`, `1e` or
  `0x10`), a lone punctuation mark and a word more than three letters
  longer than any in the dictionary are never checked.
- `candidates` and `correction` for correct words, words one and two
  edits away, numbers, `nan` (which is checked, and corrects to `can`),
  ties broken by sorted order, and words with no candidate at all; `ed1`
  and `ed2` show the edit sets themselves.
- `check` runs a document through the checker as a docs pipeline would:
  the unknown words, each one's correction and how many candidates it
  had, and a hash of the document with every correctable word replaced.
  Sixteen documents come from the corpus's templates with one to three
  typos each (deletions, transpositions, replacements and insertions,
  some of them two edits deep), and three are written by hand.
- `export` writes the dictionary as JSON with sorted keys; `open` starts a
  checker from that file. A case-sensitive checker for product names
  (`Skiff`, `Linux`, `API`) is exported and opened again as an ordinary
  checker, which folds the whole file to lower case before parsing it,
  so `Skiff: 3` and `skiff: 1` become one key holding the last count.
- A checker with distance 1 misses what distance 2 finds; setting the
  distance to 0 or 3 gives 2. An empty checker checks no word longer than
  three letters.

`oracle.py` runs the same jobs through the vendored library (`vendor/`,
the package's four Python files without its language dictionaries,
licence in `LICENSE-pyspellchecker.txt`) and wrote `expected.out`. The
library returns sets, which both sides print sorted.

## How the port works

The word frequency table is a `Map[String, i64]` with what the library's
`_update_dictionary` keeps beside it (total, unique count, letter set,
longest word), recomputed after every change as the library does. A
checker that is not case-sensitive folds every word on the way in and on
every lookup. The tokenizer is the regex `\w[\w']*\w|\w` written out: the
greedy run of word characters and apostrophes, then back to the last word
character. `_check_if_should_check` keeps its order of tests, with
Python's `float()` grammar for the number test.

`edit_distance_1` builds the deletions, transpositions, replacements and
insertions over the dictionary's letters, as a set. `candidates` returns
the word itself when it is known (in the case it was given) or not
checked, then the known words one edit away, then, at distance 2, the
known words one edit from any of those edits. That last step only asks
whether each string is in the dictionary, so the port builds each edit in
one reused buffer and looks it up, where the library makes a set of every
edit of every edit; the words it finds are the same. `correction` takes
the most frequent candidate, the first in sorted order on a tie, as
`max(sorted(...), key=...)` does. `edit_distance_2` keeps the library's
list semantics: its length counts every edit of every edit.

`export` is `json.dumps(..., sort_keys=True)` and `load_dictionary` a
small parser for the flat object it writes; a key repeated after folding
keeps the last value, as `json.loads` does, and the counts are then added
to the table as `Counter.update` adds them.

## Compiler gaps

None. `karac check` stopped my first draft twice, and both were my
mistakes: a `for` over a method's returned `Vec[String]` that moved each
word into the table needed `.into_iter()` (E0200 says so), and a `u32`
cast straight to `char` for a JSON `\u` escape needed `char.try_from`
(E_INT_AS_CHAR names it). After those the MIR interpreter matched the
library on its first run, and so did the LLVM backend, the ASan build and
the native binary, byte for byte with nothing on stderr.

The first job list took 68 s on the interpreter, over the corpus runner's
60 s limit: every unknown word with no candidate one edit away costs
an edit-distance-two search, about 200,000 strings over this
dictionary's 38 letters. The jobs now ask for fewer of those, and `check`
takes the correction from the candidates it already has instead of
computing them a second time through `correction()`, which picks the same
word.

## Scope

The port covers `SpellChecker` and `WordFrequency` with `language=None`:
building the table from text, word lists and counts, every lookup and
edit, the distance setter, and plain-JSON export and import. The bundled
language dictionaries (their data is not part of this port), gzip,
custom tokenizers, `word_usage_frequency` (a float) and `bytes` input are
left out. Text is ASCII here; the port works on characters, as Python's
`str` does, but its `\w` is ASCII letters, digits and `_` where Python's
is any Unicode word character.

## Running

```bash
python3 -s -P -B mk.py                 # writes data/corpus.txt and data/jobs.txt
python3 -I -B oracle.py > expected.out
python3 -I -B gen_kara.py              # corpus_text() and jobs_text() at the end of the .kara
karac run mini_spellchecker.kara
karac build --mir mini_spellchecker.kara && ./mini_spellchecker
```

About 27 s on the MIR interpreter (a debug build of `karac`) and 0.1 s
built. The program writes `data/corpus.txt` and `data/jobs.txt`, reads
them back, writes and reads the two exports, and removes all four.
