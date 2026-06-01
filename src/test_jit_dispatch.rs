//! `karac test` JIT dispatch — slice c.3.
//!
//! Wires the existing `cmd_test` per-test loop to a JIT-subprocess
//! execution path when `KARAC_TEST_JIT=1` is set and the binary was
//! built with the `lljit_prototype` feature. Each test runs as its own
//! `karac_jit_runner` subprocess; outcomes are mapped from the
//! subprocess's exit code + stderr (parsed for the `KARAC_TEST_FAILURE`
//! JSONL marker emitted by slice c.1's `karac_test_record_failure`
//! runtime fn).
//!
//! Per-test compile pipeline:
//!   parse-already-done
//!     → clone the module's items
//!     → `test_main_synth::append_test_main(...)` with the per-test
//!       fixtures
//!     → re-resolve + re-typecheck + re-lower (the synthesized `let
//!       __karac_test_provider_N = ctor;` bindings need typecheck to
//!       populate `var_type_names` for codegen's
//!       `infer_provider_type_name`; without this the
//!       `with_provider[R](...)` lowering rejects the call)
//!     → `compile_to_ir_with_options` → IR string
//!     → write to a tempfile
//!     → spawn `karac_jit_runner` with the IR path
//!     → capture stdout / stderr / exit code
//!     → parse stderr for `KARAC_TEST_FAILURE` JSONL → `TestOutcome`
//!
//! Pre-c.3 the slice-c.4 hang-watchdog stays out of scope; this module
//! uses `Command::output` directly. A hung test runs to completion or
//! kills the karac process; the watchdog wrap goes on in c.4 alongside
//! the per-test deadline plumbing.

#![cfg(feature = "lljit_prototype")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::ast::{Expr, Program};
use crate::interpreter::{RuntimeError, TestOutcome};
use crate::test_main_synth::{append_test_main, ProviderFixture};
use crate::token::Span;

/// Outcome of a single JIT-dispatched test run.
#[derive(Debug)]
pub enum JitTestResult {
    /// The subprocess executed to completion; `outcome` is mapped from
    /// the exit code + stderr `KARAC_TEST_FAILURE` marker.
    Completed {
        outcome: TestOutcome,
        duration_ms: u128,
    },
    /// The subprocess timed out (the c.4 watchdog will populate this;
    /// for c.3's initial form the variant exists but is never produced).
    TimedOut { duration_ms: u128 },
    /// Setup-side failure — codegen rejected the per-test program, the
    /// IR tempfile could not be written, or `karac_jit_runner` could
    /// not be located. Surfaces as a `test_fail` event with the
    /// returned message.
    SpawnFailed { message: String },
}

