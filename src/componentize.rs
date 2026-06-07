//! Embedded-WIT componentization for `--bindings component` (phase-10
//! "WASM Component Model — embedded-WIT migration").
//!
//! Turns the wasm32-wasip1 C-ABI core module the wasm link step
//! produces into a single self-describing WASI 0.2 component — the
//! artifact wasmtime/jco-class hosts run directly — by shelling out to
//! the external `wasm-tools` binary:
//!
//!   1. (host fns only) `wasm-tools component embed <wit> <core>
//!      --world <w>` — bakes the [`crate::wit::render_embed_wit`] world
//!      into the module as the component-type custom section;
//!   2. `wasm-tools component new <module> --adapt
//!      wasi_snapshot_preview1=<adapter> -o <out>` — lifts the
//!      preview1-ABI module into a component. The adapter synthesizes
//!      `export wasi:cli/run` from `_start`, so the program runs as a
//!      WASI command with zero wasi WIT files vendored here.
//!
//! Karac never bakes the Component Model spec into the compiler
//! (design.md § Component Model emission): the spec-coupled transform
//! lives in `wasm-tools`, resolved from `KARAC_WASM_TOOLS` /
//! `PATH` and pinned per-project via `kara.toml` `[toolchain]
//! wasm-tools = "<version>"` (exact match against `wasm-tools
//! --version`, hard error on drift — reproducible builds). The one
//! spec-adjacent ingredient, the `wasi_snapshot_preview1` **command**
//! adapter, is pure data vendored through the
//! `wasi-preview1-component-adapter-provider` crate (wasmtime's own
//! release artifact, pinned by Cargo.lock) and written to a temp file
//! per invocation; `KARAC_WASI_ADAPTER` substitutes an on-disk adapter.
//!
//! Like `wit.rs`/`wasm_glue.rs`, this module is **inkwell-free**
//! (codegen containment — CLAUDE.md § Architecture): it consumes the
//! plain [`HostFnSig`] surface and drives child processes.

use crate::wasm_glue::HostFnSig;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

/// A resolved `wasm-tools` binary: where it lives and what
/// `--version` reported (pin-checked when the manifest carries one).
#[derive(Debug)]
pub struct WasmTools {
    pub path: PathBuf,
    pub version: String,
}

/// The escape hatches every missing-tool / pin-drift error names.
const ESCAPE_HATCHES: &str = "set KARAC_WASM_TOOLS=<path> to point at a specific binary, or build \
     with `--bindings component-paired` (deprecated paired form, no external tool) or \
     `--bindings none` (raw core module)";

/// Locate `wasm-tools` and verify any `[toolchain]` pin. Resolution
/// order mirrors `resolve_wasm_linker`'s contract:
///   1. `KARAC_WASM_TOOLS` env var — honored verbatim (still
///      version-probed, so a pin applies to it too).
///   2. `wasm-tools` on `PATH`.
///
/// `pin` is the manifest's `[toolchain] wasm-tools` value (project
/// mode; single-file builds pass the lazily-walked-up project's pin or
/// `None`). The check is **exact** string equality against the version
/// `wasm-tools --version` reports — a pin exists to make builds
/// reproducible, so "close enough" is drift.
pub fn resolve_wasm_tools(pin: Option<&str>) -> Result<WasmTools, String> {
    let candidate = match std::env::var_os("KARAC_WASM_TOOLS") {
        Some(p) => PathBuf::from(p),
        None => PathBuf::from("wasm-tools"),
    };
    let output = Command::new(&candidate).arg("--version").output();
    let version = match output {
        Ok(ref o) if o.status.success() => parse_version_line(&String::from_utf8_lossy(&o.stdout)),
        _ => {
            return Err(format!(
                "wasm-tools not found (`--bindings component` componentizes via the external \
                 `wasm-tools` binary — design.md § Component Model emission): install it with \
                 `cargo install wasm-tools` (or `brew install wasm-tools`), {ESCAPE_HATCHES}"
            ));
        }
    };
    let Some(version) = version else {
        return Err(format!(
            "could not parse `{} --version` output to a version — expected `wasm-tools <semver>` \
             on the first line; {ESCAPE_HATCHES}",
            candidate.display()
        ));
    };
    if let Some(pin) = pin {
        if pin != version {
            return Err(format!(
                "wasm-tools version mismatch: `[toolchain] wasm-tools = \"{pin}\"` is pinned in \
                 kara.toml, but `{}` reports {version} — install the pinned version \
                 (`cargo install wasm-tools --version {pin} --locked`), update the pin, or \
                 {ESCAPE_HATCHES}",
                candidate.display()
            ));
        }
    }
    Ok(WasmTools {
        path: candidate,
        version,
    })
}

/// `wasm-tools 1.251.0` (optionally with a trailing ` (<sha> ...)`) →
/// `1.251.0`. `None` when the line doesn't look like that.
fn parse_version_line(stdout: &str) -> Option<String> {
    let first = stdout.lines().next()?.trim();
    let rest = first.strip_prefix("wasm-tools")?.trim();
    let token = rest.split_whitespace().next()?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

/// Process-unique scratch directory for one componentize invocation —
/// pid + counter so parallel builds (and the parallel test harness)
/// never collide. Best-effort removed on every exit path.
fn scratch_dir() -> Result<PathBuf, String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "karac-componentize-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create scratch dir {}: {e}", dir.display()))?;
    Ok(dir)
}

