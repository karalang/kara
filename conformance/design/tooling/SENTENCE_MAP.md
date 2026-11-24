# design.md § 17 Tooling contract: sentence-to-cell map

Status: covered, partial, FAILS (routed, the owner named), not testable, not drafted. Line numbers are docs/design.md lines (2026-11-24). Cells are named by where they live:

- `c17:`, `c17b:`, `c17c:` name a check (its printed name) in `probes/cases17.py`, `probes/cases17b.py` and `probes/cases17c.py`, run by `tests/tooling_sentence_probes_2026_11_24.rs` on the `--features llvm` leg (`K=<karac> python3 probes/<file>` by hand). `cases17` and `cases17b` are the 2026-11-21 design-14-17 pass, moved here from the shared folder; `cases17c` is this audit's.
- A bare `tests/<file>` or `lsp/tests/<file>` is a Rust test file; `repl/` is `conformance/repl/`.

**Totals (2026-11-24):** 174 table rows (normative sentences), plus the REPL subsection's 14, pinned in `conformance/repl/`. Of the 174: 155 covered, 7 partial, 2 FAILS (both outside doc, LSP and REPL; routed to the review), 10 not testable (client obligations, scope and stability statements, and `#[cfg]` conditions, since `#[cfg]` is not implemented). The probes: `cases17` 120/120, `cases17b` 20/20, `cases17c` 115/116 (its one failure is the first FAILS row). The 116 checks of `cases17c` and the four LSP tests in `lsp/tests/protocol_contract_2026_11_24.rs` are new.

**Drift found.**
- FIXED here: design.md said the language server's text sync is full-document. Since B-2026-11-21-33 it advertises incremental sync (range edits), which `lsp/tests/server.rs` pins. The row now says so.
- FAILS, routed: `karac run` ignores `[profile.dev] runtime_debug_metadata = false` (line 7557): `has_debug_metadata()` prints `true` under `karac run` while the binary `karac build` writes from the same package prints `false`. The MIR interpreter (`--interp`) answers `false` whatever the profile.
- FAILS, routed: a bare `panic()` records the message `"explicit panic"`, where line 7576 says a panic with no message records `"<no message>"`. The renderer (`src/crash_report.rs`) already falls back to `<no message>` for a record without one.

## Structured compiler output (7211–7263)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7213 | All diagnostics are available as JSON with machine-applicable fix diffs | covered | c17: check --output=json: stdout is one JSON document; diag E0500: hint diff shape, replacement shape |
| 7215 | The document's fields: `program_effects`, `diagnostics` with `id`, `severity`, `primary`, `code`, `category`, `concept`, `file`, `line`, `column`, `message`, `hints` | covered | c17: diag E0500: required fields |
| 7215 | An effect diagnostic carries `inferred_effects`, `declared_effects` and a `derivation` | covered | c17c: effect diag: inferred_effects and declared_effects; effect diag: derivation of {file,line,reason} |
| 7215 | `program_effects` is null if the build did not reach effect analysis or it reported an error | covered | c17: parse error: program_effects null; check json: program_effects listed when effect analysis ran clean |
| 7255 | A passing build: `"status": "ok"` first, `"output"` last, `diagnostics` present, empty without warnings | covered | c17: build json ok: status first, output last, diagnostics present and empty |
| 7257 | A fix is a hint whose `diff` names the lines it touches from `line`: `old` as they are, `new` after | covered | c17: diag E0500: hint diff shape; c17c: effect diag: hint diff names old and new lines |
| 7257 | Beside the hints, `replacement` `{offset, length, text}` for one edit, `fix_diff` for several; offsets are bytes of `file` | covered | c17: diag E0500: replacement shape; tests/derivation_and_fixes.rs (fix_diff) |
| 7257 | Every phase, and a package as well as a file, reports a fix in this one shape | covered | c17c: package: unused_mut fix has replacement and a hint diff; effect diag: byte edits beside the hint |
| 7259 | Codes come from per-phase bands; every code is catalogued for `karac explain` | covered | c17: explain E0500 / W0300 / E_CHAR_AS_NARROW_INT; tests/docs_conformance.rs |
| 7261 | `code` is the symbolic code, `parent_code` the family's number | covered | c17: E_CHAR_AS_NARROW_INT has parent_code E0210 |
| 7261 | A record with one code has no `parent_code` | partial | c17: diag E0500: required fields (E0500 carries none; not asserted as absent) |
| 7261 | The message does not repeat the code | covered | c17: diag E0500: message does not repeat the code |
| 7261 | The text renderer's header names the same code | covered | c17c: text header names the symbolic code |
| 7263 | A lint is one `W03xx` number; escalated it is the matching `E03xx`; the old shared code is `parent_code`; `lint_name` names it | covered | c17: redundant_clone is W0300, parent W0295; -D redundant_clone gives E0300 parent E0295 |
| 7263 | `karac explain` takes either code or the lint name | covered | c17: explain W0300, explain W0295, explain redundant_clone |

