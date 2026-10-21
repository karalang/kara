# mini_dnspython

dnspython 2.7.0, the DNS toolkit Python programs use to read and write
zone files, build and decode DNS messages, and answer queries (it is
what certbot's DNS plugins, Ansible's `dig` lookup, OctoDNS and many
small authoritative servers and monitoring scripts sit on). The port
covers:
- `dns.tokenizer`: the zone-file tokenizer, byte by byte, with its
  parentheses, quoting, escapes, comments and line counting, and its
  readers for integers, TTLs (`1h30m`), strings and names;
- `dns.name`: names from text (escapes, `@`, relative names and an
  origin) and from the wire (compression pointers and their errors),
  `fullcompare` and the subdomain tests, `relativize`, `derelativize`,
  `split`, `concatenate`, `parent`, canonical form and wire output with
  a compression table;
- `dns.rdata` for A, AAAA, NS, CNAME, PTR, DNAME, MX, TXT, SPF, SOA, SRV,
  CAA, HINFO, SSHFP, TLSA and SMIMEA, and the generic `\# <len> <hex>`
  form for every type (a known type given that way is decoded and must
  write back the same bytes), each from text, to text and to and from
  the wire, with `dns.ipv4` and `dns.ipv6`;
- `dns.zonefile`: the reader, with `$TTL`, `$ORIGIN`, `$GENERATE` (the
  `${offset,width,base}` modifiers), inherited owners and TTLs, the
  SOA-minimum default TTL, CNAME-and-other-data, and the
  `<string>:LINE:` prefix on its errors; `dns.zone` with `to_text`
  (relative, absolute, with comments);
- `dns.message`: `from_wire` (header, sections, EDNS and TSIG refused,
  TTLs over 2^31 read as 0, trailing junk), `to_text`, `make_query`,
  `make_response`, and `to_wire` through `dns.renderer`, with its
  `max_size`, `TooBig` rollback and `prefer_truncation`;
- `dns.reversename` both ways.

`mk.py` writes `data/`:
- `zones/`: the zones of a small company, Example Shop: its main zone,
  a partner zone, two reverse zones built with `$GENERATE`, and the next
  revision of the main zone;
- `cases/` and `broken/`: 79 zone files for the reader's corners and for
  each reader and record error;
- `jobs.tsv`: 285 jobs. They load zones, do name arithmetic, build
  queries, answer queries from a zone the way a small authoritative
  server does (answers, referrals with glue, CNAME chains, NXDOMAIN with
  the SOA), decode messages off the wire, some well formed and some
  broken, turn addresses into reverse names and back, and diff two
  revisions of a zone.

Each job prints `-- ` and its columns, then its output. A Python
exception prints `! `, its class and its message.

`oracle.py` runs the jobs through dnspython itself (the copy in `dns/`,
unchanged) and wrote `expected.out`. `gen_kara.py` writes the generated
section at the end of `mini_dnspython.kara`: the rdata type table and
`data/`.

The quirks are kept. Some examples:
- `$GENERATE` accepts a modifier written `${0,3,d}` with the width
  zero-padding the counter, and a lowercase `n` base gives lowercase
  nibbles while `N` gives uppercase;
- an owner written as a quoted empty string (`"" A ...`) is an
  `IndexError`: the reader looks at the token's first character before
  checking that it has one;
- `from_wire` reads a TTL above 2^31 - 1 as 0, as RFC 2181 asks;
- the generic form of a known type is decoded and printed in that
  type's own text form, not as `\#`.

dnspython is under the ISC license, in `LICENSE-dnspython.txt`.
