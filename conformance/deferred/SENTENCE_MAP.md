# deferred.md entry-to-cell map

One row per section of docs/deferred.md (8,058 lines, 213 `###` entries and their `####` parts), as of 2026-11-25, naming what a 0.8 program that reaches for the feature gets and the cell in this directory that shows it. The cells run with `python3 conformance/cases.py conformance/deferred`, on the interpreter and compiled; a `.refused` cell is held to `karac check`, which `karac run`, `run --interp` and `build` all apply first.

The unit is the entry, not the sentence: an entry makes one claim about 0.8, that the feature is refused, experimental, or a stated subset. Status values:

- **DEFERRED REFUSED**: refused on every user lane. FIXED marks a refusal this audit made name its deferred.md section (src/deferred_surface.rs: undefined names, unknown modules and items, missing methods, unknown attributes); GAP marks one still refused without the section (B-2026-11-25-71).
- **DEFERRED ACCEPTED**: deferred, but accepted with no effect or partly working. **CONTRADICTED**: the entry's own 0.8 claim is false.
- **EXPERIMENTAL**: runs as the status line says, and `karac explain` says experimental. **EXPERIMENTAL FALSE**: the status line says it runs and it does not. **UNDECLARED EXPERIMENTAL**: runs, with no status line.
- **SUBSET**: a stated 0.8 subset, which holds as stated. **SHIPPED**: out of date, the feature is in 0.8 and the text belongs in design.md (or is already there). **TOOL ABSENT**: a `karac` command or flag that does not exist, refused as unknown.
- **NO SURFACE**: design rationale, compiler internals, runtime strategy, a hosted product or an open question: nothing a 0.8 program can write. **DECLINED**: a permanent omission. **TRACK**: a track heading. **SUB**: part of the entry above it.

Counts over the 213 entries: NO SURFACE 94, DEFERRED REFUSED 67, SHIPPED 15, SUBSET 8, DEFERRED ACCEPTED 6, EXPERIMENTAL 6, TOOL ABSENT 6, DECLINED 5, DEFERRED 3, CONTRADICTED 1, EXPERIMENTAL FALSE 1, UNDECLARED EXPERIMENTAL 1.

Corpus caveat: the corpus backends `__mir-run` and `__mir-llvm-run` do not apply `karac check`, so a program every user lane refuses can still pass there (B-2026-11-25-70). Every claim below was measured on `karac check`, `run --interp` / `__mir-run` and `build`.

