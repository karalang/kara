lines: 399
build: ok
interp: same (stdout byte-identical to the built binary; both backends also print an "Error return trace: source.orig.kara:247:9" block on stderr from the expected failing second delete, which is handled and does not affect stdout)
mirror: agrees
stmt-par: none (no waiting statements: no file I/O or sleep_ms in this program)
shared types: none (the tree is an arena `Vec[Node]` with `Option[i64]` parent links and `Vec[i64]` child lists, the "central store + id handles" pattern the spec offers as the alternative to `shared struct` + `weak`)
workarounds:
- L268 `let node = ref self.nodes[id];` was originally `let node = self.nodes[id];`. The build failed with error[typecheck] E_INDEX_MOVE_NON_COPY ("cannot move out of an index expression: `v[i]` evaluates to `ref T` ... Borrow it (`ref v[i]`)"). This is a typecheck error that stops the build rather than an ownership diagnostic, so I applied the suggested `ref` borrow. It is arguably not a compiler bug, since the spec's index-operator section says this binding is rejected for non-Copy elements.
ignored diagnostics: 2 (`karac check` prints error[ownership] at L106 `let siblings = self.nodes[p].children;` (move out of a collection element) and at L324 (`path` used after being moved into `find_by_path`); the build itself printed only the L324 one, as a warning)
