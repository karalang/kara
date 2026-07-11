pub mod ast;
pub mod attribute_validator;
#[cfg(not(target_arch = "wasm32"))]
pub mod build_cache;
pub mod call_graph;
pub mod catalog;
pub mod cfg;
pub mod cheader;
// CLI / REPL / multi-file driver surfaces are native-only — they reach
// `std::fs`, `std::process`, `rustyline`, and `ureq`, none of which have
// a wasm32 surface. The browser playground (tracker line 703) consumes
// only the in-process check pipeline + `Interpreter`, so excluding these
// modules from the wasm32 build is a strict subset of what the playground
// needs — see `pub fn run_playground` at the bottom of this file.
#[cfg(not(target_arch = "wasm32"))]
pub mod cli;
#[cfg(feature = "llvm")]
pub mod codegen;
pub mod codegen_queries;
pub mod componentize;
pub mod comptime;
pub mod concurrency;
pub mod concurrency_report;
pub mod cost_summary;
pub mod def_path;
#[cfg(not(target_arch = "wasm32"))]
pub mod dep_diagnostic;
#[cfg(not(target_arch = "wasm32"))]
pub mod dep_graph;
#[cfg(not(target_arch = "wasm32"))]
pub mod dep_resolver;
pub mod desugar;
pub mod diagnostic_attrs_lint;
pub mod diagnostic_class;
#[cfg(not(target_arch = "wasm32"))]
pub mod doc;
pub mod dominator;
#[cfg(feature = "llvm")]
pub mod drop_differential;
pub mod edit_distance;
pub mod effect_graph;
pub mod effectchecker;
pub mod exhaustive;
pub mod fallible_alloc;
pub mod ffi_lint;
pub mod float_math;
pub mod fork_threshold_queries;
pub mod formatter;
#[cfg(not(target_arch = "wasm32"))]
pub mod git_fetch;
pub mod gpu_wgsl;
#[cfg(not(target_arch = "wasm32"))]
pub mod install_spec;
pub mod interpreter;
#[cfg(not(target_arch = "wasm32"))]
pub mod karac_toolchain;
pub mod layout_queries;
pub mod lexer;
pub mod lints;
pub mod lockfile;
pub mod logical_lint;
pub mod lowering;
pub mod manifest;
pub mod missing_must_use_lint;
pub mod missing_track_caller_lint;
pub mod presize;
#[cfg(not(target_arch = "wasm32"))]
pub mod pubgrub_solve;
// `module`, `walker`, `manifest` carry data types consumed by `resolver`
// and `typechecker`, so they stay always-on. The fs-reading entry points
// inside them (`build_program_tree_with`'s `fs::read_to_string`,
// `walker::walk_project`, `manifest::Manifest::load_from`) are cfg-gated
// on `not(target_arch = "wasm32")` at the function level — see those
// modules for the wasm32 surface.
pub mod cross_task_safe;
pub mod module;
pub mod monomorphization;
pub mod must_use_lint;
pub mod numeric_conv;
pub mod ownership;
pub mod ownership_oracle;
pub mod parser;
pub mod prelude;
pub mod provider_escape;
pub mod queries;
pub mod query_attributes;
pub mod raii_check;
pub mod rc_fallback_queries;
pub mod rc_predicate;
pub mod reduce_kernel;
// Registry tarball extraction is native-only: it reaches `flate2`/`tar`/
// `std::fs` and builds on the wasm-gated `dep_graph` (`RegistryProvider`,
// `MaterializedDep`) + `registry_proxy` fetch surface. Its only consumer is
// the `karac` CLI (`crate::cli`, itself wasm-gated); the browser playground
// never fetches packages. Keeping it always-on made the wasm32 build fail
// with `unresolved import crate::dep_graph` (E0432).
#[cfg(not(target_arch = "wasm32"))]
pub mod registry_extract;
pub mod registry_proxy;
#[cfg(not(target_arch = "wasm32"))]
pub mod repl;
pub mod resolver;
#[cfg(not(target_arch = "wasm32"))]
pub mod scaffold;
pub mod simd_report;
pub mod span_visitor;
pub mod specialization_queries;
pub mod target;
pub mod test_jit_dispatch;
pub mod test_main_synth;
pub mod token;
pub mod typechecker;
pub mod unsafe_lint;
pub mod use_classifier;
pub mod walker;
pub mod wasm_exports;
pub mod wasm_glue;
pub mod wit;

use crate::ast::Program;
use crate::concurrency::{ConcurrencyAnalysis, ConcurrencyChecker};
use crate::effectchecker::{EffectCheckResult, EffectChecker, PublicEffectsPolicy};
use crate::lexer::Lexer;
use crate::manifest::{CompileProfile, ProfileConfig};
use crate::ownership::{OwnershipCheckResult, OwnershipChecker};
use crate::parser::{ParseResult, Parser};
use crate::resolver::{ResolveResult, Resolver};
use crate::token::SpannedToken;
use crate::typechecker::{TypeCheckResult, TypeChecker};

