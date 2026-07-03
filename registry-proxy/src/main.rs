//! `kara-registry-proxy` — reference / dev registry proxy binary.
//!
//! ```text
//! kara-registry-proxy --root <DIR> [--addr <IP>] [--port <N>]
//! ```
//!
//! Serves the store at `<DIR>` over HTTP (see the crate docs / the wire
//! protocol at `docs/registry-proxy-protocol.md`). Point `karac` at it
//! with `KARAC_REGISTRY_PROXY=http://<addr>:<port>`. This is a reference
//! server for local mirrors and tests — not the production mirror.

use kara_registry_proxy::{looks_like_store_root, serve, FsStore};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

struct Args {
    root: PathBuf,
    addr: String,
    port: u16,
}

fn parse_args() -> Result<Args, String> {
    let mut root: Option<PathBuf> = None;
    let mut addr = "127.0.0.1".to_string();
    let mut port: u16 = 8080;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => {
                root = Some(PathBuf::from(
                    it.next().ok_or("--root needs a directory argument")?,
                ));
            }
            "--addr" => {
                addr = it.next().ok_or("--addr needs an address argument")?;
            }
            "--port" => {
                port = it
                    .next()
                    .ok_or("--port needs a number argument")?
                    .parse()
                    .map_err(|_| "--port must be a number".to_string())?;
            }
            "-h" | "--help" => {
                return Err("help".to_string());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        root: root.ok_or("--root <DIR> is required")?,
        addr,
        port,
    })
}

const USAGE: &str = "\
kara-registry-proxy — reference / dev Kāra registry proxy

USAGE:
    kara-registry-proxy --root <DIR> [--addr <IP>] [--port <N>]

OPTIONS:
    --root <DIR>   Store root: <DIR>/catalog/<name>.json and
                   <DIR>/pkg/<name>/<version>.tar.gz
    --addr <IP>    Bind address (default 127.0.0.1)
    --port <N>     Bind port (default 8080)
    -h, --help     Show this help
";

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e == "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    if !args.root.is_dir() {
        eprintln!("error: --root {:?} is not a directory", args.root);
        return ExitCode::from(2);
    }
    if !looks_like_store_root(&args.root) {
        eprintln!(
            "warning: {:?} has no catalog/ or pkg/ subdirectory — every request will 404",
            args.root
        );
    }

    let bind = format!("{}:{}", args.addr, args.port);
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: could not bind {bind}: {e}");
            return ExitCode::from(1);
        }
    };
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(bind);
    eprintln!(
        "kara-registry-proxy serving {:?} on http://{local}",
        args.root
    );

    serve(listener, Arc::new(FsStore::new(args.root)));
    ExitCode::SUCCESS
}
