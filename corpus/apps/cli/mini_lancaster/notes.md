# mini_lancaster

NLTK's Lancaster stemmer (the Paice/Husk algorithm): the aggressive suffix
stripper search indexes use to fold "connection", "connected" and
"connecting" into one term. Its rule language (custom rule sets, rules
that only fire on an untouched word, rules that stop or carry on), its
optional prefix stripping and its length guard, and a small inverted index
built on it: conflation classes, the most frequent stems, and ranked
queries.

The program writes its own data (`data/docs/*.txt`, `data/rules/*.rules`
and `data/script.txt`), plays the script and prints a report. `oracle.py`
plays the same script against `nltk/stem/lancaster.py` from NLTK 3.10.4
(`vendor/`, Apache 2.0) and is the reference; `mk.py` writes the data and
`gen_kara.py` embeds it in the program. NLTK's `redos` module routes
patterns through the third-party `regex` engine; `vendor/nltk/redos.py`
stands in for it with the stdlib's `re.compile`, which takes the
stemmer's two patterns the same way.

## The stemmer

- A word is lower-cased (full Unicode lower-casing: `ΟΔΟΣ` becomes `οδος`
  with a final sigma, `İ` becomes `i` and a combining dot), then, with the
  prefix flag, loses the first of `kilo micro milli intra ultra mega nano
  pico pseudo` it starts with (only one, and possibly the whole word).
  A word longer than 1000 characters (code points, not bytes) is then
  returned as it stands.
- A rule is `ending` `*`? `digit` `append` (`>` or `.`)?: the ending is
  written reversed, `*` means the word must still be intact (no rule has
  changed it), the digit is how many characters to remove, the letters
  after it are appended, and `.` stops stemming while `>` (or nothing)
  carries on. Endings and appended text are `a` to `z`; the digit is one
  ASCII digit (the module's `\d` would also take another script's decimal
  digit; the data has none). Anything else is
  `ValueError: The rule <rule> is invalid`, a trailing space included.
- Rules are grouped by their first letter, in order. Each pass finds the
  last letter of the word's leading run of letters (so in `ness's` it is
  the second `s`, and in `x86_64` the `x`), and tries the rules filed under
  that letter against the end of the whole word. The first that matches,
  whose intact condition holds, and that leaves an acceptable stem, is
  applied; a pass that applies nothing ends stemming.
- An acceptable stem: a word starting with a vowel (`aeiouy`) keeps at
  least two characters; any other keeps at least three, with a vowel in
  the second or third place.
- A letter is what Python's `str.isalpha` says it is; the port asks
  `char.is_alphabetic`, which agrees on every character the data uses
  (it differs on letter numbers such as `Ⅻ` and on combining vowel signs).
- A rule set with no rules is the default rule set, as the module's
  `rule_tuple if rule_tuple else default_rule_tuple` makes it.

## The index

- A document is split at runs of whitespace (as `str.split()` does; the
  French document has a no-break space), and each piece loses the
  characters ``. , ; : ! ? " ' ( ) [ ] { } « » #`` from both ends. Empty
  pieces are dropped; the rest are the document's tokens.
- Each token is stemmed. The index keeps, per document, how often each
  stem occurs and the lower-cased tokens that stemmed to it.

## The script

- Each line is split into words at runs of spaces. Blank lines and lines
  whose first word starts with `#` are skipped. Every other line is echoed
  as `> ` followed by its words joined by one space, then runs; what it
  prints follows, each line indented by two spaces.
- A problem is reported as `  error: script.txt:<line>: <message>`, with
  the messages below. An unknown command is `unknown command <cmd>`, and
  the wrong number of words is the command's usage line, as given in each
  heading. A COUNT or MIN is one to six ASCII digits, else
  `bad count <word>`.
- `usage: stem WORD...` prints `<word> -> <stem>` for each word.
- `usage: stem-repeat PIECE COUNT` stems PIECE repeated COUNT times and
  prints `<piece> x <count>: <n> chars -> <m> chars, ending "<last 8>"`,
  in characters.
- `usage: use RULES [prefix]` (the second word, when there is one, must be
  `prefix`) makes a new stemmer, with the prefix flag when it is given.
  `default` is the module's own rule set; any other name reads
  `data/rules/<name>.rules` (`no rules <path>` when there is none), whose
  lines are the rules, blank lines and lines starting with `#` left out.
  An invalid rule is the `ValueError` above, and the old stemmer and index
  stay. Otherwise it prints
  `using <name>[ with prefixes]: <r> rules under <l> letters`, and the
  index is emptied.
- `usage: index DOC` indexes `data/docs/<doc>.txt` (`no document <path>`
  when there is none), replacing an earlier index of it, and prints
  `indexed <doc>: <t> tokens, <s> stems`.
- `usage: classes [MIN]` prints, for each stem with at least MIN (2 when
  not given) distinct tokens across the index, in stem order,
  `<stem>: <tokens>`, sorted and space-separated; `no classes` when none.
- `usage: top COUNT` prints the COUNT stems that occur most often across
  the index as `<stem> <count>`, by count, ties by stem; `index is empty`
  when nothing is indexed.
- `usage: query WORD...` prints `stems: <stems>` (the words' stems, in
  order), then each document holding every stem as `<doc> <score>`, the
  score being the stems' occurrences summed (a stem named twice counts
  twice), by score and then name; `no document matches` when none does.

Strings sort by code point (the port's byte order on UTF-8 is the same).
