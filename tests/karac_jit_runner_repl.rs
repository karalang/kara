//! Slice c-repl.B.A — integration test for `karac_jit_runner --repl-mode`.
//!
//! Drives the framed-cell protocol from the parent side: spawn the
//! runner, send a couple of cells (compiled Kāra programs converted
//! to IR via the standard pipeline), and assert that the framed
//! responses carry the expected exit code + captured stdout. This
//! exercises everything `karac repl`'s JIT path will eventually use:
//! engine persistence, per-cell ResourceTracker install, dup2-based
//! stdout capture, response framing.

#![cfg(feature = "lljit_prototype")]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Lower a kara source string to LLVM IR for slice-B.A single-cell
/// tests. Uses the one-shot codegen entry that emits the literal
/// `main` symbol — appropriate for tests that send exactly one cell
/// to the runner.
fn lower_kara_to_ir(src: &str) -> String {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    karac::codegen::compile_to_ir_with_options(&parsed.program, None, None, None, None)
        .expect("compile_to_ir")
}

/// Slice c-repl.B.4 codegen entry — used by multi-cell tests so each
/// cell's `fn main()` registers under `cell_main_<id>` rather than
/// the literal `main`. Without this, cell 2's install would fail
/// with a duplicate-symbol error against cell 1's still-installed
/// `main` (B.4 removed the tracker shadowing that the slice-B.A
/// protocol leaned on; cells now coexist via unique entry names).
fn lower_kara_to_ir_for_cell(src: &str, cell_id: u64) -> String {
    let mut parsed = karac::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let main_symbol = format!("cell_main_{cell_id}");
    karac::codegen::compile_to_ir_for_repl_cell(
        &parsed.program,
        &std::collections::HashSet::new(),
        &main_symbol,
    )
    .expect("compile_to_ir_for_repl_cell")
}

struct ReplRunner {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl ReplRunner {
    fn spawn() -> Self {
        let path = env!("CARGO_BIN_EXE_karac_jit_runner");
        let mut child = Command::new(path)
            .arg("--repl-mode")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn karac_jit_runner --repl-mode");
        let stdin = child.stdin.take().expect("take stdin");
        let stdout = BufReader::new(child.stdout.take().expect("take stdout"));
        let mut runner = Self {
            child,
            stdin,
            stdout,
        };
        // Read the `ready\n` line the runner emits after engine init.
        let mut line = String::new();
        runner.stdout.read_line(&mut line).expect("read ready");
        assert_eq!(line.trim(), "ready", "expected 'ready' banner");
        runner
    }

    fn send_cell(&mut self, id: u64, ir: &str) {
        let header = format!("cell {} {}\n", id, ir.len());
        self.stdin
            .write_all(header.as_bytes())
            .expect("write header");
        self.stdin.write_all(ir.as_bytes()).expect("write ir");
        self.stdin.flush().expect("flush stdin");
    }

    /// Read the next framed result. Returns `(id, exit, stdout, stderr)`.
    fn read_result(&mut self) -> (u64, i32, Vec<u8>, Vec<u8>) {
        let mut header = String::new();
        self.stdout
            .read_line(&mut header)
            .expect("read result header");
        let trimmed = header.trim_end_matches(['\r', '\n']);
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        assert_eq!(parts.len(), 5, "header parts: {trimmed:?}");
        assert_eq!(parts[0], "result");
        let id: u64 = parts[1].parse().expect("id");
        let exit: i32 = parts[2].parse().expect("exit");
        let stdout_len: usize = parts[3].parse().expect("stdout_len");
        let stderr_len: usize = parts[4].parse().expect("stderr_len");
        let mut stdout = vec![0u8; stdout_len];
        self.stdout
            .read_exact(&mut stdout)
            .expect("read stdout bytes");
        let mut stderr = vec![0u8; stderr_len];
        self.stdout
            .read_exact(&mut stderr)
            .expect("read stderr bytes");
        (id, exit, stdout, stderr)
    }

