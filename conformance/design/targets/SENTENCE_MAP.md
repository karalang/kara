# design.md §16 Targets sentence-to-cell map (lines 6924–7207)

One row per sentence of design.md §16 Targets, as of 2026-11-25, naming the cell in this directory (or the test elsewhere) that covers it. NEW marks a cell added by this audit; FAILS marks what is not yet true of the compiler, with its ledger row; FIXED marks a gap this audit fixed. Warn-only behaviour (a lint that warns and lets the program build) is mapped to its test, since a cases.py cell cannot assert a warning.

| # | Section | Sentence | Cell(s) |
|---|---|---|---|
| s16.1 | The v1 target set | Kāra v1 has a **closed set** of compilation targets: | build_unknown_target_refused, targets_unknown_refused (the set is closed), native_artifact_path, wasm_artifact_path |
| s16.2 | The v1 target set | / Target / Purpose / /---/---/ | build_unknown_target_refused, targets_unknown_refused (the set is closed), native_artifact_path, wasm_artifact_path |
| s16.3 | The v1 target set | / `native` / Host-architecture executable or library through LLVM: | build_unknown_target_refused, targets_unknown_refused (the set is closed), native_artifact_path, wasm_artifact_path |
| s16.4 | The v1 target set | CLI tools, servers, compute programs. / | build_unknown_target_refused, targets_unknown_refused (the set is closed), native_artifact_path, wasm_artifact_path |
| s16.5 | The v1 target set | / `wasm_wasi` / A WebAssembly module for WASI hosts (wasmtime, node and other headless runtimes). / | build_unknown_target_refused, targets_unknown_refused (the set is closed), native_artifact_path, wasm_artifact_path |
| s16.6 | The v1 target set | The set is closed so that the effect checker's target-to-resource table is finite and `karac check` has a bounded number of configurations. | rationale |
| s16.7 | The v1 target set | The [web track](deferred.md#web) adds `wasm_browser` and the [GPU track](deferred.md#gpu) adds `gpu`. | build_later_track_target_refused, targets_later_track_refused, later_track_target_attribute_warns |
| s16.8 | The v1 target set | Opening the set to user-defined targets is additive. | build_later_track_target_refused, targets_later_track_refused, later_track_target_attribute_warns |
| s16.9 | The v1 target set | Under node, run a `wasm_wasi` module with `node --no-turbo-fast-api-calls`. | tests/mir_wasm.rs, tests/build_wasm_bundle.rs (node --no-turbo-fast-api-calls; needs node and the wasm archives) |
| s16.10 | The v1 target set | In node 22 a garbage collection that starts inside a WASI `fd_write` fast-API call can crash V8 itself with SIGSEGV, outside the module's me | tests/mir_wasm.rs, tests/build_wasm_bundle.rs (node --no-turbo-fast-api-calls; needs node and the wasm archives) |
| s16.11 | The v1 target set | How deep a program can recurse on `wasm_wasi` is bounded by two stacks, and the smaller one wins. | wasm_stack_size_out_of_range_refused; the depth limits are tests/mir_wasm_capacity.rs (needs node and the wasm archives) |
| s16.12 | The v1 target set | The module's own shadow stack, in linear memory, holds every frame that spills (locals whose address is taken, arrays, large structs); it is | wasm_stack_size_out_of_range_refused; the depth limits are tests/mir_wasm_capacity.rs (needs node and the wasm archives) |
| s16.13 | The v1 target set | Running out of it traps as "memory access out of bounds" (B-2026-11-19-44). | wasm_stack_size_out_of_range_refused; the depth limits are tests/mir_wasm_capacity.rs (needs node and the wasm archives) |
| s16.14 | The v1 target set | The host engine's stack holds the rest of each frame: node's default (about 1 MB) ends near 20,000 frames of a small recursive function, and | wasm_stack_size_out_of_range_refused; the depth limits are tests/mir_wasm_capacity.rs (needs node and the wasm archives) |
| s16.15 | The v1 target set | A native build runs `main` on a 1 GiB stack, so a recursion that passes natively can still need one or both raised on `wasm_wasi`. | rule (the native 1 GiB main stack: deep_shared_chain_drops_take_no_frames runs a million-deep chain) |
| s16.16 | The v1 target set | The compiler's own drops of a deep `shared` chain or tree take no frames per level (B-2026-11-16-311). | NEW deep_shared_chain_drops_take_no_frames (run and build). FAILS on `karac run --interp`: the interpreter's release recurses once per level and a 1,000,000-node chain aborts with a Rust stack overflow (300,000 drops), B-2026-11-25-27 |
| s16.17 | The v1 target set | An item that exists on some targets only is marked with `#[cfg(target: ...)]` ([§4](#4-modules-and-packages)). | NEW a_target_gated_item_is_seen_only_by_its_target (the interim `#[target]` spelling; design.md §4 says `#[cfg]` is not yet implemented) |
| s16.18 | The v1 target set | There is no runtime `if target == ...` form. | NEW there_is_no_runtime_target_test |
| s16.19 | Resources each target provides | Each target provides a subset of the built-in resources ([§12](#12-effects)). | targets_matrix_network_refused_on_wasm_only, NEW process_table_on_wasm_refused_with_the_chain |
| s16.20 | Resources each target provides | A function whose inferred effect set uses a resource the target does not provide is rejected when compiling for that target. | targets_matrix_network_refused_on_wasm_only, NEW process_table_on_wasm_refused_with_the_chain |
| s16.21 | Resources each target provides | / Resource / `native` / `wasm_wasi` / /---/---/---/ | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.22 | Resources each target provides | / `FileSystem` / ✓ / ✓ / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.23 | Resources each target provides | / `Stdin` / `Stdout` / `Stderr` / ✓ / ✓ / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.24 | Resources each target provides | / `Env` / ✓ / ✓ (from the host) / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.25 | Resources each target provides | / `Network` / ✓ / no / | targets_matrix_network_refused_on_wasm_only, single_declared_target_checks_that_target |
| s16.26 | Resources each target provides | / `Clock` / ✓ / ✓ / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.27 | Resources each target provides | / `RandomSource` / ✓ / ✓ / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.28 | Resources each target provides | / `Heap` / ✓ / ✓ / | NEW wasm_provides_files_stdio_env_clock_random_heap |
| s16.29 | Resources each target provides | / `ProcessTable` / ✓ / no / | NEW process_table_on_wasm_refused_with_the_chain |
| s16.30 | Resources each target provides | `Network` is not provided on `wasm_wasi` in v1: | targets_matrix_network_refused_on_wasm_only (a TCP socket; the HTTP client and servers are tests/cli.rs) |
| s16.31 | Resources each target provides | WASI preview 1 has no socket `connect` or `bind`, so a TCP or UDP socket, the HTTP client or a server is refused when compiling for it, with | targets_matrix_network_refused_on_wasm_only (a TCP socket; the HTTP client and servers are tests/cli.rs) |
| s16.32 | Resources each target provides | It returns with a WASI version that has sockets. | deferred |
| s16.33 | Resources each target provides | `Env` on `wasm_wasi` is what the WASI host hands the module: | NEW wasm_provides_files_stdio_env_clock_random_heap (accepted on wasm_wasi); what the host passes is tests/mir_wasm.rs |
| s16.34 | Resources each target provides | `env.args()` and `env.var` see only the arguments and variables the host passes in (wasmtime passes no variables unless asked with `--env`;  | NEW wasm_provides_files_stdio_env_clock_random_heap (accepted on wasm_wasi); what the host passes is tests/mir_wasm.rs |
| s16.35 | Resources each target provides | The calls behave as on `native` over that set. | NEW wasm_provides_files_stdio_env_clock_random_heap (accepted on wasm_wasi); what the host passes is tests/mir_wasm.rs |
| s16.36 | Resources each target provides | Pure-computation library surfaces with no resource, such as `String.normalize` and `std.regex`, work on both targets. | tests/mir_wasm_opt_in.rs (needs the wasm archives) |
| s16.37 | Resources each target provides | On `wasm_wasi` they link from opt-in runtime archives (`libkarac_runtime_wasm_{unicode,regex}.a`), which the driver selects from the symbols | tests/mir_wasm_opt_in.rs (needs the wasm archives) |
| s16.38 | Resources each target provides | A missing archive fails at link with the command that builds it, not with an undefined symbol (B-2026-11-13-1, 637125377c). | tests/mir_wasm_opt_in.rs (needs the wasm archives) |
| s16.39 | Resources each target provides | User-defined resources have no target affinity. | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches, NEW a_user_resource_reaching_network_is_gated_on_wasm |
| s16.40 | Resources each target provides | The target gate does not see a user resource directly, only the built-in resources it reaches transitively. | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches, NEW a_user_resource_reaching_network_is_gated_on_wasm |
| s16.41 | Effect-driven target gating | A function compiles for target `T` when its full inferred effect set uses only resources `T` provides. | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches |
| s16.42 | Effect-driven target gating | This is the main mechanism for cross-target correctness. | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches |
| s16.43 | Effect-driven target gating | Shared code that uses only target-agnostic effects is portable with no annotations. | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches |
| s16.44 | Effect-driven target gating | ```kara,uses=d-page-cache // Target-agnostic: uses only `Network` and a user resource. // Compiles for native and wasm_wasi. pub fn fetch_an | NEW a_user_resource_has_no_target_affinity_only_what_it_reaches |
| s16.45 | Effect-driven target gating | A function that reaches `ProcessTable` (spawning a process, for example) compiles for `native` only. | NEW process_table_on_wasm_refused_with_the_chain |
| s16.46 | Effect-driven target gating | Compiling it for `wasm_wasi` is an error at the target-gate phase. | NEW process_table_on_wasm_refused_with_the_chain |
| s16.47 | Effect-driven target gating | The compiler runs the target gate **after** effect inference, once per target in the build matrix. | NEW process_table_on_wasm_refused_with_the_chain, NEW a_user_resource_reaching_network_is_gated_on_wasm (E0411 with the chain; no warning form) |
| s16.48 | Effect-driven target gating | A function reachable from an entry point that uses a resource the target lacks is error `E0411`, which shows the chain from the entry point  | NEW process_table_on_wasm_refused_with_the_chain, NEW a_user_resource_reaching_network_is_gated_on_wasm (E0411 with the chain; no warning form) |
| s16.49 | Effect-driven target gating | It is an error whenever the build or check is for that target; there is no warning form. | NEW process_table_on_wasm_refused_with_the_chain, NEW a_user_resource_reaching_network_is_gated_on_wasm (E0411 with the chain; no warning form) |
| s16.50 | Checking several targets | When a package declares several targets, `karac check` type-checks and effect-checks the source tree **once per target**, with that target's | targets_matrix_check_tags_and_dedups |
| s16.51 | Checking several targets | - **Declaration.** The manifest declares `[build] targets = ["native", "wasm_wasi"]`, an array of target names. | plain_build_builds_each_declared_target, build_target_flag_builds_one |
| s16.52 | Checking several targets | It is distinct from `[build].target`, a rustc-style triple that selects the per-target manifest overlay for `karac build`. | plain_build_builds_each_declared_target, build_target_flag_builds_one |
| s16.53 | Checking several targets | - **Building.** A plain `karac build` builds each declared target in order, as `--target=<name>` would, and stops at the first that fails; ` | plain_build_builds_each_declared_target, plain_build_stops_at_first_failing_target, build_target_flag_builds_one |
| s16.54 | Checking several targets | `karac run` runs on the host, so it refuses a package whose targets leave out `native`, naming them (review-thread ruling 2026-11-16). | run_refuses_package_without_native |
| s16.55 | Checking several targets | - **Errors.** Unknown names, duplicates, a value that is not an array and an empty array are hard manifest errors. | targets_unknown_refused, targets_duplicate_refused, targets_not_a_list_refused, targets_empty_refused |
| s16.56 | Checking several targets | A soft warning would let a typo silently drop a target from a CI matrix; a package that means the host leaves the key out (review-thread rul | targets_unknown_refused, targets_duplicate_refused, targets_not_a_list_refused, targets_empty_refused |
| s16.57 | Checking several targets | - **Discovery.** `karac check <file>` finds the manifest by walking upward from the file's directory, the same rule as `karac run`. | check_targets_override_multi_file, single_declared_target_checks_that_target_multi_file (walk-up) |
| s16.58 | Checking several targets | - **Override.** `karac check <file> --targets=<list/all>`. | check_targets_override |
| s16.59 | Checking several targets | - **Each pass sees one target.** Each pass re-parameterizes the provided-resource set and the `#[cfg(target: ...)]` filtering, so it sees ex | NEW a_target_gated_item_is_seen_only_by_its_target |
| s16.60 | Checking several targets | - **Deduplication.** A finding identical on every target is reported once, in an "all targets" group (text) or a `shared_diagnostics` array  | targets_matrix_check_tags_and_dedups (JSONL target_start/target_complete: tests/cli.rs) |
| s16.61 | Checking several targets | JSONL output streams each target between `target_start` and `target_complete` events and leaves deduplication to the consumer. | targets_matrix_check_tags_and_dedups (JSONL target_start/target_complete: tests/cli.rs) |
| s16.62 | Checking several targets | - **One target.** A single declared target is not a matrix but still runs with that target's resource set: a `wasm_wasi`-only package is che | single_declared_target_checks_that_target, single_declared_target_checks_that_target_multi_file |
| s16.63 | Checking several targets | Check never checks the host alone when the package or the command names another target (review-thread ruling 2026-11-21, B-2026-11-22-31). | single_declared_target_checks_that_target, single_declared_target_checks_that_target_multi_file |
| s16.64 | CPU baseline | Within the `native` target, karac keeps a **per-target-triple CPU baseline table** that mirrors `rustc`'s target defaults. | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.65 | CPU baseline | LLVM's `"generic"` setting emits ARMv8.0-A on aarch64 and the original AMD64 baseline on x86_64: conservative, and right for fleets with var | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.66 | CPU baseline | Where the hardware is curated (every `aarch64-apple-darwin` machine is Apple Silicon M1 or newer), the generic baseline is strictly worse, w | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.67 | CPU baseline | / Target triple / Default `cpu` / Default `features` / Rationale / /---/---/---/---/ | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.68 | CPU baseline | / `aarch64-apple-darwin` / `apple-m1` / (M1 ISA implied by CPU) / Apple-controlled fleet; M1 is the lowest shipping Apple Silicon. | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.69 | CPU baseline | M2/M3/M4/M5 are supersets. / | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.70 | CPU baseline | / `aarch64-unknown-linux-gnu` / `generic` / `+v8a,+outline-atomics` / Fleet variance (Pi, Graviton 1–4, NXP, Ampere) requires a conservative | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.71 | CPU baseline | / `x86_64-unknown-linux-gnu` / `x86-64-v2` / (none) / The x86_64 floor: | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.72 | CPU baseline | SSE4.2 and POPCNT, on every x86_64 CPU since about 2009 and the baseline of current server distributions. / | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.73 | CPU baseline | / `x86_64-apple-darwin` / `x86-64-v2` / (none) / The same floor; every Intel Mac macOS still supports is above it. / | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.74 | CPU baseline | / _(any other triple)_ / `generic` / (none) / Safe fallback: a portable binary beats one that will not load. / | default_cpu_is_x86_64_v2 (x86_64 Linux host; other triples are tests in src/backend) |
| s16.75 | CPU baseline | **Contract.** A `karac build` with no override produces a binary that runs on every CPU the target triple promises, with `x86-64-v2` as the  | default_cpu_is_x86_64_v2 |
| s16.76 | CPU baseline | Raising the baseline (`apple-m1` to `apple-m4`, `x86-64-v2` to `x86-64-v3`) narrows the deploy set for sharper code. | target_cpu_flag_lowers_baseline, rule |
| s16.77 | CPU baseline | That is the user's explicit choice, never a silent default change. | target_cpu_flag_lowers_baseline, rule |
| s16.78 | CPU baseline | Kāra's published benchmarks build with `x86-64-v3` named, beside `rustc -C target-cpu=x86-64-v3` and `clang -march=x86-64-v3`, so their comp | target_cpu_flag_lowers_baseline, rule |
| s16.79 | CPU baseline | **CPU override**, in precedence order: | target_cpu_flag_beats_env, target_cpu_flag_lowers_baseline |
| s16.80 | CPU baseline | 1. | target_cpu_flag_beats_env, target_cpu_flag_lowers_baseline |
| s16.81 | CPU baseline | `--target-cpu=<name>` on `karac build`, in single-file and project mode; | target_cpu_flag_beats_env, target_cpu_flag_lowers_baseline |
| s16.82 | CPU baseline | 2. | target_cpu_env, target_cpu_env_beats_manifest |
| s16.83 | CPU baseline | `KARAC_TARGET_CPU=<name>`; | target_cpu_env, target_cpu_env_beats_manifest |
| s16.84 | CPU baseline | 3. | target_cpu_manifest_applies_without_release |
| s16.85 | CPU baseline | `[profile.release] target-cpu = "<name>"` in `kara.toml` (found by the `karac run` walk-up rule in single-file mode; the project's manifest  | target_cpu_manifest_applies_without_release |
| s16.86 | CPU baseline | It applies to every `karac build`, with or without `--release`, since there is no separate development code-generation profile (review-threa | target_cpu_manifest_applies_without_release |
| s16.87 | CPU baseline | An unknown CPU name for the active target is a hard error listing the supported names. | target_cpu_unknown_refused, target_cpu_unknown_in_manifest_refused |
| s16.88 | CPU baseline | (LLVM's own behaviour, warning and falling back to `generic`, is exactly the silent baseline loss this prevents.) `karac build --target-cpu= | target_cpu_help |
| s16.89 | CPU baseline | An override replaces the CPU only; the table's feature string stays, so aarch64 Linux keeps `+outline-atomics` under `--target-cpu=neoverse- | target_features_add_and_remove (aarch64 Linux's +outline-atomics is not reachable on this host) |
| s16.90 | CPU baseline | `--target-cpu=native` is available for deploys where the runtime hardware matches the build host; it is never the default. | rule (never the default: default_cpu_is_x86_64_v2) |
| s16.91 | CPU baseline | **Feature override.** `--target-features="+feat,-feat,..."` adjusts the baseline's feature set, with the same precedence resolved independen | target_features_add_and_remove, target_features_remove_default |
| s16.92 | CPU baseline | `--target-features=<list>`, then `KARAC_TARGET_FEATURES=<list>`, then `[profile.release] target-features = "<list>"`, which like `target-cpu | target_features_add_and_remove, target_features_remove_default |
| s16.93 | CPU baseline | - The list is appended after the table's default features, and LLVM resolves duplicates last-wins, so `-outline-atomics` really disables a t | target_features_remove_default |
| s16.94 | CPU baseline | - Every token needs an explicit `+` or `-` and must name a feature in LLVM's registry for the active target; anything else is a hard error w | target_features_need_sign_refused, target_features_unknown_refused |
| s16.95 | CPU baseline | `karac build --target-features=help` prints it. | target_features_need_sign_refused, target_features_unknown_refused |
| s16.96 | CPU baseline | - The two knobs compose: | target_features_add_and_remove |
| s16.97 | CPU baseline | `--target-cpu=neoverse-v1 --target-features=-sve` builds the Neoverse baseline with SVE code generation suppressed. | target_features_add_and_remove |
| s16.98 | CPU baseline | The baseline is the floor every function compiles against. | rule |
| s16.99 | CPU baseline | Per-function feature dispatch (multiversioning) is deferred to the systems track. | rule |
| s16.100 | Concurrency across targets | `par {}`, `par for`, `TaskGroup` and channels are specified once, at the source level ([§13](#13-concurrency)). | conformance/design/concurrency (the same cells under KARAC_THREADS=1 and 4) |
| s16.101 | Concurrency across targets | How they run is a lowering choice per target; the source language does not change. | conformance/design/concurrency (the same cells under KARAC_THREADS=1 and 4) |
| s16.102 | Concurrency across targets | **Commitments on every target:** | conformance/design/concurrency |
| s16.103 | Concurrency across targets | 1. | conformance/design/concurrency |
| s16.104 | Concurrency across targets | **Task semantics.** Ownership transfer into a task, effects charged at the spawning site, and completion are the same on `native` and `wasm_ | conformance/design/concurrency |
| s16.105 | Concurrency across targets | 2. | conformance/design/concurrency a_sent_value_is_moved_refused (a check error, so every target) |
| s16.106 | Concurrency across targets | **Channel ownership transfer.** A sent value moves to the receiver. | conformance/design/concurrency a_sent_value_is_moved_refused (a check error, so every target) |
| s16.107 | Concurrency across targets | Using it after the send is a compile error on every target. | conformance/design/concurrency a_sent_value_is_moved_refused (a check error, so every target) |
| s16.108 | Concurrency across targets | 3. | conformance/design/concurrency par_tuple_in_source_order_whatever_finishes_first_karac_threads_1/_4 (one worker runs them in sequence) |
| s16.109 | Concurrency across targets | **`par {}` and `par for`** mean the same on every target. | conformance/design/concurrency par_tuple_in_source_order_whatever_finishes_first_karac_threads_1/_4 (one worker runs them in sequence) |
| s16.110 | Concurrency across targets | A target that cannot run branches in parallel runs them one after another, with no source change. | conformance/design/concurrency par_tuple_in_source_order_whatever_finishes_first_karac_threads_1/_4 (one worker runs them in sequence) |
| s16.111 | Concurrency across targets | 4. | rule (core-semantics §11.6) |
| s16.112 | Concurrency across targets | **Data-race freedom** is a language guarantee ([core-semantics.md §11.6](core-semantics.md#11-concurrency-c9)) and holds on every target. | rule (core-semantics §11.6) |
| s16.113 | Concurrency across targets | **Lowering:** | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.114 | Concurrency across targets | / Target / `par`, `TaskGroup` and channel lowering / /---/---/ | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.115 | Concurrency across targets | / `native` / OS threads and the runtime scheduler. / | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.116 | Concurrency across targets | / `wasm_wasi` / Cooperative and sequential, on the main thread. / | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.117 | Concurrency across targets | On `wasm_wasi` the runtime carries a cooperative FIFO scheduler. | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.118 | Concurrency across targets | Spawning a task enqueues it, a join point runs the queue, and `par {}` runs its branches in source order. | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.119 | Concurrency across targets | There are no extra threads and no shared memory. | tests/mir_wasm.rs (the cooperative scheduler; needs node and the wasm archives) |
| s16.120 | Concurrency across targets | Channel `send`, `recv` and `try_recv` work on every target. | NEW recv_ends_when_the_producer_task_drops_its_sender |
| s16.121 | Concurrency across targets | On `native`, `recv` blocks until a value arrives or every `Sender` has been dropped; a `Sender` moved into a task is dropped by that task, s | NEW recv_ends_when_the_producer_task_drops_its_sender |
| s16.122 | Concurrency across targets | On `wasm_wasi`, `recv` on an empty channel yields to the other tasks; if no task can run, the program panics with kind `deadlock` ([Panic re | tests/mir_wasm.rs (needs node and the wasm archives) |
| s16.123 | Concurrency across targets | The native runtime and the MIR interpreter detect the same state, every task waiting on a channel no running task can serve, and stop the pr | NEW a_receive_no_task_can_serve_is_a_deadlock_panic (run --interp, run, build) |
| s16.124 | Concurrency across targets | Shared-memory threads on WebAssembly (`--features wasm-threads`) and compiler-managed transparent threading on WebAssembly belong to the [we | deferred (web track) |
| s16.125 | Concurrency across targets | The commitments above keep both open without a source change. | deferred (web track) |
| s16.126 | Build artifacts | `karac build --target T` produces a target-specific artifact set under `dist/`. | native_artifact_path, build_target_flag_builds_one |
| s16.127 | Build artifacts | The layout is stable, so downstream tools can integrate without per-project adaptation. | native_artifact_path, build_target_flag_builds_one |
| s16.128 | Build artifacts | - **`native`.** A single executable at `dist/native/<pkg>` (no extension on Unix; `.exe` on Windows). | native_artifact_path, build_target_flag_builds_one |
| s16.129 | Build artifacts | A library package has no artifact of its own in v1: | library_package_has_no_artifact |
| s16.130 | Build artifacts | `karac build` checks it, and the packages that depend on it compile it into theirs. | library_package_has_no_artifact |
| s16.131 | Build artifacts | Static and dynamic libraries for other languages arrive with the [exported C ABI](deferred.md#exported-c-abi) (review-thread ruling 2026-11- | library_package_has_no_artifact |
| s16.132 | Build artifacts | - **`wasm_wasi`.** A core WebAssembly module for WASI preview 1 at `dist/wasm/<pkg>.wasm`, with the package name from `kara.toml`; a single- | wasm_artifact_path, single_file_wasm_artifact_in_working_directory |
| s16.133 | Build artifacts | The module exports `_start`, which runs `main`. | tests/cli.rs, tests/mir_wasm.rs (`_start` runs main; `kara_host` imports) |
| s16.134 | Build artifacts | Each `host fn` is an import in the `kara_host` namespace ([Lowering on `wasm_wasi`](#lowering-on-wasm_wasi)). | tests/cli.rs, tests/mir_wasm.rs (`_start` runs main; `kara_host` imports) |
| s16.135 | Build artifacts | The Component Model form, browser bindings and the export of other functions arrive with the [web track](deferred.md#web), and the `--bindin | deferred (web track) |
| s16.136 | Build artifacts | - **Every build** runs `strip -x` on the linked binary and passes linker dead-code elimination flags: | build_strips_local_symbols |
| s16.137 | Build artifacts | `-Wl,-dead_strip` on macOS and `-Wl,--gc-sections` on Linux ELF. | build_strips_local_symbols |
| s16.138 | Build artifacts | The MIR builder emits only the functions the program reaches, so the program's own object needs no per-function sections (review-thread ruli | build_strips_local_symbols |
| s16.139 | Build artifacts | A structured manifest describing the artifact set (`.karapack`) is deferred to the [tooling track](deferred.md#tooling). | deferred (tooling track) |
| s16.140 | Build profiles | `kara.toml` has two build profiles: | tests/mir_debug_metadata.rs (runtime_debug_metadata in the profiles) |
| s16.141 | Build profiles | `[profile.dev]` for ordinary builds and `[profile.release]` for `karac build --release`. | tests/mir_debug_metadata.rs (runtime_debug_metadata in the profiles) |
| s16.142 | Build profiles | Settings that differ between debug and release builds, such as `runtime_debug_metadata` ([Debugger contract](#debugger-contract)), go in the | tests/mir_debug_metadata.rs (runtime_debug_metadata in the profiles) |
| s16.143 | Build profiles | Project profiles, which restrict a whole project (`embedded`, `kernel`), are deferred to the [systems track](deferred.md#systems) and return | deferred (systems track) |
| s16.144 | Compilation model | An AI author compiles often, and a slow compiler bottlenecks the loop. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.145 | Compilation model | The architecture keeps fast iteration possible without requiring a full incremental compilation framework in v1. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.146 | Pipeline | ``` lex → parse → desugar → resolve → typecheck → typed HIR typed HIR → MIR builder (per function, monomorphised) MIR → borrow check, drop e | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.147 | Pipeline | - **Typed HIR.** Every node has a node id. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.148 | Pipeline | Every item has a definition id, qualified by module and package. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.149 | Pipeline | Types are interned, and each call records its resolved callee and type substitution. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.150 | Pipeline | - **MIR** is built per (definition, substitution), as a pure function of the typed HIR, so it can be cached. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.151 | Pipeline | It has locals, places (a local plus a projection path), operands and rvalues; the statements `Assign`, `StorageLive`, `StorageDead` and `Set | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.152 | Pipeline | - **No unwind edges.** A panic aborts ([core-semantics.md §10](core-semantics.md#10-panics-and-errors-c8)). | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.153 | Pipeline | Every `Call` and `Drop` terminator carries a cleanup target, which is always `Abort` in v1. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.154 | Pipeline | - **Checks on MIR.** Borrow checking, drop elaboration ([core-semantics.md §7](core-semantics.md#7-destruction-c6-supersedes-the-drop-judgme | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.155 | Pipeline | - **The MIR interpreter is the reference semantics.** It tracks every allocation, so a double drop, a use after move, a leak or a read of a  | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.156 | Pipeline | It runs without LLVM. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.157 | Pipeline | - **MIR → LLVM** produces native code: type layouts, drop and clone glue, debug info and linking. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.158 | Principles | **1. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.159 | Principles | Function-local analysis by default.** Type checking, effect inference (within a strongly connected component of mutually recursive private f | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.160 | Principles | Whole-module traversals need a justification. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.161 | Principles | This is the shape a future incremental layer would key on; whole-module sweeps would lock out function-keyed caching. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.162 | Principles | **2. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.163 | Principles | The SCC is the cache unit for effect inference.** Effect inference already groups mutually recursive private functions into strongly connect | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.164 | Principles | That SCC is the natural invalidation unit for a future incremental layer, so the implementation keeps SCC boundaries. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.165 | Principles | **3. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.166 | Principles | Named dependencies between phases.** Each analysis result records what it consumed: called function signatures, resource declarations in sco | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.167 | Principles | A future incremental layer can then invalidate correctly without rederiving the dependency graph. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.168 | Principles | **4. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.169 | Principles | No numeric speed targets before measurement.** Targets picked without baseline data ("100k lines/sec", "<500 ms incremental rebuild") are ei | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.170 | Principles | The MIR interpreter is not a performance target. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.171 | Principles | First targets come from measurements on non-trivial Kāra programs. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.172 | Principles | **Known-risky passes.** These are the places where quadratic behaviour is easiest to introduce: | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.173 | Principles | - **Monomorphization.** Kept function-local and lazy: a generic function is instantiated only for the type arguments a call site requests. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.174 | Principles | - **Effect inference SCC fixed point.** Each SCC iterates over a finite lattice; the bound (functions in the SCC times atoms in scope) keeps | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.175 | Principles | - **Borrow checking and drop elaboration.** Dataflow over each function's MIR. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.176 | Principles | It stays linear as long as it is function-local. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.177 | Principles | **Full incremental compilation is a future optimization**, not a v1 requirement. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.178 | Principles | Query infrastructure with dependency tracking (Salsa-style) doubles compiler complexity. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.179 | Principles | The principles above let that layer be added later without rewriting the passes. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.180 | Codegen containment | **The MIR → LLVM stage is the only part of the compiler that knows LLVM.** Every earlier stage, and the MIR interpreter, treats the backend  | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.181 | Codegen containment | Syntax trees, types, effect records, ownership facts and MIR are plain data with no LLVM types inside. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.182 | Codegen containment | This matters for three reasons: | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.183 | Codegen containment | 1. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.184 | Codegen containment | **Backend independence of the analyses.** Type checking, effect inference and ownership checking are properties of the source program, not o | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.185 | Codegen containment | Coupling LLVM types into them would mix a backend representation into language-level analysis and make the analyses harder to test alone. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.186 | Codegen containment | 2. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.187 | Codegen containment | **A contained backend swap.** Moving to another code generation substrate (for example MLIR, in the [tooling track](deferred.md#tooling)) is | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.188 | Codegen containment | 3. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.189 | Codegen containment | **One analysis pipeline, several consumers.** The MIR interpreter and the LLVM backend consume the same MIR. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.190 | Codegen containment | **Maintainership invariant.** Contributors must not put LLVM types into syntax-level, analysis-level or MIR structures. | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.191 | Codegen containment | A stage that needs to pass a hint to code generation (a layout decision, a vectorization annotation) does so through plain-data hint records | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.192 | Codegen containment | In the compiler source this stage is `src/backend/` (MIR → LLVM, the AOT and ASan drivers, the LLJIT and its runner). | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.193 | Codegen containment | Since M3, when the legacy `src/codegen.rs` was deleted, it is the only module that imports `inkwell` or names an LLVM type, and CI's codegen | compiler architecture (no program-observable behaviour; containment is CI's codegen containment guard) |
| s16.194 | Observable phases | The streaming diagnostic protocol ([Streaming phase events](#streaming-phase-events)) reports these phases: | NEW check_jsonl_reports_the_public_phase_names |
| s16.195 | Observable phases | ``` lex → parse → resolve → typecheck → effect → ownership → codegen | NEW check_jsonl_reports_the_public_phase_names |
| s16.196 | Observable phases | Phase names are part of the public contract: renaming one is a breaking change for any client of `--output=jsonl`. | NEW check_jsonl_reports_the_public_phase_names |
| s16.197 | Observable phases | Internal iteration within a phase is not observable. | NEW check_jsonl_reports_the_public_phase_names |
| s16.198 | Observable phases | Internal stages report under these names: desugaring reports as `parse`, and MIR building and borrow checking report as `ownership`. | NEW check_jsonl_reports_the_public_phase_names |
| s16.199 | Observable phases | The concurrency checks have no phase of their own: a `par` block's conflicts report under `typecheck`, and what may cross a task boundary un | NEW check_jsonl_reports_the_public_phase_names (a `par` conflict, E0418, under typecheck) |
| s16.200 | Observable phases | `codegen` is `karac build`'s alone. | tests/cli.rs (codegen phase of karac build) |
| s16.201 | Observable phases | `karac check` reports four more, which clients treat as opaque: | tests/cli.rs (lint, provider_escape, simd_check, comptime) |
| s16.202 | Observable phases | `lint` after `typecheck`, `provider_escape` and `simd_check` after `ownership`, and `comptime` last, since compile-time evaluation runs insi | tests/cli.rs |
| s16.203 | Execution model | Kāra has one semantics, defined by the MIR interpreter, and two ways to run a program: on the MIR interpreter, or as native code from MIR →  | NEW run_interp_and_build_behave_the_same |
| s16.204 | Execution model | Both execute the same MIR, so a program behaves the same under either; only speed differs. | NEW run_interp_and_build_behave_the_same |
| s16.205 | `karac build` | The default path: source → MIR → LLVM object → native executable linked with `cc`, with the runtime archive linked statically. | every build cell here |
| s16.206 | `karac build` | The optimization level is `-O0` to `-O3` on `karac build` (default `-O2`), or the `KARAC_OPT_LEVEL` environment variable with the same four  | opt_level_flags, opt_level_env, opt_level_flag_beats_env, opt_level_invalid_refused, opt_level_env_invalid_refused |
| s16.207 | `karac build` | A manifest profile has no optimization-level key in v1. | rule |
| s16.208 | `karac build` | The CPU baseline follows [CPU baseline](#cpu-baseline). | the CPU baseline cells above |
| s16.209 | `karac build` | Bounds-check elision ([Bounds checks and `get_unchecked`](#bounds-checks-and-get_unchecked)) is what lets loops vectorize. | rule (bounds-check elision; perf) |
| s16.210 | `karac run` | `karac run` builds a program with the same semantics as `karac build` and then runs it. | NEW run_interp_and_build_behave_the_same |
| s16.211 | `karac run` | Every error `karac build` reports stops the run with exit code 1 before the program starts. | NEW run_stops_with_exit_1_on_a_build_error |
| s16.212 | `karac run` | Which backend executes the program, and so how fast it starts, is an implementation choice; the behavior is the same. | NEW run_interp_and_build_behave_the_same (run, run --interp) |
| s16.213 | `karac run` | `karac run --interp` runs the program on the reference MIR interpreter. | NEW run_interp_and_build_behave_the_same (run, run --interp) |
| s16.214 | `karac test` | `karac test` runs each test in its own process, so a panicking test fails alone ([§14](#14-testing), [core-semantics.md §10.4](core-semantic | conformance/design/testing |
| s16.215 | `karac test` | `--interp` selects the MIR interpreter, as for `karac run`. | conformance/design/testing |
| s16.216 | Panic locations | **Level 2: | NEW a_native_panic_reports_file_line_col |
| s16.217 | Panic locations | DWARF, instruction level (v1).** Every LLVM instruction is tagged with its source span. | NEW a_native_panic_reports_file_line_col |
| s16.218 | Panic locations | The panic path resolves machine addresses to source `file:line:col` through the emitted DWARF, so users see `panic at file:line:col in fn_na | NEW a_native_panic_reports_file_line_col |
| s16.219 | Panic locations | **Level 3: rustc-style snippets (planned).** On the same DWARF data, panics print source snippets with caret pointers and node-aware message | planned (Level 3) |
| s16.220 | Panic locations | It may follow v1 in a patch release rather than block it. | planned (Level 3) |
| s16.221 | Bounds checks and `get_unchecked` | Slice and array indexing in safe code is bounds-checked at every access. | NEW an_in_bounds_loop_still_checks_an_out_of_bounds_index |
| s16.222 | Bounds checks and `get_unchecked` | Emitted naively, the check puts a side exit in every loop body, which stops LLVM's loop passes (autovectorizer, loop-invariant code motion,  | rationale (perf) |
| s16.223 | Bounds checks and `get_unchecked` | The result is that `-O3` gains nothing over `-O2` on loop-heavy code. | rationale (perf) |
| s16.224 | Bounds checks and `get_unchecked` | **(a) LLVM-friendly bounds checks.** Each check is emitted in a form LLVM's range analyses (SCEV, GVN) can prove redundant when the index is | tests/mir_get_unchecked.rs and the codegen IR tests (no observable behaviour) |
| s16.225 | Bounds checks and `get_unchecked` | - a compare and branch against the slice's `len`, with the panic block marked cold and the success-path range given to LLVM through `llvm.as | tests/mir_get_unchecked.rs and the codegen IR tests (no observable behaviour) |
| s16.226 | Bounds checks and `get_unchecked` | - for idiomatic loops (`for i in 0..xs.len() { xs[i] }`, nested strided loops), LLVM then hoists or removes the check, as it does for `rustc | tests/mir_get_unchecked.rs and the codegen IR tests (no observable behaviour) |
| s16.227 | Bounds checks and `get_unchecked` | - a constant index known to be in bounds (`xs[3]` with `3 < xs.len()` known statically) skips the check before LLVM sees the code. | tests/mir_get_unchecked.rs and the codegen IR tests (no observable behaviour) |
| s16.228 | Bounds checks and `get_unchecked` | This is invisible to users: the check is still semantically present in safe code. | NEW an_in_bounds_loop_still_checks_an_out_of_bounds_index |
| s16.229 | Bounds checks and `get_unchecked` | **(c) The `unsafe { xs.get_unchecked(i) }` escape hatch.** When the programmer knows an index is in bounds but the compiler cannot prove it, | NEW get_unchecked_in_unsafe_reads_the_element |
| s16.230 | Bounds checks and `get_unchecked` | It is an `unsafe fn` taking an `i64` index and returning `ref T`, with the caller obliged to ensure `i < self.len()`, the same contract as R | NEW get_unchecked_needs_an_unsafe_block, NEW get_unchecked_in_unsafe_reads_the_element |
| s16.231 | Bounds checks and `get_unchecked` | The `unsafe` block at the call documents the reason ([`undocumented_unsafe` lint](#undocumented_unsafe-lint)). | tests/cli.rs tests/diagnostics_corpus_tooling.rs (the undocumented_unsafe lint; NEW get_unchecked_in_unsafe_reads_the_element carries its SAFETY comment) |
| s16.232 | Bounds checks and `get_unchecked` | Most cases close under (a); this is the recourse for the rest. | NEW get_unchecked_in_unsafe_reads_the_element |
| s16.233 | Bounds checks and `get_unchecked` | ```kara // (a): idiomatic loop; LLVM proves the bounds check redundant fn sum(xs: Slice[i64]) -> i64 { let mut total: i64 = 0; for i in 0..x | NEW get_unchecked_in_unsafe_reads_the_element |
| s16.234 | Bounds checks and `get_unchecked` | **(b)**, a compiler-side bounds-check elimination pass, is deferred to the [tooling track](deferred.md#tooling). | deferred (tooling track) |
| s16.235 | Panic handler | A panic aborts the process, and no `Drop`, `defer` or `errdefer` runs ([§10](#10-errors-and-panics), [core-semantics.md §10.2](core-semantic | NEW panic_handler_runs_once_after_the_record_then_exit_101 (the Guard's Drop does not run) |
| s16.236 | Panic handler | A custom panic handler is the only user code that runs on a panic: after the [panic record](#panic-record) is written to stderr and before t | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.237 | Panic handler | Programs that must release a hardware handle, feed a watchdog or dump telemetry use it. | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.238 | Panic handler | A program defines at most one handler. | NEW two_panic_handlers_refused |
| s16.239 | Panic handler | The `#[panic_handler]` attribute alone enables it; there is no manifest setting. | NEW panic_handler_runs_once_after_the_record_then_exit_101 (no manifest key) |
| s16.240 | Panic handler | ```kara,uses=d-watchdog #[panic_handler] fn on_panic(info: PanicInfo) { Watchdog.feed_one_last_time(); eprintln(f"PANIC: {info.message} at { | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.241 | Panic handler | - The handler borrows a `PanicInfo` and runs once for the whole program, not once per frame. | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.242 | Panic handler | A single coordination point is the right shape for this job. | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.243 | Panic handler | - A panic inside the handler ends the process at once, with exit code 101. | NEW a_panic_inside_the_handler_ends_the_process_at_once |
| s16.244 | Panic handler | The handler is not run again. | NEW a_panic_inside_the_handler_ends_the_process_at_once |
| s16.245 | Panic handler | - It cannot resume execution. | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.246 | Panic handler | When it returns, the process exits with code 101. | NEW panic_handler_runs_once_after_the_record_then_exit_101 |
| s16.247 | Panic handler | --- | heading/intro |
