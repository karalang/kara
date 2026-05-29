//! Subcommand argument parsing.
//!
//! Houses `parse_args` (the top-level subcommand dispatcher),
//! `parse_<subcmd>_command` per-subcommand helpers, and
//! `parse_profiles_arg` (the build profile flag).

use crate::scaffold::Template;
use std::process;

use super::help::{has_help_flag, print_subcommand_help};
use super::{Command, OutputMode, QueryKind};

pub fn parse_args(args: &[String]) -> Command {
    if args.len() < 2 {
        return Command::Help;
    }

    let subcmd = args[1].as_str();

    // Top-level help / version short-circuit.
    match subcmd {
        "help" | "--help" | "-h" => return Command::Help,
        "version" | "--version" | "-V" => return Command::Version,
        _ => {}
    }

    // Subcommand-scoped `--help` / `-h`: print help for that subcommand and
    // exit 0 before its arg parser rejects the flag as "unknown".
    if has_help_flag(&args[2..]) {
        print_subcommand_help(subcmd);
        process::exit(0);
    }

    match subcmd {
        "run" => {
            // Check for --example NAME before the generic file-arg parser.
            if args.iter().skip(2).any(|a| a == "--example") {
                parse_run_example_command(args)
            } else {
                parse_run_command(args)
            }
        }
        "check" => parse_check_command(args),
        "build" => parse_build_command(args),
        "query" => parse_query_command(args),
        "fmt" => {
            if args.len() < 3 {
                eprintln!("error: karac fmt requires a file argument");
                process::exit(1);
            }
            Command::Fmt {
                file: args[2].clone(),
            }
        }
        "fix" => parse_fix_command(args),
        "migrate" => parse_migrate_command(args),
        "init" => parse_init_command(args),
        "test" => parse_test_command(args),
        "repl" => parse_repl_command(args),
        "doc" => Command::Doc,
        "cache" => parse_cache_command(args),
        "clean" => parse_clean_command(args),
        "install" => parse_install_command(args),
        "vendor" => parse_vendor_command(args),
        "update" => parse_update_command(args),
        "explain" => parse_explain_command(args),
        "catalog" => parse_catalog_command(args),
        // Bare file path: treat as `karac run <file>`
        other if other.ends_with(".kara") => parse_run_command_from(args, 1),
        other => {
            eprintln!("error: unknown command '{other}'");
            eprintln!("Run 'karac help' for usage.");
            process::exit(1);
        }
    }
}

/// Parser for `karac run <file> [flags]`. Tracker line 898 adds
/// `--manifest=<path>` and `--no-manifest` to the run subcommand;
/// the helper preserves the existing `--output=` / `--sequential`
/// / lint flag surface. Called from both the `"run"` arm and the
/// bare-`<file>.kara` arm.
fn parse_run_command(args: &[String]) -> Command {
    parse_run_command_from(args, 2)
}

fn parse_run_command_from(args: &[String], file_idx: usize) -> Command {
    let mut file: Option<String> = None;
    let mut output = OutputMode::Text;
    let mut sequential = false;
    let mut manifest_override: Option<String> = None;
    let mut no_manifest = false;
    let mut lint_overrides = crate::lints::CliLintOverrides::default();
    let mut i = file_idx;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--output=json" {
            output = OutputMode::Json;
        } else if arg == "--output=jsonl" {
            output = OutputMode::Jsonl;
        } else if arg == "--sequential" {
            sequential = true;
        } else if let Some(rest) = arg.strip_prefix("--manifest=") {
            if rest.trim().is_empty() {
                eprintln!("error: --manifest requires a non-empty path value");
                process::exit(1);
            }
            manifest_override = Some(rest.to_string());
        } else if arg == "--manifest" {
            if i + 1 >= args.len() {
                eprintln!("error: --manifest requires a path argument");
                process::exit(1);
            }
            let val = &args[i + 1];
            if val.trim().is_empty() {
                eprintln!("error: --manifest requires a non-empty path value");
                process::exit(1);
            }
            manifest_override = Some(val.clone());
            i += 1;
        } else if arg == "--no-manifest" {
            no_manifest = true;
        } else if arg.starts_with("--output=") {
            eprintln!(
                "error: unknown output mode '{}'. Use json or jsonl.",
                arg.strip_prefix("--output=").unwrap_or(arg)
            );
            process::exit(1);
        } else if try_consume_lint_flag(args, &mut i, &mut lint_overrides) {
            // consumed
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}'");
            process::exit(1);
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("error: unexpected argument '{arg}'");
            process::exit(1);
        }
        i += 1;
    }
    if manifest_override.is_some() && no_manifest {
        eprintln!("error: --manifest and --no-manifest are mutually exclusive");
        process::exit(1);
    }
    let Some(f) = file else {
        eprintln!("error: missing file argument");
        process::exit(1);
    };
    Command::Run {
        file: f,
        output,
        sequential,
        manifest_override,
        no_manifest,
        lint_overrides,
    }
}

