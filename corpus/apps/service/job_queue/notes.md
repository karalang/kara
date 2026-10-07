lines: 342
build: ok
interp: same
mirror: agrees
stmt-par: L269-L271 three run_worker calls (each does one call_remote -> sleep_ms(2)), one per worker slot, independent of each other (the three take_next selections at L264-L266 are sequential mutations of the queue and come before the group)
shared types: none
workarounds:
- L222-L225 (run_worker): natural code was `Some(job) => { let mut job = job; job.attempts += 1; ... }`. Under `karac build` (codegen only; `karac run --interp` was correct) re-binding a pattern-bound variable under the SAME name (`let mut job = job;` / `let job = job;` inside a `match` arm or `if let` over an `Option[Job]` parameter) silently emptied the struct's String field: every payload printed as `` and every job failed validation with "empty payload" (no compile error, exit 0). Shadowing a plain by-value parameter (`fn f(job: Job) { let mut job = job; }`) and re-binding under a different name were both fine. Workaround: rename to `let mut claimed = job;`. Minimal repro: `fn run_b(slot: Option[Job]) -> i64 { match slot { None => 0, Some(job) => { let mut job = job; job.attempts += 1; println(f"b: {job.payload}"); job.id } } }` prints `b: ` under build, `b: abc` under --interp.
- L112, L117 (take_next): `let cand = self.pending[i];` was a hard typecheck error (E_INDEX_MOVE_NON_COPY, blocks the build); changed to `let cand = ref self.pending[i];` as the diagnostic suggests. (A blocking typecheck error, not an ignored ownership diagnostic.)
- Spec-learning fixes, not compiler failures: `||`/`&&`/`!` replaced with `or`/`and`/`not` (parse errors).
ignored diagnostics: 2 (both error[borrow_projection_copy]: L258 moving `for job in seed_jobs()` element into submit, L278 moving the `if let Some(outcome)` binding into settle). Also 1 warning[prelude_shadow] for `struct Stats` (left as is).
