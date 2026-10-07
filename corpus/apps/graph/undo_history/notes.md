lines: 346
build: ok
interp: same
mirror: agrees
stmt-par: none
shared types: none
workarounds:
- L23-L29 `Step.Do(Request)` with a one-field wrapper `struct Request { cmd: Command }` instead of the natural `Step.Do(Command)`. Error: `error[E_ENUM_NESTED_ENUM_PAYLOAD]: enum variant 'Step.Do' has a payload of nested enum type 'Command' — v1 only supports up to one level of enum nesting`. The diagnostic's first suggestion (`shared enum Command`) was tried and is broken under `karac build`: a `match` over a `shared enum` with struct-like variants carrying a String either segfaults at runtime (free fn `match c` / method `match self` with `ref self`) or panics codegen with `src/codegen/functions.rs:2445:53: Found StructValue ... but expected PointerValue variant` (method with by-value `self`, and the full program). Both work under `--interp`.
- L341-L342 `let final_text = editor.buffer.text;` then `final_text.len()` instead of `editor.buffer.text.len()` inside the f-string. Error: `error[chained_field_receiver]: chained field receivers (a.b.c…) are deferred to v1.x in codegen ... fails karac build`.
- L196 `let e = ref batch.edits[i];` instead of `let e = batch.edits[i];`. Error: `error[E_INDEX_MOVE_NON_COPY]: cannot move out of an index expression` (typecheck error; this is a language rule rather than a compiler bug, but it stopped the build).
- Identifier `group` renamed to `batch` in undo/redo: `'group' is a reserved keyword and cannot be used as an identifier`.
ignored diagnostics: 7

legacy after v2 edits (measured by the kata thread, not the drafting agent):
- source.kara aborts under legacy `karac build` with `free(): double free detected` and prints nothing; `--interp` matches the mirror. The cause is `karac fix`'s rewrite at L132/L134: `Command.Insert { pos, text: ref text } => (pos, text.clone())` in the SECOND `match cmd` of `Editor.execute` (an owned `cmd: Command` matched twice, the first match also binding `text: ref text`). Applying only that rewrite to the draft reproduces the abort, while applying only the L127/L129 rewrite or only the L328 `.into_iter()` change does not. A one-match minimal program does not reproduce it. Legacy is frozen, so this is a note, not a ledger row.
