# mini_filecmp

Python 3.13's `filecmp` module, ported from `Lib/filecmp.py`. It is the
standard way to compare files and directory trees, and it sits under
deploy checks, backup verification and directory syncing scripts.

The port covers:
- `cmp(f1, f2, shallow)`. It compares stat signatures (file type, size,
  modification time) first, and falls back to reading the bytes. A
  directory or other non-regular file is never equal to anything;
- the module cache of past results. It is keyed by both paths and both
  signatures and cleared once it holds more than 100 entries. It is also
  cleared by `clear_cache()`;
- `cmpfiles(a, b, common, shallow)` with its three result lists: equal,
  different, and could not be compared;
- `dircmp` with its default ignore list (`RCS`, `CVS`, `tags`, `.git`, ...),
  its default hide list (`.` and `..`), and every attribute: the sorted
  listings, common names, one-sided names, common directories, files and
  funny cases, the file verdicts and `subdirs`;
- `report`, `report_partial_closure` and `report_full_closure` in their
  exact output format, including an error raised after the `diff` line;
- `python -m filecmp [-r] dir1 dir2`, with the `getopt` rules it runs
  under: no long options, and options stop at the first plain argument.

`mk.py` writes `data/`. It holds `jobs.tsv` (six jobs) and `trees/`,
which describes nine directory trees: files with fixed modification
times, directories, and symbolic links, one of them dangling. Both
`oracle.py` and `mini_filecmp.kara` build the trees under `work/` before
the jobs run. Then three programs run on top of `filecmp`:
- a deploy drift check between a release build and the copy on a server;
- a nightly backup check that compares each day with the next, shallow
  and deep, and lists what the shallow check missed;
- a mirror sync that plans copies, updates and deletes from a deep
  `dircmp`, applies them the way `shutil.copy2`, `copytree` and `rmtree`
  do, and then checks the trees again.

`oracle.py` runs the same jobs through the real module and wrote
`expected.out`.

The quirks are kept. Some examples:
- two files with the same size and modification time are "identical"
  under a shallow compare whatever their contents, so `app.js` and
  `version.txt` pass the shallow drift check while holding different
  versions;
- a deep compare that rewrites a file in place with the same size and
  time gets the stale cached answer until `clear_cache()`;
- a dangling link, and a name that is a file on one side and a directory
  on the other, are "common funny cases"; a missing name passed to
  `cmpfiles` is "could not compare", but a directory passed to it is
  "different";
- a link to a directory is followed, so `fonts` is a common
  subdirectory although one side is a link;
- `report` prints its `diff` line before it lists the directories, so a
  missing directory fails after that line;
- `-r` given twice works; `--recursive` and an option after the
  directories are refused.

It matched on every backend the first time it ran; no gaps were found.
