# service/bt_tracker

A BitTorrent tracker on loopback HTTP, and the peers that announce to it:
announce (BEP 3), compact peer lists (BEP 23) and scrape (BEP 48). Every
answer is a bencoded dictionary, and much of what goes over the wire is
binary: the 20-byte info hash and peer id arrive percent-encoded in the
query string, a compact peer list is six raw bytes per peer, and a scrape's
`files` dictionary is keyed by raw hash.

- **Bencode and metainfo.** `Value` is an integer, a byte string, a list or
  a dictionary kept in a `SortedMap[Vec[u8], Value]`, so keys go out in the
  sorted order bencode requires. Two torrents are built as metainfo files,
  and each info hash is the SHA-1 of the bencoded `info` dictionary; a
  decode of each file must give back the same hash.
- **The tracker.** `Server.serve` answers `/announce`, `/scrape` and
  `/stats`; anything else is a 404. It parses `req.url()` itself and
  percent-decodes into bytes, since a hash is not text. An announce checks
  every parameter (info hash and peer id are 20 bytes, port, uploaded,
  downloaded and left are numbers, ip is a dotted quad, event is one of
  `started`, `completed` or `stopped`, numwant is at most 200) and answers a
  bad one with a bencoded `failure reason`. Peers are listed sorted by peer
  id without the one asking, compact or not, with or without peer ids. A
  `completed` event counts a download and a `stopped` one removes the peer.
  A bad percent escape is a 400.
- **The peers.** `seed`, `alice`, `bob` and `carol` announce, complete and
  stop; `mallory` sends every refused case. The program prints each answer
  decoded, then the tracker's counts, and cancels the server task.

The handler's `Response` declares `body: Vec[u8]`, and the server sends it
as it is with `content-type: application/octet-stream`.

The mirror `bt_tracker.py` is the same tracker and peers written
independently with `http.server` and `urllib`; it wrote `expected.out`.

Written 2026-11-23 by the real-world apps thread; karac carries the fixes
below.

## From bt_tracker.orig.kara to bt_tracker.kara

The draft was written Rust-shaped (`use std::collections::BTreeMap`,
`Arc<Mutex<Tracker>>`, `struct Decoder<'a> { data: &'a [u8] }`,
`digits.parse().map_err(..)?`, `u8::from_str_radix(h, 16).ok()?`,
`fn dict(pairs: Vec<(&str, Value)>)` consuming its argument,
`let mut d = vec![("ip", ..)]` passed on as `Vec<(&str, Value)>`,
`TaskGroup::new()` and `group.spawn(move || Server::serve(..))`, and a
`thread::sleep` retry until the server answers). Five gaps
turned up, and each was fixed in the compiler before the fix passes were
rerun:

- B-2026-11-23-60: a `Response` body could only be a `String`, so the
  tracker could not answer at all. A program's `Response` may now declare
  `body: Vec[u8]`: the server sends the bytes as they are, with
  `Content-Length` their count and `application/octet-stream` when the
  handler sets no type, and the client fills such a body with the bytes
  that came. Any other body type is refused at the handler.
- B-2026-11-23-61: `let n: i64 = digits.parse().map_err(f)?` was refused
  with "no method 'map_err' on type 'Option'" and no fix. It now gets
  `i64.parse(digits)`.
- B-2026-11-23-62: `u8.from_str_radix(h, 16).ok()?` was refused, as
  `from_str_radix` is already an `Option`, with no fix. The fix drops
  `.ok()`.
- B-2026-11-23-63: `for (k, v) in pairs { map.insert(k, v) }` over a bare
  parameter made `map` a map of borrows, refused only where it was used,
  with no cause and no fix. The refusal now names `pairs`, and the fix
  takes it `own` and iterates `pairs.into_iter()`.
- B-2026-11-23-64: `let mut d = [("ip", ..)]` passed on as
  `Vec[(Str, Value)]` was refused, as its literals had defaulted to
  `String`, with no fix. The fix annotates the `let`.

After those, `karac fix` rewrote the draft in one pass (510 fixes), with
no hand edits, and the first run matched the mirror byte for byte.
