//! `karac-kernel` — Jupyter kernel binary for Kāra.
//!
//! Tracker entry: `docs/implementation_checklist/phase-5-diagnostics.md`
//! § "Jupyter kernel MVP" (line 719).
//!
//! The binary is registered as a kernel by the Python shim (slice 6);
//! Jupyter launches it with `--connection-file=<path>` pointing at a
//! JSON file describing the five ZMQ ports + HMAC signing key. This
//! slice (1 of 6) wires the argument parser and connection-file
//! loader; opening sockets and speaking the wire protocol arrives in
//! later slices.

use std::process::ExitCode;

mod connection;
mod runtime;
mod transport;
mod wire;
mod zmq_transport;

use connection::ConnectionFile;

const USAGE: &str = "\
usage: karac-kernel --connection-file=<path>

Jupyter kernel for the Kāra language. Not intended to be run directly —
the launcher is the `karac-kernel` PyPI package, which installs the
kernelspec and points Jupyter at this binary.

Options:
  --connection-file=<path>   JSON connection file written by the
                             Jupyter frontend (required).
  -h, --help                 Print this help and exit.
  --version                  Print the kernel version and exit.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(KernelError::Usage(msg)) => {
            eprintln!("{msg}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(KernelError::HelpRequested) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(KernelError::VersionRequested) => {
            println!("karac-kernel {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Err(KernelError::Connection(err)) => {
            eprintln!("karac-kernel: {err}");
            ExitCode::FAILURE
        }
        Err(KernelError::Runtime(msg)) => {
            eprintln!("karac-kernel: {msg}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug)]
enum KernelError {
    Usage(String),
    HelpRequested,
    VersionRequested,
    Connection(connection::ConnectionFileError),
    Runtime(String),
}

impl From<connection::ConnectionFileError> for KernelError {
    fn from(err: connection::ConnectionFileError) -> Self {
        Self::Connection(err)
    }
}

fn run(args: &[String]) -> Result<(), KernelError> {
    let parsed = parse_args(args)?;
    let connection = ConnectionFile::load(&parsed.connection_file)?;
    run_kernel(connection)
}

#[cfg(feature = "real-zmq")]
fn run_kernel(connection: ConnectionFile) -> Result<(), KernelError> {
    use std::sync::Arc;
    let signer = wire::Signer::new(&connection.key);
    let transport = zmq_transport::ZmqTransport::bind(&connection)
        .map_err(|err| KernelError::Runtime(format!("could not open ZMQ sockets: {err}")))?;
    let kernel = runtime::Kernel::new(Arc::new(transport), signer, runtime::KernelInfo::default());
    let heartbeat = kernel.spawn_heartbeat();
    eprintln!(
        "karac-kernel: bound to {} (shell {}, iopub {}, stdin {}, control {}, hb {})",
        connection.ip,
        connection.shell_port,
        connection.iopub_port,
        connection.stdin_port,
        connection.control_port,
        connection.hb_port,
    );
    kernel.run();
    heartbeat.join().ok();
    Ok(())
}

#[cfg(not(feature = "real-zmq"))]
fn run_kernel(connection: ConnectionFile) -> Result<(), KernelError> {
    // Without `real-zmq` we can't bind sockets. Slice 1's smoke-test
    // posture is preserved — print what we parsed and exit non-zero
    // with an actionable hint. The Python shim (slice 6) builds with
    // `--features real-zmq` so end-users never hit this path.
    eprintln!(
        "karac-kernel: parsed connection file (shell {}, iopub {}, stdin {}, control {}, hb {}) \
         but this build was compiled without the `real-zmq` feature — rebuild with \
         `cargo build -p karac-kernel --features real-zmq` to enable the ZMQ message pump.",
        connection.shell_port,
        connection.iopub_port,
        connection.stdin_port,
        connection.control_port,
        connection.hb_port,
    );
    Err(KernelError::Runtime(
        "kernel binary built without `real-zmq` feature".to_string(),
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedArgs {
    connection_file: String,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, KernelError> {
    let mut connection_file: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(KernelError::HelpRequested),
            "--version" => return Err(KernelError::VersionRequested),
            "--connection-file" => {
                let value = iter.next().ok_or_else(|| {
                    KernelError::Usage("--connection-file requires a path argument".to_string())
                })?;
                connection_file = Some(value.clone());
            }
            other if other.starts_with("--connection-file=") => {
                let value = other.trim_start_matches("--connection-file=");
                connection_file = Some(value.to_string());
            }
            other => {
                return Err(KernelError::Usage(format!("unexpected argument: {other}")));
            }
        }
    }
    let connection_file = connection_file.ok_or_else(|| {
        KernelError::Usage("missing required --connection-file argument".to_string())
    })?;
    Ok(ParsedArgs { connection_file })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_separate_form() {
        let parsed = parse_args(&args(&["--connection-file", "/tmp/conn.json"])).unwrap();
        assert_eq!(parsed.connection_file, "/tmp/conn.json");
    }

    #[test]
    fn parses_equals_form() {
        let parsed = parse_args(&args(&["--connection-file=/tmp/conn.json"])).unwrap();
        assert_eq!(parsed.connection_file, "/tmp/conn.json");
    }

    #[test]
    fn missing_connection_file_is_usage_error() {
        let err = parse_args(&args(&[])).unwrap_err();
        assert!(matches!(err, KernelError::Usage(ref m) if m.contains("missing required")));
    }

    #[test]
    fn missing_value_after_flag_is_usage_error() {
        let err = parse_args(&args(&["--connection-file"])).unwrap_err();
        assert!(matches!(err, KernelError::Usage(ref m) if m.contains("requires a path")));
    }

    #[test]
    fn unexpected_argument_is_usage_error() {
        let err = parse_args(&args(&["--bogus"])).unwrap_err();
        assert!(matches!(err, KernelError::Usage(ref m) if m.contains("unexpected argument")));
    }

    #[test]
    fn help_short_form() {
        let err = parse_args(&args(&["-h"])).unwrap_err();
        assert!(matches!(err, KernelError::HelpRequested));
    }

    #[test]
    fn help_long_form() {
        let err = parse_args(&args(&["--help"])).unwrap_err();
        assert!(matches!(err, KernelError::HelpRequested));
    }

    #[test]
    fn version_flag() {
        let err = parse_args(&args(&["--version"])).unwrap_err();
        assert!(matches!(err, KernelError::VersionRequested));
    }
}
