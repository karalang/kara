//! iterators, ranges, collect -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter iter_range::
//!
//! New fixtures about iterators, ranges, collect belong in this file.

use super::*;

#[test]
fn test_vec_deque_iter_yields_in_front_to_back_order() {
    // `iter()` must yield items front-to-back. The runtime is
    // `Value::Array` so iter routes through the existing eager-
    // snapshot Iterator path; this pins the order against shape
    // changes.
    let out = run(r#"
        fn main() {
            let mut q: VecDeque[i64] = VecDeque.new();
            q.push_back(20);
            q.push_back(30);
            q.push_front(10);
            for x in q.iter() {
                println(x);
            }
        }
    "#);
    assert_eq!(out, "10\n20\n30\n");
}

// ── range values in index position ─────────────────────────────

/// B-2026-08-18-3 — `let r = 1..3; v[r]` panicked with an internal
/// `unreachable!()`: "index expression at 4:13: obj=Value::Array,
/// index=Value::Iterator". The index path read its bounds SYNTACTICALLY off an
/// `ExprKind::Range`, so a range arriving through a binding never entered it,
/// and an internal-error backtrace was the answer to a program `karac check`
/// had just passed.
///
/// The typechecker was right throughout — `1..3` is `Range[i64]`, `v[r]` is
/// `Slice[i64]`, and a non-contiguous iterator index is rejected with a span —
/// so the fix belonged in the evaluator, not in a new rejection.
#[test]
fn test_index_by_a_let_bound_range() {
    // The row's repro, and the inline spelling it must agree with.
    assert_eq!(
        run(
            "fn main() { let v = [10, 20, 30, 40]; let r = 1..3; let s = v[r]; println(s.len()); }"
        ),
        "2\n"
    );
    assert_eq!(
        run("fn main() { let v = [10, 20, 30, 40]; let s = v[1..3]; println(s.len()); }"),
        "2\n"
    );
    // The slice is the right WINDOW, not merely the right length.
    assert_eq!(
        run("fn main() { let v = [10, 20, 30, 40]; let r = 1..3; let s = v[r]; println(s[0]); println(s[1]); }"),
        "20\n30\n"
    );
    // Inclusive, and empty.
    assert_eq!(
        run("fn main() { let v = [10, 20, 30, 40]; let r = 1..=2; let s = v[r]; println(s.len()); println(s[1]); }"),
        "2\n30\n"
    );
    assert_eq!(
        run(
            "fn main() { let v = [10, 20, 30, 40]; let r = 2..2; let s = v[r]; println(s.len()); }"
        ),
        "0\n"
    );
}

// B-2026-09-03-9 — field-held shared structs DO fire now, on every surface.
// The note that stood here said they did not: a shared value held in a field
// is never an env binding, so it never reached a `CleanupAction::Drop` drain
// and `drop_target` (which reports a refcount only for a BARE `SharedStruct`
// slot) could not see it. The release is now performed against the holder
// instead — see `run_field_held_shared_user_drops`. What is still open is the
// TUPLE-ELEMENT spelling, and it is open on the COMPILED side: both compiled
// backends release the element before the tuple's first read, so the fix there
// is not the interpreter's. `Option[shared]` payloads are likewise untouched.

/// B-2026-09-03-9 — a `shared struct` in a struct FIELD runs its body ONCE,
/// at the holder's scope exit.
///
/// It ran ZERO times before: the plain field walk skips shared fields (their
/// drop is refcount-driven and it has no refcount to consult) and the
/// refcount path is keyed on a named binding, which a field-held value has
/// not got. Both compiled backends ran it exactly once, so this was a
/// run-vs-build divergence on the shipping side.
///
/// The trailing `post` is load-bearing: it pins WHERE the release lands.
/// B-2026-09-04-32 moved that from scope exit to the holder's LIVE-RANGE END —
/// `a`'s last use is `a.s.id`, so `dS14` belongs before `post`, not after it —
/// and moved both backends together. design.md § Drop ordering within a branch
/// names RC decrements explicitly and excludes the scope-exit stack for a
/// mid-branch last use; the old placement had ONE binding ending in two places.
#[test]
fn test_user_drop_field_held_shared_struct_fires_once_at_its_live_range_end() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
         struct Sw { s: S }\n\
         fn build(k: i64) -> Sw {\n\
             let s: S = S { id: k };\n\
             println(\"mid\");\n\
             return Sw { s: s }\n\
         }\n\
         fn main() {\n\
             let a: Sw = build(14);\n\
             println(f\"v{a.s.id}\");\n\
             println(\"post\");\n\
         }",
    );
    assert_eq!(output.concat(), "mid\nv14\ndS14\npost\n");
}

/// B-2026-09-03-9, corrected by B-2026-09-04-32 — the two halves of one holder
/// land in the SAME place, and all four surfaces agree on it.
///
/// This test used to assert the opposite, in its name as well as its
/// expectation: `Mx { r: R, s: S }` ran the plain field's body at the binding's
/// live-range end (`dR1` before `post`) and released the shared field at scope
/// exit (`dS2` after it). That split was not a backend divergence — every
/// surface agreed — but a joint departure from design.md § Drop ordering within
/// a branch, which puts an RC decrement at "each binding's live-range end, not
/// lexical scope end" and says a mid-branch last use "does not appear in the
/// end-of-branch cleanup stack at all". ONE BINDING CANNOT HAVE TWO LIVE-RANGE
/// ENDS, and a bare `shared` binding already fired at its own, which is what
/// made the aggregate the outlier rather than the rule.
///
/// So the interleaving is still what is asserted, not just the bodies — it is
/// simply the interleaving the spec names: `dR1` then `dS2`, both before
/// `post`.
#[test]
fn test_user_drop_field_held_shared_release_lands_at_its_live_range_end() {
    let (output, _drops) = run_program_with_drops(
        "shared struct S { id: i64 }\n\
         impl Drop for S { fn drop(mut ref self) { println(f\"dS{self.id}\"); } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\"); } }\n\
         struct Mx { r: R, s: S }\n\
         fn main() {\n\
             let a: Mx = Mx { r: R { id: 1 }, s: S { id: 2 } };\n\
             println(f\"v{a.s.id}\");\n\
             println(\"post\");\n\
         }",
    );
    assert_eq!(output.concat(), "v2\ndR1\ndS2\npost\n");
}

#[test]
fn test_nll_defer_referencing_binding_extends_live_range() {
    // Per design.md § Drop ordering within a branch: a defer body
    // that references a binding extends the binding's live range to
    // scope exit. The defer fires first under LIFO, with the binding
    // still alive; Drop fires after.
    let drops = drops_in(
        "fn main() {\n\
             let x = 7;\n\
             defer { println(x); }\n\
             println(\"middle\");\n\
         }",
    );
    // `x` is referenced in the defer body, so its last_use is the
    // sentinel. Drop(x) drains at scope exit, after the defer body.
    assert_eq!(drops, vec!["x"]);
}

#[test]
fn collect_all_vec_gathers_all_results_without_fail_fast() {
    // Phase 6 slice 1a — `collect_all_vec` runs EVERY closure to
    // completion and returns one Result per input, position-bound
    // (`output[i]` == outcome of `fs[i]`). Unlike fail-fast `par {}`, an
    // `Err` from one branch does NOT stop later branches: indices 0 and 2
    // (Ok) and 1 and 3 (Err) all appear, and the Ok at index 2 proves a
    // branch after an Err still ran. (design.md § Concurrency Semantics.)
    assert_eq!(
        run("fn work(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"neg:{n}\") }\n\
             }\n\
             fn main() {\n\
                 let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| work(1), || work(-2), || work(3), || work(-4)];\n\
                 let results: Vec[Result[i64, String]] = collect_all_vec(fs);\n\
                 println(f\"len={results.len()}\");\n\
                 for r in results {\n\
                     match r {\n\
                         Result.Ok(v) => { println(f\"ok {v}\"); }\n\
                         Result.Err(e) => { println(f\"err {e}\"); }\n\
                     }\n\
                 }\n\
             }"),
        "len=4\nok 10\nerr neg:-2\nok 30\nerr neg:-4\n"
    );
}

#[test]
fn collect_all_vec_panic_in_branch_dominates() {
    // A panicking branch dominates: it short-circuits the gather (via
    // `pending_cf`) so the post-`collect_all_vec` line never runs and the
    // panic propagates. (design.md § Parallel Failure and Cleanup — panic
    // cancels siblings even under `collect_all_vec`.)
    let (out, errors, _trace, _trunc) = run_program_full(
        "fn boom() -> Result[i64, String] { todo(\"kaboom\") }\n\
         fn main() {\n\
             let fs: Vec[Fn() -> Result[i64, String]] = Vec[|| Result.Ok(1), || boom(), || Result.Ok(3)];\n\
             let results: Vec[Result[i64, String]] = collect_all_vec(fs);\n\
             println(f\"after len={results.len()}\");\n\
         }",
    );
    assert!(
        !out.join("").contains("after"),
        "post-collect_all_vec line must not run after a branch panic; stdout: {:?}",
        out
    );
    assert!(
        !errors.is_empty(),
        "expected a runtime panic to propagate; got no errors"
    );
}

#[test]
fn collect_all_gathers_heterogeneous_tuple() {
    // Phase 6 — `collect_all(|| a, || b, || c)` runs every branch and
    // gathers a position-bound HETEROGENEOUS tuple. Branch error types
    // differ (String at .0, i64 at .1); no fail-fast (the `Err` at .0/.1
    // does not stop .2 from producing `Ok`).
    assert_eq!(
        run("fn fa(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"a{n}\") }\n\
             }\n\
             fn fb(s: String) -> Result[String, i64] { Result.Err(7) }\n\
             fn fc(n: i64) -> Result[i64, String] { Result.Ok(n + 100) }\n\
             fn main() {\n\
                 let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =\n\
                     collect_all(|| fa(-5), || fb(\"x\"), || fc(3));\n\
                 match t.0 { Result.Ok(v) => { println(f\"0 ok {v}\"); } Result.Err(e) => { println(f\"0 err {e}\"); } }\n\
                 match t.1 { Result.Ok(v) => { println(f\"1 ok {v}\"); } Result.Err(e) => { println(f\"1 err {e}\"); } }\n\
                 match t.2 { Result.Ok(v) => { println(f\"2 ok {v}\"); } Result.Err(e) => { println(f\"2 err {e}\"); } }\n\
             }"),
        "0 err a-5\n1 err 7\n2 ok 103\n"
    );
}

#[test]
fn collect_all_auto_thunks_bare_and_mixed_branches() {
    // design.md "closure wrappers optional" — bare-expression branches
    // (`collect_all(fa(x), fb(y))`) are auto-thunked by lowering into
    // `|| fa(x)` etc., so they gather identically to explicit closures;
    // mixed explicit/bare branches work too, and captured locals (`x`,
    // `y`) flow into the thunked closures.
    assert_eq!(
        run("fn fa(n: i64) -> Result[i64, String] {\n\
                 if n > 0 { Result.Ok(n * 10) } else { Result.Err(f\"a{n}\") }\n\
             }\n\
             fn fb(s: String) -> Result[String, i64] { Result.Err(7) }\n\
             fn main() {\n\
                 let x: i64 = 4;\n\
                 let t: (Result[i64, String], Result[String, i64], Result[i64, String]) =\n\
                     collect_all(fa(x), fb(\"z\"), || fa(-1));\n\
                 match t.0 { Result.Ok(v) => { println(f\"0 ok {v}\"); } Result.Err(e) => { println(f\"0 err {e}\"); } }\n\
                 match t.1 { Result.Ok(v) => { println(f\"1 ok {v}\"); } Result.Err(e) => { println(f\"1 err {e}\"); } }\n\
                 match t.2 { Result.Ok(v) => { println(f\"2 ok {v}\"); } Result.Err(e) => { println(f\"2 err {e}\"); } }\n\
             }"),
        "0 ok 40\n1 err 7\n2 err a-1\n"
    );
}

#[test]
fn test_slice_range_indexing_on_array() {
    let output = run("fn sum(xs: Slice[i64]) -> i64 {
             let mut acc = 0;
             for x in xs { acc = acc + x; }
             acc
         }
         fn main() {
             let a: Array[i64, 5] = [10, 20, 30, 40, 50];
             let s = a[1..4];
             println(sum(s));
         }");
    assert_eq!(output, "90\n");
}

#[test]
fn test_slice_of_slice_via_range() {
    let output = run("fn sum(xs: Slice[i64]) -> i64 {
             let mut acc = 0;
             for x in xs { acc = acc + x; }
             acc
         }
         fn main() {
             let a: Array[i64, 5] = [1, 2, 3, 4, 5];
             let outer = a[0..5];
             let inner = outer[1..4];
             println(sum(inner));
         }");
    assert_eq!(output, "9\n");
}

#[test]
fn test_bufwriter_with_capacity_write_flush() {
    // with_capacity wraps with an explicit buffer size; the write+flush
    // path is otherwise identical.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_cap.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.with_capacity(f, 4);
                     let data = [65u8, 66u8, 67u8];
                     let _ = bw.write(data[0..3]);
                     let _ = bw.flush();
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "contents=[ABC]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufwriter_write_all_writes_whole_buffer() {
    // write_all loops until the whole buffer is accepted and returns Unit;
    // flush then drains it to disk. Read-back proves every byte landed.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_write_all.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn main() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [119u8, 120u8, 121u8, 122u8];
                     match bw.write_all(data[0..4]) {{
                         Ok(_) => println(\"wrote all\"),
                         Err(_) => println(\"write err\"),
                     }}
                     let _ = bw.flush();
                 }}
                 Err(_) => println(\"create err\"),
             }}
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "wrote all\ncontents=[wxyz]\n");
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn test_bufwriter_drop_flushes_pending_writes() {
    // Omit the explicit flush — std::io::BufWriter's own Drop flushes the
    // buffered bytes through the cloned fd when the `Value::BufWriter` Arc
    // drops at scope exit, so the contents still reach disk. Read-back
    // happens after the writing scope ends.
    let tmp = std::env::temp_dir().join("karac_test_bufwriter_drop_flush.txt");
    let path = tmp.to_str().unwrap().replace('\\', "\\\\");
    let _ = std::fs::remove_file(&tmp);
    let src = format!(
        "fn write_it() {{
             match File.create(\"{path}\") {{
                 Ok(f) => {{
                     let bw = BufWriter.new(f);
                     let data = [122u8, 122u8];
                     let _ = bw.write(data[0..2]);
                 }}
                 Err(_) => println(\"create err\"),
             }}
         }}
         fn main() {{
             write_it();
             match FileSystem.read_to_string(\"{path}\") {{
                 Ok(s) => println(\"contents=[\" + s + \"]\"),
                 Err(_) => println(\"read err\"),
             }}
         }}"
    );
    let out = run_no_errors(&src);
    assert_eq!(out, "contents=[zz]\n");
    let _ = std::fs::remove_file(&tmp);
}

