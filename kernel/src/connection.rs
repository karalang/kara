//! Jupyter connection-file parser.
//!
//! When a Jupyter frontend (JupyterLab / classic Notebook / `jupyter
//! console`) launches a kernel it writes a small JSON file describing
//! the five ZMQ ports the kernel should bind to, the HMAC signing key
//! used on every wire-protocol message, and the transport / IP. The
//! frontend then passes the path on the command line as
//! `--connection-file=<path>`. The kernel is expected to read the file
//! before opening any sockets.
//!
//! Spec: <https://jupyter-client.readthedocs.io/en/stable/kernels.html#connection-files>.
//!
//! Slice 1 ships only the parser + a typed view of the file's
//! contents. Slices 3+ consume the [`ConnectionFile`] to open the ZMQ
//! sockets and sign outgoing messages.

use serde::Deserialize;
use std::fmt;
use std::path::Path;

/// Typed view of a Jupyter connection file.
///
/// All five port fields are required by the spec — a frontend always
/// picks a port (often by binding ephemerally then writing back the
/// chosen port). `signature_scheme` is documented as a free-form
/// string, but Jupyter has only ever defined `hmac-sha256`; the kernel
/// rejects anything else at startup rather than silently downgrading.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ConnectionFile {
    /// Transport prefix — `"tcp"` or `"ipc"`. Combined with `ip` and a
    /// port number to form the ZMQ endpoint URL: `tcp://127.0.0.1:54321`
    /// or `ipc:///tmp/kernel-…`.
    pub transport: String,
    /// IP address the sockets bind to. Loopback (`127.0.0.1`) for
    /// local-only frontends; `0.0.0.0` for remote access.
    pub ip: String,
    /// HMAC signing key. Every wire-protocol message is signed with
    /// this key under [`Self::signature_scheme`]. Empty string is
    /// permitted by the spec to mean "skip signing entirely" (used by
    /// some testing setups); a real frontend always writes a random
    /// key.
    pub key: String,
    /// Always `"hmac-sha256"` in practice. The kernel rejects other
    /// values at startup (we sign only with HMAC-SHA-256).
    pub signature_scheme: String,
    /// Shell channel port — request/reply for `execute_request`,
    /// `kernel_info_request`, `complete_request`, etc.
    pub shell_port: u16,
    /// IOPub channel port — broadcast of stream / display_data /
    /// execute_result / error messages to every frontend connected to
    /// the kernel.
    pub iopub_port: u16,
    /// Stdin channel port — request/reply for `input_request` (kernel
    /// asks the frontend for keyboard input on the user's behalf).
    pub stdin_port: u16,
    /// Control channel port — request/reply for `interrupt_request`,
    /// `shutdown_request`, `debug_request`. Mirrors shell but reserved
    /// for high-priority traffic that must not queue behind a long
    /// execute.
    pub control_port: u16,
    /// Heartbeat port — bare REQ/REP echo loop the frontend uses to
    /// detect kernel liveness.
    #[serde(rename = "hb_port")]
    pub hb_port: u16,
}

/// Errors surfaced while loading a connection file. Each variant
/// carries enough context for the kernel binary to print a single
/// actionable line to stderr before exiting non-zero.
#[derive(Debug)]
pub enum ConnectionFileError {
    /// Filesystem read failed (missing file, permissions, etc.).
    Io {
        path: String,
        source: std::io::Error,
    },
    /// JSON parse / schema mismatch — a required field is missing,
    /// has the wrong type, or the document isn't valid JSON.
    Parse {
        path: String,
        source: serde_json::Error,
    },
    /// `signature_scheme` was present but not `"hmac-sha256"`. Kernels
    /// that implemented other schemes never shipped; rejecting at
    /// startup avoids silently accepting messages we cannot verify.
    UnsupportedSignatureScheme { path: String, scheme: String },
}

impl fmt::Display for ConnectionFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "could not read connection file {path}: {source}")
            }
            Self::Parse { path, source } => {
                write!(f, "could not parse connection file {path}: {source}")
            }
            Self::UnsupportedSignatureScheme { path, scheme } => write!(
                f,
                "connection file {path} requested unsupported signature_scheme \
                 {scheme:?}; only \"hmac-sha256\" is supported"
            ),
        }
    }
}

impl std::error::Error for ConnectionFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
            Self::UnsupportedSignatureScheme { .. } => None,
        }
    }
}

impl ConnectionFile {
    /// Parse a connection-file JSON string. Used by [`Self::load`] and
    /// reachable directly from tests so the schema check is exercised
    /// without a temp-file dance.
    pub fn from_json(path: &str, json: &str) -> Result<Self, ConnectionFileError> {
        let parsed: ConnectionFile =
            serde_json::from_str(json).map_err(|source| ConnectionFileError::Parse {
                path: path.to_string(),
                source,
            })?;
        if parsed.signature_scheme != "hmac-sha256" {
            return Err(ConnectionFileError::UnsupportedSignatureScheme {
                path: path.to_string(),
                scheme: parsed.signature_scheme,
            });
        }
        Ok(parsed)
    }

