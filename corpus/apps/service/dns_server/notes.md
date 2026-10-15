# dns_server: an authoritative DNS server

An authoritative DNS server for `example.test.` on `127.0.0.1:47743`, using
DNS over TCP (RFC 1035 §4.2.2, RFC 7766): each message carries a 2-byte
length prefix, and several queries are pipelined on one connection.

- **Zone file.** The zone is written as a master file, read back and parsed.
  It handles `$ORIGIN`, `$TTL`, per-record TTLs, `@`, relative and absolute
  names, quoted TXT strings and `;` comments. It covers A, AAAA, CNAME, MX,
  NS, TXT and SOA records, plus a wildcard. A second zone with a bad address
  is refused with its line number.
- **Answers.** Replies carry the AA flag. Names are compressed with pointers.
  CNAME chains are followed inside the zone, and an out-of-zone target is
  left for the resolver. Wildcard answers are synthesised. NXDOMAIN and
  NODATA come with the SOA in the authority section. Other replies are
  REFUSED (outside the zone), NOTIMP (an opcode other than QUERY) and
  FORMERR (a forward compression pointer, or a cut-off question).
- **AAAA.** Addresses are parsed with `::` and printed in RFC 5952's form,
  where the first of two equal zero runs is the one compressed
  (`2001:db8::1:0:0:1`).
- **Client.** The client decodes every reply, following compression
  pointers, and prints it the way `dig` would, with the byte count.

Oracle: `dns_server.py`, the same program, which uses Python's
`ipaddress` for the address text forms. `expected.out` is its output.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-18:** there is no UDP socket, in the library spec or in v2
  (`UdpSocket` is an undefined name). DNS is normally UDP, and so are
  statsd, syslog, NTP, DHCP and QUIC. This app uses DNS over TCP.
- Known and worked around: B-2026-10-14-44 (socket addresses and `fs` paths
  are `String`), B-2026-10-12-6 (`parse` returns `Option`), B-2026-10-12-7
  (`split` is an eager `Vec[String]`).

The first draft ran on every surface with no other changes.

`dns_server.orig.kara` is the first draft.
