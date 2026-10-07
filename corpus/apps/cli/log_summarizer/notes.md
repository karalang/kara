1. lines: 343
2. build: ok
   interp: same (karac run --interp output is byte-identical to ./prog and to the Python mirror)
   mirror: agrees
3. stmt-par:
   - L324-L326 three fs.write calls (app.log, auth.log, db.log)
   - L328-L330 three fs.read_to_string calls
   - L332-L334 three independent parse_log calls (pure CPU, no waits)
4. shared types: none
5. workarounds:
   - L222 `self.component_latencies.entry(key).or_insert_with(Vec.new)` (the spec's own idiom in the Map Entry section) fails: `no associated function 'new' on type 'Vec'` when an associated fn is passed as a function value. Changed to `or_insert_with(|| Vec.new())`.
   - L221 `self.component_counts.insert(key, count + 1)` followed by `entry(key)` on the next line: karac only warns (`value 'key' moved here, used again here`), builds, and the compiled binary then reads a freed String. Result: build output had every per-component latency Vec with 1 sample (p50 == p95), while --interp was correct. Minimal repro: a loop doing `counts.insert(key, c + 1); lat.entry(key).or_insert_with(|| Vec.new()).push(i);` on two SortedMaps prints garbage keys (`\xef\xbf\xbd0i`) and lengths of 1 under `karac build`, correct under `--interp`. Use-after-move is accepted by the build and miscompiles to a use-after-free rather than being rejected. Workaround: `insert(key.clone(), count + 1)`. This is the only `.clone()` added for an ownership reason, and it was added because output was wrong.
   - L125, L147, L151, L155: indexing a `Vec[String]` by value (`spec.components[i]`, `fields[0]` passed into an enum payload) is a hard typecheck error, E_INDEX_MOVE_NON_COPY, and blocks the build. Added `.clone()` as that diagnostic suggests. The spec documents this rule, so it is not a bug, but it was a forced change. Oddly, `spec.errors[i]` as a match-arm tail (L130) is accepted by build and only reported by `karac check`.
   - L50/L145 renamed the field/local `seq` to `sequence`: `seq` is a reserved keyword (parse error). This is a naming fix, not a compiler issue.
6. ignored diagnostics: 5 (`error[` lines from `karac check source.orig.kara`: 3 borrow_projection_copy, 2 ownership; check exits 1 but `karac build` succeeds)
