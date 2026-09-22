//! GPU dispatch, tensors, autograd -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter gpu_tensor::
//!
//! New fixtures about GPU dispatch, tensors, autograd belong in this file.

use super::*;

#[test]
fn test_method_call_on_a_healthy_receiver_still_dispatches() {
    // The converse of the guard above, and the thing it could plausibly break:
    // the short-circuit keys on `pending_cf`, so a method call reached with NO
    // pending fault must dispatch exactly as before — including one whose
    // receiver is itself a fallible expression that happened to succeed.
    assert_eq!(
        run_no_errors(
            "fn main() {\n\
                 let mut v: Vec[Vec[i64]] = Vec.new();\n\
                 v.push([1i64, 2i64, 3i64]);\n\
                 let o: Option[i64] = Option.Some(7i64);\n\
                 println(f\"{v[0].len()} {o.unwrap()}\");\n\
             }"
        )
        .trim(),
        "3 7"
    );
}

#[test]
fn test_user_impl_display_dispatches_through_to_string() {
    // A user `impl Display { fn to_string(ref self) -> String }` must win over
    // the built-in renderer across all three Display positions — `.to_string()`,
    // f-string interpolation, and `println(x)` — for both enums (incl. payload
    // variants) and structs. GAP-W4 (operator-trait gate lifted for Display).
    let src = "enum Color { Red, Green, Blue }
        impl Display for Color {
            fn to_string(ref self) -> String {
                match self { Red => \"red\", Green => \"green\", Blue => \"blue\" }
            }
        }
        enum Msg { Info(i64), Quit }
        impl Display for Msg {
            fn to_string(ref self) -> String {
                match self { Info(c) => f\"info#{c}\", Quit => \"quit\" }
            }
        }
        struct Point { x: i64, y: i64 }
        impl Display for Point {
            fn to_string(ref self) -> String { f\"({self.x}, {self.y})\" }
        }
        fn main() {
            let c = Color.Green;
            println(c.to_string());      // green
            println(f\"c={c}\");           // c=green
            println(c);                  // green
            let m = Msg.Info(7);
            let q = Msg.Quit;
            println(f\"{m} {q}\");          // info#7 quit
            let p = Point { x: 3, y: 4 };
            println(f\"p={p}\");            // p=(3, 4)
        }";
    let out = run_no_errors(src);
    assert!(out.contains("green\n"), "to_string user impl: {out}");
    assert!(out.contains("c=green\n"), "f-string user impl: {out}");
    assert!(
        out.contains("info#7 quit\n"),
        "payload-enum + unit-variant user impl: {out}"
    );
    assert!(out.contains("p=(3, 4)\n"), "struct user impl: {out}");
    // No built-in variant names leak through.
    assert!(!out.contains("Green"), "built-in must not leak: {out}");
    assert!(!out.contains("Info"), "built-in must not leak: {out}");
}

#[test]
fn a_derived_display_is_unaffected_by_the_depth_dispatch() {
    // Control for the two tests above: the depth dispatch fires ONLY for a
    // hand-written impl. A `#[derive(Display)]` type keeps the derived shape at
    // every depth, so the fix cannot have shifted any existing rendering.
    let src = "#[derive(Display)]
        struct Plain { n: i64 }
        fn main() {
            let p = Plain { n: 5 };
            println(f\"top={p}\");
            println(f\"vec={[Plain { n: 5 }]}\");
            println(f\"opt={Some(Plain { n: 5 })}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "top=Plain { n: 5 }\nvec=[Plain { n: 5 }]\nopt=Some(Plain { n: 5 })\n"
    );
}

#[test]
fn test_no_arm_match_degrades_to_runtime_error_not_panic() {
    // B-2026-07-17-6 defense-in-depth: if a match ever reaches
    // `eval_match` with no arm matching (a front-end gap), the interpreter
    // must degrade to a clean runtime diagnostic rather than a Rust
    // `unreachable!` panic. `Some(v)`/`None` against an `i64` is exactly such
    // a program — the typechecker now rejects it, but `run_program_full` runs
    // the interpreter regardless of type errors, so this exercises the
    // degraded path directly. Before the fix this panicked the interp thread.
    let errors = runtime_errors(
        "fn main() {\n\
             let x: i64 = 5;\n\
             match x {\n\
                 Some(v) => println(v),\n\
                 None => println(\"none\"),\n\
             }\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("non-exhaustive match")),
        "no-arm match must record a runtime error, got: {errors:?}"
    );
}

// A `Tensor` serializes as the canonical `arrow.fixed_shape_tensor` extension
// (single-row FixedSizeList + shape metadata) and round-trips its SHAPE as
// well as its values — the shape rides the field's extension metadata.
#[test]
fn test_tensor_arrow_ipc_roundtrip_shape_and_values() {
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let bytes = t.to_arrow_ipc();\n\
             println(bytes.len() > 8);\n\
             let u: Tensor[i64, [2, 3]] = Tensor.from_arrow_ipc(bytes);\n\
             println(u.rank());\n\
             let s = u.shape();\n\
             println(s[0]);\n\
             println(s[1]);\n\
             println(u.sum());\n\
         }",
    );
    // non-empty stream; rank 2; shape [2, 3]; sum 1+..+6 = 21.
    assert_eq!(out, "true\n2\n2\n3\n21\n");
}

#[test]
fn test_tensor_arrow_ipc_roundtrip_f64() {
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.5, 2.5], [3.0, 4.0]]);\n\
             let bytes = t.to_arrow_ipc();\n\
             let u: Tensor[f64, [2, 2]] = Tensor.from_arrow_ipc(bytes);\n\
             println(u.rank());\n\
             let s = u.shape();\n\
             println(s[0]);\n\
             println(s[1]);\n\
             println(u.sum());\n\
         }",
    );
    assert_eq!(out, "2\n2\n2\n11\n");
}

// ── Weak references on shared struct fields ────────────────────

#[test]
fn test_weak_container_element_upgrades_like_a_weak_field() {
    // B-2026-08-08-14 — the interpreter modelled `weak` per struct FIELD
    // (dedicated maps on `SharedStructInner`, upgraded at the field-read site)
    // and had no home for a weak CONTAINER element, so a `Vec[weak T]` push
    // stored an ordinary STRONG handle. Two consequences: a cycle through the
    // container was uncollectable here, and the element read handed a bare
    // struct to a `match` expecting `Option[T]` — reported as "non-exhaustive
    // match ... the typechecker should have rejected this", which blamed the
    // wrong phase for a gap that was the interpreter's own.
    //
    // Both rows must agree with codegen, which is what
    // `test_e2e_vec_of_weak_read_back_upgrades_and_leaves_the_target_intact`
    // pins on the other side. The second row is the one that makes it a WEAK
    // ref rather than an awkward strong one: the target dies with the helper's
    // frame, so the read must report `None`.
    assert_eq!(
        run("shared struct N { mut v: i64 }\n             fn main() {\n                 let a: N = N { v: 41i64 };\n                 let mut w: Vec[weak N] = Vec.new();\n                 w.push(a);\n                 println(a.v);\n                 match w[0] { Some(x) => { println(x.v); } None => { println(0 - 1); } }\n             }\n"),
        "41\n41\n"
    );
    assert_eq!(
        run("shared struct N { mut v: i64 }\n             fn fill(w: mut ref Vec[weak N]) {\n                 let a: N = N { v: 7i64 };\n                 w.push(a);\n                 match w[0] { Some(x) => { println(x.v); } None => { println(0 - 1); } }\n             }\n             fn main() {\n                 let mut w: Vec[weak N] = Vec.new();\n                 fill(mut w);\n                 match w[0] { Some(x) => { println(x.v); } None => { println(0 - 2); } }\n             }\n"),
        "7\n-2\n"
    );
}

#[test]
fn test_weak_map_value_insert_downgrades_and_collects_the_cycle() {
    // B-2026-08-08-29, interpreter leg — the `Map` twin of the `Vec[weak T]`
    // push above. `Map.insert` stored an ordinary STRONG handle, so a
    // Map-mediated cycle was uncollectable here just as it leaked under
    // codegen. `impl Drop` is the observable: the bodies only run if the two
    // `Arc`s actually reach zero, so a regression that reinstates the strong
    // store prints nothing between "start" and "end".
    assert_eq!(
        run("shared struct N { mut v: i64, mut kids: Map[i64, weak N] }\n             impl Drop for N { fn drop(mut ref self) { println(f\"drop {self.v}\"); } }\n             fn cycle() {\n                 let p = N { v: 1i64, kids: Map.new() };\n                 let c = N { v: 2i64, kids: Map.new() };\n                 c.kids.insert(0i64, p);\n                 p.kids.insert(0i64, c);\n             }\n             fn main() { println(\"start\"); cycle(); println(\"end\"); }\n"),
        "start\ndrop 2\ndrop 1\nend\n"
    );
}

#[test]
fn test_weak_field_upgrade_observes_strong_field_data() {
    // The Some arm of a weak upgrade is a normal SharedStruct handle —
    // its other fields are reachable as usual. Pins that the Arc
    // returned by Weak::upgrade carries the full referent contents,
    // not a stub.
    assert_eq!(
        run("shared struct Parent { id: i64, mut count: i64 }\n\
             shared struct Child { mut weak parent: Parent }\n\
             fn main() {\n\
                 let p = Parent { id: 5, count: 100 };\n\
                 let c = Child { parent: p };\n\
                 match c.parent {\n\
                     Some(parent_ref) => { parent_ref.count = parent_ref.count + 1; println(p.count); },\n\
                     None => println(\"dangling\"),\n\
                 }\n\
             }"),
        "101\n"
    );
}

#[test]
fn test_interp_tensor_u64_sorted_and_argsort_unsigned() {
    let output = run(
        "fn main() { let t: Tensor[u64, [3]] = Tensor.from([1u64 << 63, 5u64, 1u64 << 62]); \
         let s = t.sorted(); println(f\"{s[0]},{s[1]},{s[2]}\"); \
         let a = t.argsort(); println(f\"{a[0]},{a[1]},{a[2]}\"); }",
    );
    assert_eq!(output, "5,4611686018427387904,9223372036854775808\n1,2,0\n");
}

// ── `with_provider` runtime + resource method dispatch ───────────

#[test]
fn test_with_provider_dispatches_resource_method_to_top_of_stack() {
    let output = run("effect resource UserDB;
         struct FakeDB { data: i64 }
         impl FakeDB { fn query(self, n: i64) -> i64 { self.data + n } }
         fn main() {
             with_provider[UserDB](FakeDB { data: 100 }, || {
                 println(UserDB.query(5));
             });
         }");
    assert_eq!(output, "105\n");
}

#[test]
fn test_string_push_str_interpreter_dispatch() {
    // `push_str` typecheck + codegen shipped 2026-05-23, but the
    // interpreter dispatch arm was missing — `karac run` would panic
    // on the unreachable arm with "method 'push_str' not found on type
    // 'String'". This regression makes sure the same Kāra source runs
    // through both backends with identical output.
    let output = run("fn main() {\n\
             let mut s: String = \"\";\n\
             s.push_str(\"foo\");\n\
             s.push_str(\"bar\");\n\
             println(s);\n\
             println(s.len());\n\
         }");
    assert_eq!(output, "foobar\n6\n");
}

// ── std.cli — subcommands + auto --help / --version (C1 slice) ─────

#[test]
fn test_cli_subcommand_dispatches_into_sub_parser() {
    // The deferred.md sample: parent has `--name`, subcommand `upper`
    // has its own `--shout` flag. argv `["prog", "--name", "alice",
    // "upper", "--shout"]` should populate parent's `--name = "alice"`
    // and dispatch into `upper` with `--shout` set.
    let output = run(r#"struct FakeEnv {}
         impl FakeEnv { fn args(self) -> Vec[String] { ["prog", "--name", "alice", "upper", "--shout"] } }
         fn main() {
             with_provider[Env](FakeEnv {}, || {
                 let parser = Parser.new("greet")
                     .arg("--name", Arg.string().required())
                     .subcommand("upper", Parser.new("upper").flag("--shout", short: 's', help: ""));
                 match parser.parse() {
                     Ok(args) => {
                         match args.get_string("--name") {
                             Ok(n) => println(n),
                             Err(e) => println(e.message),
                         }
                         match args.subcommand_name() {
                             Some(name) => println(name),
                             None => println("no_sub"),
                         }
                         match args.sub {
                             Some(s) => println(s.get_flag("--shout")),
                             None => println("no_sub"),
                         }
                     }
                     Err(e) => println(e.message),
                 }
             });
         }"#);
    assert_eq!(output, "alice\nupper\ntrue\n");
}

#[test]
fn test_tensor_from_arrow_ipc_rejects_shape_mismatch() {
    // A [2,3] stream bound at `Tensor[i64, [3, 2]]`. Same channel, shape face.
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.zeros([2, 3]);\n\
             let bad: Tensor[i64, [3, 2]] = Tensor.from_arrow_ipc(a.to_arrow_ipc());\n\
             println(bad.rank());\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("does not match the declared shape")),
        "expected a shape rejection, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_tensor_from_arrow_ipc_accepts_matching_shape() {
    assert_eq!(
        run("fn main() {\n\
                 let a: Tensor[i64, [2, 3]] = Tensor.zeros([2, 3]);\n\
                 let ok: Tensor[i64, [2, 3]] = Tensor.from_arrow_ipc(a.to_arrow_ipc());\n\
                 println(ok.rank());\n\
                 println(ok.shape());\n\
             }"),
        "2\n[2, 3]\n"
    );
}

#[test]
fn test_http_request_builder_dispatches_without_network() {
    // Hermetic companion to the loopback tests above: an invalid URL must
    // reach the send path and come back as `Err`. Pre-fix this was not an
    // `Err` at all — it was a runtime "method 'request' not found" abort, so
    // this distinguishes "dispatch arm exists" from "request happened to
    // fail" without binding a port.
    let output = run(r#"
fn main() with sends(Network) receives(Network) {
    let c = Client.new();
    match c.request("GET", "not-a-url").timeout(100).send() {
        Ok(_) => println("ok"),
        Err(_) => println("err"),
    }
}
"#);
    assert_eq!(output, "err\n");
}

#[test]
fn test_typeparam_assoc_fn_dispatch() {
    // `T.default()` inside a generic function dispatches to the impl
    // matching the runtime binding of `T` (driven by the caller's
    // expected type at the outer call site).
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 42 } }
}

fn make[T: Default]() -> T {
    T.default()
}

fn main() {
    let f: Foo = make();
    println(f.value);
}
"#);
    assert_eq!(output, "42\n");
}

#[test]
fn test_concrete_type_prefix_assoc_fn_dispatch() {
    // `Foo.default()` directly dispatches to the impl method.
    let output = run(r#"
trait Default {
    fn default() -> Self;
}

struct Foo { value: i64 }

impl Default for Foo {
    fn default() -> Foo { Foo { value: 5 } }
}

fn main() {
    let f = Foo.default();
    println(f.value);
}
"#);
    assert_eq!(output, "5\n");
}

/// `.cmp()` on a TUPLE under the tree-walker (B-2026-08-27-41).
///
/// `tests/codegen.rs` is entirely `#[cfg(feature = "llvm")]`, so without a
/// test here this semantics has NO coverage on the default `cargo test` leg —
/// the leg most contributors and the cheapest CI tier actually run.
///
/// The second half is the one that only exists here. A comparison written
/// against a bounded type PARAMETER lowers to `T.cmp`, and the interpreter is
/// untyped at runtime: `a` simply IS a `Value::Tuple`, so `smaller` answers
/// for a tuple with no substitution channel involved. Both compiled backends
/// still decline that spelling — the mono channel drops a tuple type argument
/// (B-2026-08-27-40) — so it is loud-refused there rather than wrong, and
/// cannot be twinned in `tests/codegen.rs` until that row lands.
///
/// `value_compare` has ordered `Value::Tuple` lexicographically all along;
/// what was missing was the `cmp` dispatch arm reaching it.
#[test]
fn tuple_cmp_dispatches_through_value_compare() {
    let out = run_no_errors(
        r#"
fn tag(o: Ordering) -> i64 {
    if o.is_lt() { return 0; }
    if o.is_eq() { return 1; }
    return 2;
}

fn main() {
    println(f"{tag((1, 2).cmp((1, 3)))}");
    println(f"{tag((1, 2).cmp((1, 2)))}");
    println(f"{tag((1, 2).cmp((0, 9)))}");
    println(f"{tag(("b", 1).cmp(("b", 2)))}");
    println(f"{tag((1, 2, 3).cmp((1, 2, 4)))}");
    println(f"{tag((1, (2, 3)).cmp((1, (2, 4))))}");
    println(f"{tag((true, 'z').cmp((true, 'a')))}");
}
"#,
    );
    assert_eq!(out, "0\n1\n2\n0\n0\n0\n2\n");
}

#[test]
fn test_slice_pattern_match_dispatches_on_length_for_vec() {
    // Different arms select on length classes; vectors of varying length
    // each route to the right arm.
    let output = run_no_errors(
        r#"
fn classify(v: Vec[i64]) -> String {
    match v {
        [] => "0",
        [_] => "1",
        [_, _] => "2",
        [_, .., _] => "3+",
    }
}
fn main() {
    let a: Vec[i64] = Vec.new();
    let mut b: Vec[i64] = Vec.new();
    b.push(1);
    let mut c: Vec[i64] = Vec.new();
    c.push(1); c.push(2);
    let mut d: Vec[i64] = Vec.new();
    d.push(1); d.push(2); d.push(3); d.push(4);
    println(classify(a));
    println(classify(b));
    println(classify(c));
    println(classify(d));
}
"#,
    );
    assert_eq!(output, "0\n1\n2\n3+\n");
}

#[test]
fn test_autograd_reverse_mode_scalar() {
    // std.autograd (phase-11) reverse-mode scalar AD — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_scalar_arithmetic /
    // test_e2e_autograd_activations. Exercises the shared-struct tape,
    // interior-mutability node recording, and the backward sweep on the
    // tree-walk backend. f(x)=x²+3x at x=2 → 10, grad 7; sigmoid(0)=0.5,
    // grad 0.25; relu(-2) grad 0; tanh(0)=0, grad 1.
    let out = run_no_errors(
        r#"
import std.autograd.{Tape, Var};
fn main() {
    let t = Tape.new();
    let x = Var.leaf(t, 2.0);
    let three = Var.leaf(t, 3.0);
    let f = x.mul(x).add(three.mul(x));
    println(f.value()); f.backward(); println(x.grad());

    let t2 = Tape.new();
    let z = Var.leaf(t2, 0.0);
    let s = z.sigmoid();
    println(s.value()); s.backward(); println(z.grad());

    let t3 = Tape.new();
    let n = Var.leaf(t3, -2.0);
    let rn = n.relu();
    rn.backward(); println(n.grad());

    let t4 = Tape.new();
    let w = Var.leaf(t4, 0.0);
    let y = w.tanh();
    println(y.value()); y.backward(); println(w.grad());
}
"#,
    );
    assert_eq!(out, "10\n7\n0.5\n0.25\n0\n0\n1\n");
}

#[test]
fn test_autograd_grad_value_and_grad() {
    // std.autograd (phase-11) higher-order API `Tape.grad` / `Tape.value_and_grad`
    // — interpreter parity with tests/codegen.rs::test_e2e_autograd_grad_value_and_grad.
    // A closure `Fn(Var) -> Var` is differentiated with no user tape bookkeeping.
    //   value_and_grad(x²+3x, 2) = (10, 7); grad(x²+x, 2) = 5; grad(x³, 2) = 12;
    //   grad(sigmoid, 0) = 0.25; grad(relu, 3) = 1; grad(relu, -1) = 0.
    let out = run_no_errors(
        r#"
import std.autograd.{Tape, Var};
fn main() {
    let vg = Tape.value_and_grad(|x| x.mul(x).add(x.add(x).add(x)), 2.0);
    println(vg.0);
    println(vg.1);
    println(Tape.grad(|x| x.mul(x).add(x), 2.0));
    println(Tape.grad(|x| x.mul(x).mul(x), 2.0));
    println(Tape.grad(|x| x.sigmoid(), 0.0));
    println(Tape.grad(|x| x.relu(), 3.0));
    println(Tape.grad(|x| x.relu(), 0.0 - 1.0));
}
"#,
    );
    assert_eq!(out, "10\n7\n5\n12\n0.25\n1\n0\n");
}

#[test]
fn test_multiversion_dispatches_correctly() {
    // `#[multiversion(...)]` desugars to per-feature variants + a cpu.supports
    // dispatch thunk; every variant computes the same result, so a correct output
    // proves the thunk dispatched. Interpreter parity with
    // tests/codegen.rs::test_e2e_multiversion_dispatches_correctly.
    let out = run_no_errors(
        r#"
#[multiversion(baseline, "avx2", "avx512f")]
fn addup(a: i64, b: i64) -> i64 { a + b }

#[multiversion(baseline, "avx2")]
fn scale(v: i64) -> i64 { v * 3 }

fn main() {
    println(addup(20, 22));
    println(scale(14));
}
"#,
    );
    assert_eq!(out, "42\n42\n");
}

#[test]
fn test_multiversion_method_and_generic_dispatch() {
    // `#[multiversion]` follow-on: `self`-receiver methods (ref / mut ref /
    // owned) and generic free functions. Every variant computes the same result,
    // so correct output proves the thunk dispatched. Interpreter parity with
    // tests/codegen.rs::test_e2e_multiversion_method_and_generic_dispatch.
    let out = run_no_errors(
        r#"
struct Acc { base: i64 }
impl Acc {
    #[multiversion(baseline, "avx2", "avx512f")]
    fn dot(ref self, x: i64) -> i64 { self.base + x }

    #[multiversion(baseline, "avx2")]
    fn scale(mut ref self, k: i64) -> i64 {
        self.base = self.base * k;
        self.base
    }

    #[multiversion(baseline, "avx2")]
    fn consume(self, y: i64) -> i64 { self.base + y }
}

#[multiversion(baseline, "avx2", "avx512f")]
fn gadd[T: Add](a: T, b: T) -> T { a + b }

fn main() {
    let a = Acc { base: 100 };
    println(a.dot(5));
    let mut b = Acc { base: 3 };
    println(b.scale(4));
    let c = Acc { base: 7 };
    println(c.consume(1));
    println(gadd(20, 22));
}
"#,
    );
    assert_eq!(out, "105\n12\n8\n42\n");
}

#[test]
fn test_autograd_reverse_mode_tensor_valued() {
    // std.autograd (phase-11) tensor-valued surface — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_tensor_valued. Element-wise
    // Tensor[f32, [?]] nodes with gradient accumulation across a fanned-out
    // input: z = x*y + x at x=[2,3], y=[4,5] → dz/dx = y+1 = [5,6],
    // dz/dy = x = [2,3]. Exercises the shared-struct tensor tape, the
    // fresh-local value/grad pushes, and the tensor backward sweep.
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([2.0, 3.0]);
    let y0: Tensor[f32, [?]] = Tensor.from([4.0, 5.0]);
    let x = TensorVar.leaf(t, x0);
    let y = TensorVar.leaf(t, y0);
    let z = x.mul(y).add(x);
    z.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
    println(y.grad_at(0));
    println(y.grad_at(1));
}
"#,
    );
    assert_eq!(out, "5\n6\n2\n3\n");
}

