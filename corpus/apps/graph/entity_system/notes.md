lines: 398

build: ok
interp: same (stdout byte-identical to the build and to Python; both backends also print a spurious `Error return trace: source.orig.kara:95:16` to stderr at exit, for a `?` whose Err was handled by the caller's `match`. stdout is unaffected.)
mirror: agrees (188 lines, byte-identical)

stmt-par: none (the program has no waits: no file I/O and no sleep_ms)

shared types: none. Target references are generational handles (`EntityRef { index, generation }`, Copy), checked with `World.is_alive` before they are followed, so no RC sharing is needed.

workarounds:
1. L256-259, `run_movement`: `let range = self.weapons[e.index].unwrap().range;` became a `match` with `Some(w) => w.range, None => 0`. With `.unwrap()` the build aborts at runtime with `free(): double free detected in tcache 2` (exit 134); `--interp` is fine. Minimal repro: a `#[derive(Copy, Clone)]` struct with 4 i64 fields, held as `Vec[Option[Weapon]]` in a struct, read in a `ref self` method as `self.weapons[i].unwrap().range` and called 3 times. With a 2-field struct it does not crash, so it looks tied to Option payloads that are boxed (wide).
2. L179-181, `World.spawn`: `self.weapons[e.index] = t.weapon;` became `if let Some(w) = t.weapon { self.weapons[e.index] = Some(w); }`. Here `t` is a local `Template` (from `template(kind).unwrap()`) whose field is `weapon: Option[Weapon]` (the same 4-field Copy struct). In the build, assigning that field into a Vec slot by index leaves the slot pointing at freed memory once `t` drops. Later reads return garbage (`range = 7306934683317579822`, which looks like string bytes), so no unit moves or attacks, and with workaround 1 removed the program double-frees. `--interp` was correct. Minimal repro: `self.weapons.push(None); self.weapons[i] = t.weapon;` in a `mut ref self` method gives a double free, while `self.weapons.push(t.weapon)` works.
3. L294-295, `run_combat`: `for Hit { attacker, target, amount } in hits {` became `for hit in hits { let Hit { attacker, target, amount } = hit;`. The build fails with `codegen: no handler for method 'label' on variable 'attacker' in World.run_combat (method dispatch fell through; this is a codegen bug ...)`. The spec says `for` patterns support destructuring. (The rewritten line makes the build print a `warning[borrow_projection_copy]`, which I ignored.)
(Not a workaround: I first wrote `match kind.as_str()` out of Rust habit and got `no method 'as_str' on type 'String'`. The spec never mentions `as_str`, and `match kind { "knight" => ... }` works.)

ignored diagnostics: 0 (`karac check source.orig.kara` prints no `error[` lines and says "All checks passed."; `karac build` prints 1 `warning[borrow_projection_copy]` at L295)