/// The vendored preview1 **command** adapter, materialized to a file
/// (`wasm-tools` takes a path, not bytes). `KARAC_WASI_ADAPTER`
/// substitutes an on-disk adapter verbatim — the escape hatch for a
/// newer/custom adapter without rebuilding karac.
fn adapter_path(scratch: &Path) -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("KARAC_WASI_ADAPTER") {
        return Ok(PathBuf::from(p));
    }
    let path = scratch.join("wasi_snapshot_preview1.command.wasm");
    std::fs::write(
        &path,
        wasi_preview1_component_adapter_provider::WASI_SNAPSHOT_PREVIEW1_COMMAND_ADAPTER,
    )
    .map_err(|e| format!("failed to write wasi adapter to {}: {e}", path.display()))?;
    Ok(path)
}

/// Run one `wasm-tools` subcommand, surfacing its stderr on failure —
/// the child's diagnostics (unresolved import, malformed WIT) are the
/// actionable part of any componentize error.
fn run_wasm_tools(tool: &WasmTools, args: &[&str]) -> Result<(), String> {
    let output = Command::new(&tool.path)
        .args(args)
        .output()
        .map_err(|e| format!("failed to spawn `{}`: {e}", tool.path.display()))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "`{} {}` failed:\n{}",
        tool.path.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr).trim_end()
    ))
}

/// Lift `core_wasm` (a wasm32-wasip1 C-ABI core module) into a single
/// embedded-WIT component at `out_path`. With host fns, the core
/// module must already import them under [`crate::wit::host_import_module`]
/// / [`crate::wit::host_import_name`] naming — codegen's
/// component-bindings import swap guarantees that; the world embedded
/// here declares the matching interface so `component new` can resolve
/// the imports.
pub fn componentize(
    tool: &WasmTools,
    core_wasm: &Path,
    host_fns: &[HostFnSig],
    package: &str,
    out_path: &Path,
) -> Result<(), String> {
    let scratch = scratch_dir()?;
    let result = componentize_in(tool, core_wasm, host_fns, package, out_path, &scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

fn componentize_in(
    tool: &WasmTools,
    core_wasm: &Path,
    host_fns: &[HostFnSig],
    package: &str,
    out_path: &Path,
    scratch: &Path,
) -> Result<(), String> {
    let adapter = adapter_path(scratch)?;
    let adapt_arg = format!("wasi_snapshot_preview1={}", adapter.display());

    // Host-fn-free modules skip the embed step entirely: `component
    // new` infers an import-free world from the module itself.
    let new_input = if host_fns.is_empty() {
        core_wasm.to_path_buf()
    } else {
        let (wit, world) = crate::wit::render_embed_wit(host_fns, package);
        let wit_path = scratch.join("embed.wit");
        std::fs::write(&wit_path, wit)
            .map_err(|e| format!("failed to write {}: {e}", wit_path.display()))?;
        let embedded = scratch.join("embedded.wasm");
        run_wasm_tools(
            tool,
            &[
                "component",
                "embed",
                &wit_path.display().to_string(),
                &core_wasm.display().to_string(),
                "--world",
                &world,
                "-o",
                &embedded.display().to_string(),
            ],
        )?;
        embedded
    };

    run_wasm_tools(
        tool,
        &[
            "component",
            "new",
            &new_input.display().to_string(),
            "--adapt",
            &adapt_arg,
            "-o",
            &out_path.display().to_string(),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_line_parses_with_and_without_trailing_metadata() {
        assert_eq!(
            parse_version_line("wasm-tools 1.251.0\n").as_deref(),
            Some("1.251.0")
        );
        assert_eq!(
            parse_version_line("wasm-tools 1.251.0 (abc1234 2026-05-28)\n").as_deref(),
            Some("1.251.0")
        );
        assert_eq!(parse_version_line("not-the-tool 9.9\n"), None);
        assert_eq!(parse_version_line(""), None);
    }

    #[test]
    fn pin_mismatch_is_a_hard_error_naming_both_versions() {
        // Drive resolve_wasm_tools through a fake `wasm-tools` so the
        // test doesn't depend on (or pass because of) a host install.
        let dir = std::env::temp_dir().join(format!(
            "karac-componentize-test-{}-pin",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("wasm-tools");
        std::fs::write(&fake, "#!/bin/sh\necho 'wasm-tools 0.0.1'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // In-process env mutation is only safe because nothing else in
        // the lib-test binary reads KARAC_WASM_TOOLS — keep it that way
        // (E2E coverage of the var lives in tests/cli.rs, where it's
        // passed to a child process instead).
        std::env::set_var("KARAC_WASM_TOOLS", &fake);
        let err = resolve_wasm_tools(Some("1.251.0")).unwrap_err();
        let ok = resolve_wasm_tools(Some("0.0.1"));
        std::env::remove_var("KARAC_WASM_TOOLS");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.contains("1.251.0") && err.contains("0.0.1"), "{err}");
        assert!(err.contains("[toolchain]"), "{err}");
        let ok = ok.expect("matching pin must resolve");
        assert_eq!(ok.version, "0.0.1");
    }
}
