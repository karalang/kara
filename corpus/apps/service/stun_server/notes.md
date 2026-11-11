# stun_server

A STUN server (RFC 5389, the Binding method) on loopback UDP
(`127.0.0.1:47766`). One reader takes datagrams off the server's socket and
hands each to a task of its own, which checks the message and answers on the
shared socket. The log and the counts live behind one `Mutex` in a
`sync struct`.

- **Checks, in order.** Under 20 bytes, the top two bits of the type set, a
  magic cookie other than `0x2112A442`, a length field that is not the body's
  length or not a multiple of 4, an attribute running past the end, or a
  `FINGERPRINT` that is not last or whose CRC-32 (XOR `0x5354554E`) does not
  match: the datagram is dropped without a reply.
- **Answers.** A response sent to the server is dropped, and an indication is
  counted and not answered. A request for a method other than Binding gets
  `400 Bad Request`. A Binding request with a comprehension-required
  attribute (type below `0x8000`) gets `420 Unknown Attribute` with
  `UNKNOWN-ATTRIBUTES` listing them; otherwise it gets a success response
  with the sender's address as `XOR-MAPPED-ADDRESS`. Every reply carries
  `SOFTWARE` and ends with `FINGERPRINT`, and optional attributes are
  ignored.

**Live run.** Thirteen scripted clients on the main task, each with a socket
of its own and a transaction ID from its seed: Binding requests without and
with a `FINGERPRINT`, one with a padded `SOFTWARE` and an optional
attribute, one whose `FINGERPRINT` is off by a byte, one with two unknown
comprehension-required attributes, an indication, a bad cookie, a short
datagram, an Allocate request, a success response, a bad length, and two
clients whose requests are in flight together. Each client decodes the reply
(class, transaction ID, mapped address against its own, error code, unknown
attributes, software, fingerprint) or prints that none came within 300 ms.
The server handles thirteen datagrams, ends when their tasks do, and prints
its log and counts.

Oracle: `stun_server.py`, the same server with `socket` and threads, and the
same clients.

## Porting notes

`stun_server.orig.kara` is the Rust-shaped first draft; `stun_server.kara` is
what `karac fix` made of it plus the hand edits below.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!`, `vec![..]`, `move ||`,
`.as_bytes()`, `usize` lengths, `&b` patterns, `for k in &v` as
`for k in ref v`, `!`/`||`/`&&` as `not`/`or`/`and`, and `.parse::<T>()`
as `T.parse(..)`.

Fixed in kara while writing it:
- `SOFTWARE_NAME.as_bytes()` on a const got no fix, and an adaptor on a
  const `Array` got its `.iter()` after the call: a method on a const parses
  as a path call, and the receiver and `)` took the whole call's span
  (B-2026-11-11-50).

By hand, around open rows:
- `attrs.concat()` on a `Slice[Vec[u8]]` is refused with a `T: Display`
  bound, and on a `Vec[Vec[u8]]` it is the elements' `Display` text, a
  `String`, where Rust flattens (B-2026-11-11-51); a loop of
  `extend_from_slice`.

`String::from_utf8_lossy(..)` in `describe` had no such function; kara
added it (B-2026-11-11-24), and the port calls it as the Rust does.

`Server` shadows a prelude type of the same name; check warns, and the
program means its own.
