# mini_pkgconfig

pkgconf 1.8.1, the `pkg-config` that Ubuntu 24.04 ships, ported from its
own C source and run over a tree of `.pc` files. It answers what the real
tool answers: the same flags in the same order, the same errors and
warnings, and the same exit status.

The run writes the `.pc` files into `data/` and a `jobs.tsv` of 124
`pkg-config` command lines, one per line with tab-separated arguments.
Leading `NAME=value` arguments are environment variables for that
command. It runs each line in `data/` with `PKG_CONFIG_LIBDIR=pc` and
prints the `$ ...` line, the command's stdout, its stderr and
`[exit N]` when it fails.

The files:
- `pc/` holds 25 real `.pc` files copied from an Ubuntu 24.04 install
  (GLib and GIO, zlib, libpng, FreeType, Fontconfig, OpenSSL, SQLite,
  libxml2, libmount, libselinux and their dependencies). It also holds a
  made-up "Acme" project whose packages require and conflict with each
  other, a package that provides another name, one with a missing
  dependency, one that wants a version that does not exist, a
  hand-written file with odd but legal syntax, and macOS-style
  `-framework` flags.
- `opt/acme/lib/pkgconfig/` holds a relocatable package, for
  `--define-prefix`.
- `build/` holds a build tree's `acme-core-uninstalled.pc` next to a
  staged `acme-core.pc`.
- `broken/` holds a file without a `Version`, one with whitespace in odd
  places, a dependency cycle and an unterminated quote.

The jobs cover:
- `--modversion`, `--exists` with version rules given as one argument
  or three, `--atleast-version`, `--exact-version`, `--max-version`,
  `--atleast-pkgconfig-version` and `--version`;
- `--cflags` and `--libs`, with `--static`, `--pure`, the `-only-I`,
  `-only-other`, `-only-l` and `-only-L` variants, `--fragment-filter`,
  `--keep-system-cflags`, `--keep-system-libs` and
  `PKG_CONFIG_ALLOW_SYSTEM_LIBS`, `--maximum-traverse-depth`,
  `Requires.internal` with and without `--internal-cflags`, and `--env`;
- `--variable`, `--define-variable`, `--print-variables`,
  `--print-requires`, `--print-requires-private`, `--print-provides`,
  `--path`, `--digraph`, `--list-all` and `--list-package-names`;
- `PKG_CONFIG_PATH`, an empty `PKG_CONFIG_LIBDIR`, `--with-path` (which
  `PKG_CONFIG_LIBDIR` overrides), a `.pc` file named on the command
  line, uninstalled packages with `--uninstalled` and
  `--no-uninstalled`, and `--define-prefix`;
- the errors for a missing package or dependency, an unsatisfied
  version, and a conflict, with `--print-errors`, `--short-errors`,
  `--silence-errors`, `--errors-to-stdout` and `--ignore-conflicts`;
- `--validate` on the broken files, a dependency cycle, no package
  named, an unknown option, an abbreviated option and a missing option
  value.

**The port** follows pkgconf's code function by function: the line
reader (backslash continuations, `#` comments), the `key: value` and
`key=value` parser with its warnings, variables (expanded as each line
is read, globals first, a quoted value dequoted), the dependency-list
state machine, the search path with its cache and `-uninstalled` files,
the scan for a package that `Provides:` a name, the graph walk with its
conflict check, and the fragment lists. Those lists hold each flag with
its type, and a word after `-framework` or `-isystem` is joined to it.
A flag seen twice is kept once, at the place pkgconf's rules keep it.
The command line is read as `getopt_long_only` reads it: one dash or
two, any unique prefix, and operands anywhere.

Not supported:
- options this run does not use: `--about`, `--help`, `--simulate`,
  `--log-file`, `--relocate`, `--msvc-syntax`, `--personality` and
  `--dump-personality`, `--prefix-variable` and
  `PKG_CONFIG_SYSROOT_DIR`;
- a second `prefix=` line under `--define-prefix` (pkgconf 1.8.1 reads
  a pointer it never sets there);
- `--list-all` lists in name order, where pkgconf lists in the order
  `readdir()` returns the files. The oracle sorts those lines. The scan
  for a provider also reads directories in name order.
- Two search directories are taken to be the same directory only when
  their paths are equal; pkgconf compares their inodes.

**Oracle.** `expected.out` is pkgconf 1.8.1's output on the same files,
made by `oracle.py` with the Ubuntu 24.04 `pkg-config` package. It is
run from this directory after `mk.py` writes `data/` and `jobs.tsv`.
`gen_kara.py` prints the Kāra data section from the same files.

## Edits from mini_pkgconfig.orig.kara to mini_pkgconfig.kara

Author errors:
- `private` is a reserved word, and the fragment functions used it as a
  parameter name. It became `is_private`.
- `new_dep` and `put_tuple` stored their parameters, which a bare
  parameter cannot give away. They take `own`.
- `LongOpt` was cloned without `#[derive(Clone)]`.
- `arg.parse[i64]()` is not how a string becomes a number; it is
  `i64.parse(arg)`.
- Six calls passed `self.globals`, `self.collected` and a local's list
  to a `mut ref` parameter without the `mut` marker. `karac fix` added
  the markers.

Compiler gaps:
- B-2026-10-18-10: the client struct was called `Client`, and
  `karac check` found none of its methods, because it sends every
  method call on a type named `Client`, `Response`, `HttpError` or
  `RequestBuilder` to the library's HTTP types. It became `PkgClient`.
- B-2026-10-15-15: there is no `rfind`. `last_index` finds the last
  slash or dot.

B-2026-10-14-70 (chained field receivers like `self.c.out`) was worked
around by folding main()'s `Run` struct into the client until its fix
in 0bacd9912b; the `Run` struct is back.

After those edits the program matched expected.out on the first run on
both MIR surfaces.

Written 2026-10-18 by the real-world apps thread; karac at 16b322caae.