/// Convert a byte offset into the source string into a (line, column)
/// pair suitable for diagnostic display. 1-indexed for both axes,
/// matching the rest of `karac`'s diagnostic output.
///
/// Originally lived in `cli.rs`; promoted here so codegen's debugger-
/// contract metadata emission (Phase 8 § Auto-Concurrency Codegen —
/// Debugger Contract slice 3) can record `(line, col)` for each `par {}`
/// site without depending on cli-private state.
pub fn byte_offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Tokenize source code into a vector of spanned tokens.
pub fn tokenize(source: &str) -> Vec<SpannedToken> {
    let mut lexer = Lexer::new(source);
    let mut tokens = Vec::new();

    loop {
        let spanned = lexer.next_token();
        let is_eof = spanned.token == crate::token::Token::EOF;
        tokens.push(spanned);
        if is_eof {
            break;
        }
    }
    tokens
}

/// Parse source code into an AST with error reporting.
pub fn parse(source: &str) -> ParseResult {
    let tokens = tokenize(source);
    let parser = Parser::new(tokens);
    parser.parse()
}

/// Resolve names in a parsed program.
pub fn resolve(program: &Program) -> ResolveResult {
    let resolver = Resolver::new(program);
    resolver.resolve()
}

/// Run every pre-resolve AST-rewriting pass over `program` in place.
/// Today this elides argument-position `impl Trait` into anonymous generic
/// parameters (slice 2 of the `impl Trait` epic). Drivers in `lib.rs` and
/// `cli.rs` call this between [`parse`] and [`resolve`]; the formatter
/// path deliberately skips it so `impl Trait` round-trips verbatim.
pub fn desugar_program(program: &mut Program) -> Vec<crate::comptime::ComptimeError> {
    crate::desugar::desugar_program(program);
    // Pre-resolve comptime item expansion: `#[proto_schema]` consts become the
    // message `struct` types their `.proto` text declares, spliced before name
    // resolution so the rest of the program can reference them (protobuf slice
    // 3). A no-op (cheap scan) when no such const is present. Returned
    // diagnostics let callers that surface comptime errors render them; the
    // in-process test/runtime cores ignore them (the generated types still
    // splice on the happy path), mirroring how the post-resolve comptime fold
    // pass's diagnostics are handled.
    crate::comptime::expand_proto_schemas(program)
}

/// Type-check a parsed and resolved program.
pub fn typecheck(program: &Program, resolve_result: &ResolveResult) -> TypeCheckResult {
    let checker = TypeChecker::new(program, resolve_result);
    checker.check()
}

/// Type-check a baked stdlib module compiled as its own program
/// (`codegen::lower_stdlib_source`). Identical to [`typecheck`] except the
/// always-injected-stdlib collision-skip (#34) is disabled — a stdlib module's
/// own types match the injected copy, so the skip would make it skip itself.
pub fn typecheck_stdlib_module(
    program: &Program,
    resolve_result: &ResolveResult,
) -> TypeCheckResult {
    let checker = TypeChecker::new(program, resolve_result).compiling_stdlib();
    checker.check()
}

/// Type-check with CLI-driven build-wide lint level overrides
/// (slice 4b polish — `-A NAME` / `-W NAME` / `-D NAME` / `-F NAME`
/// / `-D warnings`). The CLI dispatch in `src/cli.rs` calls this
/// when any of the flags is set; in-process callers that don't need
/// CLI overrides keep using [`typecheck`]. The overrides feed into
/// the cascade fall-through via
/// [`crate::typechecker::TypeChecker::effective_lint_level`].
pub fn typecheck_with_lint_overrides(
    program: &Program,
    resolve_result: &ResolveResult,
    overrides: crate::lints::CliLintOverrides,
) -> TypeCheckResult {
    let checker = TypeChecker::new(program, resolve_result).with_cli_lint_overrides(overrides);
    checker.check()
}

/// Type-check with both CLI lint overrides and the manifest's `[profile]`-table
/// knob carrier (phase-8-stdlib-floor item 4). The carrier gates the
/// `panic_on_alloc_failure`-driven rejection passes (`E_PANICKING_ALLOC_REJECTED`,
/// `E_DERIVE_CLONE_ALLOCATES`). The `Pipeline` threads its `profile_config` here;
/// callers that need neither override keep using [`typecheck`].
pub fn typecheck_with_lint_overrides_and_profile(
    program: &Program,
    resolve_result: &ResolveResult,
    overrides: crate::lints::CliLintOverrides,
    profile_config: impl Into<crate::manifest::ProfileConfig>,
) -> TypeCheckResult {
    let checker = TypeChecker::new(program, resolve_result)
        .with_cli_lint_overrides(overrides)
        .with_profile_config(profile_config);
    checker.check()
}

