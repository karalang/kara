# service/smtp_sink

An SMTP receiver over TCP: the server half of RFC 5321 for one local domain.
EHLO/HELO, MAIL FROM, RCPT TO, DATA with dot unstuffing, RSET, NOOP, VRFY and
QUIT, with the reply codes and sequence rules (503 bad sequence, 501 syntax,
550 non-local recipient, 452 too many recipients, 554 no valid recipients,
552 message too large). Each accepted message gets a queue id, Return-Path,
Received and Delivered-To headers, and is written to a maildir
(`maildir/new/Q0001.eml`). A report then lists the maildir and parses each
message back: unfolded headers, a `multipart/mixed` body split on its
boundary, and a base64 part decoded. The client pipelines three whole
sessions (delivery, errors and limits, a MIME message) and pairs each reply
with what it sent.

The mirror `smtp_sink.py` is the same state machine and parser written
independently in Python (no `email` module) and wrote `expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at 0b14473a2e.

## Edits from smtp_sink.orig.kara (source.orig.kara) to smtp_sink.kara

Author errors (the diagnostics were right, or the spec says so):
- `&&`, `||` and `!` are `and`, `or` and `not` in Kāra; the parser said so
  at every site.
- `self.phase == Phase.Connected` needs `#[derive(PartialEq)]` on `Phase`.
- `struct Server` shadows the prelude's `Server` (a warning); renamed `Smtp`.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-70: `self.env.to.len()` and `self.env.to.push(..)` are
  refused by `karac check` with the `chained_field_receiver` lint, a legacy
  codegen deferral, though every v2 surface runs them. `Envelope` got
  `count`, `add` and `list` methods.
- B-2026-10-14-43: `match verb.as_str() { "EHLO" | "HELO" => .. }` is
  refused; the verb is matched as the `String` it is.
- B-2026-10-12-7: `split`/`lines` give a `Vec[String]`, so no `.collect()`,
  `.count()` is `.len()`, `lines().next()` is `[0]`, and `ref lines[i]`
  where a `Str` element would be `Copy`.
- B-2026-10-14-44: `TcpStream.connect`, `TcpListener.bind` and
  `Command.arg` take `String`.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-44 is fixed, so
`TcpStream.connect` and `TcpListener.bind` take the `Str` address. Each
of these is the original line again, and the app still matches on all
four MIR surfaces.