fn parse_check_command(args: &[String]) -> Command {
    let mut file: Option<String> = None;
    let mut output = OutputMode::Text;
    let mut profiles: Option<Vec<crate::manifest::CompileProfile>> = None;
    let mut concurrency_report = false;
    let mut lint_overrides = crate::lints::CliLintOverrides::default();
    let mut i = 2usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--output=json" {
            output = OutputMode::Json;
        } else if arg == "--output=jsonl" {
            output = OutputMode::Jsonl;
        } else if let Some(rest) = arg.strip_prefix("--profiles=") {
            profiles = Some(parse_profiles_arg(rest));
        } else if arg == "--concurrency-report" {
            concurrency_report = true;
        } else if arg.starts_with("--output=") {
            eprintln!(
                "error: unknown output mode '{}'. Use json or jsonl.",
                arg.strip_prefix("--output=").unwrap_or(arg)
            );
            process::exit(1);
        } else if try_consume_lint_flag(args, &mut i, &mut lint_overrides) {
            // consumed
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}'");
            process::exit(1);
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("error: unexpected argument '{arg}'");
            process::exit(1);
        }
        i += 1;
    }
    let Some(file) = file else {
        eprintln!("error: missing file argument");
        process::exit(1);
    };
    Command::Check {
        file,
        output,
        profiles,
        concurrency_report,
        lint_overrides,
    }
}

/// Parser for `karac build [<file>] [--output=...] [--concurrency-report]`.
/// Mirrors `parse_check_command`'s shape so future build-only flags slot in
/// next to `--concurrency-report` without churning the bare-`build` /
/// project-mode-`build` distinction below.
fn parse_build_command(args: &[String]) -> Command {
    let mut file: Option<String> = None;
    let mut output = OutputMode::Text;
    let mut concurrency_report = false;
    let mut offline = false;
    let mut enable_hot_swap = false;
    let mut no_proxy = false;
    let mut target: Option<String> = None;
    let mut lint_overrides = crate::lints::CliLintOverrides::default();
    let mut i = 2usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--output=json" {
            output = OutputMode::Json;
        } else if arg == "--output=jsonl" {
            output = OutputMode::Jsonl;
        } else if arg == "--concurrency-report" {
            concurrency_report = true;
        } else if arg == "--offline" {
            offline = true;
        } else if arg == "--enable-hot-swap" {
            enable_hot_swap = true;
        } else if arg == "--no-proxy" {
            no_proxy = true;
        } else if let Some(rest) = arg.strip_prefix("--target=") {
            // `--target=<triple>` selects the active target for
            // `[target.<triple>.*]` overlay merge (tracker line 882).
            // Empty value is rejected up front so a typo can't silently
            // disable the overlay.
            if rest.trim().is_empty() {
                eprintln!("error: --target requires a non-empty target triple");
                process::exit(1);
            }
            target = Some(rest.to_string());
        } else if arg == "--target" {
            // Space-separated form: `--target <triple>`. Mirrors how
            // operators write the flag in shell scripts that prefer
            // POSIX-style separation.
            if i + 1 >= args.len() {
                eprintln!("error: --target requires a target triple value");
                process::exit(1);
            }
            let val = &args[i + 1];
            if val.trim().is_empty() {
                eprintln!("error: --target requires a non-empty target triple");
                process::exit(1);
            }
            target = Some(val.clone());
            i += 1;
        } else if arg.starts_with("--output=") {
            eprintln!(
                "error: unknown output mode '{}'. Use json or jsonl.",
                arg.strip_prefix("--output=").unwrap_or(arg)
            );
            process::exit(1);
        } else if try_consume_lint_flag(args, &mut i, &mut lint_overrides) {
            // consumed
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}'");
            process::exit(1);
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("error: unexpected argument '{arg}'");
            process::exit(1);
        }
        i += 1;
    }
    match file {
        Some(f) => Command::Build {
            file: f,
            output,
            concurrency_report,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
            lint_overrides,
        },
        None => Command::BuildProject {
            output,
            offline,
            enable_hot_swap,
            no_proxy,
            target,
        },
    }
}

fn parse_cache_command(args: &[String]) -> Command {
    // `karac cache <info|key> [flags]`. The sub-mode is the first
    // positional after `cache`; absent → error with the supported set
    // listed.
    let sub = match args.get(2).map(|s| s.as_str()) {
        Some("info") => parse_cache_info_args(&args[3..]),
        Some("key") => parse_cache_key_args(&args[3..]),
        Some(other) => {
            eprintln!(
                "error: unknown `karac cache` sub-mode '{other}' (expected one of: info, key)"
            );
            process::exit(1);
        }
        None => {
            eprintln!("error: `karac cache` requires a sub-mode (one of: info, key)");
            process::exit(1);
        }
    };
    let output = scan_output_mode_flag(args);
    Command::Cache { sub, output }
}

