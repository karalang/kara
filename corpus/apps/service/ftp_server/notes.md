# service/ftp_server

An FTP server over loopback TCP: RFC 959 with PASV, EPSV (RFC 2428), and
SIZE and FEAT (RFC 3659, RFC 2389), over an in-memory file tree. It supports
USER/PASS, with anonymous users read-only and `dev` able to write, and PWD,
CWD, CDUP, MKD, RMD, DELE, RNFR/RNTO, TYPE A/I, SIZE, LIST, NLST, RETR,
STOR, APPE, SYST, NOOP and QUIT. It answers with the reply codes for bad
sequence (503), bad syntax (501), no data connection (425), permission and
missing paths (550), and unknown commands (502).

Each control connection is a task reading CRLF commands through a
`BufReader`. PASV and EPSV open a listener on a free port, and the next
transfer accepts its data connection there and reads it to the end. TYPE A
turns LF into CRLF on the way out and back on the way in, so a file's SIZE
in binary differs from the bytes an ASCII RETR sends.

Scripted clients on the main task run three sessions, two of them open at
once. They parse the 227 and 229 replies to connect, and mask the port when
they print it. The server then prints its tree and its command counts.

The mirror `ftp_server.py` is the same server and clients written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-13 by the real-world apps thread; karac at e65451aad4,
which carries the fixes below.

## From ftp_server.orig.kara to ftp_server.kara

The draft was written Rust-shaped. `karac fix` rewrote it in five passes
(271, 2, 80, 1 and 16 edits). Four gaps turned up, and each was fixed in
the compiler before the fix passes were rerun:

- B-2026-11-13-70: `println!("{} CRLF", text.matches("\r\n").count())`
  had no fix, because an argument whose literal held an escape, a brace or
  a colon was refused. A hole takes such a literal as written.
- B-2026-11-13-71: `addr.rsplit(':')` was offered `split`, which starts at
  the other end. The diagnostic now names `rsplit_once`.
- B-2026-11-13-72: `group.join()` on a `TaskGroup` had no hint. It is now
  repaired to `drop(group)`, which joins the group's tasks.
- B-2026-11-13-73: `TcpStream` had no `read_to_end`, the natural way to
  read a data connection. It has one now, as does `TlsStream`.

After those, `karac check` reported five diagnostics with no machine fix,
answered by hand:

- `rsplit(':').next().unwrap().parse()` became
  `i64.parse(addr.rsplit_once(":").unwrap().1)`, as the two diagnostics
  said.
- `trim_end_matches('\r')` became `strip_suffix("\r").unwrap_or(l)`.
- `n.parse().unwrap()` inside a `map` closure became `i64.parse(n)`.
- `struct Server` shadows the prelude's `Server`; renamed `FtpServer`.
- B-2026-11-13-74: `karac fix` turned `if c { ".." } else { &arg }` into
  `{ ref arg }`, which the branch with a `".."` literal refuses ("'String'
  and 'ref String'") with no fix. Written `arg.as_str()`.

The first run after that matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