/// A clone of a NESTED collection must copy the inner collections too, not
/// bump their refcount (B-2026-08-20-32).
///
/// `Value::Array`/`Map`/`Set` are `Arc<RwLock<..>>`, so the clone arm's
/// one-level copy left `Vec[Vec[T]]`'s inner Vecs shared: a write through the
/// clone landed in the original. All three compiled surfaces deep-copy, so the
/// same program printed a different answer under `--interp` — and because it
/// ran to completion and printed something plausible, it was a silent wrong
/// answer rather than a crash.
///
/// The FLAT case was always right, which is what made this a DEPTH bug rather
/// than a broken `clone`; `test_vec_clone_independent_after_push` above covers
/// that half and passed throughout.
#[test]
fn test_clone_of_a_nested_collection_is_deep_at_every_level() {
    // Two levels — the row's own repro.
    let output = run("fn main() {\n\
             let orig: Vec[Vec[i64]] = [[1_i64, 1_i64], [1_i64, 1_i64]];\n\
             let mut copy = orig.clone();\n\
             copy[0][0] = 99_i64;\n\
             println(orig[0][0]);\n\
             println(copy[0][0]);\n\
         }");
    assert_eq!(output, "1\n99\n", "inner Vec of a Vec[Vec[T]] was shared");

    // Three levels — the aliasing was at every depth, not just the first.
    let output = run("fn main() {\n\
             let orig: Vec[Vec[Vec[i64]]] = [[[1_i64]]];\n\
             let mut copy = orig.clone();\n\
             copy[0][0][0] = 42_i64;\n\
             println(orig[0][0][0]);\n\
             println(copy[0][0][0]);\n\
         }");
    assert_eq!(output, "1\n42\n", "depth-3 nesting was shared");
}

// ── RateLimiter — token-bucket backpressure primitive ──────────────

#[test]
fn test_rate_limiter_grants_initial_burst_then_limits() {
    // Bucket starts full (capacity 3): three immediate grants for a key,
    // then the bucket is empty and the next try (microseconds later, no
    // meaningful refill at 1 token/sec) is limited. Deterministic — the
    // four calls run back-to-back well within one refill interval.
    let output = run(r#"fn main() {
         let rl = RateLimiter.new_token_bucket(1, 3);
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
         println(rl.try_acquire("k"));
     }"#);
    assert_eq!(output, "true\ntrue\ntrue\nfalse\n");
}

#[test]
fn test_rate_limiter_buckets_are_per_key() {
    // Each key gets an independent full bucket: exhausting key "a" leaves
    // key "b" with its own fresh burst.
    let output = run(r#"fn main() {
         let rl = RateLimiter.new_token_bucket(1, 1);
         println(rl.try_acquire("a"));
         println(rl.try_acquire("a"));
         println(rl.try_acquire("b"));
     }"#);
    assert_eq!(output, "true\nfalse\ntrue\n");
}

#[test]
fn test_rate_limiter_hand_rolled_zero_handle_fails_closed() {
    // A `RateLimiter { handle_id: 0 }` literal that bypassed the
    // constructor has no table entry; try_acquire reports limited
    // (false) rather than panicking.
    let output = run(r#"fn main() {
         let fake = RateLimiter { handle_id: 0 };
         println(fake.try_acquire("k"));
     }"#);
    assert_eq!(output, "false\n");
}

#[test]
fn test_chars_iterator_bound_to_variable_interpreter() {
    // B-2026-06-18-5 — `let it = s.chars()` (the char-iterator bound to a name),
    // then `it.collect()` / `for c in it`. The interpreter already snapshots
    // chars eagerly into a Value::Iterator, so it works; codegen now matches by
    // materializing a Vec[char] (`e2e_chars_iterator_bound_to_variable_codegen`).
    // Collecting the same bound iterator twice yields independent copies.
    let output = run(r#"fn main() {
            let s: String = "héllo";
            let it = s.chars();
            let v: Vec[char] = it.collect();
            let mut joined: String = "";
            for c in v { joined.push(c); }
            println(f"{joined} {joined.char_count()}");

            let it2 = s.chars();
            let mut dashed: String = "";
            for c in it2 { dashed.push(c); dashed.push('-'); }
            println(dashed);

            let it3 = s.chars();
            let a: Vec[char] = it3.collect();
            let b: Vec[char] = it3.collect();
            println(f"{a.len()} {b.len()} {a[0]} {b[4]}");
        }"#);
    assert_eq!(output, "héllo 5\nh-é-l-l-o-\n5 5 h o\n");
}

// ── Iterator: `iter()` / `into_iter()` / `next()` (wip-list2 subtask 1) ──

#[test]
fn test_iter_next_drains_vec_in_order() {
    // Calling next() repeatedly walks the source elements then yields None.
    // The cursor advance writes back through the binding, so successive
    // calls observe the new state.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\ndone\n");
}

#[test]
fn test_into_iter_matches_iter_at_runtime() {
    // The interpreter is type-erased; iter() and into_iter() produce the
    // same Value::Iterator. Verifying observable equivalence pins the
    // contract before laziness adaptors land.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let mut a = v.iter();
    let mut b = v.into_iter();
    println(a.next().unwrap());
    println(b.next().unwrap());
    println(a.next().unwrap());
    println(b.next().unwrap());
}
"#,
    );
    assert_eq!(output, "1\n1\n2\n2\n");
}

#[test]
fn test_iter_on_empty_vec_yields_none_immediately() {
    // First next() on an empty source returns None; no Some preceded it.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter();
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

// ── for-loop on iterator values (wip-list2 subtask 2) ────────────

#[test]
fn test_for_loop_on_vec_iter_walks_elements() {
    // Direct iteration over an iterator value — `for x in v.iter() { ... }`
    // walks the source elements in order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    for x in v.iter() {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_for_loop_on_iter_resumes_from_cursor() {
    // The iterator is bound, advanced manually with next(), then dropped
    // into a for-loop — the loop must resume from the cursor's current
    // position rather than restarting from the beginning.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut it = v.iter();
    let _ = it.next();
    let _ = it.next();
    for x in it {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_for_loop_break_inside_iterator_loop() {
    // break exits the for-loop early; downstream code still executes.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter() {
        if x > 2 {
            break;
        }
        println(x);
    }
    println(99);
}
"#,
    );
    assert_eq!(output, "1\n2\n99\n");
}

#[test]
fn test_for_loop_continue_inside_iterator_loop() {
    // continue skips to the next iteration without exiting the loop.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter() {
        if x == 2 {
            continue;
        }
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n4\n");
}

#[test]
fn test_iter_filter_keeps_matching_elements() {
    // `.filter(pred)` yields only elements where pred returns true.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for n in v.iter().filter(|x| x > 2) {
        println(n);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n5\n");
}

#[test]
fn test_iter_partition_scalar_and_heap() {
    // `partition(pred: Fn(T) -> bool) -> (Vec[T], Vec[T])` — split the yielded
    // elements into (matches, non-matches) (B-2026-07-19-14). Covers a scalar
    // split, composition with a leading `map`, and a HEAP `String` split (which
    // the codegen backend defers to interp).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    let (evens, odds): (Vec[i64], Vec[i64]) = v.iter().partition(|x| x % 2 == 0);
    println(f"{evens.len()}:{odds.len()}");
    let mut es = 0;
    for e in evens { es = es + e; }
    let mut os = 0;
    for o in odds { os = os + o; }
    println(f"{es}:{os}");
    let w = ["hi".to_string(), "there".to_string(), "yo".to_string(), "world".to_string()];
    let (long, short): (Vec[String], Vec[String]) = w.iter().partition(|s| s.len() > 2);
    println(f"{long.join(",")}|{short.join(",")}");
}
"#,
    );
    assert_eq!(output, "3:3\n12:9\nthere,world|hi,yo\n");
}

#[test]
fn test_iter_filter_drops_all_when_predicate_always_false() {
    // When the predicate rejects every element, next() returns None on
    // the first pull (after walking the entire source internally).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().filter(|x| x > 100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_count_returns_element_count() {
    // count() drains the iterator and returns the element count as i64.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let n: i64 = v.iter().count();
    println(n);
}
"#,
    );
    assert_eq!(output, "4\n");
}

#[test]
fn test_iter_count_empty_returns_zero() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let n: i64 = v.iter().count();
    println(n);
}
"#,
    );
    assert_eq!(output, "0\n");
}

#[test]
fn test_iter_count_after_filter_counts_kept_elements() {
    // count() composes with filter — only elements that pass the
    // predicate contribute to the count.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let n: i64 = v.iter().filter(|x| x > 2).count();
    println(n);
}
"#,
    );
    assert_eq!(output, "3\n");
}

