lines: 396
build: ok
interp: same
mirror: agrees
stmt-par: none
shared types: none (cells are addressed by name through a Map[String, i64] index into a Vec[Cell]; observers live in a plain Vec[Observer] owned by the Sheet, so no reference-semantics type was needed)
workarounds:
- L159 and L203 (`Sheet.lookup`, `Sheet.reaches`): `Map.get` is specified as returning `Option[ref V]`, so I wrote `Some(i) => Ok(*i)` and `self.cells[*i]`. The compiler rejected both with `error[typecheck]: unary '*' requires 'ref T', 'mut ref T', or a raw pointer ('*const T' / '*mut T'), found 'i64'`. It types the payload as a plain `i64`, so I removed the `*`.
- L320-L329 (`Sheet.unsubscribe`): the spec lists `Vec.remove_first(pred) -> Option[T]`. I wrote `self.observers.remove_first(|o| o.id == id)`, and it failed with `error[typecheck]: no method 'remove_first' on type 'Vec'` (plus a follow-on `no field 'id' on type 'this type'` on the closure parameter). I replaced it with an index loop that calls `Vec.remove(i)`.
- One parse error that was my own syntax mistake, not a compiler gap: `Reaction.Keep => i += 1,` is rejected because an assignment cannot be a bare match-arm body. I changed it to `Reaction.Keep => { i += 1; }`.
ignored diagnostics: 3 (2 x `error[borrow_projection_copy]` for moving the `for d in ...deps()` element into `stack.push(d)` / `SheetError.Cycle(..., d)`, and 1 x `error[ownership]` for `name` used in the `Err` arm of `apply` after `sheet.set(name, ...)`; the build also printed a `perf[rc-fallback]` note for the same `name`)
