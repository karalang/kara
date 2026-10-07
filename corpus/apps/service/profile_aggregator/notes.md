lines: 399

build: ok
interp: same
mirror: agrees

stmt-par: L257-L259 three backend fetches (`accounts.fetch` / `orders.fetch` / `recs.fetch`, each sleep_ms(3) + an in-memory map lookup) in build_profile

shared types: none

workarounds:
- `Set[108]` prefix collection literal (RecsBackend.new): typechecks, but `karac build` failed with `error: codegen failed: ... codegen: no handler for expression kind PrefixCollectionLiteral; this is a codegen bug`. Replaced with `let mut outage: Set[i64] = Set.new(); outage.insert(108);`.
- `self.table.entry(user_id).or_insert_with(Vec.new)` (the exact form design.md shows): `error[typecheck]: no associated function 'new' on type 'Vec'` when `Vec.new` is used as a function value. Wrapped it in a closure: `or_insert_with(|| Vec.new())`.
- Double free (miscompile) when a let-bound `Result[Vec[T], E]` is moved into a by-value function parameter, where `E` is an enum with a variant holding two Strings. Originally `build_profile` did the three fetches into `account_res` / `orders_res` / `recs_res` and then called `merge_profile(user_id, account_res, orders_res, recs_res)`. The built binary segfaulted with no output (valgrind: invalid read and invalid free of the cloned Vec buffer, which the callee had already freed). `karac run --interp` was correct. Building with KARAC_AUTO_PAR=0 or 1 made no difference, so auto-par is not the cause. Passing the fetch calls straight in as arguments was fine; only let-bound Results crashed, and only the Vec-payload ones. Minimal repro (prints `1` under --interp; the built binary aborts with `free(): double free detected in tcache 2`):
    enum E { A { x: String, n: i64 }, B { x: String, y: String } }
    fn make() -> Result[Vec[String], E] { Ok(["monitor", "usb hub"]) }
    fn take(r: Result[Vec[String], E]) -> i64 { 1 }
    fn main() { let r = make(); println(f"{take(r)}"); }
  It does not reproduce when `B` is removed. Workaround: dropped the `merge_profile` helper and inlined the merge (the three `match ... { Ok(..) => .., Err(e) => fallback }`) into `build_profile`, after the three fetch statements. Matching a local Result works. The fetch group stayed three consecutive independent statements.
- (Not a compiler bug, recorded for completeness: `let backend = parts[0];` on a `Vec[String]` is a hard `error[typecheck]` E_INDEX_MOVE_NON_COPY that blocks the build. Changed to `parts[0].clone()` as the diagnostic suggests. Also `!x` is a parse error in Kāra, so I used `not x`, and file-scope `const` became `let`. Both are spec-conformance fixes.)

ignored diagnostics: 4 (all `error[borrow_projection_copy]`: moving `name`/`email`/`tier` out of a bare `for` tuple element in AccountsBackend.new at L114, and `picked.push(item)` from a bare `for` over `candidates` at L249; `karac build` shows the last as a warning and builds)