#[test]
fn test_iter_collect_yields_vec_in_order() {
    // collect() v1 returns a Vec[T] preserving iterator order.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter().collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_rev_reverses_order() {
    // B-2026-07-18-41 — `rev()` yields the elements in reverse. Unlike the lazy
    // per-element adaptors, it drains the upstream, reverses, and replays, so it
    // composes with adaptors on BOTH sides: `map(f).rev()` reverses the mapped
    // sequence, `rev().map(f)` maps the reversed one, and `rev().filter(p)`
    // filters after reversal. A `for` loop and a `collect`/`fold` terminal both
    // consume the reversed iterator. (Interpreter-only; codegen defers rev.)
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter().rev() { println(x); }
    let a: Vec[i64] = v.iter().rev().collect();
    println(a.get(0));
    let b: Vec[i64] = v.iter().map(|x| x * 10).rev().collect();
    println(b.get(0));
    let c: Vec[i64] = v.iter().rev().map(|x| x * 10).collect();
    println(c.get(0));
    println(v.iter().rev().fold(0, |acc, x| acc * 10 + x));
    let d: Vec[i64] = v.iter().rev().filter(|x| x % 2 == 0).collect();
    println(d.get(0));
}
"#,
    );
    // for: 4 3 2 1; a.get(0)=Some(4); b (map*10 then rev).get(0)=Some(40);
    // c (rev then map*10).get(0)=Some(40); fold builds 4321;
    // d (rev [4,3,2,1] then keep even).get(0)=Some(4)
    assert_eq!(
        output,
        "4\n3\n2\n1\nSome(4)\nSome(40)\nSome(40)\n4321\nSome(4)\n"
    );
}

#[test]
fn test_iter_flatten_interpreter() {
    // `Iterator.flatten()` — an iterator of iterables yields the inner elements
    // in order (≡ `flat_map(|x| x)`). Eager (drain-flatten-replay, like `rev`),
    // so it composes with adaptors/terminals on both sides. Empty inner
    // collections contribute nothing. Codegen defers flatten to `--interp`
    // (tests/codegen.rs::e2e_iter_flatten_deferred_loud_message).
    let output = run_no_errors(
        r#"
fn main() {
    let nested: Vec[Vec[i64]] = [[1, 2], [3, 4], [5]];
    let flat: Vec[i64] = nested.iter().flatten().collect();
    println(flat.get(0));
    println(flat.get(4));
    for x in nested.iter().flatten() { print(x); }
    println("");
    // String elements
    let words: Vec[Vec[String]] = [["a", "b"], ["c"]];
    for w in words.iter().flatten() { print(w); }
    println("");
    // empty inners interspersed
    let e: Vec[Vec[i64]] = [[], [1], [], [2, 3], []];
    let f: Vec[i64] = e.iter().flatten().collect();
    println(f.len());
    // compose downstream: flatten then map / filter+sum / count
    println(nested.iter().flatten().map(|x| x * 2).collect().get(2));
    println(nested.iter().flatten().filter(|x| x % 2 == 1).sum());
    println(nested.iter().flatten().count());
}
"#,
    );
    // flat.get(0)=Some(1); flat.get(4)=Some(5); for=12345; strings=abc;
    // empty-inners len=3; map*2 .get(2)=Some(6); odd sum 1+3+5=9; count=5
    assert_eq!(output, "Some(1)\nSome(5)\n12345\nabc\n3\nSome(6)\n9\n5\n");
}

#[test]
fn test_iter_rev_range_interpreter() {
    // B-2026-07-18-41 range leg — `(a..b).rev()` / `(a..=b).rev()` descend over
    // the same value set. Mirrored A/B by the codegen reverse-iterate range loop
    // (tests/codegen.rs::e2e_iter_rev_range_reverse_iterate): for-loop
    // (exclusive / inclusive / empty / nested), vec indexing in reverse, and the
    // collect / fold / sum / count terminals.
    let output = run_no_errors(
        r#"
fn main() {
    for i in (0..5).rev() { print(i); }
    println("");
    for i in (1..=4).rev() { print(i); }
    println("");
    for i in (5..5).rev() { print(i); }
    println("E");
    let v: Vec[i64] = [10, 20, 30, 40];
    let mut s: i64 = 0;
    for i in (0..4).rev() { s = s + v[i]; }
    println(s);
    let w: Vec[i64] = (0..3).rev().collect();
    println(w.get(0));
    println((0..4).rev().fold(0, |acc, x| acc * 10 + x));
    println((0..4).rev().sum());
    println((1..=3).rev().count());
    for a in (0..2).rev() { for b in (0..2).rev() { print(f"{a}{b} "); } }
    println("");
}
"#,
    );
    assert_eq!(
        output,
        "43210\n4321\nE\n100\nSome(2)\n3210\n6\n3\n11 10 01 00 \n"
    );
}

#[test]
fn test_iter_collect_after_filter_drops_rejected_elements() {
    // filter then collect — only elements that pass the predicate land
    // in the resulting Vec.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n5\n");
}

#[test]
fn test_iter_fold_sums_elements() {
    // Canonical fold use — sum a Vec[i64] starting from 0.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let s: i64 = v.iter().fold(0, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "15\n");
}

#[test]
fn test_iter_sum_terminal() {
    // B-2026-07-11-19 — the numeric `sum()` terminal. Covers a bare
    // `iter().sum`, a `map().sum`, a `filter().sum`, an f64 sum, an empty
    // source (0), and a range sum.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3, 4];
    println(v.iter().sum().to_string());
    println(v.iter().map(|x: i64| x * 2i64).sum().to_string());
    println(v.iter().filter(|x: i64| x > 2i64).sum().to_string());
    let f: Vec[f64] = [1.5, 2.5, 3.0];
    println(f.iter().sum().to_string());
    let e: Vec[i64] = [];
    println(e.iter().sum().to_string());
    println((1i64..5i64).sum().to_string());
}
"#,
    );
    // 10, 20, 7, 7, 0, 10
    assert_eq!(output, "10\n20\n7\n7\n0\n10\n");
}

#[test]
fn test_iter_reduce_terminal() {
    // B-2026-07-11-19 — the `reduce(f) -> Option[A]` terminal. Folds with the
    // first element as the seed; `None` on an empty source. Covers a sum
    // reduce, a max reduce, an empty source (None), and a `map().reduce`.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3, 4];
    match v.iter().reduce(|a: i64, x: i64| a + x) { Some(s) => { println(s.to_string()); } None => { println("none"); } }
    match v.iter().reduce(|a: i64, x: i64| if a > x { a } else { x }) { Some(s) => { println(s.to_string()); } None => { println("none"); } }
    let e: Vec[i64] = [];
    match e.iter().reduce(|a: i64, x: i64| a + x) { Some(s) => { println(s.to_string()); } None => { println("none"); } }
    match v.iter().map(|x: i64| x * 2i64).reduce(|a: i64, x: i64| a + x) { Some(s) => { println(s.to_string()); } None => { println("none"); } }
}
"#,
    );
    // 10, 4, none, 20
    assert_eq!(output, "10\n4\nnone\n20\n");
}

#[test]
fn test_iter_for_each_terminal() {
    // B-2026-07-11-19 / -23 — the side-effecting `for_each` terminal, unblocked
    // by mut-ref closure capture (its natural use mutates a captured
    // accumulator). Covers a bare for_each, a map().for_each, a
    // filter().for_each, and a range for_each.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = [1, 2, 3, 4];
    let mut total = 0i64;
    v.iter().for_each(|x: i64| { total = total + x; });
    println(f"{total}");
    v.iter().map(|x: i64| x * 2i64).for_each(|x: i64| { total = total + x; });
    println(f"{total}");
    let mut ev = 0i64;
    v.iter().filter(|x: i64| x % 2i64 == 0i64).for_each(|x: i64| { ev = ev + x; });
    println(f"{ev}");
    let mut rng = 0i64;
    (1i64..5i64).for_each(|x: i64| { rng = rng + x; });
    println(f"{rng}");
}
"#,
    );
    // 10, 30 (10+20), 6 (2+4), 10 (1+2+3+4)
    assert_eq!(output, "10\n30\n6\n10\n");
}

#[test]
fn test_for_iter_mut() {
    // B-2026-07-14-10 — `for x in xs.iter_mut()` yields a mutable reference to
    // each element so `*x = …` / `*x += …` write back into the Vec. The
    // interpreter binds each element to a `VecSlotRef` over the shared element
    // storage; write-throughs land in the live Vec. Covers deref-assign,
    // compound deref-assign, a second pass, and a `String` element.
    let output = run_no_errors(
        r#"
fn main() {
    let mut v: Vec[i64] = [1i64, 2i64, 3i64, 4i64];
    for x in v.iter_mut() { *x = *x * 2i64; }
    for x in v.iter_mut() { *x += 1i64; }
    let mut s: i64 = 0i64;
    for y in v { s = s + y; }
    println(f"{s}");
    let mut words: Vec[String] = [f"a", f"bb"];
    for w in words.iter_mut() { *w = f"x"; }
    for w in words { println(f"{w}"); }
}
"#,
    );
    assert_eq!(output, "24\nx\nx\n");
}

#[test]
fn test_iter_fold_empty_returns_init_unchanged() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let s: i64 = v.iter().fold(42, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn test_iter_fold_after_filter_only_visits_kept_elements() {
    // Adaptors fire during fold's drain — filter rejects 1 and 2,
    // so the closure only runs for 3 + 4 + 5 = 12 (init 0).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let s: i64 = v.iter().filter(|x| x > 2).fold(0, |acc, x| acc + x);
    println(s);
}
"#,
    );
    assert_eq!(output, "12\n");
}

#[test]
fn test_iter_any_returns_true_on_first_match() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().any(|x| x > 3);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_position_returns_index_of_first_match() {
    // `position(pred) -> Option[i64]` — 0-based index of the first yielded
    // element matching the predicate (over the POST-adaptor sequence), or None.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    match v.iter().position(|x| x == 30) { Some(i) => println(i), None => println(-1) }
    match v.iter().position(|x| x == 99) { Some(i) => println(i), None => println(-1) }
    match v.iter().filter(|x| x % 20 == 0).position(|x| x == 40) { Some(i) => println(i), None => println(-1) }
}
"#,
    );
    // 30@index 2; miss=-1; filtered [20,40] -> 40@index 1
    assert_eq!(output, "2\n-1\n1\n");
}

#[test]
fn test_iter_find_returns_first_matching_element() {
    // `find(pred) -> Option[T]` — the first yielded element matching the
    // predicate (over the POST-adaptor sequence), or None. Element returned by
    // value; works for scalar and String (heap) elements in the interpreter.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    match v.iter().find(|x| x > 25) { Some(e) => println(e), None => println(-1) }
    match v.iter().find(|x| x > 99) { Some(e) => println(e), None => println(-1) }
    match v.iter().filter(|x| x % 20 == 0).find(|x| x > 25) { Some(e) => println(e), None => println(-1) }
    let s: Vec[String] = ["a", "bb", "ccc"];
    match s.iter().find(|w| w.len() == 2) { Some(e) => println(e), None => println("none") }
}
"#,
    );
    // >25 -> 30; miss -> -1; filter[20,40] first>25 -> 40; String len==2 -> "bb"
    assert_eq!(output, "30\n-1\n40\nbb\n");
}

#[test]
fn test_iter_last_and_nth_terminals() {
    // `last() -> Option[T]` (last yielded element, or None) and
    // `nth(n) -> Option[T]` (0-based n-th yielded element, or None). All element
    // types in the interpreter (last on a String source shown).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    match v.iter().last() { Some(x) => println(x), None => println(-1) }
    match v.iter().nth(1) { Some(x) => println(x), None => println(-1) }
    match v.iter().nth(9) { Some(x) => println(x), None => println(-1) }
    let s: Vec[String] = ["a", "b", "c"];
    match s.iter().last() { Some(x) => println(x), None => println("none") }
    let e: Vec[i64] = [];
    match e.iter().last() { Some(x) => println(x), None => println(-1) }
}
"#,
    );
    // last=4; nth1=2; nth9=None->-1; string last="c"; empty last=None->-1
    assert_eq!(output, "4\n2\n-1\nc\n-1\n");
}