## Diagnostic quality commitments (7265–7300)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7269 | The JSON document goes to stdout and holds only actionable information | covered | c17: check json: stderr has no JSON; c17b: build json: stdout is exactly one JSON doc |
| 7269 | Progress, phase timings and cache messages go to stderr | covered | c17c: build json: stdout one document, progress not on stdout |
| 7269 | Timings go to a separate file (`--emit-timings=timings.json`), never into the stream | covered | c17b: --emit-timings writes a JSON file, stdout unchanged |
| 7271 | Every diagnostic carries `id` and `primary` | covered | c17: diag E0500: required fields |
| 7271 | In v1 every entry is `primary: true` and none has `consequence_of` | covered | c17: diag E0500: primary true in v1, no consequence_of |
| 7271 | A client reads `primary` / `consequence_of` when present | not testable | a client obligation |
| 7279 | 20 primary errors per file, then a `truncated` marker; other files continue | covered | c17: cascade cap: 20 errors; c17b: cap is per file in a package; tests/design_14_17_conformance_2026_11_21.rs |
| 7279 | Consequences do not count against the cap | not testable | v1 reports no consequences |
| 7279 | Warnings are never capped | covered | c17b: warnings are not capped (25 unused_mut) |
| 7285 | The marker follows the phase's last entry and names the phase; the stream carries the same markers | covered | c17: cascade cap: truncated marker with phase and count 5; cascade cap: same marker in the stream |
| 7289 | Every diagnostic carries a `concept` from the table | covered | c17: diag E0500: concept in the table; a warning carries concept |
| 7291 | A diagnostic from transitive inference carries an ordered `derivation` of `{file, line, reason}` | covered | c17c: effect diag: derivation of {file,line,reason}; tests/derivation_and_fixes.rs |
| 7293 | The commitments table (primary required for errors, cap errors only, concept required for both, derivation when inferred) | covered | the rows above |

## Concept ids (7302–7354)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7354 | Every diagnostic has a concept, including the package checks (manifest, dependencies, toolchain pin) and the MIR build | covered | c17c: package check: every diagnostic has a concept in the table; tests/concept_ids.rs |
| 7354 | `karac explain <id>` names the sections and the codes that carry the id | covered | c17: explain concept names its codes |
| 7354 | A new id is added to the table when a diagnostic first uses it | covered | tests/concept_ids.rs (every concept emitted is in the table) |

## Error return traces (7356–7380)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7358 | Debug builds record the `?` path; it appears in `--output=json` and as a note in terminal output | covered | c17b: run --output=json: error_return_trace in the record; c17c: text mode: the trace is a note on stderr |
| 7360 | A frame is `{file, line, column, expr, context}` | covered | c17b: run --output=json(l): error_return_trace in the record |
| 7374 | `--release` strips the bookkeeping | covered | c17c: --release: no error return trace |
| 7375 | A ring buffer 64 deep | covered | c17c: depth limit (at most 64 frames) |
| 7376 | Only a `Result`'s `?` records a frame; a panic on `None` prints no trace | covered | c17c: Option ?: no Error return trace on the panic |
| 7377 | A handled error ends its trace | covered | c17c: a handled error ends its trace |
| 7377 | `unwrap`/`expect` keep it; `map_err`, `and_then`, `or_else` keep it | partial | c17b: panic record kind unwrap with its trace; `map_err` etc. in tests/mir_error_trace_drops.rs |
| 7378 | A dropped error ends its trace; printing alone does not clear; a moved value keeps its frames | covered | c17c: a dropped error ends its trace; tests/mir_error_trace_drops.rs |
| 7379 | A full buffer drops the oldest entry: `{"frames": [...], "truncated": true}` | covered | c17c: depth limit: a full buffer reports {frames, truncated: true} |
| 7380 | A chain split across parallel tasks reports `unavailable (the ? chain was split across parallel tasks)` | covered | tests/mir_error_trace_tasks.rs |
| 7380 | A panic with no `?` behind it prints no trace | covered | tests/tooling_conformance.rs tooling_only_a_propagated_error_prints_a_trace |

