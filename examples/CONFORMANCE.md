# Examples conformance

One row per example: what was checked on today's `main` and what it showed.
Audit of 2026-11-25 (Check library docs thread), on a 4-core Linux x86_64
container, with `target/release/karac` built `--features llvm` and fresh
runtime archives.

The lanes are `karac check` (counting warnings), `karac run --interp` (the
MIR interpreter), `karac run` (the JIT), and `karac build` with the binary
run. "Same" means every lane printed the same bytes. A `.expected` or
`expected.txt` beside a program is compared byte for byte.

Rows filed from this audit, in `docs/bugs-v2.jsonl`:

- **B-2026-11-25-56, interp suspend before a diverging loop.** `karac run --interp` stops with
  `String still owns a value at return; a drop is missing` when a program
  waits (accept, recv) inside an endless loop after its last use of an owned
  local. This is what stops ws_idle_holder under the interpreter.
- **B-2026-11-25-57, the playground does not build for wasm32.** `karac-playground` does not
  build for `wasm32-unknown-unknown`, so cartographer's `studio.sh` fails.
- **B-2026-11-25-58, `query ownership` contradicts itself.** Its derivation text gives a
  reason that contradicts the mode it reports (tangle).

The `Flag` column marked an example that might no longer show idiomatic 0.8
Kāra; it now records the review thread's ruling on each (2026-11-25).

## Top-level programs