#[test]
fn test_iter_any_returns_false_when_no_match() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let b: bool = v.iter().any(|x| x > 100);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_any_on_empty_returns_false() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let b: bool = v.iter().any(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_any_short_circuits_on_first_match() {
    // any() should stop iterating the moment the predicate returns true.
    // The closure prints each element it sees; with input 1..5 and
    // pred `x > 2`, only the first three elements should print before
    // any() returns. Tree-walk closures snapshot captures so we can't
    // count via mutated outer bindings — the println side-effect
    // ordering is the visible signal.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().any(|x| {
        println(x);
        x > 2
    });
    println(b);
}
"#,
    );
    assert_eq!(output, "1\n2\n3\ntrue\n");
}

#[test]
fn test_iter_all_returns_true_when_every_element_matches() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [2, 4, 6];
    let b: bool = v.iter().all(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_all_returns_false_on_first_mismatch() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [2, 4, -1, 6];
    let b: bool = v.iter().all(|x| x > 0);
    println(b);
}
"#,
    );
    assert_eq!(output, "false\n");
}

#[test]
fn test_iter_all_on_empty_returns_true() {
    // Vacuously true — no element violates the predicate.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let b: bool = v.iter().all(|x| x > 100);
    println(b);
}
"#,
    );
    assert_eq!(output, "true\n");
}

#[test]
fn test_iter_all_short_circuits_on_first_mismatch() {
    // all() should stop the moment the predicate returns false. With
    // input 1..5 and pred `x < 3`, the predicate sees 1, 2, 3 — and
    // bails on 3 (the first failing element). Element 3 is still
    // printed because the closure body runs to completion before its
    // boolean is consulted.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let b: bool = v.iter().all(|x| {
        println(x);
        x < 3
    });
    println(b);
}
"#,
    );
    assert_eq!(output, "1\n2\n3\nfalse\n");
}

#[test]
fn test_iter_enumerate_yields_index_and_item_pairs() {
    // enumerate() yields (idx, item) tuples; idx starts at 0.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    for (i, s) in v.iter().enumerate() {
        println(f"{i}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "0:a\n1:b\n2:c\n");
}

#[test]
fn test_iter_enumerate_persists_index_across_next_calls() {
    // Verifies state writeback — the Enumerate(idx) counter has to
    // survive between separate next() calls on the same iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().enumerate();
    let (a, ax) = it.next().unwrap();
    let (b, bx) = it.next().unwrap();
    println(f"{a}:{ax}");
    println(f"{b}:{bx}");
}
"#,
    );
    assert_eq!(output, "0:10\n1:20\n");
}

#[test]
fn test_iter_take_yields_only_first_n_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().take(3) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_zero_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().take(0);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_take_more_than_length_yields_all_elements() {
    // take(n) where n > len yields all elements without error.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let n: i64 = v.iter().take(100).count();
    println(n);
}
"#,
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_iter_skip_drops_first_n_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    for x in v.iter().skip(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "30\n40\n50\n");
}

#[test]
fn test_iter_skip_more_than_length_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().skip(100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_skip_then_take_window() {
    // skip + take composed forms a slice — drop 1, take next 2 → [2, 3].
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().skip(1).take(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "2\n3\n");
}

#[test]
fn test_iter_take_with_filter_yields_first_n_passing() {
    // filter then take(2) — only the first two elements that pass the
    // predicate are yielded; the source still walks past rejected items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    for x in v.iter().filter(|x| x > 2).take(2) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_iter_take_state_persists_across_next_calls() {
    // The Take(remaining) counter has to survive between next() calls
    // for the bound to actually limit total yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let mut it = v.iter().take(2);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n20\ndone\n");
}

#[test]
fn test_iter_chain_yields_left_then_right() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [10, 20, 30];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n10\n20\n30\n");
}

#[test]
fn test_iter_chain_left_empty_yields_only_right() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let w = [7, 8];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "7\n8\n");
}

#[test]
fn test_iter_chain_right_empty_yields_only_left() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [3, 4];
    let w: Vec[i64] = Vec[];
    for x in v.iter().chain(w.iter()) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n");
}

#[test]
fn test_iter_chain_preserves_per_side_adaptors() {
    // Each side keeps its own adaptor chain — left's filter and
    // right's map both fire on their own elements only.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let w = [10, 20];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).chain(w.iter().map(|y| y + 100)).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "3\n4\n110\n120\n");
}

#[test]
fn test_iter_chain_downstream_step_applies_to_both_sides() {
    // Downstream map on the result of chain applies to ALL items
    // regardless of which side they came from.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [10, 20];
    for x in v.iter().chain(w.iter()).map(|x| x * 100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "100\n200\n1000\n2000\n");
}

#[test]
fn test_iter_zip_pairs_elements_in_lockstep() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let w = ["a", "b", "c"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n3:c\n");
}

#[test]
fn test_iter_zip_stops_at_shorter_left_side() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = ["a", "b", "c", "d"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n");
}

#[test]
fn test_iter_zip_stops_at_shorter_right_side() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let w = ["a", "b"];
    for (n, s) in v.iter().zip(w.iter()) {
        println(f"{n}:{s}");
    }
}
"#,
    );
    assert_eq!(output, "1:a\n2:b\n");
}

#[test]
fn test_iter_zip_either_empty_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let w = ["a", "b"];
    let mut it = v.iter().zip(w.iter());
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_zip_preserves_per_side_adaptors() {
    // Left's filter and right's map both fire while zipping. Filtering
    // and mapping happen INSIDE each side's iteration; zip pulls from
    // the post-adaptor stream.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let w = [10, 20, 30, 40];
    for (a, b) in v.iter().filter(|x| x > 1).zip(w.iter().map(|y| y * 2)) {
        println(f"{a}+{b}");
    }
}
"#,
    );
    // Left filtered: 2, 3, 4, 5. Right mapped: 20, 40, 60, 80.
    // Zipped: (2,20) (3,40) (4,60) (5,80).
    assert_eq!(output, "2+20\n3+40\n4+60\n5+80\n");
}

#[test]
fn test_iter_zip_with_enumerate_on_one_side() {
    // Composes with enumerate on the right side — index and value.
    let output = run_no_errors(
        r#"
fn main() {
    let v = ["a", "b", "c"];
    let w = [10, 20, 30];
    for (s, (i, n)) in v.iter().zip(w.iter().enumerate()) {
        println(f"{s}:{i}={n}");
    }
}
"#,
    );
    assert_eq!(output, "a:0=10\nb:1=20\nc:2=30\n");
}

#[test]
fn test_iter_chain_state_persists_across_next_calls() {
    // After exhausting left via two next() calls, the third pull
    // should switch to right transparently.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let w = [9];
    let mut it = v.iter().chain(w.iter());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n9\ndone\n");
}

#[test]
fn test_iter_take_while_yields_until_first_failure() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 10, 4, 5];
    for x in v.iter().take_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Stops at the first element where predicate fails (10), yielding
    // only the 1, 2, 3 prefix — even though 4 and 5 follow, take_while
    // does not resume after a failure.
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_while_first_element_fails_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().take_while(|x| x < 5);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_take_while_all_pass_yields_all_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().take_while(|x| x < 100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_take_while_short_circuits_predicate() {
    // After the first false, predicate must NOT fire on subsequent
    // elements. Use println side effects to verify: the prefix prints
    // "p:N" for each predicate call, and the body prints "y:N" for
    // each yielded element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    for x in v.iter().take_while(|x| { println(f"p:{x}"); x < 5 }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Predicate fires on 1, 2 (yield), 9 (stop). Never on 3 or 4.
    // The for-loop pulls LAZILY (one element per iteration,
    // B-2026-07-14-22), so predicate and body prints INTERLEAVE —
    // matching the codegen backend's fused lowering and Rust. The
    // short-circuit guarantee is still proven by the absence of "p:3"
    // and "p:4".
    assert_eq!(output, "p:1\ny:1\np:2\ny:2\np:9\n");
}

#[test]
fn test_iter_skip_while_drops_leading_prefix() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 10, 1, 2];
    for x in v.iter().skip_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Skips 1, 2, 3 (predicate true), then yields 10 and everything
    // that follows — INCLUDING 1, 2 — because skip_while does not
    // re-test once the predicate has failed.
    assert_eq!(output, "10\n1\n2\n");
}

#[test]
fn test_iter_skip_while_all_pass_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut it = v.iter().skip_while(|x| x < 100);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_skip_while_first_element_fails_yields_all() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    for x in v.iter().skip_while(|x| x < 5) {
        println(x);
    }
}
"#,
    );
    // Predicate is false on the very first element, so skip_while
    // yields the whole iterator unchanged.
    assert_eq!(output, "10\n20\n30\n");
}

#[test]
fn test_iter_skip_while_does_not_re_test_after_first_failure() {
    // Same observation harness as the take_while short-circuit test:
    // predicate side-effects show the call sequence, body shows yields.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    for x in v.iter().skip_while(|x| { println(f"p:{x}"); x < 5 }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Predicate fires on 1, 2, 9 (trip). Never on 3 or 4. After the
    // trip, 9 is yielded and 3 / 4 pass through unconditionally.
    assert_eq!(output, "p:1\np:2\np:9\ny:9\ny:3\ny:4\n");
}

#[test]
fn test_iter_take_while_state_persists_across_next_calls() {
    // Once take_while has tripped, subsequent next() calls must
    // continue to return None — even though the source has more items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3];
    let mut it = v.iter().take_while(|x| x < 5);
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match it.next() {
        Some(_) => println("more"),
        None => println("still-done"),
    }
}
"#,
    );
    assert_eq!(output, "1\n2\ndone\nstill-done\n");
}

#[test]
fn test_iter_skip_while_state_persists_across_next_calls() {
    // Once skip_while has tripped, every subsequent next() must
    // return the next raw item without re-testing the predicate.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 9, 3, 4];
    let mut it = v.iter().skip_while(|x| x < 5);
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "9\n3\n4\ndone\n");
}

#[test]
fn test_iter_take_while_composes_with_filter() {
    // Filter feeds take_while — predicate sees only elements that
    // passed the filter; take_while stops on the first kept-but-failing.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let xs: Vec[i64] = v.iter().filter(|x| x % 2 == 0).take_while(|x| x < 7).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 2, 4, 6, 8. take_while(<7) stops at 8 → [2, 4, 6].
    assert_eq!(output, "2\n4\n6\n");
}

#[test]
fn test_iter_skip_while_then_take_while_window() {
    // skip_while drops leading prefix, take_while bounds the tail.
    // Composition produces a "while-window" view.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 5, 6, 7, 9, 1];
    let xs: Vec[i64] = v.iter().skip_while(|x| x < 5).take_while(|x| x < 9).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // skip_while drops 1, 2 → trips on 5. take_while keeps 5, 6, 7;
    // stops at 9. Trailing 1 is unreachable because take_while is
    // sticky-stop after the first failure.
    assert_eq!(output, "5\n6\n7\n");
}

#[test]
fn test_iter_step_by_yields_every_nth_element() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7];
    for x in v.iter().step_by(2) {
        println(x);
    }
}
"#,
    );
    // Yields indices 0, 2, 4, 6 → 1, 3, 5, 7.
    assert_eq!(output, "1\n3\n5\n7\n");
}

#[test]
fn test_iter_step_by_one_is_observable_noop() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(1) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_step_by_zero_clamps_to_one() {
    // n=0 would underflow on the post-yield reset; runtime clamps
    // to 1, behaving like step_by(1).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(0) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_step_by_larger_than_length_yields_only_first() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().step_by(100) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n");
}

