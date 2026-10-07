lines: 397
build: ok
interp: same
mirror: agrees
stmt-par: L332-L334 three fs.write calls (one feed file each); L336-L338 three fs.read_to_string calls (one feed file each)
shared types: none
workarounds:
- Map.entry(k).or_insert(v).method(...) where the method takes `mut ref self` (WindowStats.add, SensorLedger.record): builds, but under `karac build` the update is lost (counts stay 0, later a division-by-zero panic in round_div). `--interp` is correct. Minimal probe: `m.entry("a").or_insert(Acc{..}).add(5)` twice prints `0 0` built vs `2 12` interpreted. Replaced with remove-then-insert (`bump` helper and the ledger update in `Pipeline.ingest`).
- Before that, the first spelling `ledger = ...entry(..).or_insert(..); ledger.on_time += 1;` failed the build: `codegen: cannot resolve field 'on_time' on this receiver (its type was not recorded for codegen); this is a compiler gap`. Moved the tally into a `SensorLedger.record` method (which then hit the miscompile above).
- Passing a `Map` struct field to a `ref Map[..]` parameter (`print_windows("tumbling", pipeline.tumbling, ..)`): builds, but the built binary afterwards reads garbage from the field (`tumbling.len()` printed 3539864629464342560) and segfaults at exit; a `Vec` field or a local `Map` is fine, and so is `--interp`. Worked around by destructuring the finished pipeline (`let Pipeline { config, high_water: _, tumbling, sliding, ledgers } = pipeline;`) and passing locals.
- `let st = ref windows[k];` (as the E_INDEX_MOVE_NON_COPY diagnostic for `let st = windows[k];` suggests) fails codegen: "a `ref` binding over a `Map` element is not supported". Used `match windows.get(k) { Some(st) => .. }` instead (same for the ledger map). `let ev = ref s[i];` over a Vec element was accepted.
- Not workarounds, ordinary fixes: `||` -> `or`; `*h` on `Map.get` result rejected (it yields `i64`, not `ref i64`, contrary to the spec's `Option[ref V]`); call-site `mut` marker needed on `bump(mut self.tumbling, ..)`; renamed `Stats` -> `WindowStats` after a prelude-shadow warning.
ignored diagnostics: 0
