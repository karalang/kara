# mini_netaddr

The IPv4 CIDR algebra of `netaddr` 1.3.0 (BSD, David P. D. Moss) — the network
address library used by Ansible, Fabric and many network-automation tools to
summarise, split and exclude blocks of addresses.

The port follows the library function for function:

- **`cidr_merge`** — turn an arbitrary list of addresses and subnets into the
  smallest possible set of CIDRs, by sorting them as ranges, merging adjacent /
  overlapping / contained ones, and re-splitting each merged range optimally.
- **`spanning_cidr`** — the single smallest CIDR that covers a whole sequence of
  subnets.
- **`iprange_to_cidrs`** — the exact minimal CIDR list between two boundary
  addresses, via the `cidr_partition` subnet-splitting primitive.
- **`cidr_exclude`** — remove one subnet from another, returning what remains.
- **glob ↔ CIDR** — `valid_glob`, `glob_to_cidrs`, `cidr_to_glob`,
  `glob_to_iprange` and `iprange_to_globs`, i.e. netaddr's `x.x.x-y.*`
  glob syntax and its conversion to and from CIDRs and ranges, with the
  single-glob-else-split fallback and the "only one hyphenated octet" rule.

The sort order, the `spanning_cidr` bit-clearing loop, `cidr_partition`'s
left/middle/right split and the glob octet classification are reproduced
exactly, so the summarised output matches netaddr's. Addresses are carried as
32-bit integers and everything is integer and string arithmetic; **no float is
formed**.

**Scope.** This is the IPv4 CIDR-algebra surface (netaddr's `IPGlob` is itself
IPv4-only). IPv6, the `IPSet` class, the IANA / nmap / RFC 1924 helpers and the
IEEE OUI/IAB registry are out of scope.

**Jobs.** `data/jobs.tsv` holds one job per line, tab-separated: an operation
and its argument (multi-value arguments use `,` for lists and range endpoints,
`|` for `exclude`'s target|exclude). The set exercises each operation plus the
error paths (an unparseable network, an unrecognised glob, and a too-short
`spanning_cidr` sequence).

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the same
jobs through the copy of `netaddr` in `vendor/netaddr/` (BSD, in
`LICENSE-netaddr.txt`). `mk.py` writes `data/jobs.tsv`; `gen_kara.py` embeds the
jobs as `jobs_text()`.

Written 2026-10-22 by the real-world apps thread. It compiled clean under
`karac check` and matched `expected.out` byte for byte on `__mir-run`,
`__mir-llvm-run`, `__mir-llvm-asan` and `build --mir` on the first run, with no
new compiler gaps.