#[test]
fn test_iter_step_by_on_empty_yields_nothing() {
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().step_by(2);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_step_by_state_persists_across_next_calls() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut it = v.iter().step_by(2);
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    // Yields 10, 30, 50.
    assert_eq!(output, "10\n30\n50\ndone\n");
}

#[test]
fn test_iter_step_by_composes_with_filter() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let xs: Vec[i64] = v.iter().filter(|x| x > 2).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 3, 4, 5, 6, 7, 8. step_by(2) → 3, 5, 7.
    assert_eq!(output, "3\n5\n7\n");
}

#[test]
fn test_iter_cycle_with_take_yields_repeated_elements() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    for x in v.iter().cycle().take(5) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n1\n2\n1\n");
}

#[test]
fn test_iter_cycle_preserves_pre_adaptors() {
    // Adaptors applied BEFORE cycle live in the template's own
    // step chain — they re-run on each restart. Here the filter
    // re-rejects 1 each cycle.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    for x in v.iter().filter(|x| x > 1).cycle().take(5) {
        println(x);
    }
}
"#,
    );
    // Each cycle yields 2, 3. take(5) → 2, 3, 2, 3, 2.
    assert_eq!(output, "2\n3\n2\n3\n2\n");
}

#[test]
fn test_iter_cycle_on_empty_yields_nothing() {
    // Sticky-stop on empty template — must NOT infinite-loop.
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec[];
    let mut it = v.iter().cycle().take(10);
    match it.next() {
        Some(_) => println("had value"),
        None => println("empty"),
    }
}
"#,
    );
    assert_eq!(output, "empty\n");
}

#[test]
fn test_iter_cycle_composes_with_post_map() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    for x in v.iter().cycle().take(4).map(|x| x * 10) {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "10\n20\n10\n20\n");
}

#[test]
fn test_iter_cycle_state_persists_across_next_calls() {
    // next() pulls one item at a time, crossing cycle boundaries
    // transparently.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [7, 8];
    let mut it = v.iter().cycle();
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
}
"#,
    );
    assert_eq!(output, "7\n8\n7\n8\n7\n");
}

#[test]
fn test_iter_cycle_resets_stateful_adaptors_each_cycle() {
    // enumerate inside the template restarts at index 0 each cycle
    // because cycle clones the template (with its initial counters).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20];
    for (i, x) in v.iter().enumerate().cycle().take(4) {
        println(f"{i}:{x}");
    }
}
"#,
    );
    // First cycle: (0,10), (1,20). Second cycle re-runs enumerate
    // from 0 → (0,10), (1,20). take(4) total.
    assert_eq!(output, "0:10\n1:20\n0:10\n1:20\n");
}

#[test]
fn test_iter_step_by_then_cycle() {
    // step_by trims the template; cycle replays the trimmed
    // sequence forever.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    for x in v.iter().step_by(2).cycle().take(7) {
        println(x);
    }
}
"#,
    );
    // step_by(2) → 1, 3, 5. cycle.take(7) → 1, 3, 5, 1, 3, 5, 1.
    assert_eq!(output, "1\n3\n5\n1\n3\n5\n1\n");
}

#[test]
fn test_iter_inspect_passes_through_unchanged() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let xs: Vec[i64] = v.iter().inspect(|x| println(f"saw:{x}")).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // For-loop drains: inspect fires on each item during drain
    // (saw:10, saw:20, saw:30), then the body iterates the
    // collected Vec.
    assert_eq!(output, "saw:10\nsaw:20\nsaw:30\n10\n20\n30\n");
}

#[test]
fn test_iter_inspect_after_filter_only_fires_on_kept() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let xs: Vec[i64] = v.iter()
        .filter(|x| x > 2)
        .inspect(|x| println(f"saw:{x}"))
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter keeps 3, 4. inspect fires on 3, 4 only.
    assert_eq!(output, "saw:3\nsaw:4\n3\n4\n");
}

#[test]
fn test_iter_inspect_composes_with_downstream_steps() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let xs: Vec[i64] = v.iter()
        .inspect(|x| println(f"raw:{x}"))
        .map(|x| x * 10)
        .inspect(|x| println(f"mapped:{x}"))
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // First inspect sees raw items; map transforms; second inspect
    // sees mapped items. All firing during the drain phase.
    assert_eq!(
        output,
        "raw:1\nmapped:10\nraw:2\nmapped:20\nraw:3\nmapped:30\n10\n20\n30\n"
    );
}

#[test]
fn test_iter_scan_yields_running_sum() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    for x in v.iter().scan(0, |state, item| {
        let new = state + item;
        Some((new, new))
    }) {
        println(x);
    }
}
"#,
    );
    // 0+1=1, 1+2=3, 3+3=6, 6+4=10.
    assert_eq!(output, "1\n3\n6\n10\n");
}

#[test]
fn test_iter_scan_short_circuits_on_none() {
    // scan returning None stops iteration; subsequent items are
    // not visited.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 100, 4, 5];
    for x in v.iter().scan(0, |state, item| {
        if item > 50 {
            None
        } else {
            let new = state + item;
            Some((new, new))
        }
    }) {
        println(x);
    }
}
"#,
    );
    // Stops at 100; running sums of 1, 2, 3 are 1, 3, 6.
    assert_eq!(output, "1\n3\n6\n");
}

#[test]
fn test_iter_scan_short_circuit_does_not_re_fire() {
    // Side-effect prefix proves scan does NOT call closure on items
    // after the first None.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 100, 3, 4];
    for x in v.iter().scan(0, |state, item| {
        println(f"c:{item}");
        if item > 50 {
            None
        } else {
            let new = state + item;
            Some((new, new))
        }
    }) {
        println(f"y:{x}");
    }
}
"#,
    );
    // Closure fires on 1, 2, 100. None on 100 → stop. 3 and 4 are
    // never visited. Yields: 1 (=0+1), 3 (=1+2). The for-loop pulls
    // LAZILY (B-2026-07-14-22), so closure and body prints INTERLEAVE.
    assert_eq!(output, "c:1\ny:1\nc:2\ny:3\nc:100\n");
}

#[test]
fn test_iter_scan_state_persists_across_next_calls() {
    // next() pulls one item at a time; scan's state survives
    // between pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut it = v.iter().scan(0, |state, item| {
        let new = state + item;
        Some((new, new))
    });
    println(it.next().unwrap());
    println(it.next().unwrap());
    println(it.next().unwrap());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "10\n30\n60\ndone\n");
}

#[test]
fn test_iter_scan_after_filter() {
    // Filter first, then scan — the scan closure only sees kept
    // items.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    let xs: Vec[i64] = v.iter()
        .filter(|x| x % 2 == 0)
        .scan(0, |state, item| {
            let new = state + item;
            Some((new, new))
        })
        .collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    // Filter yields 2, 4, 6. Running sum: 2, 6, 12.
    assert_eq!(output, "2\n6\n12\n");
}

#[test]
fn test_iter_scan_state_independent_of_yielded_value() {
    // State and yielded value can differ — useful for "yield index
    // of running max" patterns.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [3, 1, 4, 1, 5, 9, 2, 6];
    for x in v.iter().scan(0, |state, item| {
        let new_state = if item > state { item } else { state };
        Some((new_state, new_state))
    }) {
        println(x);
    }
}
"#,
    );
    // Running max: 3, 3, 4, 4, 5, 9, 9, 9.
    assert_eq!(output, "3\n3\n4\n4\n5\n9\n9\n9\n");
}

#[test]
fn test_iter_peek_returns_next_without_consuming() {
    // peek() returns the upcoming element; the next next() call
    // returns the SAME element (the buffer is what gets consumed).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.next().unwrap());
}
"#,
    );
    assert_eq!(output, "10\n10\n20\n");
}

#[test]
fn test_iter_peek_idempotent_until_next() {
    // Multiple peek()s in a row see the same element; only next()
    // drains the buffer.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.peek().unwrap());
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.peek().unwrap());
}
"#,
    );
    assert_eq!(output, "1\n1\n1\n1\n2\n");
}

#[test]
fn test_iter_peek_at_end_returns_none() {
    // After draining, peek returns None; subsequent peeks stay None.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1];
    let mut p = v.iter().peekable();
    println(p.next().unwrap());
    match p.peek() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match p.peek() {
        Some(_) => println("more"),
        None => println("done"),
    }
    match p.next() {
        Some(_) => println("more"),
        None => println("done-next"),
    }
}
"#,
    );
    assert_eq!(output, "1\ndone\ndone\ndone-next\n");
}

#[test]
fn test_iter_peek_on_drained_iterator() {
    // After draining a single-element iterator with .take(0), peek
    // sees no element on a freshly constructed Peekable.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().take(0).peekable();
    match p.peek() {
        Some(_) => println("yes"),
        None => println("none"),
    }
}
"#,
    );
    assert_eq!(output, "none\n");
}

#[test]
fn test_iter_peek_does_not_re_pull_after_buffered() {
    // peek() pulls from inner exactly once per buffered slot.
    // Side-effect prefix proves the closure does NOT fire on
    // repeated peek() calls — only the first peek() pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().inspect(|x| println(f"pull:{x}")).peekable();
    println(f"peek1:{p.peek().unwrap()}");
    println(f"peek2:{p.peek().unwrap()}");
    println(f"next:{p.next().unwrap()}");
    println(f"peek3:{p.peek().unwrap()}");
}
"#,
    );
    // Drain order: first peek() triggers inner pull (pull:1) and
    // buffers; second peek() returns from buffer (no pull); next()
    // drains buffer (no pull); third peek() pulls again (pull:2).
    assert_eq!(
        output,
        "pull:1\npeek1:1\npeek2:1\nnext:1\npull:2\npeek3:2\n"
    );
}

#[test]
fn test_iter_peek_sees_post_inner_step_value() {
    // When map is applied BEFORE peekable(), the buffered (and peeked)
    // value is the mapped value — peek and next agree.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().map(|x| x * 10).peekable();
    println(p.peek().unwrap());
    println(p.next().unwrap());
    println(p.peek().unwrap());
}
"#,
    );
    assert_eq!(output, "10\n10\n20\n");
}

#[test]
fn test_iter_peekable_drains_in_for_loop() {
    // A Peekable iterator is still iterable; for-loop drains it
    // including any element already buffered by a prior peek().
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    for x in p {
        println(x);
    }
}
"#,
    );
    // Buffered 1 from peek; for-loop drains 1, 2, 3, 4.
    assert_eq!(output, "1\n1\n2\n3\n4\n");
}

#[test]
fn test_iter_peekable_count_drains_buffer() {
    // Terminal `count()` on a Peekable counts the buffered element
    // plus the rest of the inner iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut p = v.iter().peekable();
    println(p.peek().unwrap());
    println(p.count());
}
"#,
    );
    assert_eq!(output, "10\n5\n");
}

#[test]
fn test_iter_peekable_collect_includes_buffered() {
    // Round-trip: Peekable.collect() yields the same Vec as the
    // underlying iterator, even after a peek() has buffered an
    // element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let mut p = v.iter().peekable();
    let _ = p.peek();
    let xs: Vec[i64] = p.collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn test_iter_peekable_then_filter_drops_peek_capability_at_runtime() {
    // After .filter() the type is Iterator[T] (not Peekable), but
    // the underlying source chain still routes correctly — the
    // resulting iterator drains as if peekable() were a no-op
    // wrapper. This guards the runtime path that wraps the inner
    // for downstream adaptors.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let xs: Vec[i64] = v.iter().peekable().filter(|x| x % 2 == 1).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n5\n");
}

#[test]
fn test_iter_chunk_by_groups_consecutive_equal_keys() {
    // Consecutive items with the same parity group together.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 3, 5, 2, 4, 7, 9];
    for g in v.iter().chunk_by(|x| x % 2) {
        let mut s = "[";
        let mut first = true;
        for x in g {
            if not first { s = s + ","; }
            s = s + f"{x}";
            first = false;
        }
        s = s + "]";
        println(s);
    }
}
"#,
    );
    // Groups: [1,3,5] (odd), [2,4] (even), [7,9] (odd).
    assert_eq!(output, "[1,3,5]\n[2,4]\n[7,9]\n");
}