/// Type-check with the manifest's `[profile]`-table knob carrier and no CLI lint
/// overrides — the form in-process callers and tests use to exercise the
/// `panic_on_alloc_failure`-gated rejection passes (items 4–5).
pub fn typecheck_with_profile_config(
    program: &Program,
    resolve_result: &ResolveResult,
    profile_config: impl Into<crate::manifest::ProfileConfig>,
) -> TypeCheckResult {
    let checker = TypeChecker::new(program, resolve_result).with_profile_config(profile_config);
    checker.check()
}

/// Rewrite operator expressions into trait-method calls in place.
/// Runs after typecheck (uses inferred operand types) and before
/// effectcheck / ownership / interpret / codegen.
pub fn lower(program: &mut Program, tc: &TypeCheckResult) {
    crate::lowering::lower_program(program, tc);
}

/// Evaluate every `comptime { ... }` block at compile time and splice the
/// folded constant in place. Runs after [`lower`] so the comptime evaluator
/// sees the same lowered tree the interpreter / codegen will, and so
/// downstream phases see plain constants. Returns the comptime diagnostics
/// (empty on success). Substrate 1 of the comptime feature — see
/// [`crate::comptime`].
pub fn comptime_eval(
    program: &mut Program,
    tc: &TypeCheckResult,
) -> Vec<crate::comptime::ComptimeError> {
    crate::comptime::evaluate(program, tc)
}

/// Check effects in a parsed program (default policy: `Declared`).
pub fn effectcheck(program: &Program) -> EffectCheckResult {
    let checker = EffectChecker::new(program);
    checker.check()
}

/// Check effects with an explicit [`crate::manifest::ProfileConfig`] — the
/// carrier of the active profile plus its `[profile]`-table knobs (e.g.
/// `panic = "unwind" | "abort"`). Needed by the C-unwind gate
/// (`E_EXTERN_C_UNWIND_REQUIRES_UNWIND_PROFILE`), which is only lifted under an
/// explicit `panic = "unwind"`.
pub fn effectcheck_with_profile_config(
    program: &Program,
    profile_config: impl Into<crate::manifest::ProfileConfig>,
) -> EffectCheckResult {
    let checker = EffectChecker::new(program).with_profile_config(profile_config.into());
    checker.check()
}

/// Check effects in a parsed program with an explicit public-effects policy.
/// See `effectchecker::PublicEffectsPolicy`.
pub fn effectcheck_with_policy(
    program: &Program,
    policy: PublicEffectsPolicy,
) -> EffectCheckResult {
    let checker = EffectChecker::new_with_policy(program, policy);
    checker.check()
}

/// Check effects in a parsed program with an explicit policy and compile profile.
/// The profile gates which effects are legal at `extern` declaration sites.
pub fn effectcheck_with_profile(
    program: &Program,
    policy: PublicEffectsPolicy,
    profile: CompileProfile,
) -> EffectCheckResult {
    let checker = EffectChecker::new_with_policy_and_profile(program, policy, profile);
    checker.check()
}

/// Check effects with the typechecker's method-callee resolution table threaded
/// in. Required for method-call-site analyses (`with E` unification, Fn-slot
/// subtyping, function-reference arg propagation through polymorphic methods)
/// to resolve to the precise `Type.method` instead of an over-approximation
/// over every method with a matching name. Pass the result of `typecheck`
/// (specifically `tc.method_callee_types`) before `lower`.
pub fn effectcheck_with_method_types(
    program: &Program,
    policy: PublicEffectsPolicy,
    profile: CompileProfile,
    method_callee_types: std::collections::HashMap<crate::resolver::SpanKey, String>,
) -> EffectCheckResult {
    let checker = EffectChecker::new_with_policy_and_profile(program, policy, profile)
        .with_method_callee_types(method_callee_types);
    checker.check()
}

/// Check effects with both the method-callee resolution table and the per-call
/// type-parameter substitutions from the typechecker (Round 10.3 step 7).
/// Required for E0404 diagnostics on compound polymorphic calls to render the
/// callee's monomorphized signature.
pub fn effectcheck_with_typecheck_data(
    program: &Program,
    policy: PublicEffectsPolicy,
    profile_config: impl Into<ProfileConfig>,
    method_callee_types: std::collections::HashMap<crate::resolver::SpanKey, String>,
    call_type_subs: std::collections::HashMap<
        crate::resolver::SpanKey,
        std::collections::HashMap<String, String>,
    >,
) -> EffectCheckResult {
    // Accepts a bare `CompileProfile` (via `From`) or the full `ProfileConfig`
    // knob carrier — the `Pipeline` threads the latter from the manifest's
    // parsed `[profile]` table.
    let checker = EffectChecker::new_with_policy_and_config(program, policy, profile_config.into())
        .with_method_callee_types(method_callee_types)
        .with_call_type_subs(call_type_subs);
    checker.check()
}