## Streaming phase events (7382–7444)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7384 | Everything the batch mode produces is in the stream; a complete stream reconstructs the batch | covered | c17: jsonl reconstructs batch diagnostics (and the ownership and ok variants) |
| 7386 | `karac build --output=jsonl`; `karac check --output=jsonl` for the checks alone | covered | c17: jsonl checks; build jsonl: codegen phase |
| 7386 | A build's stream runs the checks, then `codegen`; a failure there is `codegen`'s diagnostic; `build_complete` adds `output` | covered | c17: build jsonl: codegen phase, build_complete has output; tests/tooling_conformance.rs tooling_a_codegen_failure_ends_the_stream |
| 7388 | One JSON object per line, `\n`, not pretty-printed, with `type`; no wrapper | covered | c17: jsonl: every line JSON with type; lines end in LF, not CRLF |
| 7408 | `build_start` once, with `build_id`, `timestamp`, the arguments | covered | c17: jsonl: build_start first with build_id, timestamp, args |
| 7409 | `phase_start` once per observable phase, optional `scope` | covered | c17: jsonl: lex and parse start; c17c: every phase_start is closed by phase_complete or phase_skipped |
| 7410 | `phase_complete`: `phase`, `errors`, `warnings`, `notes` | covered | c17: jsonl: phase_complete fields |
| 7411 | `phase_skipped`: `phase`, `reason`, `blocking` ids | covered | c17: jsonl: phase_skipped has reason and blocking ids |
| 7412 | `diagnostic` is a batch entry plus `phase` | covered | c17: jsonl: diagnostic events carry phase |
| 7413 | `build_complete` once: `build_id`, `success`, totals, `program_effects` | covered | c17: jsonl: build_complete last; build_id matches; jsonl program_effects == batch |
| 7415 | No sub-phase events in the stream | covered | c17: jsonl: every line JSON with type (the event set is the six) |
| 7417 | Clients accept unknown phase names | not testable | a client obligation |
| 7420 | `lex` and `parse` always run; the parser reports every syntax error it finds | covered | c17: jsonl: lex and parse start; tests/parser.rs recovery |
| 7421 | `resolve`, `effect` and `provider_escape` run only if `parse` reported no error | covered | c17: jsonl: parse error skips resolve, typecheck, effect, ownership; c17c: resolve error: effect runs |
| 7422 | `typecheck` runs only if `resolve` reported no error | covered | c17c: resolve error: typecheck skipped |
| 7423 | `ownership` runs only if `typecheck` ran, and runs after type errors | covered | c17c: resolve error: ownership skipped; type error: ownership still runs |
| 7424 | `lint`, `simd_check` and `comptime` always run | partial | c17c: resolve error: lint runs (simd_check and comptime unasserted) |
| 7425 | `codegen` runs only if every analysis phase had zero errors | covered | c17c: build with a type error: codegen does not run |
| 7429 | No `--stop-on-first-error` flag | covered | c17c: there is no --stop-on-first-error flag |
| 7431 | The cascade cap is the same in both modes | covered | c17: cascade cap: same marker in the stream |
| 7439 | Invariants 1 and 2: each mode's diagnostics are in the other | covered | c17: jsonl reconstructs batch diagnostics |
| 7441 | Invariant 3: `total_errors` is the sum of `phase_complete.errors` | covered | c17: jsonl: total_errors == sum of phase_complete errors (and ownership, ok) |
| 7444 | Every batch diagnostic carries `phase` | covered | c17c: every batch diagnostic carries phase |

