lines: 399

build: ok
interp: same
mirror: agrees

stmt-par: none (the program never waits: no file I/O and no sleep_ms)

shared types: none

workarounds:
- Fn value in an enum payload crashes codegen. The natural `RouteResult.Matched { handler: Handler, params }`, destructured in `dispatch` and called as `handler(ctx, mut self.store)`, makes `karac build` panic at src/codegen/closures.rs:2024 ("Found IntValue(... %handler54 = load i64 ...) but expected the StructValue variant", in compile_closure_call). Workaround: `Matched` carries a route `index: i64`, and the handler comes back from `Router.handler_at(index)`.
- Going straight to `self.router.routes[index].handler` instead failed with `error[chained_field_receiver]` ("chained field receivers (`a.b.c…`) are deferred to v1.x in codegen"). That is why the lookup goes through the small accessor method `handler_at`.
- A user `struct Request` with its own `header` method was silently wrong. The compiler only printed `warning[prelude_shadow]` ("built-in paths keyed on 'Request' no longer apply"), but in `karac run --interp` every `req.header(...)` returned None. Auth always answered 401, even for good tokens, and X-Request-Id was never picked up. Workaround: renamed the struct to `HttpRequest`.
- `handler(ctx, self.store)` inside a `mut ref self` method failed to typecheck: "parameter expects `mut ref UserStore`; call with fresh binding requires a `mut` marker". The spec (Part 1½) says arguments rooted at a `mut ref` binding forward without a marker. Wrote `handler(ctx, mut self.store)`.
- `for id in store.users.keys() { ids.push(*id); }` failed: "unary '*' requires 'ref T' ... found 'i64'". The spec table says `keys()` yields `ref K`. Changed it to `ids.push(id)`.
- `let Some(mut user) = store.users.remove(id) else { ... };` is a parse error ("'mut' is a reserved keyword and cannot be used as an identifier"). Rewrote it as `let mut user = match store.users.remove(id) { Some(u) => u, None => return Err(...) };`.
- These were language-rule corrections from my own reading, not workarounds: `!`/`||` became `not`/`or`, an assignment used as a match-arm body got braces, and `let user = store.users[id]` (move out of an index) became `store.users.get(id)`.

ignored diagnostics: 3
