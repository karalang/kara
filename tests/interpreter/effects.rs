//! effects, contracts, ambient state, panics, defer/errdefer -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter effects::
//!
//! New fixtures about effects, contracts, ambient state, panics, defer/errdefer belong in this file.

use super::*;

#[test]
fn test_no_cascading_diagnostic_after_fault_in_compound_expr() {
    // B-2026-07-15-7: a runtime fault in a SUB-expression of a compound
    // operator expression must not cascade into a spurious second diagnostic.
    // `min / -1 + 0` lowers to `i64.add(i64.div(min, -1), 0)`; the div
    // overflows and sets `pending_cf`, but the lowered-operator dispatch used
    // to evaluate the `add`'s rhs anyway (short-circuiting it to `Unit` via
    // check_cf) and then run `eval_binary(Add, Unit, Unit)`, emitting a bogus
    // "operator 'Add' is not defined for operands of type 'Unit' and 'Unit'".
    // The fix short-circuits after the faulted operand, so exactly one error —
    // the real `integer overflow` — is reported.
    let errors = runtime_errors(
        "fn main() {\n\
             let m: i64 = -9223372036854775807i64 - 1i64;\n\
             let x: i64 = m / -1i64 + 0i64;\n\
             println(x);\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "expected exactly one runtime error (no cascade), got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(
        errors[0].message.contains("integer overflow"),
        "expected the sole error to be integer overflow, got: {:?}",
        errors[0].message
    );
    assert!(
        !errors[0].message.contains("is not defined for operands"),
        "must not surface the spurious operator-on-Unit cascade, got: {:?}",
        errors[0].message
    );

    // Same guarantee for a div-by-zero fault nested in a compound expression
    // (the other common sub-expression fault), and inside an f-string
    // interpolation (the original repro shape).
    let errors = runtime_errors(
        "fn main() {\n\
             let a: i64 = 10i64; let b: i64 = 0i64;\n\
             print(f\"{a / b + 5i64}\");\n\
         }",
    );
    assert_eq!(
        errors.len(),
        1,
        "div-by-zero in an f-string compound must not cascade, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    assert!(errors[0].message.contains("division by zero"));
}

#[test]
fn test_panic_records_runtime_error_verbatim() {
    // B-2026-07-09-9: `panic("msg")` is a diverging prelude primitive
    // (mirrors todo/unreachable). Unlike those, its user message is surfaced
    // VERBATIM — no "not yet implemented"/"entered unreachable code" prefix —
    // since `panic` is the explicit user-facing form.
    let errors = runtime_errors(r#"fn main() { panic("boom"); }"#);
    assert!(
        errors.iter().any(|e| e.message.contains("boom")
            && !e.message.contains("not yet implemented")
            && !e.message.contains("entered unreachable code")),
        "expected panic() to surface its message verbatim, got {:?}",
        errors
    );
}

#[test]
fn test_panic_no_arg_uses_default_message() {
    // Bare `panic()` falls back to the "explicit panic" default.
    let errors = runtime_errors(r#"fn main() { panic(); }"#);
    assert!(
        errors.iter().any(|e| e.message.contains("explicit panic")),
        "expected bare panic() to surface the default message, got {:?}",
        errors
    );
}

// ── defer/errdefer ─────────────────────────────────────────────

#[test]
fn test_defer_runs_on_scope_exit() {
    assert_eq!(
        run("fn main() {\n\
                 print(1);\n\
                 defer { print(3); }\n\
                 print(2);\n\
             }"),
        "123"
    );
}

#[test]
fn test_defer_lifo_order() {
    assert_eq!(
        run("fn main() {\n\
                 defer { print(3); }\n\
                 defer { print(2); }\n\
                 defer { print(1); }\n\
             }"),
        "123"
    );
}

// Unified drop+defer cleanup stack — design.md § Drop ordering within a branch.

#[test]
fn test_defer_registers_when_reached_not_at_block_start() {
    // The `defer` after the early `return` is never registered, so
    // its body must not fire. Pre-walk collection (the old bug)
    // would have run "late" anyway because the defer was hoisted to
    // block start.
    assert_eq!(
        run("fn early(go: bool) {\n\
                 print(\"a\");\n\
                 if go { return; }\n\
                 defer { print(\"late\"); }\n\
                 print(\"b\");\n\
             }\n\
             fn main() {\n\
                 early(true);\n\
                 print(\"|\");\n\
                 early(false);\n\
             }"),
        "a|ablate"
    );
}

#[test]
fn test_defer_runs_on_early_return() {
    // A defer registered before the early return must fire — the
    // pre-walk impl had a bug where the `?` in eval_stmt_cf
    // short-circuited past the cleanup drain.
    assert_eq!(
        run("fn body() {\n\
                 defer { print(\"d\"); }\n\
                 print(\"a\");\n\
                 return;\n\
                 print(\"unreached\");\n\
             }\n\
             fn main() { body(); }"),
        "ad"
    );
}

#[test]
fn test_errdefer_fires_on_err_return() {
    // Param-less errdefer fires on Err return; defer always fires.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 return Err(\"boom\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "ed"
    );
}

#[test]
fn test_errdefer_skipped_on_normal_return() {
    // errdefer must NOT fire on Ok; defer always fires.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 return Ok(1);\n\
             }\n\
             fn main() { let _ = body(); }"),
        "d"
    );
}

#[test]
fn test_errdefer_runs_before_defer_on_error_path() {
    // Phase 1 (errdefer) drains LIFO, then phase 2 (drop+defer) drains LIFO.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d1\"); }\n\
                 errdefer { print(\"e1\"); }\n\
                 defer { print(\"d2\"); }\n\
                 errdefer { print(\"e2\"); }\n\
                 return Err(\"x\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "e2e1d2d1"
    );
}

// ── Ambient program-rooted resources (CR-A slice 3) ──────────────

#[test]
fn test_ambient_clock_now_returns_positive_timestamp() {
    // Bare `Clock.now()` outside any `with_provider` uses the ambient
    // default provider installed in the base frame. The system time is
    // well past the Unix epoch for all plausible test environments, so
    // a non-zero result is enough to prove the default fired.
    let output = run("fn main() {\n\
                          let t = Clock.now();\n\
                          println(t > 1_000_000_000);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_clock_with_provider_overrides_default() {
    // `with_provider[Clock]` pushes a fake on top of the ambient default.
    // Inside the scope the fake wins; after exit the ambient default is
    // still present (the frame popped back to the base frame, not past it).
    let output = run("struct FakeClock {}\n\
                      impl FakeClock { fn now(self) -> i64 { 42 } }\n\
                      fn main() {\n\
                          with_provider[Clock](FakeClock {}, || {\n\
                              println(Clock.now());\n\
                          });\n\
                          let t = Clock.now();\n\
                          println(t > 1_000_000_000);\n\
                      }");
    assert_eq!(output, "42\ntrue\n");
}

#[test]
fn test_ambient_clock_not_required_to_declare_effect_resource() {
    // `Clock` is a prelude effect resource — user code doesn't need
    // `effect resource Clock;` to call `Clock.now()` or wrap it in
    // `with_provider[Clock](...)`.
    let output = run("fn main() {\n\
                          let t = Clock.now();\n\
                          if t > 0 { println(\"ok\"); }\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_random_source_with_provider_overrides_default() {
    // `with_provider[RandomSource]` shadows the ambient xorshift. Inside
    // the scope the fake wins (returns `7` twice); after exit the
    // interpreter is back on the ambient default (two fresh non-equal
    // draws).
    let output = run("struct FakeRandom {}\n\
                      impl FakeRandom { fn next_u64(self) -> i64 { 7 } }\n\
                      fn main() {\n\
                          with_provider[RandomSource](FakeRandom {}, || {\n\
                              println(RandomSource.next_u64());\n\
                              println(RandomSource.next_u64());\n\
                          });\n\
                          let a = RandomSource.next_u64();\n\
                          let b = RandomSource.next_u64();\n\
                          println(a != b);\n\
                      }");
    assert_eq!(output, "7\n7\ntrue\n");
}

#[test]
fn test_ambient_random_source_not_required_to_declare_effect_resource() {
    // `RandomSource` is a prelude effect resource — user code doesn't need
    // `effect resource RandomSource;` to call `RandomSource.next_u64()`.
    let output = run("fn main() {\n\
                          let _ = RandomSource.next_u64();\n\
                          println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_env_args_returns_nonempty_array() {
    // `Env.args()` returns process argv as `Vec[String]`. Under `cargo
    // test`, argv[0] is the test binary path, so the array is guaranteed
    // non-empty. We don't assert on exact contents (too environment-
    // dependent) — the length check is a sharp witness the default fired.
    let output = run("fn main() {\n\
                          let a = Env.args();\n\
                          println(a.len() > 0);\n\
                      }");
    assert_eq!(output, "true\n");
}

#[test]
fn test_ambient_env_with_provider_overrides_default() {
    // `with_provider[Env]` shadows the ambient default. The fake's
    // `args()` returns a controlled Vec; after exit the ambient default
    // (real argv) is back.
    let output = run("struct FakeEnv {}\n\
                      impl FakeEnv { fn args(self) -> Vec[String] { [\"a\", \"b\"] } }\n\
                      fn main() {\n\
                          with_provider[Env](FakeEnv {}, || {\n\
                              let a = Env.args();\n\
                              println(a.len());\n\
                              println(a[0]);\n\
                              println(a[1]);\n\
                          });\n\
                          let outer = Env.args();\n\
                          println(outer.len() > 0);\n\
                      }");
    assert_eq!(output, "2\na\nb\ntrue\n");
}

#[test]
fn test_ambient_env_not_required_to_declare_effect_resource() {
    // `Env` is a prelude effect resource — user code doesn't need
    // `effect resource Env;` to call `Env.args()`.
    let output = run("fn main() {\n\
                          let _ = Env.args();\n\
                          println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_env_var_present_returns_ok() {
    // `Env.var(name)` returns `Ok(value)` for a set environment variable.
    // We set the var via `std::env::set_var` from the test harness, then
    // observe it through the interpreter's ambient default provider.
    std::env::set_var("KARAC_ENV_VAR_PRESENT_TEST", "hello");
    let output = run("fn main() {\n\
                          let r = Env.var(\"KARAC_ENV_VAR_PRESENT_TEST\");\n\
                          match r {\n\
                              Ok(v) => println(v),\n\
                              Err(_) => println(\"unset\"),\n\
                          }\n\
                      }");
    std::env::remove_var("KARAC_ENV_VAR_PRESENT_TEST");
    assert_eq!(output, "hello\n");
}

#[test]
fn test_ambient_env_var_missing_returns_err_not_present() {
    // Missing var returns `Err(VarError.NotPresent)`. We don't name the
    // variant explicitly (it's not in `PRELUDE_VARIANTS` per v49 Q2=B);
    // the `Err(_)` arm proves the Err branch fired.
    std::env::remove_var("KARAC_ENV_VAR_MISSING_TEST_XYZ");
    let output = run("fn main() {\n\
                          let r = Env.var(\"KARAC_ENV_VAR_MISSING_TEST_XYZ\");\n\
                          match r {\n\
                              Ok(_) => println(\"set\"),\n\
                              Err(_) => println(\"unset\"),\n\
                          }\n\
                      }");
    assert_eq!(output, "unset\n");
}

#[test]
fn test_ambient_env_var_with_provider_overrides_default() {
    // `with_provider[Env]` shadows the ambient default — the FakeEnv's
    // `var` method wins inside the scope. FakeEnv uses `Result[String,
    // String]` as its return type to avoid pinning the test on `VarError`
    // resolving as a user-visible name (the interpreter's resource-method
    // dispatch is duck-typed at runtime).
    let output = run("struct FakeEnv {}\n\
                      impl FakeEnv {\n\
                          fn var(self, name: ref String) -> Result[String, String] {\n\
                              if name == \"FOO\" { Ok(\"fake-foo\") } else { Err(\"\") }\n\
                          }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Env](FakeEnv {}, || {\n\
                              match Env.var(\"FOO\") {\n\
                                  Ok(v) => println(v),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                              match Env.var(\"BAR\") {\n\
                                  Ok(_) => println(\"ok\"),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                          });\n\
                      }");
    assert_eq!(output, "fake-foo\nerr\n");
}

#[test]
fn test_ambient_stdout_not_required_to_declare_effect_resource() {
    // `Stdout` is a prelude effect resource — user code doesn't need
    // `effect resource Stdout;` to call `Stdout.println(...)`.
    let output = run("fn main() {\n\
                          Stdout.println(\"ok\");\n\
                      }");
    assert_eq!(output, "ok\n");
}

#[test]
fn test_ambient_stdin_with_provider_overrides_default() {
    // Symmetric to the Stdout interception test: a `with_provider[Stdin]`
    // install routes `Stdin.read_line()` through the user's fake instead
    // of pulling from the real stdin. CannedStdin returns a fixed line;
    // the test asserts that line came back through the provider stack.
    // Uses `Result[String, String]` to dodge `IoError` name resolution
    // (the dispatch is duck-typed at runtime — same trick as the Env test).
    let output = run("struct CannedStdin {}\n\
                      impl CannedStdin {\n\
                          fn read_line(self) -> Result[String, String] {\n\
                              Ok(\"piped line\")\n\
                          }\n\
                      }\n\
                      fn main() {\n\
                          with_provider[Stdin](CannedStdin {}, || {\n\
                              match Stdin.read_line() {\n\
                                  Ok(s)  => println(s),\n\
                                  Err(_) => println(\"err\"),\n\
                              }\n\
                          });\n\
                      }");
    assert_eq!(output, "piped line\n");
}

#[test]
fn test_assert_two_arg_passing_does_not_fire() {
    // The 2-arg form must run cleanly when the condition holds.
    let out = run("fn main() { assert(1 == 1, \"unused\"); println(7); }");
    assert_eq!(out, "7\n");
}

#[test]
fn test_assert_eq_failure_integer_values() {
    let errors = runtime_errors("fn main() { assert_eq(1i64, 2i64); }");
    assert!(!errors.is_empty(), "expected a runtime error");
    let e = &errors[0];
    assert_eq!(e.left.as_deref(), Some("1"));
    assert_eq!(e.right.as_deref(), Some("2"));
}

#[test]
fn test_cli_parse_missing_required_arg_errors() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let p = Parser.new("greet")
                     .arg("--name", Arg.string().required());
                 match p.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println("err"),
                 }
             });
         }"#);
    assert_eq!(output, "err\n");
}

#[test]
fn test_cli_subcommand_missing_required_arg_errors() {
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "upper"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .subcommand("upper", Parser.new("upper").arg("--name", Arg.string().required()));
                 match parser.parse() {
                     Ok(_) => println("ok"),
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "missing required subcommand argument\n");
}

// ── std.tracing — structured logging + spans ───────────────────────

#[test]
fn test_tracing_span_root_builder() {
    let output = run(r#"fn main() {
         let s = Span.root("request", 7).with_field("method", "GET");
         println(s.name);
         println(s.span_id);
         println(s.parent_id);
         println(s.fields.len());
     }"#);
    assert_eq!(output, "request\n7\n0\n1\n");
}

#[test]
fn test_tracing_span_child_inherits_parent_id() {
    let output = run(r#"fn main() {
         let parent = Span.root("outer", 1);
         let child = parent.child("inner", 2);
         println(child.name);
         println(child.span_id);
         println(child.parent_id);
     }"#);
    assert_eq!(output, "inner\n2\n1\n");
}

#[test]
fn test_tracing_log_event_levels() {
    let output = run(r#"fn main() {
         println(LogEvent.trace("a").level);
         println(LogEvent.debug("b").level);
         println(LogEvent.info("c").level);
         println(LogEvent.warn("d").level);
         println(LogEvent.error("e").level);
     }"#);
    assert_eq!(output, "trace\ndebug\ninfo\nwarn\nerror\n");
}

#[test]
fn test_tracing_noop_exporter_implements_trait() {
    let output = run(r#"fn main() {
         let e = NoOpExporter {};
         let s = Span.root("request", 1);
         let ev = LogEvent.info("hello");
         e.export_span(s);
         e.export_event(ev);
         println("ok");
     }"#);
    assert_eq!(output, "ok\n");
}

#[test]
fn test_tracing_log_ambient_emission_all_levels() {
    // `Log.<level>("msg")` emits through the built-in StdoutExporter
    // without the caller constructing/threading an exporter value — the
    // ambient convenience layer. Each level renders its own tag.
    let output = run(r#"fn main() {
         Log.trace("t");
         Log.debug("d");
         Log.info("i");
         Log.warn("w");
         Log.error("e");
     }"#);
    assert_eq!(
        output,
        "[trace] t\n[debug] d\n[info] i\n[warn] w\n[error] e\n"
    );
}

#[test]
fn test_tracing_log_min_level_filters_below_threshold() {
    // phase-8 line 156 (interpreter half): `Log.set_min_level("warn")`
    // drops trace/debug/info; only warn + error emit. The dropped calls'
    // message args aren't even evaluated (standard log-filter semantics),
    // though string literals make that unobservable here.
    let output = run(r#"fn main() {
         Log.set_min_level("warn");
         Log.trace("t");
         Log.debug("d");
         Log.info("i");
         Log.warn("w");
         Log.error("e");
     }"#);
    assert_eq!(output, "[warn] w\n[error] e\n");
}

#[test]
fn test_tracing_log_custom_exporter_receives_events() {
    // A user `Exporter` registered as the ambient sink receives `Log.*`
    // events (dynamically dispatched), rendering its own format instead of
    // the StdoutExporter line. Also exercises the min-level filter applying
    // before the custom sink (the dropped `debug` never reaches it).
    let output = run(r#"struct Tagging { }
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
         }"#);
    assert_eq!(output, "CUSTOM<info>: hi\nCUSTOM<error>: bye\n");
}

#[test]
fn test_tracing_stdout_exporter_emits_event_line() {
    // StdoutExporter is the v1 emission surface: it renders a LogEvent
    // as one structured line — `[level] message key=value … span_id=N`,
    // with the span_id suffix only when the event is in a span.
    let output = run(r#"fn main() {
         let tracer = StdoutExporter {};
         tracer.export_event(LogEvent.info("plain"));
         tracer.export_event(
             LogEvent.info("started")
                 .with_field("user_id", "42")
                 .with_field("ip", "127.0.0.1")
                 .in_span(5));
     }"#);
    assert_eq!(
        output,
        "[info] plain\n[info] started user_id=42 ip=127.0.0.1 span_id=5\n"
    );
}

#[test]
fn test_tracing_stdout_exporter_emits_span_line() {
    // Spans render as `[span] name span_id=N parent_id=M key=value …`,
    // with the parent_id suffix suppressed for a root span (parent 0).
    let output = run(r#"fn main() {
         let tracer = StdoutExporter {};
         tracer.export_span(Span.root("request", 7));
         tracer.export_span(
             Span.root("outer", 1)
                 .child("inner", 2)
                 .with_field("route", "/health"));
     }"#);
    assert_eq!(
        output,
        "[span] request span_id=7\n[span] inner span_id=2 parent_id=1 route=/health\n"
    );
}

#[test]
fn test_tracing_with_span_stamps_active_span() {
    // phase-8 line 153: `with_span(s, ||body)` installs `s` as the ambient
    // active span, so a `Log.*` inside the body is auto-stamped with its
    // span id without the caller threading it.
    let output = run(r#"fn main() {
         let s = Span.root("req", 7);
         with_span(s, || { Log.info("inside") });
         Log.info("outside");
     }"#);
    // Inside the span → span_id=7; outside → no active span → no suffix.
    assert_eq!(output, "[info] inside span_id=7\n[info] outside\n");
}

#[test]
fn test_tracing_with_span_nesting_restores_outer() {
    // A nested `with_span` restores the outer active span on exit.
    let output = run(r#"fn main() {
         let outer = Span.root("o", 1);
         let inner = Span.root("i", 2);
         with_span(outer, || {
             Log.info("a");
             with_span(inner, || { Log.info("b") });
             Log.info("c");
         });
         Log.info("d");
     }"#);
    assert_eq!(
        output,
        "[info] a span_id=1\n[info] b span_id=2\n[info] c span_id=1\n[info] d\n"
    );
}

#[test]
fn test_tracing_explicit_in_span_overrides_active() {
    // An explicit `.in_span(id)` always wins over the ambient active span.
    let output = run(r#"fn main() {
         let s = Span.root("s", 7);
         with_span(s, || {
             let tracer = StdoutExporter {};
             tracer.export_event(LogEvent.info("x").in_span(99));
         });
     }"#);
    assert_eq!(output, "[info] x span_id=99\n");
}

#[test]
fn test_tracing_user_can_implement_exporter_trait() {
    // The whole point of the `Exporter` trait shape is that user code
    // can swap in a real implementation against the same surface. A
    // capturing exporter verifies the dispatch reaches the user impl,
    // not just the no-op default.
    let output = run(r#"shared struct CaptureExporter {
             mut span_count: i64,
             mut event_count: i64,
         }
         impl Exporter for CaptureExporter {
             fn export_span(ref self, span: Span) { self.span_count = self.span_count + 1; }
             fn export_event(ref self, event: LogEvent) { self.event_count = self.event_count + 1; }
         }
         fn main() {
             let e = CaptureExporter { span_count: 0, event_count: 0 };
             e.export_span(Span.root("a", 1));
             e.export_span(Span.root("b", 2));
             e.export_event(LogEvent.info("c"));
             println(e.span_count);
             println(e.event_count);
         }"#);
    assert_eq!(output, "2\n1\n");
}

// ── Contracts — requires / ensures runtime enforcement ─────────────
//
// design.md § Contracts: `requires` predicates are checked at function
// entry and `ensures(result) …` at the return point (debug builds); a
// false predicate faults `contract violated`. v1 covers free functions.

#[test]
fn test_contract_requires_holds_runs_body() {
    let output = run_no_errors(
        "fn checked(x: i64) -> i64 requires x > 0 { x * 2 }\n\
         fn main() { println(checked(5)); }",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_contract_requires_violation_faults() {
    let errors = runtime_errors(
        "fn checked(x: i64) -> i64 requires x > 0 { x * 2 }\n\
         fn main() { println(checked(-3)); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for a failed requires, got: {errors:?}"
    );
}

#[test]
fn test_contract_ensures_holds_runs() {
    // Both binding syntaxes are accepted; this uses the design `(result)`.
    let output = run_no_errors(
        "fn double(x: i64) -> i64 ensures(result) result > x { x * 2 }\n\
         fn main() { println(double(5)); }",
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_contract_ensures_violation_faults() {
    let errors = runtime_errors(
        "fn bad(x: i64) -> i64 ensures(result) result > 100 { x }\n\
         fn main() { println(bad(5)); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for a failed ensures, got: {errors:?}"
    );
}

#[test]
fn test_contract_requires_and_ensures_combined() {
    let output = run_no_errors(
        "fn clamp_pos(x: i64) -> i64 requires x > 0 ensures(result) result >= x { x + 1 }\n\
         fn main() { println(clamp_pos(10)); }",
    );
    assert_eq!(output, "11\n");
}

#[test]
fn test_contract_ensures_pipe_syntax_still_works() {
    // The `|result|` closure-style binding remains accepted alongside the
    // `(result)` design form.
    let output = run_no_errors(
        "fn double(x: i64) -> i64 ensures |result| result > x { x * 2 }\n\
         fn main() { println(double(5)); }",
    );
    assert_eq!(output, "10\n");
}

// ── Contracts — old(expr) pre-state + method contracts ─────────────
//
// design.md § Contracts rule 4: `old(expr)` in an `ensures` clause reads
// the value captured at function entry. Method `requires`/`ensures` are
// enforced on the method-dispatch path (same as free functions).

#[test]
fn test_contract_old_method_holds() {
    // `withdraw` reduces the balance by `amount`; the postcondition
    // `self.balance == old(self.balance) - amount` holds.
    let errors = runtime_errors(
        "struct Account { balance: i64 }\n\
         impl Account {\n\
             pub fn withdraw(mut ref self, amount: i64) -> i64\n\
                 ensures(result) self.balance == old(self.balance) - amount\n\
             { self.balance = self.balance - amount; amount }\n\
         }\n\
         fn main() { let mut a = Account { balance: 100 }; let _ = a.withdraw(30); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied old() postcondition must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_old_method_violation_faults() {
    // The body mutates the balance wrongly, so the `old()` postcondition fails.
    let errors = runtime_errors(
        "struct Account { balance: i64 }\n\
         impl Account {\n\
             pub fn withdraw(mut ref self, amount: i64) -> i64\n\
                 ensures(result) self.balance == old(self.balance) - amount\n\
             { self.balance = self.balance - 999; amount }\n\
         }\n\
         fn main() { let mut a = Account { balance: 100 }; let _ = a.withdraw(30); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` fault for the old() postcondition, got: {errors:?}"
    );
}

#[test]
fn test_contract_method_requires_violation_faults() {
    // Method `requires` is enforced on the dispatch path.
    let errors = runtime_errors(
        "struct Counter { n: i64 }\n\
         impl Counter { pub fn step(self, by: i64) -> i64 requires by > 0 { self.n + by } }\n\
         fn main() { let c = Counter { n: 5 }; let _ = c.step(-1); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a method-requires fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_old_free_function() {
    // `old(x)` in a free-function ensures evaluates to the entry value.
    let output = run_no_errors(
        "fn bump(x: i64) -> i64 ensures(result) result > old(x) { x + 1 }\n\
         fn main() { println(bump(5)); }",
    );
    assert_eq!(output, "6\n");
}

// ── Contracts — distinct "predicate panicked" fault category (step 6) ──
//
// design.md § Contracts rule 2: a predicate that *returns false* is
// `contract violated`; a predicate whose *evaluation faults* (index OOB,
// div-by-zero, unwrap) is the distinct `contract predicate panicked`.

#[test]
fn test_contract_predicate_panicked_is_distinct() {
    let errors = runtime_errors(
        "fn at(v: Vec[i64], i: i64) -> i64 requires v[i] >= 0 { 0 }\n\
         fn main() { let v: Vec[i64] = Vec[1, 2, 3]; let _ = at(v, 99); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract predicate panicked")),
        "expected a `contract predicate panicked` fault, got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "a panicking predicate must NOT be reported as `contract violated`, got: {errors:?}"
    );
}

#[test]
fn test_contract_violated_distinct_from_panicked() {
    let errors = runtime_errors(
        "fn pos(x: i64) -> i64 requires x > 0 { x }\n\
         fn main() { let _ = pos(-5); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected `contract violated` for a false predicate, got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e.message.contains("predicate panicked")),
        "a false predicate must NOT be reported as panicked, got: {errors:?}"
    );
}

#[test]
fn test_contract_predicate_panicked_in_ensures() {
    let errors = runtime_errors(
        "fn f(v: Vec[i64]) -> i64 ensures(result) v[result] >= 0 { 99 }\n\
         fn main() { let v: Vec[i64] = Vec[1, 2, 3]; let _ = f(v); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract predicate panicked")),
        "expected `contract predicate panicked` in an ensures, got: {errors:?}"
    );
}

#[test]
fn gpu_upload_download_get_clean_compiled_only_diagnostic() {
    // GPU-SLIP-4h sibling: `gpu.upload` / `gpu.download` are compiled-only
    // (the tree-walk interpreter has no device-buffer model). They must
    // produce a clean runtime diagnostic — the previous fall-through hit the
    // `unreachable!("variable 'gpu' not found")` ICE.
    let errors = runtime_errors(
        "struct P { a: f32 }\n\
         #[gpu]\n\
         fn step(p: P) -> P { p }\n\
         fn main() {\n\
             let mut v: Vec[P] = Vec.new();\n\
             v.push(P { a: 1.0 });\n\
             let buf = gpu.upload(v);\n\
             println(1);\n\
         }",
    );
    assert!(
        !errors.is_empty(),
        "gpu.upload under the interpreter must produce a runtime error"
    );
    assert!(
        errors[0].message.contains("compiled path"),
        "expected the compiled-only diagnostic, got: {:?}",
        errors[0].message
    );
}

// ── errdefer on a function-TAIL error exit (B-2026-08-23-9) ────────────────

/// The interpreter classified every tail expression as a normal exit, so a
/// function that fails by returning `Err(...)` as its LAST EXPRESSION — no
/// `return`, no `?` — silently skipped its `errdefer`. Both compiled backends
/// ran it (codegen's tail emitter routes a syntactic `Err(...)` through
/// `emit_scope_cleanup_for_error_path`, pinned since Phase 7), so the cleanup
/// a shipped binary performed was absent under `karac run --interp` — the
/// backend people debug in. `karac check` was clean.
///
/// `defer` was never affected: it fires on every exit, so the surrounding
/// output looked right and only the rollback went missing.
#[test]
fn test_errdefer_fires_on_tail_err_expression() {
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 Err(\"boom\")\n\
             }\n\
             fn main() { let _ = body(); }"),
        "ed",
        "a tail `Err(...)` is a failure exit and must fire errdefer, exactly as \
         the `return Err(...)` form above does"
    );
}

/// The `Option` twin. `None` as a tail expression is the same failure shape,
/// and the shared predicate matches it for the same reason — so it must fire
/// the param-less errdefer too.
#[test]
fn test_errdefer_fires_on_tail_none_expression() {
    assert_eq!(
        run("fn body() -> Option[i64] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 None\n\
             }\n\
             fn main() { let _ = body(); }"),
        "ed"
    );
}

/// Ordering on the tail path must match the `return` path: errdefer group
/// first, LIFO, then the drop+defer drain, LIFO (design.md § `defer` and
/// `errdefer` rules). A fix that fired the errdefers but in declaration order,
/// or after the defers, would pass a single-errdefer test.
#[test]
fn test_tail_err_runs_errdefers_lifo_then_defers() {
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d1\"); }\n\
                 errdefer { print(\"e1\"); }\n\
                 defer { print(\"d2\"); }\n\
                 errdefer { print(\"e2\"); }\n\
                 Err(\"boom\")\n\
             }\n\
             fn main() { let _ = body(); }"),
        "e2e1d2d1"
    );
}

/// The negative that keeps the rule honest: a tail `Ok(...)` is a SUCCESS
/// exit and must still skip errdefer. Without this, "classify every tail as an
/// error" would pass all four tests above.
#[test]
fn test_errdefer_still_skipped_on_tail_ok_expression() {
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 defer { print(\"d\"); }\n\
                 errdefer { print(\"e\"); }\n\
                 Ok(1)\n\
             }\n\
             fn main() { let _ = body(); }"),
        "d"
    );
}