fn parse_cache_info_args(rest: &[String]) -> super::CacheSub {
    // `karac cache info` takes no positionals and no flags other than
    // the shared `--output=` recognized at the outer level. Reject
    // anything else with the canonical pattern.
    for arg in rest {
        match arg.as_str() {
            s if s.starts_with("--output=") => {}
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac cache info`");
                process::exit(1);
            }
            other => {
                eprintln!(
                    "error: `karac cache info` takes no positional arguments (got '{other}')"
                );
                process::exit(1);
            }
        }
    }
    super::CacheSub::Info
}

fn parse_cache_key_args(rest: &[String]) -> super::CacheSub {
    // `karac cache key --pkg NAME --version V [--edition E] [--profile P]
    // [--target-triple T] [--compiler-version C]`. `--pkg` and
    // `--version` are required; everything else falls back to the
    // active toolchain's defaults at handler time.
    let mut pkg: Option<String> = None;
    let mut version: Option<String> = None;
    let mut edition: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut target_triple: Option<String> = None;
    let mut compiler_version: Option<String> = None;
    for arg in rest {
        let s = arg.as_str();
        if let Some(v) = s.strip_prefix("--pkg=") {
            pkg = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("--version=") {
            version = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("--edition=") {
            edition = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("--profile=") {
            profile = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("--target-triple=") {
            target_triple = Some(v.to_string());
        } else if let Some(v) = s.strip_prefix("--compiler-version=") {
            compiler_version = Some(v.to_string());
        } else if s.starts_with("--output=") {
            // handled at outer level
        } else if s.starts_with("--") {
            eprintln!("error: unknown flag '{s}' for `karac cache key`");
            process::exit(1);
        } else {
            eprintln!("error: `karac cache key` takes no positional arguments (got '{s}')");
            process::exit(1);
        }
    }
    let Some(pkg) = pkg else {
        eprintln!("error: `karac cache key` requires --pkg=NAME");
        process::exit(1);
    };
    let Some(version) = version else {
        eprintln!("error: `karac cache key` requires --version=V");
        process::exit(1);
    };
    super::CacheSub::Key {
        pkg,
        version,
        edition,
        profile,
        target_triple,
        compiler_version,
    }
}

// Scan args for `--output=json|jsonl` regardless of position. Used by
// `parse_cache_command` so the flag can sit either before or after
// the sub-mode word — the natural CLI ergonomics.
fn scan_output_mode_flag(args: &[String]) -> OutputMode {
    for arg in args.iter().skip(2) {
        if arg == "--output=json" {
            return OutputMode::Json;
        }
        if arg == "--output=jsonl" {
            return OutputMode::Jsonl;
        }
        if let Some(rest) = arg.strip_prefix("--output=") {
            eprintln!("error: unknown output mode '{rest}'. Use json or jsonl.");
            process::exit(1);
        }
    }
    OutputMode::Text
}

fn parse_clean_command(args: &[String]) -> Command {
    let mut global = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--global" => global = true,
            flag if flag.starts_with('-') => {
                eprintln!("error: unknown flag '{flag}' for `karac clean`");
                process::exit(1);
            }
            other => {
                eprintln!("error: unexpected argument '{other}' for `karac clean`");
                process::exit(1);
            }
        }
    }
    Command::Clean { global }
}

fn parse_install_command(args: &[String]) -> Command {
    // `karac install <bin-spec>` takes a single positional. The spec is
    // re-parsed downstream against the manifest dependency-entry shape
    // (`path = "..."` / `git = "..."` / bare registry reference). Here
    // we just lift it off the command line.
    let mut spec: Option<String> = None;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac install`");
                process::exit(1);
            }
            other => {
                if spec.is_some() {
                    eprintln!("error: `karac install` takes exactly one <bin-spec> argument");
                    process::exit(1);
                }
                spec = Some(other.to_string());
            }
        }
    }
    let Some(spec) = spec else {
        eprintln!("error: `karac install` requires a <bin-spec> argument");
        eprintln!("       e.g. `karac install path=./tools/my-tool`");
        process::exit(1);
    };
    Command::Install { spec }
}

fn parse_vendor_command(args: &[String]) -> Command {
    let mut no_proxy = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--no-proxy" => no_proxy = true,
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac vendor`");
                process::exit(1);
            }
            other => {
                eprintln!("error: `karac vendor` takes no positional arguments (got '{other}')");
                process::exit(1);
            }
        }
    }
    Command::Vendor { no_proxy }
}

fn parse_update_command(args: &[String]) -> Command {
    // `karac update [<pkg>] [--output=json|jsonl] [--no-proxy]` — at most
    // one positional. Slice 1 of line 843 parses both forms; slice 2
    // wires the surgical <pkg> validation against the resolution.
    let mut package: Option<String> = None;
    let mut output = OutputMode::Text;
    let mut no_proxy = false;
    for arg in args.iter().skip(2) {
        if arg == "--output=json" {
            output = OutputMode::Json;
        } else if arg == "--output=jsonl" {
            output = OutputMode::Jsonl;
        } else if arg == "--no-proxy" {
            no_proxy = true;
        } else if let Some(rest) = arg.strip_prefix("--output=") {
            eprintln!("error: unknown output mode '{rest}'. Use json or jsonl.");
            process::exit(1);
        } else if arg.starts_with("--") {
            eprintln!("error: unknown flag '{arg}' for `karac update`");
            process::exit(1);
        } else if package.is_some() {
            eprintln!("error: `karac update` takes at most one <pkg> argument");
            process::exit(1);
        } else {
            package = Some(arg.clone());
        }
    }
    Command::Update {
        package,
        output,
        no_proxy,
    }
}

/// Try to consume a lint-level CLI flag at `args[*i]`. Returns
/// `true` (and advances `*i` past any next-arg the flag pulled in)
/// when the arg was a lint flag; `false` otherwise so the caller's
/// loop can try other arms.
///
/// Slice 4b polish — recognised forms (per
/// `design.md § Lint Level Attributes`):
///
/// - `-A NAME` / `-A=NAME` → record `NAME → Allow`
/// - `-W NAME` / `-W=NAME` → record `NAME → Warn`
/// - `-D NAME` / `-D=NAME` → record `NAME → Deny`
/// - `-F NAME` / `-F=NAME` → record `NAME → Deny` *and* mark
///   `NAME` forbidden (rejects inner `#[allow(NAME)]`)
/// - `-D warnings` / `-D=warnings` → set `deny_warnings` catch-all
///   (every default-`Warn` lint promotes to `Deny`); no per-name
///   entry is recorded so later `-A NAME` flags can re-suppress
///
/// Repeated flags for the same name are last-write-wins (matches
/// Rust's behavior). Unknown lint names are accepted silently —
/// the catch is at the source side (the `unknown_lint` lint fires
/// at `#[allow(NAME)]` for an unknown `NAME`); a CLI flag naming
/// an unknown lint is inert (no emission site queries the name).
/// `-F` for an unknown name is still load-bearing because inner
/// `#[allow(NAME)]` rejection is name-based, not registry-gated.
pub(super) fn try_consume_lint_flag(
    args: &[String],
    i: &mut usize,
    overrides: &mut crate::lints::CliLintOverrides,
) -> bool {
    let arg = args[*i].clone();

    // Pattern 1: bare flag with value as next arg ("-A name").
    let bare = match arg.as_str() {
        "-A" => Some((crate::lints::LintLevel::Allow, false)),
        "-W" => Some((crate::lints::LintLevel::Warn, false)),
        "-D" => Some((crate::lints::LintLevel::Deny, false)),
        "-F" => Some((crate::lints::LintLevel::Deny, true)),
        _ => None,
    };
    if let Some((level, is_forbid)) = bare {
        *i += 1;
        let Some(name) = args.get(*i) else {
            eprintln!(
                "error: `{arg}` requires a lint name (e.g. `{arg} deprecated`{})",
                if arg == "-D" { " or `-D warnings`" } else { "" },
            );
            process::exit(1);
        };
        apply_lint_flag(overrides, level, is_forbid, name);
        return true;
    }

    // Pattern 2: joined flag ("-A=name").
    for (prefix, level, is_forbid) in [
        ("-A=", crate::lints::LintLevel::Allow, false),
        ("-W=", crate::lints::LintLevel::Warn, false),
        ("-D=", crate::lints::LintLevel::Deny, false),
        ("-F=", crate::lints::LintLevel::Deny, true),
    ] {
        if let Some(name) = arg.strip_prefix(prefix) {
            if name.is_empty() {
                eprintln!(
                    "error: `{prefix}` requires a lint name (e.g. `{prefix}deprecated`{})",
                    if prefix == "-D=" {
                        " or `-D=warnings`"
                    } else {
                        ""
                    },
                );
                process::exit(1);
            }
            apply_lint_flag(overrides, level, is_forbid, name);
            return true;
        }
    }
    false
}

fn apply_lint_flag(
    overrides: &mut crate::lints::CliLintOverrides,
    level: crate::lints::LintLevel,
    is_forbid: bool,
    name: &str,
) {
    // `-D warnings` is the catch-all: promote every default-Warn
    // lint to Deny. Stored as a separate flag so a later `-A NAME`
    // can re-suppress an explicitly allowed lint (per-name beats
    // catch-all). No per-name entry under `"warnings"` because the
    // name isn't a real lint and would otherwise live inertly in
    // the levels map.
    if name == "warnings" && level == crate::lints::LintLevel::Deny && !is_forbid {
        overrides.deny_warnings = true;
        return;
    }
    overrides.levels.insert(name.to_string(), level);
    if is_forbid {
        overrides.forbidden.insert(name.to_string());
    }
}

/// Parse the comma-separated profile list passed to `--profiles=...`.
/// `all` expands to every known profile in canonical order. Empty entries
/// (e.g. trailing comma) are rejected. Unknown profile names abort with a
/// hint listing the supported set so a typo doesn't silently fall through.
fn parse_profiles_arg(spec: &str) -> Vec<crate::manifest::CompileProfile> {
    use crate::manifest::CompileProfile;
    if spec.is_empty() {
        eprintln!("error: --profiles requires at least one profile name (e.g. --profiles=all or --profiles=embedded,kernel)");
        process::exit(1);
    }
    if spec == "all" {
        return vec![
            CompileProfile::Default,
            CompileProfile::Embedded,
            CompileProfile::Kernel,
        ];
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in spec.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            eprintln!("error: --profiles entry must not be empty (got '{spec}')");
            process::exit(1);
        }
        let Some(p) = CompileProfile::parse(name) else {
            eprintln!(
                "error: unknown profile '{name}'. Supported: default, embedded, kernel, all."
            );
            process::exit(1);
        };
        // De-duplicate while preserving the user's order — running the same
        // profile twice would otherwise produce identical grouped diagnostics.
        if seen.insert(p) {
            out.push(p);
        }
    }
    out
}

