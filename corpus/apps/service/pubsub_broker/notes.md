lines: 400
build: ok
interp: same
mirror: agrees
stmt-par: none
shared types: none
workarounds:
- L255-L265 (`Broker.publish`): the natural code built `msg` first and then ran `for (i, c) in self.clients.iter().enumerate() { if c.wants(msg.topic) { ... } }`. That compiled, but under `karac build` the binary aborted with `free(): double free detected in tcache 2` and printed no stdout. `karac run --interp` was correct. Minimal repro: a METHOD call that passes a String field projection to a `ref String` parameter inside a `for` loop (`for i in 0..1 { if s.wants(msg.topic) { ... } }` where `fn wants(ref self, topic: ref String) -> bool`) frees `msg.topic`. After the loop, `f"{msg.topic}"` reads freed memory and the drop double-frees. The same call outside a loop works, and so does a free function inside a loop. Workaround: match against the plain `topic` local before moving it into `Message`.
- L335-L376 (`main`): the script was a `"""..."""` multi-line literal split on "\n". `karac build` failed with `codegen: no handler for expression kind MultiStringLit; this is a codegen bug`. `--interp` handled it. It is now a `Vec` of string literals iterated with `.iter()`.
- L154 (`Subscriber.deliver`): `let mut evicted = None;`, later assigned from `self.mailbox.pop_front()`, failed with `error[typecheck]: cannot infer type parameter 'T'; add a type annotation`. Fixed by annotating it as `Option[Message]`.
- (language rule, not a compiler bug) A field named `seq` is a parse error because `seq` is a reserved keyword (`seq { ... }` expression form). Renamed it to `id`.
ignored diagnostics: 1 (`error[ownership]` at L240: 'pattern' moved inside a loop, from the `position(|p| p == pattern)` closure in `unsubscribe`). The build also printed a `perf[rc-fallback]` note on the same spot and a `warning[prelude_shadow]` for `enum Command`. Neither is counted as `error[`.