    /// Load and parse a connection file from disk. UTF-8 decode
    /// errors are folded into [`ConnectionFileError::Io`] via
    /// `read_to_string`'s built-in conversion (Jupyter always writes
    /// ASCII JSON, so the UTF-8 path is purely a safety net).
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConnectionFileError> {
        let path_ref = path.as_ref();
        let path_str = path_ref.display().to_string();
        let json = std::fs::read_to_string(path_ref).map_err(|source| ConnectionFileError::Io {
            path: path_str.clone(),
            source,
        })?;
        Self::from_json(&path_str, &json)
    }

    /// Build the ZMQ endpoint URL for one of the kernel's five ports.
    /// `tcp` and `ipc` are the two transports Jupyter uses in
    /// practice; the spec leaves the field free-form, so we just
    /// concatenate without case-folding (matches `jupyter_client`'s
    /// Python-side behavior). Consumed by slice 3 once the ZMQ
    /// sockets bind; pre-exercised under `cfg(test)` here.
    #[allow(dead_code)]
    pub fn endpoint(&self, port: u16) -> String {
        if self.transport == "ipc" {
            format!("ipc://{}-{}", self.ip, port)
        } else {
            format!("{}://{}:{}", self.transport, self.ip, port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_json() -> &'static str {
        r#"{
            "transport": "tcp",
            "ip": "127.0.0.1",
            "key": "8f7e6d5c-4b3a-2918-7f6e-5d4c3b2a1908",
            "signature_scheme": "hmac-sha256",
            "shell_port": 54321,
            "iopub_port": 54322,
            "stdin_port": 54323,
            "control_port": 54324,
            "hb_port": 54325
        }"#
    }

    #[test]
    fn parses_canonical_connection_file() {
        let cf = ConnectionFile::from_json("conn.json", canonical_json()).unwrap();
        assert_eq!(cf.transport, "tcp");
        assert_eq!(cf.ip, "127.0.0.1");
        assert_eq!(cf.key, "8f7e6d5c-4b3a-2918-7f6e-5d4c3b2a1908");
        assert_eq!(cf.signature_scheme, "hmac-sha256");
        assert_eq!(cf.shell_port, 54321);
        assert_eq!(cf.iopub_port, 54322);
        assert_eq!(cf.stdin_port, 54323);
        assert_eq!(cf.control_port, 54324);
        assert_eq!(cf.hb_port, 54325);
    }

    #[test]
    fn rejects_unknown_signature_scheme() {
        let bad = canonical_json().replace("hmac-sha256", "hmac-sha512");
        let err = ConnectionFile::from_json("conn.json", &bad).unwrap_err();
        match err {
            ConnectionFileError::UnsupportedSignatureScheme { scheme, .. } => {
                assert_eq!(scheme, "hmac-sha512");
            }
            other => panic!("expected UnsupportedSignatureScheme, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_required_field() {
        let missing_key = r#"{
            "transport": "tcp",
            "ip": "127.0.0.1",
            "signature_scheme": "hmac-sha256",
            "shell_port": 1, "iopub_port": 2, "stdin_port": 3,
            "control_port": 4, "hb_port": 5
        }"#;
        let err = ConnectionFile::from_json("conn.json", missing_key).unwrap_err();
        assert!(matches!(err, ConnectionFileError::Parse { .. }));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = ConnectionFile::from_json("conn.json", "{not json").unwrap_err();
        assert!(matches!(err, ConnectionFileError::Parse { .. }));
    }

    #[test]
    fn empty_signing_key_is_permitted() {
        // Per the spec an empty string means "skip signing entirely".
        // The kernel still parses cleanly; signing-side enforcement
        // (or skip) is a slice-2 concern.
        let json = canonical_json().replace("\"8f7e6d5c-4b3a-2918-7f6e-5d4c3b2a1908\"", "\"\"");
        let cf = ConnectionFile::from_json("conn.json", &json).unwrap();
        assert!(cf.key.is_empty());
    }

    #[test]
    fn endpoint_tcp_format() {
        let cf = ConnectionFile::from_json("conn.json", canonical_json()).unwrap();
        assert_eq!(cf.endpoint(cf.shell_port), "tcp://127.0.0.1:54321");
        assert_eq!(cf.endpoint(cf.iopub_port), "tcp://127.0.0.1:54322");
    }

    #[test]
    fn endpoint_ipc_format() {
        let json = canonical_json()
            .replace("\"tcp\"", "\"ipc\"")
            .replace("\"127.0.0.1\"", "\"/tmp/kernel-abc\"");
        let cf = ConnectionFile::from_json("conn.json", &json).unwrap();
        // `ipc` transport produces `ipc://<path>-<port>` per the
        // jupyter_client convention (one socket file per channel).
        assert_eq!(cf.endpoint(cf.shell_port), "ipc:///tmp/kernel-abc-54321");
    }

    #[test]
    fn load_from_file_round_trip() {
        let tmp = std::env::temp_dir().join("karac-kernel-conn-test.json");
        std::fs::write(&tmp, canonical_json()).unwrap();
        let cf = ConnectionFile::load(&tmp).unwrap();
        assert_eq!(cf.shell_port, 54321);
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn load_missing_file_reports_io_error() {
        let err = ConnectionFile::load("/nonexistent/karac-kernel-test.json").unwrap_err();
        assert!(matches!(err, ConnectionFileError::Io { .. }));
    }
}
