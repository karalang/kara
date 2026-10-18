# mini_semver

npm's semver 7.7.2, the library every `package.json` range goes through,
with its `semver` command-line tool and an `npm outdated`-style report on
top. It is written from semver's source, regular expressions included:
the program builds the same pattern sources as `internal/re.js` and runs
them with a small backtracking matcher (capturing groups, unset groups
told apart from empty ones, leftmost-first alternation), so every quirk
of the patterns carries over.

What it covers:
- versions, strict and loose (`=v 1.2.3`, `1.2.3beta`, leading zeros),
  with the MAX_SAFE_INTEGER checks, numeric prerelease ids and build
  metadata;
- comparison and stable sorting, and `compareBuild`;
- ranges: carets, tildes (`~>` too), x-ranges, hyphen ranges, `||`,
  `<0.0.0-0` null sets, `*` swallowing the other sets, duplicate and
  `>=0.0.0` removal, the prerelease rule and `includePrerelease`, and
  loose mode dropping comparators it cannot read;
- `inc` for every release type, with `--preid`, `-n 0|1|false` and the
  identifier checks;
- `coerce` left to right and right to left, with `includePrerelease`;
- `diff`, `minVersion`, `intersects`, `maxSatisfying`, `minSatisfying`,
  `validRange` and `simplifyRange`;
- `bin/semver.js`, flag for flag, including `--flag=value` splitting
  (which turns a version written `=1.2.4` into an empty word and
  `1.2.4`), `-i` taking the next word only when it is a level, and the
  exit status;
- `outdated`, which reads a project's `package.json`, its
  `package-lock.json` and a registry index, works out what npm would
  want for each dependency (dist-tags, `latest` when it satisfies, then
  the highest non-deprecated match), prints the table, and explains
  each row: the kind of update, an installed version newer than the
  range allows or outside it, a newer major out of range, a deprecated
  install, a dist-tag that does not exist, a package missing from the
  registry.

The inputs in `data/` were made by `mk.py`: `registry.json`, sixteen
packages with their versions, dist-tags and deprecation messages, and
two projects, `web` and `cli`, each a `package.json` and the lock file
npm writes for it (lockfileVersion 3). `jobs.txt`, written by hand, has
131 jobs: `semver` runs the tool on the words that follow, `lib`
calls one library function, and `outdated` reports on a project.

Not supported: `subset`, `outside`/`gtr`/`ltr`, `Range` and
`Comparator` objects passed back in, and semver's LRU cache (which
changes no output). The safe patterns' bounds on `\d` runs and on build
identifiers (256 and 250) are left out, since they only differ past 250
characters and no input comes near that; the bound that matters,
`\s{0,1}` for `\s*`, is kept.

**Oracle.** `expected.out` is the output of `real.js`, which runs the
jobs against the real semver 7.7.2 under node 22: the command-line jobs
through its `bin/semver.js`, the library jobs through `require`
(`npm install --prefix realnode semver@7.7.2` puts it where `real.js`
looks). `oracle.py` is the same program as this one in Python, with
semver's patterns run by Python's `re`, and its output is byte-identical.

## Edits from mini_semver.orig.kara to mini_semver.kara

Compiler gaps:
- B-2026-10-15-71: check accepts `search(if loose { rx.loose } else
  { rx.full }, s)`, an `if` whose arms are fields of a borrowed struct,
  as a `ref` argument; every v2 surface refuses it as a move out of a
  borrowed place. The eight such calls write `ref rx.loose` in each arm.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan` on the first run.

Written 2026-10-15 by the real-world apps thread; karac at 530bac16ca.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
