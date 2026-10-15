# mini_patch

The diff and patch half of a code-review or config-rollout tool. The run
writes three copies of a small web service's tree (`before/`, `after/`,
and `drifted/`, a copy someone edited locally after branching) and two
broken patch files under `patches/`, then:

- diffs `before` against `after` into `changes.patch`: a unified diff
  with three lines of context, `a/`/`b/` names, a `diff -u` line per
  file, `/dev/null` for the added `deploy.sh` and the deleted
  `legacy.ini`, and `\ No newline at end of file` for a README that lost
  its final newline;
- applies the patch to a copy of `before` (`patch -p1`), which then
  matches `after`;
- applies it in reverse (`-R`), which gets `before` back;
- applies it to the drifted copy: the nginx hunks are found 3 lines
  down, one app.py hunk is found 4 lines down, and the hunk whose
  context someone edited fails and is saved to `app.py.rej`;
- applies a malformed patch (a context line without its leading space)
  and a file of release notes with no patch in it.

After each run it prints the patch's messages, its exit status, and how
the tree differs from the one it should equal, as a unified diff,
followed by any reject files.

**The diff** is a port of Python difflib: `SequenceMatcher`'s longest
matching block first and then the pieces on each side (with its
"popular line" rule for files of 200 lines or more), the matching blocks
merged, opcodes, `get_grouped_opcodes(3)` and `unified_diff`'s hunk
headers (`,1` left out, `start,0` for an empty side).

**Applying** follows GNU patch 2.7 with `-p1 --fuzz=0`: each hunk is
looked for at its line number shifted by the offset the previous hunk
was found at, then one line further each way in turn, never before the
end of the previous hunk; a hunk with less leading than trailing context
must start the file, and one with less trailing context must end it. The
messages (`Hunk #4 succeeded at 46 (offset 4 lines).`, `Hunk #3 FAILED
at 32.`, the reject summary), the line numbers in them, the reject
file's renumbered hunk headers, exit status 1 for a failed hunk and 2
for a patch it cannot read are GNU patch's. Deleting a file to
`/dev/null` removes it.

Not supported: fuzz, context and ed-style diffs, git's extended headers
(renames, modes, binary), `-p` other than 1, and patch's prompts for an
already-applied or reversed patch.

**Oracle.** `expected.out` is difflib's and GNU patch 2.7.6's output on
the same trees, made by `oracle.py` after `mk.py` writes the trees (run
both from this directory). The trees, `patches/` and `changes.patch`
the run writes are byte-identical to the ones in this directory.

## Edits from mini_patch.orig.kara to mini_patch.kara

Compiler gaps, worked around:
- B-2026-10-15-41: the work-queue struct of the matcher was named `Range`. With a
  user `struct Range` in scope, `karac check` takes every `a..b` in the
  program as that struct and reports 52 errors on loops (`index must be
  an integer or range, found 'T'`), with no warning about the shadowed
  name, and the MIR front end refuses the program with a location-less
  "'Range' is already defined" against the library. The struct is
  `Todo`.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.