| Line | Entry | Status | Cell(s) | Note |
|---|---|---|---|---|
| 28 | ## M4a services | TRACK |  | Track summary; its features are rowed below. |
| 32 | Execution Effects (`blocks` / `suspends`) | SHIPPED | undeclared_suspends_clause_accepted | `suspends` is a v1 execution effect in design.md §12 (table at the `blocks` row: 'the second execution effect'), and `with suspends` is accepted and runs. design.md § Not in v1 ("Coroutines and `suspends` … M4a") says the opposite: a design.md-internal contradiction, routed to review. The entry's placement design (coroutines, state machines) has no 0.8 surface. |
| 62 | #### Execution Effect Inference | NO SURFACE |  | Inference design for `suspends`; the inferred effect is not reported for a primitive in 0.8. |
| 97 | Async I/O in Practice | NO SURFACE |  | Explanatory: what non-blocking I/O looks like; no new syntax (no `async`, no `await`). `await` is a reserved word: await_refused. |
| 158 | Runtime Phases | NO SURFACE |  | Runtime staging (event loop, work stealing); not observable from a 0.8 program. |
| 183 | Network Event Loop and State-Machine Transform | NO SURFACE |  | Event-loop lowering, RAII-across-yield (withdrawn), debugger contract, FFI-across-yield, drop order under cancellation: compiler-internal or withdrawn; no construct to write. |
| 187 | #### State-Machine Transform — Network-Boundary Functions | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 218 | #### RAII Across Yield Points | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 291 | #### Debugger Contract Extension for Parked Tasks | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 328 | #### FFI Across Yield Points | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 372 | #### Drop Ordering Across Yield Points | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 412 | #### Cooperative Cancellation | SHIPPED |  | Cooperative cancellation: design.md § Not in v1 (From the services track) says cancellation, deadlines and isolating groups are specified in core-semantics.md §11.8–§11.12, so this section is superseded by v1 text. |
| 426 | #### Scheduler-layer concerns and minimum invariants | SUB |  | Part of “Network Event Loop and State-Machine Transform”; covered by that row. |
| 443 | Full-Hybrid State-Machine Transform (Arbitrary `suspends` Functions) | NO SURFACE |  | Compiler strategy (state machine for every `suspends` fn). |
| 455 | `select` Across Channels | SHIPPED | select_shipped_in_v1 | `select` runs in 0.8 on both lanes and design.md § From the services track says it is in core-semantics.md §11.8–§11.12. Out of date: move to design.md (or reduce to a pointer). |
| 478 | Channel Combinators | DEFERRED REFUSED |  | No combinator names in 0.8 (`merge`, `fan_in`, ... are undefined names). Refused generically: E0100 with no deferred.md pointer (names too generic to table). |
| 497 | Par-Region Saturation Strategy Configuration | NO SURFACE |  | Runtime configuration knob; no 0.8 surface. |
| 509 | Cross-task `?` Propagation | NO SURFACE |  | No syntax: a task's `Err` reaches its joiner (v1). |
| 513 | Unstructured `spawn` | DEFERRED REFUSED | unstructured_spawn_refused | Free `spawn` is E0520, naming core-semantics §11.1. Every user lane refuses it. |
| 519 | Provider-scoped stored callbacks | NO SURFACE |  | A later relaxation of core-semantics §14.5; today's refusal is that section's own rule (pinned in corpus/core). |
| 523 | Provider-Rooted Resources (trait-based injection) | SHIPPED |  | Self-declared superseded (2026-10-20): providers are core-semantics §14. Keep or cut the historical text. |
| 659 | `providers { } in { }` Block (multi-provider bootstrapping) | SHIPPED | providers_block_shipped_in_v1 | `providers { } in { }` runs in 0.8; design.md § Effects ("Providers (`effect resource R: Trait`, `with_provider`, `providers {} in {}`, `#[with_provider]`) are in v1"). Out of date. |
| 731 | Deterministic Replay via Provider Injection | SHIPPED |  | `#[with_provider]` on a test is v1 (design.md §14 `#[with_provider(R, e)]`). Out of date. Side finding: `#[with_provider(..)]` on a plain `fn main` is accepted silently and does nothing (probe), see findings. B-2026-11-25-66. |
| 743 | Test Attributes for Live and Substituted Resources | SHIPPED |  | `#[test(requires: [R, ..])]` is v1 (design.md §14). Out of date. |
| 800 | Diagnostics and Runtime Reports for Services | NO SURFACE |  | Reports that come with the track (traces exclude Cancelled, provider stack in panic record). |
| 809 | Effect Variables for Stored Callbacks | SHIPPED |  | Self-declared: specified for v1 in design.md § Effect variables. |
| 813 | Effect Groups and Composition | DEFERRED REFUSED | effect_group_refused | E_DEFERRED_EFFECT_GROUP with the section. |
| 833 | Stdlib Scope for Non-Primitive Resources | NO SURFACE |  | Open policy question. |
| 856 | Bytes-Level Stdin and Typed Buffering | DEFERRED REFUSED |  | `stdin().read_line_bytes()` is refused (no method; FIXED: the refusal now names this section, method table). Typed buffering traits: no names to refuse. |
| 861 | Positional File I/O and Advisory Locking | DEFERRED REFUSED | positional_file_io_refused | `read_at` / `write_at` / `lock` / `try_lock` / `unlock` on `File`: E0236. FIXED: the refusal names this section. |
| 866 | Processes beyond v1 | DEFERRED REFUSED | process_signals_refused | `Signal` is E0100. FIXED: names this section. Process groups, ptys, daemons, pipe(): no names in 0.8. |
| 879 | Networking beyond v1 | DEFERRED REFUSED | multicast_refused | `join_multicast_v4/v6`, `set_broadcast` on `UdpSocket`: E0236. FIXED: names this section. |
| 888 | Language-Integrated Query (SQL DSL) and ORM | NO SURFACE |  | Open question (query DSL / ORM). |
| 915 | Canonical Postgres Driver (`kara-postgres`) — Project-Owned Package | NO SURFACE |  | A separate package (`kara-postgres`); `karac add` is not a command in 0.8. |
| 929 | std.crypto | SUBSET |  | States what shipped (SHA-2, HMAC, constant_time_eq, SHA-1 legacy) and that a wasm build refuses `std.crypto` at check: pinned by conformance/crypto and tests/mir_wasm.rs. The rest of the table has no 0.8 names. |
| 975 | Constant-Time Integer Types (`CtU64`, `CtBool`) | DEFERRED REFUSED | constant_time_integers_refused | E0100. FIXED: names this section. |
| 990 | Generalized `#[zeroize]` Attribute | DEFERRED REFUSED | zeroize_refused | E_UNKNOWN_ATTRIBUTE. FIXED: names this section instead of the `#[diagnostic::..]` hint. |
| 1005 | Taint Tracking (`Untrusted[T]` / `Validated[T]`) | DEFERRED REFUSED | taint_types_refused | E0100. FIXED: names this section. |
| 1038 | Server-side HTTP/2 | NO SURFACE |  | Servers speak HTTP/1.1; nothing to write for h2. The client's h2 negotiation is in io.md. |
| 1048 | gRPC (Streaming, Reflection, Server / Client) | NO SURFACE |  | Library or runtime work with no 0.8 name. |
| 1060 | HTTP/3 / QUIC | NO SURFACE |  | Library or runtime work with no 0.8 name. |
| 1072 | Microservice Mesh Primitives | NO SURFACE |  | Library or runtime work with no 0.8 name. |
| 1084 | Custom Executors / Pluggable Schedulers | NO SURFACE |  | Library or runtime work with no 0.8 name. |
| 1096 | Full-Stack Server Web Framework (Django / Rails / Spring Boot / Phoenix class) | NO SURFACE |  | Library or runtime work with no 0.8 name. |
| 1133 | Stdlib and Ecosystem Security Conventions | NO SURFACE |  | Conventions for later stdlib work. |
| 1149 | ## M4b auto-par | TRACK |  | Track summary. design.md's own track table says M4b comes before v1, and `seq {}` runs in 0.8 (probe). |
| 1155 | Feature 5: Auto-Concurrency via Effect Analysis | SHIPPED |  | Auto-par is on by default in `karac build` (kara 72ce9e026c, per kara-katas CLAUDE.md) and `karac query concurrency` reports its groups. The 'Feature 5' design describes 0.8 behaviour: candidate to move. |
| 1233 | Resource-Modeling Friction Lints | NO SURFACE |  | Advisory lints not yet written. |
| 1241 | Auto-Parallel Loops over Provably-Disjoint Indexed Writes | SHIPPED |  | Disjoint indexed-write loops fan out in 0.8 (query concurrency reports `disjoint_write` loops). |
| 1266 | Composition with SIMD | NO SURFACE |  | Composition rationale. |
| 1277 | Cost Model | NO SURFACE |  | Cost model deliberately unspecified. |
| 1295 | Runtime Work Estimate for Auto-Par Groups in Hot Bodies | NO SURFACE |  | Future runtime estimate. |
| 1303 | Determinism Contract | SHIPPED |  | Self-declared: v1 contract is design.md §13; this restates it for auto-par. |
| 1312 | Auto-Parallel Regions and Cleanup | NO SURFACE |  | defer/errdefer join barrier; not separately observable from a program (the effect is invisible by §11.5). |
| 1322 | `karac query concurrency` Output Schema | SHIPPED |  | `karac query concurrency` ships (probe printed its schema). Out of date: the schema belongs in design.md §17's query API. |
| 1382 | Deterministic Parallel Runtime as a Standalone Library | NO SURFACE |  | Packaging idea. |
| 1405 | ## M4c data | TRACK |  | `kara-data` package after v1. |
| 1409 | Numerical Types (Tensor, Column, DataFrame) | DEFERRED REFUSED | tensor_refused | `Tensor` E0520 names the kara-data package and this track; Column/DataFrame are conformance/data. |
| 1413 | #### Tensor — shape types | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1490 | #### Broadcasting | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1528 | #### Column — nullable 1D data | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1578 | #### DataFrame — schema-bearing tables | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1582 | #### Memory Layout Commitments (Arrow) | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1596 | #### GPU Backend (future direction) | SUB |  | Part of “Numerical Types (Tensor, Column, DataFrame)”; covered by that row. |
| 1608 | Tensor Strides / Non-Contiguous Views | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1633 | Axis-Indexed Reductions | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1657 | Shape-Parameter Arithmetic (`[A + B]`, `[N * 2]`) | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1668 | Reduced-Precision Floats (`f16`, `bf16`) | CONTRADICTED | undeclared_f16_runs | Says `f16`/`bf16` are 'ordinary identifiers, not reserved words' until the track. In 0.8 they are primitive types: `let x: f16 = 1.5` runs on both lanes (58 corpus programs use them), and `let f16 = 3` is refused as a primitive type name (E0102). Either a status line (experimental) or design.md text is missing. B-2026-11-25-68. |
| 1711 | `std.einsum` — Einstein Summation | DEFERRED REFUSED | std_einsum_refused | E0112. FIXED: names this section. |
| 1735 | `std.embeddings` — Cosine Similarity and Top-K Primitives | DEFERRED ACCEPTED |  | `import std.embeddings;` is accepted silently (runtime/stdlib has the module); every function in it takes a `Tensor`, which is refused, so nothing runs. Minor. |
| 1761 | `std.linalg` — Linear Algebra Suite | DEFERRED REFUSED | std_linalg_refused | E0112. FIXED: names this section. |
| 1787 | `std.fft` — Fourier Transforms | DEFERRED REFUSED | std_fft_refused | E0112 (it suggested `std.net`). FIXED: names this section, no guess. |
| 1809 | `std.random` — Distribution Extensions | DEFERRED REFUSED | random_distributions_refused | `std.random.{normal}`: E0113. FIXED: names this section. |
| 1833 | `std.autograd` — Automatic Differentiation (reverse-mode) | DEFERRED ACCEPTED |  | `import std.autograd;` is accepted silently, as std.embeddings. Minor. |
| 1865 | Lazy DataFrame Query Planner | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1891 | Statistical Methods on `Column` / `DataFrame` | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1918 | Data Documentation and Examples (Discoverability Surface) | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1933 | `Tensor.where` — Conditional Element Selection | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1954 | Boolean and Fancy Indexing | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 1978 | `Tensor.meshgrid` — Coordinate Grid Generation | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2001 | Tensor Element-Wise Math and Clamp | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2038 | Hand-Vectorized Data-Spine Commitment | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2084 | Tensor Construction Functions | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2120 | Scan Operations (`cumsum`, `cumprod`) | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2146 | Shape-Manipulating Operations (`concat`, `stack`, `reshape`, `squeeze`, `expand_dims`) | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2179 | Set-Like Operations (`unique`, `searchsorted`) | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2204 | NaN and Inf Handling | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2243 | `.npy` / `.npz` Array File I/O | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2273 | `Complex[T]` — Complex Number Type | DEFERRED REFUSED | complex_refused | E0100. FIXED: names this section. |
| 2314 | Typed `einsum` with Compile-Time Index Checking | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2326 | Lazy DataFrame Query Optimizer Expansion | DEFERRED REFUSED |  | Needs `Tensor`, which is refused (tensor_refused). |
| 2341 | Neural Network Framework (`std.nn` / `std.optim`) — Decision Deferred | DEFERRED REFUSED |  | `std.nn` / `std.optim`: E0112. FIXED: names this section. |
| 2357 | ## Systems | TRACK |  |  |
| 2361 | Project Profiles | DEFERRED ACCEPTED |  | `[profile]` manifest key is refused (E_PROFILE_* family is deferred in `explain`), but the per-function `#[profile(kernel)]` attribute is accepted and ENFORCED (E_PROFILE_INCOMPATIBLE_EFFECT fires), while `karac explain` calls that code 'Profiles are deferred'. Half-works. B-2026-11-25-66. |
| 2425 | #### Profile mapping | SUB |  | Part of “Project Profiles”; covered by that row. |
| 2437 | #### `no_alloc` formatting | SUB |  | Part of “Project Profiles”; covered by that row. |
| 2477 | Fallible-Allocation Mode | DEFERRED REFUSED |  | `[profile] panic_on_alloc_failure` codes E0264/E0265 are listed as deferred in explain; v1's `try_*` methods are library/collections.md. |
| 2510 | Embedded-Profile Arithmetic | NO SURFACE |  | Needs the embedded profile. |
| 2525 | Scoped `#[wrapping]` / `#[checked]` Arithmetic Regions | DEFERRED REFUSED |  | `#[wrapping] { }` is refused (E_ATTRIBUTE_NOT_VALID_ON_STATEMENT), with no deferred.md pointer. GAP (message). B-2026-11-25-71. |
| 2558 | Item-Level `#[cfg]` | SUBSET | cfg_refused_with_interim_target | Stated 0.8 subset holds: `#[cfg]` is refused (E0005) naming `#[target]` as the interim spelling. |
| 2569 | Conditional Compilation Beyond Targets | NO SURFACE |  | Beyond `#[cfg]`; nothing to write. |
| 2586 | `#[no_std]` / Bare Metal Support | DEFERRED REFUSED | no_std_refused | E_UNKNOWN_ATTRIBUTE. FIXED: names this section. |
| 2593 | Additional Compilation Targets | NO SURFACE |  | Embedded targets; `--target` refuses unknown names (design §16 cells). |
| 2602 | Atomic Operations and Memory Ordering | DEFERRED ACCEPTED | undeclared_atomic_fetch_or_runs, atomic_ordering_argument_is_deprecated, intrinsic_fence_reachable_from_a_program | library/concurrency.md says `fetch_and`, `fetch_or`, fences and weaker orderings are this track. In 0.8 `fetch_and` / `fetch_or` are accepted and run (2 corpus programs), `fence(MemoryOrdering.SeqCst)` is callable from a user program (intrinsic_fence_reachable_from_a_program), and an ordering argument is the documented deprecated form (accepted, stripped). Decision for review: promote fetch_and/fetch_or to concurrency.md (SeqCst, already correct) or refuse them. B-2026-11-25-67. |
| 2626 | #### Methods | SUB |  | Part of “Atomic Operations and Memory Ordering”; covered by that row. |
| 2636 | #### Memory Ordering | SUB |  | Part of “Atomic Operations and Memory Ordering”; covered by that row. |
| 2650 | #### Fences | SUB |  | Part of “Atomic Operations and Memory Ordering”; covered by that row. |
| 2671 | #### Effect Model | SUB |  | Part of “Atomic Operations and Memory Ordering”; covered by that row. |
| 2683 | #### Embedded Profile | SUB |  | Part of “Atomic Operations and Memory Ordering”; covered by that row. |
| 2687 | Module-Level Mutable State | EXPERIMENTAL FALSE | module_cells_refused_despite_experimental_status, module_level_mutable_state_refused, thread_local_refused, explain_module_cells_says_experimental | Status says the compiler implements module-level cells and corpus programs run them; `karac explain E_LAZYLOCK_RUNTIME_CAPTURE` says so too. But `karac check`, `run` and `build` refuse `let TABLE: LazyLock[..] = ..` with E_MODULE_LET ('mutable global state'), matching design.md § No mutable globals ('Module-level cells … are deferred'). Only the corpus backends (`__mir-run`, `__mir-llvm-run`), which skip check, run it (59 corpus entries). The status line and the explain note are wrong, or the check is. B-2026-11-25-65, -49. |
| 2835 | `#[repr(packed)]`, `#[repr(align(N))]`, `offset_of` | DEFERRED REFUSED | repr_packed_refused, repr_align_refused | E_REPR_DEFERRED, naming design.md § `#[repr]` (which points here). |
| 2862 | #### Field Offsets: `offset_of[T](field)` | EXPERIMENTAL FALSE | offset_of_not_lowered_despite_experimental_status, explain_offset_of_says_experimental | Status: implemented and runs. In 0.8 `offset_of[P](b)` checks but MIR does not lower it: E0516 'the expression kind `OffsetOf` is not lowered yet' on every lane; 7 corpus entries fail on mir-interp. The E_OFFSET_OF_* diagnostics work (corpus/diagnostics). B-2026-11-25-65. |
| 2948 | Bitfield Types | DEFERRED REFUSED | bitfield_refused | `u1`..`u63` are undefined types (E0104); `#[repr(C, bitfield)]` is not reached. GAP (message). B-2026-11-25-71. |
| 2970 | Volatile access, inline assembly, interrupts | DEFERRED |  | Volatile access, inline asm, interrupts: rowed by sub-entry. |
| 2972 | #### Volatile Memory Access (MMIO) | DEFERRED REFUSED |  | No volatile methods on pointers (`read_volatile`: no method). GAP (message). B-2026-11-25-71. |
| 3014 | #### Inline Assembly | DEFERRED REFUSED | inline_asm_refused | `asm` is a reserved word (E_RESERVED_KEYWORD). |
| 3174 | #### Interrupt Handler ABI (`#[interrupt]`) | DEFERRED ACCEPTED | undeclared_interrupt_attribute_accepted | `#[interrupt(TIMER1)]` is a recognised attribute that does nothing: the function is called directly and runs on both lanes, which this section forbids. Should be refused. B-2026-11-25-66. |
| 3255 | Codegen hints and linker control | DEFERRED |  | Rowed by sub-entry. |
| 3257 | #### Codegen Hint Attributes (`#[inline]`, `#[cold]`) | CONTRADICTED | codegen_hints_accepted_and_run, codegen_hint_conflict_refused | design.md §15 lists codegen hint attributes as deferred and `karac explain` says they are parsed and checked but not lowered; the conflict rules of this section are enforced (E_INLINE_HINT_CONFLICT). src/attribute_validator.rs says codegen lowers them to LLVM inlinehint/alwaysinline/noinline/cold. Status line missing either way. B-2026-11-25-68. |
| 3308 | #### Linker Control Attributes | DEFERRED ACCEPTED | undeclared_link_section_accepted, exported_c_abi_refused | `#[unsafe(link_section(..))]` and `#[used]` are accepted silently on a function (no effect checked). `#[unsafe(no_mangle)]` with `pub extern "C" fn` is refused by E_ARCHIVED (exporting is not in 0.8). B-2026-11-25-66. |
| 3388 | Calling conventions | DEFERRED REFUSED | calling_convention_refused | E0005: reserved ABI, 0.8 takes "C" / "C-unwind". |
| 3390 | #### C Calling Convention Variants | DEFERRED REFUSED | calling_convention_refused |  |
| 3407 | FFI unions and exported C ABI | DEFERRED |  | Rowed by sub-entry. |
| 3409 | #### C function-pointer types (callbacks) | NO SURFACE |  | C function-pointer types: `extern "C" fn` types are design.md §15 (v1) callbacks; nothing extra to refuse. |
| 3413 | #### FFI Unions: `union Foo { ... }` | DEFERRED REFUSED | union_declaration_accepted_use_refused | A `union` declaration is accepted; a use is E0516 naming this section on every lane. Acceptable (nothing runs), though the declaration itself could be refused. |
| 3514 | #### Exported C ABI | DEFERRED REFUSED | exported_c_abi_refused | E_ARCHIVED: exporting functions to C is not in 0.8. |
| 3614 | Portable SIMD — `Vector[T, N]` | UNDECLARED EXPERIMENTAL | undeclared_simd_vector_runs | `Vector[T, N]` runs on both lanes and 200 corpus programs use it (`#[require_simd]` too), with no status line here and no design.md text. Needs the 11-17 experimental status line (or a move to design.md). B-2026-11-25-68. |
| 3704 | Multiversioning: `cpu-baseline` and `#[multiversion]` | EXPERIMENTAL | experimental_target_feature_runs, explain_target_feature_says_experimental, multiversion_accepted_and_runs | `#[target_feature]` runs. The status says `#[multiversion]` 'builds no variants', but karac synthesises `addup$avx2` (its own undocumented_unsafe warnings name it, a finding) and corpus test_ir_multiversion_emits_variants_and_dispatch pins variants: the parenthesis is out of date. B-2026-11-25-68, -48. |
| 3744 | CPU feature dispatch | NO SURFACE |  | Interaction rules with the CPU baseline; `--target-features` floor. |
| 3755 | Complexity Budgets and Static Stack Depth Analysis | NO SURFACE |  | Analysis not built. |
| 3765 | #### Static Stack Depth Analysis | SUB |  | Part of “Complexity Budgets and Static Stack Depth Analysis”; covered by that row. |
| 3782 | CircularBuffer[T] | DEFERRED REFUSED | circular_buffer_refused | E0100. FIXED: names this section. |
| 3822 | ## Verification | TRACK |  |  |
| 3828 | Refinement Types | EXPERIMENTAL | experimental_refinement_runs, explain_refinement_says_experimental | Runs on both lanes; explain says experimental. |
| 4011 | Refinement Types in Inference, Declarations and Traits | NO SURFACE |  | Check-mode inference extensions of refinement types. |
| 4055 | Contracts (requires / ensures / invariant) | EXPERIMENTAL | experimental_contract_violation_aborts, explain_contracts_says_experimental | A violated `ensures` aborts (exit 101, 'contract violated') on both lanes. `impl invariant` is refused E_DEFERRED_TRACK naming this section (25 corpus entries). |
| 4149 | #### Contract evaluation semantics | SUB |  | Part of “Contracts (requires / ensures / invariant)”; covered by that row. |
| 4226 | Production Contract Checking (`#[checked]`) | DEFERRED REFUSED | checked_contracts_refused | `#[checked]` is E_UNKNOWN_ATTRIBUTE. FIXED: names this section. |
| 4249 | #### Opt-in Release-Mode Contract Checks | SUB |  | Part of “Production Contract Checking (`#[checked]`)”; covered by that row. |
| 4272 | Gradual Verification (Level 3-4) | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4276 | #### Verification Levels | SUB |  | Part of “Gradual Verification (Level 3-4)”; covered by that row. |
| 4284 | Flow-Sensitive Refinement Narrowing (Restricted) | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4288 | Spec-First Programming | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4294 | Promote Passing Test Assertions into Contracts (`karac test --suggest-contracts`) | TOOL ABSENT | tool_suggest_contracts_not_in_08 | `karac test --suggest-contracts` is an unknown flag. |
| 4334 | Auto-Derived `Arbitrary` and `Shrink` Honoring Refinements and Invariants | NO SURFACE |  | Extends `#[derive(Arbitrary)]`, itself under Test Framework Extensions. |
| 4366 | Machine-Verifiable Intent Annotations | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4370 | Formal Specification as Primary Artifact | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4374 | Oracle Synthesis from Contracts | NO SURFACE |  | Verification tooling or research; no 0.8 surface. |
| 4398 | ## Effects expressiveness | TRACK |  |  |
| 4402 | Named Effect Variables | NO SURFACE |  | Named effect variables beyond stored callbacks: the v1 part is design.md § Effect variables; the rest has no syntax yet (effect_variable_bound_refused shows a bound is a parse error). |
| 4498 | #### Resolution order for compound polymorphism | SUB |  | Part of “Named Effect Variables”; covered by that row. |
| 4569 | Parameterized Resources (opt-in finer granularity) | SUBSET | keyed_resource_charges_the_whole_resource, keyed_resource_par_conflicts_on_the_whole_resource | Stated 0.8 subset holds: `writes(UserDB[id])` is accepted, `query effects` reports `writes(UserDB)`, and two keys in a `par` conflict (E0418). |
| 4585 | #### The alias tri-state — principle before algorithm | SUB |  | Part of “Parameterized Resources (opt-in finer granularity)”; covered by that row. |
| 4649 | Effect Semver Rules | NO SURFACE |  | Needs effect groups (refused). |
| 4698 | #### `stable` Modifier on Effect Groups | SUB |  | Part of “Effect Semver Rules”; covered by that row. |
| 4706 | Effect Variable Bounds (`with E: no writes(R)`) | DEFERRED REFUSED | effect_variable_bound_refused | Parse error E0002 at the `:`. GAP (message). B-2026-11-25-71. |
| 4716 | #### Effect Variable Bounds | SUB |  | Part of “Effect Variable Bounds (`with E: no writes(R)`)”; covered by that row. |
| 4745 | Field-Level Effect Granularity for `sync struct` | NO SURFACE |  | Open design question. |
| 4758 | Process Exit as an Effect (`exits`) | DEFERRED REFUSED | exits_effect_refused | E0403 'unknown effect' (and E0100). The message says effect groups are not in 0.8, not this section. GAP (message). B-2026-11-25-71. |
| 4766 | Package Manifest Capability Declarations | NO SURFACE |  | Package-manager or audit tooling. |
| 4781 | Effect Diff Tooling for Cross-Version `panics` Surfacing | NO SURFACE |  | Package-manager or audit tooling. |
| 4796 | Effect-Row Verbosity Audit | NO SURFACE |  | Package-manager or audit tooling. |
| 4806 | ## Interactive | TRACK |  |  |
| 4810 | REPL (`karac repl`) | EXPERIMENTAL |  | `karac repl` ships, pinned by conformance/repl; design.md § REPL (experimental). `:dep` (stated subset) records and says it is not resolved: design.md's REPL table, conformance/repl. |
| 4820 | Interactive Evaluation Model | SUBSET |  | The 0.8 subset is design.md § REPL (experimental); the rest (notebooks, Jupyter, LLJIT surfaces) is not in 0.8. |
| 4832 | #### Cell Scope | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4847 | #### Ownership Across Cells | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4878 | #### Effect Semantics | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4889 | #### Cross-Cell Providers | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4953 | #### Session Dependencies | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4973 | #### Session Export | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 4979 | #### Rich Output Display Protocol | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 5014 | #### REPL and script dependency scope | SUB |  | Part of “Interactive Evaluation Model”; covered by that row. |
| 5026 | Rust ↔ Kāra Web Playground | NO SURFACE |  | Hosted product. |
| 5052 | Compiler-Decision Explorer (hosted) | NO SURFACE |  | Hosted product. |
| 5076 | Compiled-Speed Notebook Product | NO SURFACE |  | Hosted product. |
| 5096 | ## Web | TRACK | tool_wasm_browser_target_not_in_08 | `--target=wasm_browser` refused: 'not in 0.8'. `#[target(wasm_browser)]` is accepted with a later_track warning (design §16 cells). |
| 5100 | Targets and resources | DEFERRED REFUSED | tool_wasm_browser_target_not_in_08 |  |
| 5102 | #### Cross-target Compilation | SUB |  | Part of “Targets and resources”; covered by that row. |
| 5115 | #### Target-Provided Resource Sets | SUB |  | Part of “Targets and resources”; covered by that row. |
| 5135 | #### Provider Injection for SSR-shared Code (primary pattern) | SUB |  | Part of “Targets and resources”; covered by that row. |
| 5175 | Web / Host Effect Vocabulary | NO SURFACE |  | Stdlib vocabulary for browser resources, none in 0.8. |
| 5205 | WASM concurrency and async host APIs | NO SURFACE |  | wasm threads: `--features wasm-threads` exists as a runtime archive (CLAUDE.md); the browser half is the web track. |
| 5220 | #### Async Host APIs on WASM | SUB |  | Part of “WASM concurrency and async host APIs”; covered by that row. |
| 5267 | Compiler-Managed Transparent Threading on WASM | NO SURFACE |  |  |
| 5293 | `await` Keyword for Async APIs | DEFERRED REFUSED | await_refused | `await` is reserved (E_RESERVED_KEYWORD). |
| 5317 | Bindings, component model, entry points | NO SURFACE |  | Component model / entry points: web track. |
| 5319 | #### WASM (`--target wasm_browser` or `--target wasm_wasi`) | SUB |  | Part of “Bindings, component model, entry points”; covered by that row. |
| 5355 | #### Component Model emission | SUB |  | Part of “Bindings, component model, entry points”; covered by that row. |
| 5366 | #### Entry point discovery | SUB |  | Part of “Bindings, component model, entry points”; covered by that row. |
| 5378 | `host fn` browser and Component Model lowering | DEFERRED ACCEPTED |  | `host fn` parses and is checked (it must declare effects, E0001) in 0.8; whether a declared one builds is untested here. Flag for the web track. |
| 5404 | #### Browser-WASM lowering | SUB |  | Part of “`host fn` browser and Component Model lowering”; covered by that row. |
| 5417 | #### Per-target manifest blocks for the browser | SUB |  | Part of “`host fn` browser and Component Model lowering”; covered by that row. |
| 5429 | Frontend UI Framework | NO SURFACE |  | Framework. |
| 5466 | ## GPU | EXPERIMENTAL FALSE | gpu_dispatch_refused, gpu_kernel_runs_on_the_cpu_with_a_later_track_note, tool_gpu_target_not_in_08, explain_gpu_says_experimental | Status: 'the compiler implements this and corpus programs run it'. In 0.8 `gpu.*` is refused (E_ARCHIVED: 'GPU compute … is not in 0.8'), `#[gpu]` is accepted with a later_track warning and runs on the CPU, `--target=gpu` is refused, and 26 of 33 corpus GPU programs are skipped as deferred:gpu. `karac explain E0800` still says implemented and runs. The status line should say deferred. B-2026-11-25-65. |
| 5473 | Compilation Target Flexibility | NO SURFACE |  | GPU design; see the GPU track row. |
| 5489 | GPU Subset Constraints | NO SURFACE |  | GPU design; see the GPU track row. |
| 5510 | #### What is and is not GPU-compatible | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5526 | #### `GpuSafe` trait | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5537 | #### Generics and `#[gpu]` | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5557 | #### Call graph validation | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5580 | #### Data transfer via effects | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5604 | #### Whole-buffer reductions | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5686 | #### Integer reductions overflow-check, and the tree order decides when | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5709 | #### Reducing a buffer that is already on the device | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5757 | #### Prefix sum — the one that is not a fold | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5773 | #### Tiled matmul — the one that agrees with the CPU | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5797 | #### GPU (`--target gpu`) | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5803 | #### Integer arithmetic in kernels | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5807 | #### The `GpuBuffer` resource and heap allocation | SUB |  | Part of “GPU Subset Constraints”; covered by that row. |
| 5818 | Browser GPU Graphics Library (`kara-gfx` or equivalent) | NO SURFACE |  | GPU design; see the GPU track row. |
| 5841 | Heterogeneous Compute — Beyond CPU + GPU | NO SURFACE |  | GPU design; see the GPU track row. |
| 5859 | ## Comptime | EXPERIMENTAL | experimental_comptime_runs_with_a_later_track_note, explain_comptime_says_experimental | Runs (28 corpus programs), with a later_track warning that cites core-semantics.md §13 ('may be evaluated at run time') rather than this section. |
| 5865 | Comptime — AST→AST `comptime fn` | EXPERIMENTAL | experimental_comptime_runs_with_a_later_track_note |  |
| 5869 | #### Surface forms | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 5891 | #### Types as first-class values | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 5909 | #### Reflection API | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 5928 | #### AST builder API | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 5971 | #### Code generation and derive desugaring | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 5983 | #### Effect system integration | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6000 | #### Const-generic, refinement, and default-value integration | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6010 | #### Hygiene rules | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6021 | #### Resource limits | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6031 | #### Tooling integration | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6038 | #### Comptime stdlib surface | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6052 | #### Implementation phases | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6063 | #### Comptime Effect Defaults | SUB |  | Part of “Comptime — AST→AST `comptime fn`”; covered by that row. |
| 6094 | ## dyn | EXPERIMENTAL | experimental_trait_header_clause_is_the_ceiling | Trait-header `with` clause: experimental, enforced as each unclauded method's ceiling (E0230), as stated. The other bullets (upcasting, downcasting, inline values) have no 0.8 syntax beyond design.md § dyn Trait. |
| 6105 | ## Layout | TRACK |  |  |
| 6110 | Layout Blocks | DEFERRED REFUSED | layout_block_refused | E_DEFERRED_LAYOUT_BLOCK with the section. |
| 6132 | #### Layout Rules | SUB |  | Part of “Layout Blocks”; covered by that row. |
| 6150 | #### Layout Blocks and `#[repr]` | SUB |  | Part of “Layout Blocks”; covered by that row. |
| 6160 | String Small-String Optimization | NO SURFACE |  | String SSO: internal (KARAC_SSO off). |
| 6166 | #### Enums in Layout Blocks | SUB |  | Part of “String Small-String Optimization”; covered by that row. |
| 6170 | Layout-Capability Bound (Type-System-Enforced Layout Requirements) | NO SURFACE |  |  |
| 6180 | Schedule-Language Layer | NO SURFACE |  |  |
| 6192 | ## Self-hosting | TRACK |  |  |
| 6197 | Self-Hosting | NO SURFACE |  |  |
| 6207 | ## Tooling | TRACK |  |  |
| 6211 | Expected-panic tests (`should_panic`) | DEFERRED REFUSED | should_panic_refused | E_UNKNOWN_ATTRIBUTE. FIXED: names this section. |
| 6219 | Hot Reloading | NO SURFACE |  |  |
| 6227 | Workspace Scaffolding (`karac init --workspace`) | TOOL ABSENT | tool_workspace_init_not_in_08 | `karac init --workspace`: unknown flag. |
| 6249 | `karac test --coverage` (LLVM-instrumented coverage) | TOOL ABSENT | tool_coverage_not_in_08 | `karac test --coverage`: unknown flag. |
| 6277 | Structured Diagnostics and Error Class Enum (`karac explain --format=json`) | SHIPPED |  | `karac explain --format=json` ships (prints kind/class/concept records). Out of date: belongs in design.md §17. |
| 6289 | Signature-from-Call-Site Stub Diagnostic | NO SURFACE |  |  |
| 6332 | `karac tdd` Watch Driver — Unified TDD Cycle Loop | TOOL ABSENT | tool_tdd_not_in_08 | `karac tdd`: unknown command. |
| 6442 | Signature Catalog (`karac catalog`) | SHIPPED |  | `karac catalog` is a command in 0.8 (it asks for a file). Out of date, or the command is unfinished: decide which. |
| 6452 | `karac query affected-by` — Call-Graph Reach Query | SHIPPED |  | `karac query affected-by` ships (returns callers/callees/tests). Out of date. |
| 6498 | Doctests as `#[example]` Blocks on `pub` Items | DEFERRED REFUSED | example_doctest_refused | `#[example]`: E_UNKNOWN_ATTRIBUTE. FIXED: names this section. |
| 6575 | Structured Runtime Traces Keyed to Source Spans | NO SURFACE |  |  |
| 6586 | Language Server (`kara-lsp`) | SHIPPED |  | `kara-lsp` ships as its own binary (release note B-2026-11-20-55). Out of date. |
| 6617 | Pre-checked Standard Library and Package Interfaces | NO SURFACE |  |  |
| 6629 | Bidirectional Compiler Hints | NO SURFACE |  |  |
| 6633 | Lint on Explicit `ref T` for Copy Primitives | NO SURFACE |  | Lint not written. |
| 6657 | Machine-Applicable Replacement Metadata on Typechecker / Effectchecker Diagnostics | NO SURFACE |  | Fix metadata coverage (karac fix exists; extensions are internal). |
| 6683 | Package Manifest Artifact Format (`.karapack`) | NO SURFACE |  | `.karapack`: no command. |
| 6705 | Test Framework Extensions | DEFERRED ACCEPTED | tool_fuzz_not_in_08, tool_bench_not_in_08 | `karac fuzz` / `karac bench` are unknown commands. But `#[property]` and `#[snapshot]` on a `test` block are recognised attributes and accepted silently (check passes); whether `karac test` gives them meaning is not shown. Refuse, or mark experimental. B-2026-11-25-66. |
| 6717 | #### Property tests | SUB |  | Part of “Test Framework Extensions”; covered by that row. |
| 6764 | #### Snapshot tests | SUB |  | Part of “Test Framework Extensions”; covered by that row. |
| 6784 | #### Fuzz tests | SUB |  | Part of “Test Framework Extensions”; covered by that row. |
| 6799 | #### Benchmarks | SUB |  | Part of “Test Framework Extensions”; covered by that row. |
| 6813 | Performance Diagnostics | NO SURFACE |  |  |
| 6828 | #### Cumulative Cost Surface | SUB |  | Part of “Performance Diagnostics”; covered by that row. |
| 6871 | Diagnostic Namespace Attributes (`#[diagnostic::*]`) | SUBSET |  | design.md § The diagnostic namespace is v1; the attribute takes `name: value` arguments (`=` is E0001). |
| 6875 | Tool-Namespaced Attributes | DEFERRED REFUSED | tool_attribute_refused | E_UNKNOWN_ATTRIBUTE: 'tool namespaces such as `#[clippy::…]` come later'. |
| 6924 | Cranelift as a JIT Backend | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6931 | Debug-Mode Leak Detector | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6935 | Migrating `shared` Types to `sync` | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6951 | Forward-looking — `.expose()` as an attention point | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6955 | Per-SCC Effect Diagnostics | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6981 | Perceus-Style In-Place Reuse | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 6995 | Karac-Side Bounds-Check Elimination Pass | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7013 | Interprocedural Range Facts | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7026 | Type-Based Alias Analysis (TBAA) | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7042 | Profile-Guided Optimization Loop | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7064 | Continuous PGO with Shared-Object Hot-Swap | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7094 | Runtime Monomorphization JIT | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7132 | MLGO Trained Policy Artifacts | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7144 | Verifier-Backed Query Resolution | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7158 | MLIR Adoption as Codegen Substrate | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7170 | Binding Generator (`kara-bindgen`) — Mechanizing the C-Shim Consume Path | TOOL ABSENT | tool_bindgen_not_in_08 | `karac bindgen`: unknown command. |
| 7187 | Rust ↔ Kāra Bidirectional Transpiler | TOOL ABSENT | tool_transpiler_not_in_08 | `karac transpile`: unknown command. |
| 7221 | File-Level Rust / Kāra Coexistence | NO SURFACE |  |  |
| 7251 | Blast-Radius / Test-Selection Service | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7273 | Agent-Facing Package Index | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7295 | Machine-Fix-Rate Benchmark and Public Leaderboard | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7319 | Shared Front End for `check` and `build` | NO SURFACE |  | Compiler internals, optimisation or a service; no program surface. |
| 7335 | ## Unscheduled language extensions | TRACK |  |  |
| 7339 | Generic trait impls for raw-pointer types | SUBSET | raw_pointer_generic_impl_refused | Stated status holds: E0240 with the stated message and this section. |
| 7345 | References to References (`mut ref (ref T)`) | SUBSET |  | Stated decision holds: `ref (ref i64)` collapses to a reborrow (probe accepted); pinned in corpus/core reborrow pins. |
| 7355 | Tail-Call Optimization (`#[tailrec]`) | DEFERRED REFUSED | tailrec_refused | E_UNKNOWN_ATTRIBUTE. FIXED: names this section. |
| 7380 | List Comprehensions | DEFERRED REFUSED | list_comprehension_refused | E0001 naming the iterator form; no section. Acceptable (the entry says it may never come). |
| 7388 | Generators (`yield`) | DEFERRED REFUSED | yield_refused | `yield` and `gen` are reserved (E_RESERVED_KEYWORD). |
| 7398 | `if let` Chains | DEFERRED REFUSED | if_let_chain_refused | E0001 'Kāra has no let-chains … nest the test'. No section. GAP (message). B-2026-11-25-71. |
| 7419 | Yielding Subscripts | NO SURFACE |  | Yielding subscripts: no syntax. |
| 7444 | Try Blocks (`try { ... }`) | DEFERRED REFUSED | try_block_refused | E_TRY_BLOCK_NOT_IMPLEMENTED_YET with the section. |
| 7489 | #### Implementing try blocks | SUB |  | Part of “Try Blocks (`try { ... }`)”; covered by that row. |
| 7509 | Trait Aliases | DEFERRED REFUSED | trait_alias_refused | E_TRAIT_ALIAS_NOT_IMPLEMENTED_YET with the section. |
| 7572 | #### Implementing trait aliases | SUB |  | Part of “Trait Aliases”; covered by that row. |
| 7593 | Generic Associated Types (GATs) | EXPERIMENTAL | experimental_gat_runs, explain_gat_says_experimental | Runs on both lanes. |
| 7649 | Type Alias `impl Trait` (TAIT) | DEFERRED REFUSED | tait_refused | E_TAIT_NOT_IMPLEMENTED_YET with the section. |
| 7675 | #### Implementing TAIT | SUB |  | Part of “Type Alias `impl Trait` (TAIT)”; covered by that row. |
| 7701 | Declared Variance Markers | DEFERRED REFUSED | variance_marker_refused | E_VARIANCE_USER_DECL_NOT_YET 'reserved for stdlib types in 0.8' (no section). GAP (message). B-2026-11-25-71. |
| 7741 | Higher-Kinded Polymorphism and Phantom Variance | NO SURFACE |  |  |
| 7747 | Units of Measure | DEFERRED REFUSED | units_of_measure_refused | No unit syntax: `f64<m>` is E0001 (Rust spelling). Nothing to refuse beyond that. |
| 7771 | Homogeneous Varargs | DEFERRED REFUSED | homogeneous_varargs_refused | E0001 'Expected type, found `..`'. GAP (message). B-2026-11-25-71. |
| 7790 | Heterogeneous Varargs / Variadic Generics | NO SURFACE |  |  |
| 7813 | Call-Site Spread | DEFERRED REFUSED | call_site_spread_refused | `add(..t)` is E0202 (arity). GAP (message). B-2026-11-25-71. |
| 7828 | User-Defined Callable Types (`Call` trait / `apply`) | DEFERRED REFUSED | call_trait_refused | E0104. FIXED: names this section. |
| 7857 | Struct Literal Type Prefix in Check Mode | NO SURFACE |  | Check-mode inference. |
| 7871 | Non-ASCII Identifiers | DEFERRED REFUSED | non_ascii_identifier_refused | E0001 'deferred to a future edition'. No section. GAP (message, minor). B-2026-11-25-71. |
| 7875 | `std.cli` — Argument Parsing | SUBSET |  | `std.cli` has its own import diagnostic naming deferred.md § std.cli (src/library_imports.rs). |
| 7901 | `OrderedMap[K, V]` / `OrderedSet[T]` — Insertion-Ordered Collections | DEFERRED REFUSED | ordered_map_refused | E0100/E0104. FIXED: names this section. |
| 7923 | Lookup by a Borrowed Form for User Key Types (`Borrow`) | NO SURFACE |  |  |
| 7931 | String Patterns (`Pattern` trait: `char`, `Str`, predicate) | NO SURFACE |  | String patterns: `split(',')` with a `char` is E0200 'write it as the string literal'; no Pattern name to refuse. |
| 7939 | Terminal Control Library (`std.terminal` or `kara-terminal`) | DEFERRED REFUSED | std_terminal_refused | E0112. FIXED: names this section. |
| 7965 | ## Permanent omissions | TRACK |  |  |
| 7971 | Dynamic Linking (Runtime `dlopen` / Plugin Loading) | DECLINED | dynamic_linking_refused | No dlopen module (`std.dylib` unknown). |
| 7989 | Full Bytecode-First JIT (HotSpot / V8 / JVM-class) | DECLINED |  | Nothing to write. |
| 8013 | Database `database/sql`-Class Stdlib | DECLINED |  | Nothing to write. |
| 8029 | Speculative Tiering with Deopt | DECLINED |  | Nothing to write. |
| 8052 | Declined — Adjacent Products Not Built on the Language | DECLINED |  | Nothing to write. |
