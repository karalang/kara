//! Slice c-repl.B.B — parent-side client for the `karac_jit_runner
//! --repl-mode` subprocess (slice B.A).
//!
//! `ReplRunnerClient` owns the spawned subprocess, its stdin (where the
//! parent writes framed `cell <id> <ir_byte_count>\n<ir bytes>`
//! commands), and its buffered stdout (where the parent reads framed
//! `result <id> <exit> <stdout_len> <stderr_len>\n<stdout><stderr>`
//! responses). The Session holds at most one of these; it's lazily
//! spawned on the first cell and re-spawned after a cell-induced
//! exit (`emit_panic`'s `exit(1)`, runtime panics) terminates the
//! runner.
//!
//! Lookup path for the runner binary:
//!   1. `KARAC_JIT_RUNNER` env var (lets tests / dev shells override).
//!   2. Sibling of `std::env::current_exe()` named `karac_jit_runner`
//!      (the cargo build layout + the `karac install` step that copies
//!      both binaries into the same install dir).

#![cfg(feature = "lljit_prototype")]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};

/// Outcome of one `run_cell` call. `Completed` is the framed-response
/// case (the runner survived); `RunnerDied` fires when stdout closed
/// before a complete response arrived (the cell's JIT'd code called
/// `exit()` from inside the runner). Caller discards the client and
/// re-spawns to continue.
pub enum CellResult {
    Completed {
        exit: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    RunnerDied {
        /// Bytes the runner managed to write before dying — usually
        /// empty, but non-empty when a panic happened after the
        /// `result` header started flushing.
        partial_stdout: Vec<u8>,
        /// Bytes from the runner's own stderr (the unframed
        /// diagnostic channel, separate from cell stderr). Useful
        /// when a `karac_jit_runner: ...` message explains the
        /// failure path.
        runner_stderr: Vec<u8>,
        /// The dead child's exit status, when reapable.
        wait_status: Option<ExitStatus>,
    },
}

pub struct ReplRunnerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ReplRunnerClient {
    /// Spawn a fresh runner subprocess and read its `ready\n` banner.
    /// Returns `Err(message)` if the binary can't be located or the
    /// process can't be started.
    pub fn spawn() -> Result<Self, String> {
        let path = locate_runner_binary().ok_or_else(|| {
            "karac_jit_runner binary not found — set KARAC_JIT_RUNNER or \
             ensure karac was installed with --features lljit_prototype"
                .to_string()
        })?;
        let mut child = Command::new(&path)
            .arg("--repl-mode")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", path.display()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "spawned runner has no stdin".to_string())?;
        let mut stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| "spawned runner has no stdout".to_string())?,
        );
        let mut ready = String::new();
        match stdout.read_line(&mut ready) {
            Ok(n) if n > 0 && ready.trim() == "ready" => {}
            Ok(_) => {
                return Err(format!(
                    "expected 'ready' banner from runner, got {ready:?}"
                ));
            }
            Err(e) => return Err(format!("read ready banner: {e}")),
        }
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    /// Run one cell. Sends `cell <id> <ir_byte_count>\n<ir>` on stdin
    /// and reads the framed `result <id> <exit> <out_len> <err_len>\n
    /// <out><err>` response. EOF or framing error → `RunnerDied`; the
    /// caller drops the client and re-spawns.
    pub fn run_cell(&mut self, id: u64, ir: &str) -> CellResult {
        let header = format!("cell {} {}\n", id, ir.len());
        if self.stdin.write_all(header.as_bytes()).is_err()
            || self.stdin.write_all(ir.as_bytes()).is_err()
            || self.stdin.flush().is_err()
        {
            return self.collect_died();
        }

        let mut header_line = String::new();
        let n = match self.stdout.read_line(&mut header_line) {
            Ok(n) => n,
            Err(_) => return self.collect_died(),
        };
        if n == 0 {
            return self.collect_died();
        }
        let trimmed = header_line.trim_end_matches(['\r', '\n']);
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() != 5 || parts[0] != "result" {
            return self.collect_died();
        }
        let echoed_id: u64 = match parts[1].parse() {
            Ok(v) => v,
            Err(_) => return self.collect_died(),
        };
        let exit: i32 = match parts[2].parse() {
            Ok(v) => v,
            Err(_) => return self.collect_died(),
        };
        let stdout_len: usize = match parts[3].parse() {
            Ok(v) => v,
            Err(_) => return self.collect_died(),
        };
        let stderr_len: usize = match parts[4].parse() {
            Ok(v) => v,
            Err(_) => return self.collect_died(),
        };
        if echoed_id != id {
            // Framing out of sync — treat as a runner-died case
            // rather than risking misinterpretation of subsequent
            // cells.
            return self.collect_died();
        }

        let mut stdout_buf = vec![0u8; stdout_len];
        if self.stdout.read_exact(&mut stdout_buf).is_err() {
            return self.collect_died();
        }
        let mut stderr_buf = vec![0u8; stderr_len];
        if self.stdout.read_exact(&mut stderr_buf).is_err() {
            return self.collect_died();
        }
        CellResult::Completed {
            exit,
            stdout: stdout_buf,
            stderr: stderr_buf,
        }
    }

    /// Best-effort `quit\n` send + bounded `wait()`. Currently
    /// unused — Session doesn't have a Drop hook that wires this in.
    /// On Session drop the stdin pipe closes, the runner reads EOF
    /// and exits naturally, which is graceful enough for v1. Reserved
    /// for future use (signal handlers, explicit `:quit` meta).
    #[allow(dead_code)]
    pub fn quit(mut self) {
        let _ = self.stdin.write_all(b"quit\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break,
            }
        }
    }

    fn collect_died(&mut self) -> CellResult {
        // Drain whatever's left on stdout (a partial header / response
        // bytes can be useful in diagnostics).
        let mut partial_stdout = Vec::new();
        let _ = self.stdout.read_to_end(&mut partial_stdout);
        // Drain the unframed stderr channel — runner-level
        // diagnostics live here.
        let mut runner_stderr = Vec::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_end(&mut runner_stderr);
        }
        let wait_status = self.child.wait().ok();
        CellResult::RunnerDied {
            partial_stdout,
            runner_stderr,
            wait_status,
        }
    }
}

fn locate_runner_binary() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("KARAC_JIT_RUNNER") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }
    let karac_exe = std::env::current_exe().ok()?;
    let dir = karac_exe.parent()?;
    let candidate = dir.join("karac_jit_runner");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}