/// Analyze concurrency opportunities in a parsed program.
///
/// Convenience wrapper over [`concurrency_analyze_typed`] with no type info —
/// method-call network fan-out (A2b-2 Phase 2 Slice 2) is disabled (fail-closed,
/// since it needs receiver types). The full CLI pipeline calls the `_typed`
/// form; most tests use this one.
pub fn concurrency_analyze(program: &Program, effects: &EffectCheckResult) -> ConcurrencyAnalysis {
    concurrency_analyze_typed(program, effects, None)
}

/// Like [`concurrency_analyze`] but with the typecheck result, whose
/// `method_callee_types` (receiver type name per method-call span) drives the
/// method-receiver classification for A2b-2 Phase 2 Slice 2. Pass `Some(&tc)`
/// from the full pipeline; `None` disables method-call fan-out.
pub fn concurrency_analyze_typed(
    program: &Program,
    effects: &EffectCheckResult,
    types: Option<&TypeCheckResult>,
) -> ConcurrencyAnalysis {
    let checker = ConcurrencyChecker::new(program, effects, types);
    checker.analyze()
}

/// Check ownership and move semantics.
pub fn ownershipcheck(
    program: &Program,
    typecheck_result: &TypeCheckResult,
) -> OwnershipCheckResult {
    let checker = OwnershipChecker::new(program, typecheck_result);
    checker.check()
}

/// Check ownership with the manifest's `[profile]`-table knob carrier
/// (phase-8-stdlib-floor item 6). Under `panic_on_alloc_failure = false`, every
/// auto-RC fallback site becomes a hard
/// `E_RC_FALLBACK_ALLOCATES_UNDER_FALLIBLE_PROFILE` error. The `Pipeline` threads
/// its `profile_config` here; in-process callers that don't need it keep using
/// [`ownershipcheck`].
pub fn ownershipcheck_with_profile_config(
    program: &Program,
    typecheck_result: &TypeCheckResult,
    profile_config: impl Into<crate::manifest::ProfileConfig>,
) -> OwnershipCheckResult {
    let checker =
        OwnershipChecker::new(program, typecheck_result).with_profile_config(profile_config);
    checker.check()
}

/// Run the formal RC predicate pipeline (use classifier → CFG →
/// dominator tree → predicate) over every function in `program`.
/// Returns `function_key → binding → witness`, mirroring the shape
/// of `OwnershipCheckResult::rc_values`. Round 12.10 parity scaffold.
///
/// This driver does NOT replace `ownershipcheck`'s live diagnostics —
/// it only catches the trigger-1 (branch-divergent re-use after
/// consume) flavor today. Closure-capture (trigger 2) and container-store
/// (trigger 3) classification stay in `OwnershipChecker` until later
/// rounds wire them into the use classifier.
pub fn predicate_rc_candidates(
    program: &Program,
    typecheck_result: &TypeCheckResult,
) -> std::collections::HashMap<String, std::collections::HashMap<String, rc_predicate::RcWitness>> {
    rc_predicate::predicate_rc_candidates_for_program(program, typecheck_result)
}

/// Round 12.15: companion driver for direct use-after-move. Returns
/// the witnesses where a Consume site of a binding strictly precedes
/// another use (same-block source order or cross-block dominance) —
/// the error case the formal RC predicate explicitly excludes. Same
/// shape as `predicate_rc_candidates`; consumed by the parity matrix
/// in `tests/rc_predicate_parity.rs` and the eventual in-place
/// integration into `OwnershipChecker::check_function_body`.
pub fn predicate_uam_candidates(
    program: &Program,
    typecheck_result: &TypeCheckResult,
) -> std::collections::HashMap<String, std::collections::HashMap<String, rc_predicate::UamWitness>>
{
    rc_predicate::predicate_uam_candidates_for_program(program, typecheck_result)
}

/// Check for closures that capture a provider-rooted resource and escape
/// their `with_provider` / `providers { }` scope. See design.md §
/// Provider-Rooted Resources and `src/provider_escape.rs` for scope.
///
/// When `types` is `Some`, instance-method calls are resolved against
/// the typechecker's `expr_types` map; pass `None` when typecheck info
/// is unavailable (the checker still catches every other escape path).
pub fn provider_escape_check(
    program: &Program,
    types: Option<&TypeCheckResult>,
) -> Vec<provider_escape::EscapeError> {
    provider_escape::check_provider_escape(program, types)
}

/// Run the RAII-across-yield check. Implements the v1 rule from
/// design.md § Network Event Loop and State-Machine Transform > RAII
/// Across Yield Points: a network-boundary function cannot hold a
/// non-cancel-safe binding live across any yield point. Slice 1 detects
/// the unambiguous shared-struct / shared-enum case via the
/// typechecker's `is_shared` flag — see `src/raii_check.rs` for scope
/// and the (intentionally not-yet-shipped) marker-trait extensibility.
/// Returns an empty list when `types` is `None`.
pub fn raii_across_yield_check(
    program: &Program,
    types: Option<&TypeCheckResult>,
) -> Vec<raii_check::RaiiAcrossYieldError> {
    raii_check::check_raii_across_yield(program, types)
}