    fn quit(mut self) {
        let _ = self.stdin.write_all(b"quit\n");
        let _ = self.stdin.flush();
        drop(self.stdin);
        // Bounded wait so a stuck runner can't hang the test suite.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break,
            }
        }
    }
}

#[test]
fn repl_runner_passes_one_cell() {
    let mut runner = ReplRunner::spawn();
    let ir = lower_kara_to_ir(
        r#"
fn main() {
    println(42);
}
"#,
    );
    runner.send_cell(1, &ir);
    let (id, exit, stdout, _stderr) = runner.read_result();
    assert_eq!(id, 1);
    assert_eq!(exit, 0);
    assert_eq!(
        String::from_utf8_lossy(&stdout).trim(),
        "42",
        "expected captured '42' on stdout"
    );
    runner.quit();
}

#[test]
fn repl_runner_handles_multiple_cells() {
    // Two cells in one session — exercises engine persistence across
    // cells. Each cell is its own independent program (no shared
    // state); the next slice extends this with cross-cell symbol
    // visibility.
    let mut runner = ReplRunner::spawn();

    let ir1 = lower_kara_to_ir_for_cell(
        r#"
fn main() {
    println(1);
}
"#,
        1,
    );
    runner.send_cell(1, &ir1);
    let (id, exit, stdout, _) = runner.read_result();
    assert_eq!(id, 1);
    assert_eq!(exit, 0);
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), "1");

    let ir2 = lower_kara_to_ir_for_cell(
        r#"
fn main() {
    println(2);
}
"#,
        2,
    );
    runner.send_cell(2, &ir2);
    let (id, exit, stdout, _) = runner.read_result();
    assert_eq!(id, 2);
    assert_eq!(exit, 0);
    assert_eq!(String::from_utf8_lossy(&stdout).trim(), "2");

    runner.quit();
}

#[test]
fn repl_runner_dies_when_cell_calls_exit() {
    // Documented B.A limitation: a cell whose JIT'd `main` reaches
    // `emit_panic` (assert failure, OOB index, runtime panic, etc.)
    // calls libc's `exit(1)` from inside the JIT'd code itself. That
    // terminates the runner subprocess, the parent reads EOF on
    // stdout, no framed response arrives. The REPL-side integration
    // (slice B.B) handles this by detecting the EOF, joining the
    // child process, and re-spawning a fresh runner plus replaying
    // prior cells if it wants to continue.
    //
    // This test pins that "framed response never arrives" shape so
    // future work knows when it has shifted (e.g. if `emit_panic` is
    // ever rewritten to unwind instead of `exit`, or if the runner
    // grows an `atexit` handler that flushes a pre-exit framed
    // response).
    let mut runner = ReplRunner::spawn();
    let ir = lower_kara_to_ir(
        r#"
fn main() {
    assert_eq(1, 2);
}
"#,
    );
    runner.send_cell(7, &ir);
    // The runner's stdout closes on exit; reading produces EOF. We
    // check by reading raw bytes and asserting we got 0 bytes (rather
    // than calling `read_result`, which assumes a valid header).
    let mut hdr = String::new();
    let n = runner.stdout.read_line(&mut hdr).expect("read after exit");
    assert_eq!(
        n, 0,
        "expected EOF on runner stdout after cell exit(1); got partial header: {hdr:?}"
    );
    // Reap the dead child. Bounded wait so a hang shows up as a
    // failure rather than a CI lock-up.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let status = loop {
        match runner.child.try_wait().expect("try_wait") {
            Some(s) => break s,
            None if std::time::Instant::now() >= deadline => {
                runner.child.kill().ok();
                panic!("runner did not exit within 5s after cell's exit(1)");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    // Cell's exit(1) propagates through dup2-restored fds; the
    // runner's main() doesn't reach its own return path, so the child
    // exit code matches the cell's exit code.
    assert_eq!(
        status.code(),
        Some(1),
        "expected runner exit code 1 (matching cell's emit_panic), got {status:?}"
    );
}
