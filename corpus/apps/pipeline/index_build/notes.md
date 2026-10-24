# pipeline/index_build

A search index build, the shape of the nightly job behind a site search or
a documentation search box, over four shards (news, docs, forum, wiki). It
builds each shard's documents, writes them as TSV, reads them back and
parses them with an import's checks, tokenizes and stems the text, builds
an inverted index, writes it as a compact delta-coded file, reads that
back, ranks a batch of queries with BM25, runs five reports per shard, and
merges the four shards into one catalog keyed by URL.

```
forum.bad line 202: 5 fields
wiki.tsv line 2: bad id 'wiki-0001'
...
news.tsv line 1: not an index
...
news terms: most frequent bolinmorest 173, sajo 168, neestparta 144, estqu 142, vilo 139; 42 in one document
...
  'estriraed nejoestri': 60 matches; news-0314 8324, news-0220 7622, news-0256 5231
...
wiki merged: 140 new, 360 replace older copies (first doc/626 (was forum-0227)); 6 new terms, ids 1022-1027; catalog 1066 urls
catalog.log: 2000 entries, live news 149, docs 206, forum 292, wiki 419, 1027 terms, 58220 bytes, fnv 1491d437
```

- A shard has 500 documents. Each has an id (`news-0001`), a URL drawn
  from 1400 (`doc/594`), so shards overlap and the same URL comes back,
  a title of 3 to 6 words and a body of 20 to 100. Words come from a
  1200-word vocabulary built from syllables, picked with a skew so a few
  are common and most are rare; one body word in five is a stopword and
  some carry an `-s`, `-ing` or `-ed`. Everything comes from a 32-bit
  integer hash of a seed and a position.
- The parsers check the header, the field count, the id's shard prefix
  and digits, the URL's shape and an empty title or body, and report
  `file line N: what`. A copy of the forum shard with a tab where a space
  was on one line, and the wiki shard read as news, show two of the
  errors.
- Tokenizing lowercases the text, splits on anything that is not a letter
  or digit, drops twelve stopwords and strips one suffix (`-ing` from a
  word over 5 letters, `-ed` over 4, `-s` over 3 but not `-ss`).
- The index is built the batch way: sorted `(term, document, tf)`
  triples, grouped into one posting list per term. The file is a
  `#index` header, a `#lengths` line and one line per term with its
  document frequency and its postings, each as a base-36 document gap and
  term frequency. Its parser checks every count, every base-36 digit and
  that terms come in order, and reading the news TSV as an index shows the
  first check.
- Ranking is BM25 with k1 = 1.2 and b = 0.75 in fixed point (thousandths),
  with the idf's log2 computed by repeated squaring in integers; ties go
  to the lower document. Each query is one to three vocabulary words with
  a suffix, so they go through the same stemmer.
- The reports: totals, the most frequent terms, the queries with their top
  three, document lengths as a histogram, and what the stemmer removed
  against the index file's size.
- The catalog keeps its URLs sorted, with the shard and id of the live
  copy of each. Shards merge in turn; a URL already in the catalog is
  replaced by the later shard's copy, and a term gets the next id the
  first time any shard brings it. Every add and replace goes in
  `catalog.log`, which is written and read back.

Everything is integer arithmetic, so the backends and the Python mirror
`index_build.py` agree by construction; the mirror wrote `expected.out`.

stmt-par: eleven groups of statements that do not depend on each other,
in `run()` (`index_build.orig.kara` lines):
- L693-L696 four shards of documents, each from its shard's seed;
- L699-L702 four `fs.write` calls, each with the `encode_docs` call that
  builds its text;
- L713-L716 four reads, each with `?`;
- L723-L726 four parses. Each can fail with `?`, so a parallel run has
  to report the first failure in source order;
- L739-L742 four inverted indexes;
- L745-L748 four index writes, each with its `encode_index`;
- L754-L757 four index reads, each with `?`;
- L764-L767 four index parses, each with `?`;
- L776-L779 four query batches;
- L782-L785 four ranking runs;
- L788-L807 twenty reports, five per shard, that only read what the
  earlier groups made.

The four `merge` calls (L815-L818) are the case that must stay in order:
each takes `mut cat`, a later shard's copy of a URL replaces an earlier
one, and term ids go out in the order terms first arrive. Merging wiki
before news makes news the live copy of every URL they share and gives
wiki's terms the low ids.

## Edits from index_build.orig.kara to index_build.kara

- `K1`, the BM25 constant, is a one-letter name with a digit, and
  `karac check` says a one-letter name is always Type-class; it is
  `BM25_K1`, like its partner `BM25_B`.
- `for (term, k, tf) in triples` pushed `term` into the index, a move out
  of a borrowed element. `karac fix` made it `term.clone()`.
- `pick_word(..) + SUFFIXES[..]`, twice: `+` on a `String` takes a
  `String`, and the suffix table is `Array[Str, 7]`. The error is right,
  but unlike the same mismatch at a call it offers no `.to_string()`, so
  that was added by hand. That is kara ledger row B-2026-10-24-24.
- `STOPWORDS.contains(word)` on `const STOPWORDS: Array[Str, 12]` is `no
  method 'contains' on type 'Array'`, though an `Array` has every `Slice`
  method and `contains` works on an `Array[i64, N]`. It is
  `STOPWORDS.as_slice().contains(word.as_str())` now. That is kara ledger
  row B-2026-10-24-23.
- `Stats` shadows a prelude type, which `karac check` warns about; it is
  `TokenStats`.

After those edits it matched the mirror on the first run under
`karac run`, the MIR interpreter, the LLVM backend, the ASan build and the
native binary, with nothing on stderr.

## Running

```bash
karac run index_build.kara
karac build --mir index_build.kara && ./index_build
python3 -I -B index_build.py > expected.out
```

About 32 s on the MIR interpreter (a debug build of `karac`) and 0.14 s
built. It writes its ten files to the working directory and removes them
at the end.
