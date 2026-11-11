# syslog_collector

A syslog collector on loopback: datagrams on UDP (`127.0.0.1:47767`) and
connections on TCP (`127.0.0.1:47768`). One task reads the UDP socket, and
each TCP connection gets a task of its own; the records, the rejects and a
count of kept frames per transport live behind one `Mutex` in a
`sync struct`.

- **Framing (RFC 6587).** A TCP frame that starts with a digit is
  octet-counted: a decimal length, a space, then that many bytes, gathered
  across reads. A length over 8192, or no space in the first nine bytes,
  ends the connection with a reject. Any other frame runs to the next
  newline, and a connection that closes mid-line keeps that last line; one
  that closes mid octet-counted frame rejects it.
- **RFC 5424.** `<PRI>1 TIMESTAMP HOST APP PROCID MSGID SD [MSG]`, with `-`
  for a missing value. Structured data is `[id name="value" ...]` elements,
  with `\"`, `\\` and `\]` escaped in a value; a message may start with a
  byte-order mark, which is dropped.
- **RFC 3164.** Without the `1 ` version: `Mmm dd hh:mm:ss HOST TAG[PID]: MSG`.
- **Rejects.** No `<`, a priority that is not 0 to 191 (or has a leading
  zero), a missing field, a timestamp of the wrong shape, malformed
  structured data, a BSD message with no tag, or text that is not UTF-8.

**Live run.** Four datagrams (an RFC 5424 message with escaped structured
data and a byte-order mark, a BSD message, a priority of 192, a message
missing its fields), then three TCP connections: three octet-counted frames
in one write and a fourth split across two writes; three newline-ended
lines, one with a parameter whose value has no quotes and the last with no
newline; and an octet-counted frame followed by a length of 99999999. The
collector ends when the four datagrams and three connections are done, then
prints its records sorted by host, app, message ID, process ID and message,
the rejects sorted, a count per severity, and the frames kept per
transport.

Oracle: `syslog_collector.py`, the same collector with `socket` and threads,
and the same senders.

## Porting notes

`syslog_collector.orig.kara` is the Rust-shaped first draft;
`syslog_collector.kara` is what `karac fix` made of it, with no hand edits.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!`, `vec![..]`, `move ||`,
`.as_bytes()`, `usize` lengths, `[T; N]` arrays, `&b` patterns, `!`/`||`/`&&`
as `not`/`or`/`and`, `.parse()`, `'\u{feff}'`, the `mut` a Rust `write_all`
needs and Kāra's does not, and a redundant `.to_string()` on an `Err`
string.

Fixed in kara while writing it:
- `MONTHS.contains(&&rest[..3])`: an argument's `&&` lexes as one token and
  was a parse error with no fix that hid the rest of the file behind a
  top-level-statements error; it is now dropped like a single `&`
  (B-2026-11-11-70).
- `format!(..).into_bytes()` had no hint; it is now fixed to
  `.bytes().to_vec()` (B-2026-11-11-71).
