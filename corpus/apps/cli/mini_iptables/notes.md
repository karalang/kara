# mini_iptables

iptables is the packet filter on most Linux servers. The program ports
the parts an administrator meets: loading a saved rule set, listing and
editing it, and what happens to each packet that arrives. It prints what
`mini_iptables.py` prints for the same `data/jobs.txt`. The messages and
listings follow iptables v1.8.10 (legacy) and conntrack-tools 1.4.8.

The host has three interfaces: `lo`, `eth0` on 203.0.113.10 (the
internet) and `eth1` on 10.0.0.1 (a LAN with a web server and an SSH
host behind NAT). The run writes into `data/`:
- `rules.v4`, an `iptables-save` file with a `nat` table (two port
  forwards, a MASQUERADE and an SNAT that the MASQUERADE shadows) and a
  `filter` table with DROP policies, the `SSH` and `LOGDROP` chains, and
  conntrack, icmp, multiport, limit, comment and negated-source rules;
- three broken copies: an unknown option, an unknown target, and a
  missing `COMMIT`;
- `jobs.txt`, the commands and packets to run.

**Restore.** `iptables-restore` replaces each table in the file at once,
or nothing: the broken files are refused with iptables-restore's message
and line number, and leave the tables as they were.

**Rules.** `--dport`, `--sport`, `--syn` and `--icmp-type` load the
tcp, udp or icmp match implicitly, so `iptables -S` prints `-m tcp`.
Addresses print with their prefix, conntrack states in the kernel's
order, and `--limit-burst` only when it isn't the default 5.

**Packets.** Each `packet` line is traced:
- conntrack decides NEW, ESTABLISHED (in either direction) or INVALID
  (a TCP packet with no connection that isn't a plain SYN);
- a new connection runs nat PREROUTING (DNAT). Later packets reuse the
  translation recorded on the first one, and replies are translated
  back;
- the route sends the packet to INPUT, FORWARD or OUTPUT, and the filter
  chains run, with jumps, RETURN and policies;
- an accepted new connection runs nat POSTROUTING (SNAT, MASQUERADE) and
  is recorded. A RST removes it.

LOG prints the kernel's log line. REJECT reports its `--reject-with`.
The limit match is xt_limit's token bucket on the packets' virtual
clock, so a burst of pings and of dropped packets is cut off and then
recovers. Counters on rules and policies count what matched.

**Commands.** `iptables -S`, `-L -n -v` with and without
`--line-numbers`, `-A`, `-I` at a position, `-D` by rule or number,
`-N`, `-X`, `-F`, `-Z` and `-P`, with iptables' errors and exit codes;
`conntrack -L`; and `iptables-save -c`.

`mk.py` holds the input files. `gen_kara.py` prints them as the Kāra
data functions in the program.

## Edits from mini_iptables.orig.kara to mini_iptables.kara

This was an author error, not a compiler gap:
- `fail(msg: String, ..)` stored its parameter, which needs `own String`.

These are workarounds for gaps:
- `if parts[1] == "-" { "" } else { parts[1].as_str() }` and the same
  shape in `save` were refused: the literal branch stays a `String`
  instead of taking its sibling's `Str` (B-2026-10-18-6). Both now
  annotate `let policy: Str`.

`conntrack_list` once bound `c.orig` before reading `c.orig.proto`, a
workaround for B-2026-10-14-70. It was dropped after the fix in
0bacd9912b.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits, with one more by hand: `let given = a.split(",")`, now a one-shot iterator, was walked once per state inside a loop; check passed it, and MIR refused it as a use after move (B-2026-10-18-36). It collects into a `Vec[Str]` and walks `given.iter()`. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
