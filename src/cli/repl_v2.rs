//! `karac repl` on v2 (M3 item 3, B-2026-10-22-45): each cell's program is
//! checked as `karac check` checks a program and run on the MIR interpreter.
//! The REPL's session (items, replayed `let`s, providers, `:save`) stays in
//! `crate::repl`; this is its checker and executor.

use super::*;

/// Printed between the replayed `let`s and the cell's own statements, so the
/// output the replay repeats can be dropped: a cell shows what it printed,
/// not what the cells before it printed again.
pub(crate) const CELL_START: &str = "@@karac-repl-cell@@";

/// What a cell's run printed, and the bindings its program passed to
/// [`crate::mir::interp::REPL_SNAPSHOT_FN`] at its end.
pub(crate) struct Ran {
    pub stdout: String,
    pub stderr: String,
    pub bindings: Vec<crate::mir::interp::ReplBinding>,
    /// The value of the cell's bare final expression, recorded as
    /// [`SHOWN_VALUE`] (and left out of `bindings`): `Some(None)` for a value
    /// with no written form.
    pub shown: Option<Option<(String, String)>>,
}

/// The name a cell's shown value is recorded under; no binding can take it.
pub(crate) const SHOWN_VALUE: &str = "__karac_shown_value";

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
    check(source).map_err(|errors| Failed {
        stdout: String::new(),
        errors,
    })?;
    run_checked(source)
}

/// [`run_cell`] of a program [`check`] has accepted.
pub(crate) fn run_checked(source: &str) -> Result<Ran, Failed> {
    let failed = |errors: Vec<String>| Failed {
        stdout: String::new(),
        errors,
    };
    crate::mir::interp::set_source_name("<repl>");
    let source = source.to_string();
    // The MIR interpreter drops a long chain by recursion, so it runs on a
    // thread with room for that, as `karac run --interp` does.
    let run = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || {
            let l = crate::mir::lower::build_source(&source)
                .map(crate::mir::lower::Lowered::for_this_pool)?;
            crate::mir::lower::par_check(&l)?;
            Ok::<_, String>(crate::mir::interp::collect_repl_bindings(|| {
                crate::mir::interp::run_untraced(&l.program, &l.tys, "main", vec![])
            }))
        })
        .expect("failed to spawn the MIR interpreter thread")
        .join();
    let (r, mut bindings) = match run {
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
    let shown = bindings
        .iter()
        .position(|(n, _)| n == SHOWN_VALUE)
        .map(|at| bindings.remove(at).1);
    match (&r.outcome, r.exit_code()) {
        (_, Some(0)) => Ok(Ran {
            stdout,
            stderr: r.stderr,
            bindings,
            shown,
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

/// The program `source` is and its types, checked as [`check`] checks a
/// cell up to the typechecker (the library in scope), or the errors that
/// stopped it. `:type` reads a `let`'s type from it.
pub(crate) fn typed(
    source: &str,
) -> Result<(crate::ast::Program, crate::typechecker::TypeCheckResult), Vec<String>> {
    let mut p = Pipeline::new("<repl>", source).strict_core().with_library();
    p.drop_library_diagnostics();
    p.resolve();
    p.drop_library_diagnostics();
    p.typecheck();
    p.drop_library_diagnostics();
    if p.total_errors() != 0 {
        return Err(render_text_diagnostics(&p)
            .into_iter()
            .filter(|b| b.starts_with("error"))
            .collect());
    }
    match p.typed.take() {
        Some(t) => Ok((p.parsed.program, t)),
        None => Err(vec!["error: the typechecker did not run".to_string()]),
    }
}
