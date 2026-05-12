# 66 — General-purpose foundation, backend natural-fit, data as quiet bonus

**Status:** Graduated 2026-05-11. All locked decisions reflected in canonical docs: `deferred.md` (new P1 entries for `std.cli`, `std.embeddings`, `std.autograd`, lazy DataFrame Option A, stats sugar, data docs, `kara-postgres`, `kara-lsp`; P2 entries for lazy DataFrame expansion + frontend UI framework; P3 autograd entry restructured to NN-framework-only with decision deferred per Q7); `roadmap.md` (LSP into Phase 8.5 Track 3, GPU codegen pulled to v1 ship gate, lazy DataFrame lifted from v1.5 to v1, new P1 stdlib entries in Phase 8 + Phase 11, `kara-postgres` in Phase 8 Backend Platform); `implementation_checklist/phase-8-stdlib-floor.md` (v66 graduation P1 additions block with `[→ P1]` tags); `design.md` (GPU subset note confirming wgpu-primary satisfies vendor-neutrality; v1 Positioning section appended with v66 graduation summary). This brainstorm is now archival — the canonical docs are the source of truth. Ready for `brainstorming/archive/`.

**Trigger:** Conversation 2026-05-11 examining whether traditional ML / statistics / tabular workloads still matter in an LLM era, which led to a positioning question: should Kāra position as an AI-era data target, a general-purpose language, or something else? Conclusion locked in by the user: general-purpose is the non-negotiable foundation, backend is the natural fit for an AOT systems language (free with the tier), and data support ships at v1 as a quiet capability — not promoted as a positioning axis, but present and usable when developers reach for it. No-compromise build posture; ship features in P1 (v1) aggressively, accept punting to later phases later if scope forces it.

This brainstorm decides:
- Which items currently outside v1 should be **promoted to P1 (v1)** under the no-compromise stance.
- Which items should **stay deferred / P3 / out of scope** even under no-compromise — because the design enables them without the project owning them.
- The structural shape of the "quiet data bonus" — what ships, how it's discoverable, what carry-cost it implies.
- A meta observation worth recording: of the gaps identified, **none are feature/capability gaps**. All are prioritization, policy, or non-language-infrastructure work.

Per stored tier definitions: **v1 = P0 + P1**. P0 = load-bearing architectural commit; P1 = ships at v1, sequenced after the P0 spine. P2 = post-v1 but will ship. P3 = library/framework, may or may not.

Framing claim: the design is feature-complete for general-purpose + backend + data. The work remaining is sequencing, stdlib breadth, and a small number of deliberate policy / launch-posture calls.

---

## Problem 1 — Positioning frame (recorded so later sequencing inherits it)

The lead-persona pitch at v1:

> Kāra is a general-purpose AOT systems language with a flagship-grade concurrency runtime and effect-checked concurrency model.

What this commits to:

- **General-purpose is the foundation.** Anything not general-purpose-usable at v1 cannot land. This is the non-negotiable. A language without a working LSP, package registry, JSON, time, crypto, HTTP client, scripting/CLI ergonomics, and panic-debugging story is not a general-purpose v1.
- **Backend is the natural workload, not a separate persona.** An AOT systems language with effect types + auto-concurrency + 1M+ idle-connection runtime is a backend language by construction. The v64 backend-first floor stays — it's the credibility play, not a different language.
- **Data is a quiet bonus.** Tensor / Column / DataFrame + Arrow IPC + linalg + fft + random distributions + element-wise math all ship at v1 but are **not promoted as a positioning axis at launch**. The pitch reads "general-purpose AOT systems language"; users who arrive and discover "and it has first-class tensors" get the bonus, not the pitch.

What this rules out:

- "Kāra for AI" / "Mojo competitor" framings at v1. `@`-as-matmul stays settled-against because pattern-binding `@` is the right call for general-purpose. (GPU codegen is in v1 — see §5.2 — but as a systems-language compile-target capability, not an "AI play." Autodiff is in v1 — see §5.1 — as a stdlib capability under the data bonus, but the *pitch* still leads with general-purpose backend, not "ML framework.")
- Splitting v1 across two equal-billing flagship demos. The 1M-connection backend demo stays as the launch artifact; the data-engineering demo (Demo 3 in phase-8-stdlib-floor.md) ships as a verification artifact for floor-breadth, not a co-headline.

What this requires:

- Every Case 2 (general-purpose) gap identified in conversation must promote to P0 or P1 — these are launch-blockers under the chosen positioning.
- The data stdlib breadth stays committed as P1 (v1), even though it's not promoted. The "bonus" is real, not aspirational.

---

## Problem 2 — Items that need promotion to v1 (P0 or P1)

These were flagged as risks under the general-purpose-foundation framing. Each gets a recommended tier under the aggressive-keep-at-v1 stance.

### 2.1 LSP at v1 — promote from Future to P1

**Status today:** `roadmap.md:965-966` places `kara-lsp` binary + IDE integrations in the post-Phase-12 "Future" section, after self-hosting.

**Why this is a launch-blocker.** A general-purpose language v1 launch without VS Code / Neovim / JetBrains support out of the box does not get past the "I tried it but my editor was useless" early-adopter filter. Every successful general-purpose language post-2015 (Rust, Go, Swift, Kotlin, Zig late, Gleam) shipped editor integration at or before v1. Editor friction is a momentum-killer that no amount of language quality compensates for.

