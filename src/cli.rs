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
mod build_cmds;
mod diag_json;
pub mod explain;
mod fix_cmds;
mod help;
mod maintenance_cmds;
mod pkg_cmds;
mod query_cmd;
mod run_check_cmds;
mod test_cmd;

use build_cmds::*;
pub use diag_json::*;
use fix_cmds::*;
use maintenance_cmds::*;
use pkg_cmds::*;
use query_cmd::*;
use run_check_cmds::*;
use test_cmd::*;

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
        /// Arguments after a literal `--`, forwarded to the PROGRAM as its own
        /// argv tail. `env.args()` then yields `[<script>, ...program_args]` on
        /// every executor. Without this, a program's `env.args()` saw whatever
        /// process happened to host it — karac's own argv under `--interp`, and
        /// the internal `karac_jit_runner` plus a temp `.ll` path under the JIT
        /// (B-2026-07-29-18).
        program_args: Vec<String>,
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
        /// `-A` / `-W` / `-D` lint levels — see `Build.lint_overrides`.
        /// This variant did not carry them (B-2026-08-18-19), so a project
        /// build silently ignored every lint flag the invocation named.
        lint_overrides: crate::lints::CliLintOverrides,
    },
    Query {
        kind: QueryKind,
        file: String,
        function: String,
    },
    Fmt {
        file: String,
    },
    /// Render a `std.panic` crash report (`docs/design.md § 4. Crash Report
    /// Format`). `input` is a JSON file path or `-` for stdin; the default
    /// output is the human-readable form, `--output=json` re-emits the parsed
    /// structured JSON (pretty-printed). Track 6 § CLI surface.
    Debug {
        input: String,
        output: OutputMode,
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
/// reachable from the CLI.
///
/// [`Code`](ExplainTarget::Code) closes the loop the structured
/// diagnostics opened: every JSON diagnostic carries a `code` field
/// (`"E0200"`), so `explain` has to accept that same token or the
/// machine-readable surface dead-ends at the one command meant to
/// interpret it. It resolves through the code→class table in
/// `cli::explain`, which is deliberately *not* a per-code prose
/// catalogue — that remains the deferred surface this doc comment
/// originally pointed at.
#[derive(Debug, Clone)]
pub enum ExplainTarget {
    /// `--concept=NAME` — concept-page surface (closures, …).
    Concept(String),
    /// `--class=NAME` — diagnostic-class catalogue lookup. NAME is
    /// the UPPER_SNAKE wire form (`TYPE_MISMATCH`, `INVALID_CAST`,
    /// etc.). Slice 1 minted the enum; slice 3 surfaces it via the
    /// CLI.
    Class(String),
    /// `--code=NAME` — the `E0NNN` / `W0NNN` token a structured
    /// diagnostic reports in its `code` field.
    Code(String),
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
        Command::Version => println!("karac {}", crate::karac_version_string()),
        Command::Run {
            file,
            output,
            sequential,
            manifest_override,
            no_manifest,
            lint_overrides,
            timeout,
            interp,
            program_args,
        } => cmd_run(
            &file,
            output,
            sequential,
            manifest_override.as_deref(),
            no_manifest,
            lint_overrides,
            timeout,
            interp,
            &program_args,
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
            lint_overrides,
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
            lint_overrides,
        ),
        Command::Query {
            kind,
            file,
            function,
        } => cmd_query(kind, &file, &function),
        Command::Fmt { file } => cmd_fmt(&file),
        Command::Debug { input, output } => cmd_debug(&input, output),
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
    /// Items `target::filter_inactive_items` stripped because their
    /// `#[target(...)]` spec excludes the target of THIS pass — name →
    /// rendered spec. Kept so a single-target check can say so
    /// (B-2026-08-05-29): a stripped body is never parsed into any pass, so
    /// `check` reports "All checks passed" over source it did not examine,
    /// and the author has no way to tell that from source it did. The
    /// resolver gets the same map for its reference-site "not available on
    /// target X" diagnostic; this copy exists because that one only fires
    /// when something CALLS the gated item.
    target_skipped: std::collections::HashMap<String, String>,
    /// The exact text `parse` consumed, when there was one. Span-driven lints
    /// need the author's own spelling, and `filename` is NOT always a readable
    /// path — project mode labels its hand-built super-program with the
    /// package NAME and has no single source text at all. Re-reading
    /// `filename` from disk instead is what broke every project build
    /// (B-2026-08-04-7). `None` means "no single source": source-text lints
    /// skip rather than slice into a stand-in string.
    source: Option<String>,
    parsed: ParseResult,
    /// B-2026-08-13-12 — will this program's checks be followed by CODEGEN?
    ///
    /// `false` only for `karac run --interp`, whose backend is the tree-walk
    /// interpreter. The `chained_field_receiver` lint reports a codegen
    /// deferral, and the interpreter accepts that shape — erroring there would
    /// take away a working execution path to warn about one that was never
    /// used. Every other entry point (check, build, `run` on the JIT) is
    /// codegen-bound and keeps the gate.
    codegen_bound: bool,
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
            target_skipped: std::collections::HashMap::new(),
            source: Some(source.to_string()),
            parsed,
            codegen_bound: true,
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

    /// Mark this run as interpreter-bound (`karac run --interp`), so
    /// codegen-deferral diagnostics stay quiet — see `codegen_bound`.
    fn interpreter_bound(mut self) -> Self {
        self.codegen_bound = false;
        self
    }

    fn has_parse_errors(&self) -> bool {
        !self.parsed.errors.is_empty()
    }

    fn resolve(&mut self) {
        if self.has_parse_errors() {
            return;
        }
        // The three pre-resolve AST rewrites — gated-stdlib splice,
        // `#[target(...)]` stripping, desugar (which also runs the
        // `#[proto_schema]` expansion) — live in one library function so no
        // other driver can run a subset of them. See `lib.rs`
        // `prepare_for_resolve` for what each does and what skipping it costs;
        // the test harnesses go through the same call (B-2026-08-11-34).
        let (target_tombstones, schema_diags) =
            crate::prepare_for_resolve(&mut self.parsed.program);
        // Keep a copy for the check reporter (B-2026-08-05-29). The resolver
        // consumes the map for reference-site diagnostics, which only fire
        // when something CALLS a stripped item; nothing otherwise records
        // that a body went unexamined.
        self.target_skipped = target_tombstones.clone();
        // The `#[proto_schema]` diagnostics (malformed `.proto`, unsupported
        // field types) join the comptime-error channel so they render and gate
        // exactly like the post-resolve fold pass's.
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
        // Checking one file that lives inside a package's `src/` still sees
        // only that file, so its `pub` items may have readers off-screen.
        // Rename fix-its consult this before offering to rewrite a public
        // name (B-2026-07-31-33) — the same blind spot that makes a
        // single-file `karac build` on a package member a refusal.
        let in_package = is_package_member(&self.filename);
        self.resolved = Some(
            crate::resolver::Resolver::new(&self.parsed.program)
                .with_test_file(is_test_file)
                .with_external_pub_refs(in_package)
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
        // B-2026-08-03-9 — `map_value_clone_reinsert`. Emitted into
        // `typed.warnings` rather than through a bespoke lint channel so it
        // rides plumbing that already exists: the JSON collector renders
        // `warnings` (with `lint_name` and `fix_it`), and `cmd_fix` applies
        // their fix-its. The lint needs the original source text, since both
        // its same-expression checks and its rewrite reproduce the author's
        // own spelling by span.
        //
        // Read from the text `new` was handed, NOT from `filename` on disk
        // (B-2026-08-04-7): project mode labels its super-program with the
        // package NAME, so re-reading hit `read_source`'s `process::exit(1)`
        // and every `karac build` in a project died with
        // `error: cannot read '<package>'` before codegen. Project mode has no
        // single source text, so the lint sits out there rather than reading
        // spans against a stand-in — project-wide coverage needs the
        // per-module texts and is a follow-up.
        if let (Some(source), Some(typed)) = (self.source.clone(), self.typed.as_mut()) {
            let extra = crate::map_entry_lint::check_map_value_clone_reinsert(
                &self.parsed.program,
                typed,
                &source,
                &self.lint_overrides,
            );
            typed.warnings.extend(extra);
        }
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
        // B-2026-08-13-12 — surface codegen's FR4 chained-field-receiver
        // deferral at CHECK time. The shape is syntactic, so this needs
        // neither the source text nor typecheck output and is not sidelined
        // in project mode — but it MUST run post-lower (B-2026-08-17-21):
        // the predicate mirrors `lower_field_access_ptr`, which codegen
        // reaches on the LOWERED AST, and lowering synthesizes chained field
        // receivers of its own. `ORIGIN.inner.v.to_string()` parses as a
        // 4-segment `Call(Path)` and only becomes a chained `FieldAccess`
        // receiver once `rewrite_path_call_to_method_call` runs, so a
        // pre-lower placement under-fired on exactly the shape that widening
        // introduced: check clean, build refused.
        if let (true, Some(typed)) = (self.codegen_bound, self.typed.as_mut()) {
            let (extra, deny) = crate::chained_receiver_lint::check_chained_field_receivers(
                &self.parsed.program,
                &self.lint_overrides,
            );
            if deny {
                typed.errors.extend(extra);
            } else {
                typed.warnings.extend(extra);
            }
        }
        // B-2026-08-16-13 — surface codegen's E_ESCAPING_CLOSURE_NOT_YET
        // deferral at CHECK time by running codegen's OWN escape analysis
        // (`crate::closure_escape` — one predicate shared with the build gate,
        // zero drift). Runs HERE, after operator lowering + comptime folding,
        // not in `typecheck()` with the chained-receiver lint: the escape
        // analysis is not lowering-invariant (measured: a bare `[make(..)]`
        // literal is an `ArrayLiteral` pre-lower — array-owner sanctioned —
        // but lowers to a Vec `PrefixCollectionLiteral`, whose element store
        // the guard rejects), so parity with `karac build` requires the SAME
        // post-lower AST codegen compiles. `codegen_bound` gates it exactly
        // like the chained lint: the interpreter supports these shapes, so an
        // interp-bound pipeline stays quiet.
        if let (true, Some(typed)) = (self.codegen_bound, self.typed.as_mut()) {
            let (extra, deny) = crate::escaping_closure_lint::check_escaping_closures(
                &self.parsed.program,
                &self.lint_overrides,
            );
            if deny {
                typed.errors.extend(extra);
            } else {
                typed.warnings.extend(extra);
            }
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
        // `KARAC_NO_AUTOPAR=1` escape hatch: skip the concurrency analysis so
        // codegen never receives auto-parallel groups and every loop lowers
        // sequentially. Leaves `concurrency = None`, the same state
        // `compile_to_ir(_, None, _)` uses. Purpose: isolate SEQUENTIAL codegen
        // density from auto-par dispatch overhead when measuring (B-2026-07-10-5
        // density effort), and a workaround if an auto-par decision ever
        // regresses a throughput-bound loop.
        if std::env::var("KARAC_NO_AUTOPAR").as_deref() == Ok("1") {
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
            || self.has_fatal_effect_errors()
            || self.has_fatal_ownership_errors()
    }

    /// Whether any EFFECT diagnostic stops a build.
    ///
    /// B-2026-08-05-17: this arm did not exist, so `karac build` ran the effect
    /// checker and then ignored every finding — a program `karac check`
    /// rejected with `1 error(s) found` produced a binary and exited 0. Type
    /// errors were gated on both paths, so the effect system specifically was
    /// unenforced in the shipping artifact: a user who only ever runs `build`
    /// got no verification of the declared-vs-inferred contract that public
    /// signatures exist to guarantee.
    ///
    /// This is the same defect B-2026-07-31-29 fixed one phase over for
    /// ownership, and the shape of the fix is deliberately identical: one
    /// classifier, shared by every gate, so the lanes cannot drift apart again.
    fn has_fatal_effect_errors(&self) -> bool {
        self.effects.as_ref().is_some_and(|e| {
            e.errors
                .iter()
                .any(|err| Self::is_fatal_effect_kind(&err.kind))
        })
    }

    /// Which effect-diagnostic kinds are fatal. Two stay advisory by design,
    /// and both are already treated that way by `cmd_run`'s gate — this is that
    /// predicate hoisted so `build` shares it verbatim:
    ///
    ///   * `FfiLintHint` — declared "never a compile error" at its definition;
    ///     rendered as `note[effect]`.
    ///   * `TargetGateViolation` (E0411) — a target-AVAILABILITY finding, not a
    ///     correctness bug, and it already has its own target-aware abort in the
    ///     build path (see `wasm_wasi_build_aborts_on_target_gate_violation`,
    ///     which asserts a wasm build stops with the targeted E0411 diagnostic).
    ///     Routing it through this generic gate as well would make a NATIVE
    ///     build reject the deliberate cross-target dev workflow that
    ///     `karac run` supports by design.
    ///
    /// NOT aligned here, deliberately: on the native target `check` still counts
    /// `TargetGateViolation` toward its exit code (`total_errors` filters only
    /// `FfiLintHint`) while `run` reports it as `warning[effect]` and exits 0.
    /// That check-vs-run split predates this fix and changing `check`'s exit
    /// code is a separate decision — `check` being the strictest lane is the
    /// safe direction for the Mend loop's gate. Recorded on B-2026-08-05-17.
    fn is_fatal_effect_kind(kind: &EffectErrorKind) -> bool {
        !matches!(
            kind,
            EffectErrorKind::FfiLintHint | EffectErrorKind::TargetGateViolation
        )
    }

    /// Comptime fold failures are fatal: a `comptime { ... }` block that
    /// panicked, exceeded its resource ceiling, or produced a non-foldable
    /// value has no constant to splice, so the interpreter / codegen would
    /// otherwise consume an un-evaluated node (or a stale tree) and produce
    /// misleading downstream diagnostics. Stop before execution.
    fn has_fatal_comptime_errors(&self) -> bool {
        self.comptime_errors.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Which ownership diagnostics stop a build. B-2026-07-31-29: this used to
    /// promote ONLY `ExclusiveBorrowAliasedArgs`, leaving every other kind
    /// advisory at the CLI layer — so `karac check` reported `error[ownership]`
    /// and exited 1 while `karac build` produced a binary and exited 0 for the
    /// same program. The two lanes disagreed about whether a program was valid,
    /// which matters because `check` is the Mend loop's gate and the AI-first
    /// surface's contract.
    ///
    /// The rule is now the inverse: an ownership error is fatal UNLESS it is one
    /// of the two documented-advisory kinds, so `check` and `build` agree by
    /// construction and a newly added kind is fatal by default rather than
    /// silently advisory.
    ///
    /// Fatal, and why the previously-advisory ones had to be promoted:
    ///   * `ExclusiveBorrowAliasedArgs` — the original soundness gate
    ///     (B-2026-06-17-6): an aliased `mut ref` / `mut Slice` argument
    ///     (`f(mut v, mut v)`) miscompiles, because codegen passes the borrow's
    ///     value by copy per argument and assumes the two don't alias.
    ///   * `ReassignToImmutable` / `MutateImmutableBinding` — design.md § Variable
    ///     Binding Rules states outright that reassigning a `let` binding or
    ///     calling a `mut ref self` method on it "is a compile error". It was not
    ///     one: `let x = 1; x = 2;` built cleanly AND THE MUTATION TOOK EFFECT
    ///     (the binary printed 2), so `let` versus `let mut` meant nothing in
    ///     compiled code. The module-level form of the same rule is a
    ///     *typechecker* error (`ReassignToImmutableModuleBinding`, E0252) and so
    ///     was already fatal — the local form is now enforced to match.
    ///   * `UseOfUninitialized`, `NoRcViolation`, `CaptureModeViolation`,
    ///     `OwnershipCycle` — each is a real violation, not a hint, and none has
    ///     a defensive-copy story that makes the emitted code correct anyway.
    ///
    /// Advisory (reported, but neither `check` nor `build` fails):
    ///   * `RcFallbackNote` — declared "Performance note … Not blocking" at its
    ///     definition; the RC it reports is inserted and correct.
    ///   * `UseAfterMove` — codegen defensive-copies the reuse, so the binary is
    ///     memory-safe; the diagnostic carries a machine-applicable `.clone()`
    ///     fix precisely because the program compiles and runs. Keeping it
    ///     non-fatal for `build` is deliberate; it is excluded from
    ///     `total_errors` for the same reason, so `check` no longer fails on a
    ///     program the compiler is documented to accept.
    fn has_fatal_ownership_errors(&self) -> bool {
        self.ownership.as_ref().is_some_and(|o| {
            o.errors
                .iter()
                .any(|e| Self::is_fatal_ownership_kind(&e.kind))
        })
    }

    /// The single classification consulted by both `has_fatal_ownership_errors`
    /// (the build gate) and `total_errors` (the `check` exit code), so the two
    /// cannot drift apart again.
    fn is_fatal_ownership_kind(kind: &crate::ownership::OwnershipErrorKind) -> bool {
        use crate::ownership::OwnershipErrorKind as K;
        !matches!(kind, K::RcFallbackNote | K::UseAfterMove)
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
            // B-2026-07-31-29: count only the kinds that also stop a build, so
            // `karac check`'s exit code and `karac build`'s gate agree. Counting
            // every kind here is what made `check` exit 1 on a `UseAfterMove`
            // that `build` compiles by design. The advisory kinds are still
            // RENDERED — they stay in the diagnostic stream and in
            // `--output=json` — they just no longer decide the exit code.
            n += o
                .errors
                .iter()
                .filter(|e| Self::is_fatal_ownership_kind(&e.kind))
                .count();
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

/// Render the source line a diagnostic points at, with a caret run under the
/// spanned text.
///
/// Every phase already carries a precise `(line, column, length)` span and the
/// JSON renderer already exposes the structured fields, but the human renderer
/// printed only `file:line:col: message` — so a reader got a coordinate and had
/// to go find the code themselves, on a compiler whose pitch is diagnostic
/// quality. This closes that asymmetry at the rendering layer; no diagnostic
/// data changes.
///
/// Returns `None` — and the caller then prints the header alone, exactly as
/// before — when there is no single source text (project mode builds a
/// synthetic super-program and sets `source: None`, B-2026-08-04-7), or when
/// the span does not land in it. A diagnostic is still strictly better than a
/// panic here, so every failure path degrades to today's output.
fn diagnostic_snippet(
    source: Option<&str>,
    line: usize,
    column: usize,
    length: usize,
) -> Option<String> {
    let src = source?;
    if line == 0 || column == 0 {
        return None;
    }
    let line_text = src.lines().nth(line - 1)?;
    let gutter = line.to_string();
    let pad = " ".repeat(gutter.len());
    // Build the caret's left padding from the line's OWN leading characters,
    // keeping tabs as tabs: a space-per-tab would misalign the caret under any
    // tab width but 1, and tab-indented Kāra is legal.
    let prefix: String = line_text
        .chars()
        .take(column - 1)
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    // Clamp to this line: a span covering several lines underlines the first
    // one to its end rather than running past it. `max(1)` keeps a zero-length
    // span (an insertion point) visible as a single caret.
    let width = line_text
        .chars()
        .skip(column - 1)
        .take(length.max(1))
        .count()
        .max(1);
    Some(format!(
        "\n{pad} |\n{gutter} | {line_text}\n{pad} | {prefix}{}",
        "^".repeat(width)
    ))
}

/// `header` plus its source snippet, when one can be rendered.
fn with_snippet(
    header: String,
    source: Option<&str>,
    line: usize,
    column: usize,
    length: usize,
) -> String {
    match diagnostic_snippet(source, line, column, length) {
        Some(snip) => header + &snip,
        None => header,
    }
}

/// Render the text-mode diagnostic stream as one string block per
/// diagnostic (multi-line for diagnostics that carry notes/help).
/// Factored out of `print_text_diagnostics` for the multi-target check
/// driver, which compares rendered blocks across per-target pipeline
/// runs to deduplicate target-agnostic findings (`cmd_check_targets`).
/// Render one lint diagnostic as a text block, for the lints wired onto the
/// compile path by B-2026-08-18-2. Factored so the three call sites cannot
/// drift in severity word or span formatting; `must_use` keeps its own inline
/// block because it also carries `help` / `note` continuation lines.
fn render_lint_block_text(
    lint_name: &str,
    is_error: bool,
    message: &str,
    filename: &str,
    span: &crate::token::Span,
    source: Option<&str>,
) -> String {
    let severity = if is_error { "error" } else { "warning" };
    with_snippet(
        format!(
            "{severity}[{lint_name}]: {filename}:{}:{}: {message}",
            span.line, span.column
        ),
        source,
        span.line,
        span.column,
        span.length,
    )
}

/// The WARNING-severity subset of [`render_text_diagnostics`], for the build
/// path (B-2026-08-18-1).
///
/// `karac build` rendered no warning at all on a successful build: it calls
/// `print_text_diagnostics` only when `has_fatal_errors()`, so on the path that
/// actually produces a binary the whole diagnostic stream was dropped. A
/// build-only workflow therefore never saw `deprecated`, `must_use`, or any
/// other lint the compiler had already computed — measured on `#[deprecated]`,
/// which `karac check` reports and `karac build` did not.
///
/// Filtered out of the SHARED renderer rather than re-derived, so `check` and
/// `build` cannot drift in wording, span, or lint label — the same reason
/// B-2026-08-05-17 fixed the effect-gate version of this class with one shared
/// classifier. The filter reads the severity word each block already opens
/// with, which is safe because every block in that function is built with its
/// severity first (`error[`, `warning[`, `note[`, or the `{severity}[`
/// interpolation the lint helpers use); `build_warnings_are_the_warning_blocks`
/// pins that invariant.
///
/// `note[…]` is deliberately NOT included. Those are advisory hints (FFI lint
/// notes, the wasm-tools note) rather than findings about the program's
/// correctness, and printing them on every build is noise this row did not ask
/// for. Extending to them is a separate decision.
/// The JSON twin of [`render_text_warning_diagnostics`]: the warning-severity
/// entries of the same `collect_diagnostics` the check path emits
/// (B-2026-08-18-1).
///
/// Filtered out of the shared collector for the same reason the text version
/// is — one producer, so a warning cannot say one thing under `karac check
/// --output=json` and another under `karac build --output=json`.
fn collect_warning_diagnostics_json(pipeline: &Pipeline) -> Vec<String> {
    diag_json::collect_diagnostics(pipeline)
        .entries
        .into_iter()
        .filter(|entry| entry.contains("\"severity\":\"warning\""))
        .collect()
}

fn render_text_warning_diagnostics(pipeline: &Pipeline) -> Vec<String> {
    render_text_diagnostics(pipeline)
        .into_iter()
        .filter(|block| block.starts_with("warning["))
        .collect()
}

/// The rendered blocks for `TypeCheckResult::warnings` — the channel every
/// `type_lint_warning` lint rides (`deprecated`, `unstable_api`, …) plus the
/// CLI-attached `map_value_clone_reinsert`.
///
/// Extracted so `karac run` can render the SAME blocks (B-2026-08-18-20). It
/// had its own lint block for `must_use` and nothing at all for this channel,
/// so `karac run` and `karac run --interp` were both silent on `#[deprecated]`
/// while `check` reported it — an inconsistency inside one lane, since the two
/// lints differ only in which channel they ride. `run` cannot simply call
/// `render_text_diagnostics`: that also renders `must_use`, which `run` already
/// prints through its own block with its own continuation lines, so it would
/// double-print.
///
/// The bracket names the LINT when there is one — that is what `-A <name>`
/// takes, so it is the actionable label — and falls back to the phase, matching
/// the `error[typecheck]` convention.
pub(crate) fn render_typecheck_warning_blocks(
    typed: &crate::typechecker::TypeCheckResult,
    filename: &str,
    source: Option<&str>,
) -> Vec<String> {
    typed
        .warnings
        .iter()
        .map(|warn| {
            let label = warn.lint_name.as_deref().unwrap_or("typecheck");
            with_snippet(
                format!(
                    "warning[{label}]: {}:{}:{}: {}",
                    filename, warn.span.line, warn.span.column, warn.message
                ),
                source,
                warn.span.line,
                warn.span.column,
                warn.span.length,
            )
        })
        .collect()
}

fn render_text_diagnostics(pipeline: &Pipeline) -> Vec<String> {
    let filename = &pipeline.filename;
    let source = pipeline.source.as_deref();
    let mut out: Vec<String> = Vec::new();
    for err in &pipeline.parsed.errors {
        out.push(with_snippet(
            format!(
                "error[parse]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            ),
            source,
            err.span.line,
            err.span.column,
            err.span.length,
        ));
    }
    if let Some(ref r) = pipeline.resolved {
        for err in &r.errors {
            out.push(with_snippet(
                format!(
                    "error[resolve]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                ),
                source,
                err.span.line,
                err.span.column,
                err.span.length,
            ));
        }
    }
    if let Some(ref t) = pipeline.typed {
        for err in &t.errors {
            out.push(with_snippet(
                format!(
                    "error[typecheck]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                ),
                source,
                err.span.line,
                err.span.column,
                err.span.length,
            ));
        }
        // `TypeCheckResult::warnings` — the channel every `type_lint_warning`
        // lint rides (`deprecated`, `unstable_api`, …) plus the CLI-attached
        // `map_value_clone_reinsert`. The JSON emitter has always rendered it;
        // the text renderer dropped it entirely, so `karac check` printed
        // "All checks passed." while `--output=json` carried a warning for the
        // same program. The bracket names the LINT when there is one — that is
        // what `-A <name>` takes, so it is the actionable label — and falls
        // back to the phase, matching the `error[typecheck]` convention above.
        out.extend(render_typecheck_warning_blocks(t, filename, source));
    }
    // B-2026-08-17-37 — `must_use` on the COMPILE path. The lint ran only
    // from `cmd_run`, so `karac check` printed "All checks passed." and
    // `karac build` printed only "Built: …" for a program `karac run` warned
    // about. design.md § must_use calls it a compile warning; the JSON twin of
    // this block lives in `diag_json::collect_diagnostics`, and both read the
    // same lint with the same overrides so the two renderings cannot disagree.
    // Rendered here rather than pushed into `TypeCheckResult::warnings` so the
    // lint keeps its own `help`/`note` continuation lines.
    for diag in crate::must_use_lint::check_implicit_must_use(
        &pipeline.parsed.program,
        pipeline.typed.as_ref(),
        &pipeline.lint_overrides,
    ) {
        let severity = if diag.level == crate::must_use_lint::LintLevel::Error {
            "error"
        } else {
            "warning"
        };
        let mut block = with_snippet(
            format!(
                "{severity}[{}]: {}:{}:{}: {}",
                diag.lint_name, filename, diag.span.line, diag.span.column, diag.message
            ),
            source,
            diag.span.line,
            diag.span.column,
            diag.span.length,
        );
        if let Some(help) = &diag.help {
            block.push_str(&format!("\n  = help: {help}"));
        }
        if let Some(note) = &diag.note {
            block.push_str(&format!("\n  = note: {note}"));
        }
        out.push(block);
    }
    // B-2026-08-18-2 — the three SIBLING lints that were also invoked only
    // from `cmd_run`, joining `must_use` on the compile path. Which three, and
    // why not all five, is a measurement rather than a judgement call: sweeping
    // `karac check` over the 955-file examples + katas corpus with all five
    // wired produced 70 new diagnostics, and reading them split the set in two.
    //
    //   * `undocumented_unsafe` — 2 diagnostics, both on real user `unsafe`
    //     blocks with valid spans. One was a false positive on a MULTI-LINE
    //     `// Safety:` comment, fixed in the same change (see
    //     `check_unsafe_span`); the other is a genuine undocumented block.
    //   * `unsafe_op_in_unsafe_fn`, `ffi_float_eq` — 0 diagnostics. Silent on
    //     the whole corpus, so wiring them changes no existing output.
    //
    // The other two — `missing_must_use` and `missing_track_caller` — are NOT
    // here, and deliberately: they produced 68 of the 70, every one of them
    // rendered against the USER's filename with the baked stdlib item's own
    // span. `examples/autograd_training.kara` is 88 lines and drew diagnostics
    // at lines 378 and 387. Those are stdlib-hygiene findings pointing at
    // coordinates that do not exist in the file named, so putting them on the
    // JSON feed would feed the Mend loop 68 unresolvable locations — the exact
    // harm this row exists to prevent. Filed separately.
    for (lint_name, is_error, message, span) in
        crate::cli::diag_json::lint_entries_for_compile_path(pipeline)
    {
        out.push(render_lint_block_text(
            &lint_name, is_error, &message, filename, &span, source,
        ));
    }
    if let Some(ref e) = pipeline.effects {
        for err in &e.errors {
            if err.kind == EffectErrorKind::FfiLintHint {
                out.push(with_snippet(
                    format!(
                        "note[effect]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    source,
                    err.span.line,
                    err.span.column,
                    err.span.length,
                ));
            } else {
                out.push(with_snippet(
                    format!(
                        "error[effect]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    source,
                    err.span.line,
                    err.span.column,
                    err.span.length,
                ));
            }
        }
    }
    if let Some(ref o) = pipeline.ownership {
        for err in &o.errors {
            // B-2026-07-31-29: the label follows the same fatal/advisory split
            // that decides the exit code, so the rendered severity and the
            // outcome cannot contradict each other. Before, every kind printed
            // `error[ownership]` — including the advisory `UseAfterMove` that
            // codegen compiles by design — which produced the nonsense pairing
            // of an `error[…]` line immediately followed by `All checks passed.`
            let label = if Pipeline::is_fatal_ownership_kind(&err.kind) {
                "error[ownership]"
            } else {
                "warning[ownership]"
            };
            // An ownership error's `suggestion` had never reached the terminal:
            // this loop printed only `message`, while the sibling notes loop
            // below prints `help:`. So every carefully-written migration
            // suggestion — including E_CONCURRENT_SHARED_STRUCT's, which is the
            // one place the `par struct` answer is spelled out — was visible
            // only to a reader of the compiler source. Print it, in the same
            // `help:` shape the notes loop uses.
            let mut block = with_snippet(
                format!(
                    "{}: {}:{}:{}: {}",
                    label, filename, err.span.line, err.span.column, err.message
                ),
                source,
                err.span.line,
                err.span.column,
                err.span.length,
            );
            if let Some(ref sugg) = err.suggestion {
                write!(block, "\n  help: {sugg}").unwrap();
            }
            out.push(block);
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
            let mut block = with_snippet(
                format!(
                    "{}: {}:{}:{}: {}",
                    label, filename, note.span.line, note.span.column, note.message
                ),
                source,
                note.span.line,
                note.span.column,
                note.span.length,
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
            out.push(with_snippet(
                format!(
                    "error[comptime]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                ),
                source,
                err.span.line,
                err.span.column,
                err.span.length,
            ));
        }
    }
    out
}
