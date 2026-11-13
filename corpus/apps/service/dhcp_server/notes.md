# dhcp_server

A DHCP server (RFC 2131, with the options of RFC 2132) on loopback UDP
(`127.0.0.1:47769`). One server task reads each datagram in turn. It answers
from a pool of five addresses, `10.0.0.100` to `10.0.0.104`. The log, the
counts and the lease table are behind one `Mutex` in a `sync struct`.

- **Checks, in order.** A datagram is dropped without a reply if it is
  shorter than 240 bytes, its `op` is not BOOTREQUEST, it is not Ethernet
  (`htype` 1, `hlen` 6), or its magic cookie is not `99.130.83.99`. It is
  also dropped if an option runs past the end, if there is no end option,
  or if there is no message type (option 53).
- **DISCOVER.** The server drops the client's earlier offer. It then offers,
  in this order:
  - the address the client already holds;
  - the free pool address it asks for (option 50);
  - the lowest free address.

  If the pool is full, there is no answer.
- **REQUEST.** A REQUEST that names another server (option 54) gives up this
  server's offer and gets no answer. One that names this server, or names
  none (renewing or rebooting), gets an ACK only if the address it asks for
  is the client's: offered to it, or already bound to it. Otherwise it gets
  a NAK with a message (option 56).
- **RELEASE, DECLINE, INFORM.** RELEASE frees the client's own lease.
  DECLINE marks the address in use and keeps it out of the pool. Neither
  gets an answer. INFORM gets an ACK with configuration only: no address
  and no lease.
- **Answers.** Every answer carries:
  - the server identifier;
  - when it grants a lease, the lease time and the renewal (T1) and
    rebinding (T2) times;
  - of mask, router and DNS, those the client listed in its parameter
    request list (option 55), or all three if it sent none.

**Live run.** Scripted clients on the main task, each with its own socket,
MAC address and transaction ID:
- a full DISCOVER, OFFER, REQUEST, ACK exchange;
- a client that asks for an address, then picks another server, then
  rediscovers with a parameter list;
- a renewal by `ciaddr`;
- a reboot request for an address the client never held;
- a REQUEST for an address other than the one offered;
- a DECLINE;
- a RELEASE by the holder and by another client;
- three DISCOVERs that use up the pool;
- an INFORM;
- five broken datagrams.

Each client decodes the answer (kind, `yiaddr`, `ciaddr`, options) or prints
that none came within 300 ms. The server handles 25 datagrams. It then
prints its log, the counts and the lease table.

Oracle: `dhcp_server.py`, the same server and clients written with `socket`
and threads.

## Porting notes

`dhcp_server.orig.kara` is the Rust-shaped first draft. `dhcp_server.kara`
is what `karac fix` made of it, with no hand edits.

`karac fix` applied these machine-applicable diagnostics:
- `&`/`&mut` borrows, `<..>` generics, `::` paths and `[T; N]` types;
- `println!`/`format!`, `vec![..]`, and `.collect::<Vec<_>>()`;
- `usize` indexes, `!`/`||`/`&&` as `not`/`or`/`and`, and `&want`
  matches as `ref want`.

Fixed in kara while writing it:
- `pkt[4..8].copy_from_slice(xid)`, Rust's slice copy, was "no method
  'copy_from_slice' on type 'Slice'" with no fix. It also set off a
  borrow error at every later use of the buffer. `karac fix` now writes
  the range assignment `pkt[4..8] = xid` (B-2026-11-13-15).
- The cookie check `pkt[236..240] != COOKIE`, a `Slice` against an
  `Array`, was never equal in compiled code, so every datagram was dropped
  as "bad cookie" there. The interpreter was right (B-2026-11-13-16).
  The other order, `COOKIE != pkt[236..240]`, used to be refused with an
  `.as_slice()` fix; both orders are accepted now (B-2026-11-13-17).

`Request` and `Server` shadow prelude types of the same names. `check`
warns, and the program means its own types.