**Recommendation: P1 at v1.** The query API (`karac query`) and structured diagnostics infrastructure already exist; the LSP binary is a long-lived process wrapping the existing analysis surface. The work is plumbing, not new design. VS Code extension + Neovim built-in LSP client + JetBrains plugin can stage:
- **v1 ship:** `kara-lsp` binary + VS Code extension working day-one (syntax highlighting, diagnostics, go-to-def, hover, basic completion).
- **v1.x:** Neovim + JetBrains, refactoring, code actions, inline-explain, type lens.

**Cost estimate:** moderate. The analysis is reused; the LSP protocol layer + VS Code extension is well-understood engineering. Not "small," but feasible inside v1 scope.

**Carry cost:** maintaining the extension across LSP protocol revisions and IDE updates. Real, but standard.

### 2.2 Stdlib CLI argument parsing — promote to P1 at v1

**Status today:** `design.md:6251` confirms `env.args()` exists (carries `reads(Env)`). No high-level argparse surface in any phase checklist.

**Why this matters for general-purpose.** Every scripting/CLI workload — and a meaningful fraction of all v1 user code will be CLI tools — needs argument parsing beyond raw `env.args()`. Without a stdlib clap-class library, every user writes the same argument-parsing boilerplate or pulls a third-party crate before they've written their first feature. For a general-purpose v1, "argparse is third-party" is the wrong default.

**Recommendation: `std.cli` at v1, P1.** Minimum surface:
- `Parser::new(name: &str)` builder
- `parser.arg("--name", help: "...").required().value::<String>()`
- `parser.flag("--verbose", short: 'v')` for boolean flags
- `parser.subcommand("build", build_parser)` for nested commands
- `parser.parse() -> Result[Args, CliError]` materializes
- Automatic `--help` / `--version` / colored output
- Effect: `reads(Env)`

API shape can borrow from Rust's `clap` (declarative builder) or Python's `argparse`. The point at v1 is not perfection — it's having *something canonical in stdlib* so the ecosystem standardizes from day one.

**Cost estimate:** small. Mechanical engineering, no language-design questions.

### 2.3 Canonical Postgres driver — firm P1, project-owned

**Status today:** `deferred.md` "Permanent Omissions" → `database/sql`-class stdlib is explicitly community territory.

**The tension.** The stdlib-omission position is correct as a long-term principle. But for a general-purpose v1 launch, "go find a community database driver" when none yet exists is a credibility hole. Every general-purpose language launches with at least one working database story.

**Additional driver:** the user needs `kara-postgres` to dogfood Kāra against real backend workloads during v1 development. Even setting aside the launch-credibility argument, the driver is *internal* infrastructure for stress-testing the language on the workloads it's positioned to serve. This makes it firm P1, not "soft / community-may-also-do-this" P1.

**Recommendation: ship `kara-postgres` as a project-owned package, not stdlib.** Lives in a separate repo (`gowthamswe/kara-postgres` or under the `kara-lang` org), published to the package registry, installed via `karac add kara-postgres`. Officially endorsed at v1 launch as the canonical driver. **Handover-to-community policy:** deferred — not designing handover triggers now. Re-open the handover question at engineering-start time when the driver's actual maintenance shape is visible. For v1 development and launch, project owns it.

This preserves the stdlib position (Postgres is not in `std.*`) while removing the launch-day "there is no Postgres driver" objection and unblocking the user's own dogfooding.

**Minimum viable scope:** TCP connection, prepared statements, simple-query protocol, basic type mapping (i64 / String / f64 / bool / bytes / NULL / timestamp / uuid), transactions, prepared-statement parameter binding, `Pool[T]` integration. No advanced features (LISTEN/NOTIFY, COPY, async streaming) at v1.

**Cost estimate:** moderate. Driver work is straightforward but the binary protocol type-mapping surface is wide. Realistic single-person 4-6 weeks.

**Tier marker:** firm P1 at v1. The stdlib position (no `std.sql`) is unchanged; the package is project-owned and shipped with the language.

### 2.4 Argument-grade defaults — confirm all of the following are P0/P1 (audit, not new work)

These each need explicit confirmation in the implementation checklists. Most are likely already there; the work is verification.

- `std.json` ✓ confirmed landed 2026-05-09
- `std.time` ✓ confirmed Phase 8 floor
- `std.crypto` ✓ confirmed Phase 11+ P1 (v1)
- `std.http` client + server ✓ confirmed P0
- Package registry resolver + cache ✓ confirmed v1-P1 (pulled in 2026-05-08)
- `Backtrace` type + `PanicInfo` ✓ confirmed in design
- **Crash report format** — design.md references it; verify the Phase 8 implementation slice exists (the panic-strategy spec at phase-8-stdlib-floor.md:159 covers handler infra; verify the JSON crash report under `unwind` is committed as a Phase 8 slice).
- **`karac new --cli` template** — `karac new <name> --cli` should scaffold a working CLI tool with `std.cli` argparse, structured errors, help/version, and a useful starter `main()`. Verify the template content covers this (the `--data` template is mentioned at phase-8-stdlib-floor.md, `--cli` likely is too — confirm scope).

---

## Problem 3 — Data stdlib: aggressive completion under "quiet bonus" framing