#[test]
fn test_autograd_reverse_mode_tensor_activations() {
    // std.autograd tensor-valued activations — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_tensor_activations. relu gates the
    // gradient on the input sign ([0,1] for x=[-1,2]); sigmoid'(0)=0.25;
    // tanh'(0)=1.
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([-1.0, 2.0]);
    let x = TensorVar.leaf(t, x0);
    let r = x.relu();
    r.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));

    let t2 = TensorTape.new();
    let z0: Tensor[f32, [?]] = Tensor.from([0.0]);
    let z = TensorVar.leaf(t2, z0);
    let s = z.sigmoid();
    s.backward();
    println(z.grad_at(0));

    let t3 = TensorTape.new();
    let w0: Tensor[f32, [?]] = Tensor.from([0.0]);
    let w = TensorVar.leaf(t3, w0);
    let y = w.tanh();
    y.backward();
    println(w.grad_at(0));
}
"#,
    );
    assert_eq!(out, "0\n1\n0.25\n1\n");
}

#[test]
fn test_autograd_reverse_mode_activations_and_losses() {
    // Phase-11 autograd `silu`/`softmax`/`gelu` activations + `bce`/
    // `cross_entropy` losses — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_activations_and_losses (f32-exact
    // gradients: silu'(0)=0.5, softmax([0,0]) weighted grad [0.25,-0.25],
    // gelu'(0)=0.5, bce(0.8,1) grad -0.625, softmax-CE grad [-0.5,0.5]).
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t1 = TensorTape.new();
    let a = TensorVar.leaf(t1, Tensor.from([0.0, 1.0]));
    let y = a.silu(); let s = y.sum(); s.backward();
    println(a.grad_at(0));

    let t2 = TensorTape.new();
    let x = TensorVar.leaf(t2, Tensor.from([0.0, 0.0]));
    let c = TensorVar.leaf(t2, Tensor.from([1.0, 0.0]));
    let sm = x.softmax(); let l = sm.mul(c).sum(); l.backward();
    println(x.grad_at(0)); println(x.grad_at(1));

    let t3 = TensorTape.new();
    let g = TensorVar.leaf(t3, Tensor.from([0.0]));
    let gy = g.gelu(); let gs = gy.sum(); gs.backward();
    println(g.grad_at(0));

    let t4 = TensorTape.new();
    let p = TensorVar.leaf(t4, Tensor.from([0.8, 0.3]));
    let tg = TensorVar.leaf(t4, Tensor.from([1.0, 0.0]));
    let lb = p.bce(tg); lb.backward();
    println(p.grad_at(0));

    let t5 = TensorTape.new();
    let xc = TensorVar.leaf(t5, Tensor.from([0.0, 0.0]));
    let oh = TensorVar.leaf(t5, Tensor.from([1.0, 0.0]));
    let lc = xc.cross_entropy(oh); lc.backward();
    println(xc.grad_at(0)); println(xc.grad_at(1));
}
"#,
    );
    assert_eq!(out, "0.5\n0.25\n-0.25\n0.5\n-0.625\n-0.5\n0.5\n");
}

