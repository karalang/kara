# Real-world programs (`corpus/apps/`)

Application-shaped programs, as opposed to the algorithm katas: services,
command-line tools, data pipelines and object graphs, 260-400 lines each. They
exist so the M1 go/no-go and M4 have real-world numbers: how much the v2
ownership rules cost a newcomer, and whether stmt-par has waits worth
overlapping.

```
corpus/apps/<kind>/<name>/source.orig.kara   first draft, as the drafting agent saved it
corpus/apps/<kind>/<name>/source.fixed.kara  source.orig.kara after `karac fix`, run to a fixed point
corpus/apps/<kind>/<name>/source.kara        the v2 program: source.fixed.kara plus hand edits
corpus/apps/<kind>/<name>/<name>.py          Python mirror, same algorithm and data
corpus/apps/<kind>/<name>/expected.out       the mirror's stdout (`expected_from = "mirror"`)
corpus/apps/<kind>/<name>/notes.md           the drafting agent's report
corpus/apps/friction.json                    one measured row per program
```

`<kind>` is `service`, `cli`, `pipeline` or `graph`. Tags are `apps`,
`apps:<kind>`, and `stmt-par` for a program with groups of consecutive,
independent waits (file I/O on separate files, or `sleep_ms` standing in for a
remote call). The groups' line ranges are in each `meta.toml` note.

## How a program was made

1. A fresh agent that had not seen the v2 diagnostics got one spec line and
   `docs/design.md`, and nothing else from either repo. It wrote the program
   and the Python mirror, and was told to ignore v2 ownership diagnostics:
   fix only what stops `karac build` or gives wrong output, work around
   compiler failures naturally, and report each one.
2. `scripts/corpus/apps.py fix` applies `karac fix` until nothing changes.
3. Whatever `karac check` still reports is fixed by hand, with one reason per
   edit (`finalize --hand`).
4. `scripts/corpus/apps.py finalize` writes `expected.out` from the mirror,
   runs legacy build and interp against it, and stores the row.
5. `scripts/corpus/apps.py table corpus/apps` re-measures every program and
   rewrites the table below.

**This is dogfooding, not the Mend blind rate.** The drafts are blind to the
v2 diagnostics, but nobody measured a fix rate over them, and the hand edits
were made by an author who knows the rules.

**What the draft column means.** A draft had to build under legacy before it
was saved, so anything legacy itself refuses is already adapted in
`source.orig.kara`. The commonest case is the index-move error
(`E_INDEX_MOVE_NON_COPY`): drafts write `ref v[i]` or `v[i].clone()`, as
design.md says. Those adaptations are part of the natural draft, so they are
not counted. "v2 errors in draft" is therefore exactly the friction v2 adds:
the errors only `karac check` raises.

**Note 1.** In two programs `karac fix` refused the whole file. A
false-positive E0500 on matching a unit variant gets a fix that inserts `ref `
before the pattern, which does not parse, so the valid `.clone()` fixes in the
same batch were dropped too. Those clones count under fix ("refused"), not
under hand edits, because the refusal is a tool bug, not authoring friction.

**Note 2.** For three programs the legacy build disagrees with the mirror
after the v2 edits, while legacy interp agrees. Each is a frozen legacy bug,
described in the program's notes.md. These entries are recorded with
`legacy_backend = "legacy-interp"`.

## What the 24 show (2026-10-07, karac built at fa262d763)

Measured with `karac` built at fa262d763 (main 8be1a08d3 differs only in
`src/mir/borrowck.rs`). Thread A's f3d68072a and 0e66a9bd9, which landed
right after, fix the unit-variant false positive and change `karac fix`, so
the numbers below describe that compiler, not later ones.

- **v2 adds about one error per 94 lines.** There are 95 errors over 8957
  lines: E0200 (move out of a borrowed place), E0500 (use after move) and
  E0515. 5 of the 24 drafts had none.
- **`karac fix` handles almost all of them, with clones only.** It adds 93
  `.clone()` calls (5 of them in the two refused batches, note 1) and 4 `ref`
  bindings, all of them `text: ref text` patterns in undo_history. It never
  adds a `ref` parameter, never `.into_iter()`, and never `#[derive(Copy)]`.
