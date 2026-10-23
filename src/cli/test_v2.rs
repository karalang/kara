//! `karac test` on v2 (M3 item 2, B-2026-10-22-13): the same discovery and
//! JSONL events as the legacy runner, each case run on MIR — the mir-llvm JIT
//! by default, the MIR interpreter under `--interp` (or `KARAC_TEST_JIT=0`).
//!
//! The package becomes one source (project mode, [`super::project_v2`]): each
//! `test "name" { .. }` turns into a function named as the legacy runner names
//! it, the package's own `main` is left out, and a generated `main` runs the
//! case `KARAC_TEST_CASE` names. A case with `#[with_provider(R, ctor)]` runs
//! inside `with_provider[R](ctor, || case())` (core semantics §14.8,
//! B-2026-10-21-5), with a fresh provider each time because each case is a
//! run of its own.

use super::project_v2::{blank, Amalgam, Edit};
use super::test_cmd::*;
use super::*;

/// What the generated wrapper writes to stderr once a fixture's provider is
/// built, so a fault before it reads as `provider_construction_failed`.
const PROVIDER_READY: &str = "karac-test: provider ready";

/// The name the combined source goes by in diagnostics before they are
/// mapped back to the package's files.
const PACKAGE: &str = "<package>";

pub(super) fn cmd_test_v2(filter: Option<String>, all: bool, interp: bool) {
    let TestFront { mf, tree, tests } = test_front(filter.as_deref(), true);
    let jit =
        cfg!(feature = "llvm") && !interp && std::env::var("KARAC_TEST_JIT").as_deref() != Ok("0");

    let amalgam = combine(&tree, &tests).unwrap_or_else(|e| {
        emit_test_event(
            "build_tree_error",
            &format!(
                "\"message\":{}",
                json_string(&format!("error[project]: {e}"))
            ),
        );
        process::exit(1);
    });
    if let Some(keep) = std::env::var_os("KARAC_PROJECT_SOURCE") {
        let _ = std::fs::write(keep, &amalgam.text);
    }

    // What `karac check` checks, on the combined source: the v2 core rules
    // the legacy tree check above does not know.
    let mut lints = crate::lints::CliLintOverrides::default();
    lints.apply_manifest_lints(&mf.lints);
    let mut pipeline = Pipeline::new(PACKAGE, &amalgam.text)
        .with_lint_overrides(lints)
        .strict_core()
        .with_library();
    pipeline.profile = mf.profile;
    pipeline.profile_config = mf.profile_config.clone();
    pipeline.run_all_checks();
    if pipeline.total_errors() > 0 {
        for d in &collect_diagnostics(&pipeline).entries {
            println!("{}", amalgam.remap_json_text(PACKAGE, d));
        }
        process::exit(1);
    }
    drop(pipeline);

    crate::mir::interp::set_source_name(PACKAGE);
    amalgam.install();
    let mut runner = Runner::new(jit, amalgam.text.clone());
    if let Err(e) = runner.build() {
        emit_test_event(
            "build_error",
            &format!(
                "\"message\":{}",
                json_string(&format!(
                    "error[mir]: {}",
                    super::project_v2::remap_build_error(&e)
                ))
            ),
        );
        process::exit(1);
    }

    let manifest_timeout = mf.test_timeout_seconds;
    let env_timeout = std::env::var("KARAC_TEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok());
    let started = std::time::Instant::now();
    emit_test_event("run_start", &format!("\"total_tests\":{}", tests.len()));
    let (mut passed, mut failed, mut skipped) = (0usize, 0usize, 0usize);
    for (case, t) in tests.iter().enumerate() {
        let conflicts = conflict_resources(&t.requires, &t.with_providers);
        if !conflicts.is_empty() {
            failed += 1;
            emit_test_event("test_fail", &test_fail_conflict_fields(t, &conflicts));
            continue;
        }
        let missing = missing_resources(&t.requires, &mf.test_resources);
        if !missing.is_empty() {
            if all {
                failed += 1;
                emit_test_event(
                    "test_fail",
                    &test_fail_unsatisfied_requires_fields(t, &missing),
                );
            } else {
                skipped += 1;
                emit_test_event(
                    "test_skip",
                    &test_skip_unsatisfied_requires_fields(t, &missing),
                );
            }
            continue;
        }
        let timeout = resolve_test_timeout(t.timeout_seconds, manifest_timeout, env_timeout);
        let run_started = std::time::Instant::now();
        let ran = runner.run(case, timeout);
        let duration_ms = run_started.elapsed().as_millis();
        let Some((code, stderr)) = ran else {
            failed += 1;
            emit_test_event(
                "test_timeout",
                &format!(
                    "\"test\":{},\"timeout_s\":{},\"elapsed_ms\":{}",
                    json_string(&t.qualified),
                    timeout.as_secs(),
                    duration_ms
                ),
            );
            continue;
        };
        if code == 0 {
            passed += 1;
            emit_test_event(
                "test_pass",
                &format!(
                    "\"test\":{},\"duration_ms\":{}",
                    json_string(&t.qualified),
                    duration_ms
                ),
            );
            continue;
        }
        failed += 1;
        // The fixtures built before the fault; one short of all of them is
        // the one whose constructor failed.
        let ready = stderr.lines().filter(|l| *l == PROVIDER_READY).count();
        let stderr: String = stderr
            .lines()
            .filter(|l| *l != PROVIDER_READY)
            .map(|l| format!("{l}\n"))
            .collect();
        let reports = super::run_v2::RuntimeReports::parse(&stderr, code);
        let fault = reports.faults.first();
        let message = fault
            .map(|f| f.message.clone())
            .or(reports.error.clone())
            .unwrap_or_else(|| {
                let rest = reports.passthrough.trim();
                if rest.is_empty() {
                    format!("test exited with code {code}")
                } else {
                    rest.to_string()
                }
            });
        if ready < t.with_providers.len() {
            let resource = &t.with_providers[ready].resource_path;
            emit_test_event(
                "test_fail",
                &test_fail_provider_construction_fields(t, resource, &message),
            );
            continue;
        }
        let at = fault.and_then(|f| f.at.clone());
        let outcome = TestOutcome {
            message: Some(message),
            span: at.as_ref().map(|(_, line, column)| crate::token::Span {
                line: *line,
                column: *column,
                offset: 0,
                length: 0,
            }),
            left: None,
            right: None,
        };
        let file = at.map(|(file, _, _)| file).unwrap_or_default();
        let providers: Vec<String> = t
            .with_providers
            .iter()
            .map(|fx| fx.resource_path.clone())
            .collect();
        emit_test_event(
            "test_fail",
            &test_fail_fields_with_providers(t, &outcome, &file, duration_ms, &providers),
        );
    }
    emit_test_event(
        "summary",
        &format!(
            "\"total\":{},\"passed\":{},\"failed\":{},\"skipped\":{},\"duration_ms\":{}",
            tests.len(),
            passed,
            failed,
            skipped,
            started.elapsed().as_millis(),
        ),
    );
    if failed > 0 {
        process::exit(1);
    }
}

/// The package as one source: each discovered case a function, the
/// package's `main` left out, and a generated `main` that runs case `k` when
/// `KARAC_TEST_CASE` is `k`.
fn combine(tree: &ProgramTree, tests: &[DiscoveredTest]) -> Result<Amalgam, String> {
    let files = super::project_v2::source_files(tree, true)?;
    let fns = functions(&files);
    let mut edits: Vec<Vec<Edit>> = Vec::new();
    // The constructor text of each case's fixtures, read from its file.
    let mut ctors: std::collections::HashMap<String, Vec<String>> = Default::default();
    for f in &files {
        let mut e = Vec::new();
        for item in &f.items {
            match item {
                Item::Function(func) if func.name == "main" => {
                    for a in &func.attributes {
                        e.push(blank(&f.text, a.span.offset, a.span.offset + a.span.length));
                    }
                    e.push(blank(
                        &f.text,
                        func.span.offset,
                        func.span.offset + func.span.length,
                    ));
                }
                Item::TestCase(tc) if f.module.test_file.is_some() => {
                    let name = ident(&mangled_test_function_name(
                        &module_label(&f.module.path),
                        tc.name_span.line,
                        &tc.name,
                    ));
                    for a in &tc.attributes {
                        e.push(blank(&f.text, a.span.offset, a.span.offset + a.span.length));
                    }
                    let header = &f.text[tc.span.offset..tc.body.span.offset];
                    let newlines = "\n".repeat(header.matches('\n').count());
                    e.push(Edit {
                        start: tc.span.offset,
                        end: tc.body.span.offset,
                        text: format!("fn {name}() {newlines}"),
                    });
                    let texts = tc
                        .attributes
                        .iter()
                        .filter(|a| a.is_bare("with_provider") && a.args.len() >= 2)
                        .filter_map(|a| {
                            let ctor = a.args[1].value.as_ref()?;
                            Some(provider_expr(&f.text, ctor, &fns))
                        })
                        .collect();
                    ctors.insert(name, texts);
                }
                _ => {}
            }
        }
        edits.push(e);
    }

    let mut tail = String::from("\n");
    let mut dispatch = String::new();
    for (k, t) in tests.iter().enumerate() {
        let name = ident(&t.fn_name);
        let texts = ctors.get(&name).cloned().unwrap_or_default();
        let mut body = String::new();
        for (i, text) in texts.iter().enumerate() {
            body.push_str(&format!(
                "    let mut __karac_p{i} = {text};\n    eprintln(\"{PROVIDER_READY}\");\n"
            ));
        }
        let mut call = format!("{name}();");
        for (i, fx) in t.with_providers.iter().enumerate().rev() {
            let resource = fx
                .resource_path
                .rsplit('.')
                .next()
                .unwrap_or(&fx.resource_path);
            call = format!("with_provider[{resource}](__karac_p{i}, || {{ {call} }});");
        }
        tail.push_str(&format!("fn __karac_case_{k}() {{\n{body}    {call}\n}}\n"));
        dispatch.push_str(&format!(
            "    if __karac_case == \"{k}\" {{ __karac_case_{k}(); }}\n"
        ));
    }
    tail.push_str(&format!(
        "fn main() {{\n    let __karac_case = match env.var(\"KARAC_TEST_CASE\") {{\n        \
         Ok(s) => s,\n        Err(_) => String.new(),\n    }};\n{dispatch}}}\n"
    ));
    let mut edits = edits.into_iter();
    super::project_v2::amalgamate(tree, &files, |_, _| edits.next().unwrap_or_default(), &tail)
}

/// The legacy runner's name for a case, as an identifier: the root
/// module's label is `<root>`.
fn ident(mangled: &str) -> String {
    mangled
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// The package's functions a fixture can name, by dotted path (`make_db`,
/// `FakeDb.new`), each with whether it returns a `Result`.
fn functions(
    files: &[super::project_v2::SourceFile<'_>],
) -> std::collections::HashMap<String, bool> {
    let returns_result = |f: &crate::ast::Function| {
        matches!(&f.return_type, Some(crate::ast::TypeExpr {
            kind: crate::ast::TypeKind::Path(p), ..
        }) if p.segments.last().is_some_and(|s| s == "Result"))
    };
    let mut out = std::collections::HashMap::new();
    for f in files {
        for item in &f.items {
            match item {
                Item::Function(func) => {
                    out.insert(func.name.clone(), returns_result(func));
                }
                Item::ImplBlock(b) => {
                    let crate::ast::TypeKind::Path(ty) = &b.target_type.kind else {
                        continue;
                    };
                    let Some(ty) = ty.segments.last() else {
                        continue;
                    };
                    for it in &b.items {
                        if let crate::ast::ImplItem::Method(m) = it {
                            if m.self_param.is_none() {
                                out.insert(format!("{ty}.{}", m.name), returns_result(m));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// A fixture's provider as an expression (core semantics §14.8): `e` as
/// written, or a call of it when it is a path naming a function
/// (`FakeDb.new`); a `Result` from a function is unwrapped, its `Err` failing
/// the case before the body runs.
fn provider_expr(
    src: &str,
    ctor: &crate::ast::Expr,
    fns: &std::collections::HashMap<String, bool>,
) -> String {
    use crate::ast::ExprKind;
    // A method call's own span covers only its receiver; its extent ends at
    // the closing `)` of its arguments.
    let end = match &ctor.kind {
        ExprKind::MethodCall {
            args_close_span, ..
        } => args_close_span.offset + args_close_span.length,
        _ => ctor.span.offset + ctor.span.length,
    };
    let text = src.get(ctor.span.offset..end).unwrap_or("").trim();
    let (text, result) = match &ctor.kind {
        _ if expr_to_dotted_path(ctor).is_some_and(|p| fns.contains_key(&p)) => {
            let p = expr_to_dotted_path(ctor).unwrap_or_default();
            (format!("{text}()"), fns[&p])
        }
        ExprKind::Call { callee, .. } => {
            let r = expr_to_dotted_path(callee).and_then(|p| fns.get(&p).copied());
            (text.to_string(), r.unwrap_or(false))
        }
        ExprKind::MethodCall { object, method, .. } => {
            let r = expr_to_dotted_path(object)
                .and_then(|p| fns.get(&format!("{p}.{method}")).copied());
            (text.to_string(), r.unwrap_or(false))
        }
        _ => (text.to_string(), false),
    };
    if result {
        format!(
            "match {text} {{\n        Ok(p) => p,\n        Err(_) => panic(\"the constructor returned Err\"),\n    }}"
        )
    } else {
        text
    }
}

/// Runs the cases of one built program: each a fresh run of `main` with
/// `KARAC_TEST_CASE` naming it, on the JIT or the MIR interpreter.
struct Runner {
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    jit: bool,
    source: String,
    #[cfg(feature = "llvm")]
    ir: String,
    /// The interpreter's worker: it builds the program once and runs the
    /// cases it is sent. A case that times out leaves it running, so the
    /// next case starts a new one.
    worker: Option<Worker>,
}

type Ran = (i32, String);

struct Worker {
    cases: std::sync::mpsc::Sender<usize>,
    results: std::sync::mpsc::Receiver<Result<Ran, String>>,
}

impl Runner {
    fn new(jit: bool, source: String) -> Self {
        Runner {
            jit,
            source,
            #[cfg(feature = "llvm")]
            ir: String::new(),
            worker: None,
        }
    }

    /// Build the program; a refusal is a compile error.
    fn build(&mut self) -> Result<(), String> {
        #[cfg(feature = "llvm")]
        if self.jit {
            self.ir = crate::mir::lower::build_source(&self.source).and_then(|l| {
                crate::mir::lower::compiled_check(&l)?;
                crate::backend::mir::compile_mir_ir_for_jit(&l.program, &l.tys)
                    .map_err(|e| format!("llvm: {e}"))
            })?;
            return Ok(());
        }
        let worker = self.spawn()?;
        self.worker = Some(worker);
        Ok(())
    }

    fn spawn(&self) -> Result<Worker, String> {
        let (tx_case, rx_case) = std::sync::mpsc::channel::<usize>();
        let (tx, rx) = std::sync::mpsc::channel();
        let source = self.source.clone();
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn(move || {
                let lowered = crate::mir::lower::build_source(&source).and_then(|l| {
                    crate::mir::lower::par_check(&l)?;
                    Ok(l)
                });
                let lowered = match lowered {
                    Ok(l) => {
                        let _ = tx.send(Ok((0, String::new())));
                        l
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                while let Ok(case) = rx_case.recv() {
                    std::env::set_var("KARAC_TEST_CASE", case.to_string());
                    let r = crate::mir::interp::run_untraced(
                        &lowered.program,
                        &lowered.tys,
                        "main",
                        vec![],
                    );
                    let ran = match (&r.outcome, r.exit_code()) {
                        (_, Some(code)) => Ok((code, r.stderr)),
                        (crate::mir::interp::Outcome::Error(e), None) => {
                            Ok((3, format!("{}runtime error: {e}\n", r.stderr)))
                        }
                        _ => Ok((3, r.stderr)),
                    };
                    if tx.send(ran).is_err() {
                        return;
                    }
                }
            })
            .map_err(|e| format!("cannot start the MIR interpreter: {e}"))?;
        rx.recv()
            .map_err(|_| "the MIR interpreter stopped while building".to_string())??;
        Ok(Worker {
            cases: tx_case,
            results: rx,
        })
    }

    /// Run case `case`: its exit code and stderr, or `None` when it ran past
    /// `timeout`.
    fn run(&mut self, case: usize, timeout: std::time::Duration) -> Option<Ran> {
        #[cfg(feature = "llvm")]
        if self.jit {
            let k = case.to_string();
            let run = run_ir_via_jit_subprocess_with(
                &self.ir,
                &["test".to_string()],
                Some(timeout),
                true,
                true,
                &[("KARAC_TEST_CASE", k.as_str())],
            );
            return (!run.timed_out).then(|| (run.code, run.stderr.unwrap_or_default()));
        }
        let worker = match self.worker.take() {
            Some(w) => w,
            None => match self.spawn() {
                Ok(w) => w,
                Err(e) => return Some((3, format!("runtime error: {e}\n"))),
            },
        };
        if worker.cases.send(case).is_err() {
            return Some((3, "runtime error: the MIR interpreter stopped\n".into()));
        }
        match worker.results.recv_timeout(timeout) {
            Ok(Ok(ran)) => {
                self.worker = Some(worker);
                Some(ran)
            }
            Ok(Err(e)) => Some((3, format!("runtime error: {e}\n"))),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Some((3, "runtime error: the MIR interpreter stopped\n".into()))
            }
        }
    }
}