#[test]
fn test_autograd_reverse_mode_tensor_scalar_loss() {
    // std.autograd tensor-valued `sum` reduction — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_tensor_scalar_loss. L = sum(x²) at
    // x=[1,2,3] → 14; dL/dx = 2x = [2,4,6].
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([1.0, 2.0, 3.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.sum();
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
    println(x.grad_at(2));
}
"#,
    );
    assert_eq!(out, "14\n2\n4\n6\n");
}

#[test]
fn test_autograd_reverse_mode_tensor_mean_loss() {
    // std.autograd tensor-valued `mean` reduction — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_tensor_mean_loss. L = mean(x²) at
    // x=[2,4] (N=2) → 10; dL/dx = 2x/N = x = [2,4].
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([2.0, 4.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.mean();
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(x.grad_at(0));
    println(x.grad_at(1));
}
"#,
    );
    assert_eq!(out, "10\n2\n4\n");
}

#[test]
fn test_autograd_reverse_mode_matmul() {
    // std.autograd rank-2 matrix surface (MatTape / MatVar) — interpreter parity
    // with tests/codegen.rs::test_e2e_autograd_matmul. Y = A·B, A=[[1,2],[3,4]],
    // B=I; grad_A = [[1,1],[1,1]], grad_B = [[4,4],[6,6]].
    let out = run_no_errors(
        r#"
import std.autograd.{MatTape, MatVar};
fn main() {
    let t = MatTape.new();
    let a0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);
    let b0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.0], [0.0, 1.0]]);
    let a = MatVar.leaf(t, a0);
    let b = MatVar.leaf(t, b0);
    let y = a.matmul(b);
    y.backward();
    println(a.grad_at(0, 0));
    println(a.grad_at(1, 1));
    println(b.grad_at(0, 0));
    println(b.grad_at(1, 0));
}
"#,
    );
    assert_eq!(out, "1\n1\n4\n6\n");
}

#[test]
fn test_autograd_reverse_mode_mse_loss() {
    // std.autograd MSE loss — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_mse_loss. pred=[3,5], target=[1,1] →
    // mse 10, dL/dpred = 2d/N = [2,4].
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let p0: Tensor[f32, [?]] = Tensor.from([3.0, 5.0]);
    let g0: Tensor[f32, [?]] = Tensor.from([1.0, 1.0]);
    let pred = TensorVar.leaf(t, p0);
    let target = TensorVar.leaf(t, g0);
    let loss = pred.mse(target);
    let lv = loss.value();
    println(f"{lv[0]}");
    loss.backward();
    println(pred.grad_at(0));
    println(pred.grad_at(1));
}
"#,
    );
    assert_eq!(out, "10\n2\n4\n");
}

#[test]
fn test_autograd_reverse_mode_gradient_descent_training() {
    // End-to-end training loop — interpreter parity with
    // tests/codegen.rs::test_e2e_autograd_gradient_descent_training. Gradient
    // descent minimizing MSE learns target=[3,5,7]: loss 28 → 0, w → [3,5,7]
    // (rounded, so both backends agree despite f64-vs-f32 precision).
    let out = run_no_errors(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn loss_of(w: ref Tensor[f32, [?]], target: ref Tensor[f32, [?]]) -> f32 {
    let tape = TensorTape.new();
    let wv = TensorVar.leaf(tape, w);
    let tv = TensorVar.leaf(tape, target);
    let loss = wv.mse(tv);
    let lv = loss.value();
    lv[0]
}
fn main() {
    let target: Tensor[f32, [?]] = Tensor.from([3.0, 5.0, 7.0]);
    let mut w: Tensor[f32, [?]] = Tensor.from([0.0, 0.0, 0.0]);
    // B-2026-08-14-14: annotated at the tensors' element type. Unannotated the
    // literal is f64, and `grad * lr` below narrowed it silently into an f32
    // element-wise op. 0.75 is exact in both widths, so no printed value moves.
    let lr: f32 = 0.75;
    let l0 = loss_of(w, target);
    println(f"{l0.round()}");
    let mut step = 0;
    while step < 40 {
        let tape = TensorTape.new();
        let wv = TensorVar.leaf(tape, w);
        let tv = TensorVar.leaf(tape, target);
        let loss = wv.mse(tv);
        loss.backward();
        let grad: Tensor[f32, [?]] = wv.grad();
        let step_dir: Tensor[f32, [?]] = grad * lr;
        w = w - step_dir;
        step = step + 1;
    }
    let lf = loss_of(w, target);
    println(f"{lf.round()}");
    let w0 = w[0];
    let w1 = w[1];
    let w2 = w[2];
    println(f"{w0.round()}");
    println(f"{w1.round()}");
    println(f"{w2.round()}");
}
"#,
    );
    assert_eq!(out, "28\n0\n3\n5\n7\n");
}

// ── Tensor[T, Shape] interpreter MVP (Phase 11) ─────────────────────

#[test]
fn test_tensor_zeros_shape_rank() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[f64, [3, 4]] = Tensor.zeros([3, 4]);\n\
             println(t.rank());\n\
             let s = t.shape();\n\
             println(s[0]);\n\
             println(s[1]);\n\
         }",
    );
    assert_eq!(out, "2\n3\n4\n");
}

#[test]
fn test_tensor_full_and_index_get() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[i64, [2, 3]] = Tensor.full([2, 3], 7);\n\
             println(t[0, 0]);\n\
             println(t[1, 2]);\n\
         }",
    );
    assert_eq!(out, "7\n7\n");
}

#[test]
fn test_tensor_index_set_get_roundtrip() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut t: Tensor[f64, [2, 2]] = Tensor.zeros([2, 2]);\n\
             t[0, 1] = 5.5;\n\
             t[1, 0] = 2.5;\n\
             println(t[0, 1]);\n\
             println(t[1, 0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "5.5\n2.5\n0\n");
}

#[test]
fn test_tensor_rank1_bare_index() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut v: Tensor[f64, [4]] = Tensor.ones([4]);\n\
             v[2] = 9.0;\n\
             println(v[2]);\n\
             println(v[0]);\n\
         }",
    );
    assert_eq!(out, "9\n1\n");
}

#[test]
fn test_tensor_zeros_ones_int_elem_integer_semantics() {
    // An integer-element tensor's zeros/ones fill `Value::Int`, not the
    // historical blanket `Value::Float` — so a fill cell participates in
    // integer division (`1 / 2 == 0`), not float division (`1.0 / 2 ==
    // 0.5`). The element type is read off the `let`'s annotation.
    let out = run_no_errors(
        "fn main() {\n\
             let o: Tensor[i32, [2]] = Tensor.ones([2]);\n\
             println(o[0] / 2);\n\
             let z: Tensor[i64, [3]] = Tensor.zeros([3]);\n\
             println(z[0]);\n\
         }",
    );
    assert_eq!(out, "0\n0\n");
}

#[test]
fn test_tensor_zeros_ones_bool_elem() {
    // A bool-element tensor fills `Value::Bool` — zeros → false, ones →
    // true — so the cells render as `false`/`true` (not `0`/`1`) and are
    // usable as a condition. Previously the f64 fill made `b[0]` a
    // `Value::Float(0.0)`.
    let out = run_no_errors(
        "fn main() {\n\
             let b: Tensor[bool, [2]] = Tensor.zeros([2]);\n\
             println(b[0]);\n\
             let t: Tensor[bool, [2]] = Tensor.ones([2]);\n\
             println(t[0]);\n\
             if t[1] {\n\
                 println(\"flag-set\");\n\
             }\n\
         }",
    );
    assert_eq!(out, "false\ntrue\nflag-set\n");
}

#[test]
fn test_tensor_zeros_ones_float_elem_unchanged() {
    // The f64 default is preserved: a float-element tensor still fills
    // `Value::Float`, so `0.0 / 1.0` render as `0` / `1` and division is
    // float division (`1 / 2 == 0.5`).
    let out = run_no_errors(
        "fn main() {\n\
             let z: Tensor[f64, [2]] = Tensor.zeros([2]);\n\
             println(z[0]);\n\
             let o: Tensor[f32, [2]] = Tensor.ones([2]);\n\
             println(o[0] / 2.0);\n\
         }",
    );
    assert_eq!(out, "0\n0.5\n");
}

#[test]
fn test_tensor_zeros_nested_let_annotations_dont_leak() {
    // The fill hint is saved/restored around each `let` RHS, so an inner
    // tensor `let` with its own annotation doesn't corrupt an outer one.
    // Here the outer `i64` zeros and inner `bool` zeros each pick their
    // own element fill even though the inner `let` evaluates inside the
    // outer block-expr RHS.
    let out = run_no_errors(
        "fn main() {\n\
             let outer: Tensor[i64, [2]] = {\n\
                 let inner: Tensor[bool, [2]] = Tensor.zeros([2]);\n\
                 println(inner[0]);\n\
                 Tensor.zeros([2])\n\
             };\n\
             println(outer[0] + 5);\n\
         }",
    );
    // inner[0] → false (bool fill); outer[0] → Int(0), 0 + 5 → 5 (int).
    assert_eq!(out, "false\n5\n");
}

#[test]
fn test_tensor_row_major_layout_distinct_cells() {
    // Writes to distinct cells must not alias (row-major offsets).
    let out = run_no_errors(
        "fn main() {\n\
             let mut t: Tensor[i64, [2, 3]] = Tensor.full([2, 3], 0);\n\
             t[0, 0] = 1;\n\
             t[0, 2] = 3;\n\
             t[1, 0] = 4;\n\
             t[1, 2] = 6;\n\
             println(t[0, 0]);\n\
             println(t[0, 1]);\n\
             println(t[0, 2]);\n\
             println(t[1, 0]);\n\
             println(t[1, 1]);\n\
             println(t[1, 2]);\n\
         }",
    );
    assert_eq!(out, "1\n0\n3\n4\n0\n6\n");
}

#[test]
fn test_tensor_index_out_of_bounds_runtime_error() {
    // Dynamic dim (?) so the bounds miss is a runtime concern, not a
    // compile-time literal check.
    let errors = runtime_errors(
        "fn main() {\n\
             let t: Tensor[f64, [?, ?]] = Tensor.zeros([2, 2]);\n\
             let i = 5;\n\
             println(t[i, 0]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("out of bounds for dim 0 (size 2)")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_display_renders_shape() {
    let out = run_no_errors(
        "fn main() {\n\
             let t: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);\n\
             println(t);\n\
         }",
    );
    assert_eq!(out, "Tensor[2, 3]\n");
}

#[test]
fn test_tensor_from_values_c_order() {
    // Literal constructor: dims from nesting, elements land in
    // row-major order.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             println(t.rank());\n\
             println(t.shape()[0]);\n\
             println(t.shape()[1]);\n\
             println(t[0, 0]);\n\
             println(t[0, 1]);\n\
             println(t[1, 0]);\n\
             println(t[1, 1]);\n\
         }",
    );
    assert_eq!(out, "2\n2\n2\n1\n2\n3\n4\n");
}

#[test]
fn test_tensor_from_rank1_and_rank3() {
    let out = run_no_errors(
        "fn main() {\n\
             let v = Tensor.from([10, 20, 30]);\n\
             println(v.rank());\n\
             println(v[2]);\n\
             let t = Tensor.from([[[1, 2], [3, 4]], [[5, 6], [7, 8]]]);\n\
             println(t.rank());\n\
             println(t[1, 0, 1]);\n\
             println(t[0, 1, 0]);\n\
         }",
    );
    assert_eq!(out, "1\n30\n3\n6\n3\n");
}

#[test]
fn test_tensor_from_expression_leaves() {
    // Leaves are ordinary expressions, evaluated in C-order.
    let out = run_no_errors(
        "fn main() {\n\
             let x = 5.0;\n\
             let e = Tensor.from([[x, x + 1.0], [x * 2.0, 0.0]]);\n\
             println(e[0, 1]);\n\
             println(e[1, 0]);\n\
         }",
    );
    assert_eq!(out, "6\n10\n");
}

#[test]
fn test_tensor_from_mutation_after_construction() {
    let out = run_no_errors(
        "fn main() {\n\
             let mut t = Tensor.from([[1, 2], [3, 4]]);\n\
             t[0, 1] = 99;\n\
             println(t[0, 1]);\n\
             println(t[1, 1]);\n\
         }",
    );
    assert_eq!(out, "99\n4\n");
}

