# service/ntp_server

An NTP server (RFC 5905, the SNTP subset of RFC 4330) on loopback UDP, and
the clients that set their clocks from it. A packet is 48 bytes: the
leap/version/mode byte, stratum, poll and precision, root delay and
dispersion, the reference id, and four 64-bit timestamps (seconds since
1900 and a 32-bit fraction), all big-endian.

- **The clocks.** Every clock is a fake `ClockProvider` installed with
  `with_provider[Clock](..)`, whose wall time moves a fixed step on each
  read, so every timestamp, offset and delay is the same on every run. The
  server's clock is the reference; each client's starts ahead of or behind
  it, and its reads are counted in an `Atomic[i64]`.
- **The server.** One task serves the socket, under a `Mutex`. It answers
  a client request (mode 3, versions 1 to 4) with a stratum-1 `GPS` reply
  that echoes the client's transmit time as its origin, and stamps its own
  receive and transmit times. A peer that asks again within 1.5 ms gets a
  kiss-o'-death `RATE` (stratum 0). It drops a short packet, version 0 or
  5, a mode other than 3, a zero transmit time and an extension that is
  under 16 bytes or not a multiple of 4, and ignores a well-formed
  extension. A mode-7 `HALT` stops it.
- **The clients.** Each prints the reply decoded, then the offset
  ((t2 - t1) + (t3 - t4)) / 2, the round-trip delay and its corrected time.
  A laptop and a meter sync (v4 and v3), an old client syncs as v1 and
  then sends a 16-byte extension, a greedy one bursts three requests,
  a broken one sends each malformed packet, and the admin halts the server.
  The server's log and its counts print last.

The mirror `ntp_server.py` is the same server and clients written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-17 by the real-world apps thread; karac carries the fixes
below.

## From ntp_server.orig.kara to ntp_server.kara

The draft was written Rust-shaped (`use` and `::` paths,
`std::sync::atomic::{AtomicI64, Ordering}` with `Ordering::SeqCst`,
`Arc<Mutex<Server>>` with `let srv = server.clone();`,
`impl ClockProvider for FakeClock`, `with_provider::<Clock, _>(..)`,
`SystemTime::now()`, `Duration::from_nanos(u64)`, `TaskGroup::new()` and
`group.spawn(move || ..)`, `lock().unwrap()`, `&[u8]`, `vec!`, `format!`).
Five gaps turned up, and each was fixed in the compiler before the fix
passes were rerun:

- B-2026-11-17-119: `with_provider::<Clock, _>(p, || ..)`, Rust's
  turbofish after a function name, stopped the parse at the `::` with no
  fix. It is now `with_provider[Clock](p, || ..)`, and on any other
  function the list is deleted.
- B-2026-11-17-120: `Atomic[i64].new(0)` and `Mutex[i64].new(x)`, the
  qualified constructors, had "no method 'new'". They now work as
  `Vec[i64].new()` does, with the pin checked.
- B-2026-11-17-121: `use std::sync::atomic::{AtomicI64, Ordering};` was an
  unknown module with no fix. The line becomes `import std.sync.{Atomic};`
  and every `AtomicI64` in the file `Atomic[i64]`; the ordering arguments
  already had their own fix.
- B-2026-11-17-122: `Arc` in `use std::sync::{Arc, Mutex};` was an
  unknown name with no fix. It leaves the import, `Arc<T>` becomes `T`,
  `Arc::new(x)` becomes `x`, and `Arc::clone(&x)` or `x.clone()` on an
  `Arc` binding becomes `ref x`: a task borrows from the scope that made
  its `TaskGroup`.
- B-2026-11-17-123: `Duration::from_nanos(x)` mended to `Duration.ns(x)`,
  and an `x: u64` (Rust's parameter type) then had a narrowing error with
  no fix. It now gets `as i64`, as no `Duration` holds more than
  `i64::MAX` nanoseconds.

After those, `karac fix` rewrote the draft in one pass (262 fixes), with
no hand edits. `struct Server` keeps its name, so `karac check` warns that
it shadows the prelude's HTTP `Server`, which the warning says is legal.

The first run after that matched the mirror byte for byte.
