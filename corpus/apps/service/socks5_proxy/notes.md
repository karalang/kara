# socks5_proxy

A SOCKS5 proxy (RFC 1928) with username/password login (RFC 1929) on
loopback (`127.0.0.1:47761`), in front of an echo service (`47762`) that
answers each line in capitals. Each client connection is a task.

- **Greeting.** Version 5 and a list of methods; the proxy picks the
  password method (`05 02`) or refuses (`05 ff`). Any other version is
  closed without an answer.
- **Login.** RFC 1929's version 1, user and password, checked against a
  table of two made-up users; `01 00` or `01 01`.
- **Request.** `CONNECT` only (`07` otherwise), to an IPv4 address or a
  name; the only name resolved is `localhost` (`04` otherwise, so the run
  never reaches DNS), IPv6 gets `08`, and only the echo port and one closed
  port are allowed (`02` otherwise). A refused connect gets `05`. Every
  reply carries `0.0.0.0:0` as the bound address, so the output does not
  depend on an ephemeral port.
- **Relay.** On success the proxy copies bytes both ways, one direction in
  a task of its own, and passes each half-close through; the log counts
  the bytes each way.
- **Lingering close.** Every connection ends with the proxy shutting its
  write side and draining what the client still sends, so a refused client
  reads its answer rather than a reset.

Ten scripted clients, one per path, print the bytes they get back in hex
(and the relayed text); then the proxy's log, one line per connection in
connection order.

Oracle: `socks5_proxy.py`, the same proxy, echo service and clients with
`socket` and threads.

## Porting notes

`socks5_proxy.orig.kara` is the Rust-shaped first draft; `socks5_proxy.kara`
is what `karac fix` made of it plus the hand edits below.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!` (with `{:02x}`),
`vec![..]` and `vec![0u8; n]` (`Vec.filled`), `move ||`, `.as_bytes()`,
`usize` lengths, `.to_string()` on literals, and the character `'\n'`
where a string is wanted.

`u16::from_be_bytes(pb)` and `port.to_be_bytes()` were refused, with a
suggestion of `to_ne_bytes`, the wrong order; kara 6fe383ed8a added the
`le`/`be` forms and their inverses (B-2026-11-10-45), and the `.kara` uses
them.

By hand, around open rows:
- `conn.read_exact(&mut head)?` at each step of the handshake and in the
  clients: no reader has `read_exact` (B-2026-11-10-44). The `.kara` has a
  `read_exact(conn, mut buf)` helper that loops over `read`.
- `String::from_utf8(..).unwrap_or_default()`: `Result` has no
  `unwrap_or_default` (B-2026-11-10-46); `.unwrap_or(String.new())`.
- `let local = [127u8, 0, 0, 1];` is a `Vec` in Kāra, refused where the
  `[u8; 4]` parameter wants an `Array`, with no fix (B-2026-11-10-47); the
  `.kara` annotates it.
- `text.trim_end_matches('\n')`: the refusal names `strip_suffix`, and a
  second, spurious error about a temporary follows it (B-2026-11-10-48).

By hand, as the language has it: `collect::<Vec<String>>().join(" ")` in
`hexs` is bound with a typed `let` first, as the turbofish refusal says.

`Log` shadows a prelude type of the same name; check warns, and the
program means its own.