#[test]
fn test_tensor_from_ragged_runtime_error() {
    // The interpreter walks the literal syntax itself (run_program
    // doesn't gate on typecheck), so raggedness is also a runtime
    // error on the interpreter-only path.
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0]]);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("ragged tensor literal: level at depth 1 has 1 element(s), expected 2")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_iter_axis_rows_and_cols() {
    // Axis 0 yields the rows; axis 1 yields the columns (axis dropped,
    // C-order preserved within each sub-tensor).
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);\n\
             let rows = t.iter_axis(0);\n\
             println(rows.len());\n\
             for r in rows {\n\
                 println(r.shape()[0]);\n\
                 println(r[0]);\n\
                 println(r[2]);\n\
             }\n\
             let cols = t.iter_axis(1);\n\
             println(cols.len());\n\
             for c in cols {\n\
                 println(c[0]);\n\
                 println(c[1]);\n\
             }\n\
         }",
    );
    assert_eq!(out, "2\n3\n1\n3\n3\n4\n6\n3\n1\n4\n2\n5\n3\n6\n");
}

#[test]
fn test_tensor_iter_axis_rank3_middle_axis() {
    // [2, 3, 2] tensor, axis 1: three [2, 2] sub-tensors; slab i holds
    // the elements whose middle coordinate is i.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[[1, 2], [3, 4], [5, 6]], [[7, 8], [9, 10], [11, 12]]]);\n\
             let slabs = t.iter_axis(1);\n\
             println(slabs.len());\n\
             let s = slabs[1];\n\
             println(s.rank());\n\
             println(s[0, 0]);\n\
             println(s[0, 1]);\n\
             println(s[1, 0]);\n\
             println(s[1, 1]);\n\
         }",
    );
    assert_eq!(out, "3\n2\n3\n4\n9\n10\n");
}

#[test]
fn test_tensor_iter_axis_rank1_yields_scalars() {
    let out = run_no_errors(
        "fn main() {\n\
             let v = Tensor.from([10.0, 20.0, 30.0]);\n\
             for x in v.iter_axis(0) {\n\
                 println(x);\n\
             }\n\
         }",
    );
    assert_eq!(out, "10\n20\n30\n");
}

#[test]
fn test_tensor_iter_axis_yields_copies() {
    // Sub-tensors are copies, not views: writing through one leaves
    // the source (and sibling sub-tensors) untouched.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2], [3, 4]]);\n\
             let rows = t.iter_axis(0);\n\
             let mut r0 = rows[0];\n\
             r0[0] = 99;\n\
             println(r0[0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "99\n1\n");
}

#[test]
fn test_tensor_iter_axis_runtime_axis_value() {
    // The axis can be a runtime value; bounds are checked at runtime.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let n = 1;\n\
             let cols = t.iter_axis(n);\n\
             println(cols[1][0]);\n\
             println(cols[1][1]);\n\
         }",
    );
    assert_eq!(out, "2\n4\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let n = 6;\n\
             let bad = t.iter_axis(n);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("axis 6 out of bounds for rank-2 tensor")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_reshape_values_c_order() {
    // C-order data is untouched; only the dims change.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let r = t.reshape([3, 2]);\n\
             println(r.rank());\n\
             println(r[0, 0]);\n\
             println(r[0, 1]);\n\
             println(r[1, 0]);\n\
             println(r[2, 1]);\n\
             let flat = t.reshape([6]);\n\
             println(flat[4]);\n\
         }",
    );
    assert_eq!(out, "2\n1\n2\n3\n6\n5\n");
}

#[test]
fn test_tensor_reshape_is_a_copy_and_checks_count() {
    // Writing through the reshaped tensor leaves the source untouched;
    // a runtime-valued dim with a bad product errors at runtime.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2], [3, 4]]);\n\
             let mut r = t.reshape([4]);\n\
             r[0] = 99;\n\
             println(r[0]);\n\
             println(t[0, 0]);\n\
         }",
    );
    assert_eq!(out, "99\n1\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let m = 4;\n\
             let bad = t.reshape([2, m]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("element counts must match")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_permute_transpose_and_rank3() {
    // result[i, j] = t[j, i] for the rank-2 transpose; for [2, 0, 1] on
    // a rank-3 receiver, result[i, j, k] = t[j, k, i] (NumPy transpose
    // semantics).
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let p = t.permute([1, 0]);\n\
             println(p.shape()[0]);\n\
             println(p[0, 1]);\n\
             println(p[2, 0]);\n\
             let t3 = Tensor.from([[[1, 2], [3, 4], [5, 6]], [[7, 8], [9, 10], [11, 12]]]);\n\
             let p3 = t3.permute([2, 0, 1]);\n\
             println(p3.shape()[0]);\n\
             println(p3.shape()[1]);\n\
             println(p3.shape()[2]);\n\
             println(p3[0, 0, 1]);\n\
             println(p3[1, 1, 2]);\n\
         }",
    );
    assert_eq!(out, "3\n4\n3\n2\n2\n3\n3\n12\n");
}

#[test]
fn test_tensor_from_narrows_every_element_form_to_f32() {
    // B-2026-08-20-22. `Tensor.from` under a `Tensor[f32, …]` annotation must
    // narrow EVERY element, whatever expression produced it. A bare float
    // literal already arrived narrowed by another route, so `[0.1]` looked
    // right and the hole stayed hidden: `-0.1` is `Unary(Neg, Literal)` and
    // `0.05 + 0.05` is a binary op, and neither took that route.
    //
    // The AOT backend stores a packed f32 buffer, so it narrows all four by
    // construction — this is a run-vs-build divergence visible in a SINGLE
    // ELEMENT, before any arithmetic, which is the simplest possible repro of
    // the class B-2026-08-05-31 was opened for.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let x: f32 = 0.1;\n\
        \x20   let t: Tensor[f32, [?]] = Tensor.from([0.1, -0.1, x, 0.05 + 0.05]);\n\
        \x20   println(t[0]); println(t[1]); println(t[2]); println(t[3]);\n\
         }",
    );
    // f32(0.1) widened back to f64 for printing, negated for element 1.
    assert_eq!(
        out,
        "0.10000000149011612\n\
         -0.10000000149011612\n\
         0.10000000149011612\n\
         0.10000000149011612\n"
    );
}

/// An `f64` tensor must NOT be narrowed by the same code path — the width is
/// applied, and for `F64` applying it is the identity. Without this the fix
/// above could "pass" by rounding everything to f32.
#[test]
fn test_tensor_from_leaves_f64_elements_alone() {
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let t: Tensor[f64, [?]] = Tensor.from([0.1, -0.1]);\n\
        \x20   println(t[0]); println(t[1]);\n\
         }",
    );
    assert_eq!(out, "0.1\n-0.1\n");
}

#[test]
fn test_tensor_matmul_integer_overflow_traps() {
    // B-2026-08-20-27. An integer matmul is a SUM OF PRODUCTS, and the
    // element-wise `a * b` over the same tensors already traps on overflow —
    // so matmul cannot be the one integer tensor operation that wraps.
    //
    // Before the fix this printed 8589934592 under `karac run --interp` (a
    // value outside i32 entirely) and 0 under `karac build` (the wrapped
    // i64), with neither surface trapping: a silent miscompile AND a
    // run/build divergence in one operation.
    //
    // 65536 * 65536 = 2^32, twice over = 2^33, past i32 either way.
    let errors = runtime_errors(
        "fn main() {\n\
        \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[65536, 65536]]);\n\
        \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[65536], [65536]]);\n\
        \x20   println(a.matmul(b)[0, 0]);\n\
        }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("integer overflow")),
        "an overflowing integer matmul must trap, got: {:?}",
        errors.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

#[test]
fn test_tensor_matmul_integer_in_range_is_unchanged() {
    // The trap must not cost the ordinary case. Signed and unsigned, both
    // well inside range.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let a: Tensor[i32, [?, ?]] = Tensor.from([[1, 2], [3, 4]]);\n\
        \x20   let b: Tensor[i32, [?, ?]] = Tensor.from([[5, 6], [7, 8]]);\n\
        \x20   let c = a.matmul(b);\n\
        \x20   println(c[0, 0]); println(c[1, 1]);\n\
        \x20   let u: Tensor[u32, [?, ?]] = Tensor.from([[1u32, 2u32]]);\n\
        \x20   let v: Tensor[u32, [?, ?]] = Tensor.from([[3u32], [4u32]]);\n\
        \x20   println(u.matmul(v)[0, 0]);\n\
        }",
    );
    assert_eq!(out, "19\n50\n11\n");
}

#[test]
fn test_tensor_matmul_f32_accumulates_at_element_width() {
    // B-2026-08-20-21. `Tensor[f32].matmul` must accumulate IN f32, rounding
    // on every one of the `k` steps, because that is what codegen's triple
    // loop does (it accumulates in the element LLVM type). Accumulating in
    // f64 and rounding the finished sum — B-2026-08-05-31's fix — agrees only
    // while no intermediate rounding was lost, so it passed every short
    // fixture and diverged on a long one.
    //
    // THE FIXTURE IS THE POINT: a row of `1.0` followed by 100 copies of
    // `1e-8`, against a column of ones. Each `1e-8` is far below the f32 ulp
    // at 1.0 (about 1.19e-7), so an f32 accumulator absorbs every one of them
    // and the answer is exactly 1. An f64 accumulator adds them to each other
    // first, reaching 1.000001, which IS distinguishable in f32 — it rounds to
    // 1.0000009536743164, the value this test exists to keep from coming back.
    //
    // If this fails with 1.0000009536743164, the accumulation moved back to
    // f64; `karac build` still answers 1, so the failure is a run-vs-build
    // divergence, not a rounding preference.
    let out = run_no_errors(
        "fn main() {\n\
        \x20   let a: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001, 0.00000001]]);\n\
        \x20   let b: Tensor[f32, [?, ?]] = Tensor.from([[1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0], [1.0]]);\n\
        \x20   println(a.matmul(b)[0, 0]);\n\
         }",
    );
    assert_eq!(out, "1\n");
}

#[test]
fn test_tensor_matmul_and_transpose_values() {
    // B-2026-07-14-18 (was a phantom method pair). matmul:
    // [[1,2],[3,4]] @ [[5,6],[7,8]] = [[19,22],[43,50]]; non-square
    // [2x3] @ [3x2]; integer elements accumulate in i64. transpose:
    // reversed axes for rank 2 and 3; rank-1 is the identity.
    let out = run_no_errors(
        "fn main() {\n\
             let a = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let b = Tensor.from([[5.0, 6.0], [7.0, 8.0]]);\n\
             let c = a.matmul(b);\n\
             println(c[0, 0]);\n\
             println(c[0, 1]);\n\
             println(c[1, 0]);\n\
             println(c[1, 1]);\n\
             let w = Tensor.from([[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);\n\
             let x = Tensor.from([[7.0, 8.0], [9.0, 10.0], [11.0, 12.0]]);\n\
             let y = w.matmul(x);\n\
             println(y[0, 0]);\n\
             println(y[1, 1]);\n\
             let ia = Tensor.from([[1, 2], [3, 4]]);\n\
             let ib = Tensor.from([[5, 6], [7, 8]]);\n\
             let ic = ia.matmul(ib);\n\
             println(ic[0, 0]);\n\
             println(ic[1, 1]);\n\
             let t = w.transpose();\n\
             println(t.shape()[0]);\n\
             println(t[0, 1]);\n\
             println(t[2, 0]);\n\
             let chained = a.matmul(b).transpose();\n\
             println(chained[0, 1]);\n\
             let v = Tensor.from([9.0, 8.0]);\n\
             let vt = v.transpose();\n\
             println(vt[1]);\n\
         }",
    );
    assert_eq!(out, "19\n22\n43\n50\n58\n154\n19\n50\n3\n4\n3\n43\n8\n");
}

#[test]
fn test_tensor_slice_values_and_runtime_bounds() {
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let s = t.slice(1, 1, 3);\n\
             println(s.shape()[1]);\n\
             println(s[0, 0]);\n\
             println(s[1, 1]);\n\
             let rows = t.slice(0, 1, 2);\n\
             println(rows.shape()[0]);\n\
             println(rows[0, 2]);\n\
             let empty = t.slice(1, 2, 2);\n\
             println(empty.shape()[1]);\n\
         }",
    );
    assert_eq!(out, "2\n2\n6\n1\n6\n0\n");
    let errors = runtime_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let e = 5;\n\
             let bad = t.slice(1, 2, e);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("slice end 5 out of bounds for dim 1 (size 3)")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_squeeze_values_and_runtime_check() {
    let out = run_no_errors(
        "fn main() {\n\
             let u = Tensor.from([[[7, 8, 9]]]);\n\
             let q = u.squeeze();\n\
             println(q.rank());\n\
             println(q[2]);\n\
             let one = u.squeeze(0);\n\
             println(one.rank());\n\
             println(one[0, 1]);\n\
         }",
    );
    assert_eq!(out, "1\n9\n2\n8\n");
    // A `?`-typed (runtime-checked) squeeze axis whose size isn't 1.
    let errors = runtime_errors(
        "fn main() {\n\
             let t: Tensor[f64, [1, ?]] = Tensor.zeros([1, 4]);\n\
             let bad = t.squeeze(1);\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("cannot squeeze axis 1: its size is 4, not 1")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_reshape_of_permuted_data() {
    // Chained transforms: permute reorders the buffer, reshape then
    // reads the *new* C-order — pins that permute produced a real
    // reordered copy, not a view.
    let out = run_no_errors(
        "fn main() {\n\
             let t = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let p = t.permute([1, 0]);\n\
             let flat = p.reshape([6]);\n\
             println(flat[0]);\n\
             println(flat[1]);\n\
             println(flat[2]);\n\
             println(flat[5]);\n\
         }",
    );
    assert_eq!(out, "1\n4\n2\n6\n");
}

#[test]
fn test_tensor_elementwise_arithmetic() {
    // + - * /, scalar broadcast both sides (incl. int-literal promotion to
    // a float element), unary neg; the operands stay usable afterward
    // (borrow, not move).
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[f64, [2, 2]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);\n\
             let b: Tensor[f64, [2, 2]] = Tensor.from([[10.0, 20.0], [30.0, 40.0]]);\n\
             let c = a + b;\n\
             println(c[0, 0]);\n\
             println(c[1, 1]);\n\
             let d = a * b;\n\
             println(d[0, 1]);\n\
             let s = a + 100.0;\n\
             println(s[0, 0]);\n\
             let p = a + 2;\n\
             println(p[0, 0]);\n\
             let n = -a;\n\
             println(n[1, 0]);\n\
             let sl = 100.0 - a;\n\
             println(sl[0, 0]);\n\
             println(a[0, 0]);\n\
         }",
    );
    assert_eq!(out, "11\n44\n40\n101\n3\n-3\n99\n1\n");
}

