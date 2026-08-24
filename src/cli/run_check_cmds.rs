//! `karac run` / `check` (+ profiles and per-target checks) — the
//! interpret/JIT executor drivers and the check pipeline reporting.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 3).

use super::*;

// ── Commands ────────────────────────────────────────────────────

pub(super) fn format_error_trace_json(frames: &[ErrorTraceFrame], truncated: bool) -> String {
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

pub(super) fn cmd_run_example(
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
    // `run --example` takes no trailing program arguments — the examples it
    // runs are self-contained demos, so the program argv is just the script.
    cmd_run(
        &path,
        output,
        sequential,
        None,
        true,
        lint_overrides,
        None,
        false,
        &[],
    );
}

pub(super) fn list_available_examples() {
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
pub(super) fn main_return_is_exitcode(program: &Program) -> bool {
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
pub(super) fn build_super_program_for_run(tree: &ProgramTree) -> Program {
    let order = module::emission_order(tree);
    // Flattening drops every `import` declaration and puts all modules in one
    // scope, which breaks two things the tree-aware per-module passes got
    // right: an ALIAS binding names nothing once its import is gone
    // (B-2026-08-20-20), and two modules declaring the same top-level name
    // collide (B-2026-08-20-24). `module_rename` repairs both, per module, in
    // one substitution — and the BUILD path's merge calls the same helper, so
    // the two cannot answer differently again.
    let renames = crate::module_rename::plan(tree);
    let mut items: Vec<Item> = Vec::new();
    for &id in &order {
        if tree.modules[id].is_synthetic {
            continue;
        }
        items.extend(crate::module_rename::flatten_module_items(
            tree, id, &renames,
        ));
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
pub(super) fn quiet_dep_package_walks(root: &std::path::Path) -> Vec<module::DepPackageWalk> {
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
/// Refuse to run a package whose module-aware check fails.
///
/// `karac run` executes a merged super-program (see
/// [`try_build_run_super_program`]), which is exactly why it needs this: the
/// merge flattens away the module boundaries that carry visibility, so the
/// run path's own resolve cannot see a `private` item being imported across
/// directories. Without the gate, `karac run` was the one surface that
/// executed a program `karac build` rejects.
///
/// Diagnostics are rendered exactly as the build renders them, so the two
/// commands are quotable against each other.
fn gate_run_on_package_check(root: &std::path::Path, output: OutputMode) {
    let mut lints = crate::lints::CliLintOverrides::default();
    if let Ok(mf) = manifest::load_from_root(root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    // A walk/tree failure is NOT fatal here: the single-file path below will
    // surface its own diagnostic against the entry file, and this gate exists
    // to add errors the run path cannot see, never to invent a new way for
    // `karac run` to fail on a package the build handles.
    let Ok(pc) = super::build_cmds::package_check(root, &lints, crate::target::active_target())
    else {
        return;
    };
    if !pc.has_errors() {
        return;
    }
    match output {
        OutputMode::Json => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            println!("{{\"diagnostics\":[{}]}}", diags.join(","));
        }
        OutputMode::Jsonl => {
            for e in super::build_cmds::parse_errors_jsonl(&pc.parse_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::cycles_jsonl(&pc.cycles, &pc.tree) {
                println!("{e}");
            }
            for e in super::build_cmds::resolve_errors_jsonl(&pc.resolve_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::type_errors_jsonl(&pc.type_errors) {
                println!("{e}");
            }
        }
        OutputMode::Text => {
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
        }
    }
    process::exit(1);
}

pub(super) fn try_build_run_super_program(filename: &str, no_manifest: bool) -> Option<Program> {
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
pub(super) fn run_ir_via_jit_subprocess(ir: &str, program_argv: &[String]) -> i32 {
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
    // Hand the PROGRAM's argv to the runner's runtime. Under the JIT the
    // hosting process is `karac_jit_runner`, so `std::env::args()` inside
    // `karac_runtime_env_args_into` reported the runner's path plus the temp
    // `.ll` file rather than anything the user wrote (B-2026-07-29-18). The
    // runtime prefers this variable when present and falls back to process
    // argv otherwise, which keeps an AOT binary — where the process IS the
    // program — on exactly the path it always used.
    //
    // Unit separator (U+001F) rather than NUL: an environment variable's value
    // is a NUL-terminated C string, so NUL cannot appear inside it. U+001F is
    // valid in a value and does not occur in real arguments.
    // `KARAC_DBG_OUTPUT` (the JIT'd program's `dbg` output format) is INHERITED
    // from this process rather than set here — `cmd_run` sets it from
    // `--output` before reaching this helper, which has no view of the flag.
    let status = std::process::Command::new(&runner)
        .arg(&ir_path)
        .env("KARAC_PROGRAM_ARGS", program_argv.join("\u{1F}"))
        .status();
    let _ = std::fs::remove_file(&ir_path);
    match status {
        Ok(s) => exit_code_of(&s),
        Err(e) => {
            eprintln!("error: could not spawn karac_jit_runner: {e}");
            1
        }
    }
}

/// The runner's exit status as a shell-visible code, INCLUDING a death by
/// signal — B-2026-08-19-7.
///
/// `ExitStatus::code()` returns `None` on Unix when the child was killed by a
/// signal, and the old `unwrap_or(1)` collapsed every such death to a generic
/// failure. That matters here because a signal death is the CORRECT outcome for
/// the most common case: `karac run prog | head -2` closes the reader, the
/// kernel kills the runner with SIGPIPE, and the AOT binary for the same source
/// reports 141. Reporting 1 instead would keep `karac run` diverging from
/// `karac build` on the very status the sibling fix exists to align.
///
/// `128 + signal` is the shell's own convention (bash, dash, zsh) for
/// reporting a signal death through `$?`, so this is the encoding any harness
/// checking the producer's status already expects.
///
/// Gated on `llvm` because its only caller is the JIT path: without that
/// feature there is no runner to spawn, the function is dead, and CI runs the
/// DEFAULT clippy leg — which is where an ungated version fails.
#[cfg(feature = "llvm")]
fn exit_code_of(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// The whole-buffer GPU reductions, which reach `karac_runtime_gpu_*` WITHOUT
/// a `#[gpu]` kernel anywhere in the program.
///
/// `gpu.sum(v)` names no kernel — the operation is named instead, so its
/// associativity is the compiler's guarantee rather than a user combiner's
/// promise. That is the whole point of the surface, and it is exactly why
/// [`program_declares_gpu_kernel`] cannot see these: there is no `#[gpu] fn`
/// to find. Without this list a reduction program takes the JIT lane and dies
/// with `Symbols not found: [ karac_runtime_gpu_reduce_f32 ]` while
/// `karac build` runs it correctly — a run-vs-build divergence.
#[cfg(feature = "llvm")]
const GPU_REDUCTION_CALLS: &[&str] = &[
    "gpu.sum(",
    "gpu.prod(",
    "gpu.min(",
    "gpu.max(",
    "gpu.mean(",
    "gpu.dot(",
    "gpu.argmin(",
    "gpu.argmax(",
    "gpu.variance(",
    "gpu.stddev(",
    "gpu.prefix_sum(",
    "gpu.matmul(",
];

/// True when the program reaches the GPU runtime at all, and so must route to
/// the tree-walk interpreter rather than the JIT.
///
/// Two ways to get there, and they need different detection:
///
///  * a `#[gpu]` kernel — a `#[gpu] fn`, top-level or an impl method — reached
///    through `gpu.dispatch`. Detected on the AST. The kernel is a sound
///    superset of an actual dispatch call: a `#[gpu]` fn is only reachable
///    through dispatch, and a declared-but-never-dispatched kernel routes to
///    the interpreter harmlessly (it runs every program).
///  * a whole-buffer REDUCTION (`gpu.sum` and friends), which has no kernel to
///    detect. Scanned in the source, like the Arrow IPC gate beside it — a
///    false positive only routes a non-GPU program to the correct-if-slower
///    interpreter.
///
/// Either way the JIT runner's runtime rlib is built WITHOUT the opt-in `gpu`
/// feature (the heavy wgpu/Metal backend), so the LLJIT `dlsym` generator
/// cannot resolve `karac_runtime_gpu_*` and `main` fails to materialize
/// (B-2026-07-10-6). The AOT `karac build` path, which auto-selects
/// `libkarac_runtime_gpu.a` and links the real backend, is unaffected.
#[cfg(feature = "llvm")]
pub(super) fn program_uses_gpu_runtime(program: &crate::ast::Program, source: &str) -> bool {
    program_declares_gpu_kernel(program)
        || GPU_REDUCTION_CALLS.iter().any(|call| source.contains(call))
}

/// True when the program declares a `#[gpu]` kernel — a `#[gpu] fn`, top-level
/// or an impl method. The AST half of [`program_uses_gpu_runtime`]; see there
/// for why a reduction needs the source-scan half as well.
#[cfg(feature = "llvm")]
pub(super) fn program_declares_gpu_kernel(program: &crate::ast::Program) -> bool {
    use crate::ast::{ImplItem, Item};
    program.items.iter().any(|item| match item {
        Item::Function(f) => f.is_gpu,
        Item::ImplBlock(b) => b
            .items
            .iter()
            .any(|ii| matches!(ii, ImplItem::Method(m) if m.is_gpu)),
        _ => false,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_run(
    filename: &str,
    output: OutputMode,
    sequential: bool,
    manifest_override: Option<&str>,
    no_manifest: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    timeout: Option<std::time::Duration>,
    interp: bool,
    program_args: &[String],
) {
    // `dbg()` output format for a COMPILED run (design.md § `dbg()` —
    // "Structured mode (`--output=json` or `--output=jsonl`)"). The interpreter
    // takes the mode as a value (`DbgOutputMode`, below); a compiled program has
    // no CLI flag of its own, so its runtime reads `KARAC_DBG_OUTPUT`. Set here,
    // once, so the JIT subprocess inherits it — `run_ir_via_jit_subprocess`
    // never sees `output` (B-2026-08-23-18).
    if matches!(output, OutputMode::Json | OutputMode::Jsonl) {
        std::env::set_var("KARAC_DBG_OUTPUT", "json");
    }
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
    // B-2026-08-13-12 — `--interp` runs the tree-walk backend, which accepts the
    // chained-field-receiver shape codegen defers; the gate would otherwise take
    // away a working execution path over a deferral that run never reaches.
    let mut pipeline = if interp {
        Pipeline::new(filename, &source)
            .with_lint_overrides(lint_overrides)
            .interpreter_bound()
    } else {
        Pipeline::new(filename, &source).with_lint_overrides(lint_overrides)
    };
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
        // The merged super-program is a FLAT list of every module's items, so
        // the resolve below no longer knows which module an item came from —
        // and therefore cannot enforce visibility. `import db.helpers.secret;`
        // on a `private` item in another directory ran happily here while
        // `karac build` refused it with E0111 (B-2026-08-20-16): the default
        // execution path was running a program the compiler rejects.
        //
        // Gate on the module-aware check before executing. Errors are rendered
        // for the WHOLE package, not just the entry file, because running
        // requires all of it to be sound — unlike `karac check <file>`, which
        // answers about the file it was asked about.
        if !no_manifest {
            if let Some(root) = package_root_of_member(filename) {
                gate_run_on_package_check(&root, output);
            }
        }
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
    // Shared with the build gate (`Pipeline::has_fatal_effect_errors`) so the
    // two lanes cannot drift apart — B-2026-08-05-17, and the same
    // one-classifier discipline B-2026-07-31-29 established for ownership.
    let is_fatal_effect = Pipeline::is_fatal_effect_kind;
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
                    // Lint-warning band (`kind_is_lint_warning`,
                    // B-2026-08-21-2) — advisory, named by its lint.
                    ref k if crate::effectchecker::kind_is_lint_warning(k) => eprintln!(
                        "warning[pure_loop_in_par]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    // Note-severity kinds, per the one shared predicate
                    // (`kind_is_note`, B-2026-08-21-2).
                    ref k if crate::effectchecker::kind_is_note(k) => eprintln!(
                        "note[effect]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    ),
                    _ => {}
                }
            }
        }
        // B-2026-08-18-20 — `TypeCheckResult::warnings`, the channel
        // `deprecated` / `unstable_api` / `map_value_clone_reinsert` ride.
        // `cmd_run` had a block for `must_use` (just below) and nothing for
        // this one, so a `#[deprecated]` call was reported by `karac check`
        // and silent under BOTH `karac run` and `karac run --interp` — an
        // inconsistency inside one lane, since the two lints differ only in
        // which channel carries them.
        //
        // Rendered through the same helper `check` and `build` use rather than
        // a third hand-rolled block, so the three lanes cannot word one
        // warning differently. Not `render_text_diagnostics` wholesale: that
        // also renders `must_use`, which this path already prints below with
        // its own `note`/`help` continuation lines, so it would double-print.
        if let Some(ref t) = pipeline.typed {
            for block in
                crate::cli::render_typecheck_warning_blocks(t, filename, pipeline.source.as_deref())
            {
                eprintln!("{block}");
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
        // Lints NOT run on a user compile: `missing_must_use` and
        // `missing_track_caller` (B-2026-08-18-16).
        //
        // Both are stdlib-HYGIENE lints: they fire only on items where
        // `Function.stdlib_origin == true`, which on a user compile means
        // exactly the items `prelude::synthetic_prelude_items` splices in.
        // The comment that used to sit here said "end-user compiles see no
        // output from this pass because baked stdlib items aren't spliced
        // into the user program AST". That premise is no longer true, and a
        // corpus sweep is what caught it: across the 955 `.kara` files in
        // examples/ + kara-katas these two produced 68 diagnostics on 6 real
        // user files.
        //
        // Worse than noise, the diagnostics are UNLOCATABLE. The span belongs
        // to the baked stdlib source while the renderer prints the user's
        // filename, so `examples/autograd_training.kara` — 88 lines long —
        // drew diagnostics at lines 378 and 387, and `mandelbrot.kara` was
        // told `fn animation_frames` lacks `#[must_use]` at line 94, where
        // that function is not defined at all (it is imported from
        // `std.web.time`).
        //
        // Suppressing here is the fix rather than a workaround, because there
        // is no rendering that would make these actionable for the person
        // compiling: the finding is about karac's own stdlib, in a file they
        // did not write and cannot edit from their program. The lints keep
        // their value where it belongs — `tests/missing_must_use_lint.rs` and
        // `tests/missing_track_caller_lint.rs` run them against
        // `STDLIB_PROGRAMS` directly, which is the scope their own module docs
        // describe. A future bundled-stdlib-source compile mode is where they
        // would legitimately return to a command.
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
    // B-2026-07-10-6 — route any program that reaches the GPU runtime to the
    // tree-walk interpreter (element-wise CPU) regardless of the JIT-default.
    // Its codegen emits `karac_runtime_gpu_*` calls, but the JIT runner's
    // runtime rlib is built WITHOUT the opt-in `gpu` feature (the heavy
    // wgpu/Metal backend), so the LLJIT dlsym generator can't resolve the
    // symbol and `main` fails to materialize (`Symbols not found:
    // [ karac_runtime_gpu_map ]`). The interpreter runs the same program
    // correctly with no GPU dependency, so this closes the run-vs-build
    // divergence. The AOT `karac build` path (which auto-selects
    // libkarac_runtime_gpu.a and links the real backend) is unaffected.
    //
    // "Reaches the GPU runtime" is BOTH a `#[gpu]` kernel and a whole-buffer
    // reduction — `gpu.sum(v)` names no kernel, so the AST check alone missed
    // it and every reduction program died on the JIT lane.
    #[cfg(feature = "llvm")]
    let program_uses_gpu = program_uses_gpu_runtime(&pipeline.parsed.program, &source);
    // Arrow IPC programs (`col.to_arrow_ipc()` / `Column.from_arrow_ipc(..)`)
    // route to the interpreter on the JIT lane — NOT because codegen lacks the
    // lowering (the `to_arrow_ipc` twins ship for all three receivers) but
    // because the sibling `karac_jit_runner` links the runtime WITHOUT the
    // opt-in `arrow` feature, so the `karac_arrow_*` symbols it would `dlsym`
    // don't exist there. Same structural reason as the `gpu` fallback above.
    // The AOT `karac build` path is unaffected: it auto-selects
    // `libkarac_runtime_arrow.a` and links the real backend. A source scan
    // (not an AST walk) keeps the gate cheap; a false positive only routes a
    // non-arrow program to the (correct, if unoptimized) interpreter.
    #[cfg(feature = "llvm")]
    let program_uses_arrow_ipc =
        source.contains(".to_arrow_ipc(") || source.contains("from_arrow_ipc(");
    // LazyFrame programs run on the JIT lane like everything else since
    // the codegen twin covered the full op surface (phase-11 slice 9);
    // the interpreter-routing gate that lived here was the interim
    // measure. A residual unsupported shape (impl methods / closures
    // returning Lazy values) is an ordinary codegen gap: loud Err +
    // the --interp hint below.
    #[cfg(feature = "llvm")]
    if output == OutputMode::Text
        && timeout.is_none()
        && !interp
        && !program_uses_gpu
        && !program_uses_arrow_ipc
        && std::env::var("KARAC_RUN_JIT").as_deref() != Ok("0")
    {
        // Codegen consumes ownership + concurrency; run them now so the
        // emitted IR matches `karac build`'s.
        pipeline.ownershipcheck();
        pipeline.concurrencycheck();
        // B-2026-08-17-13 — consult the ownership VERDICT, not just the
        // result. This lane used to hand `pipeline.ownership` to codegen as
        // data while never reading its errors, so an ownership-rejected
        // program (e.g. a definite-assignment failure) surfaced as a cryptic
        // `codegen failed: Undefined variable` instead of the ownership
        // diagnostic `karac check`/`build` print. Same fatal/advisory
        // classifier as the build gate (B-2026-07-31-29's one-classifier
        // discipline).
        if let Some(ref o) = pipeline.ownership {
            let fatal: Vec<_> = o
                .errors
                .iter()
                .filter(|e| Pipeline::is_fatal_ownership_kind(&e.kind))
                .collect();
            if !fatal.is_empty() {
                for err in &fatal {
                    eprintln!(
                        "error[ownership]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    );
                    if let Some(sug) = &err.suggestion {
                        eprintln!("  help: {sug}");
                    }
                }
                process::exit(1);
            }
        }
        match crate::codegen::compile_to_ir_with_options(
            &pipeline.parsed.program,
            pipeline.ownership.as_ref(),
            pipeline.concurrency.as_ref(),
            Some(filename),
            Some(&source),
        ) {
            Ok(ir) => {
                // B-2026-07-14-19 — a program that uses `Regex.compile` /
                // `is_match` emits `call ... @karac_regex_*`, resolved only from
                // the opt-in `libkarac_runtime_regex.a`. The JIT runner's runtime
                // is built WITHOUT the `regex` feature, so the LLJIT dlsym
                // generator can't materialize those symbols (`Symbols not found:
                // [ karac_regex_validate ]`). The interpreter runs regex
                // correctly with no extra dep, so route there — closing the
                // run-vs-build divergence (mirrors the `gpu` fallback above; the
                // AOT `karac build` path auto-selects the regex archive and is
                // unaffected). Gate on a `call` line, not the unconditional
                // extern *declaration* every module carries.
                let uses_regex = ir
                    .lines()
                    .any(|l| l.contains("@karac_regex_") && l.contains("call"));
                // B-2026-08-20-41 — `String.normalize(form)` has the same
                // shape: its `karac_unicode_*` symbols live only in the opt-in
                // `libkarac_runtime_unicode.a`, which the JIT runner does not
                // build, so the dlsym generator cannot materialize them. The
                // interpreter normalizes through the same `icu_normalizer`, so
                // routing there is byte-identical rather than merely close.
                let uses_unicode = ir
                    .lines()
                    .any(|l| l.contains("@karac_unicode_") && l.contains("call"));
                if !uses_regex && !uses_unicode {
                    let mut jit_argv = Vec::with_capacity(program_args.len() + 1);
                    jit_argv.push(filename.to_string());
                    jit_argv.extend_from_slice(program_args);
                    process::exit(run_ir_via_jit_subprocess(&ir, &jit_argv));
                }
                // else: fall through to the interpreter below.
            }
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

    // B-2026-08-17-13 — the interpreter lane must not execute programs the
    // ownership phase rejects. It deliberately skipped `ownershipcheck`
    // (codegen was the only consumer), which meant a definite-assignment
    // failure like `let x: i64; println(f"{x}")` RAN here and printed `()`
    // — the Unit value in an i64 slot — while `karac build` refused the
    // same program. Run the check and gate on the same fatal classifier as
    // build; advisory kinds (RcFallbackNote, UseAfterMove) stay lenient.
    if pipeline.ownership.is_none() {
        pipeline.ownershipcheck();
    }
    let fatal_ownership: Vec<crate::ownership::OwnershipError> = pipeline
        .ownership
        .as_ref()
        .map(|o| {
            o.errors
                .iter()
                .filter(|e| Pipeline::is_fatal_ownership_kind(&e.kind))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if !fatal_ownership.is_empty() {
        match output {
            OutputMode::Text => {
                for err in &fatal_ownership {
                    eprintln!(
                        "error[ownership]: {}:{}:{}: {}",
                        filename, err.span.line, err.span.column, err.message
                    );
                    if let Some(sug) = &err.suggestion {
                        eprintln!("  help: {sug}");
                    }
                }
            }
            OutputMode::Json => emit_json_output(&pipeline),
            OutputMode::Jsonl => {
                for err in &fatal_ownership {
                    emit_jsonl_event(
                        "diagnostic",
                        &format!(
                            "\"severity\":\"error\",\"phase\":\"ownership\",{},\"message\":{}",
                            span_to_json(&err.span, filename),
                            json_string(&err.message),
                        ),
                    );
                }
            }
        }
        process::exit(1);
    }

    // Run
    let mut interp = Interpreter::new(&pipeline.parsed.program, pipeline.typed.as_ref().unwrap());
    interp.set_source_filename(filename);
    interp.set_program_args(filename, program_args);
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
        // B-2026-08-23-21: render through the value's Kāra `Display` impl, the
        // same renderer the compiled backends use (they compile
        // `f"Error: {e}\n"`, which dispatches to the user's `to_string`).
        //
        // This used to interpolate the `Value` directly — `format!("{e}")` —
        // which is Rust's `Display for Value`, a DIFFERENT renderer: it ignores
        // the user impl and walks a `HashMap` for a struct's fields, so `run`
        // and `build` printed different strings AND `run` printed a different
        // field order on every execution. The typechecker already guarantees
        // there is an impl to call: `E_MAIN_ERR_NOT_DISPLAY` rejects a
        // `main() -> Result[(), E]` whose `E` has none, telling the user the
        // runtime "prints a returned `Err(e)` as `Error: {e}` using its
        // `Display` impl" — which is precisely what this now does.
        let rendered = interp.render_value_via_display(e);
        match output {
            OutputMode::Text => eprintln!("Error: {rendered}"),
            OutputMode::Json => {
                println!("{{\"error\":{}}}", json_string(&rendered));
            }
            OutputMode::Jsonl => {
                emit_jsonl_event("error", &format!("\"message\":{}", json_string(&rendered)));
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

    // Emit error return trace if present. The `?`-propagation ring buffer is a
    // debug diagnostic written to STDERR (never stdout): it records the last
    // error's propagation path and is cleared on a subsequent successful `?`
    // (Ok/Some). It intentionally prints for a HANDLED error too — the codegen
    // path and `tests/codegen.rs::test_e2e_question_trace_single_frame_on_err`
    // (a `?`-error caught by a `match`) codify that, and it stays on stderr so it
    // never pollutes program output.
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

    // A faulting program exits nonzero (previously always 0 — scripts couldn't
    // detect interpreter-level failures). Gated on `runtime_errors` so a clean
    // run still exits 0. Faults take precedence over an `ExitCode` return — a
    // runtime error unwinds before `main` produces a clean value, so
    // `main_result` is `Unit`, not the intended code, in that case anyway.
    //
    // The two nonzero codes are NOT interchangeable (design.md § Entry Point,
    // B-2026-08-23-17). A PANIC — which is what every interpreter runtime error
    // is: `unwrap()` on `None`, an out-of-bounds index, an overflowing add —
    // exits `101`; a `main() -> Result` that returned `Err(e)` exits `1` (the
    // `Error:` line was already printed above). The spec's whole reason for two
    // codes is that a shell pipeline can tell an expected failure from a bug,
    // so collapsing both onto 1 (as this did) erases the distinction. The AOT
    // and JIT backends exit 101 from `emit_panic` for the same reason.
    if !runtime_errors.is_empty() {
        process::exit(101);
    }
    if main_err_exit {
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

/// `karac check` with no file argument: check the whole package in the current
/// directory, the analysis twin of bare `karac build` (B-2026-08-20-16).
///
/// The command used to answer `error: missing file argument`, which left a user
/// who had just been told a single-file check is unreliable on a package member
/// with nowhere to go. `--concurrency-report` / `--simd-report` are accepted so
/// the flag surface matches `karac check <file>`, and noted as not yet wired for
/// project mode rather than silently ignored.
pub(super) fn cmd_check_project(
    output: OutputMode,
    concurrency_report: bool,
    simd_report: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    target: Option<&str>,
    targets: Option<&[String]>,
    platforms: Option<&[crate::walker::Platform]>,
) {
    // `--target=<v1 name>`, the singular spelling, means here what it means to
    // `karac build`: check for THIS target. It has to be applied before the
    // walk, because the walk's platform-suffix filter is derived from it
    // (`--target=wasm_*` selects `_wasm` files) — design.md § Platform-specific
    // modules > Target selection, B-2026-08-20-25.
    let build_target = resolve_build_target(target);
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
    if concurrency_report || simd_report {
        eprintln!(
            "note: --concurrency-report / --simd-report read a single file's late-phase \
             analysis; project-mode reporting is a follow-up. Point them at one file, or \
             use `karac build --concurrency-report`."
        );
    }
    let mut lints = lint_overrides;
    lints.apply_manifest_lints(&mf.lints);

    // `--platform=` names design.md's third axis directly instead of taking
    // what the compilation target derives — which is the host for everything
    // but `wasm_*`, and is therefore the reason the other half of a platform
    // split cannot be checked from one machine (B-2026-08-20-29). More than
    // one platform is a SWEEP: each is checked in full, under its own header,
    // and any failure fails the command.
    if let Some(platforms) = platforms {
        if platforms.len() > 1 {
            check_platform_sweep(&root, &lints, output, platforms);
            return;
        }
    }

    // The compilation-target axis, swept the same way. `--targets=` names it
    // explicitly; absent that, a package's declared `[build].targets` does —
    // which project mode used to parse and then quietly check once under the
    // default target (B-2026-08-20-29).
    let declared: Vec<String> = mf.build_targets.clone();
    let target_sweep: Option<&[String]> = match (targets, target.is_some()) {
        (Some(list), _) if list.len() > 1 => Some(list),
        // An explicit singular `--target=` is a request for ONE target and
        // outranks the manifest's list.
        (None, false) if declared.len() > 1 => Some(&declared),
        _ => None,
    };
    if let Some(list) = target_sweep {
        check_target_sweep(&root, &lints, output, list, platforms);
        return;
    }

    let platform = platforms
        .and_then(|p| p.first().copied())
        .unwrap_or_else(|| super::build_cmds::walk_platform_for_target(build_target));
    let pc = match super::build_cmds::package_check_on(&root, &lints, platform) {
        Ok(pc) => pc,
        Err(e) => {
            emit_build_error(&e, output);
            process::exit(1);
        }
    };
    let errors = pc.error_count();
    match output {
        OutputMode::Json => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            println!("{{\"diagnostics\":[{}]}}", diags.join(","));
        }
        OutputMode::Jsonl => {
            for e in super::build_cmds::parse_errors_jsonl(&pc.parse_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::cycles_jsonl(&pc.cycles, &pc.tree) {
                println!("{e}");
            }
            for e in super::build_cmds::resolve_errors_jsonl(&pc.resolve_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::type_errors_jsonl(&pc.type_errors) {
                println!("{e}");
            }
        }
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            if errors == 0 {
                println!("All checks passed.");
            } else {
                println!("\n{errors} error(s) found.");
            }
        }
    }
    if errors > 0 {
        process::exit(1);
    }
}

/// `karac check --platform=all` (or any multi-platform list): check the package
/// once per platform, each under its own header, and fail if any platform does.
///
/// This is the coverage guarantee design.md's *Missing-platform rule* used to
/// promise and no command provided — the text was corrected when that was
/// measured (B-2026-08-20-25) and this is the feature (B-2026-08-20-29). Before
/// it, a `_macos` module could not be type-checked from a Linux box at all, so a
/// break in it stayed invisible until a CI matrix reached a mac.
///
/// Diagnostics are NOT deduplicated across platforms, unlike the per-file target
/// matrix. Platforms share every file that carries no suffix, so an error in one
/// of those does repeat — but a platform sweep is asked in order to learn WHICH
/// platform is broken, and a per-platform block answers that directly. The
/// target matrix dedupes because its runs differ only in a handful of
/// `#[target]`-gated items; the platform runs differ by whole files.
fn check_platform_sweep(
    root: &std::path::Path,
    lints: &crate::lints::CliLintOverrides,
    output: OutputMode,
    platforms: &[crate::walker::Platform],
) {
    let mut failed: Vec<&'static str> = Vec::new();
    for (i, platform) in platforms.iter().enumerate() {
        let name = platform.as_suffix();
        match output {
            OutputMode::Jsonl => emit_jsonl_event(
                "platform_start",
                &format!("\"platform\":{}", json_string(name)),
            ),
            OutputMode::Json => {}
            OutputMode::Text => {
                if i > 0 {
                    println!();
                }
                println!("── platform: {name} ──");
            }
        }
        let pc = match super::build_cmds::package_check_on(root, lints, *platform) {
            Ok(pc) => pc,
            Err(e) => {
                // A walk/tree failure is per-platform too: a package whose only
                // entry file carries a suffix has no entry on other platforms.
                emit_build_error(&e, output);
                failed.push(name);
                continue;
            }
        };
        let errors = pc.error_count();
        render_package_check(&pc, output, errors, Some(name));
        if errors > 0 {
            failed.push(name);
        }
        if let OutputMode::Jsonl = output {
            emit_jsonl_event(
                "platform_complete",
                &format!(
                    "\"platform\":{},\"success\":{},\"total_errors\":{}",
                    json_string(name),
                    errors == 0,
                    errors,
                ),
            );
        }
    }
    if !failed.is_empty() {
        if let OutputMode::Text = output {
            println!(
                "\n{} of {} platform(s) failed: {}",
                failed.len(),
                platforms.len(),
                failed.join(", ")
            );
        }
        process::exit(1);
    }
    if let OutputMode::Text = output {
        println!("\nAll {} platform(s) checked clean.", platforms.len());
    }
}

/// `karac check --targets=<a,b>` in project mode, or a package whose manifest
/// declares more than one `[build].targets` entry: check the package once per
/// v1 compilation target.
///
/// The platform axis rides along — each target derives its own OS platform
/// unless `--platform=` named one — so `--targets=native,wasm_wasi` checks the
/// host's files under `native` and the `_wasm` files under `wasm_wasi`, which
/// is the pair a package with a platform split most wants verified.
fn check_target_sweep(
    root: &std::path::Path,
    lints: &crate::lints::CliLintOverrides,
    output: OutputMode,
    targets: &[String],
    platforms: Option<&[crate::walker::Platform]>,
) {
    let mut failed: Vec<String> = Vec::new();
    for (i, target) in targets.iter().enumerate() {
        if let Err(e) = crate::target::set_active_target(target) {
            eprintln!("error: {e}");
            process::exit(1);
        }
        match output {
            OutputMode::Jsonl => emit_jsonl_event(
                "target_start",
                &format!("\"target\":{}", json_string(target)),
            ),
            OutputMode::Json => {}
            OutputMode::Text => {
                if i > 0 {
                    println!();
                }
                println!("── target: {target} ──");
            }
        }
        let platform = platforms
            .and_then(|p| p.first().copied())
            .unwrap_or_else(|| super::build_cmds::walk_platform_for_target(target));
        let errors = match super::build_cmds::package_check_on(root, lints, platform) {
            Ok(pc) => {
                let errors = pc.error_count();
                render_package_check(&pc, output, errors, None);
                errors
            }
            Err(e) => {
                emit_build_error(&e, output);
                1
            }
        };
        if errors > 0 {
            failed.push(target.clone());
        }
        if let OutputMode::Jsonl = output {
            emit_jsonl_event(
                "target_complete",
                &format!(
                    "\"target\":{},\"success\":{},\"total_errors\":{}",
                    json_string(target),
                    errors == 0,
                    errors,
                ),
            );
        }
    }
    if !failed.is_empty() {
        if let OutputMode::Text = output {
            println!(
                "\n{} of {} target(s) failed: {}",
                failed.len(),
                targets.len(),
                failed.join(", ")
            );
        }
        process::exit(1);
    }
    if let OutputMode::Text = output {
        println!("\nAll {} target(s) checked clean.", targets.len());
    }
}

/// Render one [`PackageCheck`] in the requested output mode. Split out of
/// `cmd_check_project` so the platform sweep renders each run identically to a
/// single check — `platform` is `Some` only inside a sweep, where the text mode
/// has already printed the header and must not repeat the summary line.
fn render_package_check(
    pc: &super::build_cmds::PackageCheck,
    output: OutputMode,
    errors: usize,
    platform: Option<&str>,
) {
    match output {
        OutputMode::Json => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            match platform {
                Some(name) => println!(
                    "{{\"platform\":{},\"diagnostics\":[{}]}}",
                    json_string(name),
                    diags.join(",")
                ),
                None => println!("{{\"diagnostics\":[{}]}}", diags.join(",")),
            }
        }
        OutputMode::Jsonl => {
            for e in super::build_cmds::parse_errors_jsonl(&pc.parse_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::cycles_jsonl(&pc.cycles, &pc.tree) {
                println!("{e}");
            }
            for e in super::build_cmds::resolve_errors_jsonl(&pc.resolve_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::type_errors_jsonl(&pc.type_errors) {
                println!("{e}");
            }
        }
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            if errors == 0 {
                println!("All checks passed.");
            } else {
                println!("\n{errors} error(s) found.");
            }
        }
    }
}

/// `karac check <file>` where `<file>` is a member of a `kara.toml` package.
///
/// Runs the WHOLE package through the module-aware pipeline (the same one
/// `karac build` runs) and then renders only the requested file's diagnostics,
/// with a one-line pointer when sibling modules carry errors of their own.
///
/// The alternative — refusing the invocation the way single-file `karac build`
/// does — was rejected: `karac check <file>` is what editors, the Mend loop and
/// every `karac fix` cycle call, and answering "run it differently" to all of
/// them trades an incorrect answer for no answer. Widening the ANALYSIS while
/// keeping the REPORT scoped to the file the caller named gives the correct
/// answer to the question actually asked.
fn cmd_check_package_member(
    filename: &str,
    root: &std::path::Path,
    output: OutputMode,
    lint_overrides: &crate::lints::CliLintOverrides,
    platform: Option<crate::walker::Platform>,
) -> bool {
    let mut lints = lint_overrides.clone();
    if let Ok(mf) = manifest::load_from_root(root) {
        lints.apply_manifest_lints(&mf.lints);
    }
    // `--platform=` overrides what the compilation target derives, so a
    // `_macos` member can be checked from any host (B-2026-08-20-29).
    let platform = platform.unwrap_or_else(|| {
        super::build_cmds::walk_platform_for_target(crate::target::active_target())
    });
    let mut pc = match super::build_cmds::package_check_on(root, &lints, platform) {
        Ok(pc) => pc,
        Err(e) => {
            // No package VIEW could be formed at all — most often a directory
            // holding a `kara.toml` and modules but no `src/main.kara` /
            // `src/lib.kara` entry. Several `examples/` packages are shaped
            // that way, and `karac build` refuses them for the same reason.
            //
            // Fall back to the single-file check rather than failing: the
            // caller asked about one file, and "this package has no entry
            // point" is not an answer about that file. The single-file view is
            // the blind one this function exists to replace, so say so once —
            // narrowing silently would be the original bug wearing a new hat.
            if output == OutputMode::Text {
                eprintln!(
                    "note: checking `{filename}` on its own — the package at `{}` could \
                     not be assembled ({e}), so imports of sibling modules are not \
                     resolved here",
                    root.display(),
                );
            }
            return false;
        }
    };
    let had_errors = pc.has_errors();
    let elsewhere = pc.restrict_to_file(std::path::Path::new(filename));

    match output {
        OutputMode::Json => {
            let mut diags: Vec<String> = Vec::new();
            diags.extend(super::build_cmds::parse_errors_json(&pc.parse_errors));
            diags.extend(super::build_cmds::type_warnings_json(&pc.type_warnings));
            diags.extend(super::build_cmds::cycles_json(&pc.cycles, &pc.tree));
            diags.extend(super::build_cmds::resolve_errors_json(&pc.resolve_errors));
            diags.extend(super::build_cmds::type_errors_json(&pc.type_errors));
            println!("{{\"diagnostics\":[{}]}}", diags.join(","));
        }
        OutputMode::Jsonl => {
            for e in super::build_cmds::parse_errors_jsonl(&pc.parse_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::cycles_jsonl(&pc.cycles, &pc.tree) {
                println!("{e}");
            }
            for e in super::build_cmds::resolve_errors_jsonl(&pc.resolve_errors) {
                println!("{e}");
            }
            for e in super::build_cmds::type_errors_jsonl(&pc.type_errors) {
                println!("{e}");
            }
        }
        OutputMode::Text => {
            super::build_cmds::print_type_warnings_text(&pc.type_warnings);
            super::build_cmds::print_parse_errors_text(&pc.parse_errors);
            super::build_cmds::print_cycles_text(&pc.cycles, &pc.tree);
            super::build_cmds::print_resolve_errors_text(&pc.resolve_errors);
            super::build_cmds::print_type_errors_text(&pc.type_errors);
            let shown = pc.error_count();
            if shown == 0 && elsewhere == 0 {
                println!("All checks passed.");
            } else if shown > 0 {
                println!("\n{shown} error(s) found.");
            }
            // Never let a sibling module's real error vanish just because the
            // caller named one file: say it exists and where to see it.
            if elsewhere > 0 {
                eprintln!(
                    "note: {elsewhere} further error(s) in other modules of the package at \
                     `{}` — run `cd {} && karac build` to see them",
                    root.display(),
                    root.display(),
                );
            }
        }
    }
    if had_errors {
        process::exit(1);
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_check(
    filename: &str,
    output: OutputMode,
    profiles: Option<Vec<crate::manifest::CompileProfile>>,
    targets: Option<Vec<String>>,
    platforms: Option<Vec<crate::walker::Platform>>,
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
    // A platform SWEEP reports per platform, and a file-mode check reports one
    // file — which may not even exist on every platform in the list. The sweep
    // is a project-mode question, so say so rather than inventing a per-file
    // rendering for it (B-2026-08-20-29).
    let platform = match platforms.as_deref() {
        Some([]) | None => None,
        Some([one]) => Some(*one),
        Some(many) => {
            eprintln!(
                "error: --platform with {} platforms is a package-wide sweep; drop the \
                 file argument (`karac check --platform=all`) to run it",
                many.len()
            );
            process::exit(1);
        }
    };

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
    // Neither asked for nor declared: cover whatever the SOURCE ITSELF
    // gates. A `#[target(T)]` item is deleted before resolution under any
    // other target, so a single-target check never reads its body —
    // "All checks passed" on a fn with two plain type errors in it
    // (B-2026-08-05-29). A `#[target]` annotation names the target the
    // body is written for, so checking it there is what the annotation
    // asks for; files with no gated items keep the single-pass default
    // and pay nothing.
    let targets = targets.or_else(|| {
        let extra = gated_check_targets(&source);
        if extra.is_empty() {
            return None;
        }
        let mut list = vec![crate::target::active_target().to_string()];
        list.extend(extra.into_iter().map(str::to_string));
        Some(list)
    });
    if let Some(list) = targets {
        cmd_check_targets(filename, &source, output, &list, lint_overrides);
        return;
    }

    // A file inside a package's `src/` is not a program on its own: its
    // `import`s name sibling modules that a single-file check never loads, so
    // the pipeline below would answer about a program the user did not write
    // and `karac build` will not build (B-2026-08-20-16). Route it through the
    // same module-aware walk → resolve → typecheck the build runs.
    //
    // Placed AFTER the `--profiles` / `--targets` dispatches on purpose: those
    // are matrices over one file's pipeline, and a package member under them
    // keeps the single-file behaviour rather than gaining a half-defined
    // product of two matrices. The single-pass path below is the one the Mend
    // loop and every plain `karac check <file>` take, and the one that was
    // reporting invented type errors.
    if let Some(root) = package_root_of_member(filename) {
        if cmd_check_package_member(filename, &root, output, &lint_overrides, platform) {
            return;
        }
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
                    // Before the verdict, not after: "All checks passed" is
                    // exactly the line the note qualifies (B-2026-08-05-29).
                    if let Some(note) = target_skip_note(&pipeline) {
                        eprintln!("\n{note}");
                    }
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
pub(super) fn emit_concurrency_report(pipeline: &Pipeline) {
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
pub(super) fn emit_simd_report(pipeline: &Pipeline) {
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
pub(super) fn cmd_check_profiles(
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

/// Source-side trigger for multi-target check: the targets named by
/// `#[target(...)]` attributes in `source` that a native check would not
/// reach. See `target::extra_check_targets_for` for why a gated body is
/// otherwise never examined (B-2026-08-05-29).
///
/// Parses on its own rather than reusing the pipeline's parse, because
/// the target list has to be known *before* the first pipeline is built.
/// A source that does not parse yields no extra targets and falls through
/// to the single-pass path, which reports the parse errors — discovery
/// must never be the thing that reports them.
///
/// Keyed on the ACTIVE target rather than on `native`: the non-llvm
/// `karac build --target=T` fallback reaches `cmd_check` with T already
/// active, and there the user asked for exactly T — a discovery that
/// hardcoded `native` would silently widen that one-target request into
/// a matrix.
///
/// SCOPE: the entry file's own top-level items. A `#[target]` inside an
/// IMPORTED module is not discovered here, so a gated body in a
/// dependency module still needs an explicit `--targets`. Recorded on
/// B-2026-08-05-29 rather than silently narrowed.
pub(super) fn gated_check_targets(source: &str) -> Vec<&'static str> {
    let parsed = crate::parse(source);
    if !parsed.errors.is_empty() {
        return Vec::new();
    }
    crate::target::extra_check_targets_for(&parsed.program.items, crate::target::active_target())
}

/// Manifest-side trigger for multi-target check: walk upward from the
/// checked file's own directory (the `karac run` discovery rule —
/// the file's filesystem location is the stable identity, not the
/// cwd) and return the discovered manifest's `[build].targets`.
/// No manifest found → empty (single-file scripts outside any project
/// keep the single-pass default). A malformed manifest is a hard error
/// — same posture as `karac run`'s discovery.
pub(super) fn manifest_build_targets_for(filename: &str, output: OutputMode) -> Vec<String> {
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
pub(super) fn read_target_cpu_env() -> Option<String> {
    std::env::var("KARAC_TARGET_CPU")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Read the `KARAC_TARGET_FEATURES` env var — the middle tier of the
/// `--target-features` precedence chain (resolved independently of the
/// CPU chain). Same empty-means-unset contract as `read_target_cpu_env`.
#[cfg(feature = "llvm")]
pub(super) fn read_target_features_env() -> Option<String> {
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
pub(super) fn manifest_release_field_for(
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
pub(super) fn manifest_wasm_knobs_for(
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

/// Effective panic strategy for a single-file build, via the same lazy
/// manifest walk-up as [`manifest_release_field_for`] (discover the manifest
/// from the file's own directory; no manifest → the built-in default). Returns
/// [`crate::manifest::ProfileConfig::panic_strategy`] — the `[profile] panic`
/// selection or the profile default (abort at v1). phase-8
/// `panic = "unwind" | "abort"` slice 2: the codegen build path gates on this
/// via [`reject_unsupported_panic_strategy`]. `llvm`-gated — the only consumer
/// is codegen (`karac run` has no abort/unwind distinction).
#[cfg(feature = "llvm")]
pub(super) fn manifest_panic_strategy_for(
    filename: &str,
    output: OutputMode,
) -> crate::manifest::PanicStrategy {
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
            Ok(m) => m.profile_config.panic_strategy(),
            Err(e) => {
                emit_manifest_error(&e, output);
                process::exit(1);
            }
        },
        None => crate::manifest::ProfileConfig::default().panic_strategy(),
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
pub(super) fn manifest_link_config_for(
    filename: &str,
    output: OutputMode,
) -> (Vec<String>, Vec<String>) {
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
pub(super) fn warn_unlowered_exports(
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
pub(super) fn emit_wasm_threads_artifact(
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

/// Resolve the arch-portable `cpu-baseline` level to its concrete native
/// `(target-cpu, target-features)` override, applying the design default `v3`
/// when no explicit level is declared. Native non-wasm only — wasm gets no CPU
/// baseline (`(None, None)`). The mapped value is the LOWEST tier of the CPU /
/// feature chains, so an explicit `--target-cpu` / `KARAC_TARGET_CPU` /
/// `[release] target-cpu` (or `[release] cpu-baseline`) always wins.
///
/// **Deploy-baseline commitment (design.md § Multiversioning > `cpu-baseline`,
/// default `"v3"`):** a default `karac build` now targets `x86-64-v3` on x86-64
/// (Haswell+, ~2013 — excludes pre-Haswell) and `+v8.4a` on aarch64 (Apple M1+,
/// Graviton 3+). This narrows the deploy set for sharper codegen — the design's
/// single-binary-distribution posture. Pre-baseline hardware opts down with
/// `[release] cpu-baseline = "v1" | "v2"` (or an explicit `--target-cpu`). The
/// codegen / asan / abi test harnesses drive `compile_to_object*` directly (not
/// this CLI path), so they stay on the per-target `generic` default and the
/// heavy CI legs are unaffected; the CLI-spawned build tests run on x86 (v3) and
/// `macos-latest` (Apple M1+, ≥ v8.4), both at or above the baseline.
#[cfg(feature = "llvm")]
pub(super) fn resolve_native_cpu_baseline(
    explicit: Option<&str>,
) -> (Option<String>, Option<String>) {
    if crate::target::active_target_is_wasm() {
        return (None, None);
    }
    let level = explicit.unwrap_or("v3");
    crate::manifest::cpu_baseline_native_override(level, std::env::consts::ARCH)
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
pub(super) fn apply_target_cpu_override(resolved: Option<String>) {
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
pub(super) fn apply_target_features_override(resolved: Option<String>) {
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
pub(super) fn apply_native_link_config(libs: Vec<String>, search_paths: Vec<String>) {
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
pub(super) fn strip_diag_id(entry: &str) -> String {
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
pub(super) fn cmd_check_targets(
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
        skipped: std::collections::HashMap<String, String>,
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
            skipped: pipeline.target_skipped.clone(),
        });
    }

    // Items NO requested target checked (B-2026-08-05-29). Reported once
    // for the whole run, not per pass — see `unchecked_across_targets`.
    let per_target: Vec<_> = runs.iter().map(|r| r.skipped.clone()).collect();
    let unchecked = unchecked_across_targets(&per_target);
    let scope = if targets.len() == 1 {
        format!("target '{}'", targets[0])
    } else {
        format!("every checked target ({})", targets.join(", "))
    };

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
            if let Some(note) = render_target_skip_note(&unchecked, &scope) {
                eprintln!();
                eprintln!("{note}");
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
                "{{\"targets\":[{}],\"shared_diagnostics\":[{}],\"target_skipped\":{},\"success\":{}}}",
                blocks.join(","),
                shared_json
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                // Peer to the single-pass field, same shape: items NO
                // requested target checked. An EMPTY array is meaningful —
                // it says the matrix covered everything.
                render_target_skipped_json(&unchecked),
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
pub(super) fn resolve_build_target(target: Option<&str>) -> &'static str {
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
pub(super) fn resolve_effective_bindings(
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
pub(super) fn validate_wasm_threads_scope(
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
pub(super) fn print_staticlib_rust_host_note(kind: NativeCrateType) {
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
pub(super) fn emit_build_error(msg: &str, output: OutputMode) {
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

/// The root of the `kara.toml` package `filename` belongs to, or `None` when
/// it is a standalone file (no package root above it, or it sits outside the
/// package's `src/`). Extracted so the two callers that care whether a file
/// has unseen sibling modules — the single-file build refusal below and the
/// resolver's `pub`-rename guard (B-2026-07-31-33) — agree on the answer.
pub(super) fn package_root_of_member(filename: &str) -> Option<PathBuf> {
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
    abs_file.starts_with(&abs_src).then_some(root)
}

/// Whether `filename` is a module of a multi-file package, and so may have
/// readers of its `pub` items that a single-file check never sees.
pub(super) fn is_package_member(filename: &str) -> bool {
    package_root_of_member(filename).is_some()
}

/// If `filename` is a source file inside a `kara.toml` package's `src/`
/// directory, return an actionable refusal message: a single-file `karac build`
/// there silently drops the package's sibling modules and produces a truncated
/// binary (B-2026-07-08-19). `None` for a standalone file — no package root, or
/// the file is not under the package's `src/` (a script that merely sits at or
/// near the package root stays buildable single-file).
pub(super) fn package_member_build_refusal(filename: &str) -> Option<String> {
    let root = package_root_of_member(filename)?;
    let root_disp = root.display();
    Some(format!(
        "`{filename}` is a source file of the package at `{root_disp}` — a \
         single-file `karac build` drops the package's sibling modules and \
         produces a truncated binary. Build the whole package instead: `cd \
         {root_disp} && karac build` (or `karac run {filename}` to run it \
         directly)."
    ))
}
