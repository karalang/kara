//! effects, contracts, ambient state, panics, defer/errdefer -- fixtures for `tests/codegen.rs`.
//!
//! Split out of `tests/codegen.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test codegen` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test codegen effects::
//!
//! New fixtures about effects, contracts, ambient state, panics, defer/errdefer belong in this file.

use super::*;

#[test]
fn e2e_assert_two_arg_message_form_compiles_and_runs() {
    // B-2026-07-18-26: the 2-arg `assert(cond, "msg")` form (accepted by
    // the typechecker + interpreter, and emitted by the compiler for tensor
    // shape-checks) was REJECTED by codegen ("assert() expects 1 argument,
    // found 2") — a run-vs-build divergence. Codegen now accepts 1-or-2 args
    // and threads a string-literal message. A passing 2-arg assert must run
    // cleanly and fall through to the rest of `main`.
    if let Some(out) = run_program(
        "fn main() {\n\
                 assert(1 == 1, \"never fires\");\n\
                 assert(2 > 1);\n\
                 println(42);\n\
             }",
    ) {
        assert_eq!(out, "42\n");
    }
}

/// `#[track_caller]` codegen (phase-5 slices 4+5): a panic inside a
/// `#[track_caller]` fn reports the CALLER's source location, while the
/// `in <fn>` frame name still identifies the emitting function. Pins the
/// baseline (a non-`#[track_caller]` fn reports its OWN location) and
/// transitivity (a `#[track_caller]` fn calling another forwards the
/// received location). The `\`-continued strings strip leading whitespace,
/// so each Kāra statement sits at column 1 and only the line number varies.
#[test]
fn e2e_track_caller_redirects_panic_location() {
    // boom() panics on line 3; main calls it on line 6. The attribute makes
    // the reported location the caller's line 6, not the callee's line 3.
    let with_attr = "#[track_caller]\n\
                         fn boom() {\n\
                         unreachable()\n\
                         }\n\
                         fn main() {\n\
                         boom();\n\
                         }\n";
    if let Some(cap) = run_program_capturing(with_attr) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains(":6:") && cap.stderr.contains("in boom"),
            "track_caller must report the caller's line (6) with the emitting frame name; \
                 stdout={:?}",
            cap.stdout
        );
        assert!(
            !cap.stdout.contains(":3:"),
            "the emitting frame's own line (3) must be redirected away; stdout={:?}",
            cap.stdout
        );
    }

    // Baseline: without the attribute, the panic is NOT redirected to the
    // caller. The `#[track_caller]` location travels as a RUNTIME arg (from
    // the call site's span), so it survives even here where the harness does
    // not thread `source_filename`; the compile-time-location path, by
    // contrast, is gated on `source_filename` and stays inert under
    // `run_program_capturing`, yielding the bare `panic: <msg>` form. So the
    // load-bearing contrast the harness can observe is: the attribute ADDS a
    // caller location (`panic at …:5:… in boom`) the baseline lacks. (Under a
    // real `karac build` the baseline would report its OWN line 2 via the
    // compile-time span path — covered by the manual E2E, not reproducible
    // in this harness.)
    let no_attr = "fn boom() {\n\
                       unreachable()\n\
                       }\n\
                       fn main() {\n\
                       let x = 1;\n\
                       let _ = x;\n\
                       boom();\n\
                       }\n";
    if let Some(cap) = run_program_capturing(no_attr) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            !cap.stderr.contains("panic at") && !cap.stderr.contains(":5:"),
            "baseline must not redirect to the caller's line (5); stderr={:?}",
            cap.stderr
        );
        assert!(
            cap.stderr.contains("entered unreachable code"),
            "baseline still fires the same underlying panic; stderr={:?}",
            cap.stderr
        );
    }

    // Transitivity: main calls outer() on line 12; outer (track_caller)
    // forwards its received location to inner (track_caller), which panics.
    // The reported location is main's call site (12), frame name `inner`.
    let transitive = "#[track_caller]\n\
                          fn inner() {\n\
                          unreachable()\n\
                          }\n\
                          #[track_caller]\n\
                          fn outer() {\n\
                          inner()\n\
                          }\n\
                          fn main() {\n\
                          let x = 1;\n\
                          let _ = x;\n\
                          outer();\n\
                          }\n";
    if let Some(cap) = run_program_capturing(transitive) {
        assert_eq!(cap.status.code(), Some(101), "stderr={:?}", cap.stderr);
        assert!(
            cap.stderr.contains(":12:") && cap.stderr.contains("in inner"),
            "transitivity must forward the caller's line (12) to the innermost frame; \
                 stderr={:?}",
            cap.stderr
        );
    }
}

