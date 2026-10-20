# mini_unidiff

unidiff 0.7.5 by Matias Bordese, the unified diff parser Python code
review bots, patch tools and CI checks read diffs with. The port follows:
- `PatchSet`: the line-by-line parse of git, svn, hg, bzr and plain
  `diff -u` output, with the three forms of `diff --git` header (`a/`
  and `b/` prefixes, URI-like names, no prefix), new and deleted file
  modes, renames, `Binary files ... differ` lines and `GIT binary
  patch` blocks, the patch info lines kept before each file, no-newline
  markers inside and after hunks, blank context lines and trailing
  blank lines, and every `UnidiffParseError` the parser raises;
- reading from a string, from a file (universal newlines) and with
  `metadata_only`, which counts lines without keeping them;
- `PatchedFile`, `Hunk` and `Line` with their counts, line numbers,
  `is_added_file` / `is_removed_file` / `is_modified_file` /
  `is_rename`, `path`, `is_valid`, their reprs, and `str(patch_set)`,
  which writes the diff back out.

Each regex in unidiff's `constants` module is a function here that
matches the way Python's `re` does, so the odd cases come out the same:
the lazy filenames of the binary-diff regex (`Binary files a/x and y.png
and b/x and y.png differ` names the target `y.png and b/x and y.png`),
section headers that keep a trailing `\r`, and a binary file with no
target, whose `path` raises the parser's own `TypeError`.

`mk.py` writes `data/`. It holds:
- `samples/`: the twenty sample diffs from unidiff's test suite;
- `extra/`: malformed diffs (hunks too long and too short, a target
  without a source, orphan hunks, modes and markers, bad body lines)
  and corners (hunk headers without lengths, section headers, blank
  context lines with LF and CRLF, mid-hunk markers, empty hunks,
  URI-like and spaced names, non-ASCII text, an email patch);
- `project/`: a small Python project at four versions, and git diffs
  of each change at three context widths (`-U3`, `-U1`, `-U0`), with
  edits, new and deleted files, renames, a changed image, and files
  with no newline at the end;
- `comments*.tsv`, review comments on lines of the new versions, and
  `rules.txt`, words the scanner flags;
- `jobs.tsv`, the jobs.

Each job prints `== ` and its fields, then its output:
- `parse`: the patch set, file by file, hunk by hunk and line by line,
  and whether `str()` gives the input back (and what it gives when not);
- `stat`: a `git diff --stat` style summary, bars scaled to 40;
- `apply` and `revert`: the diff applied to the old version (or
  backwards to the new one), checking every context and removed line,
  compared with the other version; two diffs are applied to the wrong
  base, so hunks fail;
- `position`: where review comments on new-file lines fall in the diff,
  as GitHub counts positions;
- `scan`: added lines holding a flagged word.

`oracle.py` runs the jobs through the unidiff package beside it,
unchanged, and `expected.out` is its output. Errors print as
`error: <exception class>: <message>`.

unidiff is MIT-licensed; see `LICENSE-unidiff.txt`. The samples come
from its test suite under the same license.
