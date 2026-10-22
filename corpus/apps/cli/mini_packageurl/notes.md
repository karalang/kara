# mini_packageurl

The parse / canonicalize / build core of `packageurl-python` 0.17.6's
`PackageURL` (MIT) — the Package-URL (purl) library that SBOM,
SCA and supply-chain security tooling (SPDX, OSV, CycloneDX, dependency
scanners) use to name a package unambiguously across ecosystems, as
`pkg:type/namespace/name@version?qualifiers#subpath`.

The port follows the library's `packageurl/__init__.py`, function for
function:

- **`from_string`** parses a purl into its six components, applying the
  per-type normalization rules — lowercasing namespace and name for the types
  the spec names (github, bitbucket, pypi, composer, npm, …), pypi's
  `_`→`-`, hackage's `_`→`-`, pub's `[^a-z0-9]`→`_`, cpan's uppercase
  namespace, mlflow's Azure-ML / Databricks case rule, npm's leading `@scope`
  namespace, golang's multi-segment namespace and subpath — and
  percent-decoding each component on the way in.
- **`to_string`** rebuilds the canonical purl: it re-applies the type rules,
  percent-encodes each component (keeping `/` and restoring `:`, as the
  library's `quote` does), and sorts and case-folds the qualifier keys.
- The **component constructor** applies the same rules without a backing
  string, so a purl built from parts canonicalizes identically.

Every `ValueError` is reproduced with the library's exact text — the missing
`pkg` scheme, the missing type, the missing name, a percent-encoded qualifier
key, and the required constructor arguments — including CPython's `repr` of the
offending string.

**The urlsplit reduction.** `from_string` calls `urllib.parse.urlsplit` to
separate the path, the `?`-qualifiers and the `#`-subpath. For a well-formed
purl the spec strips the leading slashes before the type, so the remainder has
no `//` authority and urlsplit's scheme/colon handling is a no-op; the split
then reduces to *fragment after the first `#`, query after the first `?`, path
the rest*, with colons preserved. This port reproduces that reduction directly
rather than porting urlsplit whole — the docker `@sha256:…` version and a
`download_url=https://…` qualifier both exercise the preserved colon and the
query-side `//`.

**Jobs.** `data/jobs.tsv` holds 55 jobs, one per line, tab-separated. Field 0
is `parse` (a purl string) or `build` (six component fields, `-` for None). A
`parse` job prints the six components (`-` for an absent one, qualifiers as
sorted `key=value` joined by `&`) and the canonical string, or `err=<message>`.
A `build` job prints `str=<purl>` or `err=<message>`. The parse set is the
`package-url/purl-spec` test suite (the copy vendored with the library),
covering per-type lowercasing, npm's `@` namespace, golang subpaths, docker's
colon version, percent-encoded and literal `@` names, qualifier sorting and
case-folding, the slash-stripped `pkg:/`, `pkg://` and `pkg:///` forms, and
every error path, plus real-world pypi/cargo/golang/generic/oci/huggingface
purls. The build set exercises the constructor and the per-type name rules
(pypi, hackage, pub, cpan, mlflow) and output percent-encoding.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `packageurl` in `vendor/packageurl/` (MIT, in
`LICENSE-packageurl.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds
it into the Kāra source as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
