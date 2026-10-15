# service/todo_api

A REST service for todo items, built the way a small web framework is: a
hand-written JSON library (recursive-descent parser with offset-positioned
errors, `\uXXXX` escapes, a depth limit; a writer that escapes control
characters), a router of function values with `:id` path parameters and
404 / 405 answers, a middleware chain of closures (bearer auth on writes, a
body-size limit), percent-decoded query filters, an access log, and HTTP/1.1
keep-alive with every request of a session pipelined on one TCP connection.
The mirror `todo_api.py` reimplements the same JSON library and wrote
`expected.out`.

Written 2026-10-14 by the real-world apps thread; karac at 84621d3858.

## Edits from source.orig.kara to source.kara

Author errors (the diagnostics were right, or the spec says so):
- `|r: ref HttpReq| -> Option[HttpResp] { .. }`: closures do not declare a
  return type.
- `fn word(mut ref self, w: Str, v: JVal) -> Result[JVal, String]` returned
  the borrowed `v`; it is `own JVal` (D5).
- `char.from_u32(code)` is Rust's name; the message pointed at
  `char.try_from`, which returns a `Result`.
- `u8.from_str_radix` returns `Option` on v2; it is not in the library spec.
- `route(.., handler: Fn(..))` stored a non-escaping parameter; the message
  said to write `escaping Fn(..)`.
- `match want_tag { Some(g) => .. }` inside a loop moved the option; the
  hint said `Some(ref g)`.
- `let h = rt.handler; h(..)` moves a function value out of a borrow, which
  design.md forbids; the spec form is `(rt.handler)(..)`. Check accepted the
  move (B-2026-10-14-62) and the MIR borrow checker refused it.
- `Json`, `Request`, `Response` and `Server` shadow prelude types (warnings);
  renamed to `JVal`, `HttpReq`, `HttpResp` and `App` in both files.

Compiler gaps, worked around and listed in the source header:
- B-2026-10-14-58: a handler that reaches a recursive allocating function
  (`write_json`) does not fit a bare `Fn(..)` slot; `allocates(Heap)` should
  be default-permitted. The slots name it.
- B-2026-10-14-59: a closure with `return None;` and a tail `Some(..)` fails
  to infer; the binding is annotated.
- B-2026-10-14-60: check refuses returning `Some(it)` for a `for` element of
  a borrowed collection; `field` delegates to an index loop.
- B-2026-10-14-61: the MIR typecheck cannot call a `ref Fn` from `.iter()`.
- B-2026-10-12-7: `head.split("\r\n").iter()` borrows an eager temporary
  Vec, which the MIR borrow checker rejects (check accepts it); the Vec is
  bound first.
- B-2026-10-12-6 and B-2026-10-14-44 as in the other apps.

PASSES on mir-interp, mir-llvm, mir-llvm-asan and mir-build.

Reverted 2026-10-15, karac at aec4d15cc0: B-2026-10-14-58 and
B-2026-10-14-44 are fixed, so the handler and middleware slots are bare
`Fn(..)` types again and `TcpStream.connect` / `TcpListener.bind` take
the `Str` address. Each of these is the original line again, and the app
still matches on all four MIR surfaces.
