lines: 395

build: ok
interp: same (the final version matches byte for byte. An earlier draft differed: `return Ok(Command.List(Filter.parse(which)?));` returned `Ok(())` under `karac run --interp` when `Filter.parse` failed, instead of propagating the Err, and the run then crashed with `method 'matches' not found on type 'unknown'`. Minimal repro: `fn outer(x: i64) -> Result[i64, E] { return Ok(inner(x)?); }` prints `ok ()` under --interp but `err neg` under build. The tail form `Ok(inner(x)?)` works. That draft was rewritten for length, not because of this bug, and the bug no longer occurs in the current code.)
mirror: agrees

stmt-par: none requested by the spec. Two existing pairs are consecutive, independent waits on separate files: L328-L329 (fs.write store + fs.write undo in undo_last) and L375-L376 (fs.write store + fs.write undo resetting state in main)

shared types: `shared enum Filter`. This was not my design choice. `Command.List(Filter)` failed the build with error[E_ENUM_NESTED_ENUM_PAYLOAD] ("v1 only supports up to one level of enum nesting"), and the compiler suggested `shared` as the fix.

workarounds:
- `shared enum Filter` (see above). The natural code is a plain `enum Filter` carried in `Command.List(Filter)`. Error: `error[typecheck] E_ENUM_NESTED_ENUM_PAYLOAD: enum variant 'Command.List' has a payload of nested enum type 'Filter' — v1 only supports up to one level of enum nesting`.
- Not workarounds, for the record:
  - I renamed the local `verb` to `op` because `verb` is a reserved keyword (parse error).
  - `let op = words[0].clone();` is needed because `let op = words[0];` is a typecheck error (E_INDEX_MOVE_NON_COPY). The spec says this, so it is not a compiler bug.
- Observations (not workarounds):
  - Warning: `enum Command` shadows a prelude type.
  - Spurious note[effect]: "mutual recursion group: `Store.parse`, `parse_id`". `parse_id` only calls `i64.parse`, so the effect checker seems to resolve `i64.parse` to the user's `Store.parse`.

ignored diagnostics: 3 (2 x error[borrow_projection_copy] on moving a `for` element (`line` at L240, `tag` at L353), 1 x error[ownership] on `cmd` used after `if let Command.Undo = cmd`)

legacy after v2 edits (measured by the kata thread, not the drafting agent):
- source.kara aborts under legacy `karac build` with `free(): double free detected` before printing anything; `--interp` matches the mirror. The cause is the hand edit at L336, `let cmd = match cmd { Command.Undo => return undo_last(), other => other };`, which rebinds an owned enum through a catch-all arm. Applied alone to the draft, it reproduces the abort; the two `.clone()` edits do not. Legacy is frozen, so this is a note, not a ledger row.
