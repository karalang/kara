//! CLI command dispatch and compiler pipeline orchestration.
//!
//! Handles subcommand parsing, output modes (text/json/jsonl),
//! and running the appropriate compiler phases.

use crate::ast::EffectVerbKind;
use crate::ast::{Function, Item, Program};
use crate::concurrency::ConcurrencyAnalysis;
use crate::effectchecker::{DeclaredEffects, EffectCheckResult, EffectErrorKind};
use crate::interpreter::{DbgOutputMode, ErrorTraceFrame, Interpreter, TestOutcome};
use crate::manifest;
use crate::module::{
    self, BuildTreeError, BuildTreeOk, BuildTreeOpts, Cycle, ModuleId, ModuleParseErrors,
    ProgramTree,
};
use crate::ownership::{OwnershipCheckResult, OwnershipMode};
use crate::parser::ParseResult;
use crate::resolver::ResolveResult;
use crate::resolver::{ResolveError, ResolveErrorKind, Resolver};
use crate::scaffold::{self, ScaffoldOpts, Template};
use crate::token::Span;
use crate::typechecker::TypeCheckResult;
use crate::walker::{self, EntryKind, WalkResult, WalkerOpts};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;
use std::process;

mod args;
pub mod explain;
mod help;

pub use args::parse_args;
use help::print_help;

// ── Output Mode ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputMode {
    Text,
    Json,
    Jsonl,
}

// ── WASM bindings mode ──────────────────────────────────────────

/// `--bindings browser|component|none` — output-shape selector for the
/// WASM build path (`design.md § Target Build Artifacts`, phase-10
/// `--bindings` flag entry). The flag has no meaning on non-WASM
/// targets (it is accepted-but-inert there); on a WASM build the
/// default is inferred from the target — `wasm_browser` → `Browser`,
/// `wasm_wasi` → `Component` — because the `--target` choice already
/// declares the host family (no universal default, no silent
/// browser-lock-in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingsMode {
    /// ES-module JS glue next to the `.wasm` (`<stem>.js` — host fn
    /// import plumbing + WASI preview-1 polyfill; see `wasm_glue`).
    Browser,
    /// Component Model output — a single embedded-WIT component
    /// (`<stem>.wasm` IS the component; wasmtime/jco-class hosts run
    /// it directly): the C-ABI core module is lifted via the external
    /// `wasm-tools` binary (`componentize`; pinnable through
    /// `kara.toml` `[toolchain]`), with `host fn` imports lowered to
    /// canonical-ABI `kara:<pkg>/host` entries (see `wit` /
    /// `target::wasm_component_host_package`). The phase-10
    /// "embedded-WIT migration" swap of the former paired default.
    Component,
    /// Raw `.wasm` only — no glue, no declarations. For users wrapping
    /// Kāra WASM with custom host integration.
    None,
}

// ── Native crate type (producer-mode library artifacts) ─────────

/// `--crate-type bin|staticlib|cdylib` — native artifact-kind selector
/// for the *producer* half of additive interop (`design.md § Exported C
/// ABI`; [`spikes/additive-interop-adoption.md`] Slice 2). `bin` (the
/// default) builds an executable as always; `staticlib` / `cdylib` build
/// a linkable library exposing the program's `pub extern "C" fn` surface
/// with a C ABI, so a foreign C / Rust host can `#include` the emitted
/// header and link the Kāra kernel in. Native targets only — a wasm
/// build has its own export surface (`--bindings`), so the flag is
/// rejected there rather than silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeCrateType {
    /// Executable — the historical `karac build` behavior.
    Bin,
    /// `.a` static archive (thick: the runtime archive is bundled in, so
    /// the consumer links with no karac toolchain present).
    StaticLib,
    /// `.so` (Linux) / `.dylib` (macOS) shared library (the runtime is
    /// statically pulled in; on macOS the install-name is `@rpath`-based).
    CDylib,
}

// ── Subcommands ─────────────────────────────────────────────────

#[derive(Debug)]
pub enum Command {
    Run {
        file: String,
        output: OutputMode,
        sequential: bool,
        /// Optional `--manifest=<path>` override (tracker line 898).
        /// When `Some`, the supplied `kara.toml` is loaded *as if* it
        /// were discovered at the script's directory. Mutually
        /// exclusive with `no_manifest`.
        manifest_override: Option<String>,
        /// `--no-manifest` (tracker line 898): skip manifest
        /// discovery entirely and run stdlib-only. Mutually exclusive
        /// with `manifest_override`.
        no_manifest: bool,
        /// Build-wide lint level overrides set via `-A NAME` /
        /// `-W NAME` / `-D NAME` / `-F NAME` / `-D warnings`. Slice
        /// 4b polish. Threaded into [`Pipeline`] via
        /// [`Pipeline::with_lint_overrides`].
        lint_overrides: crate::lints::CliLintOverrides,
        /// Optional `--timeout DURATION` opt-in wall-clock cap on the
        /// interpreter (tracker line 861). `None` for the default
        /// behaviour: no cap — `karac run` legitimately targets
        /// long-running services (web servers, daemons, REPLs, batch
        /// jobs) where a default would silently break real workloads.
        /// `Some(d)` makes the runner fail loudly after `d` instead
        /// of hanging — useful for CI smoke tests, scripted
        /// invocations, and exploratory `karac run examples/foo.kara`
        /// where forgetting about a runaway costs real laptop
        /// battery. Exit code on timeout: 124, matching GNU
        /// `timeout(1)` so existing shell pipelines compose.
        timeout: Option<std::time::Duration>,
        /// `--interp`: force the tree-walk interpreter instead of the default
        /// LLJIT executor (LLJIT-productionization Slice 6c — the `karac run`
        /// JIT-default flip, mirroring the Slice-5 repl/test flip). The
        /// interpreter is retained as a dev/debug backend (design.md § Tree-walk
        /// interpreter (dev / debug only)); this is the ergonomic equivalent of
        /// `KARAC_RUN_JIT=0`. No-op on a non-`llvm` build (the interpreter is the
        /// only executor there), and the interpreter is also used regardless for
        /// the affordances the JIT one-shot doesn't provide — `--output=json`/
        /// `jsonl` structured run envelopes and the `--timeout` cooperative
        /// deadline.
        interp: bool,
    },
    RunExample {
        name: String,
        output: OutputMode,
        sequential: bool,
        /// See [`Command::Run::lint_overrides`].
        lint_overrides: crate::lints::CliLintOverrides,
    },
    Check {
        file: String,
        output: OutputMode,
        /// Optional list of profiles to typecheck against. `None` means
        /// "use the default behavior — single pass at the manifest's
        /// (or `default`) profile". `Some(list)` means "run the full
        /// pipeline once per profile and group diagnostics per profile".
        /// `--profiles=all` expands to every known profile.
        profiles: Option<Vec<crate::manifest::CompileProfile>>,
        /// Optional list of v1 compilation targets to check against
        /// (phase-10 multi-target verification). `None` means "consult
        /// the discovered manifest's `[build].targets`, falling back to
        /// a single pass under the active (`native`) target".
        /// `Some(list)` runs the full pipeline once per target,
        /// parameterizing the target-provided resource set each time,
        /// tags diagnostics with the producing target, and dedupes the
        /// target-agnostic ones. `--targets=all` expands to the closed
        /// v1 set. Mutually exclusive with `profiles`.
        targets: Option<Vec<String>>,
        /// `--concurrency-report` (Slice D, 2026-05-08): also emit the
        /// human-readable concurrency analysis to stdout after checks
        /// complete. Already runs `concurrencycheck()` via
        /// `Pipeline::run_all_checks`, so wiring is purely render-side.
        concurrency_report: bool,
        /// `--simd-report=verbose` (phase-7-codegen.md line 308, slice 5b):
        /// also emit the per-function SIMD lowering-tier report to stdout
        /// after checks complete. Reuses the `simd_check` findings already
        /// gathered by `run_all_checks`, so wiring is purely render-side.
        simd_report: bool,
        /// See [`Command::Run::lint_overrides`].
        lint_overrides: crate::lints::CliLintOverrides,
    },
    Build {
        file: String,
        output: OutputMode,
        /// `--concurrency-report` (Slice D, 2026-05-08): emit the
        /// human-readable concurrency analysis to stdout alongside the
        /// binary build. Pairs with the auto-par execution path landed
        /// in Slice A to make the compiler's reasoning visible alongside
        /// the speedup. See `docs/dogfooding.md § Parallax ("What the demo shows")` for the locked
        /// output shape.
        concurrency_report: bool,
        /// `--simd-report=verbose` (phase-7-codegen.md line 308, slice 5b):
        /// emit the per-function SIMD lowering-tier report to stdout
        /// alongside the binary build, so a developer can see which
        /// `Vector[T, N]` ops lowered native / wide / scalar on the target.
        simd_report: bool,
        /// `--offline`: read resolved dependencies only from the
        /// project-root `vendor/` directory (populated by
        /// `karac vendor`) and refuse any network access. Air-gap
        /// workflow per `design.md § Package System > Vendoring`.
        /// v1 surface — actual offline gating wires up alongside the
        /// dependency-resolution slice; v1 honors the flag at the
        /// arg-parsing layer and emits a "not yet wired" notice from
        /// the build command body so callers can scaffold their CI
        /// config against the canonical flag name today.
        offline: bool,
        /// `--enable-hot-swap`: emit PLT-style indirection for
        /// `extern`-public module symbols so the AOT artifact format
        /// is forward-compatible with the post-v1 continuous-PGO +
        /// shared-object reload story (`deferred.md § Continuous PGO
        /// with Shared-Object Hot-Swap`). Off by default in v1. The
        /// codegen consumption lands in slice 2 of phase-7 line 5;
        /// slice 1 plumbs the flag and gates incompatible profiles.
        enable_hot_swap: bool,
        /// `--no-proxy`: opt out of the registry proxy at
        /// `proxy.kara-lang.org` (or whatever `KARAC_REGISTRY_PROXY`
        /// names). Registry / git deps would then have to be fetched
        /// direct-from-source — a v1.1.x carve-out; today the flag
        /// is honored at the parse layer and surfaces a confirmation
        /// `note:` so CI scripts pinning to the flag can already
        /// scaffold against the final name.
        no_proxy: bool,
        /// `--target=<triple>`: override the active target triple for
        /// `[target.<triple>.dependencies]` / `[target.<triple>.profile]`
        /// overlay selection (tracker line 882). Single-file mode runs
        /// no manifest-driven target merge, so the flag is accepted for
        /// shape compatibility with project mode but does not affect
        /// codegen today.
        target: Option<String>,
        /// `--bindings=browser|component|none`: WASM output-shape
        /// selector (see [`BindingsMode`]). `None` here means "flag
        /// omitted" — `cmd_build` infers the mode from the WASM target
        /// (`wasm_browser` → browser, `wasm_wasi` → component). On a
        /// non-WASM target the flag is accepted-but-inert, consistent
        /// with `--offline` / single-file `--target=<triple>` above.
        bindings: Option<BindingsMode>,
        /// `--target-cpu=<name|help>`: CPU baseline override for codegen
        /// (phase-10; design.md § CPU Baseline Targeting). `None` here
        /// means "flag omitted" — `cmd_build` then consults the
        /// `KARAC_TARGET_CPU` env var, then the discovered manifest's
        /// `[release] target-cpu`, then the per-target default table in
        /// `codegen/driver.rs::default_cpu_and_features`. The literal
        /// value `help` prints LLVM's supported-CPU listing for the
        /// active target and exits (mirrors `rustc -C target-cpu=help`);
        /// any other name is validated against that same listing before
        /// codegen so a typo can't silently fall back to `generic`
        /// (LLVM's native behavior on an unknown CPU is warn-and-ignore).
        target_cpu: Option<String>,
        /// `--target-features=<+feat,-feat,…|help>`: feature-string
        /// override, the `--target-cpu` sibling (design.md § CPU
        /// Baseline Targeting > Feature-string override). Own precedence
        /// chain resolved independently of the CPU's: this flag, then
        /// `KARAC_TARGET_FEATURES`, then `[release] target-features`.
        /// The resolved list appends *after* the per-target default
        /// features (LLVM resolves duplicates last-wins, so a user
        /// `-feat` genuinely disables a table default). Every token
        /// must carry a `+`/`-` prefix and name a feature in LLVM's
        /// per-target registry — hard error otherwise; `help` prints
        /// the annotated listing and exits.
        target_features: Option<String>,
        /// `--features=wasm-threads`: shared-memory multithreading opt-in
        /// for `wasm_browser` builds (phase-10; design.md § WASM
        /// Concurrency Lowering). Emits a second, threaded module
        /// (`<stem>.threads.wasm` — Web Worker pool + SharedArrayBuffer +
        /// atomics on the `wasm32-wasip1-threads` substrate, auto-par
        /// re-enabled) alongside the sequential one; the JS glue picks at
        /// load time by SAB/COI feature-detection. Hard error off
        /// `wasm_browser` (wasi-threads and the component model don't
        /// compose) and with `--bindings=component`. CLI-only enable —
        /// the manifest's `[wasm]` table tunes (pool size, fallback
        /// posture, max memory) but never enables, keeping the COOP/COEP
        /// deployment contract visible at the flag.
        wasm_threads: bool,
        /// `--monomorphization-budget=warn:N,error:M` (v1.x, single-file
        /// only): per-generic instantiation ceiling enforced after
        /// typecheck. A disabled (all-`None`) budget — the default — skips
        /// the check. Thresholds are opt-in; default thresholds are
        /// deferred to v1.x pending codegen data (phase-7-codegen.md line
        /// 266). Reads the same instantiation table as `karac query
        /// monomorphization`.
        monomorphization_budget: crate::monomorphization::MonomorphizationBudget,
        /// `--release`: strip debug-only runtime checks from the emitted
        /// binary. Today this means contracts (`requires` / `ensures` /
        /// `old` / `invariant`) per design.md § Contracts ("checked at
        /// runtime in debug builds, stripped in release"); the future
        /// `?`-propagation trace strip lands behind the same flag. A bare
        /// `karac build` is the debug profile (contracts checked). Note
        /// that mid-end optimization is already `-O2` by default
        /// (`KARAC_OPT_LEVEL`), so `--release` is about removing runtime
        /// *checks*, not turning the optimizer on. Composes with the
        /// `KARAC_STRIP_CONTRACTS` env knob (OR): either strips.
        release: bool,
        /// `--crate-type=bin|staticlib|cdylib` — native artifact kind
        /// (`design.md § Exported C ABI`, additive-interop Slice 2).
        /// Default [`NativeCrateType::Bin`]. `staticlib`/`cdylib` route
        /// the `pub extern "C" fn` surface into a linkable library +
        /// emitted `.h`; rejected on wasm targets (which use `--bindings`).
        crate_type: NativeCrateType,
        /// `-o <path>` / `--out <path>` — explicit output path for the
        /// build artifact. For a library build (`--crate-type
        /// staticlib/cdylib`) this names the `.a`/`.so`/`.dylib`; when
        /// omitted the artifact defaults to `lib<stem>.<ext>` in CWD (a
        /// distinct name from the `<stem>` executable, so a library build
        /// never clobbers a stray binary — the producer-mode gotcha).
        out_path: Option<String>,
        /// See [`Command::Run::lint_overrides`].
        lint_overrides: crate::lints::CliLintOverrides,
    },
    /// Project-mode build: no file argument. Walks up from CWD to find
    /// `kara.toml`, loads the manifest, and (once CR-24 slices 3+ land) runs
    /// the multi-file pipeline. In slice 2 this is a stub that loads the
    /// manifest and reports. Missing manifest → E0227 NotInsideKaraProject.
    BuildProject {
        output: OutputMode,
        /// `--offline` — see `Build.offline` above. Same v1 contract.
        offline: bool,
        /// `--enable-hot-swap` — see `Build.enable_hot_swap` above.
        /// In project mode this also gates against the manifest's
        /// `[package].profile`: `embedded` and `kernel` lack the
        /// dynamic-symbol-resolution machinery hot-swap requires, so
        /// the combination hard-errors before codegen.
        enable_hot_swap: bool,
        /// `--no-proxy` — see `Build.no_proxy` above.
        no_proxy: bool,
        /// `--target=<triple>`: active target triple for the build.
        /// Drives `[target.<triple>.dependencies]` / `[target.<triple>.
        /// profile]` overlay selection (tracker line 882). Precedence:
        /// `--target=<triple>`, then `[build].target` from the
        /// manifest, then `build_cache::host_target_triple()`. A v1
        /// target *name* (`native` / `wasm_wasi` / `wasm_browser`)
        /// instead selects the compilation target, as in single-file
        /// mode — wasm names drive the `dist/wasm/<pkg>.*` artifact
        /// layout and pin the overlay triple to `wasm32-wasip1`.
        target: Option<String>,
        /// `--bindings=browser|component|none` — see `Build.bindings`
        /// above. Shapes the project-mode WASM artifact set
        /// (`dist/wasm/<pkg>.wasm` [+ `<pkg>.js` + `<pkg>.d.ts` under
        /// browser bindings]); accepted-but-inert on non-WASM targets.
        bindings: Option<BindingsMode>,
        /// `--target-cpu=<name|help>` — see `Build.target_cpu` above.
        /// Same precedence chain; the manifest tier reads the project's
        /// own `kara.toml` (already loaded for the build) instead of a
        /// file-relative walk-up.
        target_cpu: Option<String>,
        /// `--target-features=<list|help>` — see `Build.target_features`
        /// above. Same project-manifest tier note as `target_cpu`.
        target_features: Option<String>,
        /// `--features=wasm-threads` — see `Build.wasm_threads` above.
        /// Same scope rules; the threaded module lands at
        /// `dist/wasm/<pkg>.threads.wasm`.
        wasm_threads: bool,
        /// `--release` — see `Build.release` above. Same debug/release
        /// semantics (strips debug-only runtime checks — contracts today —
        /// not an optimizer toggle) and the same OR-composition with
        /// `KARAC_STRIP_CONTRACTS`. Threaded through `cmd_build_project` →
        /// `run_multi_file_codegen` → `compile_to_object_with_hot_swap`.
        release: bool,
        /// `--crate-type=bin|staticlib|cdylib` — see `Build.crate_type`.
        /// In project mode, overrides the manifest `[lib] crate-type`.
        /// `Bin` here means "flag omitted"; `cmd_build_project` falls back
        /// to the manifest's `[lib]` table to decide the artifact kind.
        crate_type: NativeCrateType,
        /// `-o <path>` — see `Build.out_path`. Names the library artifact
        /// for a project library build; omitted → `dist/lib<name>.<ext>`.
        out_path: Option<String>,
    },
    Query {
        kind: QueryKind,
        file: String,
        function: String,
    },
    Fmt {
        file: String,
    },
    /// Apply machine-applicable suggestions back into the source file.
    /// v1 covers `did you mean` corrections on undefined names / types
    /// emitted by the resolver. With `--dry-run`, prints the would-be
    /// rewrites without touching disk.
    Fix {
        file: String,
        dry_run: bool,
    },
    /// Scaffold a new Kāra project. Bare `karac init` scaffolds into the
    /// current directory; `karac init <name>` creates `./<name>/` first. See
    /// `docs/design.md § Package System § Project Scaffolding`.
    Init {
        /// When `Some(name)`, create `./<name>/` and scaffold there.
        directory: Option<String>,
        template: Template,
        force: bool,
    },
    /// Run the project's tests. Walks the project root, discovers
    /// `_test.kara` files, merges them into their production sibling
    /// modules, and invokes every `test_*` function via the interpreter.
    /// Output schema documented in `docs/design.md § Testing › Test
    /// runner output format`.
    Test {
        /// Optional substring filter — only tests whose fully-qualified ID
        /// (`<module_path>::<fn_name>`) contains this substring run.
        filter: Option<String>,
        /// Promote skipped tests to failures. Tests gated by
        /// `#[test(requires = [...])]` skip silently when their resources
        /// are unavailable; with `--all` the runner instead emits
        /// `test_fail` (with `reason: "unsatisfied_requires"`) and the
        /// process exits non-zero. Used in CI when every required service
        /// must be live.
        all: bool,
        /// `--interp`: force the tree-walk interpreter instead of the
        /// default LLJIT executor (LLJIT-productionization Slice 5). The
        /// interpreter is retained as a dev/debug backend (design.md §
        /// Tree-walk interpreter (dev / debug only)); this is the ergonomic
        /// equivalent of `KARAC_TEST_JIT=0`. No-op on a non-`llvm` build
        /// (the interpreter is the only executor there).
        interp: bool,
    },
    /// Launch the interactive REPL over the tree-walk interpreter. P0
    /// delivery item per `roadmap.md § Interactive Development`. See
    /// `src/repl.rs` for the cell-scope semantics. Flags mirror
    /// `repl::ReplOptions` and are surfaced through the `--auto-clone`
    /// CLI form (and, eventually, `%set auto-clone on` once the kernel
    /// magic ships).
    Repl {
        /// `--auto-clone`: opt-in cross-cell ownership ergonomics — the
        /// REPL auto-inserts `.clone()` at consume sites flagged by a
        /// cross-cell `UseAfterMove`. Each insertion emits a
        /// `perf[auto-clone-in-repl]` note (never silent).
        auto_clone: bool,
        /// `--interp`: force the tree-walk interpreter instead of the
        /// default LLJIT executor (LLJIT-productionization Slice 5). The
        /// ergonomic equivalent of `KARAC_REPL_JIT=0`; the interpreter is
        /// retained as a dev/debug backend. No-op on a non-`llvm` build.
        interp: bool,
    },
    /// Walk the project, parse every module, render one HTML page per
    /// documented item under `dist/doc/`. v1 MVP — no cross-references,
    /// no effect display, flat per-module directory layout.
    Doc,
    /// Remove the project's build artifact cache. Bare form deletes the
    /// project-local `dist/` directory (idempotent — a missing directory
    /// is not an error). `--global` instead targets the user-wide cache
    /// at `~/.kara/cache/` per `design.md § Package System > Build
    /// artifact cache`.
    Clean {
        global: bool,
    },
    /// Inspect the global build-artifact cache at `~/.kara/cache/build/`.
    /// Two sub-modes:
    /// - `karac cache info` — print the cache root and aggregate stats
    ///   (populated entry count, total artifact bytes). Useful for
    ///   eyeballing how much disk the cache currently holds.
    /// - `karac cache key --pkg NAME --version V [--edition E] [--profile P]
    ///   [--target-triple T] [--compiler-version C]` — derive and print
    ///   the cache-key digest for the given five-tuple. Lets CI verify
    ///   that the key derivation matches an external expectation
    ///   without having to populate the cache first.
    ///
    /// The cache itself is consumed by the build pipeline when per-dep
    /// codegen ships (v1.1.x carve-out); this subcommand surfaces the
    /// typed cache protocol today so tooling can integrate against it
    /// from day one. `karac clean --global` evicts the cache; this
    /// command never mutates anything.
    Cache {
        sub: CacheSub,
        output: OutputMode,
    },
    /// Build a binary package and install it into `~/.kara/bin/`. The
    /// `spec` accepts `path = ...`, `git = ...`, or a registry-proxy
    /// reference per the manifest dependency spec shape. v1 surface —
    /// the full resolver wiring lands in a follow-up alongside the
    /// dependency-resolution slice; this arm parses the invocation and
    /// emits a "not yet wired" diagnostic until then.
    Install {
        spec: String,
    },
    /// Copy all resolved dependencies into a project-root `vendor/`
    /// directory. Subsequent `karac build --offline` reads from
    /// `vendor/` and refuses network access.
    Vendor {
        /// `--no-proxy` — see `Build.no_proxy`. Registry-proxy fetch
        /// is a v1.1.x follow-up; today the flag is plumbed and the
        /// path-dep copy is unaffected.
        no_proxy: bool,
    },
    /// Re-run the resolver and rewrite `kara.lock`. Bare form refreshes
    /// every locked package; surgical form (`karac update <pkg>`) targets
    /// one package. v1.1 with path-deps only: bumping isn't meaningful
    /// (path-deps are manifest-pinned), so both forms re-derive the
    /// lockfile from the current manifest. Real version-bumping lands
    /// alongside the registry-proxy fetch surface (tracker line 845).
    Update {
        package: Option<String>,
        output: OutputMode,
        /// `--no-proxy` — see `Build.no_proxy`.
        no_proxy: bool,
    },
    /// Resolve the dependency graph and print it — a read-only debugging
    /// view of what `karac build` would resolve, *without* driving a build
    /// or rewriting `kara.lock` (unlike `karac update`). Runs the same
    /// resolver + fetch path as `build` (registry / git deps are fetched
    /// when configured), then renders each resolved package with its pinned
    /// version, source, and the parents that declared it. Registry-proxy
    /// follow-up (j) at `phase-5-diagnostics.md` line 896.
    Resolve {
        output: OutputMode,
        /// `--offline` — resolve against `./vendor/` only (see `Build`).
        offline: bool,
        /// `--no-proxy` — see `Build.no_proxy`.
        no_proxy: bool,
    },
    /// Emit the project's public API surface as JSONL on stdout. One record
    /// per exported item (`fn`, `struct`, `enum`, `trait`, `const`,
    /// `type_alias`, `distinct_type`, `effect_resource`, `extern_fn`,
    /// plus `impl_method` rows for `pub` methods inside `impl` blocks).
    /// Each record carries the item's signature shape (generics with
    /// bounds, parameters with modes and types, return type, declared
    /// effect row, refinement constraints) and source span. Public-only
    /// — inferred reported-tier effect rows of non-`pub` items are not
    /// stable enough to index. See `docs/deferred.md § Signature
    /// Catalog (karac catalog)` and `phase-5-diagnostics.md` line 643.
    Catalog {
        file: String,
    },
    /// Preemptive `shared struct` → `par struct` migration tool. Phase-7
    /// L215a foundation slice — covers the type-definition rewrite
    /// (keyword rename + `mut ` strip + `Mutex[T]` wrap), dry-run /
    /// `--apply` modes, and the workspace dirty-check guard. Consumer-
    /// site rewrites (`lock self.field { ... }` at every read/write of
    /// the migrated bindings across the workspace) are tracked as a
    /// follow-up L215b entry; the v1 surface produces a starting-point
    /// diff and leaves consumer migration as the documented hand-finish
    /// step (matches `design.md § Compiler-assisted migration from
    /// `shared struct` to `par struct`` — "manual at the review step").
    Migrate {
        /// The type name to migrate. Currently only `shared struct` →
        /// `par struct` is in scope (the `shared-to-par <Type>` form
        /// in the spec); the kind-discriminator argument is fixed by
        /// the subcommand shape rather than a separate flag.
        type_name: String,
        /// `--apply` writes the rewrite to disk. Default (dry-run)
        /// prints the diff to stdout.
        apply: bool,
        /// `--force` bypasses the workspace-uncommitted-changes guard
        /// that otherwise refuses to run when `git status --porcelain`
        /// reports any modifications outside the rewrite footprint.
        /// Honored only in apply mode (dry-run never writes, so the
        /// guard is moot).
        force: bool,
        /// Optional positional file argument. When provided, treats
        /// the named file as the migration scope (single-file mode);
        /// when omitted, walks up from CWD for `kara.toml` and uses
        /// the project's `src/` tree as the scope (L215b4 project mode).
        file: Option<String>,
        /// The L215c Atomic[T] heuristic, on by default in project-mode.
        /// When set, project-mode classifies each mut field as Atomic[T]
        /// (every observed write across the workspace is a bare `=`
        /// assignment AND T is in the lock-free Copy set: `i32`,
        /// `i64`, `u32`, `u64`, `usize`, `isize`, `bool`) or Mutex[T]
        /// (anything else). Atomic-classified fields' consumer sites are
        /// auto-rewritten to `.store(v, Ordering)` / `.load(Ordering)`
        /// (L215c-cons) rather than lock-wrapped. `--no-atomic` clears
        /// this, restoring the L215a–b4 default (all-Mutex with consumer
        /// wraps). Always false in single-file mode (no workspace
        /// visibility for the classifier).
        atomic: bool,
    },
    /// Concept-level explainer surface. `karac explain --concept=closures`
    /// renders a per-concept page covering the relevant analysis rules,
    /// diagnostic shapes, and inspection commands. The concept name is
    /// validated against the registered set at render time so a typo
    /// produces a focused diagnostic listing the supported set.
    ///
    /// Line 619 slice 3 adds `--class=NAME` for diagnostic-class
    /// lookup (`karac explain --class=TYPE_MISMATCH` returns the
    /// catalogue entry for a class) and `--format=json` for opt-in
    /// machine-consumable output. `--concept` and `--class` are
    /// mutually exclusive; exactly one must be supplied.
    Explain {
        target: ExplainTarget,
        format: ExplainFormat,
    },
    Help,
    Version,
}

/// Sub-mode for `karac cache`. Line 861 slice 2 — info + key
/// inspection. The five-tuple key fields are all optional except
/// `pkg` and `version`; missing optionals default to the active
/// compiler's view of the world (the compiler version from
/// `CARGO_PKG_VERSION`, the host target triple, edition `2026`,
/// profile `default`) so the common case is short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheSub {
    /// `karac cache info` — print the cache root and aggregate stats.
    Info,
    /// `karac cache key --pkg ... --version ...` — derive + print
    /// the cache-key digest for the supplied five-tuple. Each
    /// optional field falls back to a sensible default so a bare
    /// `--pkg foo --version 1.2.3` is enough.
    Key {
        pkg: String,
        version: String,
        edition: Option<String>,
        profile: Option<String>,
        target_triple: Option<String>,
        compiler_version: Option<String>,
    },
}

/// What `karac explain` should look up. Line 619 slice 3 widens the
/// command from concept-only to concept-or-class so the diagnostic
/// catalogue surface (`DiagnosticClass` enum, slice 1) is
/// reachable from the CLI. Future slices extend this with
/// `--code=E_XXX` for direct E_*-code lookups when the per-code
/// catalogue stabilises.
#[derive(Debug, Clone)]
pub enum ExplainTarget {
    /// `--concept=NAME` — concept-page surface (closures, …).
    Concept(String),
    /// `--class=NAME` — diagnostic-class catalogue lookup. NAME is
    /// the UPPER_SNAKE wire form (`TYPE_MISMATCH`, `INVALID_CAST`,
    /// etc.). Slice 1 minted the enum; slice 3 surfaces it via the
    /// CLI.
    Class(String),
}

/// Output format selector for `karac explain`. Defaults to `Text`
/// (human prose, the existing surface); `--format=json` opts into
/// the machine-consumable shape that line 619's deferred entry asks
/// for. The JSON envelope is documented per command in
/// `src/cli/explain.rs::render_json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExplainFormat {
    Text,
    Json,
}

#[derive(Debug)]
pub enum QueryKind {
    Effects,
    Ownership,
    Concurrency,
    /// `karac query affected-by <target>` — call-graph reach query.
    /// Surfaces the call graph (already computed for effect
    /// inference and shared with codegen) as a public JSONL view:
    /// given a function, file, or file:line range, return the
    /// transitive callers, callees, and reaching test functions.
    /// Structural prerequisite for the `karac tdd` `--related` /
    /// `--since` test-selection flags and `karac test --coverage`'s
    /// `coverage_delta` event. See `docs/deferred.md § karac query
    /// affected-by`.
    AffectedBy {
        target: crate::call_graph::TargetSpec,
        tests_only: bool,
        direction: AffectedByDirection,
    },
    /// Whole-file cost-surface aggregator. Unlike the per-function query
    /// kinds above, this one ignores the `function` slot — the static
    /// counts are reported per-function inside the JSON envelope.
    CostSummary,
    /// Walk the program and emit one JSON record per multi-segment
    /// attribute (`#[diagnostic::*]`, `#[karafmt::*]`, …). Tool-facing
    /// read surface for the tool-namespaced-attribute work (v60 item
    /// 37). Also a whole-file kind — the `function` slot is unused.
    /// `tool_prefix` filters the output by first-segment match;
    /// `None` emits every multi-segment attribute.
    Attributes {
        tool_prefix: Option<String>,
    },
    /// Phase-8 stdlib-floor § Compiler queries channel sub-item 3.
    /// Run the full pipeline and collate every `CompilerQuery` from
    /// every phase result into a single JSON report. Whole-file kind
    /// — the `function` slot is unused. v1 ships an empty array when
    /// no phase populates queries yet; the surface lands so external
    /// tooling can integrate against `karac query queries` without
    /// waiting for catalogue entries.
    Queries,
    /// Phase-7-codegen.md line 97 + `design.md § Compiler Query API
    /// — karac query monomorphization`. Walks the typechecker's
    /// per-call-site type-arg table (`call_type_subs`) and emits one
    /// JSON record per generic function with its distinct
    /// `(T1..Tk)` tuples. Whole-file kind — the `function` slot is
    /// unused.
    Monomorphization,
}

/// Direction filter for `karac query affected-by`. Default `All`
/// emits both `callers` and `callees`; `Callers` skips the callees
/// array (still always emits `tests`, which derives from callers
/// independently); `Callees` skips both `callers` and `tests`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AffectedByDirection {
    Callers,
    Callees,
    All,
}

// ── Command Execution ───────────────────────────────────────────

pub fn execute(cmd: Command) {
    match cmd {
        Command::Help => print_help(),
        Command::Version => println!("karac 0.1.0"),
        Command::Run {
            file,
            output,
            sequential,
            manifest_override,
            no_manifest,
            lint_overrides,
            timeout,
            interp,
        } => cmd_run(
            &file,
            output,
            sequential,
            manifest_override.as_deref(),
            no_manifest,
            lint_overrides,
            timeout,
            interp,
        ),
        Command::RunExample {
            name,
            output,
            sequential,
            lint_overrides,
        } => cmd_run_example(&name, output, sequential, lint_overrides),
        Command::Check {
            file,
            output,
            profiles,
            targets,
            concurrency_report,
            simd_report,
            lint_overrides,
        } => cmd_check(
            &file,
            output,
            profiles,
            targets,
            concurrency_report,
            simd_report,
            lint_overrides,
        ),
        Command::Build {
            file,
            output,
            concurrency_report,
            simd_report,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
            bindings,
            target_cpu,
            target_features,
            wasm_threads,
            monomorphization_budget,
            release,
            crate_type,
            out_path,
            lint_overrides,
        } => cmd_build(
            &file,
            output,
            concurrency_report,
            simd_report,
            offline,
            enable_hot_swap,
            no_proxy,
            target.as_deref(),
            bindings,
            target_cpu.as_deref(),
            target_features.as_deref(),
            wasm_threads,
            monomorphization_budget,
            release,
            crate_type,
            out_path.as_deref(),
            lint_overrides,
        ),
        Command::BuildProject {
            output,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
            bindings,
            target_cpu,
            target_features,
            wasm_threads,
            release,
            crate_type,
            out_path,
        } => cmd_build_project(
            output,
            offline,
            enable_hot_swap,
            no_proxy,
            target.as_deref(),
            bindings,
            target_cpu.as_deref(),
            target_features.as_deref(),
            wasm_threads,
            release,
            crate_type,
            out_path.as_deref(),
        ),
        Command::Query {
            kind,
            file,
            function,
        } => cmd_query(kind, &file, &function),
        Command::Fmt { file } => cmd_fmt(&file),
        Command::Fix { file, dry_run } => cmd_fix(&file, dry_run),
        Command::Init {
            directory,
            template,
            force,
        } => cmd_init(directory, template, force),
        Command::Test {
            filter,
            all,
            interp,
        } => cmd_test(filter, all, interp),
        Command::Repl { auto_clone, interp } => {
            crate::repl::run_with_options(crate::repl::ReplOptions { auto_clone, interp })
        }
        Command::Doc => cmd_doc(),
        Command::Clean { global } => cmd_clean(global),
        Command::Cache { sub, output } => cmd_cache(sub, output),
        Command::Install { spec } => cmd_install(&spec),
        Command::Vendor { no_proxy } => cmd_vendor(no_proxy),
        Command::Update {
            package,
            output,
            no_proxy,
        } => cmd_update(package.as_deref(), output, no_proxy),
        Command::Resolve {
            output,
            offline,
            no_proxy,
        } => cmd_resolve(output, offline, no_proxy),
        Command::Explain { target, format } => explain::render(&target, format),
        Command::Catalog { file } => cmd_catalog(&file),
        Command::Migrate {
            type_name,
            apply,
            force,
            file,
            atomic,
        } => cmd_migrate(&type_name, apply, force, file.as_deref(), atomic),
    }
}

fn cmd_catalog(filename: &str) {
    let source = read_source(filename);
    let pipeline = Pipeline::new(filename, &source);
    // Catalog is a pure AST walk over signatures — name resolution
    // failures (unknown types in a half-written file, undeclared
    // effect resources, etc.) don't affect the per-item shape we
    // surface. Gate on parse only so external tooling can index a
    // file even when resolve / typecheck would later flag unrelated
    // issues. Parse failures still hard-fail because a half-parsed
    // item has no faithful signature to emit.
    if pipeline.has_parse_errors() {
        print_text_diagnostics(&pipeline);
        process::exit(1);
    }
    let out = crate::catalog::render(&pipeline.parsed.program, filename);
    if !out.is_empty() {
        // `render` already terminates the last record with `\n`; print as-is.
        print!("{out}");
    }
}

// ── Read Source ──────────────────────────────────────────────────

fn read_source(filename: &str) -> String {
    match fs::read_to_string(filename) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read '{filename}': {e}");
            process::exit(1);
        }
    }
}

// ── Pipeline Phases ─────────────────────────────────────────────

struct Pipeline {
    filename: String,
    parsed: ParseResult,
    resolved: Option<ResolveResult>,
    typed: Option<TypeCheckResult>,
    effects: Option<EffectCheckResult>,
    ownership: Option<OwnershipCheckResult>,
    concurrency: Option<ConcurrencyAnalysis>,
    provider_escape: Option<Vec<crate::provider_escape::EscapeError>>,
    /// Phase 6 line 31 slice 1: RAII-across-yield rejections for the
    /// network-event-loop state-machine transform. One error per
    /// (binding × function) pair where a non-cancel-safe binding lives
    /// across at least one yield point in a network-boundary function's
    /// body. Populated by [`Pipeline::raii_check`] after `effectcheck`
    /// (depends on `state_struct_layouts` + `yield_points`); merged into
    /// the final error count + diagnostic output alongside the other
    /// post-typecheck checkers.
    raii_errors: Option<Vec<crate::raii_check::RaiiAcrossYieldError>>,
    /// Phase-7-codegen.md line 308 slice 5a: `#[require_simd]` violations —
    /// one per `Vector[T, N]` op that would scalarize on the target inside a
    /// `#[require_simd]` function. Populated by [`Pipeline::simd_check`] after
    /// `typecheck` (depends only on `expr_types`); merged into the final error
    /// count + diagnostic output alongside the other post-typecheck checkers.
    /// A hard error: a function asking for the no-scalarization guarantee must
    /// not silently fall back. The interpreter path (`karac run`) does not
    /// enforce it — the tree-walker never vectorizes, so the guarantee is
    /// vacuous there; it is a codegen/`check` surface.
    simd_errors: Option<Vec<crate::simd_report::SimdFinding>>,
    /// Comptime fold diagnostics (`E_COMPTIME_PANIC` /
    /// `E_COMPTIME_NON_FOLDABLE_RESULT` / `E_COMPTIME_ITER_LIMIT_EXCEEDED`).
    /// Populated by [`Pipeline::lower`], which runs the comptime fold pass
    /// (`crate::comptime`, substrate 1) right after operator lowering so the
    /// AST every downstream phase consumes already has each `comptime { ... }`
    /// block replaced by its folded constant. Merged into the final error
    /// count + diagnostic output alongside the other post-typecheck checkers.
    comptime_errors: Option<Vec<crate::comptime::ComptimeError>>,
    profile: crate::manifest::CompileProfile,
    /// Per-profile `[profile]`-table knob carrier from the manifest. Carries
    /// the active profile plus any typed knobs; threaded into the effect
    /// checker at [`Pipeline::effectcheck`]. Its `.profile` is kept aligned
    /// with `profile` (which per-target overrides may rewrite) at the point
    /// of use. Defaulted in [`Pipeline::new`]; populated from
    /// `Manifest::profile_config` by the per-subcommand entry points.
    profile_config: crate::manifest::ProfileConfig,
    /// Build-wide lint level overrides from CLI flags
    /// (`-A NAME` / `-W NAME` / `-D NAME` / `-F NAME` / `-D warnings`).
    /// Slice 4b polish. Defaulted empty in [`Pipeline::new`]; the
    /// per-subcommand entry points set this via
    /// [`Pipeline::with_lint_overrides`] from the parsed
    /// [`crate::cli::args`] flags. Threaded into [`Pipeline::typecheck`]
    /// via [`crate::typecheck_with_lint_overrides`].
    lint_overrides: crate::lints::CliLintOverrides,
}

impl Pipeline {
    fn new(filename: &str, source: &str) -> Self {
        let parsed = crate::parse(source);
        Pipeline {
            filename: filename.to_string(),
            parsed,
            resolved: None,
            typed: None,
            effects: None,
            ownership: None,
            concurrency: None,
            provider_escape: None,
            raii_errors: None,
            simd_errors: None,
            comptime_errors: None,
            profile: crate::manifest::CompileProfile::Default,
            profile_config: crate::manifest::ProfileConfig::default(),
            lint_overrides: crate::lints::CliLintOverrides::default(),
        }
    }

    fn with_lint_overrides(mut self, overrides: crate::lints::CliLintOverrides) -> Self {
        self.lint_overrides = overrides;
        self
    }

    fn has_parse_errors(&self) -> bool {
        !self.parsed.errors.is_empty()
    }

    fn resolve(&mut self) {
        if self.has_parse_errors() {
            return;
        }
        // Phase-10 (`std.web`): single-file mode has no ProgramTree for
        // gated stdlib imports to resolve against — expand them into the
        // real baked items in place (replacing the import binding), so
        // the resolver, effect checker, interpreter, and codegen all see
        // ordinary declarations.
        crate::prelude::expand_gated_stdlib_imports(&mut self.parsed.program);
        // Phase-10 `#[target(...)]`: items gated to a non-current target
        // are absent from this compilation — strip them before any pass
        // sees the program (their bodies may reference target-specific
        // names). Tombstones feed the resolver's "not available on
        // target X" diagnostic at reference sites.
        let target_tombstones = crate::target::filter_inactive_items(
            &mut self.parsed.program,
            crate::target::active_target(),
        );
        // `desugar_program` also runs the pre-resolve `#[proto_schema]`
        // expansion (protobuf slice 3); its diagnostics (malformed `.proto`,
        // unsupported field types) join the comptime-error channel so they
        // render and gate exactly like the post-resolve fold pass's.
        let schema_diags = crate::desugar_program(&mut self.parsed.program);
        if !schema_diags.is_empty() {
            self.comptime_errors
                .get_or_insert_with(Vec::new)
                .extend(schema_diags);
        }
        // Single-file mode infers the test-file flag from the filename
        // suffix — multi-module flows route through `resolve_modules`
        // and read it off `Module.is_test_file`. Phase-5-diagnostics
        // line 633 (signature-from-call-site stub) needs the flag set
        // so it fires when `karac check foo_test.kara` surfaces an
        // unresolved-call site.
        let is_test_file = self.filename.ends_with("_test.kara");
        self.resolved = Some(
            crate::resolver::Resolver::new(&self.parsed.program)
                .with_test_file(is_test_file)
                .with_target_tombstones(target_tombstones)
                .resolve(),
        );
    }

    fn has_resolve_errors(&self) -> bool {
        self.resolved.as_ref().is_some_and(|r| !r.errors.is_empty())
    }

    /// Hard typecheck errors only — warnings are stored separately in
    /// `TypeCheckResult.warnings` via `type_lint_warning` and are
    /// intentionally non-fatal at the CLI layer. Sibling to
    /// `has_parse_errors` / `has_resolve_errors`; consumed by
    /// `has_fatal_errors` so `cmd_build` stops before codegen when the
    /// typechecker rejected any expression. Without this, a typecheck
    /// error like "no associated function 'from_utf8' on type 'String'"
    /// gets collected silently and the user only sees the downstream
    /// codegen explosion ("no handler for method 'unwrap' on variable
    /// 'parsed'"), which sends them chasing a phantom codegen bug.
    fn has_type_errors(&self) -> bool {
        self.typed.as_ref().is_some_and(|t| !t.errors.is_empty())
    }

    fn typecheck(&mut self) {
        if self.resolved.is_none() || self.has_resolve_errors() {
            return;
        }
        // Thread the manifest's `[profile]`-table knob carrier into the
        // typechecker, realigning its active profile with any per-target
        // override (mirrors the effect-checker leg in `effectcheck`). The
        // `panic_on_alloc_failure` knob gates the fallible-alloc rejection
        // passes (phase-8-stdlib-floor items 4–5).
        let mut profile_config = self.profile_config.clone();
        profile_config.profile = self.profile;
        self.typed = Some(crate::typecheck_with_lint_overrides_and_profile(
            &self.parsed.program,
            self.resolved.as_ref().unwrap(),
            self.lint_overrides.clone(),
            profile_config,
        ));
    }

    /// Apply the operator-lowering pass. Runs after typecheck (uses inferred
    /// operand types) and before any downstream phase that consumes the AST
    /// (effectcheck / ownership / interpreter / codegen).
    fn lower(&mut self) {
        if self.typed.is_none() {
            return;
        }
        // `#[derive(X)]` expansion (B-2026-07-08-15 Layer 1): a derive SPLICES
        // new items (methods/impls) into the program. Those generated bodies
        // must be name-resolved and typechecked so codegen's span-keyed side-
        // tables (element types of un-annotated locals, `let b = self.make()`
        // where `make` returns a `Vec`, etc.) are populated — otherwise codegen
        // fails dispatch ("no handler for method 'push' on variable 'v'"). So
        // when derives are present, fold+expand FIRST, then RE-RESOLVE and
        // RE-TYPECHECK the mutated program, then operator-lower. Pure
        // `comptime { … }`-block folding adds no items and keeps the original
        // lower→fold order (no re-typecheck cost). This runs in `lower()` so
        // every path that lowers (check / build / run) gets it uniformly.
        if crate::comptime::has_derives_to_expand(&self.parsed.program) {
            let typed = self.typed.take().unwrap();
            let fold_errors = crate::comptime::evaluate(&mut self.parsed.program, &typed);
            self.comptime_errors
                .get_or_insert_with(Vec::new)
                .extend(fold_errors);
            // Re-run name resolution + typecheck over the spliced program so
            // generated items resolve and their side-tables populate.
            let resolved = crate::resolve(&self.parsed.program);
            let retyped = crate::typecheck(&self.parsed.program, &resolved);
            self.resolved = Some(resolved);
            crate::lower(&mut self.parsed.program, &retyped);
            self.typed = Some(retyped);
        } else {
            let typed = self.typed.as_ref().unwrap();
            crate::lower(&mut self.parsed.program, typed);
            let fold_errors = crate::comptime::evaluate(&mut self.parsed.program, typed);
            self.comptime_errors
                .get_or_insert_with(Vec::new)
                .extend(fold_errors);
        }
    }

    fn effectcheck(&mut self) {
        if self.has_parse_errors() {
            return;
        }
        // Thread the typechecker's `method_callee_types` resolution table so
        // method-call sites can reach the same `with E` / Fn-slot / polymorphic
        // arg propagation paths the free-call form already gets. Falls back to
        // an empty map when typecheck didn't run (e.g. resolve errors aborted
        // earlier in the pipeline). `call_type_subs` is threaded alongside so
        // E0404 diagnostics on compound polymorphic calls can render a fully
        // monomorphized callee signature (Round 10.3 step 7).
        let method_types = self
            .typed
            .as_ref()
            .map(|t| t.method_callee_types.clone())
            .unwrap_or_default();
        let call_type_subs = self
            .typed
            .as_ref()
            .map(|t| t.call_type_subs.clone())
            .unwrap_or_default();
        // Thread the manifest's `[profile]`-table knob carrier into the effect
        // checker. Realign its active profile with `self.profile` so any
        // per-target profile override (which rewrites `self.profile`) is
        // reflected for the moot-flag scaffold and downstream knob consumers.
        let mut profile_config = self.profile_config.clone();
        profile_config.profile = self.profile;
        self.effects = Some(crate::effectcheck_with_typecheck_data(
            &self.parsed.program,
            crate::effectchecker::PublicEffectsPolicy::default(),
            profile_config,
            method_types,
            call_type_subs,
        ));
        // Populate `Program.callee_effectful` from the effect-check result so
        // codegen can narrow the par-branch cooperative cancel-check to calls
        // whose callee actually carries reads/writes/sends/receives. Mirrors
        // the wiring of `Program.question_conversions` from the lowering pass.
        if let Some(ref effects) = self.effects {
            self.parsed.program.callee_effectful = build_callee_effectful_table(effects);
            self.parsed.program.callee_network_yield_effect =
                build_callee_network_yield_effect_table(effects);
            // Slice 8ab: forward the effect-checker's
            // `call_effect_subs` into the AST-level table so codegen
            // can consume per-call effect-variable resolutions
            // (slice 8y consumer).
            self.parsed.program.call_effect_subs = build_call_effect_subs_table(effects);
            // Slice 8y: mark callees whose declared effects are
            // purely `Polymorphic` (no static fixed portion). Codegen
            // uses this set together with `call_effect_subs` to gate
            // the per-mono caller-side state-machine intercept per
            // call site.
            self.parsed.program.callee_purely_polymorphic_effects =
                build_callee_purely_polymorphic_effects_set(effects);
        }
        // Now that `callee_network_yield_effect` is populated, walk each
        // network-boundary function body and enumerate its yield points.
        // Resolves `MethodCall` sites through the typechecker's
        // `method_callee_types`; absent that data (e.g. when typecheck
        // didn't run), method-call yield points are silently dropped, which
        // is fine for the not-typechecked path that produces no codegen
        // anyway. The walker reads the program tree by shared reference, so
        // we route the assignment through a local to avoid borrowing
        // `self.parsed.program` mutably and immutably at the same time.
        let method_callee_types_for_yields = self
            .typed
            .as_ref()
            .map(|t| t.method_callee_types.clone())
            .unwrap_or_default();
        let yield_points = build_yield_points_table(
            &self.parsed.program,
            &self.parsed.program.callee_network_yield_effect,
            &method_callee_types_for_yields,
        );
        self.parsed.program.yield_points = yield_points;
        // Slice 4: synthesize the per-function state-struct layout (union
        // of captured-locals across yield points + their typechecker-known
        // surface type names where recorded). Routed through a local copy
        // of `pattern_binding_types` for the same borrow-discipline reason
        // as the yield-points walker above. The typed phase may be absent
        // (e.g. parse-only pipelines); in that case `pattern_binding_types`
        // is empty and every field's `type_name` resolves to `None`, which
        // matches codegen's primitive-sizing fallback path.
        let pattern_binding_types_for_layouts = self
            .typed
            .as_ref()
            .map(|t| t.pattern_binding_types.clone())
            .unwrap_or_default();
        let state_struct_layouts = build_state_struct_layouts(
            &self.parsed.program,
            &self.parsed.program.callee_network_yield_effect,
            &method_callee_types_for_yields,
            &pattern_binding_types_for_layouts,
        );
        self.parsed.program.state_struct_layouts = state_struct_layouts;
    }

    fn ownershipcheck(&mut self) {
        if self.typed.is_none() {
            return;
        }
        // Thread the manifest's `[profile]`-table knob carrier (realigned to the
        // active profile) so `panic_on_alloc_failure = false` turns auto-RC
        // fallback into a hard error (phase-8-stdlib-floor item 6).
        let mut profile_config = self.profile_config.clone();
        profile_config.profile = self.profile;
        self.ownership = Some(crate::ownershipcheck_with_profile_config(
            &self.parsed.program,
            self.typed.as_ref().unwrap(),
            profile_config,
        ));
    }

    fn concurrencycheck(&mut self) {
        if self.effects.is_none() {
            return;
        }
        self.concurrency = Some(crate::concurrency_analyze_typed(
            &self.parsed.program,
            self.effects.as_ref().unwrap(),
            self.typed.as_ref(),
        ));
    }

    fn provider_escape_check(&mut self) {
        if self.has_parse_errors() {
            return;
        }
        self.provider_escape = Some(crate::provider_escape_check(
            &self.parsed.program,
            self.typed.as_ref(),
        ));
    }

    /// Phase 6 line 31 slice 1: run the RAII-across-yield check. Depends
    /// on `effectcheck` having populated `Program.state_struct_layouts` +
    /// `Program.yield_points` (slices 4 + 2 under line 26) and on
    /// `typecheck` having populated `struct_info` / `enum_info` for
    /// classifying surface type names as shared. With parse errors the
    /// check is a no-op (the layouts are empty and the typecheck index
    /// is missing); with typecheck errors but no parse errors, the
    /// check still runs against whatever made it into the layouts.
    fn raii_check(&mut self) {
        if self.has_parse_errors() {
            return;
        }
        self.raii_errors = Some(crate::raii_across_yield_check(
            &self.parsed.program,
            self.typed.as_ref(),
        ));
    }

    /// `#[require_simd]` guarantee (phase-7-codegen.md line 308 slice 5a).
    /// Pure post-typecheck analysis over `expr_types` — no LLVM backend
    /// needed, so it runs on the `check` path too (not just `build`),
    /// surfacing scalarization-guarantee violations at fast-feedback time.
    /// A no-op (empty list) when typecheck didn't run.
    fn simd_check(&mut self) {
        let findings =
            crate::simd_report::analyze_program(&self.parsed.program, self.typed.as_ref());
        self.simd_errors = Some(crate::simd_report::require_simd_errors(&findings));
    }

    /// Run all analysis phases (no execution).
    fn run_all_checks(&mut self) {
        self.resolve();
        self.typecheck();
        self.lower();
        self.effectcheck();
        self.ownershipcheck();
        self.concurrencycheck();
        self.provider_escape_check();
        self.raii_check();
        self.simd_check();
    }

    /// Collect all errors across phases. Typecheck errors are included —
    /// the typechecker is a hard gate, not a hint phase; a build that
    /// proceeds past typecheck errors produces misleading downstream
    /// diagnostics (e.g., the codegen "no handler for method 'unwrap'"
    /// surfaced 2026-05-22 from a typecheck-but-silent
    /// `String.from_utf8(buf)` call). Effect, ownership, and concurrency
    /// errors remain non-fatal here so the analysis surface continues to
    /// run for diagnostics-only consumers; consider extending this
    /// predicate further if the same diagnostic-swallowing pattern
    /// appears for any of those phases.
    fn has_fatal_errors(&self) -> bool {
        self.has_parse_errors()
            || self.has_resolve_errors()
            || self.has_type_errors()
            || self.has_fatal_comptime_errors()
            || self.has_fatal_ownership_errors()
    }

    /// Comptime fold failures are fatal: a `comptime { ... }` block that
    /// panicked, exceeded its resource ceiling, or produced a non-foldable
    /// value has no constant to splice, so the interpreter / codegen would
    /// otherwise consume an un-evaluated node (or a stale tree) and produce
    /// misleading downstream diagnostics. Stop before execution.
    fn has_fatal_comptime_errors(&self) -> bool {
        self.comptime_errors.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Most ownership errors are advisory at the CLI layer (see the note on
    /// `has_fatal_errors`), but the exclusive-borrow rule (B-2026-06-17-6) is a
    /// soundness gate, not a lint: an aliased `mut ref` / `mut Slice` argument
    /// (`f(mut v, mut v)`, `f(mut v, v)`) miscompiles — codegen passes the
    /// borrow's value by copy per argument and assumes the two don't alias — so
    /// it must stop before codegen. Only this one kind is promoted to fatal; the
    /// rest of the ownership surface stays diagnostic-only.
    fn has_fatal_ownership_errors(&self) -> bool {
        self.ownership.as_ref().is_some_and(|o| {
            o.errors
                .iter()
                .any(|e| e.kind == crate::ownership::OwnershipErrorKind::ExclusiveBorrowAliasedArgs)
        })
    }

    fn total_errors(&self) -> usize {
        let mut n = self.parsed.errors.len();
        if let Some(ref r) = self.resolved {
            n += r.errors.len();
        }
        if let Some(ref t) = self.typed {
            n += t.errors.len();
        }
        if let Some(ref e) = self.effects {
            n += e
                .errors
                .iter()
                .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
                .count();
        }
        if let Some(ref o) = self.ownership {
            n += o.errors.len();
        }
        if let Some(ref esc) = self.provider_escape {
            n += esc.len();
        }
        if let Some(ref r) = self.raii_errors {
            n += r.len();
        }
        if let Some(ref s) = self.simd_errors {
            n += s.len();
        }
        if let Some(ref c) = self.comptime_errors {
            n += c.len();
        }
        n
    }
}

// ── Text Output ─────────────────────────────────────────────────

fn print_text_diagnostics(pipeline: &Pipeline) {
    for block in render_text_diagnostics(pipeline) {
        eprintln!("{block}");
    }
}

/// Render the text-mode diagnostic stream as one string block per
/// diagnostic (multi-line for diagnostics that carry notes/help).
/// Factored out of `print_text_diagnostics` for the multi-target check
/// driver, which compares rendered blocks across per-target pipeline
/// runs to deduplicate target-agnostic findings (`cmd_check_targets`).
fn render_text_diagnostics(pipeline: &Pipeline) -> Vec<String> {
    let filename = &pipeline.filename;
    let mut out: Vec<String> = Vec::new();
    for err in &pipeline.parsed.errors {
        out.push(format!(
            "error[parse]: {}:{}:{}: {}",
            filename, err.span.line, err.span.column, err.message
        ));
    }
    if let Some(ref r) = pipeline.resolved {
        for err in &r.errors {
            out.push(format!(
                "error[resolve]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            ));
        }
    }
    if let Some(ref t) = pipeline.typed {
        for err in &t.errors {
            out.push(format!(
                "error[typecheck]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            ));
        }
    }
    if let Some(ref e) = pipeline.effects {
        for err in &e.errors {
            if err.kind == EffectErrorKind::FfiLintHint {
                out.push(format!(
                    "note[effect]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                ));
            } else {
                out.push(format!(
                    "error[effect]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                ));
            }
        }
    }
    if let Some(ref o) = pipeline.ownership {
        for err in &o.errors {
            out.push(format!(
                "error[ownership]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            ));
        }
        // RC-fallback (and other ownership) notes must reach the terminal too.
        // The ownership pass records every RC insertion as a `RcFallbackNote`
        // in `o.notes` (design.md § Part 4 *Note policy*: the note "fires by
        // default" so RC overhead — a silent heap-box + refcount — is visible
        // at the default build surface). The JSON/LSP path renders these
        // (`collect_diagnostics`); without this loop the human text renderer
        // iterated only `o.errors`, leaving `karac build` silent about RC
        // fallback. `RcFallbackNote` uses the design's Tier-1 `perf[rc-fallback]`
        // label; other note kinds (e.g. the unused-`mut`-capture note) render as
        // `note[ownership]`. Suppression (`#[allow(rc_fallback)]`) is already
        // applied upstream in `emit_rc_fallback_notes`, so whatever survives
        // into `o.notes` is meant to be shown.
        for note in &o.notes {
            let label = match note.kind {
                crate::ownership::OwnershipErrorKind::RcFallbackNote => "perf[rc-fallback]",
                _ => "note[ownership]",
            };
            let mut block = format!(
                "{}: {}:{}:{}: {}",
                label, filename, note.span.line, note.span.column, note.message
            );
            if let Some(ref s) = note.suggestion {
                write!(block, "\n  help: {s}").unwrap();
            }
            out.push(block);
        }
    }
    if let Some(ref esc) = pipeline.provider_escape {
        for err in esc {
            out.push(format!(
                "error[provider_escape]: {}:{}:{}: {}",
                filename,
                err.closure_span.line,
                err.closure_span.column,
                err.message()
            ));
        }
    }
    if let Some(ref raii) = pipeline.raii_errors {
        for err in raii {
            let mut block = format!(
                "error[E_RAII_ACROSS_YIELD]: {}:{}:{}: {}",
                filename,
                err.yield_span.line,
                err.yield_span.column,
                err.message(),
            );
            if let Some(ref bs) = err.binding_span {
                write!(
                    block,
                    "\n  note: binding declared here at {}:{}:{}",
                    filename, bs.line, bs.column,
                )
                .unwrap();
            }
            if let Some(ref sv) = err.state_violation {
                write!(
                    block,
                    "\n  note: soiled by `.{}()` here at {}:{}:{}",
                    sv.soiling_method, filename, sv.soil_span.line, sv.soil_span.column,
                )
                .unwrap();
            }
            write!(block, "\n  help: {}", err.help()).unwrap();
            out.push(block);
        }
    }
    if let Some(ref simd) = pipeline.simd_errors {
        for err in simd {
            out.push(format!(
                "error[E_REQUIRE_SIMD]: {}:{}:{} (in `{}`): {}\n  help: {}",
                filename,
                err.span.line,
                err.span.column,
                err.func_name,
                err.message(),
                err.help(),
            ));
        }
    }
    if let Some(ref comptime) = pipeline.comptime_errors {
        for err in comptime {
            // The message already carries its `error[E_COMPTIME_*]:` prefix.
            out.push(format!(
                "error[comptime]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            ));
        }
    }
    out
}

// ── JSON Output ─────────────────────────────────────────────────

fn span_to_json(span: &Span, filename: &str) -> String {
    format!(
        "\"file\":{},\"line\":{},\"column\":{}",
        json_string(filename),
        span.line,
        span.column
    )
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_string_list(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| json_string(s)).collect();
    format!("[{}]", parts.join(","))
}

/// Build the per-callee "is effectful" side-table from an `EffectCheckResult`.
///
/// A callee is "effectful" iff its inferred or declared effect set contains
/// any `reads` / `writes` / `sends` / `receives` verb (the four verbs that
/// drive cooperative-cancellation observability). `allocates`, `panics`,
/// `blocks`, `suspends`, `UserDefined` are excluded — they don't motivate
/// the per-call cancel check.
fn build_callee_effectful_table(
    effects: &EffectCheckResult,
) -> std::collections::HashMap<String, bool> {
    fn set_is_effectful(set: &crate::effectchecker::EffectSet) -> bool {
        set.effects.iter().any(|t| {
            matches!(
                t.effect.verb,
                EffectVerbKind::Reads
                    | EffectVerbKind::Writes
                    | EffectVerbKind::Sends
                    | EffectVerbKind::Receives
            )
        })
    }
    let mut table = std::collections::HashMap::new();
    for (name, set) in &effects.inferred_effects {
        table.insert(name.clone(), set_is_effectful(set));
    }
    for (name, decl) in &effects.declared_effects {
        // Polymorphic / PolymorphicWithFixed callees may carry effects per
        // monomorphization — treat them as effectful (conservative).
        let effectful = match decl {
            DeclaredEffects::Explicit(set) => set_is_effectful(set),
            // The polymorphic portion may pick up any effect at a
            // monomorphization site, so treat as effectful even if the fixed
            // set is empty.
            DeclaredEffects::PolymorphicWithFixed(_) | DeclaredEffects::Polymorphic => true,
            DeclaredEffects::None => false,
        };
        table
            .entry(name.clone())
            .and_modify(|v| *v = *v || effectful)
            .or_insert(effectful);
    }
    table
}

/// Phase 6 line 26 slice 8ab: convert the effect-checker's
/// `call_effect_subs` (keyed by `SpanKey` with internal `Effect`
/// values) into the AST-level `CallEffectSubsTable` (keyed by
/// `(offset, length)` with plain-data `EffectKey` values) so codegen
/// can read it without taking a dependency on the effectchecker's
/// `Effect` struct. Each entry's verb is rendered via a local
/// `verb_to_name` mirror of the effectchecker's diagnostic rendering;
/// resource names round-trip unchanged.
pub fn build_call_effect_subs_table(
    effects: &EffectCheckResult,
) -> crate::ast::CallEffectSubsTable {
    fn verb_to_name(verb: &EffectVerbKind) -> String {
        match verb {
            EffectVerbKind::Reads => "reads".to_string(),
            EffectVerbKind::Writes => "writes".to_string(),
            EffectVerbKind::Sends => "sends".to_string(),
            EffectVerbKind::Receives => "receives".to_string(),
            EffectVerbKind::Allocates => "allocates".to_string(),
            EffectVerbKind::Panics => "panics".to_string(),
            EffectVerbKind::Blocks => "blocks".to_string(),
            EffectVerbKind::Suspends => "suspends".to_string(),
            EffectVerbKind::UserDefined(name) => name.clone(),
        }
    }
    let mut table = crate::ast::CallEffectSubsTable::new();
    for (span_key, bindings) in &effects.call_effect_subs {
        let mut inner = std::collections::HashMap::new();
        for (var_name, effect_set) in bindings {
            let keys: Vec<crate::ast::EffectKey> = effect_set
                .iter()
                .map(|e| crate::ast::EffectKey {
                    verb: verb_to_name(&e.verb),
                    resource: e.resource.clone(),
                })
                .collect();
            inner.insert(var_name.clone(), keys);
        }
        table.insert((span_key.0, span_key.1), inner);
    }
    table
}

/// Phase 6 line 26 slice 8y: build the set of callee names whose
/// declared effects are `DeclaredEffects::Polymorphic` only — purely
/// `with E` (or `with _`) with no static fixed portion. Codegen uses
/// this set to identify callees for which `call_effect_subs` is the
/// sole authoritative source of "does this call resolve to a
/// network-yield effect", as opposed to `PolymorphicWithFixed` or
/// `Explicit` callees whose static portion may already carry
/// `sends(Network)` / `receives(Network)` and therefore must always
/// flow through the state-machine transform regardless of `E`
/// resolution.
///
/// Mirrors `build_callee_network_yield_effect_table`'s sourcing of
/// `declared_effects`; inferred effects on private fns are never
/// `Polymorphic` (`DeclaredEffects::Polymorphic` is set only via an
/// explicit `with E` / `with _` annotation), so they are excluded by
/// construction.
pub fn build_callee_purely_polymorphic_effects_set(
    effects: &EffectCheckResult,
) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    for (name, decl) in &effects.declared_effects {
        if matches!(decl, DeclaredEffects::Polymorphic) {
            set.insert(name.clone());
        }
    }
    set
}

/// Build the per-callee "is network-boundary" side-table from an
/// `EffectCheckResult`.
///
/// A callee is "network-boundary" iff its inferred or declared effect set
/// contains a `sends(Network)` or `receives(Network)` verb-resource pair.
/// These are the only effects that route through the network event loop's
/// non-blocking park-and-yield path at v1 (design.md § Network Event Loop
/// and State-Machine Transform > State-Machine Transform — Network-Boundary
/// Functions). Functions whose suspension is rooted in other verbs
/// (`Receiver.recv` via `suspends`, custom user `suspends`, future channel
/// waits) continue to thread-block at v1 and are NOT marked.
///
/// Consumed by:
///   - the state-machine transform codegen (phase 6 line 26) — only callees
///     marked `true` are candidates for the transform;
///   - codegen lowering at network-effect call sites (phase 6 line 17
///     sub-item 6) — a call to a `true` callee lowers to "register fd +
///     park + yield" instead of a synchronous call.
pub fn build_callee_network_yield_effect_table(
    effects: &EffectCheckResult,
) -> std::collections::HashMap<String, bool> {
    fn set_has_network_yield(set: &crate::effectchecker::EffectSet) -> bool {
        set.effects.iter().any(|t| {
            matches!(
                t.effect.verb,
                EffectVerbKind::Sends | EffectVerbKind::Receives
            ) && t.effect.resource == "Network"
        })
    }
    let mut table = std::collections::HashMap::new();
    for (name, set) in &effects.inferred_effects {
        table.insert(name.clone(), set_has_network_yield(set));
    }
    for (name, decl) in &effects.declared_effects {
        // Polymorphic effect parameters may bind to a `sends(Network)` /
        // `receives(Network)` at a monomorphization site, so conservatively
        // mark as network-boundary candidate. The state-machine transform
        // itself reads the resolved monomorphized effect set when deciding
        // to apply, so over-counting here is harmless — it just keeps the
        // function in the candidate pool that the transform pass filters.
        let network_yield = match decl {
            DeclaredEffects::Explicit(set) => set_has_network_yield(set),
            DeclaredEffects::PolymorphicWithFixed(_) | DeclaredEffects::Polymorphic => true,
            DeclaredEffects::None => false,
        };
        table
            .entry(name.clone())
            .and_modify(|v| *v = *v || network_yield)
            .or_insert(network_yield);
    }
    table
}

/// Walk every function/method body in `program` and, for each
/// network-boundary function (one marked `true` in `network_yield`),
/// produce its ordered list of yield points — call sites whose callee is
/// itself in `network_yield` with value `true`.
///
/// Callee resolution rules at a call site:
///   - `Call { callee: Identifier(name) }` → callee key is `name`;
///   - `Call { callee: Path { segments, .. } }` → callee key is the joined
///     segments separated by `.` (matches `Type.method` shape from
///     `EffectCheckResult` keys);
///   - `MethodCall { .. }` → callee key looked up in `method_callee_types`
///     via the call expression's span;
///   - All other callee shapes (indirect through closure value, function
///     pointer, etc.) → skipped — the codegen lowering pass can't park
///     through an unresolved callee without a stable effect signature.
///
/// Functions without any yield-point calls are omitted from the table
/// (they may still be network-boundary if classified via Polymorphic
/// effect declaration, but they have no concrete suspension points within
/// their bodies for the state-machine transform to lower against).
pub fn build_yield_points_table(
    program: &Program,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
) -> std::collections::HashMap<String, Vec<crate::ast::YieldPoint>> {
    let mut table = std::collections::HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(func) => {
                let key = func.name.clone();
                if network_yield.get(&key).copied().unwrap_or(false) {
                    let yps = walk_fn_for_yield_points(func, network_yield, method_callee_types);
                    if !yps.is_empty() {
                        table.insert(key, yps);
                    }
                }
            }
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => continue,
                };
                for ii in &imp.items {
                    let method = match ii {
                        crate::ast::ImplItem::Method(m) => m,
                        crate::ast::ImplItem::AssocType(_) => continue,
                    };
                    let key = format!("{}.{}", type_name, method.name);
                    if network_yield.get(&key).copied().unwrap_or(false) {
                        let yps =
                            walk_fn_for_yield_points(method, network_yield, method_callee_types);
                        if !yps.is_empty() {
                            table.insert(key, yps);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    table
}

/// Walker state for one function body. Threads the network-boundary
/// classification + method-callee resolution maps (read-only), tracks a
/// running scope stack of in-scope binding names (push on let / pattern
/// binding, pop on block exit), and accumulates yield-point records.
/// Centralizes the recursive-walk state cleaner than threading every
/// argument through each helper.
struct YieldPointWalker<'a> {
    network_yield: &'a std::collections::HashMap<String, bool>,
    method_callee_types: &'a std::collections::HashMap<crate::resolver::SpanKey, String>,
    /// Flat stack of in-scope local-binding names in source-introduction
    /// order. Function parameters occupy the bottom of the stack; later
    /// pushes come from `let` / `let-else` / `if let` / `while let` /
    /// `for` / match-arm pattern bindings as the walker crosses them.
    /// On every block exit, the walker truncates back to a recorded
    /// length (lexical scope discipline).
    scope: Vec<String>,
    out: Vec<crate::ast::YieldPoint>,
}

fn walk_fn_for_yield_points(
    func: &crate::ast::Function,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
) -> Vec<crate::ast::YieldPoint> {
    let mut walker = YieldPointWalker {
        network_yield,
        method_callee_types,
        scope: Vec::new(),
        out: Vec::new(),
    };
    // Function parameters are in scope throughout the body. `self` is
    // bound automatically when `self_param` is present (method bodies).
    // Each non-self param has a `Pattern` that may bind one (simple
    // `name: T`) or multiple (destructuring `let (a, b): (i64, i64)`)
    // names; collect them all.
    if func.self_param.is_some() {
        walker.scope.push("self".to_string());
    }
    for p in &func.params {
        for name in p.pattern.binding_names() {
            walker.scope.push(name);
        }
    }
    walker.walk_block(&func.body);
    walker.out
}

fn callee_key_from_call(callee: &crate::ast::Expr) -> Option<String> {
    use crate::ast::ExprKind;
    match &callee.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::Path { segments, .. } => Some(segments.join(".")),
        _ => None,
    }
}

impl YieldPointWalker<'_> {
    fn snapshot_scope(&self) -> Vec<String> {
        self.scope.clone()
    }

    fn walk_block(&mut self, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    /// Walk a block where the pattern's bindings are pre-pushed onto the
    /// scope (used for `if let` / `while let` / `for` bodies and the
    /// match-arm `Block` form). Pattern bindings live through the entire
    /// block and pop when it exits.
    fn walk_block_with_pattern(&mut self, pat: &crate::ast::Pattern, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for name in pat.binding_names() {
            self.scope.push(name);
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    /// Same idea for a match-arm body expression (which may be a Block
    /// or any other Expr — non-block arms still need pattern scope).
    fn walk_expr_with_pattern(&mut self, pat: &crate::ast::Pattern, expr: &crate::ast::Expr) {
        let scope_mark = self.scope.len();
        for name in pat.binding_names() {
            self.scope.push(name);
        }
        self.walk_expr(expr);
        self.scope.truncate(scope_mark);
    }

    fn walk_stmt(&mut self, stmt: &crate::ast::Stmt) {
        use crate::ast::StmtKind;
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, pattern, .. } => {
                // Walk the value FIRST — yield points in the RHS see the
                // pre-binding scope. Then introduce the pattern's bindings
                // into the parent scope.
                self.walk_expr(value);
                for name in pattern.binding_names() {
                    self.scope.push(name);
                }
            }
            StmtKind::LetUninit { name, .. } => {
                self.scope.push(name.clone());
            }
            StmtKind::LetElse {
                value,
                pattern,
                else_block,
                ..
            } => {
                // Value walks against the pre-binding scope.
                self.walk_expr(value);
                // Else block walks in its own nested scope — it must
                // diverge, so its bindings never propagate to the parent.
                self.walk_block(else_block);
                // Success-branch pattern bindings flow into the parent
                // scope after the let-else statement.
                for name in pattern.binding_names() {
                    self.scope.push(name);
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_expr(&mut self, expr: &crate::ast::Expr) {
        use crate::ast::ExprKind;
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(key) = callee_key_from_call(callee) {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        let captured = self.snapshot_scope();
                        self.out.push(crate::ast::YieldPoint {
                            callee: key,
                            span: expr.span.clone(),
                            captured_locals: captured,
                        });
                    }
                }
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                if let Some(key) = self
                    .method_callee_types
                    .get(&crate::resolver::SpanKey::from_span(&expr.span))
                    .cloned()
                {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        let captured = self.snapshot_scope();
                        self.out.push(crate::ast::YieldPoint {
                            callee: key,
                            span: expr.span.clone(),
                            captured_locals: captured,
                        });
                    }
                }
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    for arg in arglist {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        // Guards execute under the arm's pattern bindings.
                        let scope_mark = self.scope.len();
                        for name in arm.pattern.binding_names() {
                            self.scope.push(name);
                        }
                        self.walk_expr(g);
                        self.scope.truncate(scope_mark);
                    }
                    self.walk_expr_with_pattern(&arm.pattern, &arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body)
            }
            // Closures form their own state machine — a yield point inside
            // a closure body is the closure's yield, not the enclosing
            // function's. Do NOT walk into the closure body for the outer
            // function's yield-point enumeration.
            ExprKind::Closure { .. } => {}
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Return(None) => {}
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue { .. } => {}
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { body, .. } => self.walk_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            // Leaves / no-call shapes.
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e) = part {
                        self.walk_expr(e);
                    }
                }
            }
        }
    }
}

/// Build the per-function state-struct layout table from a fully-typed
/// `Program` whose `yield_points` table is populated. For each
/// network-boundary function with at least one concrete yield point,
/// produces a `StateStructLayout` whose `fields` list is the union of
/// every yield point's captured-locals set in source-introduction order
/// (parameters first left-to-right, then per-block let-binding sequence;
/// first occurrence across yield points fixes position).
///
/// Each field's `type_name` is looked up in `pattern_binding_types`
/// against the introducing pattern's span — primitives and other shapes
/// the typechecker doesn't record there yield `None`, and codegen falls
/// through to its primitive-sizing path on absent entries.
///
/// `self` is recorded with `type_name` set to the impl block's target
/// type name (not via `pattern_binding_types` — there is no pattern
/// span for `self`; the impl target supplies the canonical name
/// directly).
///
/// Shadowed bindings get separate field slots — collision is keyed on
/// the introducing pattern's span, not the binding name, so the v1
/// layout faithfully reflects the source-level binding identity.
///
/// Functions network-boundary by Polymorphic declared-effect candidacy
/// without any concrete sub-call yield points are omitted from the
/// table (mirrors `YieldPointsTable`'s presence rule).
pub fn build_state_struct_layouts(
    program: &Program,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
) -> std::collections::HashMap<String, crate::ast::StateStructLayout> {
    let mut table = std::collections::HashMap::new();
    for item in &program.items {
        match item {
            Item::Function(func) => {
                let key = func.name.clone();
                if network_yield.get(&key).copied().unwrap_or(false) {
                    if let Some(layout) = walk_fn_for_state_struct_layout(
                        func,
                        None,
                        network_yield,
                        method_callee_types,
                        pattern_binding_types,
                    ) {
                        table.insert(key, layout);
                    }
                }
            }
            Item::ImplBlock(imp) => {
                let type_name = match &imp.target_type.kind {
                    crate::ast::TypeKind::Path(p) => p.segments.last().cloned().unwrap_or_default(),
                    _ => continue,
                };
                for ii in &imp.items {
                    let method = match ii {
                        crate::ast::ImplItem::Method(m) => m,
                        crate::ast::ImplItem::AssocType(_) => continue,
                    };
                    let key = format!("{}.{}", type_name, method.name);
                    if network_yield.get(&key).copied().unwrap_or(false) {
                        if let Some(layout) = walk_fn_for_state_struct_layout(
                            method,
                            Some(type_name.as_str()),
                            network_yield,
                            method_callee_types,
                            pattern_binding_types,
                        ) {
                            table.insert(key, layout);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    table
}

/// Walker state for one function body's state-struct layout synthesis.
/// Mirrors `YieldPointWalker`'s scope-tracking discipline (push on binding
/// introduction; truncate on block exit) but enriches each scope slot with
/// the `SpanKey` of the pattern that introduced the binding so the
/// typechecker's `pattern_binding_types` lookup resolves at yield-point
/// snapshots. The walker accumulates a per-function field union directly
/// — duplicate (name, span) pairs across yield points are coalesced via
/// `seen`. Same-name bindings introduced at different spans (shadowing)
/// get distinct slots.
struct StateStructLayoutWalker<'a> {
    network_yield: &'a std::collections::HashMap<String, bool>,
    method_callee_types: &'a std::collections::HashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &'a std::collections::HashMap<crate::resolver::SpanKey, String>,
    /// Flat stack of in-scope binding (name, introducing-pattern-span)
    /// pairs in source-introduction order. `self` carries a fixed sentinel
    /// span-key — its type comes from the impl target, not from the
    /// pattern_binding_types map.
    scope: Vec<ScopeEntry>,
    fields: Vec<crate::ast::StateStructField>,
    seen: std::collections::HashSet<ScopeEntryKey>,
    /// Flips `true` the first time the walker recognises a network-effect
    /// call site (yield point). Drives the presence rule: a network-boundary
    /// function without any concrete yield-point call in its body — even
    /// one classified by Polymorphic candidacy at the FFI primitive layer
    /// — produces no table entry, mirroring `YieldPointsTable`.
    had_yield_point: bool,
}

#[derive(Clone)]
struct ScopeEntry {
    name: String,
    /// `Some(key)` for ordinary bindings (param, let, pattern); `None`
    /// for `self` and any future synthetic binding without a recorded
    /// pattern span. When `None`, `type_override` carries the surface
    /// type directly.
    span_key: Option<crate::resolver::SpanKey>,
    /// Source `Span` of the binding's introducing pattern, threaded
    /// into `StateStructField.binding_span` so `raii_check` can anchor
    /// a "binding declared here" secondary highlight. `SpanKey` is
    /// lossy (offset+length only), so the full `Span` is carried in
    /// parallel rather than reconstructed. `None` mirrors `span_key:
    /// None` (synthetic bindings like `self`).
    binding_span: Option<crate::token::Span>,
    type_override: Option<String>,
}

#[derive(Clone, Eq, PartialEq, Hash)]
enum ScopeEntryKey {
    Span(crate::resolver::SpanKey),
    Synthetic(String),
}

fn walk_fn_for_state_struct_layout(
    func: &crate::ast::Function,
    impl_target_type: Option<&str>,
    network_yield: &std::collections::HashMap<String, bool>,
    method_callee_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
    pattern_binding_types: &std::collections::HashMap<crate::resolver::SpanKey, String>,
) -> Option<crate::ast::StateStructLayout> {
    let mut walker = StateStructLayoutWalker {
        network_yield,
        method_callee_types,
        pattern_binding_types,
        scope: Vec::new(),
        fields: Vec::new(),
        seen: std::collections::HashSet::new(),
        had_yield_point: false,
    };
    if func.self_param.is_some() {
        walker.scope.push(ScopeEntry {
            name: "self".to_string(),
            span_key: None,
            binding_span: None,
            type_override: impl_target_type.map(|s| s.to_string()),
        });
    }
    for p in &func.params {
        for (name, span) in p.pattern.binding_name_spans() {
            walker.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
    }
    walker.walk_block(&func.body);
    if walker.had_yield_point {
        Some(crate::ast::StateStructLayout {
            fields: walker.fields,
        })
    } else {
        None
    }
}

impl StateStructLayoutWalker<'_> {
    fn record_yield_point_capture(&mut self) {
        self.had_yield_point = true;
        for entry in &self.scope {
            let key = match entry.span_key {
                Some(k) => ScopeEntryKey::Span(k),
                None => ScopeEntryKey::Synthetic(entry.name.clone()),
            };
            if self.seen.insert(key) {
                let type_name = entry.type_override.clone().or_else(|| {
                    entry
                        .span_key
                        .and_then(|k| self.pattern_binding_types.get(&k).cloned())
                });
                self.fields.push(crate::ast::StateStructField {
                    name: entry.name.clone(),
                    type_name,
                    binding_span: entry.binding_span.clone(),
                });
            }
        }
    }

    fn walk_block(&mut self, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_block_with_pattern(&mut self, pat: &crate::ast::Pattern, block: &crate::ast::Block) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            self.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(ref expr) = block.final_expr {
            self.walk_expr(expr);
        }
        self.scope.truncate(scope_mark);
    }

    fn walk_expr_with_pattern(&mut self, pat: &crate::ast::Pattern, expr: &crate::ast::Expr) {
        let scope_mark = self.scope.len();
        for (name, span) in pat.binding_name_spans() {
            self.scope.push(ScopeEntry {
                name,
                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                binding_span: Some(span),
                type_override: None,
            });
        }
        self.walk_expr(expr);
        self.scope.truncate(scope_mark);
    }

    fn walk_stmt(&mut self, stmt: &crate::ast::Stmt) {
        use crate::ast::StmtKind;
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, pattern, .. } => {
                self.walk_expr(value);
                for (name, span) in pattern.binding_name_spans() {
                    self.scope.push(ScopeEntry {
                        name,
                        span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                        binding_span: Some(span),
                        type_override: None,
                    });
                }
            }
            StmtKind::LetUninit {
                name, name_span, ..
            } => {
                self.scope.push(ScopeEntry {
                    name: name.clone(),
                    span_key: Some(crate::resolver::SpanKey::from_span(name_span)),
                    binding_span: Some(name_span.clone()),
                    type_override: None,
                });
            }
            StmtKind::LetElse {
                value,
                pattern,
                else_block,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(else_block);
                for (name, span) in pattern.binding_name_spans() {
                    self.scope.push(ScopeEntry {
                        name,
                        span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                        binding_span: Some(span),
                        type_override: None,
                    });
                }
            }
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(expr) => self.walk_expr(expr),
        }
    }

    fn walk_expr(&mut self, expr: &crate::ast::Expr) {
        use crate::ast::ExprKind;
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(key) = callee_key_from_call(callee) {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield_point_capture();
                    }
                }
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                if let Some(key) = self
                    .method_callee_types
                    .get(&crate::resolver::SpanKey::from_span(&expr.span))
                    .cloned()
                {
                    if self.network_yield.get(&key).copied().unwrap_or(false) {
                        self.record_yield_point_capture();
                    }
                }
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } => self.walk_expr(operand),
            ExprKind::Question(inner) => self.walk_expr(inner),
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(arglist) = args {
                    for arg in arglist {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object)
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b)
            | ExprKind::Comptime(b)
            | ExprKind::Unsafe(b)
            | ExprKind::Try(b)
            | ExprKind::Seq(b)
            | ExprKind::Par(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                pattern,
                then_block,
                else_branch,
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(ref g) = arm.guard {
                        let scope_mark = self.scope.len();
                        for (name, span) in arm.pattern.binding_name_spans() {
                            self.scope.push(ScopeEntry {
                                name,
                                span_key: Some(crate::resolver::SpanKey::from_span(&span)),
                                binding_span: Some(span),
                                type_override: None,
                            });
                        }
                        self.walk_expr(g);
                        self.scope.truncate(scope_mark);
                    }
                    self.walk_expr_with_pattern(&arm.pattern, &arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet {
                value,
                pattern,
                body,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::For {
                pattern,
                iterable,
                body,
                ..
            } => {
                self.walk_expr(iterable);
                self.walk_block_with_pattern(pattern, body);
            }
            ExprKind::Loop { body, .. } | ExprKind::LabeledBlock { body, .. } => {
                self.walk_block(body)
            }
            // Closures form their own state machine — same as YieldPointWalker.
            ExprKind::Closure { .. } => {}
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Return(None) => {}
            ExprKind::Break { value, .. } => {
                if let Some(v) = value {
                    self.walk_expr(v);
                }
            }
            ExprKind::Continue { .. } => {}
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for e in items {
                    self.walk_expr(e);
                }
            }
            ExprKind::RepeatLiteral { value, count, .. } => {
                self.walk_expr(value);
                self.walk_expr(count);
            }
            ExprKind::MapLiteral(pairs) => {
                for (k, v) in pairs {
                    self.walk_expr(k);
                    self.walk_expr(v);
                }
            }
            ExprKind::StructLiteral { fields, spread, .. } => {
                for f in fields {
                    self.walk_expr(&f.value);
                }
                if let Some(s) = spread {
                    self.walk_expr(s);
                }
            }
            ExprKind::Pipe { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Cast { expr, .. } => self.walk_expr(expr),
            ExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.walk_expr(s);
                }
                if let Some(e) = end {
                    self.walk_expr(e);
                }
            }
            ExprKind::Lock { body, .. } => self.walk_block(body),
            ExprKind::Providers { bindings, body } => {
                for b in bindings {
                    self.walk_expr(&b.value);
                }
                self.walk_block(body);
            }
            ExprKind::Integer(_, _)
            | ExprKind::Float(_, _)
            | ExprKind::CharLit(_)
            | ExprKind::ByteLit(_)
            | ExprKind::StringLit(_)
            | ExprKind::MultiStringLit(_)
            | ExprKind::CStringLit { .. }
            | ExprKind::Bool(_)
            | ExprKind::Identifier(_)
            | ExprKind::Path { .. }
            | ExprKind::SelfValue
            | ExprKind::SelfType
            | ExprKind::PipePlaceholder
            | ExprKind::OffsetOf { .. }
            | ExprKind::Error => {}
            ExprKind::InterpolatedStringLit(parts) => {
                for part in parts {
                    if let crate::ast::ParsedInterpolationPart::Expr(e) = part {
                        self.walk_expr(e);
                    }
                }
            }
        }
    }
}

fn effect_verb_str(v: &EffectVerbKind) -> &str {
    match v {
        EffectVerbKind::Reads => "reads",
        EffectVerbKind::Writes => "writes",
        EffectVerbKind::Sends => "sends",
        EffectVerbKind::Receives => "receives",
        EffectVerbKind::Allocates => "allocates",
        EffectVerbKind::Panics => "panics",
        EffectVerbKind::Blocks => "blocks",
        EffectVerbKind::Suspends => "suspends",
        EffectVerbKind::UserDefined(s) => s.as_str(),
    }
}

fn ownership_mode_str(m: &OwnershipMode) -> &str {
    match m {
        OwnershipMode::Own => "own",
        OwnershipMode::Ref => "ref",
        OwnershipMode::MutRef => "mut_ref",
    }
}

struct DiagEntry<'a> {
    id: &'a str,
    severity: &'a str,
    phase: &'a str,
    code: &'a str,
    category: &'a str,
    message: &'a str,
    filename: &'a str,
    span: &'a Span,
    suggestion: Option<&'a str>,
    /// Optional pre-formatted JSON fields appended verbatim to the entry object.
    extra_json: Option<String>,
    /// Registered lint name when this entry is a warning routed through
    /// a lint (slice 7 of the lint-level entry — see
    /// `phase-5-diagnostics.md`). Surfaced as `"lint_name":"..."` in the
    /// JSON output so `karac --output=json` consumers can route, group,
    /// and filter by lint. `None` on hard errors and on warnings that
    /// haven't migrated to a registered lint yet.
    lint_name: Option<&'a str>,
    /// Machine-applicable fix-it edit when the diagnostic supplies one
    /// (`#[non_exhaustive]` slice 7 introduces the producers for the
    /// cross-package pattern and match diagnostics). Surfaced as a
    /// nested `"fix_it":{"span":{...},"replacement":"..."}` object so
    /// IDE / formatter consumers can apply it without re-parsing the
    /// message text. `None` for every other diagnostic; widens as
    /// more producers land.
    fix_it: Option<&'a crate::typechecker::FixIt>,
    /// Broad-category class label for the structured-diagnostic
    /// JSON envelope (`karac --output=json` consumers — LLM agents,
    /// IDE tooling). Auto-derived from the typechecker error's
    /// `kind` at `TypeError` construction time; the wire form is
    /// the UPPER_SNAKE `DiagnosticClass::as_str()`. Line 619 slice
    /// 4 surfaces it on every type/effect/lint diagnostic.
    class: Option<&'static str>,
    /// Display form of the *expected* type / shape at the diagnostic
    /// site, when populated by the typechecker via the typed-fields
    /// helper. Surfaces as `"expected":"i32"` in the JSON record.
    /// Line 619 slice 4.
    expected: Option<&'a str>,
    /// Display form of the *got* / actual type at the diagnostic
    /// site. Mirror of `expected`. Line 619 slice 4.
    got: Option<&'a str>,
    /// Pre-rendered JSON object for a `hints[]` entry carrying a
    /// signature-from-call-site stub diff (phase-5-diagnostics line
    /// 633). Set on unresolved-call diagnostics emitted inside a
    /// `_test.kara` file; left `None` everywhere else. The string is
    /// the inner JSON object `{"description":"…","diff":{"file":…,
    /// "line":…,"old":"","new":"…"}}` — spliced into the unified
    /// `hints` array alongside the existing `did you mean`
    /// description entry when both are present.
    stub_hint_json: Option<String>,
}

struct DiagnosticJson {
    entries: Vec<String>,
}

impl DiagnosticJson {
    fn new() -> Self {
        DiagnosticJson {
            entries: Vec::new(),
        }
    }

    fn add(&mut self, d: DiagEntry<'_>) {
        let mut entry = format!(
            "{{\"id\":{},\"severity\":{},\"primary\":true,\"phase\":{},\"code\":{},\"category\":{},{},\"message\":{}",
            json_string(d.id),
            json_string(d.severity),
            json_string(d.phase),
            json_string(d.code),
            json_string(d.category),
            span_to_json(d.span, d.filename),
            json_string(d.message),
        );
        // Unified `hints` array: combines the (existing) `suggestion`
        // description entry and any signature-from-call-site stub-diff
        // entry (line 633). At least one of the two must be set for
        // the field to appear; both can coexist on the same
        // diagnostic (e.g. an unresolved-call in a test file that
        // also has a `did you mean` neighbour).
        let mut hints: Vec<String> = Vec::new();
        if let Some(s) = d.suggestion {
            hints.push(format!("{{\"description\":{}}}", json_string(s)));
        }
        if let Some(ref sj) = d.stub_hint_json {
            hints.push(sj.clone());
        }
        if !hints.is_empty() {
            write!(entry, ",\"hints\":[{}]", hints.join(",")).unwrap();
        }
        if let Some(name) = d.lint_name {
            write!(entry, ",\"lint_name\":{}", json_string(name)).unwrap();
        }
        if let Some(class) = d.class {
            write!(entry, ",\"class\":{}", json_string(class)).unwrap();
        }
        if let Some(expected) = d.expected {
            write!(entry, ",\"expected\":{}", json_string(expected)).unwrap();
        }
        if let Some(got) = d.got {
            write!(entry, ",\"got\":{}", json_string(got)).unwrap();
        }
        if let Some(fix) = d.fix_it {
            // `#[non_exhaustive]` slice 7 — surface the
            // machine-applicable edit as a nested object. `length` is
            // included so consumers can distinguish insertion
            // (length=0) from replacement without re-deriving from
            // start/end markers.
            write!(
                entry,
                ",\"fix_it\":{{\"span\":{{{},\"offset\":{},\"length\":{}}},\"replacement\":{}}}",
                span_to_json(&fix.span, d.filename),
                fix.span.offset,
                fix.span.length,
                json_string(&fix.replacement),
            )
            .unwrap();
            // Line 619 slice 5 — also emit the multi-edit `fixes`
            // array form per the structured-diagnostic spec. The
            // single-edit `fix_it` field stays for back-compat with
            // existing consumers; the array form is what new LLM /
            // IDE consumers should consume going forward. Each fix
            // carries a `description` (derived from the lint name
            // when available, else a generic "apply suggested
            // edit") and an `edits` array of `{span, replacement}`
            // entries. v1 ships one entry per fix; the array shape
            // is forward-compatible with multi-edit fixes when they
            // land.
            let description = d.lint_name.unwrap_or("apply suggested edit");
            write!(
                entry,
                ",\"fixes\":[{{\"description\":{},\"edits\":[{{\"span\":{{{},\"offset\":{},\"length\":{}}},\"replacement\":{}}}]}}]",
                json_string(description),
                span_to_json(&fix.span, d.filename),
                fix.span.offset,
                fix.span.length,
                json_string(&fix.replacement),
            )
            .unwrap();
        }
        if let Some(ref extra) = d.extra_json {
            write!(entry, ",{}", extra).unwrap();
        }
        entry.push('}');
        self.entries.push(entry);
    }

    fn to_json_array(&self) -> String {
        if self.entries.is_empty() {
            return "[]".to_string();
        }
        format!("[{}]", self.entries.join(","))
    }
}

/// Munge the path of a `_test.kara` file to its sibling production
/// file: `src/math_test.kara` → `src/math.kara`. Returns the input
/// unchanged when the basename does not match the `_test.kara`
/// convention — defensive fallback so a future test-file convention
/// change does not silently mis-route the stub diff.
fn sibling_production_file(test_path: &str) -> String {
    // Split the last path component so the `_test.kara` suffix swap
    // does not touch directory names containing `_test` substrings.
    if let Some(stripped) = test_path.strip_suffix("_test.kara") {
        format!("{stripped}.kara")
    } else {
        test_path.to_string()
    }
}

/// Best-effort line count for the sibling production file. Used as the
/// `line` field of the stub-hint diff so the consumer (LLM agent / IDE)
/// knows where in the file the insertion lands. When the file does not
/// exist yet (pure-TDD opener: test file written first, production
/// file not yet created), returns `1` so the diff describes "create
/// the file with this body."
fn target_append_line(target_file: &str) -> u32 {
    match std::fs::read_to_string(target_file) {
        Ok(contents) => {
            // Append after the last existing line. Line count + 1 even
            // when the file ends with a trailing newline — the new
            // content lands on the line *after* the trailing newline.
            let line_count = contents.lines().count();
            (line_count as u32) + 1
        }
        Err(_) => 1,
    }
}

/// Render a single `hints[]` entry carrying a signature-from-call-site
/// stub diff (phase-5-diagnostics line 633). The output is the inner
/// JSON object — the surrounding `[ ]` is added by `DiagnosticJson::add`
/// when assembling the unified hints array.
fn render_stub_hint_json(filename: &str, hint: &crate::resolver::StubHint) -> String {
    let target_file = sibling_production_file(filename);
    let line = target_append_line(&target_file);
    let new_source = hint.render_source();
    let description = format!(
        "stub `{}` in {} with inferred signature",
        hint.callee_name, target_file
    );
    format!(
        "{{\"description\":{},\"diff\":{{\"file\":{},\"line\":{},\"old\":{},\"new\":{}}}}}",
        json_string(&description),
        json_string(&target_file),
        line,
        json_string(""),
        json_string(&new_source),
    )
}

fn collect_diagnostics(pipeline: &Pipeline) -> DiagnosticJson {
    let mut diags = DiagnosticJson::new();
    let filename = &pipeline.filename;
    let mut id_counter = 0u32;

    for err in &pipeline.parsed.errors {
        id_counter += 1;
        // Parse-phase machine-applicable fix (e.g. delete a stray comma in a
        // comma-separated `with` clause), matched to this diagnostic by span.
        // Same `"replacement":{offset,length,text}` shape the resolver emits,
        // so `karac fix` and IDE quick-fix consumers read it uniformly.
        let replacement_json = pipeline
            .parsed
            .fix_edits
            .get(&crate::resolver::SpanKey::from_span(&err.span))
            .map(|e| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    e.offset,
                    e.length,
                    json_string(&e.replacement),
                )
            });
        diags.add(DiagEntry {
            id: &format!("d{id_counter}"),
            severity: "error",
            phase: "parse",
            code: "E0001",
            category: "parse",
            message: &err.message,
            filename,
            span: &err.span,
            suggestion: None,
            extra_json: replacement_json,
            lint_name: None,
            fix_it: None,
            class: None,
            expected: None,
            got: None,
            stub_hint_json: None,
        });
    }

    if let Some(ref r) = pipeline.resolved {
        for err in &r.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::resolver::ResolveErrorKind::UndefinedName => "E0100",
                crate::resolver::ResolveErrorKind::DuplicateDefinition => "E0101",
                crate::resolver::ResolveErrorKind::ReservedIdentifier => "E0102",
                crate::resolver::ResolveErrorKind::PrivateAccess => "E0103",
                crate::resolver::ResolveErrorKind::UndefinedType => "E0104",
                crate::resolver::ResolveErrorKind::UndefinedVariant => "E0105",
                crate::resolver::ResolveErrorKind::UndefinedField => "E0106",
                crate::resolver::ResolveErrorKind::UndefinedLabel => "E0107",
                crate::resolver::ResolveErrorKind::OperatorTraitImplRestricted => "E0108",
                crate::resolver::ResolveErrorKind::IntoTraitImplNotAllowed => "E0109",
                crate::resolver::ResolveErrorKind::ImplLevelEffectVarNotAllowed => "E0110",
                crate::resolver::ResolveErrorKind::UnknownModule => "E0224",
                crate::resolver::ResolveErrorKind::UnknownItemInModule => "E0225",
                crate::resolver::ResolveErrorKind::PrivateItemAccess => "E0222",
                crate::resolver::ResolveErrorKind::ReservedEffectResource => "E0228",
                crate::resolver::ResolveErrorKind::CompilerBuiltinReserved => "E0237",
                crate::resolver::ResolveErrorKind::ContinueOnBlockLabel => "E0238",
                crate::resolver::ResolveErrorKind::NonExhaustiveInvalidTarget => "E0239",
                crate::resolver::ResolveErrorKind::TrackCallerInvalidTarget => "E0240",
                crate::resolver::ResolveErrorKind::GpuInvalidTarget => "E0800",
                crate::resolver::ResolveErrorKind::CodegenHintInvalidTarget => {
                    "E_CODEGEN_HINT_INVALID_POSITION"
                }
                crate::resolver::ResolveErrorKind::CodegenHintOnExternDecl => {
                    "E_CODEGEN_HINT_ON_EXTERN_DECL"
                }
                crate::resolver::ResolveErrorKind::DeprecatedOnImpl => "E0241",
                crate::resolver::ResolveErrorKind::DeprecatedOnField => "E0242",
                crate::resolver::ResolveErrorKind::UnknownAttribute => "E0243",
                crate::resolver::ResolveErrorKind::ProfileInvalidTarget => "E0244",
                crate::resolver::ResolveErrorKind::UnknownProfile => "E0245",
                crate::resolver::ResolveErrorKind::QueryResolutionConflict => {
                    "E_QUERY_RESOLUTION_CONFLICT"
                }
                crate::resolver::ResolveErrorKind::UnionNonExhaustiveForbidden => {
                    "E_UNION_NON_EXHAUSTIVE_FORBIDDEN"
                }
                crate::resolver::ResolveErrorKind::DefaultAttributeInvalidPosition => {
                    "E_DEFAULT_ATTRIBUTE_INVALID_POSITION"
                }
                crate::resolver::ResolveErrorKind::DefaultAttributeWithoutDerive => {
                    "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE"
                }
                crate::resolver::ResolveErrorKind::MalformedAttributeArgs => {
                    "E_MALFORMED_ATTRIBUTE_ARGS"
                }
            };
            // Surface the machine-applicable replacement (when present)
            // alongside the human-readable suggestion. Consumers like
            // `karac fix` and IDE quick-fix UIs read this directly to
            // produce one-click rewrites.
            let replacement_json = err.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            let stub_hint_json = err
                .stub_hint
                .as_ref()
                .map(|s| render_stub_hint_json(filename, s));
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "resolve",
                code,
                category: "resolve",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: err.suggestion.as_deref(),
                extra_json: replacement_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json,
            });
        }
    }

    if let Some(ref t) = pipeline.typed {
        for err in &t.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::typechecker::TypeErrorKind::TypeMismatch => "E0200",
                crate::typechecker::TypeErrorKind::UndefinedField => "E0201",
                crate::typechecker::TypeErrorKind::WrongNumberOfArgs => "E0202",
                crate::typechecker::TypeErrorKind::MissingField => "E0203",
                crate::typechecker::TypeErrorKind::ExtraField => "E0204",
                crate::typechecker::TypeErrorKind::NonExhaustiveMatch => "E0205",
                crate::typechecker::TypeErrorKind::NotCallable => "E0206",
                crate::typechecker::TypeErrorKind::NotAStruct => "E0207",
                crate::typechecker::TypeErrorKind::InvalidBinaryOp => "E0208",
                crate::typechecker::TypeErrorKind::InvalidUnaryOp => "E0209",
                crate::typechecker::TypeErrorKind::InvalidCast => "E0210",
                crate::typechecker::TypeErrorKind::ConditionNotBool => "E0211",
                crate::typechecker::TypeErrorKind::BranchTypeMismatch => "E0212",
                crate::typechecker::TypeErrorKind::ReturnTypeMismatch => "E0213",
                crate::typechecker::TypeErrorKind::InvalidTupleIndex => "E0214",
                crate::typechecker::TypeErrorKind::LabelMismatch => "E0215",
                crate::typechecker::TypeErrorKind::NonContiguousLabels => "E0216",
                crate::typechecker::TypeErrorKind::InvalidPipePlaceholder => "E0217",
                crate::typechecker::TypeErrorKind::MissingMutMarker => "E0218",
                crate::typechecker::TypeErrorKind::InvalidMutMarker => "E0219",
                crate::typechecker::TypeErrorKind::UnsupportedNumericSuffix => "E0220",
                crate::typechecker::TypeErrorKind::PrivateTypeInPublicSignature => "E0221",
                crate::typechecker::TypeErrorKind::RefutablePattern => "E0222",
                crate::typechecker::TypeErrorKind::MissingSupertrait => "E0229",
                crate::typechecker::TypeErrorKind::TraitBoundNotSatisfied => "E0232",
                crate::typechecker::TypeErrorKind::AmbiguousAssocFn => "E0233",
                crate::typechecker::TypeErrorKind::CannotInferAssocFn => "E0234",
                crate::typechecker::TypeErrorKind::OnceFnIntoFnSlot => "E0235",
                crate::typechecker::TypeErrorKind::NoMethodFound => "E0236",
                crate::typechecker::TypeErrorKind::UnreachableArm => "W0237",
                crate::typechecker::TypeErrorKind::RefinementDomainTooWide => "W0238",
                crate::typechecker::TypeErrorKind::CannotInferTypeParam => "E0238",
                crate::typechecker::TypeErrorKind::AmbiguousMethod => "E0239",
                crate::typechecker::TypeErrorKind::ConflictingImpl => "E0240",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackageLiteral => "E0241",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackageMatch => "E0242",
                crate::typechecker::TypeErrorKind::NonExhaustiveCrossPackagePattern => "E0243",
                crate::typechecker::TypeErrorKind::UnknownLint => "W0244",
                // `Deprecated` only appears as a warning under default
                // settings; if `#[deny(deprecated)]` promotes it to an
                // error the same code is reused as `E0245`.
                crate::typechecker::TypeErrorKind::Deprecated => "E0245",
                // `MissingNonExhaustive` is `Deny`-by-default per
                // `STARTER_LINTS`, so it normally surfaces as an error
                // (W-prefixed because the underlying carrier is a lint).
                crate::typechecker::TypeErrorKind::MissingNonExhaustive => "W0246",
                // Lint-level slice 4b polish — emitted only when the
                // CLI sets `-F NAME` and an inner `#[allow(NAME)]`
                // is rejected; never appears as a warning (the
                // diagnostic is a hard error by construction).
                crate::typechecker::TypeErrorKind::ForbiddenLintAllow => "E0247",
                // Lint-level slice 5 — `#[expect(unfulfilled_lint_expectation)]`
                // rejection (would be circular).
                crate::typechecker::TypeErrorKind::ExpectOnUnfulfilled => "E0248",
                // Lint-level slice 5 — appears on the errors path only
                // when promoted via `#[deny(unfulfilled_lint_expectation)]`.
                crate::typechecker::TypeErrorKind::UnfulfilledLintExpectation => "E0249",
                // Module-level `let` / `let mut` slice 4 — see
                // `docs/implementation_checklist/phase-8-stdlib-floor.md`
                // mod-let entry. The const-init structural rule and the
                // §1297 heap-String rejection both surface here.
                crate::typechecker::TypeErrorKind::ModuleBindingEffectfulInit => "E0250",
                crate::typechecker::TypeErrorKind::ModuleBindingHeapType => "E0251",
                // Slice 5 — assignment to a module-level immutable `let`.
                crate::typechecker::TypeErrorKind::ReassignToImmutableModuleBinding => "E0252",
                // Phase 6 line 218 slice 2 — ScopeLocal escape
                // diagnostic (design.md § ScopeLocal). Fires when a
                // `ScopeLocal` marker-trait type appears in a
                // function return, struct/enum field, or
                // `Sender.send` argument.
                crate::typechecker::TypeErrorKind::ScopeLocalEscape => "E0253",
                // Phase 6 line 170 slice 3a — cross-task-safe boundary
                // check at `spawn(closure)` / `TaskGroup.spawn(closure)`
                // call sites. Fires when a captured binding's type
                // reaches a cross-task-unsafe leaf (`Rc[T]`, `shared`,
                // `OnceCell[T]`, raw pointer) per
                // `src/cross_task_safe.rs`'s closed structural list.
                crate::typechecker::TypeErrorKind::CrossTaskUnsafeCapture => "E0254",
                // Phase-8 line 49 — `#[unstable]` use-site lint
                // promoted to error via `#[deny(unstable_api)]`.
                // Reuses the same numeric slot as the warning
                // (`W0255`) by convention with `Deprecated`.
                crate::typechecker::TypeErrorKind::UnstableApi => "E0255",
                // Phase 9 line 25 step 1 — a refinement type's `where`
                // predicate uses a construct outside the allowed
                // constraint language (design.md § Refinement Types).
                crate::typechecker::TypeErrorKind::InvalidRefinementPredicate => "E0256",
                // Phase 6 `par struct` slice A — a `mut` field of a
                // `par struct` / `par enum` is not `Atomic[T]` / `Mutex[T]`
                // (design.md § Part 5b > Field constraints).
                crate::typechecker::TypeErrorKind::ParFieldNotConcurrent => "E0257",
                // Phase 6 `par struct` slice A — a `par struct` / `par enum`
                // method declares a `mut self` receiver; only `ref self` (and
                // consuming `self`) are permitted because `par` values are
                // always Arc with potential multiple holders (design.md
                // § Part 5b > `ref self` receivers only).
                crate::typechecker::TypeErrorKind::ParMutSelfReceiver => "E0258",
                // E0259 retired: a `lock` block body MAY now contain early exits
                // (`return` / `break` / `continue`) — codegen seeds the release
                // as a cleanup-frame action so it fires on every exit path.
                // Phase 6 `Mutex` / `lock` — the `lock` target is not a
                // `Mutex[T]` binding.
                crate::typechecker::TypeErrorKind::LockTargetNotMutex => "E0260",
                // Phase 8 `@` bindings slice 4 — owned scrutinee, outer
                // `@` alias and an inner sub-pattern binding both claim
                // non-Copy ownership of overlapping content (design.md
                // § @ Bindings > Owned scrutinee).
                crate::typechecker::TypeErrorKind::AtBindingDoubleConsume => "E0261",
                // Type Aliases (v60 item 50) — a generic alias use-site arg
                // fails a trait bound declared on the alias parameter.
                crate::typechecker::TypeErrorKind::TypeAliasBoundNotSatisfied => "E0262",
                // Range Patterns (v60 item 51) — a const-named range bound
                // does not resolve to a module-level int/char const.
                crate::typechecker::TypeErrorKind::RangePatternBoundNotConst => "E0263",
                // Fallible Allocation (v60 item 46) — a panicking heap-allocating
                // operation appears under `panic_on_alloc_failure = false`.
                crate::typechecker::TypeErrorKind::PanickingAllocRejected => "E0264",
                // Fallible Allocation (v60 item 46) — `#[derive(Clone)]` whose
                // synthesized clone may panic on allocation failure under
                // `panic_on_alloc_failure = false`.
                crate::typechecker::TypeErrorKind::DeriveCloneAllocates => "E0265",
                // Phase-8 entry-point contract Slice C — `main()` declares a
                // return type outside `()` / `Result[(), E: Display]` /
                // `ExitCode` (design.md § Entry Point).
                crate::typechecker::TypeErrorKind::MainReturnType => "E0266",
                // Slice C — `main() -> Result[(), E]` where `E` lacks `Display`.
                crate::typechecker::TypeErrorKind::MainErrNotDisplay => "E0267",
                // `s[i]` (scalar index) on a `String` — UTF-8 is
                // variable-width, so `[]` is rejected in favour of
                // `s.char_at(i)` / `s.bytes()[i]` (design.md § Character type).
                crate::typechecker::TypeErrorKind::StringNotIndexable => "E0268",
                // B-2026-06-30-3 — reassignment of a non-`mut` field on a
                // `shared struct` / `par struct` (design.md § Shared Types).
                crate::typechecker::TypeErrorKind::SharedFieldNotMut => "E0269",
                // An `Atomic[T]` op (`load`/`store`/`fetch_*`/`swap`) called
                // without its required explicit `MemoryOrdering` argument
                // (deferred.md § Atomic Operations — no implicit-ordering form).
                crate::typechecker::TypeErrorKind::AtomicMissingOrdering => "E0270",
                // A return-position `impl Trait` yielding 2+ distinct concrete
                // witnesses (design.md § `impl Trait`: one witness per
                // monomorphization). Run-fatal (B-2026-07-08-1).
                crate::typechecker::TypeErrorKind::ImplTraitMultipleWitnesses => "E0271",
                // FE-2 — a `#[gpu]` function uses a non-GPU-safe type.
                crate::typechecker::TypeErrorKind::GpuNotSafe => "E0801",
            };
            // Also surface a typecheck fix-it as the top-level
            // `"replacement":{offset,length,text}` shape every other phase
            // (resolver/parse/effect/ownership) uses, so `karac fix` and the
            // Mend loop detect typecheck fixes uniformly. The nested
            // `"fix_it"`/`"fixes"` forms below stay for IDE consumers.
            let replacement_json = err.fix_it.as_ref().map(|f| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    f.span.offset,
                    f.span.length,
                    json_string(&f.replacement),
                )
            });
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "typecheck",
                code,
                category: "typecheck",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json: replacement_json,
                lint_name: err.lint_name.as_deref(),
                fix_it: err.fix_it.as_ref(),
                class: Some(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
                expected: err.expected.as_deref(),
                got: err.got.as_deref(),
                stub_hint_json: None,
            });
        }
        for warn in &t.warnings {
            id_counter += 1;
            let code = match warn.kind {
                crate::typechecker::TypeErrorKind::UnreachableArm => "W0237",
                crate::typechecker::TypeErrorKind::RefinementDomainTooWide => "W0238",
                crate::typechecker::TypeErrorKind::UnknownLint => "W0244",
                crate::typechecker::TypeErrorKind::Deprecated => "W0245",
                crate::typechecker::TypeErrorKind::MissingNonExhaustive => "W0246",
                crate::typechecker::TypeErrorKind::UnfulfilledLintExpectation => "W0249",
                crate::typechecker::TypeErrorKind::UnstableApi => "W0255",
                // Other kinds aren't expected to appear as warnings today.
                _ => "W0299",
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "warning",
                phase: "typecheck",
                code,
                category: "typecheck",
                message: &warn.message,
                filename,
                span: &warn.span,
                suggestion: None,
                extra_json: None,
                lint_name: warn.lint_name.as_deref(),
                fix_it: warn.fix_it.as_ref(),
                class: Some(warn.class.map(|c| c.as_str()).unwrap_or("OTHER")),
                expected: warn.expected.as_deref(),
                got: warn.got.as_deref(),
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref e) = pipeline.effects {
        for err in &e.errors {
            id_counter += 1;
            let (code, severity) = match err.kind {
                crate::effectchecker::EffectErrorKind::MissingEffectDeclaration => {
                    ("E0400", "error")
                }
                crate::effectchecker::EffectErrorKind::OverDeclaredEffect => ("E0401", "error"),
                crate::effectchecker::EffectErrorKind::CircularEffectGroup => ("E0402", "error"),
                crate::effectchecker::EffectErrorKind::UndefinedEffectGroup => ("E0403", "error"),
                crate::effectchecker::EffectErrorKind::EffectSubtypeViolation => ("E0404", "error"),
                crate::effectchecker::EffectErrorKind::ProfileViolation => ("E0405", "error"),
                crate::effectchecker::EffectErrorKind::ImplExceedsTraitCeiling => {
                    ("E0230", "error")
                }
                crate::effectchecker::EffectErrorKind::TraitDefaultExceedsCeiling => {
                    ("E0231", "error")
                }
                crate::effectchecker::EffectErrorKind::FfiLintHint => ("L0001", "note"),
                crate::effectchecker::EffectErrorKind::EffectVariableConflict => ("E0406", "error"),
                crate::effectchecker::EffectErrorKind::ProfileIncompatibleEffect => {
                    ("E0407", "error")
                }
                crate::effectchecker::EffectErrorKind::ModuleBindingWriteInPar => {
                    ("E0408", "error")
                }
                crate::effectchecker::EffectErrorKind::PubFnSyntheticResource => ("E0409", "error"),
                crate::effectchecker::EffectErrorKind::ForbiddenEffectInContract => {
                    ("E0410", "error")
                }
                crate::effectchecker::EffectErrorKind::TargetGateViolation => ("E0411", "error"),
                crate::effectchecker::EffectErrorKind::ResourceReceiverContradiction => {
                    ("E0412", "error")
                }
                crate::effectchecker::EffectErrorKind::ExternCUnwindRequiresPanics => {
                    ("E0413", "error")
                }
                crate::effectchecker::EffectErrorKind::ExternExportSuspendsUnsupported => {
                    ("E0414", "error")
                }
                crate::effectchecker::EffectErrorKind::GpuEffectViolation => ("E0802", "error"),
            };
            let subtype_json = err.subtype_trace.as_ref().map(|t| {
                let slot = json_string_list(&t.slot_effects);
                let arg = json_string_list(&t.argument_effects);
                let offending = json_string_list(&t.offending_effects);
                let signature_json = match &t.monomorphized_signature {
                    Some(sig) => format!(",\"signature\":{}", json_string(sig)),
                    None => String::new(),
                };
                format!(
                    "\"effect-subset-fail\":{{\"slot\":{slot},\"argument\":{arg},\"offending\":{offending}{signature_json}}}"
                )
            });
            // Surface the machine-applicable replacement (when present)
            // alongside the structured subtype trace — same payload shape
            // as the resolver/ownership `replacement` field, so `karac
            // fix` and IDE quick-fix consumers handle all three phases
            // uniformly. The two never co-occur today (trace ⇒ E0404,
            // replacement ⇒ E0412) but the merge is future-proof.
            let replacement_json = err.replacement.as_deref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            let extra_json = match (subtype_json, replacement_json) {
                (Some(a), Some(b)) => Some(format!("{a},{b}")),
                (a, b) => a.or(b),
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity,
                phase: "effect",
                code,
                category: "effects",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref o) = pipeline.ownership {
        for err in &o.errors {
            id_counter += 1;
            let code = match err.kind {
                crate::ownership::OwnershipErrorKind::UseAfterMove => "E0500",
                crate::ownership::OwnershipErrorKind::OwnershipCycle => "E0501",
                crate::ownership::OwnershipErrorKind::NoRcViolation => "E0502",
                crate::ownership::OwnershipErrorKind::RcFallbackNote => "N0503",
                crate::ownership::OwnershipErrorKind::CaptureModeViolation => "E0504",
                crate::ownership::OwnershipErrorKind::UseOfUninitialized => "E0505",
                crate::ownership::OwnershipErrorKind::ReassignToImmutable => "E0506",
                crate::ownership::OwnershipErrorKind::UnusedMutCaptureNote => "N0507",
                crate::ownership::OwnershipErrorKind::RefCaptureEscapesScope => "E0508",
                crate::ownership::OwnershipErrorKind::SliceFromTemporaryEscapes => {
                    "E_SLICE_FROM_TEMPORARY_ESCAPES"
                }
                crate::ownership::OwnershipErrorKind::SliceBorrowConflict { .. } => {
                    "E_SLICE_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::CrossBorrowConflict => {
                    "E_CROSS_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::ClosureCaptureBorrowConflict => {
                    "E_CLOSURE_CAPTURE_BORROW_CONFLICT"
                }
                crate::ownership::OwnershipErrorKind::RcBudgetExceeded { .. } => {
                    "E_RC_BUDGET_EXCEEDED"
                }
                crate::ownership::OwnershipErrorKind::ConcurrentSharedStruct { .. } => {
                    "E_CONCURRENT_SHARED_STRUCT"
                }
                crate::ownership::OwnershipErrorKind::ConcurrentPlainStruct { .. } => {
                    "E_CONCURRENT_PLAIN_STRUCT"
                }
                crate::ownership::OwnershipErrorKind::BorrowReturnNotSourcePinned { .. } => "E0509",
                crate::ownership::OwnershipErrorKind::RcFallbackAllocatesUnderFallibleProfile => {
                    "E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE"
                }
                crate::ownership::OwnershipErrorKind::ExclusiveBorrowAliasedArgs => {
                    "E_EXCLUSIVE_BORROW_ALIASED_ARGS"
                }
            };
            let replacement_json = err.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            // Phase-7 line 197 follow-up: multi-edit fix_diff envelope.
            // `ConcurrentSharedStruct` / `ConcurrentPlainStruct` carry
            // their per-mut-field `Mutex[T]` wrap edits in the sibling
            // `error_fix_diffs` map keyed by the diagnostic's primary
            // span. Render as a JSON array `"fix_diff":[{...},{...}]`
            // and splice into the diagnostic's extra_json slot. The
            // single-edit `replacement` and multi-edit `fix_diff` are
            // mutually exclusive in v1 (the new kinds emit
            // `replacement: None`), so either-or is sufficient — when
            // a future kind needs both, this site combines them.
            let fix_diff_json = o
                .error_fix_diffs
                .get(&crate::resolver::SpanKey::from_span(&err.span))
                .filter(|v| !v.is_empty())
                .map(|edits| {
                    let items: Vec<String> = edits
                        .iter()
                        .map(|e| {
                            format!(
                                "{{\"offset\":{},\"length\":{},\"text\":{}}}",
                                e.offset,
                                e.length,
                                json_string(&e.replacement),
                            )
                        })
                        .collect();
                    format!("\"fix_diff\":[{}]", items.join(","))
                });
            let extra_json = match (replacement_json, fix_diff_json) {
                (Some(r), Some(f)) => Some(format!("{r},{f}")),
                (Some(r), None) => Some(r),
                (None, Some(f)) => Some(f),
                (None, None) => None,
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "ownership",
                code,
                category: "ownership",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: err.suggestion.as_deref(),
                extra_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
        for note in &o.notes {
            id_counter += 1;
            let code = match note.kind {
                crate::ownership::OwnershipErrorKind::UnusedMutCaptureNote => "N0507",
                _ => "N0503",
            };
            let replacement_json = note.replacement.as_ref().map(|r| {
                format!(
                    "\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
                    r.offset,
                    r.length,
                    json_string(&r.replacement),
                )
            });
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "note",
                phase: "ownership",
                code,
                category: "ownership",
                message: &note.message,
                filename,
                span: &note.span,
                suggestion: note.suggestion.as_deref(),
                extra_json: replacement_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref esc) = pipeline.provider_escape {
        for err in esc {
            id_counter += 1;
            let message = err.message();
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "provider_escape",
                code: "E0600",
                category: "provider_escape",
                message: &message,
                filename,
                span: &err.closure_span,
                suggestion: None,
                extra_json: None,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref raii) = pipeline.raii_errors {
        for err in raii {
            id_counter += 1;
            let message = err.message();
            let mut extra_parts: Vec<String> = Vec::new();
            if let Some(ref bs) = err.binding_span {
                extra_parts.push(format!(
                    "\"binding_span\":{{{}}}",
                    span_to_json(bs, filename)
                ));
            }
            if let Some(ref sv) = err.state_violation {
                extra_parts.push(format!(
                    "\"state_violation\":{{\"soiling_method\":{},\"clear_method_name\":{},\"soil_span\":{{{}}}}}",
                    json_string(&sv.soiling_method),
                    json_string(&sv.clear_method_name),
                    span_to_json(&sv.soil_span, filename),
                ));
            }
            let extra_json = if extra_parts.is_empty() {
                None
            } else {
                Some(extra_parts.join(","))
            };
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "raii_check",
                code: "E_RAII_ACROSS_YIELD",
                category: "raii_across_yield",
                message: &message,
                filename,
                span: &err.yield_span,
                suggestion: None,
                extra_json,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref simd) = pipeline.simd_errors {
        for err in simd {
            id_counter += 1;
            let message = err.message();
            let help = err.help();
            let func = json_string(&err.func_name);
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "simd_check",
                code: "E_REQUIRE_SIMD",
                category: "require_simd",
                message: &message,
                filename,
                span: &err.span,
                suggestion: Some(&help),
                extra_json: Some(format!("\"function\":{func}")),
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    if let Some(ref comptime) = pipeline.comptime_errors {
        for err in comptime {
            id_counter += 1;
            diags.add(DiagEntry {
                id: &format!("d{id_counter}"),
                severity: "error",
                phase: "comptime",
                code: "E_COMPTIME",
                category: "comptime",
                message: &err.message,
                filename,
                span: &err.span,
                suggestion: None,
                extra_json: None,
                lint_name: None,
                fix_it: None,
                class: None,
                expected: None,
                got: None,
                stub_hint_json: None,
            });
        }
    }

    diags
}

fn program_effects_json(pipeline: &Pipeline) -> String {
    match &pipeline.effects {
        Some(effects) => {
            // Collect all effects from main() or program-level
            let mut all_effects: Vec<String> = Vec::new();
            if let Some(main_effects) = effects.inferred_effects.get("main") {
                for te in &main_effects.effects {
                    all_effects.push(format!(
                        "{}({})",
                        effect_verb_str(&te.effect.verb),
                        te.effect.resource
                    ));
                }
            }
            if all_effects.is_empty() {
                "[]".to_string()
            } else {
                json_string_list(&all_effects)
            }
        }
        None => "null".to_string(),
    }
}

fn public_function_effects_json(pipeline: &Pipeline) -> String {
    let Some(effects) = &pipeline.effects else {
        return "{}".to_string();
    };
    let mut names: Vec<&String> = effects
        .function_visibility
        .iter()
        .filter_map(|(n, is_pub)| {
            if *is_pub && n != "main" {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    names.sort();
    if names.is_empty() {
        return "{}".to_string();
    }
    let entries: Vec<String> = names
        .iter()
        .map(|name| {
            let list: Vec<String> = effects
                .inferred_effects
                .get(*name)
                .map(|set| {
                    set.effects
                        .iter()
                        .map(|te| {
                            format!(
                                "{}({})",
                                effect_verb_str(&te.effect.verb),
                                te.effect.resource
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("{}:{}", json_string(name), json_string_list(&list))
        })
        .collect();
    format!("{{{}}}", entries.join(","))
}

fn mutual_recursion_groups_json(pipeline: &Pipeline) -> String {
    match &pipeline.effects {
        Some(effects) => {
            if effects.mutual_recursion_groups.is_empty() {
                return "[]".to_string();
            }
            let groups: Vec<String> = effects
                .mutual_recursion_groups
                .iter()
                .map(|g| {
                    let funcs = json_string_list(&g.functions);
                    let traces: Vec<String> = g
                        .resolution_trace
                        .iter()
                        .map(|r| {
                            format!(
                                "{{\"call_site\":\"{}:{}\",\"resolved_via\":{},\"effect\":{}}}",
                                r.call_site_function,
                                r.call_site_line,
                                json_string(&r.resolved_via),
                                json_string(&r.effect),
                            )
                        })
                        .collect();
                    format!(
                        "{{\"functions\":{},\"resolution_trace\":[{}]}}",
                        funcs,
                        traces.join(","),
                    )
                })
                .collect();
            format!("[{}]", groups.join(","))
        }
        None => "[]".to_string(),
    }
}

/// Render a `crate::unsafe_lint::LintDiagnostic` in rustc-style format:
/// the primary line plus optional `= note:` and `= help:` continuation
/// lines. The `note:` carries the conceptual explanation (e.g. the two
/// distinct roles of `unsafe`); the `help:` carries the actionable
/// suggestion (wrap in `unsafe { ... }` and add a `// Safety:` comment).
fn render_unsafe_lint_diag(diag: &crate::unsafe_lint::LintDiagnostic, filename: &str) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::unsafe_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

/// Render a `crate::must_use_lint::LintDiagnostic` in the same
/// rustc-style three-piece shape (primary / `= note:` / `= help:`) as
/// `render_unsafe_lint_diag`. Kept parallel rather than unified because
/// each lint module currently owns its own `LintDiagnostic` struct (the
/// pre-existing pattern across `unsafe_lint`, `logical_lint`,
/// `ffi_lint`); a future lint-registry refactor (`docs/implementation_
/// checklist/phase-5-diagnostics.md` § "Lint level attributes") would
/// unify these.
fn render_must_use_lint_diag(diag: &crate::must_use_lint::LintDiagnostic, filename: &str) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::must_use_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

/// Render a `crate::missing_must_use_lint::LintDiagnostic` in the same
/// rustc-style three-piece shape. Structurally identical to
/// `render_must_use_lint_diag` — the two `LintDiagnostic` types share
/// shape but live in separate modules to keep each lint self-contained
/// (the established pattern across `unsafe_lint`, `must_use_lint`,
/// `logical_lint`, `ffi_lint`). A future lint-registry refactor (per
/// the deferred "Lint level attributes" entry in
/// `phase-5-diagnostics.md`) would unify these renderers.
fn render_missing_must_use_lint_diag(
    diag: &crate::missing_must_use_lint::LintDiagnostic,
    filename: &str,
) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::missing_must_use_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

fn render_missing_track_caller_lint_diag(
    diag: &crate::missing_track_caller_lint::LintDiagnostic,
    filename: &str,
) {
    eprintln!(
        "{}[{}]: {}:{}:{}: {}",
        if diag.level == crate::missing_track_caller_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        },
        diag.lint_name,
        filename,
        diag.span.line,
        diag.span.column,
        diag.message
    );
    if let Some(note) = &diag.note {
        eprintln!("   = note: {note}");
    }
    if let Some(help) = &diag.help {
        eprintln!("   = help: {help}");
    }
}

fn emit_json_output(pipeline: &Pipeline) {
    let diags = collect_diagnostics(pipeline);
    let effects = program_effects_json(pipeline);
    let pub_effects = public_function_effects_json(pipeline);
    let mrg = mutual_recursion_groups_json(pipeline);
    println!(
        "{{\"program_effects\":{},\"public_function_effects\":{},\"mutual_recursion_groups\":{},\"diagnostics\":{}}}",
        effects,
        pub_effects,
        mrg,
        diags.to_json_array()
    );
}

// ── JSONL Streaming Output ──────────────────────────────────────

fn emit_jsonl_event(event_type: &str, fields: &str) {
    println!("{{\"type\":{},{}}}", json_string(event_type), fields);
}

fn run_pipeline_jsonl(pipeline: &mut Pipeline) {
    let filename = &pipeline.filename.clone();

    // build_start
    emit_jsonl_event(
        "build_start",
        &format!("\"timestamp\":\"\",\"files\":[{}]", json_string(filename)),
    );

    // lex phase (already done during parse)
    emit_jsonl_event(
        "phase_start",
        &format!(
            "\"phase\":\"lex\",\"scope\":{{\"files\":[{}]}}",
            json_string(filename)
        ),
    );
    emit_jsonl_event(
        "phase_complete",
        "\"phase\":\"lex\",\"errors\":0,\"warnings\":0,\"notes\":0",
    );

    // parse phase
    emit_jsonl_event("phase_start", "\"phase\":\"parse\"");
    let parse_errors = pipeline.parsed.errors.len();
    if parse_errors > 0 {
        let diags = collect_diagnostics(pipeline);
        for entry in &diags.entries {
            // Re-emit parse diagnostics as streaming events
            println!("{entry}");
        }
    }
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"parse\",\"errors\":{},\"warnings\":0,\"notes\":0",
            parse_errors
        ),
    );

    if pipeline.has_parse_errors() {
        // Skip remaining phases
        for phase in &["resolve", "typecheck", "effect", "ownership"] {
            emit_jsonl_event(
                "phase_skipped",
                &format!(
                    "\"phase\":{},\"reason\":\"parse errors in input\",\"blocking\":[\"d1\"]",
                    json_string(phase)
                ),
            );
        }
        emit_jsonl_event(
            "build_complete",
            &format!(
                "\"success\":false,\"total_errors\":{},\"total_warnings\":0,\"program_effects\":null",
                parse_errors
            ),
        );
        return;
    }

    // resolve phase
    emit_jsonl_event("phase_start", "\"phase\":\"resolve\"");
    pipeline.resolve();
    let resolve_errors = pipeline.resolved.as_ref().map_or(0, |r| r.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"resolve\",\"errors\":{},\"warnings\":0,\"notes\":0",
            resolve_errors
        ),
    );

    if pipeline.has_resolve_errors() {
        for phase in &["typecheck", "effect", "ownership"] {
            emit_jsonl_event(
                "phase_skipped",
                &format!(
                    "\"phase\":{},\"reason\":\"resolve errors in input\",\"blocking\":[]",
                    json_string(phase)
                ),
            );
        }
        let total = parse_errors + resolve_errors;
        emit_jsonl_event(
            "build_complete",
            &format!(
                "\"success\":false,\"total_errors\":{},\"total_warnings\":0,\"program_effects\":null",
                total
            ),
        );
        return;
    }

    // typecheck phase
    emit_jsonl_event("phase_start", "\"phase\":\"typecheck\"");
    pipeline.typecheck();
    pipeline.lower();
    let type_errors = pipeline.typed.as_ref().map_or(0, |t| t.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"typecheck\",\"errors\":{},\"warnings\":0,\"notes\":0",
            type_errors
        ),
    );

    // effect phase
    emit_jsonl_event("phase_start", "\"phase\":\"effect\"");
    pipeline.effectcheck();
    let (effect_errors, effect_notes) = pipeline.effects.as_ref().map_or((0, 0), |e| {
        let errors = e
            .errors
            .iter()
            .filter(|e| e.kind != EffectErrorKind::FfiLintHint)
            .count();
        let notes = e
            .errors
            .iter()
            .filter(|e| e.kind == EffectErrorKind::FfiLintHint)
            .count();
        (errors, notes)
    });
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"effect\",\"errors\":{},\"warnings\":0,\"notes\":{}",
            effect_errors, effect_notes
        ),
    );

    // ownership phase
    emit_jsonl_event("phase_start", "\"phase\":\"ownership\"");
    pipeline.ownershipcheck();
    let ownership_errors = pipeline.ownership.as_ref().map_or(0, |o| o.errors.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"ownership\",\"errors\":{},\"warnings\":0,\"notes\":0",
            ownership_errors
        ),
    );

    // provider escape phase
    emit_jsonl_event("phase_start", "\"phase\":\"provider_escape\"");
    pipeline.provider_escape_check();
    let escape_errors = pipeline.provider_escape.as_ref().map_or(0, |e| e.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"provider_escape\",\"errors\":{},\"warnings\":0,\"notes\":0",
            escape_errors
        ),
    );

    // RAII-across-yield phase (phase 6 line 31 slice 1)
    emit_jsonl_event("phase_start", "\"phase\":\"raii_check\"");
    pipeline.raii_check();
    let raii_errors = pipeline.raii_errors.as_ref().map_or(0, |r| r.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"raii_check\",\"errors\":{},\"warnings\":0,\"notes\":0",
            raii_errors
        ),
    );

    // `#[require_simd]` guarantee phase (phase-7-codegen.md line 308 slice 5a)
    emit_jsonl_event("phase_start", "\"phase\":\"simd_check\"");
    pipeline.simd_check();
    let simd_errors = pipeline.simd_errors.as_ref().map_or(0, |s| s.len());
    emit_jsonl_event(
        "phase_complete",
        &format!(
            "\"phase\":\"simd_check\",\"errors\":{},\"warnings\":0,\"notes\":0",
            simd_errors
        ),
    );

    let comptime_errors = pipeline.comptime_errors.as_ref().map_or(0, |c| c.len());
    let total = parse_errors
        + resolve_errors
        + type_errors
        + effect_errors
        + ownership_errors
        + escape_errors
        + raii_errors
        + simd_errors
        + comptime_errors;
    let effects = program_effects_json(pipeline);
    emit_jsonl_event(
        "build_complete",
        &format!(
            "\"success\":{},\"total_errors\":{},\"total_warnings\":0,\"program_effects\":{}",
            total == 0,
            total,
            effects,
        ),
    );
}

// ── Commands ────────────────────────────────────────────────────

fn format_error_trace_json(frames: &[ErrorTraceFrame], truncated: bool) -> String {
    let entries: Vec<String> = frames
        .iter()
        .map(|f| {
            format!(
                "{{\"file\":{},\"line\":{},\"column\":{}}}",
                json_string(&f.file),
                f.line,
                f.column,
            )
        })
        .collect();
    if truncated {
        format!("{{\"frames\":[{}],\"truncated\":true}}", entries.join(","))
    } else {
        format!("[{}]", entries.join(","))
    }
}

fn cmd_run_example(
    name: &str,
    output: OutputMode,
    sequential: bool,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Try single-file form first, then project-style directory form.
    let single_file = format!("examples/{name}.kara");
    let dir_entry = format!("examples/{name}/src/main.kara");
    let path = if std::path::Path::new(&single_file).exists() {
        single_file
    } else if std::path::Path::new(&dir_entry).exists() {
        dir_entry
    } else {
        eprintln!("error: example '{name}' not found");
        eprintln!("  looked for: {single_file}");
        eprintln!("              {dir_entry}");
        list_available_examples();
        process::exit(1);
    };
    // `karac run --example NAME` runs an example file out of the
    // examples/ directory; it has no `kara.toml`-style project root,
    // so manifest discovery is intentionally skipped.
    // `interp = false`: `run --example` uses the JIT-default backend too (6c),
    // with the same `--interp`/`KARAC_RUN_JIT=0` escape hatches honored inside.
    cmd_run(
        &path,
        output,
        sequential,
        None,
        true,
        lint_overrides,
        None,
        false,
    );
}

fn list_available_examples() {
    let names = walker::walk_examples(std::path::Path::new("."));
    if names.is_empty() {
        return;
    }
    eprintln!("available examples:");
    for n in &names {
        eprintln!("  {n}");
    }
}

/// Whether the program's `fn main` declares a `-> ExitCode` return type
/// (Phase-8 entry-point contract Slice B). The interpreter is
/// type-erased — a returned `ExitCode` is an ordinary `Value::Int` —
/// so `cmd_run` consults the AST signature to decide whether `main`'s
/// returned integer is a process exit code. Per design.md § Entry Point.
fn main_return_is_exitcode(program: &Program) -> bool {
    program.items.iter().any(|item| match item {
        Item::Function(f) if f.name == "main" => matches!(
            f.return_type.as_ref().map(|t| &t.kind),
            Some(crate::ast::TypeKind::Path(p))
                if p.segments.len() == 1 && p.segments[0] == "ExitCode"
        ),
        _ => false,
    })
}

/// Merge a multi-module project's `ProgramTree` into a single `Program` for
/// the interpreter — the `run`-side analog of `run_multi_file_codegen`'s
/// super-program build: items concatenated in topological (dependency-first)
/// order, dropping `import` declarations (resolved upstream) and synthetic
/// modules, plus gated-stdlib import expansions. No `ModuleSpanTable` — that is
/// a codegen late-phase-diagnostic concern; the lenient `run` path doesn't need
/// it. Kept separate from `run_multi_file_codegen` so the codegen path is
/// untouched.
fn build_super_program_for_run(tree: &ProgramTree) -> Program {
    let order = module::emission_order(tree);
    let mut items: Vec<Item> = Vec::new();
    for &id in &order {
        let m = &tree.modules[id];
        if m.is_synthetic {
            continue;
        }
        for item in &m.items {
            if matches!(item, Item::Import(_)) {
                continue;
            }
            items.push(item.clone());
        }
    }
    // Gated baked-stdlib modules are synthetic, so the loop above skips them;
    // append the expansion of every gated import (deduped on the bound name),
    // mirroring `run_multi_file_codegen`.
    {
        let mut seen: std::collections::HashSet<(Vec<String>, String)> =
            std::collections::HashSet::new();
        for m in &tree.modules {
            if m.is_synthetic {
                continue;
            }
            for imp in &m.imports {
                let deduped: Vec<crate::ast::ImportItem> = imp
                    .items
                    .iter()
                    .filter(|ii| {
                        let bound = ii.alias.as_ref().unwrap_or(&ii.name);
                        seen.insert((imp.path.clone(), bound.clone()))
                    })
                    .cloned()
                    .collect();
                if let Some(expansion) = crate::prelude::gated_items_for_import(&imp.path, &deduped)
                {
                    items.extend(expansion);
                }
            }
        }
    }
    Program {
        items,
        ..Program::default()
    }
}

/// Best-effort dependency walks for the lenient `karac run` path: resolve
/// the manifest's `[dependencies]` and walk each path-dep, returning an
/// empty list on any failure (no diagnostics — the strict build path owns
/// error reporting, and `run`'s resolver pass surfaces unknown-module
/// diagnostics naturally when dep modules are absent).
fn quiet_dep_package_walks(root: &std::path::Path) -> Vec<module::DepPackageWalk> {
    let Ok(mf) = manifest::load_from_root(root) else {
        return Vec::new();
    };
    if mf.dependencies.is_empty() {
        return Vec::new();
    }
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root: None,
        include_dev_deps: false,
        // The lenient `karac run` walk stays path-dep-only by design: it is
        // best-effort (empty on any failure) and must not perform network I/O.
        // Registry and git fetch are activated on the strict `karac build` /
        // `karac test` path (`run_dep_resolution`); a registry or git dep here
        // still surfaces its unsupported diagnostic from the resolver, which
        // this quiet walk swallows.
        registry_provider: None,
        git_provider: None,
        // Path-dep-only lenient walk — no lockfile pinning here.
        pins: None,
    };
    let Ok(graph) = crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options)
    else {
        return Vec::new();
    };
    let active = crate::dep_resolver::active_toolchain_version();
    let Ok(resolution) = crate::dep_resolver::resolve(&graph, &active) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pkg in resolution.packages.values() {
        let crate::dep_resolver::ResolvedSource::Path(dep_root) = &pkg.source else {
            continue;
        };
        let Ok(walked) = walker::walk_project(dep_root, WalkerOpts::default()) else {
            return Vec::new();
        };
        if walked.entry != walker::EntryKind::Lib {
            return Vec::new();
        }
        out.push(module::DepPackageWalk {
            name: pkg.name.clone(),
            walked,
        });
    }
    out
}

/// If `filename` is the entry of a multi-module project, build the merged
/// super-program so `karac run` sees every sibling module's items (GAP-W3 —
/// previously the interpreter only registered the entry file's items, so
/// cross-module calls failed at runtime despite resolving + typechecking).
/// Returns `None` for a single-file script or a one-module project (the caller
/// keeps the single-file fast path), so behavior is unchanged outside real
/// multi-module projects. Canonicalizes the entry first so the canonical
/// invocation `karac run src/main.kara` (relative path) discovers the root —
/// `discover_project_root` can't walk up a bare relative `src`.
fn try_build_run_super_program(filename: &str, no_manifest: bool) -> Option<Program> {
    if no_manifest {
        return None; // operator opted out of project/manifest discovery
    }
    let entry = std::fs::canonicalize(filename).ok()?;
    let root = manifest::discover_project_root(entry.parent()?)?;
    // A `walk_project` error here (e.g. mixed `main.kara` + `lib.kara` entry
    // files) is not ours to report on the run path — fall back to single-file
    // and let the normal flow surface any diagnostic.
    let walked = walker::walk_project(&root, WalkerOpts::default()).ok()?;
    // Cross-package module loading (phase-5 line 898): merge resolved
    // path-deps' modules so the interpreter sees imported dep items. Same
    // lenient posture as the rest of this helper — any dep-resolution or
    // dep-walk failure just proceeds without dependency modules, and the
    // resolver surfaces its usual diagnostics downstream.
    let dep_walks = quiet_dep_package_walks(&root);
    let built =
        module::build_program_tree_with_deps(&walked, &dep_walks, module::BuildTreeOpts::default())
            .ok()?;
    // A clean tree is required — fall back to the single-file path (which will
    // surface the parse error against the entry file) if any module failed to
    // parse.
    if !built.parse_errors.is_empty() {
        return None;
    }
    let tree = built.tree;
    let non_synthetic = tree.modules.iter().filter(|m| !m.is_synthetic).count();
    if non_synthetic <= 1 {
        return None; // single-module project — single-file path is equivalent
    }
    // Only merge when the entry file is actually part of this project's tree;
    // otherwise the super-program could be missing the entry's `main`.
    let entry_in_tree = tree.modules.iter().filter(|m| !m.is_synthetic).any(|m| {
        std::fs::canonicalize(&m.file)
            .map(|p| p == entry)
            .unwrap_or(false)
    });
    if !entry_in_tree {
        return None;
    }
    Some(build_super_program_for_run(&tree))
}

/// LLJIT Slice 6b: run a codegen-emitted IR module through the
/// `karac_jit_runner` one-shot subprocess and return its exit code. The
/// runner JIT-compiles the module and calls `main`; its stdio is INHERITED
/// (not captured) so the program's output flows straight to the user's
/// terminal, and its `main`-return / `emit_panic` exit code propagates back —
/// giving `karac run` the same execution + fault + exit semantics as a built
/// binary. Mirrors the machinery `karac test` already uses (proven at
/// 2084/2084 codegen-E2E-via-JIT parity), but one-shot rather than batched.
#[cfg(feature = "llvm")]
fn run_ir_via_jit_subprocess(ir: &str) -> i32 {
    let ir_path = std::env::temp_dir().join(format!("karac_run_{}_jit.ll", std::process::id()));
    if let Err(e) = std::fs::write(&ir_path, ir) {
        eprintln!(
            "error: could not write JIT IR to {}: {e}",
            ir_path.display()
        );
        return 1;
    }
    let runner = match crate::test_jit_dispatch::locate_karac_jit_runner() {
        Some(p) => p,
        None => {
            eprintln!(
                "error: karac_jit_runner not found — set KARAC_JIT_RUNNER, or install \
                 karac with --features llvm (the runner ships beside the karac binary)"
            );
            let _ = std::fs::remove_file(&ir_path);
            return 1;
        }
    };
    // `.status()` inherits stdin/stdout/stderr, so the JIT'd program writes
    // straight to the user's terminal and its exit code is the run's exit code.
    let status = std::process::Command::new(&runner).arg(&ir_path).status();
    let _ = std::fs::remove_file(&ir_path);
    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("error: could not spawn karac_jit_runner: {e}");
            1
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_run(
    filename: &str,
    output: OutputMode,
    sequential: bool,
    manifest_override: Option<&str>,
    no_manifest: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    timeout: Option<std::time::Duration>,
    interp: bool,
) {
    // Mutual exclusion at the entry point — both flags together would
    // be ambiguous (which wins?). Reject early so the operator gets a
    // clear diagnostic rather than a silent precedence rule.
    if manifest_override.is_some() && no_manifest {
        eprintln!("error: --manifest and --no-manifest are mutually exclusive");
        process::exit(1);
    }

    // Script-dir manifest discovery (tracker line 898). Unless
    // `--no-manifest` is set, walk upward from the script's own
    // directory looking for `kara.toml`. The discovered manifest's
    // `[package].profile` becomes the pipeline's active profile so
    // running a script that lives inside an embedded/kernel project
    // honors the project's compile profile. A `karac-toolchain.toml`
    // pin in the same ancestor chain is enforced here too.
    let script_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let discovered_manifest: Option<manifest::Manifest> = if no_manifest {
        None
    } else if let Some(explicit) = manifest_override {
        let path = std::path::PathBuf::from(explicit);
        match std::fs::read_to_string(&path) {
            Ok(src) => match manifest::parse_manifest(&path, &src) {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_manifest_error(&e, output);
                    process::exit(1);
                }
            },
            Err(e) => {
                eprintln!(
                    "error: cannot read `{}` for --manifest override: {}",
                    path.display(),
                    e
                );
                process::exit(1);
            }
        }
    } else {
        // Walk upward from the script's directory. Treat a missing
        // manifest as "stdlib-only" — single-file scripts often run
        // outside any project, and the pre-line-898 behavior was to
        // not consult a manifest at all.
        match manifest::discover_project_root(&script_dir) {
            Some(root) => match manifest::load_from_root(&root) {
                Ok(m) => Some(m),
                Err(e) => {
                    emit_manifest_error(&e, output);
                    process::exit(1);
                }
            },
            None => None,
        }
    };

    // Toolchain pin enforcement (tracker line 892) runs from the
    // script-dir ancestor chain. Skipped when --no-manifest is set
    // (the operator explicitly opted out of project-level gating).
    if !no_manifest && !enforce_toolchain_pin(&script_dir, output) {
        process::exit(1);
    }

    // Resolver follow-up (m), run slice: surface dependency-resolution
    // diagnostics before executing, so a broken dep graph (cycle / version
    // conflict / MSRV / missing path-dep / workspace-deref) fails `karac run`
    // exactly as it fails `check` / `build` — instead of the lenient path
    // silently swallowing the resolver's finding (via `quiet_dep_package_walks`)
    // and running anyway. Path-dep-only (no network), same policy as `check`:
    // registry/git deps stay lenient (unsupported findings skipped). Scoped to
    // the normal project-discovery case — `--no-manifest` opts out (as does
    // `karac run --example`, which passes it), and a `--manifest` override is a
    // single-file-script mode where project dep resolution doesn't apply.
    if !no_manifest && manifest_override.is_none() {
        if let (Some(root), Some(mf)) = (
            manifest::discover_project_root(&script_dir),
            discovered_manifest.as_ref(),
        ) {
            let mf = manifest::merge_target_overlay(mf, Some(&default_resolution_target(mf)));
            let has_deps = !mf.dependencies.is_empty()
                || !mf.dev_dependencies.is_empty()
                || mf.kara_version.is_some();
            if has_deps && !surface_dep_graph_diagnostics(&root, mf, output) {
                process::exit(1);
            }
        }
    }

    let source = read_source(filename);
    let mut lint_overrides = lint_overrides;
    if let Some(ref m) = discovered_manifest {
        lint_overrides.apply_manifest_lints(&m.lints);
    }
    let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
    if let Some(ref m) = discovered_manifest {
        pipeline.profile = m.profile;
        pipeline.profile_config = m.profile_config.clone();
    }
    // Multi-module project support (GAP-W3, examples/db_pipeline shape): when
    // the entry file belongs to a discoverable project that has sibling
    // modules, replace the single-file program with the merged super-program
    // so the resolver / typechecker / interpreter see every module's items.
    // Before this, `karac run src/main.kara` registered only the entry file's
    // items, so cross-module free *and* associated calls failed at runtime
    // even though they resolved + typechecked. No-op for single-file scripts
    // and one-module projects (`try_build_run_super_program` returns `None`).
    if let Some(super_program) = try_build_run_super_program(filename, no_manifest) {
        pipeline.parsed.program = super_program;
    }
    pipeline.resolve();

    if pipeline.has_fatal_errors() {
        match output {
            OutputMode::Text => {
                print_text_diagnostics(&pipeline);
                process::exit(1);
            }
            OutputMode::Json => {
                emit_json_output(&pipeline);
                process::exit(1);
            }
            OutputMode::Jsonl => {
                run_pipeline_jsonl(&mut pipeline);
                process::exit(1);
            }
        }
    }

    // Type-check. Post-Slice-6 (run-leniency stripped) any type error is
    // fatal for `karac run`, gated below — matching `check`/`build`.
    pipeline.typecheck();
    pipeline.lower();
    // Effect-check. Post-Slice-6 hard effect errors are fatal for `karac run`
    // (gated below), same as `check`/`build` — the phase-10 downgrade-to-
    // `warning[effect]` leniency is gone. Running the pass here is still
    // load-bearing for two consumers that read its outputs on this path:
    // `raii_check` (keys off `Program.state_struct_layouts` / `yield_points`,
    // populated by `Pipeline::effectcheck` — without this call the run-path
    // RAII gate below was vacuously green) and the `missing_track_caller`
    // lint (reads `pipeline.effects`). FFI lint *hints* stay advisory notes.
    pipeline.effectcheck();

    // Comptime fold failures are run-fatal even on the lenient script path.
    // A `comptime { ... }` block that panicked / overran its ceiling / had a
    // non-foldable result was left un-spliced; the interpreter's defensive
    // `Comptime` arm would re-evaluate it at runtime and either fault again
    // or run effectful code at the wrong phase. Like the run-fatal type-error
    // gate just below, this is an execution-soundness violation: abort rather
    // than warn. (`comptime_errors` is populated by `lower()` above.)
    if pipeline.has_fatal_comptime_errors() {
        if let Some(ref comptime) = pipeline.comptime_errors {
            match output {
                OutputMode::Text => {
                    for err in comptime {
                        eprintln!(
                            "error[comptime]: {}:{}:{}: {}",
                            filename, err.span.line, err.span.column, err.message
                        );
                    }
                }
                OutputMode::Json => emit_json_output(&pipeline),
                OutputMode::Jsonl => {
                    for err in comptime {
                        emit_jsonl_event(
                            "diagnostic",
                            &format!(
                                "\"severity\":\"error\",\"phase\":\"comptime\",{},\"message\":{}",
                                span_to_json(&err.span, filename),
                                json_string(&err.message),
                            ),
                        );
                    }
                }
            }
        }
        process::exit(1);
    }

    // LLJIT Slice 6 — run-leniency STRIPPED. `karac run` now rejects the same
    // static-contract violations `karac check` / `karac build` reject: ANY
    // type error and any hard effect error (FfiLintHint notes excepted) abort
    // the run instead of downgrading to `warning[...]` and executing. This
    // collapses the run/build *acceptance* divergence that was the epic's
    // headline tax — the phase-10 run-leniency decision (2026-06-06, "static
    // contracts warn on the lenient script path") is superseded by the
    // 2026-07-06 LLJIT-productionization owner decision (see
    // docs/spikes/lljit-productionization.md § Slice 6). The blast radius was
    // measured first (examples/ + kara-katas + examples/mend sweep, 0 breaks
    // after fixes — docs/spikes/lljit-slice6-leniency-sweep.md), never stripped
    // blind. `TypeErrorKind::is_run_fatal` is now vestigial for this path —
    // the run gate no longer filters by it (every type error is fatal, so the
    // old invalid-cast-only gate, B-2026-06-13-15, is subsumed). The classifier
    // is kept as public API pinned by typechecker tests that document which
    // kinds are value-corrupting; it no longer gates `karac run`. Execution-
    // soundness gates (comptime above; provider escape, RAII below) and
    // ownership keep their own handling.
    // A run-fatal effect error is any hard finding EXCEPT the two advisory
    // classes that stay lenient by design:
    //   - `FfiLintHint`  — a `note[effect]` lint, never an error.
    //   - `TargetGateViolation` (E0411) — a *target-availability* finding, not
    //     a correctness bug. Running a `std.web` program on the `native`
    //     target with its web resources stubbed is a deliberate cross-target
    //     dev workflow (`karac run webby.kara` to exercise logic locally); it
    //     stays a `warning[effect]` and executes. `build`/`check` treat it the
    //     same on native, so this is not a run/build divergence — Slice 6
    //     strips *correctness* leniency, not portability affordances.
    let is_fatal_effect = |k: &EffectErrorKind| {
        !matches!(
            k,
            EffectErrorKind::FfiLintHint | EffectErrorKind::TargetGateViolation
        )
    };
    let has_type_errs = pipeline.has_type_errors();
    let has_effect_errs = pipeline
        .effects
        .as_ref()
        .is_some_and(|e| e.errors.iter().any(|er| is_fatal_effect(&er.kind)));
    if has_type_errs || has_effect_errs {
        match output {
            OutputMode::Text => {
                if let Some(ref t) = pipeline.typed {
                    for err in &t.errors {
                        eprintln!(
                            "error[typecheck]: {}:{}:{}: {}",
                            filename, err.span.line, err.span.column, err.message
                        );
                    }
                }
                if let Some(ref e) = pipeline.effects {
                    for err in e.errors.iter().filter(|er| is_fatal_effect(&er.kind)) {
                        eprintln!(
                            "error[effect]: {}:{}:{}: {}",
                            filename, err.span.line, err.span.column, err.message
                        );
                    }
                }
            }
            OutputMode::Json => emit_json_output(&pipeline),
            OutputMode::Jsonl => {
                if let Some(ref t) = pipeline.typed {
                    for err in &t.errors {
                        emit_jsonl_event(
                            "diagnostic",
                            &format!(
                                "\"severity\":\"error\",\"phase\":\"typecheck\",{},\"message\":{}",
                                span_to_json(&err.span, filename),
                                json_string(&err.message),
                            ),
                        );
                    }
                }
                if let Some(ref e) = pipeline.effects {
                    for err in e.errors.iter().filter(|er| is_fatal_effect(&er.kind)) {
                        emit_jsonl_event(
                            "diagnostic",
                            &format!(
                                "\"severity\":\"error\",\"phase\":\"effect\",{},\"message\":{}",
                                span_to_json(&err.span, filename),
                                json_string(&err.message),
                            ),
                        );
                    }
                }
            }
        }
        process::exit(1);
    }

    if output == OutputMode::Text {
        // LLJIT Slice 6: with correctness leniency stripped, hard type + effect
        // errors already aborted the run above. Two advisory effect classes
        // survive on this path and stay warnings/notes (they don't gate
        // execution): `TargetGateViolation` (E0411) — the cross-target
        // "run std.web on native with stubbed resources" affordance — prints
        // `warning[effect]`; FFI lint hints keep their `note[effect]` severity.
        if let Some(ref e) = pipeline.effects {
            for err in &e.errors {
                match err.kind {
                    EffectErrorKind::TargetGateViolation => eprintln!(
                        "warning[effect]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    EffectErrorKind::FfiLintHint => eprintln!(
                        "note[effect]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    _ => {}
                }
            }
        }
        // Lint: undocumented_unsafe
        for diag in crate::unsafe_lint::check_undocumented_unsafe(
            &pipeline.parsed.program,
            &source,
            &pipeline.lint_overrides,
        ) {
            render_unsafe_lint_diag(&diag, filename);
        }
        // Lint: unsafe_op_in_unsafe_fn (slice 3) — walks every fn body
        // and rejects raw-pointer deref / unsafe-fn calls outside an
        // `unsafe { }` block. Runs post-typecheck because raw-ptr deref
        // detection consults `expr_types` and method-call dispatch reads
        // `method_callee_types`.
        for diag in crate::unsafe_lint::check_unsafe_op_in_unsafe_fn(
            &pipeline.parsed.program,
            pipeline.typed.as_ref(),
        ) {
            render_unsafe_lint_diag(&diag, filename);
        }
        // Lint: must_use (slice 1 — implicit `#[must_use]` for the two
        // language-level types `Result[T, E]` and `Option[T]`). Walks
        // every fn body and warns on discarded values of either type at
        // statement position. Needs typecheck info to recognise the
        // types from `expr_types`.
        for diag in crate::must_use_lint::check_implicit_must_use(
            &pipeline.parsed.program,
            pipeline.typed.as_ref(),
            &pipeline.lint_overrides,
        ) {
            render_must_use_lint_diag(&diag, filename);
        }
        // Lint: missing_must_use (slice 3 — stdlib-hygiene). Warns on
        // baked stdlib `pub fn` returning iterator-shaped or
        // new-value-from-self values that lack `#[must_use]`. Silent
        // on user code by design (the lint walks `stdlib_origin == true`
        // items only). Today end-user compiles see no output from this
        // pass because baked stdlib items aren't spliced into the user
        // program AST; the lint surfaces during karac's own stdlib-
        // hygiene tests (`tests/missing_must_use_lint.rs`) and during
        // any future bundled-stdlib-source compile mode.
        for diag in crate::missing_must_use_lint::check_missing_must_use(
            &pipeline.parsed.program,
            &pipeline.lint_overrides,
        ) {
            render_missing_must_use_lint_diag(&diag, filename);
        }
        // Lint: missing_track_caller (slice 7 of the `#[track_caller]` for
        // stdlib panic-emitters entry). Reads the effect-checker's
        // `inferred_effects` map plus each function's declared `panics`
        // effect to identify stdlib `pub fn`s that panic without
        // `#[track_caller]`. Pre-codegen-slice-4 surface: the lint fires
        // even though the codegen pass doesn't yet propagate the
        // attribute — the slice-6 annotation pass will drive this lint
        // clean and surface every missing-attribute site mechanically.
        for diag in crate::missing_track_caller_lint::check_missing_track_caller(
            &pipeline.parsed.program,
            pipeline.effects.as_ref(),
            &pipeline.lint_overrides,
        ) {
            render_missing_track_caller_lint_diag(&diag, filename);
        }
        // Lint: ffi_float_eq
        for diag in
            crate::ffi_lint::check_ffi_float_eq(&pipeline.parsed.program, &pipeline.lint_overrides)
        {
            let prefix = if diag.level == crate::ffi_lint::LintLevel::Error {
                "error"
            } else {
                "warning"
            };
            eprintln!(
                "{prefix}[ffi_float_eq]: {}:{}:{}: {}",
                filename, diag.span.line, diag.span.column, diag.message
            );
        }
        // Lint: ambiguous_not_comparison
        for diag in crate::logical_lint::check_ambiguous_not_comparison(
            &pipeline.parsed.program,
            &pipeline.lint_overrides,
        ) {
            eprintln!(
                "{}[{}]: {}:{}:{}: {}",
                if diag.level == crate::logical_lint::LintLevel::Error {
                    "error"
                } else {
                    "warning"
                },
                diag.lint_name,
                filename,
                diag.span.line,
                diag.span.column,
                diag.message
            );
        }
        // Lint: malformed_diagnostic_attribute (slice 3 of item 36 —
        // shape + placeholder checks for `#[diagnostic::on_unimplemented]`).
        for diag in crate::diagnostic_attrs_lint::check_diagnostic_attributes(
            &pipeline.parsed.program,
            &pipeline.lint_overrides,
        ) {
            let prefix = if diag.level == crate::diagnostic_attrs_lint::LintLevel::Error {
                "error"
            } else {
                "warning"
            };
            eprintln!(
                "{prefix}[malformed_diagnostic_attribute]: {}:{}:{}: {}",
                filename, diag.span.line, diag.span.column, diag.message
            );
        }
    }

    // Provider-rooted resource escape — a hard error per design.md §
    // Provider-Rooted Resources. Unlike type errors in the interpreter-
    // first path, escape violations break the language's test-isolation
    // and teardown guarantees, so they abort execution rather than
    // downgrade to a warning.
    pipeline.provider_escape_check();
    if let Some(ref esc) = pipeline.provider_escape {
        if !esc.is_empty() {
            match output {
                OutputMode::Text => {
                    for err in esc {
                        eprintln!(
                            "error[provider_escape]: {}:{}:{}: {}",
                            filename,
                            err.closure_span.line,
                            err.closure_span.column,
                            err.message()
                        );
                    }
                }
                OutputMode::Json => emit_json_output(&pipeline),
                OutputMode::Jsonl => {
                    for err in esc {
                        emit_jsonl_event(
                            "diagnostic",
                            &format!(
                                "\"severity\":\"error\",\"phase\":\"provider_escape\",\"code\":\"E0600\",{},\"message\":{}",
                                span_to_json(&err.closure_span, filename),
                                json_string(&err.message()),
                            ),
                        );
                    }
                }
            }
            process::exit(1);
        }
    }

    // RAII-across-yield — phase 6 line 31 slice 1. Same hard-error
    // contract as provider_escape: the network-event-loop state-machine
    // transform can't soundly lower a function that would leak resources
    // under cooperative cancellation, so the run path aborts rather
    // than proceeds to the interpreter. (This gate only became live on
    // the run path with the `effectcheck()` call above — the check keys
    // off `state_struct_layouts`/`yield_points`, which nothing populated
    // here before the phase-10 run-leniency slice.)
    pipeline.raii_check();
    if let Some(ref raii) = pipeline.raii_errors {
        if !raii.is_empty() {
            match output {
                OutputMode::Text => {
                    for err in raii {
                        eprintln!(
                            "error[E_RAII_ACROSS_YIELD]: {}:{}:{}: {}",
                            filename,
                            err.yield_span.line,
                            err.yield_span.column,
                            err.message(),
                        );
                        if let Some(ref bs) = err.binding_span {
                            eprintln!(
                                "  note: binding declared here at {}:{}:{}",
                                filename, bs.line, bs.column,
                            );
                        }
                        if let Some(ref sv) = err.state_violation {
                            eprintln!(
                                "  note: soiled by `.{}()` here at {}:{}:{}",
                                sv.soiling_method, filename, sv.soil_span.line, sv.soil_span.column,
                            );
                        }
                        eprintln!("  help: {}", err.help());
                    }
                }
                OutputMode::Json => emit_json_output(&pipeline),
                OutputMode::Jsonl => {
                    for err in raii {
                        let binding_span_json = err
                            .binding_span
                            .as_ref()
                            .map(|bs| {
                                format!(",\"binding_span\":{{{}}}", span_to_json(bs, filename))
                            })
                            .unwrap_or_default();
                        let state_violation_json = err
                            .state_violation
                            .as_ref()
                            .map(|sv| {
                                format!(
                                    ",\"state_violation\":{{\"soiling_method\":{},\"clear_method_name\":{},\"soil_span\":{{{}}}}}",
                                    json_string(&sv.soiling_method),
                                    json_string(&sv.clear_method_name),
                                    span_to_json(&sv.soil_span, filename),
                                )
                            })
                            .unwrap_or_default();
                        emit_jsonl_event(
                            "diagnostic",
                            &format!(
                                "\"severity\":\"error\",\"phase\":\"raii_check\",\"code\":\"E_RAII_ACROSS_YIELD\",{},\"message\":{}{}{}",
                                span_to_json(&err.yield_span, filename),
                                json_string(&err.message()),
                                binding_span_json,
                                state_violation_json,
                            ),
                        );
                    }
                }
            }
            process::exit(1);
        }
    }

    // `--interp` / the JIT-default gate below only exist under `--features
    // llvm`; a non-llvm build has no JIT engine and always uses the interpreter.
    #[cfg(not(feature = "llvm"))]
    let _ = interp;
    // LLJIT Slice 6c (JIT-DEFAULT flip) — `karac run` executes the SAME codegen
    // as `karac build` through the LLJIT engine, so the interpreter-vs-codegen
    // divergence on type-clean programs (the epic's second divergence source,
    // after 6a closed the acceptance divergence) is gone BY CONSTRUCTION: one
    // lowering invoked two ways (AOT + JIT). This flips the Slice-6b opt-in
    // (`KARAC_RUN_JIT=1`) to a JIT-default opt-OUT, mirroring the Slice-5
    // repl/test flip — the JIT lane is exercised and green across the codegen
    // suite (2098) and the full examples corpus (JIT==AOT byte-for-byte, 0
    // divergences; see docs/spikes/lljit-productionization.md § 6c). The
    // interpreter is retained as a dev/debug backend, reached via `--interp`
    // (the `interp` param) or the `KARAC_RUN_JIT=0` env escape hatch. Consistent
    // with Slice 5, a codegen-compile failure is a HARD error (no interp
    // fallback) — codegen completeness is the gate, not something to paper over.
    // Scoped to plain text output with no `--timeout`: the JSON/JSONL structured
    // run envelopes and the cooperative `--timeout` deadline are interpreter-only
    // affordances the JIT one-shot doesn't provide, so those keep the
    // interpreter regardless. Compiled out on a non-`llvm` build (no JIT engine).
    #[cfg(feature = "llvm")]
    if output == OutputMode::Text
        && timeout.is_none()
        && !interp
        && std::env::var("KARAC_RUN_JIT").as_deref() != Ok("0")
    {
        // Codegen consumes ownership + concurrency (the interpreter path skips
        // both); run them now so the emitted IR matches `karac build`'s.
        pipeline.ownershipcheck();
        pipeline.concurrencycheck();
        match crate::codegen::compile_to_ir_with_options(
            &pipeline.parsed.program,
            pipeline.ownership.as_ref(),
            pipeline.concurrency.as_ref(),
            Some(filename),
            Some(&source),
        ) {
            Ok(ir) => process::exit(run_ir_via_jit_subprocess(&ir)),
            Err(e) => {
                eprintln!("error: codegen failed: {e}");
                // The JIT is the default backend (Slice 6c). A codegen gap the
                // interpreter still covers is recoverable — point the user at
                // the escape hatch so a not-yet-lowerable construct doesn't dead-
                // end their run. (The tree-walk interpreter is the retained
                // dev/debug backend.)
                eprintln!(
                    "  hint: this program uses a construct the codegen backend does not yet \
                     support; re-run with `--interp` (or `KARAC_RUN_JIT=0`) to use the tree-walk \
                     interpreter."
                );
                process::exit(1);
            }
        }
    }

    // Run
    let mut interp = Interpreter::new(&pipeline.parsed.program, pipeline.typed.as_ref().unwrap());
    interp.set_source_filename(filename);
    interp.set_source_text(&source);
    interp.set_dbg_output_mode(match output {
        OutputMode::Json | OutputMode::Jsonl => DbgOutputMode::Json,
        OutputMode::Text => DbgOutputMode::Terminal,
    });
    interp.sequential_mode = sequential;
    // `karac run --timeout DURATION` (tracker line 861): opt-in
    // wall-clock cap on the interpreter. Reuses the per-test deadline
    // mechanism the test runner ships with — interpreter polls the
    // deadline at every statement boundary and raises
    // `ControlFlow::TimedOut` on observation past it. Default is no
    // cap (long-running services / daemons / REPLs are legitimate
    // `karac run` workloads, so a default would silently break real
    // operations). On timeout: print the GNU `timeout(1)`-style
    // diagnostic to stderr and exit with code 124 so existing shell
    // pipelines compose.
    if let Some(d) = timeout {
        interp.set_test_deadline(Some(std::time::Instant::now() + d));
    }
    let main_result = interp.run();
    if interp.timed_out {
        if let Some(d) = timeout {
            eprintln!("karac: timed out after {}s", d.as_secs());
        }
        process::exit(124);
    }

    // design.md § Entry Point: a `main() -> Result[(), E]` returning `Err(e)`
    // prints `Error: {e}` to stderr (Display) and exits 1; `Ok(())` exits 0.
    // This mirrors the AOT codegen adaptation (B-2026-06-12-9) so `karac run`
    // and a built binary agree on entry-point semantics. Computed here, before
    // the error-return-trace block, so the `Error:` line precedes the trace —
    // the same order the compiled binary emits. A plain `fn main()` returns
    // `Unit`, so `as_result_err_payload` is `None` and this is a no-op.
    let main_err_exit = main_result.as_result_err_payload().is_some();
    if let Some(e) = main_result.as_result_err_payload() {
        match output {
            OutputMode::Text => eprintln!("Error: {e}"),
            OutputMode::Json => {
                println!("{{\"error\":{}}}", json_string(&e.to_string()));
            }
            OutputMode::Jsonl => {
                emit_jsonl_event(
                    "error",
                    &format!("\"message\":{}", json_string(&e.to_string())),
                );
            }
        }
    }

    // Surface runtime faults. The interpreter records every fault — contract
    // violations, index-out-of-bounds, divide-by-zero, `unwrap` of `None`,
    // explicit aborts — in `runtime_errors` "for callers to inspect". `cmd_run`
    // previously inspected only the `?`-return trace below, so the fault MESSAGE
    // was dropped (the user saw a bare `Error return trace: file:line`) AND the
    // process still exited 0. Print the message(s) with location, then exit
    // nonzero, so a faulting program is both legible and detectable by scripts.
    let runtime_errors: Vec<crate::interpreter::RuntimeError> = interp.runtime_errors.clone();
    if !runtime_errors.is_empty() {
        match output {
            OutputMode::Json => {
                let arr = runtime_errors
                    .iter()
                    .map(|e| {
                        format!(
                            "{{\"message\":{},\"location\":{{\"file\":{},\"line\":{},\"col\":{}}}}}",
                            json_string(&e.message),
                            json_string(filename),
                            e.span.line,
                            e.span.column,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",");
                println!("{{\"runtime_errors\":[{arr}]}}");
            }
            OutputMode::Jsonl => {
                for e in &runtime_errors {
                    emit_jsonl_event(
                        "runtime_error",
                        &format!(
                            "\"message\":{},\"location\":{{\"file\":{},\"line\":{},\"col\":{}}}",
                            json_string(&e.message),
                            json_string(filename),
                            e.span.line,
                            e.span.column,
                        ),
                    );
                }
            }
            OutputMode::Text => {
                for e in &runtime_errors {
                    eprintln!(
                        "runtime error: {}\n  at {}:{}:{}",
                        e.message, filename, e.span.line, e.span.column,
                    );
                }
            }
        }
    }

    // Emit error return trace ONLY when the program actually terminated with an
    // unhandled error — main returned `Err` (`main_err_exit`) or a runtime fault
    // occurred (`runtime_errors`). The `?`-propagation ring buffer accumulates a
    // frame per `?` that re-propagates an `Err`, and is cleared only by a LATER
    // successful `?` (Ok/Some) — never when the propagated `Err` is CAUGHT by a
    // `match`/`if let`. So a program that uses `?` internally and handles every
    // error (e.g. `match parse(x) { Err(e) => … }`) left stale frames that printed
    // an "Error return trace" to output despite exiting cleanly (B-2026-07-11-8,
    // surfaced by the `examples/json.kara` dogfood). Gating on the actual error
    // outcome suppresses the stale trace while preserving it for a real
    // unhandled-error exit (main `Err` or a fault).
    if !interp.error_trace().is_empty() && (main_err_exit || !runtime_errors.is_empty()) {
        let trace = format_error_trace_json(interp.error_trace(), interp.error_trace_truncated());
        match output {
            OutputMode::Json => {
                println!("{{\"error_return_trace\":{}}}", trace);
            }
            OutputMode::Jsonl => {
                emit_jsonl_event(
                    "error_return_trace",
                    &format!(
                        "\"frames\":{},\"truncated\":{}",
                        trace,
                        interp.error_trace_truncated()
                    ),
                );
            }
            OutputMode::Text => {
                eprintln!("Error return trace:");
                for frame in interp.error_trace() {
                    let file_part = if frame.file.is_empty() {
                        String::new()
                    } else {
                        format!("{}:", frame.file)
                    };
                    eprintln!("  {}{}:{}", file_part, frame.line, frame.column);
                }
                if interp.error_trace_truncated() {
                    eprintln!("  ... (trace truncated, max {} frames)", 64);
                }
            }
        }
    }

    // A faulting program exits nonzero (previously always 0 — scripts couldn't
    // detect interpreter-level failures). Gated on `runtime_errors` so a clean
    // run still exits 0. `main_err_exit` adds the design.md § Entry Point case:
    // a `main() -> Result` that returned `Err(e)` exits 1 (the `Error:` line
    // was already printed above). Faults take precedence over an `ExitCode`
    // return — a runtime error unwinds before `main` produces a clean value, so
    // `main_result` is `Unit`, not the intended code, in that case anyway.
    if !runtime_errors.is_empty() || main_err_exit {
        process::exit(1);
    }

    // design.md § Entry Point: `fn main() -> ExitCode` exits with the
    // returned code (Slice B). The interpreter is type-erased, so the
    // `ExitCode` arrives as a plain `Value::Int`; the AST signature
    // (`main_return_is_exitcode`) is what tells us to treat it as an exit
    // code. Mirrors the AOT codegen `ret i32 <code>` arm so `karac run`
    // and a built binary agree. `0` falls through to the normal clean
    // exit; any nonzero code exits explicitly.
    if main_return_is_exitcode(&pipeline.parsed.program) {
        if let crate::interpreter::Value::Int(code) = main_result {
            process::exit(code as i32);
        }
    }
}

fn cmd_check(
    filename: &str,
    output: OutputMode,
    profiles: Option<Vec<crate::manifest::CompileProfile>>,
    targets: Option<Vec<String>>,
    concurrency_report: bool,
    simd_report: bool,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Both drivers are "run the pipeline N times and group diagnostics"
    // matrices; combining them would be an N×M product nobody has asked
    // for. Reject loudly rather than picking a silent precedence.
    if profiles.is_some() && targets.is_some() {
        eprintln!("error: --profiles and --targets are mutually exclusive");
        process::exit(1);
    }

    // Resolver follow-up (m): when `karac check <file>` runs inside a project,
    // surface dependency-resolution diagnostics so a broken dep graph (cycle /
    // version conflict / MSRV / missing path-dep / workspace-deref) fails the
    // check exactly as it fails `karac build`. Runs once up front, before the
    // profiles/targets/single dispatch below, so it fires regardless of matrix
    // mode. No-op for a single-file script outside any project, or a project
    // that declares no deps / MSRV. Path-dep-only (no network) — see
    // `surface_dep_graph_diagnostics`.
    let file_dir = std::path::Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    if let Some(root) = manifest::discover_project_root(&file_dir) {
        if let Ok(mf) = manifest::load_from_root(&root) {
            let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));
            let has_deps = !mf.dependencies.is_empty()
                || !mf.dev_dependencies.is_empty()
                || mf.kara_version.is_some();
            if has_deps && !surface_dep_graph_diagnostics(&root, mf, output) {
                process::exit(1);
            }
        }
    }

    let source = read_source(filename);

    if let Some(list) = profiles {
        cmd_check_profiles(filename, &source, output, &list, lint_overrides);
        return;
    }

    // Multi-target verification (phase-10): `--targets=` wins; absent,
    // consult the discovered manifest's `[build].targets` (walking
    // upward from the file's own directory, same discovery rule as
    // `karac run`). An empty/undeclared list keeps the single-pass
    // default below — check under the active (`native`) target.
    let targets = targets.or_else(|| {
        let declared = manifest_build_targets_for(filename, output);
        if declared.is_empty() {
            None
        } else {
            Some(declared)
        }
    });
    if let Some(list) = targets {
        cmd_check_targets(filename, &source, output, &list, lint_overrides);
        return;
    }

    match output {
        OutputMode::Jsonl => {
            let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
            run_pipeline_jsonl(&mut pipeline);
            if pipeline.total_errors() > 0 {
                process::exit(1);
            }
        }
        _ => {
            let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
            pipeline.run_all_checks();

            // Slice D: concurrency report fires after `run_all_checks` (which
            // already runs `concurrencycheck()`) and before the final OK /
            // error summary so the report sits with the rest of stdout.
            if concurrency_report {
                emit_concurrency_report(&pipeline);
            }
            // SIMD lowering report (slice 5b) — same render-side placement.
            if simd_report {
                emit_simd_report(&pipeline);
            }

            match output {
                OutputMode::Text => {
                    print_text_diagnostics(&pipeline);
                    let total = pipeline.total_errors();
                    if total > 0 {
                        eprintln!("\n{total} error(s) found.");
                        process::exit(1);
                    } else {
                        eprintln!("All checks passed.");
                    }
                }
                OutputMode::Json => {
                    emit_json_output(&pipeline);
                    if pipeline.total_errors() > 0 {
                        process::exit(1);
                    }
                }
                OutputMode::Jsonl => unreachable!(),
            }
        }
    }
}

/// Slice D helper: render the human-readable concurrency report from the
/// pipeline's already-populated `concurrency` and `effects` fields and
/// emit it to stdout. No-op when either field is None (the analysis didn't
/// run because earlier phases failed); the build/check paths still surface
/// the upstream errors through the normal diagnostic channel.
fn emit_concurrency_report(pipeline: &Pipeline) {
    let (Some(concurrency), Some(effects)) = (&pipeline.concurrency, &pipeline.effects) else {
        return;
    };
    let report = crate::concurrency_report::render_concurrency_report(
        concurrency,
        effects,
        &pipeline.parsed.program,
    );
    print!("{report}");
}

/// `--simd-report=verbose` helper (slice 5b): render the per-function SIMD
/// lowering-tier report from the typechecked program and emit it to stdout.
/// Reuses `simd_report::analyze_program` — the same walk `simd_check` runs —
/// but renders *all* tiers (Native/Wide/Scalar), not just the `#[require_simd]`
/// errors. A no-op-shaped report (`<no vector operations>`) when the program
/// has no vector ops or typecheck didn't run.
fn emit_simd_report(pipeline: &Pipeline) {
    let findings =
        crate::simd_report::analyze_program(&pipeline.parsed.program, pipeline.typed.as_ref());
    print!("{}", crate::simd_report::render_simd_report(&findings));
}

/// Multi-profile typecheck driver. Runs the full pipeline once per named
/// profile and groups diagnostics by profile so a CI matrix can verify
/// "this library compiles cleanly under default + embedded + kernel" from a
/// single invocation. Exits non-zero if any profile fails. Profile only
/// affects the effect-checker today (extern declarations are validated
/// against the profile's forbidden-effect set per `manifest::CompileProfile`),
/// so the parse / resolve / typecheck phases produce identical output across
/// profiles — only the effect phase diverges. Per-profile grouping keeps the
/// output skimmable when one profile fails and the others pass.
fn cmd_check_profiles(
    filename: &str,
    source: &str,
    output: OutputMode,
    profiles: &[crate::manifest::CompileProfile],
    lint_overrides: crate::lints::CliLintOverrides,
) {
    let mut any_failed = false;
    let mut blocks: Vec<String> = Vec::new();
    for (idx, profile) in profiles.iter().enumerate() {
        let mut pipeline =
            Pipeline::new(filename, source).with_lint_overrides(lint_overrides.clone());
        pipeline.profile = *profile;

        match output {
            OutputMode::Text => {
                pipeline.run_all_checks();
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                if idx > 0 {
                    eprintln!();
                }
                eprintln!("── profile: {} ──", profile.as_str());
                print_text_diagnostics(&pipeline);
                if total > 0 {
                    eprintln!("{total} error(s) under '{}' profile.", profile.as_str());
                } else {
                    eprintln!("All checks passed under '{}' profile.", profile.as_str());
                }
            }
            OutputMode::Json => {
                pipeline.run_all_checks();
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                let diags = collect_diagnostics(&pipeline);
                let block = format!(
                    "{{\"profile\":{},\"success\":{},\"total_errors\":{},\"diagnostics\":{}}}",
                    json_string(profile.as_str()),
                    total == 0,
                    total,
                    diags.to_json_array(),
                );
                blocks.push(block);
            }
            OutputMode::Jsonl => {
                emit_jsonl_event(
                    "profile_start",
                    &format!("\"profile\":{}", json_string(profile.as_str())),
                );
                run_pipeline_jsonl(&mut pipeline);
                let total = pipeline.total_errors();
                if total > 0 {
                    any_failed = true;
                }
                emit_jsonl_event(
                    "profile_complete",
                    &format!(
                        "\"profile\":{},\"success\":{},\"total_errors\":{}",
                        json_string(profile.as_str()),
                        total == 0,
                        total,
                    ),
                );
            }
        }
    }

    if let OutputMode::Json = output {
        println!(
            "{{\"profiles\":[{}],\"success\":{}}}",
            blocks.join(","),
            !any_failed,
        );
    }

    if any_failed {
        process::exit(1);
    }
}

/// Manifest-side trigger for multi-target check: walk upward from the
/// checked file's own directory (the `karac run` discovery rule —
/// the file's filesystem location is the stable identity, not the
/// cwd) and return the discovered manifest's `[build].targets`.
/// No manifest found → empty (single-file scripts outside any project
/// keep the single-pass default). A malformed manifest is a hard error
/// — same posture as `karac run`'s discovery.
fn manifest_build_targets_for(filename: &str, output: OutputMode) -> Vec<String> {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => m.build_targets,
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => Vec::new(),
    }
}

/// Read the `KARAC_TARGET_CPU` env var — the middle tier of the
/// `--target-cpu` precedence chain (flag, then env, then `[release]
/// target-cpu`, then the per-target default table). Empty /
/// whitespace-only is treated
/// as unset so `KARAC_TARGET_CPU= karac build …` can neutralize an
/// outer-scope export without tripping validation.
#[cfg(feature = "llvm")]
fn read_target_cpu_env() -> Option<String> {
    std::env::var("KARAC_TARGET_CPU")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the `KARAC_TARGET_FEATURES` env var — the middle tier of the
/// `--target-features` precedence chain (resolved independently of the
/// CPU chain). Same empty-means-unset contract as `read_target_cpu_env`.
#[cfg(feature = "llvm")]
fn read_target_features_env() -> Option<String> {
    std::env::var("KARAC_TARGET_FEATURES")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Discover the manifest by walking upward from the built file's own
/// directory (the `karac run` discovery rule, same shape as
/// `manifest_build_targets_for` above) and return one `[release]` field
/// picked by `pick`. No manifest → `None`. A malformed manifest is a
/// hard error — but note the callers only reach this tier when neither
/// the CLI flag nor the env var supplied a value, so explicit overrides
/// never gain a manifest failure mode. (The cpu and features chains
/// resolve lazily and independently, so a build may walk twice — the
/// walk is cheap and idempotent.)
#[cfg(feature = "llvm")]
fn manifest_release_field_for(
    filename: &str,
    output: OutputMode,
    pick: fn(&manifest::Manifest) -> Option<String>,
) -> Option<String> {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => pick(&m),
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => None,
    }
}

/// The `[wasm]` table's wasm-threads tuning knobs, via the same lazy
/// manifest walk-up as [`manifest_release_field_for`] (single-file
/// builds discover the manifest from the file's own directory; no
/// manifest → all-`None` defaults). Returns `(pool_size, fallback,
/// max_memory_pages)`. Only consulted on a `--features wasm-threads`
/// build, so plain builds never gain a manifest failure mode from it.
#[cfg(feature = "llvm")]
fn manifest_wasm_knobs_for(
    filename: &str,
    output: OutputMode,
) -> (Option<u32>, Option<bool>, Option<u32>) {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => (m.wasm_pool_size, m.wasm_fallback, m.wasm_max_memory_pages),
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => (None, None, None),
    }
}

/// Resolve the `[link]` directive for a single-file build by walking up
/// from the file's own directory (the [`manifest_release_field_for`]
/// discovery rule). No manifest → two empty vecs. Manifest-only: unlike the
/// CPU/features chains there is no CLI-flag or env tier, because a library
/// search path is intrinsically a project/environment fact (it comes from
/// `llvm-config --libdir`), not a per-invocation toggle. A malformed
/// manifest is a hard error, same posture as the sibling walk-ups.
#[cfg(feature = "llvm")]
fn manifest_link_config_for(filename: &str, output: OutputMode) -> (Vec<String>, Vec<String>) {
    let file_dir = std::path::Path::new(filename)
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::path::PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    match manifest::discover_project_root(&file_dir) {
        Some(root) => match manifest::load_from_root(&root) {
            Ok(m) => (m.link_libs, m.link_search_paths),
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => (Vec::new(), Vec::new()),
    }
}

/// Default `--max-memory` for the threaded wasm module, in 64 KiB pages:
/// 16384 pages = 1 GiB — rustc's own wasm32-wasip1-threads target
/// default (shared memories must declare a maximum; the reservation is
/// address space, committed lazily). `[wasm] max-memory-pages`
/// overrides.
#[cfg(feature = "llvm")]
const WASM_THREADS_DEFAULT_MAX_MEMORY_PAGES: u32 = 16384;

/// Phase-10 WASM entry-point discovery (sub-slice B): emit a
/// non-blocking note for each discovered export whose param/return types
/// are not bare scalars. Such exports are still raw wasm exports
/// (callable via `instance.exports`), but their idiomatic typed/marshalled
/// surface (struct / `Option` / `Result` JS shapes; rich WIT) lands with
/// the export trampoline + canonical-ABI sub-slice — so they are omitted
/// from the typed `.d.ts` / WIT for now rather than silently mis-typed.
#[cfg(feature = "llvm")]
fn warn_unlowered_exports(
    exports: &[crate::wasm_exports::ExportSig],
    lowerable: fn(&crate::wasm_exports::ExportSig) -> bool,
) {
    for e in exports.iter().filter(|e| !lowerable(e)) {
        eprintln!(
            "note: wasm export '{}' has parameter/return types not yet marshalled for this \
             binding — omitted from the typed surface for now (richer types land with later \
             phase-10 export-trampoline steps); it remains a raw wasm export.",
            e.name
        );
    }
}

/// Run the threaded pass of a `--features wasm-threads` build (phase-10
/// wasm-threads entry): codegen the SAME front-end output again with
/// auto-par re-enabled on the wasip1-threads machine, link it
/// `--shared-memory` against the threaded runtime archive, and read the
/// linked module's imported-memory limits back out (wasm-ld computes
/// `initial`; the glue must mirror the limits exactly). Returns the
/// glue config describing the artifact. Shared by single-file and
/// project mode — `threads_wasm_path` is the final artifact path,
/// `threads_filename` the sibling-relative name baked into the glue.
#[cfg(feature = "llvm")]
#[allow(clippy::too_many_arguments)]
fn emit_wasm_threads_artifact(
    program: &crate::ast::Program,
    ownership: Option<&crate::ownership::OwnershipCheckResult>,
    concurrency: Option<&crate::concurrency::ConcurrencyAnalysis>,
    source_filename: Option<&str>,
    source_text: Option<&str>,
    release: bool,
    obj_path: &str,
    threads_wasm_path: &std::path::Path,
    threads_filename: &str,
    knobs: (Option<u32>, Option<bool>, Option<u32>),
) -> crate::wasm_glue::WasmThreadsGlueConfig {
    let (pool_size, fallback, max_pages) = knobs;
    if let Err(e) = crate::codegen::compile_to_object_wasm_threaded(
        program,
        obj_path,
        ownership,
        concurrency,
        source_filename,
        source_text,
        release,
    ) {
        eprintln!("error: wasm-threads codegen failed: {e}");
        process::exit(1);
    }
    let max_memory_pages = max_pages.unwrap_or(WASM_THREADS_DEFAULT_MAX_MEMORY_PAGES);
    let wasm_export_names = crate::wasm_exports::link_export_names(
        &crate::wasm_exports::collect_wasm_exports(program, crate::target::active_target()),
    );
    let link_result = crate::codegen::link_wasm_executable_threaded(
        obj_path,
        threads_wasm_path.to_str().unwrap_or(threads_filename),
        u64::from(max_memory_pages) * 65536,
        &wasm_export_names,
    );
    let _ = std::fs::remove_file(obj_path);
    if let Err(e) = link_result {
        eprintln!("error: wasm-threads link failed: {e}");
        process::exit(1);
    }
    // Mirror the linked module's memory-import limits into the glue —
    // instantiation fails the import match on any divergence.
    let bytes = match std::fs::read(threads_wasm_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "error: cannot read back threaded module {}: {e}",
                threads_wasm_path.display()
            );
            process::exit(1);
        }
    };
    let Some((mem_initial_pages, mem_max_pages)) = crate::wasm_glue::imported_memory_limits(&bytes)
    else {
        eprintln!(
            "error: threaded module {} carries no imported env.memory — \
             the --shared-memory link should have produced one (linker drift?)",
            threads_wasm_path.display()
        );
        process::exit(1);
    };
    crate::wasm_glue::WasmThreadsGlueConfig {
        threads_filename: threads_filename.to_string(),
        no_fallback: fallback == Some(false),
        pool_size_override: pool_size,
        mem_initial_pages,
        mem_max_pages,
    }
}

/// Act on the resolved `--target-cpu` value (phase-10; design.md § CPU
/// Baseline Targeting). `None` — the common case — keeps the per-target
/// default table. The literal `help` prints LLVM's supported-CPU
/// listing for the active target and exits 0 (`rustc -C
/// target-cpu=help` mirror). Any other name is validated against that
/// same listing — LLVM's native behavior on an unknown CPU is
/// warn-and-fall-back-to-generic, i.e. exactly the silent baseline
/// neutering the validation closes — then installed process-wide for
/// the codegen driver's target-machine constructors.
#[cfg(feature = "llvm")]
fn apply_target_cpu_override(resolved: Option<String>) {
    let Some(cpu) = resolved else { return };
    if cpu == "help" {
        crate::codegen::print_target_cpu_listing();
        process::exit(0);
    }
    if let Err(msg) = crate::codegen::validate_target_cpu(&cpu) {
        eprintln!("{msg}");
        process::exit(1);
    }
    crate::target::set_target_cpu_override(&cpu);
}

/// Act on the resolved `--target-features` value — the
/// `apply_target_cpu_override` sibling (design.md § CPU Baseline
/// Targeting > Feature-string override). `help` prints the same
/// per-target dump (its `Available features` section is the relevant
/// half) and exits 0. Any other value is token-validated (`+`/`-`
/// prefixes, names in LLVM's per-target feature registry — LLVM's
/// native behavior on an unknown feature is warn-and-ignore, the same
/// silent neutering the CPU validation closes) and installed for the
/// target-machine constructors, which append it after the per-target
/// default features.
#[cfg(feature = "llvm")]
fn apply_target_features_override(resolved: Option<String>) {
    let Some(features) = resolved else { return };
    if features == "help" {
        crate::codegen::print_target_cpu_listing();
        process::exit(0);
    }
    if let Err(msg) = crate::codegen::validate_target_features(&features) {
        eprintln!("{msg}");
        process::exit(1);
    }
    crate::target::set_target_features_override(&features);
}

/// Install the resolved `[link]` directive process-wide for the native
/// linker (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking). A no-op
/// when both lists are empty so a `[link]`-free build never touches the
/// global (and never differs from the pre-`[link]` link line). The codegen
/// driver's `link_executable_impl` is the only reader; wasm-ld ignores it,
/// so callers may skip this on wasm builds.
#[cfg(feature = "llvm")]
fn apply_native_link_config(libs: Vec<String>, search_paths: Vec<String>) {
    if libs.is_empty() && search_paths.is_empty() {
        return;
    }
    crate::target::set_native_link_config(libs, search_paths);
}

/// Normalize a rendered `DiagnosticJson` entry for cross-target
/// comparison by dropping its run-local `"id":"dN"` field (always the
/// first field — see `DiagnosticJson::add`). An entry unique to one
/// target shifts every subsequent id in that run, so raw string
/// equality would misclassify otherwise-identical diagnostics.
fn strip_diag_id(entry: &str) -> String {
    let Some(rest) = entry.strip_prefix("{\"id\":\"") else {
        return entry.to_string();
    };
    match rest.find("\",") {
        Some(idx) => format!("{{{}", &rest[idx + 2..]),
        None => entry.to_string(),
    }
}

/// Multi-target check driver (phase-10, design.md § Cross-target
/// Compilation > `karac check` Under Multiple Targets). Runs the full
/// type-check + effect-check pipeline once per target, parameterizing
/// the target-provided resource set each time via
/// `target::set_active_target` — which also re-parameterizes
/// `#[target(...)]` absence filtering and tombstone diagnostics, so
/// each pass sees exactly the item set and gate that target's build
/// would see. Diagnostics are tagged with the producing target;
/// diagnostics identical on EVERY target are deduplicated into a
/// shared "all targets" group (they're target-agnostic bugs, not
/// target-specific) — text and JSON modes only; JSONL streams
/// per-target between `target_start`/`target_complete` markers and
/// leaves dedup to the consumer, mirroring the profiles driver.
/// Bounded by construction: the target set is closed at four.
fn cmd_check_targets(
    filename: &str,
    source: &str,
    output: OutputMode,
    targets: &[String],
    lint_overrides: crate::lints::CliLintOverrides,
) {
    let mut any_failed = false;

    if let OutputMode::Jsonl = output {
        for target in targets {
            crate::target::set_active_target(target)
                .expect("target names validated at parse/manifest load");
            emit_jsonl_event(
                "target_start",
                &format!("\"target\":{}", json_string(target)),
            );
            let mut pipeline =
                Pipeline::new(filename, source).with_lint_overrides(lint_overrides.clone());
            run_pipeline_jsonl(&mut pipeline);
            let total = pipeline.total_errors();
            if total > 0 {
                any_failed = true;
            }
            emit_jsonl_event(
                "target_complete",
                &format!(
                    "\"target\":{},\"success\":{},\"total_errors\":{}",
                    json_string(target),
                    total == 0,
                    total,
                ),
            );
        }
        if any_failed {
            process::exit(1);
        }
        return;
    }

    // Text + JSON: run every target first, collecting both rendered
    // text blocks and JSON entries per target, then split shared vs
    // target-specific. Each mode dedups over its own rendering — text
    // over the rendered block (phase + span + message), JSON over the
    // entry normalized for its run-local `"id"` counter (an entry
    // unique to one target shifts every later id, so raw string
    // equality would under-dedup). The splits can differ at the
    // margin (JSON carries typecheck warnings text mode doesn't);
    // each is consistent within its own output.
    struct TargetRun {
        target: String,
        total_errors: usize,
        text_blocks: Vec<String>,
        json_entries: Vec<String>,
    }
    let mut runs: Vec<TargetRun> = Vec::new();
    for target in targets {
        crate::target::set_active_target(target)
            .expect("target names validated at parse/manifest load");
        let mut pipeline =
            Pipeline::new(filename, source).with_lint_overrides(lint_overrides.clone());
        pipeline.run_all_checks();
        let total = pipeline.total_errors();
        if total > 0 {
            any_failed = true;
        }
        runs.push(TargetRun {
            target: target.clone(),
            total_errors: total,
            text_blocks: render_text_diagnostics(&pipeline),
            json_entries: collect_diagnostics(&pipeline).entries,
        });
    }

    // A diagnostic is target-agnostic when its rendered block appears
    // on every target. Set semantics — exact duplicate blocks within
    // one target collapse, which is already redundant output. With a
    // single requested target there is nothing to compare against, so
    // everything stays target-tagged.
    let shared: Vec<String> = if runs.len() > 1 {
        runs[0]
            .text_blocks
            .iter()
            .filter(|block| runs[1..].iter().all(|r| r.text_blocks.contains(block)))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    let shared_set: std::collections::HashSet<&str> = shared.iter().map(|s| s.as_str()).collect();

    match output {
        OutputMode::Text => {
            if !shared.is_empty() {
                eprintln!("── all targets ──");
                for block in &shared {
                    eprintln!("{block}");
                }
            }
            for (idx, run) in runs.iter().enumerate() {
                if idx > 0 || !shared.is_empty() {
                    eprintln!();
                }
                eprintln!("── target: {} ──", run.target);
                for block in &run.text_blocks {
                    if shared_set.contains(block.as_str()) {
                        continue;
                    }
                    eprintln!("{block}");
                }
                if run.total_errors > 0 {
                    eprintln!(
                        "{} error(s) under target '{}'.",
                        run.total_errors, run.target
                    );
                } else {
                    eprintln!("All checks passed under target '{}'.", run.target);
                }
            }
        }
        OutputMode::Json => {
            // Shared entries are reported once (drawn from the first
            // target's run, ids included); per-target arrays carry the
            // remainder. Dedup key: the entry minus its run-local id.
            let shared_keys: std::collections::HashSet<String> = if runs.len() > 1 {
                runs[0]
                    .json_entries
                    .iter()
                    .map(|e| strip_diag_id(e))
                    .filter(|key| {
                        runs[1..]
                            .iter()
                            .all(|r| r.json_entries.iter().any(|e| strip_diag_id(e) == *key))
                    })
                    .collect()
            } else {
                std::collections::HashSet::new()
            };
            let shared_json: Vec<&String> = runs
                .first()
                .map(|r| {
                    r.json_entries
                        .iter()
                        .filter(|e| shared_keys.contains(&strip_diag_id(e)))
                        .collect()
                })
                .unwrap_or_default();
            let blocks: Vec<String> = runs
                .iter()
                .map(|run| {
                    let entries: Vec<&String> = run
                        .json_entries
                        .iter()
                        .filter(|e| !shared_keys.contains(&strip_diag_id(e)))
                        .collect();
                    format!(
                        "{{\"target\":{},\"success\":{},\"total_errors\":{},\"diagnostics\":[{}]}}",
                        json_string(&run.target),
                        run.total_errors == 0,
                        run.total_errors,
                        entries
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                })
                .collect();
            println!(
                "{{\"targets\":[{}],\"shared_diagnostics\":[{}],\"success\":{}}}",
                blocks.join(","),
                shared_json
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                !any_failed,
            );
        }
        OutputMode::Jsonl => unreachable!("handled above"),
    }

    if any_failed {
        process::exit(1);
    }
}

/// Classify a `--target` value and activate it when it names a v1
/// compilation target (phase-10 WASM build path).
///
/// - v1 names: `native`, `wasm_wasi`, and `wasm_browser` are buildable —
///   the name is installed as the process-wide active target (see
///   `target::set_active_target`) so `#[target(...)]` absence semantics,
///   tombstone diagnostics, and the E0411 target gate all key on it.
///   `wasm_browser` additionally emits the `<stem>.js` ES-module glue
///   next to the `.wasm` (see `wasm_glue`). `gpu` (kernels are
///   dispatched from a host program, not standalone built) is rejected
///   loudly rather than silently producing a native binary.
/// - Anything else is a rustc-style triple — project mode's manifest
///   `[target.<triple>.*]` overlay selector — and leaves the active
///   target at `native`.
///
/// Returns the active v1 target name for the build.
fn resolve_build_target(target: Option<&str>) -> &'static str {
    match target {
        Some(name) if crate::target::is_v1_target_name(name) => match name {
            "gpu" => {
                eprintln!(
                    "error: `--target=gpu` is not a standalone build target — GPU kernels \
                     are consumed by a host program via gpu.dispatch (design.md § Target \
                     Build Artifacts)."
                );
                process::exit(1);
            }
            buildable => {
                crate::target::set_active_target(buildable)
                    .expect("v1 target name membership checked above");
                crate::target::active_target()
            }
        },
        _ => crate::target::active_target(),
    }
}

/// Phase-10 `--bindings` flag: resolve the effective WASM output shape
/// for a build (single-file and project mode share this). Explicit flag
/// wins; omitted, the mode is inferred from the target (`wasm_browser`
/// → browser, `wasm_wasi` → component — design.md § Target Build
/// Artifacts: the `--target` choice already declares the host family,
/// so defaulting off it avoids silent browser-lock-in). On a non-WASM
/// target the flag is accepted-but-inert per the tracker entry — there
/// is no glue concept for a native binary.
fn resolve_effective_bindings(
    build_target: &str,
    bindings: Option<BindingsMode>,
) -> Option<BindingsMode> {
    let is_wasm = build_target == "wasm_wasi" || build_target == "wasm_browser";
    if !is_wasm {
        return None;
    }
    Some(bindings.unwrap_or(if build_target == "wasm_browser" {
        BindingsMode::Browser
    } else {
        BindingsMode::Component
    }))
}

/// `--features wasm-threads` scope gate, shared by single-file and
/// project build (phase-10 wasm-threads entry). The flag is
/// `wasm_browser`-only: the threaded substrate is the wasi-threads ABI
/// (`wasi.thread-spawn` / `wasi_thread_start`), which the component
/// model does not compose with — and `wasm_wasi`'s default bindings are
/// component (host-thread integration for wasm_wasi stays the design.md
/// § WASM Concurrency Lowering future concern). The same reasoning
/// rejects an explicit `--bindings=component` on a `wasm_browser`
/// threaded build; `--bindings=none` is fine (both modules are emitted,
/// the embedder owns `wasi.thread-spawn`). No-op when the flag is off.
///
/// Pure argument validation — no codegen, no LLVM types — so it is NOT
/// `llvm`-gated: the flag/target rejection must be identical whether or
/// not karac was built with the backend. (A `#[cfg(feature = "llvm")]`
/// guard here silently let the gate fall through to manifest discovery
/// in non-llvm project builds, surfacing "no kara.toml" instead of the
/// scope rejection.)
fn validate_wasm_threads_scope(
    wasm_threads: bool,
    build_target: &str,
    effective_bindings: Option<BindingsMode>,
) {
    // Record the threads opt-in for checker/codegen passes (the host-async
    // timer gate in `codegen/channel.rs` keys on it). Called in both build
    // paths before codegen; `karac check` never reaches here, so it stays
    // at its default (false) and the codegen-only gate never fires there.
    crate::target::set_wasm_threads(wasm_threads);
    if !wasm_threads {
        return;
    }
    if build_target != "wasm_browser" {
        eprintln!(
            "error: --features wasm-threads requires --target=wasm_browser (got `{build_target}`). \
             The threaded lowering rides the wasi-threads ABI, which the component model \
             (wasm_wasi's default bindings) does not compose with. Drop the flag or switch targets."
        );
        process::exit(1);
    }
    if effective_bindings == Some(BindingsMode::Component) {
        eprintln!(
            "error: --features wasm-threads is incompatible with --bindings=component \
             (wasi-threads and the component model do not compose). \
             Use --bindings=browser (default) or --bindings=none."
        );
        process::exit(1);
    }
}

/// After a `--crate-type staticlib` build, print a one-line note steering
/// Rust hosts to the cdylib. The thick `.a` bundles the Kāra runtime — a Rust
/// crate that carries `std` — so a Rust host static-linking it hits a cryptic
/// consumer-side `duplicate symbol: rust_eh_personality` (+ other std symbols)
/// with no pointer back to the fix. A `.so`/`.dylib`/`.dll` encapsulates those
/// internal symbols, so the collision only exists for the static archive. C /
/// C++ hosts have no `std` to clash with, so the note is scoped to Rust and
/// printed on stderr (informational, doesn't pollute a `Built:`-parsing pipe).
///
/// Only the `--features llvm` build reaches a real library link (the non-llvm
/// path stubs the codegen), so this is gated to match its call sites.
#[cfg(feature = "llvm")]
fn print_staticlib_rust_host_note(kind: NativeCrateType) {
    if kind == NativeCrateType::StaticLib {
        eprintln!(
            "note: for a Rust host, link the cdylib (build with --crate-type cdylib), \
             not this static archive — the bundled Kāra runtime's `std` symbols \
             collide with the Rust host's `std` at static-link time. C/C++ hosts \
             may link either."
        );
    }
}

// CLI dispatch helpers naturally land more flag-shaped arguments
// than the clippy default; factoring them into a struct here would
// just move the flag list rather than tighten it.
/// Emit a single-file-build error respecting the output mode (text to stderr,
/// a minimal one-line JSON diagnostic under `--output=json`).
fn emit_build_error(msg: &str, output: OutputMode) {
    match output {
        OutputMode::Json | OutputMode::Jsonl => {
            println!(
                "{{\"severity\":\"error\",\"phase\":\"build\",\"message\":{}}}",
                json_string(msg)
            );
        }
        OutputMode::Text => eprintln!("error: {msg}"),
    }
}

/// If `filename` is a source file inside a `kara.toml` package's `src/`
/// directory, return an actionable refusal message: a single-file `karac build`
/// there silently drops the package's sibling modules and produces a truncated
/// binary (B-2026-07-08-19). `None` for a standalone file — no package root, or
/// the file is not under the package's `src/` (a script that merely sits at or
/// near the package root stays buildable single-file).
fn package_member_build_refusal(filename: &str) -> Option<String> {
    let path = std::path::Path::new(filename);
    let parent = path.parent()?;
    let file_dir = if parent.as_os_str().is_empty() {
        std::path::Path::new(".")
    } else {
        parent
    };
    let root = manifest::discover_project_root(file_dir)?;
    let abs_file = std::fs::canonicalize(path).ok()?;
    let abs_src = std::fs::canonicalize(root.join("src")).ok()?;
    if !abs_file.starts_with(&abs_src) {
        return None;
    }
    let root_disp = root.display();
    Some(format!(
        "`{filename}` is a source file of the package at `{root_disp}` — a \
         single-file `karac build` drops the package's sibling modules and \
         produces a truncated binary. Build the whole package instead: `cd \
         {root_disp} && karac build` (or `karac run {filename}` to run it \
         directly)."
    ))
}

#[allow(clippy::too_many_arguments)]
fn cmd_build(
    filename: &str,
    output: OutputMode,
    concurrency_report: bool,
    simd_report: bool,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
    bindings: Option<BindingsMode>,
    target_cpu: Option<&str>,
    target_features: Option<&str>,
    wasm_threads: bool,
    monomorphization_budget: crate::monomorphization::MonomorphizationBudget,
    release: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Single-file mode runs no dep resolution and reaches no network surface,
    // so `--offline` is silently accepted for ergonomic CLI consistency with
    // project mode (operators script both via the same flag set).
    let _ = offline;
    // Phase-10 WASM build path: a `--target` value from the closed v1 name
    // set (`native`, `wasm_browser`, `wasm_wasi`, `gpu`) selects the
    // compilation target — it swaps the process-wide active target that
    // `filter_inactive_items` (`#[target(...)]` absence semantics), the
    // resolver's tombstone diagnostics, and the effect checker's E0411
    // target gate all read. Any other value is a rustc-style triple, which
    // only project mode consumes (manifest `[target.<triple>.*]` overlay
    // merge) and stays accepted-but-inert in single-file mode.
    let build_target = resolve_build_target(target);
    // Single-file `karac build` on a file that is a member of a `kara.toml`
    // PACKAGE (lives under the package's `src/` directory) silently drops the
    // sibling modules it `import`s and emits a truncated binary that links but
    // does nothing — an unresolvable local-module import is accepted rather
    // than erroring in single-file mode (B-2026-07-08-19). `karac run` on the
    // same file auto-discovers the package and works, so this is a build-only
    // footgun. Refuse it with actionable guidance instead of producing junk;
    // gated tightly (file under `<root>/src/`) so a genuinely standalone script
    // that merely sits near a manifest is unaffected.
    if let Some(msg) = package_member_build_refusal(filename) {
        emit_build_error(&msg, output);
        process::exit(1);
    }
    emit_no_proxy_note(no_proxy);
    let _ = no_proxy;
    #[cfg(feature = "llvm")]
    {
        // CPU baseline override (phase-10 `--target-cpu`; design.md §
        // CPU Baseline Targeting). Precedence: CLI flag >
        // `KARAC_TARGET_CPU` env > the discovered manifest's
        // `[release] target-cpu` (walk-up from the file's directory —
        // the `karac run` discovery rule; only consulted when both
        // higher tiers are absent, so an explicit flag/env build never
        // gains a manifest-error failure mode). Runs after
        // `resolve_build_target` — `help` and validation are
        // per-active-target — and before any pipeline pass, failing
        // fast on a typo'd name.
        apply_target_cpu_override(
            target_cpu
                .map(str::to_string)
                .or_else(read_target_cpu_env)
                .or_else(|| {
                    manifest_release_field_for(filename, output, |m| m.release_target_cpu.clone())
                }),
        );
        // Feature-string override — the sibling chain, resolved
        // independently (a flag-supplied CPU does not suppress a
        // manifest-supplied feature list, and vice versa).
        apply_target_features_override(target_features.map(str::to_string).or_else(|| {
            read_target_features_env().or_else(|| {
                manifest_release_field_for(filename, output, |m| m.release_target_features.clone())
            })
        }));
        let is_wasm = build_target == "wasm_wasi" || build_target == "wasm_browser";
        // Library-artifact producer mode (additive-interop Slice 2;
        // design.md § Exported C ABI) is native-only. A wasm build already
        // has its own producer surface — module exports selected by
        // `--bindings` (`crate::wasm_exports`) — so a `--crate-type
        // staticlib/cdylib` there is a category error, not a silent no-op.
        // Reject before any pipeline work (the `--target-cpu` fail-fast
        // posture).
        if is_wasm && crate_type != NativeCrateType::Bin {
            eprintln!(
                "error: --crate-type staticlib/cdylib is a native-only producer mode; \
                 for a wasm library surface use `--target={build_target}` with `--bindings` \
                 (the module-export path). See design.md § Exported C ABI."
            );
            process::exit(1);
        }
        // External native-library linking (`kara.toml` `[link]` table) —
        // native targets only (wasm-ld ignores it). Manifest-only, no
        // CLI/env tier; discovered by the same walk-up as the CPU/features
        // chains. Set before codegen so the linker invocation sees it.
        if !is_wasm {
            let (link_libs, link_search_paths) = manifest_link_config_for(filename, output);
            apply_native_link_config(link_libs, link_search_paths);
        }
        let effective_bindings = resolve_effective_bindings(build_target, bindings);
        // Hot-swap requires dynamic symbol resolution at runtime; a wasm
        // module has none. Same gate as project mode (the wasm half of
        // the phase-7 hot-swap target gating).
        if enable_hot_swap && is_wasm {
            eprintln!(
                "error: --enable-hot-swap is incompatible with --target={build_target} \
                 (no dynamic-symbol-resolution machinery on wasm hosts)"
            );
            process::exit(1);
        }
        // `--features wasm-threads` scope gate (phase-10 wasm-threads
        // entry). The flag is wasm_browser-only: wasi-threads (the
        // preview1-era host-threading ABI the threaded substrate builds
        // on) and the component model don't compose, and wasm_wasi's
        // default bindings are component — host-thread integration for
        // wasm_wasi is a design.md § WASM Concurrency Lowering future
        // concern, not a v1 surface. Same reasoning rejects an explicit
        // `--bindings=component` on a wasm_browser threaded build.
        validate_wasm_threads_scope(wasm_threads, build_target, effective_bindings);
        // Phase-10 WASM entry-point discovery: browser + component
        // bindings marshal rich exports (canonical-ABI trampolines);
        // `--bindings none` keeps raw core exports. Signal codegen before
        // it runs.
        crate::target::set_wasm_export_marshalling(matches!(
            effective_bindings,
            Some(BindingsMode::Browser) | Some(BindingsMode::Component)
        ));
        // Derive the output stem early — embedded component bindings
        // need it as the WIT package name before codegen runs.
        let exe_name = std::path::Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        // Embedded-WIT component bindings (phase-10 "embedded-WIT
        // migration"): resolve the external componentization tool up
        // front — a missing or mis-pinned wasm-tools fails before any
        // pipeline work, the `--target-cpu` fail-fast posture — and
        // install the package name that flips codegen's host-fn import
        // attachment to canonical-ABI `kara:<pkg>/host` naming. The
        // pin rides the same lazy manifest walk-up as the `[release]`
        // chain (`[toolchain] wasm-tools`).
        let wasm_tools = match effective_bindings {
            Some(BindingsMode::Component) => {
                let pin = manifest_release_field_for(filename, output, |m| {
                    m.toolchain_wasm_tools.clone()
                });
                match crate::componentize::resolve_wasm_tools(pin.as_deref()) {
                    Ok(tool) => {
                        crate::target::set_wasm_component_host_package(exe_name);
                        Some(tool)
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            }
            _ => None,
        };
        let source = read_source(filename);
        let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
        pipeline.resolve();
        pipeline.typecheck();
        pipeline.lower();
        pipeline.effectcheck();
        pipeline.ownershipcheck();
        // Auto-par codegen (slice 2): populate `pipeline.concurrency` so the
        // codegen call below picks up inferred parallel groups via
        // `Codegen::parallel_groups_for_current_fn`. `concurrencycheck` is a
        // no-op when `effectcheck` produced no result (`self.effects.is_none()`),
        // so phase ordering follows effects → ownership → concurrency.
        pipeline.concurrencycheck();

        // Slice D: emit the human-readable concurrency report before the
        // codegen / link stage so it lands on stdout next to the
        // `Built: <exe>` line, regardless of whether codegen later fails.
        if concurrency_report {
            emit_concurrency_report(&pipeline);
        }
        // SIMD lowering report (slice 5b) — same pre-codegen placement, so it
        // prints even when a `#[require_simd]` violation later aborts the build.
        if simd_report {
            emit_simd_report(&pipeline);
        }

        if pipeline.has_fatal_errors() {
            match output {
                OutputMode::Text => {
                    print_text_diagnostics(&pipeline);
                    process::exit(1);
                }
                OutputMode::Json => {
                    emit_json_output(&pipeline);
                    process::exit(1);
                }
                OutputMode::Jsonl => unreachable!(),
            }
        }

        // `#[require_simd]` guarantee (phase-7-codegen.md line 308, slice 5a):
        // a function annotated `#[require_simd]` must not contain any
        // `Vector[T, N]` op that would scalarize on the target. Checked after
        // a clean typecheck, before codegen — the analysis consumes the
        // `expr_types` side-table the typechecker populated. Aborts the build
        // (the `check` path surfaces the same diagnostics non-fatally through
        // `simd_check` in `run_all_checks`). Print only the SIMD diagnostics
        // here — effect/ownership/concurrency findings are non-fatal at this
        // build stage and are intentionally not surfaced by this abort.
        pipeline.simd_check();
        let simd_errors = pipeline.simd_errors.clone().unwrap_or_default();
        if !simd_errors.is_empty() {
            match output {
                OutputMode::Json => emit_json_output(&pipeline),
                OutputMode::Text | OutputMode::Jsonl => {
                    for e in &simd_errors {
                        eprintln!(
                            "error[E_REQUIRE_SIMD]: {}:{}:{} (in `{}`): {}",
                            filename,
                            e.span.line,
                            e.span.column,
                            e.func_name,
                            e.message(),
                        );
                        eprintln!("  help: {}", e.help());
                    }
                }
            }
            process::exit(1);
        }

        // Monomorphization budget (v1.x): per-generic instantiation
        // ceiling enforced after a clean typecheck, before codegen. A
        // disabled budget is a no-op; an error-level violation fails the
        // build here (sparing codegen work), warn-level emits a note and
        // continues. See phase-7-codegen.md line 266.
        if monomorphization_budget.is_enabled() {
            enforce_monomorphization_budget(&pipeline, &monomorphization_budget, output);
        }

        // Library-artifact C-ABI honesty gate (additive-interop Slice 4):
        // an exported signature whose return/params cross the boundary as
        // neither a transparent-by-value type nor an auto-boxable
        // `Vec`/`String` would emit a dishonest `KaraHandle` while codegen
        // returns/expects a multi-register aggregate — a silent miscompile.
        // Reject before codegen so the produced `.a`/`.so`/`.h` is always
        // ABI-honest. Only fires for a library build (the export IS the C
        // surface); an executable's `pub extern "C" fn` called only from
        // Kāra keeps the internal ABI.
        if crate_type != NativeCrateType::Bin {
            let export_errs = crate::cheader::validate_exports(&pipeline.parsed.program);
            if !export_errs.is_empty() {
                for (fn_name, reason) in &export_errs {
                    eprintln!("error[E_EXPORT_ABI]: exported `{fn_name}`: {reason}");
                }
                process::exit(1);
            }
        }

        // Phase-10: effect findings stay non-fatal for native builds (the
        // long-standing build/check asymmetry, documented at the `karac
        // run`-vs-effects tracker entry), but a target-gate violation
        // (E0411) on a wasm build is different in kind — it proves the
        // program reaches a host resource this target cannot provide, so
        // letting it through just converts a precise diagnostic into an
        // undefined-symbol linker error (or silent misbehavior). Abort
        // with the real message instead.
        if is_wasm {
            if let Some(ref effects) = pipeline.effects {
                let gate_errors: Vec<_> = effects
                    .errors
                    .iter()
                    .filter(|e| e.kind == EffectErrorKind::TargetGateViolation)
                    .collect();
                if !gate_errors.is_empty() {
                    for e in &gate_errors {
                        eprintln!(
                            "error[E0411]: {}:{}:{}: {}",
                            filename, e.span.line, e.span.column, e.message
                        );
                    }
                    process::exit(1);
                }
            }
        }

        // Output executable name — the stem derived before the
        // component-bindings setup above.
        // Scratch object path: `temp_dir()` + PID + stem, mirroring the
        // project-mode build (`cmd_build_project`). Keying on the stem alone
        // (`/tmp/karac_<stem>.o`) let two concurrent `karac build` invocations
        // with the same stem clobber each other's intermediate — a real race
        // for parallel build systems (`make -j`) and the cause of flaky
        // parallel `cargo test` wasm runs. PID disambiguates concurrent
        // processes (each invocation is its own process).
        let obj_path = std::env::temp_dir()
            .join(format!("karac_{}_{exe_name}.o", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let exe_path = if is_wasm {
            // WASI command module — the artifact is loaded by a wasm
            // host, never exec'd directly, so it always carries the
            // extension. (`dist/wasm/<pkg>.wasm` layout is project
            // mode's concern — the artifact-emission tracker entry.)
            format!("{exe_name}.wasm")
        } else if cfg!(windows) {
            format!("{exe_name}.exe")
        } else {
            exe_name.to_string()
        };

        if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
            &pipeline.parsed.program,
            &obj_path,
            pipeline.ownership.as_ref(),
            // WASM concurrency lowering (sequential default / wasm-threads)
            // is its own phase-10 entry — until it lands, suppress the
            // auto-par groups so wasm modules lower sequentially instead of
            // emitting spawn-site calls into a runtime archive that has no
            // scheduler.
            if is_wasm {
                None
            } else {
                pipeline.concurrency.as_ref()
            },
            Some(filename),
            Some(&source),
            enable_hot_swap,
            release,
            true, // A2: coroutines on for `karac build` (bug-C fix reaches real builds)
        ) {
            eprintln!("error: codegen failed: {e}");
            process::exit(1);
        }

        // Library-artifact producer mode (additive-interop Slice 2 + 3;
        // design.md § Exported C ABI). The emitted object carries the
        // program's `pub extern "C" fn` surface with External linkage +
        // bare C symbols; archive/link it into a `.a`/`.so`/`.dylib` and
        // emit the companion C header, instead of linking an executable.
        // Native-only (guaranteed: the wasm × crate-type combination was
        // rejected above). Returns from `cmd_build` — the wasm/exe link
        // tail below is `Bin`-only.
        if crate_type != NativeCrateType::Bin {
            // Export-boundary effect violations are FATAL for a library
            // artifact — the exported C surface IS the deliverable, so
            // unlike an executable (where native effect findings are
            // non-fatal, the long-standing build/check asymmetry) a
            // suspending export must stop the build rather than ship a
            // library that misbehaves on a bare foreign thread. (The
            // C-unwind-export case is already caught earlier at codegen.)
            if let Some(effects) = pipeline.effects.as_ref() {
                let export_errs: Vec<_> = effects
                    .errors
                    .iter()
                    .filter(|e| e.kind == EffectErrorKind::ExternExportSuspendsUnsupported)
                    .collect();
                if !export_errs.is_empty() {
                    for e in &export_errs {
                        eprintln!(
                            "error[E0414]: {}:{}:{}: {}",
                            filename, e.span.line, e.span.column, e.message
                        );
                    }
                    let _ = std::fs::remove_file(&obj_path);
                    process::exit(1);
                }
            }
            let lib_kind = match crate_type {
                NativeCrateType::StaticLib => crate::codegen::NativeLibKind::StaticLib,
                NativeCrateType::CDylib => crate::codegen::NativeLibKind::CDylib,
                NativeCrateType::Bin => unreachable!(),
            };
            // Default artifact path: `lib<stem>.<ext>` in CWD — a name
            // distinct from the `<stem>` executable, so a library build
            // never clobbers a stray binary (the producer-mode gotcha).
            // `-o <path>` overrides verbatim.
            let default_name = format!("lib{exe_name}{}", lib_kind.artifact_extension());
            let art_path = out_path.map(str::to_string).unwrap_or(default_name);
            // Symbols the artifact must publish — needed for the Windows DLL
            // `/EXPORT:` list (a no-op on unix, which exports every
            // default-visibility symbol). AST-derived so it stays in lockstep
            // with the emitted C header.
            let export_syms = crate::cheader::export_symbols(&pipeline.parsed.program);
            if let Err(e) = crate::codegen::link_native_library(
                &obj_path,
                &art_path,
                lib_kind,
                exe_name,
                &export_syms,
            ) {
                eprintln!("error: link failed: {e}");
                let _ = std::fs::remove_file(&obj_path);
                process::exit(1);
            }
            let _ = std::fs::remove_file(&obj_path);
            // Emit the companion C header next to the artifact (Slice 3):
            // `<artifact-dir>/lib<stem>.h`. `--no-header` is a follow-up;
            // at this slice the header always rides along.
            let header_path = std::path::Path::new(&art_path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|dir| dir.join(format!("lib{exe_name}.h")))
                .unwrap_or_else(|| std::path::PathBuf::from(format!("lib{exe_name}.h")));
            let header = crate::cheader::emit_c_header(&pipeline.parsed.program, exe_name);
            match std::fs::write(&header_path, header) {
                Ok(()) => {
                    println!("Built: {art_path}");
                    println!("Built: {}", header_path.display());
                }
                Err(e) => {
                    eprintln!(
                        "warning: library `{art_path}` built, but writing the C header to {} failed: {e}",
                        header_path.display()
                    );
                    println!("Built: {art_path}");
                }
            }
            print_staticlib_rust_host_note(crate_type);
            return;
        }

        // For embedded component bindings, wasm-ld's output is an
        // intermediate — link the C-ABI core module to a scratch path,
        // then lift it into the single component at `exe_path` below. The
        // scratch basename is source-derived (not pid-bearing) so the
        // module name wasm-ld embeds — and the component carries — is
        // reproducible across rebuilds (B-2026-06-22-3); the enclosing
        // dir carries the per-process uniqueness.
        let (link_scratch_dir, link_out) = if wasm_tools.is_some() {
            match crate::componentize::link_core_scratch(exe_name) {
                Ok((dir, core)) => (Some(dir), core.to_string_lossy().into_owned()),
                Err(e) => {
                    eprintln!("error: link failed: {e}");
                    let _ = std::fs::remove_file(&obj_path);
                    process::exit(1);
                }
            }
        } else {
            (None, exe_path.clone())
        };
        let wasm_export_names =
            crate::wasm_exports::link_export_names(&crate::wasm_exports::collect_wasm_exports(
                &pipeline.parsed.program,
                crate::target::active_target(),
            ));
        match crate::codegen::link_executable_exports(&obj_path, &link_out, &wasm_export_names) {
            Err(e) => {
                eprintln!("error: link failed: {e}");
                let _ = std::fs::remove_file(&obj_path);
                process::exit(1);
            }
            Ok(()) => {
                let _ = std::fs::remove_file(&obj_path);
                if let Some(tool) = &wasm_tools {
                    let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
                    let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                        &pipeline.parsed.program,
                        crate::target::active_target(),
                    );
                    warn_unlowered_exports(
                        &wasm_exports,
                        crate::wasm_exports::ExportSig::component_lowerable,
                    );
                    let result = crate::componentize::componentize(
                        tool,
                        std::path::Path::new(&link_out),
                        &host_fns,
                        &wasm_exports,
                        exe_name,
                        std::path::Path::new(&exe_path),
                    );
                    if let Some(dir) = &link_scratch_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    if let Err(e) = result {
                        eprintln!("error: componentize failed: {e}");
                        process::exit(1);
                    }
                }
                // Companion artifacts keyed on the resolved bindings
                // mode — not the target name: `--target=wasm_browser
                // --bindings=none` suppresses them (raw module) and
                // `--target=wasm_wasi --bindings=browser` opts a wasi
                // module in (browser/none both lower host fns to
                // the same `kara_host` import entries, so each
                // companion is target-agnostic). Browser bindings ship
                // the ES-module glue (host fn import plumbing + WASI
                // preview-1 polyfill; see `wasm_glue`) plus its
                // TypeScript declarations; embedded component bindings
                // ship NO companion — `<stem>.wasm` is the single
                // self-describing component. The `(json key, path)`
                // pairs feed both output modes.
                let mut companions: Vec<(&str, String)> = Vec::new();
                // `--features wasm-threads`: the dual artifact's second
                // pass — same front-end output, auto-par re-enabled,
                // wasip1-threads machine, --shared-memory link against
                // the threaded runtime archive. Runs after the
                // sequential link so a clean build always has the
                // fallback module on disk first.
                let threads_glue_cfg = if wasm_threads {
                    let threads_filename = format!("{exe_name}.threads.wasm");
                    let threads_obj = std::env::temp_dir()
                        .join(format!("karac_{}_{exe_name}.threads.o", std::process::id()));
                    let cfg = emit_wasm_threads_artifact(
                        &pipeline.parsed.program,
                        pipeline.ownership.as_ref(),
                        pipeline.concurrency.as_ref(),
                        Some(filename),
                        Some(&source),
                        release,
                        &threads_obj.to_string_lossy(),
                        std::path::Path::new(&threads_filename),
                        &threads_filename,
                        manifest_wasm_knobs_for(filename, output),
                    );
                    companions.push(("threads_wasm", threads_filename));
                    Some(cfg)
                } else {
                    None
                };
                match effective_bindings {
                    Some(BindingsMode::Browser) => {
                        let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
                        let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                            &pipeline.parsed.program,
                            crate::target::active_target(),
                        );
                        warn_unlowered_exports(
                            &wasm_exports,
                            crate::wasm_exports::ExportSig::component_lowerable,
                        );
                        let glue = crate::wasm_glue::render_glue(
                            &host_fns,
                            &wasm_exports,
                            &exe_path,
                            threads_glue_cfg.as_ref(),
                        );
                        let js_path = format!("{exe_name}.js");
                        if let Err(e) = std::fs::write(&js_path, glue) {
                            eprintln!("error: failed to write JS glue {js_path}: {e}");
                            process::exit(1);
                        }
                        companions.push(("glue", js_path));
                        let dts = crate::wasm_glue::render_dts(
                            &host_fns,
                            &wasm_exports,
                            &exe_path,
                            threads_glue_cfg.is_some(),
                        );
                        let dts_path = format!("{exe_name}.d.ts");
                        if let Err(e) = std::fs::write(&dts_path, dts) {
                            eprintln!("error: failed to write TS declarations {dts_path}: {e}");
                            process::exit(1);
                        }
                        companions.push(("dts", dts_path));
                    }
                    Some(BindingsMode::Component) | Some(BindingsMode::None) | None => {}
                }
                // Strip DWARF debug info from emitted .wasm artifacts. wasm-ld
                // keeps the `.debug_*` custom sections (the native link path
                // strips by default; the wasm path does not), and they are
                // ~90%+ of an unstripped module — a 482 KiB browser hello is
                // 93% DWARF, collapsing to ~30 KiB. Strip by default for every
                // wasm artifact (the main module/component plus any
                // `.threads.wasm` sibling); `KARAC_WASM_KEEP_DEBUG=1` opts out
                // for source-level wasm debugging. Best-effort: Component
                // bindings already resolved+required the tool above; for
                // browser/raw builds resolve it lazily here, and a missing or
                // failed strip is a warning, never a build failure.
                if is_wasm && std::env::var_os("KARAC_WASM_KEEP_DEBUG").is_none() {
                    let strip_tool = wasm_tools.clone().or_else(|| {
                        let pin = manifest_release_field_for(filename, output, |m| {
                            m.toolchain_wasm_tools.clone()
                        });
                        crate::componentize::resolve_wasm_tools(pin.as_deref()).ok()
                    });
                    match strip_tool {
                        Some(tool) => {
                            let mut artifacts = vec![exe_path.clone()];
                            artifacts.extend(
                                companions
                                    .iter()
                                    .filter(|(k, _)| *k == "threads_wasm")
                                    .map(|(_, p)| p.clone()),
                            );
                            for artifact in &artifacts {
                                if let Err(e) = crate::componentize::strip_debug(
                                    &tool,
                                    std::path::Path::new(artifact),
                                ) {
                                    eprintln!(
                                        "warning: wasm debug-strip skipped for {artifact}: {e}"
                                    );
                                }
                            }
                        }
                        None => eprintln!(
                            "note: wasm-tools not found — emitted .wasm retains debug info \
                             (install wasm-tools for ~10x smaller modules, or set \
                             KARAC_WASM_KEEP_DEBUG=1 to silence this note)"
                        ),
                    }
                }
                match output {
                    OutputMode::Text => {
                        let mut line = format!("Built: {exe_path}");
                        for (_, path) in &companions {
                            line.push_str(&format!(" + {path}"));
                        }
                        println!("{line}");
                    }
                    OutputMode::Json => {
                        let mut fields = format!("{{\"status\":\"ok\",\"output\":\"{exe_path}\"");
                        for (key, path) in &companions {
                            fields.push_str(&format!(",\"{key}\":\"{path}\""));
                        }
                        fields.push('}');
                        println!("{fields}");
                    }
                    OutputMode::Jsonl => unreachable!(),
                }
            }
        }
    }
    #[cfg(not(feature = "llvm"))]
    {
        let _ = build_target;
        let _ = enable_hot_swap;
        // `--bindings` only shapes WASM artifact emission, which rides
        // the llvm build path — accepted-but-inert here, consistent
        // with --offline / --target above.
        let _ = bindings;
        // `--target-cpu` / `--target-features` only parameterize the
        // LLVM target machine — accepted-but-inert on the non-llvm
        // check fallback.
        let _ = target_cpu;
        let _ = target_features;
        // `--release` only affects codegen (contract stripping), which the
        // non-llvm fallback doesn't reach — accepted-but-inert, consistent
        // with --offline / --target / --enable-hot-swap above.
        let _ = release;
        // The budget check rides the llvm build path (after typecheck,
        // before codegen); the non-llvm fallback type-checks only, so the
        // flag is accepted-but-inert here, consistent with --offline /
        // --target.
        let _ = monomorphization_budget;
        // `--features wasm-threads` shapes WASM codegen+link, which the
        // non-llvm fallback doesn't reach — accepted-but-inert, the
        // `--bindings` posture.
        let _ = wasm_threads;
        // `--crate-type staticlib/cdylib` + `-o` drive the producer-mode
        // library link path, which rides the llvm build — accepted-but-
        // inert on the non-llvm check fallback.
        let _ = crate_type;
        let _ = out_path;
        eprintln!("note: karac build requires the llvm feature; falling back to type check");
        cmd_check(
            filename,
            output,
            None,
            None,
            concurrency_report,
            simd_report,
            lint_overrides,
        );
    }
}

/// Enforce a `--monomorphization-budget` ceiling after typecheck. Human-
/// readable `warning[monomorphization-budget]` / `error[…]` diagnostics
/// go to stderr (keeping stdout reserved for the build result). Any
/// error-level violation fails the build with status 1 before codegen
/// runs — in JSON mode it also emits a diagnostics envelope on stdout,
/// mirroring the `has_fatal_errors` JSON path. The caller gates on
/// `is_enabled`, so a disabled budget never reaches here.
#[cfg(feature = "llvm")]
fn enforce_monomorphization_budget(
    pipeline: &Pipeline,
    budget: &crate::monomorphization::MonomorphizationBudget,
    output: OutputMode,
) {
    use crate::monomorphization::{BudgetLevel, BudgetViolation};

    let Some(tc) = pipeline.typed.as_ref() else {
        return;
    };
    let table =
        crate::monomorphization::analyze(&pipeline.parsed.program, tc, pipeline.effects.as_ref());
    let violations = table.budget_violations(budget);
    if violations.is_empty() {
        return;
    }

    let render = |v: &BudgetViolation| {
        let kind = match v.level {
            BudgetLevel::Error => "error",
            BudgetLevel::Warning => "warning",
        };
        format!(
            "{kind}[monomorphization-budget]: {}:{}:{}: generic `{}` has {} instantiations (limit {})",
            pipeline.filename, v.site.line, v.site.column, v.generic, v.count, v.threshold
        )
    };

    // Human-readable diagnostics (warnings and errors alike) always go to
    // stderr so stdout stays reserved for the single build-result line.
    for v in &violations {
        eprintln!("{}", render(v));
    }

    let errors: Vec<&BudgetViolation> = violations
        .iter()
        .filter(|v| v.level == BudgetLevel::Error)
        .collect();
    if errors.is_empty() {
        // Warn-only: the build continues to codegen.
        return;
    }

    match output {
        OutputMode::Text => process::exit(1),
        OutputMode::Json => {
            let diags: Vec<String> = errors
                .iter()
                .map(|v| {
                    format!(
                        "{{\"severity\":\"error\",\"phase\":\"monomorphization-budget\",\"generic\":{},\"count\":{},\"limit\":{},\"site\":{}}}",
                        json_string(&v.generic),
                        v.count,
                        v.threshold,
                        json_string(&format!(
                            "{}:{}:{}",
                            pipeline.filename, v.site.line, v.site.column
                        )),
                    )
                })
                .collect();
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{}]}}",
                diags.join(",")
            );
            process::exit(1);
        }
        OutputMode::Jsonl => unreachable!(),
    }
}

/// Project-mode build entry point.
///
/// Discovers the project root via `kara.toml` walk-up, loads the manifest,
/// walks `src/` to map each `.kara` file to a module path (CR-24 slice 3),
/// parses every file into its own `Program`, assembles the module graph
/// Render documentation for the current project. v1 MVP — walks the
/// project tree, parses every module, and emits one HTML page per
/// documented item under `dist/doc/`. Items without `///` doc comments
/// are skipped silently. Resolver / typechecker passes are intentionally
/// not run: doc rendering only needs the AST surface, and producing
/// docs against a project that doesn't fully type-check is useful for
/// a programmer trying to understand half-finished code.
fn cmd_doc() {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, _mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let walked = match walker::walk_project(&root, WalkerOpts::default()) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let built = match module::build_program_tree(&walked) {
        Ok(ok) => ok,
        Err(e) => {
            emit_build_tree_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let BuildTreeOk { tree, parse_errors } = built;
    if !parse_errors.is_empty() {
        // Surface parse errors but keep going — render docs for what
        // parsed cleanly. The user can iterate.
        print_parse_errors_text(&parse_errors);
    }

    // Run effectcheck once on a merged Program containing every
    // non-synthetic module's items so cross-module callee resolution
    // works at the bare-name level the effectchecker indexes by. See
    // `build_doc_effects_table` for the trade-offs.
    let effects = build_doc_effects_table(&tree);

    let output_dir = root.join("dist").join("doc");
    match crate::doc::build_docs(&tree, &output_dir, Some(&effects)) {
        Ok(result) => {
            println!(
                "rendered {} doc page(s) under {}",
                result.written.len().saturating_sub(1), // minus the index
                output_dir.display()
            );
        }
        Err(e) => {
            eprintln!("error[doc]: {e}");
            process::exit(1);
        }
    }
}

/// Build the `(module_path, fn_name) → EffectDisplay` table consumed
/// by the doc renderer.
///
/// Strategy: merge every non-synthetic module's items into a single
/// `Program` and run `effectcheck` once. The effectchecker indexes
/// functions by bare name, and cross-module call sites also resolve
/// to bare names (Kāra's `import` brings a callee into scope under
/// its bare name). A per-module check would treat every cross-module
/// call as effect-empty — `pub fn`s whose inferred set depends on a
/// callee in another module would surface incomplete `with` clauses
/// in the rendered docs.
///
/// Trade-off: when two modules define functions with the same bare
/// name, the merge keeps only one and the doc display is approximate.
/// This is doc-only; the main pipeline (`build`, `check`, `run`)
/// still runs effectcheck per-module via the regular phase wiring.
/// Effectcheck errors raised by the merged pass (e.g. duplicate
/// resource declarations across modules, missing effect declarations)
/// are deliberately ignored here — the doc renderer is best-effort.
fn build_doc_effects_table(tree: &ProgramTree) -> crate::doc::EffectsByItem {
    use crate::ast::Item;
    use crate::doc::{EffectDisplay, EffectsByItem};
    use crate::effectchecker::{DeclaredEffects, EffectSet};

    let mut merged_items = Vec::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        merged_items.extend(module.items.iter().cloned());
    }
    let merged_program = Program {
        items: merged_items,
        ..Program::default()
    };
    let effects = crate::effectcheck(&merged_program);

    let mut out: EffectsByItem = std::collections::HashMap::new();
    for module in &tree.modules {
        if module.is_synthetic {
            continue;
        }
        for item in &module.items {
            let Item::Function(f) = item else { continue };
            if !f.is_pub {
                continue;
            }

            // Prefer the declared annotation (the user's contract);
            // fall back to the inferred set if no explicit annotation.
            let display = match effects.declared_effects.get(&f.name) {
                Some(DeclaredEffects::Explicit(set)) => effect_set_to_display(set, false),
                Some(DeclaredEffects::Polymorphic) => EffectDisplay {
                    effects: Vec::new(),
                    polymorphic: true,
                },
                Some(DeclaredEffects::PolymorphicWithFixed(set)) => {
                    effect_set_to_display(set, true)
                }
                Some(DeclaredEffects::None) | None => effects
                    .inferred_effects
                    .get(&f.name)
                    .map(|set: &EffectSet| effect_set_to_display(set, false))
                    .unwrap_or_default(),
            };

            if !display.effects.is_empty() || display.polymorphic {
                out.insert((module.path.clone(), f.name.clone()), display);
            }
        }
    }

    out
}

fn effect_set_to_display(
    set: &crate::effectchecker::EffectSet,
    polymorphic: bool,
) -> crate::doc::EffectDisplay {
    let mut effects: Vec<(crate::ast::EffectVerbKind, String)> = set
        .effects
        .iter()
        .map(|t| (t.effect.verb.clone(), t.effect.resource.clone()))
        .collect();
    // Stable order across runs: by verb name, then resource.
    effects.sort_by(|a, b| {
        let an = effect_verb_str(&a.0);
        let bn = effect_verb_str(&b.0);
        an.cmp(bn).then_with(|| a.1.cmp(&b.1))
    });
    crate::doc::EffectDisplay {
        effects,
        polymorphic,
    }
}

/// (slice 4), runs Tarjan's SCC to reject circular module dependencies
/// (`E0223`), and runs cross-module name resolution per module
/// (slice 5, `E0224` / `E0225`). Visibility enforcement and typechecking
/// across modules arrive in slice 6+.
// Same flag-shaped-argument posture as `cmd_build` above — a struct
// here would just move the flag list rather than tighten it.
#[allow(clippy::too_many_arguments)]
fn cmd_build_project(
    output: OutputMode,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
    bindings: Option<BindingsMode>,
    target_cpu: Option<&str>,
    target_features: Option<&str>,
    wasm_threads: bool,
    release: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
) {
    // Phase-10: v1 target names are classified the same way as in
    // single-file mode. A wasm name selects the project-mode WASM build:
    // super-program codegen → wasm-ld → the `dist/wasm/<pkg>.wasm`
    // artifact layout (+ `<pkg>.js` / `<pkg>.d.ts` under browser
    // bindings — the "WASM browser artifact emission" entry). Triples
    // pass through to the manifest `[target.<triple>.*]` overlay merge
    // below unchanged.
    let build_target = resolve_build_target(target);
    let is_wasm = build_target == "wasm_wasi" || build_target == "wasm_browser";
    let effective_bindings = resolve_effective_bindings(build_target, bindings);
    // Hot-swap requires dynamic symbol resolution at runtime; a wasm
    // module has none (no dlopen in a browser/WASI host). This is the
    // wasm half of the phase-7 hot-swap target gating, actionable now
    // that `--target=wasm_*` reaches project mode.
    if enable_hot_swap && is_wasm {
        eprintln!(
            "error: --enable-hot-swap is incompatible with --target={build_target} \
             (no dynamic-symbol-resolution machinery on wasm hosts)"
        );
        process::exit(1);
    }
    // `--features wasm-threads` scope gate — single-file contract
    // (see `validate_wasm_threads_scope`): wasm_browser-only, no
    // component bindings. Runs pre-manifest so the failure mode is
    // identical from any directory — and llvm-independent, so a
    // non-llvm build rejects the flag here rather than tripping the
    // manifest-not-found check below.
    validate_wasm_threads_scope(wasm_threads, build_target, effective_bindings);
    // Phase-10 WASM entry-point discovery: browser + component bindings
    // marshal rich exports (canonical-ABI trampolines); `--bindings none`
    // keeps raw core exports. Signal codegen before it runs.
    crate::target::set_wasm_export_marshalling(matches!(
        effective_bindings,
        Some(BindingsMode::Browser) | Some(BindingsMode::Component)
    ));
    // `--target-cpu=help` / `--target-features=help` exit before
    // manifest discovery so the listing works from any directory — it
    // needs only the active target, not a project. Name validation for
    // a real value waits until the manifest is loaded (the `[release]`
    // tier of each precedence chain lives there); see below.
    #[cfg(feature = "llvm")]
    if target_cpu == Some("help") || target_features == Some("help") {
        crate::codegen::print_target_cpu_listing();
        process::exit(0);
    }
    // --offline implies --no-proxy at the contract level (vendor-only walk
    // can't talk to the proxy). Suppress the redundant no-proxy note when
    // both are set so the offline operator sees one clean status line.
    if !offline {
        emit_no_proxy_note(no_proxy);
    }
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, raw_manifest) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, output);
            process::exit(1);
        }
    };

    // Toolchain pin (tracker line 892). When `karac-toolchain.toml`
    // exists somewhere in the project's ancestor chain, the active
    // compiler version must satisfy the declared constraint. Halts
    // the build with a focused diagnostic on mismatch — no auto-
    // switch (that's the karaup follow-up).
    if !enforce_toolchain_pin(&root, output) {
        process::exit(1);
    }

    // Resolve the active target triple for `[target.<triple>.*]` overlay
    // selection (tracker line 882). Precedence: `--target=<triple>` >
    // `[build].target` > host triple. Recorded as a single owned value
    // so the overlay merge consumes a stable reference. A v1 target
    // *name* is not a triple: a wasm name pins the overlay triple to
    // the real compilation triple (`wasm32-wasip1` — both wasm names
    // build the same module flavor), and an explicit `native` pins the
    // host triple (an explicit flag outranks `[build].target`, the
    // chain's documented precedence).
    let active_target: String = match target {
        Some(t) if !crate::target::is_v1_target_name(t) => t.to_string(),
        Some(_) if is_wasm => "wasm32-wasip1".to_string(),
        Some(_) => crate::build_cache::host_target_triple(),
        None => raw_manifest
            .build_default_target
            .clone()
            .unwrap_or_else(crate::build_cache::host_target_triple),
    };

    // Merge `[target.<triple>].dependencies` / `[target.<triple>].profile`
    // overlays onto the manifest before any downstream consumer reads it
    // (dep resolution, profile gating, codegen). Always applied with the
    // resolved active triple so the build sees one consistent view.
    let mf = manifest::merge_target_overlay(&raw_manifest, Some(active_target.as_str()));

    // CPU baseline override — same precedence chain as single-file mode
    // (flag > `KARAC_TARGET_CPU` > `[release] target-cpu`), with the
    // manifest tier read from the project's own manifest (already
    // loaded) instead of a file-relative walk-up. Installed before
    // codegen runs; `help` was handled above, pre-discovery.
    #[cfg(feature = "llvm")]
    apply_target_cpu_override(
        target_cpu
            .map(str::to_string)
            .or_else(read_target_cpu_env)
            .or_else(|| mf.release_target_cpu.clone()),
    );
    // Feature-string override — the independent sibling chain.
    #[cfg(feature = "llvm")]
    apply_target_features_override(
        target_features
            .map(str::to_string)
            .or_else(read_target_features_env)
            .or_else(|| mf.release_target_features.clone()),
    );
    #[cfg(not(feature = "llvm"))]
    let _ = (target_cpu, target_features, out_path);

    // External native-library linking (`[link]` table) — native targets
    // only (wasm-ld ignores it). Read from the project's own manifest
    // (already loaded), no walk-up. Installed before codegen runs.
    #[cfg(feature = "llvm")]
    if !is_wasm {
        apply_native_link_config(mf.link_libs.clone(), mf.link_search_paths.clone());
    }

    // Embedded-WIT component bindings — the single-file `cmd_build`
    // contract: resolve the external wasm-tools up front (failing fast
    // on missing/mis-pinned, pin from the project's own `[toolchain]`
    // table — already loaded, no walk-up needed) and install the
    // package name that flips codegen's host-fn import attachment to
    // canonical-ABI `kara:<pkg>/host` naming. Runtime-
    // gated on the llvm feature: the non-llvm fallback builds nothing,
    // so a missing tool must not fail what is effectively a check run.
    let wasm_tools = if cfg!(feature = "llvm") {
        match effective_bindings {
            Some(BindingsMode::Component) => {
                match crate::componentize::resolve_wasm_tools(mf.toolchain_wasm_tools.as_deref()) {
                    Ok(tool) => {
                        crate::target::set_wasm_component_host_package(&mf.name);
                        Some(tool)
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // Phase-7 line 5 sub-item 3 — target gating. Hot-swap requires dynamic
    // symbol resolution at runtime, which embedded and kernel profiles
    // do not provide. Reject the combination before any work.
    // The wasm-target half of the entry's gating defers until a wasm
    // CompileProfile (or `--target=`) lands; no enum variant to gate
    // against yet. Reads `mf.profile` post-overlay so a target-specific
    // override is honored here.
    if enable_hot_swap
        && matches!(
            mf.profile,
            crate::manifest::CompileProfile::Embedded | crate::manifest::CompileProfile::Kernel
        )
    {
        eprintln!(
            "error: --enable-hot-swap is incompatible with [package].profile = \"{}\" (no dynamic-symbol-resolution machinery on this profile)",
            mf.profile.as_str()
        );
        process::exit(1);
    }

    // Offline-mode pre-check: the vendor root must exist before the
    // resolver consults it. A missing `./vendor/` is a clear operator
    // mistake — the right action is "run `karac vendor`", not "fix
    // every transitive dep". Skipped when the manifest declares no deps
    // and no MSRV constraint — solo projects pay nothing for `--offline`.
    let has_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let vendor_root_buf = root.join("vendor");
    if offline && has_deps && !vendor_root_buf.is_dir() {
        emit_offline_no_vendor_dir(&vendor_root_buf, output);
        process::exit(1);
    }
    let offline_root: Option<&std::path::Path> = if offline {
        Some(vendor_root_buf.as_path())
    } else {
        None
    };

    // Slice 7 of the PubGrub-resolver entry: validate the dep graph
    // before the walker even runs. Errors halt the build; unsupported-
    // source warnings (registry/git, until fetch ships at line 819)
    // surface as notices and the build continues. Skipped entirely when
    // the manifest declares no deps and no MSRV constraint — the common
    // single-package, no-dep case pays zero overhead.
    // Build mode: dev-dependencies are excluded from resolution
    // (tracker line 884). The test runner re-invokes resolution with
    // `include_dev_deps=true` so `[dev-dependencies]` surface only
    // when actually compiling tests.
    let dep_resolution: Option<crate::dep_resolver::Resolution> = if has_deps {
        match run_dep_resolution(
            &root,
            mf.clone(),
            output,
            offline_root,
            false,
            no_proxy,
            true,
        ) {
            Ok(r) => r,
            Err(()) => process::exit(1),
        }
    } else {
        None
    };

    // Project-mode platform-suffix selection must follow the *build* target,
    // not the host. A `--target=wasm_*` build has to select `_wasm` platform
    // modules (and drop `_macos`/`_linux`/`_windows`), exactly as a single-file
    // cross-target build does — otherwise a wasm build wrongly compiles the
    // host's native platform modules (and omits the wasm ones), so an example
    // that swaps its host/IO layer per target via platform suffixes builds the
    // wrong half. Native builds keep the host platform; cross-triple native
    // selection is a separate concern that `host()` preserves unchanged.
    let walk_opts = WalkerOpts {
        target: if is_wasm {
            walker::Platform::Wasm
        } else {
            walker::Platform::host()
        },
        ..WalkerOpts::default()
    };
    let walked = match walker::walk_project(&root, walk_opts) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, output);
            process::exit(1);
        }
    };

    // Cross-package module loading (phase-5 line 898): walk each resolved
    // path-dep's source tree so its modules join the program tree under
    // package-prefixed paths, making `import <pkg>.…` resolve.
    let dep_walks = match dep_package_walks(dep_resolution.as_ref(), walk_opts.target, output) {
        Ok(v) => v,
        Err(()) => process::exit(1),
    };

    let built = match module::build_program_tree_with_deps(
        &walked,
        &dep_walks,
        module::BuildTreeOpts::default(),
    ) {
        Ok(ok) => ok,
        Err(e) => {
            emit_build_tree_error(&e, output);
            process::exit(1);
        }
    };

    let BuildTreeOk { tree, parse_errors } = built;

    let cycles = module::detect_cycles(&tree);

    // Slice 5: run cross-module name resolution per module. Only attempt
    // resolution when the graph is acyclic and every file parsed cleanly —
    // otherwise we would cascade dozens of spurious E0224/E0225s atop the
    // real failure.
    let resolve_errors: Vec<ModuleResolveErrors> = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree)
    } else {
        Vec::new()
    };

    // Slice 6 (follow-up): run the typechecker per module with the project
    // tree attached so cross-module `E0221` and the CR-18 field-access rule
    // can fire. Skipped when earlier phases reported errors, since a half-
    // resolved program produces unhelpful type cascades.
    // Phase-8 line 49 prereq 4 — lift `[lints].allow_unstable_api`
    // from the manifest into a per-module `CliLintOverrides` so the
    // project-build typecheck honors the global opt-in. Today this
    // is the only manifest-driven lint override; future fields land
    // beside it.
    let mut module_lint_overrides = crate::lints::CliLintOverrides::default();
    module_lint_overrides.apply_manifest_lints(&mf.lints);
    let type_errors: Vec<ModuleTypeErrors> =
        if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
            typecheck_modules(&tree, &module_lint_overrides)
        } else {
            Vec::new()
        };

    // Theme 4 (2026-05-10) — multi-file project-mode codegen. Per-module
    // resolve + typecheck above produce per-file diagnostics; once those
    // pass, the codegen path concatenates all module items (in topological
    // order, dropping `import` declarations + the synthetic prelude) into a
    // single super-program and drives it through the existing single-file
    // pipeline (`lower` → `effectcheck` → `ownershipcheck` →
    // `concurrencycheck` → codegen → link). Per-module wiring of the post-
    // typecheck phases would lose cross-module callee-effect visibility
    // (concurrency analysis depends on knowing imported functions' effects);
    // the super-program approach gives correct cross-module analysis at the
    // cost of less granular file-context in late-phase diagnostics. Symbol
    // mangling deferred to v2 — cross-module function-name collisions
    // surface as duplicate-symbol errors at the LLVM linker (clear failure,
    // ungainly diagnostic; structured detection is a follow-up).
    // Resolve the effective library-artifact kind (additive-interop Slice 2,
    // project-mode `[lib]`): a CLI `--crate-type staticlib/cdylib` wins; else
    // the manifest `[lib] crate-type`; else an executable. A bare/omitted
    // `bin` CLI flag reads as "unset" and falls to the manifest — a project
    // with a `[lib]` table builds a library by default.
    let effective_crate_type = if crate_type != NativeCrateType::Bin {
        crate_type
    } else {
        match mf.lib_crate_type.as_deref() {
            Some("staticlib") => NativeCrateType::StaticLib,
            Some("cdylib") => NativeCrateType::CDylib,
            _ => NativeCrateType::Bin,
        }
    };
    // Library-artifact mode is native-only (single-file posture).
    if is_wasm && effective_crate_type != NativeCrateType::Bin {
        eprintln!(
            "error: --crate-type staticlib/cdylib (or a `[lib]` table) is a native-only producer \
             mode; for a wasm library surface use `--target={build_target}` with `--bindings`."
        );
        process::exit(1);
    }
    let mut codegen_status: BuildCodegenStatus = BuildCodegenStatus::Skipped;
    if !cfg!(feature = "llvm") {
        // Mirror the single-file `cmd_build` no-llvm fallback (line ~2393).
        codegen_status = BuildCodegenStatus::NoLlvmFeature;
    } else if parse_errors.is_empty()
        && cycles.is_empty()
        && resolve_errors.is_empty()
        && type_errors.is_empty()
    {
        codegen_status = run_multi_file_codegen(
            &tree,
            &mf,
            &root,
            enable_hot_swap,
            release,
            is_wasm,
            effective_bindings,
            wasm_tools.as_ref(),
            wasm_threads,
            effective_crate_type,
            out_path,
        );
    }

    let failed = !parse_errors.is_empty()
        || !cycles.is_empty()
        || !resolve_errors.is_empty()
        || !type_errors.is_empty()
        || matches!(codegen_status, BuildCodegenStatus::Failed { .. });

    match output {
        OutputMode::Text => {
            for w in &mf.warnings {
                eprintln!("warning[manifest]: {}", w.message);
            }
            print_parse_errors_text(&parse_errors);
            print_cycles_text(&cycles, &tree);
            print_resolve_errors_text(&resolve_errors);
            print_type_errors_text(&type_errors);
            println!("project: {}", mf.name);
            println!("edition: {}", mf.edition);
            println!("root:    {}", root.display());
            println!("target:  {}", walk_opts.target.as_suffix());
            println!("entry:   {}", entry_label(walked.entry));
            println!("modules: {}", walked.modules.len());
            for m in &walked.modules {
                let path = if m.path.is_empty() {
                    "<crate root>".to_string()
                } else {
                    m.path.join(".")
                };
                let plat = match m.platform {
                    Some(p) => format!(" [{}]", p.as_suffix()),
                    None => String::new(),
                };
                println!("  {path}{plat}  {}", m.file.display());
            }
            if !dep_walks.is_empty() {
                let dep_module_count: usize =
                    dep_walks.iter().map(|d| d.walked.modules.len()).sum();
                println!(
                    "deps:    {} package(s), {} module(s)",
                    dep_walks.len(),
                    dep_module_count
                );
                for d in &dep_walks {
                    println!("  {}  {}", d.name, d.walked.src_dir.display());
                }
            }
            if failed {
                let total = parse_errors.iter().map(|pe| pe.errors.len()).sum::<usize>()
                    + cycles.len()
                    + resolve_errors
                        .iter()
                        .map(|re| re.errors.len())
                        .sum::<usize>()
                    + type_errors.iter().map(|te| te.errors.len()).sum::<usize>()
                    + codegen_status.error_count();
                if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                    eprintln!("error[{phase}]: {message}");
                }
                eprintln!("\n{total} error(s) found.");
                process::exit(1);
            }
            match &codegen_status {
                BuildCodegenStatus::Built {
                    exe_path,
                    glue_path,
                    dts_path,
                    threads_wasm_path,
                } => {
                    let mut line = format!("Built: {}", exe_path.display());
                    for extra in [threads_wasm_path, glue_path, dts_path]
                        .into_iter()
                        .flatten()
                    {
                        line.push_str(&format!(" + {}", extra.display()));
                    }
                    println!("{line}");
                }
                BuildCodegenStatus::NoLlvmFeature => {
                    eprintln!(
                        "note: karac build requires the llvm feature; project type-checked but no executable was produced."
                    );
                }
                BuildCodegenStatus::Skipped | BuildCodegenStatus::Failed { .. } => {}
            }
        }
        OutputMode::Json => {
            let warnings: Vec<String> = mf
                .warnings
                .iter()
                .map(|w| {
                    format!(
                        "{{\"severity\":\"warning\",\"phase\":\"manifest\",\"message\":{}}}",
                        json_string(&w.message),
                    )
                })
                .collect();
            let mut diags = warnings;
            diags.extend(parse_errors_json(&parse_errors));
            diags.extend(cycles_json(&cycles, &tree));
            diags.extend(resolve_errors_json(&resolve_errors));
            diags.extend(type_errors_json(&type_errors));
            if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                diags.push(format!(
                    "{{\"severity\":\"error\",\"phase\":{},\"message\":{}}}",
                    json_string(phase),
                    json_string(message),
                ));
            }
            let modules = render_walked_modules_json(&walked);
            let status = if failed { "error" } else { "ok" };
            let output_field = match &codegen_status {
                BuildCodegenStatus::Built {
                    exe_path,
                    glue_path,
                    dts_path,
                    threads_wasm_path,
                } => {
                    let mut field = format!(
                        ",\"output\":{}",
                        json_string(&exe_path.display().to_string())
                    );
                    if let Some(tw) = threads_wasm_path {
                        field.push_str(&format!(
                            ",\"threads_wasm\":{}",
                            json_string(&tw.display().to_string())
                        ));
                    }
                    if let Some(js) = glue_path {
                        field.push_str(&format!(
                            ",\"glue\":{}",
                            json_string(&js.display().to_string())
                        ));
                    }
                    if let Some(dts) = dts_path {
                        field.push_str(&format!(
                            ",\"dts\":{}",
                            json_string(&dts.display().to_string())
                        ));
                    }
                    field
                }
                _ => String::new(),
            };
            println!(
                "{{\"status\":{},\"project\":{},\"edition\":{},\"root\":{},\"target\":{},\"entry\":{},\"modules\":[{}],\"diagnostics\":[{}]{}}}",
                json_string(status),
                json_string(&mf.name),
                json_string(&mf.edition),
                json_string(&root.display().to_string()),
                json_string(walk_opts.target.as_suffix()),
                json_string(entry_label(walked.entry)),
                modules,
                diags.join(","),
                output_field,
            );
            if failed {
                process::exit(1);
            }
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "manifest_loaded",
                &format!(
                    "\"project\":{},\"edition\":{},\"root\":{}",
                    json_string(&mf.name),
                    json_string(&mf.edition),
                    json_string(&root.display().to_string()),
                ),
            );
            for w in &mf.warnings {
                emit_jsonl_event(
                    "manifest_warning",
                    &format!("\"message\":{}", json_string(&w.message)),
                );
            }
            let modules = render_walked_modules_json(&walked);
            emit_jsonl_event(
                "modules_discovered",
                &format!(
                    "\"target\":{},\"entry\":{},\"modules\":[{}]",
                    json_string(walk_opts.target.as_suffix()),
                    json_string(entry_label(walked.entry)),
                    modules,
                ),
            );
            for entry in parse_errors_jsonl(&parse_errors) {
                println!("{entry}");
            }
            for entry in cycles_jsonl(&cycles, &tree) {
                println!("{entry}");
            }
            for entry in resolve_errors_jsonl(&resolve_errors) {
                println!("{entry}");
            }
            for entry in type_errors_jsonl(&type_errors) {
                println!("{entry}");
            }
            if let BuildCodegenStatus::Failed { phase, message } = &codegen_status {
                emit_jsonl_event(
                    "codegen_error",
                    &format!(
                        "\"phase\":{},\"message\":{}",
                        json_string(phase),
                        json_string(message),
                    ),
                );
            }
            if let BuildCodegenStatus::Built {
                exe_path,
                glue_path,
                dts_path,
                threads_wasm_path,
            } = &codegen_status
            {
                let mut fields = format!(
                    "\"output\":{}",
                    json_string(&exe_path.display().to_string())
                );
                if let Some(tw) = threads_wasm_path {
                    fields.push_str(&format!(
                        ",\"threads_wasm\":{}",
                        json_string(&tw.display().to_string())
                    ));
                }
                if let Some(js) = glue_path {
                    fields.push_str(&format!(
                        ",\"glue\":{}",
                        json_string(&js.display().to_string())
                    ));
                }
                if let Some(dts) = dts_path {
                    fields.push_str(&format!(
                        ",\"dts\":{}",
                        json_string(&dts.display().to_string())
                    ));
                }
                emit_jsonl_event("build_artifact", &fields);
            }
            emit_jsonl_event(
                "build_complete",
                &format!(
                    "\"success\":{},\"total_errors\":{}",
                    !failed,
                    parse_errors.iter().map(|pe| pe.errors.len()).sum::<usize>()
                        + cycles.len()
                        + resolve_errors
                            .iter()
                            .map(|re| re.errors.len())
                            .sum::<usize>()
                        + type_errors.iter().map(|te| te.errors.len()).sum::<usize>()
                        + codegen_status.error_count(),
                ),
            );
            if failed {
                process::exit(1);
            }
        }
    }
}

/// Result of the Theme 4 multi-file codegen pass appended to
/// [`cmd_build_project`]. Each variant maps to a downstream output mode
/// (text "Built: ..." line / JSON `"output"` field / JSONL
/// `build_artifact` event). `Built` and `Failed` are only constructed
/// under `cfg(feature = "llvm")` since the codegen pass itself is gated
/// on the same feature.
#[cfg_attr(not(feature = "llvm"), allow(dead_code))]
#[derive(Debug, Clone)]
enum BuildCodegenStatus {
    /// Earlier per-module phases failed (parse / cycles / resolve /
    /// typecheck), so codegen never ran. Output modes don't emit anything
    /// extra in this case — the per-phase diagnostics carry the failure.
    Skipped,
    /// `karac` was built without the `llvm` feature; project type-checks
    /// but no executable can be produced. Mirrors the single-file
    /// `cmd_build` no-llvm branch.
    NoLlvmFeature,
    /// All phases succeeded; the linked artifact is at `exe_path` (a
    /// native executable, or `dist/wasm/<pkg>.wasm` on a wasm target —
    /// under embedded component bindings that single file IS the
    /// componentized output). Browser-bindings WASM builds additionally
    /// carry the companion ES-module glue (`<pkg>.js`) and TypeScript
    /// declarations (`<pkg>.d.ts`) — each `None` on every other build
    /// shape.
    /// `--features wasm-threads` builds also carry the threaded module
    /// (`<pkg>.threads.wasm` — the dual artifact's second leg); `None`
    /// otherwise.
    Built {
        exe_path: PathBuf,
        glue_path: Option<PathBuf>,
        dts_path: Option<PathBuf>,
        threads_wasm_path: Option<PathBuf>,
    },
    /// Late-phase failure (effect / ownership / concurrency / codegen /
    /// link). `phase` names the failing phase for the diagnostic output;
    /// `message` is the rendered error.
    Failed { phase: String, message: String },
}

impl BuildCodegenStatus {
    fn error_count(&self) -> usize {
        match self {
            BuildCodegenStatus::Failed { .. } => 1,
            _ => 0,
        }
    }
}

/// Drive the multi-file codegen path: concatenate all module items into a
/// single super-program (in topological order, dropping `import`
/// declarations and the synthetic prelude), run the post-typecheck
/// pipeline (lower / effect / ownership / concurrency), then codegen +
/// link. Caller has already verified parse / cycles / resolve / typecheck
/// passed; this function returns a structured status the caller renders
/// per output mode.
///
/// **Multi-module diagnostics.** Late-phase diagnostics (effect /
/// ownership / concurrency / codegen / link) for the merged super-
/// program recover file-of-origin context via a `SpanLookupKey →
/// module_index` table built at concat time and consulted by
/// `format_pipeline_errors`. When a span resolves to exactly one
/// module the diagnostic is prefixed with `file:line:col`; when the
/// span is absent (e.g., synthesized post-concat) or ambiguous
/// (collision across modules — rare in practice but possible when
/// two distinct files have identical leading bytes), the formatter
/// falls back to the file-less `line:col` form. Per-file
/// diagnostics for parse / cycles / resolve / typecheck still fire
/// upstream of this call.
#[cfg(feature = "llvm")]
#[allow(clippy::too_many_arguments)]
fn run_multi_file_codegen(
    tree: &ProgramTree,
    mf: &crate::manifest::Manifest,
    project_root: &std::path::Path,
    enable_hot_swap: bool,
    release: bool,
    is_wasm: bool,
    effective_bindings: Option<BindingsMode>,
    wasm_tools: Option<&crate::componentize::WasmTools>,
    wasm_threads: bool,
    crate_type: NativeCrateType,
    out_path: Option<&str>,
) -> BuildCodegenStatus {
    // 1. Topological emission order — dependencies before dependents.
    let order = module::emission_order(tree);

    // 2. Concatenate items. Drop `import` declarations (their effect was
    // resolved upstream by per-module resolve) and skip synthetic
    // modules. Items keep their original spans, which downstream
    // diagnostics use for line:col reporting.
    //
    // While concatenating, build a `ModuleSpanTable`: for each non-
    // synthetic module we register its file path once, then walk every
    // appended item's spans so late-phase diagnostics can recover the
    // file-of-origin via `format_pipeline_errors`.
    let mut super_items: Vec<Item> = Vec::new();
    let mut span_table = crate::span_visitor::ModuleSpanTable::new();
    for &id in &order {
        let m = &tree.modules[id];
        if m.is_synthetic {
            continue;
        }
        let module_idx = span_table.register_module(m.file.clone());
        for item in &m.items {
            if matches!(item, Item::Import(_)) {
                continue;
            }
            span_table.record_item(module_idx, item);
            super_items.push(item.clone());
        }
    }

    // Phase-10 (`std.web`): gated baked stdlib modules are synthetic, so
    // the loop above never carries their items — an imported `fetch`
    // resolves and typechecks per-module (those passes chase the tree)
    // but its body would be missing here. Append the expansion of every
    // gated import found in user modules, deduplicated on the bound name
    // so two files importing the same item don't define it twice.
    {
        let mut seen: std::collections::HashSet<(Vec<String>, String)> =
            std::collections::HashSet::new();
        for m in &tree.modules {
            if m.is_synthetic {
                continue;
            }
            for imp in &m.imports {
                let deduped: Vec<crate::ast::ImportItem> = imp
                    .items
                    .iter()
                    .filter(|ii| {
                        let bound = ii.alias.as_ref().unwrap_or(&ii.name);
                        seen.insert((imp.path.clone(), bound.clone()))
                    })
                    .cloned()
                    .collect();
                if let Some(expansion) = crate::prelude::gated_items_for_import(&imp.path, &deduped)
                {
                    super_items.extend(expansion);
                }
            }
        }
    }

    let super_program = Program {
        items: super_items,
        ..Program::default()
    };

    // 3. Drive the rest of the pipeline by hand-constructing a Pipeline
    // with the synthetic ParseResult. This mirrors what `Pipeline::new`
    // would do on a single-file source, except we skip the parse step
    // entirely (we have a pre-built Program already).
    let parsed = ParseResult {
        program: super_program,
        errors: Vec::new(),
        fix_edits: std::collections::HashMap::new(),
    };
    let mut pipeline = Pipeline {
        filename: mf.name.clone(),
        parsed,
        resolved: None,
        typed: None,
        effects: None,
        ownership: None,
        concurrency: None,
        provider_escape: None,
        raii_errors: None,
        simd_errors: None,
        comptime_errors: None,
        profile: crate::manifest::CompileProfile::Default,
        profile_config: crate::manifest::ProfileConfig::default(),
        lint_overrides: crate::lints::CliLintOverrides::default(),
    };
    pipeline.resolve();
    if pipeline.has_resolve_errors() {
        return BuildCodegenStatus::Failed {
            phase: "resolve".to_string(),
            message: format_pipeline_errors(&pipeline, "resolve", Some(&span_table)),
        };
    }
    pipeline.typecheck();
    if pipeline
        .typed
        .as_ref()
        .is_some_and(|t| !t.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "typecheck".to_string(),
            message: format_pipeline_errors(&pipeline, "typecheck", Some(&span_table)),
        };
    }
    pipeline.lower();
    pipeline.effectcheck();
    if pipeline
        .effects
        .as_ref()
        .is_some_and(|e| !e.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "effect".to_string(),
            message: format_pipeline_errors(&pipeline, "effect", Some(&span_table)),
        };
    }
    pipeline.ownershipcheck();
    if pipeline
        .ownership
        .as_ref()
        .is_some_and(|o| !o.errors.is_empty())
    {
        return BuildCodegenStatus::Failed {
            phase: "ownership".to_string(),
            message: format_pipeline_errors(&pipeline, "ownership", Some(&span_table)),
        };
    }
    pipeline.concurrencycheck();
    if pipeline.has_fatal_errors() {
        return BuildCodegenStatus::Failed {
            phase: "checks".to_string(),
            message: format_pipeline_errors(&pipeline, "checks", Some(&span_table)),
        };
    }

    // Library-artifact C-ABI honesty gate (additive-interop Slice 2,
    // project-mode `[lib]`): reject a non-transparent, non-boxable export
    // return/param before codegen so the produced `.a`/`.so`/`.h` is always
    // ABI-honest (single-file posture; see `cmd_build`).
    if crate_type != NativeCrateType::Bin {
        let export_errs = crate::cheader::validate_exports(&pipeline.parsed.program);
        if !export_errs.is_empty() {
            let message = export_errs
                .iter()
                .map(|(fn_name, reason)| format!("exported `{fn_name}`: {reason}"))
                .collect::<Vec<_>>()
                .join("\n");
            return BuildCodegenStatus::Failed {
                phase: "export-abi".to_string(),
                message,
            };
        }
    }

    // 4. Codegen — write to a temp object then link to the manifest's
    // `name` field as the binary basename in the project root. A wasm
    // build instead lands in the `dist/wasm/<pkg>.wasm` artifact layout
    // (phase-10 WASM artifact emission; `link_executable` dispatches to
    // wasm-ld off the active target, same as single-file mode).
    let exe_path = if is_wasm {
        let dist = project_root.join("dist").join("wasm");
        if let Err(e) = std::fs::create_dir_all(&dist) {
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("cannot create {}: {e}", dist.display()),
            };
        }
        dist.join(format!("{}.wasm", mf.name))
    } else {
        project_root.join(&mf.name)
    };
    let obj_path = std::env::temp_dir().join(format!(
        "karac_proj_{}_{}.o",
        std::process::id(),
        mf.name.replace(['/', '\\'], "_"),
    ));

    if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
        &pipeline.parsed.program,
        &obj_path.to_string_lossy(),
        pipeline.ownership.as_ref(),
        // WASM concurrency lowering is its own phase-10 entry — until it
        // lands, suppress auto-par groups on wasm so modules lower
        // sequentially instead of emitting spawn-site calls into a
        // runtime archive with no scheduler (the single-file posture).
        if is_wasm {
            None
        } else {
            pipeline.concurrency.as_ref()
        },
        None,
        None,
        enable_hot_swap,
        // `--release` strips debug-only contract machinery in project mode,
        // same as single-file. OR-composes with `KARAC_STRIP_CONTRACTS`
        // (which still applies via the `Codegen::new` default when `release`
        // is false).
        release,
        true, // A2: coroutines on for project builds (bug-C fix reaches real builds)
    ) {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "codegen".to_string(),
            message: format!("codegen failed: {e}"),
        };
    }

    // Library-artifact producer mode (additive-interop Slice 2, project-
    // mode `[lib]`): archive/link the emitted object into a `.a`/`.so`/
    // `.dylib` under `dist/` and emit the companion `.h`, instead of an
    // executable. Native-only (the wasm × library combination was rejected
    // in `cmd_build_project`). Returns early — the wasm/exe link tail below
    // is `Bin`-only.
    if crate_type != NativeCrateType::Bin {
        let lib_kind = match crate_type {
            NativeCrateType::StaticLib => crate::codegen::NativeLibKind::StaticLib,
            NativeCrateType::CDylib => crate::codegen::NativeLibKind::CDylib,
            NativeCrateType::Bin => unreachable!(),
        };
        let lib_name = mf.lib_name.as_deref().unwrap_or(&mf.name);
        let dist = project_root.join("dist");
        if let Err(e) = std::fs::create_dir_all(&dist) {
            let _ = std::fs::remove_file(&obj_path);
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("cannot create {}: {e}", dist.display()),
            };
        }
        let art_path = out_path.map(std::path::PathBuf::from).unwrap_or_else(|| {
            dist.join(format!("lib{lib_name}{}", lib_kind.artifact_extension()))
        });
        let export_syms = crate::cheader::export_symbols(&pipeline.parsed.program);
        if let Err(e) = crate::codegen::link_native_library(
            &obj_path.to_string_lossy(),
            &art_path.to_string_lossy(),
            lib_kind,
            lib_name,
            &export_syms,
        ) {
            let _ = std::fs::remove_file(&obj_path);
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!("link failed: {e}"),
            };
        }
        let _ = std::fs::remove_file(&obj_path);
        let header_path = art_path
            .parent()
            .map(|d| d.join(format!("lib{lib_name}.h")))
            .unwrap_or_else(|| std::path::PathBuf::from(format!("lib{lib_name}.h")));
        let header = crate::cheader::emit_c_header(&pipeline.parsed.program, lib_name);
        if let Err(e) = std::fs::write(&header_path, header) {
            return BuildCodegenStatus::Failed {
                phase: "link".to_string(),
                message: format!(
                    "library `{}` built, but writing the C header to {} failed: {e}",
                    art_path.display(),
                    header_path.display()
                ),
            };
        }
        print_staticlib_rust_host_note(crate_type);
        return BuildCodegenStatus::Built {
            exe_path: art_path,
            glue_path: None,
            dts_path: None,
            threads_wasm_path: None,
        };
    }

    // For embedded component bindings, wasm-ld's output is an
    // intermediate — link the C-ABI core module to a scratch path, then
    // lift it into the single component at `dist/wasm/<pkg>.wasm`. The
    // scratch basename is package-derived (not pid-bearing) so the module
    // name wasm-ld embeds — and the component carries — is reproducible
    // across rebuilds (B-2026-06-22-3); the dir carries the uniqueness.
    let (link_scratch_dir, link_out) = if wasm_tools.is_some() {
        match crate::componentize::link_core_scratch(&mf.name) {
            Ok((dir, core)) => (Some(dir), core),
            Err(e) => {
                let _ = std::fs::remove_file(&obj_path);
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: e,
                };
            }
        }
    } else {
        (None, exe_path.clone())
    };
    let wasm_export_names =
        crate::wasm_exports::link_export_names(&crate::wasm_exports::collect_wasm_exports(
            &pipeline.parsed.program,
            crate::target::active_target(),
        ));
    if let Err(e) = crate::codegen::link_executable_exports(
        &obj_path.to_string_lossy(),
        &link_out.to_string_lossy(),
        &wasm_export_names,
    ) {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "link".to_string(),
            message: format!("link failed: {e}"),
        };
    }
    let _ = std::fs::remove_file(&obj_path);
    if let Some(tool) = wasm_tools {
        let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
        let wasm_exports = crate::wasm_exports::collect_wasm_exports(
            &pipeline.parsed.program,
            crate::target::active_target(),
        );
        warn_unlowered_exports(
            &wasm_exports,
            crate::wasm_exports::ExportSig::component_lowerable,
        );
        let result = crate::componentize::componentize(
            tool,
            &link_out,
            &host_fns,
            &wasm_exports,
            &mf.name,
            &exe_path,
        );
        if let Some(dir) = &link_scratch_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        if let Err(e) = result {
            return BuildCodegenStatus::Failed {
                phase: "componentize".to_string(),
                message: format!("componentize failed: {e}"),
            };
        }
    }

    // `--features wasm-threads`: the dual artifact's second pass — same
    // front-end output, auto-par re-enabled, wasip1-threads machine,
    // --shared-memory link. Lands as `dist/wasm/<pkg>.threads.wasm`
    // next to the sequential module; knobs come straight from the
    // project's own manifest (already loaded — no walk-up).
    let (threads_wasm_path, threads_glue_cfg) = if wasm_threads {
        let threads_filename = format!("{}.threads.wasm", mf.name);
        let threads_path = exe_path.with_file_name(&threads_filename);
        let threads_obj = std::env::temp_dir().join(format!(
            "karac_proj_{}_{}.threads.o",
            std::process::id(),
            mf.name.replace(['/', '\\'], "_"),
        ));
        let cfg = emit_wasm_threads_artifact(
            &pipeline.parsed.program,
            pipeline.ownership.as_ref(),
            pipeline.concurrency.as_ref(),
            None,
            None,
            release,
            &threads_obj.to_string_lossy(),
            &threads_path,
            &threads_filename,
            (
                mf.wasm_pool_size,
                mf.wasm_fallback,
                mf.wasm_max_memory_pages,
            ),
        );
        (Some(threads_path), Some(cfg))
    } else {
        (None, None)
    };

    // Companion artifacts next to the module in `dist/wasm/`, keyed on
    // the resolved bindings mode — exactly the single-file `cmd_build`
    // contract: browser bindings ship the ES-module glue + TypeScript
    // declarations (`<pkg>.js` / `<pkg>.d.ts`, see `wasm_glue`);
    // embedded component bindings ship NO companion (`<pkg>.wasm` IS
    // the self-describing component).
    let mut glue_path = None;
    let mut dts_path = None;
    match effective_bindings {
        Some(BindingsMode::Browser) => {
            let host_fns = crate::wasm_glue::collect_host_fns(&pipeline.parsed.program);
            let wasm_exports = crate::wasm_exports::collect_wasm_exports(
                &pipeline.parsed.program,
                crate::target::active_target(),
            );
            warn_unlowered_exports(
                &wasm_exports,
                crate::wasm_exports::ExportSig::component_lowerable,
            );
            let wasm_filename = format!("{}.wasm", mf.name);
            let js = exe_path.with_extension("js");
            if let Err(e) = std::fs::write(
                &js,
                crate::wasm_glue::render_glue(
                    &host_fns,
                    &wasm_exports,
                    &wasm_filename,
                    threads_glue_cfg.as_ref(),
                ),
            ) {
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: format!("failed to write JS glue {}: {e}", js.display()),
                };
            }
            glue_path = Some(js);
            let dts = exe_path.with_extension("d.ts");
            if let Err(e) = std::fs::write(
                &dts,
                crate::wasm_glue::render_dts(
                    &host_fns,
                    &wasm_exports,
                    &wasm_filename,
                    threads_glue_cfg.is_some(),
                ),
            ) {
                return BuildCodegenStatus::Failed {
                    phase: "link".to_string(),
                    message: format!("failed to write TS declarations {}: {e}", dts.display()),
                };
            }
            dts_path = Some(dts);
        }
        Some(BindingsMode::Component) | Some(BindingsMode::None) | None => {}
    }
    BuildCodegenStatus::Built {
        exe_path,
        glue_path,
        dts_path,
        threads_wasm_path,
    }
}

/// Stub for the no-llvm build — never invoked because the caller gates
/// on `cfg!(feature = "llvm")`. Kept as a parallel signature so the call
/// site doesn't need cfg gating itself.
#[cfg(not(feature = "llvm"))]
#[allow(clippy::too_many_arguments)]
fn run_multi_file_codegen(
    _tree: &ProgramTree,
    _mf: &crate::manifest::Manifest,
    _project_root: &std::path::Path,
    _enable_hot_swap: bool,
    _release: bool,
    _is_wasm: bool,
    _effective_bindings: Option<BindingsMode>,
    _wasm_tools: Option<&crate::componentize::WasmTools>,
    _wasm_threads: bool,
    _crate_type: NativeCrateType,
    _out_path: Option<&str>,
) -> BuildCodegenStatus {
    BuildCodegenStatus::NoLlvmFeature
}

/// Render a structured error list across the post-typecheck pipeline
/// phases for the multi-file project-mode build path. `table` is the
/// per-module span lookup built at concat time in
/// `run_multi_file_codegen` — when present and the span resolves to
/// exactly one module, the diagnostic line is prefixed with
/// `file:line:col`; otherwise it falls back to bare `line:col` so
/// callers without a table (or with a span absent from the table /
/// shared across modules) still get a useful location.
#[cfg(feature = "llvm")]
fn format_pipeline_errors(
    pipeline: &Pipeline,
    phase: &str,
    table: Option<&crate::span_visitor::ModuleSpanTable>,
) -> String {
    use std::fmt::Write;
    let mut out = format!("multi-file {phase} failed:");
    let prefix = |span: &crate::token::Span| -> String {
        if let Some(t) = table {
            if let Some(p) = t.lookup(span) {
                return format!("{}:", p.display());
            }
        }
        String::new()
    };
    match phase {
        "resolve" => {
            if let Some(r) = &pipeline.resolved {
                for e in &r.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
        }
        "typecheck" => {
            if let Some(t) = &pipeline.typed {
                for e in &t.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
        }
        "effect" => {
            if let Some(e) = &pipeline.effects {
                for err in &e.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        "ownership" => {
            if let Some(o) = &pipeline.ownership {
                for err in &o.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        // The "checks" branch is reached when `has_fatal_errors`
        // returns true after a late-phase pass; today that flag is
        // driven by parse + resolve errors only (concurrency analysis
        // emits structured decisions, not errors), but we surface
        // every accumulated error here so the user gets file-context
        // wherever a span is available rather than the generic
        // "late-phase analysis failed" stub.
        "checks" => {
            if let Some(r) = &pipeline.resolved {
                for e in &r.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
            if let Some(t) = &pipeline.typed {
                for e in &t.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&e.span),
                        e.span.line,
                        e.span.column,
                        e.message,
                    );
                }
            }
            if let Some(e) = &pipeline.effects {
                for err in &e.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
            if let Some(o) = &pipeline.ownership {
                for err in &o.errors {
                    let _ = write!(
                        &mut out,
                        "\n  {}{}:{}: {}",
                        prefix(&err.span),
                        err.span.line,
                        err.span.column,
                        err.message,
                    );
                }
            }
        }
        _ => {}
    }
    out
}

fn print_parse_errors_text(parse_errors: &[ModuleParseErrors]) {
    for pe in parse_errors {
        for err in &pe.errors {
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                pe.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

/// Resolver errors collected for one specific module, with the source file
/// retained so diagnostics can be printed with their original location.
struct ModuleResolveErrors {
    file: PathBuf,
    errors: Vec<ResolveError>,
}

/// Run the resolver per module with the full `ProgramTree` attached so
/// cross-module imports can be validated. Returns only modules that produced
/// errors — a module with a clean resolve contributes nothing.
fn resolve_modules(tree: &ProgramTree) -> Vec<ModuleResolveErrors> {
    let mut out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Compiler-injected modules (CR-24 slice 8: `std.prelude` placeholder)
        // skip per-module passes — their stub items only exist to surface the
        // module path to cross-module resolution.
        if m.is_synthetic {
            continue;
        }
        // Resolver still takes a `&Program`, so wrap the module's items
        // in a freshly-owned `Program` view. Clone cost is negligible next
        // to the resolver pass itself.
        let program = Program {
            items: m.items.clone(),
            ..Program::default()
        };
        let result = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .with_test_file(m.is_test_file)
            .resolve();
        if !result.errors.is_empty() {
            out.push(ModuleResolveErrors {
                file: m.file.clone(),
                errors: result.errors,
            });
        }
    }
    out
}

fn resolve_error_code(kind: &ResolveErrorKind) -> &'static str {
    match kind {
        ResolveErrorKind::UnknownModule => "E0224",
        ResolveErrorKind::UnknownItemInModule => "E0225",
        ResolveErrorKind::PrivateItemAccess => "E0222",
        ResolveErrorKind::UndefinedName => "E0100",
        ResolveErrorKind::DuplicateDefinition => "E0101",
        ResolveErrorKind::ReservedIdentifier => "E0102",
        ResolveErrorKind::PrivateAccess => "E0103",
        ResolveErrorKind::UndefinedType => "E0104",
        ResolveErrorKind::UndefinedVariant => "E0105",
        ResolveErrorKind::UndefinedField => "E0106",
        ResolveErrorKind::UndefinedLabel => "E0107",
        ResolveErrorKind::OperatorTraitImplRestricted => "E0108",
        ResolveErrorKind::IntoTraitImplNotAllowed => "E0109",
        ResolveErrorKind::ImplLevelEffectVarNotAllowed => "E0110",
        ResolveErrorKind::ReservedEffectResource => "E0228",
        ResolveErrorKind::CompilerBuiltinReserved => "E0237",
        ResolveErrorKind::ContinueOnBlockLabel => "E0238",
        ResolveErrorKind::NonExhaustiveInvalidTarget => "E0239",
        ResolveErrorKind::TrackCallerInvalidTarget => "E0240",
        ResolveErrorKind::GpuInvalidTarget => "E0800",
        ResolveErrorKind::CodegenHintInvalidTarget => "E_CODEGEN_HINT_INVALID_POSITION",
        ResolveErrorKind::CodegenHintOnExternDecl => "E_CODEGEN_HINT_ON_EXTERN_DECL",
        ResolveErrorKind::DeprecatedOnImpl => "E0241",
        ResolveErrorKind::DeprecatedOnField => "E0242",
        ResolveErrorKind::UnknownAttribute => "E0243",
        ResolveErrorKind::ProfileInvalidTarget => "E0244",
        ResolveErrorKind::UnknownProfile => "E0245",
        ResolveErrorKind::QueryResolutionConflict => "E_QUERY_RESOLUTION_CONFLICT",
        ResolveErrorKind::UnionNonExhaustiveForbidden => "E_UNION_NON_EXHAUSTIVE_FORBIDDEN",
        ResolveErrorKind::DefaultAttributeInvalidPosition => "E_DEFAULT_ATTRIBUTE_INVALID_POSITION",
        ResolveErrorKind::DefaultAttributeWithoutDerive => "E_DEFAULT_ATTRIBUTE_WITHOUT_DERIVE",
        ResolveErrorKind::MalformedAttributeArgs => "E_MALFORMED_ATTRIBUTE_ARGS",
    }
}

fn print_resolve_errors_text(per_module: &[ModuleResolveErrors]) {
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                re.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
            if let Some(ref s) = err.suggestion {
                eprintln!("  help: did you mean `{s}`?");
            }
            if let Some(ref stub) = err.stub_hint {
                let target_file = sibling_production_file(&file);
                eprintln!(
                    "  hint: stub `{}` in {} with inferred signature:",
                    stub.callee_name, target_file
                );
                for line in stub.render_source().lines() {
                    eprintln!("    {line}");
                }
            }
        }
    }
}

/// Render `err.replacement` as `,"replacement":{...}` JSON tail (or empty
/// string when no replacement is attached). Mirrors the single-file
/// `print_diagnostics_json` path at the top of this file so IDE quick-fix
/// consumers see the same payload regardless of how `karac check` was
/// invoked. Multi-file-only diagnostics (E0223 / E0225) reach IDEs only
/// through this path.
fn replacement_json_tail(err: &crate::resolver::ResolveError) -> String {
    match err.replacement.as_deref() {
        Some(r) => format!(
            ",\"replacement\":{{\"offset\":{},\"length\":{},\"text\":{}}}",
            r.offset,
            r.length,
            json_string(&r.replacement),
        ),
        None => String::new(),
    }
}

fn resolve_errors_json(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"resolve\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                hints,
            ));
        }
    }
    out
}

fn resolve_errors_jsonl(per_module: &[ModuleResolveErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for re in per_module {
        let file = re.file.display().to_string();
        for err in &re.errors {
            let code = resolve_error_code(&err.kind);
            let suggestion = match err.suggestion.as_deref() {
                Some(s) => format!(",\"suggestion\":{}", json_string(s)),
                None => String::new(),
            };
            let replacement = replacement_json_tail(err);
            let hints = stub_hints_tail(&file, err);
            out.push(format!(
                "{{\"type\":\"resolve_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{}{}{}{}}}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                suggestion,
                replacement,
                hints,
            ));
        }
    }
    out
}

/// Emit the `,"hints":[…]` JSON tail when `err` carries a stub hint —
/// the multi-module resolve-error emitters' counterpart to the
/// `hints[].diff` wiring inside `DiagnosticJson::add`. Returns the
/// empty string when no stub hint is present so the JSON shape stays
/// lean for the common case.
fn stub_hints_tail(file: &str, err: &crate::resolver::ResolveError) -> String {
    match err.stub_hint.as_ref() {
        Some(s) => format!(",\"hints\":[{}]", render_stub_hint_json(file, s)),
        None => String::new(),
    }
}

/// Typechecker errors collected for one specific module.
struct ModuleTypeErrors {
    file: PathBuf,
    errors: Vec<crate::typechecker::TypeError>,
}

/// Run the typechecker per module with the full `ProgramTree` attached so
/// the CR-24 slice-6 cross-module `E0221` + field-access rules can fire.
/// A fresh resolver pass per module provides the `ResolveResult` the
/// typechecker still consumes internally.
fn typecheck_modules(
    tree: &ProgramTree,
    lint_overrides: &crate::lints::CliLintOverrides,
) -> Vec<ModuleTypeErrors> {
    let mut out = Vec::new();
    for (id, m) in tree.modules.iter().enumerate() {
        // Skip the compiler-injected `std.prelude` placeholder — its stubs
        // would clash with `register_builtin_types` if pushed through the
        // typechecker's normal item-collection.
        if m.is_synthetic {
            continue;
        }
        let program = Program {
            items: m.items.clone(),
            ..Program::default()
        };
        let resolved = Resolver::new(&program)
            .with_tree(tree, id as ModuleId)
            .resolve();
        let result = crate::typechecker::TypeChecker::new(&program, &resolved)
            .with_tree(tree, id as ModuleId)
            .with_cli_lint_overrides(lint_overrides.clone())
            .check();
        if !result.errors.is_empty() {
            out.push(ModuleTypeErrors {
                file: m.file.clone(),
                errors: result.errors,
            });
        }
    }
    out
}

fn type_error_code(kind: &crate::typechecker::TypeErrorKind) -> &'static str {
    use crate::typechecker::TypeErrorKind as K;
    match kind {
        K::PrivateTypeInPublicSignature => "E0221",
        K::TypeMismatch => "E0200",
        K::UndefinedField => "E0201",
        K::WrongNumberOfArgs => "E0202",
        K::MissingField => "E0203",
        K::ExtraField => "E0204",
        K::NonExhaustiveMatch => "E0205",
        K::NotCallable => "E0206",
        K::NotAStruct => "E0207",
        K::InvalidBinaryOp => "E0208",
        K::InvalidUnaryOp => "E0209",
        K::InvalidCast => "E0210",
        K::ConditionNotBool => "E0211",
        K::BranchTypeMismatch => "E0212",
        K::ReturnTypeMismatch => "E0213",
        K::GpuNotSafe => "E0801",
        K::StringNotIndexable => "E0268",
        K::SharedFieldNotMut => "E0269",
        K::AtomicMissingOrdering => "E0270",
        _ => "E0200",
    }
}

fn print_type_errors_text(per_module: &[ModuleTypeErrors]) {
    for te in per_module {
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            eprintln!(
                "error[{code}]: {}:{}:{}: {}",
                te.file.display(),
                err.span.line,
                err.span.column,
                err.message,
            );
        }
    }
}

fn type_errors_json(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"severity\":\"error\",\"phase\":\"typecheck\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

fn type_errors_jsonl(per_module: &[ModuleTypeErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for te in per_module {
        let file = te.file.display().to_string();
        for err in &te.errors {
            let code = type_error_code(&err.kind);
            let mut record = format!(
                "{{\"type\":\"type_error\",\"code\":{},\"file\":{},\"line\":{},\"column\":{},\"message\":{},\"class\":{}",
                json_string(code),
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
                json_string(err.class.map(|c| c.as_str()).unwrap_or("OTHER")),
            );
            if let Some(expected) = &err.expected {
                record.push_str(&format!(",\"expected\":{}", json_string(expected)));
            }
            if let Some(got) = &err.got {
                record.push_str(&format!(",\"got\":{}", json_string(got)));
            }
            record.push('}');
            out.push(record);
        }
    }
    out
}

fn print_cycles_text(cycles: &[Cycle], tree: &ProgramTree) {
    for c in cycles {
        eprintln!("error[E0223]: circular module dependency");
        eprintln!("  cycle: {}", c.format(tree));
        eprintln!(
            "  suggestion: extract the shared items into a lower-layer module that both ends of the cycle can depend on."
        );
    }
}

fn parse_errors_json(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
        for err in &pe.errors {
            out.push(format!(
                "{{\"severity\":\"error\",\"phase\":\"parse\",\"code\":\"E0001\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

fn cycles_json(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            let files: Vec<String> = c
                .nodes
                .iter()
                .map(|id| json_string(&tree.modules[*id].file.display().to_string()))
                .collect();
            format!(
                "{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}],\"cycle_files\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
                files.join(","),
            )
        })
        .collect()
}

fn parse_errors_jsonl(parse_errors: &[ModuleParseErrors]) -> Vec<String> {
    let mut out = Vec::new();
    for pe in parse_errors {
        let file = pe.file.display().to_string();
        for err in &pe.errors {
            out.push(format!(
                "{{\"type\":\"parse_error\",\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                json_string(&file),
                err.span.line,
                err.span.column,
                json_string(&err.message),
            ));
        }
    }
    out
}

fn cycles_jsonl(cycles: &[Cycle], tree: &ProgramTree) -> Vec<String> {
    cycles
        .iter()
        .map(|c| {
            let paths: Vec<String> = c
                .nodes
                .iter()
                .map(|id| {
                    let p = &tree.modules[*id].path;
                    if p.is_empty() {
                        String::new()
                    } else {
                        p.join(".")
                    }
                })
                .collect();
            let paths_json: Vec<String> = paths.iter().map(|s| json_string(s)).collect();
            format!(
                "{{\"type\":\"module_cycle\",\"code\":\"E0223\",\"message\":{},\"cycle_paths\":[{}]}}",
                json_string(&c.format(tree)),
                paths_json.join(","),
            )
        })
        .collect()
}

fn emit_build_tree_error(e: &BuildTreeError, output: OutputMode) {
    let code = e.code().unwrap_or("module");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"module_graph\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "build_tree_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

fn entry_label(entry: EntryKind) -> &'static str {
    match entry {
        EntryKind::Bin => "bin",
        EntryKind::Lib => "lib",
        EntryKind::None => "none",
    }
}

fn render_walked_modules_json(walked: &WalkResult) -> String {
    walked
        .modules
        .iter()
        .map(|m| {
            let path = if m.path.is_empty() {
                String::new()
            } else {
                m.path.join(".")
            };
            let role = match m.role {
                walker::ModuleRole::Ordinary => "ordinary",
                walker::ModuleRole::Entry => "entry",
                walker::ModuleRole::Test => "test",
            };
            let platform = match m.platform {
                Some(p) => json_string(p.as_suffix()),
                None => "null".to_string(),
            };
            format!(
                "{{\"path\":{},\"role\":{},\"platform\":{},\"file\":{}}}",
                json_string(&path),
                json_string(role),
                platform,
                json_string(&m.file.display().to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn emit_manifest_error(e: &manifest::ManifestError, output: OutputMode) {
    let code = e.code().unwrap_or("manifest");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"manifest\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "manifest_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

/// The active target triple for resolver invocations that carry no `--target`
/// flag — `test` / `resolve` / `update`. Precedence mirrors the `None`-flag
/// case of `cmd_build_project`'s target resolution: the manifest's
/// `[build].default-target` if set, else the host triple. Threading this
/// through `merge_target_overlay` (below) makes these commands consume the
/// same per-target `[dependencies]` view a plain `karac build` would — the
/// resolver-follow-up-(e) gap where only `cmd_build_project` merged the
/// overlay, so `[target.<triple>.dependencies]` silently dropped out of
/// `test` / `resolve` / `update`.
fn default_resolution_target(mf: &manifest::Manifest) -> String {
    mf.build_default_target
        .clone()
        .unwrap_or_else(crate::build_cache::host_target_triple)
}

/// Surface dependency-resolution diagnostics for the static/lenient commands
/// that consult a project but don't fetch — `karac check` and `karac run`
/// (resolver follow-up (m)). **Path-dep-only** — both are fast passes that must
/// not touch the network, so no registry/git provider is threaded in. Returns
/// `false` (halt the command) when a fatal graph error is emitted; `true` to
/// continue.
///
/// A structural graph error — a dependency cycle, a missing path-dep, or a
/// workspace-deref failure — fails `build_dep_graph_with_options` and halts.
/// A version conflict or an MSRV (`kara-version`) violation surfaces from the
/// resolve step and halts. Registry/git deps, by contrast, cannot be satisfied
/// without a fetch and neither command fetches or loads dependency modules from
/// a registry/git source, so an `E_*_DEP_UNSUPPORTED` finding is a build-time
/// concern — it is skipped here (the same dep surfaces on `karac build`).
fn surface_dep_graph_diagnostics(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
) -> bool {
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root: None,
        include_dev_deps: false,
        registry_provider: None,
        git_provider: None,
        // Path-dep-only lenient walk — no lockfile pinning here.
        pins: None,
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve(&graph, &active) {
        Ok(_) => true,
        Err(boxed) => {
            if matches!(
                boxed.code(),
                "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED"
            ) {
                return true;
            }
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            emit_dep_diagnostic(&diag, output, "error");
            false
        }
    }
}

/// Build the dep graph and resolve it against the active toolchain. Returns
/// `true` to continue with the build, `false` to halt. Registry/git
/// unsupported errors downgrade to warnings — the rest are fatal. Slice 7
/// of the PubGrub-resolver entry (`docs/implementation_checklist/phase-5-
/// diagnostics.md` line 813). Wiring point: `cmd_build_project` right
/// after the manifest loads.
///
/// `include_dev_deps` activates the test-mode walk (line 884) — the root
/// manifest's `[dev-dependencies]` participate in resolution. Off in
/// build mode; on in test mode. Dev-deps do not propagate through
/// transitive children regardless of the flag.
/// Resolve the project's dependency graph. `Err(())` means a fatal
/// diagnostic was emitted and the build must halt. `Ok(Some(resolution))`
/// carries the concrete package set for cross-package module loading
/// (phase-5 line 898); `Ok(None)` is the legacy warning-and-continue path
/// (unsupported registry/git sources outside offline mode), where the
/// build proceeds without dependency modules.
fn run_dep_resolution(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
    offline_root: Option<&std::path::Path>,
    include_dev_deps: bool,
    no_proxy: bool,
    persist_lock: bool,
) -> Result<Option<crate::dep_resolver::Resolution>, ()> {
    let loader = crate::dep_graph::FsLoader;

    // Activate the registry fetch path. A `ProxyRegistryProvider` — the
    // cache → retry → live-HTTP decorator stack — is threaded into the graph
    // walk so a `[dependencies]` registry entry is fetched, extracted, and
    // recursed into exactly like a path-dep. The *only* difference between
    // the proxy path (slice 4) and the direct-from-source path (follow-ups
    // (j)/(k)) is which base URL the HTTP client points at; the whole stack
    // below (retry + tarball cache + extraction) is base-URL-agnostic.
    //
    // Decide the effective registry base URL, if any:
    //   * `--offline` / no usable cache root: never touch the network — the
    //     `vendor/` walk owns resolution — so no base URL, provider stays off.
    //   * `--no-proxy` (direct-from-source, follow-ups (j)/(k)): fetch straight
    //     from the configured upstream registry (`KARAC_REGISTRY_URL` /
    //     `[build].registry`), bypassing the proxy. Unconfigured → `None`, so a
    //     registry dep keeps the warn-and-continue contract
    //     (`E_REGISTRY_DEP_UNSUPPORTED`) rather than fetching against nothing.
    //   * otherwise (proxy path): fetch through the configured proxy. The
    //     built-in `DEFAULT_PROXY_URL` is a not-yet-live placeholder, so this
    //     only activates once an operator points `KARAC_REGISTRY_PROXY` (or
    //     `[build].registry-proxy`) at a real proxy (`explicit_proxy_configured`).
    //
    // The client stack and provider live in locals whose borrows outlive the
    // `build_dep_graph_with_options` call below (it returns an owned graph
    // with every registry resolution already materialized to disk).
    let cache_root = crate::registry_proxy::default_registry_cache_root();
    let registry_base: Option<String> = if offline_root.is_some() || cache_root.is_none() {
        None
    } else if no_proxy {
        crate::registry_proxy::resolve_direct_registry_url(mf.build_registry.as_deref())
    } else if crate::registry_proxy::explicit_proxy_configured(mf.build_registry_proxy.as_deref()) {
        Some(
            crate::registry_proxy::ProxyConfig::resolve(
                crate::registry_proxy::ProxyMode::Default,
                mf.build_registry_proxy.as_deref(),
            )
            .url,
        )
    } else {
        None
    };
    let client_stack: Option<Box<dyn crate::registry_proxy::ProxyClient>> =
        registry_base.map(|url| {
            // A per-user `KARAC_REGISTRY_TOKEN` authenticates a private proxy
            // or private direct registry alike.
            let token = crate::registry_proxy::registry_token_from_env();
            let http = crate::registry_proxy::HttpProxyClient::with_token(url, token);
            let retrying = crate::registry_proxy::RetryingProxyClient::new(
                Box::new(http),
                crate::registry_proxy::RetryPolicy::default(),
            );
            // Tarball cache under <root>/<name>/<version>/package.tar.gz; the
            // provider extracts to the sibling <root>/<name>/<version>/src, so
            // the two share one root without colliding.
            let caching = crate::registry_proxy::CachingProxyClient::new(
                Box::new(retrying),
                cache_root.clone().unwrap_or_default(),
            );
            Box::new(caching) as Box<dyn crate::registry_proxy::ProxyClient>
        });
    let provider = client_stack.as_ref().map(|c| {
        crate::registry_extract::ProxyRegistryProvider::new(
            c.as_ref(),
            cache_root.clone().unwrap_or_default(),
        )
    });

    // Git deps are direct-from-source (no proxy in the loop), so git fetch is
    // gated only on `--offline` — not on `--no-proxy` or an explicitly
    // configured proxy. A git URL is real (unlike the placeholder default
    // proxy), so cloning whenever a git dep is declared is always correct.
    let git_provider = if offline_root.is_none() {
        crate::git_fetch::default_git_cache_root().map(crate::git_fetch::GitCliProvider::new)
    } else {
        None
    };

    // Lockfile-pin-over-catalog (follow-up (d)/(h)): read an existing
    // `kara.lock` and prefer its recorded registry-package versions, so a
    // rebuild reproduces the locked graph rather than drifting to the newest
    // compatible version. The pins feed BOTH the graph walk (slice 4 — a pinned
    // registry dep is fetched at exactly that version via `fetch_exact`, even if
    // since-yanked, and added to the candidate set) and version selection (slice
    // 2). Best-effort — an absent / unreadable / malformed lockfile yields no
    // pins (fresh selection), never a build error. Pins bite only where the
    // registry candidate set is widened; path/git deps ignore them.
    let pins = read_lockfile_pins(root);
    let options = crate::dep_graph::DepGraphOptions {
        offline_root,
        include_dev_deps,
        registry_provider: provider
            .as_ref()
            .map(|p| p as &dyn crate::dep_graph::RegistryProvider),
        git_provider: git_provider
            .as_ref()
            .map(|p| p as &dyn crate::git_fetch::GitProvider),
        pins: Some(&pins),
    };
    let graph = match crate::dep_graph::build_dep_graph_with_options(root, mf, &loader, options) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            return Err(());
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::dep_resolver::resolve_with_pins(&graph, &active, offline_root, &pins) {
        Ok(resolution) => {
            // Warn on any resolved version the catalog marks yanked (follow-up
            // (h), slice 4). Fresh selection excludes yanked versions, so this
            // fires only when a `kara.lock` pin lands on a version yanked since
            // it was recorded — reproducibility kept it, and the user should
            // hear the pin is now withdrawn.
            emit_yanked_pin_warnings(&resolution, &graph, output);
            // `karac resolve` is read-only — it inspects the graph without
            // rewriting `kara.lock`. Only build / test persist the pin.
            if persist_lock {
                persist_lockfile(root, &resolution, output);
            }
            Ok(Some(resolution))
        }
        Err(boxed) => {
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            let code = boxed.code();
            // In offline mode, registry/git deps can't be satisfied from
            // vendor/ today (registry/git vendoring lands alongside line
            // 845); the unsupported-source diagnostic must halt the build
            // so the operator doesn't get a silent partial resolution.
            // Outside offline, the existing warning-and-continue behavior
            // preserves the pre-fetch v1.1 contract.
            let severity = if offline_root.is_some() {
                "error"
            } else {
                match code {
                    "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED" => "warning",
                    _ => "error",
                }
            };
            emit_dep_diagnostic(&diag, output, severity);
            if severity == "warning" {
                Ok(None)
            } else {
                Err(())
            }
        }
    }
}

/// Walk each resolved path-dependency's source tree for cross-package
/// module loading (phase-5 line 898). Returns one [`module::DepPackageWalk`]
/// per path-sourced package, in `Resolution`'s deterministic (BTreeMap,
/// name-sorted) order. `Err(())` means a diagnostic was already emitted.
///
/// Dependencies must be library packages: a dep whose entry is
/// `src/main.kara` (or which has no entry at all) is a hard error, since
/// its items have nowhere to hoist and a binary cannot be imported.
/// Dependency test companions are excluded (`include_tests: false`) — a
/// consumer never compiles its deps' tests.
fn dep_package_walks(
    resolution: Option<&crate::dep_resolver::Resolution>,
    target: walker::Platform,
    output: OutputMode,
) -> Result<Vec<module::DepPackageWalk>, ()> {
    let Some(resolution) = resolution else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for pkg in resolution.packages.values() {
        // Path-deps, fetched registry deps, and cloned git deps all carry an
        // on-disk source root the module loader compiles (each threaded its
        // materialized directory into its `ResolvedSource` variant). `Root`
        // is the project itself.
        let dep_root: &std::path::Path = match &pkg.source {
            crate::dep_resolver::ResolvedSource::Path(dir) => dir,
            crate::dep_resolver::ResolvedSource::Registry { dir, .. } => dir,
            crate::dep_resolver::ResolvedSource::Git { dir, .. } => dir,
            _ => continue,
        };
        let walk_opts = WalkerOpts {
            target,
            include_tests: false,
        };
        let walked = match walker::walk_project(dep_root, walk_opts) {
            Ok(w) => w,
            Err(e) => {
                emit_dep_walk_error(&pkg.name, &e.to_string(), output);
                return Err(());
            }
        };
        if walked.entry != walker::EntryKind::Lib {
            let why = match walked.entry {
                walker::EntryKind::Bin => {
                    "it has `src/main.kara` — a binary package cannot be imported"
                }
                _ => "it has no `src/lib.kara` entry file",
            };
            emit_dep_walk_error(
                &pkg.name,
                &format!(
                    "dependency `{}` is not a library package: {}",
                    pkg.name, why
                ),
                output,
            );
            return Err(());
        }
        out.push(module::DepPackageWalk {
            name: pkg.name.clone(),
            walked,
        });
    }
    Ok(out)
}

/// Render a dependency-walk failure (walker error or non-library dep) in
/// the active output mode. Mirrors `emit_walker_error`'s shape with the
/// owning package named in the message.
fn emit_dep_walk_error(pkg: &str, message: &str, output: OutputMode) {
    match output {
        OutputMode::Text => {
            eprintln!("error[walker]: in dependency `{pkg}`: {message}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":\"walker\",\"package\":{},\"message\":{}}}]}}",
                json_string(pkg),
                json_string(message),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":\"walker\",\"package\":{},\"message\":{}",
                    json_string(pkg),
                    json_string(message),
                ),
            );
        }
    }
}

/// `karac-toolchain.toml` enforcement (tracker line 892). Returns
/// `true` to continue with the build, `false` to halt. When the file
/// is absent the function is a no-op. When present, the declared
/// `version` constraint is intersected against the active compiler
/// version; mismatch surfaces `E_TOOLCHAIN_VERSION_MISMATCH` with a
/// `karaup` hint. Parse errors halt with the file-specific symbolic
/// code so the operator hears about a malformed pin rather than
/// silently building against an unintended toolchain.
fn enforce_toolchain_pin(root: &std::path::Path, output: OutputMode) -> bool {
    let load = crate::karac_toolchain::load_from_start(root);
    let (path, spec) = match load {
        Ok(Some(pair)) => pair,
        Ok(None) => return true,
        Err(e) => {
            emit_toolchain_load_error(&e, output);
            return false;
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    match crate::karac_toolchain::enforce(&spec, &path, &active) {
        Ok(()) => true,
        Err(mismatch) => {
            emit_toolchain_mismatch(&mismatch, output);
            false
        }
    }
}

/// Render a `karac_toolchain::ToolchainError` (parse / IO failure) into
/// the active output mode. Symbolic code surfaces so downstream tooling
/// can recognize the kind of failure without parsing the message.
fn emit_toolchain_load_error(err: &crate::karac_toolchain::ToolchainError, output: OutputMode) {
    let code = err.code();
    let primary = err.to_string();
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// Render a `karac_toolchain::ToolchainMismatch` diagnostic into the
/// active output mode. The note documents the v1 limitation: karac
/// today reads the pin but does not auto-switch — operators install
/// the required toolchain via `karaup` (deferred) or manually.
fn emit_toolchain_mismatch(
    mismatch: &crate::karac_toolchain::ToolchainMismatch,
    output: OutputMode,
) {
    let code = mismatch.code();
    let primary = mismatch.message();
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: install a matching toolchain via `karaup install {}` (karaup ships post-v1)", mismatch.required);
            eprintln!("   = help: or relax the `version` constraint in `karac-toolchain.toml` to admit the active toolchain");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"toolchain_pin\",\"code\":{},\"message\":{},\"required\":{},\"active\":{}}}]}}",
                json_string(code),
                json_string(&primary),
                json_string(&mismatch.required.to_string()),
                json_string(&mismatch.active.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "toolchain_pin_error",
                &format!(
                    "\"code\":{},\"message\":{},\"required\":{},\"active\":{}",
                    json_string(code),
                    json_string(&primary),
                    json_string(&mismatch.required.to_string()),
                    json_string(&mismatch.active.to_string()),
                ),
            );
        }
    }
}

/// Pre-check diagnostic for `karac build --offline` when the project root
/// has no `./vendor/` directory. The resolver would otherwise error per-dep
/// with `E_OFFLINE_VENDOR_ENTRY_MISSING`; surfacing the missing root once,
/// up front, is a clearer operator hint.
fn emit_offline_no_vendor_dir(vendor_dir: &std::path::Path, output: OutputMode) {
    let code = "E_OFFLINE_NO_VENDOR_DIR";
    let primary = format!(
        "offline build requires a vendor directory at `{}` but none was found",
        vendor_dir.display()
    );
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {primary}");
            eprintln!("   = note: --offline resolves every transitive path-dep against `./vendor/<name>/`");
            eprintln!("   = help: run `karac vendor` to populate the vendor directory, then re-run with `--offline`");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "dep_resolution_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

/// Read the project's `kara.lock` (if present) and extract the registry-package
/// version pins for lockfile-pin-over-catalog resolution (follow-up (d)/(h),
/// slice 3). Best-effort: a missing / unreadable / malformed lockfile yields an
/// empty pin map, so a fresh project or a corrupt lockfile falls back to fresh
/// version selection rather than failing the build.
fn read_lockfile_pins(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, semver::Version> {
    let path = root.join("kara.lock");
    let Ok(source) = std::fs::read_to_string(&path) else {
        return std::collections::BTreeMap::new();
    };
    match crate::lockfile::Lockfile::parse(&path, &source) {
        Ok(lock) => lock.version_pins(),
        Err(_) => std::collections::BTreeMap::new(),
    }
}

/// Emit a `W_DEPENDENCY_YANKED` warning for each resolved package whose selected
/// version the catalog marks yanked (resolver follow-up (h), slice 4). Fresh
/// selection never picks a yanked version, so this only fires when a `kara.lock`
/// pin lands on a version yanked *since* it was recorded — reproducibility kept
/// the pin, and the user should hear it is now withdrawn. Purely advisory: it
/// never fails the build.
fn emit_yanked_pin_warnings(
    resolution: &crate::dep_resolver::Resolution,
    graph: &crate::dep_graph::DepGraph,
    output: OutputMode,
) {
    for pkg in resolution.packages.values() {
        let Some(yanked) = graph.yanked_versions.get(&pkg.name) else {
            continue;
        };
        if !yanked.contains(&pkg.version) {
            continue;
        }
        let diag = crate::dep_diagnostic::Diagnostic {
            code: "W_DEPENDENCY_YANKED",
            primary: format!(
                "dependency `{}` is pinned to version {}, which the registry has yanked",
                pkg.name, pkg.version
            ),
            notes: vec![
                "the version is recorded in `kara.lock`, but the registry has since withdrawn it — a yanked release is kept resolvable for reproducibility, yet should not be relied on for new work".to_string(),
            ],
            help: Some(format!(
                "run `karac update {}` to move to a non-yanked version, or pin a different version in `kara.toml`",
                pkg.name
            )),
        };
        emit_dep_diagnostic(&diag, output, "warning");
    }
}

/// Slice 4 of the lockfile entry (phase-5 line 831). Materializes a fresh
/// `kara.lock` from the resolver's output and writes it at the project root.
/// On read-then-rewrite paths, suppresses the write when the bytes are
/// identical so file mtimes are stable across no-op rebuilds. Any lockfile
/// IO failure is emitted as a warning (build-blocking would be too strict
/// in v1.1 — the resolver already succeeded; lockfile drift is recoverable
/// on the next build). Errors mid-build don't fail the build.
fn persist_lockfile(
    root: &std::path::Path,
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) {
    let lockfile = match crate::lockfile::Lockfile::from_resolution(
        resolution,
        root,
        crate::lockfile::compute_path_dep_hash,
    ) {
        Ok(lf) => lf,
        Err(e) => {
            emit_lockfile_warning(&e, output);
            return;
        }
    };

    let lockfile_path = root.join("kara.lock");
    let fresh_toml = lockfile.to_toml();

    // No-op-when-unchanged: avoid touching file mtime on a quiet rebuild.
    if let Ok(existing) = std::fs::read_to_string(&lockfile_path) {
        if existing == fresh_toml {
            return;
        }
    }

    if let Err(io) = std::fs::write(&lockfile_path, &fresh_toml) {
        let err = crate::lockfile::LockfileError::Io {
            path: lockfile_path,
            error: io.to_string(),
        };
        emit_lockfile_warning(&err, output);
    }
}

fn emit_lockfile_warning(err: &crate::lockfile::LockfileError, output: OutputMode) {
    let primary = err.to_string();
    let code = err.code();
    match output {
        OutputMode::Text => {
            eprintln!("warning[{code}]: {primary}");
            eprintln!("   = note: the resolver succeeded; the lockfile write is a follow-up step");
            eprintln!("   = help: check filesystem permissions for the project root");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"ok\",\"diagnostics\":[{{\"severity\":\"warning\",\"phase\":\"lockfile\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&primary),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "lockfile_warning",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&primary),
                ),
            );
        }
    }
}

fn emit_dep_diagnostic(
    diag: &crate::dep_diagnostic::Diagnostic,
    output: OutputMode,
    severity: &str,
) {
    match output {
        OutputMode::Text => {
            eprintln!(
                "{}[{}]: {}",
                if severity == "warning" {
                    "warning"
                } else {
                    "error"
                },
                diag.code,
                diag.primary,
            );
            for note in &diag.notes {
                eprintln!("   = note: {note}");
            }
            if let Some(help) = &diag.help {
                eprintln!("   = help: {help}");
            }
        }
        OutputMode::Json => {
            let notes_json = diag
                .notes
                .iter()
                .map(|n| json_string(n))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "{{\"status\":\"{}\",\"diagnostics\":[{{\"severity\":\"{}\",\"phase\":\"dep_resolution\",\"code\":{},\"message\":{},\"notes\":[{}]{}}}]}}",
                if severity == "warning" { "ok" } else { "error" },
                severity,
                json_string(diag.code),
                json_string(&diag.primary),
                notes_json,
                diag.help
                    .as_ref()
                    .map(|h| format!(",\"help\":{}", json_string(h)))
                    .unwrap_or_default(),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                &format!("dep_resolution_{severity}"),
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(diag.code),
                    json_string(&diag.primary),
                ),
            );
        }
    }
}

/// Emit a one-line confirmation `note:` when `--no-proxy` is set. The note
/// reports both the active proxy URL (so the operator sees what `karac`
/// would have consulted) and a pointer at the v1.1.x registry-proxy
/// follow-up. Silent when `--no-proxy` is absent — the proxy is the
/// default and the existing registry-dep-unsupported warning carries the
/// status. Emitted on the first cmd_* entry point so it is consistent
/// across `build`, `update`, and `vendor`.
fn emit_no_proxy_note(no_proxy: bool) {
    if !no_proxy {
        return;
    }
    // Best-effort: if we're in a project, honor its `[build]` pins so the
    // reported URLs match what a fetch would consult. Outside a project (or on
    // a malformed manifest) fall through to env/default.
    let manifest = std::env::current_dir()
        .ok()
        .and_then(|cwd| manifest::load_from_cwd(&cwd).ok())
        .map(|(_, mf)| mf);
    let manifest_proxy = manifest
        .as_ref()
        .and_then(|mf| mf.build_registry_proxy.clone());
    let manifest_registry = manifest.as_ref().and_then(|mf| mf.build_registry.clone());
    let config = crate::registry_proxy::ProxyConfig::resolve(
        crate::registry_proxy::ProxyMode::Disabled,
        manifest_proxy.as_deref(),
    );
    // When a direct upstream registry is configured (env or `[build].registry`),
    // `--no-proxy` fetches direct-from-source (follow-ups (j)/(k)) rather than
    // warn-and-continue; name the registry so the operator sees where deps come
    // from. Otherwise keep the pre-fetch note.
    match crate::registry_proxy::resolve_direct_registry_url(manifest_registry.as_deref()) {
        Some(registry_url) => eprintln!(
            "note: --no-proxy active; registry deps fetch direct-from-source at {registry_url} (proxy at {} bypassed)",
            config.url
        ),
        None => eprintln!(
            "note: --no-proxy active; registry deps will not consult the proxy at {} (set KARAC_REGISTRY_URL or [build].registry to fetch direct-from-source)",
            config.url
        ),
    }
}

fn emit_walker_error(e: &walker::WalkerError, output: OutputMode) {
    let code = e.code().unwrap_or("walker");
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {e}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"walker\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&e.to_string()),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "walker_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&e.to_string()),
                ),
            );
        }
    }
}

fn cmd_query(kind: QueryKind, filename: &str, function: &str) {
    let source = read_source(filename);
    let mut pipeline = Pipeline::new(filename, &source);
    pipeline.resolve();

    if pipeline.has_fatal_errors() {
        print_text_diagnostics(&pipeline);
        process::exit(1);
    }

    match kind {
        QueryKind::Effects => {
            // typecheck + lower BEFORE effectcheck so the effect-inference
            // walker can resolve instance-method callees to their precise
            // `Type.method` key. `Pipeline::effectcheck` sources its
            // `method_callee_types` table from `self.typed`, falling back to
            // an empty map when typecheck didn't run — without this the query
            // under-reports any effect that propagates through an instance
            // method (`c.get(...)` shows no `Network`). Mirrors what `build` /
            // `test` see (they always typecheck first). Phase-8 line 101.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            query_effects(&pipeline, function, filename);
        }
        QueryKind::Ownership => {
            pipeline.typecheck();
            pipeline.lower();
            pipeline.ownershipcheck();
            query_ownership(&pipeline, function);
        }
        QueryKind::Concurrency => {
            // Same instance-method-effect-resolution requirement as the
            // Effects arm above — concurrency analysis consumes the
            // effect-check result, so its inputs must come from the
            // typechecked pipeline too (phase-8 line 101).
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            pipeline.concurrencycheck();
            query_concurrency(&pipeline, function, filename);
        }
        QueryKind::CostSummary => {
            // cost-summary draws from the ownership pass for `rc_ops` and
            // walks the AST directly for `arc_provider_wraps` and
            // `borrow_flag_fields`. It needs typecheck + lower (so the
            // ownership pass sees the same AST every other phase does).
            pipeline.typecheck();
            pipeline.lower();
            pipeline.ownershipcheck();
            query_cost_summary(&pipeline);
        }
        QueryKind::Attributes { tool_prefix } => {
            // Pure AST walk — no further pipeline phases needed beyond
            // the resolve already done above (which gates fatal parse /
            // resolve errors). Tool-namespaced attributes have no
            // semantic effect on later phases, so we can emit a usable
            // result even when typecheck / ownership would have flagged
            // unrelated problems.
            query_attributes(&pipeline, tool_prefix);
        }
        QueryKind::Queries => {
            // Run every phase that may populate `queries`, then fold in
            // the P1.3 codegen analyzer in `query_queries`. The envelope
            // carries the P1.3 catalogue entries (inlining / branch-hint)
            // when they fire; the remaining phase `queries` vecs (P1.1,
            // P1.2, P1.4, P1.6) are still empty, so a program with no
            // hot-looking helper or skewed branch renders `{"queries":[]}`.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            pipeline.ownershipcheck();
            pipeline.concurrencycheck();
            query_queries(&pipeline);
        }
        QueryKind::Monomorphization => {
            // Reads from `TypeCheckResult.call_type_subs` +
            // `method_callee_types` for the type tuple, and from
            // `EffectCheckResult.call_effect_subs` for each instance's
            // effective effect set. Effect resolution needs the same
            // typecheck + lower precondition as the Effects/Concurrency
            // arms (so `with E` bindings resolve against the lowered AST
            // the effect checker walks); call_type_subs spans survive
            // lowering, so the type tuple is unaffected.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            query_monomorphization(&pipeline);
        }
        QueryKind::AffectedBy {
            target,
            tests_only,
            direction,
        } => {
            // Call-graph query — pure AST walk; resolution and
            // typecheck are not required (the graph is built from
            // the parsed program). Single-file mode infers the
            // test-file flag from the filename suffix per the same
            // `*_test.kara` heuristic the resolver uses.
            let is_test_file = filename.ends_with("_test.kara");
            let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
            query_affected_by(&graph, &target, tests_only, direction, filename);
        }
    }
}

fn query_affected_by(
    graph: &crate::call_graph::CallGraph,
    target: &crate::call_graph::TargetSpec,
    tests_only: bool,
    direction: AffectedByDirection,
    filename: &str,
) {
    let seeds = graph.resolve_target(target);
    let input_label = render_target_label(target, filename);
    // Union the per-seed reach sets so a multi-seed target (file or
    // file:range) collapses to a single envelope. De-dup happens via
    // BTreeMap keyed on node `key`.
    let mut callers: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    let mut callees: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    let mut tests: std::collections::BTreeMap<String, &crate::call_graph::NodeInfo> =
        std::collections::BTreeMap::new();
    for seed in &seeds {
        if matches!(
            direction,
            AffectedByDirection::Callers | AffectedByDirection::All
        ) {
            for n in graph.transitive_callers(seed) {
                callers.insert(n.key.clone(), n);
                if n.is_test {
                    tests.insert(n.key.clone(), n);
                }
            }
        }
        if matches!(
            direction,
            AffectedByDirection::Callees | AffectedByDirection::All
        ) {
            for n in graph.transitive_callees(seed) {
                callees.insert(n.key.clone(), n);
            }
        }
    }
    // `--tests-only` suppresses both callers and callees and emits
    // just the test set. Useful for the test-selection consumer.
    if tests_only {
        let line = render_affected_by_envelope_tests_only(&input_label, &tests);
        println!("{line}");
        return;
    }
    let line = render_affected_by_envelope(&input_label, &callers, &callees, &tests, direction);
    println!("{line}");
}

fn render_target_label(target: &crate::call_graph::TargetSpec, _filename: &str) -> String {
    match target {
        crate::call_graph::TargetSpec::File(f) => f.clone(),
        crate::call_graph::TargetSpec::FileRange(f, lo, hi) => {
            if lo == hi {
                format!("{f}:{lo}")
            } else {
                format!("{f}:{lo}-{hi}")
            }
        }
        crate::call_graph::TargetSpec::Function(name) => name.clone(),
    }
}

fn render_affected_by_envelope(
    input: &str,
    callers: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    callees: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    tests: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
    direction: AffectedByDirection,
) -> String {
    let mut s = String::new();
    s.push('{');
    s.push_str("\"type\":\"affected_by\",");
    write!(s, "\"input\":{}", json_string(input)).unwrap();
    if matches!(
        direction,
        AffectedByDirection::Callers | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"callers\":{}", render_node_array(callers)).unwrap();
    }
    if matches!(
        direction,
        AffectedByDirection::Callees | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"callees\":{}", render_node_array(callees)).unwrap();
    }
    if matches!(
        direction,
        AffectedByDirection::Callers | AffectedByDirection::All
    ) {
        s.push(',');
        write!(s, "\"tests\":{}", render_node_array(tests)).unwrap();
    }
    s.push('}');
    s
}

fn render_affected_by_envelope_tests_only(
    input: &str,
    tests: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
) -> String {
    let mut s = String::new();
    s.push('{');
    s.push_str("\"type\":\"affected_by\",");
    write!(s, "\"input\":{}", json_string(input)).unwrap();
    s.push(',');
    write!(s, "\"tests\":{}", render_node_array(tests)).unwrap();
    s.push('}');
    s
}

fn render_node_array(
    nodes: &std::collections::BTreeMap<String, &crate::call_graph::NodeInfo>,
) -> String {
    let entries: Vec<String> = nodes
        .values()
        .map(|n| {
            format!(
                "{{\"fn\":{},\"file\":{},\"line\":{}}}",
                json_string(&n.key),
                json_string(&n.file),
                n.line
            )
        })
        .collect();
    format!("[{}]", entries.join(","))
}

/// Phase-8 stdlib-floor § Compiler queries channel sub-item 3.
/// Collate every `CompilerQuery` across all phase results plus the
/// P1.3 codegen analyzer (`crate::codegen_queries`) and emit them as
/// a single JSON envelope on stdout. The envelope shape is
/// `{"queries":[…]}`; adding new catalogue entries or phases is
/// non-breaking.
fn query_queries(pipeline: &Pipeline) {
    let mut all: Vec<crate::queries::CompilerQuery> = Vec::new();
    if let Some(r) = pipeline.resolved.as_ref() {
        all.extend(r.queries.iter().cloned());
    }
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(t.queries.iter().cloned());
    }
    if let Some(e) = pipeline.effects.as_ref() {
        all.extend(e.queries.iter().cloned());
    }
    if let Some(o) = pipeline.ownership.as_ref() {
        all.extend(o.queries.iter().cloned());
    }
    if let Some(c) = pipeline.concurrency.as_ref() {
        all.extend(c.queries.iter().cloned());
    }
    // P1.3 codegen queries — plain-data analyzer over the parsed AST.
    // Runs unconditionally; cheap (single AST walk) and doesn't
    // require any later-phase side-tables.
    all.extend(crate::codegen_queries::analyze(&pipeline.parsed.program));

    // P1.2 specialization queries — reads the monomorphization counter,
    // so it needs the typecheck result (the type tuples). Skips silently
    // when typecheck didn't run; `effects` enriches nothing here but is
    // threaded for a uniform analyzer signature.
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(crate::specialization_queries::analyze(
            &pipeline.parsed.program,
            t,
            pipeline.effects.as_ref(),
        ));
    }

    // P1.1 RC-fallback queries — plain-data walk over the ownership
    // pass's `rc_values`. Skips silently when the ownership pass didn't
    // run.
    if let Some(o) = pipeline.ownership.as_ref() {
        all.extend(crate::rc_fallback_queries::analyze(
            &pipeline.parsed.program,
            o,
        ));
    }

    // P1.6 fork-threshold queries — plain-data walk over the concurrency
    // analysis's per-function parallelization decisions. Skips silently
    // when the concurrency pass didn't run.
    if let Some(c) = pipeline.concurrency.as_ref() {
        all.extend(crate::fork_threshold_queries::analyze(
            &pipeline.parsed.program,
            c,
        ));
    }

    // P1.5 layout-choice queries — plain-data walk over the AST keyed by
    // the typechecker's `expr_types` + `struct_info`. Skips silently when
    // typecheck didn't run.
    if let Some(t) = pipeline.typed.as_ref() {
        all.extend(crate::layout_queries::analyze(&pipeline.parsed.program, t));
    }

    println!("{}", render_queries_envelope(&all, &pipeline.filename));
}

fn render_queries_envelope(queries: &[crate::queries::CompilerQuery], filename: &str) -> String {
    if queries.is_empty() {
        return "{\"queries\":[]}".to_string();
    }
    let entries: Vec<String> = queries
        .iter()
        .map(|q| render_compiler_query(q, filename))
        .collect();
    format!("{{\"queries\":[{}]}}", entries.join(","))
}

fn render_compiler_query(q: &crate::queries::CompilerQuery, filename: &str) -> String {
    use crate::queries::{Confidence, Phase, QueryKind};
    let kind = match q.kind {
        QueryKind::Stub => "stub",
        QueryKind::InliningDecision => "inlining_decision",
        QueryKind::BranchHint => "branch_hint",
        QueryKind::SpecializationDecision => "specialization_decision",
        QueryKind::RcFallbackDecision => "rc_fallback_decision",
        QueryKind::ForkThresholdDecision => "fork_threshold_decision",
        QueryKind::LayoutChoice => "layout_choice",
    };
    let confidence = match q.default_confidence {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
    };
    let origin = q.cross_phase_origin.map(|p| match p {
        Phase::Resolver => "resolver",
        Phase::TypeChecker => "typechecker",
        Phase::EffectChecker => "effectchecker",
        Phase::Ownership => "ownership",
        Phase::Concurrency => "concurrency",
        Phase::Codegen => "codegen",
    });
    let options_json: Vec<String> = q
        .options
        .iter()
        .map(|opt| {
            let note = opt
                .note
                .as_deref()
                .map(|n| format!(",\"note\":\"{}\"", json_escape(n)))
                .unwrap_or_default();
            format!("{{\"label\":\"{}\"{}}}", json_escape(&opt.label), note)
        })
        .collect();
    let resolution_json: Vec<String> = q
        .resolution_surface
        .attributes
        .iter()
        .map(|a| format!("\"{}\"", json_escape(a)))
        .collect();
    let origin_field = origin
        .map(|o| format!(",\"cross_phase_origin\":\"{}\"", o))
        .unwrap_or_default();
    format!(
        "{{\"id\":\"{}\",\"site\":{{\"file\":\"{}\",\"line\":{},\"column\":{},\"offset\":{},\"length\":{}}},\"kind\":\"{}\",\"options\":[{}],\"default\":{},\"default_confidence\":\"{}\",\"resolution_surface\":[{}]{}}}",
        json_escape(&q.id.to_string()),
        json_escape(filename),
        q.site.line,
        q.site.column,
        q.site.offset,
        q.site.length,
        kind,
        options_json.join(","),
        q.default,
        confidence,
        resolution_json.join(","),
        origin_field,
    )
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn query_attributes(pipeline: &Pipeline, tool_prefix: Option<String>) {
    let filter = crate::query_attributes::AttributeQueryFilter {
        tool_prefix: tool_prefix.clone(),
    };
    let records = crate::query_attributes::collect_attributes(&pipeline.parsed.program, &filter);
    println!(
        "{}",
        render_attribute_query_json(&records, &pipeline.filename, tool_prefix.as_deref())
    );
}

fn render_attribute_query_json(
    records: &[crate::query_attributes::AttributeQueryRecord],
    filename: &str,
    tool_prefix: Option<&str>,
) -> String {
    let records_json: Vec<String> = records
        .iter()
        .map(|r| render_attribute_record(r, filename))
        .collect();
    let prefix_field = match tool_prefix {
        Some(p) => json_string(p),
        None => "null".to_string(),
    };
    format!(
        "{{\"tool_prefix\":{},\"attributes\":[{}]}}",
        prefix_field,
        records_json.join(","),
    )
}

fn render_attribute_record(
    r: &crate::query_attributes::AttributeQueryRecord,
    filename: &str,
) -> String {
    let path = json_string_list(&r.path);
    let args: Vec<String> = r
        .args
        .iter()
        .map(|a| render_attribute_arg(a, filename))
        .collect();
    let span = span_to_json(&r.span, filename);
    format!(
        "{{\"path\":{},\"args\":[{}],\"attached_to\":{},\"span\":{{{}}}}}",
        path,
        args.join(","),
        json_string(&r.attached_to),
        span,
    )
}

fn render_attribute_arg(a: &crate::query_attributes::AttributeQueryArg, filename: &str) -> String {
    let name = match &a.name {
        Some(n) => json_string(n),
        None => "null".to_string(),
    };
    let value = match &a.value {
        Some(v) => render_attribute_value(v),
        None => "null".to_string(),
    };
    let span = span_to_json(&a.span, filename);
    format!(
        "{{\"name\":{},\"value\":{},\"span\":{{{}}}}}",
        name, value, span,
    )
}

fn render_attribute_value(v: &crate::query_attributes::AttributeQueryValue) -> String {
    use crate::query_attributes::AttributeQueryValue;
    match v {
        AttributeQueryValue::String(s) => {
            format!("{{\"kind\":\"string\",\"value\":{}}}", json_string(s))
        }
        AttributeQueryValue::Int(n) => format!("{{\"kind\":\"int\",\"value\":{}}}", n),
        AttributeQueryValue::Bool(b) => format!("{{\"kind\":\"bool\",\"value\":{}}}", b),
        AttributeQueryValue::Path(p) => {
            format!("{{\"kind\":\"path\",\"value\":{}}}", json_string(p))
        }
        AttributeQueryValue::Other => "{\"kind\":\"expr\"}".to_string(),
    }
}

fn query_monomorphization(pipeline: &Pipeline) {
    let tc = match pipeline.typed.as_ref() {
        Some(t) => t,
        None => {
            // Typecheck didn't run (resolve errors short-circuited).
            // Emit an empty envelope so the CLI is still scriptable in
            // that case.
            println!(
                "{{\"scope\":{},\"by_generic\":[],\"totals\":{{\"generic_count\":0,\"instance_count\":0}}}}",
                json_string(&pipeline.filename),
            );
            return;
        }
    };
    let table =
        crate::monomorphization::analyze(&pipeline.parsed.program, tc, pipeline.effects.as_ref());
    println!(
        "{}",
        render_monomorphization_json(&table, &pipeline.filename),
    );
}

fn render_monomorphization_json(
    table: &crate::monomorphization::MonomorphizationTable,
    filename: &str,
) -> String {
    let entries: Vec<String> = table
        .by_generic
        .iter()
        .map(|g| {
            let instances: Vec<String> = g
                .instances
                .iter()
                .map(|i| render_monomorphization_instance(i, filename))
                .collect();
            format!(
                "{{\"generic\":{},\"instance_count\":{},\"instances\":[{}]}}",
                json_string(&g.generic),
                g.instances.len(),
                instances.join(","),
            )
        })
        .collect();
    format!(
        "{{\"scope\":{},\"by_generic\":[{}],\"totals\":{{\"generic_count\":{},\"instance_count\":{}}}}}",
        json_string(filename),
        entries.join(","),
        table.generic_count(),
        table.instance_count(),
    )
}

fn render_monomorphization_instance(
    instance: &crate::monomorphization::Instance,
    filename: &str,
) -> String {
    let site = format!(
        "{}:{}:{}",
        filename, instance.site.line, instance.site.column
    );
    format!(
        "{{\"types\":{},\"effects\":{},\"site\":{}}}",
        json_string_list(&instance.types),
        json_string_list(&instance.effects),
        json_string(&site),
    )
}

fn query_cost_summary(pipeline: &Pipeline) {
    let Some(ownership) = pipeline.ownership.as_ref() else {
        eprintln!("error: ownership pass did not run (earlier phase failed)");
        process::exit(1);
    };
    let summary =
        crate::cost_summary::build(&pipeline.filename, &pipeline.parsed.program, ownership);
    println!("{}", render_cost_summary_json(&summary, &pipeline.filename));
}

fn render_cost_summary_json(s: &crate::cost_summary::CostSummary, filename: &str) -> String {
    let totals = format!(
        "{{\"rc_ops\":{{\"count\":{},\"rc\":{},\"arc\":{},\"suppressed\":{}}},\"arc_provider_wraps\":{},\"borrow_flag_fields\":{},\"partition_guard_sites\":{},\"auto_clone_insertions\":{}}}",
        s.totals.rc_ops.count,
        s.totals.rc_ops.rc,
        s.totals.rc_ops.arc,
        s.totals.rc_ops.suppressed,
        s.totals.arc_provider_wraps,
        s.totals.borrow_flag_fields,
        s.totals.partition_guard_sites,
        s.totals.auto_clone_insertions,
    );
    let by_function: Vec<String> = s
        .by_function
        .iter()
        .map(|row| {
            let derivation: Vec<String> = row
                .derivation
                .iter()
                .map(|d| {
                    let site = span_to_json(&d.site, filename);
                    format!(
                        "{{\"reason\":{},\"site\":{{{}}}}}",
                        json_string(&d.reason),
                        site,
                    )
                })
                .collect();
            format!(
                "{{\"function\":{},\"rc_ops\":{},\"rc_ops_suppressed\":{},\"arc_provider_wraps\":{},\"derivation\":[{}]}}",
                json_string(&row.function),
                row.rc_ops,
                row.rc_ops_suppressed,
                row.arc_provider_wraps,
                derivation.join(","),
            )
        })
        .collect();
    let perf_notes: Vec<String> = s
        .perf_notes
        .iter()
        .map(|n| {
            let site = span_to_json(&n.site, filename);
            format!(
                "{{\"code\":{},\"message\":{},\"site\":{{{}}}}}",
                json_string(&n.code),
                json_string(&n.message),
                site,
            )
        })
        .collect();
    format!(
        "{{\"scope\":{},\"totals\":{},\"by_function\":[{}],\"perf_notes\":[{}]}}",
        json_string(&s.scope),
        totals,
        by_function.join(","),
        perf_notes.join(","),
    )
}

fn query_effects(pipeline: &Pipeline, function: &str, filename: &str) {
    let effects = pipeline.effects.as_ref().unwrap();

    // Whole-program mode: an empty `function` (a bare `<file>.kara`
    // target) emits every function's effects plus the call-graph edges
    // — the effect-graph artifact Cartographer consumes.
    if function.is_empty() {
        query_effects_whole_program(pipeline, effects, filename);
        return;
    }

    let inferred = effects.inferred_effects.get(function);
    let declared = effects.declared_effects.get(function);

    if inferred.is_none() && declared.is_none() {
        eprintln!("error: function '{function}' not found");
        process::exit(1);
    }

    let inferred_str = inferred
        .map(crate::effect_graph::effect_set_json)
        .unwrap_or_else(|| "[]".to_string());

    println!(
        "{{\"function\":{},\"inferred_effects\":{},\"declared_effects\":{}}}",
        json_string(function),
        inferred_str,
        crate::effect_graph::declared_effects_json(declared),
    );
}

/// Whole-program effect graph: one node per source-defined function
/// (free fn, impl method, trait default method) carrying its inferred +
/// declared effects, plus the directed call-graph edges between them.
/// Delegates to the wasm-safe [`crate::effect_graph`] builder so the CLI
/// and the browser studio emit a byte-identical graph.
fn query_effects_whole_program(pipeline: &Pipeline, effects: &EffectCheckResult, filename: &str) {
    let is_test_file = filename.ends_with("_test.kara");
    let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
    println!(
        "{}",
        crate::effect_graph::build_effect_graph_json(effects, &graph, filename)
    );
}

fn query_ownership(pipeline: &Pipeline, function: &str) {
    let ownership = pipeline.ownership.as_ref().unwrap();

    match ownership.param_modes.get(function) {
        Some(params) => {
            let param_entries: Vec<String> = params
                .iter()
                .map(|(name, mode)| {
                    let repr = ownership
                        .representations
                        .get(&format!("{}.{}", function, name))
                        .cloned()
                        .unwrap_or_else(|| match mode {
                            crate::ownership::OwnershipMode::Own => "owned (stack)".to_string(),
                            _ => "ref (borrow)".to_string(),
                        });
                    format!(
                        "{{\"name\":{},\"mode\":{},\"representation\":{}}}",
                        json_string(name),
                        json_string(ownership_mode_str(mode)),
                        json_string(&repr),
                    )
                })
                .collect();
            let rc_entries: Vec<String> = ownership
                .rc_values
                .get(function)
                .map(|m| {
                    let mut v: Vec<&crate::ownership::RcEntry> = m.values().collect();
                    v.sort_by(|a, b| a.binding.cmp(&b.binding));
                    v.into_iter()
                        .map(|e| {
                            let arc = ownership
                                .arc_values
                                .get(function)
                                .is_some_and(|s| s.contains(&e.binding));
                            let kind = if arc { "Arc" } else { "Rc" };
                            format!(
                                "{{\"binding\":{},\"kind\":{},\"trigger\":{},\"consume_line\":{},\"other_use_line\":{}}}",
                                json_string(&e.binding),
                                json_string(kind),
                                json_string(rc_trigger_str(&e.trigger)),
                                e.consume_span.line,
                                e.other_use_span.line,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Round 12.25: closures created inside `function` are
            // surfaced as a `"closures"` array. Each entry carries
            // the closure expression's source location plus the
            // round-12.23 inferred parameter modes and round-12.24
            // captures. Sorted by (line, column) for deterministic
            // output.
            let mut closures_to_emit: Vec<(&crate::resolver::SpanKey, &crate::token::Span)> =
                ownership
                    .closure_function
                    .iter()
                    .filter(|(_, fn_key)| fn_key.as_str() == function)
                    .filter_map(|(k, _)| ownership.closure_spans.get(k).map(|sp| (k, sp)))
                    .collect();
            closures_to_emit.sort_by_key(|(_, sp)| (sp.line, sp.column));
            let closure_entries: Vec<String> = closures_to_emit
                .iter()
                .map(|(key, span)| {
                    let params_json: Vec<String> = ownership
                        .closure_param_modes
                        .get(key)
                        .map(|ms| {
                            ms.iter()
                                .map(|(name, mode)| {
                                    format!(
                                        "{{\"name\":{},\"mode\":{}}}",
                                        json_string(name),
                                        json_string(ownership_mode_str(mode)),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let captures_json: Vec<String> = ownership
                        .closure_captures
                        .get(key)
                        .map(|cs| {
                            cs.iter()
                                .map(|(name, mode)| {
                                    format!(
                                        "{{\"name\":{},\"mode\":{}}}",
                                        json_string(name),
                                        json_string(ownership_mode_str(mode)),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    format!(
                        "{{\"line\":{},\"column\":{},\"parameters\":[{}],\"captures\":[{}]}}",
                        span.line,
                        span.column,
                        params_json.join(","),
                        captures_json.join(","),
                    )
                })
                .collect();
            println!(
                "{{\"function\":{},\"parameters\":[{}],\"rc_values\":[{}],\"closures\":[{}]}}",
                json_string(function),
                param_entries.join(","),
                rc_entries.join(","),
                closure_entries.join(","),
            );
        }
        None => {
            eprintln!("error: function '{function}' not found");
            process::exit(1);
        }
    }
}

fn rc_trigger_str(t: &crate::ownership::RcTrigger) -> &'static str {
    match t {
        crate::ownership::RcTrigger::DirectReuseAfterConsume => "direct_reuse_after_consume",
        crate::ownership::RcTrigger::ClosureCaptureWithOuterUse => "closure_capture_with_outer_use",
        crate::ownership::RcTrigger::ContainerStoreWithSubsequentUse => {
            "container_store_with_subsequent_use"
        }
    }
}

fn query_concurrency(pipeline: &Pipeline, function: &str, filename: &str) {
    let analysis = pipeline.concurrency.as_ref().unwrap();

    // Whole-program mode: an empty `function` (a bare `<file>.kara`
    // target) emits the parallel-band decision for every analyzed
    // function — the concurrency layer of Cartographer's effect graph.
    if function.is_empty() {
        query_concurrency_whole_program(pipeline, analysis, filename);
        return;
    }

    match analysis.function_decisions.get(function) {
        Some(fc) => {
            println!(
                "{{\"function\":{},\"total_statements\":{},\"statement_spans\":{},\"parallel_groups\":{},\"serialization_points\":{},\"reorder_opportunities\":{}}}",
                json_string(function),
                fc.total_statements,
                crate::effect_graph::statement_spans_json(fc, filename),
                crate::effect_graph::parallel_groups_json(fc),
                crate::effect_graph::serialization_points_json(fc),
                crate::effect_graph::reorder_opportunities_json(fc),
            );
        }
        None => {
            eprintln!("error: function '{function}' not found");
            process::exit(1);
        }
    }
}

/// Whole-program concurrency report: one entry per analyzed source
/// function (in call-graph key order), carrying its statement count and
/// parallel groups. Function keys join with the effect-graph nodes from
/// `query effects <file>`, so a consumer can overlay parallel bands onto
/// the effect graph.
fn query_concurrency_whole_program(
    pipeline: &Pipeline,
    analysis: &ConcurrencyAnalysis,
    filename: &str,
) {
    let is_test_file = filename.ends_with("_test.kara");
    let graph = crate::call_graph::build(&pipeline.parsed.program, filename, is_test_file);
    println!(
        "{}",
        crate::effect_graph::build_concurrency_graph_json(analysis, &graph, filename)
    );
}

fn cmd_fmt(filename: &str) {
    let source = read_source(filename);
    let parsed = crate::parse(&source);
    if !parsed.errors.is_empty() {
        for err in &parsed.errors {
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
        process::exit(1);
    }
    let formatted = crate::formatter::format_program(&parsed.program);
    print!("{formatted}");
}

/// Apply machine-applicable suggestions back into the source file.
///
/// Runs the full single-file pipeline (resolve → typecheck → lower →
/// effectcheck → ownership → ...), then collects every diagnostic that
/// carries a `replacement: Some(_)` payload across all phases that have
/// gained machine-applicable metadata. Edits are sorted in reverse
/// byte-offset order (so earlier edits don't invalidate later offsets)
/// and the source file is overwritten. With `dry_run = true`, prints the
/// would-be rewrites to stdout without touching disk.
///
/// Phases that contribute fixes today:
/// - Resolver: E0223 (UnknownModule, round 12.29), E0225
///   (UnknownItemInModule, round 12.28), E0228 (UndefinedName) and E0229
///   (UndefinedType) — both pre-12-era. All four are `did you mean`
///   corrections; the suggestion is a concrete identifier and the error
///   span is the misspelled token.
/// - Ownership: N0507 (UnusedMutCaptureNote, round 12.31) — closure
///   prefix `mut ref` → `ref`. Note (not error), so it does not block
///   compilation; `karac fix` applies it opportunistically.
///
/// Other diagnostic kinds carry descriptive (multi-step) suggestions
/// that are not mechanically applicable; they remain visible through
/// `karac check` and must be acted on by hand.
fn cmd_fix(filename: &str, dry_run: bool) {
    let source = read_source(filename);
    let mut pipeline = Pipeline::new(filename, &source);
    let mut edits: Vec<crate::resolver::TextEdit> = Vec::new();
    if pipeline.has_parse_errors() {
        // The file doesn't fully parse, but parsing may still have
        // synthesized machine-applicable recovery edits (e.g. deleting a
        // stray comma in a comma-separated `with` clause). Apply those —
        // each pass unblocks the next re-check. Post-parse phases can't run
        // on an unparseable file, so only parse edits are available here; if
        // there are none, report the parse errors and exit as before.
        edits.extend(pipeline.parsed.fix_edits.values().cloned());
        if edits.is_empty() {
            for err in &pipeline.parsed.errors {
                eprintln!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            }
            process::exit(1);
        }
    } else {
        pipeline.run_all_checks();
        if let Some(ref r) = pipeline.resolved {
            edits.extend(
                r.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
        }
        if let Some(ref ef) = pipeline.effects {
            edits.extend(
                ef.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
        }
        if let Some(ref o) = pipeline.ownership {
            edits.extend(
                o.errors
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            edits.extend(
                o.notes
                    .iter()
                    .filter_map(|e| e.replacement.as_deref().cloned()),
            );
            // Multi-edit `fix_diff` envelopes (B-2026-07-06-4). The
            // `ConcurrentSharedStruct` / `ConcurrentPlainStruct`
            // diagnostics carry a full machine-applicable migration
            // (`par struct` keyword insert + per-mut-field `Mutex[T]`
            // wraps) in `error_fix_diffs`, keyed by the diagnostic's
            // primary span. `collect_diagnostics` already emits these as
            // a top-level `"fix_diff":[...]` array to JSON, but until now
            // `cmd_fix` collected only each error's single-edit
            // `.replacement` — so `karac fix` applied nothing for these
            // two even though the JSON advertised a fix. Flatten every
            // envelope's edits in here; the descending-offset sort +
            // overlap dedup below applies them safely alongside the
            // single-edit replacements.
            edits.extend(o.error_fix_diffs.values().flatten().cloned());
        }
        if let Some(ref t) = pipeline.typed {
            // Typecheck fix-its (e.g. E0205 missing-match-arm insertion, the
            // `#[non_exhaustive]` cross-package wildcard) use FixIt{span,
            // replacement}; convert to the TextEdit offset/length form.
            edits.extend(t.errors.iter().filter_map(|e| {
                e.fix_it.as_ref().map(|f| crate::resolver::TextEdit {
                    offset: f.span.offset,
                    length: f.span.length,
                    replacement: f.replacement.clone(),
                })
            }));
        }
    }

    if edits.is_empty() {
        println!("(no fixable diagnostics in {filename})");
        return;
    }

    // Drop overlapping edits (e.g. the same token reported by multiple
    // sources). Sort by offset descending so that applying them in order
    // does not invalidate the offsets of later edits.
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut deduped: Vec<crate::resolver::TextEdit> = Vec::with_capacity(edits.len());
    let mut last_start = usize::MAX;
    for edit in edits {
        let end = edit.offset.saturating_add(edit.length);
        if end > last_start {
            // Overlaps a later (higher-offset) edit already in the buffer
            // — skip silently. This is a defense-in-depth measure; the
            // resolver shouldn't normally emit overlapping replacements.
            continue;
        }
        last_start = edit.offset;
        deduped.push(edit);
    }

    if dry_run {
        println!("would apply {} fix(es) to {filename}:", deduped.len());
        for edit in deduped.iter().rev() {
            // Render in source order for human readability.
            let original = source
                .get(edit.offset..edit.offset.saturating_add(edit.length))
                .unwrap_or("<?>");
            let (line, col) = crate::byte_offset_to_line_col(&source, edit.offset);
            println!(
                "  {filename}:{line}:{col}: `{}` → `{}`",
                original, edit.replacement
            );
        }
        return;
    }

    let mut rewritten = source.clone();
    for edit in &deduped {
        let end = edit.offset.saturating_add(edit.length);
        if end > rewritten.len() {
            // Source shrank between read and apply — bail rather than
            // produce an out-of-bounds slice.
            eprintln!(
                "error: fix would write past end of file ({} > {}) — aborting without modifying {filename}",
                end,
                rewritten.len()
            );
            process::exit(1);
        }
        rewritten.replace_range(edit.offset..end, &edit.replacement);
    }
    if let Err(e) = std::fs::write(filename, &rewritten) {
        eprintln!("error: failed to write {filename}: {e}");
        process::exit(1);
    }
    println!("applied {} fix(es) to {filename}", deduped.len());
}

// `byte_offset_to_line_col` was promoted to `crate::byte_offset_to_line_col`
// in `src/lib.rs` so codegen's debugger-contract metadata emission can reuse
// it. The cli still calls it from `apply_fixes` below; the rename is a single
// crate-path tweak with no behavior change.

/// Implementation of `karac migrate shared-to-par <Type>` — phase-7
/// L215a foundation slice. Locates the `shared struct <Type>` definition
/// in the parsed source, runs the L201a type-definition rewrite via
/// [`crate::ownership::build_fix_diff_edits`], and prints (dry-run) or
/// writes (`--apply`) the resulting edits.
///
/// **Scope (v1, L215a–L215b4).** Type-definition rewrite (keyword rename
/// `shared` → `par`, `mut ` strip per mut field, `Mutex[T]` wrap per mut
/// field) plus consumer-site `lock self.field { ... }` wraps across every
/// read/write of bindings of `<Type>` — annotated bindings (L215b1),
/// `lock self.field` wrap shape + read-site rewrite (L215b2), typecheck-
/// resolved inferred bindings + mutating-method-call wraps (L215b3), and
/// cross-file workspace walk (L215b4). When the file argument is omitted,
/// the tool discovers the project root via `kara.toml`, walks every
/// `.kara` module under `src/`, and runs the per-file rewrite pipeline
/// against each.
///
/// **Workspace dirty-check** (`--apply` only). When `--apply` is set
/// without `--force`, the tool refuses to run if `git status --porcelain`
/// reports any modifications. The check shells out to `git`; absence
/// of `git` (or running outside a repo) is treated as "no dirt to
/// guard against" rather than an error — the guard is opportunistic,
/// not load-bearing. `--force` bypasses the check unconditionally.
/// In project-mode the check runs from the project root; in single-
/// file mode it runs from the file's parent directory.
fn cmd_migrate(type_name: &str, apply: bool, force: bool, file: Option<&str>, atomic: bool) {
    match file {
        Some(f) => cmd_migrate_single_file(type_name, apply, force, f),
        None => cmd_migrate_project(type_name, apply, force, atomic),
    }
}

/// Single-file migration (L215a–b3 surface). When the user passes
/// `<file.kara>` explicitly, only that file is parsed + rewritten — the
/// struct definition must live in the named file or the tool errors.
fn cmd_migrate_single_file(type_name: &str, apply: bool, force: bool, filename: &str) {
    let source = read_source(filename);
    let outcome = compute_migration_edits_for_file(filename, &source, type_name);
    match outcome {
        FileMigrationOutcome::ParseFailed(msgs) => {
            for m in &msgs {
                eprintln!("{m}");
            }
            process::exit(1);
        }
        FileMigrationOutcome::WrongKind => {
            eprintln!(
                "error: `{type_name}` is not a `shared struct` — `karac migrate shared-to-par` only applies to `shared struct` definitions (run `karac fix` on a `par {{ ... }}` diagnostic instead)"
            );
            process::exit(1);
        }
        FileMigrationOutcome::NoStructDef => {
            eprintln!(
                "error: no struct named `{type_name}` found in `{filename}` — `karac migrate shared-to-par` rewrites the type definition in place, so the type must be defined in the migration file"
            );
            process::exit(1);
        }
        FileMigrationOutcome::Ok(plan) => {
            if plan.edits.is_empty() {
                println!("(no migration edits needed for `{type_name}` in {filename})");
                return;
            }
            if apply && !force && workspace_has_uncommitted_changes(filename) {
                eprintln!(
                    "error: workspace has uncommitted changes — refusing to run `karac migrate --apply` without `--force`"
                );
                eprintln!(
                    "       commit or stash pending work first, or re-run with `--force` to bypass the guard."
                );
                process::exit(1);
            }
            emit_migration_for_file(&plan, apply);
            if !apply {
                println!(
                    "(dry-run — re-run with `--apply` to write changes; consumer-site lock-block wraps cover assign / compound-assign writes, reads, and mutating method calls against bindings of `{type_name}` in this file — including type-inferred bindings when the file typechecks. Cross-file walks now run by default when `<file>` is omitted; see project-mode below)"
                );
            }
        }
    }
}

/// Project-mode migration (L215b4). Discovers the project root via
/// `kara.toml`, walks every module under `src/`, runs the per-file
/// rewrite pipeline against each, and aggregates the results. Exactly
/// one walked file must contain `shared struct <Type>`; zero or more
/// than one is a hard error. Files with no edits are silently skipped.
///
/// The pass is two-stage so that consumer-only modules participate:
/// the def-file's mut-field set is collected first, then every file's
/// consumer rewrite runs with that set (using
/// [`build_consumer_rewrite_edits_with_mut_fields`]).
fn cmd_migrate_project(type_name: &str, apply: bool, force: bool, atomic: bool) {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };
    let Some(root) = manifest::discover_project_root(&cwd) else {
        eprintln!(
            "error: `karac migrate shared-to-par` could not find a `kara.toml` in the current directory or any ancestor — run from inside a project, or pass an explicit `<file.kara>` argument for single-file mode"
        );
        process::exit(1);
    };
    let walked = match walker::walk_project(&root, WalkerOpts::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: cannot walk project at `{}`: {}", root.display(), e);
            process::exit(1);
        }
    };

    // Stage 1: parse every file (resolve + typecheck for type_ctx), find
    // the def-file, and collect its mut-field set. Parse errors abort —
    // a file that doesn't parse can't be safely rewritten. Typecheck
    // errors degrade gracefully (L215b3 "manual at the review step").
    struct PreparedFile {
        filename: String,
        source: String,
        pipeline: Pipeline,
        has_shared_def: bool,
        has_wrong_kind: bool,
    }
    let mut prepared: Vec<PreparedFile> = Vec::new();
    for module in &walked.modules {
        let filename = module.file.to_string_lossy().into_owned();
        let source = match std::fs::read_to_string(&module.file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read `{}`: {e}", module.file.display());
                process::exit(1);
            }
        };
        let mut pipeline = Pipeline::new(&filename, &source);
        if pipeline.has_parse_errors() {
            for err in &pipeline.parsed.errors {
                eprintln!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            }
            process::exit(1);
        }
        pipeline.resolve();
        pipeline.typecheck();
        let struct_def = pipeline
            .parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                Item::StructDef(s) if s.name == type_name => Some(s),
                _ => None,
            });
        let (has_shared_def, has_wrong_kind) = match struct_def {
            Some(s) if s.is_shared => (true, false),
            Some(_) => (false, true),
            None => (false, false),
        };
        prepared.push(PreparedFile {
            filename,
            source,
            pipeline,
            has_shared_def,
            has_wrong_kind,
        });
    }

    let def_files: Vec<&PreparedFile> = prepared.iter().filter(|p| p.has_shared_def).collect();
    let wrong_kind_files: Vec<&PreparedFile> =
        prepared.iter().filter(|p| p.has_wrong_kind).collect();
    if def_files.is_empty() && !wrong_kind_files.is_empty() {
        eprintln!(
            "error: `{type_name}` is not a `shared struct` (found a non-shared definition in `{}`) — `karac migrate shared-to-par` only applies to `shared struct` definitions",
            wrong_kind_files[0].filename
        );
        process::exit(1);
    }
    if def_files.is_empty() {
        eprintln!(
            "error: no `shared struct {type_name}` found in any module under `{}/src/` — `karac migrate shared-to-par` rewrites the type definition in place, so the type must be defined somewhere in the project",
            root.display()
        );
        process::exit(1);
    }
    if def_files.len() > 1 {
        let names: Vec<String> = def_files.iter().map(|p| p.filename.clone()).collect();
        eprintln!(
            "error: multiple `shared struct {type_name}` definitions found across the project ({} files); each migration target must be unique. Files: {}",
            def_files.len(),
            names.join(", ")
        );
        process::exit(1);
    }

    // Stage 1b: compute per-field Atomic/Mutex classification (L215c).
    // On by default; `--no-atomic` clears `atomic` and restores the
    // L215a–b4 behavior (every mut field is Mutex[T] and the consumer-
    // rewrite wraps every site). Project-mode only — single-file mode
    // lacks workspace visibility for the "every write is a bare `=`
    // assign" judgment, so its `atomic` is always false.
    let mut_fields = crate::ownership::collect_struct_mut_field_names(
        type_name,
        &def_files[0].pipeline.parsed.program.items,
    );
    let field_kinds: std::collections::HashMap<String, crate::ownership::FieldWrapKind> = if atomic
    {
        let project_files: Vec<crate::ownership::ProjectMigrationFile<'_>> = prepared
            .iter()
            .map(|f| crate::ownership::ProjectMigrationFile {
                program_items: &f.pipeline.parsed.program.items,
                type_ctx: f.pipeline.typed.as_ref().map(|t| {
                    crate::ownership::ConsumerRewriteTypeCtx {
                        pattern_binding_types: &t.pattern_binding_types,
                        method_callee_types: &t.method_callee_types,
                    }
                }),
            })
            .collect();
        crate::ownership::classify_field_wrap_kinds(
            type_name,
            &mut_fields,
            &def_files[0].pipeline.parsed.program.items,
            &project_files,
        )
    } else {
        std::collections::HashMap::new()
    };
    // L215c-cons — Atomic-classified fields' consumer sites are now
    // auto-rewritten by `build_consumer_rewrite_edits_with_mut_fields`:
    // bare `c.f = v` writes become `c.f.store(v, MemoryOrdering.Release)`
    // and bare `c.f` reads become `c.f.load(MemoryOrdering.Acquire)`.
    // The Mutex-classified fields continue to receive the lock-wrap
    // shape from the same walker. Pass the full mut-fields set as the
    // rewrite target and the Atomic subset as the dispatch discriminator.
    let atomic_fields: std::collections::HashSet<String> = field_kinds
        .iter()
        .filter_map(|(name, k)| match k {
            crate::ownership::FieldWrapKind::Atomic => Some(name.clone()),
            crate::ownership::FieldWrapKind::Mutex => None,
        })
        .collect();
    let atomic_field_count = atomic_fields.len();

    // Stage 2: run the type-def + consumer rewrite per file with the
    // classifier-aware emitter for the type def, and the Mutex-only
    // subset for the consumer wrap.
    let mut plans: Vec<FileMigrationPlan> = Vec::with_capacity(prepared.len());
    for file in &prepared {
        let typedef_edits = if file.has_shared_def {
            crate::ownership::build_fix_diff_edits_with_field_kinds(
                type_name,
                crate::ownership::BindingKind::Shared,
                &file.pipeline.parsed.program.items,
                &field_kinds,
            )
        } else {
            Vec::new()
        };
        let type_ctx =
            file.pipeline
                .typed
                .as_ref()
                .map(|t| crate::ownership::ConsumerRewriteTypeCtx {
                    pattern_binding_types: &t.pattern_binding_types,
                    method_callee_types: &t.method_callee_types,
                });
        let consumer_edits = crate::ownership::build_consumer_rewrite_edits_with_mut_fields(
            type_name,
            &file.pipeline.parsed.program.items,
            type_ctx,
            &mut_fields,
            &atomic_fields,
        );
        let mut edits: Vec<crate::resolver::TextEdit> = typedef_edits;
        edits.extend(consumer_edits);
        edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
        edits.dedup_by(|a, b| {
            a.offset == b.offset && a.length == b.length && a.replacement == b.replacement
        });
        if edits.is_empty() {
            continue;
        }
        plans.push(FileMigrationPlan {
            filename: file.filename.clone(),
            source: file.source.clone(),
            edits,
        });
    }

    if plans.is_empty() {
        println!(
            "(no migration edits needed for `{type_name}` across {} module(s) under {})",
            walked.modules.len(),
            root.display()
        );
        return;
    }

    if apply && !force && workspace_has_uncommitted_changes(&root.to_string_lossy()) {
        eprintln!(
            "error: workspace has uncommitted changes — refusing to run `karac migrate --apply` without `--force`"
        );
        eprintln!(
            "       commit or stash pending work first, or re-run with `--force` to bypass the guard."
        );
        process::exit(1);
    }

    let total_edits: usize = plans.iter().map(|p| p.edits.len()).sum();
    if !apply {
        println!(
            "would apply {total_edits} migration edit(s) across {} file(s) for `{type_name}`:",
            plans.len()
        );
    }
    for plan in &plans {
        emit_migration_for_file(plan, apply);
    }
    if !apply {
        println!(
            "(dry-run — re-run with `--apply` to write changes; consumer-site lock-block wraps cover assign / compound-assign writes, reads, and mutating method calls against bindings of `{type_name}` across the project — including type-inferred bindings in each file that typechecks)"
        );
        if atomic_field_count > 0 {
            println!(
                "(note: {atomic_field_count} field(s) on `{type_name}` were classified as `Atomic[T]` — their consumer assigns rewritten to `.store(v, MemoryOrdering.Release)` and reads rewritten to `.load(MemoryOrdering.Acquire)`)"
            );
        }
    } else if atomic_field_count > 0 {
        println!(
            "(note: {atomic_field_count} field(s) on `{type_name}` were rewritten as `Atomic[T]` — their consumer assigns auto-rewritten to `.store(v, MemoryOrdering.Release)` and reads to `.load(MemoryOrdering.Acquire)`)"
        );
    }
}

/// Outcome of running the migration pipeline against a single file.
enum FileMigrationOutcome {
    /// Parse failed; the inner messages are pre-formatted error lines.
    ParseFailed(Vec<String>),
    /// A struct named `<Type>` exists in this file but is not a
    /// `shared struct` (`shared-to-par` is the only migration kind today,
    /// so a plain struct of the same name is "you ran the wrong tool").
    WrongKind,
    /// No struct named `<Type>` in this file. Single-file mode treats
    /// this as a hard error (the def must live in the migration file);
    /// project-mode bypasses this enum entirely and computes consumer
    /// edits via [`build_consumer_rewrite_edits_with_mut_fields`].
    NoStructDef,
    /// File defines `shared struct <Type>` and edits were computed.
    Ok(FileMigrationPlan),
}

/// Per-file rewrite payload — `filename` + `source` are carried through
/// so the emitter can compute line/column previews and the apply path
/// can write the rewritten bytes back without re-reading.
struct FileMigrationPlan {
    filename: String,
    source: String,
    edits: Vec<crate::resolver::TextEdit>,
}

/// Run the parse → resolve → typecheck → rewrite pipeline against a
/// single file's source. Shared between single-file and project-mode
/// entry points. The struct-definition lookup happens here so the
/// caller can distinguish the three "no struct def in this file" /
/// "struct def is a plain struct" / "struct def is shared" cases.
fn compute_migration_edits_for_file(
    filename: &str,
    source: &str,
    type_name: &str,
) -> FileMigrationOutcome {
    let mut pipeline = Pipeline::new(filename, source);
    if pipeline.has_parse_errors() {
        let msgs: Vec<String> = pipeline
            .parsed
            .errors
            .iter()
            .map(|err| {
                format!(
                    "error[parse]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                )
            })
            .collect();
        return FileMigrationOutcome::ParseFailed(msgs);
    }
    pipeline.resolve();
    pipeline.typecheck();

    let struct_def = pipeline
        .parsed
        .program
        .items
        .iter()
        .find_map(|it| match it {
            Item::StructDef(s) if s.name == type_name => Some(s),
            _ => None,
        });
    let has_shared_def = match struct_def {
        Some(s) if s.is_shared => true,
        Some(_) => return FileMigrationOutcome::WrongKind,
        None => false,
    };

    let typedef_edits = if has_shared_def {
        crate::ownership::build_fix_diff_edits(
            type_name,
            crate::ownership::BindingKind::Shared,
            &pipeline.parsed.program.items,
        )
    } else {
        Vec::new()
    };
    let type_ctx = pipeline
        .typed
        .as_ref()
        .map(|t| crate::ownership::ConsumerRewriteTypeCtx {
            pattern_binding_types: &t.pattern_binding_types,
            method_callee_types: &t.method_callee_types,
        });
    let consumer_edits = crate::ownership::build_consumer_rewrite_edits_in_program(
        type_name,
        &pipeline.parsed.program.items,
        type_ctx,
    );

    let mut edits: Vec<crate::resolver::TextEdit> = typedef_edits;
    edits.extend(consumer_edits);
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    edits.dedup_by(|a, b| {
        a.offset == b.offset && a.length == b.length && a.replacement == b.replacement
    });

    if has_shared_def {
        FileMigrationOutcome::Ok(FileMigrationPlan {
            filename: filename.to_string(),
            source: source.to_string(),
            edits,
        })
    } else {
        FileMigrationOutcome::NoStructDef
    }
}

/// Render the dry-run preview block or apply the plan to disk. Shared
/// between single-file and project-mode emitters so the per-file
/// output shape stays identical across both paths. The single-file
/// dry-run footer and the project-mode top-level header/footer are
/// emitted by the respective callers, not here.
fn emit_migration_for_file(plan: &FileMigrationPlan, apply: bool) {
    let filename = &plan.filename;
    let source = &plan.source;
    let sorted = &plan.edits;
    if !apply {
        println!(
            "would apply {} migration edit(s) to {filename}:",
            sorted.len()
        );
        for edit in sorted.iter().rev() {
            let original = source
                .get(edit.offset..edit.offset.saturating_add(edit.length))
                .unwrap_or("<?>");
            let (line, col) = crate::byte_offset_to_line_col(source, edit.offset);
            let preview = if edit.length == 0 {
                format!("(insert) → `{}`", edit.replacement)
            } else {
                format!("`{}` → `{}`", original, edit.replacement)
            };
            println!("  {filename}:{line}:{col}: {preview}");
        }
        return;
    }

    let mut rewritten = source.clone();
    for edit in sorted {
        let end = edit.offset.saturating_add(edit.length);
        if end > rewritten.len() {
            eprintln!(
                "error: migrate would write past end of file ({} > {}) — aborting without modifying {filename}",
                end,
                rewritten.len()
            );
            process::exit(1);
        }
        rewritten.replace_range(edit.offset..end, &edit.replacement);
    }
    if let Err(e) = std::fs::write(filename, &rewritten) {
        eprintln!("error: failed to write {filename}: {e}");
        process::exit(1);
    }
    println!("applied {} migration edit(s) to {filename}", sorted.len());
}

/// Returns `true` when `git status --porcelain` reports any modified
/// or untracked files. The check is opportunistic — when `git` is
/// absent, the path isn't a git repo, or the invocation fails for any
/// other reason, the result is `false` (no guard rather than spurious
/// rejection). The intent is to prevent `karac migrate --apply` from
/// burying user work under a tool-applied diff, not to enforce a
/// universal pre-flight check.
fn workspace_has_uncommitted_changes(filename: &str) -> bool {
    let working_dir = std::path::Path::new(filename)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let Ok(output) = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&working_dir)
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    !output.stdout.is_empty()
}

// ── Tests ────────────────────────────────────────────────────────

/// Emit one JSONL test-runner event on stdout. Schema documented in
/// `docs/design.md § Testing › Test runner output format`. The discriminator
/// key is `"type"`, matching the build pipeline's [`emit_jsonl_event`] —
/// JSONL clients consume one shape across all `karac` outputs.
fn emit_test_event(event: &str, fields: &str) {
    if fields.is_empty() {
        println!("{{\"type\":{}}}", json_string(event));
    } else {
        println!("{{\"type\":{},{}}}", json_string(event), fields);
    }
}

/// Render a module path for the qualified test ID, e.g.
/// `db.connection::test_reconnect`. The crate-root module renders as
/// `<root>` so users can distinguish a test in the entry file from any
/// other.
fn module_label(path: &[String]) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        path.join(".")
    }
}

#[derive(Debug, Clone)]
struct DiscoveredTest {
    module_id: usize,
    fn_name: String,
    qualified: String,
    /// Fully-qualified resource paths (e.g. `"db.UserDB"`) the test
    /// declares via `#[test(requires = [...])]`. Empty when the test has
    /// no `requires` clause; the runner gates execution on the probe
    /// result for each entry.
    requires: Vec<String>,
    /// `#[with_provider(resource_path, constructor_expr)]` fixtures on
    /// the test, preserved in source order (outer-to-inner). The runner
    /// evaluates each constructor before the test body and pushes a
    /// matching provider frame so resource-method calls inside the test
    /// resolve against the fixture. See design.md § Testing.
    with_providers: Vec<WithProviderFixture>,
    /// Per-test timeout in seconds from `#[test(timeout_seconds = N)]`.
    /// `None` when the attribute is absent; the runner then falls back to
    /// the kara.toml `[test].timeout_seconds`, the `KARAC_TEST_TIMEOUT_SECS`
    /// env var, and finally the 30 s default (phase-7 line 847 sub-step 3).
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct WithProviderFixture {
    /// Fully-qualified resource path (e.g. `"Clock"` or `"db.UserDB"`).
    resource_path: String,
    /// Constructor expression — evaluated at test setup to produce the
    /// provider value bound into the frame. Arbitrary expression; a
    /// `panic` / runtime error / control-flow exit during evaluation
    /// produces `provider_construction_failed`.
    constructor: crate::ast::Expr,
}

/// Stable opaque interpreter-side identifier for an `Item::TestCase`.
/// The synthesized `Item::Function` (see [`lower_test_case_to_function`])
/// registers under this name so [`Interpreter::run_test_function`] can
/// dispatch through the regular `call_function` path with no extra
/// branching. Format: `__test_<sanitized-module-label>_<line>_<8-hex>`.
///
/// The hash prefix is `blake3(case_name)[..8]` — first 8 hex chars of
/// the case name's blake3 digest. Two cases at the same (module, line)
/// with different names can't both legally exist (one source line, one
/// item), so the line component already pins identity; the hash is a
/// belt-and-braces guard against module-path edge cases (synthetic
/// label collisions across re-export scaffolds, etc.) and gives the
/// mangled name a recognizable shape even when several cases share a
/// line through future macro expansion. Dots in the module label
/// become underscores so the mangled string stays a single contiguous
/// token in debugger / profiler views.
fn mangled_test_function_name(module_label: &str, line: usize, case_name: &str) -> String {
    let label_safe: String = module_label
        .chars()
        .map(|c| if c == '.' || c == ':' { '_' } else { c })
        .collect();
    let digest = blake3::hash(case_name.as_bytes());
    let hex = digest.to_hex();
    format!("__test_{}_{}_{}", label_safe, line, &hex.as_str()[..8])
}

/// Synthesize an `Item::Function` shell from an `Item::TestCase` so
/// the regular resolve / typecheck / interpret pipeline can chew the
/// body without growing TestCase-specific arms in every phase. The
/// synthesized function has:
///
/// - the mangled name from [`mangled_test_function_name`]
/// - the case body, cloned verbatim
/// - no params, no self-param, no return type, no effects, no
///   contracts — the runner calls it as `call_function(name, &[])`
///   and inspects `runtime_errors` for failure details, so any
///   declared signature surface would be unused.
/// - `is_pub: false`, `is_private: false` — visibility is already
///   rejected at the parse site; the synthesized function is
///   module-internal regardless.
/// - the attribute list copied from the TestCase. Slice 4 lifts
///   `#[test(requires=[...])]` / `#[with_provider(...)]` extraction
///   onto `TestCase.attributes`; until then the field carries
///   whatever the parser attached without behavior change.
fn lower_test_case_to_function(tc: &crate::ast::TestCase, mangled_name: String) -> Function {
    Function {
        span: tc.span.clone(),
        attributes: tc.attributes.clone(),
        doc_comment: tc.doc_comment.clone(),
        is_pub: false,
        is_private: false,
        is_unsafe: false,
        is_comptime: false,
        name: mangled_name,
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        return_type: None,
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: tc.body.clone(),
        stdlib_origin: false,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        is_gpu: false,
        inline_hint: None,
        is_cold: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        abi: None,
    }
}

/// Rewrite every `Item::TestCase` in the program tree to a
/// synthesized `Item::Function` *and* collect the parallel
/// `DiscoveredTest` list in one pass. The mangled function name on
/// each lowered `Item::Function` matches the `fn_name` field on the
/// returned `DiscoveredTest`, so the runner's later
/// `Interpreter::run_test_function(t.fn_name)` finds the entry the
/// standard `register_items` walk already registered.
///
/// Lowering happens *before* the resolver / typechecker run on the
/// program tree. Without that ordering, a typo or undefined-symbol
/// reference inside a test body would slip past name resolution (the
/// no-op `TestCase` arms in resolver / typechecker skip the body
/// unread) and only surface as a runtime error in the per-test loop —
/// breaking the contract that compile failures exit non-zero with no
/// test events emitted.
///
/// Test cases are structural: `Item::TestCase` entries from
/// `test "case" { body }` syntax per design.md § Testing. The
/// convention-based `fn test_*` discovery is gone — helper functions
/// in `_test.kara` files (any name, including `fn test_*`) stay
/// `Item::Function` and are never picked up as tests, closing the
/// silent-skip failure mode where a project written to the design
/// silently ran zero tests because the runner walked `fn test_*`
/// instead of `Item::TestCase`.
fn lower_and_discover_test_cases(tree: &mut ProgramTree) -> Vec<DiscoveredTest> {
    let mut tests = Vec::new();
    for (mod_id, module) in tree.modules.iter_mut().enumerate() {
        if module.is_synthetic {
            continue;
        }
        if module.test_items_start.is_none() {
            continue;
        }
        let label = module_label(&module.path);
        let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len());
        for item in module.items.drain(..) {
            match item {
                Item::TestCase(tc) => {
                    let mangled = mangled_test_function_name(&label, tc.name_span.line, &tc.name);
                    tests.push(DiscoveredTest {
                        module_id: mod_id,
                        fn_name: mangled.clone(),
                        // User-visible qualifier — design.md § Testing
                        // pins this to the case-name string verbatim:
                        // the string `--filter` matches against, the
                        // `test` field on every JSONL event.
                        qualified: tc.name.clone(),
                        requires: extract_requires(&tc.attributes),
                        with_providers: extract_with_providers(&tc.attributes),
                        timeout_seconds: extract_timeout_seconds(&tc.attributes),
                    });
                    new_items.push(Item::Function(lower_test_case_to_function(&tc, mangled)));
                }
                other => new_items.push(other),
            }
        }
        module.items = new_items;
    }
    tests
}

/// Pull resource paths out of a `#[test(requires = [a.b, c.d])]` attribute.
/// Other `#[test(...)]` arg shapes are tolerated and ignored, so future
/// slices can add new keys (e.g. `cases = N`) without breaking earlier
/// runners. Non-path expressions in the array are silently dropped — the
/// parser will already have errored if the attribute body is malformed
/// (the typechecker leaves attribute values alone, so what reaches us is
/// well-formed but possibly not a path).
fn extract_requires(attributes: &[crate::ast::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attributes {
        if !attr.is_bare("test") {
            continue;
        }
        for arg in &attr.args {
            if arg.name.as_deref() != Some("requires") {
                continue;
            }
            let Some(value) = arg.value.as_ref() else {
                continue;
            };
            if let crate::ast::ExprKind::ArrayLiteral(elems) = &value.kind {
                for elem in elems {
                    if let Some(path) = expr_to_dotted_path(elem) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

/// Pull the per-test timeout from a `#[test(timeout_seconds = N)]` attribute
/// (phase-7 line 847 sub-step 3). Returns the first positive integer value
/// found across the test's attributes; `None` when absent. A non-positive or
/// non-integer value is ignored (it simply doesn't set a per-test override, so
/// the kara.toml / env / 30 s chain applies) — the parser already accepts any
/// expression in attribute args, and silently dropping a malformed value
/// matches `extract_requires`' tolerant stance toward unknown `#[test(...)]`
/// arg shapes.
fn extract_timeout_seconds(attributes: &[crate::ast::Attribute]) -> Option<u64> {
    for attr in attributes {
        if !attr.is_bare("test") {
            continue;
        }
        for arg in &attr.args {
            if arg.name.as_deref() != Some("timeout_seconds") {
                continue;
            }
            if let Some(value) = arg.value.as_ref() {
                if let crate::ast::ExprKind::Integer(n, _) = &value.kind {
                    if *n > 0 {
                        return Some(*n as u64);
                    }
                }
            }
        }
    }
    None
}

/// Resolve the effective per-test timeout from the precedence chain
/// (phase-7 line 847): a per-test `#[test(timeout_seconds = N)]` attribute >
/// the kara.toml `[test].timeout_seconds` > the `KARAC_TEST_TIMEOUT_SECS` env
/// var > the built-in 30 s default. Each layer is an `Option<u64>` of seconds;
/// the first present wins.
fn resolve_test_timeout(
    per_test: Option<u64>,
    manifest_default: Option<u64>,
    env_default: Option<u64>,
) -> std::time::Duration {
    let secs = per_test.or(manifest_default).or(env_default).unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// Pull `#[with_provider(resource_path, constructor_expr)]` fixtures out
/// of a function's attribute list. Multiple attributes are preserved in
/// source order (outer-to-inner, matching design.md's stacking rule).
/// Attributes with fewer than two positional args are silently dropped —
/// the parser will already have reported a shape error if the attribute
/// body is malformed.
fn extract_with_providers(attributes: &[crate::ast::Attribute]) -> Vec<WithProviderFixture> {
    let mut out = Vec::new();
    for attr in attributes {
        if !attr.is_bare("with_provider") {
            continue;
        }
        if attr.args.len() < 2 {
            continue;
        }
        // Expect two positional args (name is None); tolerate named-
        // attribute shape by pulling values only when present.
        let Some(resource_expr) = attr.args[0].value.as_ref() else {
            continue;
        };
        let Some(constructor_expr) = attr.args[1].value.as_ref() else {
            continue;
        };
        let Some(resource_path) = expr_to_dotted_path(resource_expr) else {
            continue;
        };
        out.push(WithProviderFixture {
            resource_path,
            constructor: constructor_expr.clone(),
        });
    }
    out
}

/// Reconstruct a dotted-path string from a parsed expression. The parser
/// breaks `db.UserDB` into `FieldAccess(Path(["db"]), "UserDB")` (and
/// deeper chains nest the same way), so walking the AST left-to-right
/// produces the original surface text. Returns `None` for anything
/// that is not a pure dotted identifier chain — such elements simply do
/// not contribute a resource entry.
fn expr_to_dotted_path(expr: &crate::ast::Expr) -> Option<String> {
    use crate::ast::ExprKind;
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::Path { segments, .. } => {
            if segments.is_empty() {
                None
            } else {
                Some(segments.join("."))
            }
        }
        ExprKind::FieldAccess { object, field } => {
            let prefix = expr_to_dotted_path(object)?;
            Some(format!("{prefix}.{field}"))
        }
        _ => None,
    }
}

/// True iff the resource is reachable. Order of precedence matches
/// `docs/design.md § Testing › Resource availability probing`:
///   1. `[test.resources]` shell command — present iff the manifest
///      lists one for this resource path; available iff exit 0.
///   2. Env var `KARA_RESOURCE_<UPPER_DOT_SLASH_>` (dots → underscores)
///      — available iff set and non-empty.
fn probe_resource(resource: &str, overrides: &std::collections::BTreeMap<String, String>) -> bool {
    if let Some(cmd) = overrides.get(resource) {
        return run_health_check(cmd);
    }
    let env_var = resource_env_var(resource);
    matches!(std::env::var(&env_var), Ok(v) if !v.is_empty())
}

/// Translate a dotted resource path into the env-var probe name. Matches
/// the design (`KARA_RESOURCE_DB_USERDB` from `db.UserDB`): the prefix is
/// fixed so the namespace is reserved, dots become underscores so the
/// shell can set the variable without quoting, and the result is upper-
/// cased so case-insensitive shells (Windows `cmd`) still hit it.
fn resource_env_var(resource: &str) -> String {
    format!(
        "KARA_RESOURCE_{}",
        resource.replace('.', "_").to_uppercase()
    )
}

/// Run a shell health-check command and report whether it succeeded.
/// Uses `sh -c` so users can write the command exactly as they would
/// in a terminal (pipes, env-var interpolation, quoting). Stdout and
/// stderr are captured (not forwarded) so a noisy probe does not
/// pollute the JSONL stream — the only signal we care about is the
/// exit code.
fn run_health_check(cmd: &str) -> bool {
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

fn cmd_test(filter: Option<String>, all: bool, interp: bool) {
    // `interp` is only consulted inside the `cfg(feature = "llvm")` JIT
    // dispatch below; on a non-`llvm` build the interpreter is the only
    // executor, so the flag is accepted (for CLI uniformity) but unused.
    #[cfg(not(feature = "llvm"))]
    let _ = interp;
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            // Surface manifest errors as a JSONL diagnostic event so consumers
            // can recognize and act on them; then exit non-zero before any
            // run_start/summary so the schema stays clean (no half-runs).
            emit_test_event(
                "manifest_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };
    // Merge `[target.<triple>].dependencies` / `.dev-dependencies` overlays for
    // the host-default triple so per-target deps participate in test-mode
    // resolution exactly as they do under `karac build` (resolver follow-up
    // (e)). Applied before the `has_resolvable_deps` gate so a project whose
    // deps are declared *only* under `[target.*]` still resolves.
    let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));

    // Toolchain pin (tracker line 892). Same enforcement as
    // cmd_build_project — runs before walk so a failing toolchain
    // gate halts before any test run_start lands in the stream.
    if !enforce_toolchain_pin(&root, OutputMode::Jsonl) {
        process::exit(1);
    }

    // Test-mode dep resolution (tracker line 884). Runs only when the
    // manifest declares at least one dep entry (regular, dev, or
    // workspace) — solo packages pay zero overhead. dev-dependencies
    // participate here (the test-vs-build split) so a test_dep declared
    // under `[dev-dependencies]` is resolved and recorded into the
    // lockfile alongside the build-mode deps. Errors surface as a
    // `dep_resolution_error` event and abort before any run_start.
    // The resolution is kept (resolver-block follow-up (f)): its
    // path-dep packages are walked below so root-package tests can
    // `import <pkg>.…` exactly as production code under `karac build`
    // can. `run_dep_resolution` emits through `emit_dep_diagnostic`,
    // whose Jsonl arm produces the same `dep_resolution_error`
    // envelope as before (registry/git unsupported still downgrade —
    // they surface as `dep_resolution_warning` and resolution is
    // skipped, matching the build flow).
    let has_resolvable_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let dep_resolution: Option<crate::dep_resolver::Resolution> = if has_resolvable_deps {
        // `karac test` has no `--no-proxy` flag; the fetch path self-gates on
        // an explicitly-configured proxy (see `run_dep_resolution`), so a
        // registry dep is fetched only when the operator points at a real one.
        match run_dep_resolution(
            &root,
            mf.clone(),
            OutputMode::Jsonl,
            None,
            true,
            false,
            true,
        ) {
            Ok(r) => r,
            Err(()) => process::exit(1),
        }
    } else {
        None
    };

    let walk_opts = WalkerOpts {
        include_tests: true,
        ..WalkerOpts::default()
    };
    let walked = match walker::walk_project(&root, walk_opts) {
        Ok(w) => w,
        Err(e) => {
            emit_test_event(
                "walker_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };

    // Cross-package module loading for the test surface (phase-5 line
    // 898 follow-up iii / resolver-block follow-up (f)): walk each
    // resolved path-dep so its modules join the tree under package-
    // prefixed paths. Dep test companions stay excluded —
    // `dep_package_walks` walks deps with `include_tests: false`, so
    // `merge_test_companions` below only ever folds the *root*
    // package's `_test.kara` files; only the root package's tests run.
    let dep_walks =
        match dep_package_walks(dep_resolution.as_ref(), walk_opts.target, OutputMode::Jsonl) {
            Ok(v) => v,
            Err(()) => process::exit(1),
        };

    let built = match module::build_program_tree_with_deps(
        &walked,
        &dep_walks,
        BuildTreeOpts {
            merge_test_companions: true,
        },
    ) {
        Ok(ok) => ok,
        Err(e) => {
            emit_test_event(
                "build_tree_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };

    let BuildTreeOk {
        mut tree,
        parse_errors,
    } = built;

    // Lower every `Item::TestCase` to a synthesized `Item::Function`
    // and collect the parallel `DiscoveredTest` list, *before* resolve
    // / typecheck run. Putting the lowering ahead of name resolution
    // is what gives the runner its compile-failure contract: an
    // undefined symbol inside a test body produces a resolve error
    // here at the global step, and the runner exits non-zero with no
    // test events. See `lower_and_discover_test_cases`.
    let discovered_tests = lower_and_discover_test_cases(&mut tree);

    let cycles = module::detect_cycles(&tree);

    let resolve_errors: Vec<ModuleResolveErrors> = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree)
    } else {
        Vec::new()
    };

    // Phase-8 line 49 prereq 4 — mirror the build path: lift
    // `[lints].allow_unstable_api` from the manifest into the
    // per-module typecheck overrides so `karac test` honors the
    // global opt-in.
    let mut module_lint_overrides = crate::lints::CliLintOverrides::default();
    module_lint_overrides.apply_manifest_lints(&mf.lints);
    let type_errors: Vec<ModuleTypeErrors> =
        if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
            typecheck_modules(&tree, &module_lint_overrides)
        } else {
            Vec::new()
        };

    let compile_failed = !parse_errors.is_empty()
        || !cycles.is_empty()
        || !resolve_errors.is_empty()
        || !type_errors.is_empty();

    if compile_failed {
        for entry in parse_errors_jsonl(&parse_errors) {
            println!("{entry}");
        }
        for entry in cycles_jsonl(&cycles, &tree) {
            println!("{entry}");
        }
        for entry in resolve_errors_jsonl(&resolve_errors) {
            println!("{entry}");
        }
        for entry in type_errors_jsonl(&type_errors) {
            println!("{entry}");
        }
        process::exit(1);
    }

    // Apply filter to the discovery list built before resolve. Sort
    // by (module_id, fn_name) so order is stable across runs —
    // declaration order within a module (each case lives on a
    // distinct source line, and the mangled name embeds the line, so
    // sorting by mangled name matches source order), modules in walk
    // order. LLM consumers diffing two test runs depend on this.
    let mut tests = discovered_tests;
    if let Some(needle) = &filter {
        tests.retain(|t| t.qualified.contains(needle.as_str()));
    }
    tests.sort_by(|a, b| {
        a.module_id
            .cmp(&b.module_id)
            .then_with(|| a.fn_name.cmp(&b.fn_name))
    });

    let run_started = std::time::Instant::now();
    emit_test_event("run_start", &format!("\"total_tests\":{}", tests.len()));

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;

    // One merged execution program for the whole suite, mirroring `karac
    // run`'s super-program. The previous per-module items-only `Program`
    // meant any name imported from a sibling module — or, with cross-
    // package loading, from a path-dep — resolved and typechecked at the
    // tree level but was *absent* at execution: the interpreter hit its
    // "should be caught by resolver" unreachable panic, and the JIT path
    // failed to compile the missing symbol. Merging all modules is
    // execution-equivalent to `karac run` and `karac build`, both of
    // which already concatenate the full tree (dep modules included), so
    // it imposes no constraint those surfaces don't. The resolve +
    // typecheck here feed the executor only — the compile-failure
    // contract already ran tree-wide above, imports and visibility
    // included, so a test body reaches this point only if every name it
    // touches resolved under module scoping.
    let exec_program = build_super_program_for_run(&tree);
    let exec_resolved = Resolver::new(&exec_program).resolve();
    let exec_typed = crate::typechecker::TypeChecker::new(&exec_program, &exec_resolved).check();

    // One persistent JIT runner for the whole suite (amortizes LLVM init
    // across tests; re-spawns on a faulting test). Lazily spawns on the
    // first JIT-dispatched test, so a suite running under `--interp` /
    // `KARAC_TEST_JIT=0` or built without the feature pays nothing.
    #[cfg(feature = "llvm")]
    let mut batch_runner = crate::test_jit_dispatch::TestBatchRunner::new(
        std::env::temp_dir().join(format!("karac_test_batch_{}", std::process::id())),
    );
    // Modules whose tests override a TRAIT-LESS resource via `#[with_provider]`
    // can't use the persistent-module cache. A trait-less resource — a prelude
    // ambient one (`Clock`/`Env`/…) OR a user `effect resource R;` with no
    // provider trait — has no canonical method order: codegen derives the order
    // per module from the override type's inherent impl at the `with_provider`
    // site, so the `R.method()` call site can only dispatch correctly when the
    // `with_provider` lives in the SAME module. The cache splits the two (the
    // `with_provider` lands in the per-test `main`, the call site in the shared
    // persistent module), silently dropping the override — `R.method()` falls
    // through to the const-0 / FFI default, or a faulting ctor errors with "no
    // method order for resource". So any test with a trait-less fixture runs
    // each test self-contained (full mode — see `TestBatchRunner::cache_module`).
    //
    // TRAIT-FUL user resources (`effect resource R: T;`) are exempt: their
    // vtable comes from the impl blocks that live in the persistent module, and
    // the trait pins a canonical method order the call site shares — so the
    // split is sound and they keep the cache. Build the set of trait-ful
    // resource names from the whole tree; a fixture forces full mode unless its
    // resource is in that set (an unrecognized / qualified name falls to full
    // mode, which is always correct, just uncached).
    #[cfg(feature = "llvm")]
    let traitful_resources: std::collections::HashSet<&str> = tree
        .modules
        .iter()
        .flat_map(|m| m.items.iter())
        .filter_map(|it| match it {
            crate::ast::Item::EffectResource(d) if d.provider_trait.is_some() => {
                Some(d.name.as_str())
            }
            _ => None,
        })
        .collect();
    #[cfg(feature = "llvm")]
    let full_mode_fixture_modules: std::collections::HashSet<usize> = tests
        .iter()
        .filter(|t| {
            t.with_providers
                .iter()
                .any(|fx| !traitful_resources.contains(fx.resource_path.as_str()))
        })
        .map(|t| t.module_id)
        .collect();

    // Per-test timeout precedence inputs (phase-7 line 847 sub-steps 2+3),
    // computed once: the kara.toml `[test].timeout_seconds` and the
    // `KARAC_TEST_TIMEOUT_SECS` env var. The per-test attribute layer is read
    // from each `DiscoveredTest` inside the loop, and `resolve_test_timeout`
    // applies the full chain (per-test attr > kara.toml > env var > 30 s).
    let manifest_test_timeout: Option<u64> = mf.test_timeout_seconds;
    let env_test_timeout: Option<u64> = std::env::var("KARAC_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());

    for t in &tests {
        // `#[test(requires = [X])]` and `#[with_provider(X, ...)]` for the
        // *same* resource are contradictory: one gates on an external
        // service, the other supplies a fake. Per design.md § Testing,
        // reject at discovery time with a structured `test_fail` carrying
        // `reason = "requires_and_with_provider_conflict"`. Must precede
        // the missing-requires probe — a test shape error always beats a
        // resource-availability outcome, regardless of `--all`.
        let conflicts = conflict_resources(&t.requires, &t.with_providers);
        if !conflicts.is_empty() {
            failed += 1;
            emit_test_event("test_fail", &test_fail_conflict_fields(t, &conflicts));
            continue;
        }

        // Probe `requires` next — a skipped test must not pay the
        // per-module compile cost and must not load the interpreter.
        // Both halves of the contract (silent skip by default, hard
        // failure under `--all`) need the same `missing` list, so we
        // compute it once and branch.
        let missing = missing_resources(&t.requires, &mf.test_resources);
        if !missing.is_empty() {
            if all {
                failed += 1;
                emit_test_event(
                    "test_fail",
                    &test_fail_unsatisfied_requires_fields(t, &missing),
                );
            } else {
                skipped += 1;
                emit_test_event(
                    "test_skip",
                    &test_skip_unsatisfied_requires_fields(t, &missing),
                );
            }
            continue;
        }

        // `Item::TestCase` lowering has already happened at the global
        // tree level (see `lower_and_discover_test_cases`), so the merged
        // program hands the standard resolver / typechecker / interpreter
        // pipeline a regular `Item::Function` body that
        // `run_test_function(t.fn_name)` looks up through the usual
        // `call_function` path (mangled names embed the module label, so
        // merging cannot collide two modules' test functions).
        let program_ref = &exec_program;
        let typed_ref = &exec_typed;
        let module = &tree.modules[t.module_id];

        let test_file_path = module
            .test_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

        // Slice c.3 — JIT subprocess dispatch. Bypasses the per-test
        // `Interpreter` and instead synthesizes a main calling `t.fn_name`
        // (via `test_main_synth`), compiles to IR, spawns `karac_jit_runner`,
        // and parses stderr for the `KARAC_TEST_FAILURE` JSONL marker emitted
        // by c.1's runtime bridge. Same JSONL event emitters fire below —
        // only the outcome source changes.
        //
        // JIT is the default execution path (L577 step (c), 2026-06-01),
        // symmetric with the `karac repl` flip (`e06d877a`). All four
        // codegen-path gaps that held this back are closed: (a) cross-boundary
        // ambient `with_provider` (`acd63e65`), (b) contract-fault category
        // (`a68e72b2`), (c) trait-less user-resource dispatch (`2cf859d8`), and
        // (d) diverging-tail IR (`6307933e`) — the last of which made a
        // panicking fixture ctor *compile* and surface a non-zero exit.
        // `KARAC_TEST_JIT=0` is now the regression-bisect escape hatch rather
        // than `=1` being the opt-in.
        //
        // The `provider_construction_failed` outcome distinction (a faulting
        // ctor reported separately from a faulting body, with the resource
        // named and `duration_ms` 0 — the interpreter's behaviour) is
        // preserved under JIT via the synth main's per-ctor `PROVIDER_CTOR_MARKER`
        // checkpoints: `dispatch` counts them in the captured stdout and
        // returns `provider_ctor_failed: Some(idx)` when a ctor faulted before
        // the body ran (see `test_main_synth` / `test_jit_dispatch`). The
        // `Completed` arm below maps that to the same event the interpreter
        // path emits.
        // LLJIT Slice 5 (JIT-default flip): under `--features llvm` the JIT
        // batch runner is now the DEFAULT `karac test` executor. The
        // interpreter is the retained dev/debug backend, reachable via
        // `--interp` (the `interp` param) or the `KARAC_TEST_JIT=0`
        // regression-bisect escape hatch. So dispatch to the JIT unless
        // either opt-out fires. (Slice 1 had this as opt-in `== Ok("1")`; the
        // sign-off to flip landed this session — see
        // docs/spikes/lljit-productionization.md § Slice 5.)
        #[cfg(feature = "llvm")]
        if !interp && std::env::var("KARAC_TEST_JIT").as_deref() != Ok("0") {
            let timeout =
                resolve_test_timeout(t.timeout_seconds, manifest_test_timeout, env_test_timeout);
            let fixtures: Vec<(String, crate::ast::Expr)> = t
                .with_providers
                .iter()
                .map(|fx| (fx.resource_path.clone(), fx.constructor.clone()))
                .collect();
            let active_providers: Vec<String> = t
                .with_providers
                .iter()
                .map(|fx| fx.resource_path.clone())
                .collect();
            // Persistent batch runner: one `karac_jit_runner --test-batch`
            // subprocess for the whole suite (LLVM init paid once, not
            // per-test), re-spawned only when a faulting test exits it. See
            // `test_jit_dispatch::TestBatchRunner`.
            let use_cache = !full_mode_fixture_modules.contains(&t.module_id);
            let result = batch_runner.dispatch(
                t.module_id,
                use_cache,
                program_ref,
                &t.fn_name,
                &fixtures,
                &test_file_path,
                timeout,
            );
            match result {
                crate::test_jit_dispatch::JitTestResult::Completed {
                    outcome,
                    duration_ms,
                    provider_ctor_failed,
                } => {
                    if outcome.passed {
                        passed += 1;
                        emit_test_event(
                            "test_pass",
                            &format!(
                                "\"test\":{},\"duration_ms\":{}",
                                json_string(&t.qualified),
                                duration_ms
                            ),
                        );
                    } else if let Some(idx) = provider_ctor_failed {
                        // A fixture constructor faulted before the body ran:
                        // report `provider_construction_failed` for the failing
                        // resource with `duration_ms` 0, exactly as the
                        // interpreter path does. `idx` is the source-order
                        // index of the fixture whose ctor faulted.
                        failed += 1;
                        let resource = t
                            .with_providers
                            .get(idx)
                            .map(|fx| fx.resource_path.as_str())
                            .unwrap_or("");
                        let message = outcome
                            .message
                            .as_deref()
                            .unwrap_or("provider constructor failed");
                        emit_test_event(
                            "test_fail",
                            &test_fail_provider_construction_fields(t, resource, message),
                        );
                    } else {
                        failed += 1;
                        emit_test_event(
                            "test_fail",
                            &test_fail_fields_with_providers(
                                t,
                                &outcome,
                                &test_file_path,
                                duration_ms,
                                &active_providers,
                            ),
                        );
                    }
                }
                crate::test_jit_dispatch::JitTestResult::TimedOut { duration_ms } => {
                    failed += 1;
                    emit_test_event(
                        "test_timeout",
                        &format!(
                            "\"test\":{},\"timeout_s\":{},\"elapsed_ms\":{}",
                            json_string(&t.qualified),
                            timeout.as_secs(),
                            duration_ms
                        ),
                    );
                }
                crate::test_jit_dispatch::JitTestResult::SpawnFailed { message } => {
                    failed += 1;
                    let outcome = crate::interpreter::TestOutcome {
                        passed: false,
                        message: Some(message),
                        span: None,
                        left: None,
                        right: None,
                    };
                    emit_test_event(
                        "test_fail",
                        &test_fail_fields_with_providers(
                            t,
                            &outcome,
                            &test_file_path,
                            0,
                            &active_providers,
                        ),
                    );
                }
            }
            continue;
        }

        let mut interp = Interpreter::new(program_ref, typed_ref);
        interp.set_source_filename(&test_file_path);
        interp.register_for_tests();

        // Evaluate `#[with_provider(R, ctor)]` fixtures in source order,
        // pushing one provider frame per successful constructor. On the
        // first constructor failure we pop whatever we already pushed,
        // emit `provider_construction_failed`, and move to the next test
        // without running its body. Reset test state once up front so
        // constructor evaluation starts from a clean slate (same as
        // `run_test_function` does when it takes over).
        interp.reset_test_state();
        let mut pushed_frames: usize = 0;
        let mut constructor_failure: Option<(String, String)> = None;
        for fx in &t.with_providers {
            match interp.test_eval_provider_constructor(&fx.constructor) {
                Ok(v) => {
                    interp.test_push_provider(fx.resource_path.clone(), v);
                    pushed_frames += 1;
                }
                Err(msg) => {
                    constructor_failure = Some((fx.resource_path.clone(), msg));
                    break;
                }
            }
        }

        if let Some((resource, message)) = constructor_failure {
            for _ in 0..pushed_frames {
                interp.test_pop_provider_frame();
            }
            failed += 1;
            emit_test_event(
                "test_fail",
                &test_fail_provider_construction_fields(t, &resource, &message),
            );
            continue;
        }

        let active_providers: Vec<String> = t
            .with_providers
            .iter()
            .map(|fx| fx.resource_path.clone())
            .collect();

        // Per-test timeout (line 847). 30 s default — generous enough for
        // slow integration tests, tight enough that a runaway loop surfaces
        // in seconds rather than hours. Precedence (sub-steps 2+3, now live):
        // a per-test `#[test(timeout_seconds = N)]` attribute > the kara.toml
        // `[test].timeout_seconds` > the `KARAC_TEST_TIMEOUT_SECS` env var >
        // the 30 s default — resolved by `resolve_test_timeout`. Interpreter
        // polls the deadline at every statement boundary and raises
        // `ControlFlow::TimedOut` on the first observation past it, unified
        // with the existing par-cancel check point.
        let timeout =
            resolve_test_timeout(t.timeout_seconds, manifest_test_timeout, env_test_timeout);
        let deadline = std::time::Instant::now() + timeout;
        interp.set_test_deadline(Some(deadline));

        let started = std::time::Instant::now();
        let outcome = interp.run_test_function(&t.fn_name);
        let duration_ms = started.elapsed().as_millis();
        let timed_out = interp.timed_out;

        // Clear the deadline so any post-test interpreter use (e.g.
        // provider frame teardown) doesn't accidentally re-trigger.
        interp.set_test_deadline(None);

        // Pop every fixture frame before emitting the event so any error
        // handling below sees a clean stack for the next test.
        for _ in 0..pushed_frames {
            interp.test_pop_provider_frame();
        }

        if timed_out {
            failed += 1;
            emit_test_event(
                "test_timeout",
                &format!(
                    "\"test\":{},\"timeout_s\":{},\"elapsed_ms\":{}",
                    json_string(&t.qualified),
                    timeout.as_secs(),
                    duration_ms
                ),
            );
        } else if outcome.passed {
            passed += 1;
            emit_test_event(
                "test_pass",
                &format!(
                    "\"test\":{},\"duration_ms\":{}",
                    json_string(&t.qualified),
                    duration_ms
                ),
            );
        } else {
            failed += 1;
            emit_test_event(
                "test_fail",
                &test_fail_fields_with_providers(
                    t,
                    &outcome,
                    &test_file_path,
                    duration_ms,
                    &active_providers,
                ),
            );
        }
    }

    let total_duration_ms = run_started.elapsed().as_millis();
    emit_test_event(
        "summary",
        &format!(
            "\"total\":{},\"passed\":{},\"failed\":{},\"skipped\":{},\"duration_ms\":{}",
            tests.len(),
            passed,
            failed,
            skipped,
            total_duration_ms,
        ),
    );

    if failed > 0 {
        process::exit(1);
    }
}

/// Subset of `requires` whose resources are NOT currently available.
/// Order is preserved from the source list so the diagnostic reads in
/// declaration order — the runner emits this slice into the
/// `resources` field of the `test_skip`/`test_fail` event.
fn missing_resources(
    requires: &[String],
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    requires
        .iter()
        .filter(|r| !probe_resource(r, overrides))
        .cloned()
        .collect()
}

fn test_skip_unsatisfied_requires_fields(t: &DiscoveredTest, missing: &[String]) -> String {
    format!(
        "\"test\":{},\"reason\":\"unsatisfied_requires\",\"resources\":{}",
        json_string(&t.qualified),
        json_string_array(missing),
    )
}

fn test_fail_unsatisfied_requires_fields(t: &DiscoveredTest, missing: &[String]) -> String {
    // `--all` promotes the same condition to a failure. The shape mirrors a
    // normal `test_fail` (test, message) plus a `reason` + `resources` pair
    // so consumers that filter by `reason` work uniformly across skip- and
    // fail-events. `duration_ms` is 0 — the test never executed.
    let message = format!(
        "required resource{} unavailable: {}",
        if missing.len() == 1 { "" } else { "s" },
        missing.join(", "),
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"unsatisfied_requires\",\"resources\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string_array(missing),
        json_string(&message),
    )
}

/// Render a `Vec<String>` as a JSON array literal. Each element runs
/// through [`json_string`] for proper escaping.
fn json_string_array(items: &[String]) -> String {
    let mut s = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&json_string(item));
    }
    s.push(']');
    s
}

fn test_fail_fields(
    t: &DiscoveredTest,
    outcome: &TestOutcome,
    file_path: &str,
    duration_ms: u128,
) -> String {
    let mut s = format!(
        "\"test\":{},\"duration_ms\":{}",
        json_string(&t.qualified),
        duration_ms
    );
    if let Some(span) = &outcome.span {
        s.push_str(&format!(
            ",\"location\":{{\"file\":{},\"line\":{},\"col\":{}}}",
            json_string(file_path),
            span.line,
            span.column,
        ));
    }
    let message = outcome.message.as_deref().unwrap_or("test failed");
    s.push_str(&format!(",\"message\":{}", json_string(message)));
    // Typed fault category for contract failures (design.md § Contracts rule 2,
    // phase-9 step 7): so a consumer (CI / LLM) filters on a stable field rather
    // than string-matching the human message. Derived from the interpreter's
    // canonical fault text — the single source of truth that already
    // distinguishes the two categories (eval_call / method_call). Only emitted
    // for contract faults; ordinary assertion / panic failures carry no
    // `category`, same conditional-presence convention as `left`/`right`.
    if let Some(category) = contract_fault_category(message) {
        s.push_str(&format!(",\"category\":{}", json_string(category)));
    }
    if let Some(left) = &outcome.left {
        s.push_str(&format!(",\"left\":{}", json_string(left)));
    }
    if let Some(right) = &outcome.right {
        s.push_str(&format!(",\"right\":{}", json_string(right)));
    }
    s
}

/// Classify a test-failure message into a typed contract-fault category, or
/// `None` for a non-contract failure (assertion, plain panic, timeout, infra).
/// `contract predicate panicked` is checked **first**: a nested fault message
/// can read `contract predicate panicked: contract violated: …` (a contract
/// violation surfaced from inside a predicate's evaluation), which is a
/// predicate-panic, not a violation. The match strings are the canonical fault
/// names from design.md, emitted by both the interpreter (`eval_call` /
/// `method_call`) and codegen (`emit_panic`), so they don't drift.
fn contract_fault_category(message: &str) -> Option<&'static str> {
    if message.contains("contract predicate panicked") {
        Some("contract_predicate_panicked")
    } else if message.contains("contract violated") {
        Some("contract_violated")
    } else {
        None
    }
}

/// Like `test_fail_fields` but also emits a `providers` array listing
/// the fully-qualified resource paths active for the test. Per design.md
/// § Testing, pass events stay lean; only failure events grow this
/// field so consumers reading pass/fail diffs can attribute the failure
/// to the fixture stack. Empty provider lists still emit the field for
/// shape consistency — it's `"providers":[]` in that case.
fn test_fail_fields_with_providers(
    t: &DiscoveredTest,
    outcome: &TestOutcome,
    file_path: &str,
    duration_ms: u128,
    providers: &[String],
) -> String {
    let mut s = test_fail_fields(t, outcome, file_path, duration_ms);
    s.push_str(&format!(",\"providers\":{}", json_string_array(providers)));
    s
}

/// Intersection of `#[test(requires = [...])]` resources and
/// `#[with_provider(...)]` resource paths. Preserves `requires` order so
/// the conflict list reads in source declaration order.
fn conflict_resources(requires: &[String], with_providers: &[WithProviderFixture]) -> Vec<String> {
    let with_set: std::collections::BTreeSet<&str> = with_providers
        .iter()
        .map(|f| f.resource_path.as_str())
        .collect();
    requires
        .iter()
        .filter(|r| with_set.contains(r.as_str()))
        .cloned()
        .collect()
}

fn test_fail_conflict_fields(t: &DiscoveredTest, conflicts: &[String]) -> String {
    let message = format!(
        "resource{} cannot appear in both `requires` and `with_provider`: {}",
        if conflicts.len() == 1 { "" } else { "s" },
        conflicts.join(", "),
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"requires_and_with_provider_conflict\",\"resources\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string_array(conflicts),
        json_string(&message),
    )
}

/// `test_fail` event for `provider_construction_failed` — constructor
/// expression panicked, returned `Err`, or otherwise did not complete
/// normally. `duration_ms` is 0 — the test body never ran. Includes the
/// resource path whose constructor failed and the diagnostic message so
/// CI / LLM consumers can distinguish construction failures from test-
/// body failures.
fn test_fail_provider_construction_fields(
    t: &DiscoveredTest,
    resource: &str,
    message: &str,
) -> String {
    let wrapped = format!(
        "provider for `{}` failed to construct: {}",
        resource, message
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"provider_construction_failed\",\"resource\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string(resource),
        json_string(&wrapped),
    )
}

/// Scaffold a new Kāra project (CR-36). Validates the package name, prepares
/// the target directory (creating `./<name>/` for the positional form), then
/// writes the template files via `scaffold::scaffold_project`. Every failure
/// aborts before any file is written — name validation and target-dir checks
/// run up front so a broken invocation never leaves partial state behind.
fn cmd_init(directory: Option<String>, template: Template, force: bool) {
    let (target_dir, package_name) = match directory {
        Some(name) => {
            if let Err(e) = scaffold::validate_package_name(&name) {
                eprintln!("error[scaffold/{}]: {e}", e.tag());
                process::exit(1);
            }
            let target = PathBuf::from(&name);
            if let Err(e) = scaffold::prepare_new_target_dir(&target) {
                eprintln!("error[scaffold/{}]: {e}", e.tag());
                process::exit(1);
            }
            (target, name)
        }
        None => {
            let cwd = match std::env::current_dir() {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("error: cannot read current directory: {e}");
                    process::exit(1);
                }
            };
            let basename = cwd
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if let Err(e) = scaffold::validate_package_name(&basename) {
                eprintln!("error[scaffold/{}]: {e}", e.tag());
                eprintln!(
                    "  note: deriving package name from the current directory basename `{}`",
                    cwd.display(),
                );
                process::exit(1);
            }
            (cwd, basename)
        }
    };

    let opts = ScaffoldOpts { template, force };
    match scaffold::scaffold_project(&target_dir, &package_name, opts) {
        Ok(()) => {
            let kind = match template {
                Template::Bin => "binary",
                Template::Lib => "library",
                Template::Backend => "backend",
            };
            println!(
                "Scaffolded {kind} project `{package_name}` in {}",
                target_dir.display(),
            );
        }
        Err(e) => {
            eprintln!("error[scaffold/{}]: {e}", e.tag());
            process::exit(1);
        }
    }
}

// ── karac clean ──────────────────────────────────────────────────
//
// Remove a build-artifact cache. Bare form targets the project-local
// `dist/`; `--global` targets the user-wide `~/.kara/cache/` per
// `design.md § Package System > Build artifact cache`. Both forms are
// idempotent — a missing directory is logged and treated as success.

fn cmd_clean(global: bool) {
    let target: PathBuf = if global {
        match dirs_kara_cache_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("error: cannot resolve global cache path: {e}");
                process::exit(1);
            }
        }
    } else {
        let cwd = match std::env::current_dir() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error: cannot read current directory: {e}");
                process::exit(1);
            }
        };
        cwd.join("dist")
    };

    let scope_label = if global {
        "global cache"
    } else {
        "project dist/"
    };
    match fs::metadata(&target) {
        Ok(_) => match fs::remove_dir_all(&target) {
            Ok(()) => {
                println!("removed {} ({})", target.display(), scope_label);
            }
            Err(e) => {
                eprintln!("error: failed to remove {}: {e}", target.display());
                process::exit(1);
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "{} already absent ({}); nothing to do",
                target.display(),
                scope_label
            );
        }
        Err(e) => {
            eprintln!("error: cannot stat {}: {e}", target.display());
            process::exit(1);
        }
    }
}

// ── karac cache ──────────────────────────────────────────────────
//
// Inspect the global build-artifact cache. Two sub-modes: `info`
// prints aggregate stats; `key` derives + prints the cache-key digest
// for a hypothetical five-tuple. The cache root is sourced through
// `build_cache::default_cache_root()` so the `KARAC_BUILD_CACHE_ROOT`
// env override works without any per-call plumbing.

fn cmd_cache(sub: crate::cli::CacheSub, output: OutputMode) {
    let root = match crate::build_cache::default_cache_root() {
        Ok(p) => p,
        Err(e) => {
            emit_cache_error(&e, output);
            process::exit(1);
        }
    };
    match sub {
        crate::cli::CacheSub::Info => cmd_cache_info(&root, output),
        crate::cli::CacheSub::Key {
            pkg,
            version,
            edition,
            profile,
            target_triple,
            compiler_version,
        } => cmd_cache_key(
            &pkg,
            &version,
            edition.as_deref(),
            profile.as_deref(),
            target_triple.as_deref(),
            compiler_version.as_deref(),
            output,
        ),
    }
}

fn cmd_cache_info(root: &std::path::Path, output: OutputMode) {
    let stats = match crate::build_cache::stats(root) {
        Ok(s) => s,
        Err(e) => {
            emit_cache_error(&e, output);
            process::exit(1);
        }
    };
    match output {
        OutputMode::Text => {
            println!("karac cache info:");
            println!("  root:    {}", root.display());
            println!("  entries: {}", stats.entry_count);
            println!("  bytes:   {}", stats.total_bytes);
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"ok\",\"command\":\"cache_info\",\"root\":{},\"entries\":{},\"bytes\":{}}}",
                json_string(&root.display().to_string()),
                stats.entry_count,
                stats.total_bytes,
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "cache_info",
                &format!(
                    "\"root\":{},\"entries\":{},\"bytes\":{}",
                    json_string(&root.display().to_string()),
                    stats.entry_count,
                    stats.total_bytes,
                ),
            );
        }
    }
}

fn cmd_cache_key(
    pkg: &str,
    version: &str,
    edition: Option<&str>,
    profile: Option<&str>,
    target_triple: Option<&str>,
    compiler_version: Option<&str>,
    output: OutputMode,
) {
    let key = crate::build_cache::CacheKey {
        compiler_version: compiler_version
            .unwrap_or_else(|| crate::build_cache::active_compiler_version())
            .to_string(),
        package_name: pkg.to_string(),
        package_version: version.to_string(),
        edition: edition.unwrap_or("2026").to_string(),
        profile: profile.unwrap_or("default").to_string(),
        target_triple: target_triple
            .map(|s| s.to_string())
            .unwrap_or_else(crate::build_cache::host_target_triple),
    };
    let digest = key.digest();
    match output {
        OutputMode::Text => {
            println!("karac cache key:");
            println!("  pkg:              {}", key.package_name);
            println!("  version:          {}", key.package_version);
            println!("  edition:          {}", key.edition);
            println!("  profile:          {}", key.profile);
            println!("  target-triple:    {}", key.target_triple);
            println!("  compiler-version: {}", key.compiler_version);
            println!("  digest:           {digest}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"ok\",\"command\":\"cache_key\",\"pkg\":{},\"version\":{},\"edition\":{},\"profile\":{},\"target_triple\":{},\"compiler_version\":{},\"digest\":{}}}",
                json_string(&key.package_name),
                json_string(&key.package_version),
                json_string(&key.edition),
                json_string(&key.profile),
                json_string(&key.target_triple),
                json_string(&key.compiler_version),
                json_string(&digest),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "cache_key",
                &format!(
                    "\"pkg\":{},\"version\":{},\"edition\":{},\"profile\":{},\"target_triple\":{},\"compiler_version\":{},\"digest\":{}",
                    json_string(&key.package_name),
                    json_string(&key.package_version),
                    json_string(&key.edition),
                    json_string(&key.profile),
                    json_string(&key.target_triple),
                    json_string(&key.compiler_version),
                    json_string(&digest),
                ),
            );
        }
    }
}

fn emit_cache_error(e: &crate::build_cache::CacheError, output: OutputMode) {
    let code = e.code();
    let message = e.to_string();
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {message}");
        }
        OutputMode::Json => {
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"cache\",\"code\":{},\"message\":{}}}]}}",
                json_string(code),
                json_string(&message),
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "cache_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(&message),
                ),
            );
        }
    }
}

// Resolve `~/.kara/cache/`. Honors `$HOME` first (matches the canonical
// behavior on Unix); on Windows-like setups where `$HOME` is unset,
// falls back to `$USERPROFILE`. No external crate dependency because
// the lookup is two env vars; an unset both-of-these case is the rare
// CI image with no home directory and is treated as a hard error.
fn dirs_kara_cache_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "$HOME (and $USERPROFILE) unset".to_string())?;
    Ok(PathBuf::from(home).join(".kara").join("cache"))
}

// ── karac install ────────────────────────────────────────────────
//
// Build a binary package from a `<bin-spec>` and install the resulting
// executable into `~/.kara/bin/`. The spec accepts the same shapes as
// the manifest dependency entry: `path=<path>`, `git=<url>`, a bare
// registry reference `<name>`, or a pinned `<name>@<version>`.
//
// **Path sources are fully wired** as of line 874: the install spec is
// resolved, the build pipeline runs against the resolved directory
// (via a recursive `karac build` invocation so all phases — dep
// resolution, MSRV check, codegen, link — are inherited for free),
// and the produced executable is copied into `<install-root>/<name>`.
//
// **Git / registry sources still surface a forward-compat error.** The
// fetch surface they depend on (tracker line 845) hasn't shipped, so
// there's no source tree to feed the build pipeline. The diagnostic
// names the unsupported source kind and the tracker entry the operator
// should watch.
//
// The install root resolves from `$KARAC_INSTALL_ROOT` first (for tests
// and power-user overrides — empty / whitespace-only values are
// ignored so a stale shell export doesn't silently misroute), then
// falls back to `<HOME>/.kara/bin/`. Same precedence rule the cache
// uses for `KARAC_BUILD_CACHE_ROOT`.

fn cmd_install(spec: &str) {
    use crate::install_spec::{parse_install_spec, InstallSource};

    let source = match parse_install_spec(spec) {
        Ok(src) => src,
        Err(e) => {
            eprintln!("error[{code}]: {e}", code = e.code());
            eprintln!("       received `<bin-spec>` argument: `{spec}`");
            process::exit(1);
        }
    };

    match source {
        InstallSource::Path { path } => install_from_path(&path),
        InstallSource::Git { url } => {
            eprintln!(
                "error[E_INSTALL_GIT_UNSUPPORTED]: git sources are not yet supported by `karac install`"
            );
            eprintln!("       received: git={url}");
            eprintln!(
                "       note: git fetch lands alongside the package-fetch slice (tracker line 845);\n             \
                          once it ships, this install path activates without spec changes."
            );
            process::exit(2);
        }
        InstallSource::Registry { name, version } => {
            let rendered = match &version {
                Some(v) => format!("{name}@{v}"),
                None => name.clone(),
            };
            eprintln!(
                "error[E_INSTALL_REGISTRY_UNSUPPORTED]: registry sources are not yet supported by `karac install`"
            );
            eprintln!("       received: {rendered}");
            eprintln!(
                "       note: registry fetch lands alongside the package-fetch slice (tracker line 845);\n             \
                          once it ships, this install path activates without spec changes."
            );
            process::exit(2);
        }
    }
}

// Resolve the install-binary root. Honors `$KARAC_INSTALL_ROOT` first
// (test + power-user override; whitespace-only values are ignored so
// a stale shell export doesn't silently misroute), then falls back to
// `<HOME>/.kara/bin/`. Mirrors the precedence rule that
// `build_cache::default_cache_root` uses for `KARAC_BUILD_CACHE_ROOT`.
fn install_bin_root() -> Result<PathBuf, String> {
    if let Ok(v) = std::env::var("KARAC_INSTALL_ROOT") {
        if !v.trim().is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "$HOME (and $USERPROFILE) unset".to_string())?;
    Ok(PathBuf::from(home).join(".kara").join("bin"))
}

// Build the project at `path` (via a recursive `karac build` so the
// full pipeline is inherited verbatim — dep resolution, MSRV check,
// codegen, link) and copy the produced executable into the install
// root. On non-zero build exit, the subprocess already streamed its
// own diagnostics; install exits with the same code so CI scripts see
// the underlying failure.
fn install_from_path(path: &std::path::Path) {
    // 1. Canonicalize the path so the subprocess sees a stable cwd
    // even if the operator passed `./tools/my_tool` or a symlink. A
    // missing path surfaces a focused diagnostic — the spec parsed
    // fine, but the filesystem disagreed.
    let canonical = match std::fs::canonicalize(path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!(
                "error[E_INSTALL_PATH_NOT_FOUND]: cannot resolve install source path `{}`: {e}",
                path.display()
            );
            eprintln!(
                "       note: the spec parsed but the filesystem entry doesn't exist or is unreadable."
            );
            process::exit(1);
        }
    };
    if !canonical.is_dir() {
        eprintln!(
            "error[E_INSTALL_PATH_NOT_DIR]: install source `{}` is not a directory",
            canonical.display()
        );
        eprintln!(
            "       note: a path install spec must point at a project root (the directory holding `kara.toml`)."
        );
        process::exit(1);
    }

    // 2. Load the manifest to discover the binary name (the build
    // pipeline writes the executable to `<root>/<mf.name>`; the
    // install copies it to `<install-root>/<mf.name>`). Surfacing
    // manifest errors here — before invoking the build subprocess —
    // gives the operator a focused diagnostic instead of letting the
    // subprocess report the same thing under "build failure".
    let manifest = match manifest::load_from_root(&canonical) {
        Ok(mf) => mf,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };
    let binary_name = manifest.name.clone();

    // 3. Resolve the install root and ensure it exists. The directory
    // is created lazily — a fresh machine never has `~/.kara/bin/`.
    let install_root = match install_bin_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error[E_INSTALL_HOME_UNSET]: cannot resolve install root: {e}");
            process::exit(1);
        }
    };
    if let Err(e) = std::fs::create_dir_all(&install_root) {
        eprintln!(
            "error[E_INSTALL_BIN_DIR_UNWRITABLE]: cannot create install directory `{}`: {e}",
            install_root.display()
        );
        process::exit(1);
    }

    // 4. Invoke the build subprocess. Spawning ourselves with `build`
    // as the verb inherits every pipeline feature (dep resolution,
    // MSRV check, codegen, link) for free — the alternative would
    // require refactoring `cmd_build_project` to accept a root
    // parameter, which is a larger surgery than this slice warrants.
    // Stdio is inherited so build progress reaches the operator
    // directly.
    let karac_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error[E_INSTALL_EXE_UNRESOLVABLE]: cannot locate karac executable: {e}");
            process::exit(1);
        }
    };
    eprintln!(
        "karac install: building `{binary_name}` from `{}`",
        canonical.display()
    );
    let build_status = std::process::Command::new(&karac_exe)
        .arg("build")
        .current_dir(&canonical)
        .status();
    let build_status = match build_status {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error[E_INSTALL_BUILD_SPAWN_FAILED]: cannot spawn build subprocess: {e}");
            process::exit(1);
        }
    };
    if !build_status.success() {
        // The subprocess already streamed its diagnostics; mirror its
        // exit code so CI scripts see the underlying failure rather
        // than a synthetic install code.
        let code = build_status.code().unwrap_or(1);
        eprintln!("error[E_INSTALL_BUILD_FAILED]: build of `{binary_name}` failed (exit {code})");
        process::exit(code);
    }

    // 5. The build wrote the executable to `<root>/<mf.name>`. If it
    // isn't there, the most likely cause is karac was built without
    // the `llvm` feature — the build "succeeds" in that mode but
    // emits a note rather than an executable. Surface that case
    // explicitly so the operator isn't left wondering why a clean
    // build produced nothing to install.
    let built_exe = canonical.join(&binary_name);
    if !built_exe.exists() {
        eprintln!(
            "error[E_INSTALL_NO_EXECUTABLE]: build succeeded but no executable was produced at `{}`",
            built_exe.display()
        );
        eprintln!(
            "       note: karac must be built with `--features llvm` to emit a binary; without llvm\n             \
                          the build only type-checks the project."
        );
        process::exit(1);
    }

    // 6. Copy into the install root. Overwriting is the intended
    // behavior — reinstalling an updated version should replace the
    // existing binary. `std::fs::copy` preserves the executable bit
    // on Unix (it copies the source's mode); on Windows the file is
    // copied byte-for-byte and stays executable by virtue of its
    // extension.
    let dest = install_root.join(&binary_name);
    if let Err(e) = std::fs::copy(&built_exe, &dest) {
        eprintln!(
            "error[E_INSTALL_COPY_FAILED]: cannot copy `{}` → `{}`: {e}",
            built_exe.display(),
            dest.display()
        );
        process::exit(1);
    }

    println!(
        "karac install: installed `{binary_name}` → {}",
        dest.display()
    );
}

// ── karac vendor ─────────────────────────────────────────────────
//
// Copy all resolved dependencies into a `vendor/` directory at the
// project root. Subsequent `karac build --offline` reads from
// `vendor/` and refuses network access. v1 surface — the resolver
// wiring lands alongside the dependency-resolution slice. v1 emits a
// "not yet wired" diagnostic that points operators at the canonical
// flag pairing (`vendor` + `build --offline`) so air-gap CI scripts
// can be scaffolded against the final surface today.

fn cmd_vendor(no_proxy: bool) {
    emit_no_proxy_note(no_proxy);
    let _ = no_proxy;
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, OutputMode::Text);
            process::exit(1);
        }
    };

    let loader = crate::dep_graph::FsLoader;
    let graph = match crate::dep_graph::build_dep_graph(&root, mf, &loader) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, OutputMode::Text, "error");
            process::exit(1);
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    let resolution = match crate::dep_resolver::resolve(&graph, &active) {
        Ok(r) => r,
        Err(boxed) => {
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            let code = boxed.code();
            let severity = match code {
                "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED" => "warning",
                _ => "error",
            };
            emit_dep_diagnostic(&diag, OutputMode::Text, severity);
            if severity == "error" {
                process::exit(1);
            }
            // Warnings (registry/git unsupported until line 845 ships)
            // leave an empty resolution — the vendor copy walks zero
            // path-deps and exits cleanly with the warning above.
            crate::dep_resolver::Resolution {
                packages: std::collections::BTreeMap::new(),
            }
        }
    };

    let vendor_dir = root.join("vendor");
    let mut copied = 0usize;
    let mut skipped_non_path = 0usize;
    for (name, pkg) in &resolution.packages {
        match &pkg.source {
            crate::dep_resolver::ResolvedSource::Path(src_dir) => {
                let dest = vendor_dir.join(name);
                if let Err(e) = copy_dir_recursive(src_dir, &dest) {
                    eprintln!(
                        "error[E_VENDOR_COPY_FAILED]: failed to copy `{name}` into `vendor/`: {e}"
                    );
                    process::exit(1);
                }
                copied += 1;
            }
            crate::dep_resolver::ResolvedSource::Root => {
                // Root is the host project — nothing to vendor.
            }
            crate::dep_resolver::ResolvedSource::Registry { .. }
            | crate::dep_resolver::ResolvedSource::Git { .. } => {
                // Forward-compat: the fetched copy lands in vendor/ once
                // line 845 / git fetch ships. For now we observe and report.
                skipped_non_path += 1;
            }
        }
    }

    if skipped_non_path > 0 {
        eprintln!(
            "note: {skipped_non_path} non-path dependency entr{} skipped — registry/git \
             vendoring lands alongside the fetch surface (tracker line 845).",
            if skipped_non_path == 1 { "y" } else { "ies" }
        );
    }
    eprintln!(
        "karac vendor: copied {copied} package{} into {}",
        if copied == 1 { "" } else { "s" },
        vendor_dir.display()
    );
}

/// Recursive directory copy used by `karac vendor`. Creates `dest` if
/// missing; replaces any existing contents at `dest` to keep vendoring
/// idempotent across reruns (a manifest change at the source surfaces
/// in the next vendor invocation). Errors propagate the offending path.
fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if dest.exists() {
        std::fs::remove_dir_all(dest)?;
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_symlink() {
            // Resolve symlinks so the vendored copy stands alone.
            let target = std::fs::read_link(&from)?;
            let resolved = if target.is_relative() {
                from.parent().unwrap_or(src).join(target)
            } else {
                target
            };
            if resolved.is_dir() {
                copy_dir_recursive(&resolved, &to)?;
            } else {
                std::fs::copy(&resolved, &to)?;
            }
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ── karac update ─────────────────────────────────────────────────
//
// Re-run the resolver against the current manifest and rewrite
// `kara.lock`. v1.1 ships path-deps only — bumping versions isn't
// meaningful for path-deps (they're manifest-pinned), so bare and
// surgical forms re-derive the lockfile identically today. Slice 2
// of line 843 wires the surgical form's positional `<pkg>` validation;
// slice 1 (this code) ships the bare-form behavior.
//
// Tracker: docs/implementation_checklist/phase-5-diagnostics.md line 843.

fn cmd_update(package: Option<&str>, output: OutputMode, no_proxy: bool) {
    emit_no_proxy_note(no_proxy);
    let _ = no_proxy;
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, output);
            process::exit(1);
        }
    };
    // Merge `[target.<triple>].dependencies` for the host-default triple so the
    // refreshed lockfile pins the same per-target deps `karac build` resolves
    // (resolver follow-up (e)).
    let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));

    // Unlike cmd_build_project, we *always* run the resolver here even when
    // the manifest declares no deps. The user explicitly asked to refresh
    // the lockfile — honoring that is the whole point of the subcommand.
    let loader = crate::dep_graph::FsLoader;
    let graph = match crate::dep_graph::build_dep_graph(&root, mf, &loader) {
        Ok(g) => g,
        Err(e) => {
            let diag = crate::dep_diagnostic::render_dep_graph_error(&e);
            emit_dep_diagnostic(&diag, output, "error");
            process::exit(1);
        }
    };
    let active = crate::dep_resolver::active_toolchain_version();
    let resolution = match crate::dep_resolver::resolve(&graph, &active) {
        Ok(r) => r,
        Err(boxed) => {
            let diag = crate::dep_diagnostic::render_resolver_error(&boxed);
            let code = boxed.code();
            let severity = match code {
                "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED" => "warning",
                _ => "error",
            };
            emit_dep_diagnostic(&diag, output, severity);
            if severity == "error" {
                process::exit(1);
            }
            // Warning: still produce an empty-but-valid lockfile via a
            // pseudo-resolution. Practically v1.1 paths trip the
            // path-dep / MSRV branches first; the registry-warn case
            // surfaces here as a no-op-on-update-but-don't-crash.
            crate::dep_resolver::Resolution {
                packages: std::collections::BTreeMap::new(),
            }
        }
    };

    if let Some(pkg) = package {
        if !validate_update_target(pkg, &resolution, output) {
            process::exit(1);
        }
    }

    persist_lockfile(&root, &resolution, output);
    emit_update_summary(&resolution, output);
}

/// Slice 2 of line 843 — surgical `<pkg>` validation. Returns `true` to
/// proceed with the bare-form rewrite, `false` to halt the command.
/// Three outcomes:
/// - `<pkg>` names the root package → hard-error
///   (`E_UPDATE_ROOT_PACKAGE`); the root can't update itself
/// - `<pkg>` not in the resolution → hard-error
///   (`E_UPDATE_UNKNOWN_PACKAGE`); with a fuzzy suggestion when a similar
///   name exists
/// - `<pkg>` names a path-dep (the only non-root v1.1 case) →
///   informational note that path-deps are manifest-pinned, then proceed
fn validate_update_target(
    pkg: &str,
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) -> bool {
    let Some(resolved) = resolution.packages.get(pkg) else {
        let suggestion = nearest_package_name(pkg, resolution);
        emit_update_target_error(
            output,
            "E_UPDATE_UNKNOWN_PACKAGE",
            &format!("unknown package `{pkg}`"),
            suggestion
                .as_deref()
                .map(|s| format!("did you mean `{s}`?"))
                .as_deref(),
        );
        return false;
    };

    if matches!(resolved.source, crate::dep_resolver::ResolvedSource::Root) {
        emit_update_target_error(
            output,
            "E_UPDATE_ROOT_PACKAGE",
            &format!("`{pkg}` is the root package and cannot be the target of `karac update`"),
            Some("omit the positional argument to refresh every locked package"),
        );
        return false;
    }

    if matches!(
        resolved.source,
        crate::dep_resolver::ResolvedSource::Path(_)
    ) {
        if let OutputMode::Text = output {
            eprintln!(
                "note: `{pkg}` is a path-dep; its version is pinned by the on-disk manifest. \
                 `karac update {pkg}` re-derives the lockfile entry but cannot bump versions \
                 until the registry-proxy fetch surface (tracker line 845) ships."
            );
        }
    }

    true
}

fn emit_update_target_error(output: OutputMode, code: &str, message: &str, help: Option<&str>) {
    match output {
        OutputMode::Text => {
            eprintln!("error[{code}]: {message}");
            if let Some(h) = help {
                eprintln!("   = help: {h}");
            }
        }
        OutputMode::Json => {
            let help_field = help
                .map(|h| format!(",\"help\":{}", json_string(h)))
                .unwrap_or_default();
            println!(
                "{{\"status\":\"error\",\"diagnostics\":[{{\"severity\":\"error\",\"phase\":\"update\",\"code\":{},\"message\":{}{}}}]}}",
                json_string(code),
                json_string(message),
                help_field,
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event(
                "update_error",
                &format!(
                    "\"code\":{},\"message\":{}",
                    json_string(code),
                    json_string(message),
                ),
            );
        }
    }
}

fn nearest_package_name(
    target: &str,
    resolution: &crate::dep_resolver::Resolution,
) -> Option<String> {
    let names: Vec<&str> = resolution.packages.keys().map(String::as_str).collect();
    crate::edit_distance::suggest_similar(target, &names)
}

fn emit_update_summary(resolution: &crate::dep_resolver::Resolution, output: OutputMode) {
    let count = resolution.packages.len();
    match output {
        OutputMode::Text => {
            eprintln!(
                "karac update: re-derived kara.lock ({count} locked package{})",
                if count == 1 { "" } else { "s" }
            );
            for (name, pkg) in &resolution.packages {
                let source_kind = describe_resolved_source(&pkg.source);
                eprintln!("  - {name} ({source_kind})");
            }
        }
        OutputMode::Json => {
            let entries: Vec<String> = resolution
                .packages
                .iter()
                .map(|(name, pkg)| {
                    format!(
                        "{{\"name\":{},\"source\":{}}}",
                        json_string(name),
                        json_string(describe_resolved_source(&pkg.source)),
                    )
                })
                .collect();
            println!(
                "{{\"status\":\"ok\",\"command\":\"update\",\"locked\":[{}]}}",
                entries.join(",")
            );
        }
        OutputMode::Jsonl => {
            emit_jsonl_event("update_complete", &format!("\"locked_count\":{count}"));
        }
    }
}

fn describe_resolved_source(src: &crate::dep_resolver::ResolvedSource) -> &'static str {
    match src {
        crate::dep_resolver::ResolvedSource::Root => "root",
        crate::dep_resolver::ResolvedSource::Path(_) => "path",
        crate::dep_resolver::ResolvedSource::Registry { .. } => "registry",
        crate::dep_resolver::ResolvedSource::Git { .. } => "git",
    }
}

/// Richer, human-facing rendering of a resolved source for `karac resolve`'s
/// text view — the source kind plus its locating detail (path / URL / short
/// commit). The machine views (`--output=json|jsonl`) use the bare kind from
/// [`describe_resolved_source`] instead, keying the detail off dedicated
/// fields.
fn describe_resolved_source_detail(src: &crate::dep_resolver::ResolvedSource) -> String {
    match src {
        crate::dep_resolver::ResolvedSource::Root => "root".to_string(),
        crate::dep_resolver::ResolvedSource::Path(dir) => format!("path {}", dir.display()),
        crate::dep_resolver::ResolvedSource::Registry { url, .. } => format!("registry {url}"),
        crate::dep_resolver::ResolvedSource::Git {
            url, resolved_rev, ..
        } => {
            if resolved_rev.is_empty() {
                format!("git {url}")
            } else {
                let short = &resolved_rev[..resolved_rev.len().min(12)];
                format!("git {url}@{short}")
            }
        }
    }
}

/// `karac resolve` — read-only dependency-graph inspection (registry-proxy
/// follow-up (j) at `phase-5-diagnostics.md` line 896). Runs the same
/// resolver + fetch path `karac build` would, then prints the resolved graph
/// *without* rewriting `kara.lock` (unlike `karac update`).
fn cmd_resolve(output: OutputMode, offline: bool, no_proxy: bool) {
    emit_no_proxy_note(no_proxy);

    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };
    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            emit_manifest_error(&e, output);
            process::exit(1);
        }
    };
    // Consume `[target.<triple>].dependencies` for the host-default triple so
    // `karac resolve` prints the same graph `karac build` would (resolver
    // follow-up (e)). Applied before the `has_deps` gate below.
    let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));

    // Mirror the build path's `--offline` handling: resolve against `./vendor/`,
    // and a project that has deps but no vendor dir is a hard error rather than
    // a silent empty resolution.
    let has_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let vendor_root_buf = root.join("vendor");
    if offline && has_deps && !vendor_root_buf.is_dir() {
        emit_offline_no_vendor_dir(&vendor_root_buf, output);
        process::exit(1);
    }
    let offline_root: Option<&std::path::Path> = if offline {
        Some(vendor_root_buf.as_path())
    } else {
        None
    };

    // `persist_lock = false` — this command inspects, it does not pin. Fetch
    // still activates (registry / git deps resolve to real sources) so the
    // printed graph is exactly what a build would see.
    let resolution =
        match run_dep_resolution(&root, mf, output, offline_root, false, no_proxy, false) {
            Ok(Some(r)) => r,
            // Warn-and-continue path (unsupported registry/git source with no
            // fetch configured): the diagnostic already surfaced. Show an empty
            // graph so the command still exits cleanly with a valid envelope.
            Ok(None) => crate::dep_resolver::Resolution {
                packages: std::collections::BTreeMap::new(),
            },
            Err(()) => process::exit(1),
        };

    emit_resolution_graph(&resolution, output);
}

/// Render a resolved dependency graph for `karac resolve` in the requested
/// output mode. Each package carries its pinned version, source, and the
/// `declared_by` edges (which parent required it, with what constraint).
fn emit_resolution_graph(resolution: &crate::dep_resolver::Resolution, output: OutputMode) {
    let count = resolution.packages.len();
    match output {
        OutputMode::Text => {
            eprintln!(
                "karac resolve: {count} package{}",
                if count == 1 { "" } else { "s" }
            );
            for (name, pkg) in &resolution.packages {
                eprintln!(
                    "  {name} {} ({})",
                    pkg.version,
                    describe_resolved_source_detail(&pkg.source)
                );
                for edge in &pkg.declared_by {
                    let req = edge
                        .req
                        .as_ref()
                        .map(|r| r.to_string())
                        .unwrap_or_else(|| "*".to_string());
                    eprintln!("    <- {} ({req})", edge.parent);
                }
            }
        }
        OutputMode::Json => {
            let entries: Vec<String> = resolution
                .packages
                .iter()
                .map(|(name, pkg)| {
                    let edges: Vec<String> = pkg
                        .declared_by
                        .iter()
                        .map(|e| {
                            format!(
                                "{{\"parent\":{},\"req\":{}}}",
                                json_string(&e.parent),
                                match &e.req {
                                    Some(r) => json_string(&r.to_string()),
                                    None => "null".to_string(),
                                }
                            )
                        })
                        .collect();
                    format!(
                        "{{\"name\":{},\"version\":{},\"source\":{},\"declared_by\":[{}]}}",
                        json_string(name),
                        json_string(&pkg.version.to_string()),
                        json_string(describe_resolved_source(&pkg.source)),
                        edges.join(",")
                    )
                })
                .collect();
            println!(
                "{{\"status\":\"ok\",\"command\":\"resolve\",\"packages\":[{}]}}",
                entries.join(",")
            );
        }
        OutputMode::Jsonl => {
            for (name, pkg) in &resolution.packages {
                emit_jsonl_event(
                    "resolve_package",
                    &format!(
                        "\"name\":{},\"version\":{},\"source\":{}",
                        json_string(name),
                        json_string(&pkg.version.to_string()),
                        json_string(describe_resolved_source(&pkg.source)),
                    ),
                );
            }
            emit_jsonl_event("resolve_complete", &format!("\"package_count\":{count}"));
        }
    }
}

#[cfg(test)]
mod diagnostic_json_tests {
    //! Direct-construction tests for the `DiagnosticJson` JSON
    //! emitter. The CLI integration tests in `tests/cli.rs`
    //! exercise the same emitter via real fixtures; these unit tests
    //! pin the *shape* against a synthetic `DiagEntry` so the
    //! field-by-field wire format is testable without standing up a
    //! full pipeline.
    use super::{DiagEntry, DiagnosticJson};
    use crate::token::Span;
    use crate::typechecker::FixIt;

    fn synth_span() -> Span {
        Span {
            line: 1,
            column: 5,
            offset: 4,
            length: 0,
        }
    }

    #[test]
    fn fix_it_emits_both_legacy_field_and_fixes_array() {
        // Line 619 slice 5 pin — a DiagEntry carrying a FixIt
        // produces both the legacy `fix_it` object (single-edit
        // form, kept for backward compat) and the new `fixes` array
        // (the spec's preferred shape per `docs/deferred.md` §
        // Structured Diagnostics). Both wire from the same FixIt
        // data; the legacy form has no `description` field, the
        // array form does.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        let fix = FixIt {
            span: span.clone(),
            replacement: ", ..".to_string(),
        };
        diags.add(DiagEntry {
            id: "d1",
            severity: "error",
            phase: "typecheck",
            code: "E_NON_EXHAUSTIVE_CROSS_PACKAGE_PATTERN",
            category: "typecheck",
            message: "test message",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: None,
            fix_it: Some(&fix),
            class: Some("OTHER"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        // Legacy field still present.
        assert!(
            json.contains("\"fix_it\":"),
            "expected legacy fix_it field; got: {json}"
        );
        // New array form.
        assert!(
            json.contains("\"fixes\":["),
            "expected fixes array; got: {json}"
        );
        // Array entry carries description + edits.
        assert!(json.contains("\"description\":"));
        assert!(json.contains("\"edits\":[{"));
        // Edits entry carries span + replacement.
        assert!(json.contains("\"replacement\":\", ..\""));
        // No fix-it on plain diagnostics — confirm the field is
        // omitted when fix_it: None.
    }

    #[test]
    fn no_fix_it_omits_both_fix_fields() {
        // When `fix_it: None`, neither the legacy `fix_it` field nor
        // the new `fixes` array should appear in the JSON — keeps
        // the lean shape that consumers expect for diagnostics
        // without machine-applicable patches.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        diags.add(DiagEntry {
            id: "d1",
            severity: "error",
            phase: "typecheck",
            code: "E_TYPE_MISMATCH",
            category: "typecheck",
            message: "test",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: None,
            fix_it: None,
            class: Some("TYPE_MISMATCH"),
            expected: Some("i32"),
            got: Some("String"),
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        assert!(!json.contains("\"fix_it\":"));
        assert!(!json.contains("\"fixes\":"));
        // Typed fields are still present.
        assert!(json.contains("\"class\":\"TYPE_MISMATCH\""));
        assert!(json.contains("\"expected\":\"i32\""));
        assert!(json.contains("\"got\":\"String\""));
    }

    #[test]
    fn fixes_array_description_falls_back_to_lint_name() {
        // When the diagnostic carries a `lint_name`, the fix's
        // description uses it instead of the generic "apply
        // suggested edit". Gives LLM/IDE consumers a recognisable
        // anchor for which rule the fix derives from.
        let mut diags = DiagnosticJson::new();
        let span = synth_span();
        let fix = FixIt {
            span: span.clone(),
            replacement: "_".to_string(),
        };
        diags.add(DiagEntry {
            id: "d1",
            severity: "warning",
            phase: "typecheck",
            code: "W0246",
            category: "typecheck",
            message: "test",
            filename: "test.kara",
            span: &span,
            suggestion: None,
            extra_json: None,
            lint_name: Some("missing_non_exhaustive"),
            fix_it: Some(&fix),
            class: Some("LINT_WARNING"),
            expected: None,
            got: None,
            stub_hint_json: None,
        });
        let json = diags.to_json_array();
        assert!(
            json.contains("\"description\":\"missing_non_exhaustive\""),
            "fix description should adopt lint_name; got: {json}"
        );
    }
}