#[test]
fn test_tensor_int_arithmetic_integer_division() {
    let out = run_no_errors(
        "fn main() {\n\
             let i: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);\n\
             let j: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
             let k = i - j;\n\
             println(k[2]);\n\
             let q = i / j;\n\
             println(q[1]);\n\
         }",
    );
    assert_eq!(out, "27\n10\n");
}

#[test]
fn test_tensor_arithmetic_runtime_shape_mismatch_and_divzero() {
    // `?`-dim operands pass the typechecker; the interpreter re-checks shape
    // equality at runtime (run_program bypasses typecheck).
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Tensor[f64, [?]] = Tensor.zeros([3]);\n\
             let b: Tensor[f64, [?]] = Tensor.zeros([4]);\n\
             let c = a + b;\n\
         }",
    );
    assert!(
        errors.iter().any(|e| e
            .message
            .contains("tensor shape mismatch in element-wise operator")),
        "{errors:?}",
    );
    // Element-wise div-by-zero traps just like the scalar op.
    let errors = runtime_errors(
        "fn main() {\n\
             let i: Tensor[i64, [2]] = Tensor.from([10, 20]);\n\
             let z: Tensor[i64, [2]] = Tensor.from([2, 0]);\n\
             let q = i / z;\n\
             println(q[0]);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("division by zero")),
        "{errors:?}",
    );
}

#[test]
fn test_tensor_full_reduce() {
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             println(a.sum());\n\
             println(a.prod());\n\
             println(a.min());\n\
             println(a.max());\n\
             println(a.mean());\n\
             let v: Tensor[f64, [4]] = Tensor.from([2.0, 4.0, 6.0, 8.0]);\n\
             println(v.sum());\n\
             println(v.mean());\n\
         }",
    );
    // mean of [1..6] = 3.5; mean of [2,4,6,8] = 5.0.
    assert_eq!(out, "21\n720\n1\n6\n3.5\n20\n5\n");
}

#[test]
fn test_tensor_axis_reduce() {
    let out = run_no_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let s0 = a.sum_axis(0);\n\
             println(s0[0]); println(s0[1]); println(s0[2]);\n\
             let s1 = a.sum_axis(1);\n\
             println(s1[0]); println(s1[1]);\n\
             let m0 = a.mean_axis(0);\n\
             println(m0[0]); println(m0[2]);\n\
             let v: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);\n\
             println(v.sum_axis(0));\n\
         }",
    );
    // sum_axis(0)=[5,7,9]; sum_axis(1)=[6,15]; mean_axis(0)=[2.5,3.5,4.5];
    // rank-1 sum_axis -> scalar 10.
    assert_eq!(out, "5\n7\n9\n6\n15\n2.5\n4.5\n10\n");
}

#[test]
fn test_tensor_reduce_empty_traps() {
    for m in ["sum", "prod", "min", "max", "mean"] {
        let errors = runtime_errors(&format!(
            "fn main() {{\n\
                 let e: Tensor[i64, [0]] = Tensor.zeros([0]);\n\
                 let r = e.{m}();\n\
             }}",
        ));
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("cannot reduce an empty tensor")),
            "{m}: {errors:?}",
        );
    }
}

#[test]
fn test_tensor_broadcast() {
    let out = run_no_errors(
        "fn main() {\n\
             let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             // row [1,3] broadcasts over the 2 rows.\n\
             let row: Tensor[i64, [1, 3]] = Tensor.from([[10, 20, 30]]);\n\
             let r = m.broadcast_add(row);\n\
             println(r[0, 0]); println(r[1, 2]);\n\
             // column [2,1] broadcasts over the 3 cols.\n\
             let col: Tensor[i64, [2, 1]] = Tensor.from([[100], [200]]);\n\
             let c = m.broadcast_mul(col);\n\
             println(c[0, 1]); println(c[1, 0]);\n\
             // rank-mismatch: [3] aligns to the trailing axis.\n\
             let v: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);\n\
             let d = m.broadcast_sub(v);\n\
             println(d[0, 0]); println(d[1, 1]);\n\
             // operands are read, not moved — reuse afterward.\n\
             println(m[1, 2]);\n\
         }",
    );
    // r=[ [11,22,33],[14,25,36] ]; c=[ [100,200,300],[800,1000,1200] ];
    // d=[ [0,0,0],[3,3,3] ]; m reused = 6.
    assert_eq!(out, "11\n36\n200\n800\n0\n3\n6\n");
}

#[test]
fn test_tensor_broadcast_div_and_two_singletons() {
    let out = run_no_errors(
        "fn main() {\n\
             let m: Tensor[f64, [2, 2]] = Tensor.from([[2.0, 4.0], [6.0, 8.0]]);\n\
             let col: Tensor[f64, [2, 1]] = Tensor.from([[2.0], [4.0]]);\n\
             let q = m.broadcast_div(col);\n\
             println(q[0, 0]); println(q[1, 0]);\n\
             // [1,3] broadcast with [2,1] -> [2,3], both singletons expand.\n\
             let row: Tensor[i64, [1, 3]] = Tensor.from([[1, 2, 3]]);\n\
             let coli: Tensor[i64, [2, 1]] = Tensor.from([[10], [20]]);\n\
             let g = row.broadcast_add(coli);\n\
             println(g[0, 2]); println(g[1, 0]);\n\
         }",
    );
    // q=[ [1,2],[1.5,2] ]; g=[ [11,12,13],[21,22,23] ].
    assert_eq!(out, "1\n1.5\n13\n21\n");
}

#[test]
fn test_tensor_broadcast_incompatible_traps() {
    let errors = runtime_errors(
        "fn main() {\n\
             let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);\n\
             let b: Tensor[i64, [2, 4]] = Tensor.from([[1, 2, 3, 4], [5, 6, 7, 8]]);\n\
             let r = a.broadcast_add(b);\n\
         }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("not broadcast-compatible")),
        "{errors:?}",
    );
}

#[test]
fn unqualified_struct_variant_construction_method_dispatch() {
    // B-2026-06-13-12 (interpreter twin): an UNQUALIFIED struct-variant
    // construction `Variant { .. }` must build a `Value::EnumVariant` carrying
    // the *enum* name, not fall through to a `Value::Struct` named after the
    // variant. Pre-fix, `value_type_name` reported the variant ("A"), so a
    // method call on the binding failed with "method 'code' not found on type
    // 'A'" — even with an explicit `let a: E = ...` annotation (the interpreter
    // ignores it). The qualified form (`E.A { .. }`) already worked. Covers the
    // annotation-present and annotation-absent shapes.
    let out = run_no_errors(
        r#"
enum E { A { n: i64 }, B }
impl E { fn code(ref self) -> i64 { match self { A { n } => n, B => 0 } } }
fn main() {
    let a = A { n: 7 };
    let b: E = A { n: 9 };
    println(a.code());
    println(b.code());
}
"#,
    );
    assert_eq!(out, "7\n9\n");
}

/// (B-2026-07-02-10) A user struct method that shares a builtin container
/// method name (`first`, `last`, `get_unchecked`) must dispatch to the
/// user's impl, not be captured by the builtin seq arm — which swallowed
/// non-seq receiver shapes into `Value::Unit`, so `b.first()` on a struct
/// silently returned `()`. Surfaced by the S6-pre trait probes: any trait
/// impl whose method names overlap Vec/Slice builtins hit this.
#[test]
fn struct_method_named_like_seq_builtin_dispatches_to_impl() {
    let out = run_no_errors(
        "trait Wrap[T] {\n\
         \x20   fn first(ref self) -> T;\n\
         }\n\
         struct Box2 { n: i64 }\n\
         impl Wrap[i64] for Box2 {\n\
         \x20   fn first(ref self) -> i64 { return self.n; }\n\
         }\n\
         impl Box2 {\n\
         \x20   fn last(ref self) -> i64 { return self.n * 10; }\n\
         \x20   fn get_unchecked(ref self) -> i64 { return self.n + 1; }\n\
         }\n\
         fn main() {\n\
         \x20   let b = Box2 { n: 7 };\n\
         \x20   println(b.first());\n\
         \x20   println(b.first() + 1);\n\
         \x20   println(b.last());\n\
         \x20   println(b.get_unchecked());\n\
         }\n",
    );
    assert_eq!(out, "7\n8\n70\n8\n");
}

// ── S6a: baked Reduce trait, bound-generic dispatch ────────────────

#[test]
fn stdlib_reduce_trait_bound_dispatch_column_and_tensor() {
    // S6a: `fn spread[C: Reduce[i64]](c: ref C)` accepts both builtin
    // implementors; the receiver dispatches through the value-shape
    // intercepts to the shared reduction kernel (the baked
    // `#[compiler_builtin]` impl bodies never run).
    let out = run_no_errors(
        r#"
fn spread[C: Reduce[i64]](c: ref C) -> i64 {
    c.max() - c.min()
}
fn avg[C: Reduce[i64]](c: ref C) -> f64 {
    c.mean()
}
fn main() {
    let c: Column[i64] = Column.from_vec([10, 20, 30]);
    let t: Tensor[i64, [4]] = Tensor.from([2, 4, 6, 8]);
    println(f"{spread(c)} {spread(t)}");
    println(f"{avg(c)} {avg(t)}");
    println(f"{c.sum()} {t.sum()}");
}
"#,
    );
    assert_eq!(out, "20 6\n20 5\n60 20\n");
}

#[test]
fn stdlib_reduce_trait_bound_prod_column_and_tensor() {
    // S6c-11: `prod` on the `Reduce` trait surface — a `fn f[C: Reduce[T]]`
    // body may call `c.prod()`, dispatched to the concrete `Column`/`Tensor`
    // kernel exactly like `sum`/`min`/`max` (a required method, no default
    // body — no `One`-trait mul-identity needed). Covers i64 + f64 elements and
    // combination with `sum` in one bound-generic function.
    let out = run_no_errors(
        r#"
fn totalprod[C: Reduce[i64]](c: ref C) -> i64 { c.prod() }
fn fprod[C: Reduce[f64]](c: ref C) -> f64 { c.prod() }
fn sumprod[C: Reduce[i64]](c: ref C) -> i64 { c.sum() + c.prod() }
fn main() {
    let ci: Column[i64] = Column.from_vec([2, 3, 4]);
    let ti: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 5]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.0, 4.0]);
    println(f"{totalprod(ci)} {totalprod(ti)}");
    println(f"{fprod(cf)}");
    println(f"{sumprod(ci)}");
}
"#,
    );
    // ci: 2*3*4=24; ti: 1*2*3*5=30; cf: 1.5*2*4=12; sumprod: (2+3+4)+(2*3*4)=33.
    assert_eq!(out, "24 30\n12\n33\n");
}

#[test]
fn user_trait_impl_over_column_dispatches() {
    // S6c-12: a user `impl Trait for Column[i64]` method calls the builtin
    // reductions on `self` and is itself dispatched from a value receiver.
    // Before, a Column receiver never reached `try_eval_impl_method` (gated to
    // struct-shaped values) and `value_type_name` returned "unknown", so
    // `c.doubled_sum()` errored "method not found on type 'unknown'". Covers a
    // self-calls-another-user-method chain (`quad` → `twice`) and an f64 twin.
    let out = run_no_errors(
        r#"
trait Combo[T] { fn twice(ref self) -> T; fn quad(ref self) -> T; }
impl Combo[i64] for Column[i64] {
    fn twice(ref self) -> i64 { self.sum() + self.sum() }
    fn quad(ref self) -> i64 { self.twice() + self.twice() }
}
trait Spread[T] { fn spread(ref self) -> T; }
impl Spread[f64] for Column[f64] {
    fn spread(ref self) -> f64 { self.max() - self.min() }
}
fn main() {
    let ci: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let cf: Column[f64] = Column.from_vec([1.5, 4.0, 2.5]);
    println(f"{ci.quad()}");
    println(f"{cf.spread()}");
}
"#,
    );
    // quad = 4*sum = 4*10 = 40; spread = 4.0 - 1.5 = 2.5.
    assert_eq!(out, "40\n2.5\n");
}

