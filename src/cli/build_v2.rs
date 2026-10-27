//! `karac build` on v2 (M3 item 4, CLI half, B-2026-10-22-14): the program is
//! checked as `karac check` checks it, built to MIR and compiled by mir-llvm
//! into a native executable. A package with several modules is built as one
//! source (project mode, src/cli/project_v2.rs), from a member file or, with
//! no file named, from the package in the current directory. A flag v2 does not
//! honour yet, or one whose feature went to the archive with the legacy
//! pipeline at M3, is refused by name.

use super::*;

/// The build flags, as `karac build` parsed them.
pub(super) struct BuildArgs<'a> {
    pub output: OutputMode,
    pub offline: bool,
    pub no_proxy: bool,
    pub concurrency_report: bool,
    pub simd_report: bool,
    pub enable_hot_swap: bool,
    pub target: Option<&'a str>,
    pub bindings: Option<BindingsMode>,
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub target_cpu: Option<&'a str>,
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    pub target_features: Option<&'a str>,
    pub wasm_threads: bool,
    pub monomorphization_budget: crate::monomorphization::MonomorphizationBudget,
    pub release: bool,
    pub crate_type: NativeCrateType,
    pub out_path: Option<&'a str>,
    pub lint_overrides: crate::lints::CliLintOverrides,
}

/// `karac build <file>` on v2.
pub(super) fn cmd_build_v2_file(filename: &str, a: BuildArgs) {
    refuse_unsupported(&a);
    set_cross_triple(a.target);
    let output = a.output;
    let manifest = run_manifest_preamble(filename, output, None, false);
    refuse_hot_swap(&a, manifest.as_ref().map(|m| m.profile));
    #[cfg(feature = "llvm")]
    if set_wasm_target(&a) {
        list_cpus_if_asked(&a);
    } else {
        apply_cpu(&a, |pick| {
            manifest_release_field_for(filename, output, pick)
        });
    }
    // The `[link]` table's libraries, for the program's `extern "C"`
    // functions (B-2026-10-22-10).
    super::args::install_manifest_link(filename);
    let project = super::run_v2::package_source(filename, false, output, "build");
    let stem = std::path::Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("main");
    let exe = a
        .out_path
        .map_or_else(|| format!("{stem}{}", exe_suffix(&a)), str::to_string);
    build(filename, project, manifest.as_ref(), a, &exe, None);
}

