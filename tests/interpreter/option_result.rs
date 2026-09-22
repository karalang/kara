//! Option, Result, `?`, try -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter option_result::
//!
//! New fixtures about Option, Result, `?`, try belong in this file.

use super::*;

#[test]
fn eq_on_a_shared_struct_reads_a_niche_option_field_as_a_niche() {
    // B-2026-08-27-5. `==` on a shared struct with a niche-encoded
    // `Option[shared T]` field answered `false` for structurally equal values
    // on the compiled backends. The slot is ONE pointer (null = None); the
    // comparator `emit_eq_fn_for_type_expr` builds for `Option[T]` is for the
    // conventional `{tag, w0, w1, w2}`, FOUR words — so its byte loop ran off
    // the end of the field.
    //
    // The `padded` case is the one that shows the mechanism rather than just
    // the symptom: with three trailing `i64` fields the surplus 24 bytes land
    // on p1/p2/p3, and the compiled answer came out RIGHT purely because those
    // were equal. Same defect, opposite symptom — which is why a test that
    // only checked the last-field shape could be satisfied by a fix that
    // merely shifted the over-read.
    // Codegen twin: `test_e2e_eq_on_a_shared_struct_niche_option_field`.
    let src = "#[derive(Hash, Eq, PartialEq)]
        shared struct Node { v: i64, next: Option[Node] }
        #[derive(Hash, Eq, PartialEq)]
        shared struct Padded { v: i64, next: Option[Padded], p1: i64, p2: i64 }
        fn chain(n: i64) -> Node {
            if n == 0 { return Node { v: 0, next: None }; }
            return Node { v: n, next: Some(chain(n - 1)) };
        }
        fn main() {
            let a = Node { v: 1, next: None };
            let b = Node { v: 1, next: None };
            println(f\"none-none={a == b}\");

            let leaf = Node { v: 2, next: None };
            let c = Node { v: 1, next: Some(leaf) };
            let leaf2 = Node { v: 2, next: None };
            let d = Node { v: 1, next: Some(leaf2) };
            println(f\"some-some={c == d}\");
            println(f\"some-none={c == a}\");

            let x = chain(4);
            let y = chain(4);
            let z = chain(3);
            println(f\"deep={x == y}\");
            println(f\"deep-ne={x == z}\");

            let p = Padded { v: 1, next: None, p1: 7, p2: 8 };
            let q = Padded { v: 1, next: None, p1: 7, p2: 8 };
            let r = Padded { v: 1, next: None, p1: 7, p2: 9 };
            println(f\"padded={p == q}\");
            println(f\"padded-ne={p == r}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "none-none=true\nsome-some=true\nsome-none=false\n\
         deep=true\ndeep-ne=false\n\
         padded=true\npadded-ne=false\n"
    );
}

// ── `?.` (optional chaining) ───────────────────────────────────

/// B-2026-08-17-28 — design.md line 782: "`user.address?.city?.name`
/// short-circuits to `None` if any level is absent." Three separate defects
/// stood between that sentence and the interpreter, all rooted in `?.` never
/// FLATTENING its projection.
///
/// The chain is the test, because the chain is what flattening is for: a
/// second `?.` can only project from a payload, so if the first produced
/// `Option[Option[City]]` the rest of the spec's example cannot work.
#[test]
fn test_optional_chain_flattens_and_short_circuits() {
    let decls = "struct City { name: String, zip: i64 }\n\
                 struct Address { city: Option[City], zip: i64 }\n\
                 struct User { address: Option[Address] }\n";
    let build = |addr: &str| {
        format!(
            "{decls}fn main() {{ let u = User {{ address: {addr} }};\n\
             match u.address?.city?.name {{ Some(n) => {{ println(n); }} None => {{ println(\"none\"); }} }} }}"
        )
    };
    // Every level present: the spec's own example. Used to print "none".
    assert_eq!(
        run(&build(
            "Some(Address { city: Some(City { name: \"Paris\", zip: 7 }), zip: 3 })"
        )),
        "Paris\n"
    );
    // Absent at the INNER level, and at the OUTER level: both short-circuit.
    assert_eq!(
        run(&build("Some(Address { city: None, zip: 3 })")),
        "none\n"
    );
    assert_eq!(run(&build("None")), "none\n");
}

/// The single-level chain in both member shapes. A struct-typed member is the
/// one that ICE'd: the Some-arm binding held `Some(City)` rather than `City`,
/// so reading a field off it tripped `eval_field_access`'s `unreachable!`.
#[test]
fn test_optional_chain_single_level_binds_the_payload_not_a_wrapper() {
    let decls = "struct City { name: String, zip: i64 }\n\
                 struct Address { city: Option[City], zip: i64 }\n\
                 fn mk() -> Option[Address] { return Some(Address { city: Some(City { name: \"Paris\", zip: 7 }), zip: 3 }); }\n";
    // An `Option`-typed member FLATTENS — `c` is a `City`, so `c.name` reads.
    assert_eq!(
        run(&format!(
            "{decls}fn main() {{ match mk()?.city {{ Some(c) => {{ println(c.name); }} None => {{ println(\"none\"); }} }} }}"
        )),
        "Paris\n"
    );
    // A NON-`Option` member wraps, which is the case that always worked and
    // must keep working — the flattening rule has to leave it alone.
    assert_eq!(
        run(&format!(
            "{decls}fn main() {{ match mk()?.zip {{ Some(z) => {{ println(z); }} None => {{ println(\"none\"); }} }} }}"
        )),
        "3\n"
    );
}

/// The method form, which the row did not name: `args` was discarded and the
/// member looked up as a FIELD, so `c?.label()` found nothing and produced
/// `Unit` — surfacing later as a bogus "operator 'Add' is not defined for
/// 'String' and 'Unit'" on a program `karac check` had passed.
#[test]
fn test_optional_chain_method_form_calls_the_method() {
    let src = "struct City { name: String, zip: i64 }\n\
               impl City { fn label(ref self) -> String { return self.name; } }\n\
               fn mk(present: bool) -> Option[City] {\n\
                   if present { return Some(City { name: \"Paris\", zip: 7 }); }\n\
                   return None;\n\
               }\n";
    assert_eq!(
        run(&format!(
            "{src}fn main() {{ match mk(true)?.label() {{ Some(s) => {{ println(s); }} None => {{ println(\"none\"); }} }} }}"
        )),
        "Paris\n"
    );
    // and it still short-circuits rather than calling the method on nothing
    assert_eq!(
        run(&format!(
            "{src}fn main() {{ match mk(false)?.label() {{ Some(s) => {{ println(s); }} None => {{ println(\"none\"); }} }} }}"
        )),
        "none\n"
    );
}

/// The stripped payload must be a usable `T`, not merely one that prints
/// right. This is the shape the row reported dying on: with the wrapper left
/// in place, `+ 1` hit "operator 'Add' is not defined for operands of type
/// 'EnumVariant' and 'Int'" at runtime, on a program `karac check` had passed.
#[test]
fn test_nil_coalesce_result_is_the_bare_payload() {
    assert_eq!(
        run(
            "fn find(k: i64) -> Option[i64] { if k == 7 { return Some(7); } return None; }\n\
             fn main() { println((find(7) ?? -1) + 1); println((find(3) ?? -1) + 1); }"
        ),
        "8\n0\n"
    );
}

// ── Result/Option & ? operator ──────────────────────────────────

#[test]
fn test_option_some_unwrap() {
    assert_eq!(
        run("fn main() { let x = Some(42); println(x.unwrap()); }"),
        "42\n"
    );
}

#[test]
fn test_option_result_unwrap_or() {
    // B-2026-06-11-10: `unwrap_or(default)` — eager fallback. Present
    // (Some/Ok) yields the payload, absent (None/Err) the default. Interp
    // oracle for the codegen E2E `test_e2e_option_result_unwrap_or`.
    assert_eq!(
        run("fn main() {\n\
                 let a: Option[i64] = Some(5);\n\
                 let b: Option[i64] = None;\n\
                 println(a.unwrap_or(0));\n\
                 println(b.unwrap_or(7));\n\
                 let r: Result[String, i64] = Ok(\"got\");\n\
                 let e: Result[String, i64] = Err(404);\n\
                 println(r.unwrap_or(\"fb\"));\n\
                 println(e.unwrap_or(\"fb\"));\n\
             }"),
        "5\n7\ngot\nfb\n"
    );
}

#[test]
fn test_option_none_nil_coalesce() {
    assert_eq!(run("fn main() { let x = None; println(x ?? 99); }"), "99\n");
}

#[test]
fn test_result_ok_question_mark() {
    assert_eq!(
        run("fn get_value() -> i64 {\n\
                 let r = Ok(42);\n\
                 let v = r?;\n\
                 v\n\
             }\n\
             fn main() { println(get_value()); }"),
        "42\n"
    );
}

#[test]
fn test_result_err_question_mark_propagates() {
    assert_eq!(
        run("fn might_fail() -> i64 {\n\
                 let r = Err(0);\n\
                 let v = r?;\n\
                 v\n\
             }\n\
             fn check() -> i64 {\n\
                 let result = might_fail();\n\
                 match result {\n\
                     Ok(v) => v,\n\
                     Err(e) => -1,\n\
                 }\n\
             }\n\
             fn main() { println(check()); }"),
        "-1\n"
    );
}

#[test]
fn test_option_is_some_is_none() {
    assert_eq!(
        run("fn main() {\n\
                 let a = Some(1);\n\
                 let b = None;\n\
                 println(a.is_some());\n\
                 println(b.is_none());\n\
             }"),
        "true\ntrue\n"
    );
}

// ── Error Return Trace Tests ──────────────────────────────────

#[test]
fn test_error_trace_single_question_mark() {
    let (_output, trace, truncated) = run_program_with_trace(
        "fn might_fail() {\n\
             let r = Err(42);\n\
             r?\n\
         }\n\
         fn main() {\n\
             let result = might_fail();\n\
             match result {\n\
                 Ok(v) => println(v),\n\
                 Err(e) => println(e),\n\
             }\n\
         }",
    );
    assert!(
        !trace.is_empty(),
        "Error trace should have at least one frame"
    );
    assert!(!truncated);
    assert_eq!(trace.len(), 1);
    // The ? is on line 3
    assert_eq!(trace[0].line, 3);
}

// ── ? cross-error From propagation (Step 5) ────────────────────

#[test]
fn test_question_cross_error_calls_from_impl() {
    let output = run("struct ParseError { msg: String }\n\
         struct AppError { msg: String }\n\
         impl From for AppError {\n\
             fn from(e: ParseError) -> AppError {\n\
                 AppError { msg: e.msg }\n\
             }\n\
         }\n\
         fn produce() -> Result[i64, ParseError] { Err(ParseError { msg: \"bad\" }) }\n\
         fn run_it() -> Result[i64, AppError] {\n\
             let x: i64 = produce()?;\n\
             Ok(x)\n\
         }\n\
         fn main() {\n\
             match run_it() {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(e.msg),\n\
             }\n\
         }");
    assert_eq!(output, "bad\n");
}

#[test]
fn test_option_ordering_helper_methods() {
    // `impl Option[Ordering] { fn is_lt … }` per design.md § Comparison
    // Traits (lines 5268-5277). Lives in baked source
    // `runtime/stdlib/option.kara`. None yields `false` for every
    // predicate (IEEE-754 NaN semantics). Exercises 4 inputs × 5
    // helpers = 20 round-trips through the args-aware impl-table lookup
    // (`Option[Ordering]` impl wins over the absence on generic
    // `Option[T]`).
    let output = run("fn main() {\n\
             let lt: Option[Ordering] = Some(Ordering.Less);\n\
             let eq: Option[Ordering] = Some(Ordering.Equal);\n\
             let gt: Option[Ordering] = Some(Ordering.Greater);\n\
             let none: Option[Ordering] = None;\n\
             println(lt.is_lt());\n\
             println(lt.is_le());\n\
             println(lt.is_gt());\n\
             println(lt.is_ge());\n\
             println(lt.is_eq());\n\
             println(eq.is_lt());\n\
             println(eq.is_le());\n\
             println(eq.is_gt());\n\
             println(eq.is_ge());\n\
             println(eq.is_eq());\n\
             println(gt.is_lt());\n\
             println(gt.is_le());\n\
             println(gt.is_gt());\n\
             println(gt.is_ge());\n\
             println(gt.is_eq());\n\
             println(none.is_lt());\n\
             println(none.is_le());\n\
             println(none.is_gt());\n\
             println(none.is_ge());\n\
             println(none.is_eq());\n\
         }");
    assert_eq!(
        output,
        "true\ntrue\nfalse\nfalse\nfalse\n\
         false\ntrue\nfalse\ntrue\ntrue\n\
         false\nfalse\ntrue\ntrue\nfalse\n\
         false\nfalse\nfalse\nfalse\nfalse\n",
    );
}

#[test]
fn test_var_error_question_propagation_produces_io_error_not_found() {
    // End-to-end: `env.var(missing)?` in a function returning
    // `Result[String, IoError]` must propagate as `IoError.NotFound`
    // (because `env.var` returns `VarError.NotPresent` for an unset key,
    // and the `?` operator desugars through `IoError.from(...)`).
    std::env::remove_var("__KARAC_FROM_VAR_TO_IO_NO_SUCH_VAR__");
    let output = run("fn read_var() -> Result[String, IoError] with reads(Env) {
             let s: String = env.var(\"__KARAC_FROM_VAR_TO_IO_NO_SUCH_VAR__\")?;
             Ok(s)
         }
         fn main() {
             match read_var() {
                 Ok(v) => println(v),
                 Err(IoError.NotFound) => println(\"not_found\"),
                 Err(IoError.InvalidUtf8) => println(\"invalid_utf8\"),
                 Err(_) => println(\"other_io_err\"),
             }
         }");
    assert_eq!(output, "not_found\n");
}

#[test]
fn test_distinct_where_try_from_ok_and_err() {
    // `Even.try_from` returns `Ok` for an even value and `Err` for an odd.
    let ok = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn main() {\n\
             match Even.try_from(8) { Ok(e) => println(e.raw()), Err(_) => println(-1) }\n\
         }",
    );
    assert_eq!(ok, "8\n");
    let err = run_no_errors(
        "distinct type Even = i64 where self % 2 == 0;\n\
         fn main() {\n\
             match Even.try_from(7) { Ok(e) => println(e.raw()), Err(_) => println(-1) }\n\
         }",
    );
    assert_eq!(err, "-1\n");
}

#[test]
fn test_try_extend_alias_wraps_ok() {
    // `try_extend` is the spelling design.md § Fallible Allocation's table
    // uses (`extend(iter)` / `try_extend(iter)`); it denotes the same
    // operation as `try_extend_from_slice`, exactly as the panicking `extend`
    // aliases `extend_from_slice`. Asserted against the sibling spelling in
    // the SAME program so the two cannot silently drift apart.
    // B-2026-08-25-20.
    let output = run("fn main() {\n\
             let mut a: Vec[i64] = [1_i64];\n\
             let mut b: Vec[i64] = [1_i64];\n\
             let src: Vec[i64] = [2_i64, 3_i64];\n\
             match a.try_extend(src) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             match b.try_extend_from_slice(src) {\n\
                 Ok(_) => println(\"ok\"),\n\
                 Err(e) => println(\"err\"),\n\
             }\n\
             println(a.len());\n\
             println(b.len());\n\
             println(a[2]);\n\
             println(b[2]);\n\
         }");
    assert_eq!(output, "ok\nok\n3\n3\n3\n3\n");
}

#[test]
fn test_try_companion_not_shadowing_user_method() {
    // A user type that defines its own `try_push` is dispatched normally —
    // the builtin-collection gate keeps the fallible-alloc interception off
    // non-collection receivers. The user method returns a bare `i64` (not an
    // `Ok`-wrapped value), proving it was not intercepted.
    let output = run("struct Bag { n: i64 }\n\
         impl Bag { fn try_push(ref self, x: i64) -> i64 { x + 100_i64 } }\n\
         fn main() {\n\
             let b = Bag { n: 0 };\n\
             println(b.try_push(3_i64));\n\
             println(b.try_push(4_i64));\n\
         }");
    assert_eq!(output, "103\n104\n");
}

#[cfg(unix)]
#[test]
fn test_process_try_wait_returns_none_for_still_running_child() {
    // Spawn a child that sleeps long enough that try_wait sees it
    // still running. /bin/sleep is POSIX-ubiquitous. 0.5s is short
    // enough to keep the test fast but long enough that try_wait
    // fires before exit (interpreter wall-clock has zero scheduling
    // jitter compared to the OS spawn latency). Gated on `unix`
    // because the hard-coded path doesn't resolve on Windows.
    let output = run(r#"fn main() {
         let cmd = Command.new("/bin/sleep").arg("0.5");
         match cmd.spawn() {
             Ok(child) => {
                 match child.try_wait() {
                     Ok(None) => println("still_running"),
                     Ok(Some(_)) => println("already_exited"),
                     Err(_) => println("err"),
                 }
                 match child.wait() {
                     Ok(_) => println("waited"),
                     Err(_) => println("wait_err"),
                 }
             }
             Err(_) => println("spawn_err"),
         }
     }"#);
    assert_eq!(output, "still_running\nwaited\n");
}

#[test]
fn test_a_declined_optres_temporary_runs_its_payload_drop_body() {
    // B-2026-09-02-12 — an `Option`/`Result` TEMPORARY the pattern declined ran
    // its payload's `Drop` body on no surface, through all three `let`-family
    // spellings. `if let Ok(w) = mkerr()` builds a `Result[W, W]` holding
    // `Err(W { .. })`, the arm does not take it, and the temporary dies there —
    // so the body is due, exactly as it is for `mkerr();`, the discard spelling
    // of the same value, which has always run it.
    //
    // The miss edges reached a DECLARED-type walker that cannot see through
    // `Ok(T)` / `Err(E)`, whose payloads are the enum's own generic parameters.
    // They now route into the value-driven `Option`/`Result` arm of
    // `run_discarded_value_user_drops` — the arm the discard spelling already
    // used — so the two spellings agree by construction.
    //
    // On the DEFAULT leg because half the fix is the interpreter's, and the
    // cross-backend matrix
    // (`e2e_a_declined_optres_temporary_runs_its_payload_drop_body`) is gated
    // on `--features llvm`.
    const PRELUDE: &str = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
         fn mkr(n: i64) -> Result[W, W] {\n\
         if n < 1 { return Ok(W { id: n }); }\n\
         return Err(W { id: 7 });\n\
         }\n\
         fn mkerr() -> Result[W, W] { return Err(W { id: 7 }); }\n\
         fn mksome() -> Option[W] { return Some(W { id: 3 }); }\n";
    let wrap =
        |body: &str| format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"after\");\n}}\n");

    assert_eq!(
        run(&wrap("if let Ok(w) = mkerr() { println(f\"v{w.id}\"); }")),
        "dW7\nafter\n"
    );

    // TWO `W`s are constructed — the one the last pass bound and the `Err` that
    // ended the loop — so two bodies are due.
    assert_eq!(
        run(&wrap(
            "let mut i: i64 = 0;\n\
             while let Ok(w) = mkr(i) { println(f\"v{w.id}\"); i = i + 1; }"
        )),
        "v0\ndW0\ndW7\nafter\n"
    );

    // The spelling the row left unmeasured. The body lands BEFORE the else
    // block, per design.md § "Scrutinee temporary scope".
    assert_eq!(
        run(&wrap(
            "let Ok(w) = mkerr() else { println(\"miss\"); return };\n\
             println(f\"v{w.id}\");"
        )),
        "dW7\nmiss\n"
    );

    // The `Option` half: a `None` carries nothing, so the payload has to sit in
    // the variant the pattern REJECTS.
    assert_eq!(
        run(&wrap(
            "if let None = mksome() { println(\"none\"); } else { println(\"some\"); }"
        )),
        "dW3\nsome\nafter\n"
    );

    // THE GATE on the hit edge: the arm's binding owns the payload and runs the
    // body itself, so firing on a match would double every `if let`.
    assert_eq!(
        run(&wrap("if let Ok(w) = mkr(0) { println(f\"v{w.id}\"); }")),
        "v0\ndW0\nafter\n"
    );

    // CONTROL: `match` binding both arms was correct throughout.
    assert_eq!(
        run(&wrap(
            "match mkerr() { Ok(w) => println(f\"v{w.id}\"), Err(e) => println(f\"e{e.id}\") }"
        )),
        "e7\ndW7\nafter\n"
    );

    // FRESH TEMPORARIES ONLY. A bound local keeps its own walk, so firing here
    // would double it — see `test_a_bound_optres_local_whose_arm_missed_...`
    // for the local's own half (B-2026-09-02-14).
    assert_eq!(
        run(&wrap("let r: Result[W, W] = mkerr();\n    println(\"x\");")),
        "dW7\nx\nafter\n"
    );
}

#[test]
fn test_a_nested_optres_payload_runs_its_inner_drop_body() {
    // B-2026-09-10-15 (interpreter half) — an `Option`/`Result` payload that
    // is ITSELF an `Option`/`Result`.
    //
    // The row was filed against the COMPILED backends, where a fresh-temp
    // argument printed nothing while `--interp` printed the body: the
    // interpreter reaches that position through its value-driven
    // `run_discarded_value_user_drops`, whose `Option`/`Result` arm recurses
    // structurally and so handles nesting for free. A NAMED LOCAL is a
    // different path — `optres_payload_bodies_tes` — and there the
    // interpreter was silent too, so the two backends agreed and nothing
    // showed. Giving codegen its envelope arm broke that agreement in the
    // named-local, discarded-local and returned-then-bound positions, which
    // is why both halves move in one commit.
    //
    // Two changes here, mirroring the two the tuple payload needed in
    // B-2026-09-09-20: the registration gate now qualifies an envelope
    // payload through a RECURSIVE predicate, and the walk gained an envelope
    // arm that re-enters itself with the declared inner payload type instead
    // of returning outright.
    //
    // The old gate was one level deep in the same way codegen's was:
    // `field_te_runs_user_drop` reads a nested envelope's arg head name and
    // asks `type_name_runs_user_drop("Option")`, which is false. That is why
    // the three-deep cell below is here.
    const PRELUDE: &str = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n";

    // THE NAMED LOCAL, never read, so it dies at its own `let`.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[Option[W]] = Some(Some(W {{ id: 1 }}));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\nx\n"
    );

    // The `Result` twin on the `Ok` side.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let r: Result[Result[W, i64], i64] = Result.Ok(Result.Ok(W {{ id: 1 }}));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\nx\n"
    );

    // THREE deep — fails on any predicate with a one-level horizon.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[Option[Option[W]]] = Some(Some(Some(W {{ id: 1 }})));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\nx\n"
    );

    // CONTROL — the INNER envelope is `None`. The outer arm is taken and the
    // recursion runs, finding nothing; a walk that went a level too far would
    // fault or print here.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[Option[W]] = Some(None);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "x\n"
    );

    // CONTROL — the OUTER envelope is `None`.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[Option[W]] = None;\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "x\n"
    );

    // CONTROL — ONE level. Correct before this commit; here so a recursion
    // that re-enters at the wrong level prints `dW1` twice.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[W] = Some(W {{ id: 1 }});\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\nx\n"
    );

    // CONTROL — the FRESH-TEMP argument the row was filed on. This one always
    // printed under `--interp` (the value-driven discard walk handles nesting
    // structurally), so it pins the position the widened registration must not
    // start double-counting.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn takeW(x: Option[Option[W]]) {{\n\
             \x20   match x {{ Some(t) => {{ println(\"ok\"); }} None => {{ println(\"n\"); }} }}\n\
             }}\n\
             fn main() {{\n\
             \x20   takeW(Some(Some(W {{ id: 1 }})));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "ok\ndW1\nx\n"
    );
}

#[test]
fn test_a_named_local_option_tuple_payload_runs_its_element_bodies() {
    // B-2026-09-09-20 (interpreter half) — a NAMED LOCAL whose `Option`/
    // `Result` payload is a TUPLE ran no element `Drop` body at all.
    //
    // The walk is reached through `optres_payload_bodies_tes`, and that table
    // is populated only when `type_expr_runs_user_drop` says the payload runs
    // one. That predicate answers for a `TypeKind::Path` and returns `false`
    // for everything else, so `Option[(W, W)]` classified drop-free, no record
    // was made, and `run_optres_payload_user_drops` returned on its first line.
    // The same value built as a FRESH TEMP printed both bodies throughout —
    // an argument temp goes down the call path, which never consults the table
    // — which is what made this look like a call-shape question rather than a
    // registration one.
    //
    // Two changes, both needed: the registration gate now qualifies a tuple
    // payload on its ELEMENTS, and the walk itself gained a `Value::Tuple` arm
    // (it bound `Value::Struct` and returned otherwise, exactly as it bound
    // only structs before B-2026-08-28-58 added the user-enum arm).
    //
    // ORDER is element order, matching the compiled `__karac_dropelems_tuple_*`
    // walk, which GEPs each element off the payload base in declaration order.
    //
    // The row's OTHER half — that the compiled backend fires "TOO EARLY",
    // before a following statement — does not survive re-measurement as a
    // defect. `let r = mk(20); println("ok")` over a plain `Drop`-bearing
    // struct prints `dR20` then `ok` on BOTH backends and always has: an
    // unread binding dies at its own `let` under the NLL model this tree
    // implements on purpose (`exec.rs`'s `note_unread`: "Bindings introduced
    // but never read: NLL says they die immediately after the let"). The
    // `Option[(R, i64)]` spelling now does the same thing on both backends,
    // which is agreement with the model rather than a second fix.
    const PRELUDE: &str = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n";

    // THE ROW: a named local, never read, so it dies at its own `let`.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[(W, W)] = Some((W {{ id: 1 }}, W {{ id: 2 }}));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\ndW2\nx\n"
    );

    // A DESTRUCTURING arm over the same local, where the leaves take the
    // elements and each owns its own body. This is the spelling that was
    // already correct, and it is here so the registration widened above cannot
    // start double-counting it.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[(W, W)] = Some((W {{ id: 1 }}, W {{ id: 2 }}));\n\
             \x20   println(\"x\");\n\
             \x20   match o {{ Some((a, b)) => {{ println(f\"t{{a.id}}\"); }} None => {{ println(\"n\"); }} }}\n}}\n"
        )),
        "x\nt1\ndW2\ndW1\n"
    );

    // A RECORDED RESIDUAL, deliberately not asserted: a WHOLE-payload binding
    // over a LOCAL (`match o { Some(t) => { println("hit") } .. }`) still runs
    // no element body, on BOTH backends — the local twin of the arm-level
    // disarm this commit narrowed for a by-value PARAM. It is left alone here
    // because the two backends agree on it, so it breaks no parity rule, and
    // because pinning it would freeze the silence rather than the behaviour.
    // Memory is balanced on it either way (valgrind: 0 errors, nothing lost).

    // The `Result` twin, on the `Ok` side.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let r: Result[(W, W), i64] = Result.Ok((W {{ id: 1 }}, W {{ id: 2 }}));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dW1\ndW2\nx\n"
    );

    // CONTROL — the `Err` side of the same type constructs no `W` at all, so
    // the tag guard has to keep the walk silent rather than reading a payload
    // that is not there.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let r: Result[(W, W), i64] = Result.Err(9);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "x\n"
    );

    // CONTROL — a tuple payload with NO Drop-bearing element must not arm the
    // walk, which is what the per-element `any` in the gate preserves.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[(i64, i64)] = Some((1, 2));\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "x\n"
    );

    // CONTROL — `None`, where there is no payload to walk.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let o: Option[(W, W)] = None;\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "x\n"
    );
}

