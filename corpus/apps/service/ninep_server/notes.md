# service/ninep_server

A 9P2000 file server (the Plan 9 file protocol, also v9fs in Linux and the
protocol under WSL's and QEMU's file sharing) over TCP on loopback, with an
in-memory tree, and the clients that use it. It is a binary protocol:
every message is little-endian `size[4] type[1] tag[2]` and a body, read
with `read_exact` and built with `to_le_bytes` / `from_le_bytes`.

- **The server.** Each connection is a session with its own fids, and the
  tree is shared by every session under one `Mutex`. It negotiates the
  message size (at most 8192) and the version (`9P2000.u` is answered with
  `9P2000`), refuses anything before `Tversion`, and takes attach, walk
  (partial walks, `..`, a clone with no names), open (permission bits for
  owner and other, `OTRUNC`), create (files and `DMDIR` directories),
  read (file bytes, and directories as whole stat entries whose offset must
  continue the previous read), write (a write past the end pads with
  zeros), clunk, remove (which clunks the fid whatever happens), stat,
  flush and auth (which it does not need). Every request ticks a logical
  clock that stamps atime and mtime; a change bumps the qid's version and
  the muid. Writing `halt` to `/ctl` stops the server.
- **The clients.** Alice attaches before negotiating, sends the hex of her
  `Tversion`, asks for a tree that does not exist, walks, reads a file and
  pages through the root directory (including a bad offset and a count too
  small for one entry), creates and writes `/tmp/notes`. Bob negotiates
  `9P2000.u`, is refused writing `/etc/motd` and `/ctl`, truncates and
  rewrites Alice's notes with a hole, makes `/tmp/box` and is refused
  removing `/readme`. Alice reads the hole back, pipelines three `Tstat`s,
  sends a type that does not exist, `Tauth` and `Tflush`, and removes what
  she can. The admin halts the server. The server's log, each connection's
  message count and the final tree print last.

The mirror `ninep_server.py` is the same server and clients written
independently with `socket`, `struct` and `threading`; it wrote
`expected.out`.

Written 2026-11-15 by the real-world apps thread; karac at 3e72c72f48,
which carries the fixes below. Re-mended from the draft at 5fa96a69c9,
after B-2026-11-15-77 landed, with no hand edits.

## From ninep_server.orig.kara to ninep_server.kara

The draft was written Rust-shaped (`use` and `::` paths, `&str`, `&mut`,
`vec!`, `format!`, `write!`, `#[derive]`, `impl fmt::Display` with a
`fn fmt(&self, f: &mut fmt::Formatter)`, `impl From<&str>`,
`Err("..".into())`, `&mut guard` for a lock's value, `let (sock, _) =
listener.accept()`, `mutex.into_inner().unwrap()`, `R<'a>` with a
`&'a [u8]` field, `b"..."` literals). Seven gaps turned up, and each was
fixed in the compiler before the fix passes were rerun:

- B-2026-11-15-70: `t += &format!(..)` was fixed to `t += ref f".."`,
  which is refused (Kāra has no borrow of a temporary). The fix now drops
  the `&` of a call's result or a string.
- B-2026-11-15-71: `impl std::fmt::Display for NineError` stopped the
  parse with `Expected {, found ::`, no hint and no fix. The trait's path is
  now fixed to `Display`, and a `fmt` whose body returns `write!(f, ..)` is
  rewritten to `fn to_string(self) -> String` (Debug: `fmt_debug`).
- B-2026-11-15-72: `Err("unknown fid".into())` found no target type, and
  `"..".into()` with `impl From[Str]` looked for `From[String]`. The `Err`,
  `Ok` or `Some` slot now chooses the conversion, and a string literal takes
  an `impl From[Str]`.
- B-2026-11-15-73: `sess.handle(&mut guard, ..)` and
  `srv.lock().counts` were refused with no fix. Both are now fixed to the
  guard's `value` field.
- B-2026-11-15-74: `let (sock, _) = listener.accept().unwrap()` was
  refused as a tuple pattern with no fix. It is now fixed to `let sock`.
- B-2026-11-15-75: `Mutex` had no `into_inner`; it now has one, which both
  backends lower for any `T` (the LLVM backend crashed on a struct), and
  `.unwrap()` after `lock()` or `into_inner()` is fixed away (no
  poisoning).
- B-2026-11-15-76: `self.take(2)?.try_into().unwrap()` for
  `u16.from_le_bytes` was refused as "no method 'try_into' on type 'Slice'".
  The diagnostic now says what to write. Kāra had no slice-to-array
  conversion (B-2026-11-15-77); since 3751c0d9be a slice has
  `try_into_array()`, and the fix writes it.

After those, `karac fix` rewrote the draft over five passes to a program
that checks clean, with no hand edits: `self.take(2)?.try_into().unwrap()`
(and the `u32` and `u64` readers) became
`self.take(2)?.try_into_array().unwrap()`.

`struct Server` keeps its name, so `karac check` warns that it shadows the
prelude's HTTP `Server`, which the warning says is legal.

The first run after that matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
