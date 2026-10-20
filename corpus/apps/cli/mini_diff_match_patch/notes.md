# mini_diff_match_patch

diff-match-patch 20241021, Neil Fraser's library for synchronizing plain
text. Collaborative editors, wikis, note apps and sync servers use it to
diff two versions of a document, find a fuzzy match for a piece of text,
and apply a patch to a copy that has changed in the meantime. The port
covers:
- diffs: the common prefix and suffix, the half match, Myers' bisection,
  the line-level pass for long texts, and the semantic, lossless and
  efficiency cleanups with their scores for word, line and blank-line
  boundaries;
- deltas (lengths in UTF-16 units, insertions %-escaped), the
  Levenshtein distance, `diff_xIndex` and the HTML report;
- fuzzy matching with Bitap, scored by errors and by distance from the
  expected place, with `Match_Threshold` and `Match_Distance`;
- patches: making them, their GNU-diff-like text with %-escapes, parsing
  that text back, and applying patches with padding, splitting of long
  patches and fuzzy placement, with `Patch_DeleteThreshold` and
  `Patch_Margin`.

`mk.py` writes `data/`. It holds:
- documents in several versions: a deploy runbook edited by two people,
  YAML configs edited by ops, by a developer and rewritten, a story, a
  contract with accented text, and a server log with a few lines edited;
- a hand-written patch file;
- `jobs.txt`, 105 jobs. Many are the examples from the library's own
  tests, and the others run the documents through diff, patch and sync.

The run writes `data/`, then runs each job and prints its result. For a
diff it prints the diffs, the delta and whether it reads back, the
distance and the HTML. For a patch it prints the patch text, whether it
parses back, and what applying it gives. A sync job makes a patch from
two versions and applies it to a third, as a sync server does.

`oracle.py` runs the same jobs through diff-match-patch itself (the copy
in `diff_match_patch/`, unchanged) and wrote `expected.out`.
`gen_kara.py` writes the data section at the end of the program.

The quirks are kept. Some examples:
- The "Delta length" error counts bytes of UTF-16, twice the units: an
  `=2` delta on `abc` reports 4 against 6.
- An invalid patch mode error carries the rest of the line on a second
  line of the message.
- `patch_splitMax` loops over `range(len(patches))` as the list was when
  it started, while it inserts the pieces, so the index the body moves
  past the pieces is thrown away at the next turn.
- In Bitap the library narrows `start` inside the loop over `j`, but the
  range was fixed when the loop began, so that has no effect.
- A fuzzy placement of a patch longer than `Match_MaxBits` is dropped
  when its Levenshtein distance is more than `Patch_DeleteThreshold` of
  the patch's length; shorter patches are applied however far off the
  match is.

The library stops a diff after `Diff_Timeout` seconds. The port always
finishes, which gives the same diffs whenever the library finished in
time; on these inputs it always does. Only the sign of the timeout
matters to the port: at 0 the half match is not tried.

License: diff-match-patch is Apache 2.0 (`LICENSE-diff-match-patch`).
