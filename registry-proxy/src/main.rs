//! `kara-registry-proxy` — reference / dev registry proxy binary.
//!
//! ```text
//! kara-registry-proxy serve --root <DIR> [--addr <IP>] [--port <N>]
//! kara-registry-proxy build --from <DIR> --out <DIR>
//! ```
//!
//! `serve` hosts a store directory over HTTP (see the crate docs / the
//! wire protocol at `docs/registry-proxy-protocol.md`). `build` turns a
//! folder of packages (`<name>/<version>.tar.gz` + optional `upstream`
//! file) into a servable store in one step. Point `karac` at a running
//! server with `KARAC_REGISTRY_PROXY=http://<addr>:<port>`. This is a
//! reference server for local mirrors and tests — not the production
//! mirror.
//!
//! Back-compat: with no subcommand, a leading `--…` flag is treated as
//! `serve` (so `kara-registry-proxy --root DIR` still works).

use kara_registry_proxy::{build_store, looks_like_store_root, serve, FsStore};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

const USAGE: &str = "\
kara-registry-proxy — reference / dev Kāra registry proxy

USAGE:
    kara-registry-proxy serve --root <DIR> [--addr <IP>] [--port <N>]
    kara-registry-proxy build --from <DIR> --out <DIR>

serve — host a store directory over HTTP:
    --root <DIR>   Store root: <DIR>/catalog/<name>.json and
                   <DIR>/pkg/<name>/<version>.tar.gz
    --addr <IP>    Bind address (default 127.0.0.1)
    --port <N>     Bind port (default 8080)

build — assemble a store from a folder of packages:
    --from <DIR>   Source: <DIR>/<name>/<version>.tar.gz plus an optional
                   <DIR>/<name>/upstream file with the source URL
    --out <DIR>    Store root to create (ready to `serve --root`)

    -h, --help     Show this help
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    match args.peek().map(String::as_str) {
        Some("serve") => {
            args.next();
            run_serve(args.collect())
        }
        Some("build") => {
            args.next();
            run_build(args.collect())
        }
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        // Back-compat: `kara-registry-proxy --root DIR …` == `serve …`.
        Some(flag) if flag.starts_with("--") => run_serve(args.collect()),
        None => {
            eprintln!("error: expected a subcommand\n\n{USAGE}");
            ExitCode::from(2)
        }
        Some(other) => {
            eprintln!("error: unknown subcommand {other:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Pull `--flag value` pairs out of a flat arg list into a lookup, erroring
/// on a dangling flag or an unknown token.
fn parse_flags(args: Vec<String>, allowed: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        if arg == "-h" || arg == "--help" {
            return Err("help".to_string());
        }
        let key = arg
            .strip_prefix("--")
            .filter(|k| allowed.contains(k))
            .ok_or_else(|| format!("unexpected argument {arg:?}"))?;
        let value = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
        out.push((key.to_string(), value));
    }
    Ok(out)
}

fn flag<'a>(flags: &'a [(String, String)], name: &str) -> Option<&'a str> {
    flags
        .iter()
        .rev()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

fn run_serve(args: Vec<String>) -> ExitCode {
    let flags = match parse_flags(args, &["root", "addr", "port"]) {
        Ok(f) => f,
        Err(e) if e == "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let Some(root) = flag(&flags, "root").map(PathBuf::from) else {
        eprintln!("error: serve needs --root <DIR>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    let addr = flag(&flags, "addr").unwrap_or("127.0.0.1");
    let port: u16 = match flag(&flags, "port").unwrap_or("8080").parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("error: --port must be a number");
            return ExitCode::from(2);
        }
    };

    if !root.is_dir() {
        eprintln!("error: --root {root:?} is not a directory");
        return ExitCode::from(2);
    }
    if !looks_like_store_root(&root) {
        eprintln!(
            "warning: {root:?} has no catalog/ or pkg/ subdirectory — every request will 404 \
             (did you mean to `build` a store first?)"
        );
    }

    let bind = format!("{addr}:{port}");
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: could not bind {bind}: {e}");
            return ExitCode::from(1);
        }
    };
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(bind);
    eprintln!("kara-registry-proxy serving {root:?} on http://{local}");

    serve(listener, Arc::new(FsStore::new(root)));
    ExitCode::SUCCESS
}

fn run_build(args: Vec<String>) -> ExitCode {
    let flags = match parse_flags(args, &["from", "out"]) {
        Ok(f) => f,
        Err(e) if e == "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let (Some(from), Some(out)) = (
        flag(&flags, "from").map(PathBuf::from),
        flag(&flags, "out").map(PathBuf::from),
    ) else {
        eprintln!("error: build needs --from <DIR> and --out <DIR>\n\n{USAGE}");
        return ExitCode::from(2);
    };
    if !from.is_dir() {
        eprintln!("error: --from {from:?} is not a directory");
        return ExitCode::from(2);
    }

    match build_store(&from, &out) {
        Ok(report) => {
            let total: usize = report.packages.iter().map(|(_, n)| n).sum();
            eprintln!(
                "built store at {out:?}: {} package{}, {total} version{}",
                report.packages.len(),
                if report.packages.len() == 1 { "" } else { "s" },
                if total == 1 { "" } else { "s" },
            );
            for (name, n) in &report.packages {
                eprintln!("  {name}: {n} version{}", if *n == 1 { "" } else { "s" });
            }
            eprintln!(
                "serve it with: kara-registry-proxy serve --root {}",
                out.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}