#[test]
fn same_head_impls_dispatch_to_their_own_target() {
    // B-2026-08-13-8 — the INTERPRETER half. `register_impl_methods` bound both
    // impls under the env key `Vec.describe`, so the LAST registration won and
    // answered every receiver — the mirror image of codegen, which took the
    // FIRST. A type-erased runtime value cannot break the tie on its own (a
    // `Value::Array` of ints knows nothing about its static element type), so
    // the env key now carries the impl's target args and the typechecker hands
    // over the winner for each call site.
    //
    // The `Box` pair pins that the scope is every generic type, not just the
    // builtin containers the row was reported against.
    let out = run_no_errors(
        r#"
struct Box[T] { v: T }
trait Zero { fn describe(ref self) -> String; }
impl Zero for Vec[i64] { fn describe(ref self) -> String { return f"VEC-I64"; } }
impl Zero for Vec[String] { fn describe(ref self) -> String { return f"VEC-STR"; } }
impl Zero for Box[i64] { fn describe(ref self) -> String { return f"BOX-I64"; } }
impl Zero for Box[String] { fn describe(ref self) -> String { return f"BOX-STR"; } }
fn main() {
    let mut a: Vec[i64] = Vec.new();
    a.push(1);
    let mut b: Vec[String] = Vec.new();
    b.push("x");
    println(a.describe());
    println(a.len());
    println(b.describe());
    println(a.describe());
    let p: Box[i64] = Box { v: 1 };
    let q: Box[String] = Box { v: "s" };
    println(p.describe());
    println(q.describe());
}
"#,
    );
    assert_eq!(out, "VEC-I64\n1\nVEC-STR\nVEC-I64\nBOX-I64\nBOX-STR\n");
}

#[test]
fn two_from_impls_dispatch_by_source_type() {
    // B-2026-08-27-1 — two `impl From[X] for AppError` both wanted the name
    // `AppError.from`. The interpreter's env kept the LAST registered and
    // codegen's module handed out the FIRST, so `--interp` and `karac build`
    // ran DIFFERENT conversion functions on the same program.
    //
    // The two error payloads have DIFFERENT SHAPES here (a `String` field
    // against an `i64` field) on purpose. With matching shapes a wrong
    // dispatch is merely a wrong answer; with these, feeding a `ParseError` to
    // the `DbError` impl was a type confusion — the interpreter hit
    // `unreachable!` ("field 'code' not found on struct 'ParseError'") and AOT
    // silently read the `String`'s words as an `i64`, printing a raw heap
    // pointer and exiting 0.
    //
    // All THREE spellings are exercised, because they are three different
    // resolution paths that happened to share one broken name: `?`
    // (question_conversions), `.into()` (into_conversions via the blanket
    // lowering), and a direct `AppError.from(x)` (a two-segment path call,
    // which additionally used to be REJECTED outright for the second source
    // type — "expected 'ParseError', found 'DbError'", naming a type the
    // author never wrote).
    let expect = "PARSE:p\nDB:7\nPARSE:p\nDB:7\nPARSE:p\nDB:7\n";
    assert_eq!(run_no_errors(&two_from_impls_src(true)), expect);
    assert_eq!(run_no_errors(&two_from_impls_src(false)), expect);
}

#[test]
fn user_trait_impl_over_slice_dispatches() {
    // B-2026-08-13-7 — the INTERPRETER half, which is the piece that had to land
    // before the typecheck and codegen halves could. A `Value::Slice` receiver is
    // snapshotted into a `Value::Array` before dispatch so the builtin seq
    // surface sees a uniform shape; that snapshot renames it `Vec`, so
    // `try_eval_impl_method` built the key `Vec.describe` and never found the
    // impl registered under `Slice.describe`. The snapshot is now skipped for
    // exactly the names the builtin surface does not answer.
    //
    // Both a `Slice` and a `Vec` impl are in scope so the two receivers must be
    // told apart, and `s.len()` pins that the builtin keeps precedence — with
    // the snapshot skipped, `len` still has to reach the seq surface.
    let out = run_no_errors(
        r#"
trait Zero { fn describe(ref self) -> String; }
impl Zero for Slice[i64] { fn describe(ref self) -> String { return f"S{self.len()}"; } }
impl Zero for Vec[i64] { fn describe(ref self) -> String { return f"V{self.len()}"; } }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(10);
    v.push(20);
    v.push(30);
    let s: Slice[i64] = v[..];
    println(s.describe());
    println(s.len());
    let sub: Slice[i64] = v[1..3];
    println(sub.describe());
    println(v.describe());
}
"#,
    );
    assert_eq!(out, "S3\n3\nS2\nV3\n");
}

#[test]
fn user_trait_impl_over_tensor_dispatches() {
    // S6c-12 slice 2: Tensor twin of `user_trait_impl_over_column_dispatches`.
    // The interpreter already named Tensor in `value_type_name` and admitted a
    // Tensor receiver to `try_eval_impl_method` in slice 1; this locks in parity.
    let out = run_no_errors(
        r#"
trait Combo[T] { fn twice(ref self) -> T; fn quad(ref self) -> T; }
impl Combo[i64] for Tensor[i64, [4]] {
    fn twice(ref self) -> i64 { self.sum() + self.sum() }
    fn quad(ref self) -> i64 { self.twice() + self.twice() }
}
trait Spread[T] { fn spread(ref self) -> T; }
impl Spread[f64] for Tensor[f64, [3]] {
    fn spread(ref self) -> f64 { self.max() - self.min() }
}
fn main() {
    let ti: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 4.0, 2.5]);
    println(f"{ti.quad()}");
    println(f"{tf.spread()}");
}
"#,
    );
    assert_eq!(out, "40\n2.5\n");
}

#[test]
fn user_trait_default_method_over_container_dispatches() {
    // S6c-12 slice 3: a user trait DEFAULT method inherited by a Column/Tensor
    // impl dispatches under the interpreter. Locks in the coverage (the desugar
    // splice pass + slice 1/2 dispatch already carry it).
    let out = run_no_errors(
        r#"
trait Stat[T: Add] {
    fn total(ref self) -> T;
    fn total_or(ref self, fallback: T) -> T { self.total() }
    fn twice_total(ref self) -> T { self.total() + self.total() }
}
impl Stat[i64] for Column[i64] {
    fn total(ref self) -> i64 { self.sum() }
}
impl Stat[i64] for Tensor[i64, [3]] {
    fn total(ref self) -> i64 { self.sum() }
}
fn main() {
    let c: Column[i64] = Column.from_vec([4, 5, 6]);
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{c.total_or(0)} {c.twice_total()}");
    println(f"{t.total_or(0)} {t.twice_total()}");
}
"#,
    );
    assert_eq!(out, "15 30\n6 12\n");
}

#[test]
fn user_generic_trait_impl_over_container_dispatches() {
    // S6c-12 slice 4: a GENERIC container impl (`impl[T: Add] Trait[T] for
    // Column[T]`/`Tensor`) with a required method, across two element monos.
    // Locks in the base generic-container-impl feature under the interpreter.
    let out = run_no_errors(
        r#"
trait Doubler[T: Add] { fn doubled_sum(ref self) -> T; }
impl[T: Add] Doubler[T] for Column[T] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
impl[T: Add] Doubler[T] for Tensor[T, [3]] {
    fn doubled_sum(ref self) -> T { self.sum() + self.sum() }
}
fn main() {
    let ci: Column[i64] = Column.from_vec([1, 2, 3]);
    let cf: Column[f64] = Column.from_vec([1.5, 2.5, 3.0]);
    let ti: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{ci.doubled_sum()} {cf.doubled_sum()} {ti.doubled_sum()}");
}
"#,
    );
    assert_eq!(out, "12 14 12\n");
}

#[test]
fn inherent_impl_over_container_adds_methods_dispatches() {
    // S6c-12 final: user *inherent* impls (no trait) that ADD new method names
    // to a builtin container dispatch correctly under `karac run`. Two disjoint
    // inherent impls on `Column[i64]` plus a self-calls-another-inherent chain
    // and a Tensor. The interpreter has no overlap check, so the only gate was
    // the typechecker admitting the impl (method-granular overlap admission).
    let out = run_no_errors(
        r#"
impl Column[i64] {
    fn doubled_sum(ref self) -> i64 { self.sum() + self.sum() }
    fn quad_sum(ref self) -> i64 { self.doubled_sum() + self.doubled_sum() }
}
impl Column[i64] {
    fn spread(ref self) -> i64 { self.max() - self.min() }
}
impl Tensor[i64, [3]] {
    fn twice_sum(ref self) -> i64 { self.sum() + self.sum() }
}
fn main() {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let t: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{c.doubled_sum()} {c.quad_sum()} {c.spread()}");
    println(f"{t.twice_sum()}");
}
"#,
    );
    // sum=10 → doubled=20, quad=40; spread = 4-1 = 3. tensor sum=6 → 12.
    assert_eq!(out, "20 40 3\n12\n");
}

#[test]
fn stdlib_reduce_trait_bound_fold_column_and_tensor() {
    // S6c: `fold` on the `Reduce` trait surface — a `fn f[C: Reduce[i64]]`
    // body may call `c.fold(init, |a, x| ...)`, dispatched to the concrete
    // implementor's kernel per instantiation. The closure params `(A, T)` are
    // typed from `init` + the bound's element (`Reduce[i64]` → `T = i64`).
    // Covers a Column receiver (nulls skipped) and a Tensor receiver (dense),
    // plus a non-sum fold body (count > 2) proving it's the general primitive
    // and an empty column returning `init` unchanged (the fold identity).
    let out = run_no_errors(
        r#"
fn accumulate[C: Reduce[i64]](c: ref C) -> i64 {
    c.fold(0, |a, x| a + x)
}
fn count_gt2[C: Reduce[i64]](c: ref C) -> i64 {
    c.fold(0, |a, x| if x > 2 { a + 1 } else { a })
}
fn main() {
    let col: Column[i64] = Column.from_vec([3, 1, 4, 1, 5]);
    let t: Tensor[i64, [3]] = Tensor.from([10, 20, 5]);
    let mut nulled: Column[i64] = Column.new();
    nulled.push(10);
    nulled.push_null();
    nulled.push(30);
    let empty: Column[i64] = Column.new();
    println(f"{accumulate(col)} {accumulate(t)}");
    println(f"{count_gt2(col)}");
    println(f"{accumulate(nulled)} {accumulate(empty)}");
}
"#,
    );
    assert_eq!(out, "14 35\n3\n40 0\n");
}

#[test]
fn stdlib_ewmap_trait_bound_map_zip_column_and_tensor() {
    // S6c: `map` / `zip_with` on the `ElementwiseMap` trait surface — a
    // `fn f[C: ElementwiseMap[i64]]` body may call `c.map(|x| ...)` /
    // `a.zip_with(b, |x, y| ...)`, each returning `Self = C` (a fresh
    // same-shaped container). Dispatched to the concrete implementor's kernel
    // per instantiation (Column nulls preserved; Tensor dense). The result
    // container is bound and reduced (`.sum()`) to observe it.
    let out = run_no_errors(
        r#"
fn doubled[C: ElementwiseMap[i64]](c: ref C) -> C {
    c.map(|x| x * 2)
}
fn combine[C: ElementwiseMap[i64]](a: ref C, b: ref C) -> C {
    a.zip_with(b, |x, y| x + y)
}
fn main() {
    let col: Column[i64] = Column.from_vec([1, 2, 3]);
    let t: Tensor[i64, [3]] = Tensor.from([10, 20, 5]);
    let a: Column[i64] = Column.from_vec([1, 2, 3]);
    let b: Column[i64] = Column.from_vec([10, 20, 30]);
    let dc: Column[i64] = doubled(col);
    let dt: Tensor[i64, [3]] = doubled(t);
    let z: Column[i64] = combine(a, b);
    println(f"{dc.sum()} {dt.sum()}");
    println(f"{z.sum()}");
}
"#,
    );
    assert_eq!(out, "12 70\n66\n");
}

#[test]
fn builtin_column_tensor_range_default_method() {
    // The BAKED `Reduce[T]::range` DEFAULT (`max - min`) on the BUILTIN
    // `Column[T]` / `Tensor[T, S]` implementors. They don't inherit it via the
    // user-impl splice, so a dedicated `range` arm routes through the same
    // min/max reduction path (which traps on an empty/all-null input, like
    // `min`/`max`). Covers i64 + f64 for both containers.
    let out = run_no_errors(
        r#"
fn main() {
    let ci: Column[i64] = Column.from_vec([3, 9, 1, 7]);
    let cf: Column[f64] = Column.from_vec([1.5, 9.0, 4.0]);
    let ti: Tensor[i64, [4]] = Tensor.from([2, 4, 6, 8]);
    let tf: Tensor[f64, [3]] = Tensor.from([1.5, 9.0, 4.0]);
    println(f"{ci.range()} {ti.range()}");
    println(f"{cf.range()} {tf.range()}");
}
"#,
    );
    assert_eq!(out, "8 6\n7.5 7.5\n");
}