/// Run one test via the JIT subprocess path.
///
/// `module_program` is the per-module `Program` built by the runner
/// (matches what's passed to `Interpreter::new` in the interpreter path).
/// `fixtures` mirrors the runner's `t.with_providers` after
/// `extract_with_providers` has parsed the `#[with_provider(R, ctor)]`
/// attribute payloads.
pub fn run_test_via_jit(
    module_program: &Program,
    test_fn_name: &str,
    fixtures: &[(String, Expr)],
    source_filename: &str,
    timeout: Duration,
) -> JitTestResult {
    let runner_path = match locate_karac_jit_runner() {
        Some(p) => p,
        None => {
            return JitTestResult::SpawnFailed {
                message: "karac_jit_runner binary not found alongside karac executable — \
                          rebuild karac with `--features lljit_prototype` so cargo emits \
                          the runner alongside the main binary"
                    .to_string(),
            };
        }
    };

    let fixtures_vec: Vec<ProviderFixture> = fixtures
        .iter()
        .map(|(rp, ctor)| ProviderFixture {
            resource_path: rp.clone(),
            constructor: ctor.clone(),
        })
        .collect();

    let mut per_test_program = clone_program_items(module_program);
    append_test_main(&mut per_test_program, test_fn_name, &fixtures_vec);

    let resolved = crate::resolver::Resolver::new(&per_test_program).resolve();
    let typed = crate::typechecker::TypeChecker::new(&per_test_program, &resolved).check();
    crate::lowering::lower_program(&mut per_test_program, &typed);

    let ir = match crate::codegen::compile_to_ir_with_options(
        &per_test_program,
        None,
        None,
        Some(source_filename),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            return JitTestResult::SpawnFailed {
                message: format!("codegen failed for test '{test_fn_name}': {e}"),
            };
        }
    };

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ir_path: PathBuf =
        std::env::temp_dir().join(format!("karac_test_jit_{}_{}.ll", std::process::id(), id));
    if let Err(e) = std::fs::write(&ir_path, ir) {
        return JitTestResult::SpawnFailed {
            message: format!("could not write IR tempfile {}: {e}", ir_path.display()),
        };
    }

    let mut cmd = std::process::Command::new(&runner_path);
    cmd.arg(&ir_path);

    let started = std::time::Instant::now();
    let sub_result = run_subprocess_with_timeout(cmd, timeout);
    let duration_ms = started.elapsed().as_millis();
    let _ = std::fs::remove_file(&ir_path);

    match sub_result {
        SubprocessResult::Completed(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let outcome = map_exit_to_outcome(exit_code, &stdout, &stderr);
            JitTestResult::Completed {
                outcome,
                duration_ms,
            }
        }
        SubprocessResult::TimedOut => JitTestResult::TimedOut { duration_ms },
        SubprocessResult::SpawnFailed(message) => JitTestResult::SpawnFailed { message },
    }
}

/// Internal subprocess-result shape — `run_subprocess_with_timeout`
/// returns one of these; `run_test_via_jit` maps each variant to the
/// equivalent `JitTestResult`.
enum SubprocessResult {
    Completed(std::process::Output),
    TimedOut,
    SpawnFailed(String),
}

/// Spawn a subprocess and wait for it with a hard timeout. Mirrors the
/// `tests/common/mod.rs::output_with_hang_watchdog` shape but returns a
/// structured result instead of panicking on timeout — the runner's
/// `test_timeout` JSONL event captures the user-visible signal.
///
/// stdin is piped from /dev/null; stdout/stderr are captured. On
/// timeout the watchdog kills the child via `kill -9` so the parent's
/// `wait_with_output` returns immediately. The kill is observable as
/// a non-zero status on the returned `Output` when `Completed` fires
/// — but the `killed` flag is what disambiguates from a regular
/// non-zero exit, so we return `TimedOut` specifically.
fn run_subprocess_with_timeout(
    mut cmd: std::process::Command,
    timeout: Duration,
) -> SubprocessResult {
    use std::process::Stdio;
    use std::sync::mpsc;

    let child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return SubprocessResult::SpawnFailed(format!("could not spawn child: {e}")),
    };
    let pid = child.id();

    let (tx, rx) = mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(timeout).is_err() {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
            true
        } else {
            false
        }
    });

    let output = child.wait_with_output();
    let _ = tx.send(());
    let killed = watchdog.join().unwrap_or(false);

    match output {
        Ok(_) if killed => SubprocessResult::TimedOut,
        Ok(o) => SubprocessResult::Completed(o),
        Err(e) => SubprocessResult::SpawnFailed(format!("wait_with_output failed: {e}")),
    }
}

/// Clone a `Program` by copying its items vector. Other fields use
/// `Default` — every late-phase consumer of `Program` reads only
/// `items` (see `cli.rs`'s per-module program build at the same spot).
fn clone_program_items(p: &Program) -> Program {
    Program {
        items: p.items.clone(),
        ..Program::default()
    }
}