## Compiler query API (7446–7482)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7450 | `query effects`, `ownership`, `monomorphization`, `queries` each return JSON | covered | c17: query effects / ownership / monomorphization / queries: JSON, exit 0 |
| 7459 | Every inferred decision is reachable through `karac query`, with a `derivation` | covered | c17: query effects process: derivation; query ownership: derivation; c17b: query effects derivation entries {file,line,reason} |
| 7459 | The ownership query reports the body-level classification behind would-be-mode diagnostics | covered | c17b: query ownership (`representation: "ref (borrow)"`, the reason) |
| 7461 | `query queries` returns the compiler queries report | covered | c17: query queries main.kara; tests/specialization_queries.rs |
| 7463 | Batch, one-shot, narrow; no subscription | not testable | a scope statement |
| 7465 | Monomorphization: one entry per generic, instance count, types and site per instance, totals | covered | c17: query monomorphization: shape; id has 2 instances with types and site |

## `karac fix` and `karac explain` (7484–7497)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7486 | `karac fix` applies the machine-applicable diffs | covered | c17: fix: applied unused_mut and redundant_clone; fix: idempotent; check after fix: no machine-applicable left |
| 7488 | `fix` applies exactly what `check` reports and takes the same `-A/-W/-D/-F` | covered | c17b: fix -A / -D / -F / -W agrees with check |
| 7488 | The style lints are off in every command; `-W`, `#[warn]` or `[lints]` turns one on | covered | c17: fix: style lint off by default; c17c: <lint>: off by default in check, for each of the seven |
| 7489 | `counter_while_loop` becomes `for i in a..b` | covered | c17c: counter_while_loop: -W reports it with a fix; fix -W applies it |
| 7490 | `manual_compound_assign`: `x = x + e` becomes `x += e` | covered | c17c: manual_compound_assign |
| 7491 | `needless_return` | covered | c17: fix -W needless_return applies it |
| 7492 | `redundant_literal_method` | covered | c17c: redundant_literal_method |
| 7493 | `redundant_ref_param` | covered | c17c: redundant_ref_param |
| 7494 | `qualified_prelude_variant`, in an expression or a pattern | covered | c17c: qualified_prelude_variant (both) |
| 7495 | `unused_variable`: a binding nothing names becomes `_name` and is kept | covered | c17c: unused_variable: fix -W applies it |
| 7495 | Not reported: only-assigned, parameter (fn and closure), or-pattern name, shorthand field, `@` binding | covered | c17c: unused_variable not reported: assigned, param, `a`, `n`, whole, unused_closure_param |
| 7497 | `explain` a code, and a concept id by its sections | covered | c17: explain E0500, explain move-semantics, explain concept names its codes |
| 7497 | `explain` an item: effects with derivation and would-be mode beside the declared | covered | c17b: explain item shows effects and parameters |
| 7497 | A method as `<file>.Type.method` | covered | c17c: explain <file>.Type.method: effects |
| 7497 | A `with` clause is trusted: the derivation ends at the clause | covered | c17c: explain: a with clause ends the derivation at the clause |

## Language server (7499–7519)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7501 | Stdio, the same analysis `karac check` runs, no user code executed | covered | lsp/tests/server.rs; lsp diag parity (shared folder `conformance/lsp/diag_parity.py`) |
| 7501 | Any request not served answers `MethodNotFound` | covered (new) | lsp/tests/protocol_contract_2026_11_24.rs lsp_contract_unserved_request_is_method_not_found |
| 7505 | `initialize` / `shutdown`; text sync | covered; FIXED text | lsp/tests/server.rs (sync is incremental, B-2026-11-21-33; design.md said full-document) |
| 7506 | `didOpen`/`didChange` check and publish; `didClose` clears | covered | lsp/tests/server.rs server_publishes_diagnostics_and_shuts_down_cleanly, server_clears_diagnostics_on_close |
| 7506 | `didSave` checks and publishes | covered (new) | lsp/tests/protocol_contract_2026_11_24.rs lsp_contract_did_save_checks_and_publishes |
| 7506 | A change inside one body re-checks that body | covered | lsp/tests/incremental.rs, lsp/tests/conformance_incremental.rs |
| 7507 | `hover`: the inferred type, and a function's effects | covered | lsp/tests/server.rs; lsp/tests/hover_as_written_2026_11_24.rs |
| 7508 | `definition` | covered | lsp/tests/server.rs server_answers_definition_and_document_symbols; lsp/tests/imports_nav.rs |
| 7509 | `references` in the document | covered | lsp/tests/conformance_workspace.rs, probe_lsp_rows.rs |
| 7510 | `rename` in the document | covered | lsp/tests/rename_members.rs, conformance_workspace.rs |
| 7511 | `documentSymbol`: top-level items | covered | lsp/tests/server.rs |
| 7512 | `completion` after `.` | covered | lsp/tests/completion.rs, completion_bounds.rs |
| 7513 | `signatureHelp` on `(` and `,` | covered | lsp/tests/signature_hints.rs |
| 7514 | `inlayHint`: a `let`'s type and a private function's effects | covered | lsp/tests/signature_hints.rs, incremental.rs |
| 7515 | `codeAction`: the fixes `karac fix` applies | covered | lsp/tests/server.rs server_offers_quick_fixes_over_the_protocol; shared `conformance/lsp/fix_parity.py` |
| 7516 | `formatting`: `karac fmt`'s output as one whole-document edit | covered | lsp/tests/server.rs server_formats_a_document; shared `conformance/lsp/fmt_parity.py` |
| 7517 | `kara/renderCrashReport` takes the record `karac run --output=json` carries, renders as `karac debug` | covered (new) | lsp/tests/protocol_contract_2026_11_24.rs lsp_contract_render_crash_report_takes_the_run_record; lsp/src/analysis.rs render_crash_report_matches_shared_renderer |
| 7519 | Positions are UTF-16 | covered (new) | lsp/tests/protocol_contract_2026_11_24.rs lsp_contract_positions_are_utf16 |
| 7519 | A diagnostic's `code` is its catalogue code, or its phase when it has none | partial | lsp/tests/server.rs (catalogue code); every diagnostic the probes raised has a code, so the fallback is unexercised |
| 7519 | References and rename do not cross files in v1 | covered | lsp/tests/conformance_workspace.rs |

