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
pub(crate) fn run_cell(worker: &Worker, source: &str, take: bool) -> Result<Ran, Failed> {
    check(source).map_err(|errors| Failed {
        stdout: String::new(),
        errors,
    })?;
    run_checked(worker, source, take)
}

/// A job for a session's [`Worker`].
type Job = Box<dyn FnOnce() + Send>;

/// The thread a REPL session's programs run on, one after another: the
/// interpreter's carry (B-2026-11-19-3, `crate::mir::interp::carry_on`)
/// lives on it, so the session is one heap and one task pool. The MIR
/// interpreter drops a long chain by recursion, so the thread has room for
/// that, as `karac run --interp`'s does.
pub(crate) struct Worker {
    tx: Option<std::sync::mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub(crate) fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("karac-repl".to_string())
            .stack_size(1 << 30)
            .spawn(move || {
                crate::mir::interp::carry_on(true);
                for job in rx {
                    job();
                }
            })
            .ok();
        Worker {
            tx: Some(tx),
            thread,
        }
    }

    /// `f`'s result, run on the worker; `None` when it panicked or the
    /// worker is gone.
    pub(crate) fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let (rtx, rrx) = std::sync::mpsc::channel();
        let job: Job = Box::new(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
            let _ = rtx.send(r.ok());
        });
        self.tx.as_ref()?.send(job).ok()?;
        rrx.recv().ok().flatten()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// [`run_cell`] of a program [`check`] has accepted, on `worker`. With
/// `take`, the program starts from the session's carried state, and takes
/// its bindings ([`crate::mir::interp::REPL_TAKE_FN`]).
pub(crate) fn run_checked(worker: &Worker, source: &str, take: bool) -> Result<Ran, Failed> {
    let failed = |errors: Vec<String>| Failed {
        stdout: String::new(),
        errors,
    };
    let source = source.to_string();
    let run = worker.run(move || {
        crate::mir::interp::set_source_name("<repl>");
        crate::mir::interp::carry_take(take);
        let l = crate::mir::lower::build_source(&source)
            .map(crate::mir::lower::Lowered::for_this_pool)?;
        crate::mir::lower::par_check(&l)?;
        Ok::<_, String>(crate::mir::interp::collect_repl_bindings(|| {
            crate::mir::interp::run_untraced(&l.program, &l.tys, "main", vec![])
        }))
    });
    let (r, mut bindings) = match run {
        Some(Ok(r)) => r,
        Some(Err(e)) => return Err(failed(vec![format!("error[mir]: {e}")])),
        None => {
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
