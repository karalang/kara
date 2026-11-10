# tcp_balancer

A layer-4 load balancer in the shape of HAProxy's TCP mode, on loopback.
Clients connect to the front (`127.0.0.1:47756`); each connection goes to a
healthy backend (`47757` alpha, `47758` beta, `47759` gamma), and the
balancer copies bytes both ways until each side has finished, passing a
half-close through (`shutdown(Write)` on the other side once one direction
ends). One direction runs as a task, the other on the connection's own task.

- **Health checks.** Each round sends `PING` to every backend with a read
  timeout and wants `PONG` back. A backend comes up after `RISE` (2) good
  checks in a row and goes down after `FALL` (2) bad ones: a refused
  connect, a timeout, a close without an answer, or any other answer
  (`FAIL`). Each change is an event in the balancer's log.
- **Picking.** Round-robin over the backends that are up, or the one with
  the fewest active connections (ties to the earliest). The pool is one
  `Mutex` in a `sync struct`; the count of open proxied connections is an
  `Atomic`.
- **No backend up.** The client gets `503 no backend available` and a
  lingering close: the balancer writes, shuts its write side, and drains
  what the client sent (with a timeout) before closing, so the client
  reads the answer rather than a reset. A backend that refuses the
  connect gets the same treatment with a 502.
- **Backends** answer `PING` with `PONG` (or `FAIL` while the program
  marks them sick) and any other line with `<name> <LINE in capitals>`.

**Self-test.** The health logic runs first against a fake `Network`
provider whose connect, write and read follow a script per check: gamma
refuses its first three connections, beta answers `FAIL` twice, delta
never answers. Five rounds print each backend's state and streak. Then
picking: round-robin over three, least connections from 3/1/1 active, and
none when all are down.

**Live run.** alpha and beta come up (gamma is not running), four clients
are served round-robin, beta turns sick and is taken out, gamma starts and
comes up, then a least-connections round with one client held open on
alpha sends the next two to gamma. With every backend sick, a client gets
the 503. The run ends with each backend's connection and byte totals.

Oracle: `tcp_balancer.py`, the same balancer with `socket` and threads,
and a fake network following the same script. `expected.out` is its
output.

## Edits from tcp_balancer.orig.kara to tcp_balancer.kara

By `karac fix`, from machine-applicable diagnostics: Rust's `&`/`&mut`
borrows, `<..>` generics, `!`, `&&`, `.to_string()` on literals,
`format!` with plain arguments, `vec![..]`, `move ||`, `copied()` and
`&self`/`&mut self` receivers.

By hand:
- `format!("{}={}{}", b.name, if b.up {..} else {..}, if b.streak > 0
  { format!(..) } else {..})` in `show_pool` gets no machine fix. The
  rewrite to an f-string first refused any argument holding braces
  (B-2026-11-10-17, fixed by 3be914cd0e); it now takes an `if`, but not one
  holding a nested `format!`, since a nested f-string is deprecated
  (design.md §3). The `.kara` writes the closure as a block that binds
  `streak` first.
- `std.mem.take(&mut g.value.events)` is `mem.take(mut g.value.events)`:
  a path starting at `std` does not resolve in an expression, with no help
  (B-2026-11-10-16).

Found while writing it:
- `serve_backend` reads its connection through
  `BufReader.new(conn.try_clone().unwrap()).lines()`. A `BufReader` first
  wrapped only a `File` or a child pipe: the interpreter failed with
  "BufReader[TcpStream].new of a closed file" and a compiled build freed
  the socket as a file (B-2026-11-10-14, fixed by e1523f1438). It now reads a
  connection, suspending a task while it waits.
- The interpreter's wait for a socket read peeked the socket for
  readiness and dropped the peek's error, so a reset connection read as an
  end of stream under `__mir-run` while a compiled build and Python report
  `os error 104` (B-2026-11-10-15, fixed by 1ca1f71e30). The app's
  lingering close keeps resets out of its own output.
