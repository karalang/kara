# irc_lite

An IRC-style chat server, a subset of RFC 1459/2812, on loopback
(`127.0.0.1:47760`). Each connection is a task that reads CRLF lines through
a `BufReader` over its `TcpStream`; the server state (clients, nicknames,
channels, command counts) is one `Mutex` in a `sync struct`, and every line
a command produces is written to its recipients' sockets under that lock,
so each client sees events in the order they happened.

- **Lines.** An optional `:prefix`, a command (case-insensitive) and up to
  15 parameters, the last of which may be a `:trailing` one holding spaces.
  A line over 510 bytes gets `417`.
- **Registration.** `NICK` and `USER`, in either order, then `001`. Nicks
  are a letter then letters, digits or `-_[]\`^{}|`, at most 9, unique
  without regard to case (`432`, `433`). Anything else first gets `451`;
  a second `USER` gets `462`.
- **Channels.** `JOIN #a,#b` (the creator is the operator, shown `@`),
  with the topic (`332`) and the names (`353`, `366`); `PART` with an
  optional reason; `TOPIC` to read or (operators only, `482`) set;
  `NAMES`. An empty channel goes away.
- **Messages.** `PRIVMSG` to a channel (every member but the sender; `404`
  from outside it) or to a nick (`401` for none); `411`/`412` for a missing
  target or text. A nick change is told to everyone sharing a channel.
- **Leaving.** `QUIT` answers `ERROR :Closing link (..)` and closes; a
  dropped connection is a quit with `Connection closed`. Both are told to
  the client's channel peers. `PING` gets `PONG`; any other command `421`.

**Self-test.** Ten lines through the parser (prefix, trailing with spaces,
an empty trailing, runs of spaces, no command, more than 15 parameters) and
seven nicks through the validity check.

**Live run.** Three scripted clients on the main task, each step reading
exactly the lines the server owes it (a 2 s read timeout turns a missing
line into an error in the output instead of a hang): registration and its
errors, two channels, a topic and an operator check, channel and private
messages and their errors, an over-long line, a nick change, a part, a
dropped connection, two quits. The server accepts three connections and
ends when their tasks do, then prints its command counts.

Oracle: `irc_lite.py`, the same server with `socket` and threads, and the
same scripted clients.

## Porting notes

`irc_lite.orig.kara` is the Rust-shaped first draft; `irc_lite.kara` is
what `karac fix` made of it plus the hand edits below.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!`, `vec![..]`, `move ||`,
`.as_bytes()`, `copied()`, `&self`/`&mut self` receivers, and `own` on
`serve`'s connection parameter. `unwrap_or_default()` is Kāra's too.

By hand:
- `let params: String = m.params.iter().map(|x| f"[{x}]").collect();` in
  `show` is Rust's `FromIterator<String> for String`. A Kāra `String`
  gathers `char`s (library/iterators.md), so the `.kara` collects a
  `Vec[String]` and joins it. The refusal now says so (B-2026-11-10-34).

Found while writing it:
- `serve(srv, id, conn: TcpStream)` hands `conn` to `BufReader.new`, as
  Rust's owned parameter does. The parameter borrows in Kāra, and the call
  got E0297 "not a `ref TcpStream`" with no fix, where any other `own`
  parameter gets the refusal and the fix that writes `own`
  (B-2026-11-10-33).

`Channel` and `Server` shadow prelude types of the same name; check warns,
and the program means its own.
