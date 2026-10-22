# mini_requirements

A port of [packaging](https://github.com/pypa/packaging) 26.3's
`packaging.requirements` (Apache-2.0 OR BSD-2-Clause, Donald Stufft &
contributors) — the PEP 508 `Requirement` that every `requirements.txt` line
and every package's `Requires-Dist` metadata is written in. A requirement is
`name [extras] specifier ; marker`, or `name [extras] @ url ; marker`.

## What it does

`mk.py` writes `data/jobs.tsv`, one tab-separated job per line:

- `req <requirement-string>` → the parsed parts: `name=… | extras=… | spec=… |
  url=…` (extras sorted, specifier in packaging's canonical deduplicated-and-
  sorted form, `-` where a part is absent);
- `eval <requirement-string> <extra>` → the requirement's environment marker
  evaluated against a fixed environment (a Linux / CPython 3.11.5 profile),
  with `<extra>` naming the extra being installed (`-` for none) → `True` /
  `False` / `(no marker)`.

Both print `INVALID` for a requirement the grammar rejects. `oracle.py` runs
each job through the vendored `packaging` and prints `expected.out`;
`mini_requirements.kara` reproduces it byte-for-byte on every backend.

## How the port works

The requirement parser reads the name (which must start and end alphanumeric),
an optional `[a,b]` extras list, then either `@ url` or a specifier set up to
the `;`, then an optional marker. A malformed name, a specifier clause with no
operator or no version, or trailing junk is rejected exactly where packaging
rejects it.

The marker evaluator is a recursive-descent over a tokenized expression
(`and` / `or` / parentheses / `VAR op VALUE`). Each comparison is oriented so
the bare-word side is the environment variable, then:

- on the four **version keys** (`python_version`, `python_full_version`,
  `implementation_version`, `platform_release`) the comparison is
  `Specifier(op + rhs).contains(lhs, prereleases=True)` — the same PEP 440
  range membership the specifier core (shared with `mini_specifiers`)
  implements, so `python_version >= "3.8"` means what pip means by it;
- every other key uses PEP 508's string table: `in` / `not in` are substring
  membership, `==` / `<=` / `>=` are equality, `!=` inequality, and `<` / `>`
  on a string marker are always false;
- an `extra == "..."` comparison canonicalizes both the literal and the
  environment value per PEP 503 (lowercase, `-_.` runs collapsed), so
  `extra == "Dev"` matches the extra `dev`.

## Scope

The marker environment is fixed so the output is reproducible on any host; the
`req` output omits the marker's canonical serialization (it reports marker
*evaluation* instead), and the deprecated dotted marker variable spellings
(`os.name`, `sys.platform`, …) and the set-valued `extras` / `dependency_groups`
lock-file markers are out of scope.

## Running

```bash
karac run mini_requirements.kara              # interpret
karac build --mir mini_requirements.kara      # native binary
python3 -s -P -B mk.py                          # regenerate data/jobs.tsv
python3 -I oracle.py                             # regenerate expected.out
python3 -s -P -B gen_kara.py                     # regenerate the embedded jobs block
```