fn parse_run_example_command(args: &[String]) -> Command {
    let mut name: Option<String> = None;
    let mut output = OutputMode::Text;
    let mut sequential = false;
    let mut lint_overrides = crate::lints::CliLintOverrides::default();
    let mut i = 2usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--example" {
            i += 1;
            name = Some(args.get(i).cloned().unwrap_or_else(|| {
                eprintln!("error: --example requires a name argument");
                process::exit(1);
            }));
        } else if arg == "--output=json" {
            output = OutputMode::Json;
        } else if arg == "--output=jsonl" {
            output = OutputMode::Jsonl;
        } else if arg == "--sequential" {
            sequential = true;
        } else if let Some(rest) = arg.strip_prefix("--output=") {
            eprintln!("error: unknown output mode '{rest}'. Use json or jsonl.");
            process::exit(1);
        } else if try_consume_lint_flag(args, &mut i, &mut lint_overrides) {
            // consumed
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}' for `karac run --example`");
            process::exit(1);
        } else {
            eprintln!("error: unexpected argument '{arg}' (use --example NAME to specify which example to run)");
            process::exit(1);
        }
        i += 1;
    }
    let name = name.unwrap_or_else(|| {
        eprintln!("error: --example requires a name argument");
        process::exit(1);
    });
    Command::RunExample {
        name,
        output,
        sequential,
        lint_overrides,
    }
}