#[test]
fn test_iter_chunk_by_singleton_groups_when_all_keys_differ() {
    // Each item is its own group when key_fn returns a unique key.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut count = 0;
    for g in v.iter().chunk_by(|x| x) {
        for x in g {
            println(x);
        }
        count = count + 1;
    }
    println(f"groups:{count}");
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n4\ngroups:4\n");
}

#[test]
fn test_iter_chunk_by_one_group_when_all_keys_equal() {
    // Constant key — single group containing every element.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40];
    let mut count = 0;
    for g in v.iter().chunk_by(|x| 0) {
        for x in g {
            println(x);
        }
        count = count + 1;
    }
    println(f"groups:{count}");
}
"#,
    );
    assert_eq!(output, "10\n20\n30\n40\ngroups:1\n");
}

#[test]
fn test_iter_chunk_by_collects_into_vec_of_vec() {
    // Terminal collect() yields Vec[Vec[T]] — exercises the heap
    // allocation per group end-to-end.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunk_by(|x| x).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 groups: lengths 2, 3, 1.
    assert_eq!(output, "3\n2\n3\n1\n");
}

#[test]
fn test_iter_chunk_by_state_persists_across_next_calls() {
    // Calling next() one group at a time threads pending-item state
    // correctly across pulls.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 3, 3];
    let mut it = v.iter().chunk_by(|x| x);
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "2\n1\n2\ndone\n");
}

#[test]
fn test_iter_chunk_by_after_filter_only_groups_kept_items() {
    // Filter first, then chunk_by — the groups only include items
    // that passed the filter.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter()
        .filter(|x| x != 4)
        .chunk_by(|x| x < 5)
        .collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // After filter: [1, 2, 3, 5, 6, 7, 8]. Keys: T, T, T, F, F, F, F.
    // Groups: [1, 2, 3] (len 3), [5, 6, 7, 8] (len 4).
    assert_eq!(output, "2\n3\n4\n");
}

#[test]
fn test_iter_chunk_by_key_fn_fires_once_per_item() {
    // Side-effect prefix proves key_fn is called exactly once per
    // inner item — even though the same item's key is consulted
    // twice (when ending a group and when seeding the next).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2];
    let groups: Vec[Vec[i64]] = v.iter()
        .chunk_by(|x| {
            println(f"k:{x}");
            x
        })
        .collect();
    println(groups.len());
}
"#,
    );
    // key_fn fires once per element (not twice per boundary item).
    assert_eq!(output, "k:1\nk:1\nk:2\nk:2\n2\n");
}

#[test]
fn test_iter_chunk_by_with_take_short_circuits() {
    // Downstream take(n) limits how many groups we drain, so
    // chunk_by's inner pulls stop early.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 1, 2, 2, 3, 3, 4, 4];
    for g in v.iter().chunk_by(|x| x).take(2) {
        for x in g {
            println(x);
        }
    }
}
"#,
    );
    // First two groups only: [1, 1], [2, 2].
    assert_eq!(output, "1\n1\n2\n2\n");
}

#[test]
fn test_iter_chunk_by_on_empty_yields_no_groups() {
    // Empty source → no groups; for-loop body never runs.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1];
    let groups: Vec[Vec[i64]] = v.iter().take(0).chunk_by(|x| x).collect();
    println(f"groups:{groups.len()}");
}
"#,
    );
    assert_eq!(output, "groups:0\n");
}

#[test]
fn test_iter_chunks_groups_into_n_sized_pieces() {
    // Non-overlapping groups of n consecutive items; trailing
    // remainder is shorter when source isn't a multiple of n.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7];
    let groups: Vec[Vec[i64]] = v.iter().chunks(3).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 chunks: [1,2,3], [4,5,6], [7]. Lengths: 3, 3, 1.
    assert_eq!(output, "3\n3\n3\n1\n");
}

#[test]
fn test_iter_chunks_exact_multiple_yields_no_partial() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6];
    let groups: Vec[Vec[i64]] = v.iter().chunks(2).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // 3 chunks of size 2 each.
    assert_eq!(output, "3\n2\n2\n2\n");
}

#[test]
fn test_iter_chunks_n_larger_than_source_yields_one_partial() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunks(10).collect();
    println(groups.len());
    println(groups[0].len());
}
"#,
    );
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_iter_chunks_zero_clamps_to_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let groups: Vec[Vec[i64]] = v.iter().chunks(0).collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // n=0 clamps to n=1: 3 singleton chunks.
    assert_eq!(output, "3\n1\n1\n1\n");
}

#[test]
fn test_iter_chunks_state_persists_across_next() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [10, 20, 30, 40, 50];
    let mut it = v.iter().chunks(2);
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    println(it.next().unwrap().len());
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    assert_eq!(output, "2\n2\n1\ndone\n");
}

#[test]
fn test_iter_chunks_after_filter_only_groups_kept() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter()
        .filter(|x| x % 2 == 0)
        .chunks(2)
        .collect();
    println(groups.len());
    for g in groups {
        println(g.len());
    }
}
"#,
    );
    // After filter: [2, 4, 6, 8]. Chunks(2): [2,4], [6,8].
    assert_eq!(output, "2\n2\n2\n");
}

#[test]
fn test_iter_windows_slides_by_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
    for w in wins {
        let mut s = "[";
        let mut first = true;
        for x in w {
            if not first { s = s + ","; }
            s = s + f"{x}";
            first = false;
        }
        s = s + "]";
        println(s);
    }
}
"#,
    );
    // 3 windows: [1,2,3], [2,3,4], [3,4,5].
    assert_eq!(output, "3\n[1,2,3]\n[2,3,4]\n[3,4,5]\n");
}

#[test]
fn test_iter_windows_smaller_than_n_yields_nothing() {
    // No partial windows — when source is shorter than n, windows
    // emits zero items (matches Rust's [T].windows semantics).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
}
"#,
    );
    assert_eq!(output, "0\n");
}

#[test]
fn test_iter_windows_exactly_n_yields_one() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let wins: Vec[Vec[i64]] = v.iter().windows(3).collect();
    println(wins.len());
    println(wins[0].len());
}
"#,
    );
    assert_eq!(output, "1\n3\n");
}

#[test]
fn test_iter_windows_state_persists_across_next() {
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4];
    let mut it = v.iter().windows(2);
    println(it.next().unwrap()[0]);
    println(it.next().unwrap()[0]);
    println(it.next().unwrap()[0]);
    match it.next() {
        Some(_) => println("more"),
        None => println("done"),
    }
}
"#,
    );
    // 3 windows of size 2: [1,2], [2,3], [3,4]. First element each.
    assert_eq!(output, "1\n2\n3\ndone\n");
}

#[test]
fn test_iter_windows_zero_clamps_to_one() {
    // n=0 clamps to n=1 — degenerates to "each item as its own
    // singleton window" (still allocates per window).
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3];
    let wins: Vec[Vec[i64]] = v.iter().windows(0).collect();
    println(wins.len());
    for w in wins {
        println(w[0]);
    }
}
"#,
    );
    assert_eq!(output, "3\n1\n2\n3\n");
}

#[test]
fn test_iter_chunks_with_take_short_circuits() {
    // Downstream take(n) limits how many chunks we drain.
    let output = run_no_errors(
        r#"
fn main() {
    let v = [1, 2, 3, 4, 5, 6, 7, 8];
    let groups: Vec[Vec[i64]] = v.iter().chunks(2).take(2).collect();
    println(groups.len());
}
"#,
    );
    assert_eq!(output, "2\n");
}

// ── Range pattern matching (interpreter) ────────────────────────────
//
// All five forms — `..hi`, `lo..hi`, `lo..=hi`, `..=hi`, `lo..` — must
// match the value space correctly. Previously the interpreter had a
// "simplified" RangePattern arm that always returned `true`; this test
// pins the per-form semantics.

#[test]
fn test_range_pattern_matches_correctly() {
    let output = run_no_errors(
        r#"
fn classify(n: i32) -> i32 {
    match n {
        ..0 => -1,
        0..=9 => 1,
        10..100 => 2,
        100.. => 3,
        _ => 0,
    }
}
fn main() {
    println(classify(-5));
    println(classify(0));
    println(classify(5));
    println(classify(9));
    println(classify(10));
    println(classify(50));
    println(classify(99));
    println(classify(100));
    println(classify(200));
}
"#,
    );
    assert_eq!(output, "-1\n1\n1\n1\n2\n2\n2\n3\n3\n");
}

// ── Range / RangeInclusive as Iterator ─────────────────────────
//
// Range and RangeInclusive evaluate to `Value::Iterator` so the adaptor
// surface (`step_by`, `map`, `filter`, `take`, `collect`, ...) dispatches
// directly without a redundant `.iter()` layer. The for-loop iterable
// path drains `Value::Iterator` via `iterator_step`, so for-loop
// semantics are preserved.

#[test]
fn test_range_iter_step_by_collect() {
    // `(0..10).step_by(2).collect()` — half-open Range as Iterator.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..10).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n2\n4\n6\n8\n");
}

#[test]
fn test_range_inclusive_step_by_collect() {
    // `(1..=10).step_by(2).collect()` — RangeInclusive as Iterator pins
    // the inclusive-end semantics (10 is reachable but the stride
    // skips it).
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (1..=10).step_by(2).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "1\n3\n5\n7\n9\n");
}

#[test]
fn test_range_redundant_iter() {
    // `(0..5).iter().collect()` — the redundant iter() call on a Range
    // is a no-op pass-through. Pins the iter/into_iter early-return
    // guard (sub-step (d)) — without it, this hits an `unreachable!`
    // because Range now produces `Value::Iterator` rather than
    // `Value::Array`.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..5).iter().collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n3\n4\n");
}

#[test]
fn test_range_chained_adaptors() {
    // Multi-step adaptor composition through the new entry surface:
    // map doubles each element, filter keeps those > 5.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..10).map(|x| x * 2).filter(|x| x > 5).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "6\n8\n10\n12\n14\n16\n18\n");
}

#[test]
fn test_range_for_loop_unchanged() {
    // Regression — for-loop semantics must survive the Range → Iterator
    // eval change. Sums 0+1+2+3+4 = 10.
    let output = run_no_errors(
        r#"
fn main() {
    let mut s = 0;
    for x in 0..5 {
        s = s + x;
    }
    println(s);
}
"#,
    );
    assert_eq!(output, "10\n");
}