/// `karac build` with no file on v2: the package in the current directory,
/// from its `src/main.kara`, into `<root>/<package name>` (or `-o`).
pub(super) fn cmd_build_v2_project(a: BuildArgs) {
    refuse_unsupported(&a);
    // The CPU listing needs no package.
    #[cfg(feature = "llvm")]
    {
        set_wasm_target(&a);
        list_cpus_if_asked(&a);
    }
    let output = a.output;
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        fail_with(
            &format!("error: cannot read current directory: {e}"),
            output,
        );
        process::exit(1);
    });
    let (root, raw) = manifest::load_from_cwd(&cwd).unwrap_or_else(|e| {
        emit_manifest_error(&e, output);
        process::exit(1);
    });
    if !super::build_cmds::enforce_toolchain_pin(&root, output) {
        process::exit(1);
    }
    // The target: `--target`, else `[build].target`, else the host. A
    // triple selects the `[target.<triple>.*]` overlay and, when it is not
    // the host's, is what the build emits and links for (B-2026-10-27-4).
    let target = a.target.or(raw.build_default_target.as_deref());
    let overlay = match target {
        _ if is_wasm_wasi(&a) => "wasm32-wasip1".to_string(),
        Some(t) if !crate::target::is_v1_target_name(t) => t.to_string(),
        _ => crate::build_cache::host_target_triple(),
    };
    set_cross_triple(Some(overlay.as_str()));
    let mf = manifest::merge_target_overlay(&raw, Some(overlay.as_str()));
    refuse_hot_swap(&a, Some(mf.profile));
    if output == OutputMode::Text {
        for w in &mf.warnings {
            eprintln!("warning[manifest]: {}", w.message);
        }
    }
    #[cfg(feature = "llvm")]
    if !set_wasm_target(&a) {
        apply_cpu(&a, |pick| pick(&mf));
    }
    if !mf.link_libs.is_empty() || !mf.link_search_paths.is_empty() {
        crate::target::set_native_link_config(mf.link_libs.clone(), mf.link_search_paths.clone());
    }
    // The dependencies, fetched and pinned in kara.lock as the legacy build
    // does: `--offline` reads them from `vendor/`, `--no-proxy` fetches a
    // registry dependency from its registry directly.
    if !a.offline {
        emit_no_proxy_note(a.no_proxy);
    }
    let has_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let vendor = root.join("vendor");
    if a.offline && has_deps && !vendor.is_dir() {
        emit_offline_no_vendor_dir(&vendor, output);
        process::exit(1);
    }
    let resolution = if has_deps {
        let offline_root = a.offline.then_some(vendor.as_path());
        run_dep_resolution(
            &root,
            mf.clone(),
            output,
            offline_root,
            false,
            a.no_proxy,
            true,
        )
        .unwrap_or_else(|()| process::exit(1))
    } else {
        None
    };
    // The walk follows the build target: a wasm build takes the `_wasm`
    // platform modules.
    let walk = WalkerOpts {
        target: walk_platform_for_target(target.unwrap_or("native")),
        ..WalkerOpts::default()
    };
    let walked = walker::walk_project(&root, walk).unwrap_or_else(|e| {
        emit_walker_error(&e, output);
        process::exit(1);
    });
    let deps = dep_package_walks(resolution.as_ref(), walk.target, output)
        .unwrap_or_else(|()| process::exit(1));
    // The package-wide check walks these too, the fetched ones included.
    install_resolved_dep_walks(deps.clone());
    let tree =
        module::build_program_tree_with_deps(&walked, &deps, module::BuildTreeOpts::default())
            .unwrap_or_else(|e| {
                emit_build_tree_error(&e, output);
                process::exit(1);
            });
    // A missing `src/`, an empty one, or one with no entry file was refused
    // by the walk and the tree; a library package is the gap left.
    let entry = root.join("src").join("main.kara");
    if walked.entry != walker::EntryKind::Bin {
        fail_with(
            &format!(
                "error: the v2 build makes an executable from src/main.kara, and {} has none; \
                 v2 does not build a library package yet",
                root.display()
            ),
            output,
        );
        process::exit(1);
    }
    let summary = ProjectSummary {
        name: &mf.name,
        edition: &mf.edition,
        root: &root,
        walked: &walked,
        platform: walk.target,
        deps: &deps,
    };
    if output == OutputMode::Text {
        summary.print();
    }
    let filename = super::project_v2::shown(&entry);
    // One module is a lone file; anything else (a parse error included,
    // which the package check reports) is the package.
    let project = (!tree.parse_errors.is_empty()
        || tree.tree.modules.iter().filter(|m| !m.is_synthetic).count() > 1)
        .then(|| super::run_v2::combine(&root, &tree.tree, output, "build"));
    if project.is_none() {
        super::run_v2::refuse_lone_package_imports(&root, output);
    }
    let exe = a.out_path.map_or_else(
        || {
            let name = format!("{}{}", mf.name, exe_suffix(&a));
            root.join(name).display().to_string()
        },
        str::to_string,
    );
    let fields = (output == OutputMode::Json).then(|| summary.json_fields());
    build(&filename, project, Some(&mf), a, &exe, fields.as_deref());
}

/// `--target=wasm_wasi`: a plain wasm32-wasip1 module, linked against
/// `libkarac_runtime_wasm.a` (M3 item 4, B-2026-10-22-23). The component
/// model (`--bindings`) and `wasm_browser` are not on v2 yet.
fn is_wasm_wasi(a: &BuildArgs) -> bool {
    a.target == Some("wasm_wasi")
}

/// The output's extension when `-o` does not name it.
fn exe_suffix(a: &BuildArgs) -> &'static str {
    let windows = match crate::target::cross_triple() {
        Some(t) => t.contains("-windows"),
        None => cfg!(windows),
    };
    if is_wasm_wasi(a) {
        ".wasm"
    } else if windows {
        ".exe"
    } else {
        ""
    }
}