#[test]
fn tensor_fold_reduction() {
    // `Tensor.fold[A](init, |acc, x| ...)` — the general left-fold, parity with
    // `Column.fold`. Every element folds (a tensor has no null concept; a 2-D
    // tensor folds all cells in C order). Covers a numeric fold, a String
    // accumulator (run-only — the interpreter is not restricted to POD `A`),
    // and a 2-D fold.
    let out = run_no_errors(
        r#"
fn main() {
    let t: Tensor[i64, [5]] = Tensor.from([1, 2, 3, 4, 5]);
    println(f"{t.fold(0, |a, x| a + x)}");
    println(f"{t.fold(1, |a, x| a * x)}");
    println(f"{t.fold(0, |a, x| if x > 2 { a + 1 } else { a })}");

    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    println(f"{m.fold(0, |a, x| a + x)}");

    // String accumulator — the interpreter is not restricted to POD `A`.
    let joined = t.fold("", |acc, x| acc + "!");
    println(f"{joined}");
}
"#,
    );
    assert_eq!(out, "15\n120\n3\n21\n!!!!!\n");
}

#[test]
fn tensor_map_reduction() {
    // `Tensor.map(|x| ...) -> Tensor[T, ...S]` — element-wise map producing a
    // fresh tensor of the same shape (parity with `Column.map`). Covers a 1-D
    // map, a captured outer variable, and a 2-D map (all cells).
    let out = run_no_errors(
        r#"
fn main() {
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let d = t.map(|x| x * 2);
    println(f"{d.sum()}");

    let k: i64 = 100;
    let e = t.map(|x| x + k);
    println(f"{e.sum()}");

    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let g = m.map(|x| x + 10);
    println(f"{g.sum()}");
}
"#,
    );
    // sum([2,4,6,8])=20; sum([101,102,103,104])=410; sum([11..16])=81.
    assert_eq!(out, "20\n410\n81\n");
}

#[test]
fn column_tensor_zip_with_reduction() {
    // `zip_with(other, |a, b| ...)` — element-wise combine of two same-shape
    // containers through the closure. Column propagates nulls (bitmap AND);
    // Tensor requires an identical shape. Covers a Column add, Column null
    // propagation, a Tensor multiply, and a 2-D Tensor.
    let out = run_no_errors(
        r#"
fn main() {
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    println(f"{a.zip_with(b, |x, y| x + y).sum()}");

    let mut p: Column[i64] = Column.new();
    p.push(1);
    p.push_null();
    p.push(3);
    let mut q: Column[i64] = Column.new();
    q.push(10);
    q.push(20);
    q.push_null();
    let z = p.zip_with(q, |x, y| x + y);
    println(f"{z.sum()} {z.valid_count()}");

    let t1: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let t2: Tensor[i64, [4]] = Tensor.from([2, 2, 2, 2]);
    println(f"{t1.zip_with(t2, |x, y| x * y).sum()}");

    let m1: Tensor[i64, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);
    let m2: Tensor[i64, [2, 2]] = Tensor.from([[10, 20], [30, 40]]);
    println(f"{m1.zip_with(m2, |x, y| x + y).sum()}");
}
"#,
    );
    // 110; null-prop: only slot0 (1+10=11), 1 valid; 2+4+6+8=20; 11+22+33+44=110.
    assert_eq!(out, "110\n11 1\n20\n110\n");
}

#[test]
fn column_tensor_argmin_argmax_reduction() {
    // `argmin()`/`argmax() -> Option[i64]` (ElementwiseOrd, S6c). Column reports
    // the ORIGINAL slot index over the valid slots (nulls skipped in the
    // compare); Tensor the flat C-order index over all elements. Ties keep the
    // first occurrence; an empty/all-null column -> None.
    let out = run_no_errors(
        r#"
fn show(o: Option[i64]) {
    match o {
        Some(i) => println(f"{i}"),
        None => println("none"),
    }
}
fn main() {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    show(c.argmin());
    show(c.argmax());

    let mut n: Column[i64] = Column.new();
    n.push(10);
    n.push_null();
    n.push(5);
    n.push_null();
    n.push(20);
    show(n.argmin());
    show(n.argmax());

    let mut allnull: Column[i64] = Column.with_capacity(2);
    allnull.push_null();
    allnull.push_null();
    show(allnull.argmin());

    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    show(t.argmin());
    show(t.argmax());

    let m: Tensor[i64, [2, 3]] = Tensor.from([[3, 1, 4], [1, 5, 9]]);
    show(m.argmin());
    show(m.argmax());
}
"#,
    );
    // c: min@5, max@1. n [10,null,5,null,20]: min@2, max@4. all-null: none.
    // t: min@1, max@4. m flat [3,1,4,1,5,9]: min@1, max@5.
    assert_eq!(out, "5\n1\n2\n4\nnone\n1\n4\n1\n5\n");
}

#[test]
fn bound_generic_dispatch_over_user_type() {
    // B-2026-07-06-2 (interpreter side / run==build parity): a bound-generic
    // `fn f[C: Trait](c: ref C) { c.m() }` dispatched over a USER-struct
    // implementor. The interpreter always handled this; the test locks the run
    // side of the parity the codegen fix restores. Covers ref, owned, and
    // mut-ref receivers plus the stdlib `Reduce` surface over a user type.
    let out = run_no_errors(
        r#"
trait Doubler[T] { fn dbl(ref self) -> T; }
struct Wrap { v: i64 }
impl Doubler[i64] for Wrap { fn dbl(ref self) -> i64 { self.v + self.v } }

trait Owned[T] { fn triple(self) -> T; }
struct Own { v: i64 }
impl Owned[i64] for Own { fn triple(self) -> i64 { self.v * 3 } }

trait Bump { fn bump(mut ref self) -> i64; }
struct Ctr { n: i64 }
impl Bump for Ctr { fn bump(mut ref self) -> i64 { self.n = self.n + 1; self.n } }

struct Pair { a: i64, b: i64 }
impl Reduce[i64] for Pair {
    fn sum(ref self) -> i64 { self.a + self.b }
    fn prod(ref self) -> i64 { self.a * self.b }
    fn min(ref self) -> i64 { if self.a < self.b { self.a } else { self.b } }
    fn max(ref self) -> i64 { if self.a > self.b { self.a } else { self.b } }
    fn mean(ref self) -> f64 { 0.0 }
    fn fold[A](ref self, init: A, f: Fn(A, i64) -> A) -> A { f(f(init, self.a), self.b) }
}

fn dref[C: Doubler[i64]](c: ref C) -> i64 { c.dbl() }
fn towned[C: Owned[i64]](c: C) -> i64 { c.triple() }
fn twice[C: Bump](c: mut ref C) -> i64 { c.bump() + c.bump() }
fn total[C: Reduce[i64]](c: ref C) -> i64 { c.sum() + c.max() }

fn main() {
    let w: Wrap = Wrap { v: 21 };
    let mut ct: Ctr = Ctr { n: 0 };
    let p: Pair = Pair { a: 3, b: 4 };
    println(f"{dref(w)}");
    println(f"{towned(Own { v: 5 })}");
    println(f"{twice(mut ct)}");
    println(f"{total(p)}");
}
"#,
    );
    assert_eq!(out, "42\n15\n3\n11\n");
}

#[test]
fn elementwise_ord_trait_bound_and_user_impl_dispatches() {
    // S6c: with the baked `impl ElementwiseOrd for Column/Tensor`, a
    // bound-generic `fn f[C: ElementwiseOrd[i64]](c: ref C)` dispatches
    // argmin/argmax/sorted to the concrete implementor's kernel (Column and
    // Tensor), a USER TYPE that implements the trait by hand dispatches to its
    // own body, and a user trait impl over a container calls the methods on
    // `self`. All three implementor shapes under one bound-generic caller.
    let out = run_no_errors(
        r#"
struct Trio { a: i64, b: i64, c: i64 }
impl ElementwiseOrd[i64] for Trio {
    fn argmin(ref self) -> Option[i64] {
        if self.a <= self.b and self.a <= self.c { return Some(0); }
        if self.b <= self.c { return Some(1); }
        Some(2)
    }
    fn argmax(ref self) -> Option[i64] { Some(2) }
    fn sorted(ref self) -> Vec[i64] { [self.a, self.b, self.c] }
    fn argsort(ref self) -> Vec[i64] { [0, 1, 2] }
}
fn lo[C: ElementwiseOrd[i64]](c: ref C) -> i64 { c.argmin().unwrap() }
fn top[C: ElementwiseOrd[i64]](c: ref C) -> i64 { let s: Vec[i64] = c.sorted(); s[0] }
trait Ranked { fn span(ref self) -> i64; }
impl Ranked for Column[i64] {
    fn span(ref self) -> i64 { self.argmin().unwrap() + self.argmax().unwrap() }
}
fn main() {
    let col: Column[i64] = Column.from_vec([3, 1, 2]);
    let t: Tensor[i64, [3]] = Tensor.from([30, 10, 20]);
    let tr: Trio = Trio { a: 5, b: 2, c: 8 };
    println(f"{lo(col)} {top(col)}");
    println(f"{lo(t)} {top(t)}");
    println(f"{lo(tr)}");
    println(f"{col.span()}");
}
"#,
    );
    // col [3,1,2]: argmin@1, sorted[0]=1. t [30,10,20]: argmin@1, sorted[0]=10.
    // Trio a=5,b=2,c=8: argmin=1. col.span = argmin(1)+argmax(0) = 1.
    assert_eq!(out, "1 1\n1 10\n1\n1\n");
}

#[test]
fn column_tensor_sorted_argsort_reduction() {
    // `sorted() -> Vec[T]` / `argsort() -> Vec[i64]` (ElementwiseOrd, S6c).
    // Column operates on the VALID slots (nulls dropped; argsort reports the
    // ORIGINAL slot positions); Tensor over all elements in flat C-order. Ties
    // are stable. The interpreter handles all element widths (unlike the
    // i64/f64-only native first cut).
    let out = run_no_errors(
        r#"
fn vshow(v: Vec[i64]) {
    let mut s = "";
    let mut i = 0;
    while i < v.len() {
        s = s + v[i].to_string() + ",";
        i = i + 1;
    }
    println(s);
}
fn main() {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let cs = c.sorted();
    vshow(cs);
    let ca = c.argsort();
    vshow(ca);

    let mut n: Column[i64] = Column.with_capacity(5);
    n.push(10); n.push_null(); n.push(5); n.push_null(); n.push(20);
    let ns = n.sorted();
    vshow(ns);
    let na = n.argsort();
    vshow(na);

    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let ts = t.sorted();
    vshow(ts);
    let ta = t.argsort();
    vshow(ta);
}
"#,
    );
    // c sorted [1,3,3,5,8,9]; argsort [5,2,3,0,4,1].
    // n valid-only sorted [5,10,20]; argsort original slots [2,0,4].
    // t sorted [2,2,4,7,9,9]; argsort [1,3,0,2,4,5].
    assert_eq!(
        out,
        "1,3,3,5,8,9,\n5,2,3,0,4,1,\n5,10,20,\n2,0,4,\n2,2,4,7,9,9,\n1,3,0,2,4,5,\n"
    );
}

/// B-2026-08-05-31 — an `f32` tensor's elements must be rounded to f32
/// precision, matching codegen's packed f32 buffer.
///
/// The interpreter stores every float as f64 (`Value::Float(f64)`), so before
/// this a `Tensor[f32]` computed in full double precision while `karac build`
/// used real f32 lanes, and the two backends printed DIFFERENT answers for the
/// same program: `0.1 * 3` gave 0.30000000000000004 under `karac run --interp`
/// and 0.30000001192092896 from the built binary. AOT was the correct one.
///
/// `Value::Tensor` now carries the declared element width and the element-wise
/// ops round through it.
///
/// The scalar line was written as a control on the claim that "a plain `f32`
/// local is f64 on BOTH backends, so this is a tensor-only narrowing". That
/// claim no longer holds and its expectation moved: B-2026-08-14-7 gave the
/// SCALAR binop path the same rounding, so `s * 3.0` now yields the f32 value
/// here too. It is kept, rather than deleted, because it is the marker for what
/// is left: `karac build` still prints 0.30000000000000004 for this line, and
/// the reason is neither backend's arithmetic but the BINDING — an unsuffixed
/// `0.1` at an `f32` annotation is never narrowed to f32 on either surface, so
/// codegen is multiplying a double it should not be holding. Spelled `0.1f32`
/// or `0.1 as f32`, both surfaces agree on the f32 answer.
#[test]
fn tensor_f32_elements_round_to_f32_precision() {
    let out = run_no_errors(
        r#"
fn main() {
    let a: Tensor[f32, [1]] = Tensor.from([0.1]);
    let b: Tensor[f32, [1]] = a * 3.0;
    println(b.sum());
    let neg: Tensor[f32, [1]] = -b;
    println(neg.sum());
    let d: Tensor[f64, [1]] = Tensor.from([0.1]);
    let e: Tensor[f64, [1]] = d * 3.0;
    println(e.sum());
    let s: f32 = 0.1;
    println(s * 3.0);
}
"#,
    );
    // f32 lane: 0.1f32 * 3 rounds to 0.30000001192092896 — the value `karac
    // build` produces. The f64 lane keeps full double precision. The scalar
    // now rounds too (B-2026-08-14-7); see the doc comment for why `karac
    // build` still disagrees on that last line alone.
    assert_eq!(
        out,
        "0.30000001192092896\n-0.30000001192092896\n0.30000000000000004\n0.30000001192092896\n"
    );
}

