# mini_lunr

A port of [lunr.py](https://github.com/yeraydiazdiaz/lunr.py) 0.8.0 (MIT,
Yeray Díaz Díaz), the Python port of lunr.js: the small full-text search
engine that static documentation sites (MkDocs among them) build an index
with at deploy time and search in the browser. It builds the index over a
documentation site's pages, runs the searches its readers type, writes the
index as the JSON lunr.js loads, reads it back and searches again.

```
index: 50 pages, 181 terms, 150 field vectors
q74 "error codes": 26 results
  ref/api/errors 29.146942 code,error
  faq/manifest 3.691097 error
  ...
    error title 0+5
    error tags 4+6
    error body 17+5 701+5 753+5
q25 "conf*~1": 20 results
  ref/cli/config 9.095000 config
...
q117 "~1": error: Expected either a field or a term, found EDIT_DISTANCEwith value "1"
serialized: 193398 bytes, crc32 68be1d32
loaded index: 1035 of 1035 lines agree
```

## What it does

`mk.py` writes `data/docs.txt`, the 50 pages of a made-up registry
client's documentation (guides, command and API reference, FAQ, release
notes), each with a title, tags and a body of generated sentences with the
punctuation real pages have (`(see`, `e.g.`, `don't`, `--force`,
`max_retries`, `x509:`, `sha256:3f2a`), three of them boosted; and
`data/queries.txt`, 145 searches. `oracle.py` indexes the pages with the
vendored lunr.py, the title boosted ten times, with `position` metadata
kept for highlighting, prints each search's result count, its top five
pages with their scores and matched terms, and where each term matched on
the best page, then the serialized index's size and CRC-32 and how many
lines the reloaded index reproduces. `mini_lunr.kara` does the same with
the data `gen_kara.py` embeds.

The searches cover plain terms and several, stop words, field scopes
(`title:push`), presence (`+push +denied`, `-scan`, a query of only
prohibited terms, which returns every page without them at score 0), boosts (`push^10`,
`pull^0`), wildcards at either end and in the middle (`*tion`,
`re*ion`, `*`), edit distances (`regstry~1`, `imgaes~2`, `conf*~1`),
escapes (`foo\:bar`, `\-scan`), terms that match nothing with and without
`+`, and every error the parser raises.

## How the port works

- **The pipeline** is lunr's: the tokenizer splits on whitespace, the
  no-break space and the hyphen and records each token's position; the
  trimmer is `^\W*?([^\W]+)\W*?$`, so `(see` loses its parenthesis but
  `e.g.` and `don't` stay whole; then the stop word list (read off the
  vendored source by `gen_kara.py`) and lunr.py's Porter stemmer, its
  Python negative indexes included (`ies` stems to `i`). Searches run the
  stemmer alone.
- **The index** follows `Builder`: postings in the order terms and pages
  first come, BM25 weights with lunr's k1 and b, the field boost and the
  page boost, rounded the way `round(x, 3)` rounds, sparse field vectors,
  and the token set built from the sorted terms by `TokenSetBuilder` with
  its minimisation.
- **Searching** follows `Index.query`: the query lexer and parser as state
  machines, a token set for each term (a wildcard is a self-loop, an edit
  distance the fuzzy automaton with its insertions, deletions,
  substitutions and transpositions), the intersection with the index's
  token set walked with lunr's stacks so expanded terms, match data and
  ties come out in lunr's order, the required and prohibited sets with
  lunr's `CompleteSet`, and the cosine of query and field vectors.
- **Quirks kept**: the parse errors' wording, `EDIT_DISTANCEwith` with its
  missing space, and the presence error that names the presence lexeme's
  type rather than the one after it; `--force` is two presence operators,
  so it is refused; a query of only prohibited terms scores every page
  without them 0; and `pull^0` still matches its pages, at score 0.
- **One quirk not kept**: lunr keys a token-set node for minimisation by
  its finality, each edge's label and the child's id run together, so a
  digit label next to an id could make two different nodes look alike.
  The port keeps label and id apart. On these pages lunr's keys never
  collide (checked by comparing every merge lunr makes, for the built and
  the reloaded index), so the indexes are the same.
- **The JSON** is written as `json.dumps(index.serialize())` writes it,
  each field vector value as Python's `repr` of a 3-decimal float, and
  read back with a small JSON reader into the same index.

## Compiler gaps

One, kara ledger row B-2026-10-24-42: `s.as_bytes()` on a `String`, at 13
places, is a bare `no method 'as_bytes' on type 'String'` with no note or
fix-it, though the method is `bytes()` and the typechecker's CStr table
accepts `as_bytes`. The calls read `.bytes().to_vec()`, which is the
library's method. Thread A fixed the row (kara 4f6892cea5): the message
now names `bytes()` and `karac fix` rewrites the call.

The rest of what `karac check` raised was mine: `K1` and `B` as constant
names (a one-letter name is Type-class, as the message says; now
`BM25_K1` and `BM25_B`), `i64.from_str_radix` returning an `Option`
rather than a `Result`, and the JSON value enum missing `#[derive(Clone)]`,
which `karac fix` added. It also warned that a struct named `Vector`
shadows the prelude's, so it is `SparseVec`. `mini_lunr.orig.kara` is the program as first written. After those
edits the MIR interpreter, the LLVM backend, the ASan build and the native binary all
matched the oracle on the first run, with nothing on stderr.

## Scope

- English only: no `lunr.languages` (they need NLTK), no custom
  separators, extractors or pipeline functions, and no `Query` built by
  hand with the leading and trailing wildcard flags.
- Pages and searches are ASCII, so `\W` and `lower()` are ASCII's.
- `Index.load` takes the JSON this program writes; the JSON reader is not
  general.

## Running

```bash
karac run mini_lunr.kara                    # interpret
karac build --mir mini_lunr.kara            # native binary
python3 -s -P -B mk.py                      # regenerate data/
python3 -I -B oracle.py > expected.out      # regenerate expected.out
python3 -I -B gen_kara.py                   # re-embed data/ in mini_lunr.kara
```

About 9 s on the MIR interpreter (a debug build of `karac`), 2 s on the
LLVM backend and 0.06 s built. It writes `mini_lunr_index.json` to the
working directory, reads it back and removes it.