/// Makes `target`, when it is a triple other than the host's, the triple
/// the build emits and links for.
fn set_cross_triple(target: Option<&str>) {
    if let Some(t) = target.filter(|t| !crate::target::is_v1_target_name(t) && *t != host_triple())
    {
        crate::target::set_cross_triple(t);
    }
}

fn host_triple() -> String {
    crate::build_cache::host_target_triple()
}

/// Makes wasm32-wasip1 the active target for `--target=wasm_wasi`; true
/// when it did (the native CPU baseline then does not apply).
#[cfg(feature = "llvm")]
fn set_wasm_target(a: &BuildArgs) -> bool {
    if !is_wasm_wasi(a) {
        return false;
    }
    if let Err(e) = crate::target::set_active_target("wasm_wasi") {
        fail_with(&e, a.output);
        process::exit(1);
    }
    true
}

/// `--enable-hot-swap` (deferred.md § Continuous PGO with Shared-Object
/// Hot-Swap) needs dynamic symbol resolution at run time, so it is refused
/// on a wasm target and on a `[package].profile` that has none.
fn refuse_hot_swap(a: &BuildArgs, profile: Option<manifest::CompileProfile>) {
    if !a.enable_hot_swap {
        return;
    }
    let msg = match profile {
        _ if is_wasm_wasi(a) || a.target.is_some_and(|t| t.starts_with("wasm32-")) => format!(
            "error: --enable-hot-swap is incompatible with --target={} \
             (no dynamic-symbol-resolution machinery on wasm hosts)",
            a.target.unwrap_or("wasm_wasi")
        ),
        Some(p @ (manifest::CompileProfile::Embedded | manifest::CompileProfile::Kernel)) => {
            format!(
                "error: --enable-hot-swap is incompatible with [package].profile = \"{}\" \
                 (no dynamic-symbol-resolution machinery on this profile)",
                p.as_str()
            )
        }
        _ => return,
    };
    fail_with(&msg, a.output);
    process::exit(1);
}

