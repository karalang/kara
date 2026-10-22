# mini_packaging

A port of PyPA [`packaging`](https://github.com/pypa/packaging) 26.3
(Apache-2.0 OR BSD) — the version logic that every `pip install`, build
backend and dependency resolver in the Python ecosystem runs through. This
kata ports `packaging.version.Version`: parsing a [PEP 440](https://peps.python.org/pep-0440/)
version string, rendering its canonical form, and ordering two versions.

## What it does

`mk.py` writes `data/jobs.tsv`, one job per line:

    <op> \t <payload>

- `norm <version>` → `str(Version(version))`, the canonical PEP 440 form, or
  the literal `INVALID` when `packaging` raises `InvalidVersion`;
- `cmp <a>\x1f<b>` → `<`, `=` or `>` for the ordering of the two versions
  (the two operands are separated by a US byte, `0x1f`).

`oracle.py` runs the jobs through the vendored `packaging`;
`mini_packaging.kara` reproduces `expected.out` byte-for-byte on every backend.

## How the port works

A version is parsed into an epoch, a release tuple, an optional pre-release
(its letter normalized — `alpha`/`a` → `a`, `beta`/`b` → `b`,
`c`/`pre`/`preview`/`rc` → `rc` — plus a number, with an implicit `0`), an
optional post release (`.post`, the `-N` shorthand, or `rev`/`r`), an optional
dev release, and an optional local segment (dot/underscore/hyphen-separated
parts, each a number or a lowercased word). The hand-written parser follows the
library's anchored, case-insensitive regex, so trailing junk makes the whole
string invalid.

Rendering reproduces `Version.__str__`. Comparison reproduces `_cmpkey`:

- the release tuple has its trailing zeros stripped, so `1.0.0 == 1`, and a
  shorter tuple sorts before a longer one with the same prefix;
- the pre/post/dev state folds into a six-integer suffix rank — a dev-only
  release (`1.0.dev1`) sorts before any alpha; a release with no pre-release
  tag sorts after `rc`; no-post sorts before post; dev sorts before no-dev;
- local parts compare as `(number, "")` for digits and `(-1, word)` for words,
  so a word sorts before a number, and a version with no local segment sorts
  before one that has one.

## Scope

The PEP 440 **version** grammar and ordering. Specifier-set matching
(`>=1,<2`, the compatible-release `~=`, prefix `==x.*`), requirement parsing
(`name[extra]>=1; marker`) and environment-marker evaluation are out of scope —
each is its own sizeable grammar and would suit its own kata. Job inputs are
well-formed PEP 440 strings plus a handful of deliberately invalid ones. The
vendored `packaging/` subset (`version.py`, `_structures.py`) is pure-Python
with no third-party dependencies.

## Running

```bash
karac run mini_packaging.kara                  # interpret
karac build --mir mini_packaging.kara          # native binary
python3 -s -P -B mk.py                           # regenerate data/jobs.tsv
python3 -I oracle.py                              # regenerate expected.out
python3 -s -P -B gen_kara.py                      # regenerate the embedded jobs block
```