The data stdlib breadth that ships at v1 (already P1 unless otherwise noted): Tensor[T, Shape] with rich shape typing, Column[T] with Arrow + bitmap nulls + SQL semantics + NaN handling, DataFrame, std.linalg, std.fft, std.einsum, std.random distributions, Arrow IPC / Parquet / CSV readers (Phase 8 floor), `.npy`/`.npz` I/O, Complex[T], Tensor.where + boolean indexing + meshgrid + scan ops + concat/stack/reshape, element-wise math + clamp.

Items to **promote into v1 under the no-compromise stance:**

### 3.1 `std.embeddings` — promote from "not committed" to P1

**Why.** RAG, semantic search, and recommendation workloads are mainstream now — every general-purpose language adopter doing AI-adjacent work expects to do cosine similarity and top-k batched dot products. The surface is small.

**Minimum surface (P1, v1):**
- `cosine_similarity(a: ref Tensor[f32, [D]], b: ref Tensor[f32, [D]]) -> f32`
- `cosine_similarity_batched(query: ref Tensor[f32, [D]], corpus: ref Tensor[f32, [N, D]]) -> Tensor[f32, [N]]`
- `top_k(scores: ref Tensor[f32, [N]], k: usize) -> Tensor[(i64, f32), [k]]` (indices + scores)
- `l2_normalize(t: ref mut Tensor[f32, S])` (in-place) and `l2_normalize_to(t: ref Tensor[f32, S]) -> Tensor[f32, S]`
- `dot_batched(a: ref Tensor[f32, [N, D]], b: ref Tensor[f32, [M, D]]) -> Tensor[f32, [N, M]]`

That's it for v1. Vector indices (HNSW, IVF, scalar quantization) stay community territory — those are stateful data structures with engineering trade-offs that the stdlib shouldn't pre-commit.

**Cost estimate:** small. Five functions over existing Tensor ops.

### 3.2 Lazy DataFrame query planner — promote to P1

**Why.** Eager DataFrame ops are fine for small data but the analytical workload that makes Polars beat pandas (and DuckDB feel cheap) is the lazy planner + query optimizer (predicate pushdown, projection pushdown, common-subexpression elimination). Without it, "Kāra has DataFrame" reads as "Kāra has a slow pandas." With it, "Kāra has DataFrame" reads as "Kāra has Polars-class analytics built in."

**Design shape (library on top of existing types, no new language features):**
- `LazyDataFrame` constructed via `df.lazy()` or `DataFrame.scan_csv(path).lazy()` / `.scan_parquet(path).lazy()`
- Expression API: `col("name")`, `col("a") + col("b")`, `col("x").mean()`, `when(...).then(...).otherwise(...)`
- Lazy operations return new `LazyDataFrame`: `.filter(expr)`, `.select([expr1, expr2])`, `.group_by([col("k")]).agg([col("v").sum()])`, `.join(other, on=..., how=...)`, `.sort(...)`, `.limit(...)`
- `.collect() -> DataFrame` materializes; `.explain() -> String` prints the optimized plan
- Optimizer passes: predicate pushdown, projection pushdown, common-subexpression elimination, push aggregations through joins where legal, scan-time filter

**This is genuine engineering, not just plumbing.** Polars' query optimizer in Rust is ~10K LOC. A v1 minimum-viable optimizer is smaller (~2-3K) but still real work. Worth it: the lazy planner is what makes the "data bonus" actually feel competitive vs. defaulting to DuckDB.

**Cost estimate:** moderate-to-large. Realistic 6-10 weeks of focused implementation. This is the single biggest scope-add in this brainstorm.

**Punt option if scope pressure forces it:** ship eager DataFrame at v1, lazy planner as a v1.1 deliverable announced at launch. Keeps the "data bonus" honest without forcing the lazy planner into the v1 critical path. Note: if punted, the v1 DataFrame story is meaningfully weaker than Polars — set expectations accordingly.

### 3.3 Tensor strides — confirm P1 at v1 (not just Phase 11 timing)

**Status today:** `design.md:12332` says Arrow strides are committed at the layout level; Phase 11 has the implementation. The tier on the strides implementation needs explicit confirmation as P1 (v1), not P2.

**Why this matters.** Without strides, every `arr.T` / `arr.transpose()` / step-slice materializes a copy. For ML data prep that's a real perf cliff. The `.compact()` escape hatch is the right primitive — but only if strided views are the default.

**Recommendation: explicit P1 tag at v1.** No new design work; verify the Phase 11 entry carries the `[→ P1]` marker per the priority-tier convention (`feedback_wip_lists_uncommitted.md` references this convention; cross-check with `priority_tiers.md`).

**Cost estimate:** small. Implementation work in Phase 11, already designed.

### 3.4 Statistical sugar on Column / DataFrame — promote to P1

**Why.** General-purpose data work routinely calls `.mean()`, `.std()`, `.var()`, `.quantile()`, `.median()`, `.corr()`, `.describe()`. These are individually trivial; the question is whether they're committed as v1 stdlib surface or punted to "community will fill it in."

**Recommendation: P1 at v1.** The methods are mechanical (reductions over Column[f64]) and the absence of them is the kind of "Kāra doesn't have basic stats?" objection that's cheap to prevent. Surface:
- `Column[T: Numeric]`: `.mean() -> T`, `.std() -> T`, `.var() -> T`, `.median() -> T`, `.quantile(q: f64) -> T`, `.min()`, `.max()`, `.sum()`
- `Column[f64]`: above + `.corr(other: ref Column[f64]) -> f64`
- `DataFrame.describe() -> DataFrame` (count / mean / std / min / quartiles / max per numeric column)

