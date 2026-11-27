# service/radius_server

A RADIUS server on two loopback UDP ports, authentication and accounting,
with one task per port over a shared state behind a `Mutex`, and the scripted
NAS clients that drive it. It follows RFC 2865 (authentication), RFC 2866
(accounting), RFC 3579 (Message-Authenticator) and RFC 5997 (Status-Server).
MD5 and HMAC-MD5 are written out in the program, since RADIUS is built on
them.

- **Packets.** A packet is a code, an id, a two-byte length, a 16-byte
  authenticator and attributes in type, length, value form. These are
  dropped silently, as the RFC asks:
  - a datagram under 20 bytes;
  - a length field outside 20..4096 or past the datagram;
  - an attribute shorter than 2 bytes or running past the end;
  - a code the port does not serve.
  
  Bytes past the length field are padding and ignored. An unknown client
  address is dropped too.
- **Authentication.**
  - PAP passwords are hidden as section 5.2 says, chained over 16-byte
    blocks; one test password takes three blocks.
  - CHAP is checked against a CHAP-Challenge attribute or, without one, the
    Request Authenticator.
  - carol has a second factor: her password earns an Access-Challenge with
    a State, and the 6-digit code sent back with that State is checked
    once.
  - An accepted user gets Service-Type, Framed-IP-Address, Session-Timeout,
    Class and a Reply-Message.
  - alice may hold one session at a time (Simultaneous-Use), which the
    accounting port's open sessions decide.
- **Accounting.**
  - Start, Interim-Update and Stop track a session's octets, time and
    terminate cause.
  - Accounting-On closes every session of the NAS that sent it.
  - A request whose Request Authenticator does not match the secret is
    dropped.
- **Integrity.**
  - Every reply carries a Response Authenticator.
  - A request with a Message-Authenticator is checked (HMAC-MD5 over the
    packet), and its reply gets one too.
  - Status-Server without a Message-Authenticator is dropped.
  - A repeated id with the same authenticator is a duplicate: the cached
    reply is sent again.
- **The clients.**
  - nas1 starts its ids at 250, so they wrap past 255.
  - nas3 is registered with one secret and signs with an old one, so the
    server rejects its passwords and the client sees a bad Response
    Authenticator.
  - rogue is not registered at all.
  - A real NAS draws its Request Authenticators at random. These are an MD5
    of the client's name and a counter, so every run sends the same bytes
    and the printed authenticators can be compared.
  - At the end, the admin halts each port with a Status-Server carrying a
    vendor attribute (enterprise 99999). Each port then prints its log,
    followed by the open and closed sessions and the counts.

  All secrets and passwords are obviously fake.

The mirror `radius_server.py` is the same server and clients, written
independently with `socket`, `threading`, `hashlib` and `hmac`. It wrote
`expected.out`.

Written 2026-11-27 by the real-world apps thread, with explicit std imports
since the prelude shrink. karac carries the fixes below.

## From radius_server.orig.kara to radius_server.kara

The draft was written Rust-shaped:

- `use std::sync::{Arc, Mutex}` and `use std::thread`;
- a `Port { state: Arc<Mutex<State>> }` per task, made with
  `Arc::clone(&state)`;
- `fn serve(mut self) -> Port` handing the port back from
  `thread::spawn(move || port.serve())`, then `.join().unwrap()`;
- `set_read_timeout(Some(Duration::from_millis(wait)))` with `wait: u64`;
- MD5 with `!b`, `wrapping_add`, `rotate_left`, `to_le_bytes`;
- `const` patterns in `match`, `User { max_sessions: 1, ..user(..) }`;
- an `Array` of `(&str, Option<&[u8]>, &str)` tuples walked with `for`.

Six gaps turned up. Each was fixed in the compiler before the fix passes
were rerun:

- B-2026-11-27-40: `mut self` was always fixed to `mut ref self`. A method
  that hands `self` back (`-> Port { .. self }`) consumes it, and now gets
  `own self`.
- B-2026-11-27-41: `Arc<Mutex<State>>` in a struct field became an owned
  `Mutex[State]`, so the second `Arc::clone` was a move. A field the file
  never fills with `Arc::new` is now `ref Mutex[State]`, which a task
  borrows from its group's scope.
- B-2026-11-27-42: after `Duration::from_millis(wait)` became `wait`, a
  `u64` was left where `set_read_timeout` takes `i64` milliseconds, with no
  fix. It now gets `as i64`.
- B-2026-11-27-43: with a `ref` field in `Port`, calling
  `self.handle(nas, secret, data)` was refused (E0509/E0516), as if `self`
  could keep the `Str` and `Slice` arguments. A view argument the receiver
  has no slot for now leaves only what its value holds.
- B-2026-11-27-44: a `for` over an `Array` in a function that suspends
  failed with "index is held across a suspension point". The frame-held
  index is now loaded before each use.
- B-2026-11-27-45: `int_of(Some(ref a.v))`, with a `Vec[u8]` at an
  `Option[Slice[u8]]`, passed the check and built invalid MIR. The `ref` to
  a sequence is now viewed as a `Slice`, as the sequence itself was.

After those, `karac fix` rewrote the draft in one run of passes (729 fixes)
with no hand edits. The first run matched the mirror byte for byte.
