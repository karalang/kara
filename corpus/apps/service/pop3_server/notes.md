# service/pop3_server

A POP3 server over loopback TCP: RFC 1939 with CAPA and the response codes
of RFC 2449. USER/PASS with a lockout after three failed passwords, STAT,
LIST and UIDL (one message or all), RETR and TOP with dot byte-stuffing,
DELE, RSET, NOOP and QUIT, with `-ERR` for bad sequence, bad arguments,
over-long lines and deleted messages. Each connection is a task reading
CRLF lines through a `BufReader`. The maildrops and the per-command counts
sit in one `Mutex` in a `sync struct`; a session holds its maildrop's lock
from PASS until QUIT or hang-up (a second login to the same maildrop gets
`-ERR [IN-USE]`), and only QUIT removes the messages it marked deleted.
Scripted clients on the main task run six sessions in turn, decode each
multi-line answer, and check a retrieved message's size against what the
server claimed. The server then prints its command counts and what is left
in each maildrop.

The mirror `pop3_server.py` is the same server and clients written
independently with `socket` and `threading`; it wrote `expected.out`.

Written 2026-11-13 by the real-world apps thread; karac at 5ecc16ddf9.

## From pop3_server.orig.kara to pop3_server.kara

The draft was written Rust-shaped (`::` paths, `&str`, `Vec<T>`,
`format!`, `&` on arguments, turbofish). `karac fix` rewrote all of it in
four passes (252, 6, 46 and 39 edits), including the parameter modes
(`mut ref`, `own`) and one `body.clone()` where the draft moved a borrowed
parameter. After that, `karac check` reported two warnings and no errors,
and both were the author's to answer:

- W0282: `struct Server` shadows the prelude's `Server`; renamed
  `MailServer`.
- W0278: `c.read_line();` discards an `Option` (the greeting); written
  `let _ = c.read_line();`.

No compiler gap was found: the first run matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