**Cost estimate:** small. Pure reduction code over existing primitives.

---

## Problem 4 — Discoverability: data depth needs surfacing without promotion

The "quiet bonus" framing has a real discoverability cost. If the data stdlib ships but isn't promoted, users arriving at Kāra from the general-purpose angle won't know it's there.

**Recommendation: P1 at v1, documentation-only.**

### 4.1 `docs/book/src/data.md` — dedicated book chapter

Single chapter under the existing book structure. Covers:
- Tensor: rank, shape types, indexing, broadcasting, common ops
- Column: nullable 1D data, null semantics, NaN handling, Arrow layout
- DataFrame: schema, read_csv / read_parquet / write_*, basic ops
- One end-to-end example (~50 lines): load CSV → filter → group by → compute → write Parquet
- Pointers to `std.linalg`, `std.fft`, `std.einsum`, `std.embeddings`, `std.random.distributions` with one-line each

Not a promotional document — a structural reference so the depth is reachable from the book's table of contents.

### 4.2 `examples/data/` directory with 3-4 small programs

Each ~30-80 lines:
- `csv-to-parquet.kara` — basic ETL
- `embeddings-rag.kara` — load corpus, embed (calls an external embedder via HTTP), top-k semantic search
- `stats-summary.kara` — group-by + describe over a CSV
- `lazy-query.kara` — Polars-class lazy DataFrame analytical query

These also serve as integration tests against the data stdlib — verifying the breadth actually composes.

**Cost estimate:** small. Doc + example work, no new code.

---

## Problem 5 — Items that stay deferred or settled, even under no-compromise

The no-compromise stance does *not* mean "promote everything." Some items should stay where they are because they conflict with the positioning or have legitimate scope reasons to defer.

### 5.1 Autodiff — promote to P1 (moved out of this section)

**Decision:** Autodiff (reverse-mode gradient engine on Tensor) ships at v1 as `std.autograd`. Promoted from P3 because the user committed to it as v1 scope under the no-compromise stance.

**Why this is meaningful even under "data is a quiet bonus" framing.** Autodiff is the dividing line between "Kāra has tensors" (commodity capability — every language has a tensor library) and "Kāra can train models" (a category most general-purpose languages do not occupy at launch). Combined with GPU codegen also at v1, this puts Kāra in a position where someone evaluating "can I do ML work here?" gets a yes — not as the pitch, but as a discoverable depth.

**Scope split:** autograd (gradient engine) vs neural-net framework (layers, optimizers, training-loop helpers) are separable. The user's "autodiff goes in P1" lands `std.autograd` cleanly. Whether `std.nn` (Linear, Conv2d, BatchNorm, etc.) and `std.optim` (SGD, Adam) also ship at v1 is a separate question worth pinning — see Open Question §7.

**Minimum viable v1 `std.autograd`:**
- `shared struct Tape` — single-use, append-only record of operations. Effect: `writes(GradTape)`.
- `Tensor[T, S]` with `requires_grad: bool` flag (or equivalent — a wrapper type `Var[T, S]` is cleaner; design choice worth pinning at engineering-start time).
- Operator overloads for differentiable ops on tracked tensors: `+`, `-`, `*`, `/`, matmul, broadcasting, reductions (`sum`, `mean`), reshape, transpose, indexing.
- Activations with hand-coded backwards: `relu`, `sigmoid`, `tanh`, `softmax`, `gelu`, `silu`.
- Loss functions with backwards: `mse_loss`, `cross_entropy`, `binary_cross_entropy`.
- `grad(fn: F, args: Args) -> Args::Grads` — higher-order, returns gradients w.r.t. inputs.
- `value_and_grad(fn: F, args: Args) -> (Output, Args::Grads)`.
- Reverse-mode only at v1. Forward-mode and higher-order derivatives are Phase 11+ or post-v1.
- GPU-aware: autograd ops on `Tensor` that live on GPU record on a GPU-side tape (or host-side tape with GPU kernel launches as the recorded operations).

**Out of v1 `std.autograd` scope:**
- Forward-mode AD.
- Higher-order gradients (`grad(grad(f))`).
- Custom backward definitions (`@custom_vjp` decorator equivalent). Stdlib-blessed ops only.
- Checkpointing / activation rematerialization.
- JIT-traced graphs (eager only at v1).
- Distributed AD / multi-GPU gradient sync.

**Cost estimate:** moderate-to-large. Tape engine + differentiable ops over existing Tensor primitives + GPU integration is real work but standard — every major ML framework has a published implementation to reference. Realistic 2-4 months focused.

**Carry cost:** every new differentiable Tensor op must ship with its backward; every breaking change to Tensor semantics must consider autograd compatibility. Real but bounded.

**Design dependency note.** Autograd at v1 makes the GPU codegen scope question (Open Q §6) more urgent — autograd that's CPU-only at v1 with GPU codegen also at v1 is incoherent. Either autograd works on GPU tensors at v1, or the v1 GPU story is narrower than implied. Worth pinning at engineering-start.

### 5.2 GPU codegen — promote to P1 (moved out of this section)

**Decision:** GPU codegen ships at v1. The case for staying at Phase 10 (post-v1) was made under the "data is a quiet bonus" framing — but GPU matters for both axes of v1, not just the data axis:

- **Systems-language credibility.** A serious AOT systems language in 2026+ that can't target GPU is one capability short of the full hardware story. The pitch is "the compiler handles memory layout and concurrency; the programmer handles intent — and hardware targets, like GPU, when they matter." That promise is empty without GPU codegen at launch.
- **Backend workloads.** Embedding inference, vector-search re-ranking, image/video transcoding, and increasingly LLM inference on the request path are mainstream backend workloads. A backend-positioned language without GPU is a language that hands those workloads to Python+CUDA.
- **Data workloads.** Tensor operations at scale only feel competitive when GPU is reachable. CPU-only Tensor is fine for prep; GPU is what makes the data story not-toy.
- **Design is done.** GPU subset constraints, `GpuSafe` trait, `#[gpu]` attribute, call-graph validation, kernel-launch effects — all specified. The lift to v1 is implementation, not new design work.

**Recommendation:** Pull GPU codegen forward from Phase 10 into the v1 P1 window. Roadmap entries for Phase 10 (Additional Compilation Targets) need re-sequencing — GPU moves into the v1 window; WASM stays at Phase 10 as the remaining target.

**Multi-vendor commitment at v1 (CUDA + Apple Metal at minimum).** Rejecting the initial CUDA-only framing — see Open Q §6 resolution. Reasons: (a) user develops Kāra on a Mac; CUDA-only means he cannot dogfood GPU codegen on his own machine, same failure pattern as Postgres dogfooding (Q3); (b) backend developer hardware in 2026 is heavily Mac-based — CUDA-only means every Mac-using backend developer hits "GPU codegen doesn't work on my machine" in their first hour. ROCm and SPIR-V/Vulkan stay post-v1.

**Detailed scope (codegen boundary, stdlib subset on GPU, launch ergonomics, BLAS/MPS shims, GPU memory pool) deferred to engineering-start time.** Documented placeholders only at this brainstorm:
- `#[gpu]` kernel attribute, `GpuSafe` trait, call-graph validation enforcing GPU subset constraints — load-bearing at v1, exact spec deferred.
- API design must be vendor-neutral from day one (see Open Q §6 resolution).
- Memory transfer primitives: `Tensor.to_gpu()`, `Tensor.to_cpu()`, explicit allocation effects.
- Effect system integration: GPU kernel calls carry appropriate `allocates(GpuMem)` / execution verb effects.
- Subset of stdlib usable in GPU: at minimum `std.math` + element-wise Tensor ops; broader scope deferred.

**Out of v1 GPU scope:**
- ROCm, SPIR-V/Vulkan. (CUDA + Metal only at v1.)
- Multi-GPU / NCCL / collective operations.
- Auto-kernel-fusion / kernel-graph optimizers.
- `std.einsum` GPU lowering. (CPU `std.einsum` ships; GPU lowering of einsum stays post-v1.)

**Cost estimate:** large. The single biggest scope add in this brainstorm; multi-vendor pushes the estimate further. Engineering-start scoping will pin the realistic timeline. Acceptable under the no-compromise stance per user's explicit instruction.

**Carry cost:** non-trivial. Each language change must re-validate GPU subset across vendors; each runtime change must consider GPU host-side ergonomics; CUDA + Metal toolchain coverage at install time. Documented as a real ongoing cost.

**Punt-fallback (if scope pressure forces it):** ship `#[gpu]` + `GpuSafe` + call-graph validation + a single trivial-kernel demo on both CUDA and Metal at v1, full stdlib-on-GPU coverage at v1.1. Keeps the multi-vendor language commitment visible while letting the breadth lag.

### 5.3 `@` matmul stays settled-against

**Why no change.** Pattern-binding `@` is the right call for a general-purpose language. `a.matmul(b)` is fine for the rare numerical user; method call ergonomics are not a launch blocker.

### 5.4 Frontend UI framework — P2 (post-v1, will ship)

**Decision:** Frontend UI framework is **P2, not P3**. It is not optional — it ships post-v1. A general-purpose language with no story for the browser is, in 2026+, a language with a hole where most consumer-facing software lives. Kāra needs an answer; the answer just doesn't have to be at v1.

**What this means concretely:**
- WASM as a compile target stays Phase 10 (v1). Codegen target completeness for WASM is unchanged.
- A React/Solid-class component framework on top of WASM is committed for post-v1. Lives in `deferred.md` under a new P2 section (currently P3 — needs promotion).
- DOM/JS-interop ergonomics layer — the bridge between Kāra's type system and the browser DOM API surface — needs design work that does *not* yet exist. This is genuine pre-implementation design effort, not just "implement what's already designed."
- The framework itself (component model, reactivity, routing, SSR story) is also pre-design — needs a separate brainstorm at the time it's pulled forward.

**Why P2 not P1.** Under the locked positioning (general-purpose foundation, backend natural-fit, data quiet bonus), the v1 launch story does not require frontend. Pulling it into v1 trades 6-12 months of frontend design+impl against a launch that already has enough surface to defend ("general-purpose AOT systems language with flagship-grade concurrency, plus data, plus GPU"). Better to ship v1 and then commit serious effort to a frontend story than to delay v1 for it.

**Why P2 not P3.** P3 framing ("library on top, may or may not ship") understates the importance. The project will ship a frontend story; the only question is which post-v1 release it lands in.