/// Run a closure on a freshly spawned thread with a 16 MB stack and
/// return its result. The tree-walk interpreter's `eval_expr_inner` and
/// `eval_call` are huge match-on-AST functions; debug builds give them
/// large frames, and each Kāra function call traverses ~8 Rust frames,
/// so a `fib(10)` (Kāra depth 10) overflows the default 2 MB cargo-test
/// thread stack on Windows. Lifting the interpreter onto a fat-stack
/// scoped thread keeps the fix local to the entry points without
/// instrumenting every recursion site with `stacker::maybe_grow`. Panics
/// inside the closure propagate back to the caller via `resume_unwind`
/// so test assertions and runtime panics still surface normally.
#[cfg(not(target_arch = "wasm32"))]
fn run_on_interp_thread<R, F>(f: F) -> R
where
    R: Send,
    F: FnOnce() -> R + Send,
{
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, f)
            .expect("failed to spawn interpreter thread");
        match handle.join() {
            Ok(v) => v,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// wasm32 (the browser playground): thread spawning is unsupported —
/// `std::thread::Builder::spawn_scoped` returns `Err(Unsupported)` and
/// the `.expect` above turned EVERY playground `run` into a wasm trap.
/// Run inline on the caller's stack instead; the browser main thread's
/// stack is what the playground gets, and the fat-stack workaround only
/// matters for the Windows cargo-test default (see above).
#[cfg(target_arch = "wasm32")]
fn run_on_interp_thread<R, F>(f: F) -> R
where
    R: Send,
    F: FnOnce() -> R + Send,
{
    f()
}

/// Run a program through all phases and execute it via the interpreter.
/// Returns captured output lines (for testing).
pub fn run_program(source: &str) -> Vec<String> {
    let (output, _trace, _truncated) = run_program_with_trace(source);
    output
}

/// Run a program and return (output_lines, error_trace, trace_truncated).
pub fn run_program_with_trace(
    source: &str,
) -> (Vec<String>, Vec<interpreter::ErrorTraceFrame>, bool) {
    let (output, _errors, trace, truncated) = run_program_full(source);
    (output, trace, truncated)
}

/// Run a program and return (output_lines, drop_trace). The drop trace
/// records the order in which `CleanupAction::Drop` slots fire — both
/// NLL early-drops (mid-block, after a binding's last use) and scope-exit
/// drops drained from the unified cleanup stack. Used by sub-step 3
/// (live-range-end placement) tests since the interpreter has no
/// observable user-`impl Drop` dispatch yet.
pub fn run_program_with_drops(source: &str) -> (Vec<String>, Vec<String>) {
    run_on_interp_thread(|| {
        let mut parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "Parse errors: {:?}",
            parsed.errors
        );
        desugar_program(&mut parsed.program);
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "Resolve errors: {:?}",
            resolved.errors
        );
        let typed = typecheck(&parsed.program, &resolved);
        lower(&mut parsed.program, &typed);
        comptime_eval(&mut parsed.program, &typed);
        let mut interp = interpreter::Interpreter::new(&parsed.program, &typed);
        interp.captured_output = Some(Vec::new());
        interp.run();
        let output = interp.captured_output.take().unwrap_or_default();
        let drops = std::mem::take(&mut interp.drop_trace);
        (output, drops)
    })
}

/// Run a program with `dbg()` output capture enabled. Returns
/// `(stdout_lines, dbg_lines)`. The interpreter is configured with
/// `source_text` set so `dbg()` can slice expression text from the
/// source. `mode` selects terminal-format vs JSON-format dbg lines —
/// see [`interpreter::DbgOutputMode`]. Each dbg line includes the
/// trailing `\n`. Used by `tests/interpreter.rs` to assert exact dbg
/// formatting.
pub fn run_program_with_dbg(
    source: &str,
    mode: interpreter::DbgOutputMode,
) -> (Vec<String>, Vec<String>) {
    run_on_interp_thread(|| {
        let mut parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "Parse errors: {:?}",
            parsed.errors
        );
        desugar_program(&mut parsed.program);
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "Resolve errors: {:?}",
            resolved.errors
        );
        let typed = typecheck(&parsed.program, &resolved);
        lower(&mut parsed.program, &typed);
        comptime_eval(&mut parsed.program, &typed);
        let mut interp = interpreter::Interpreter::new(&parsed.program, &typed);
        interp.captured_output = Some(Vec::new());
        interp.captured_dbg = Some(Vec::new());
        interp.set_source_text(source);
        interp.set_source_filename("test.kara");
        interp.set_dbg_output_mode(mode);
        interp.run();
        let stdout = interp.captured_output.take().unwrap_or_default();
        let dbg = interp.captured_dbg.take().unwrap_or_default();
        (stdout, dbg)
    })
}