#[test]
fn test_option_result_map() {
    // B-2026-07-12-11 — `Option[T].map(f)` / `Result[T, E].map(f)` were
    // unimplemented in both runtimes despite typechecking. The interpreter now
    // applies `f` to a present payload (`Some`/`Ok`) and re-wraps, passing an
    // absent receiver (`None`/`Err`) through. Covers a fn-reference and an
    // annotated closure, a type-changing map (i64 -> bool), and a chain.
    let output = run_no_errors(
        r#"
fn dbl(n: i64) -> i64 { n * 2i64 }
fn main() {
    let a: Option[i64] = Some(5i64);
    println(f"{a.map(dbl).unwrap_or(0i64 - 1i64)}");
    let n: Option[i64] = None;
    println(f"{n.map(dbl).unwrap_or(0i64 - 1i64)}");
    let ok: Result[i64, String] = Ok(21i64);
    println(f"{ok.map(dbl).unwrap_or(0i64 - 1i64)}");
    let er: Result[i64, String] = Err(f"boom");
    println(f"{er.map(dbl).unwrap_or(0i64 - 99i64)}");
    println(f"{a.map(dbl).map(|x: i64| x + 1i64).unwrap_or(0i64 - 1i64)}");
    let m = a.map(|x: i64| x > 3i64);
    match m { Some(b) => { println(f"{b}"); } None => { println("none"); } }
}
"#,
    );
    assert_eq!(output, "10\n-1\n42\n-99\n11\ntrue\n");
}

#[test]
fn test_option_result_combinators() {
    // B-2026-07-14-6 — the standard Option/Result combinator family, previously
    // rejected at typecheck with no runtime dispatch. Non-closure batch:
    // `ok`/`err`/`or`/`and`/`ok_or`/`flatten`. Closure batch: `unwrap_or_else`/
    // `map_or`/`map_or_else`/`map_err`/`and_then`/`or_else`/`filter`. The
    // interpreter is the oracle for the parity that codegen is verified against.
    let output = run_no_errors(
        r#"
fn main() {
    let r: Result[i64, i64] = Ok(5i64);
    println(f"{r.ok().unwrap_or(0i64)}");
    let e: Result[i64, i64] = Err(7i64);
    println(f"{e.err().unwrap_or(0i64)}");
    let n: Option[i64] = None;
    println(f"{n.or(Some(9i64)).unwrap_or(0i64)}");
    let s: Option[i64] = Some(9i64);
    println(f"{s.and(Some(3i64)).unwrap_or(0i64)}");
    let s2: Option[i64] = Some(9i64);
    println(f"{s2.ok_or(99i64).unwrap()}");
    let oo: Option[Option[i64]] = Some(Some(42i64));
    println(f"{oo.flatten().unwrap_or(0i64)}");
    let n2: Option[i64] = None;
    println(f"{n2.unwrap_or_else(|| 42i64)}");
    let s3: Option[i64] = Some(5i64);
    println(f"{s3.map_or(0i64, |x: i64| x + 1i64)}");
    let s4: Option[i64] = Some(5i64);
    println(f"{s4.map_or_else(|| 0i64, |x: i64| x * 2i64)}");
    let er: Result[i64, i64] = Err(3i64);
    println(f"{er.map_err(|x: i64| x * 10i64).unwrap_err()}");
    let s5: Option[i64] = Some(5i64);
    println(f"{s5.and_then(|x: i64| Some(x + 1i64)).unwrap_or(0i64)}");
    let n3: Option[i64] = None;
    println(f"{n3.or_else(|| Some(9i64)).unwrap_or(0i64)}");
    let s6: Option[i64] = Some(5i64);
    println(f"{s6.filter(|x: i64| x > 3i64).unwrap_or(0i64)}");
    let s7: Option[i64] = Some(2i64);
    println(f"{s7.filter(|x: i64| x > 3i64).unwrap_or(0i64 - 1i64)}");
    let mut tk: Option[i64] = Some(5i64);
    let t = tk.take();
    println(f"{t.unwrap_or(0i64)}");
    println(f"{tk.unwrap_or(0i64 - 1i64)}");
    let mut gi: Option[i64] = None;
    println(f"{gi.get_or_insert(9i64)}");
    println(f"{gi.unwrap_or(0i64 - 1i64)}");
}
"#,
    );
    assert_eq!(
        output,
        "5\n7\n9\n3\n9\n42\n42\n6\n10\n30\n6\n9\n5\n-1\n5\n-1\n9\n9\n"
    );
}

/// Interpreter oracle for B-2026-08-27-34 — the branch-leaf `Option[shared]`
/// family, each selected value consumed three times (five on leg 11).
///
/// The interpreter was CORRECT throughout this bug: it has no refcount to get
/// wrong, so it is the oracle rather than a second suspect. This test pins
/// that side independently of the codegen twin, which lives in
/// `tests/codegen.rs` behind `#[cfg(feature = "llvm")]` and so does not run
/// under a plain `cargo test` at all — without this, the semantics the fix
/// restores would have no coverage on the default leg.
#[test]
fn option_shared_branch_leaf_survives_repeated_consumption() {
    let src = "shared struct Node { val: i64 }\n\
            fn make(n: i64) -> Option[Node] { return Some(Node { val: n }); }\n\
            fn show(t: Option[Node]) -> i64 {\n\
                match t { None => { return 0; } Some(n) => { return n.val; } }\n\
            }\n\
            fn main() {\n\
                let a1 = make(1); let b1 = make(2);\n\
                let t1 = if true { a1 } else { b1 };\n\
                println(f\"{show(t1)} {show(t1)} {show(t1)}\");\n\
                let a2 = make(3); let b2 = make(4);\n\
                let t2 = if false { a2 } else { b2 };\n\
                println(f\"{show(t2)} {show(t2)} {show(t2)}\");\n\
                let a3 = make(5); let b3 = make(6); let o3 = Some(1);\n\
                let t3 = if let Some(x) = o3 { a3 } else { b3 };\n\
                println(f\"{show(t3)} {show(t3)} {show(t3)}\");\n\
                let a4 = make(7); let b4 = make(8); let o4: Option[i64] = None;\n\
                let t4 = if let Some(y) = o4 { a4 } else { b4 };\n\
                println(f\"{show(t4)} {show(t4)} {show(t4)}\");\n\
                let a5 = make(9); let b5 = make(10); let k5 = 0;\n\
                let t5 = match k5 { 0 => a5, _ => b5 };\n\
                println(f\"{show(t5)} {show(t5)} {show(t5)}\");\n\
                let a6 = make(11);\n\
                let t6 = { a6 };\n\
                println(f\"{show(t6)} {show(t6)} {show(t6)}\");\n\
                let a7 = make(12); let b7 = make(13); let c7 = make(14); let k7 = 1;\n\
                let t7 = if k7 == 0 { a7 } else if k7 == 1 { b7 } else { c7 };\n\
                println(f\"{show(t7)} {show(t7)} {show(t7)}\");\n\
                let a8 = make(15); let b8 = make(16);\n\
                let t8 = if true { if true { a8 } else { b8 } } else { make(99) };\n\
                println(f\"{show(t8)} {show(t8)} {show(t8)}\");\n\
                let a9 = make(17);\n\
                let t9 = if true { a9 } else { make(99) };\n\
                println(f\"{show(t9)} {show(t9)} {show(t9)}\");\n\
                let a10 = make(18);\n\
                let t10 = if false { a10 } else { make(19) };\n\
                println(f\"{show(t10)} {show(t10)} {show(t10)}\");\n\
                let a11 = make(20); let b11 = make(21);\n\
                let t11 = if true { a11 } else { b11 };\n\
                println(f\"{show(t11)} {show(t11)} {show(t11)} {show(t11)} {show(t11)}\");\n\
                let a12 = make(22);\n\
                let t12 = if false { a12 } else { None };\n\
                println(f\"{show(t12)} {show(t12)} {show(t12)}\");\n\
            }";
    assert_eq!(
        run_no_errors(src),
        "1 1 1\n4 4 4\n5 5 5\n8 8 8\n9 9 9\n11 11 11\n13 13 13\n15 15 15\n17 17 17\n19 19 19\n20 20 20 20 20\n0 0 0\n",
        "every read of a branch-leaf `Option[shared]` must see the same value"
    );
}

#[test]
fn test_mut_ref_option_shared_writeback() {
    // B-2026-07-12-3 — reassigning through a `mut ref Option[shared]` param
    // propagates to the caller. The interpreter has always been correct; this
    // is the oracle the codegen fix (writeback through the borrow pointer) is
    // verified against.
    let output = run_no_errors(
        r#"
shared struct Node { mut val: i64, mut next: Option[Node] }
fn setit(prev: mut ref Option[Node], n: Node) { prev = Some(n); }
fn main() {
    let mut cur: Option[Node] = Some(Node { val: 1i64, next: None });
    let a = Node { val: 2i64, next: None };
    setit(mut cur, a);
    let b = Node { val: 3i64, next: None };
    setit(mut cur, b);
    match cur {
        None => { println("none"); }
        Some(p) => { println(f"val: {p.val}"); }
    }
}
"#,
    );
    assert_eq!(output, "val: 3\n");
}

// ── Refinement types: runtime predicate enforcement (phase-9 step 5b) ──

