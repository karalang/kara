//! `karac install` / `vendor` / `update` / `resolve` — the package/registry commands.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 1).
//! Free functions are `pub(super)` — private plumbing of the CLI module.

use super::*;

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

pub(super) fn cmd_install(spec: &str) {
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
pub(super) fn install_bin_root() -> Result<PathBuf, String> {
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
pub(super) fn install_from_path(path: &std::path::Path) {
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

pub(super) fn cmd_vendor(no_proxy: bool) {
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
pub(super) fn copy_dir_recursive(
    src: &std::path::Path,
    dest: &std::path::Path,
) -> std::io::Result<()> {
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

pub(super) fn cmd_update(package: Option<&str>, output: OutputMode, no_proxy: bool) {
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
pub(super) fn validate_update_target(
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

pub(super) fn emit_update_target_error(
    output: OutputMode,
    code: &str,
    message: &str,
    help: Option<&str>,
) {
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

pub(super) fn nearest_package_name(
    target: &str,
    resolution: &crate::dep_resolver::Resolution,
) -> Option<String> {
    let names: Vec<&str> = resolution.packages.keys().map(String::as_str).collect();
    crate::edit_distance::suggest_similar(target, &names)
}

pub(super) fn emit_update_summary(
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) {
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

pub(super) fn describe_resolved_source(src: &crate::dep_resolver::ResolvedSource) -> &'static str {
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
pub(super) fn describe_resolved_source_detail(src: &crate::dep_resolver::ResolvedSource) -> String {
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
pub(super) fn cmd_resolve(output: OutputMode, offline: bool, no_proxy: bool) {
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
pub(super) fn emit_resolution_graph(
    resolution: &crate::dep_resolver::Resolution,
    output: OutputMode,
) {
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
