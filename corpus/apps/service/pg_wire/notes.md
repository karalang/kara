# service/pg_wire

A PostgreSQL server over loopback TCP, speaking the frontend/backend wire
protocol version 3.0: the startup handshake with cleartext password
authentication, then the simple query protocol over one in-memory table,
`items(id int4, name text, qty int4)`.

After a StartupMessage the server asks for a password and answers with
AuthenticationOk, two ParameterStatus messages, BackendKeyData and
ReadyForQuery, or with an ErrorResponse for a wrong password (28P01) or an
unsupported protocol version (0A000). A Query may hold several statements
separated by `;`, each answered in turn: SELECT with a column list or `*`,
WHERE with a comparison, and ORDER BY (NULLs last); INSERT, UPDATE and
DELETE; and BEGIN, COMMIT and ROLLBACK over a snapshot of the table. An
error inside a transaction leaves it failed (status `E`, then 25P02) until
ROLLBACK. Errors carry their SQLSTATE: unique violation (23505), syntax
(42601), bad integer text (22P02), type mismatch (42804), unknown column
(42703), unknown table (42P01) and unknown operator (42883). An empty query
gets EmptyQueryResponse, a nested BEGIN or a COMMIT outside a transaction a
NoticeResponse, and a message other than Query or Terminate a protocol
error (08P01).

Each connection is a task reading length-prefixed messages through a
`BufReader` with `read_exact`. Scripted clients on the main task run four
sessions, the last two open at once, and print every message they send and
receive. The server then prints its query count and the final table.

The mirror `pg_wire.py` is the same server and clients written
independently with `socket`, `struct` and `threading`; it wrote
`expected.out`.

Written 2026-11-13 by the real-world apps thread; karac at 9bb4bd1caf.

## From pg_wire.orig.kara to pg_wire.kara

The draft was written Rust-shaped (`::` paths, `&` borrows, `vec!`,
`format!`, `b'X'` bytes, `i32::from_be_bytes`, `split_whitespace`,
`Ordering`). `karac fix` rewrote it in four passes (475, 4, 38 and 26
edits), after which `karac check` was clean. There were no hand edits, and
the first run matched the mirror byte for byte.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build, at
`KARAC_THREADS=1` and `4`.