#[test]
fn test_e2e_ambient_resource_clock_now_and_env_set() {
    // Ambient built-in resource methods lower to runtime FFIs
    // (`karac_runtime_env_set` / `karac_runtime_clock_now`) — the
    // codegen counterpart of the interpreter's BuiltinDefault
    // dispatch. `clock.now()` returns a positive Unix timestamp;
    // `env.set` runs without error. Regression guard for the
    // dispatch fall-through that previously errored with
    // "no handler for method 'set' on variable 'env'".
    let out = run_program(
        r#"
fn main() writes(Env) reads(Clock) {
    env.set("KARA_E2E_X", "y");
    let t = clock.now();
    if t > 0 { println("clock-ok"); } else { println("clock-bad"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "clock-ok");
    }
}

#[test]
fn test_e2e_ambient_env_args_returns_nonempty() {
    // `env.args()` -> Vec[String] lowers to the
    // `karac_runtime_env_args_into` out-pointer FFI (first
    // aggregate-returning ambient method). Under the E2E test binary,
    // argv[0] is the spawned exe path, so the Vec is guaranteed
    // non-empty; we assert `len() > 0` rather than exact contents
    // (environment-dependent). Mirrors the interpreter's
    // `test_ambient_env_args_returns_nonempty_array`. Regression guard
    // for "ambient resource method 'Env.args' is not yet lowered".
    let out = run_program(
        r#"
fn main() reads(Env) {
    let a = env.args();
    if a.len() > 0 { println("args-ok"); } else { println("args-empty"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "args-ok");
    }
}

#[test]
fn test_e2e_ambient_env_args_capitalized_form() {
    // The capitalized `Env.args()` form must reach the same FFI lowering
    // as the lowercase `env.args()` alias. Before slice 2 the capitalized
    // form fell through to `compile_assoc_call` and errored "no handler";
    // the `ambient_ffi_lowered` routing gate (call_dispatch.rs) now sends
    // no-vtable-slot ambient pairs to `compile_ambient_resource_method`.
    let out = run_program(
        r#"
fn main() reads(Env) {
    let a = Env.args();
    if a.len() > 0 { println("args-ok"); } else { println("args-empty"); }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "args-ok");
    }
}

#[test]
fn test_e2e_ambient_env_var_present_returns_ok() {
    // `env.var(name) -> Result[String, VarError]`. Set a unique var with
    // `env.set`, then read it back: the codegen `("Env","var")` arm calls
    // the `karac_runtime_env_var` FFI (found=true, heap String written)
    // and builds `Result.Ok(string)`, which the match destructures.
    // Exercises runtime-conditional enum construction + the seeded
    // `VarError`/`Result` layouts. Mirrors the interpreter's
    // `test_ambient_env_var_present_returns_ok`.
    let out = run_program(
        r#"
fn main() writes(Env) reads(Env) {
    env.set("KARAC_E2E_VAR_PRESENT", "hello-var");
    match env.var("KARAC_E2E_VAR_PRESENT") {
        Ok(v) => { println(v); }
        Err(_) => { println("missing"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "hello-var");
    }
}

#[test]
fn test_e2e_ambient_env_var_missing_returns_err() {
    // A guaranteed-absent key: the FFI returns found=false and codegen
    // builds `Result.Err(VarError.NotPresent)`, which the match's Err arm
    // takes. Pins the not-found half of the runtime branch + the
    // `VarError.NotPresent` construction. Mirrors the interpreter's
    // `test_env_var_missing_key_returns_err`.
    let out = run_program(
        r#"
fn main() reads(Env) {
    match env.var("__KARAC_E2E_NO_SUCH_VAR_ZZZ__") {
        Ok(v) => { println(v); }
        Err(_) => { println("missing"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "missing");
    }
}

#[test]
fn test_e2e_ambient_env_var_capitalized_form() {
    // The capitalized `Env.var(name)` form reaches the same FFI lowering
    // as the lowercase alias via the `ambient_ffi_lowered` routing gate.
    let out = run_program(
        r#"
fn main() writes(Env) reads(Env) {
    Env.set("KARAC_E2E_VAR_CAP", "cap-val");
    match Env.var("KARAC_E2E_VAR_CAP") {
        Ok(v) => { println(v); }
        Err(_) => { println("missing"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "cap-val");
    }
}

#[test]
fn test_e2e_ambient_stdin_read_line_eof_returns_ok_empty() {
    // `Stdin.read_line() -> Result[String, IoError]`. The E2E harness
    // spawns the binary via `Command::output`, which closes the child's
    // stdin — so `read_line` hits immediate EOF and returns `Ok("")`
    // (Rust's `read_line` yields `Ok(0)` with an empty buffer). Lowers
    // through the shared `karac_runtime_stdin_read_line` +
    // `lower_kara_io_result(StringPayload)` path, same as
    // `FileSystem.read_to_string`. The Ok arm binds the String payload
    // and checks `len() == 0`, exercising the full Result/String unpack
    // (payload-typed binding + method dispatch), not just the branch.
    // (Capitalized form is the resolvable surface — the lowercase `stdin`
    // alias is not yet wired into the resolver; see the lowercase-ambient
    // -alias gap entry in phase-7-codegen.md.)
    let out = run_program(
        r#"
fn main() reads(Stdin) {
    match Stdin.read_line() {
        Ok(s) => { if s.len() == 0 { println("eof-ok"); } else { println("got-line"); } }
        Err(_) => { println("io-err"); }
    }
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "eof-ok");
    }
}

#[test]
fn test_e2e_with_provider_ambient_override() {
    // `with_provider[Clock](FakeClock {}, || ...)` overrides the
    // ambient `Clock` resource with a statically-typed provider. The
    // capitalized `Clock.now()` call inside the body must dispatch to
    // the override's `FakeClock.now` (returning 42), NOT the builtin
    // `karac_runtime_clock_now` FFI. Static-monomorphization path —
    // the override decision is entirely compile-time (no runtime
    // provider vtable). Mirrors the interpreter's ambient override
    // (`karac run` of the same source prints 42) and the repl
    // `:provide Clock = FakeClock {}` flow. Regression guard for the
    // historical `with_provider: unknown effect resource 'Clock'`
    // codegen error (and the capitalized-Clock.now()-returns-0 bug).
    let out = run_program(
        r#"
struct FakeClock {}
impl FakeClock { fn now(ref self) -> i64 { 42 } }
fn main() reads(Clock) {
    with_provider[Clock](FakeClock {}, || {
        println(Clock.now());
    });
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "42");
    }
}

/// B-2026-07-09-9: `panic()` is a diverging prelude primitive, lowered
/// inline via `compile_diverge` exactly like `todo`/`unreachable` — so a
/// `panic()` tail in a value-returning fn emits an `unreachable`
/// terminator, not a `ret <placeholder>` mismatch.
#[test]
fn test_ir_panic_tail_emits_unreachable_not_ret() {
    let ir = ir_for(
        "struct FakeClock { t: i64 }\n\
             fn boom() -> FakeClock { panic(\"kaboom\") }\n\
             fn main() { let _c = boom(); }",
    );
    let body = function_body(&ir, "boom").expect("boom body");
    assert!(
        body.contains("unreachable"),
        "diverging `panic()` tail should emit an `unreachable` terminator; body was:\n{}",
        body
    );
    assert!(
        !body.contains("ret i64"),
        "diverging `panic()` tail must not emit `ret i64 <placeholder>`; body was:\n{}",
        body
    );
}

#[test]
fn test_ir_ptr_from_exposed_compiles_inside_unsafe() {
    let ir =
        ir_for("fn caller(a: usize) -> *const i64 { unsafe { ptr.from_exposed(a) } } fn main() {}");
    assert!(
        ir.contains("@caller"),
        "caller fn should be emitted; got IR:\n{ir}"
    );
}

#[test]
fn test_e2e_panic_location_has_line_col() {
    // Pin the line:col so this is a real regression guard for the span
    // plumbing, not just a format check. With the leading newline in the
    // raw string, `fn main()` is line 2 and the panicking `v[10]` read is
    // on line 4. The column points within that indexing expression.
    let captured = run_program_capturing_with_filename(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let _x = v[10];
    println(7);
}
"#,
        "linecol.kara",
    );
    if let Some(c) = captured {
        // Format: `panic at linecol.kara:<line>:<col> in main: <msg>`.
        let marker = "linecol.kara:";
        let idx = c.stderr.find(marker).unwrap_or_else(|| {
            panic!(
                "no panic location in stderr={:?} stdout={:?}",
                c.stderr, c.stdout
            )
        });
        let rest = &c.stderr[idx + marker.len()..];
        // The two fields immediately after the filename must be numeric
        // `<line>:<col>` — proves real span data, not a `0:0` placeholder.
        let line: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        assert!(
            !line.is_empty() && line != "0",
            "expected a non-zero line number after the filename, got stdout={:?}",
            c.stdout
        );
        let after_line = &rest[line.len()..];
        assert!(
            after_line.starts_with(':'),
            "expected `:` between line and column, got stdout={:?}",
            c.stdout
        );
        let col: String = after_line[1..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        assert!(
            !col.is_empty() && col != "0",
            "expected a non-zero column number, got stdout={:?}",
            c.stdout
        );
        assert_eq!(
            line, "4",
            "panicking index is on source line 4; stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_panic_bare_form_without_filename() {
    // Regression guard for the gate: with NO filename threaded in, the
    // original `panic: <msg>` form is preserved byte-for-byte (callers
    // that don't supply a filename — bare-IR tests, ad-hoc dumps — must
    // not see the rich form).
    let captured = run_program_capturing(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let _x = v[10];
    println(7);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("panic: vec index out of bounds"),
            "expected bare panic form without a filename, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("panic at "),
            "must NOT use the rich `panic at` form when no filename is supplied, got stdout={:?}",
            c.stdout
        );
    }
}

// ── defer / errdefer codegen ─────────────────────────────────
//
// Slice 1 (Phase 7 § *defer / errdefer codegen*) pins user `defer`
// on normal scope exit only: the deferred block compiles via
// `compile_block` from the `UserDefer(Block)` cleanup variant when
// `emit_scope_cleanup` drains the function's top frame at return.
// LIFO order is mandatory per design.md § *Drop ordering within a
// branch* — "last declared, first drained". Error-exit, panic, and
// `?`-propagation paths are slice 2's scope; block-scoped dispatch
// and runtime-reachability of defers inside not-taken branches are
// tracked as a follow-on entry in phase-7-codegen.md.

#[test]
fn test_e2e_defer_fires_on_normal_scope_exit() {
    let out = run_program(
        r#"
fn main() {
    defer { println("deferred"); }
    println("inline");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["inline", "deferred"]);
    }
}

#[test]
fn test_e2e_defer_lifo_order_within_function() {
    let out = run_program(
        r#"
fn main() {
    defer { println("a"); }
    defer { println("b"); }
    println("body");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["body", "b", "a"]);
    }
}

#[test]
fn test_e2e_defer_fires_on_explicit_early_return() {
    // `return` from the function body calls `emit_scope_cleanup`
    // before `build_return`, which walks every live cleanup frame
    // LIFO — including registered user defers. Pins that the early-
    // return path drains the same cleanup stack as the normal-exit
    // path. (Block-scope dispatch and runtime-reachability — defer
    // inside a not-taken branch must NOT fire — are tracked
    // separately in phase-7-codegen.md as a follow-on; this slice
    // covers function-scoped, compile-time-registered defer.)
    let out = run_program(
        r#"
fn body() {
    defer { println("d"); }
    println("a");
    return;
    println("unreached");
}

fn main() { body(); }
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["a", "d"]);
    }
}

#[test]
fn test_e2e_defer_with_three_actions_lifo() {
    let out = run_program(
        r#"
fn main() {
    defer { println("1"); }
    defer { println("2"); }
    defer { println("3"); }
    println("0");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["0", "3", "2", "1"]);
    }
}

// --- Phase 7 § *defer codegen* slice 1.5: block-scope + runtime-reachability ---
//
// Slice 1 ships function-scoped, compile-time-registered defer:
// every `defer` lands on the function's top cleanup frame and
// fires at function exit, regardless of which enclosing block
// declared it or whether that block's runtime branch was taken.
// Slice 1.5 narrows the drain scope by pushing a fresh
// `scope_cleanup_actions` frame inside the "naked block" path
// (if-arms, bare `{ ... }` / `Seq` / `unsafe` expression-blocks,
// nested defer bodies) and draining it at block exit via
// `compile_block_with_frame`. Because the drain IR lives at the
// end of the *emitted* block body, a not-taken conditional arm
// bypasses the drain instruction entirely — runtime-reachability
// falls out of block-scoping for free. These four tests pin the
// four observable shapes that distinguish slice 1.5 from slice 1.

#[test]
fn test_e2e_defer_in_if_true_fires_at_end_of_arm() {
    // The defer is declared inside the if's then-block. Slice 1.5
    // requires it to fire when control falls off the end of that
    // block (before the merge BB), not at function exit. Expected
    // stream: `before-if` → `in-if` → `after-defer` → defer body
    // (`a`) → `after-if`.
    let out = run_program(
        r#"
fn main() {
    println("before-if");
    if true {
        println("in-if");
        defer { println("a"); }
        println("after-defer");
    }
    println("after-if");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(
            lines,
            vec!["before-if", "in-if", "after-defer", "a", "after-if"]
        );
    }
}

#[test]
fn test_e2e_defer_in_if_false_does_not_fire() {
    // Runtime-reachability: the defer's drain IR is emitted
    // inside the then-arm's BB. Because the conditional jump
    // skips that BB at runtime (`if false`), the drain
    // instruction is never executed, so the defer body never
    // runs. This is the v1 correctness gap vs the interpreter
    // that slice 1.5 closes (interpreter's
    // `test_defer_registers_when_reached_not_at_block_start`).
    let out = run_program(
        r#"
fn main() {
    println("before");
    if false {
        defer { println("not-printed"); }
    }
    println("after");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["before", "after"]);
    }
}

#[test]
fn test_e2e_defer_in_while_body_fires_per_iteration() {
    // While bodies already push a per-iteration cleanup frame
    // (`compile_while` in `src/codegen/control_flow.rs:499`)
    // so defer landed on it correctly in slice 1; this test
    // pins that semantic in a lock-in form alongside the new
    // if-arm tests above so a regression to function-scoped
    // defer (which would print all three `d`s after `after`)
    // is caught here.
    let out = run_program(
        r#"
fn main() {
    let mut i = 0;
    while i < 3 {
        defer { println("d"); }
        i = i + 1;
    }
    println("after");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["d", "d", "d", "after"]);
    }
}

// --- Phase 7 § *defer / errdefer codegen* slice 2: errdefer on error paths ---
//
// Slice 2 wires `UserErrDefer { binding: Option<String>, body: Block }`
// onto the same `scope_cleanup_actions` stack `UserDefer` lives on, then
// splits the drain into two phases routed by exit path:
//   - `emit_scope_cleanup` (normal exit) filters out `UserErrDefer` so an
//     `errdefer { ... }` declared in a normally-exiting function never
//     fires.
//   - `emit_scope_cleanup_for_error_path` (error exit) drains errdefers
//     LIFO in phase 1, then drops + defers LIFO in phase 2, innermost
//     frame first. Mirrors the interpreter's per-scope `run_cleanup`
//     shape (`src/interpreter/eval_stmt.rs:364-408`).
//
// Error-exit sites today: the `?` operator's Err-propagation branch
// (`compile_question`'s `fail_bb`), early `return Err(...)` / `return
// None` (the `ExprKind::Return` arm in `compile_expr`), and tail-position
// `Err(...)` / `None` (`compile_function`'s tail emitter). Detection is
// purely syntactic via `Codegen::is_error_exit_value`.
//
// Out of scope for slice 2 (deferred):
//   - Binding form `errdefer(e) { ... }` — pushed in slice 4; the
//     compile_stmt push gates on `binding.is_none()` so for now the
//     binding form falls through to the catch-all `_ => Ok(())` arm
//     and stays a no-op at codegen, mirroring slice 1's deferral of
//     block-scoped defer to slice 1.5.
//   - Panic landing pads — the panic-unwind story is not in this
//     compiler today (panic=abort-style), so there are no landing pads
//     to wire. Phase 7 § *defer/errdefer codegen* slice 2 design called
//     this out for coordination if the panic story landed first.

#[test]
fn test_e2e_errdefer_fires_on_explicit_return_err() {
    // Param-less errdefer fires when control exits via
    // `return Err(...)`. Mirrors interpreter
    // `test_errdefer_fires_on_err_return`.
    let out = run_program(
        r#"
fn body() -> Result[i64, i64] {
    defer { println("d"); }
    errdefer { println("e"); }
    return Err(7_i64);
}
fn main() {
    match body() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Phase 1 (errdefer) then phase 2 (defer), then caller's
        // match arm. errdefer prints "e" before defer's "d".
        assert_eq!(lines, vec!["e", "d", "caller-err"]);
    }
}

#[test]
fn test_e2e_errdefer_skipped_on_normal_return() {
    // errdefer must NOT fire on a normal `return Ok(...)`; defer
    // always fires. Mirrors interpreter
    // `test_errdefer_skipped_on_normal_return`.
    let out = run_program(
        r#"
fn body() -> Result[i64, i64] {
    defer { println("d"); }
    errdefer { println("e"); }
    return Ok(1_i64);
}
fn main() {
    match body() {
        Ok(_) => println("caller-ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["d", "caller-ok"]);
    }
}

#[test]
fn test_e2e_errdefer_phase1_before_defer_lifo() {
    // Phase ordering pin: on an error path the LIFO drain runs
    // (errdefer e2, errdefer e1) in phase 1, then (defer d2, defer
    // d1) in phase 2 — full output `e2 e1 d2 d1`. Mirrors the
    // interpreter's `test_errdefer_runs_before_defer_on_error_path`
    // shape.
    let out = run_program(
        r#"
fn body() -> Result[i64, i64] {
    defer { println("d1"); }
    errdefer { println("e1"); }
    defer { println("d2"); }
    errdefer { println("e2"); }
    return Err(0_i64);
}
fn main() {
    match body() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["e2", "e1", "d2", "d1", "caller-err"]);
    }
}

#[test]
fn test_e2e_errdefer_fires_on_tail_err_expression() {
    // Function-tail error-exit: when the function's final
    // expression is syntactically `Err(...)` (no leading `return`),
    // `compile_function`'s tail emitter routes through
    // `emit_scope_cleanup_for_error_path` and the errdefer fires.
    // Distinguishes the tail-expression path from the explicit
    // `return` path covered above — both flow through the same
    // `is_error_exit_value` detector but different emitters.
    let out = run_program(
        r#"
fn body() -> Result[i64, i64] {
    errdefer { println("tail-err-cleanup"); }
    Err(42_i64)
}
fn main() {
    match body() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["tail-err-cleanup", "caller-err"]);
    }
}

#[test]
fn test_e2e_defer_lifo_with_two_let_vecs_interleave() {
    // Mirrors design.md's canonical example:
    //     let v = Vec.new(); defer A; let w = Vec.new(); defer B;
    // → push order `[FreeVec(v), Defer A, FreeVec(w), Defer B]`,
    //   LIFO drain `[Defer B, FreeVec(w), Defer A, FreeVec(v)]`.
    //
    // Each defer's body prints its respective Vec's `.len()`.
    // The interleaved LIFO drain order guarantees both reads land
    // BEFORE the matching `FreeVec` for that Vec: defer B reads w
    // before FreeVec(w); defer A reads v before FreeVec(v). The
    // stdout order is `wlen=2` then `vlen=1` because B drains
    // before A (LIFO between the two defers, both alive on the
    // same unified stack). Distinguishes from "all drops then all
    // defers" by the live read; the LIFO-vs-FIFO defer order pin
    // is additional.
    let out = run_program(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1_i64);
    defer { println(f"vlen={v.len()}"); }
    let mut w: Vec[i64] = Vec.new();
    w.push(10_i64);
    w.push(20_i64);
    defer { println(f"wlen={w.len()}"); }
    println("body");
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines, vec!["body", "wlen=2", "vlen=1"]);
    }
}

// --- Phase 7 § *defer / errdefer codegen* slice 4: errdefer(e) binding form ---
//
// Slice 4 lifts the `binding.is_none()` gate in `compile_stmt`'s
// ErrDefer arm so `errdefer(e) { ... }` also lands on the unified
// `scope_cleanup_actions` frame. The binding-form dispatch in
// `emit_cleanup_action_at`'s UserErrDefer branch reads
// `pending_errdefer_payload` (staged by each error-exit site
// immediately before `emit_scope_cleanup_for_error_path`),
// allocates an entry-block alloca of the payload's LLVM type,
// stores the staged value, and registers the binding in
// `self.variables` for the duration of the body's
// `compile_block_with_frame` call. After the body emits, the prior
// `variables[name]` (if any) is restored; otherwise the slot is
// removed.
//
// Three staging sites stage `pending_errdefer_payload`:
//   1. `compile_question`'s `fail_bb` — stages `w0` (the i64-wide
//      payload word extracted from the result struct's field 1).
//   2. `ExprKind::Return(Err(arg))` in `compile_expr` — re-compiles
//      `arg` to get the source-level value (rather than peeling
//      fields off the constructed Err struct).
//   3. Function-tail `Err(arg)` in `compile_function` — same shape
//      as the explicit-return site.
//
// Out of scope for slice 4 (deferred):
//   - Wider E payloads (`Result[T, String]`, `Result[T, struct]`):
//     today the `?` site extracts only `w0` (one i64), so the
//     binding form sees a coerced i64 word rather than the
//     reconstructed source-level value when E is multi-word. The
//     explicit-return and tail-Err paths re-compile the inner Err
//     arg directly, so they work for any payload type; the `?`
//     path is the residual gap.
//   - Double-evaluation of side-effecting payload expressions in
//     the explicit-return and tail-Err sites: `return Err(expensive())`
//     calls `expensive()` once for the return-struct construction
//     and again to stage the binding-form payload. Acceptable for
//     v1's narrow common case (typed `i64` / identifier payloads)
//     but pinned as a known limitation.

#[test]
fn test_e2e_errdefer_with_binding_on_explicit_return_err() {
    // Mirrors interpreter `test_errdefer_with_binding_sees_err_payload`
    // (`tests/interpreter.rs:1288`). The `errdefer(e) { println(e); }`
    // body binds `e` to the about-to-be-returned Err payload from
    // `return Err(7_i64)`. Without slice 4, the binding form was
    // gated out at `compile_stmt`; with slice 4 wired, the binding
    // is registered into `self.variables` during the body's
    // emission and the print resolves to the staged payload.
    let out = run_program(
        r#"
fn body() -> Result[i64, i64] {
    errdefer(e) { println(e); }
    return Err(7_i64);
}
fn main() {
    match body() {
        Ok(_) => println(0_i64),
        Err(_) => println(1_i64),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // errdefer(e) prints the bound 7, then the caller's
        // Err arm prints 1.
        assert_eq!(lines, vec!["7", "1"]);
    }
}

#[test]
fn test_e2e_errdefer_with_binding_no_double_eval_on_impure_err_arg() {
    // Slice 4 follow-up (b) — double-eval fix (2026-05-26).
    // Slice 4 staged the Err binding-form payload by unconditionally
    // re-compiling `payload_expr`, which evaluated side-effecting Err
    // args twice (`return Err(some_io());` → `some_io()` ran for the
    // return-struct construction AND again to stage the payload).
    //
    // This test pins single-evaluation via `Self::is_pure_recompilable`:
    // `make_err()` is an impure call (writes "called" to stdout), so it
    // routes through the extract-from-v path rather than being
    // re-compiled. With the fix, "called" appears exactly once in output;
    // pre-fix it would have appeared twice.
    //
    // B-2026-08-23-19 — WHAT THE BINDING SEES has changed, and the
    // comment here used to describe the defect. That extract-from-v path
    // staged the constructed Err struct's field 1 as a raw i64 and the
    // text called it "the i64-coerced payload", framing it as a precision
    // trade. It is free only when `E` IS an integer, which is exactly the
    // case this test uses (`E = i64`, payload `7`) — which is why this
    // test kept passing while `Err(msg.to_string())` handed its cleanup
    // block a data pointer. The impure path now reconstructs the
    // source-typed value through `rebuild_value_from_payload_words`, the
    // same helper the `?` site uses; for `E = i64` that is still `7`, so
    // the assertion below is unchanged. The non-integer half of the
    // matrix — where the old staging was simply wrong — is pinned across
    // all three backends by `test_errdefer_binding_value_parity_across_backends`
    // in `tests/cli.rs`.
    //
    // The binding-form errdefer reads the staged payload (`7` here) and
    // prints it.
    let out = run_program(
        r#"
fn make_err() -> i64 {
    println("called");
    7_i64
}
fn body() -> Result[i64, i64] {
    errdefer(e) { println(e); }
    return Err(make_err());
}
fn main() {
    match body() {
        Ok(_) => println("ok"),
        Err(_) => println("caller-err"),
    }
}
"#,
    );
    if let Some(out) = out {
        let lines: Vec<&str> = out.trim().lines().collect();
        // Expected sequence:
        //   "called" — make_err() runs exactly ONCE for the Err
        //              return struct's construction (single eval).
        //   "7"      — errdefer(e) prints the staged i64 payload
        //              (extracted from the constructed struct's
        //              field 1).
        //   "caller-err" — the caller's Err arm.
        assert_eq!(lines, vec!["called", "7", "caller-err"]);
        // Stronger assertion against regression: count "called"
        // occurrences. Pre-fix it would have been 2 (double-eval).
        let called_count = out.matches("called").count();
        assert_eq!(
                called_count, 1,
                "impure Err arg should evaluate exactly once; got {called_count} `called` lines:\n{out}",
            );
    }
}

// ── Slice c.1 — assert / assert_eq / assert_ne codegen lowering ──
//
// Pre-c.1 the codegen path silently dropped these prelude calls
// (unknown-callee fallback returning const-0), so any kara program
// calling `assert_eq` from `karac build` ran past the assertion as
// if it had succeeded. c.1 lowers each to `karac_test_record_failure`
// (stderr JSONL) + `exit(1)` on failure; pass paths flow through
// unchanged. Tests assert both halves: success runs the trailing
// code, failure emits the marker and short-circuits.

#[test]
fn test_assert_true_continues() {
    let out = run_program(
        r#"
fn main() {
    assert(true)
    println(7)
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "7");
    }
}

#[test]
fn test_assert_false_emits_failure_and_short_circuits() {
    let captured = run_program_capturing(
        r#"
fn main() {
    assert(false)
    println(99)
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("KARAC_TEST_FAILURE "),
            "expected failure marker; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("\"message\":\"assertion failed\""),
            "expected bare 'assertion failed' message; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("\"left\":null") && c.stderr.contains("\"right\":null"),
            "bare assert(cond) should emit null left/right; got stderr={:?}",
            c.stderr
        );
        assert!(
            !c.stdout.contains("99"),
            "code after failing assert must not run; stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_assert_ne_pass() {
    let out = run_program(
        r#"
fn main() {
    assert_ne(1, 2)
    println(42)
}
"#,
    );
    if let Some(s) = out {
        assert_eq!(s.trim(), "42");
    }
}

#[test]
fn test_assert_eq_bool_fail() {
    let captured = run_program_capturing(
        r#"
fn main() {
    assert_eq(true, false)
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("\"left\":\"true\"") && c.stderr.contains("\"right\":\"false\""),
            "expected formatted true/false; got stderr={:?}",
            c.stderr
        );
    }
}

#[test]
fn test_assert_record_failure_carries_call_site_line_col() {
    // Filename threaded through codegen lands in the JSON `file` field.
    let captured = run_program_capturing_with_filename(
        r#"
fn main() {
    assert_eq(1, 2)
}
"#,
        "demo.kara",
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("\"file\":\"demo.kara\""),
            "expected file=\"demo.kara\"; got stderr={:?}",
            c.stderr
        );
        // The `assert_eq` call sits on line 3, column 5 of the source above.
        assert!(
            c.stderr.contains("\"line\":3"),
            "expected line=3; got stderr={:?}",
            c.stderr
        );
        assert!(
            c.stderr.contains("\"column\":5"),
            "expected column=5; got stderr={:?}",
            c.stderr
        );
    }
}

#[test]
fn test_e2e_unsigned_requires_contract_accepts() {
    // `requires` / `ensures` are the same shape — a synthesized predicate
    // that never went through lowering — and were broken identically.
    let out = run_program(
        r#"
fn clamp_port(p: u16) -> u16
    requires p <= 65535
{
    p
}
fn main() {
    let r = clamp_port(80);
    println("contract-ok");
}
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "contract-ok");
    }
}

// ── Contracts — requires preconditions (codegen / AOT) ─────────

#[test]
fn test_e2e_contract_requires_holds() {
    let out = run_program(
        r#"
fn checked(x: i64) -> i64 requires x > 0 { x * 2 }
fn main() { println(checked(5)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_contract_requires_violation_aborts() {
    // A failed `requires` aborts with `contract violated`, and code after
    // the call does not run.
    let captured = run_program_capturing(
        r#"
fn checked(x: i64) -> i64 requires x > 0 { x * 2 }
fn main() {
    println(checked(-3));
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected a contract-violation abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after a violated requires must not run"
        );
    }
}

#[test]
fn test_e2e_contract_method_requires_aborts() {
    // `requires` is emitted on the method-dispatch path too.
    let captured = run_program_capturing(
        r#"
struct C { n: i64 }
impl C { fn step(self, by: i64) -> i64 requires by > 0 { self.n + by } }
fn main() {
    let c = C { n: 5 };
    println(c.step(-1));
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected a method-requires abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

// ── Contracts — ensures + old() postconditions (codegen / AOT) ──

#[test]
fn test_e2e_contract_ensures_holds() {
    let out = run_program(
        r#"
fn double(x: i64) -> i64 ensures(result) result > x { x * 2 }
fn main() { println(double(5)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "10");
    }
}

#[test]
fn test_e2e_contract_ensures_violation_aborts() {
    let captured = run_program_capturing(
        r#"
fn bad(x: i64) -> i64 ensures(result) result > 100 { x }
fn main() { println(bad(5)); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected an ensures abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_ensures_old_holds() {
    // The canonical `old()` postcondition runs in an AOT binary: the
    // pre-state balance is captured at entry and read back at exit.
    let out = run_program(
        r#"
struct Account { balance: i64 }
impl Account {
    pub fn withdraw(mut ref self, amount: i64) -> i64
        ensures(result) self.balance == old(self.balance) - amount
    { self.balance = self.balance - amount; amount }
}
fn main() { let mut a = Account { balance: 100 }; println(a.withdraw(30)); }
"#,
    );
    if let Some(out) = out {
        assert_eq!(out.trim(), "30");
    }
}

#[test]
fn test_e2e_contract_ensures_old_violation_aborts() {
    let captured = run_program_capturing(
        r#"
struct Account { balance: i64 }
impl Account {
    pub fn withdraw(mut ref self, amount: i64) -> i64
        ensures(result) self.balance == old(self.balance) - amount
    { self.balance = self.balance - 999; amount }
}
fn main() { let mut a = Account { balance: 100 }; println(a.withdraw(30)); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected an old() ensures abort, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_ensures_on_explicit_return() {
    // The check fires on an explicit `return`, not just the tail.
    let captured = run_program_capturing(
        r#"
fn f(x: i64) -> i64 ensures(result) result > 100 { if x > 0 { return x; } 999 }
fn main() { println(f(5)); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected an ensures abort on the explicit-return path, got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_ir_contracts_present_in_debug_default() {
    // Baseline: with the default (debug) settings, contract asserts ARE
    // emitted — the `contract violated` fault string appears in the IR.
    let ir = ir_for(ALL_CONTRACTS_SRC);
    assert!(
        ir.contains("contract violated"),
        "debug build must emit contract asserts; IR had no `contract violated` marker"
    );
}

#[test]
fn test_ir_strip_contracts_keeps_function_bodies() {
    // Stripping removes only contracts, not the checked code: the bodies
    // still compile (the module still defines `checked` and `main`, and
    // the `x * 2` multiply survives).
    let ir = ir_for_contracts_stripped(ALL_CONTRACTS_SRC);
    assert!(
        ir.contains("define") && ir.contains("@checked") && ir.contains("@main"),
        "stripped module must still define the functions; IR:\n{ir}"
    );
    assert!(
        ir.contains("mul"),
        "stripped `checked` must still emit its `x * 2` body; IR:\n{ir}"
    );
}

#[test]
fn test_ir_strip_contracts_requires_only() {
    // A `requires`-only function: the precondition assert is present by
    // default and gone when stripped.
    let src = r#"
fn checked(x: i64) -> i64 requires x > 0 { x * 2 }
fn main() { println(checked(5)); }
"#;
    assert!(ir_for(src).contains("contract violated"));
    assert!(!ir_for_contracts_stripped(src).contains("contract violated"));
}

// ── Contracts — `contract predicate panicked` fault category (AOT) ──
//
// design.md § Contracts rule 2: a predicate that *returns false* is
// `contract violated`; a predicate whose *evaluation* faults (index OOB,
// div-by-zero, unwrap) is the distinct `contract predicate panicked`.
// These mirror the interpreter coverage in tests/interpreter.rs for the
// AOT path: panics emitted while compiling the predicate carry the
// distinct prefix.

#[test]
fn test_e2e_contract_predicate_panicked_is_distinct() {
    // `requires v[i] >= 0` with `i` out of range: the `v[i]` bounds check
    // fires during predicate evaluation — the distinct
    // `contract predicate panicked` fault, NOT `contract violated`.
    let captured = run_program_capturing(
        r#"
fn at(v: ref Vec[i64], i: i64) -> i64 requires v[i] >= 0 { 0 }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    println(at(v, 99));
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract predicate panicked"),
            "expected a `contract predicate panicked` fault, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("contract violated"),
            "a panicking predicate must NOT report `contract violated`, got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_violated_distinct_from_panicked() {
    // A predicate that simply returns false is `contract violated`, NOT
    // `contract predicate panicked` — the flag is cleared before the
    // explicit false-branch.
    let captured = run_program_capturing(
        r#"
fn pos(x: i64) -> i64 requires x > 0 { x }
fn main() { println(pos(-5)); println(42); }
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract violated"),
            "expected `contract violated` for a false predicate, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("predicate panicked"),
            "a false predicate must NOT report panicked, got stdout={:?}",
            c.stdout
        );
    }
}

#[test]
fn test_e2e_contract_predicate_panicked_in_ensures() {
    // The distinct category fires for an `ensures` predicate too:
    // `v[result]` with `result` out of range panics during evaluation.
    let captured = run_program_capturing(
        r#"
fn f(v: ref Vec[i64]) -> i64 ensures(result) v[result] >= 0 { 99 }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    println(f(v));
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
            c.stderr.contains("contract predicate panicked"),
            "expected `contract predicate panicked` in an ensures, got stdout={:?} stderr={:?}",
            c.stdout,
            c.stderr
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

#[test]
fn test_e2e_contract_predicate_panicked_cross_call() {
    // The cross-call case: the `requires` predicate calls a helper, and the
    // panic (`v[i]` OOB) fires inside the HELPER's body — a separate
    // function compiled with no lexical predicate context. The prior
    // compile-time flag could not categorize this (the helper's panic site
    // compiles with the flag clear); the runtime depth counter set around
    // the predicate's evaluation does, so it still reports
    // `contract predicate panicked`, NOT a plain `panic:` and NOT
    // `contract violated`. This is the divergence-from-interpreter the
    // step-7 inline slice left open, now closed.
    let captured = run_program_capturing(
        r#"
fn deref_at(v: ref Vec[i64], i: i64) -> bool { v[i] >= 0 }
fn at(v: ref Vec[i64], i: i64) -> i64 requires deref_at(v, i) { 0 }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1); v.push(2); v.push(3);
    println(at(v, 99));
    println(42);
}
"#,
    );
    if let Some(c) = captured {
        assert!(
                c.stderr.contains("contract predicate panicked"),
                "a panic inside a fn the predicate CALLS must report `contract predicate panicked`, got stdout={:?} stderr={:?}",
                c.stdout,
                c.stderr
            );
        assert!(
            !c.stdout.contains("contract violated"),
            "a cross-call predicate panic must NOT report `contract violated`, got stdout={:?}",
            c.stdout
        );
        assert!(
            !c.stdout.contains("42"),
            "code after the abort must not run"
        );
    }
}

// ── Contracts — contract-free panic-prefix fold ──────────────────
//
// A program with no contract can never have a non-zero predicate depth,
// so `emit_panic` folds the fault-category prefix to the static `""`
// instead of calling `karac_runtime_panic_prefix()`. No call → no
// relocation → the runtime archive member carrying the thread-local
// depth counter is never pulled in, and its writable 16 KiB __DATA page
// dead-strips from every contract-free binary; the panic landing pad
// stays a static-string leaf (the unconditional call regressed a
// bounds-check-hot loop 1.34× — kata-5, 2026-06-05). Contracted
// programs keep the runtime read — the feature is correct, only its
// unconditional cost was the defect.

#[test]
fn test_ir_contract_free_panic_folds_static_prefix() {
    // A bounds-checked index is a panic site, but the program declares no
    // contract: the prefix must fold static — no runtime-prefix call.
    let ir = ir_for(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(v[0]);
}
"#,
    );
    assert!(
        !ir.contains("call ptr @karac_runtime_panic_prefix"),
        "contract-free program must not CALL karac_runtime_panic_prefix"
    );
    assert!(
        ir.contains("panic_prefix_static"),
        "expected the folded static empty-string prefix at the panic site"
    );
}

#[test]
fn test_ir_panic_bodies_outlined_cold() {
    // Panic bodies (printf + exit) are outlined into per-site zero-arg
    // `internal` `cold`+`noinline`+`noreturn` functions; the landing pad
    // in the enclosing function is a single zero-operand call. This keeps
    // panic sites near-free for the LLVM inline cost model — growing the
    // panic-site printf to 7 operands (fault prefix + location, both
    // 2026-05-31) pushed bounds-check-bearing functions past the O2
    // inline threshold and regressed kata-5's hot loop 1.34× (the
    // un-inlined helper re-ran two loop-invariant guards per iteration).
    let ir = ir_for(
        r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(v[0]);
}
"#,
    );
    assert!(
        ir.contains("define internal void @__karac_panic_site_0()"),
        "panic body must be outlined into an internal per-site function"
    );
    assert!(
        ir.contains("call void @__karac_panic_site_0()"),
        "the landing pad must be a zero-operand call to the outlined body"
    );
    // The printf must live in the outlined body, not the landing pad:
    // the attribute group attached to the site fn carries cold+noinline.
    let attrs_line = ir
        .lines()
        .find(|l| l.starts_with("attributes") && l.contains("cold") && l.contains("noinline"))
        .unwrap_or("");
    assert!(
        attrs_line.contains("noreturn"),
        "outlined panic body must be cold+noinline+noreturn, got: {attrs_line:?}"
    );
}

#[test]
fn test_ir_contracted_program_keeps_runtime_prefix() {
    // Any `requires` anywhere in the program keeps the runtime read at
    // EVERY panic site — a panic in a function the predicate transitively
    // calls must still categorize as `contract predicate panicked`.
    let ir = ir_for(
        r#"
fn pos(x: i64) -> i64 requires x > 0 { x }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(v[0] + pos(1));
}
"#,
    );
    assert!(
        ir.contains("call ptr @karac_runtime_panic_prefix"),
        "a program with a contract must keep the runtime prefix read"
    );
}

#[test]
fn test_ir_contracts_stripped_panic_folds_static_prefix() {
    // Stripped contracts (release) emit no predicate brackets, so the
    // prefix folds static even when the source declares contracts.
    let ir = ir_for_contracts_stripped(
        r#"
fn pos(x: i64) -> i64 requires x > 0 { x }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    println(v[0] + pos(1));
}
"#,
    );
    assert!(
        !ir.contains("call ptr @karac_runtime_panic_prefix"),
        "contracts-stripped program must not call karac_runtime_panic_prefix"
    );
}

// ── std.tracing — emission surface (StdoutExporter) E2E ──────────
//
// Proves the baked `std.tracing` surface compiles + links + runs as a
// real binary, not just IR-shape: the struct layouts, the builder
// methods (`LogEvent.info` / `Span.root` / `.child` / `.with_field` /
// `.in_span`), and the `StdoutExporter` trait-impl bodies all lower
// through codegen and round-trip the expected structured lines. This
// is the codegen counterpart to the interpreter coverage in
// `tests/interpreter.rs` (`test_tracing_stdout_exporter_*`). Before
// the tracing-codegen slice the whole surface was interpreter-only —
// a compiled `LogEvent { ... }.message` read `0`.

#[test]
fn e2e_tracing_stdout_exporter_emits_events() {
    let out = run_program(
        r#"fn main() {
                let tracer = StdoutExporter {};
                tracer.export_event(LogEvent.info("plain"));
                tracer.export_event(
                    LogEvent.info("started")
                        .with_field("user_id", "42")
                        .with_field("ip", "127.0.0.1")
                        .in_span(5));
            }"#,
    );
    // Two sequential exports + an owned-event temporary dropped after
    // each: this is the shape the f-string-in-a-loop double-free
    // (`test_e2e_fstring_in_loop_no_double_free`) used to crash on. The
    // exporter bodies now use the natural multi-interpolation f-string
    // form again (the underlying codegen bug is fixed), so a green run
    // here also guards against that regression returning.
    assert_eq!(
        out.as_deref(),
        Some("[info] plain\n[info] started user_id=42 ip=127.0.0.1 span_id=5\n"),
    );
}

#[test]
fn e2e_tracing_stdout_exporter_emits_spans() {
    let out = run_program(
        r#"fn main() {
                let tracer = StdoutExporter {};
                tracer.export_span(Span.root("request", 7));
                tracer.export_span(
                    Span.root("outer", 1)
                        .child("inner", 2)
                        .with_field("route", "/health"));
            }"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("[span] request span_id=7\n[span] inner span_id=2 parent_id=1 route=/health\n"),
    );
}

#[test]
fn e2e_tracing_log_ambient_emission() {
    // `Log.<level>(...)` — the ambient emission namespace — compiles
    // and runs in a real binary. `Log.info` etc. are assoc-fn bodies
    // over `StdoutExporter`/`LogEvent`, so they lower through the same
    // tracing-codegen pass; this pins that `karac build` (not just
    // `karac run`) honors the convenience layer.
    let out = run_program(
        r#"fn main() {
                Log.info("server started");
                Log.warn("disk 85%");
                Log.error("connection refused");
            }"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("[info] server started\n[warn] disk 85%\n[error] connection refused\n"),
    );
}

#[test]
fn e2e_tracing_with_span_stamps_active_span() {
    // phase-8 line 153: `with_span(s, ||body)` installs `s` as the
    // ambient active span in a compiled binary — a `Log.*` inside the
    // body is auto-stamped with the span id (via the per-thread
    // `karac_tracing_*` register), and the active span is restored on
    // exit. Proves the codegen `with_span` lowering + the
    // `tracing_active_span()`-fed auto-stamp round-trip through a real
    // executable, and that a nested span restores the outer one.
    let out = run_program(
        r#"fn main() {
                let outer = Span.root("o", 1);
                let inner = Span.root("i", 2);
                with_span(outer, || {
                    Log.info("a");
                    with_span(inner, || { Log.info("b") });
                    Log.info("c");
                });
                Log.info("d");
            }"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("[info] a span_id=1\n[info] b span_id=2\n[info] c span_id=1\n[info] d\n"),
    );
}

#[test]
fn e2e_tracing_noop_exporter_is_silent() {
    // The default exporter compiles + runs and emits nothing — the
    // `NoOpExporter` impl bodies lower through the same path.
    let out = run_program(
        r#"fn main() {
                let drop_all = NoOpExporter {};
                drop_all.export_event(LogEvent.warn("ignored"));
                drop_all.export_span(Span.root("ignored", 9));
                println("done");
            }"#,
    );
    assert_eq!(out.as_deref(), Some("done\n"));
}

// ── std.tracing — configurable ambient exporter (line 156) E2E ───
//
// The codegen half of phase-8 line 156: a *compiled* `Log.*` honors
// `Log.set_min_level` / `set_exporter` / `reset` via the process-global
// runtime config (`runtime/src/tracing.rs`) + the `tracing_*` builtin
// lowerings. These mirror the interpreter coverage in
// `tests/interpreter.rs` (`test_tracing_log_{min_level_filters,
// set_exporter_noop_silences,reset_restores_default,
// custom_exporter_receives_events}`), pinning identical behavior under
// `karac build` as under `karac run`.

#[test]
fn e2e_tracing_log_min_level_filters_below_threshold() {
    // `Log.set_min_level("warn")` drops trace/debug/info; warn + error
    // emit. Proves the compiled level gate consults the runtime global.
    let out = run_program(
        r#"fn main() {
                Log.set_min_level("warn");
                Log.trace("t");
                Log.debug("d");
                Log.info("i");
                Log.warn("w");
                Log.error("e");
            }"#,
    );
    assert_eq!(out.as_deref(), Some("[warn] w\n[error] e\n"));
}

#[test]
fn e2e_tracing_log_custom_exporter_receives_events() {
    // A user `Exporter` registered as the ambient sink receives
    // compiled `Log.*` events via the indirect-dispatch path, rendering
    // its own format. Also exercises the min-level filter applying
    // before the custom sink (the dropped `debug` never reaches it) and
    // `Log.set_exporter`'s call-site type inference for a user struct.
    let out = run_program(
        r#"struct Tagging { }
            impl Exporter for Tagging {
                fn export_span(ref self, span: Span) { }
                fn export_event(ref self, event: LogEvent) {
                    println(f"CUSTOM<{event.level}>: {event.message}");
                }
            }
            fn main() {
                Log.set_exporter(Tagging {});
                Log.set_min_level("info");
                Log.debug("dropped");
                Log.info("hi");
                Log.error("bye");
            }"#,
    );
    assert_eq!(
        out.as_deref(),
        Some("CUSTOM<info>: hi\nCUSTOM<error>: bye\n"),
    );
}

#[test]
fn e2e_tracing_unused_in_program_emits_no_tracing_bodies() {
    // The usage gate: a program that never touches std.tracing must
    // not carry any tracing impl-method body. Pins that the unused-
    // declaration deletion in `compile_tracing_stdlib_method_bodies`
    // keeps tracing-free binaries lean (and is what lets the IR-shape
    // tests above stay valid).
    let ir = ir_for(
        r#"
fn main() {
    let buf: Array[i64, 8] = [42; 8];
    let _ = buf[0];
}
"#,
    );
    assert!(
        !ir.contains("@LogEvent.")
            && !ir.contains("@Span.")
            && !ir.contains("@StdoutExporter.")
            && !ir.contains("@NoOpExporter."),
        "a tracing-free program must emit no tracing impl methods; got IR:\n{}",
        ir
    );
}

/// The effect gate catches what `karac check` catches — B-2026-08-19-5.
///
/// This is the row's own repro, and it is a test OF THE HARNESS rather than
/// of the compiler: before the gate existed, this exact program ran here and
/// the test passed, while `karac check` and `karac build` both exited 1 on
/// the identical source with "public function 'push' performs
/// reads(RequestCh) but does not declare it". A public function's effects
/// are VERIFIED rather than inferred, so the wrong verb is a hard error.
///
/// Asserting the PANIC is the point: it pins that the suite which exercises
/// the most real binaries can no longer pin behaviour for a program no user
/// can compile. If the gate is ever removed or stops running, this test goes
/// green-by-not-panicking and fails.
#[test]
#[should_panic(expected = "[effect-gate]")]
fn effect_gate_rejects_public_fn_whose_declared_verb_is_wrong() {
    let src = r#"
trait Channel {
    fn send(ref self, v: i64) -> i64;
}

effect resource RequestCh: Channel;

pub fn push(v: i64) -> i64 with sends(RequestCh) {
    RequestCh.send(v)
}

fn main() {
    let _ = 0;
}
"#;
    let _ = run_program(src);
}

/// The gate is no STRICTER than production either, which is the other half
/// of "mirror `karac build`" and the easier half to get wrong.
///
/// `TargetGateViolation` (E0411) is an effect-checker finding that `karac
/// build` deliberately does NOT treat as fatal — it is a target-AVAILABILITY
/// result, and the cross-target dev workflow `karac run` supports depends on
/// a native build not rejecting it. A gate written as "any effect error"
/// would fail this program, deleting real coverage. Measured: this exact
/// source reaches codegen and runs, while `karac check` reports E0411.
#[test]
fn effect_gate_admits_the_advisory_target_gate_violation() {
    let src = r#"
effect resource Console;

unsafe extern "C" {
    fn puts(s: *const u8) -> i32 with writes(Console);
}

pub fn main() with writes(Console) blocks {
    puts(c"advisory".as_ptr());
}
"#;
    if let Some(out) = run_program(src) {
        assert_eq!(out, "advisory\n");
    }
}