/// Look for `karac_jit_runner` in the same directory as the current
/// `karac` executable. Cargo writes both binaries next to each other
/// (target/release/karac, target/release/karac_jit_runner); installed
/// `karac` users get them paired through the same install step (the
/// `reference_karac_install_path` memory pins how this is done).
fn locate_karac_jit_runner() -> Option<PathBuf> {
    let karac_exe = std::env::current_exe().ok()?;
    let dir = karac_exe.parent()?;
    let candidate = dir.join("karac_jit_runner");
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Map exit code + stderr to a `TestOutcome`. Exit 0 → pass. Any
/// non-zero exit with a `KARAC_TEST_FAILURE ` line on stderr → parse
/// the JSON payload into the outcome fields. Non-zero exit without a
/// marker → a synthetic outcome with a generic message (the subprocess
/// died for some other reason — a runtime panic the assert lowering
/// didn't emit a marker for, or a setup-side abort).
fn map_exit_to_outcome(exit_code: i32, stdout: &str, stderr: &str) -> TestOutcome {
    if exit_code == 0 {
        return TestOutcome {
            passed: true,
            message: None,
            span: None,
            left: None,
            right: None,
        };
    }
    if let Some(parsed) = parse_failure_marker(stderr) {
        return TestOutcome {
            passed: false,
            message: Some(parsed.message),
            span: Some(parsed.span),
            left: parsed.left,
            right: parsed.right,
        };
    }
    // Contract faults (`requires`/`ensures`/`invariant`) abort through
    // `emit_panic` (a `printf` — i.e. to **stdout** — + `exit(1)`), NOT
    // through the `assert` lowering's `KARAC_TEST_FAILURE` stderr marker,
    // so they reach here with no marker. Recover the panic message off
    // stdout so the shared `contract_fault_category` classifier (cli.rs)
    // can tag the `test_fail` event `contract_violated` /
    // `contract_predicate_panicked` exactly as the interpreter path does.
    // Without this the category is lost and the outcome is a generic
    // "exited with code N".
    if let Some(parsed) = parse_panic_line(stdout) {
        return TestOutcome {
            passed: false,
            message: Some(parsed.message),
            span: Some(parsed.span),
            left: None,
            right: None,
        };
    }
    TestOutcome {
        passed: false,
        message: Some(format!("test subprocess exited with code {exit_code}")),
        span: None,
        left: None,
        right: None,
    }
}

/// Recover a panic message + location from `emit_panic`'s stdout output
/// (`emit_panic` uses `printf`, which writes to stdout, not stderr).
/// `emit_panic` (src/codegen/runtime.rs) prints one of two fixed forms:
///   `panic at <file>:<line>:<col> in <fn>: <msg>`  (filename threaded —
///       the `karac test` codegen path always supplies one)
///   `panic: <msg>`                                  (no filename)
/// `<msg>` carries the canonical fault text (`contract violated: …`,
/// `contract predicate panicked: …`) that `contract_fault_category`
/// matches on. We scan for the `panic ` prefix specifically so the
/// runtime's `?`-error-trace lines on stderr aren't misread as panics.
fn parse_panic_line(stderr: &str) -> Option<ParsedFailure> {
    let line = stderr
        .lines()
        .find(|l| l.starts_with("panic at ") || l.starts_with("panic: "))?;
    if let Some(rest) = line.strip_prefix("panic at ") {
        // rest = "<file>:<line>:<col> in <fn>: <msg>". Split the message
        // off after the " in <fn>: " segment (fn names are identifiers,
        // so the first ": " after " in " starts the message).
        if let Some(in_idx) = rest.find(" in ") {
            let loc = &rest[..in_idx];
            let after_in = &rest[in_idx + 4..];
            let message = after_in
                .split_once(": ")
                .map(|x| x.1)
                .unwrap_or(after_in)
                .to_string();
            return Some(ParsedFailure {
                message,
                span: parse_panic_loc(loc),
                left: None,
                right: None,
            });
        }
    }
    // `panic: <msg>` form (or an unexpected `panic at` shape) — take the
    // text after the first ": " as the message, no location.
    let message = line
        .split_once(": ")
        .map(|x| x.1)
        .unwrap_or(line)
        .to_string();
    Some(ParsedFailure {
        message,
        span: Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        },
        left: None,
        right: None,
    })
}

