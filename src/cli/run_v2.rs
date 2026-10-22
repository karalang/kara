//! `karac run` on v2 (M3 item 1, B-2026-10-22-12): the program is checked as
//! `karac check` checks it, built to MIR and run on the mir-llvm JIT, or on the
//! MIR interpreter under `--interp`. `--legacy` (or `KARAC_LEGACY=1`) keeps the
//! legacy pipeline reachable until M3 deletes it.

use super::*;

/// `--legacy`, or `KARAC_LEGACY=1` for a harness that cannot add a flag to
/// every command it spawns.
pub(super) fn legacy_requested(flag: bool) -> bool {
    flag || std::env::var("KARAC_LEGACY").as_deref() == Ok("1")
}

/// `karac run` on v2, or on the legacy pipeline when it was asked for.
#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_run_either(
    legacy: bool,
    filename: &str,
    output: OutputMode,
    sequential: bool,
    manifest_override: Option<&str>,
    no_manifest: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    timeout: Option<std::time::Duration>,
    interp: bool,
    program_args: &[String],
) {
    if legacy_requested(legacy) {
        return cmd_run(
            filename,
            output,
            sequential,
            manifest_override,
            no_manifest,
            lint_overrides,
            timeout,
            interp,
            program_args,
        );
    }
    cmd_run_v2(
        filename,
        output,
        sequential,
        manifest_override,
        no_manifest,
        lint_overrides,
        timeout,
        interp,
        program_args,
    );
}

#[allow(clippy::too_many_arguments)]
fn cmd_run_v2(
    filename: &str,
    output: OutputMode,
    sequential: bool,
    manifest_override: Option<&str>,
    no_manifest: bool,
    lint_overrides: crate::lints::CliLintOverrides,
    timeout: Option<std::time::Duration>,
    interp: bool,
    program_args: &[String],
) {
    // A `par` branch is a task of its own on v2 (core-semantics.md §11.7), so
    // running the branches one after another is not the same program: a
    // branch that waits on a later one would never finish.
    if sequential {
        emit_build_error(
            "--sequential runs `par` branches one after another, which only the legacy \
             pipeline does; on v2 each branch is a task (core-semantics.md §11.7). \
             Use `karac run --legacy --sequential` until M3 deletes it",
            output,
        );
        process::exit(1);
    }
    // The program's `dbg` writes JSON under a structured output mode; the
    // runtime and the MIR interpreter both read this.
    if matches!(output, OutputMode::Json | OutputMode::Jsonl) {
        std::env::set_var("KARAC_DBG_OUTPUT", "json");
    }
    let manifest = run_manifest_preamble(filename, output, manifest_override, no_manifest);
    // v2 builds one file. A program with sibling modules needs project mode,
    // which `karac build` gains in B-2026-10-22-14.
    if try_build_run_super_program(filename, no_manifest).is_some() {
        emit_build_error(
            &format!(
                "{filename} belongs to a package with other modules, which v2 does not \
                 run yet (project mode, B-2026-10-22-14); `karac run --legacy` runs it"
            ),
            output,
        );
        process::exit(1);
    }

    let source = read_source(filename);
    let mut lint_overrides = lint_overrides;
    if let Some(m) = &manifest {
        lint_overrides.apply_manifest_lints(&m.lints);
    }
    // The checks `karac check` runs: an error there fails the run too, as it
    // fails `karac build --mir` (core semantics §0).
    let checked = || {
        let mut p = Pipeline::new(filename, &source)
            .with_lint_overrides(lint_overrides.clone())
            .strict_core()
            .with_library();
        if let Some(m) = &manifest {
            p.profile = m.profile;
            p.profile_config = m.profile_config.clone();
        }
        p
    };
    let mut pipeline = checked();
    pipeline.run_all_checks();
    let total = pipeline.total_errors();
    if total > 0 {
        match output {
            OutputMode::Text => {
                print_text_diagnostics(&pipeline);
                eprintln!("\n{total} error(s) found.");
            }
            OutputMode::Json => emit_json_output(&pipeline),
            OutputMode::Jsonl => run_pipeline_jsonl(&mut checked()),
        }
        process::exit(1);
    }
    if output == OutputMode::Text {
        // Warnings only: the check passed.
        print_text_diagnostics(&pipeline);
    }
    drop(pipeline);

    crate::mir::interp::set_source_name(filename);
    let mut argv = Vec::with_capacity(program_args.len() + 1);
    argv.push(filename.to_string());
    argv.extend_from_slice(program_args);
    #[cfg(feature = "llvm")]
    if !interp {
        run_on_jit(&source, output, timeout, &argv);
    }
    // Without the llvm feature the MIR interpreter is the only executor.
    let _ = interp;
    run_on_interpreter(source, output, timeout, argv);
}