**Pre-design work that should start during v1 development (not blocking, not P1):**
- Sketch DOM/JS-interop type-system bridge. How does an effect-typed language interact with JS callbacks? What's the equivalent of `wasm-bindgen`?
- Survey the design space (Yew, Leptos, Sycamore, Dioxus from Rust; Solid/React from JS). What does Kāra's effect system change about the reactivity model?
- Identify whether the framework is a separate-team-effort post-v1 or a project-owned reference (parallel to the `kara-postgres` decision).

**Cost estimate (when it does get built):** large. Realistic 6-12 months for a credible v1 frontend framework with reactivity, component lifecycle, DOM diffing, routing, basic SSR. Plus ongoing maintenance commitment that's larger than any other stdlib module.

### 5.5 `karac publish` stays out of v1

**Why no change.** Registry proxy client + git-URL identity + lockfile are in v1. `karac publish` is gated on adoption signals (correct call). Users at v1 publish via git URLs; the publish command lands when the ecosystem proves it needs one.

---

## Problem 6 — Meta: most "gaps" are prioritization, not capability

Worth recording explicitly because it's load-bearing for project posture.

**Of every item discussed across this brainstorm, none are feature/capability gaps in the design.** Every gap reduces to one of:

1. **Prioritization** — when does this stdlib module / feature ship? (LSP, argparse, embeddings, lazy DataFrame planner, GPU codegen, autodiff)
2. **Policy** — does the project own this or is it community territory? (Postgres driver, autodiff library, frontend framework)
3. **Non-language infrastructure** — service operation, ecosystem partnerships, doc surface. (Package registry service, IDE marketplace presence, ecosystem onboarding)
4. **Discoverability** — is the capability visible from launch-day docs? (Data depth without data promotion)

This is the strong-position observation. Most pre-v1 languages have unresolved feature questions still in flight. Kāra is past that point. The decisions remaining are sequencing, scope, and posture — all easier classes of decision than "is this even possible."

**What this implies for v1 sequencing.** The bottleneck is engineering throughput against a known feature surface, not design-decision throughput. That changes how to think about timeline pressure: scope-cuts at v1 should be lever-pulled on stdlib breadth (e.g., punt lazy DataFrame planner to v1.1) rather than on language-design completeness.

---

## Decision summary

| Item | Current state | Recommended | Cost | Punt-fallback |
|---|---|---|---|---|
| LSP at v1 | Post Phase 12 (Future) | P1 — `kara-lsp` + VS Code at v1, others at v1.x | Moderate | VS Code only at v1, Neovim/JetBrains v1.1 |
| `std.cli` argparse | Not committed | P1 — at v1 | Small | None — must ship |
| Canonical Postgres driver | Stdlib omission | **Firm P1 — project-owned package**; handover policy deferred to engineering-start | Moderate (4-6 wk) | Doubles as dogfooding infra; no v1 punt — project ships it |
| Crash report format slice | Implied in spec | Verify Phase 8 slice exists | n/a | Verification only |
| `karac new --cli` template | Likely exists | Verify scope covers argparse + stack traces | n/a | Verification only |
| `std.embeddings` | Not committed | P1 — at v1 (5-function surface) | Small | Defer to v1.1; flag at launch |
| Lazy DataFrame planner | Not committed | P1 — at v1 | Moderate-to-large | Eager at v1, lazy v1.1 announced at launch |
| Tensor strides at v1 | Phase 11 | Confirm P1 tag | Small | Already designed — implementation only |
| Stats sugar on Column / DataFrame | Not explicitly committed | P1 — at v1 | Small | None — table stakes |
| `docs/book/src/data.md` | Doesn't exist | P1 — at v1 | Small | None — doc work |
| `examples/data/` programs | Doesn't exist | P1 — at v1 | Small | None — doc work |
| Autodiff (`std.autograd`) | P3 | **P1 — promote to v1** (reverse-mode, GPU-aware) | Mod-large (2-4 mo) | Reverse-mode CPU-only at v1; GPU autograd v1.1 |
| NN framework (`std.nn`, `std.optim`) | P3 | **Open Q §7** — decide at engineering-start | Mod (1-2 mo if shipped) | Defer to community / v1.1 |
| GPU codegen | Phase 10 | **P1 — promote to v1** (CUDA + Metal at minimum; vendor-neutral API from day one); detailed scoping deferred to engineering-start | Large (engineering-start will pin) | Subset at v1 (attribute + validation + trivial demo on both vendors), full stdlib-on-GPU at v1.1 |
| `@` matmul | Settled against | No change | n/a | n/a |
| Frontend UI framework | P3 (out of scope) | **P2 — post-v1, will ship**; pre-design starts during v1 | Large (6-12 mo when built) | n/a — not v1 work |
| `karac publish` | Out of v1 | No change | n/a | Adoption-signal-gated |

**Net new v1 scope:** LSP (mod), `std.cli` (small), `kara-postgres` package (mod), `std.embeddings` (small), lazy DataFrame (mod-large), stats sugar (small), data docs + examples (small), **GPU codegen CUDA-only (large)**, **`std.autograd` reverse-mode (mod-large)**. Total realistic engineering: 8-15 months of focused work on top of current v1 trajectory. GPU codegen + autograd are the dominant new line items and the dominant risk; they are also interdependent (autograd-on-GPU is the coherent v1 story). Acceptable under the user's stated "take additional time if needed" stance; if scope pressure forces cuts, the highest-leverage punts in order are (1) GPU subset-at-v1 fallback (attribute + validation + demo, full coverage v1.1), (2) autograd CPU-only at v1 with GPU autograd at v1.1, (3) lazy DataFrame planner to v1.1.