/// Parse `<file>:<line>:<col>` into a `Span` (line/col only). Splits from
/// the right so a file path is unaffected by the two trailing numeric
/// fields; a path containing `:` would only blunt the location, never
/// misclassify the fault.
fn parse_panic_loc(loc: &str) -> Span {
    let mut it = loc.rsplitn(3, ':');
    let column = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let line = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    Span {
        line,
        column,
        offset: 0,
        length: 0,
    }
}

#[derive(Debug)]
struct ParsedFailure {
    message: String,
    span: Span,
    left: Option<String>,
    right: Option<String>,
}

/// Scan `stderr` for a `KARAC_TEST_FAILURE {...JSON...}` line and parse
/// the trailing JSON. Tolerant of multiple markers (record-and-continue
/// semantics aren't on by default in c.1, but if a future codegen
/// emits two markers, the first one wins — matches the interpreter's
/// `runtime_errors.first()` semantics).
fn parse_failure_marker(stderr: &str) -> Option<ParsedFailure> {
    const PREFIX: &str = "KARAC_TEST_FAILURE ";
    let payload = stderr.lines().find_map(|line| line.strip_prefix(PREFIX))?;
    parse_failure_payload(payload)
}

/// Parse the JSON payload `{"file":"...","line":N,"column":N,"message":"...","left":...,"right":...}`.
/// Hand-rolled rather than `serde_json` to avoid a karac dep on
/// serde just for this — the runtime's `write_json_string` produces
/// the only writer, so the field set + ordering is fixed.
fn parse_failure_payload(payload: &str) -> Option<ParsedFailure> {
    // `file` field is intentionally not read here — the test runner
    // already knows the file path from `module.test_file` and threads
    // it into the `test_fail` event from there. We still require it to
    // be present in the marker (round-trip integrity check) but discard
    // the value.
    let _file = extract_json_string(payload, "\"file\"")?;
    let line = extract_json_number(payload, "\"line\"")? as usize;
    let column = extract_json_number(payload, "\"column\"")? as usize;
    let message = extract_json_string(payload, "\"message\"")?;
    let left = extract_json_string_or_null(payload, "\"left\"");
    let right = extract_json_string_or_null(payload, "\"right\"");
    Some(ParsedFailure {
        message,
        span: Span {
            line,
            column,
            offset: 0,
            length: 0,
        },
        left,
        right,
    })
}

/// Find `key:"<value>"` and return the unescaped value. Mirrors the
/// runtime's `write_json_string` escapes (the only producer): `\"`,
/// `\\`, `\n`, `\r`, `\t`, `\u00XX`.
fn extract_json_string(payload: &str, key: &str) -> Option<String> {
    let key_pos = payload.find(key)?;
    let after_key = &payload[key_pos + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let mut out = String::new();
    let mut chars = after_colon[1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                let esc = chars.next()?;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        let code = u32::from_str_radix(&hex, 16).ok()?;
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            other => out.push(other),
        }
    }
    None
}

