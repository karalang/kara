//! `karac repl` on v2 (M3 item 3, B-2026-10-22-45): each cell's program is
//! checked as `karac check` checks a program and run on the MIR interpreter.
//! The REPL's session (items, replayed `let`s, providers, `:save`) stays in
//! `crate::repl`; this is its checker and executor. `--legacy` (or
//! `KARAC_LEGACY=1`) keeps the legacy REPL until M3 deletes it.

use super::*;

/// Printed between the replayed `let`s and the cell's own statements, so the
/// output the replay repeats can be dropped: a cell shows what it printed,
/// not what the cells before it printed again.
pub(crate) const CELL_START: &str = "@@karac-repl-cell@@";

/// What a cell's run printed.
pub(crate) struct Ran {
    pub stdout: String,
    pub stderr: String,
}

/// Why a cell did not run to the end: its diagnostics, or a panic's message,
/// with what it printed before stopping.
pub(crate) struct Failed {
    pub stdout: String,
    pub errors: Vec<String>,
}

/// The errors `karac check` reports for `source`, one rendered block each
/// (warnings are left out: a cell's replayed `let`s are often unused).
pub(crate) fn check(source: &str) -> Result<(), Vec<String>> {
    let mut p = Pipeline::new("<repl>", source).strict_core().with_library();
    p.run_all_checks();
    if p.total_errors() == 0 {
        return Ok(());
    }
    Err(render_text_diagnostics(&p)
        .into_iter()
        .filter(|b| b.starts_with("error"))
        .collect())
}

/// Checks `source` and runs its `main` on the MIR interpreter. The output
/// before the [`CELL_START`] line is the replay's and is dropped.
pub(crate) fn run_cell(source: &str) -> Result<Ran, Failed> {
    let failed = |errors: Vec<String>| Failed {
        stdout: String::new(),
        errors,
    };
    check(source).map_err(failed)?;
    crate::mir::interp::set_source_name("<repl>");
    let source = source.to_string();
    // The MIR interpreter drops a long chain by recursion, so it runs on a
    // thread with room for that, as `karac run --interp` does.
    let run = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || {
            let l = crate::mir::lower::build_source(&source)?;
            crate::mir::lower::par_check(&l)?;
            Ok::<_, String>(crate::mir::interp::run_untraced(
                &l.program,
                &l.tys,
                "main",
                vec![],
            ))
        })
        .expect("failed to spawn the MIR interpreter thread")
        .join();
    let r = match run {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(failed(vec![format!("error[mir]: {e}")])),
        Err(_) => {
            return Err(failed(vec![
                "error[mir]: the interpreter stopped without a result".to_string(),
            ]))
        }
    };
    let marker = format!("{CELL_START}\n");
    let stdout = match r.output.find(&marker) {
        Some(at) => r.output[at + marker.len()..].to_string(),
        // The replay stopped before the cell began.
        None => String::new(),
    };
    match (&r.outcome, r.exit_code()) {
        (_, Some(0)) => Ok(Ran {
            stdout,
            stderr: r.stderr,
        }),
        (crate::mir::interp::Outcome::Error(e), _) => Err(Failed {
            stdout,
            errors: vec![format!("{}error[mir]: run: {e}", r.stderr)],
        }),
        (_, code) => {
            let mut msg = r.stderr.trim_end().to_string();
            if msg.is_empty() {
                msg = format!("the cell exited with code {}", code.unwrap_or(3));
            }
            Err(Failed {
                stdout,
                errors: vec![msg],
            })
        }
    }
}
