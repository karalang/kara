# mini_pip

The part of pip that decides what to install. Given a requirements file
and a package index, it works out one version of every project the file
needs, directly or through dependencies, and the order to install them
in. To do that it needs everything under it in `packaging`, the library
pip vendors, so this is a port of packaging 26.3 with a resolver on top:

- **Versions** (PEP 440): every spelling the spec allows (`v1.0`,
  `1.0-1`, `1.0.alpha.2`, `1!2.0`, `1.0+Ubuntu.1`, white space around
  it), the normalized form, epoch, release, pre, post, dev and local
  parts, and the ordering, where `1.0.dev0 < 1.0a0 < 1.0 < 1.0+local <
  1.0.post0`.
- **Specifiers and specifier sets** as packaging 26 evaluates them: each
  operator (`~= == != <= >= < > ===`, with `.*` wildcards) becomes a set
  of intervals with boundaries for post-releases and local labels, a set
  is the intersection, and `is_unsatisfiable` asks whether that is
  empty. `contains` and `filter` follow packaging's pre-release rules:
  `contains` accepts a pre-release unless told not to, and `filter`
  keeps pre-releases only when nothing else matches.
- **Markers and requirement strings** (PEP 508) with packaging's
  tokenizer, its error messages and their `~~~^` pointers, and marker
  evaluation against an environment, including `UndefinedComparison`
  and `UndefinedEnvironmentName`.
- **A backtracking resolver** over a JSON index with `requires_dist`,
  `requires_python`, yanked releases and extras. It reads requirements
  files with comments, `\` continuations, `-r` includes and `-c`
  constraints, and prints the pins, how many times it backtracked, and
  an install order with dependencies first. When there is no solution it
  prints the last conflict it hit.

The run writes the cases into `data/` and prints five sections:
versions, specifiers, markers, requirements, and one resolution for each
line of `jobs.txt`, which names a requirements file and one of three
environments (Linux CPython 3.12, Windows CPython 3.9, macOS PyPy 3.10).

The resolver tries the newest candidate first and backtracks on the
first conflict, depth first, in the order requirements were added. It
is deliberately simpler than pip's resolvelib: it does not prefer the
most constrained project, so its backtrack counts are its own.

Not supported:
- downloading, wheels, sdists, building and installing anything;
- hashes, `--index-url` and the other requirements-file options, editable
  installs, and environment variables in requirements files;
- URL requirements in resolution (they parse but the index has none);
- dependency groups (`dependency_groups` parses but nothing sets it).

**Oracle.** `expected.out` is the output of `oracle.py`, the same tool
written with packaging 26.3 itself and Python's `json`. Run it from this
directory after `mk.py` writes `data/`.

## Edits from mini_pip.orig.kara to mini_pip.kara

- The local-label closure in `Tokenizer.scan` declared `-> bool`, which
  is Rust syntax; Kāra infers a closure's return type. Author error.
- `parse_full_marker` and `parse_requirement` pass their fresh
  `Tokenizer` as `mut t`. Author error; `karac fix` applied both.
- `eval_marker` matches `ref items[i]`. `match items[i]` with an arm that
  only compares its binding (`MNode.Word(w) => if w == "or"`) is refused
  as a move out of an index; that is B-2026-10-15-7, already open.
- `Ver` gained `impl Eq for Ver {}`, which `==` needs to reach the
  hand-written `PartialEq`. The diagnostic said so.
- The JSON value enum is `JVal`, not `Json`, so it does not shadow the
  prelude's `Json` (a warning, not an error).

After those, the program matched expected.out on the first run on all
three MIR surfaces.

Written 2026-10-15 by the real-world apps thread; karac at 72cc0d4a00.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