/// Variant of `extract_json_string` that accepts a literal `null` as a
/// valid value. Used for the `left` / `right` slots on the failure
/// marker — bare `assert(cond)` failures emit them as null.
fn extract_json_string_or_null(payload: &str, key: &str) -> Option<String> {
    let key_pos = payload.find(key)?;
    let after_key = &payload[key_pos + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    if after_colon.starts_with("null") {
        return None;
    }
    extract_json_string(payload, key)
}

fn extract_json_number(payload: &str, key: &str) -> Option<u64> {
    let key_pos = payload.find(key)?;
    let after_key = &payload[key_pos + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    let end = after_colon
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_colon.len());
    after_colon[..end].parse::<u64>().ok()
}

/// Stand-in to silence the unused-import lint when this module compiles
/// against a build that doesn't currently reference `RuntimeError` from
/// outside. Kept around so future expansion (mapping runtime panics
/// into structured outcomes) has the import already wired.
#[allow(dead_code)]
fn _force_runtime_error_import() -> Option<RuntimeError> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_failure_marker() {
        let stderr = "KARAC_TEST_FAILURE {\"file\":\"x.kara\",\"line\":3,\"column\":5,\"message\":\"assertion failed: left != right\",\"left\":\"1\",\"right\":\"2\"}\n";
        let p = parse_failure_marker(stderr).expect("expected to parse marker");
        assert_eq!(p.message, "assertion failed: left != right");
        assert_eq!(p.span.line, 3);
        assert_eq!(p.span.column, 5);
        assert_eq!(p.left.as_deref(), Some("1"));
        assert_eq!(p.right.as_deref(), Some("2"));
    }

    #[test]
    fn parses_null_left_right() {
        let stderr = "KARAC_TEST_FAILURE {\"file\":\"x.kara\",\"line\":2,\"column\":5,\"message\":\"assertion failed\",\"left\":null,\"right\":null}\n";
        let p = parse_failure_marker(stderr).expect("expected to parse marker");
        assert!(p.left.is_none());
        assert!(p.right.is_none());
    }

    #[test]
    fn unescapes_json_strings() {
        let stderr = "KARAC_TEST_FAILURE {\"file\":\"x\\nz\",\"line\":1,\"column\":1,\"message\":\"with \\\"quotes\\\"\",\"left\":null,\"right\":null}\n";
        let p = parse_failure_marker(stderr).expect("expected to parse marker");
        assert_eq!(p.message, "with \"quotes\"");
    }

    #[test]
    fn no_marker_yields_none() {
        let stderr = "some unrelated stderr noise\n";
        assert!(parse_failure_marker(stderr).is_none());
    }

    #[test]
    fn map_exit_zero_is_pass() {
        let o = map_exit_to_outcome(0, "", "");
        assert!(o.passed);
    }

    #[test]
    fn map_nonzero_no_marker_is_generic_fail() {
        let o = map_exit_to_outcome(2, "", "");
        assert!(!o.passed);
        assert_eq!(
            o.message.as_deref().unwrap(),
            "test subprocess exited with code 2"
        );
    }

    #[test]
    fn contract_violation_panic_on_stdout_recovers_message() {
        // A contract fault aborts via `emit_panic` (printf → stdout),
        // not the `KARAC_TEST_FAILURE` stderr marker. The panic line must
        // be recovered as the message + span so `contract_fault_category`
        // (cli.rs) can tag the event `contract_violated`.
        let stdout = "panic at /tmp/p/src/main_test.kara:2:40 in checked: contract violated: requires clause\n";
        let o = map_exit_to_outcome(1, stdout, "");
        assert!(!o.passed);
        assert_eq!(
            o.message.as_deref().unwrap(),
            "contract violated: requires clause"
        );
        let span = o.span.expect("span recovered from panic location");
        assert_eq!((span.line, span.column), (2, 40));
    }

    #[test]
    fn predicate_panic_on_stdout_preserves_panicked_prefix() {
        // Predicate-panic carries the `contract predicate panicked:`
        // prefix (set at runtime by `karac_runtime_panic_prefix`); the
        // recovered message must keep it so the category resolves to
        // `contract_predicate_panicked`, not `contract_violated`.
        let stdout = "panic at /tmp/p/src/main_test.kara:5:9 in at: contract predicate panicked: vec index out of bounds\n";
        let o = map_exit_to_outcome(1, stdout, "");
        assert_eq!(
            o.message.as_deref().unwrap(),
            "contract predicate panicked: vec index out of bounds"
        );
    }

    #[test]
    fn stderr_marker_wins_over_stdout_panic() {
        // When both a `KARAC_TEST_FAILURE` stderr marker and stdout text
        // are present, the marker (assert lowering) takes precedence.
        let stderr = "KARAC_TEST_FAILURE {\"file\":\"f\",\"line\":1,\"column\":2,\"message\":\"assert_eq failed\",\"left\":\"1\",\"right\":\"2\"}\n";
        let o = map_exit_to_outcome(1, "panic at f:1:2 in g: contract violated: x\n", stderr);
        assert_eq!(o.message.as_deref().unwrap(), "assert_eq failed");
        assert_eq!(o.left.as_deref(), Some("1"));
    }
}
