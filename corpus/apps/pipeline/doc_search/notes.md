# pipeline/doc_search

A full-text search tool over a folder of documents, the core of a docs-site
search or a desktop indexer. It writes six help pages for a made-up build
tool, indexes each one in its own `TaskGroup` task, and collects the
per-document indexes over a channel; the merge sorts them by name, so the
result does not depend on which task finished first.

Indexing lowercases, splits on anything that is not a letter or digit,
drops stopwords, strips a plural or "-ing" ending and records each term's
positions. Queries are AND by default, with `OR`, `-term`, `"quoted
phrases"` (matched by position, so stopwords inside are skipped) and
`prefix*`; results are ranked by tf * idf in integers and printed with a
snippet around the first hit. Bad queries are reported (an unterminated
quote, a dangling OR, a one-letter prefix, only stopwords).

The index is saved to `search.idx` with each document's FNV-1a content
hash. After one page is rewritten, one added and one removed from the set,
a restart reloads the index, re-indexes only the changed and new pages (in
parallel again), drops the removed one and keeps the rest. A damaged index
file is reported with its line and rebuilt from scratch.

The mirror `doc_search.py` is the same algorithm in Python, with a
three-worker `ThreadPoolExecutor` and a `queue.Queue` in place of the task
group and channel, and wrote `expected.out`.

Written 2026-10-15 by the real-world apps thread; karac at 0e5d2694d3.

## Edits from doc_search.orig.kara to doc_search.kara

Author errors (the diagnostics were right, or the spec says so):
- `if c { ok = false }`: an assignment is a statement, so the block needs
  `ok = false;`.
- `drop(tx)` to close the channel: Kāra has no `drop` function. The last
  sender is moved into a block (`let tx = tx;`) and drops at its end.
- `enum Match` shadows the prelude's `Match` (a warning); renamed `Want`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-15-11 (new): the implemented task and channel surface is not
  concurrency.md's. `TaskGroup.new(limit: 3)` is refused, `send` returns
  `()` instead of a `Result`, `recv` returns `T` instead of `Option[T]`,
  `Receiver` has no `into_iter`, and a `TaskHandle` cannot be returned
  from a function. The app drains the channel with `try_recv`.
- B-2026-10-15-12 (new): check keeps the borrow an `any(|j| j.0 ==
  d.name)` closure took after the call, so the following `kept.push(d)` is
  refused. A `for` loop does the test.
- B-2026-10-14-48: no `channel(cap)`; `Channel.new()`.
- B-2026-10-12-3: no `SortedMap.get_mut`; `entry(t).or_insert(..)`.
- B-2026-10-15-5: `save` cannot `map_err` `fs.write` into a
  `Result[(), String]`, because `Unit` is not `()`.
- B-2026-10-14-68: `vec!["cache", ..]` is a `Vec[String]`, so the query
  lists are annotated `Vec[Str]`.
- B-2026-10-14-52: iterating the `const DOCS` array aborts both compiled
  surfaces with an out-of-bounds index; main walks a local copy.

Went in clean: closures spawned on a `TaskGroup` capturing a borrowed
stopword set and an owned sender clone, joining handles, `SortedMap` /
`SortedSet` of `String`, `entry().or_insert().push()`, tuple returns,
`filter_map` / `chain` / `collect` chains, `sort_by` with `cmp`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.