/// The MIR builder refused the program: a compile error, exit 1.
fn refused(e: &str, output: OutputMode) -> ! {
    match output {
        OutputMode::Text => eprintln!("error[mir]: {e}"),
        _ => emit_build_error(&format!("error[mir]: {e}"), output),
    }
    process::exit(1);
}

#[cfg(feature = "llvm")]
fn run_on_jit(
    source: &str,
    output: OutputMode,
    timeout: Option<std::time::Duration>,
    argv: &[String],
) -> ! {
    let ir = crate::mir::lower::build_source(source)
        .and_then(|l| {
            crate::mir::lower::compiled_check(&l)?;
            crate::codegen::mir::compile_mir_ir_for_jit(&l.program, &l.tys)
                .map_err(|e| format!("llvm: {e}"))
        })
        .unwrap_or_else(|e| refused(&e, output));
    // `KARAC_MIR_LL=<path>` keeps a copy of the IR, as `__mir-llvm-run` does.
    if let Some(keep) = std::env::var_os("KARAC_MIR_LL") {
        let _ = std::fs::write(keep, &ir);
    }
    let run = run_ir_via_jit_subprocess_with(&ir, argv, timeout, output != OutputMode::Text);
    if run.timed_out {
        timed_out(timeout);
    }
    finish(run.code, run.stderr.as_deref(), output)
}

/// What the interpreter thread sends back: the build's verdict first, so the
/// deadline counts only the run, then the run.
enum Step {
    Built(Result<(), String>),
    Ran(crate::mir::interp::RunResult),
}

fn run_on_interpreter(
    source: String,
    output: OutputMode,
    timeout: Option<std::time::Duration>,
    argv: Vec<String>,
) -> ! {
    crate::mir::interp::set_program_args(argv);
    let stream = output == OutputMode::Text;
    let (tx, rx) = std::sync::mpsc::channel();
    // The MIR interpreter drops a long chain (a 100 000-node list) by
    // recursion, so it runs on a thread with room for that.
    let worker = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || {
            let lowered = crate::mir::lower::build_source(&source).and_then(|l| {
                crate::mir::lower::par_check(&l)?;
                Ok(l)
            });
            let lowered = match lowered {
                Ok(l) => {
                    let _ = tx.send(Step::Built(Ok(())));
                    l
                }
                Err(e) => {
                    let _ = tx.send(Step::Built(Err(e)));
                    return;
                }
            };
            let run = if stream {
                crate::mir::interp::run_streaming
            } else {
                crate::mir::interp::run_untraced
            };
            let r = run(&lowered.program, &lowered.tys, "main", vec![]);
            let _ = tx.send(Step::Ran(r));
        })
        .expect("failed to spawn the MIR interpreter thread");
    let gone = |worker: std::thread::JoinHandle<()>| -> ! {
        match worker.join() {
            Err(p) => std::panic::resume_unwind(p),
            Ok(()) => {
                eprintln!("error[mir]: the interpreter stopped without a result");
                process::exit(3);
            }
        }
    };
    match rx.recv() {
        Ok(Step::Built(Ok(()))) => {}
        Ok(Step::Built(Err(e))) => refused(&e, output),
        Ok(Step::Ran(_)) | Err(_) => gone(worker),
    }
    let ran = match timeout {
        None => rx.recv().ok(),
        Some(d) => match rx.recv_timeout(d) {
            Ok(step) => Some(step),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => timed_out(timeout),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
        },
    };
    let Some(Step::Ran(r)) = ran else {
        gone(worker)
    };
    use std::io::Write;
    print!("{}", r.output);
    let _ = std::io::stdout().flush();
    let code = match (&r.outcome, r.exit_code()) {
        (_, Some(code)) => code,
        (crate::mir::interp::Outcome::Error(e), None) => {
            eprint!("{}", r.stderr);
            eprintln!("error[mir]: run: {e}");
            process::exit(3);
        }
        _ => 3,
    };
    let stderr = (!stream).then_some(r.stderr.as_str());
    finish(code, stderr, output)
}

/// GNU `timeout(1)`'s code and legacy's message.
fn timed_out(timeout: Option<std::time::Duration>) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    if let Some(d) = timeout {
        eprintln!("karac: timed out after {}s", d.as_secs());
    }
    process::exit(124);
}

