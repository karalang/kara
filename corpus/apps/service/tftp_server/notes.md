# tftp_server

A TFTP server (RFC 1350) on loopback UDP (`127.0.0.1:47765`). The server
reads requests on its well-known port. Each read (RRQ) or write (WRQ) runs
in a task with a UDP socket of its own, whose port is the transfer ID, and
moves the file in 512-byte blocks, each acknowledged before the next; a
block shorter than 512 bytes, empty included, ends the transfer. The files
live in memory behind one `Mutex` in a `sync struct`.

- **Read.** `ERROR 1` for a missing file. A block whose `ACK` does not come
  within 500 ms is sent again, up to three tries; an older `ACK` is
  ignored. `netascii` sends each line end as CR LF; the mode is matched
  without regard to case.
- **Write.** `ERROR 6` for an existing file, else `ACK 0`, then an `ACK`
  for each block. A block sent twice is acknowledged again and kept once.
  A `netascii` upload's CR LF become line ends.
- **Strangers and errors.** A datagram to a transfer socket from any other
  port gets `ERROR 5` and the transfer carries on. On the well-known port,
  an opcode other than RRQ/WRQ gets `ERROR 4`, a request without its two
  NUL-terminated strings `ERROR 4`, and a mode other than `octet` or
  `netascii` `ERROR 0`.

**Live run.** Ten scripted clients on the main task, each step reading
exactly the datagrams the server owes it (a 2 s read timeout turns a
missing one into a line in the output instead of a hang): a one-block read,
a three-block read whose second `ACK` is held back until the block comes
again, a 1024-byte file that ends with an empty block, a missing file, a
`netascii` read, a two-block write with a duplicate block and a stranger
writing to its transfer ID, a write over an existing file, an unknown mode,
a stray `ACK` to the server's port, and the written file read back. The
server handles ten requests and ends when their tasks do, then prints how
each went and the files it holds.

Oracle: `tftp_server.py`, the same server with `socket` and threads, and
the same scripted clients.

## Porting notes

`tftp_server.orig.kara` is the Rust-shaped first draft; `tftp_server.kara`
is what `karac fix` made of it plus the hand edit below.

By `karac fix`, from machine-applicable diagnostics: `&`/`&mut` borrows,
`<..>` generics, `::` paths, `println!`/`format!`, `vec![..]`, `move ||`,
`.as_bytes()`, `usize` lengths, `&b` patterns in `for` and closures,
`!`/`||`/`&&` as `not`/`or`/`and`, and the `.into_iter()` an adaptor on a
returned `Vec` needs.

Fixed in kara while writing it:
- `pkt[2..].split(|&b| b == 0).collect()` was fixed with `.iter()`, which
  lends from the `Vec` that `split` returns and the statement drops, so the
  fixed program was refused with E0516; a call's result now gets
  `.into_iter()` (B-2026-11-11-46).

By hand, around an open row:
- `String::from_utf8_lossy(..)` in `show`: no such function
  (B-2026-11-11-24); `String.from_utf8(..).unwrap_or(String.new())`.

`Server` shadows a prelude type of the same name; check warns, and the
program means its own. Check also warns at each discarded `Option` from
`recv`, which the script reads only for the line it prints.