#[test]
fn test_collect_into_every_documented_from_iterator_target() {
    // ORACLE for B-2026-08-17-36. design.md § Iterator Adaptors: "Every
    // standard collection (`Vec`, `Map`, `Set`, `VecDeque`, `TreeMap`,
    // `String`) implements `FromIterator` for its natural element type."
    // `collect()` used to produce `Vec[T]` and nothing else, so every one of
    // these annotations was rejected as a mismatch the user had caused.
    //
    // The two `String` legs are the point of the uniform lowering: the element
    // is a `char` in one and a `String` in the other, and a pre-typecheck
    // desugar cannot tell them apart -- so both go through
    // `push_str(it.to_string())`. `TreeMap`, the sixth target, cannot be named
    // at all yet (B-2026-08-17-38) and so has no leg here.
    //
    // `Vec` is included to pin the NON-rewrite: it must still take the plain
    // path, untouched by the desugar.
    let output = run_no_errors(
        r#"
fn main() {
    let s = "hi there";
    let up: String = s.chars().map(|c| c.to_uppercase()).collect();
    println(up);

    let mut words: Vec[String] = Vec.new();
    words.push("ab");
    words.push("cd");
    let joined: String = words.iter().map(|w| w.to_uppercase()).collect();
    println(joined);

    let mut raw: Vec[i64] = Vec.new();
    raw.push(1); raw.push(2); raw.push(2); raw.push(3);

    let plain: Vec[i64] = raw.iter().map(|x| x * 2).collect();
    println(plain.len());

    let st: Set[i64] = raw.iter().map(|x| x).collect();
    println(st.len());

    let dq: VecDeque[i64] = raw.iter().map(|x| x * 10).collect();
    println(dq.len());
    println(dq[0]);

    let mp: Map[i64, i64] = raw.iter().map(|x| (x, x * 100)).collect();
    println(mp.len());
    println(mp[3]);

    let empty: Vec[i64] = Vec.new();
    let es: Set[i64] = empty.iter().map(|x| x).collect();
    println(es.len());
}
"#,
    );
    // raw has 4 elements with one duplicate: Vec keeps 4, Set collapses to 3,
    // VecDeque keeps 4 in order (first = 10), Map keys collapse to 3.
    assert_eq!(output, "HI THERE\nABCD\n4\n3\n4\n10\n3\n300\n0\n");
}

#[test]
fn test_collect_desugar_leaves_a_user_defined_collect_alone() {
    // The guard on B-2026-08-17-36's desugar. `collect` is an ordinary method
    // name, so a user type may define one returning a `Set`/`Map`/`String`.
    // Rewriting that would re-iterate an ALREADY-BUILT collection into a fresh
    // one -- silently wrong rather than loudly wrong. The desugar therefore
    // fires only when the receiver is a recognized iterator source or adaptor;
    // a plain identifier receiver like `b` below is not one.
    let output = run_no_errors(
        r#"
struct Bag { n: i64 }

impl Bag {
    fn collect(self) -> Set[i64] {
        let mut s: Set[i64] = Set.new();
        s.insert(self.n);
        return s;
    }
}

fn main() {
    let b = Bag { n: 7 };
    let s: Set[i64] = b.collect();
    println(s.len());
}
"#,
    );
    assert_eq!(output, "1\n");
}