/// Exits with the program's code. Under `--output=json`/`jsonl` the program's
/// stderr was captured, and its runtime reports become the JSON objects legacy
/// `karac run` printed: `main`'s `Err` (`error`), a fault (`runtime_errors`)
/// and the error return trace; the rest of its stderr passes through.
fn finish(code: i32, stderr: Option<&str>, output: OutputMode) -> ! {
    if let Some(text) = stderr {
        let reports = RuntimeReports::parse(text, code);
        eprint!("{}", reports.passthrough);
        reports.emit(output);
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    process::exit(code);
}

/// A runtime error the program reported: `runtime error: MSG` then
/// `  at FILE:LINE:COL`.
struct Fault {
    message: String,
    at: Option<(String, usize, usize)>,
}

/// The reports a v2 program writes to stderr, read back out of it.
#[derive(Default)]
struct RuntimeReports {
    /// `Error: MSG`, `main`'s `Err` (exit code 1, and nothing but the trace
    /// after it).
    error: Option<String>,
    faults: Vec<Fault>,
    trace: Vec<(String, usize, usize)>,
    truncated: bool,
    /// Everything else the program wrote.
    passthrough: String,
}

/// `FILE:LINE:COL`, where FILE may hold colons of its own.
fn location(s: &str) -> Option<(String, usize, usize)> {
    let mut parts = s.rsplitn(3, ':');
    let col = parts.next()?.trim().parse().ok()?;
    let line = parts.next()?.trim().parse().ok()?;
    let file = parts.next()?.to_string();
    Some((file, line, col))
}

impl RuntimeReports {
    fn parse(text: &str, code: i32) -> Self {
        let mut r = RuntimeReports::default();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if let Some(message) = line.strip_prefix("runtime error: ") {
                let at = lines
                    .get(i + 1)
                    .and_then(|l| l.strip_prefix("  at "))
                    .and_then(location);
                i += if at.is_some() { 2 } else { 1 };
                r.faults.push(Fault {
                    message: message.to_string(),
                    at,
                });
                continue;
            }
            if line == "Error return trace:" {
                i += 1;
                while let Some(l) = lines.get(i).and_then(|l| l.strip_prefix("  ")) {
                    if l.starts_with("... (trace truncated") {
                        r.truncated = true;
                    } else if let Some(frame) = location(l) {
                        r.trace.push(frame);
                    } else {
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            if code == 1 && r.error.is_none() {
                if let Some(message) = line.strip_prefix("Error: ") {
                    // Only the last thing the program wrote, the trace aside.
                    let rest_is_trace = lines[i + 1..]
                        .first()
                        .is_none_or(|l| *l == "Error return trace:");
                    if rest_is_trace {
                        r.error = Some(message.to_string());
                        i += 1;
                        continue;
                    }
                }
            }
            r.passthrough.push_str(line);
            r.passthrough.push('\n');
            i += 1;
        }
        r
    }

    fn emit(&self, output: OutputMode) {
        let at_json = |at: &Option<(String, usize, usize)>| match at {
            Some((file, line, col)) => format!(
                "{{\"file\":{},\"line\":{line},\"col\":{col}}}",
                json_string(file)
            ),
            None => "null".to_string(),
        };
        let frames = self
            .trace
            .iter()
            .map(|(file, line, column)| {
                format!(
                    "{{\"file\":{},\"line\":{line},\"column\":{column}}}",
                    json_string(file)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        match output {
            OutputMode::Text => {}
            OutputMode::Json => {
                if let Some(e) = &self.error {
                    println!("{{\"error\":{}}}", json_string(e));
                }
                if !self.faults.is_empty() {
                    let arr = self
                        .faults
                        .iter()
                        .map(|f| {
                            format!(
                                "{{\"message\":{},\"location\":{}}}",
                                json_string(&f.message),
                                at_json(&f.at)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    println!("{{\"runtime_errors\":[{arr}]}}");
                }
                if !self.trace.is_empty() {
                    let trace = if self.truncated {
                        format!("{{\"frames\":[{frames}],\"truncated\":true}}")
                    } else {
                        format!("[{frames}]")
                    };
                    println!("{{\"error_return_trace\":{trace}}}");
                }
            }
            OutputMode::Jsonl => {
                if let Some(e) = &self.error {
                    emit_jsonl_event("error", &format!("\"message\":{}", json_string(e)));
                }
                for f in &self.faults {
                    emit_jsonl_event(
                        "runtime_error",
                        &format!(
                            "\"message\":{},\"location\":{}",
                            json_string(&f.message),
                            at_json(&f.at)
                        ),
                    );
                }
                if !self.trace.is_empty() {
                    emit_jsonl_event(
                        "error_return_trace",
                        &format!("\"frames\":[{frames}],\"truncated\":{}", self.truncated),
                    );
                }
            }
        }
    }
}