fn parse_test_command(args: &[String]) -> Command {
    let mut filter: Option<String> = None;
    let mut all = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--all" => all = true,
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac test`");
                process::exit(1);
            }
            substring => {
                if filter.is_some() {
                    eprintln!("error: `karac test` takes at most one positional substring filter");
                    process::exit(1);
                }
                filter = Some(substring.to_string());
            }
        }
    }
    Command::Test { filter, all }
}

fn parse_repl_command(args: &[String]) -> Command {
    let mut auto_clone = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--auto-clone" => auto_clone = true,
            flag if flag.starts_with("--") || flag.starts_with('-') => {
                eprintln!("error: unknown flag '{flag}' for `karac repl`");
                process::exit(1);
            }
            other => {
                eprintln!("error: `karac repl` takes no positional arguments (got '{other}')");
                process::exit(1);
            }
        }
    }
    Command::Repl { auto_clone }
}

fn parse_init_command(args: &[String]) -> Command {
    let mut directory: Option<String> = None;
    let mut bin = false;
    let mut lib = false;
    let mut force = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--bin" => bin = true,
            "--lib" => lib = true,
            "--force" => force = true,
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac init`");
                process::exit(1);
            }
            name => {
                if directory.is_some() {
                    eprintln!("error: `karac init` takes at most one positional argument");
                    process::exit(1);
                }
                directory = Some(name.to_string());
            }
        }
    }
    if bin && lib {
        eprintln!("error: --bin and --lib are mutually exclusive");
        process::exit(1);
    }
    // `--bin` is the default per CR-36 T1. Absence of both flags is the
    // common case — scaffold a binary project.
    let template = if lib { Template::Lib } else { Template::Bin };
    Command::Init {
        directory,
        template,
        force,
    }
}