/// The bodies `--enable-hot-swap` calls through `karac_hotswap_table`: the
/// program's own non-generic `pub fn`s at module scope (a generic one has
/// no single symbol to swap), `main` aside.
#[cfg(feature = "llvm")]
fn hot_swap_names(source: &str) -> Vec<String> {
    crate::parse(source)
        .program
        .items
        .iter()
        .filter_map(|item| match item {
            crate::ast::Item::Function(f)
                if f.is_pub
                    && f.generic_params.is_none()
                    && f.self_param.is_none()
                    && f.name != "main" =>
            {
                Some(f.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// A flag the v2 build does not honour yet: refused by name, exit 1.
fn refuse_unsupported(a: &BuildArgs) {
    // A triple is built for (a cross one included); of the v1 names,
    // `native` and `wasm_wasi` are.
    let target = a
        .target
        .filter(|t| *t != "native" && *t != "wasm_wasi" && crate::target::is_v1_target_name(t));
    // B-2026-10-22-21: a feature that went to the archive with the legacy
    // backend is refused by name rather than as an unknown flag.
    let archived_target = match target {
        Some("wasm_browser") => Some("the browser wasm bindings"),
        Some("gpu") => Some("GPU compute"),
        _ => None,
    };
    let refused: Vec<String> = [
        (a.concurrency_report, "--concurrency-report".to_string()),
        (a.simd_report, "--simd-report".to_string()),
        (
            target.is_some() && archived_target.is_none(),
            format!("--target={}", target.unwrap_or("")),
        ),
    ]
    .into_iter()
    .filter_map(|(on, flag)| on.then_some(format!("`{flag}`")))
    .collect();
    let archived: Vec<(String, &str)> = [
        (
            archived_target.is_some(),
            format!("--target={}", target.unwrap_or("")),
            archived_target.unwrap_or(""),
        ),
        (
            // `--bindings=none` asks for the raw core module, which is what
            // the v2 `wasm_wasi` build emits.
            a.bindings
                .is_some_and(|b| !(b == super::BindingsMode::None && is_wasm_wasi(a))),
            "--bindings".to_string(),
            "the wasm component bindings",
        ),
        (
            a.wasm_threads,
            "--features=wasm-threads".to_string(),
            "threaded wasm",
        ),
        (
            a.crate_type != NativeCrateType::Bin,
            "--crate-type".to_string(),
            "the exported C ABI",
        ),
    ]
    .into_iter()
    .filter_map(|(on, flag, feature)| on.then_some((format!("`{flag}`"), feature)))
    .collect();
    let mut lines = Vec::new();
    if !refused.is_empty() {
        lines.push(format!(
            "error: the v2 build does not honour {} yet",
            refused.join(", ")
        ));
    }
    for (flag, feature) in archived {
        lines.push(format!(
            "error: {flag} builds {feature}, which goes to the archive with the legacy \
             backend at M3 and returns on v2 on demand"
        ));
    }
    if !lines.is_empty() {
        fail_with(&lines.join("\n"), a.output);
        process::exit(1);
    }
}

/// `--target-cpu=help` or `--target-features=help`: the active target's
/// CPU and feature tables, then exit 0.
#[cfg(feature = "llvm")]
fn list_cpus_if_asked(a: &BuildArgs) {
    if a.target_cpu == Some("help") || a.target_features == Some("help") {
        crate::backend::driver::print_target_cpu_listing();
        process::exit(0);
    }
}

/// The CPU baseline, as `karac build` sets it: the flag, then
/// `KARAC_TARGET_CPU`, then the manifest's `[release] target-cpu`, then its
/// `cpu-baseline` (`v3` when absent); the feature string likewise.
#[cfg(feature = "llvm")]
fn apply_cpu(
    a: &BuildArgs,
    manifest_field: impl Fn(fn(&manifest::Manifest) -> Option<String>) -> Option<String>,
) {
    list_cpus_if_asked(a);
    let (baseline_cpu, baseline_features) =
        resolve_native_cpu_baseline(manifest_field(|m| m.release_cpu_baseline.clone()).as_deref());
    apply_target_cpu_override(
        a.target_cpu
            .map(str::to_string)
            .or_else(read_target_cpu_env)
            .or_else(|| manifest_field(|m| m.release_target_cpu.clone()))
            .or(baseline_cpu),
    );
    apply_target_features_override(
        a.target_features
            .map(str::to_string)
            .or_else(read_target_features_env)
            .or_else(|| manifest_field(|m| m.release_target_features.clone()))
            .or(baseline_features),
    );
}

/// What a package build reports before it builds: the package, its entry,
/// the modules the walk found (with each one's platform) and the
/// dependencies' packages.
struct ProjectSummary<'a> {
    name: &'a str,
    edition: &'a str,
    root: &'a std::path::Path,
    walked: &'a walker::WalkResult,
    platform: walker::Platform,
    deps: &'a [module::DepPackageWalk],
}

impl ProjectSummary<'_> {
    fn module_path(m: &walker::WalkedModule) -> String {
        m.path.join(".")
    }

    fn entry(&self) -> &'static str {
        match self.walked.entry {
            walker::EntryKind::Bin => "bin",
            walker::EntryKind::Lib => "lib",
            walker::EntryKind::None => "none",
        }
    }

    fn print(&self) {
        println!("project: {}", self.name);
        println!("edition: {}", self.edition);
        println!("root:    {}", self.root.display());
        println!("target:  {}", self.platform.as_suffix());
        println!("entry:   {}", self.entry());
        println!("modules: {}", self.walked.modules.len());
        for m in &self.walked.modules {
            let path = match Self::module_path(m) {
                p if p.is_empty() => "<crate root>".to_string(),
                p => p,
            };
            let plat = m
                .platform
                .map_or(String::new(), |p| format!(" [{}]", p.as_suffix()));
            println!("  {path}{plat}  {}", m.file.display());
        }
        if !self.deps.is_empty() {
            let modules: usize = self.deps.iter().map(|d| d.walked.modules.len()).sum();
            println!(
                "deps:    {} package(s), {modules} module(s)",
                self.deps.len()
            );
            for d in self.deps {
                println!("  {}  {}", d.name, d.walked.src_dir.display());
            }
        }
    }

    /// The summary's fields of the `--output=json` document.
    fn json_fields(&self) -> String {
        let modules = self
            .walked
            .modules
            .iter()
            .map(|m| {
                let role = match m.role {
                    walker::ModuleRole::Ordinary => "ordinary",
                    walker::ModuleRole::Entry => "entry",
                    walker::ModuleRole::Test => "test",
                };
                let platform = m
                    .platform
                    .map_or("null".to_string(), |p| json_string(p.as_suffix()));
                format!(
                    "{{\"path\":{},\"role\":{},\"platform\":{platform},\"file\":{}}}",
                    json_string(&Self::module_path(m)),
                    json_string(role),
                    json_string(&m.file.display().to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "\"project\":{},\"edition\":{},\"root\":{},\"target\":{},\"entry\":{},\"modules\":[{modules}],\"diagnostics\":[],",
            json_string(self.name),
            json_string(self.edition),
            json_string(&self.root.display().to_string()),
            json_string(self.platform.as_suffix()),
            json_string(self.entry()),
        )
    }
}

/// Check, build and link the program into `exe`. `KARAC_MIR_OPT=<0..3>` sets
/// the IR optimisation level (2 by default); `KARAC_MIR_LL=<path>` keeps a
/// copy of the IR. `fields` are a package build's summary, for the
/// `--output=json` document.
fn build(
    filename: &str,
    project: Option<super::project_v2::Amalgam>,
    manifest: Option<&manifest::Manifest>,
    a: BuildArgs,
    exe: &str,
    fields: Option<&str>,
) {
    let output = a.output;
    // The backend is abort-only: a panic never unwinds (B-2026-10-25-18).
    if manifest
        .is_some_and(|m| m.profile_config.panic_strategy() == manifest::PanicStrategy::Unwind)
    {
        fail_with(
            "error: `panic = \"unwind\"` is not supported: the backend is abort-only, so \
             a panic never unwinds; remove the `[profile] panic` key or set \
             `panic = \"abort\"`",
            output,
        );
        process::exit(1);
    }
    if a.release {
        // `dbg()`, contracts and the error return trace are stripped from
        // release builds (design.md § `dbg()`, § Contracts, § Error return
        // traces).
        std::env::set_var("KARAC_STRIP_DBG", "1");
        std::env::set_var("KARAC_STRIP_CONTRACTS", "1");
        std::env::set_var("KARAC_STRIP_ERROR_TRACE", "1");
    }
    let source = match &project {
        Some(p) => p.text.clone(),
        None => std::fs::read_to_string(filename).unwrap_or_else(|e| {
            fail_with(&format!("error: {filename}: {e}"), output);
            process::exit(1);
        }),
    };
    let checked = super::run_v2::check_v2(
        filename,
        &source,
        project.as_ref(),
        manifest,
        a.lint_overrides.clone(),
        output,
    );
    if a.monomorphization_budget.is_enabled() {
        enforce_monomorphization_budget(
            &checked,
            project.as_ref(),
            &a.monomorphization_budget,
            output,
        );
    }
    // The check's warnings ride the JSON success object under `diagnostics`,
    // only when there are some; a package's summary already has the key.
    let warnings: Vec<String> = match output {
        OutputMode::Text => Vec::new(),
        _ => collect_diagnostics(&checked)
            .entries
            .iter()
            .map(|d| match &project {
                Some(p) => p.remap_json_text(filename, d),
                None => d.clone(),
            })
            .collect(),
    };
    let fields: Option<String> = match fields {
        Some(f) => Some(f.replace(
            "\"diagnostics\":[],",
            &format!("\"diagnostics\":[{}],", warnings.join(",")),
        )),
        None if !warnings.is_empty() => Some(format!("\"diagnostics\":[{}],", warnings.join(","))),
        None => None,
    };
    let fields = fields.as_deref();
    crate::mir::interp::set_source_name(filename);
    if let Some(p) = &project {
        p.install();
    }
    #[cfg(feature = "llvm")]
    {
        let fail = |msg: String| -> ! {
            fail_with(&msg, output);
            process::exit(1);
        };
        let opt = match std::env::var("KARAC_MIR_OPT") {
            Ok(v) => v
                .parse::<u8>()
                .ok()
                .filter(|n| *n <= 3)
                .unwrap_or_else(|| fail(format!("error: KARAC_MIR_OPT={v}: expected 0 to 3"))),
            Err(_) => 2,
        };
        let stem = std::path::Path::new(exe)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("main");
        let obj = std::env::temp_dir().join(format!("karac_mir_{}_{stem}.o", process::id()));
        let keep_ll = std::env::var_os("KARAC_MIR_LL").map(std::path::PathBuf::from);
        let lowered = crate::mir::lower::build_source(&source)
            .and_then(|l| {
                // Conflicting `par` branches and invalid MIR are refused,
                // as on every MIR backend.
                crate::mir::lower::compiled_check(&l)?;
                Ok(l)
            })
            .unwrap_or_else(|e| super::run_v2::refused(&e, output));
        let hot_swap = if a.enable_hot_swap {
            hot_swap_names(&source)
        } else {
            Vec::new()
        };
        let built = crate::backend::mir_aot::compile_program_object_with(
            &lowered.program,
            &lowered.tys,
            &obj,
            opt,
            keep_ll.as_deref(),
            &hot_swap,
        )
        .map_err(|e| format!("llvm: {e}"))
        .and_then(|()| {
            let mut t = std::time::Instant::now();
            let r = crate::backend::driver::link_executable(&obj.to_string_lossy(), exe);
            crate::mir::lower::phase_time("link", &mut t);
            r
        });
        let _ = std::fs::remove_file(&obj);
        if let Err(e) = built {
            fail(format!("error[mir]: {e}"));
        }
        match output {
            OutputMode::Text => println!("Built: {exe}"),
            _ => println!(
                "{{\"status\":\"ok\",{}\"output\":{}}}",
                fields.unwrap_or(""),
                json_string(exe)
            ),
        }
    }
    // Without the llvm feature there is no code generator: the check above
    // is the whole build, as on the legacy path.
    // A package build's document still reports the package.
    #[cfg(not(feature = "llvm"))]
    if let Some(f) = fields {
        println!("{{\"status\":\"ok\",{}}}", f.trim_end_matches(','));
    }
    #[cfg(not(feature = "llvm"))]
    eprintln!(
        "note: karac build requires the llvm feature; {filename} type-checked but no \
         executable was produced (wanted at {exe})."
    );
}

/// A build error: the message as written in text mode (it names its own
/// `error:` prefix), the build-error object under `--output=json`/`jsonl`.
fn fail_with(msg: &str, output: OutputMode) {
    match output {
        OutputMode::Text => eprintln!("{msg}"),
        _ => emit_build_error(msg, output),
    }
}

/// `--monomorphization-budget` after the check, as legacy enforced it: each
/// generic whose instantiations meet a threshold is reported on stderr as
/// `warning[monomorphization-budget]` or `error[…]`, and an error fails the
/// build before code generation (in JSON mode with a diagnostics envelope on
/// stdout). B-2026-10-23-16.
fn enforce_monomorphization_budget(
    pipeline: &Pipeline,
    project: Option<&super::project_v2::Amalgam>,
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
    let at = |v: &BudgetViolation| {
        let place = format!("{}:{}:{}", pipeline.filename, v.site.line, v.site.column);
        match project {
            Some(a) => a.remap_text(&pipeline.filename, &place),
            None => place,
        }
    };
    for v in &violations {
        let kind = match v.level {
            BudgetLevel::Error => "error",
            BudgetLevel::Warning => "warning",
        };
        eprintln!(
            "{kind}[monomorphization-budget]: {}: generic `{}` has {} instantiations (limit {})",
            at(v),
            v.generic,
            v.count,
            v.threshold
        );
    }
    let errors: Vec<&BudgetViolation> = violations
        .iter()
        .filter(|v| v.level == BudgetLevel::Error)
        .collect();
    if errors.is_empty() {
        return;
    }
    if output != OutputMode::Text {
        let diags: Vec<String> = errors
            .iter()
            .map(|v| {
                format!(
                    "{{\"severity\":\"error\",\"phase\":\"monomorphization-budget\",\"generic\":{},\"count\":{},\"limit\":{},\"site\":{}}}",
                    json_string(&v.generic),
                    v.count,
                    v.threshold,
                    json_string(&at(v)),
                )
            })
            .collect();
        println!(
            "{{\"status\":\"error\",\"diagnostics\":[{}]}}",
            diags.join(",")
        );
    }
    process::exit(1);
}