#[test]
fn test_interp_refinement_try_from_ok() {
    // `Even.try_from(4)` evaluates the predicate `self % 2 == 0` against 4,
    // which holds, so the construction returns `Ok(4)`.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(4) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    assert_eq!(output.trim(), "4");
}

#[test]
fn test_interp_refinement_try_from_err() {
    // 3 fails `self % 2 == 0`, so `try_from` returns `Err(<message>)` — the
    // recoverable construction surface. No runtime fault is raised.
    let output = run_no_errors(
        r#"
type Even = i64 where self % 2 == 0;
fn main() {
    match Even.try_from(3) {
        Ok(v) => println(v),
        Err(e) => println(e),
    }
}
"#,
    );
    assert!(
        output.contains("refinement `Even`"),
        "expected an Err message naming the refinement, got: {output:?}"
    );
}

#[test]
fn test_ref_at_binding_over_option_payload() {
    let out = run_no_errors(
        "fn main() {\n\
             let opt = Some(\"hello\");\n\
             match opt {\n\
                 ref x @ Some(y) => { println(y); }\n\
                 None => { println(\"none\"); }\n\
             }\n\
             match opt {\n\
                 Some(z) => { println(z); }\n\
                 None => { }\n\
             }\n\
         }",
    );
    assert_eq!(out, "hello\nhello\n");
}

#[test]
fn builtin_enum_equality_option_result_ordering() {
    let output = run(r#"
fn main() {
    println(f"{Some(1) == Some(1)}");
    println(f"{Some(1) == Some(2)}");
    let a: Result[i64, i64] = Ok(3);
    let b: Result[i64, i64] = Ok(3);
    println(f"{a == b}");
    println(f"{3.cmp(5) == Ordering.Less}");
    println(f"{3.cmp(5) == Ordering.Greater}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\ntrue\nfalse\n");
}

/// B-2026-07-29-27 / B-2026-07-29-31 — the interpreter half of the synthesized
/// `#[derive(Clone)]` method. The worker is `deep_clone_value`, not the derived
/// Rust `Clone`: the latter bumps the `Arc<RwLock<..>>` behind a `Value::Array`
/// FIELD, so the copy would share the source's storage and `b.items.push(..)`
/// would be visible through `a`. Value semantics demand independent storage,
/// and codegen's emitters deep-copy — so a shallow interpreter clone would also
/// be a run-vs-build divergence.
#[test]
fn test_derived_clone_is_deep_on_struct_enum_and_option() {
    assert_eq!(
        run("#[derive(Clone)]\n\
             struct S { n: i64, items: Vec[i64] }\n\
             #[derive(Clone)]\n\
             enum E { A(i64), B(String) }\n\
             fn dup[T: Clone](x: T) -> T { x.clone() }\n\
             let mut a = S { n: 1, items: Vec.new() };\n\
             a.items.push(10);\n\
             let mut b = a.clone();\n\
             b.items.push(20);\n\
             b.n = 99;\n\
             println(f\"{a.n} {a.items.len()} {b.n} {b.items.len()}\");\n\
             let e = E.B(\"payload\");\n\
             match dup(e) { E.A(k) => println(f\"{k}\"), E.B(t) => println(t) }\n\
             let o: Option[String] = Option.Some(\"inner\");\n\
             println(o.clone().unwrap());\n"),
        "1 1 99 2\npayload\ninner\n"
    );
}

/// B-2026-07-30-11 (Option/Result leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_optres_payload_runs_user_drop_bodies`, same
/// source and expected string. The walk keys on the te the Let arm records
/// through codegen's exact resolution chain (`record_optres_payload_te`), so
/// both backends fire for the same set of bindings; the moved-out sets
/// mirror codegen's compile-time retractions at the match/combinator/ctor
/// sites.
/// B-2026-09-09-18 — the interpreter half of the fresh-temp `Option`/`Result`
/// argument's payload `Drop` body, landed in the same commit as the codegen
/// half (`e2e_freshtemp_optres_argument_runs_its_payload_drop_body_once`).
///
/// Both backends had the SAME hole through the same door: the owner of a
/// by-value optres argument's payload bodies is the caller's binding, keyed by
/// NAME here (`optres_payload_bodies_tes`, recorded by the Let arm) and by a
/// let-site registration in codegen. A fresh temp has no binding to key on, so
/// neither backend ran the body — which is why the row recorded "every surface
/// agrees" and no A/B check could see it.
///
/// The two halves must land together: the note on `stmts.rs`'s optres let arm
/// records that fixing only the interpreter once turned an agreed defect into
/// a run-vs-build split and was backed out. Same program as the codegen test
/// on purpose, so a future divergence between the two shows up as one of them
/// failing rather than as a silent drift.
#[test]
fn a_freshtemp_optres_argument_runs_its_payload_drop_body_once() {
    assert_eq!(
        run("struct R2 { s: String, t: String, u: String }\n\
             impl Drop for R2 { fn drop(mut ref self) { println(f\"d:{self.s.len()}\") } }\n\
             fn mkr(i: i64) -> R2 { return R2 { s: f\"ssssssss{i}\", t: f\"tttttttt{i}\", u: f\"uuuuuuuu{i}\" }; }\n\
             fn ignore(x: Option[R2]) { println(\"  ig\"); }\n\
             fn matchit(x: Option[R2]) { match x { Option.Some(r) => { println(f\"  m:{r.s}\"); } Option.None => { println(\"  mn\"); } } }\n\
             fn giveback(x: Option[R2]) -> Option[R2] { println(\"  gb\"); return x; }\n\
             fn resig(x: Result[R2, i64]) { println(\"  rig\"); }\n\
             fn main() {\n\
                 println(\"A named->ignore\");   { let a = Option.Some(mkr(1)); ignore(a); }\n\
                 println(\"B named->matchit\");  { let b = Option.Some(mkr(2)); matchit(b); }\n\
                 println(\"C named->giveback\"); { let c = Option.Some(mkr(3)); let r = giveback(c); }\n\
                 println(\"D temp->ignore\");    ignore(Option.Some(mkr(4)));\n\
                 println(\"E temp->matchit\");   matchit(Option.Some(mkr(5)));\n\
                 println(\"F temp->giveback\");  { let r = giveback(Option.Some(mkr(6))); }\n\
                 println(\"G temp->resig\");     resig(Result.Ok(mkr(7)));\n\
                 println(\"end\");\n\
             }\n"),
        "A named->ignore\n  ig\nd:9\n\
         B named->matchit\n  m:ssssssss2\nd:9\n\
         C named->giveback\n  gb\nd:9\n\
         D temp->ignore\n  ig\nd:9\n\
         E temp->matchit\n  m:ssssssss5\nd:9\n\
         F temp->giveback\n  gb\nd:9\n\
         G temp->resig\n  rig\nd:9\n\
         end\n"
    );
}

#[test]
fn test_optres_payload_runs_user_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(90 + self.id); } }\n\
             struct HeapRes { name: String, id: i64 }\n\
             impl Drop for HeapRes { fn drop(mut ref self) { println(80 + self.id); } }\n\
             fn mk(i: i64) -> Option[HeapRes] {\n\
                 return Option.Some(HeapRes { name: \"payload-string-data\", id: i });\n\
             }\n\
             fn main() {\n\
                 let a = Option.Some(Res { id: 1 });\n\
                 println(1);\n\
                 let b: Result[i64, Res] = Err(Res { id: 2 });\n\
                 println(2);\n\
                 let c = mk(3);\n\
                 println(3);\n\
                 let d: Option[Res] = Option.None;\n\
                 println(4);\n\
                 let e = Option.Some(Res { id: 5 });\n\
                 match e {\n\
                     Some(r) => { println(20 + r.id); }\n\
                     None => { println(0); }\n\
                 }\n\
                 println(5);\n\
                 let f = Option.Some(Res { id: 6 });\n\
                 match f {\n\
                     Some(_) => { println(40); }\n\
                     None => { println(0); }\n\
                 }\n\
                 println(6);\n\
                 let g = Option.Some(Res { id: 7 });\n\
                 let r7 = g.unwrap();\n\
                 println(10 + r7.id);\n\
                 println(7);\n\
                 let h = Res { id: 8 };\n\
                 let s = Option.Some(h);\n\
                 println(8);\n\
             }\n"),
        "91\n1\n92\n2\n83\n3\n4\n25\n95\n5\n40\n96\n6\n17\n97\n7\n98\n8\n"
    );
}

/// B-2026-09-10-22 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_generic_by_value_optres_param_payload_body_runs`.
///
/// The interpreter was the CORRECT backend for this row (it ran the payload
/// element's body all along; the compiled surfaces lost it on the generic leg), so
/// this fixture's job is to pin that it did not move.
///
/// IT NOW HOLDS THE SAME TRANSCRIPT AS ITS TWIN, which it did not when written.
/// `g8`'s `dW8` used to run TWICE here — `g8 dW8 8 dW8 end` — because
/// `g8[T](o: Option[(T, i64)], d: T) -> T` returns `t.0`, a sub-value moved out
/// of a by-value `Option` payload, and the interpreter ran that part's body at
/// the payload's death as well as at the caller's binding. That was
/// B-2026-09-13-5, and the note here predicted that whoever fixed it would drop
/// the second `dW8` and leave the pair holding one string. That is what
/// happened: this expectation is now byte-identical to the twin's.
///
/// `dW108` — the unused `d` argument's body on the `Some` path — is missing from
/// BOTH sides and always was. It is not this row's and not B-2026-09-13-5's:
/// `d` escapes only on the `None` arm, the call takes `Some`, so `d` dies in
/// the callee and one body is owed that no surface runs. That is
/// B-2026-09-17-32 — B-2026-08-28-22's per-path conditional-escape flag covers
/// an `if`/`else` tail and not a `match` arm tail — and it is agreed on all
/// four surfaces, therefore invisible to the A/B rule. Whoever fixes it adds a
/// `dW108` line to this string and to its twin's.
#[test]
fn test_generic_by_value_optres_param_payload_body_runs() {
    assert_eq!(
        run(r#"struct W { id: i64 }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}") } }

fn g1[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }
fn g2(o: Option[(W, i64)]) -> i64 { match o { Some(t) => { return t.1; } None => { return 0; } } }
fn g3[T](o: Result[(T, i64), i64]) -> i64 { match o { Ok(t) => { return t.1; } Err(e) => { return e; } } }
fn g4[T](o: Option[(T, i64)]) -> i64 { match o { Some(_) => { return 5; } None => { return 0; } } }
fn g5[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { println(f"    p{t.1}"); return 0; } None => { return 0; } } }
fn g6[T](o: Option[(T, i64)]) -> i64 { match o { Some(t) => { let x = t.1; return x; } None => { return 0; } } }
fn g7[T](o: Option[T]) -> i64 { match o { Some(t) => { return 1; } None => { return 0; } } }
fn g8[T](o: Option[(T, i64)], d: T) -> T { match o { Some(t) => { return t.0; } None => { return d; } } }

fn main() {
  println("g1"); println(f"  {g1(Some((W { id: 1 }, 9)))}");
  println("g2"); println(f"  {g2(Some((W { id: 2 }, 9)))}");
  println("g3"); println(f"  {g3(Ok((W { id: 3 }, 9)))}");
  println("g4"); println(f"  {g4(Some((W { id: 4 }, 9)))}");
  println("g5"); println(f"  {g5(Some((W { id: 5 }, 9)))}");
  println("g6"); println(f"  {g6(Some((W { id: 6 }, 9)))}");
  println("g7"); println(f"  {g7(Some(W { id: 7 }))}");
  println("g8"); let r = g8(Some((W { id: 8 }, 9)), W { id: 108 }); println(f"  {r.id}");
  println("end");
}
"#),
        r#"g1
dW1
  9
g2
dW2
  9
g3
dW3
  9
g4
dW4
  5
g5
    p9
dW5
  0
g6
dW6
  9
g7
dW7
  1
g8
  8
dW8
end
"#
    );
}

