lines: 353

build: ok
interp: same (karac run --interp output is byte-identical to ./prog)
mirror: agrees (python3 lru_cache.py output is byte-identical to ./prog)

stmt-par:
- L344-L345 two fs.write calls (session.script, query.script)
- L347-L348 two fs.read_to_string calls (session.script, query.script)

shared types:
- `shared struct Node` (L6-L11): the doubly linked list nodes. Each node is reached from the key index (Map[String, Node]) and from its neighbours' prev/next links, plus the cache's head/tail, so it needs reference semantics; the spec names linked lists as the use case for `shared struct`. prev/next are `mut Option[Node]` (strong both ways; links are cleared on unlink).

workarounds:
- L242, L244, L250: indexing a Vec[String] by value (`words[0]`, `words[i]`) is a hard typecheck error that stops the build: `error[E_INDEX_MOVE_NON_COPY]: cannot move out of an index expression: v[i] evaluates to ref T, and this element type is not Copy`. Added `.clone()` at those three sites as the diagnostic suggests. This is a language rule rather than a compiler bug, but it blocked the build so it was the only clone added.
- (Not a workaround, just newcomer syntax fixes: `!x` -> `not x` and `||` -> `or`, from parse errors.)

ignored diagnostics: 11 (`karac check`: 6 error[borrow_projection_copy] + 5 error[ownership]; the build still succeeds. Also 2 warning[prelude_shadow] for `Command` and `Stats`, plus perf[rc-fallback] notes, left as-is.)
