# service/smtp_relay

An SMTP relay (RFC 5321) and the mail exchangers it delivers to, all over
TCP on loopback. Every socket binds port 0; a lab table maps each
exchanger's documentation address (192.0.2.x) to the socket it got.

- **The relay.** It speaks ESMTP with PIPELINING, SIZE and 8BITMIME, and
  takes AUTH PLAIN (RFC 4954, base64 through `std.encoding`). A client that
  has not authenticated may mail only the relay's own mailboxes; anything
  else is `554 relay access denied`. It refuses an unknown local mailbox, a
  domain with no exchanger, a declared SIZE over the limit, and commands out
  of order. A message it accepts gets a `Received:` line; local recipients
  are delivered at once and the rest are queued.
- **The queue.** A queue run (`XRUN <t>`, which sets a logical clock) groups
  each message's due recipients by domain and tries the domain's exchangers
  in preference order: one that refuses the connection, or greets with 421,
  passes the attempt to the next. A 4xx reply defers the recipient on a
  backoff of 300, 900, 1800 and 3600 seconds, a 5xx bounces it, and a
  recipient still pending after 5400 seconds expires. Every failed recipient
  is reported in a bounce (from `MAILER-DAEMON`, with the original headers)
  delivered to a local sender's mailbox.
- **The exchangers.** `mx1.example.org` takes everything; `flaky.test`'s
  preferred exchanger is busy (421) and its backup takes the mail;
  `mx.strict.test` knows only `frank@`; `mx.slow.test` greylists its first
  two connections (451); `mx.nowhere.test` has nothing listening; and
  `mx.big.test` advertises SIZE 1000 and refuses a larger message (552).
  Dot-stuffing is applied on the way out and undone on the way in.
- **The clients.** Alice tries to relay before authenticating, fails AUTH
  three ways, then authenticates and pipelines a message to nine
  recipients, sends one too big for its exchanger, declares one too big for
  the relay, and runs the queue at t = 0, 300, 1200, 3000 and 6600. A
  stranger may still mail `carol@relay.test` but not relay. The relay log,
  the local mailboxes and each exchanger's inbox print last.

The mirror `smtp_relay.py` is the same relay, exchangers and clients written
independently with `socket`, `base64` and `threading`; it wrote
`expected.out`.

Written 2026-11-15 by the real-world apps thread; karac at 2e243f86c1,
which carries the fixes below.

## From smtp_relay.orig.kara to smtp_relay.kara

The draft was written Rust-shaped (`use` and `::` paths, `&str` and
`&'static str`, `&[(..)]` constants, `&mut`, `vec!`, `format!`,
`#[derive]`, `*guard = ..` on a lock, `std::mem::take`, `group.join()`).
Four gaps turned up, and each was fixed in the compiler before the fix
passes were rerun:

- B-2026-11-15-12: `*lab.relay.lock() = Some(relay)` and
  `take(&mut *lab.exchangers.lock())` were refused with no fix. The check now
  writes the guard's `.value`.
- B-2026-11-15-13: `line.split_once(' ').unwrap_or((&line, ""))` became
  `(ref line, "")`, which was refused against `(Str, Str)` with no hint and
  no fix. The check now writes `line.as_str()`.
- B-2026-11-15-14: `const MX: &[(&str, u16, &str, &str)] = &[..]` became a
  `Slice` constant, which the spec does not allow and the MIR builder could
  not lower at a `for` loop. The check now refuses it with a fix to
  `Array[T, N]`, and a constant lent as a place lives in a static, so a view
  of it borrows nothing, as core semantics §5.3 says.
- B-2026-11-15-15: `for (i, (_d, _p, name, ip)) in MX.iter().enumerate()`
  kept the loop's item borrowed for as long as `name` was used, so a task
  spawned with it was refused. Overwriting a local that holds a reference
  now leaves reborrows through it valid wherever the reference sits in it,
  as it already did for a reference held directly.

After those, `karac fix` rewrote the draft over several passes, and three
diagnostics with no machine fix were answered by hand, as each says:

- `let body = [..]` passed where a `Slice[Str]` is wanted: annotated
  `Vec[Str]`.
- `let all: String = lines.iter().map(..).collect()`: collected into a
  `Vec[String]` and joined.
- `stop.line();` discards an `Option` (`#[must_use]`): written
  `let _ = stop.line();`.

The first run after that matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
