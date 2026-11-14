# service/dns_resolver

An iterative DNS resolver over UDP, with the authoritative servers it walks,
all on loopback. Every socket binds port 0; a lab table maps each server's
documentation address (192.0.2.x) to the socket it got, so glue and NS
records carry real-looking addresses.

- **The servers.** A root server delegates `test.` to a TLD server, which
  delegates `example.test.` with glue (its first nameserver is lame: it
  takes queries and never answers) and `other.test.` without glue (its
  nameserver `ns.example.test.` lies inside another zone). Each answers
  with AA, a referral with the child's NS and glue, NODATA or NXDOMAIN with
  the zone's SOA at its negative TTL (RFC 2308), or REFUSED outside its
  zone. An answer over 512 bytes goes out with TC set and no records, and
  `example.test.`'s server also answers over TCP with the 2-byte length
  prefix (RFC 7766).
- **The resolver.** It starts from a root hint and walks referrals to the
  closest cached zone cut, keeping only in-bailiwick records. It resolves a
  glueless nameserver's address on its own, holds a server that timed out
  (200 ms) off for 900 seconds, retries a truncated answer over TCP, follows
  CNAME chains across zones and stops a loop after 8 links with SERVFAIL. Its
  cache holds positive and negative entries with TTLs counted down against a
  logical clock, which a CHAOS-class TXT query (`advance.<n>.clock.`)
  moves on. Stub queries with RD clear are answered from the cache or
  REFUSED; a cut-short question gets FORMERR, an opcode other than QUERY
  NOTIMP. Names are matched case-insensitively.
- **The stub.** The main task sends each query, prints the reply as `dig`
  would, and stops everything at the end. The resolver's log of every
  upstream query, and each server's query count, print last.

The mirror `dns_resolver.py` is the same servers, resolver and stub written
independently with `socket`, `struct` and `threading`; it wrote
`expected.out`.

Written 2026-11-14 by the real-world apps thread; karac at bdba568e93,
which carries the fixes below.

## From dns_resolver.orig.kara to dns_resolver.kara

The draft was written Rust-shaped (`::` paths, `&` and `&mut`, `vec!`,
`format!`, `Vec<T>`, `#[derive]`, `to_be_bytes`, `u16::from_be_bytes`,
`HashMap::entry`). Three gaps turned up, and each was fixed in the compiler
before the fix passes were rerun:

- B-2026-11-14-25: `&'static str` stopped the parse with no fix. Rust
  lifetimes are now deleted by `karac fix`, in borrows and generic lists.
- B-2026-11-14-26: `.map(|(_, n)| *n)` over lent `String`s passed the check
  and failed the build with no fix. The check now refuses it and writes
  `n.clone()`.
- B-2026-11-14-27: the interpreter could not send `b"STOP"` with
  `send_to`, which the compiled build sent.

After those, `karac fix` rewrote the draft over several passes, and four
diagnostics with no machine fix were answered by hand:

- `const A: u16 = 1` is refused: a one-letter name is a type name. Renamed
  `TYPE_A`.
- `collect::<Vec<_>>().join(".")`, twice: as the diagnostic says, bound to a
  typed `let` first.
- `trim_end_matches('.')`, twice: written `strip_suffix(".").unwrap_or(n)`,
  as the diagnostic says.
- `let names: Vec<&str> = [(QR, "qr"), ..].iter()...`: the literals are
  `String`s, so the annotation became `Vec[String]`.

The first run after that matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
