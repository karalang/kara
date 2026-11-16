# service/modbus_server

A Modbus/TCP server (the industrial register protocol PLCs, meters and
drives speak) on loopback TCP, and the clients that use it. A frame is the
7-byte MBAP header (transaction id, protocol id, length, unit id, all
big-endian) and a PDU whose first byte is the function code; an error is
the function code with its high bit set and an exception code.

- **The server.** Each connection is served by its own task, and the data
  model (64 coils, 32 discrete inputs, 32 holding and 16 input registers)
  is shared under one lock. It answers read coils / discrete inputs /
  holding / input registers (01 to 04), write single coil and register
  (05, 06), write multiple coils and registers (0F, 10), mask write
  (16), read/write multiple registers (17) and read device identification
  (2B/0E, basic stream and single object). It checks quantities and
  address ranges as the specification does, answers an unknown function
  with exception 01, a unit it does not serve with 0B (gateway target
  failed to respond) and the broadcast unit 255 as its own, skips a frame
  with a foreign protocol id, and closes a connection whose header's length
  is under 2 or over 254 or that ends in the middle of a frame. Writing ON to
  the last coil stops it.
- **The clients.** Each sends hand-built frames and prints the
  transaction id, the unit, and the reply decoded: bits, registers, echoes,
  device objects or the exception. A PLC client reads and writes every
  table; an edge client walks the boundaries (zero and oversized
  quantities, the last address, short PDUs, unknown functions, other
  units); a pipeline client sends three requests in one write and a frame
  with a foreign protocol id; broken clients send a 300-byte length, half
  a frame and three bytes, each closing their write half; and the admin
  halts the server. The server's log and its counts print last.

The mirror `modbus_server.py` is the same server and clients written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-16 by the real-world apps thread; karac carries the fixes
below.

## From modbus_server.orig.kara to modbus_server.kara

The draft was written Rust-shaped (`use` and `::` paths, `&[u8]`,
`&mut`, `vec!`, `format!`, `struct Exception(u8)`, the `Read`/`Write`
trait imports, `*counts.entry(k).or_insert(0) += 1`,
`group.spawn(move || ..)`, `lock().unwrap()`, `sock.shutdown(Shutdown::Write)`, `&[&[5], &be16(n)]`
with `parts.concat()`). Three gaps turned up, and each was fixed in the
compiler before the fix passes were rerun:

- B-2026-11-16-170: `use std::io::{Read, Write};` mended to an import of
  names `std.io` does not have, and `Shutdown` in
  `use std::net::{TcpListener, TcpStream, Shutdown};` likewise, with no
  fix; then `shutdown(Shutdown.Write)` was an undefined name. The io line
  is now deleted (Kāra's `read` and `write_all` are methods of the
  handle), `Shutdown` leaves its group, and the call becomes
  `shutdown_write()`.
- B-2026-11-16-171: `pdu([[5], be16(5), be16(0xFF00)])`, arrays passed as
  a `Slice[Slice[u8]]`, passed `karac check` and stopped at "invalid MIR:
  array element of the wrong type". Each element is now viewed as a call
  argument is.
- B-2026-11-16-172: `parts.concat()` over a `Slice[Slice[u8]]` took the
  deprecated `Display` join, whose fix wrote `join("")`, which does not
  check (`Slice[u8]` is not `Display`). `concat()` now flattens a
  sequence of slices as it does one of `Vec`s, as Rust's does.

After those, `karac fix` rewrote the draft in one pass (478 fixes), with
no hand edits. `struct Server` keeps its name, so `karac check` warns that
it shadows the prelude's HTTP `Server`, which the warning says is legal.

The first run after that matched the mirror byte for byte, on all four
MIR backends at `KARAC_THREADS=1` and `4`.
