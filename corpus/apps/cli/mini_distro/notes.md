# mini_distro

Linux distribution detection, as distro 1.9.0 does it. distro is the
library that pip, Ansible's fact gathering, cloud-init and many install
scripts call to find out which OS they are on. The port is made from
the `distro.py` module vendored here (Apache-2.0, see `LICENSE-distro`).

`LinuxDistribution` reads its data sources in distro's order of trust:

- os-release, from `etc` and falling back to `usr/lib`. It is parsed
  with shlex in POSIX mode, so quotes, escapes, comments, multi-line
  values and stray tokens all behave as they do in distro. The codename
  comes from `VERSION`, or from `VERSION_CODENAME` or `UBUNTU_CODENAME`
  when those are present.
- `lsb_release -a`.
- The distro release file, found by scanning `etc` in sorted order. The
  scan skips the ignore list, directories, names the basename pattern
  refuses, and files that yield no name.
- `uname -rs`.
- AIX's `oslevel`.

Each source is read lazily and cached, as distro's cached properties
are. So `repr()` names the release file before the scan has found it,
and an os-release that raises does so again on every call.

On top of the sources sit:

- `id` with the three normalization tables (`ol`, `opensuse-leap`, the
  Red Hat lsb IDs, `redhat`) and CloudLinux's override.
- `name` and its pretty form.
- `version`: pretty, the "best" rule that prefers the most dots, the
  `debian_version` candidate for Debian-like systems, and the oslevel
  candidate on AIX.
- `version_parts`, `like`, `codename`, `linux_distribution` and `info`.
- The command line (`--root-dir`/`-r`, `--json`/`-j`, a cluster like
  `-jr`, `--root-dir=`), including `json.dumps` with `sort_keys` and
  `ensure_ascii`.

distro parses release lines by matching a regex against the reversed
line. For that, and for its other four regexes, the port carries
mini_re's engine (Python 3.13's parser and an sre-style backtracking
matcher) and calls it the way distro does. The shlex lexer comes from
mini_shlex.

The run writes `data/` and works through `data/jobs.tsv`. Every fake
root under `data/roots` goes through the full report:

- Ubuntu 22.04 and 14.04, Debian 12 and testing, Fedora 39, CentOS 7
  and 5, RHEL 9 and 6, Rocky 9, Alpine 3.19, Arch, openSUSE Leap 15.5
  (`usr/lib` only), SLES 12, Amazon Linux 2, Mint 21, Raspbian 11,
  Gentoo, Slackware 15, CloudLinux 6, Oracle Linux 8, Mageia 9 and
  NixOS.
- Roots with no `etc`, an empty root, and a root that does not exist.
- An os-release with an unclosed quote, and one that uses all of the
  shell syntax.
- A root with several release files.
- A root where no name matches the pattern.

The remaining jobs:

- Explicit `os_release_file` and `distro_release_file` paths.
- Outputs of `lsb_release`, `uname` and `oslevel` fed into a root.
  distro never runs those commands from a root directory, so the oracle
  sets the cached properties directly.
- The constructor's guard against that.
- The four parsers on their own.
- The command line.

`expected.out` is the output of `oracle.py`, which runs the same jobs
through distro itself. `mk.py` writes `data/`, and `gen_kara.py` prints
the data section at the end of `mini_distro.kara`.

One workaround is in place. `major minor build` takes each part as
`(d.version_parts(re, false)?).0`, because `x?.0` does not parse
(B-2026-10-19-30).
