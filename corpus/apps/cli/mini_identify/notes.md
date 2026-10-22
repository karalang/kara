# mini_identify

A port of [identify](https://github.com/pre-commit/identify) 2.6.20 (MIT, Chris
Kuehl & Anthony Sottile) — the library [pre-commit](https://pre-commit.com) uses
to tag a file by what it is (`python`, `shell`, `json`, `binary`, …) from its
name and shebang, so each hook can decide which files to run on.

## What it does

`mk.py` writes `data/jobs.tsv`, one tab-separated job per line (`op \t arg`).
`oracle.py` runs each through the vendored `identify` package and prints
`expected.out`. `mini_identify.kara` reimplements the name/shebang path and must
match byte-for-byte on every backend.

Ops:

- `filename <name>` → `tags_from_filename` (sorted tags, or `(none)`);
- `interpreter <interp>` → `tags_from_interpreter`;
- `shebang <line>` → the parsed `parse_shebang` command tokens, in order;
- `shebang_tags <line>` → `tags_from_interpreter` of the shebang's first token.

## How the port works

identify's four lookup tables — `EXTENSIONS` (343 entries),
`EXTENSIONS_NEED_BINARY_CHECK` (5), `NAMES` (96) and `INTERPRETERS` (25) — are
embedded verbatim by `gen_kara.py` as a text block (`tables_text`, one
`kind⇥key⇥sorted-tags` row per entry) and parsed once at startup into parallel
arrays. That keeps them out of a giant compile-time constructor (which would hit
the quadratic-build gap a 287-line literal table triggered in another app) — the
whole 469-row program compiles in ~2 s.

The algorithms follow the library exactly:

- **`tags_from_filename`**: `os.path.splitext`'s leading-dot rule (a name whose
  only dots are leading, like `.bashrc`, has no extension); the NAMES lookup
  over the full name *then* each dot-split piece, so `Dockerfile.xenial` matches
  `Dockerfile`; then the extension against EXTENSIONS and the binary-check
  table. The result is a set, so the union of the NAMES and extension matches is
  de-duplicated and sorted for a stable line.
- **`tags_from_interpreter`**: strip the `/usr/bin/` path, then narrow
  `python3.5.2` → `python3.5` → `python3` until a table entry matches.
- **`parse_shebang`**: require `#!` and printable ASCII, then the
  `/usr/bin/env` and `/usr/bin/env -S` rewrites.

## Scope

Shebang tokenization is whitespace splitting; the library best-guesses with
`shlex` first for the rare quoted shebang, but every real (unquoted, ASCII)
shebang tokenizes identically. The `nix-shell` multi-line `-i` form, and the
filesystem/`stat` and license-detection paths, are out of scope.

## Running

```bash
karac run mini_identify.kara              # interpret
karac build --mir mini_identify.kara      # native binary
python3 -s -P -B mk.py                      # regenerate data/jobs.tsv
python3 -I oracle.py                         # regenerate expected.out
python3 -s -P -B gen_kara.py                 # regenerate the embedded tables/jobs block
```