---

## Open questions for follow-up (not blocking)

1. ~~**Lazy DataFrame planner scope.**~~ **Resolved 2026-05-11. Option A locked for v1.** Minimum-viable optimizer, written fresh: predicate pushdown + projection pushdown + constant folding + CSE. Target ~2-3K LOC, 6-8 weeks focused. Docs explicitly frame as "Polars-comparable on simple-to-moderate queries, weaker on complex multi-join analytics; reach for DuckDB for warehouse queries." Two alternatives deferred to P2 (graduate to `deferred.md` later):
   - **P2 — Lazy DataFrame Query Optimizer Expansion.** Adds join reordering, filter combining, push aggregations through joins, scan-time filter pushdown, projection-aware Parquet reads. Target ~5-7K LOC additional, 3-4 months focused. This is what makes "Polars-class" honest. Lands as v1.1 or v1.2 follow-on. Non-breaking — optimizer extension only; user-facing `LazyDataFrame` API unchanged. **Re-evaluation trigger:** v1 user feedback showing multi-join analytical workloads as a recurring friction point, OR a flagship-data-engineering demo where the v1 optimizer's join handling is the visible weakness.
   - **P2 alternative (designs-not-taken) — DataFusion integration.** Considered at v1: wire `LazyDataFrame` → DataFusion `LogicalPlan` → DataFusion optimizer → Kāra physical execution. Rejected because (a) plan-IR bridge work in both directions is non-trivial and underestimated by the "4-6 weeks integration" framing, (b) external optimizer dependency conflicts with the language's "owns the stack" posture, (c) Kāra Column nullability and NaN semantics would have to bend to DataFusion's Arrow assumptions or accept a semantic-mismatch layer. Documented as the alternative considered so future contributors don't re-litigate. If Option B expansion proves harder than expected post-v1, this revives as a fallback — but with full awareness of the trade-offs above.

2. ~~**LSP feature scope at v1.**~~ **Resolved 2026-05-11.** Three buckets pinned:
   - **v1 floor (must ship):** syntax highlighting (TextMate grammar), diagnostics streaming, go-to-definition, hover (type + effect signature), find references, document symbols / outline, **type-aware completion** (method/field completion after `.`, requires partial-parse + typecheck-of-incomplete-source — ~4-6 weeks engineering), formatting via LSP, signature help (parameter-info popup).
   - **v1 stretch (ship if engineering time allows, else v1.1):** rename symbol, code actions (apply structured fix-diffs from `karac`), semantic tokens (full semantic highlighting beyond TextMate), workspace symbols / global search.
   - **v1.x explicitly (post-launch):** **effect-aware completion** (filter `.`-completions by effect compatibility with the current `with`-clause — Kāra-specific differentiator, ~2-3 weeks on top of type-aware), inline-explain / type lens (surface `karac explain` reasoning in-editor — high marketing-value, distinctive), refactoring (extract function, inline variable).
   - **Reasoning recap:** type-aware completion is the line below which the LSP feels half-broken — table stakes for any general-purpose language v1. Effect-aware completion is a delightful differentiator best landed post-launch when the floor is solid and shippable as a "Kāra LSP now does X" announcement.

3. ~~**`kara-postgres` ownership transition.**~~ **Resolved 2026-05-11.** Driver itself is firm P1 (doubles as dogfooding infra; user needs it to stress-test Kāra on real backend workloads during v1 development). Community-handover policy is **explicitly deferred** — not designing handover triggers now. Re-open the handover question at engineering-start time when the driver's actual maintenance shape is visible. For v1 development and launch, project owns it without timeline pressure to hand off.

4. ~~**Cost-model relative ranking.**~~ **Resolved 2026-05-11.** Flexibility-documentation, not active scope decision. If scope pressure ever forces v1 cuts, the punt order is pre-decided so no re-litigation under timeline duress. Engineering proceeds aggressively on all P1 items absent that pressure.

   **Ranking by "hardest to defer without launch-day pain" (highest pain first):**
   1. LSP floor — editor friction kills momentum; cohort that tries Kāra in week 1 leaves and doesn't come back if VS Code support is missing.
   2. `std.cli` — every scripting/CLI workload needs it; punting screams "stdlib breadth is poor."
   3. `kara-postgres` — firm P1, doubles as dogfooding infra. **Unpuntable** — punting it punts the ability to validate the language's distinctive capabilities (effect system, `Pool[T]`, auto-concurrency, `with_provider`) on real backend workloads during v1 development.
   4. GPU codegen — full punt = "AOT systems language with no GPU" hole; subset-at-v1 fallback exists.
   5. Stats sugar — cheap to do; without `.mean()` / `.std()` / `.describe()` the data bonus reads as barely usable.
   6. Data docs + examples — discoverability gate for the data bonus; cheap.
   7. `std.embeddings` — 5-function surface; cheap; absence forces hand-rolled cosine similarity.
   8. `std.autograd` — elevates the data story significantly; absence weakens but doesn't kill it given backend-headlined positioning.
   9. Lazy DataFrame planner — eager DataFrame works without it; fallback ("eager at v1, lazy at v1.1 announced at launch") is honest and viable.

   **Punt sequence if scope pressure hits (most-defensible-cut first):**
   1. Lazy DataFrame planner → v1.1 (eager ships, lazy announced)
   2. `std.autograd` → v1.1 (Tensor ships, autograd announced)
   3. `std.embeddings` → v1.1 (or keep — 5 functions, cheap)
   4. GPU codegen full → subset-at-v1 fallback (attribute + validation + trivial demo at v1; full stdlib-on-GPU at v1.1)
   5. LSP stretch features → v1.1 (keep floor; punt rename, code actions, semantic tokens, workspace symbols)

   **Never punt:** LSP floor, `std.cli`, `kara-postgres`, stats sugar, data docs, GPU subset (attribute + validation + demo).

