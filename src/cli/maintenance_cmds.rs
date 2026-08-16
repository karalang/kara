//! `karac init` / `clean` / `cache` — project scaffolding and cache upkeep.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 1).
//! Free functions are `pub(super)` — private plumbing of the CLI module.

use super::*;

/// Scaffold a new Kāra project (CR-36). Validates the package name, prepares
/// the target directory (creating `./<name>/` for the positional form), then
/// writes the template files via `scaffold::scaffold_project`. Every failure
/// aborts before any file is written — name validation and target-dir checks
/// run up front so a broken invocation never leaves partial state behind.
pub(super) fn cmd_init(directory: Option<String>, template: Template, force: bool) {
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

pub(super) fn cmd_clean(global: bool) {
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

pub(super) fn cmd_cache(sub: crate::cli::CacheSub, output: OutputMode) {
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

pub(super) fn cmd_cache_info(root: &std::path::Path, output: OutputMode) {
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

pub(super) fn cmd_cache_key(
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

pub(super) fn emit_cache_error(e: &crate::build_cache::CacheError, output: OutputMode) {
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
pub(super) fn dirs_kara_cache_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_err(|_| "$HOME (and $USERPROFILE) unset".to_string())?;
    Ok(PathBuf::from(home).join(".kara").join("cache"))
}
