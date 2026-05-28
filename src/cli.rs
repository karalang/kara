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
        /// `--concurrency-report` (Slice D, 2026-05-08): also emit the
        /// human-readable concurrency analysis to stdout after checks
        /// complete. Already runs `concurrencycheck()` via
        /// `Pipeline::run_all_checks`, so wiring is purely render-side.
        concurrency_report: bool,
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
        /// the speedup. See `docs/demo_ideas.md:80-88` for the locked
        /// output shape.
        concurrency_report: bool,
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
        /// profile]` overlay selection (tracker line 882). Precedence
        /// is `--target=<triple>` > `[build].target` from the manifest
        /// > `build_cache::host_target_triple()`.
        target: Option<String>,
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
        /// `--atomic` opts into the L215c Atomic[T] heuristic. When
        /// set, project-mode classifies each mut field as Atomic[T]
        /// (every observed write across the workspace is a bare `=`
        /// assignment AND T is in the lock-free Copy set: `i32`,
        /// `i64`, `u32`, `u64`, `usize`, `isize`, `bool`) or Mutex[T]
        /// (anything else). Atomic-classified fields' consumer sites
        /// are NOT lock-wrapped — the reviewer hand-converts to
        /// `.store(v, Ordering)` / `.load(Ordering)`. Without the
        /// flag the L215a–b4 default (all-Mutex with consumer wraps)
        /// applies. Ignored in single-file mode (no workspace
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
        } => cmd_run(
            &file,
            output,
            sequential,
            manifest_override.as_deref(),
            no_manifest,
            lint_overrides,
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
            concurrency_report,
            lint_overrides,
        } => cmd_check(&file, output, profiles, concurrency_report, lint_overrides),
        Command::Build {
            file,
            output,
            concurrency_report,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
            lint_overrides,
        } => cmd_build(
            &file,
            output,
            concurrency_report,
            offline,
            enable_hot_swap,
            no_proxy,
            target.as_deref(),
            lint_overrides,
        ),
        Command::BuildProject {
            output,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
        } => cmd_build_project(
            output,
            offline,
            enable_hot_swap,
            no_proxy,
            target.as_deref(),
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
        Command::Test { filter, all } => cmd_test(filter, all),
        Command::Repl { auto_clone } => {
            crate::repl::run_with_options(crate::repl::ReplOptions { auto_clone })
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
    profile: crate::manifest::CompileProfile,
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
            profile: crate::manifest::CompileProfile::Default,
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
        crate::desugar_program(&mut self.parsed.program);
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
        self.typed = Some(crate::typecheck_with_lint_overrides(
            &self.parsed.program,
            self.resolved.as_ref().unwrap(),
            self.lint_overrides.clone(),
        ));
    }

    /// Apply the operator-lowering pass. Runs after typecheck (uses inferred
    /// operand types) and before any downstream phase that consumes the AST
    /// (effectcheck / ownership / interpreter / codegen).
    fn lower(&mut self) {
        if let Some(ref typed) = self.typed {
            crate::lower(&mut self.parsed.program, typed);
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
        self.effects = Some(crate::effectcheck_with_typecheck_data(
            &self.parsed.program,
            crate::effectchecker::PublicEffectsPolicy::default(),
            self.profile,
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
        self.ownership = Some(crate::ownershipcheck(
            &self.parsed.program,
            self.typed.as_ref().unwrap(),
        ));
    }

    fn concurrencycheck(&mut self) {
        if self.effects.is_none() {
            return;
        }
        self.concurrency = Some(crate::concurrency_analyze(
            &self.parsed.program,
            self.effects.as_ref().unwrap(),
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
        self.has_parse_errors() || self.has_resolve_errors() || self.has_type_errors()
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
        n
    }
}

// ── Text Output ─────────────────────────────────────────────────

fn print_text_diagnostics(pipeline: &Pipeline) {
    let filename = &pipeline.filename;
    for err in &pipeline.parsed.errors {
        eprintln!(
            "error[parse]: {}:{}:{}: {}",
            filename, err.span.line, err.span.column, err.message
        );
    }
    if let Some(ref r) = pipeline.resolved {
        for err in &r.errors {
            eprintln!(
                "error[resolve]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
    }
    if let Some(ref t) = pipeline.typed {
        for err in &t.errors {
            eprintln!(
                "error[typecheck]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
    }
    if let Some(ref e) = pipeline.effects {
        for err in &e.errors {
            if err.kind == EffectErrorKind::FfiLintHint {
                eprintln!(
                    "note[effect]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            } else {
                eprintln!(
                    "error[effect]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
            }
        }
    }
    if let Some(ref o) = pipeline.ownership {
        for err in &o.errors {
            eprintln!(
                "error[ownership]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
    }
    if let Some(ref esc) = pipeline.provider_escape {
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
    if let Some(ref raii) = pipeline.raii_errors {
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
            extra_json: None,
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
            };
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
                extra_json: None,
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
                crate::typechecker::TypeErrorKind::UnknownLint => "W0244",
                crate::typechecker::TypeErrorKind::Deprecated => "W0245",
                crate::typechecker::TypeErrorKind::MissingNonExhaustive => "W0246",
                crate::typechecker::TypeErrorKind::UnfulfilledLintExpectation => "W0249",
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
            };
            let extra_json = err.subtype_trace.as_ref().map(|t| {
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

    let total = parse_errors
        + resolve_errors
        + type_errors
        + effect_errors
        + ownership_errors
        + escape_errors
        + raii_errors;
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
    cmd_run(&path, output, sequential, None, true, lint_overrides);
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

fn cmd_run(
    filename: &str,
    output: OutputMode,
    sequential: bool,
    manifest_override: Option<&str>,
    no_manifest: bool,
    lint_overrides: crate::lints::CliLintOverrides,
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

    let source = read_source(filename);
    let mut pipeline = Pipeline::new(filename, &source).with_lint_overrides(lint_overrides);
    if let Some(ref m) = discovered_manifest {
        pipeline.profile = m.profile;
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

    // Type-check (non-fatal for interpreter)
    pipeline.typecheck();
    pipeline.lower();

    if output == OutputMode::Text {
        // Print type warnings to stderr
        if let Some(ref t) = pipeline.typed {
            for err in &t.errors {
                eprintln!(
                    "warning[typecheck]: {}:{}:{}: {}",
                    filename, err.span.line, err.span.column, err.message
                );
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
    // than proceeds to the interpreter.
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

    // Run
    let mut interp = Interpreter::new(&pipeline.parsed.program, pipeline.typed.as_ref().unwrap());
    interp.set_source_filename(filename);
    interp.set_source_text(&source);
    interp.set_dbg_output_mode(match output {
        OutputMode::Json | OutputMode::Jsonl => DbgOutputMode::Json,
        OutputMode::Text => DbgOutputMode::Terminal,
    });
    interp.sequential_mode = sequential;
    interp.run();

    // Emit error return trace if present
    if !interp.error_trace().is_empty() {
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
}

fn cmd_check(
    filename: &str,
    output: OutputMode,
    profiles: Option<Vec<crate::manifest::CompileProfile>>,
    concurrency_report: bool,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    let source = read_source(filename);

    if let Some(list) = profiles {
        cmd_check_profiles(filename, &source, output, &list, lint_overrides);
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

// CLI dispatch helpers naturally land more flag-shaped arguments
// than the clippy default; factoring them into a struct here would
// just move the flag list rather than tighten it.
#[allow(clippy::too_many_arguments)]
fn cmd_build(
    filename: &str,
    output: OutputMode,
    concurrency_report: bool,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
    lint_overrides: crate::lints::CliLintOverrides,
) {
    // Single-file mode runs no dep resolution and reaches no network surface,
    // so `--offline` is silently accepted for ergonomic CLI consistency with
    // project mode (operators script both via the same flag set). Likewise
    // `--target` has no manifest to apply against and is accepted-but-inert.
    let _ = offline;
    let _ = target;
    emit_no_proxy_note(no_proxy);
    let _ = no_proxy;
    #[cfg(feature = "llvm")]
    {
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

        // Derive output executable name from the source filename.
        let exe_name = std::path::Path::new(filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output");
        let obj_path = format!("/tmp/karac_{exe_name}.o");
        let exe_path = if cfg!(windows) {
            format!("{exe_name}.exe")
        } else {
            exe_name.to_string()
        };

        if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
            &pipeline.parsed.program,
            &obj_path,
            pipeline.ownership.as_ref(),
            pipeline.concurrency.as_ref(),
            Some(filename),
            Some(&source),
            enable_hot_swap,
        ) {
            eprintln!("error: codegen failed: {e}");
            process::exit(1);
        }
        match crate::codegen::link_executable(&obj_path, &exe_path) {
            Err(e) => {
                eprintln!("error: link failed: {e}");
                let _ = std::fs::remove_file(&obj_path);
                process::exit(1);
            }
            Ok(()) => {
                let _ = std::fs::remove_file(&obj_path);
                match output {
                    OutputMode::Text => println!("Built: {exe_path}"),
                    OutputMode::Json => println!("{{\"status\":\"ok\",\"output\":\"{exe_path}\"}}"),
                    OutputMode::Jsonl => unreachable!(),
                }
            }
        }
    }
    #[cfg(not(feature = "llvm"))]
    {
        let _ = enable_hot_swap;
        eprintln!("note: karac build requires the llvm feature; falling back to type check");
        cmd_check(filename, output, None, concurrency_report, lint_overrides);
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
fn cmd_build_project(
    output: OutputMode,
    offline: bool,
    enable_hot_swap: bool,
    no_proxy: bool,
    target: Option<&str>,
) {
    // --offline implies --no-proxy at the contract level (vendor-only walk
    // can't talk to the proxy). Suppress the redundant no-proxy note when
    // both are set so the offline operator sees one clean status line.
    if !offline {
        emit_no_proxy_note(no_proxy);
    }
    let _ = no_proxy;
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
    // so the overlay merge consumes a stable reference.
    let active_target: String = target
        .map(str::to_string)
        .or_else(|| raw_manifest.build_default_target.clone())
        .unwrap_or_else(crate::build_cache::host_target_triple);

    // Merge `[target.<triple>].dependencies` / `[target.<triple>].profile`
    // overlays onto the manifest before any downstream consumer reads it
    // (dep resolution, profile gating, codegen). Always applied with the
    // resolved active triple so the build sees one consistent view.
    let mf = manifest::merge_target_overlay(&raw_manifest, Some(active_target.as_str()));

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
    if has_deps && !run_dep_resolution(&root, mf.clone(), output, offline_root, false) {
        process::exit(1);
    }

    let walk_opts = WalkerOpts::default();
    let walked = match walker::walk_project(&root, walk_opts) {
        Ok(w) => w,
        Err(e) => {
            emit_walker_error(&e, output);
            process::exit(1);
        }
    };

    let built = match module::build_program_tree(&walked) {
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
    let type_errors: Vec<ModuleTypeErrors> =
        if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
            typecheck_modules(&tree)
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
    let mut codegen_status: BuildCodegenStatus = BuildCodegenStatus::Skipped;
    if !cfg!(feature = "llvm") {
        // Mirror the single-file `cmd_build` no-llvm fallback (line ~2393).
        codegen_status = BuildCodegenStatus::NoLlvmFeature;
    } else if parse_errors.is_empty()
        && cycles.is_empty()
        && resolve_errors.is_empty()
        && type_errors.is_empty()
    {
        codegen_status = run_multi_file_codegen(&tree, &mf, &root, enable_hot_swap);
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
                BuildCodegenStatus::Built { exe_path } => {
                    println!("Built: {}", exe_path.display());
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
                BuildCodegenStatus::Built { exe_path } => format!(
                    ",\"output\":{}",
                    json_string(&exe_path.display().to_string()),
                ),
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
            if let BuildCodegenStatus::Built { exe_path } = &codegen_status {
                emit_jsonl_event(
                    "build_artifact",
                    &format!(
                        "\"output\":{}",
                        json_string(&exe_path.display().to_string())
                    ),
                );
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
    /// All phases succeeded; the linked executable is at `exe_path`.
    Built { exe_path: PathBuf },
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
fn run_multi_file_codegen(
    tree: &ProgramTree,
    mf: &crate::manifest::Manifest,
    project_root: &std::path::Path,
    enable_hot_swap: bool,
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
        profile: crate::manifest::CompileProfile::Default,
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

    // 4. Codegen — write to a temp object then link to the manifest's
    // `name` field as the binary basename in the project root.
    let exe_path = project_root.join(&mf.name);
    let obj_path = std::env::temp_dir().join(format!(
        "karac_proj_{}_{}.o",
        std::process::id(),
        mf.name.replace(['/', '\\'], "_"),
    ));

    if let Err(e) = crate::codegen::compile_to_object_with_hot_swap(
        &pipeline.parsed.program,
        &obj_path.to_string_lossy(),
        pipeline.ownership.as_ref(),
        pipeline.concurrency.as_ref(),
        None,
        None,
        enable_hot_swap,
    ) {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "codegen".to_string(),
            message: format!("codegen failed: {e}"),
        };
    }
    if let Err(e) =
        crate::codegen::link_executable(&obj_path.to_string_lossy(), &exe_path.to_string_lossy())
    {
        let _ = std::fs::remove_file(&obj_path);
        return BuildCodegenStatus::Failed {
            phase: "link".to_string(),
            message: format!("link failed: {e}"),
        };
    }
    let _ = std::fs::remove_file(&obj_path);
    BuildCodegenStatus::Built { exe_path }
}

/// Stub for the no-llvm build — never invoked because the caller gates
/// on `cfg!(feature = "llvm")`. Kept as a parallel signature so the call
/// site doesn't need cfg gating itself.
#[cfg(not(feature = "llvm"))]
fn run_multi_file_codegen(
    _tree: &ProgramTree,
    _mf: &crate::manifest::Manifest,
    _project_root: &std::path::Path,
    _enable_hot_swap: bool,
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
        ResolveErrorKind::DeprecatedOnImpl => "E0241",
        ResolveErrorKind::DeprecatedOnField => "E0242",
        ResolveErrorKind::UnknownAttribute => "E0243",
        ResolveErrorKind::ProfileInvalidTarget => "E0244",
        ResolveErrorKind::UnknownProfile => "E0245",
        ResolveErrorKind::QueryResolutionConflict => "E_QUERY_RESOLUTION_CONFLICT",
        ResolveErrorKind::UnionNonExhaustiveForbidden => "E_UNION_NON_EXHAUSTIVE_FORBIDDEN",
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
fn typecheck_modules(tree: &ProgramTree) -> Vec<ModuleTypeErrors> {
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
fn run_dep_resolution(
    root: &std::path::Path,
    mf: crate::manifest::Manifest,
    output: OutputMode,
    offline_root: Option<&std::path::Path>,
    include_dev_deps: bool,
) -> bool {
    let loader = crate::dep_graph::FsLoader;
    let options = crate::dep_graph::DepGraphOptions {
        offline_root,
        include_dev_deps,
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
    match crate::dep_resolver::resolve_with_offline(&graph, &active, offline_root) {
        Ok(resolution) => {
            persist_lockfile(root, &resolution, output);
            true
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
            severity == "warning"
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
    let config =
        crate::registry_proxy::ProxyConfig::from_env(crate::registry_proxy::ProxyMode::Disabled);
    eprintln!(
        "note: --no-proxy active; registry deps will not consult the proxy at {} (registry-proxy fetch ships in v1.1.x)",
        config.url
    );
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
            pipeline.effectcheck();
            query_effects(&pipeline, function);
        }
        QueryKind::Ownership => {
            pipeline.typecheck();
            pipeline.lower();
            pipeline.ownershipcheck();
            query_ownership(&pipeline, function);
        }
        QueryKind::Concurrency => {
            pipeline.effectcheck();
            pipeline.concurrencycheck();
            query_concurrency(&pipeline, function);
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
            // Run every phase that may populate `queries`. v1 catalogue
            // is empty so the output is `{"queries":[]}`; the surface
            // lands so external tooling pinning to `karac query queries`
            // gets a stable command without waiting for P1.x entries.
            pipeline.typecheck();
            pipeline.lower();
            pipeline.effectcheck();
            pipeline.ownershipcheck();
            pipeline.concurrencycheck();
            query_queries(&pipeline);
        }
        QueryKind::Monomorphization => {
            // Reads from `TypeCheckResult.call_type_subs` +
            // `method_callee_types`; typecheck is the only phase
            // required.
            pipeline.typecheck();
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
    let table = crate::monomorphization::analyze(&pipeline.parsed.program, tc);
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

fn query_effects(pipeline: &Pipeline, function: &str) {
    let effects = pipeline.effects.as_ref().unwrap();

    let inferred = effects.inferred_effects.get(function);
    let declared = effects.declared_effects.get(function);

    if inferred.is_none() && declared.is_none() {
        eprintln!("error: function '{function}' not found");
        process::exit(1);
    }

    let mut inferred_list: Vec<String> = Vec::new();
    if let Some(set) = inferred {
        for te in &set.effects {
            inferred_list.push(format!(
                "{{\"verb\":{},\"resource\":{}}}",
                json_string(effect_verb_str(&te.effect.verb)),
                json_string(&te.effect.resource),
            ));
        }
    }

    let declared_str = match declared {
        Some(DeclaredEffects::Explicit(set)) => {
            let mut list: Vec<String> = Vec::new();
            for te in &set.effects {
                list.push(format!(
                    "{{\"verb\":{},\"resource\":{}}}",
                    json_string(effect_verb_str(&te.effect.verb)),
                    json_string(&te.effect.resource),
                ));
            }
            format!("[{}]", list.join(","))
        }
        Some(DeclaredEffects::Polymorphic) => "\"polymorphic\"".to_string(),
        Some(DeclaredEffects::PolymorphicWithFixed(set)) => {
            let mut list: Vec<String> = Vec::new();
            for te in &set.effects {
                list.push(format!(
                    "{{\"verb\":{},\"resource\":{}}}",
                    json_string(effect_verb_str(&te.effect.verb)),
                    json_string(&te.effect.resource),
                ));
            }
            format!("{{\"polymorphic\":true,\"fixed\":[{}]}}", list.join(","))
        }
        Some(DeclaredEffects::None) | None => "null".to_string(),
    };

    println!(
        "{{\"function\":{},\"inferred_effects\":[{}],\"declared_effects\":{}}}",
        json_string(function),
        inferred_list.join(","),
        declared_str,
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

fn query_concurrency(pipeline: &Pipeline, function: &str) {
    let analysis = pipeline.concurrency.as_ref().unwrap();

    match analysis.function_decisions.get(function) {
        Some(fc) => {
            let group_entries: Vec<String> = fc
                .parallel_groups
                .iter()
                .map(|g| {
                    let indices: Vec<String> =
                        g.statement_indices.iter().map(|i| i.to_string()).collect();
                    format!(
                        "{{\"statements\":[{}],\"reason\":{}}}",
                        indices.join(","),
                        json_string(&g.reason),
                    )
                })
                .collect();
            println!(
                "{{\"function\":{},\"total_statements\":{},\"parallel_groups\":[{}]}}",
                json_string(function),
                fc.total_statements,
                group_entries.join(","),
            );
        }
        None => {
            eprintln!("error: function '{function}' not found");
            process::exit(1);
        }
    }
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
    if pipeline.has_parse_errors() {
        for err in &pipeline.parsed.errors {
            eprintln!(
                "error[parse]: {}:{}:{}: {}",
                filename, err.span.line, err.span.column, err.message
            );
        }
        process::exit(1);
    }
    pipeline.run_all_checks();

    let mut edits: Vec<crate::resolver::TextEdit> = Vec::new();
    if let Some(ref r) = pipeline.resolved {
        edits.extend(
            r.errors
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
    // Opt-in via `--atomic` flag — without the flag, the L215a–b4
    // default behavior applies (every mut field is Mutex[T] and the
    // consumer-rewrite wraps every site). Project-mode only — single-
    // file mode lacks workspace visibility for the "every write is a
    // bare `=` assign" judgment.
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
        is_track_caller: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
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

fn cmd_test(filter: Option<String>, all: bool) {
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
    let has_resolvable_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    if has_resolvable_deps {
        let loader = crate::dep_graph::FsLoader;
        let options = crate::dep_graph::DepGraphOptions {
            offline_root: None,
            include_dev_deps: true,
        };
        let graph_result =
            crate::dep_graph::build_dep_graph_with_options(&root, mf.clone(), &loader, options);
        match graph_result {
            Ok(graph) => {
                let active = crate::dep_resolver::active_toolchain_version();
                match crate::dep_resolver::resolve(&graph, &active) {
                    Ok(resolution) => {
                        persist_lockfile(&root, &resolution, OutputMode::Jsonl);
                    }
                    Err(boxed) => {
                        let code = boxed.code();
                        // Registry/git unsupported downgrade to a note here too —
                        // the test surface continues even when those v1.1.x
                        // deps can't yet be fetched, matching the build flow.
                        if !matches!(code, "E_REGISTRY_DEP_UNSUPPORTED" | "E_GIT_DEP_UNSUPPORTED") {
                            emit_test_event(
                                "dep_resolution_error",
                                &format!(
                                    "\"code\":{},\"message\":{}",
                                    json_string(code),
                                    json_string(&boxed.to_string()),
                                ),
                            );
                            process::exit(1);
                        }
                    }
                }
            }
            Err(e) => {
                emit_test_event(
                    "dep_resolution_error",
                    &format!(
                        "\"code\":{},\"message\":{}",
                        json_string(e.code()),
                        json_string(&e.to_string()),
                    ),
                );
                process::exit(1);
            }
        }
    }

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

    let built = match module::build_program_tree_with(
        &walked,
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

    let type_errors: Vec<ModuleTypeErrors> =
        if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
            typecheck_modules(&tree)
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
    let mut current_module: Option<usize> = None;
    // Per-module state: built lazily on first test in each module.
    let mut current_program: Option<Program> = None;
    let mut current_typed: Option<TypeCheckResult> = None;

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

        // Lazily prepare per-module Program + typecheck result so we don't
        // re-parse / re-resolve / re-typecheck for every test in the same
        // module. Tests are sorted by `module_id`, so each `current_module`
        // transition happens exactly once per module.
        if current_module != Some(t.module_id) {
            let m = &tree.modules[t.module_id];
            // `Item::TestCase` lowering has already happened at the
            // global tree level (see `lower_and_discover_test_cases`),
            // so cloning the module's items hands the standard
            // resolver / typechecker / interpreter pipeline a regular
            // `Item::Function` body that `run_test_function(t.fn_name)`
            // looks up through the usual `call_function` path.
            let program = Program {
                items: m.items.clone(),
                ..Program::default()
            };
            let resolved = Resolver::new(&program)
                .with_tree(&tree, t.module_id as ModuleId)
                .resolve();
            let typed = crate::typechecker::TypeChecker::new(&program, &resolved)
                .with_tree(&tree, t.module_id as ModuleId)
                .check();
            current_program = Some(program);
            current_typed = Some(typed);
            current_module = Some(t.module_id);
        }
        let program_ref = current_program.as_ref().unwrap();
        let typed_ref = current_typed.as_ref().unwrap();
        let module = &tree.modules[t.module_id];

        let test_file_path = module
            .test_file
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();

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

        let started = std::time::Instant::now();
        let outcome = interp.run_test_function(&t.fn_name);
        let duration_ms = started.elapsed().as_millis();

        // Pop every fixture frame before emitting the event so any error
        // handling below sees a clean stack for the next test.
        for _ in 0..pushed_frames {
            interp.test_pop_provider_frame();
        }

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
    if let Some(left) = &outcome.left {
        s.push_str(&format!(",\"left\":{}", json_string(left)));
    }
    if let Some(right) = &outcome.right {
        s.push_str(&format!(",\"right\":{}", json_string(right)));
    }
    s
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
