//! Slice c-repl.B.A — integration test for `karac_jit_runner --repl-mode`.
//!
//! Drives the framed-cell protocol from the parent side: spawn the
//! runner, send a couple of cells (compiled Kāra programs converted
//! to IR via the standard pipeline), and assert that the framed
//! responses carry the expected exit code + captured stdout. This
//! exercises everything `karac repl`'s JIT path will eventually use:
//! engine persistence, per-cell ResourceTracker install, dup2-based
//! stdout capture, response framing.

#![cfg(feature = "llvm")]

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
fn repl_runner_salvages_output_when_cell_calls_exit() {
    // A cell whose JIT'd `main` reaches `emit_panic` (assert failure, OOB
    // index, runtime panic, contract violation) calls libc's `exit(1)` from
    // inside the JIT'd code. That still terminates the runner subprocess —
    // but the runner now installs an `atexit` handler (B-2026-07-09-4) that,
    // before the process dies, flushes and frames the cell's CAPTURED output
    // as a `faulted <id> <exit> <stdout_len> <stderr_len>\n<bytes>` frame on
    // stdout. So instead of a silent EOF, the parent receives the fault
    // output (the `panic …` message + any pre-fault prints) and THEN EOF.
    // The REPL-side client (`jit_runner_client.rs`) maps `faulted` onto its
    // runner-died path: surface the salvaged output, reap, re-spawn.
    //
    // This test pins the salvage-frame shape (it superseded the older
    // "framed response never arrives" pin, which this test used to assert —
    // the atexit-handler future-work note in that pin is now the present).
    let mut runner = ReplRunner::spawn();
    let ir = lower_kara_to_ir(
        r#"
fn main() {
    assert_eq(1, 2);
}
"#,
    );
    runner.send_cell(7, &ir);

    // A `faulted` frame arrives before EOF. Parse it by hand (read_result
    // asserts a `result` verb).
    let mut header = String::new();
    runner
        .stdout
        .read_line(&mut header)
        .expect("read faulted header");
    let trimmed = header.trim_end_matches(['\r', '\n']);
    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    assert_eq!(parts.len(), 5, "faulted header parts: {trimmed:?}");
    assert_eq!(parts[0], "faulted", "expected a `faulted` salvage frame");
    assert_eq!(parts[1], "7", "echoed cell id");
    assert_eq!(parts[2], "1", "salvage frames the emit_panic exit code (1)");
    let stdout_len: usize = parts[3].parse().expect("stdout_len");
    let stderr_len: usize = parts[4].parse().expect("stderr_len");
    let mut cell_stdout = vec![0u8; stdout_len];
    runner
        .stdout
        .read_exact(&mut cell_stdout)
        .expect("read salvaged stdout");
    let mut cell_stderr = vec![0u8; stderr_len];
    runner
        .stdout
        .read_exact(&mut cell_stderr)
        .expect("read salvaged stderr");
    // The fault text must survive the salvage — an assert failure lands on
    // stderr (the `KARAC_TEST_FAILURE` marker path); an OOB/contract panic's
    // `panic …` line lands on stdout. Assert the union is non-empty and
    // carries a recognizable fault token.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&cell_stdout),
        String::from_utf8_lossy(&cell_stderr)
    );
    assert!(
        !combined.is_empty()
            && (combined.contains("panic")
                || combined.contains("assert")
                || combined.contains("KARAC_TEST_FAILURE")),
        "salvaged fault output should carry the fault text; got {combined:?}"
    );

    // EOF follows the frame — the runner is dead.
    let mut tail = String::new();
    let n = runner
        .stdout
        .read_line(&mut tail)
        .expect("read after salvage");
    assert_eq!(n, 0, "expected EOF after the faulted frame; got {tail:?}");

    // Reap the dead child. Bounded wait so a hang shows up as a failure.
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
    assert_eq!(
        status.code(),
        Some(1),
        "expected runner exit code 1 (matching cell's emit_panic), got {status:?}"
    );
}