/// B-2026-08-29-10, interpreter leg — a method's owned `Option[T]` /
/// value-enum argument runs its payload's `Drop` body ONCE, in the caller,
/// exactly where the free-function spelling runs it.
///
/// This is the retraction of 57bfb26, which made a method frame's owned-param
/// scrutinee "consuming" so the ARM would fire the body. Its premise was that a
/// method frame's arguments reach no caller-side fire. They do — a method whose
/// owned enum param is never matched has always run its body in the caller, on
/// every surface. What made the fire look absent is that the callee frame's
/// moved-out mark on its own parameter leaked out and disarmed a caller binding
/// sharing that name; the probe spelled both `b`, so the two defects cancelled
/// and the shape read as "zero bodies".
///
/// Firing at the arm as well was therefore a DOUBLE body the moment the names
/// differ, which is what `*-distinct-name` pins: `t.opt_bind(carg)` ran `dR8`
/// twice here against once on all three compiled backends. Every shape appears
/// in both namings because, before this, a rename was a semantic change.
///
/// `mid9` / `dR9` is a callee LOCAL. It is what makes the assertions pin the
/// PLACE rather than the count — an arm fire lands before `mid9`, a callee
/// scope-exit fire between `mid9` and `dR9`, the caller-side fire after `dR9`.
/// 57bfb26 had no such marker, which is how it moved the fire into the callee
/// and still measured "correct".
///
/// `*-returns-payload` keeps B-2026-08-29-9 pinned: when the arm hands the
/// payload back, the caller's result binding owns it and one body is due. That
/// held at HEAD only in the same-name spelling; the method peer of
/// `record_passthrough_arg_moves` now records the suppression against the
/// ARGUMENT, so both namings answer alike.
#[test]
fn method_owned_optres_arg_runs_its_payload_body_like_a_free_fn() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct T { n: i64 }\n\
         impl T {\n\
         \x20   fn opt_bind(ref self, b: Option[R]) -> i64 {\n\
         \x20       let mut out: i64 = 0;\n\
         \x20       match b { Some(r) => { out = r.id; } None => { out = 0; } }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         \x20   fn enum_bind(ref self, b: E) -> i64 {\n\
         \x20       let mut out: i64 = 0;\n\
         \x20       match b { E.A(r) => { out = r.id; } E.B => { out = 0; } }\n\
         \x20       let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         \x20   fn opt_ret(ref self, b: Option[R]) -> R {\n\
         \x20       match b { Some(r) => { return r } None => { return R { id: 0, tag: f\"t0\" } } } }\n\
         \x20   fn enum_ret(ref self, b: E) -> R {\n\
         \x20       match b { E.A(r) => { return r } E.B => { return R { id: 0, tag: f\"t0\" } } } } }\n\
         fn f_opt_bind(b: Option[R]) -> i64 {\n\
         \x20   let mut out: i64 = 0;\n\
         \x20   match b { Some(r) => { out = r.id; } None => { out = 0; } }\n\
         \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out; }\n\
         fn f_enum_bind(b: E) -> i64 {\n\
         \x20   let mut out: i64 = 0;\n\
         \x20   match b { E.A(r) => { out = r.id; } E.B => { out = 0; } }\n\
         \x20   let loc = R { id: 9, tag: f\"t9\" }; println(f\"mid{loc.id}\"); return out }\n";
    for (label, body, want) in [
        // The interpreter fired these at the ARM (before `mid9`) after 57bfb26.
        (
            "method-option-bindout-distinct-name",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = t.opt_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        (
            "free-option-bindout-oracle",
            "let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = f_opt_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        (
            "method-enum-bindout-distinct-name",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = t.enum_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        (
            "free-enum-bindout-oracle",
            "let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = f_enum_bind(carg); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        // Same shapes with the caller's binding spelled as the callee's
        // parameter — the spelling that used to cancel the two defects.
        (
            "method-option-bindout-shadowed-name",
            "let t = T { n: 1 }; let b: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = t.opt_bind(b); println(f\"v{v}\");\n",
            "mid9\ndR9\ndR8\nv8\npost\n",
        ),
        (
            "method-enum-bindout-shadowed-name",
            "let t = T { n: 1 }; let b: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: i64 = t.enum_bind(b); println(f\"v{v}\");\n",
            "mid9\ndR9\ndE\ndR8\nv8\npost\n",
        ),
        // B-2026-08-29-9's boundary, both namings.
        (
            "method-option-returns-payload-distinct-name",
            "let t = T { n: 1 }; let carg: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: R = t.opt_ret(carg); println(f\"v{v.id}\");\n",
            "v8\ndR8\npost\n",
        ),
        (
            "method-option-returns-payload-shadowed-name",
            "let t = T { n: 1 }; let b: Option[R] = Some(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: R = t.opt_ret(b); println(f\"v{v.id}\");\n",
            "v8\ndR8\npost\n",
        ),
        (
            "method-enum-returns-payload-distinct-name",
            "let t = T { n: 1 }; let carg: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             \x20 let v: R = t.enum_ret(carg); println(f\"v{v.id}\");\n",
            "dE\nv8\ndR8\npost\n",
        ),
        // Two calls through one method, the first escaping and the second
        // dying: the escaping call's marks must not silence the next call's
        // argument (B-2026-08-29-11's shape, one channel over).
        (
            "two-calls-second-must-still-fire",
            "let t = T { n: 1 }; let c1: E = E.A(R { id: 1, tag: f\"t1\" }); let c2: E = E.A(R { id: 2, tag: f\"t2\" });\n\
             \x20 let a: i64 = t.enum_bind(c1); println(f\"a{a}\");\n\
             \x20 let d: i64 = t.enum_bind(c2); println(f\"b{d}\");\n",
            "mid9\ndR9\ndE\ndR1\na1\nmid9\ndR9\ndE\ndR2\nb2\npost\n",
        ),
    ] {
        let src = format!("{H}fn main() {{ {body} println(\"post\") }}\n");
        assert_eq!(run(&src), want, "{label}");
    }
}

/// B-2026-08-28-58, interpreter leg — the twin of
/// `codegen::e2e_own_drop_enum_as_optres_payload_runs_its_body`.
///
/// The last position in the -46/-47/-54/-55 family: an own-`Drop` enum held as
/// an `Option`/`Result` PAYLOAD. The interpreter ran nothing for any of these
/// shapes, because `run_optres_payload_user_drops` bound the payload as
/// `Value::Struct` and returned on anything else, and `type_name_runs_user_drop`
/// — the gate that arms the registration at all — recursed only through
/// `StructDef`, so a payload-only enum never qualified either.
///
/// Both halves are pinned here. The compiled twin additionally carries the
/// ORDERING, which is where its own second defect lived (the memory channel ran
/// the body at scope exit); the interpreter never had that channel, so its rows
/// are about the missing body alone.
#[test]
fn own_drop_enum_as_optres_payload_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         enum H { A(R), B }\n\
         enum J { A(i64), B }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "option-unit",
            "fn main() { let o: Option[E] = Some(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        (
            "option-payload",
            "fn main() { let o: Option[E] = Some(E.A(R { id: 4 }));\n\
             \x20            println(\"mid\"); }\n",
            "drop E\ndrop R4\nmid\n",
        ),
        (
            "result-unit",
            "fn main() { let r: Result[E, i64] = Ok(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        (
            "result-payload",
            "fn main() { let r: Result[E, i64] = Ok(E.A(R { id: 5 }));\n\
             \x20            println(\"mid\"); }\n",
            "drop E\ndrop R5\nmid\n",
        ),
        (
            "result-err-position",
            "fn main() { let r: Result[i64, E] = Err(E.B); println(\"mid\"); }\n",
            "drop E\nmid\n",
        ),
        // -54's predicate reaching this position: no own `Drop`, Drop-bearing
        // payload. This is the half that needed the enum leg on
        // `type_name_runs_user_drop`, not just the runner.
        (
            "payload-only-option",
            "fn main() { let o: Option[H] = Some(H.A(R { id: 6 }));\n\
             \x20            println(\"mid\"); }\n",
            "drop R6\nmid\n",
        ),
        (
            "payload-only-result",
            "fn main() { let r: Result[H, i64] = Ok(H.A(R { id: 7 }));\n\
             \x20            println(\"mid\"); }\n",
            "drop R7\nmid\n",
        ),
        // BOUNDARY — neither an own body nor a Drop-bearing payload.
        (
            "no-drop-option",
            "fn main() { let o: Option[J] = Some(J.A(3)); println(\"mid\"); }\n",
            "mid\n",
        ),
        // BOUNDARY — `None` has no live payload.
        (
            "none-runs-nothing",
            "fn main() { let o: Option[E] = None; println(\"mid\"); }\n",
            "mid\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
    // The MOVE-OUT peer, and the reason this fix cannot double-fire: a
    // consuming `match` arm binds the payload out, so the SOURCE's walk must
    // stay retracted and only the BINDING's registration fires.
    //
    // This row read `got` alone when it was written, and said so: the
    // consuming arm losing the bound enum's body was a separate pre-existing
    // gap, filed as B-2026-08-28-63 and fixed there. The binding now runs its
    // body once, which is what the updated expectation pins — the claim is
    // unchanged (exactly one fire), only the count it was measuring against.
    assert_eq!(
        run(&format!(
            "{H}fn main() {{ let o: Option[E] = Some(E.A(R {{ id: 8 }}));\n\
             \x20            match o {{ Some(e) => {{ println(\"got\") }}\n\
             \x20                       None => {{ println(\"none\") }} }} }}\n"
        )),
        "got\ndrop E\ndrop R8\n",
        "consuming-match-fires-exactly-once"
    );
}

#[test]
fn test_optres_payload_bodies_in_nested_positions() {
    // B-2026-08-03-1 — interpreter twin of `tests/codegen.rs`'s
    // `e2e_optres_payload_bodies_in_nested_positions`, same source and
    // expected string. Both backends were silent pre-fix and each needed its
    // own wiring: the interpreter's `field_te_runs_user_drop` gate, its
    // value-level `field_value_carries_user_drop`, and three separate walks
    // (struct field, Vec element, and BOTH the field-level and binding-level
    // map walks, which are distinct functions on this side).
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct H { o: Option[Res], t: i64 }\n\
             fn main() {\n\
                 println(\"field:\");\n\
                 {\n\
                     let h = H { o: Option.Some(Res { id: 1, name: f\"a{1}\" }), t: 2 };\n\
                     println(h.t);\n\
                 }\n\
                 println(\"vecelem:\");\n\
                 {\n\
                     let mut v: Vec[Option[Res]] = Vec.new();\n\
                     v.push(Option.Some(Res { id: 2, name: f\"b{2}\" }));\n\
                     println(v.len());\n\
                 }\n\
                 println(\"mapval:\");\n\
                 {\n\
                     let mut m: Map[i64, Option[Res]] = Map.new();\n\
                     m.insert(5, Option.Some(Res { id: 3, name: f\"c{3}\" }));\n\
                     println(m.len());\n\
                 }\n\
                 println(\"tupelem:\");\n\
                 {\n\
                     let t = (Option.Some(Res { id: 4, name: f\"d{4}\" }), 7);\n\
                     println(t.1);\n\
                 }\n\
                 println(\"none:\");\n\
                 {\n\
                     let h2 = H { o: Option.None, t: 9 };\n\
                     println(h2.t);\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "field:\n2\ndrop 1 a1\nvecelem:\n1\ndrop 2 b2\nmapval:\n1\ndrop 3 c3\n\
         tupelem:\n7\ndrop 4 d4\nnone:\n9\nend\n"
    );
}

/// B-2026-07-30-11 (optres bare-statement leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_optres_bare_discard_payload_body`, same source
/// and expected string. Pre-fix the bare Call shape hit the enum leg's
/// declared-type walk (a no-op for the built-in `Option`) and the Path-ctor
/// shape wasn't handled at all — both silent while `let _ =` fired.
#[test]
fn test_optres_bare_discard_payload_body() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             fn mkopt(n: i64) -> Option[Res] {\n\
                 return Option.Some(Res { id: n, name: f\"o{n}\" });\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let _ = mkopt(1);\n\
                 println(\"b\");\n\
                 mkopt(2);\n\
                 println(\"c\");\n\
                 Option.Some(Res { id: 3, name: f\"o3\" });\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 1 o1\nb\ndrop 2 o2\nc\ndrop 3 o3\nend\n"
    );
}

/// B-2026-07-30-11 (boxed-payload bodies) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_boxed_option_payload_body_once`, same source
/// and expected string.
#[test]
fn test_boxed_option_payload_body_once() {
    assert_eq!(
        run("struct Res { id: i64, s: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let mut v: Vec[Res] = Vec.new();\n\
                 v.push(Res { id: 1, s: f\"one-{1}\" });\n\
                 v.push(Res { id: 2, s: f\"two-{2}\" });\n\
                 println(\"a\");\n\
                 while let Option.Some(r) = v.pop() {\n\
                     println(f\"got {r.id} len {r.s.len()}\");\n\
                 }\n\
                 println(\"b\");\n\
                 let mut w: Vec[Res] = Vec.new();\n\
                 w.push(Res { id: 3, s: f\"three-{3}\" });\n\
                 if let Option.Some(r) = w.pop() {\n\
                     println(f\"iflet {r.id} len {r.s.len()}\");\n\
                 }\n\
                 println(\"c\");\n\
                 let mut u: Vec[Res] = Vec.new();\n\
                 u.push(Res { id: 4, s: f\"four-{4}\" });\n\
                 match u.pop() {\n\
                     Option.Some(r) => { println(f\"match {r.id} len {r.s.len()}\"); }\n\
                     Option.None => { println(\"none\"); }\n\
                 }\n\
                 println(\"end\");\n\
             }\n"),
        "a\ngot 2 len 5\ndrop 2\ngot 1 len 5\ndrop 1\nb\niflet 3 len 7\ndrop 3\nc\n\
         match 4 len 6\ndrop 4\nend\n"
    );
}

#[test]
fn test_tuple_held_optres_payload_bodies_fire() {
    // B-2026-08-03-3 interp twin. The leak this row is about is codegen-only
    // (the interpreter frees by value), but two of the six positions were also
    // BODY-silent here: a `Vec[(Option[Res], i64)]` element (the tuple item walk
    // only looked at direct structs) and a NESTED tuple. Both are pinned as the
    // parity side of the codegen fix; the other four already fired and are kept
    // as controls so a future change can't quietly silence them.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             fn take(t: (Option[Res], i64)) -> i64 { t.1 }\n\
             fn mk() -> (Option[Res], i64) { (Option.Some(Res { id: 3, name: f\"c{3}\" }), 30) }\n\
             fn main() {\n\
                 println(\"binding:\");\n\
                 { let t = (Option.Some(Res { id: 1, name: f\"a{1}\" }), 10); println(t.1); }\n\
                 println(\"param:\");\n\
                 { let t = (Option.Some(Res { id: 2, name: f\"bb{2}\" }), 20); println(take(t)); }\n\
                 println(\"returned:\");\n\
                 { let t = mk(); println(t.1); }\n\
                 println(\"result:\");\n\
                 { let t: (Result[Res, i64], i64) = (Result.Ok(Res { id: 4, name: f\"dddd{4}\" }), 40); println(t.1); }\n\
                 println(\"vec-of-tuple:\");\n\
                 {\n\
                     let mut v: Vec[(Option[Res], i64)] = Vec.new();\n\
                     v.push((Option.Some(Res { id: 5, name: f\"eeeee{5}\" }), 50));\n\
                     println(v.len());\n\
                 }\n\
                 println(\"nested-tuple:\");\n\
                 { let t = ((Option.Some(Res { id: 6, name: f\"ffffff{6}\" }), 60), 600); println(t.1); }\n\
                 println(\"end\");\n\
             }\n"),
        "binding:\n10\ndrop 1 a1\nparam:\n20\ndrop 2 bb2\n\
         returned:\n30\ndrop 3 c3\nresult:\n40\ndrop 4 dddd4\n\
         vec-of-tuple:\n1\ndrop 5 eeeee5\n\
         nested-tuple:\n600\ndrop 6 ffffff6\nend\n"
    );
}

#[test]
fn test_result_struct_payload_field_freed_and_single_body() {
    // B-2026-08-03-3 leg B — the ORACLE half. The interpreter was already
    // correct on every one of these seven positions (it frees through Rust
    // ownership, so the codegen leak has no analogue), which is exactly why it
    // is worth pinning: the codegen fix is judged by matching this, and two of
    // the three defects it had to close were AOT-only body-COUNT bugs that no
    // sanitizer sees — `local-match` fired a spurious body over the zeroed
    // source slot, and `param-match` fired the callee's on top of the caller's.
    // Keep this expectation byte-identical to the codegen twin
    // `test_e2e_result_struct_payload_field_freed_and_single_body`.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
             fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") }\n\
             }\n\
             struct H { r: Result[Res, i64], t: i64 }\n\
             struct Hm { r: Result[Res, String], t: i64 }\n\
             fn take(h: H) -> i64 { h.t }\n\
             fn mk() -> H { H { r: Result.Ok(Res { id: 2, name: f\"bb{2}\" }), t: 20 } }\n\
             fn consume(h: H) -> i64 { match h.r { Result.Ok(x) => x.id, Result.Err(e) => e } }\n\
             fn main() {\n\
             println(\"binding:\");\n\
             { let h = H { r: Result.Ok(Res { id: 1, name: f\"a{1}\" }), t: 10 }; println(h.t); }\n\
             println(\"returned:\");\n\
             { let h = mk(); println(h.t); }\n\
             println(\"byvalue:\");\n\
             { let h = H { r: Result.Ok(Res { id: 3, name: f\"ccc{3}\" }), t: 30 }; println(take(h)); }\n\
             println(\"moveout:\");\n\
             { let h = H { r: Result.Ok(Res { id: 4, name: f\"dddd{4}\" }), t: 40 }; let x = h.r; println(h.t); }\n\
             println(\"local-match:\");\n\
             { let h = H { r: Result.Ok(Res { id: 5, name: f\"eeeee{5}\" }), t: 50 };\n\
             let v = match h.r { Result.Ok(x) => x.id, Result.Err(e) => e }; println(v); }\n\
             println(\"param-match:\");\n\
             { let h = H { r: Result.Ok(Res { id: 6, name: f\"ffffff{6}\" }), t: 60 }; println(consume(h)); }\n\
             println(\"mixed-halves:\");\n\
             { let h = Hm { r: Result.Err(f\"ggggggg{7}\"), t: 70 }; println(h.t); }\n\
             println(\"end\");\n\
             }\n"),
        "binding:\n10\ndrop 1 a1\n\
         returned:\n20\ndrop 2 bb2\n\
         byvalue:\n30\ndrop 3 ccc3\n\
         moveout:\ndrop 4 dddd4\n40\n\
         local-match:\ndrop 5 eeeee5\n5\n\
         param-match:\n6\ndrop 6 ffffff6\n\
         mixed-halves:\n70\nend\n"
    );
}

#[test]
fn test_optres_tuple_payload_is_owned_exactly_once() {
    // B-2026-08-05-3 — the ORACLE half. The interpreter has always owned an
    // `Option`/`Result` tuple payload exactly once; codegen leaked it three
    // ways (an ungated Result arm suppression, an operator-desugar misread in
    // the consumption classifier for guarded arms, and a box-only drop for the
    // Option carrier's boxed tuple payload). Keep in step with the codegen twin
    // `e2e_optres_tuple_payload_is_owned_exactly_once`.
    //
    // The seed is the literal 1 here rather than `env.args().len()`: the codegen
    // fixture needs an opaque seed to survive -O2 folding and 1 is what that
    // yields under its harness, while `env.args()` in an in-process interpreter
    // test would report the TEST binary's argv.
    assert_eq!(
        run(
            "struct H { a: Vec[i64], b: i64 }\n\
             fn mkv(k: i64) -> Vec[i64] { let mut v: Vec[i64] = Vec.new(); v.push(k); v.push(k + 1i64); return v; }\n\
             fn mks(k: i64) -> String { let mut s: String = String.new(); s.push_str(f\"pay-{k}\"); return s; }\n\
             fn dig(i: i64) -> String { let mut d: String = String.new(); d.push_str(f\"{i}\"); return d; }\n\
             fn sinkt(t: (Vec[i64], i64)) -> i64 { return t.0[0i64]; }\n\
             fn main() {\n\
             \x20   let base: i64 = 1i64;\n\
             \x20   let mut acc = 0i64;\n\
             \x20   let mut i = base;\n\
             \x20   while i < base + 100i64 {\n\
             \x20       // 1. Result[tuple] bound and READ, never moved — the leg the ungated\n\
             \x20       //    arm suppression broke.\n\
             \x20       let r1: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r1 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0[0i64] + x.1; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 2. Result[tuple] never matched at all — the let-site walk alone.\n\
             \x20       let r2: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       acc = acc + 1i64;\n\
             \x20       // 3. GUARDED arm — the `x.1 == 5` operator desugar.\n\
             \x20       let r3: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r3 {\n\
             \x20           Result.Ok(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // 4. String element, read through its BYTES.\n\
             \x20       let r4: Result[(String, i64), i64] = Result.Ok((mks(i), 5i64));\n\
             \x20       match r4 {\n\
             \x20           Result.Ok(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 5. Vec[String] element.\n\
             \x20       let mut vs: Vec[String] = Vec.new();\n\
             \x20       vs.push(mks(i));\n\
             \x20       vs.push(mks(i + 1i64));\n\
             \x20       let r5: Result[(Vec[String], i64), i64] = Result.Ok((vs, 5i64));\n\
             \x20       match r5 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 6. Heap on the Err half.\n\
             \x20       let r6: Result[i64, (Vec[i64], i64)] = Result.Err((mkv(i), 5i64));\n\
             \x20       match r6 {\n\
             \x20           Result.Ok(v) => { acc = acc + v; }\n\
             \x20           Result.Err(e) => { acc = acc + e.0[0i64]; }\n\
             \x20       }\n\
             \x20       // 7. if-let, borrow-only and moving.\n\
             \x20       let r7: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       if let Result.Ok(x) = r7 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
             \x20       let r8: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       let mut g: Vec[i64] = Vec.new();\n\
             \x20       if let Result.Ok(x) = r8 { g = x.0; } else { acc = acc - 1i64; }\n\
             \x20       acc = acc + g[0i64] + g.len();\n\
             \x20       // 8. Moved out of the match into an owned-param callee.\n\
             \x20       let r9: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r9 {\n\
             \x20           Result.Ok(x) => { acc = acc + sinkt(x); }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // 9. Element MOVED out through the match value.\n\
             \x20       let r10: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       let g2: Vec[i64] = match r10 { Result.Ok(x) => x.0, Result.Err(e) => Vec.new() };\n\
             \x20       acc = acc + g2[1i64];\n\
             \x20       // --- CONTROLS: shapes that must NOT gain a second owner ---\n\
             \x20       // C1. Tuple PATTERN destructure — the elements own themselves.\n\
             \x20       let r11: Result[(Vec[i64], i64), i64] = Result.Ok((mkv(i), 5i64));\n\
             \x20       match r11 {\n\
             \x20           Result.Ok((v, k)) => { acc = acc + v[0i64] + k; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // C2. STRUCT payload — must keep its unconditional arm suppression.\n\
             \x20       let r12: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
             \x20       match r12 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.a[0i64] + x.b; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       let r13: Result[H, i64] = Result.Ok(H { a: mkv(i), b: 5i64 });\n\
             \x20       match r13 {\n\
             \x20           Result.Ok(x) if x.b == 5i64 => { acc = acc + x.a[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // C4. All-scalar tuple payload — must get no drop at all.\n\
             \x20       let r15: Result[(i64, i64), i64] = Result.Ok((i, 5i64));\n\
             \x20       match r15 {\n\
             \x20           Result.Ok(x) => { acc = acc + x.0 + x.1; }\n\
             \x20           Result.Err(e) => { acc = acc + e; }\n\
             \x20       }\n\
             \x20       // --- OPTION carrier (B-2026-08-05-3's own shape; second attempt) ---\n\
             \x20       // O1. Bound and READ, never moved.\n\
             \x20       let o1: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o1 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0[0i64] + x.1; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O2. NEVER matched at all — the let-site walk alone. This is what\n\
             \x20       //     shows the bug is scope-exit, not arm binding.\n\
             \x20       let o2: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       acc = acc + 1i64;\n\
             \x20       // O3. GUARDED arm.\n\
             \x20       let o3: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o3 {\n\
             \x20           Option.Some(x) if x.1 == 5i64 => { acc = acc + x.0[0i64]; }\n\
             \x20           _ => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O4. String element, read through its BYTES.\n\
             \x20       let o4: Option[(String, i64)] = Option.Some((mks(i), 5i64));\n\
             \x20       match o4 {\n\
             \x20           Option.Some(x) => { if x.0.contains(dig(i)) { acc = acc + x.0.len(); } }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O5. Vec[String] element.\n\
             \x20       let mut ovs: Vec[String] = Vec.new();\n\
             \x20       ovs.push(mks(i));\n\
             \x20       ovs.push(mks(i + 1i64));\n\
             \x20       let o5: Option[(Vec[String], i64)] = Option.Some((ovs, 5i64));\n\
             \x20       match o5 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0.len() + x.0[0i64].len(); }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O6. if-let, borrow-only and moving.\n\
             \x20       let o6: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       if let Option.Some(x) = o6 { acc = acc + x.0[0i64]; } else { acc = acc - 1i64; }\n\
             \x20       let o7: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       let mut og: Vec[i64] = Vec.new();\n\
             \x20       if let Option.Some(x) = o7 { og = x.0; } else { acc = acc - 1i64; }\n\
             \x20       acc = acc + og[0i64] + og.len();\n\
             \x20       // O8. Moved into an owned-param callee.\n\
             \x20       let o8: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o8 {\n\
             \x20           Option.Some(x) => { acc = acc + sinkt(x); }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // --- The two shapes that DEFEATED the first attempt at this leg. ---\n\
             \x20       // O9. Element MOVED out through the match value. The arm takes the\n\
             \x20       //     box's interior, so the box drop must retract to box-only.\n\
             \x20       let o9: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       let og2: Vec[i64] = match o9 { Option.Some(x) => x.0, Option.None => Vec.new() };\n\
             \x20       acc = acc + og2[1i64];\n\
             \x20       // O10. Tuple PATTERN destructure — the leaf bindings own their\n\
             \x20       //      elements, so the box drop must retract here too.\n\
             \x20       let o10: Option[(Vec[i64], i64)] = Option.Some((mkv(i), 5i64));\n\
             \x20       match o10 {\n\
             \x20           Option.Some((v, k)) => { acc = acc + v[0i64] + k; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O11. STRUCT payload control — a different channel owns it.\n\
             \x20       let o11: Option[H] = Option.Some(H { a: mkv(i), b: 5i64 });\n\
             \x20       match o11 {\n\
             \x20           Option.Some(x) => { acc = acc + x.a[0i64] + x.b; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       // O12. All-scalar tuple payload — must get no drop at all.\n\
             \x20       let o12: Option[(i64, i64)] = Option.Some((i, 5i64));\n\
             \x20       match o12 {\n\
             \x20           Option.Some(x) => { acc = acc + x.0 + x.1; }\n\
             \x20           Option.None => { acc = acc - 1i64; }\n\
             \x20       }\n\
             \x20       i = i + 1i64;\n\
             \x20   }\n\
             \x20   println(f\"acc={acc}\");\n\
             }\n"
        ),
        "acc=108568\n"
    );
}

#[test]
fn test_question_reconstructs_wide_and_boxed_ok_payloads() {
    // B-2026-08-04-9 — the ORACLE half. The interpreter has always unwrapped
    // `?` at the payload's real width; codegen rebuilt from at most three
    // words read straight out of the enum aggregate, which broke two ways:
    // a BOXED payload (`Full`, 6 words) came back as the box pointer, and a
    // WIDE-BUT-INLINE one (`Mid`, 4 words through `Result`'s 5-word area)
    // lost every word past the third. Both were silent under AOT.
    //
    // `Mid` appears under BOTH carriers on purpose — inline through `Result`,
    // boxed through `Option` — so one type exercises both defects.
    //
    // Keep byte-identical to the codegen twin
    // `e2e_question_reconstructs_wide_and_boxed_ok_payloads`.
    assert_eq!(
        run("struct Full { name: String, buf: Vec[i64] }\n\
             struct Mid { name: String, pad: i64 }\n\
             fn mkf(i: i64) -> Full {\n\
             let mut b: Vec[i64] = Vec.new();\n\
             b.push(i);\n\
             let mut s: String = String.new();\n\
             s.push_str(\"wide-\");\n\
             s.push_str(f\"{i}\");\n\
             return Full { name: s, buf: b };\n\
             }\n\
             fn mkm(i: i64) -> Mid {\n\
             let mut s: String = String.new();\n\
             s.push_str(\"mid-\");\n\
             s.push_str(f\"{i}\");\n\
             return Mid { name: s, pad: i };\n\
             }\n\
             fn resf(i: i64) -> Result[Full, String] { return Result.Ok(mkf(i)); }\n\
             fn optf(i: i64) -> Option[Full] { return Option.Some(mkf(i)); }\n\
             fn optm(i: i64) -> Option[Mid] { return Option.Some(mkm(i)); }\n\
             fn resm(i: i64) -> Result[Mid, String] { return Result.Ok(mkm(i)); }\n\
             fn run_res(i: i64) -> Result[i64, String] {\n\
             let a = resf(i)?;\n\
             println(f\"a:{a.name}:{a.buf.len()}\");\n\
             let b = resm(i)?;\n\
             println(f\"b:{b.name}:{b.pad}\");\n\
             let c = resf(i)?;\n\
             let Full { name, buf: _ } = c;\n\
             println(f\"c:{name}\");\n\
             return Result.Ok(1i64);\n\
             }\n\
             fn run_opt(i: i64) -> Option[i64] {\n\
             let d = optf(i)?;\n\
             println(f\"d:{d.name}:{d.buf.len()}\");\n\
             let e = optm(i)?;\n\
             println(f\"e:{e.name}:{e.pad}\");\n\
             return Option.Some(2i64);\n\
             }\n\
             fn main() {\n\
             match run_res(3i64) {\n\
             Result.Ok(v) => { println(f\"res:{v}\"); }\n\
             Result.Err(er) => { println(f\"err:{er}\"); }\n\
             }\n\
             match run_opt(4i64) {\n\
             Option.Some(v) => { println(f\"opt:{v}\"); }\n\
             Option.None => { println(\"opt:none\"); }\n\
             }\n\
             }\n"),
        "a:wide-3:1\nb:mid-3:3\nc:wide-3\nres:1\n\
         d:wide-4:1\ne:mid-4:4\nopt:2\n"
    );
}

#[test]
fn gpu_min_max_return_option_and_ignore_nan() {
    // `min`/`max` are `Option[f32]`, matching `Stats.min` / `Vec.min`: an
    // empty buffer has no extremum, and answering with the shader's +inf
    // padding identity would be a plausible wrong number.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [3.0, 1.5, 2.0];\n\
        \x20   let m = gpu.min(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "1.5");

    let out = run_no_errors(
        "fn main() {\n\
        \x20   let v: Vec[f32] = [];\n\
        \x20   let m = gpu.max(v);\n\
        \x20   match m {\n\
        \x20       Some(x) => println(f\"{x}\"),\n\
        \x20       None => println(\"empty\"),\n\
        \x20   }\n\
        }",
    );
    assert_eq!(out.trim(), "empty");

    // NaN is IGNORED from either side, and the two orders must agree. A
    // NaN-propagating min would answer 1 for one of these and NaN for the
    // other — the positional behaviour WGSL's `min` builtin has, and the
    // reason the emitted shader does not call it.
    for buf in ["[nan, 1.0, 2.0]", "[2.0, 1.0, nan]"] {
        let out = run_no_errors(&format!(
            "fn main() {{\n\
            \x20   let zero: f32 = 0.0;\n\
            \x20   let nan: f32 = zero / zero;\n\
            \x20   let v: Vec[f32] = {buf};\n\
            \x20   let m = gpu.min(v);\n\
            \x20   match m {{\n\
            \x20       Some(x) => println(f\"{{x}}\"),\n\
            \x20       None => println(\"empty\"),\n\
            \x20   }}\n\
            }}"
        ));
        assert_eq!(out.trim(), "1", "NaN must not change the answer in {buf}");
    }
}

// ── B-2026-08-27-46: a synthesized receiver must not adopt another span's type ──

#[test]
fn variant_literal_cmp_receiver_is_not_retagged_to_the_call_result_enum() {
    // B-2026-08-27-46. `.cmp()` on a bare enum-variant LITERAL answered a
    // CONSTANT — `E.A.cmp(E.B)`, `E.B.cmp(E.A)` and `E.A.cmp(E.A)` all returned
    // the same `Ordering` (measured: `2 2 2`, constant Greater) — while both
    // compiled backends answered correctly. Silent: `karac check` passed and
    // the program exited 0.
    //
    // The receiver never reached the `cmp` arm as an `E`. `lowering.rs`'s
    // `rewrite_enum_literal_method_call` materializes the literal receiver into
    // `{ let __karac_elm_N = E.A; __karac_elm_N.cmp(other) }` and gives the
    // synthesized identifier the ORIGINAL CALLEE's span — at which the
    // typechecker had recorded the enclosing call's RESULT type, `Ordering`.
    // `retag_bare_unit_variant` read that and rewrote the receiver to
    // `Ordering.A`, a variant `Ordering` does not declare; `value_compare` then
    // compared two unrelated enums and returned the same answer every time.
    //
    // Asserting all three orderings is the point: a fix that returns a
    // different constant still passes a one-pair test.
    assert_eq!(
        run_no_errors(
            "#[derive(Ord, Eq)]
             enum E { A, B }
             fn tag(o: Ordering) -> i64 {
                 if o.is_lt() { return 0; }
                 if o.is_eq() { return 1; }
                 return 2;
             }
             fn main() {
                 let a = E.A;
                 let b = E.B;
                 println(f\"{tag(E.A.cmp(E.B))} {tag(E.B.cmp(E.A))} {tag(E.A.cmp(E.A))}\");
                 println(f\"{tag(a.cmp(b))} {tag(b.cmp(a))} {tag(a.cmp(a))}\");
                 println(f\"{tag(a.cmp(E.B))} {tag(b.cmp(E.A))} {tag(a.cmp(E.A))}\");
             }"
        ),
        // Bound receiver and literal ARGUMENT were always correct; the literal
        // RECEIVER row is the one that was wrong. All three must now agree.
        "0 2 1\n0 2 1\n0 2 1\n"
    );
}

// ── B-2026-08-21-26: auto-generated `TryFrom[intN]` ─────────────
//
// design.md § Enum Discriminant Runtime Surface. The inbound twin of
// `.discriminant()` below, reading the same folded table backwards.

#[test]
fn enum_try_from_maps_a_declared_value_to_its_variant() {
    // The spec's own example, plus a value in no variant. The declared values
    // are non-positional, so a lowering that compared layout TAGS would answer
    // `Ok` for 0 and `Err` for 8.
    let out = run("#[repr(u8)]\n\
    enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }\n\
    fn probe(raw: u8) {\n\
        match UsbClass.try_from(raw) {\n\
            Ok(c) => println(f\"ok{c.discriminant()}\"),\n\
            Err(e) => match e {\n\
                DiscriminantError.OutOfRange { value } => println(f\"no{value}\"),\n\
            },\n\
        }\n\
    }\n\
    fn main() {\n\
        probe(0u8); probe(1u8); probe(3u8); probe(8u8); probe(255u8);\n\
    }");
    assert_eq!(out, "no0\nok1\nok3\nok8\nno255\n");
}

#[test]
fn test_question_mark_propagation_survives_the_guard() {
    // `?` propagates an `Err` through `set_cf(Return(..))` — the same funnel,
    // but on a real value rather than a fault poison. Guarding the funnel must
    // not touch it.
    assert_eq!(
        run_no_errors(
            "fn half(n: i64) -> Result[i64, String] {\n\
                 if n % 2 != 0 { return Err(\"odd\") }\n\
                 Ok(n / 2)\n\
             }\n\
             fn chain(n: i64) -> Result[i64, String] { let h = half(n)?; Ok(h + 1) }\n\
             fn main() {\n\
                 match chain(9) { Ok(v) => println(f\"ok {v}\"), Err(e) => println(f\"err {e}\") }\n\
                 match chain(8) { Ok(v) => println(f\"ok {v}\"), Err(e) => println(f\"err {e}\") }\n\
             }\n"
        ),
        "err odd\nok 5\n"
    );
}

/// B-2026-09-05-11 — the INTERPRETER twin of
/// `e2e_nested_optres_ctor_let_over_param_runs_one_body`, same program and
/// the same expected string. The interpreter's half of the row was the METHOD
/// spelling only: `method_frame_caller_retains_args` asks every by-value
/// param by argument SHAPE, so a literal `true` (or an `i64` the method
/// returns) answered for the whole frame, the frame stopped retracting, and
/// `let o = Option.Some(r)` minted a slot beside the caller's fire. A scalar
/// carries no `Drop` work and is now neutral there; the free-fn spelling never
/// consulted the predicate and was right all along.
#[test]
fn test_nested_optres_ctor_let_over_param_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"drop {self.id} {self.name}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn fl(r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); } return 7; }
fn flb(r: R, keep: bool) -> i64 { if keep { let o = Some(r); println("held"); } return 7; }
fn flt(r: R) -> i64 { let o = Option.Some(r); println("held"); return 7; }
fn flr(r: R, keep: bool) -> i64 { if keep { let o: Result[R, i64] = Result.Ok(r); println("held"); } return 7; }
fn flu(r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); let p = o; println("used"); } return 7; }
fn fln(r: R, n: i64) -> i64 { let o = Option.Some(r); println("held"); return n; }
struct K { z: i64 }
impl K {
    fn l(ref self, r: R, keep: bool) -> i64 { if keep { let o = Option.Some(r); println("held"); } return 7; }
    fn lb(ref self, r: R, keep: bool) -> i64 { if keep { let o = Some(r); println("held"); } return 7; }
    fn lt(ref self, r: R) -> i64 { let o = Option.Some(r); println("held"); return 7; }
    fn lr(ref self, r: R, keep: bool) -> i64 { if keep { let o: Result[R, i64] = Result.Ok(r); println("held"); } return 7; }
    fn ln(ref self, r: R, n: i64) -> i64 { let o = Option.Some(r); println("held"); return n; }
}
fn main() {
    let k = K { z: 0 };
    println("fl-f"); let _ = fl(mk(1), false);
    println("fl-t"); let _ = fl(mk(2), true);
    println("flb-t"); let _ = flb(mk(3), true);
    println("flt"); let _ = flt(mk(4));
    println("flr-t"); let _ = flr(mk(5), true);
    println("flu-t"); let _ = flu(mk(6), true);
    println("fln"); let _ = fln(mk(7), 9);
    println("fl-n"); let a = mk(8); let _ = fl(a, true);
    println("ml-f"); let _ = k.l(mk(11), false);
    println("ml-t"); let _ = k.l(mk(12), true);
    println("mlb-t"); let _ = k.lb(mk(13), true);
    println("mlt"); let _ = k.lt(mk(14));
    println("mlr-t"); let _ = k.lr(mk(15), true);
    println("mln"); let _ = k.ln(mk(16), 9);
    println("ml-n"); let b = mk(17); let _ = k.l(b, true);
    println("ml-var"); let t = true; let _ = k.l(mk(18), t);
    println("end");
}"#),
        "fl-f\ndrop 1 h1\nfl-t\nheld\ndrop 2 h2\nflb-t\nheld\ndrop 3 h3\nflt\nheld\ndrop 4 h4\nflr-t\nheld\ndrop 5 h5\nflu-t\nheld\nused\ndrop 6 h6\nfln\nheld\ndrop 7 h7\nfl-n\nheld\ndrop 8 h8\nml-f\ndrop 11 h11\nml-t\nheld\ndrop 12 h12\nmlb-t\nheld\ndrop 13 h13\nmlt\nheld\ndrop 14 h14\nmlr-t\nheld\ndrop 15 h15\nmln\nheld\ndrop 16 h16\nml-n\nheld\ndrop 17 h17\nml-var\nheld\ndrop 18 h18\nend\n",
        "a nested Option/Result ctor let over a by-value param runs one body"
    );
}

/// B-2026-09-06-53 — the interpreter half. Its own classifier carried the
/// same scalar false positive, and its whole-alias closure carried it one step
/// further: `let x = mkUses(i); let y = keep(x)` lost the body here while both
/// compiled backends ran it once, so the scalar test sits in the shared AST
/// predicate both backends read.
///
/// Twin of `tests/codegen.rs`'s `e2e_scalar_argument_does_not_make_the_result_a_view`, pinned to the same string.
#[test]
fn test_scalar_argument_does_not_make_the_result_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct H { r: R, n: i64 }
fn mkUses(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn mkIgnores(i: i64) -> R { return R { id: 0, name: "z" }; }
fn mkName(i: i64) -> R { return R { id: 7, name: f"n{i}" }; }
fn mkOpt(i: i64) -> Option[R] { return Option.Some(R { id: i, name: f"o{i}" }); }
fn mkFlt(x: f64) -> R { return R { id: 20, name: f"f{x}" }; }
fn mkChr(c: char) -> R { return R { id: 21, name: f"c{c}" }; }
fn keep(r: R) -> R { return r; }
fn wrap(r: R) -> H { return H { r: r, n: 1 }; }
fn both(i: i64, r: R) -> R { return r; }
impl H { fn make(i: i64) -> R { return R { id: i, name: f"a{i}" }; } }

fn scalar_arg(i: i64) { let x = mkUses(i); println(f"  v={x.id}"); }
fn scalar_rebound(i: i64) { let j = i; let x = mkUses(j); println(f"  v={x.id}"); }
fn scalar_chained(i: i64) { let x = mkUses(i); let y = keep(x); println(f"  v={y.id}"); }
fn scalar_unused(i: i64) { let x = mkIgnores(i); println(f"  v={x.id}"); }
fn scalar_interpolated(i: i64) { let x = mkName(i); println(f"  v={x.id}"); }
fn scalar_constant(i: i64) { let x = mkUses(9); println(f"  v={x.id}"); }
fn scalar_arith(i: i64) { let x = mkUses(i + 0); println(f"  v={x.id}"); }
fn scalar_option(i: i64) { let o = mkOpt(i); match o { Option.Some(r) => { println(f"  v={r.id}"); } Option.None => { println("  v=none"); } } }
fn scalar_float(x: f64) { let r = mkFlt(x); println(f"  v={r.id}"); }
fn scalar_char(c: char) { let r = mkChr(c); println(f"  v={r.id}"); }
fn scalar_assoc(i: i64) { let x = H.make(i); println(f"  v={x.id}"); }
fn owned_handback(r: R) { let x = keep(r); println(f"  v={x.id}"); }
fn owned_wrapped(r: R) { let x = wrap(r); println(f"  v={x.r.id}"); }
fn mixed_args(i: i64, r: R) { let x = both(i, r); println(f"  v={x.id}"); }
fn local_scalar() { let n = 31; let x = mkUses(n); println(f"  v={x.id}"); }

fn main() {
    println("scalar_arg"); scalar_arg(1);
    println("scalar_rebound"); scalar_rebound(2);
    println("scalar_chained"); scalar_chained(3);
    println("scalar_unused"); scalar_unused(4);
    println("scalar_interpolated"); scalar_interpolated(5);
    println("scalar_constant"); scalar_constant(6);
    println("scalar_arith"); scalar_arith(8);
    println("scalar_option"); scalar_option(10);
    println("scalar_float"); scalar_float(1.5);
    println("scalar_char"); scalar_char('q');
    println("scalar_assoc"); scalar_assoc(11);
    println("owned_handback"); owned_handback(mkUses(12));
    println("owned_wrapped"); owned_wrapped(mkUses(13));
    println("mixed_args"); mixed_args(14, mkUses(15));
    println("local_scalar"); local_scalar();
    println("end");
}
"#),
        r#"scalar_arg
  v=1
  dR1
scalar_rebound
  v=2
  dR2
scalar_chained
  v=3
  dR3
scalar_unused
  v=0
  dR0
scalar_interpolated
  v=7
  dR7
scalar_constant
  v=9
  dR9
scalar_arith
  v=8
  dR8
scalar_option
  v=10
  dR10
scalar_float
  v=20
  dR20
scalar_char
  v=21
  dR21
scalar_assoc
  v=11
  dR11
owned_handback
  v=12
  dR12
owned_wrapped
  v=13
  dR13
mixed_args
  v=15
  dR15
local_scalar
  v=31
  dR31
end
"#
    );
}

/// B-2026-09-06-58 — the interpreter half, on the same three readers the
/// scalar sibling gated: both classifiers and the whole-alias closure. The
/// `chained` cell is the one that needs the third — `let x = from_string(60, s);
/// let y = hand_back(x)` — exactly as it was for the scalar case.
///
/// Twin of `tests/codegen.rs`'s `e2e_drop_free_argument_does_not_make_the_result_a_view`, pinned to the same string.
#[test]
fn test_drop_free_argument_does_not_make_the_result_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct H { r: R, n: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.n}") } }
struct Q { s: String, n: i64 }
struct W { r: R, n: i64 }
struct N { r: R, n: i64 }

fn from_string(i: i64, s: String) -> R { return R { id: i, name: s }; }
fn from_vec(i: i64, v: Vec[String]) -> R { return R { id: i, name: v[0] }; }
fn from_struct(q: Q) -> R { return R { id: q.n, name: q.s }; }
fn from_two(a: String, b: String) -> R { return R { id: 30, name: a }; }
fn into_option(s: String) -> Option[R] { return Option.Some(R { id: 40, name: s }); }
fn into_bodyless(s: String) -> N { return N { r: R { id: 50, name: s }, n: 1 }; }
fn hand_back(r: R) -> R { return r; }
fn wrap_bodyless(r: R) -> W { return W { r: r, n: 2 }; }
fn wrap_bodied(r: R) -> H { return H { r: r, n: 3 }; }
fn from_scalar(i: i64) -> R { return R { id: i, name: f"s{i}" }; }
fn no_store(i: i64, v: Vec[i64]) -> R { return R { id: i, name: f"n{v.len()}" }; }

fn string_arg(i: i64, nm: String) { let x = from_string(i, nm); println(f"  v={x.id}"); }
fn vec_arg(i: i64, v: Vec[String]) { let x = from_vec(i, v); println(f"  v={x.id}"); }
fn struct_arg(q: Q) { let x = from_struct(q); println(f"  v={x.id}"); }
fn two_string_args(a: String, b: String) { let x = from_two(a, b); println(f"  v={x.id}"); }
fn option_return(s: String) { let o = into_option(s); match o { Option.Some(r) => { println(f"  v={r.id}"); } Option.None => { println("  v=none"); } } }
fn bodyless_return(s: String) { let n = into_bodyless(s); println(f"  v={n.r.id}"); }
fn chained(s: String) { let x = from_string(60, s); let y = hand_back(x); println(f"  v={y.id}"); }
fn same_type(r: R) { let y = hand_back(r); println(f"  v={y.id}"); }
fn wrap_control(r: R) { let w = wrap_bodyless(r); println(f"  v={w.r.id}"); }
fn wrap_bodied_control(r: R) { let h = wrap_bodied(r); println(f"  v={h.r.id}"); }
fn scalar_control(i: i64) { let x = from_scalar(i); println(f"  v={x.id}"); }
fn read_only_arg(i: i64, v: Vec[i64]) { let x = no_store(i, v); println(f"  v={x.id}"); }

fn main() {
    println("string_arg"); string_arg(1, "a");
    println("vec_arg"); vec_arg(2, ["b"]);
    println("struct_arg"); struct_arg(Q { s: "c", n: 3 });
    println("two_string_args"); two_string_args("d", "e");
    println("option_return"); option_return("f");
    println("bodyless_return"); bodyless_return("g");
    println("chained"); chained("h");
    println("same_type"); same_type(R { id: 7, name: "i" });
    println("wrap_control"); wrap_control(R { id: 8, name: "j" });
    println("wrap_bodied_control"); wrap_bodied_control(R { id: 9, name: "k" });
    println("scalar_control"); scalar_control(10);
    println("read_only_arg"); read_only_arg(11, [1, 2]);
    println("end");
}
"#),
        r#"string_arg
  v=1
  dR1
vec_arg
  v=2
  dR2
struct_arg
  v=3
  dR3
two_string_args
  v=30
  dR30
option_return
  v=40
  dR40
bodyless_return
  v=50
  dR50
chained
  v=60
  dR60
same_type
  v=7
  dR7
wrap_control
  v=8
  dR8
wrap_bodied_control
  v=9
  dH3
  dR9
scalar_control
  v=10
  dR10
read_only_arg
  v=11
  dR11
end
"#
    );
}

/// B-2026-09-03-21 — AN `Option` ELEMENT OF A TUPLE **TEMP-LITERAL ARGUMENT**
/// MUST NOT LOSE ITS PAYLOAD'S `Drop` BODY (OR ITS HEAP).
///
/// `takes((mk(1), Option.Some(mk(11))))` ran `dR11` under `--interp` and nothing
/// on jit / build / AUTO_PAR=0, and leaked the payload's heap besides — 22 B over
/// the filing row's probe, which reported only the lost body.
///
/// THE CAUSE IS AN ERASED ELEMENT TYPE, not the by-value tuple param the row
/// blamed. `infer_arg_elem_te` resolved a variant-ctor element through the
/// namers, which yield the bare head `Option` with `generic_args: None`, and
/// every optres consumer — `emit_optres_payload_user_drop_bodies_fn` for the
/// body, `tuple_elem_optres_drop_ok` for the memory — reads `generic_args` and
/// declines outright without them. `enumtemp` is the control that isolates it:
/// a USER-ENUM element in the identical position was always correct, because its
/// walkers key on the NAME, which survives the erasure.
///
/// `localarg` and `localonly` are the controls that put the fault on the
/// ARGUMENT FORM rather than on the param: the same callee reached from a NAMED
/// LOCAL, and the same tuple never passed at all, were both correct throughout.
/// That is why the two fixes the filing row records — one in the param prologue,
/// one in the shared `emit_tuple_elem_drops` optres leg — could not work: the
/// owner is the caller's temp registration.
///
/// `nonetemp` pins `Option.None` (nothing to run), `destr` the destructured
/// spelling of the same call, `errtemp` a `Result` whose payload is a scalar.
///
/// A `Result.Ok` element carrying a Drop payload is covered by
/// `test_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body` below, and by
/// its codegen twin. It was split out (B-2026-09-04-26) on the reading that
/// enriching its type traded a lost body for a 56-byte leak; that reading was
/// an artifact — the leak was already there at `KARAC_OPT_LEVEL=0`, hidden at
/// `-O2` because dead-allocation elimination deletes a `malloc` nothing reads.
/// Filling `E` from the callee's declared parameter type closes both halves.
/// Every cell in this fixture is valgrind-clean ("All heap blocks were freed").
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_optres_ctor_tuple_temp_arg_keeps_its_payload_drop_body`, pinned to the
/// same string.
#[test]
fn test_optres_ctor_tuple_temp_arg_keeps_its_payload_drop_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}/{self.xs.len()}") } }
enum W { A(R), N }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] }; }

fn optArg(t: (R, Option[R]))       -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn resArg(t: (R, Result[R, i64]))  -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn enumArg(t: (R, W))              -> i64 { println(f"  rd{t.0.id}"); return 0; }
fn destr(t: (R, Option[R]))        -> i64 { let (r, o) = t; println(f"  rd{r.id}"); return 0; }

fn main() {
    println("opttemp");  let _ = optArg((mk(1), Option.Some(mk(11))));  println("opttemp end")
    println("errtemp");  let _ = resArg((mk(3), Result.Err(7)));        println("errtemp end")
    println("enumtemp"); let _ = enumArg((mk(4), W.A(mk(44))));         println("enumtemp end")
    println("destr");    let _ = destr((mk(5), Option.Some(mk(55))));   println("destr end")
    println("nonetemp"); let _ = optArg((mk(6), Option.None));          println("nonetemp end")
    println("localarg"); let a = (mk(7), Option.Some(mk(77))); let _ = optArg(a); println("localarg end")
    println("localonly"); let b = (mk(8), Option.Some(mk(88))); println(f"  rd{b.0.id}"); println("localonly end")
    println("done")
}
"#),
        r#"opttemp
  rd1
dR1/t1/1
dR11/t11/1
opttemp end
errtemp
  rd3
dR3/t3/1
errtemp end
enumtemp
  rd4
dR4/t4/1
dR44/t44/1
enumtemp end
destr
  rd5
dR5/t5/1
dR55/t55/1
destr end
nonetemp
  rd6
nonetemp end
localarg
  rd7
dR7/t7/1
dR77/t77/1
localarg end
localonly
  rd8
dR8/t8/1
dR88/t88/1
localonly end
done
"#
    );
}

/// B-2026-09-04-26 — the `Result` head of the sibling above, on the
/// interpreter, which was correct throughout: it is the reference the compiled
/// backends were wrong against, so pinning it is what keeps the two from
/// drifting apart again. Twin of `tests/codegen.rs`'s
/// `e2e_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body`, same program
/// and same string.
#[test]
fn test_result_ctor_tuple_temp_arg_keeps_its_payload_drop_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.xs.len()}") } }
fn mk(k: i64) -> R { return R { id: k, tag: "t", xs: [1, 2, 3] } }
fn resArg(t: (R, Result[R, i64])) { println(f"rd{t.0.id}") }
fn resStr(t: (R, Result[R, String])) { println(f"rs{t.0.id}") }
fn both(a: (R, Result[R, i64]), b: (R, Option[R])) { println(f"bo{a.0.id}{b.0.id}") }
fn main() {
    resArg((mk(2), Result.Ok(mk(22))));
    resArg((mk(3), Result.Ok(mk(33))));
    both((mk(4), Result.Ok(mk(44))), (mk(5), Option.Some(mk(55))));
    resStr((mk(6), Result.Ok(mk(66))));
    resArg((mk(7), Result.Err(9)));
    { let t: (R, Result[R, i64]) = (mk(8), Result.Ok(mk(88))); resArg(t); }
    println("end")
}
"#),
        "rd2\ndR2/3\ndR22/3\nrd3\ndR3/3\ndR33/3\nbo45\ndR5/3\ndR55/3\ndR4/3\ndR44/3\nrs6\ndR6/3\ndR66/3\nrd7\ndR7/3\nrd8\ndR8/3\ndR88/3\nend\n"
    );
}

/// B-2026-09-04-22 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_result_agg_leaf_boxed_payload_by_value_call_keeps_body`, same program
/// and string. The interpreter was the correct reference throughout.
#[test]
fn test_result_agg_leaf_boxed_payload_by_value_call_keeps_body() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
struct HoW { a: R, b: Result[W, String] }
fn eatw(w: W) { println(f"eat{w.id}") }
fn mkw(k: i64) -> W { return W { id: k, x: f"x{k}", y: f"y{k}" } }
fn main() {
    { let t: (R, Result[W, String]) = (R { id: 1 }, Result.Ok(mkw(11))); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("one") }
    { let h: HoW = HoW { a: R { id: 2 }, b: Result.Ok(mkw(22)) }; let HoW { a, b } = h; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("two") }
    { let t: (R, Result[W, String]) = (R { id: 3 }, Result.Ok(mkw(33))); let (a, b) = t; if let Result.Ok(w) = b { eatw(w) } println("three") }
    { let t: (R, Result[W, String]) = (R { id: 4 }, Result.Err("e4")); let (a, b) = t; match b { Result.Ok(w) => eatw(w), Result.Err(e) => println(f"err{e}") } println("four") }
    { let t: (R, Option[W]) = (R { id: 5 }, Option.Some(mkw(55))); let (a, b) = t; match b { Option.Some(w) => eatw(w), Option.None => println("none") } println("five") }
    { let r: Result[W, String] = Result.Ok(mkw(66)); match r { Result.Ok(w) => eatw(w), Result.Err(e) => println("err") } println("six") }
    { let t: (R, Result[W, String]) = (R { id: 7 }, Result.Ok(mkw(77))); let (a, b) = t; if let Result.Ok(w) = b { let g: W = w; println(f"g{g.id}") } println("seven") }
    println("end")
}
"#),
        "dR1\neat11\ndW11/x11y11\none\ndR2\neat22\ndW22/x22y22\ntwo\ndR3\neat33\ndW33/x33y33\nthree\ndR4\nerre4\nfour\ndR5\neat55\ndW55/x55y55\nfive\neat66\ndW66/x66y66\nsix\ndR7\ng77\ndW77/x77y77\nseven\nend\n"
    );
}

/// B-2026-09-04-9 — the INTERPRETER half of
/// `e2e_fresh_tuple_option_binding_leaf_owns_its_payload_body`, pinned to the
/// same string.
///
/// UNUSUALLY FOR THIS FAMILY, this side was not already correct. Two of the
/// fifteen cells were red HERE and green on all three compiled surfaces once
/// the codegen half landed: `nested` and `nestpl` lost their payload's body,
/// because `record_destructure_optres_payload_tes` walked only the top level of
/// a tuple pattern and the element types it reads flattened a nested tuple to
/// `None`. The wildcard sibling of the same nesting was correct through a
/// different path, which is what made the gap look like a binding/wildcard
/// asymmetry rather than a depth one.
///
/// So this file is not merely the invariant here — it is half the regression.
#[test]
fn test_fresh_tuple_option_binding_leaf_owns_its_payload_body() {
    let out = run(r#"struct R { id: i64, s: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}:{self.s}:{self.xs.len()}") } }
struct W { p: Option[R] }
fn mk(n: i64) -> R { return R { id: n, s: f"s{n}", xs: [n, n] }; }
fn eat(xa: Option[R]) -> i64 { match xa { Option.Some(ra) => { ra.id }, Option.None => { 0 } } }
fn give() -> Option[R] { let (_, ob) = (mk(56), Option.Some(mk(156))); return ob }

fn bind()   { let (_, oc) = (mk(32), Option.Some(mk(132))); println("  b") }
fn bindsib(){ let (ad, bd) = (mk(48), Option.Some(mk(148))); println(f"  rd{ad.id}") }
fn slot0()  { let (oe, ke) = (Option.Some(mk(72)), 5); println(f"  k{ke}") }
fn nested() { let ((_, of), nf) = ((mk(2), Option.Some(mk(102))), 3); println(f"  n{nf}") }
fn nestpl() { let tg = ((mk(6), Option.Some(mk(106))), 7); let ((_, og), ng) = tg; println(f"  p{ng}") }
fn loopb()  { let mut i = 0; while i < 2 { let (_, oh) = (mk(64), Option.Some(mk(164))); i = i + 1; } println("  l") }
fn consume(){ let (_, oi) = (mk(52), Option.Some(mk(152))); match oi { Option.Some(ri) => { println(f"  c{ri.id}") }, Option.None => { println("  z") } } }
fn moved()  { let (_, oj) = (mk(54), Option.Some(mk(154))); let qj = oj; println(f"  m{qj.is_some()}") }
fn ret()    { let zk = give(); println(f"  r{zk.is_some()}") }
fn arg()    { let (_, ol) = (mk(58), Option.Some(mk(158))); println(f"  a{eat(ol)}") }
fn field()  { let (_, om) = (mk(76), Option.Some(mk(176))); let wm = W { p: om }; println(f"  f{wm.p.is_some()}") }
fn instr()  { let (_, on) = (mk(60), Option.Some(f"q60")); println("  s") }
fn none()   { let np: Option[R] = Option.None; let (_, op) = (mk(62), np); println("  o") }
fn plainel(){ let (_, oq) = (mk(9), mk(109)); println("  q") }
fn wildcd() { let (_, _) = (mk(31), Option.Some(mk(131))); println("  w") }

fn main() {
  println("bind");    bind()
  println("bindsib"); bindsib()
  println("slot0");   slot0()
  println("nested");  nested()
  println("nestpl");  nestpl()
  println("loopb");   loopb()
  println("consume"); consume()
  println("moved");   moved()
  println("ret");     ret()
  println("arg");     arg()
  println("field");   field()
  println("instr");   instr()
  println("none");    none()
  println("plainel"); plainel()
  println("wildcd");  wildcd()
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"bind
dR32:s32:2
dR132:s132:2
  b
bindsib
dR148:s148:2
  rd48
dR48:s48:2
slot0
dR72:s72:2
  k5
nested
dR2:s2:2
dR102:s102:2
  n3
nestpl
dR6:s6:2
dR106:s106:2
  p7
loopb
dR64:s64:2
dR164:s164:2
dR64:s64:2
dR164:s164:2
  l
consume
dR52:s52:2
  c152
dR152:s152:2
moved
dR54:s54:2
  mtrue
dR154:s154:2
ret
dR56:s56:2
  rtrue
dR156:s156:2
arg
dR58:s58:2
  a158
dR158:s158:2
field
dR76:s76:2
  ftrue
dR176:s176:2
instr
dR60:s60:2
  s
none
dR62:s62:2
  o
plainel
dR9:s9:2
dR109:s109:2
  q
wildcd
dR31:s31:2
dR131:s131:2
  w
done
"#
    );
}

/// B-2026-09-04-1 — a STRUCT-FIELD destructure leaf typed `Result[<struct with
/// a Drop body>, _]` runs the payload's body exactly once, on every surface.
///
/// The row's cell (`loc`): the arm only borrows `r`, so nothing at the arm owned
/// the body, and the leaf's memory action was a body-less struct drop — `dR101`
/// ran under `--interp` and on none of jit/aot/AUTO_PAR=0. `iflet`, `errs`,
/// `noread`, `solo` and `rename` are the same defect in five spellings the row
/// did not record; `unused` is the UNCONSUMED leaf, which lost the body on every
/// backend (the B-2026-09-03-33 deferral, closed here). `fcall` / `flit` /
/// `fcallu` are FRESH sources, whose `Result` field had no owner at all (a leak
/// the optimizer's dead-chain mask hid until a body walk made the words live;
/// `fstru` is the unmasked direct-`String` twin, valgrind: 4 bytes). The `w*`
/// cells are the heap-BOXED payload (seven words), which the inline tracker
/// declined and nothing else owned. `param` is the by-value-param source, whose
/// own field walk already ran the body — unchanged.
///
/// The interpreter twin runs the same string; the ASAN twin is
/// `asan_struct_field_result_leaf_is_balanced`.
///
/// Interpreter twin of `e2e_struct_field_result_leaf_owns_its_payload_body` (tests/codegen.rs) — same program string, same
/// pin.
#[test]
fn test_struct_field_result_leaf_owns_its_payload_body() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct W { id: i64, x: String, y: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.id}/{self.x}{self.y}") } }
fn mkw(n: i64) -> W { return W { id: n, x: f"x{n}", y: f"y{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct SoloRes { b: Result[R, String] }
struct HoStr { a: R, b: Result[String, String] }
struct HoW { a: R, b: Result[W, String] }
fn mkho(n: i64) -> HoRes { return HoRes { a: mk(n), b: Result.Ok(mk(n + 100)) }; }
fn mkhs(n: i64) -> HoStr { return HoStr { a: mk(n), b: Result.Ok(f"s{n + 100}") }; }
fn mkhw(n: i64) -> HoW { return HoW { a: mk(n), b: Result.Ok(mkw(n + 100)) }; }

fn loc()    { let h = HoRes { a: mk(1), b: Result.Ok(mk(101)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn iflet()  { let h = HoRes { a: mk(2), b: Result.Ok(mk(102)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              if let Result.Ok(r) = b { println(f"  ok{r.id}") } }
fn errs()   { let h = HoErr { a: mk(3), b: Result.Err(mk(103)) }; let HoErr { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.id}") } }
fn noread() { let h = HoRes { a: mk(4), b: Result.Ok(mk(104)) }; let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println("  ok"), Result.Err(e) => println(f"  er{e}") } }
fn solo()   { let h = SoloRes { b: Result.Ok(mk(105)) }; let SoloRes { b } = h;
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn rename() { let h = HoRes { a: mk(6), b: Result.Ok(mk(106)) }; let HoRes { a: aa, b: bb } = h; println(f"  rd{aa.id}")
              match bb { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn unused() { let h = HoRes { a: mk(7), b: Result.Ok(mk(107)) }; let HoRes { a, b } = h; println(f"  rd{a.id}") }
fn fcall()  { let HoRes { a, b } = mkho(8); println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn flit()   { let HoRes { a, b } = HoRes { a: mk(9), b: Result.Ok(mk(109)) }; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }
fn fcallu() { let HoRes { a, b } = mkho(10); println(f"  rd{a.id}") }
fn fstru()  { let HoStr { a, b } = mkhs(11); println(f"  rd{a.id}") }
fn fstr()   { let HoStr { a, b } = mkhs(12); println(f"  rd{a.id}")
              match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(e) => println(f"  er{e}") } }
fn wloc()   { let h = HoW { a: mk(13), b: Result.Ok(mkw(113)) }; let HoW { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wunused(){ let h = HoW { a: mk(14), b: Result.Ok(mkw(114)) }; let HoW { a, b } = h; println(f"  rd{a.id}") }
fn wfcall() { let HoW { a, b } = mkhw(15); println(f"  rd{a.id}")
              match b { Result.Ok(w) => println(f"  ok{w.id}"), Result.Err(e) => println(f"  er{e}") } }
fn wfcallu(){ let HoW { a, b } = mkhw(16); println(f"  rd{a.id}") }
fn param()  { take(HoRes { a: mk(17), b: Result.Ok(mk(117)) }) }
fn take(h: HoRes) { let HoRes { a, b } = h; println(f"  rd{a.id}")
              match b { Result.Ok(r) => println(f"  ok{r.id}"), Result.Err(e) => println(f"  er{e}") } }

fn main() {
  println("loc");     loc()
  println("iflet");   iflet()
  println("errs");    errs()
  println("noread");  noread()
  println("solo");    solo()
  println("rename");  rename()
  println("unused");  unused()
  println("fcall");   fcall()
  println("flit");    flit()
  println("fcallu");  fcallu()
  println("fstru");   fstru()
  println("fstr");    fstr()
  println("wloc");    wloc()
  println("wunused"); wunused()
  println("wfcall");  wfcall()
  println("wfcallu"); wfcallu()
  println("param");   param()
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"loc
  rd1
dR1/t1
  ok101
dR101/t101
iflet
  rd2
dR2/t2
  ok102
dR102/t102
errs
  rd3
dR3/t3
  er103
dR103/t103
noread
  rd4
dR4/t4
  ok
dR104/t104
solo
  ok105
dR105/t105
rename
  rd6
dR6/t6
  ok106
dR106/t106
unused
dR107/t107
  rd7
dR7/t7
fcall
  rd8
dR8/t8
  ok108
dR108/t108
flit
  rd9
dR9/t9
  ok109
dR109/t109
fcallu
dR110/t110
  rd10
dR10/t10
fstru
  rd11
dR11/t11
fstr
  rd12
dR12/t12
  oks112
wloc
  rd13
dR13/t13
  ok113
dW113/x113y113
wunused
dW114/x114y114
  rd14
dR14/t14
wfcall
  rd15
dR15/t15
  ok115
dW115/x115y115
wfcallu
dW116/x116y116
  rd16
dR16/t16
param
  rd17
  ok117
dR117/t117
dR17/t17
done
"#
    );
}

/// B-2026-09-04-21 — a struct destructured out of a PROJECTION of an owned local
/// (`let HoRes { a, b } = w.inner;`) hands its `Option` / `Result` leaf the
/// field, on every surface.
///
/// On the parent the leaf was a bit-copy VIEW registered in no set while the
/// root kept the memory, so `rmatch` (a consuming arm on a `Result[R, String]`
/// leaf) freed the payload's String from the arm binding and again from the
/// root's drop — glibc's `free(): double free detected in tcache 2` on an
/// ordinary build — and `rcall` / `resc` / `rrebind` / `two` / `awild` aborted
/// the same way; `unused` lost the body on every backend; `omatch` / `ocall` /
/// `oesc` ran the `Option` payload's body at the ROOT's last use (before the
/// leaf was read, or a second time) and `orebind` crashed silently. The leaf
/// now TRANSFERS the field out of the root (cap-zero in place, the same move the
/// by-value-param destructure makes) when the read is a move, and owns its own
/// defensive copy when the root is read again (`rlive`, `olive`, `livecall`);
/// the root's walk is masked for the field either way, so the body fires at
/// the leaf's last use. `wild` / `errs` are the no-payload controls; `nest`
/// moves the destructure into a block. A by-value-PARAM root is deliberately
/// not on this path (its own walk runs the bodies) and keeps its own row.
///
/// Interpreter twin of `e2e_projection_source_optres_leaf_owns_its_field` (tests/codegen.rs) — same program string, same
/// pin.
#[test]
fn test_projection_source_optres_leaf_owns_its_field() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn unused()  { let w = WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn rmatch()  { let w = WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive()   { let w = WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } }; let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }
fn ounused() { let w = WrapO { inner: HoOpt { a: mk(4), b: Option.Some(mk(104)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }
fn omatch()  { let w = WrapO { inner: HoOpt { a: mk(5), b: Option.Some(mk(105)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive()   { let w = WrapO { inner: HoOpt { a: mk(6), b: Option.Some(mk(106)) } }; let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
               match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
               println(f"  w{w.inner.a.id}") }
fn two()     { let g = Outer { h: WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } } }; let HoRes { a, b } = g.h.inner; println(f"  rd{a.id}")
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall()   { let w = WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn resc()    { let w = WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } }; let HoRes { a, b } = w.inner;
               let g = match b { Result.Ok(r) => r, Result.Err(e) => mk(0) }; println(f"  got{g.id}") }
fn rrebind() { let w = WrapR { inner: HoRes { a: mk(10), b: Result.Ok(mk(110)) } }; let HoRes { a, b } = w.inner; let c = b;
               match c { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn ocall()   { let w = WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } }; let HoOpt { a, b } = w.inner;
               match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn oesc()    { let w = WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } }; let HoOpt { a, b } = w.inner;
               let g = match b { Option.Some(r) => r, Option.None => mk(0) }; println(f"  got{g.id}") }
fn orebind() { let w = WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } }; let HoOpt { a, b } = w.inner; let c = b;
               match c { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn wild()    { let w = WrapR { inner: HoRes { a: mk(14), b: Result.Ok(mk(114)) } }; let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild()   { let w = WrapR { inner: HoRes { a: mk(15), b: Result.Ok(mk(115)) } }; let HoRes { a: _, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn nest()    { let w = WrapR { inner: HoRes { a: mk(16), b: Result.Ok(mk(116)) } };
               { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
               println("  outer") }
fn errs()    { let w = WrapR { inner: HoRes { a: mk(17), b: Result.Err("e17") } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(){ let w = WrapR { inner: HoRes { a: mk(18), b: Result.Ok(mk(118)) } }; let HoRes { a, b } = w.inner;
               match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
               println(f"  w{w.inner.a.id}") }

fn main() {
  println("unused");   unused()
  println("rmatch");   rmatch()
  println("rlive");    rlive()
  println("ounused");  ounused()
  println("omatch");   omatch()
  println("olive");    olive()
  println("two");      two()
  println("rcall");    rcall()
  println("resc");     resc()
  println("rrebind");  rrebind()
  println("ocall");    ocall()
  println("oesc");     oesc()
  println("orebind");  orebind()
  println("wild");     wild()
  println("awild");    awild()
  println("nest");     nest()
  println("errs");     errs()
  println("livecall"); livecall()
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"unused
dR101/t101
  rd1
dR1/t1
rmatch
  rd2
dR2/t2
  okt102
dR102/t102
rlive
  rd3
dR3/t3
  okt103
dR103/t103
  w3
ounused
dR104/t104
  rd4
dR4/t4
omatch
  rd5
dR5/t5
  okt105
dR105/t105
olive
  rd6
dR6/t6
  okt106
dR106/t106
  w6
two
  rd7
dR7/t7
  okt107
dR107/t107
rcall
dR8/t8
  eat108
dR108/t108
resc
dR9/t9
  got109
dR109/t109
rrebind
dR10/t10
  okt110
dR110/t110
ocall
dR11/t11
  eat111
dR111/t111
oesc
dR12/t12
  got112
dR112/t112
orebind
dR13/t13
  okt113
dR113/t113
wild
dR114/t114
  rd14
dR14/t14
awild
dR15/t15
  okt115
dR115/t115
nest
dR116/t116
  rd16
dR16/t16
  outer
errs
dR17/t17
  ere17
livecall
dR18/t18
  eat118
dR118/t118
  w18
done
"#
    );
}

/// B-2026-09-04-25 — a struct destructured out of a PROJECTION of a by-value
/// PARAM (`fn f(w: WrapR) { let HoRes { a, b } = w.inner; .. }`) hands each
/// `Option` / `Result` leaf the field, on every surface.
///
/// On the parent the leaf was a view of the param's storage registered in no
/// set, while the param's own `StructDrop` still owned the field: `rmatch` ran
/// the payload's body twice on aot and aborted with glibc's `free(): double free
/// detected in tcache 2` on jit; `rlive` ran it twice around the later read;
/// `rcall` ran it from the arm binding and again from `eat`'s callee copy; and
/// under `--interp`, `wild` / `awild` ran a discarded field's body at the
/// destructure and again at the param's exit. The projection now takes the
/// identifier source's transfer (`sfld.move` into the param in place, the leaf
/// owning the field) and its leaves are param views (`mark_views`), so a
/// consuming arm's binding takes the memory-only struct drop its identifier
/// twin takes; the interpreter's discard walk defers a projected wildcard to
/// the param's drop as it already did for `let HoRes { a, b: _ } = h`. `two`
/// is the two-hop root, `errs` the `Err` side, `runused` / `ounused` the unread
/// leaf, `livecall` a moving arm with the param read again. A rebind of the
/// leaf and a `self` receiver split identically for the identifier source and
/// keep their own row.
///
/// Interpreter twin of `e2e_param_projection_optres_leaf_owns_its_field` (tests/codegen.rs) — same program string, same
/// pin.
#[test]
fn test_param_projection_optres_leaf_owns_its_field() {
    let out = run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
fn mk(n: i64) -> R { return R { id: n, tag: f"t{n}" }; }
struct HoRes { a: R, b: Result[R, String] }
struct HoErr { a: R, b: Result[String, R] }
struct HoOpt { a: R, b: Option[R] }
struct WrapR { inner: HoRes }
struct WrapE { inner: HoErr }
struct WrapO { inner: HoOpt }
struct Outer { h: WrapR }
fn eat(x: R) { println(f"  eat{x.id}") }

fn rmatch(w: WrapR)  { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rlive(w: WrapR)   { let HoRes { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn two(o: Outer)     { let HoRes { a, b } = o.h.inner; println(f"  rd{a.id}")
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn rcall(w: WrapR)   { let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") } }
fn runused(w: WrapR) { let HoRes { a, b } = w.inner; println(f"  rd{a.id}") }
fn errs(w: WrapE)    { let HoErr { a, b } = w.inner;
                       match b { Result.Ok(s) => println(f"  ok{s}"), Result.Err(r) => println(f"  er{r.tag}") } }
fn wild(w: WrapR)    { let HoRes { a, b: _ } = w.inner; println(f"  rd{a.id}") }
fn awild(w: WrapR)   { let HoRes { a: _, b } = w.inner;
                       match b { Result.Ok(r) => println(f"  ok{r.tag}"), Result.Err(e) => println(f"  er{e}") } }
fn livecall(w: WrapR){ let HoRes { a, b } = w.inner;
                       match b { Result.Ok(r) => eat(r), Result.Err(e) => println(f"  er{e}") }
                       println(f"  w{w.inner.a.id}") }
fn omatch(w: WrapO)  { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") } }
fn olive(w: WrapO)   { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}")
                       match b { Option.Some(r) => println(f"  ok{r.tag}"), Option.None => println("  none") }
                       println(f"  w{w.inner.a.id}") }
fn ocall(w: WrapO)   { let HoOpt { a, b } = w.inner;
                       match b { Option.Some(r) => eat(r), Option.None => println("  none") } }
fn ounused(w: WrapO) { let HoOpt { a, b } = w.inner; println(f"  rd{a.id}") }

fn main() {
  println("rmatch");   rmatch(WrapR { inner: HoRes { a: mk(1), b: Result.Ok(mk(101)) } })
  println("rlive");    rlive(WrapR { inner: HoRes { a: mk(2), b: Result.Ok(mk(102)) } })
  println("two");      two(Outer { h: WrapR { inner: HoRes { a: mk(3), b: Result.Ok(mk(103)) } } })
  println("rcall");    rcall(WrapR { inner: HoRes { a: mk(4), b: Result.Ok(mk(104)) } })
  println("runused");  runused(WrapR { inner: HoRes { a: mk(5), b: Result.Ok(mk(105)) } })
  println("errs");     errs(WrapE { inner: HoErr { a: mk(6), b: Result.Err(mk(106)) } })
  println("wild");     wild(WrapR { inner: HoRes { a: mk(7), b: Result.Ok(mk(107)) } })
  println("awild");    awild(WrapR { inner: HoRes { a: mk(8), b: Result.Ok(mk(108)) } })
  println("livecall"); livecall(WrapR { inner: HoRes { a: mk(9), b: Result.Ok(mk(109)) } })
  println("omatch");   omatch(WrapO { inner: HoOpt { a: mk(10), b: Option.Some(mk(110)) } })
  println("olive");    olive(WrapO { inner: HoOpt { a: mk(11), b: Option.Some(mk(111)) } })
  println("ocall");    ocall(WrapO { inner: HoOpt { a: mk(12), b: Option.Some(mk(112)) } })
  println("ounused");  ounused(WrapO { inner: HoOpt { a: mk(13), b: Option.Some(mk(113)) } })
  println("done")
}
"#);
    assert_eq!(
        out,
        r#"rmatch
  rd1
  okt101
dR101/t101
dR1/t1
rlive
  rd2
  okt102
  w2
dR102/t102
dR2/t2
two
  rd3
  okt103
dR103/t103
dR3/t3
rcall
  eat104
dR104/t104
dR4/t4
runused
  rd5
dR105/t105
dR5/t5
errs
  ert106
dR106/t106
dR6/t6
wild
  rd7
dR107/t107
dR7/t7
awild
  okt108
dR108/t108
dR8/t8
livecall
  eat109
  w9
dR109/t109
dR9/t9
omatch
  rd10
  okt110
dR110/t110
dR10/t10
olive
  rd11
  okt111
  w11
dR111/t111
dR11/t11
ocall
  eat112
dR112/t112
dR12/t12
ounused
  rd13
dR113/t113
dR13/t13
done
"#
    );
}

/// B-2026-09-17-37 — A NAMED-LOCAL `Option`/`Result` ARGUMENT NO LONGER
/// DOUBLES THE `Drop` BODY OF A PART THE CALLEE CONSUMES.
///
/// The INTERPRETER side of this was already correct — B-2026-09-14-7 gave it
/// the `(binding, path)` mask that the compiled fix is now the twin of, read
/// from the SAME predicate (`fn_consumed_param_payload_part_paths`) so the two
/// ends of one call cannot compute "did the callee consume this part"
/// separately and drift into a lost body or a doubled one.
///
/// What this pins is that the correct half stays put while the compiled half
/// moves to meet it. `mixed` is the cell to read first: it consumes one tuple
/// element and RETURNS the other, and its `dR5 got5 dR5` is an agreed double on
/// both backends that the compiled fix deliberately does not touch.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_named_optres_arg_does_not_double_a_consumed_part_body`, byte-identical
/// source and expectation.
#[test]
fn test_named_optres_arg_does_not_double_a_consumed_part_body() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct Hd { n: i64 }
impl Hd { fn eat(ref self, o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } } }
struct Snk { n: i64 }
impl Snk { fn eat(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } } }
fn eat(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } }
fn eatr(o: Result[(R, i64), i64]) { match o { Result.Ok(t) => { let x = t.0; println("  mid"); } Result.Err(e) => { println("  n"); } } }
fn eat1(o: Option[(i64, R)]) { match o { Option.Some(t) => { let x = t.1; println("  mid"); } Option.None => { println("  n"); } } }
fn eat2(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); } Option.None => { println("  n"); } } }
fn peek(o: Option[(R, i64)]) { match o { Option.Some(t) => { println(f"  mid{t.1}"); } Option.None => { println("  n"); } } }
fn hands(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { let y = t.1; println("  mid"); return t.0; } Option.None => { return R { id: 0 }; } } }
fn main() {
    println("named");    { let a = Option.Some((R { id: 5 }, 9)); eat(a); } println("  out")
    println("temp");     { eat(Option.Some((R { id: 5 }, 9))); } println("  out")
    println("result");   { let a: Result[(R, i64), i64] = Result.Ok((R { id: 5 }, 9)); eatr(a); } println("  out")
    println("method");   { let h = Hd { n: 1 }; let a = Option.Some((R { id: 5 }, 9)); h.eat(a); } println("  out")
    println("assoc");    { let a = Option.Some((R { id: 5 }, 9)); Snk.eat(a); } println("  out")
    println("second");   { let a = Option.Some((9, R { id: 5 })); eat1(a); } println("  out")
    println("sibling");  { let a = Option.Some((R { id: 5 }, R { id: 6 })); eat2(a); } println("  out")
    println("nomove");   { let a = Option.Some((R { id: 5 }, 9)); peek(a); } println("  out")
    println("mixed");    { let a = Option.Some((R { id: 5 }, R { id: 6 })); let r = hands(a); println(f"  got{r.id}"); } println("  out")
    println("end")
}
"#);
    assert_eq!(out, "named\n  dR5\n  mid\n  out\ntemp\n  dR5\n  mid\n  out\nresult\n  dR5\n  mid\n  out\nmethod\n  dR5\n  mid\n  out\nassoc\n  dR5\n  mid\n  out\nsecond\n  dR5\n  mid\n  out\nsibling\n  dR5\n  mid\n  dR6\n  out\nnomove\n  mid9\n  dR5\n  out\nmixed\n  dR6\n  mid\n  dR5\n  got5\n  dR5\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-13-5 — the interpreter ran a payload part's `Drop` body TWICE
/// when an arm bound a by-value `Option`/`Result` payload WHOLE and returned
/// only a PROJECTION of it.
///
/// `fn eat(o: Option[(R, i64)]) -> R { match o { Some(t) => return t.0, .. } }`
/// printed `dR5 got:5 dR5 end` against a due `got:5 dR5 end` — one owner, one
/// body — while the three compiled surfaces were correct on that cell. The
/// value is MOVED into the caller's binding, so the caller's fresh-temp
/// argument walk must not also run it.
///
/// THE DIRECTION IS THE REVERSE of the neighbouring rows (B-2026-09-13-3,
/// B-2026-09-12-15, B-2026-09-09-18, B-2026-09-12-17 are all "compiled loses a
/// body"), which is why the fix is PART-PRECISE rather than a stand-down:
/// suppressing the argument's payload walk would lose a sibling part's only
/// body, and cells 2/3/5/8 are the ones that would show it — each has an
/// unmoved `Drop`-bearing part whose body is still owed at the payload's
/// death.
///
/// CELLS 9-13 ARE THE GUARDS, and each pins a reason the new predicate
/// declines:
///
/// * 9 — whole binding returned: the pre-existing whole-payload gate's arm,
///   which this channel must not answer a second time.
/// * 10 — a SCALAR leaf (`t.0.id`): masking it would hand the enclosing
///   struct's own body a hole to read, so the consumer's `value_leaf_can_own`
///   gate declines and the body stays.
/// * 11 — a scalar SIBLING read (`t.1` over `(R, i64)`): nothing owns-worthy
///   escapes at all.
/// * 12 — the nested-destructure spelling (`Some((a, b)) => return a`), which
///   binds the part directly and was already correct through the tuple-arm
///   channel.
/// * 13 — a CONDITIONAL escape on the branch that does NOT take it. This is
///   the one that made the scan record only at the arm's own statement level:
///   a union over branches would mask `t.r` on the `k = false` run and lose
///   `dR5`, where this backend is correct today.
///
/// NOT A LEAK and no sanitizer can see any of it — a `Drop` body frees nothing,
/// so the whole family is body-only (the row records `0 errors`, `0 bytes` at
/// `-O0` on every cell).
///
/// A NAMED-LOCAL argument is deliberately NOT here: it is not a fresh temp, so
/// it never reaches this walk, and it doubles on all four surfaces alike
/// (B-2026-09-14-6, open) — fixing it here would create a divergence out of an
/// agreed answer.
#[test]
fn test_optres_arg_payload_projection_runs_each_part_body_once() {
    const R: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n";
    for (label, src, want) in [
        // 1 — the row's headline cell.
        (
            "tuple-elem-returned",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        // 2 — a `Drop`-bearing SIBLING: element 1 never moves, so its body is
        //     owed at the payload's death and the mask must keep it.
        (
            "tuple-two-droppers-first-returned",
            format!(
                "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.0; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "dR6\ngot:5\ndR5\nend\n",
        ),
        // 3 — the other element, which also pins that the mask is per-PATH and
        //     not "the first one".
        (
            "tuple-two-droppers-second-returned",
            format!(
                "{R}fn eat(o: Option[(R, R)]) -> R {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "dR5\ngot:6\ndR6\nend\n",
        ),
        // 4 — a STRUCT payload's field, the row's second headline spelling. The
        //     holder declares no `Drop` of its own, which is what makes the
        //     partial move legal (`partial_move_of_drop_struct` otherwise).
        (
            "struct-field-returned",
            format!(
                "{R}struct Hd {{ r: R, n: i64 }}\n\
                 fn eat(o: Option[Hd]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some(Hd {{ r: R {{ id: 5 }}, n: 9i64 }})); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        // 5 — the struct sibling of cell 2: a second `Drop`-bearing field that
        //     stays behind.
        (
            "struct-field-returned-dropping-sibling",
            format!(
                "{R}struct Hd2 {{ r: R, q: R }}\n\
                 fn eat(o: Option[Hd2]) -> R {{ match o {{ Some(t) => {{ return t.r; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some(Hd2 {{ r: R {{ id: 5 }}, q: R {{ id: 6 }} }})); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "dR6\ngot:5\ndR5\nend\n",
        ),
        // 6 — `Result`, the other seeded head.
        (
            "result-payload-elem-returned",
            format!(
                "{R}fn eat(o: Result[(R, i64), i64]) -> R {{ match o {{ Ok(t) => {{ return t.0; }} Err(e) => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Ok((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        // 7 — `if let`, and the TAIL spelling (no `return`), which is a yield
        //     site only because the `match` sits in the function's tail.
        (
            "if-let-spelling",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> R {{ if let Some(t) = o {{ return t.0; }} return R {{ id: 0 }}; }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        (
            "arm-tail-no-return",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some(t) => {{ t.0 }} None => {{ R {{ id: 0 }} }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        // 8 — a NESTED projection (`t.0.1`): the mask steps through the tuple
        //     hop and element 0's unmoved sibling keeps its body.
        (
            "nested-projection",
            format!(
                "{R}fn eat(o: Option[((R, R), i64)]) -> R {{ match o {{ Some(t) => {{ return t.0.1; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some(((R {{ id: 5 }}, R {{ id: 6 }}), 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "dR5\ngot:6\ndR6\nend\n",
        ),
        // 9 — GUARD: the WHOLE binding returned. The pre-existing whole-payload
        //     gate stands the argument down here; the new channel reports
        //     nothing so the walk is not masked twice.
        (
            "guard-whole-binding-returned",
            format!(
                "{R}fn eat(o: Option[(R, R)]) -> (R, R) {{ match o {{ Some(t) => {{ return t; }} None => {{ return (R {{ id: 0 }}, R {{ id: 1 }}); }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, R {{ id: 6 }}))); println(f\"got:{{got.0.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\ndR6\nend\n",
        ),
        // 10 — GUARD: a SCALAR leaf reached THROUGH the `Drop`-bearing element.
        //      Nothing owns-worthy escapes, and masking the path would leave
        //      `R`'s own body reading a struct with its field removed.
        (
            "guard-scalar-leaf-through-dropper",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.0.id; }} None => {{ return 0i64; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
            ),
            "dR5\ngot:5\nend\n",
        ),
        // 11 — GUARD: a scalar SIBLING read, the cell B-2026-09-14-5 fixed on
        //      the compiled side. Both backends owe the body here.
        (
            "guard-scalar-sibling-read",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> i64 {{ match o {{ Some(t) => {{ return t.1; }} None => {{ return 0i64; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got}}\"); println(\"end\") }}\n"
            ),
            "dR5\ngot:9\nend\n",
        ),
        // 12 — GUARD: the nested-destructure spelling, correct before this
        //      change through the tuple-arm channel and unmoved by it.
        (
            "guard-nested-destructure",
            format!(
                "{R}fn eat(o: Option[(R, i64)]) -> R {{ match o {{ Some((a, b)) => {{ return a; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some((R {{ id: 5 }}, 9i64))); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "got:5\ndR5\nend\n",
        ),
        // 13 — GUARD: a CONDITIONAL escape, on the run that does NOT take it.
        //      The scan records only at the arm's own statement level exactly
        //      so this keeps its body; a union over branches loses `dR5` here.
        (
            "guard-conditional-escape-not-taken",
            format!(
                "{R}struct Hd3 {{ r: R, n: i64 }}\n\
                 fn eat(o: Option[Hd3], k: bool) -> R {{ match o {{ Some(t) => {{ if k {{ return t.r; }} return R {{ id: 1 }}; }} None => {{ return R {{ id: 0 }}; }} }} }}\n\
                 fn main() {{ let got = eat(Some(Hd3 {{ r: R {{ id: 5 }}, n: 9i64 }}), false); println(f\"got:{{got.id}}\"); println(\"end\") }}\n"
            ),
            "dR5\ngot:1\ndR1\nend\n",
        ),
    ] {
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-19-36 — the remainder of B-2026-09-19-21, one `Option` layer
/// deeper. A generic callee that wraps its boxed payload parameter in
/// `Option.Some` before placing it in the returned aggregate
/// (`return Ho { g: Option.Some(g) }`) left two owners on one box:
/// `free(): double free detected in tcache 2` where `--interp` printed `mx 10`.
///
/// TWO gates declined it, not one, which is why the earlier fix did not reach
/// it. `fn_returns_param` decides whether the argument is even a hand-back
/// CANDIDATE, and its `expr_is_ident` recognized a bare identifier, a struct
/// literal and a tuple — but not a variant constructor, so `Option.Some(g)`
/// answered `false` and the disarm was never attempted. Past that,
/// `collect_handback_box_words` looked only for a leaf whose TYPE equals the
/// argument slot's; `coerce_to_payload_words` decomposes the argument's
/// envelope into the OUTER envelope's payload words, so the box survives as a
/// plain `i64` at some index and no leaf carries that type any more.
///
/// Naming `Option` and `Result` in the predicate is COMPLETE rather than a
/// shortcut: `E_ENUM_NESTED_ENUM_PAYLOAD` rejects a user enum with a plain
/// enum payload outright, so those two are the only wrappers a plain generic
/// enum can reach.
///
/// `optF` / `resF` / `bareF` are the dies-inside legs and carry the weight
/// here, because a WIDER disarm is the direction that strands boxes: the
/// callee keeps the argument and returns a payload-free variant, so the
/// caller must still free its own. `discard` hands the result to nobody.
/// `diesin` never wraps at all. `optAll` is the all-paths spelling, whose
/// argument was already disarmed statically and which this must not disturb.
/// `bareT` answers the row's own open question — a bare `Option[G1[T]]`
/// return with no surrounding struct had the same double free, and the same
/// fix reaches it.
///
/// Measured per cell against the parent tree: `optT`, `resT` and `bareT`
/// aborted at exit 134 with one `Invalid free()` each and now exit 0; no
/// negative cell moved. As with B-2026-09-19-21, the three repaired cells
/// trade the double free for a 24-byte leak, and `optAll` is the proof that
/// leak pre-dates the change — it is the one cell already disarmed on the
/// parent tree and the one cell that already leaked there. B-2026-09-19-35
/// owns that missing drop.
///
/// Byte-identical to the codegen twin, which is the assertion.
#[test]
fn test_option_wrapped_handback_leaves_one_owner_on_the_payload_box() {
    let out = run(r#"enum G1[T] { Y(T), N }
struct Ho[T] { g: Option[G1[T]] }
struct Hr[T] { g: Result[G1[T], i64] }

fn optW[T](g: G1[T], c: bool) -> Ho[T] { if c { return Ho { g: Option.Some(g) } } return Ho { g: Option.None }; }
fn optAll[T](g: G1[T]) -> Ho[T] { return Ho { g: Option.Some(g) }; }
fn resW[T](g: G1[T], c: bool) -> Hr[T] { if c { return Hr { g: Result.Ok(g) } } return Hr { g: Result.Err(7) }; }
fn optBare[T](g: G1[T], c: bool) -> Option[G1[T]] { if c { return Option.Some(g) } return Option.None; }
fn eats[T](g: G1[T], c: bool) -> i64 { match g { G1.Y(v) => { return 1; } G1.N => { return 0; } } }

fn shwO(o: Option[G1[String]]) { match o { Option.Some(i) => { match i { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } } Option.None => { println("  none") } } }
fn shwR(r: Result[G1[String], i64]) { match r { Result.Ok(i) => { match i { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } } Result.Err(e) => { println("  err") } } }

fn main() {
    println("optT");     { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let h = optW(g, true); shwO(h.g) }
    println("optF");     { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let h = optW(g, false); shwO(h.g) }
    println("optAll");   { let g: G1[String] = G1.Y(f"aaaaaaaa-3"); let h = optAll(g); shwO(h.g) }
    println("resT");     { let g: G1[String] = G1.Y(f"aaaaaaaa-4"); let h = resW(g, true); shwR(h.g) }
    println("resF");     { let g: G1[String] = G1.Y(f"aaaaaaaa-5"); let h = resW(g, false); shwR(h.g) }
    println("bareT");    { let g: G1[String] = G1.Y(f"aaaaaaaa-6"); let o = optBare(g, true); shwO(o) }
    println("bareF");    { let g: G1[String] = G1.Y(f"aaaaaaaa-7"); let o = optBare(g, false); shwO(o) }
    println("diesin");   { let g: G1[String] = G1.Y(f"aaaaaaaa-8"); let n = eats(g, true); println(f"  e{n}") }
    println("discard");  { let g: G1[String] = G1.Y(f"aaaaaaaa-9"); optW(g, true); println("  x") }
    println("end");
}
"#);
    assert_eq!(out, "optT\n  mx 10\noptF\n  none\noptAll\n  mx 10\nresT\n  mx 10\nresF\n  err\nbareT\n  mx 10\nbareF\n  none\ndiesin\n  e1\ndiscard\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-17-38 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_optres_payload_sibling_part_keeps_its_body_when_its_peer_is_consumed`.
/// The same fourteen shapes in one program; the interpreter was the surface
/// that was already RIGHT on every one of them, so this output is unchanged by
/// the commit. That is exactly what the twin is here to hold: the fix is
/// codegen-only, and a later change that "fixes" the interpreter into
/// agreement with the old compiled answer would lose element 1's body on both
/// backends at once, where no A/B rule could see it.
#[test]
fn test_optres_payload_sibling_part_keeps_its_body_when_its_peer_is_consumed() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { r: R, q: R }
struct H { n: i64 }
impl H { fn eat(ref self, o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } } }

fn eat(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } }
fn eat1(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.1; println("  mid") } Option.None => { println("  n") } } }
fn eatr(o: Result[(R, R), i64]) { match o { Result.Ok(t) => { let x = t.0; println("  mid") } Result.Err(e) => { println("  n") } } }
fn eat3(o: Option[(R, R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } }
fn eatif(o: Option[(R, R)]) { if let Option.Some(t) = o { let x = t.0; println("  mid") } }
fn later(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println("  mid"); println(f"  v{x.id}") } Option.None => { println("  n") } } }
fn nomove(o: Option[(R, R)]) { match o { Option.Some(t) => { println("  mid") } Option.None => { println("  n") } } }
fn scalar(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } }
fn both(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; let y = t.1; println("  mid") } Option.None => { println("  n") } } }
fn hands(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } }
fn named(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
fn mk() -> (R, R) { (R { id: 5 }, R { id: 6 }) }

fn main() {
    println("row");      { eat(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("second");   { eat1(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("result");   { eatr(Result.Ok((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("method");   { let h = H { n: 1 }; h.eat(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("three");    { eat3(Option.Some((R { id: 5 }, R { id: 6 }, R { id: 7 }))) } println("  out")
    println("iflet");    { eatif(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("call-arg"); { eat(Option.Some(mk())) } println("  out")
    println("later");    { later(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("ctl-named-arg"); { let a = Option.Some((R { id: 5 }, R { id: 6 })); eat(a) } println("  out")
    println("ctl-nomove"); { nomove(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("ctl-scalar"); { scalar(Option.Some((R { id: 5 }, 9))) } println("  out")
    println("ctl-both");   { both(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
    println("ctl-escape"); { let g = hands(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
    println("ctl-struct"); { named(Option.Some(P { r: R { id: 5 }, q: R { id: 6 } })) } println("  out")
    println("end")
}
"#);
    assert_eq!(out, "row\n  dR5\n  mid\n  dR6\n  out\nsecond\n  dR6\n  mid\n  dR5\n  out\nresult\n  dR5\n  mid\n  dR6\n  out\nmethod\n  dR5\n  mid\n  dR6\n  out\nthree\n  dR5\n  mid\n  dR6\n  dR7\n  out\niflet\n  dR5\n  mid\n  dR6\n  out\ncall-arg\n  dR5\n  mid\n  dR6\n  out\nlater\n  mid\n  v5\n  dR5\n  dR6\n  out\nctl-named-arg\n  dR5\n  mid\n  dR6\n  out\nctl-nomove\n  mid\n  dR5\n  dR6\n  out\nctl-scalar\n  dR5\n  mid\n  out\nctl-both\n  dR5\n  dR6\n  mid\n  out\nctl-escape\n  dR6\n  got5\n  dR5\n  out\nctl-struct\n  dR5\n  mid\n  dR6\n  out\nend\n", "got:\n{out}");
}