- **12 hand edits in total.** There are 5 `#[derive(Copy)]` on fieldless enums
  (Level, EditKind, Currency, EvictReason) or a plain-int struct (Stats), 5
  `.into_iter()` on a loop that consumes its elements, and 2 others: a double
  clone `karac fix` wrote, and a rebind around the unit-variant false
  positive. The derive(Copy) count is kept separately for the language
  question "are fieldless enums Copy without a derive".
- **Fix-tool bugs (sent to Thread A):**
  - a false-positive E0500 on matching a unit variant, whose `ref ` fix does
    not parse, so the whole file is refused (2 programs);
  - a const `String` used twice is reported as a move (rate_limiter);
  - `k.clone().clone()` (lru_cache);
  - one clone per round for repeated uses of a binding (7 rounds in
    http_router).
- **`shared` appears 5 times in 4 programs.** Two are natural: an LRU list
  node and a JSON tree. Three are forced by `E_ENUM_NESTED_ENUM_PAYLOAD`
  (an enum as another enum's payload), whose diagnostic suggests `shared`.
- **stmt-par:** 12 of 24 programs have groups of independent waits, all of
  them file reads and writes or `sleep_ms` remote calls.
- **Legacy, frozen, notes only:** the drafts worked around 46 compiler gaps.
  The commonest kinds are:
  - a double free or wrong output under `karac build` only, in 11 drafts, and
    in 2 more programs only after the v2 edits (note 2);
  - `Map.entry(..).or_insert(..)` on a struct value;
  - `"""` literals that fail codegen;
  - chained field receivers;
  - nested enum payloads;
  - `*` on a `Map.get` result, which the spec types `Option[ref V]` (5
    programs);
  - `Vec.sort()` ignoring a user `Ord` impl on both backends.

  All 24 `legacy.out` files equal the mirror (3 on legacy-interp, note 2).
  None runs on `mir-interp` yet: each stops at a type or library method that
  is not lowered.

## Language-design measurements

On the final `source.kara` of every program, for the review thread's
parameter-default, integer-width and effect-group questions. Method in
`scripts/corpus/apps_lang.py`; raw rows in `lang.json`.

<!-- LANG START (scripts/corpus/apps_lang.py) -->
### 1. Non-Copy parameters: stored or only read

| program | non-Copy params | stored | only read | mutated (`mut ref`) | by-value but only read | declared `ref` |
|---|---|---|---|---|---|---|
| cli/config_merge | 22 | 6 | 13 | 3 | 3 | 10 |
| cli/csv_report | 18 | 2 | 16 | 0 | 0 | 16 |
| cli/log_summarizer | 9 | 1 | 8 | 0 | 0 | 8 |
| cli/text_diff | 15 | 5 | 10 | 0 | 1 | 9 |
| cli/todo_store | 19 | 4 | 15 | 0 | 0 | 15 |
| cli/word_freq_multi | 13 | 4 | 9 | 0 | 1 | 8 |
| graph/dep_resolver | 21 | 5 | 13 | 3 | 4 | 9 |
| graph/doc_tree | 15 | 6 | 8 | 1 | 4 | 4 |
| graph/entity_system | 4 | 1 | 3 | 0 | 0 | 3 |
| graph/lru_cache | 17 | 6 | 11 | 0 | 10 | 1 |
| graph/observer_list | 25 | 7 | 16 | 2 | 5 | 11 |
| graph/undo_history | 7 | 5 | 2 | 0 | 0 | 2 |
| pipeline/etl_orders | 11 | 2 | 8 | 1 | 2 | 6 |
| pipeline/invoice_pipeline | 20 | 2 | 16 | 2 | 3 | 13 |
| pipeline/join_dedupe | 13 | 2 | 11 | 0 | 1 | 10 |
| pipeline/schema_transform | 18 | 1 | 12 | 5 | 3 | 9 |
| pipeline/sensor_merge | 17 | 3 | 14 | 0 | 2 | 12 |
| pipeline/windowed_stats | 14 | 4 | 9 | 1 | 5 | 4 |
| service/http_router | 38 | 6 | 25 | 7 | 3 | 22 |
| service/job_queue | 10 | 4 | 6 | 0 | 2 | 4 |
| service/profile_aggregator | 13 | 2 | 11 | 0 | 2 | 9 |
| service/pubsub_broker | 26 | 8 | 17 | 1 | 3 | 14 |
| service/rate_limiter | 11 | 2 | 8 | 1 | 0 | 8 |
| service/session_store | 22 | 5 | 17 | 0 | 0 | 17 |
| **total** | 398 | 93 | 278 | 27 | 54 | 224 |

Stored: 93 of 398 (23%); only read: 278 (70%); mutated through `mut ref`: 27 (7%). Of the 147 declared by value, 54 are only read (37%). 2 of the stored count are `.into_iter()` consumptions the ownership query reports as `ref`. Caveat: a declared `ref` was the author's choice under a spec where every parameter declares its mode, and some were forced: legacy refuses a by-value index read (`E_INDEX_MOVE_NON_COPY`), so two drafts turned owned-String helpers into `ref String` ones (dep_resolver, invoice_pipeline).

### 2. Integer widths and `as` casts

14 `as` casts in 2 programs: -> i64 x3, char -> u32 x11. Every `char -> u32` is lossless and every `-> i64` widens a u32, so none can lose a value. Numeric types other than i64 appear only in pipeline/schema_transform (u32 x12), service/session_store (u32 x1); there are no floats. Binary operators with operands of different integer widths: 0, because the u32 values only meet u32 values or literals. One draft (session_store) tried `i64 + u32` and was refused (`cannot mix integer types`), although design.md lists u32 -> i64 as implicit.

### 3. Effect resources per function

`pub fn`: 0 in all 24 programs (single-file programs declare nothing public), so the count covers every function (533). Distinct resources named by a function's inferred effects:

| resources | functions | functions, not counting Heap |
|---|---|---|
| 0 | 211 | 457 |
| 1 | 253 | 60 |
| 2 | 53 | 16 |
| 3 | 16 | 0 |

Resources seen: Heap x315, Stdout x67, FileSystem x25.

<!-- LANG END -->

## Friction table

<!-- TABLE START (scripts/corpus/apps.py table) -->
| program | lines | v2 errors in draft | clones by fix | refs by fix | fix rounds | hand: derive(Copy) | hand: into_iter | hand: other | shared types | stmt-par | index-move adapted in draft (legacy refuses) | compiler gaps worked around in draft | legacy vs mirror |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| service/http_router | 399 | E0200 x2, E0500 x1 | 8 | 0 | 7 | 0 | 0 | 0 | 0 | no | no | 4 | agrees |
| service/job_queue | 342 | E0200 x2 | 0 | 0 | 1 | 0 | 2 | 0 | 0 | yes | yes | 1 | agrees |
| service/profile_aggregator | 399 | E0200 x4 | 4 | 0 | 2 | 0 | 0 | 0 | 0 | yes | yes | 3 | agrees |
| service/pubsub_broker | 400 | E0500 x1 | 1 | 0 | 2 | 0 | 0 | 0 | 0 | no | no | 3 | agrees |
| service/rate_limiter | 399 | E0200 x3, E0500 x1 | 4 | 0 | 2 | 0 | 0 | 0 | 0 | no | yes | 3 | interp agrees; build does not (note 2) |
| service/session_store | 399 | none | 0 | 0 | 1 | 0 | 0 | 0 | 2 | no | no | 3 | agrees |
| cli/config_merge | 389 | E0200 x2, E0500 x3 | 6 | 0 | 3 | 0 | 0 | 0 | 0 | yes | no | 0 | agrees |
| cli/csv_report | 398 | E0200 x2, E0500 x1 | 3 | 0 | 2 | 0 | 0 | 0 | 0 | no | no | 3 | agrees |
| cli/log_summarizer | 344 | E0200 x3, E0500 x1, E0515 x1 | 0 (+3 refused, note 1) | 0 | 1 | 1 | 1 | 0 | 0 | yes | yes | 2 | agrees |
| cli/text_diff | 308 | E0200 x8, E0515 x5 | 12 | 0 | 2 | 1 | 0 | 0 | 0 | no | yes | 0 | agrees |
| cli/todo_store | 395 | E0200 x2, E0500 x1 | 0 (+2 refused, note 1) | 0 | 1 | 0 | 0 | 1 | 1 | yes | yes | 1 | interp agrees; build does not (note 2) |
| cli/word_freq_multi | 264 | E0200 x2 | 2 | 0 | 2 | 0 | 0 | 0 | 0 | yes | no | 3 | agrees |
| pipeline/etl_orders | 345 | E0200 x3 | 2 | 0 | 2 | 0 | 1 | 0 | 0 | yes | no | 0 | agrees |
| pipeline/invoice_pipeline | 401 | E0200 x7, E0500 x2, E0515 x4 | 12 | 0 | 2 | 1 | 0 | 0 | 0 | no | yes | 2 | agrees |
| pipeline/join_dedupe | 379 | E0200 x1 | 1 | 0 | 2 | 0 | 0 | 0 | 0 | yes | no | 2 | agrees |
| pipeline/schema_transform | 397 | none | 0 | 0 | 1 | 0 | 0 | 0 | 1 | no | no | 6 | agrees |
| pipeline/sensor_merge | 335 | none | 0 | 0 | 1 | 0 | 0 | 0 | 0 | yes | no | 0 | agrees |
| pipeline/windowed_stats | 397 | none | 0 | 0 | 1 | 0 | 0 | 0 | 0 | yes | yes | 2 | agrees |
| graph/dep_resolver | 373 | E0200 x7, E0500 x2, E0515 x1 | 12 | 0 | 4 | 0 | 0 | 0 | 0 | yes | yes | 1 | agrees |
| graph/doc_tree | 399 | E0500 x1, E0515 x1 | 2 | 0 | 2 | 0 | 0 | 0 | 0 | no | yes | 0 | agrees |
| graph/entity_system | 398 | none | 0 | 0 | 1 | 0 | 0 | 0 | 0 | no | no | 3 | agrees |
| graph/lru_cache | 355 | E0200 x6, E0500 x5 | 9 | 0 | 2 | 2 | 0 | 1 | 1 | yes | yes | 0 | agrees |
| graph/observer_list | 396 | E0200 x2, E0500 x1 | 3 | 0 | 2 | 0 | 0 | 0 | 0 | no | no | 2 | agrees |
| graph/undo_history | 346 | E0200 x4, E0500 x3 | 7 | 4 | 4 | 0 | 1 | 0 | 0 | no | yes | 2 | interp agrees; build does not (note 2) |
| **24 programs** | 8957 | 95 | 93 | 4 | | 5 | 5 | 2 | 5 | 12 | 12 | 46 | build agrees on 21 |

Per program: hand edits (why) and the compiler gaps the drafting agent worked around.

**cli/config_merge**
- draft gap: L186/L269/L285 .to_string() on a borrowed for-loop element going into an owned String field (author's type error, not a compiler gap)
- stmt-par groups (source.orig.kara lines): L345-L346 two fs.write of separate files (app.conf, app.local.conf); L347-L348 two fs.read_to_string of those files

**cli/csv_report**
- draft gap: pad_left(f"{orders}", 6): an f-string temporary passed straight to a ref String param double-frees under build; .to_string() instead
- draft gap: L297 current = t.region.clone(): the copy out of a borrowed for element double-frees under build without it (also what v2 requires)
- draft gap: *p on a Map.get result rejected (the spec says Option[ref V]); dropped the *

**cli/log_summarizer**
- hand: L8 #[derive(Copy)] on fieldless enum Level: 'match level' moved it, then it was used again (E0500)
- hand: L130 spec.errors[i] -> .clone(): a String taken out of a Vec element as a match-arm value (E0515)
- hand: L225 entry.message -> .clone(): Map key taken from a borrowed for element (E0200)
- hand: L233 message -> .clone(): Map key from a borrowed map walk moved into a struct (E0200)
- hand: L241 'for r in ranks' -> '.into_iter()': each element moved into another Vec (E0200)
- draft gap: L222 or_insert_with(Vec.new) rejected (associated fn as a value); written as a closure
- draft gap: L221 insert(key, ..) then entry(key) builds with a warning and reads a freed String under build; cloned key
- draft gap: L125/L147/L151/L155 .clone() on Vec[String] index reads (E_INDEX_MOVE_NON_COPY)
- fix refused: karac fix refused the whole batch: its fix for the E0500 on a unit-variant pattern inserts `ref ` before the pattern (`ref Level.Error =>`), which does not parse, so the good .clone() fixes were not applied either (L128)
- stmt-par groups (source.orig.kara lines): L324-L326 three fs.write of separate log files; L328-L330 three fs.read_to_string; L332-L334 three parses, one per file

**cli/text_diff**
- hand: L8 #[derive(Copy)] on the fieldless enum EditKind: copying e.kind out of a borrowed Edit (E0200); karac fix offers no fix for it
- draft gap: ref edits[k] where an index read by value was rejected (E_INDEX_MOVE_NON_COPY)

**cli/todo_store**
- hand: L240 line.clone(): a borrowed for element moved into an Err payload inside return (E0200); karac fix left it
- hand: L353 tag.clone(): a borrowed for element moved into a map key (E0200); karac fix left it
- hand: L336 let cmd = match cmd { Command.Undo => return undo_last(), other => other }: the checker treats if let Command.Undo = cmd (a unit variant, no bindings) as a move of cmd (E0500, a false positive), and ref cmd is not supported (E_REF_OPERAND_UNSUPPORTED)
- draft gap: shared enum Filter: Command.List(Filter) rejected (E_ENUM_NESTED_ENUM_PAYLOAD), and shared is what the diagnostic suggests
- draft gap: let op = words[0].clone() (E_INDEX_MOVE_NON_COPY on the by-value read)
- fix refused: karac fix refused the whole batch: its fix for the E0500 on a unit-variant pattern inserts `ref ` before the pattern (`if let ref Command.Undo = cmd`), which does not parse, so the good .clone() fixes were not applied either (L336)
- legacy note: legacy build double-frees on the hand edit at L336 (let cmd = match cmd { Command.Undo => return .., other => other }); interp agrees
- stmt-par groups (source.orig.kara lines): L328-L329 fs.write of the store and of the undo file in undo_last; L375-L376 the same pair in main

**cli/word_freq_multi**
- draft gap: FreqTable.top: Vec.sort() ignored a user Ord impl on WordCount on BOTH backends (sorted by field order); sorts (-count, word) tuples instead
- draft gap: match fs.write(..) { Ok(()) => .. } is non-exhaustive (the unit pattern does not cover ()); Ok(_)
- draft gap: *c on a Map.get result and *count on a map-walk value rejected (the spec says ref V); dropped the *
- stmt-par groups (source.orig.kara lines): L238-L242 five fs.read_to_string, one per text file

**graph/dep_resolver**
- draft gap: Registry folded into Resolver: self.registry.releases[i] fails build (chained_field_receiver), and a ref-returning accessor is rejected (borrow-return form not yet supported)
- draft gap: let rel = ref self.releases[i] and parse(text: ref String) where an index read by value was rejected (E_INDEX_MOVE_NON_COPY)
- stmt-par groups (source.orig.kara lines): L332-L335 four fs.write, each to its own manifest file

**graph/doc_tree**
- draft gap: L268 let node = ref self.nodes[id] (E_INDEX_MOVE_NON_COPY on the by-value read)

**graph/entity_system**
- draft gap: self.weapons[i].unwrap().range on a Vec[Option[4-field Copy struct]] double-frees under build; match instead
- draft gap: self.weapons[i] = t.weapon (an Option[Copy struct] field) leaves the slot dangling under build; if let Some(w) .. = Some(w)
- draft gap: for Hit { attacker, target, amount } in hits: codegen has no handler for methods on the destructured binding; destructure inside the loop

**graph/lru_cache**
- hand: L13 #[derive(Copy)] on fieldless enum EvictReason: 'reason' passed by value inside a listener loop (E0500)
- hand: L60 #[derive(Copy)] on Stats (six i64): 'let s = cache.stats' moved the field out of cache (E0500); 'let s = ref cache.stats' is the natural fix but borrowing a field is not implemented yet (E_REF_OPERAND_UNSUPPORTED)
- hand: L321 removed a second .clone() that karac fix added: k.clone().clone(), two identical edits at 321:45 in its first round
- draft gap: L242/L244/L250 .clone() on words[i] (E_INDEX_MOVE_NON_COPY on the by-value read)
- stmt-par groups (source.orig.kara lines): L344-L345 two fs.write of separate files; L347-L348 two fs.read_to_string of those files

**graph/observer_list**
- draft gap: Vec.remove_first(pred), listed in the spec, is missing (no method 'remove_first'); index loop + remove(i)
- draft gap: *i on a Map.get result rejected (the spec says Option[ref V]); dropped the *

**graph/undo_history**
- hand: L328 script().into_iter(): the loop consumes each Step by value (E0200 on the bare for element); karac fix offers no fix for this
- draft gap: Step.Do(Command) rejected (E_ENUM_NESTED_ENUM_PAYLOAD); shared enum Command, the diagnostic's suggestion, segfaults or panics codegen under build, so Step.Do wraps a struct Request
- draft gap: editor.buffer.text.len() fails build (chained_field_receiver); bound to a local first
- draft gap: let e = ref batch.edits[i] (E_INDEX_MOVE_NON_COPY on the by-value read)
- legacy note: legacy build double-frees on karac fix's `text: ref text` + .clone() in the second match on an owned cmd (L132/L134); interp agrees

**pipeline/etl_orders**
- hand: L295 'for c in customers' -> 'customers.into_iter()': the loop moves each Customer into a Map (E0200)
- stmt-par groups (source.orig.kara lines): L285-L286 two fs.write of separate files; L288-L289 load_orders / load_customers, each a read_to_string + parse of its own file

**pipeline/invoice_pipeline**
- hand: L6 #[derive(Copy)] on the fieldless enum Currency: order.currency copied out of a borrowed Order into the Invoice (E0200, 'this type has no .clone()'); karac fix offers no fix
- draft gap: E_INDEX_MOVE_NON_COPY on f[i] passed to owned-String parse helpers (9 sites); helpers take ref String. The same index reads as struct-literal fields were NOT rejected
- draft gap: a multi-line string literal fails codegen (no handler for MultiStringLit); input built with push_str per record
- draft gap: *v on a SortedMap.get result rejected (the spec says Option[ref V]); dropped the *

**pipeline/join_dedupe**
- draft gap: let (crm, merged) = dedupe(..) with a SortedMap in the tuple fails build (no handler for method 'len' on a tuple-destructured SortedMap); reads .0/.1 instead
- draft gap: generic dedupe[T: Record] MISCOMPILED under build (a local SortedMap[String, T] in a generic fn returns a wrong value from get); split into two concrete functions
- stmt-par groups (source.orig.kara lines): L293-L294 two fs.write of separate files; L300-L301 two fs.read_to_string of those files

**pipeline/schema_transform**
- draft gap: enum named Json (prelude shadow) panicked codegen (stmts.rs StructValue vs PointerValue); renamed JsonValue
- draft gap: match on a ref shared-enum binding the Obj map, then obj.get(..): codegen has no handler for get; split into migrate + transform(obj: ref SortedMap)
- draft gap: for (i, raw) in v1_records().iter().enumerate() not lowered in codegen; bound records first
- draft gap: a closure returning a shared-enum value fails LLVM verification; inline match instead
- draft gap: ? on Result[char, E] then s.push(c) fails LLVM verification (char loaded as i64); helper pushes into a mut ref String
- draft gap: for t in tags (SortedSet[String]) by value pushing into an enum payload printed garbage / double-freed under build; tags.iter() + t.clone()

**pipeline/sensor_merge**
- stmt-par groups (source.orig.kara lines): L302-L304 three fs.write, one per sensor file; L310-L312 three fs.read_to_string of those files

**pipeline/windowed_stats**
- draft gap: Map.entry(k).or_insert(v).method() with a mut ref self method loses the update under build; remove-then-insert instead
- draft gap: ref-param pass of a Map struct field corrupts the field under build (garbage len, segfault at exit); pipeline destructured first
- draft gap: let st = ref windows[k] over a Map element fails codegen, though E_INDEX_MOVE_NON_COPY suggests it; match windows.get(k) instead
- stmt-par groups (source.orig.kara lines): L332-L334 three fs.write, one feed file each; L336-L338 three fs.read_to_string of those files

**service/http_router**
- draft gap: Fn value in an enum payload panics codegen (closures.rs compile_closure_call); Matched carries a route index
- draft gap: self.router.routes[i].handler fails build (chained_field_receiver); accessor method
- draft gap: user struct Request with a header method was silently wrong under interp (prelude_shadow only warns); renamed HttpRequest
- draft gap: handler(ctx, self.store) in a mut ref self method demands a call-site mut marker, which the spec says is not needed

**service/job_queue**
- hand: L257 'for job in seed_jobs()' -> '.into_iter()': each job is moved into the queue (E0200)
- hand: L274 'for slot in [o1, o2, o3]' -> '.into_iter()': each outcome is moved into settle (E0200)
- draft gap: L222 `let mut job = job` in a match arm emptied the String under build (same-name rebind); renamed to claimed
- draft gap: L112/L117 let cand = ref self.pending[i] (E_INDEX_MOVE_NON_COPY)
- stmt-par groups (source.orig.kara lines): L269-L271 three run_worker calls, each one call_remote standing in as sleep_ms(2); the take_next selections before them are sequential

**service/profile_aggregator**
- draft gap: Set[108] prefix collection literal fails codegen (no handler for PrefixCollectionLiteral); Set.new() + insert
- draft gap: or_insert_with(Vec.new), the form design.md shows, rejected (associated fn as a value); closure
- draft gap: a let-bound Result[Vec[String], E] moved into a by-value param double-frees under build when E has a two-String variant; merge inlined
- draft gap: parts[0].clone() (E_INDEX_MOVE_NON_COPY on the by-value read)
- stmt-par groups (source.orig.kara lines): L257-L259 three backend fetches (accounts, orders, recs), each sleep_ms(3) plus a map lookup

**service/pubsub_broker**
- draft gap: a method call passing msg.topic to a ref String param inside a for loop frees the field under build (double free); matches on the topic local before building Message
- draft gap: a multi-line string literal fails codegen (MultiStringLit); Vec of literals
- draft gap: let mut evicted = None assigned later from pop_front: cannot infer T; annotated Option[Message]

**service/rate_limiter**
- draft gap: Decision.Throttle(Reason) rejected (E_ENUM_NESTED_ENUM_PAYLOAD); reasons folded into Decision
- draft gap: entry(k).or_insert(struct) as mut ref V fails codegen (no handler / field not recorded) or segfaults when chained; take-out/put-back with remove + insert
- draft gap: self.policy_for(req.route) on a ref Request double-frees at exit under build; lookup inlined into handle
- draft gap: let s = ref self.clients[id] (the E_INDEX_MOVE_NON_COPY suggestion) fails in interp for a Map element; let Some(s) = self.clients.get(id) else ..
- fix note: L365 karac fix cloned a const: `const LOG_PATH: String` used twice is reported as use-after-move (E0500), a false positive since a const is not a moved binding
- legacy note: legacy build refuses LOG_PATH.clone() on a const String (codegen: Vec/String method 'clone' is not yet supported), the clone karac fix added at L365

**service/session_store**
- draft gap: shared enum Expiry and shared enum TokenRef: a plain enum as another enum's payload is rejected (E_ENUM_NESTED_ENUM_PAYLOAD), and shared is the diagnostic's suggestion
- draft gap: L354 match out.evicted inside Ok(out) => double-freed the evicted token under build (interp right); destructured LoginOutcome { token, evicted } in the arm instead
- draft gap: L118 (c as u32) as i64: i64 + u32 arithmetic rejected although the spec lists u32 -> i64 as an implicit widening

<!-- TABLE END -->