## REPL (experimental) (7521–7546)

Every sentence is pinned by a cell in `conformance/repl/` (its README maps them): cells run in order (`cells_see_earlier_mutation`), shadowing (`shadowing_ends_nothing`), a lent shown value (`show_lends_the_value`), a refused or panicking cell (`refused_cell_changes_nothing`, `panicking_cell_keeps_nothing`), providers spanning cells (`provider_state_persists`, `no_provider_names_the_cell`), `--auto-clone` (`auto_clone`), `:effects` (`effects_report`), `:save` (`save_session`), `:dep` (`dep_is_recorded`), the meta-commands (`meta_commands`). 14 sentences, all covered.

## Debugger contract (7548–7568)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7550 | Building debugger tools is out of scope; the contract and metadata are the deliverable | not testable | scope |
| 7557 | Metadata on by default in `[profile.dev]`, off in `[profile.release]` | covered | c17c: debug metadata on in dev; release: no debug metadata |
| 7557 | Set by `runtime_debug_metadata` in either profile | FAILS (routed) | `karac build` honors it (tests/mir_debug_metadata.rs mir_debug_metadata_follows_the_manifest_profiles); `karac run` does not (c17c: [profile.dev] runtime_debug_metadata = false turns it off) |
| 7559 | A `SpawnSiteId` per `par {}` block, stable across runs | covered | tests/mir_debug_metadata.rs mir_debug_metadata_lists_running_par_blocks |
| 7560 | A parent-frame reference per worker; the root's is `"root"` | partial | tests/mir_debug_metadata.rs (worker entries; the root sentinel unasserted) |
| 7561 | An await-chain `WaitTarget` per suspended task | covered | tests/mir_debug_metadata.rs mir_debug_metadata_lists_suspended_tasks |
| 7562 | `list_tasks()` / `list_par_blocks()`; empty without the metadata | covered | tests/mir_debug_metadata.rs; c17c: release: no debug metadata, lists empty |
| 7566 | `has_debug_metadata()` reports it | covered | c17c: debug metadata on in dev |
| 7568 | Stable within a major version | not testable | a stability policy |

