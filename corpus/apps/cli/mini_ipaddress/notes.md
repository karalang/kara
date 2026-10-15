# mini_ipaddress

IP addresses and networks as Python 3.13's `ipaddress` module handles
them, with an address planner and a firewall built on top. Cloud
network tools, firewalls, load balancers, log pipelines and access
lists all parse addresses, test membership and carve up address space.
The program ports the module with 128-bit `u128` arithmetic and its
exact error messages.

The run has five parts:
- **addresses**: the 53 strings in `data/addresses.txt`. For each valid
  address the program prints:
  - its repr, version and integer;
  - the exploded and compressed forms;
  - the reverse DNS name and the packed bytes;
  - the `is_*` properties;
  - for IPv6, its IPv4-mapped, 6to4, Teredo and scope id parts.

  The inputs include zero-run compression, `%` zones, IPv4-mapped
  addresses printed in dotted form, and an IPv4 tail on a full address.
  For each invalid string it prints `ip_address`'s `ValueError` and the
  `AddressValueError` of both `IPv4Address` and `IPv6Address`. That
  covers every message the two parsers have.
- **networks**: 31 networks written with prefix lengths, netmasks and
  hostmasks, strict and loose. For each one it prints the masks, the
  `with_*` forms, the broadcast address, the first hosts (with the /31,
  /32, /127 and /128 rules), the exploded form and the properties. It
  also prints the supernet and the two halves, and every
  `NetmaskValueError` and "host bits set" error. One input is a
  network with a scope id, whose `exploded` raises inside CPython; the
  port raises the same error.
- **operations**: 52 calls to `subnets`, `supernet`, `address_exclude`,
  `collapse_addresses`, `summarize_address_range`, `in`, `overlaps`,
  `subnet_of`, sorting, address arithmetic and its overflow errors,
  `ip_interface` and `__format__` with `#`, `_` and `b`/`x`/`X`/`n`.
- **plan**: an IPAM over an IPv4 and an IPv6 pool. It reserves fixed
  blocks, refuses one that overlaps, allocates best-fit blocks, refuses
  requests that don't fit, frees a block, and shows each pool's usage,
  allocations and free blocks. The free list is kept with
  `address_exclude` and `collapse_addresses`.
- **firewall**: an ACL whose rules name allocations from the plan
  (`@web`), so a rule naming the freed allocation is skipped with an
  error. It decides 21 connections first-match, with protocols, port
  lists and ranges, IPv6 and zoned sources, and two malformed lines. It
  labels each source global, private or shared (100.64/10), counts the
  hits per rule, and collapses the denied global sources into a
  blocklist.

Not supported: constructing addresses and networks from integers, bytes
or tuples (the program parses strings and builds the rest internally),
and `__format__` with a width or fill. The planner and the firewall are the
program's own and are the same in both versions.

**Oracle.** `expected.out` is the output of `oracle.py`, the same
program written with `ipaddress`. Run it from this directory after
`mk.py` writes `data/`.

## Edits from mini_ipaddress.orig.kara to mini_ipaddress.kara

Compiler gaps:
- **B-2026-10-15-52 (new):** `prefix_from_ip_int` wrote
  `let leading = if trailing >= 128 { 0 } else { ip >> trailing };`
  with `ip: u128`. `karac check` typed the `if` from the literal in its
  first branch, so `leading` became `i64` and the comparison with a
  `u128` was refused. The literal is now `0u128`. With the branches the
  other way round it passes.
- **B-2026-10-15-53 (new):** `supernet` had the same shape,
  `let m = if d >= 128 { 0 } else { netmask(n) << d };`, and then
  `n.addr.ip & m`. Here check passed `u128 & i64`, which it refuses for
  `+`, and every v2 surface stopped with "invalid MIR: binary operands
  of different types u128 and i64". The literal is now `0u128`.
- **B-2026-10-15-54 (new):** an unsuffixed literal above `i64::MAX` is
  refused at parse even where the expected type is `u64` or `u128`, so
  `let all: u128 = 0xFFFF_FFFF_FFFF_FFFF;` doesn't compile. A probe
  found this before the program was written, so the original already
  spells such literals with a `u128` suffix.

Author errors:
- `Rule` needed `#[derive(Clone)]`.
- `num_addresses` of `::/0` is 2**128, one more than a `u128` holds,
  and the first run stopped with an overflow there. The network report
  now prints the count as text.

After that, the output matched `expected.out` on `__mir-run`,
`__mir-llvm-run` and `__mir-llvm-asan`.

Written 2026-10-15 by the real-world apps thread; karac at eb605b2e5b.