/// Parser for `karac migrate shared-to-par <Type> [<file>] [--apply] [--force] [--no-atomic]`.
/// `<file>` is optional: when present, single-file mode runs against just
/// that path; when omitted, project-mode discovers `kara.toml` and walks
/// every module under `src/`. The L215c Atomic[T] heuristic is on by
/// default in project-mode: each mut field is classified as Atomic[T]
/// (every observed write is bare `=` AND T is in the lock-free Copy set)
/// or Mutex[T] (anything else). `--no-atomic` opts out, restoring the
/// L215a–b4 all-Mutex behavior. `--atomic` is still accepted as an
/// explicit (now redundant) opt-in. Single-file mode always emits
/// all-Mutex regardless of the flags (no workspace visibility for the
/// classifier). Only the `shared-to-par` migration kind is in scope
/// today; future kinds (e.g. `plain-to-par`) would extend the
/// positional-kind argument here.
fn parse_migrate_command(args: &[String]) -> Command {
    if args.len() < 3 {
        eprintln!(
            "error: `karac migrate` requires a migration kind (try `karac migrate shared-to-par <Type>`)"
        );
        process::exit(1);
    }
    let kind = args[2].as_str();
    if kind != "shared-to-par" {
        eprintln!("error: unknown migration kind '{kind}' (supported: shared-to-par)");
        process::exit(1);
    }
    let mut type_name: Option<String> = None;
    let mut file: Option<String> = None;
    let mut apply = false;
    let mut force = false;
    let mut atomic = true;
    for arg in args.iter().skip(3) {
        match arg.as_str() {
            "--apply" => apply = true,
            "--force" => force = true,
            "--atomic" => atomic = true,
            "--no-atomic" => atomic = false,
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac migrate`");
                process::exit(1);
            }
            other if other.ends_with(".kara") => {
                if file.is_some() {
                    eprintln!("error: `karac migrate` takes at most one file argument");
                    process::exit(1);
                }
                file = Some(other.to_string());
            }
            other => {
                if type_name.is_some() {
                    eprintln!(
                        "error: `karac migrate shared-to-par` takes a single type name (got '{other}' after type was already set)"
                    );
                    process::exit(1);
                }
                type_name = Some(other.to_string());
            }
        }
    }
    let Some(type_name) = type_name else {
        eprintln!(
            "error: missing type name for `karac migrate shared-to-par` (try `karac migrate shared-to-par Elevator`)"
        );
        process::exit(1);
    };
    Command::Migrate {
        type_name,
        apply,
        force,
        file,
        atomic,
    }
}

fn parse_fix_command(args: &[String]) -> Command {
    let mut file: Option<String> = None;
    let mut dry_run = false;
    for arg in args.iter().skip(2) {
        match arg.as_str() {
            "--dry-run" | "-n" => dry_run = true,
            flag if flag.starts_with("--") => {
                eprintln!("error: unknown flag '{flag}' for `karac fix`");
                process::exit(1);
            }
            other => {
                if file.is_some() {
                    eprintln!("error: `karac fix` takes at most one file argument");
                    process::exit(1);
                }
                file = Some(other.to_string());
            }
        }
    }
    let Some(file) = file else {
        eprintln!("error: missing file argument for `karac fix`");
        process::exit(1);
    };
    Command::Fix { file, dry_run }
}

/// Parser for `karac catalog <file.kara>`. Takes a single positional file
/// argument and rejects any flags — JSONL output is the only mode (no
/// `--output=text` form), so there is nothing to switch.
fn parse_catalog_command(args: &[String]) -> Command {
    let mut file: Option<String> = None;
    for arg in args.iter().skip(2) {
        if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}' for `karac catalog`");
            process::exit(1);
        }
        if file.is_some() {
            eprintln!("error: `karac catalog` takes a single file argument");
            process::exit(1);
        }
        file = Some(arg.clone());
    }
    let Some(file) = file else {
        eprintln!("error: missing file argument for `karac catalog`");
        process::exit(1);
    };
    Command::Catalog { file }
}

/// Parser for `karac explain --concept=NAME [--format=FMT]` and
/// `karac explain --class=NAME [--format=FMT]`. Exactly one of
/// `--concept` / `--class` is required. `--format` defaults to
/// `text`; `--format=json` opts into the machine-consumable shape
/// minted by line 619 slice 3. The concept / class *name* itself
/// is validated at render time so the supported-set message lives
/// in one place (`src/cli/explain.rs`).
fn parse_explain_command(args: &[String]) -> Command {
    let mut concept: Option<String> = None;
    let mut class: Option<String> = None;
    let mut format: Option<crate::cli::ExplainFormat> = None;
    for arg in args.iter().skip(2) {
        if let Some(rest) = arg.strip_prefix("--concept=") {
            if rest.is_empty() {
                eprintln!("error: --concept requires a name (e.g. --concept=closures)");
                process::exit(1);
            }
            if concept.is_some() {
                eprintln!("error: --concept may only be specified once");
                process::exit(1);
            }
            concept = Some(rest.to_string());
        } else if let Some(rest) = arg.strip_prefix("--class=") {
            if rest.is_empty() {
                eprintln!("error: --class requires a name (e.g. --class=TYPE_MISMATCH)");
                process::exit(1);
            }
            if class.is_some() {
                eprintln!("error: --class may only be specified once");
                process::exit(1);
            }
            class = Some(rest.to_string());
        } else if let Some(rest) = arg.strip_prefix("--format=") {
            if format.is_some() {
                eprintln!("error: --format may only be specified once");
                process::exit(1);
            }
            format = Some(match rest {
                "text" => crate::cli::ExplainFormat::Text,
                "json" => crate::cli::ExplainFormat::Json,
                other => {
                    eprintln!("error: unknown --format value '{other}' (supported: text, json)");
                    process::exit(1);
                }
            });
        } else if arg.starts_with('-') {
            eprintln!("error: unknown flag '{arg}' for `karac explain`");
            process::exit(1);
        } else {
            eprintln!("error: unexpected argument '{arg}' (use --concept=NAME or --class=NAME)");
            process::exit(1);
        }
    }
    let target = match (concept, class) {
        (Some(c), None) => crate::cli::ExplainTarget::Concept(c),
        (None, Some(c)) => crate::cli::ExplainTarget::Class(c),
        (Some(_), Some(_)) => {
            eprintln!("error: --concept and --class are mutually exclusive");
            process::exit(1);
        }
        (None, None) => {
            eprintln!(
                "error: `karac explain` requires --concept=NAME or --class=NAME (e.g. --concept=closures, --class=TYPE_MISMATCH)"
            );
            process::exit(1);
        }
    };
    let format = format.unwrap_or(crate::cli::ExplainFormat::Text);
    Command::Explain { target, format }
}

fn parse_query_command(args: &[String]) -> Command {
    if args.len() < 4 {
        eprintln!(
            "Usage: karac query <effects|ownership|concurrency|cost-summary|attributes|queries|monomorphization|affected-by> [flags] <target>"
        );
        eprintln!("       <target> is `<file>.<function>` for the per-function kinds,");
        eprintln!("                or `<file>` for cost-summary, attributes, queries, and monomorphization,");
        eprintln!("                or `<file>[:<line>|<line>-<line>|<fn>]` for affected-by.");
        eprintln!("       attributes accepts `--tool=PREFIX` to filter by first-segment match.");
        eprintln!(
            "       affected-by accepts `--tests-only` and `--direction=callers|callees|all`."
        );
        process::exit(1);
    }
    // The `attributes` and `affected-by` kinds accept flags before
    // the target — collect them so the target is whatever comes
    // next. The per-function and cost-summary kinds don't accept
    // flags today.
    let kind_str = args[2].as_str();
    let mut tool_prefix: Option<String> = None;
    let mut tests_only = false;
    let mut direction = crate::cli::AffectedByDirection::All;
    let mut target_idx = 3;
    if kind_str == "attributes" {
        while target_idx < args.len() {
            let a = &args[target_idx];
            if let Some(rest) = a.strip_prefix("--tool=") {
                tool_prefix = Some(rest.to_string());
                target_idx += 1;
            } else if a == "--tool" {
                if target_idx + 1 >= args.len() {
                    eprintln!("error: `--tool` flag requires a value");
                    process::exit(1);
                }
                tool_prefix = Some(args[target_idx + 1].clone());
                target_idx += 2;
            } else {
                break;
            }
        }
        if target_idx >= args.len() {
            eprintln!("error: `karac query attributes` requires a file target");
            process::exit(1);
        }
    } else if kind_str == "affected-by" {
        // Allow flags interspersed with the positional target — the
        // user can write either `--tests-only foo.kara:bar` or
        // `foo.kara:bar --tests-only`. Walk every arg from index 3
        // onward, classifying flags into the slot above and the
        // last non-flag arg into the target.
        let mut positional: Option<String> = None;
        let mut idx = 3;
        while idx < args.len() {
            let a = &args[idx];
            if a == "--tests-only" {
                tests_only = true;
            } else if let Some(rest) = a.strip_prefix("--direction=") {
                direction = match rest {
                    "callers" => crate::cli::AffectedByDirection::Callers,
                    "callees" => crate::cli::AffectedByDirection::Callees,
                    "all" => crate::cli::AffectedByDirection::All,
                    other => {
                        eprintln!(
                            "error: unknown --direction value '{other}' (supported: callers, callees, all)"
                        );
                        process::exit(1);
                    }
                };
            } else if a.starts_with("--") {
                eprintln!("error: unknown flag '{a}' for `karac query affected-by`");
                process::exit(1);
            } else if positional.is_some() {
                eprintln!("error: `karac query affected-by` takes a single target");
                process::exit(1);
            } else {
                positional = Some(a.clone());
            }
            idx += 1;
        }
        let Some(raw) = positional else {
            eprintln!("error: `karac query affected-by` requires a target");
            process::exit(1);
        };
        let (file, target_spec) = parse_affected_by_target(&raw);
        return Command::Query {
            kind: QueryKind::AffectedBy {
                target: target_spec,
                tests_only,
                direction,
            },
            file,
            function: String::new(),
        };
    }
    let kind = match kind_str {
        "effects" => QueryKind::Effects,
        "ownership" => QueryKind::Ownership,
        "concurrency" => QueryKind::Concurrency,
        "cost-summary" => QueryKind::CostSummary,
        "attributes" => QueryKind::Attributes { tool_prefix },
        "queries" => QueryKind::Queries,
        "monomorphization" => QueryKind::Monomorphization,
        // `affected-by` returns via the dedicated branch above and
        // never reaches this match arm.
        other => {
            eprintln!(
                "error: unknown query kind '{other}'. Use 'effects', 'ownership', 'concurrency', 'cost-summary', 'attributes', 'queries', 'monomorphization', or 'affected-by'."
            );
            process::exit(1);
        }
    };
    let target = &args[target_idx];
    // cost-summary, attributes, and queries take a bare file path. The
    // other kinds parse `file.function` via rsplit (multi-dot file
    // paths are fine since Kāra identifiers cannot contain `.`).
    let (file, function) = match &kind {
        QueryKind::CostSummary
        | QueryKind::Attributes { .. }
        | QueryKind::Queries
        | QueryKind::Monomorphization => (target.clone(), String::new()),
        QueryKind::AffectedBy { .. } => unreachable!("affected-by returned via dedicated branch"),
        _ => match target.rsplit_once('.') {
            Some((f, func)) => (f.to_string(), func.to_string()),
            None => {
                eprintln!("error: query target must be <file>.<function>, got '{target}'");
                process::exit(1);
            }
        },
    };
    Command::Query {
        kind,
        file,
        function,
    }
}

/// Parse the `<target>` argument of `karac query affected-by` into
/// `(file_path, TargetSpec)`. Supported forms:
///   - `src/foo.kara`            → File
///   - `src/foo.kara:42`         → FileRange (single line)
///   - `src/foo.kara:42-58`      → FileRange (inclusive)
///   - `src/foo.kara:my_fn`      → Function (bare)
///   - `src/foo.kara:Type.method`→ Function (qualified)
fn parse_affected_by_target(raw: &str) -> (String, crate::call_graph::TargetSpec) {
    // The `:` separator divides a file path from an optional
    // qualifier. Windows-style absolute paths use `C:` too — but
    // that's not a v1 platform concern (Kāra single-file mode is
    // unix-style today).
    let Some(colon) = raw.find(':') else {
        return (
            raw.to_string(),
            crate::call_graph::TargetSpec::File(raw.to_string()),
        );
    };
    let (file, rest) = raw.split_at(colon);
    let rest = &rest[1..]; // skip the `:`
    if rest.is_empty() {
        eprintln!("error: empty target qualifier after `:` in '{raw}'");
        process::exit(1);
    }
    // Numeric forms are line / line-range.
    let starts_with_digit = rest.chars().next().is_some_and(|c| c.is_ascii_digit());
    if starts_with_digit {
        if let Some((lo_str, hi_str)) = rest.split_once('-') {
            let lo: usize = lo_str.parse().unwrap_or_else(|_| {
                eprintln!("error: invalid line range start '{lo_str}' in '{raw}'");
                process::exit(1);
            });
            let hi: usize = hi_str.parse().unwrap_or_else(|_| {
                eprintln!("error: invalid line range end '{hi_str}' in '{raw}'");
                process::exit(1);
            });
            if lo > hi {
                eprintln!("error: line range start ({lo}) exceeds end ({hi}) in '{raw}'");
                process::exit(1);
            }
            return (
                file.to_string(),
                crate::call_graph::TargetSpec::FileRange(file.to_string(), lo, hi),
            );
        }
        let line: usize = rest.parse().unwrap_or_else(|_| {
            eprintln!("error: invalid line number '{rest}' in '{raw}'");
            process::exit(1);
        });
        return (
            file.to_string(),
            crate::call_graph::TargetSpec::FileRange(file.to_string(), line, line),
        );
    }
    // Otherwise treat the rest as a function key. `::`-joined paths
    // (multi-module convention) collapse to a `.`-joined key so the
    // resolver sees the canonical call-graph shape.
    let name = rest.replace("::", ".");
    (
        file.to_string(),
        crate::call_graph::TargetSpec::Function(name),
    )
}