/// Run a program and return output, runtime errors, error trace, and truncation flag.
/// Used by tests that need to assert on user-triggered runtime errors (division by
/// zero, integer overflow, unwrap of None, index out of bounds, etc.) without
/// catching a Rust panic.
pub fn run_program_full(
    source: &str,
) -> (
    Vec<String>,
    Vec<interpreter::RuntimeError>,
    Vec<interpreter::ErrorTraceFrame>,
    bool,
) {
    run_on_interp_thread(|| {
        let mut parsed = parse(source);
        assert!(
            parsed.errors.is_empty(),
            "Parse errors: {:?}",
            parsed.errors
        );
        desugar_program(&mut parsed.program);
        let resolved = resolve(&parsed.program);
        assert!(
            resolved.errors.is_empty(),
            "Resolve errors: {:?}",
            resolved.errors
        );
        // Type-check but don't abort on errors — the tree-walk interpreter
        // is dynamically typed and handles generics, partial types, etc.
        let typed = typecheck(&parsed.program, &resolved);
        // Operator lowering: rewrite Binary/Unary into trait-method calls.
        lower(&mut parsed.program, &typed);
        // Comptime fold: evaluate `comptime { ... }` blocks at compile time
        // and splice their constant results in before interpretation.
        comptime_eval(&mut parsed.program, &typed);
        let mut interp = interpreter::Interpreter::new(&parsed.program, &typed);
        interp.captured_output = Some(Vec::new());
        interp.run();
        let trace = interp.error_trace().to_vec();
        let truncated = interp.error_trace_truncated();
        let errors = std::mem::take(&mut interp.runtime_errors);
        let output = interp.captured_output.take().unwrap_or_default();
        (output, errors, trace, truncated)
    })
}

// ── Browser playground entry point ──────────────────────────────────
//
// Tracker line 703 — `play.kara-lang.org`. Single source string in,
// structured envelope out: captured stdout + compile / runtime
// diagnostics + an overall `ok` flag. The wasm-bindgen wrapper in the
// `playground/` workspace member serializes this verbatim to JS-side
// JSON. Native unit tests pin the contract independent of any wasm
// machinery so the entrypoint stays observable in plain `cargo test`.

/// Single normalized diagnostic emitted by any compiler phase or by the
/// interpreter. `phase` is one of `"parse"`, `"resolve"`, `"typecheck"`,
/// `"effect"`, `"ownership"`, `"runtime"` — the playground UI keys severity /
/// color off this field. Span fields are 1-indexed line/column matching the
/// rest of `karac`'s diagnostic surface plus the raw byte offset/length so
/// the editor can move the cursor with no recomputation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaygroundDiagnostic {
    pub phase: &'static str,
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub offset: usize,
    pub length: usize,
}

/// Structured result of one playground run.
///
/// - `stdout`: captured `println` / `print` output lines (no trailing `\n`).
/// - `diagnostics`: every error / warning produced across the pipeline,
///   plus any runtime errors recorded by the interpreter. Empty on a fully
///   clean run.
/// - `ok`: true iff every phase ran cleanly AND the interpreter recorded
///   no runtime errors. A type / effect / ownership warning marks `ok = false`
///   even though the interpreter still ran — the UI uses this to switch
///   the run-button indicator from green to amber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaygroundResult {
    pub stdout: Vec<String>,
    pub diagnostics: Vec<PlaygroundDiagnostic>,
    pub ok: bool,
}

fn push_diag(
    out: &mut Vec<PlaygroundDiagnostic>,
    phase: &'static str,
    message: String,
    span: &crate::token::Span,
) {
    out.push(PlaygroundDiagnostic {
        phase,
        message,
        line: span.line,
        column: span.column,
        offset: span.offset,
        length: span.length,
    });
}

/// Run `source` through the full check pipeline and (when possible) the
/// interpreter, returning a [`PlaygroundResult`] envelope suitable for
/// rendering in a browser playground.
///
/// Posture mirrors `run_program_full` for the post-resolve phases (typecheck
/// / effect / ownership errors are collected but the interpreter still runs —
/// the tree-walk interpreter is dynamically typed and does not enforce
/// effects or ownership). Parse and resolve errors are hard stops: the
/// downstream phases would surface noise rooted in unresolved names, so we
/// return early with just the parse / resolve diagnostics.
pub fn run_playground(source: &str) -> PlaygroundResult {
    let (mut diagnostics, artifacts) = run_static_checks(source);
    let Some((program, typed)) = artifacts else {
        // Parse or resolve hard-stopped — return the collected diagnostics
        // without executing.
        return PlaygroundResult {
            stdout: Vec::new(),
            diagnostics,
            ok: false,
        };
    };

    let pre_interp_diag_count = diagnostics.len();
    let (stdout, runtime_errors) = run_on_interp_thread(|| {
        let mut interp = interpreter::Interpreter::new(&program, &typed);
        interp.captured_output = Some(Vec::new());
        interp.run();
        let errors = std::mem::take(&mut interp.runtime_errors);
        let output = interp.captured_output.take().unwrap_or_default();
        (output, errors)
    });
    for e in &runtime_errors {
        push_diag(&mut diagnostics, "runtime", e.message.clone(), &e.span);
    }

    let ok = pre_interp_diag_count == 0 && runtime_errors.is_empty();
    PlaygroundResult {
        stdout,
        diagnostics,
        ok,
    }
}