5. ~~**WASM-with-DOM design effort (P2 frontend pre-design).**~~ **Resolved 2026-05-11.** Frontend stays at P2. Pre-design timing decision **explicitly deferred** — not committing to "start during Phase 8/9" or "defer until v1 ships" now. Re-open the question once v1 implementation is underway and there's concrete signal about (a) available bandwidth, (b) whether early adopters are asking for a browser story, (c) whether the user himself wants to start exploring frontend during v1. At that point: promote to P1 if conditions warrant, leave at P2 if not. Same pattern as Q3 (Postgres handover): defer governance/timing decisions until there's real signal to inform them.

6. ~~**GPU codegen v1 scope detail.**~~ **Partially resolved 2026-05-11.** GPU codegen stays at P1. **CUDA-only assumption rejected** — multi-vendor including Apple Metal is required at v1 because (a) the user develops Kāra on a Mac and cannot dogfood GPU codegen with CUDA-only (same pattern as Postgres dogfooding from Q3), (b) backend developer hardware reality in 2026 is heavily Mac-based — CUDA-only means every Mac-using backend developer hits "GPU codegen doesn't work on my machine" in their first hour with Kāra. Detailed scoping (codegen boundary, stdlib subset on GPU, launch ergonomics, BLAS shim, GPU memory pool) **explicitly deferred** to engineering-start time.

   **One sub-question worth surfacing before that detailed-scoping moment:** the v1 GPU API design must be vendor-neutral from day one regardless of which backends ship initially. If the abstraction is CUDA-flavored at v1 (PTX intrinsics leaking, CUDA cooperative groups in the surface), then adding Metal later is a v2 API redesign. If the abstraction is vendor-neutral from v1, then "ship CUDA + Metal at v1" and "ship vendor-neutral API with CUDA-only implementation at v1, Metal implementation v1.1" become two viable options. The API-design question is the load-bearing one and affects how the `#[gpu]` attribute and `GpuSafe` trait get spec'd — worth flagging to revisit before engineering-start.

7. ~~**`std.nn` and `std.optim` at v1?**~~ **Resolved 2026-05-11.** Decision deferred — same pattern as Q3 (Postgres handover) and Q5 (frontend pre-design timing). Autograd is the load-bearing primitive locked at v1; `std.nn` (layers, Sequential) and `std.optim` (optimizers, lr schedulers) are pure composition on top, with no language-primitive at stake. Decide at engineering-start when there's signal on (a) how clean the manual-layer-composition story feels with autograd-only, (b) whether early v1 users / dogfooding workloads are asking for layer abstractions in stdlib, (c) whether positioning-tension (NN framework pulls Kāra harder toward "ML framework" framing) has cashed out in practice. Until then: stays out of v1 scope by default; promoteable to P1 if signal warrants.

8. ~~**Wrapper type for autograd: `Tensor.requires_grad: bool` vs separate `Var[T, S]` type?**~~ **Resolved 2026-05-11. Option B locked: separate `Var[T, S]` wrapper type.** Reasoning: (a) Kāra's type system (shape types + effect types + ownership) is the differentiator — autograd should leverage it, not bypass it with runtime flags; (b) effect tracking is cleaner — only `Var` ops carry `writes(GradTape)`, vs A where every Tensor op potentially writes to a tape and effect inference can't distinguish; (c) shape types compose naturally as `Var[f32, [N, D]]`; (d) PyTorch chose `requires_grad: bool` because Python's type system couldn't express the alternative — Kāra doesn't have that constraint and inheriting A would be inheriting a workaround. Low-level design (Tensor↔Var conversion ergonomics, `Differentiable` trait shape, duplicating Tensor API surface on `Var`, operator overloading once-on-trait vs twice-on-types) deferred to engineering-start when `std.autograd` work begins.

---

## Cross-references

- `design.md § v1 Positioning — Backend-First` — current v1 positioning narrative; the "general-purpose foundation, backend natural fit, data quiet bonus" frame is compatible (backend stays the headline workload).
- `docs/roadmap.md § Phase 8: Standard Library — Floor` — most of the new P1 items land here.
- `docs/implementation_checklist/phase-8-stdlib-floor.md` — where the verification items (crash report slice, `karac new --cli` template) get checked.
- `docs/deferred.md § P3 — Post-v1 Build Targets` — autodiff and frontend UI framework live here; this brainstorm leaves them in place.
- `brainstorming/archive/v64_backend_first_v1_concurrency.md` — established the backend-first floor that the general-purpose positioning inherits.
- `memory/project_priority_tiers.md` — authoritative tier definitions used throughout this doc.