#[test]
fn test_range_inclusive_take() {
    // `(0..=100).take(3).collect()` pins the eager-snapshot truncation
    // under `take` — the source pre-materialises 101 items but the
    // step-layer adaptor pulls only the first three.
    let output = run_no_errors(
        r#"
fn main() {
    let xs: Vec[i64] = (0..=100).take(3).collect();
    for x in xs {
        println(x);
    }
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n");
}

// ── Slice[T] Iterator impl ─────────────────────────────────────
//
// `Slice[T]` IS `Iterator[T]` — `s.iter()` and `s.into_iter()` route
// through the same Iterator dispatch as `Vec.iter()`, so chained
// adaptors compose. The for-loop iterable path drains `Value::Slice`
// directly, so `for x in s { ... }` keeps working without `.iter()`.
// Sibling to the Range / RangeInclusive Iterator entry above.

#[test]
fn test_interpreter_slice_iter_basic() {
    // `s.iter()` over a borrowed slice yields each element; sums to 6.
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let mut sum = 0;
    for x in s.iter() {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_interpreter_slice_iter_chain_with_filter_sum() {
    // `s.iter().filter(|x| x % 2 == 0).fold(0, |a, b| a + b)` returns 2.
    // Sums via `fold` since `Iterator.sum` is not on the shipped terminal
    // surface (`next` / `count` / `collect` / `fold` / `any` / `all`).
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let total: i64 = s.iter().filter(|x| x % 2 == 0).fold(0, |a, b| a + b);
    println(total);
}
"#,
    );
    assert_eq!(output, "2\n");
}

#[test]
fn test_interpreter_slice_into_iter_works() {
    // `s.into_iter()` round-trips identical to `s.iter()` at the
    // tree-walk layer (the borrow-vs-consume distinction is a
    // typechecker concern; sums 1+2+3 = 6).
    let output = run_no_errors(
        r#"
fn main() {
    let v = Vec[1, 2, 3];
    let s: Slice[i64] = v.as_slice();
    let mut sum = 0;
    for x in s.into_iter() {
        sum = sum + x;
    }
    println(sum);
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn test_interpreter_slice_iter_empty_slice() {
    // Empty slice iterator yields no elements; `.collect()` returns [].
    let output = run_no_errors(
        r#"
fn main() {
    let v: Vec[i64] = Vec.new();
    let s: Slice[i64] = v.as_slice();
    let xs: Vec[i64] = s.iter().collect();
    println(xs.len());
}
"#,
    );
    assert_eq!(output, "0\n");
}

#[test]
fn test_range_pattern_const_bounds_int() {
    // Const-named range bounds (design.md § Range Patterns) match the same
    // as the literal forms once resolved.
    let output = run_no_errors(
        r#"
const LO: i64 = 10;
const HI: i64 = 20;
fn classify(n: i64) -> i64 {
    match n {
        ..LO => 1,
        LO..=HI => 2,
        _ => 3,
    }
}
fn main() {
    println(classify(5));
    println(classify(10));
    println(classify(15));
    println(classify(20));
    println(classify(25));
}
"#,
    );
    assert_eq!(output, "1\n2\n2\n2\n3\n");
}

#[test]
fn test_range_pattern_const_bounds_char() {
    let output = run_no_errors(
        r#"
const LOWER_A: char = 'a';
const LOWER_Z: char = 'z';
fn is_lower(c: char) -> i64 {
    match c { LOWER_A..=LOWER_Z => 1, _ => 0 }
}
fn main() {
    println(is_lower('m'));
    println(is_lower('M'));
}
"#,
    );
    assert_eq!(output, "1\n0\n");
}

/// B-2026-08-14-9, interpreter half — a `mut Slice[T]` mutator must write
/// through to the collection it borrows.
///
/// `swap` was the ONLY method behind the mut-Slice fence in `method_call.rs`;
/// `fill`, `reverse`, `sort`, `sort_by` and `sort_by_key` fell through to the
/// Slice→Array normalization, ran against that fresh snapshot, and had their
/// results discarded. The row that filed this called all eight
/// "interp-correct" — for these five that was not true, and the failure was
/// strictly worse than the codegen gap it was filed for: a build error is
/// loud, `xs.sort()` doing nothing is not.
///
/// The PARTIAL-WINDOW line is the one that pins the fix rather than just the
/// symptom. Writing back a whole snapshot over `storage[0..len]` would look
/// right for a slice of the entire collection and silently corrupt one that
/// starts at an offset; here the receiver is the second half of a
/// `split_at_mut`, so the first half must come back untouched.
#[test]
fn mut_slice_mutators_write_through_to_the_collection() {
    let out = run_no_errors(
        r#"
fn msort(xs: mut Slice[i64]) { xs.sort(); }
fn mrev(xs: mut Slice[i64]) { xs.reverse(); }
fn mfill(xs: mut Slice[i64]) { xs.fill(9i64); }
fn mkey(xs: mut Slice[i64]) { xs.sort_by_key(|x| 0i64 - x); }
fn mby(xs: mut Slice[i64]) { xs.sort_by(|a, b| b.cmp(a)); }
fn mswap(xs: mut Slice[i64]) { xs.swap(0i64, 3i64); }

fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(3i64); a.push(1i64); a.push(2i64); a.push(5i64);
    msort(a.as_slice_mut());
    println(f"01 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mrev(a.as_slice_mut());
    println(f"02 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mkey(a.as_slice_mut());
    println(f"03 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mby(a.as_slice_mut());
    println(f"04 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mswap(a.as_slice_mut());
    println(f"05 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");
    mfill(a.as_slice_mut());
    println(f"06 {a[0i64]} {a[1i64]} {a[2i64]} {a[3i64]}");

    let mut b: Vec[i64] = Vec.new();
    b.push(9i64); b.push(8i64); b.push(3i64); b.push(1i64); b.push(2i64);
    let mut bs = b.as_slice_mut();
    let h = bs.split_at_mut(2i64);
    msort(h.1);
    println(f"07 {b[0i64]} {b[1i64]} {b[2i64]} {b[3i64]} {b[4i64]}");
}
"#,
    );
    assert_eq!(
        out,
        "01 1 2 3 5\n\
         02 5 3 2 1\n\
         03 5 3 2 1\n\
         04 5 3 2 1\n\
         05 1 3 2 5\n\
         06 9 9 9 9\n\
         07 9 8 1 2 3\n"
    );
}

#[test]
fn direct_vec_iterator_terminals_survive_being_chained() {
    // B-2026-08-11-19, interpreter leg. The desugar of `v.max()` into
    // `v.iter().max()` is shared by both backends, and its gate used to be
    // keyed by the call's own span — which the parser sets equal to the
    // receiver's, so a chain collapsed to one key and the last write won. One
    // extra chained call then skipped the rewrite and the interpreter reported
    // "method 'max' not found on type 'Vec' (no interpreter dispatch arm)",
    // blaming a method that was fine.
    assert_eq!(
        run("fn main() {\n\
                 let xs: Vec[i64] = [1, 5, 3];\n\
                 println(xs.max().unwrap());\n\
                 println(\"max=\" + xs.max().unwrap().to_string());\n\
                 let s = xs.min().unwrap().to_string();\n\
                 println(s);\n\
                 println(\"sum=\" + xs.sum().to_string());\n\
                 println(\"prod=\" + xs.product().to_string());\n\
             }\n"),
        "5\nmax=5\n1\nsum=9\nprod=15\n"
    );
}

/// B-2026-08-18-14 — the interpreter oracle for a RANGE SUBSCRIPT used
/// directly as a METHOD RECEIVER. `v[0..3].first_or(-1)` failed the BUILD with
/// "no handler for expression kind Range" while `karac check` accepted it and
/// the interpreter ran it, so this side is what the compiled fix was measured
/// against — a check/build divergence, not a wrong answer.
///
/// BOTH SPELLINGS, because before the fix they failed for DIFFERENT reasons.
/// With `.to_string()` chained on, the postfix span collapse recorded the
/// chain's String-ness against the `Vec` receiver's own span key, so
/// `compile_index` built a String slice where a `{ptr,len}` view was due. With
/// the call split across two statements there is no chain to collapse, and what
/// remained was a gate admitting only BUILTIN slice method names — so a user
/// `impl … for Slice[i64]` method was declined and fell through to the
/// element-pointer lowering. One line each, so a regression in either cause
/// shows up on its own line.
#[test]
fn test_range_subscript_as_method_receiver_oracle() {
    let out = run_no_errors(
        "trait Head { fn first_or(ref self, d: i64) -> i64; }\n\
         impl Head for Slice[i64] {\n\
             fn first_or(ref self, d: i64) -> i64 {\n\
                 if self.len() == 0 { return d; }\n\
                 return self[0];\n\
             }\n\
         }\n\
         fn main() {\n\
             let v: Vec[i64] = [10, 20, 30];\n\
             println(v[0..3].first_or(-1).to_string());\n\
             let r = v[1..3].first_or(-1);\n\
             println(r.to_string());\n\
             let empty: Vec[i64] = [];\n\
             println(empty[0..0].first_or(-1).to_string());\n\
         }\n",
    );
    assert_eq!(out, "10\n20\n-1\n");
}

/// B-2026-08-18-18 — the interpreter oracle for `collect()` into a non-`Vec`
/// target in RETURN position, and in a function body's TAIL.
///
/// design.md says `collect()` "infers the target type from context";
/// B-2026-08-17-36 delivered that for an annotated `let`, and these are two of
/// the positions it left fixed to `Vec` ("expected 'Set[i64]', found
/// 'Vec[i64]'"). Both are pure typecheck failures, so the interpreter answers
/// here are what the rewrite has to reproduce.
///
/// THE CLOSURE CASE IS THE POINT OF THE THIRD FUNCTION. A `return` inside a
/// closure returns from the CLOSURE, so the enclosing fn's declared return type
/// must not reach it — rewriting there would aim at `Set[i64]` where the
/// closure genuinely yields `Vec[i64]`, and would look like it worked.
#[test]
fn test_collect_target_in_return_and_tail_position_oracle() {
    let out = run_no_errors(
        "fn build_ret(v: Vec[i64]) -> Set[i64] {\n\
             return v.iter().map(|x| x * 2).collect();\n\
         }\n\
         fn build_tail(v: Vec[i64]) -> VecDeque[i64] {\n\
             v.iter().map(|x| x + 1).collect()\n\
         }\n\
         fn apply(f: Fn(i64) -> Vec[i64], n: i64) -> Vec[i64] { return f(n); }\n\
         fn closure_return_is_the_closures(v: Vec[i64]) -> Set[i64] {\n\
             let f = |x: i64| { return v.iter().map(|y| y + x).collect(); };\n\
             let inner = apply(f, 10);\n\
             let mut out: Set[i64] = Set.new();\n\
             for e in inner { out.insert(e); }\n\
             return out;\n\
         }\n\
         fn main() {\n\
             let v: Vec[i64] = [1, 2, 3];\n\
             let a = build_ret(v);\n\
             println(a.len().to_string());\n\
             let b = build_tail(v);\n\
             println(b.len().to_string());\n\
             let c = closure_return_is_the_closures(v);\n\
             println(c.len().to_string());\n\
         }\n",
    );
    assert_eq!(out, "3\n3\n3\n");
}

/// B-2026-08-18-27 — the interpreter oracle for `collect()` into a non-`Vec`
/// target in ARGUMENT position, the third and last of design.md's "infers the
/// target type from context" positions.
///
/// The rewrite is the same pre-typecheck desugar as the `let` and `return`
/// halves, so this side is the reference the compiled backends must match.
///
/// THE `ambush` CALL IS THE POINT. A local closure shadows the top-level
/// `fn ambush(s: Set[i64])`, so the rewrite must NOT fire: `4` is the closure's
/// answer over the four-element `Vec`, `3` would mean the argument was aimed at
/// the shadowed function's `Set` parameter and a working program had been
/// turned into a typecheck error.
#[test]
fn test_collect_target_in_argument_position_oracle() {
    let out = run_no_errors(
        "fn take_set(s: Set[i64]) -> i64 { return s.len() as i64; }\n\
         fn take_deque(d: VecDeque[i64]) -> i64 { return d.len() as i64; }\n\
         fn take_map(m: Map[i64, i64]) -> i64 { return m.len() as i64; }\n\
         fn take_str(s: String) -> i64 { return s.len() as i64; }\n\
         fn ambush(s: Set[i64]) -> i64 { return s.len() as i64; }\n\
         fn main() {\n\
             let v: Vec[i64] = [3, 1, 3, 2];\n\
             println(take_set(v.iter().collect()).to_string());\n\
             println(take_deque(v.iter().map(|x| x + 1).collect()).to_string());\n\
             println(take_map(v.iter().map(|x| (x, x * 2)).collect()).to_string());\n\
             let w: Vec[String] = [\"ab\", \"cd\"];\n\
             println(take_str(w.iter().collect()).to_string());\n\
             let s: String = \"xyz\";\n\
             println(take_str(s.chars().collect()).to_string());\n\
             let ambush = |x: Vec[i64]| x.len() as i64;\n\
             println(ambush(v.iter().collect()).to_string());\n\
         }\n",
    );
    assert_eq!(out, "3\n4\n3\n4\n3\n4\n");
}

/// A NEGATIVE reserve must be a no-op on every collection. This is not
/// symmetry for its own sake: the `Map`/`Set` path goes through an FFI
/// boundary, and typing that parameter `u64` instead of `i64` reinterprets
/// `reserve(-5)` as a reservation of 18 quintillion entries — measured, it
/// aborted the process with `panic: out of memory`.
#[test]
fn a_negative_reserve_is_a_no_op_on_every_collection() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    v.reserve(-100);
    let mut s: String = String.new();
    s.push_str("k");
    s.reserve(-100);
    let mut m: Map[i64, i64] = Map.new();
    m.insert(1, 1);
    m.reserve(-100);
    let mut t: Set[i64] = Set.new();
    t.insert(1);
    t.reserve(-100);
    println(f"{v.len()} [{s}] {m.len()} {t.len()}");
}
"#);
    assert_eq!(out, "1 [k] 1 1\n");
}

/// `Vec.from_iter(it)` IS `it.collect()` — the typechecker types it by
/// inferring that synthetic call and lowering rewrites to it, so the two
/// spellings run one implementation. The last case has no type annotation,
/// which `collect` alone cannot do.
#[test]
fn vec_from_iter_matches_collect_across_iterator_shapes() {
    let out = run(r#"
fn main() {
    let a: Vec[i64] = Vec.from_iter(0..4);
    println(f"{a.len()} {a[0]} {a[3]}");
    let b: Vec[i64] = Vec.from_iter((0..6).map(|x| x * 2));
    println(f"{b.len()} {b[5]}");
    let src: Vec[i64] = [5, 6, 7];
    let c: Vec[i64] = Vec.from_iter(src.iter().map(|x| x + 1));
    println(f"{c.len()} {c[0]} {c[2]}");
    let d = Vec.from_iter(0..3);
    println(f"{d.len()} {d[2]}");
}
"#);
    assert_eq!(out, "4 0 3\n6 10\n3 6 8\n3 2\n");
}

// ── B-2026-08-26-27: the fallible collect ───────────────────────

/// `Vec.try_from_iter` accepts exactly the iterator shapes `.collect()` does,
/// because it IS the collect engine with a fallible switch rather than a
/// parallel lowering. The interpreter always answers `Ok` — its host allocator
/// does not OOM — which is the same contract every other `try_*` companion has
/// in this backend.
#[test]
fn vec_try_from_iter_collects_every_shape_collect_accepts() {
    let out = run(r#"
fn build() -> Result[i64, AllocError] {
    let a: Vec[i64] = Vec.try_from_iter(0..4)?;
    println(f"{a.len()} {a[0]} {a[3]}");
    let b: Vec[i64] = Vec.try_from_iter((0..6).map(|x| x * 2))?;
    println(f"{b.len()} {b[5]}");
    let src: Vec[i64] = [5, 6, 7];
    let c: Vec[i64] = Vec.try_from_iter(src.iter().map(|x| x + 1))?;
    println(f"{c.len()} {c[2]}");
    return Ok(a.len() + b.len() + c.len());
}
fn main() {
    match build() {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("oom"); }
    }
}
"#);
    assert_eq!(out, "4 0 3\n6 10\n3 8\nok 13\n");
}

/// Heap elements, where the accumulator owns per-element buffers rather than a
/// flat spine of scalars. This covers the `Ok` path only — the interpreter has
/// no allocation-failure path to take, so the partial-container question lives
/// with the codegen backend. `asan_vec_try_from_iter_over_heap_elements_no_leak`
/// is the fixture that answers it there.
#[test]
fn vec_try_from_iter_handles_heap_elements() {
    let out = run(r#"
fn build() -> Result[i64, AllocError] {
    let src: Vec[String] = ["a", "bb", "ccc"];
    let out: Vec[String] = Vec.try_from_iter(src.iter().map(|s| s.to_uppercase()))?;
    println(f"{out.len()} {out[0]} {out[2]}");
    return Ok(out.len());
}
fn main() {
    match build() {
        Ok(n) => { println(f"ok {n}"); }
        Err(e) => { println("oom"); }
    }
}
"#);
    assert_eq!(out, "3 A CCC\nok 3\n");
}

/// B-2026-09-01-19 — the shapes that stay ACCEPTED around the new range check
/// agree on every backend.
///
/// The rejected shapes have no runtime left to compare, so what needs pinning
/// is the boundary the check draws: an in-range suffixed literal in a seeded
/// payload slot still compiles and still means what it says, and the `as u8`
/// an author writes to mean the truncation gives the same 44 everywhere rather
/// than the `300`/`44` split the unchecked literal used to.
/// `255i64` sits on the boundary itself, which is where an off-by-one in the
/// range comparison would show. Verified byte-identical under
/// `karac run --interp`, `karac run`, `karac build`, and
/// `KARAC_AUTO_PAR=0 karac build`.
#[test]
fn test_seeded_slot_range_check_boundary_agrees_across_backends() {
    assert_eq!(
        run(r#"fn main() {
    let e: Option[u8] = Option.Some(5i64);
    println(f"{e}");
    let g: Option[u8] = Option.Some(255i64);
    println(f"{g}");
    let h: Option[u8] = Option.Some(300i64 as u8);
    println(f"{h}");
}
"#),
        "Some(5)\nSome(255)\nSome(44)\n"
    );
}

/// B-2026-09-19-41 — the interpreter twin of `tests/codegen.rs`'s
/// `e2e_named_struct_optres_payload_part_drops_at_its_own_live_range_end`.
/// Byte-identical source; the expectation differs in exactly the two cells
/// that row pins divergent (`nomove` and `two`), where the interpreter runs
/// the payload body ONCE and the compiled backends run it twice. Everything
/// else agrees, and this output is unchanged by the commit — the fix is
/// codegen-only, which is what the twin is here to hold.
#[test]
fn test_named_struct_optres_payload_part_drops_at_its_own_live_range_end() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct P { r: R, n: i64 }
struct Q { r: R, s: R }
struct H { name: String, id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"  dH{self.id}") } }
struct Ph { h: H, n: i64 }

fn eat(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
fn eat_after(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } println("  after") }
fn eat_two(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
fn eat_heap(o: Option[Ph]) { match o { Option.Some(t) => { let x = t.h; println("  mid") } Option.None => { println("  n") } } }
fn eat_tuple(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println("  mid") } Option.None => { println("  n") } } }
fn eat_nomove(o: Option[P]) { match o { Option.Some(t) => { println(f"  peek{t.n}") } Option.None => { println("  n") } } }

struct Sink { tag: i64 }
impl Sink {
  fn take(ref self, o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
  fn grab(o: Option[P]) { match o { Option.Some(t) => { let x = t.r; println("  mid") } Option.None => { println("  n") } } }
}

fn main() {
  println("named")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat(a) }
  println("  out")

  println("temp")
  eat(Option.Some(P { r: R { id: 5 }, n: 9 }))
  println("  out")

  println("after")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat_after(a) }
  println("  out")

  println("heap")
  { let a = Option.Some(Ph { h: H { name: "n5", id: 5 }, n: 9 }); eat_heap(a) }
  println("  out")

  println("tuple")
  { let a = Option.Some((R { id: 5 }, 9)); eat_tuple(a) }
  println("  out")

  println("nomove")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); eat_nomove(a) }
  println("  out")

  println("method")
  { let s = Sink { tag: 1 }; let a = Option.Some(P { r: R { id: 5 }, n: 9 }); s.take(a) }
  println("  out")

  println("assoc")
  { let a = Option.Some(P { r: R { id: 5 }, n: 9 }); Sink.grab(a) }
  println("  out")

  println("two")
  { let a = Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }); eat_two(a) }
  println("  out")

  println("none")
  { let a: Option[P] = Option.None; eat(a) }
  println("  out")

  println("end")
}
"#);
    assert_eq!(out, "named\n  dR5\n  mid\n  out\ntemp\n  dR5\n  mid\n  out\nafter\n  dR5\n  mid\n  after\n  out\nheap\n  dH5\n  mid\n  out\ntuple\n  dR5\n  mid\n  out\nnomove\n  peek9\n  dR5\n  out\nmethod\n  dR5\n  mid\n  out\nassoc\n  dR5\n  mid\n  out\ntwo\n  dR5\n  mid\n  dR6\n  out\nnone\n  n\n  out\nend\n", "got:\n{out}");
}