/// Shared static-check pipeline for [`check_source`] and [`run_playground`] —
/// the single source of truth for phase ordering and the parse/resolve
/// hard-stop posture. Runs parse → desugar → resolve → typecheck → lower →
/// effect → ownership, pushing a normalized [`PlaygroundDiagnostic`] per error.
/// Returns the collected diagnostics plus, when parse + resolve both succeeded
/// (so execution is *possible*), the lowered program and its typecheck result
/// for a caller that wants to go on and interpret. Parse and resolve errors are
/// hard stops (downstream phases would surface noise rooted in unresolved
/// names), signalled by a `None` artifact.
fn run_static_checks(
    source: &str,
) -> (
    Vec<PlaygroundDiagnostic>,
    Option<(Program, TypeCheckResult)>,
) {
    let mut diagnostics = Vec::new();

    let mut parsed = parse(source);
    if !parsed.errors.is_empty() {
        for e in &parsed.errors {
            push_diag(&mut diagnostics, "parse", e.message.clone(), &e.span);
        }
        return (diagnostics, None);
    }

    desugar_program(&mut parsed.program);

    let resolved = resolve(&parsed.program);
    if !resolved.errors.is_empty() {
        for e in &resolved.errors {
            push_diag(&mut diagnostics, "resolve", e.message.clone(), &e.span);
        }
        return (diagnostics, None);
    }

    let typed = typecheck(&parsed.program, &resolved);
    for e in &typed.errors {
        push_diag(&mut diagnostics, "typecheck", e.message.clone(), &e.span);
    }

    lower(&mut parsed.program, &typed);

    let effects = effectcheck(&parsed.program);
    for e in &effects.errors {
        push_diag(&mut diagnostics, "effect", e.message.clone(), &e.span);
    }

    let ownership = ownershipcheck(&parsed.program, &typed);
    for e in &ownership.errors {
        push_diag(&mut diagnostics, "ownership", e.message.clone(), &e.span);
    }

    (diagnostics, Some((parsed.program, typed)))
}

/// Run `source` through the full **static-check** pipeline (parse → desugar →
/// resolve → typecheck → effect → ownership) and return every diagnostic in the
/// normalized [`PlaygroundDiagnostic`] form, WITHOUT executing the program. The
/// interpreter-free sibling of [`run_playground`]: identical phase order and
/// parse/resolve hard-stop posture (both delegate to [`run_static_checks`]), but
/// no side effects. This is the entry point editor tooling — the `kara-lsp`
/// language server — drives on every edit: static feedback only, never running
/// user code.
pub fn check_source(source: &str) -> Vec<PlaygroundDiagnostic> {
    run_static_checks(source).0
}

#[cfg(test)]
mod playground_tests {
    use super::*;