| Example | Checked | Result | Changed | Flag |
|---|---|---|---|---|
| array_basics.kara | check, interp, run, build | same; 0 warnings after fix | 2 deprecated `Array[..]` prefix literals replaced by typed `[..]` literals (`karac fix`'s edit) | |
| elevator.kara | check, interp, run, build | same (no .expected) | | |
| heap.kara | check, interp, run, build, .expected | same, matches | header: "tree-walk interpreter" → MIR interpreter | |
| json.kara | check, interp, run, build, .expected | same, matches | header wording as heap | |
| pipeline.kara | check, interp, run, build, .expected | same, matches | header wording as heap | |
| protobuf_schema.kara | check, interp, run, build | same (no .expected) | | |
| sha256.kara | check, interp, run, build, ASan, .expected | same, matches, 0 warnings | rewritten on `u32` words: `wrapping_add`, `rotate_right`, `~`, `u32.from_be_bytes` / `to_be_bytes` (review ruling: no u64 workaround) | |
| sigv4.kara | check, interp, run, build, .expected | same, matches | | |
| slice_basics.kara | check, interp, run, build | same (no .expected) | | |
| vm.kara | check, interp, run, build, .expected | same, matches | | |
| word_count.kara | check, interp, run, build | same (no .expected) | | |

## Directories

| Example | Checked | Result | Changed | Flag |
|---|---|---|---|---|
| cartographer | run output; README `query` claims (get_dashboard parallel group [0,1,2], double_audit serialized on AuditLog via record_access); cartograph.sh; studio source checks | holds | build_dashboard row now names the default-permitted `allocates(Heap)`; graph.js regenerated | studio.sh fails (B-2026-11-25-57) |
| crash | `karac debug` on all 3 fixtures (tests/cli.rs pins them) | render, exit 0 | | |
| db_pipeline | check, run, build | same output | 3 deprecated `Map[..]` prefix literals fixed by `karac fix`; 0 warnings | |
| deferred | check | does not check, as its README says; excluded from tests/example_corpus.rs and conformance | README now lists what each program waits for; `interop/` moved here (review ruling) | |
| elevator_project | check, interp, run, build | same, 0 warnings | | |
| fathom | check; README banner | browser-only: needs `--target=wasm_browser`, not in 0.8; check gives E0411 (Display, Timer on native) + 1 must_use warning; pinned as known not to check in tests/example_corpus.rs | pin reason names wasm_browser | Kept, as Prism/Veil (review ruling) |
| game_of_life | check, interp, run, build | same, 0 warnings | | |
| interop → deferred/interop | check against its banner | refusals match the banner | moved to examples/deferred/interop (waits for deferred.md § Exported C ABI); links updated | |
| interop-zlib | check, interp, run, build, expected output | 4 lanes identical, matches | "`karac run` and FFI" rewritten: all three lanes honor `[link]` via dlopen; `--interp` added to verified surfaces | |
| iris | native build, run, interp, `KARAC_AUTO_PAR=0` | checksums agree across lanes (no independent oracle); browser leg not in 0.8, as the banner says | | |
| leetcode | 9 programs: check, interp, run, build; Python mirrors | all match; mirrors match after normalizing number format (max_depth's mirror prints extra lines for extra methods) | | |
| mend | 5 solutions with expected.txt under run and build; converge test; test_mend_score; 4 dry-runs | match; pass; converge (4 solutions have no expected.txt) | layout tree lists all 9 examples and the .cast | |
| parallax | check, build, `query concurrency`, timing with and without auto-par | 0.027 s both; reason `writes(Stdout)` keeps order | README cites the rule: auto-par runs only what the `par` conflict test proves independent (design.md, core-semantics.md §11.5), and two `writes(Stdout)` conflict; not a compiler gap | |
| parallax_lite | build, timing, `query concurrency` | auto-par runs the 3 calls: 0.020 s vs 0.050 s with `KARAC_AUTO_PAR=0`, 0.018 s with written `par` | banner, query description (`tasks: true`) and Measured table updated | |
| phase0 → docs/legacy/examples/phase0 | run, `query`, bench.sh | match README; bench.sh 3.0x (it times the Rust models) | moved out of examples/ (review ruling: a design-era mockup); corpus entry keeps its copy, source path updated | |
| plume | check; README banner | as fathom | | Kept, as fathom |
| relay | check, run with Python backends, metrics line every 10 requests (under a pty), bench dry-run | works; benchmarks not re-measured (no wrk) | | |
| shortener | every route under run, build, interp | match | header: both backends read the `headers` field; test description says "the compiled backend" | Own `struct Response` under `#[allow(prelude_shadow)]` is the io.md pattern (review ruling) |
| slipstream | run, build, `KARAC_AUTO_PAR=0`, checksums, packed-double count | 1582897806 / 793640938 / 680974524 match; 172 packed-double ops; interp matched its first 2 lines, then ran over 10 min | two sin/cos "stdlib gap" claims updated (`f64.sin/cos/atan2` exist) | |
| ssr_counter | check, server leg | server leg runs; client leg needs wasm_browser | banner: client leg not in 0.8, later_track warning explained; "CI E2E" now "pre-M3 CI E2E" | |
| std_net | http_hello under `/hello`; ws_echo and ws_echo_spawn with ws_loop_client.py; https check only | pass | table rows for ws_echo_spawn and ws_echo_freespawn (not 0.8), client script line, qualified "compile clean" | ws_echo_freespawn.kara moved to docs/legacy/examples/std_net (free `spawn` was removed, so it waits for nothing; the IOCP spike cites it) | Own `struct Response` is the io.md pattern (review ruling) |
| tangle | all 5 programs' output; `query` claims; ASan/LSan | match; LSan-clean after fix | parent_tree's back-edge is now `weak TreeNode` (it leaked 6 nodes); E0500 snippet, jsonc excerpt and Status rewritten | doubly_linked keeps its strong `prev` to show the cycle: LSan is clean (one node at exit), so it stays (review ruling); README says a longer-lived list declares it `weak` |
| tests | check, `karac test` (interp and default), `karac run`, build | 5 of 5 tests pass; a wrong assertion fails 3 | made a 0.8 package: `mut ref self`, `own` key and value, `.cloned()`, tests in src/main_test.kara and a small `main`; pinned in tests/example_packages.rs | |
| wasm_hello | `--target=wasm_wasi` build, `node run_wasi.mjs`, native | 42 / 610 on both | | |
| weave | check, interp, run, build | same | README lists the three lanes | |
| ws_idle_holder | check, build, README smoke test verbatim, a TLS WebSocket echo under build, run, interp | build and run: 101 + MATCH, echo returns; interp stops at the first accept (B-2026-11-25-56) | README build steps (release karac, `dist/native/ws_idle_holder`, `KARAC_ANNOUNCE_PORT=1`, which the smoke test needs to see the port); "why inlined" (the `fs.read_to_string` gap is closed); header comment claims about loading from disk and stdout | |

The bench rigs (ws_idle_holder/bench, relay/bench, parallax/bench) need
hosts or tools this container does not have and were not re-measured; their
reports keep the dates they were measured on.