#[test]
fn tensor_narrow_element_storage_and_sort_reduction() {
    // The `run` surface for narrow-width tensor storage + ops (the native
    // backend gained these once B-2026-07-03-35 was fixed; the interpreter is
    // width-agnostic, so this locks the parity). i32 indexing + sum, an f32
    // tensor from float literals, an f64 tensor from INTEGER literals, and
    // narrow tensor `sorted`/`argsort`.
    let out = run_no_errors(
        r#"
fn main() {
    let t: Tensor[i32, [4]] = Tensor.from([40, 10, 30, 20]);
    println(f"{t[0]} {t[3]} {t.sum()}");
    let f: Tensor[f32, [3]] = Tensor.from([1.5, 2.5, 3.5]);
    println(f"{f[0]} {f.sum()} {f.mean()}");
    let d: Tensor[f64, [3]] = Tensor.from([1, 2, 3]);
    println(f"{d.sum()}");
    let si = t.sorted();
    let ai = t.argsort();
    println(f"{si[0]} {si[3]} | {ai[0]} {ai[3]}");
}
"#,
    );
    // t [40,10,30,20] t[0]=40 t[3]=20 sum=100; f sum=7.5 mean=2.5; d sum=6;
    // t sorted [10,20,30,40] si[0]=10 si[3]=40; argsort [1,3,2,0] ai[0]=1 ai[3]=0.
    assert_eq!(out, "40 20 100\n1.5 7.5 2.5\n6\n10 40 | 1 0\n");
}

#[test]
fn primitive_trait_impl_direct_dispatch_by_declared_width() {
    // B-2026-07-03-5: a user trait impl on a PRIMITIVE target dispatched for a
    // direct value-receiver call under `karac run`. The interpreter's runtime
    // value is width-erased (`Value::Int` → "i64", `Value::Float` → "f64"), so
    // a `u8`/`u16`/… receiver would miss its own impl and — worse, when an
    // `i64`/`f64` impl also exists — wrongly dispatch to that erased-key impl.
    // The fix recovers the DECLARED receiver type the typechecker recorded for
    // the call site (`method_callee_types`). Each width has a distinguishing
    // impl so this asserts the CORRECT per-width impl is selected.
    let src = "trait Tag { fn tag(self) -> i64; }
        impl Tag for i8  { fn tag(self) -> i64 { -8 } }
        impl Tag for i16 { fn tag(self) -> i64 { -16 } }
        impl Tag for i32 { fn tag(self) -> i64 { -32 } }
        impl Tag for u8  { fn tag(self) -> i64 { 8 } }
        impl Tag for u16 { fn tag(self) -> i64 { 16 } }
        impl Tag for u32 { fn tag(self) -> i64 { 32 } }
        impl Tag for f32 { fn tag(self) -> i64 { 320 } }
        impl Tag for f64 { fn tag(self) -> i64 { 640 } }
        fn main() {
            let a: i8 = 1; let b: i16 = 1; let c: i32 = 1;
            let d: u8 = 1; let e: u16 = 1; let f: u32 = 1;
            let g: f32 = 1.0; let h: f64 = 1.0;
            println(a.tag()); println(b.tag()); println(c.tag());
            println(d.tag()); println(e.tag()); println(f.tag());
            println(g.tag()); println(h.tag());
        }";
    assert_eq!(run_no_errors(src), "-8\n-16\n-32\n8\n16\n32\n320\n640\n");
}

#[test]
fn generic_bound_primitive_dispatch_by_instantiation() {
    // B-2026-07-03-24: a generic bound over a primitive trait impl
    // (`fn tag_it[T: Tag](x: T) { x.tag() }`) dispatches to the correct
    // per-width impl under `karac run`. The receiver `x: T` is a type param —
    // `Value::Int`/`Value::Float` are width-erased and the typechecker checks
    // the body once with T abstract, so the concrete width can only come from
    // the per-call type-subs stack. The fix records the receiver's type-param
    // name (`method_typeparam_receiver`) and resolves it through that stack.
    // Distinguishing per-width impls incl. the f32-vs-f64 case (both erase to
    // "f64" at runtime), which must NOT collapse.
    let src = "trait Tag { fn tag(self) -> i64; }
        impl Tag for i8  { fn tag(self) -> i64 { -8 } }
        impl Tag for i16 { fn tag(self) -> i64 { -16 } }
        impl Tag for i32 { fn tag(self) -> i64 { -32 } }
        impl Tag for u8  { fn tag(self) -> i64 { 8 } }
        impl Tag for u16 { fn tag(self) -> i64 { 16 } }
        impl Tag for u32 { fn tag(self) -> i64 { 32 } }
        impl Tag for f32 { fn tag(self) -> i64 { 320 } }
        impl Tag for f64 { fn tag(self) -> i64 { 640 } }
        fn tag_it[T: Tag](x: T) -> i64 { x.tag() }
        fn main() {
            let a: i8 = 1; let b: i16 = 1; let c: i32 = 1;
            let d: u8 = 1; let e: u16 = 1; let f: u32 = 1;
            let g: f32 = 1.0; let h: f64 = 1.0;
            println(tag_it(a)); println(tag_it(b)); println(tag_it(c));
            println(tag_it(d)); println(tag_it(e)); println(tag_it(f));
            println(tag_it(g)); println(tag_it(h));
        }";
    assert_eq!(run_no_errors(src), "-8\n-16\n-32\n8\n16\n32\n320\n640\n");
}

#[test]
fn generic_bound_user_type_dispatch_unregressed() {
    // Guard: the generic-bound dispatch for USER types (name-carrying Values)
    // must keep working alongside the B-2026-07-03-24 primitive path.
    let src = "trait Greet { fn greet(self) -> i64; }
        struct Person { id: i64 }
        struct Robot { serial: i64 }
        impl Greet for Person { fn greet(self) -> i64 { 100 } }
        impl Greet for Robot { fn greet(self) -> i64 { 200 } }
        fn do_greet[T: Greet](x: T) -> i64 { x.greet() }
        fn main() {
            let p = Person { id: 1 };
            let r = Robot { serial: 2 };
            println(do_greet(p));
            println(do_greet(r));
        }";
    assert_eq!(run_no_errors(src), "100\n200\n");
}

// ── GPU dispatch (spike slice-0c) ───────────────────────────────
// `karac run` has no GPU, so `gpu.dispatch(kernel, buffer)` runs the `#[gpu]`
// kernel element-wise on the CPU — the same result the compiled Metal/wgpu
// path computes (run == build parity). See docs/spikes/gpu-wgsl-slice0.md.

#[test]
fn test_gpu_dispatch_runs_kernel_elementwise_on_cpu() {
    let src = "#[gpu]\n\
               fn double(x: f32) -> f32 { x * 2.0 }\n\
               fn main() {\n\
                   let buf: Vec[f32] = [1.0, 2.0, 3.0, 4.0];\n\
                   let out = gpu.dispatch(double, buf);\n\
                   for v in out { println(f\"{v}\"); }\n\
               }";
    assert_eq!(run_no_errors(src), "2\n4\n6\n8\n");
}

#[test]
fn test_gpu_dispatch_arithmetic_kernel() {
    let src = "#[gpu]\n\
               fn affine(x: f32) -> f32 { x * 3.0 + 1.0 }\n\
               fn main() {\n\
                   let buf: Vec[f32] = [0.0, 1.0, 2.0];\n\
                   let out = gpu.dispatch(affine, buf);\n\
                   for v in out { println(f\"{v}\"); }\n\
               }";
    assert_eq!(run_no_errors(src), "1\n4\n7\n");
}

#[test]
fn test_gpu_dispatch_i32_kernel_cpu() {
    // Non-f32 element type (i32) — the CPU map runs the integer kernel.
    let src = "#[gpu]\n\
               fn triple(x: i32) -> i32 { x * 3 }\n\
               fn main() {\n\
                   let buf: Vec[i32] = [1, 2, 3];\n\
                   let out = gpu.dispatch(triple, buf);\n\
                   for v in out { println(f\"{v}\"); }\n\
               }";
    assert_eq!(run_no_errors(src), "3\n6\n9\n");
}

/// Interpreter ORACLE for the non-`let` generic-struct binding sites
/// (B-2026-08-27-36). The match-arm leg SEGFAULTED under JIT and AOT at
/// `T = String` while this side was correct throughout — which is what made
/// the interpreter the oracle rather than a parity check, exactly as it was
/// for the rest of this family (B-2026-08-25-7/-25/-27/-28).
///
/// Both legs run at BOTH `i64` and `String`; the scalar leg was silently
/// correct on the broken tree, so a scalar-only test proves nothing here.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_generic_non_let_binding_instantiation_across_sites`.
#[test]
fn non_let_generic_binding_sites_dispatch_to_the_monomorph() {
    for mk in [
        "fn mk(v: Vec[T]) -> Vec[T] { let bags = [Bag { xs: v }]; \
         let mut out: Vec[T] = Vec.new(); for b in bags { out = b.inner(); } out }",
        "fn mk(v: Vec[T]) -> Vec[T] { let o = Some(Bag { xs: v }); \
         match o { Some(b) => { b.inner() } None => { Vec.new() } } }",
    ] {
        let out = run(&format!(
            "struct Bag[=T] {{ xs: Vec[T] }}\n\
             impl[T: Ord] Bag[T] {{\n    \
                 fn swap2(mut ref self, i: i64, j: i64) {{ self.xs.swap(i, j); }}\n    \
                 fn arrange(mut ref self) {{ let n = self.xs.len(); if n > 1 {{ self.swap2(0, n - 1); }} }}\n    \
                 fn inner(self) -> Vec[T] {{ let mut b = self; b.arrange(); b.xs }}\n    \
                 {mk}\n\
             }}\n\
             fn main() {{\n    \
                 let a = Bag.mk([\"x\", \"y\", \"z\"]); println(a[0]);\n    \
                 let b = Bag.mk([1, 2, 3]); println(b[0]);\n\
             }}\n"
        ));
        assert_eq!(out, "z\n3\n", "site `{mk}` did not round-trip");
    }
}

/// B-2026-08-14-17 — the interpreter twin of
/// `tests/codegen.rs::test_e2e_index_a_tensor_temporary`, same source and same
/// expected string.
///
/// This surface always ran these programs correctly — the bug was that
/// `karac build` could not compile them, which is what made it run-vs-build
/// rather than a wrong answer. So this test passes before the fix and after,
/// and its job is to be the ORACLE the compiled twin is checked against: the
/// two assert one string, so a codegen fix that compiled but computed
/// something else would fail the pair rather than silently redefine it.
#[test]
fn test_index_a_tensor_temporary() {
    assert_eq!(
        run("fn main() {\n\
                 let t: Tensor[f64, [2]] = Tensor.from([1.0, 2.0]);\n\
                 println((t * 2)[0]);\n\
                 println((t + t)[1]);\n\
                 println((0.0 - t)[0]);\n\
                 println(Tensor.from([5.0, 6.0])[1]);\n\
                 println(((t + t) * 2)[0]);\n\
                 let m: Tensor[i64, [2, 2]] = Tensor.from([[1, 2], [3, 4]]);\n\
                 println((m + m)[1, 0]);\n\
                 let r = t * 2;\n\
                 println(r[0]);\n\
             }"),
        "2\n4\n-1\n6\n4\n6\n2\n"
    );
}

/// THE ARM-SHADOWING GUARD. Interpreter method dispatch is by NAME, so
/// `Vec.reserve` and `String.reserve` must be served by ONE match arm. A
/// separate earlier `"reserve"` arm for String shadows the Vec one completely —
/// measured, and it failed loudly with `method 'reserve' not found on type
/// 'Vec' (no interpreter dispatch arm)` rather than silently. Both receivers in
/// one program is what keeps them from drifting back apart.
#[test]
fn vec_and_string_reserve_are_served_by_the_same_dispatch_arm() {
    let out = run(r#"
fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.reserve(32);
    v.push(1);
    let mut s: String = String.new();
    s.reserve(32);
    s.push_str("x");
    println(f"{v.len()} {v.capacity() >= 32} [{s}]");
}
"#);
    assert_eq!(out, "1 true [x]\n");
}