## Panic record (7570–7587)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7575 | Kind is one of the ten | covered | c17c: panic record kind explicit, index_out_of_bounds, overflow, div_by_zero, unwrap, assertion, unreachable; tests/mir_tasks.rs (deadlock); tests/isolating_alloc_failure.rs (alloc_failure) |
| 7575 | `borrow_conflict` | partial | no probe raises it on the v2 backends |
| 7576 | The message, or `"<no message>"` | FAILS (routed) | a bare `panic()` records `"explicit panic"` (probe by hand; see Drift above) |
| 7577 | Location `file:line:col`; inside a `#[track_caller]` library function, the caller's | covered | c17c: panic record <kind>: location file/line/col; track_caller: unwrap's location is the caller's line; tests/compiled_panic_places.rs |
| 7578 | The error-return trace, in debug builds; none without `?` | covered | c17b: run --output=json: error_return_trace in the record; tests/tooling_conformance.rs |
| 7581 | The record goes to stderr in human-readable form | covered | c17c: built binary run directly: exit 101, human report, no JSON |
| 7582 | The custom panic handler runs after it | covered | c17c: custom panic handler runs after the record, exit 101; tests/mir_build.rs |
| 7583 | Exit code 101 | covered | c17: run panic: exit 101 |
| 7585 | `run --output=json(l)` and `karac test` carry the record; text mode and a built binary have only the human report | covered | c17: run --output=json carries a panic record; run text mode: no JSON panic record; c17b: karac test --output=jsonl / json accepted |
| 7587 | The JSONL event is stable within a major version | not testable | a stability policy |

## Canonical formatting (7589–7598)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7591 | Deterministic output | covered | c17: fmt idempotent |
| 7592 | Imports sorted | covered | c17: fmt sorts imports, keeps a + b / b + a |
| 7592 | `impl` and `trait` bodies: associated types, then no-`self` functions, then methods, alphabetical | covered | c17: fmt orders impl; c17c: fmt trait: associated types first, then no-self fns, then methods alphabetically |
| 7592 | Doc comments, comments and attributes travel with their item | covered | c17c: fmt impl: doc comment and attribute travel; a comment travels with its item |
| 7596 | `a + b` and `b + a` stay as written | covered | c17: fmt sorts imports, keeps a + b / b + a |
| 7598 | Package: no path formats `src/` in place; `--check` writes nothing, lists files, exits 1 | covered | c17: fmt --check in a package; fmt in a package: formats in place; fmt --check clean: exit 0 |
| 7598 | A directory is formatted in place; a file prints to stdout unless `--write` | covered | c17: fmt on a directory; fmt file: prints to stdout; fmt --write writes the same text |

## Lint levels (7600–7639)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7606 | `#[allow]` suppresses | covered | c17b: #[allow] silences |
| 7607 | `#[warn]` emits a warning | covered | c17c: outer allow cascades; inner warn overrides |
| 7608 | `#[deny]` emits an error; the build fails | covered | c17b: #[deny(redundant_clone)] gives E0300 error and fails |
| 7609 | `#[expect]` suppresses and reports `unfulfilled_lint_expectation` if it did not fire | covered | c17: unfulfilled_lint_expectation: reported as warning; c17c: a fulfilled #[expect] reports nothing |
| 7611 | The four attach to an item, block, statement or module level | covered | c17c: #[allow] on a statement: only that statement; c17: accepted silently: #![allow(unused_mut)]; tests/lint_level_scope_rows_2026_11_19.rs |
| 7611 | An inner attribute: only lint levels and `rc_budget`; another is an error naming its item | covered | c17c: inner attribute other than a lint level or rc_budget is an error; tests/lint_level_scope_rows_2026_11_19.rs |
| 7611 | An unknown inner attribute is `E_UNKNOWN_ATTRIBUTE` | covered | tests/lint_level_scope_rows_2026_11_19.rs |
| 7611 | Outer levels cascade; an inner one overrides for the named lint | covered | c17c: outer allow cascades; inner warn overrides for its scope |
| 7611 | One attribute may name several lints | covered | c17c: one attribute names several lints |
| 7611 | The same lint twice in one scope is `E_DUPLICATE_LINT_LEVEL` | covered | c17: E_DUPLICATE_LINT_LEVEL; c17c: the same lint named twice |
| 7613 | `-A/-W/-D/-F`; `-D warnings`; `-F` cannot be overridden by `#[allow]` | covered | c17: -A redundant_clone silences it; -D warnings fails a warning; c17b: -D: a source #[allow] still overrides it; -F: a source #[allow] does not override it |
| 7613 | Lint policy in `kara.toml` is after v1 | not testable | deferred |
| 7624 | The `unfulfilled_lint_expectation` text at the attribute | covered | c17: unfulfilled_lint_expectation: reported as warning |
| 7635 | `#[allow(unfulfilled_lint_expectation)]` suppresses it | covered | c17c: #[allow(unfulfilled_lint_expectation)] suppresses it |
| 7635 | `#[expect(unfulfilled_lint_expectation)]` is `E_EXPECT_ON_UNFULFILLED` | covered | c17: E_EXPECT_ON_UNFULFILLED |
| 7637 | An unknown lint is the `unknown_lint` warning, suppressible | covered | c17: unknown_lint: reported as warning; c17c: #[allow(unknown_lint)] suppresses unknown_lint |
| 7639 | The registry is listed by `karac lint --list`; the v1 set | covered | c17: lint --list names (17 lints); c17c: lint --list names inferred_dependency_effects, self_rebinding, unused_variable |
| 7639 | `self_rebinding` is W0323 with a fix; `unused_variable` W0324, allowed by default | covered | c17c: self_rebinding: warns with code W0323 and a fix; unused_variable is W0324; unused_variable: off by default |
| 7639 | `unsafe_op_in_unsafe_fn` is a hard rule and takes no level | covered | c17c: unsafe_op_in_unsafe_fn takes no level (not a lint) |

