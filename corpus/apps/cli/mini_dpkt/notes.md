# mini_dpkt

dpkt 1.9.8, Dug Song's packet parsing and building library for Python,
on a small office network's captures. Intrusion detectors, flow
exporters, capture triage scripts and test harnesses all read traffic
they did not make. They have to accept both pcap byte orders and pcapng,
decode the layers they know and keep the bytes of the ones they don't,
survive frames that are cut short or lie about their lengths, and build
frames whose lengths and checksums are right.

The port follows dpkt's own object model rather than a typed struct per
header: each packet is a list of `__hdr__` fields with struct formats and
defaults, bit-field properties, the instance attributes in insertion
order, and `data`. That is what lets `__repr__` print exactly what dpkt
prints, which is only the fields that differ from their defaults.

The inputs in `data/` were made by `mk.py` with the copy of dpkt in
`dpkt/`, with addresses from the documentation ranges:
- **office.pcap**: a little-endian microsecond pcap of 37 frames: ARP,
  DHCP discover and offer, DNS (A through a CNAME with authority and
  glue records, MX, TXT, NXDOMAIN with an SOA, AAAA over IPv6), HTTP over
  TCP with the request and response each split over two segments (one
  chunked, one short), TLS ClientHello and ServerHello, ping, a
  traceroute probe answered with port unreachable, NTP, syslog on VLAN
  100, IPv6 ping, an MLDv2 report behind a hop-by-hop header, a datagram
  in two fragments, a segment with wrong checksums, LLDP (which dpkt
  does not decode) and a frame cut short by the snap length.
- **capture.pcapng**: a little-endian pcapng with section and interface
  options, a timestamp resolution, enhanced packet blocks (some with a
  comment), and an interface statistics block that the reader skips.
- **nano.pcap**: a big-endian nanosecond pcap.
- **notes.txt**: not a capture.

The program runs the jobs in `data/jobs.txt`:
- `info` and `list`: the format, link type, snap length and timestamps
  of each capture, and one line per frame with addresses, ports, flags
  and lengths.
- `repr` and `hexdump`: dpkt's `repr` of chosen frames and its hexdump.
- `dns`, `dhcp`, `ntp`: the decoded messages, their records and options,
  and NTP's timestamps.
- `http`: TCP streams reassembled by sequence number, parsed as requests
  and responses with their headers and bodies, and dpkt's errors for the
  ones that end early.
- `tls`: records, handshakes, the hellos, and each cipher suite read
  against dpkt's table (key exchange, cipher, MAC, forward secrecy,
  AEAD), with the extensions.
- `checksums`: the IPv4 header, TCP, UDP and ICMPv6 checksums of every
  frame, recomputed over the pseudo-headers (also behind VLAN tags and
  IPv6 extension headers).
- `stats`: ether types, IP protocols, services and talkers.
- `ngblocks`: the pcapng blocks and their options.
- `build`: a DNS answer packed with name compression, then UDP, TCP,
  ICMP, ICMPv6 and VLAN frames packed with every length and checksum
  filled in, each read back; then both capture formats written to
  `data/out/` and read back.
- `errors`: 26 broken inputs and what dpkt raises for each.

Not ported: the protocol modules the captures do not reach (LLC, STP,
MPLS, BGP, SCTP, radiotap and the rest), and the classes of the ported
modules that no job uses. Layer dispatch covers the ether types and IP
protocols of those modules only, so anything else stays bytes, as dpkt
leaves a layer it cannot decode.

**Oracle.** `expected.out` is the output of `oracle.py`, which runs the
same jobs through the copy of dpkt in `dpkt/` (BSD licence, in
`LICENSE-dpkt.txt`). `gen_kara.py` writes the data files and dpkt's
cipher suite table into the Kāra source.

## Edits from the first draft

Author errors:
- Bare parameters borrow (D5), so `fld`, `set` and `with_data`, which
  keep the value they are given, take it `own`, and `tls_multi` moves
  the records out with `.into_iter()`.
- `let (sid, mut at) = ..` is Rust syntax; Kāra puts `mut` before the
  whole pattern, so the parsers bind `at0` and copy it.
- `for s in mut out` does not exist; the stream sort walks the vector by
  index.

After that, the output matched `expected.out` on `__mir-run` and
`__mir-llvm-run` on the first run.

Written 2026-10-21 by the real-world apps thread; karac at 13fdc32828.