    #[test]
    fn run_playground_pure_expression_succeeds_with_no_output() {
        let result = run_playground("fn main() { let x = 1 + 2; }");
        assert!(result.ok, "diagnostics: {:?}", result.diagnostics);
        assert!(result.stdout.is_empty());
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn run_playground_captures_println_output() {
        let result = run_playground("fn main() { println(\"hello\"); println(\"world\"); }");
        assert!(result.ok, "diagnostics: {:?}", result.diagnostics);
        // captured_output preserves trailing `\n`s; the browser UI joins this
        // verbatim into a <pre>. See the existing run_program tests in
        // tests/interpreter.rs for the same convention.
        assert_eq!(
            result.stdout,
            vec!["hello\n".to_string(), "world\n".to_string()]
        );
    }

    #[test]
    fn run_playground_reports_parse_error_without_running() {
        let result = run_playground("fn main() { let = ; }");
        assert!(!result.ok);
        assert!(!result.diagnostics.is_empty());
        assert!(result.diagnostics.iter().all(|d| d.phase == "parse"));
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn run_playground_reports_resolve_error_without_running() {
        let result = run_playground("fn main() { let _ = undefined_name(); }");
        assert!(!result.ok);
        assert!(result.diagnostics.iter().any(|d| d.phase == "resolve"));
        assert!(result.stdout.is_empty());
    }

    #[test]
    fn run_playground_runs_through_typecheck_warning_and_captures_output() {
        // A program with stdout that runs cleanly proves the post-warning
        // run path works; type errors specifically don't block execution
        // (tree-walk interpreter is dynamically typed).
        let result = run_playground("fn main() { println(\"executed\"); }");
        assert!(result.ok);
        assert_eq!(result.stdout, vec!["executed\n".to_string()]);
    }

    #[test]
    fn run_playground_records_runtime_error_as_diagnostic() {
        // Unwrap-of-None is the canonical user-triggerable runtime error
        // that records into `interp.runtime_errors` without panicking the
        // host process. The `panics` effect on main is required since the
        // unwrap path tracks the `panics` effect.
        let result =
            run_playground("fn main() panics { let x: Option[i32] = None; let _ = x.unwrap(); }");
        assert!(!result.ok);
        assert!(
            result.diagnostics.iter().any(|d| d.phase == "runtime"),
            "expected a runtime diagnostic, got: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn run_playground_diagnostic_span_carries_line_column_offset() {
        let result = run_playground("fn main() { let _ = undefined_name(); }");
        let first = result
            .diagnostics
            .first()
            .expect("expected at least one diagnostic");
        assert_eq!(first.phase, "resolve");
        assert!(first.line >= 1);
        assert!(first.column >= 1);
        assert!(first.length > 0);
    }

    #[test]
    fn run_playground_reports_multiple_resolve_errors_in_one_pass() {
        // Two undefined names in one function — both must surface as separate
        // diagnostics so the playground's diagnostics list shows all the
        // pinpoints rather than only the first. Pins that the early-return
        // copies every error, not just `errors[0]`.
        let result = run_playground("fn main() { let _ = first_undef(); let _ = second_undef(); }");
        assert!(!result.ok);
        let resolve_count = result
            .diagnostics
            .iter()
            .filter(|d| d.phase == "resolve")
            .count();
        assert!(
            resolve_count >= 2,
            "expected at least 2 resolve diagnostics, got: {:?}",
            result.diagnostics
        );
    }

    #[test]
    fn run_playground_ok_false_when_typecheck_error_even_if_stdout_present() {
        // A program with a type error still gets executed (the tree-walk
        // interpreter is dynamically typed), so stdout is captured — but
        // `ok` must be false so the playground UI flags the run as not
        // clean. Specifically pins that the `pre_interp_diag_count`
        // bookkeeping flows into `ok` correctly.
        let result =
            run_playground("fn main() { let x: i32 = \"not an int\"; println(\"ran anyway\"); }");
        assert!(
            !result.ok,
            "expected ok=false; diagnostics: {:?}",
            result.diagnostics
        );
        assert_eq!(result.stdout, vec!["ran anyway\n".to_string()]);
        assert!(result.diagnostics.iter().any(|d| d.phase == "typecheck"));
    }

    #[test]
    fn check_source_clean_program_has_no_diagnostics() {
        assert!(check_source("fn main() { let x = 1 + 2; }").is_empty());
    }

    #[test]
    fn check_source_surfaces_parse_error() {
        let diags = check_source("fn main() { let = ; }");
        assert!(!diags.is_empty());
        assert!(diags.iter().all(|d| d.phase == "parse"));
    }

    #[test]
    fn check_source_surfaces_resolve_error_with_span() {
        let diags = check_source("fn main() { let _ = undefined_name(); }");
        let first = diags.first().expect("expected a diagnostic");
        assert_eq!(first.phase, "resolve");
        assert!(first.line >= 1 && first.column >= 1 && first.length > 0);
    }

    #[test]
    fn check_source_surfaces_typecheck_error() {
        let diags = check_source("fn main() { let x: i32 = \"not an int\"; }");
        assert!(diags.iter().any(|d| d.phase == "typecheck"));
    }

    #[test]
    fn check_source_does_not_execute_the_program() {
        // The interpreter-free contract: a program whose ONLY observable
        // effect is a runtime error (unwrap of None) must yield ZERO
        // diagnostics from `check_source` — it type/effect/ownership-checks
        // clean and is never run, so no `"runtime"` phase entry appears
        // (whereas `run_playground` on the same source records one).
        let diags =
            check_source("fn main() panics { let x: Option[i32] = None; let _ = x.unwrap(); }");
        assert!(
            diags.iter().all(|d| d.phase != "runtime"),
            "check_source must not execute; got: {diags:?}"
        );
    }

    #[test]
    fn check_source_matches_run_playground_static_prefix() {
        // The two entry points must agree on the static (pre-interpreter)
        // diagnostics — they share `run_static_checks`, and this pins that
        // they stay in sync.
        let src = "fn main() { let x: i32 = \"nope\"; let _ = undefined(); }";
        let check = check_source(src);
        let play = run_playground(src);
        let play_static: Vec<_> = play
            .diagnostics
            .into_iter()
            .filter(|d| d.phase != "runtime")
            .collect();
        assert_eq!(check, play_static);
    }
}