## Attributes (7641–7720)

| Line | Sentence | Status | Cells |
|---|---|---|---|
| 7643 | An attribute not in the registry is `E_UNKNOWN_ATTRIBUTE` | covered | c17: E_UNKNOWN_ATTRIBUTE: reported as error |
| 7649 | The v1 core set is in the registry: derive, default, serde, target, repr, non_exhaustive, track_caller, no_effect | covered | c17c: #[<name>] is in the registry |
| 7652 | `#[cfg(...)]` | partial | c17c: cfg is refused as not implemented, naming `#[target]`; line 7653 says `#[target]` is interim until `#[cfg]` is implemented |
| 7655 | `#[must_use]` warns on a discarded value | covered | c17c: #[must_use]: discarding the value warns |
| 7657 | `#[deprecated]` warns at a use | covered | c17c: #[deprecated]: a use warns |
| 7659 | `#[panic_handler]` | covered | c17c: custom panic handler runs after the record |
| 7661 | `#[unstable]`: a use needs `#[allow(unstable_api)]` | covered | c17c: #[unstable]: a use without allow is reported; allow(unstable_api) permits the use |
| 7662 | `#[no_effect]` forbids the effect | covered | c17c: #[no_effect] forbids the effect in the body |
| 7664 | `#[compiler_builtin]`: standard library only | covered | c17c: #[compiler_builtin] outside the standard library is refused |
| 7666 | Tests are blocks, not an attribute | covered | c17c: #[test] is not an attribute |
| 7668 | Arguments are written like a call's: positional and `key: value` | covered | c17c: #[deprecated]: key: value arguments accepted |
| 7668 | No `#[name = value]` form and no `key = value` argument | covered | c17c: #[name = value] form is refused; key = value argument is refused |
| 7668 | `cfg` conditions and `any(...)` repeating a key | not testable | `#[cfg]` is not implemented (row 7652) |
| 7670 | `karac::` holds the derive helpers; an unknown `karac::` name is an error | covered | c17c: an unknown karac:: name is an error; tests/derive_helper_position_check.rs |
| 7670 | Any other multi-segment path is an error | covered | c17: refused: #[tool::name] |
| 7692 | `on_unimplemented` on a trait: `message`, `label`, `note`, `{Self}` interpolated | covered | c17c: on_unimplemented: message with {Self} interpolated; label and note |
| 7692 | An unknown placeholder warns at the trait and renders literally | covered | c17c: on_unimplemented: unknown placeholder warns at the trait and renders literally |
| 7694 | `do_not_recommend` leaves the impl out of the suggestion list | covered | c17c: do_not_recommend: the impl is left out of the suggestion list |
| 7698 | The compiler may ignore a `diagnostic::` attribute without changing code | not testable | a freedom |
| 7700 | An unrecognized `diagnostic::` attribute is silently accepted | covered | c17: accepted silently: #[diagnostic::polish_me] |
| 7701 | A malformed one is the `malformed_diagnostic_attribute` warning; the build proceeds | covered | c17c: malformed diagnostic:: attribute is the malformed_diagnostic_attribute warning |
| 7712 | On an unsupported item kind: the malformed warning, not an error | covered | c17c: on_unimplemented on a fn; do_not_recommend on a fn |
| 7712 | Two `on_unimplemented` on one trait: the warning, and the first is used | covered | c17c: two on_unimplemented: warning, and the first is used |
