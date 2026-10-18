# rev_proxy: a reverse proxy over real sockets

A reverse proxy on `127.0.0.1:47737` in front of origin servers on 47738 and
47739. Two more upstreams, on 47740 and 47741, are in the pools but nothing
ever listens on them. The proxy:

- picks a pool by the longest matching path prefix (`/api/admin/` beats
  `/api/`), and strips the prefix when the route asks for it;
- balances each pool round-robin, with a passive health check: an upstream
  that refuses a connection sits out the next three requests through its pool;
- retries a GET that gets a 503 on the next upstream, and never retries a POST;
- strips hop-by-hop headers in both directions, including any header the
  `Connection` header names, and adds `X-Forwarded-For`, `X-Forwarded-Prefix`,
  `Via` and `X-Upstream`;
- decodes a chunked origin reply and re-sends it with a `Content-Length`;
- answers 400 (malformed), 404 (no route), 413 (body over 64 bytes) and 502
  (no healthy upstream) itself.

Halfway through, origin b2 closes its listener, and the health check routes
around it. The run ends with the access log and per-upstream counters.

Everything runs in one thread. The client writes a request and shuts down its
write side, the proxy accepts and reads it, connects to the chosen origin and
writes the rewritten request, the origin accepts and answers, and the replies
flow back. The kernel buffers each step.

Oracle: `rev_proxy.py`, the same program with Python's `socket` module.
`expected.out` is its output.

## Compiler gaps (filed in kara `docs/bug-ledger.jsonl`)

- **B-2026-10-15-13:** `match place { mut ref p => .. }` passes `karac check`
  but every MIR surface refuses it with the legacy backend's error.
- **B-2026-10-15-14:** check refuses `let p: mut ref T = w.field` (it wants
  `let mut p`, then calls the place moved), and refuses the element form with
  E_INDEX_MOVE_NON_COPY. There is no `mut ref place` expression either. So
  there is no way to bind a mutable borrow of one upstream in a local. The app
  passes `mut proxy.pools[pi]` to a helper and indexes `pool.members[idx]` at
  every write.
- Known and worked around: B-2026-10-14-44 (socket addresses are `String`),
  B-2026-10-12-6 (`parse` returns `Option`), B-2026-10-12-7 (`split` is an
  eager `Vec[String]`).

## Author errors

- `let pool = mut ref proxy.pools[pi];` (the Rust shape) is a parse error.
- A struct named `Pool` shadows the prelude's (a warning); renamed `Cluster`.

`rev_proxy.orig.kara` is the first draft.

## Library migration, 2026-10-18

Thread A's a0ca700228 and acd593fc77 changed the library to match docs/library: a number's `parse` returns `Result[T, ParseError]`, and `split`, `lines`, `split_whitespace` and `trim` give `Str` views (`split` and `lines` are now lazy iterators). Arguments forwarded to a `mut ref` parameter are now marked too. `karac fix` made the edits. The app matches expected.out on all four MIR backends again (karac at a4b0967495).
