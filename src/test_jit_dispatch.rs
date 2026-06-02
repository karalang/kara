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

/// Build the **persistent shared-module** IR: the source module's items +
/// test fns + Debugger-Contract globals, with the user `fn main` removed (it
/// would collide with the per-test `main`; tests never call it). Installed
/// once in the runner and referenced declare-only by every per-test `main`,
/// so the suite's functions are JIT-compiled once instead of per test.
fn build_module_ir(module_program: &Program, source_filename: &str) -> Result<String, String> {
    let mut prog = clone_program_items(module_program);
    prog.items
        .retain(|it| !matches!(it, crate::ast::Item::Function(f) if f.name == "main"));
    let resolved = crate::resolver::Resolver::new(&prog).resolve();
    let typed = crate::typechecker::TypeChecker::new(&prog, &resolved).check();
    crate::lowering::lower_program(&mut prog, &typed);
    crate::codegen::compile_to_ir_for_test_module(&prog, Some(source_filename))
        .map_err(|e| format!("shared-module codegen failed: {e}"))
}

/// The module's concrete (non-generic) free-fn names and `Type.method`
/// symbol keys — passed as `declare_only_fns` so the per-test `main` codegen
/// emits `declare`s (not bodies) for them, linking to the persistent
/// module's definitions. Generic fns are excluded: they have no concrete
/// body in the persistent module; a test that triggers an instantiation gets
/// it defined locally in its own per-test module. The user `main` is
/// excluded (removed from both modules). Mirrors the symbol keyspace
/// `compile_program` declares impl methods under.
fn compute_declare_only(p: &Program) -> std::collections::HashSet<String> {
    use crate::ast::{ImplItem, Item};
    let mut out = std::collections::HashSet::new();
    for item in &p.items {
        match item {
            Item::Function(f) if f.generic_params.is_none() && f.name != "main" => {
                out.insert(f.name.clone());
            }
            Item::ImplBlock(imp) => {
                if let Some(target) = crate::codegen::impl_target_name_for_repl(&imp.target_type) {
                    for ii in &imp.items {
                        if let ImplItem::Method(m) = ii {
                            if m.generic_params.is_none() {
                                out.insert(format!("{target}.{}", m.name));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Build the per-test `main` IR: clone `source_program`'s items, append a
/// synthesized `main` that installs the `#[with_provider]` fixtures and
/// calls the test fn, re-run resolve/typecheck/lower (the synthesized
/// fixture `let` bindings need typecheck to populate `var_type_names` for
/// the `with_provider` codegen), then emit via the repl-cell codegen entry
/// so `declare_only` module symbols emit as declares and the
/// Debugger-Contract globals are suppressed (they live in the persistent
/// module). Only the tiny synth `main` carries a body — the module's
/// functions are NOT re-emitted here.
///
/// `source_program` is the **signature-only skeleton** ([`build_skeleton`])
/// for a no-fixture test, else the full module — see the body comment and the
/// `dispatch` call site for why the skeleton is sound only for no-fixture
/// tests. Either way it carries every module signature; only the bodies differ
/// (stubs vs real), and module bodies are never emitted here.
fn build_test_main_ir(
    source_program: &Program,
    test_fn_name: &str,
    fixtures: &[(String, Expr)],
    declare_only: &std::collections::HashSet<String>,
) -> Result<String, String> {
    let fixtures_vec: Vec<ProviderFixture> = fixtures
        .iter()
        .map(|(rp, ctor)| ProviderFixture {
            resource_path: rp.clone(),
            constructor: ctor.clone(),
        })
        .collect();
    // `source_program` is the **signature-only skeleton** of the module when
    // the runner has one cached for a no-fixture test (concrete fn/method
    // bodies replaced by `unreachable()` — see `build_skeleton`), else the
    // full module. Cloning + resolving + typechecking + lowering the skeleton
    // is far cheaper than the real module because the 1-stmt stub bodies
    // typecheck and lower trivially, while still carrying every signature the
    // synth `main` needs in scope. The module fns are declare-only in the
    // emitted IR (their real bodies live in the persistent module), so the
    // stub bodies are never codegen'd.
    let mut per_test_program = clone_program_items(source_program);
    append_test_main(&mut per_test_program, test_fn_name, &fixtures_vec);
    let resolved = crate::resolver::Resolver::new(&per_test_program).resolve();
    let typed = crate::typechecker::TypeChecker::new(&per_test_program, &resolved).check();
    crate::lowering::lower_program(&mut per_test_program, &typed);
    // Emit the synth `main` under a NON-`main` symbol. A JIT entry literally
    // named `main` collides with the C `_main` in the process symbol table
    // (the runner's own `main`), so materialization fails — the same reason
    // the repl names cell entries `cell_main_<id>`. The runner looks this
    // symbol up by the matching name. `compile_program`'s `main`-special
    // i32-return handling keys on the AST fn name (`"main"`), not the LLVM
    // symbol, so it still fires.
    crate::codegen::compile_to_ir_for_repl_cell(&per_test_program, declare_only, TEST_MAIN_SYMBOL)
        .map_err(|e| format!("codegen failed for test '{test_fn_name}': {e}"))
}

/// Build a **signature-only skeleton** of `module_program`: a fresh clone in
/// which every concrete (non-generic) free-fn and impl-method body is replaced
/// by `{ unreachable() }`. The synth per-test `main` only references module
/// items by *signature* (it calls the concrete test fn, which is declare-only),
/// so the skeleton carries everything `main`'s resolve/typecheck/lower need
/// while making those passes cheap — checking + lowering a 1-stmt stub is
/// near-free next to the real body. Cached once per module by the runner and
/// cloned per no-fixture test in place of the full module.
///
/// **Why no fixtures, why only concretes.** The skeleton is sound precisely
/// because no stubbed body is ever emitted: concrete fns are declare-only
/// (linked to the persistent module's real definitions), and generic fns are
/// emitted only on instantiation. A no-fixture synth `main` is just
/// `{ test_fn(); }` — one call to a concrete fn — so it triggers zero local
/// generic instantiation. A *fixture* synth `main` evaluates arbitrary
/// constructor expressions that could instantiate a generic into the per-test
/// module, where it would be emitted from the stub → wrong IR; those tests use
/// the full module instead (the caller gates on `fixtures.is_empty()`). For
/// the same reason generic items keep their real bodies here (cheap — generics
/// are rare; correctness over the last few µs).
///
/// `requires`/`ensures` are cleared on the stubbed items: declare-only fns
/// emit no contract checks, and a divergent stub body would otherwise drag the
/// (irrelevant) contract predicates through typecheck. The stub body reuses the
/// original body's span so its span-keyed typecheck entries don't collide with
/// the synth `main`'s all-`(0,0)` spans.
fn build_skeleton(module_program: &Program) -> Program {
    use crate::ast::{Block, ExprKind, ImplItem, Item, Stmt, StmtKind};
    fn stub(body_span: Span) -> Block {
        let call = Expr {
            kind: ExprKind::Call {
                callee: Box::new(Expr {
                    kind: ExprKind::Identifier("unreachable".to_string()),
                    span: body_span.clone(),
                }),
                args: Vec::new(),
            },
            span: body_span.clone(),
        };
        Block {
            stmts: vec![Stmt {
                kind: StmtKind::Expr(call),
                span: body_span.clone(),
            }],
            final_expr: None,
            span: body_span,
        }
    }
    let mut p = clone_program_items(module_program);
    for item in &mut p.items {
        match item {
            // `main` is removed from the per-test program by `append_test_main`
            // anyway, but the module never carries one (`_test.kara`); the
            // guard is defensive and keeps the skeleton faithful.
            Item::Function(f) if f.name != "main" && f.generic_params.is_none() => {
                f.body = stub(f.body.span.clone());
                f.requires.clear();
                f.ensures.clear();
            }
            Item::ImplBlock(imp) => {
                for ii in &mut imp.items {
                    if let ImplItem::Method(m) = ii {
                        if m.generic_params.is_none() {
                            m.body = stub(m.body.span.clone());
                            m.requires.clear();
                            m.ensures.clear();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    p
}

/// LLVM symbol the synthesized per-test `main` is emitted under (and the
/// runner looks up). NOT `main` — see `build_test_main_ir`.
pub const TEST_MAIN_SYMBOL: &str = "__karac_test_entry";

/// Persistent JIT test runner (cold-start amortization). Holds one
/// `karac_jit_runner --test-batch` subprocess that runs every test in the
/// suite, paying LLVM target init + engine construction ONCE instead of
/// per-test (the one-shot path spawns a fresh runner per test — ~15 ms
/// each, dominated by that init).
///
/// **Re-spawn on death.** A failing test (`assert` / contract fault /
/// `unreachable()`) lowers to `emit_panic` → `exit(1)`, which kills the
/// whole runner. The connection is then dropped (`conn = None`) and the
/// next `run_test` lazily re-spawns. So the suite pays one engine init +
/// one more per *failing* test — a mostly-passing suite (the common case)
/// amortizes the init across all its passing tests. The failing test's
/// stdout/stderr (incl. the `KARAC_TEST_FAILURE` marker) is redirected by
/// the runner to parent-known tempfiles, so it survives the runner's death
/// for the parent to read + map.
pub struct TestBatchRunner {
    /// `None` until the first test (lazy spawn) and after a death (lazy
    /// re-spawn on the next test).
    conn: Option<Conn>,
    /// Tempfile path prefix passed to the runner; per-test stdout/stderr
    /// land at `<prefix>.<id>.out` / `.err`.
    prefix: PathBuf,
    /// Monotonic per-test id — also the tempfile discriminator.
    next_id: u64,
    /// Source-module identity of the currently-cached module IR; when a
    /// test from a different module arrives, the module is re-codegen'd
    /// and re-installed in the runner.
    current_module_id: Option<usize>,
    /// The cached persistent-module IR (all the source module's items +
    /// test fns + globals, user `main` removed) — JIT-compiled ONCE in the
    /// runner and referenced declare-only by every per-test `main`.
    module_ir: Option<String>,
    /// The module's concrete fn / `Type.method` symbol names — passed as
    /// `declare_only_fns` to per-test codegen so their bodies aren't
    /// re-emitted (they link to the persistent module's definitions).
    declare_only: std::collections::HashSet<String>,
    /// The cached **signature-only skeleton** of the current module
    /// ([`build_skeleton`]): the module's items with concrete fn/method bodies
    /// stubbed to `unreachable()`. Built once per module (alongside
    /// `module_ir`) and cloned per *no-fixture* test in place of the full
    /// module — so each test's resolve/typecheck/lower runs over trivial stub
    /// bodies instead of the real ones, the dominant parent-side per-test cost
    /// (~54% of it is typechecking bodies that are identical across tests and
    /// never emitted). `None` for full-mode modules (no cache).
    skeleton_program: Option<Program>,
    /// Whether the cached `module_ir` has been installed in the *current*
    /// runner connection. Reset to false on (re-)spawn and on module change
    /// so the next test re-sends the `module` command first.
    module_sent: bool,
    /// Whether the current module uses the persistent-module cache (split
    /// codegen). `false` for "full mode": modules whose tests override an
    /// **ambient** prelude resource (`Clock`/`Env`/…) via `with_provider`.
    /// Ambient override dispatch is decided at compile time per module (the
    /// call site emits a runtime branch only if a `with_provider` vtable for
    /// that resource exists *in the same module*); splitting the
    /// `with_provider` site (per-test `main`) from the `R.method()` call site
    /// (test fn, in the persistent module) would silently drop the override.
    /// So those modules run each test as one self-contained module (empty
    /// declare-only, no persistent install) — gap-a's cross-fn-boundary
    /// dispatch works within a single module. User-resource overrides
    /// (trait-ful / trait-less) are unaffected — their vtable comes from the
    /// impl blocks, which live in the persistent module. NOTE: re-measure if
    /// this is ever relaxed; full-mode modules forgo the cache win.
    cache_module: bool,
}

struct Conn {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    pid: u32,
}

impl TestBatchRunner {
    /// Create the runner handle. Does NOT spawn yet — the subprocess is
    /// spawned lazily on the first `run_test` so a suite with zero JIT
    /// tests pays nothing. `prefix` should be a unique path in the temp
    /// dir (caller includes the karac pid).
    pub fn new(prefix: PathBuf) -> Self {
        Self {
            conn: None,
            prefix,
            next_id: 0,
            current_module_id: None,
            module_ir: None,
            declare_only: std::collections::HashSet::new(),
            skeleton_program: None,
            module_sent: false,
            cache_module: false,
        }
    }

    fn spawn_conn(&self) -> Result<Conn, String> {
        use std::process::{Command, Stdio};
        let runner_path = locate_karac_jit_runner().ok_or_else(|| {
            "karac_jit_runner binary not found alongside karac executable — rebuild karac \
             with `--features lljit_prototype`"
                .to_string()
        })?;
        let mut child = Command::new(&runner_path)
            .arg("--test-batch")
            .arg(&self.prefix)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn {}: {e}", runner_path.display()))?;
        let pid = child.id();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "runner has no stdin".to_string())?;
        let mut stdout = std::io::BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| "runner has no stdout".to_string())?,
        );
        let mut ready = String::new();
        use std::io::BufRead;
        match stdout.read_line(&mut ready) {
            Ok(n) if n > 0 && ready.trim() == "ready" => {}
            Ok(_) => return Err(format!("expected 'ready' banner, got {ready:?}")),
            Err(e) => return Err(format!("read ready banner: {e}")),
        }
        Ok(Conn {
            child,
            stdin,
            stdout,
            pid,
        })
    }

    /// Ensure there's a live connection with the current module installed.
    /// Spawns the runner if needed (resetting `module_sent`), then installs
    /// the cached module IR via the `module` command if not yet sent on this
    /// connection. Idempotent across tests in the same module.
    fn ensure_ready(&mut self) -> Result<(), String> {
        if self.conn.is_none() {
            self.conn = Some(self.spawn_conn()?);
            self.module_sent = false;
        }
        // Full-mode modules (ambient-fixture) install nothing persistent —
        // each per-test module is self-contained.
        if self.cache_module && !self.module_sent {
            let module_ir = self
                .module_ir
                .clone()
                .ok_or_else(|| "no module IR cached (dispatch not called?)".to_string())?;
            self.send_module(&module_ir)?;
            self.module_sent = true;
        }
        Ok(())
    }

    /// Send the `module <ir_len>\n<ir>` command and await the `moduleok`
    /// ack. A dead pipe / `moduleerr` drops the connection so the caller
    /// re-spawns.
    fn send_module(&mut self, ir: &str) -> Result<(), String> {
        use std::io::{BufRead, Write};
        let conn = self.conn.as_mut().expect("conn present in send_module");
        let header = format!("module {}\n", ir.len());
        if conn.stdin.write_all(header.as_bytes()).is_err()
            || conn.stdin.write_all(ir.as_bytes()).is_err()
            || conn.stdin.flush().is_err()
        {
            self.drop_conn();
            return Err("module send: runner pipe closed".to_string());
        }
        let mut line = String::new();
        match conn.stdout.read_line(&mut line) {
            Ok(n) if n > 0 && line.trim() == "moduleok" => Ok(()),
            other => {
                self.drop_conn();
                Err(format!(
                    "module install failed (got {line:?}, read {other:?})"
                ))
            }
        }
    }

    /// Drop the current connection and mark the module un-installed, so the
    /// next test lazily re-spawns the runner and re-sends the `module`
    /// command before running.
    fn drop_conn(&mut self) {
        self.conn = None;
        self.module_sent = false;
    }

    /// Build the per-test `main` IR and run it on the persistent runner —
    /// `cmd_test` creates one `TestBatchRunner` for the whole suite and
    /// calls this per test. The source module (`module_id` / `module_program`)
    /// is codegen'd + installed ONCE and referenced declare-only by every
    /// per-test `main`, so the suite's functions are JIT-compiled once, not
    /// per test (the cold-start amortization). A test from a different
    /// source module triggers a module re-codegen + re-install.
    // Genuinely distinct per-test inputs (per-module identity/program +
    // per-test fn/fixtures + timeout); bundling them into a struct would
    // just move the noise to the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn dispatch(
        &mut self,
        module_id: usize,
        use_cache: bool,
        module_program: &Program,
        test_fn_name: &str,
        fixtures: &[(String, Expr)],
        source_filename: &str,
        timeout: Duration,
    ) -> JitTestResult {
        // On a source-module change, drop the connection (clear any prior
        // module's persistent install — it would collide) and rebuild the
        // cache state. `use_cache` is false for modules that override an
        // ambient prelude resource (see `cache_module`): those run each test
        // self-contained (empty declare-only, no persistent module).
        if self.current_module_id != Some(module_id) {
            self.drop_conn();
            self.current_module_id = Some(module_id);
            self.cache_module = use_cache;
            if use_cache {
                match build_module_ir(module_program, source_filename) {
                    Ok(ir) => self.module_ir = Some(ir),
                    Err(message) => return JitTestResult::SpawnFailed { message },
                }
                self.declare_only = compute_declare_only(module_program);
                // Cache the signature-only skeleton so each no-fixture test
                // clones + resolves + typechecks + lowers stub bodies instead
                // of the real module — the dominant parent-side per-test cost.
                self.skeleton_program = Some(build_skeleton(module_program));
            } else {
                self.module_ir = None;
                self.declare_only = std::collections::HashSet::new();
                self.skeleton_program = None;
            }
        }
        // Per-test `main`: module items declare-only, synth main with body,
        // Debugger-Contract globals suppressed (so they don't collide with
        // the persistent module's). Built fresh each test — only the tiny
        // main is codegen'd here; the module's bodies are not re-emitted.
        //
        // Source program for that build: the cached signature-only skeleton
        // when this is a no-fixture test on a cached module, else the full
        // module. The skeleton is sound ONLY for no-fixture tests — a
        // fixture synth `main` evaluates arbitrary constructor exprs that
        // could instantiate a generic locally (where a stubbed body would be
        // emitted → wrong IR); no-fixture mains just call the concrete,
        // declare-only test fn and instantiate nothing. See `build_skeleton`.
        let source_program = match (fixtures.is_empty(), self.skeleton_program.as_ref()) {
            (true, Some(skel)) => skel,
            _ => module_program,
        };
        let ir =
            match build_test_main_ir(source_program, test_fn_name, fixtures, &self.declare_only) {
                Ok(s) => s,
                Err(message) => return JitTestResult::SpawnFailed { message },
            };
        self.run_test(&ir, timeout)
    }

    /// Run one already-built per-test `main` IR. Lazily (re-)spawns the
    /// runner + (re-)installs the persistent module, sends the test IR, and
    /// maps the outcome. On runner death (test faulted) or timeout the
    /// connection is dropped so the next call re-spawns + re-installs.
    fn run_test(&mut self, ir: &str, timeout: Duration) -> JitTestResult {
        let id = self.next_id;
        self.next_id += 1;
        if let Err(message) = self.ensure_ready() {
            return JitTestResult::SpawnFailed { message };
        }
        // Build paths byte-identically to the runner's
        // `format!("{prefix}.{id}.out")` — `with_extension` would *replace*
        // a trailing extension, desyncing if the prefix contained a dot.
        let pfx = self.prefix.display();
        let out_path = PathBuf::from(format!("{pfx}.{id}.out"));
        let err_path = PathBuf::from(format!("{pfx}.{id}.err"));
        let started = std::time::Instant::now();
        let result = self.exchange(id, ir, timeout);
        let duration_ms = started.elapsed().as_millis();

        let read_file = |p: &PathBuf| std::fs::read(p).unwrap_or_default();
        let map = |rc: i32| {
            let out = String::from_utf8_lossy(&read_file(&out_path)).into_owned();
            let err = String::from_utf8_lossy(&read_file(&err_path)).into_owned();
            map_exit_to_outcome(rc, &out, &err)
        };
        let outcome = match result {
            Exchange::Survived(rc) => JitTestResult::Completed {
                outcome: map(rc),
                duration_ms,
            },
            Exchange::Died(rc) => {
                self.drop_conn(); // force re-spawn + module re-install next test
                JitTestResult::Completed {
                    outcome: map(rc),
                    duration_ms,
                }
            }
            Exchange::TimedOut => {
                self.drop_conn();
                JitTestResult::TimedOut { duration_ms }
            }
            Exchange::Protocol(message) => {
                self.drop_conn();
                JitTestResult::SpawnFailed { message }
            }
        };
        let _ = std::fs::remove_file(&out_path);
        let _ = std::fs::remove_file(&err_path);
        outcome
    }

    /// Send `test <id> <ir_len>\n<ir>` and read the `done <id> <rc>`
    /// response under a kill-on-timeout watchdog. EOF before a complete
    /// frame means the runner died inside the test (the failure path).
    fn exchange(&mut self, id: u64, ir: &str, timeout: Duration) -> Exchange {
        use std::io::{BufRead, Write};
        use std::sync::mpsc;
        let conn = self.conn.as_mut().expect("conn spawned in run_test");
        let header = format!("test {} {}\n", id, ir.len());
        if conn.stdin.write_all(header.as_bytes()).is_err()
            || conn.stdin.write_all(ir.as_bytes()).is_err()
            || conn.stdin.flush().is_err()
        {
            // Pipe broke — runner already gone; reap for its exit code.
            let rc = conn.child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
            return Exchange::Died(rc);
        }

        // Arm a watchdog that hard-kills the runner if the test hangs.
        // Killing closes the pipe, so the blocking `read_line` returns EOF.
        let pid = conn.pid;
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

        let mut line = String::new();
        let read = conn.stdout.read_line(&mut line);
        let _ = tx.send(());
        let killed = watchdog.join().unwrap_or(false);

        match read {
            Ok(0) | Err(_) => {
                // Pipe closed before a `done` frame. Either the watchdog
                // killed a hung test (timeout) or the test faulted and
                // `exit()`d the runner (failure). Reap to drain the zombie
                // + recover the real exit code for the failure case.
                let rc = conn.child.wait().ok().and_then(|s| s.code()).unwrap_or(1);
                if killed {
                    Exchange::TimedOut
                } else {
                    Exchange::Died(rc)
                }
            }
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                let parts: Vec<&str> = trimmed.split_whitespace().collect();
                if parts.len() == 3 && parts[0] == "done" && parts[1].parse::<u64>() == Ok(id) {
                    let rc = parts[2].parse::<i32>().unwrap_or(0);
                    Exchange::Survived(rc)
                } else {
                    // Out-of-sync framing — discard the connection.
                    Exchange::Protocol(format!("unexpected runner response: {trimmed:?}"))
                }
            }
        }
    }
}

impl Drop for TestBatchRunner {
    fn drop(&mut self) {
        // Graceful shutdown: close stdin (runner reads EOF → exits) and
        // reap. Best-effort; a hung runner is killed.
        if let Some(mut conn) = self.conn.take() {
            use std::io::Write;
            let _ = conn.stdin.write_all(b"quit\n");
            let _ = conn.stdin.flush();
            drop(conn.stdin);
            // The runner exits within ~1 ms of the stdin EOF above, so poll
            // at fine granularity to keep `karac test`'s exit latency low
            // (this Drop is on every run's critical path); the 2 s deadline
            // is just a wedged-runner backstop.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match conn.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        let _ = conn.child.kill();
                        let _ = conn.child.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                    Err(_) => break,
                }
            }
        }
    }
}

/// Result of one `exchange` round-trip with the batch runner.
enum Exchange {
    /// Runner survived; the test's `main` returned this code (0 = pass).
    Survived(i32),
    /// Runner died inside the test (the failure path) with this reaped
    /// exit code.
    Died(i32),
    /// The watchdog killed a hung test.
    TimedOut,
    /// Framing desync — the connection is unusable.
    Protocol(String),
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

    /// `build_skeleton` stubs every concrete (non-generic) free-fn and
    /// impl-method body to a single `unreachable()` call, clears its
    /// `requires`/`ensures`, and preserves the original body span — while
    /// leaving generic fns, type defs, and signatures untouched.
    #[test]
    fn build_skeleton_stubs_concrete_bodies_only() {
        use crate::ast::{Block, ExprKind, ImplItem, Item, StmtKind};
        let src = "\
struct Point { x: i64, y: i64 }
impl Point {
    fn sum(self) -> i64 { self.x + self.y }
    fn gen_method[T](self, v: T) -> T { v }
}
fn helper(n: i64) -> i64 requires n > 0 { n + 1 }
fn generic[T](a: T) -> T { a }
";
        let prog = crate::parse(src).program;
        let skel = build_skeleton(&prog);

        // A 1-stmt `{ unreachable() }` block whose stmt span equals the
        // original body span (so no collision with the synth main's (0,0)).
        let is_stub = |body: &Block, orig_span: &Span| -> bool {
            body.final_expr.is_none()
                && body.stmts.len() == 1
                && body.span.offset == orig_span.offset
                && matches!(
                    &body.stmts[0].kind,
                    StmtKind::Expr(Expr { kind: ExprKind::Call { callee, args }, .. })
                        if args.is_empty()
                            && matches!(&callee.kind, ExprKind::Identifier(n) if n == "unreachable")
                )
        };

        // Capture original body spans before comparing (clone of the parse).
        let orig = crate::parse(src).program;
        let orig_helper_span = match &orig.items[2] {
            Item::Function(f) => f.body.span.clone(),
            _ => panic!("expected helper fn at index 2"),
        };

        // Concrete free fn `helper` — stubbed, contracts cleared.
        match &skel.items[2] {
            Item::Function(f) => {
                assert_eq!(f.name, "helper");
                assert!(
                    is_stub(&f.body, &orig_helper_span),
                    "helper body not stubbed"
                );
                assert!(f.requires.is_empty(), "requires not cleared");
                assert!(f.ensures.is_empty(), "ensures not cleared");
            }
            _ => panic!("expected helper fn"),
        }
        // Generic free fn `generic` — body untouched (kept real: emitted only
        // on instantiation, which a no-fixture main never triggers).
        match &skel.items[3] {
            Item::Function(f) => {
                assert_eq!(f.name, "generic");
                assert!(
                    !is_stub(&f.body, &f.body.span),
                    "generic fn body must NOT be stubbed"
                );
            }
            _ => panic!("expected generic fn"),
        }
        // Impl methods: concrete `sum` stubbed, generic `gen_method` untouched.
        match &skel.items[1] {
            Item::ImplBlock(imp) => {
                for ii in &imp.items {
                    if let ImplItem::Method(m) = ii {
                        if m.name == "sum" {
                            assert!(is_stub(&m.body, &m.body.span), "sum body not stubbed");
                        } else if m.name == "gen_method" {
                            assert!(
                                !is_stub(&m.body, &m.body.span),
                                "generic method body must NOT be stubbed"
                            );
                        }
                    }
                }
            }
            _ => panic!("expected impl block"),
        }
        // Struct def is structurally preserved (signatures untouched).
        assert!(matches!(&skel.items[0], Item::StructDef(s) if s.name == "Point"));
    }
}
