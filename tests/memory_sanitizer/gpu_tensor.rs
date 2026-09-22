//! GPU dispatch, tensors, autograd -- fixtures for `tests/memory_sanitizer.rs`.
//!
//! Split out of `tests/memory_sanitizer.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test memory_sanitizer` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test memory_sanitizer gpu_tensor::
//!
//! New fixtures about GPU dispatch, tensors, autograd belong in this file.

use super::*;

#[test]
fn asan_shared_struct_vec_tensor_field_drop() {
    // (a) push-only — the shared-struct `Vec[Tensor]` field drop must free
    // each element block. No overwrite involved.
    assert_clean_asan_run(
        r#"
shared struct S { mut grads: Vec[Tensor[f32, [?]]] }
fn main() {
    let s = S { grads: Vec.new() };
    let z: Tensor[f32, [?]] = Tensor.zeros([2]);
    s.grads.push(z);
    let r = ref s.grads[0];
    println(r[0]);
}
"#,
        &["0"],
        "asan_shared_struct_vec_tensor_field_drop",
    );
}

#[test]
fn asan_freshtemp_tensor_ref_arg_assoc_call_no_crash() {
    // B-2026-07-18-9: a FRESH-TEMP tensor (`Tensor.from(...)`) passed as a
    // `ref Tensor` arg to an ASSOCIATED fn (`S.ins(...)`) that pushes
    // `value + 0.0` into a shared-struct `Vec[Tensor]` field. The assoc-call
    // arg path passed the fresh temp BY VALUE (only Identifier args got the
    // `get_data_ptr` pointer); the callee then dereferenced the tensor
    // block's rank word as a pointer → SIGSEGV under `karac build` (a named
    // binding was clean). Now the rvalue is materialized into a slot and its
    // pointer passed, matching the free-fn path. This is the exact shape the
    // autograd `TensorVar.leaf(t, Tensor.from(…))` call hits. Looped so any
    // per-iteration imbalance in the pushed element blocks accumulates.
    assert_clean_asan_run(
        r#"
shared struct S { mut vals: Vec[Tensor[f32, [?]]] }
impl S {
    fn new() -> S { S { vals: Vec.new() } }
    fn ins(t: S, value: ref Tensor[f32, [?]]) {
        let v: Tensor[f32, [?]] = value + 0.0;
        t.vals.push(v);
    }
}
fn main() {
    let mut n = 0;
    while n < 3 {
        let s = S.new();
        S.ins(s, Tensor.from([-1.0, 2.0, 3.0]));
        println(f"{s.vals.len()}");
        n = n + 1;
    }
}
"#,
        &["1", "1", "1"],
        "asan_freshtemp_tensor_ref_arg_assoc_call_no_crash",
    );
}

#[test]
fn asan_freshtemp_tensor_ref_arg_free_fn_no_leak() {
    // B-2026-07-18-10 residual: the FREE-FN sibling — an inline
    // `Tensor.from([…])` passed as a `ref Tensor` arg to a free fn. The
    // materialized fresh-temp block is freed via `FreeTensor` (not leaked)
    // and the borrow reads correctly. Looped so any per-call leak accumulates.
    assert_clean_asan_run(
        r#"
fn sum_first_two(v: ref Tensor[f32, [?]]) -> f32 {
    let s: Tensor[f32, [?]] = v + 0.0;
    s[0] + s[1]
}
fn main() {
    let mut n = 0;
    while n < 3 {
        let r = sum_first_two(Tensor.from([1.0, 2.0, 3.0]));
        println(f"{r}");
        n = n + 1;
    }
}
"#,
        &["3", "3", "3"],
        "asan_freshtemp_tensor_ref_arg_free_fn_no_leak",
    );
}

#[test]
fn asan_tensor_transcendental_map_no_leak() {
    // The data-spine transcendental-map vectorizer allocates a fresh
    // result tensor per `map`; its FreeTensor cleanup must run (no
    // leak) and the strip-mined `<8 x float>` main loop + scalar-splat
    // tail must stay in bounds (no ASan OOB on the vector load/store).
    // Looped so any per-iteration leak accumulates; the bound map (the
    // vectorizer's target) carries a captured scalar + a transcendental,
    // and the length (10) exercises one full width-8 chunk + a 2-element
    // tail. Self-checking so the expected output is poly-independent.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut n = 0;
    while n < 3 {
        let t: Tensor[f32, [10]] = Tensor.from([-2.0, -1.5, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
        let scale: f32 = 1.5f32;
        let m = t.map(|x| (x * scale).exp());
        let xs: Tensor[f32, [10]] = Tensor.from([-2.0, -1.5, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5, 2.0, 2.5]);
        let mut ok: bool = true;
        for i in 0..10 {
            let want: f32 = (xs[i] * scale).exp();
            let d: f32 = m[i] - want;
            let ad: f32 = if d < 0.0f32 { 0.0f32 - d } else { d };
            let aw: f32 = if want < 0.0f32 { 0.0f32 - want } else { want };
            if ad > 0.001f32 * aw + 0.001f32 { ok = false; }
        }
        println(ok);
        n = n + 1;
    }
}
"#,
        &["true", "true", "true"],
        "asan_tensor_transcendental_map_no_leak",
    );
}

#[test]
fn asan_vec_tensor_element_overwrite() {
    // (b) overwrite — `s.grads[0] = old + g` frees the displaced old element
    // block (read into the fresh sum first) and takes over the new one.
    assert_clean_asan_run(
        r#"
shared struct S { mut grads: Vec[Tensor[f32, [?]]] }
fn go(s: S) {
    let z: Tensor[f32, [?]] = Tensor.zeros([2]);
    s.grads.push(z);
    let g: Tensor[f32, [?]] = Tensor.ones([2]);
    let old = ref s.grads[0];
    s.grads[0] = old + g;
}
fn main() {
    let s = S { grads: Vec.new() };
    go(s);
    let r = ref s.grads[0];
    println(r[0]);
}
"#,
        &["1"],
        "asan_vec_tensor_element_overwrite",
    );
}

#[test]
fn asan_vec_tensor_accumulate_loop() {
    // (b)+(c) the backward-pass shape: a moved named-tensor store (`seed`)
    // plus repeated accumulation into ONE slot across a loop
    // (`grads[0] = grads[0] + g`), overwriting the same slot each step.
    assert_clean_asan_run(
        r#"
shared struct T { mut grads: Vec[Tensor[f32, [?]]] }
fn main() {
    let t = T { grads: Vec.new() };
    let seed: Tensor[f32, [?]] = Tensor.ones([3]);
    t.grads.push(seed);
    let mut k = 1;
    while k < 4 {
        let o: Tensor[f32, [?]] = Tensor.ones([3]);
        t.grads.push(o);
        k = k + 1;
    }
    let mut i = 1;
    while i < 4 {
        let g = ref t.grads[i];
        let acc = ref t.grads[0];
        t.grads[0] = acc + g;
        i = i + 1;
    }
    let r = ref t.grads[0];
    println(r[0]);
}
"#,
        &["4"],
        "asan_vec_tensor_accumulate_loop",
    );
}

#[test]
fn asan_autograd_tensor_valued_module() {
    // The full `std.autograd` tensor-valued surface end-to-end: leaf copies,
    // the fresh-local value/grad pushes, the `backward` accumulation, and the
    // copy-return `value()`/`grad()` — all through the gated import. z = x*y+x
    // with x fanned out, so grads accumulate. LSan-clean over the whole tape.
    assert_clean_asan_run(
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
    let gx = x.grad();
    println(gx[0]);
    println(x.grad_at(1));
    println(y.grad_at(0));
}
"#,
        &["5", "6", "2"],
        "asan_autograd_tensor_valued_module",
    );
}

#[test]
fn asan_autograd_tensor_activations() {
    // The tensor-valued activation path: `Tensor.map` forwards plus the
    // backward VJPs that allocate extra temps (relu's `mask`, sigmoid/tanh's
    // `g * s * (1-s)` chains). LSan-clean over the whole tape.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([-1.0, 2.0, 0.5]);
    let x = TensorVar.leaf(t, x0);
    let a = x.relu();
    let b = a.tanh();
    let c = b.sigmoid();
    c.backward();
    println(x.grad_at(0));
}
"#,
        &["0"],
        "asan_autograd_tensor_activations",
    );
}

#[test]
fn asan_autograd_tensor_scalar_loss() {
    // The `sum` reduction path — the shape-changing terminal ([N]→[1]) and
    // its broadcasting backward (`xa * 0.0 + gs`), which allocate the [1]
    // value/grad and a broadcast temp. Full scalar-loss tape, LSan-clean.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([1.0, 2.0, 3.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.sum();
    loss.backward();
    println(x.grad_at(2));
}
"#,
        &["6"],
        "asan_autograd_tensor_scalar_loss",
    );
}

#[test]
fn asan_autograd_tensor_mean_loss() {
    // The `mean` reduction path — the shape-changing [N]→[1] terminal plus
    // its 1/N-broadcasting backward, which allocates the count temp
    // (`(xa*0+1).sum()`) on top of the broadcast. LSan-clean over the tape.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let x0: Tensor[f32, [?]] = Tensor.from([2.0, 4.0]);
    let x = TensorVar.leaf(t, x0);
    let sq = x.mul(x);
    let loss = sq.mean();
    loss.backward();
    println(x.grad_at(1));
}
"#,
        &["4"],
        "asan_autograd_tensor_mean_loss",
    );
}

#[test]
fn asan_autograd_matmul() {
    // The rank-2 MatTape/MatVar matmul path — a matmul→add chain whose
    // backward allocates transpose + matmul-product temps per node, all
    // stored into the shared-struct Vec[Tensor[f32,[?,?]]] columns.
    // LSan-clean over the whole rank-2 tape. Y = (A·B) + C, B=I → grad flows
    // grad_A = ones·Bᵀ = ones, so a.grad_at(0,0) = 1.
    assert_clean_asan_run(
        r#"
import std.autograd.{MatTape, MatVar};
fn main() {
    let t = MatTape.new();
    let a0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);
    let b0: Tensor[f32, [?, ?]] = Tensor.from([[1.0, 0.0], [0.0, 1.0]]);
    let c0: Tensor[f32, [?, ?]] = Tensor.from([[5.0, 6.0], [7.0, 8.0]]);
    let a = MatVar.leaf(t, a0);
    let b = MatVar.leaf(t, b0);
    let c = MatVar.leaf(t, c0);
    let y = a.matmul(b).add(c);
    y.backward();
    println(a.grad_at(0, 0));
}
"#,
        &["1"],
        "asan_autograd_matmul",
    );
}

#[test]
fn asan_autograd_mse_loss() {
    // The composed MSE loss (sub → mul → mean) — exercises the whole tensor
    // tape through a realistic loss + its backward. LSan-clean.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t = TensorTape.new();
    let p0: Tensor[f32, [?]] = Tensor.from([3.0, 5.0]);
    let g0: Tensor[f32, [?]] = Tensor.from([1.0, 1.0]);
    let pred = TensorVar.leaf(t, p0);
    let target = TensorVar.leaf(t, g0);
    let loss = pred.mse(target);
    loss.backward();
    println(pred.grad_at(1));
}
"#,
        &["4"],
        "asan_autograd_mse_loss",
    );
}

#[test]
fn asan_autograd_activations_and_losses() {
    // The Phase-11 autograd activations (`silu`/`softmax`/`gelu`) and losses
    // (`bce`/`cross_entropy`) end-to-end: each forward pushes value/grad
    // tensors onto the tape, backward walks the new op-codes (11 softmax /
    // 12 gelu / 13 bce / 14 cross_entropy; silu composes mul∘sigmoid) with
    // their temp allocations, then the tape drops. Leaf inputs are inline
    // `Tensor.from([…])` — the fresh-temp `ref Tensor` arg path (B-2026-07-18-9)
    // + the f32 element-width threading (B-2026-07-18-10). LSan-clean over the
    // whole tape lifecycle.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let t1 = TensorTape.new();
    let a = TensorVar.leaf(t1, Tensor.from([0.0, 1.0, -2.0]));
    let y = a.silu();
    let z = y.softmax();
    let g = z.gelu();
    let s = g.sum();
    s.backward();
    let ga = a.grad_at(0);
    println(f"{ga == ga}");

    let t2 = TensorTape.new();
    let p = TensorVar.leaf(t2, Tensor.from([0.8, 0.3]));
    let tg = TensorVar.leaf(t2, Tensor.from([1.0, 0.0]));
    let lb = p.bce(tg);
    lb.backward();
    println(f"{p.grad_at(0) < 0.0}");

    let t3 = TensorTape.new();
    let x = TensorVar.leaf(t3, Tensor.from([1.0, 2.0, 3.0]));
    let oh = TensorVar.leaf(t3, Tensor.from([0.0, 0.0, 1.0]));
    let lc = x.cross_entropy(oh);
    lc.backward();
    println(f"{x.grad_at(2) < 0.0}");
}
"#,
        &["true", "true", "true"],
        "asan_autograd_activations_and_losses",
    );
}

#[test]
fn asan_tensor_var_reassign_loop_no_leak() {
    // B-2026-07-17-17: a tensor VARIABLE reassignment (`w = w + d`) never
    // freed the displaced old block — one leak per assignment, unbounded in a
    // loop (the gradient-descent `w = w - lr·grad` update). The slot's old
    // `[rank][dims][data]` pointer is now freed before the overwrite. 5
    // iterations → 5 leaked blocks pre-fix; LSan-clean after.
    assert_clean_asan_run(
        r#"
fn main() {
    let mut w: Tensor[f32, [?]] = Tensor.from([0.0, 0.0, 0.0]);
    let d: Tensor[f32, [?]] = Tensor.from([1.0, 1.0, 1.0]);
    let mut k = 0;
    while k < 5 {
        w = w + d;
        k = k + 1;
    }
    let w0 = w[0];
    println(f"{w0}");
}
"#,
        &["5"],
        "asan_tensor_var_reassign_loop_no_leak",
    );
}

#[test]
fn asan_tensor_temporary_index_no_leak() {
    // B-2026-08-14-17: indexing a tensor-valued TEMPORARY (`(t * 2)[0]`)
    // now compiles, which means codegen mallocs a fresh control block that
    // nothing else owns. It gets the same `FreeTensor` a `let`-bound tensor
    // gets — the fix is literally the workaround this bug had (`let r =
    // t * 2; r[0]`), emitted — so this asserts the free actually runs.
    //
    // LOOPED, because that is the only way a per-iteration leak becomes
    // visible: one stranded block is a fixed cost LSan would still catch,
    // but 50 makes the failure unambiguous and rules out a
    // free-once-outside-the-loop bug. Every producing shape is inside the
    // loop — arithmetic, a nested arithmetic temporary, a constructor and a
    // unary — since each reaches a different guard on the way down and only
    // arithmetic/negation are freed unconditionally.
    assert_clean_asan_run(
        r#"
fn main() {
    let t: Tensor[f64, [3]] = Tensor.from([1.0, 2.0, 3.0]);
    let mut acc: f64 = 0.0;
    let mut k = 0;
    while k < 50 {
        acc = acc + (t * 2.0)[0];
        acc = acc + ((t + t) * 2.0)[1];
        acc = acc + Tensor.from([7.0, 8.0])[1];
        acc = acc + (0.0 - t)[2];
        k = k + 1;
    }
    println(f"{acc}");
}
"#,
        &["750"],
        "asan_tensor_temporary_index_no_leak",
    );
}

#[test]
fn asan_autograd_gradient_descent_training() {
    // The end-to-end training loop — the strongest leak test in the autograd
    // set: it builds and drops a FRESH tape every iteration (each owning its
    // Vec[Tensor] value/grad columns) and reassigns the carried `w` tensor
    // per step. A per-iteration leak (a stranded tape, an unfreed old `w`,
    // an unfreed grad temp) would accumulate across all 12 steps. LSan-clean.
    assert_clean_asan_run(
        r#"
import std.autograd.{TensorTape, TensorVar};
fn main() {
    let target: Tensor[f32, [?]] = Tensor.from([3.0, 5.0, 7.0]);
    let mut w: Tensor[f32, [?]] = Tensor.from([0.0, 0.0, 0.0]);
    // B-2026-08-14-14: annotated at the tensors' element type. Unannotated the
    // literal is f64, and `grad * lr` below narrowed it silently into an f32
    // element-wise op. 0.75 is exact in both widths, so no printed value moves.
    let lr: f32 = 0.75;
    let mut step = 0;
    while step < 12 {
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
    let w0 = w[0];
    println(f"{w0.round()}");
}
"#,
        &["3"],
        "asan_autograd_gradient_descent_training",
    );
}

/// Tensor heap lifecycle (phase-11 codegen core slice): one malloc'd
/// `[rank][dims][data]` block per tensor, freed once at scope exit
/// via `FreeTensor`'s null-guard. Exercises every ownership-transfer
/// shape in one program — construction (all four constructors,
/// including the temporary-dims-Vec eager free), mutation, `let b =
/// a;` move (source slot nulled — double-free would trip ASAN),
/// fn-boundary moves (owned arg + tail return), and `shape()`'s
/// fresh Vec (its own FreeVecBuffer). Leak detection on Linux
/// (detect_leaks=1) additionally catches a missing free.
#[test]
fn asan_tensor_lifecycle_clean() {
    let label = "tensor_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn make() -> Tensor[f64, [2, 2]] {
    let t: Tensor[f64, [2, 2]] = Tensor.full([2, 2], 9.0);
    t
}

fn first(t: Tensor[f64, [2, 2]]) -> f64 {
    t[0, 0]
}

fn main() {
    let z: Tensor[f64, [2, 3]] = Tensor.zeros([2, 3]);
    println(z[1, 2]);
    let o: Tensor[i64, [4]] = Tensor.ones([4]);
    println(o[3]);
    let mut f = Tensor.from([[1, 2], [3, 4]]);
    f[0, 1] = 42;
    println(f[0, 1]);
    let s = f.shape();
    println(s[0]);
    let moved = f;
    println(moved[1, 0]);
    let m = make();
    println(m[1, 1]);
    println(first(make()));
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check FreeTensor double-free/leak on the move-suppression paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["0", "1", "42", "2", "3", "9", "9"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-08-05-7 tail — a fresh-temp Tensor passed DIRECTLY as an OWNED
/// argument (`first(make())`). Owned tensor params are caller-retains, like
/// Vec/String: the callee registers no `FreeTensor`, so a temp with no
/// binding to own it leaked the whole `[rank][dims][data]` block. The
/// kitchen-sink `asan_tensor_lifecycle_clean` fixture caught it once at
/// 56 B; this one scales it 40x so a regression can't hide under a single
/// block, and pairs it with the two shapes that were ALWAYS clean and must
/// stay so — the caller-bound form and the PASSTHROUGH callee, where the
/// result binding owns the block and a caller-side free would double it.
///
/// THE CALLEE READS THE WHOLE BLOCK (`t.sum()`), NOT ONE ELEMENT, and that
/// is what makes this a DEFAULT-BUILD gate. The originating fixture reads
/// `t[0, 0]`, which leaves the allocation provably dead and lets LLVM delete
/// it outright at `-O2` — the B-2026-08-04-17 vacuity, and the reason this
/// leak read as `-O0`-only. Measured unfixed with the summing callee:
/// 2,240 B / 40 blocks at KARAC_OPT_LEVEL=0 **and** at the default `-O2`.
#[test]
fn asan_tensor_fresh_temp_owned_arg_no_leak() {
    assert_clean_asan_run(
        r#"
fn make(v: f64) -> Tensor[f64, [2, 2]] {
    let t: Tensor[f64, [2, 2]] = Tensor.full([2, 2], v);
    t
}

fn total(t: Tensor[f64, [2, 2]]) -> f64 { t.sum() }

fn id(t: Tensor[f64, [2, 2]]) -> Tensor[f64, [2, 2]] { t }

fn main() {
    let mut i: i64 = 0;
    let mut acc: f64 = 0.0;
    while i < 40 {
        // the leaking shape: fresh temp into an owned param
        acc = acc + total(make(1.0));
        // control 1 — caller binding owns it (always clean)
        let m = make(2.0);
        acc = acc + total(m);
        // control 2 — passthrough callee; the RESULT binding owns the block,
        // so the caller-side free must NOT also fire
        let p = id(make(3.0));
        acc = acc + p.sum();
        i = i + 1;
    }
    println(acc);
}
"#,
        &["960"],
        "tensor_fresh_temp_owned_arg",
    );
}

/// Tensor element-wise arithmetic heap lifecycle (phase-11 line 47):
/// every `+ - * /` / unary `-` mallocs a fresh result; operands are
/// borrowed (keep their own `FreeTensor`); a fresh-temp intermediate in
/// `a + b + c` / `(a + b) * (b + c)` / `-a + b` is freed after the copy
/// (the `tensor_operand_is_owned_fresh_temp` path) — a missing free leaks
/// (Linux detect_leaks), a wrong free double-frees (caught everywhere).
/// Operand reuse after the ops pins that nothing was wrongly consumed.
#[test]
fn asan_tensor_arithmetic_lifecycle_clean() {
    let label = "tensor_arithmetic_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a: Tensor[i64, [3]] = Tensor.from([1, 2, 3]);
    let b: Tensor[i64, [3]] = Tensor.from([10, 20, 30]);
    let c: Tensor[i64, [3]] = Tensor.from([100, 200, 300]);
    let r = a + b + c;
    println(r[0]);
    let r2 = (a + b) * (b + c);
    println(r2[1]);
    let n = -a + b;
    println(n[0]);
    let s = a + 5;
    println(s[2]);
    let m = a * 3 - b;
    println(m[1]);
    println(a[0]);
    println(b[2]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh-temp operand free / FreeTensor double-free paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["111", "4840", "9", "8", "-14", "1", "30"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Tensor matmul + transpose heap lifecycle (B-2026-07-14-18). Each
/// call mallocs a fresh result block reclaimed by the let-binding
/// `FreeTensor`; identifier receivers/args are borrowed (keep their own
/// frees); the CHAINED forms (`a.matmul(b).transpose()`,
/// `a.transpose().matmul(b)`) exercise the fresh-temp intermediate free
/// (receiver leg) and the matmul-ARG fresh-temp free
/// (`b.transpose()` as the right-hand side) — a missing free leaks
/// (Linux detect_leaks), a wrong free double-frees (caught everywhere).
/// Receiver + argument reuse after the ops pins borrow-not-move.
#[test]
fn asan_tensor_matmul_transpose_lifecycle_clean() {
    let label = "tensor_matmul_transpose_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a = Tensor.from([[1.0, 2.0], [3.0, 4.0]]);
    let b = Tensor.from([[5.0, 6.0], [7.0, 8.0]]);
    let c = a.matmul(b);
    println(c[0, 0]);
    let t = a.transpose();
    println(t[0, 1]);
    let chained = a.matmul(b).transpose();
    println(chained[0, 1]);
    let arg_temp = a.matmul(b.transpose());
    println(arg_temp[0, 0]);
    let both = a.transpose().matmul(b.transpose());
    println(both[1, 0]);
    println(a[0, 0]);
    println(b[1, 1]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the matmul/transpose fresh-temp free paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["19", "3", "43", "17", "34", "1", "8"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Tensor broadcasting heap lifecycle (phase-11 "Explicit broadcasting
/// methods"). Each `broadcast_*` mallocs a fresh result block the
/// let-binding `FreeTensor` reclaims; the identifier receiver and an
/// identifier argument are borrowed (keep their own frees); a fresh-temp
/// argument (`one + one`) is freed after the copy (the
/// `tensor_operand_is_owned_fresh_temp` path) — a missing free leaks
/// (Linux detect_leaks), a wrong free double-frees (caught everywhere).
/// Receiver + argument reuse after the ops pins borrow-not-move.
#[test]
fn asan_tensor_broadcast_lifecycle_clean() {
    let label = "tensor_broadcast_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let m: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let row: Tensor[i64, [1, 3]] = Tensor.from([[10, 20, 30]]);
    let r = m.broadcast_add(row);
    println(r[1, 2]);
    let col: Tensor[i64, [2, 1]] = Tensor.from([[100], [200]]);
    let c = m.broadcast_mul(col);
    println(c[1, 0]);
    let one: Tensor[i64, [1, 3]] = Tensor.from([[1, 1, 1]]);
    let h = m.broadcast_add(one + one);
    println(h[0, 0]);
    println(m[0, 0]);
    println(row[0, 1]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the broadcast result FreeTensor / fresh-temp-arg free paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["36", "800", "3", "1", "20"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Tensor reduction heap lifecycle (phase-11 line 47, Slice B). Full
/// reduces return a scalar (no malloc); axis reduces malloc a fresh
/// rank-1-lower block that the let-binding `FreeTensor` must reclaim. A
/// chained `m.sum_axis(0)` on a let-bound axis-reduce result and receiver
/// reuse after the reduces pin that nothing is double-freed or read after
/// free.
#[test]
fn asan_tensor_reduce_lifecycle_clean() {
    let label = "tensor_reduce_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a: Tensor[i64, [2, 3]] = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    println(a.sum());
    println(a.max());
    let s0 = a.sum_axis(0);
    println(s0[1]);
    let s1 = a.sum_axis(1);
    println(s1[0]);
    let m = a.mean_axis(0);
    println(m[2]);
    let chained = m.sum_axis(0);
    println(chained);
    println(a[1, 2]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the axis-reduce result FreeTensor / double-free paths",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["21", "6", "7", "6", "4.5", "10.5", "6"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Tensor shape-transform heap lifecycle (phase-11 follow-on slice):
/// reshape / permute / slice / squeeze each malloc a fresh result
/// block and copy the data; the receiver is borrowed (keeps its own
/// `FreeTensor`). A chained `permute(..).reshape(..)` additionally
/// exercises the fresh-temporary free of the intermediate (the
/// `receiver_is_fresh_temp` path) — a missing free leaks (Linux
/// detect_leaks), a wrong free double-frees (caught everywhere).
#[test]
fn asan_tensor_shape_transform_lifecycle_clean() {
    let label = "tensor_shape_transform_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let r = a.reshape([3, 2]);
    println(r[2, 1]);
    let p = a.permute([1, 0]);
    println(p[2, 1]);
    let sl = a.slice(1, 1, 3);
    println(sl[1, 1]);
    let b = Tensor.from([[[7], [8], [9]]]);
    let sq = b.squeeze();
    println(sq[2]);
    let chained = a.permute([1, 0]).reshape([6]);
    println(chained[5]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh-result free and the chained-intermediate free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["6", "6", "6", "9", "6"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `iter_axis` Vec[Tensor] heap lifecycle (phase-11 follow-on slice):
/// the result `Vec` holds a buffer of tensor `ptr`s, each a separate
/// `[rank][dims][data]` block. The `Vec[Tensor]` cleanup
/// (`track_vec_of_tensors_var` → `cleanup.tdrop`) must free every
/// element block and the outer buffer exactly once — a missing free
/// leaks (Linux detect_leaks), a double free trips ASAN everywhere.
/// Exercises the `let`-bound result (indexed) and the for-loop
/// method-source materialization (which queues the synth temp's
/// cleanup), plus the rank-1 `Vec[T]` form (a plain buffer).
#[test]
fn asan_tensor_iter_axis_lifecycle_clean() {
    let label = "tensor_iter_axis_lifecycle";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let rows = a.iter_axis(0);
    println(rows.len());
    println(rows[1][2]);
    for c in a.iter_axis(1) {
        println(c[0]);
    }
    let v = Tensor.from([10, 20, 30]);
    let scal = v.iter_axis(0);
    println(scal[2]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the Vec[Tensor] element drop (track_vec_of_tensors_var)",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["2", "6", "1", "2", "3", "30"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// Owned fn-return / method-return receiver of a shape transform
/// (phase-11 line 39). `make().reshape(..)` and `f.build().slice(..)`
/// each produce a fresh OWNED tensor temporary that the transform
/// copies out of and must then free exactly once
/// (`tensor_receiver_is_owned_fresh_temp` → the `receiver_is_fresh_temp`
/// free). A missing free leaks the intermediate (Linux detect_leaks);
/// a free applied to a *borrowed* receiver, or a double free of the
/// result, trips ASAN everywhere. The identifier receivers in the
/// other tensor ASAN tests pin the negative (don't-free) side; this
/// pins the fresh-temp positive side for both the free-fn and method
/// return sources.
#[test]
fn asan_tensor_fnret_receiver_free_clean() {
    let label = "tensor_fnret_receiver_free";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn make() -> Tensor[i64, [2, 3]] {
    Tensor.from([[1, 2, 3], [4, 5, 6]])
}
struct Factory {}
impl Factory {
    fn build(ref self) -> Tensor[i64, [2, 3]] {
        Tensor.from([[10, 20, 30], [40, 50, 60]])
    }
}
fn main() {
    let r = make().reshape([3, 2]);
    println(r[2, 1]);
    let f = Factory {};
    let m = f.build().slice(0, 1, 2);
    println(m[0, 2]);
    let p = make().permute([1, 0]);
    println(p[2, 1]);
    let sq = make().squeeze();
    println(sq[1, 2]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the fresh fn-return/method-return receiver free",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["6", "60", "6", "6"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// `ref Tensor` returns are BORROWS — the by-value ref ABI hands back
/// the owner's block pointer, so the borrow must never be freed
/// (phase-11 line 40). Exercises the free-fn return (inline transform
/// receiver + let-bound) and a user `-> ref Tensor` accessor method
/// (inline transform receiver + let-bound). The ordering is adversarial:
/// `h.view().permute(..)` (an inline borrow receiver) is followed by a
/// later `h.view()` and `a[0,0]` — if a borrow receiver were wrongly
/// freed (the chained-method span-collision hazard), the later reads
/// would be use-after-free (ASAN) and the owner's scope-exit drop a
/// double free. Clean here means every owner block is freed exactly
/// once and no borrow frees anything.
#[test]
fn asan_tensor_ref_return_borrow_clean() {
    let label = "tensor_ref_return_borrow";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn firstrow(t: ref Tensor[i64, [2, 3]]) -> ref Tensor[i64, [2, 3]] {
    t
}
struct Holder { t: Tensor[i64, [2, 3]] }
impl Holder {
    fn view(ref self) -> ref Tensor[i64, [2, 3]] {
        self.t
    }
}
fn main() {
    let a = Tensor.from([[1, 2, 3], [4, 5, 6]]);
    let r = firstrow(a).reshape([3, 2]);
    println(r[2, 1]);
    let b = firstrow(a);
    println(b[1, 2]);
    let h = Holder { t: Tensor.from([[10, 20, 30], [40, 50, 60]]) };
    let m = h.view().permute([1, 0]);
    println(m[2, 1]);
    let v = h.view();
    println(v[1, 2]);
    println(a[0, 0]);
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             a borrowed ref-Tensor return must not be freed",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["6", "6", "60", "60", "1"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

/// B-2026-08-29-12 — a `Tensor` FIELD of a struct was never freed: 72 B per
/// struct, with no ref return, no method, and no read of the field needed.
///
/// The field is one pointer to the `[rank][dims][data]` block, and
/// `emit_struct_drop_synthesis`'s `FieldDrop` classifier had no arm for it,
/// so it read as `None` and the drop walk skipped it. The fix is the pair
/// `FieldDrop::TensorFree` (free it) + a `"Tensor"` arm in
/// `zero_struct_move_caps_mono` (null the source on a move) — and the
/// second half is not optional: with only the drop, a struct move armed BOTH
/// structs against one block and aborted with `free(): double free or
/// corruption`. `tensor-field-moved-into-vec` is the row that catches that
/// AT -O2 — verified by building with the drop arm alone, where it aborts
/// while the flat `tensor-field-moved` row below still passes, because -O2
/// has elided the flat one's blocks entirely.
///
/// This is the row the parent B-2026-08-28-74 called "a tensor ref-return".
/// It is not — `asan_tensor_ref_return_borrow_clean` above reached the leak
/// only because it happened to hold its tensor in a `struct Holder`.
///
/// THE `Vec[Holder]` ROW IS THE ONE THAT ASSERTS AT -O2, and that is why it
/// leads. This suite compiles at -O2, where LLVM promotes a never-freed
/// non-escaping tensor block out of existence: measured pre-fix, every flat
/// spelling below drops to 8 allocations and reports clean, while the
/// `Vec`-held pair keeps its blocks and leaks 144 B (11 allocs / 9 frees).
/// The flat rows are kept because they are the minimal statements of the
/// bug and they DO assert under the -O0 leg (`scripts/asan-o0-leg.sh`), but
/// their `min_allocs` floor is deliberately low — at -O2 there is nothing
/// left to count.
#[test]
fn asan_struct_tensor_field_is_freed() {
    // The -O2-visible row: a Vec of tensor-bearing structs. Real floor.
    assert_clean_asan_run_min_allocs(
        "struct H { t: Tensor[i64, [2, 3]], n: i64 }\n\
             fn mk(n: i64) -> H { return H { t: Tensor.from([[n, 2, 3], [4, 5, 6]]), n: n }; }\n\
             fn main() {\n\
             \x20   let mut hs: Vec[H] = [];\n\
             \x20   hs.push(mk(1));\n\
             \x20   hs.push(mk(2));\n\
             \x20   println(hs[1].n);\n\
             }\n",
        &["2"],
        "tensor-field-in-vec",
        // 3 = the Vec's buffer plus one block per tensor-bearing element,
        // measured identically on macOS and arm64 Linux. The 10 here was an
        // estimate that had never been checkable — see B-2026-09-07-26 and
        // [`asan_alloc_floor`]. Below 3, a tensor block has gone missing,
        // which is the elision this floor exists to catch.
        3,
    );
    // The -O2-visible MOVE row: a struct moved into a `Vec`, so the tensor
    // stays live in memory the optimizer cannot promote away. This is the
    // row that fails (double free, not a leak) if the `"Tensor"` arm in
    // `zero_struct_move_caps_mono` is removed while the drop arm stays.
    assert_clean_asan_run_min_allocs(
        "struct H { t: Tensor[i64, [2, 3]], n: i64 }\n\
             fn mk(n: i64) -> H { return H { t: Tensor.from([[n, 2, 3], [4, 5, 6]]), n: n }; }\n\
             fn main() {\n\
             \x20   let h = mk(1);\n\
             \x20   let mut hs: Vec[H] = [];\n\
             \x20   hs.push(h);\n\
             \x20   println(hs[0].n);\n\
             }\n",
        &["1"],
        "tensor-field-moved-into-vec",
        // 2, measured on both hosts — the moved-from struct contributes no
        // second block, which is the whole point of the `zero_struct_move_caps`
        // half of the fix, so this row allocates one fewer than its sibling
        // above. The 10 was an estimate; see B-2026-09-07-26. Below 2 a
        // tensor block has been elided and the row asserts nothing.
        2,
    );
    // The flat spellings. -O2 elides these (see the note above), so they do
    // their real work under the -O0 leg and NOT here.
    //
    // They carried a `min_allocs` floor of 4 until B-2026-09-07-26, and that
    // floor was never capable of failing: it compared against ASAN's raw
    // process-wide count, whose per-host start-up floor (10 on arm64 Linux,
    // 199 on macOS) clears 4 on its own. Measured floor-relative, ALL FIVE
    // ROWS ALLOCATE EXACTLY ZERO at -O2 — which is precisely what the note
    // above predicts, so the fixtures are behaving as designed and it is the
    // floor that was decorative.
    //
    // The plain predicate is therefore the honest one. A floor of 0 would
    // pass unconditionally, and a floor that cannot fail is worse than no
    // floor: it reads, to anyone scanning this file, as a guard. What these
    // rows still assert here is real but narrow — ASAN-clean, and the right
    // value out — and their allocation-level claim lives in
    // `scripts/asan-o0-leg.sh`. The two `Vec`-held rows above keep their
    // floors, because they are the ones with something left to count.
    let rows: [(&str, &str, &str); 5] = [
            (
                "fn main() { let h = H { t: Tensor.from([[10, 20, 30], [40, 50, 60]]), n: 5 }; println(7); }",
                "7",
                "tensor-field-never-read",
            ),
            (
                "fn main() { let h = H { t: Tensor.from([[10, 20, 30], [40, 50, 60]]), n: 5 }; println(h.t[1, 2]); }",
                "60",
                "tensor-field-read",
            ),
            // The move: with the drop but WITHOUT the move-suppression null,
            // this is a double free, not a leak.
            (
                "fn main() { let h = H { t: Tensor.from([[10, 20, 30], [40, 50, 60]]), n: 5 }; let g = h; println(g.t[1, 2]); }",
                "60",
                "tensor-field-moved",
            ),
            // A by-value param: a Tensor-bearing struct is NOT copy-supported
            // (`field_copy_supported` falls to its `_ => false` default), so the
            // param stays caller-retains and the callee mints no second owner.
            // Clean before the fix and after — the row exists to keep it so.
            (
                "fn take(h: H) -> i64 { return h.t[1, 2]; }\n\
                 fn main() { let h = H { t: Tensor.from([[10, 20, 30], [40, 50, 60]]), n: 5 }; println(take(h)); }",
                "60",
                "tensor-field-by-value-param",
            ),
            // Returned from a fn rather than built in place.
            (
                "fn mk() -> H { return H { t: Tensor.from([[10, 20, 30], [40, 50, 60]]), n: 5 }; }\n\
                 fn main() { let h = mk(); println(h.n); }",
                "5",
                "tensor-field-returned-struct",
            ),
        ];
    for (body, expected, label) in rows {
        let src = format!("struct H {{ t: Tensor[i64, [2, 3]], n: i64 }}\n{body}\n");
        assert_clean_asan_run(&src, &[expected], label);
    }
}

/// Mono-body owned-local cleanup (phase-11): a monomorphized
/// (shape-generic) body that binds an owned `Tensor` local must free it
/// exactly once at scope exit when it's dropped, and NOT free it when
/// it's moved out as the return value (the caller frees). Exercises both
/// in one program: `build_id` returns its `out` local (moved out → caller
/// frees once), `diag_then_drop` drops its `t` local (freed at the mono's
/// scope exit). Both have auto-par-eligible loops, so this also guards
/// the `branch_cancel_ptr` reset (a stale ptr would mis-compile, not
/// leak). A missing drain leaks `t` (Linux detect_leaks); a double-free
/// of a moved-out tensor trips ASAN everywhere.
#[test]
fn asan_mono_body_owned_tensor_local_clean() {
    let label = "mono_body_owned_tensor_local";
    if !asan_available() {
        eprintln!("[{label}] ASAN unavailable on this host — skipping");
        return;
    }
    let Some((stdout, status)) = run_under_asan(
        r#"
fn build_id[N](n: i64) -> Tensor[f64, [N, N]] {
    let mut out: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);
    for i in 0..n { out[i, i] = 1.0; }
    out
}
fn diag_then_drop[N](n: i64) -> f64 {
    let mut t: Tensor[f64, [?, ?]] = Tensor.zeros([n, n]);
    for i in 0..n { t[i, i] = 2.0; }
    let mut s = 0.0;
    for i in 0..n { s = s + t[i, i]; }
    s
}
fn main() {
    let a = build_id(3);
    println(a[2, 2]);
    let b = build_id(2);
    println(b[0, 0]);
    println(diag_then_drop(4));
}
"#,
        label,
    ) else {
        eprintln!("[{label}] setup failed — skipping");
        return;
    };
    assert!(
        status.success(),
        "[{label}] ASAN reported a memory error (exit code {:?}) — \
             check the mono-body FreeTensor drain (drop vs move-out)",
        status.code()
    );
    assert_eq!(
        stdout.trim().lines().collect::<Vec<_>>(),
        vec!["1", "1", "8"],
        "[{label}] unexpected stdout (ASAN passed, output mismatched)"
    );
}

#[test]
fn asan_column_tensor_map_freed_no_leak() {
    // S6c-2: `Column.map` / `Tensor.map` each allocate a FRESH result
    // container (control + data buffer, plus a validity bitmap for the
    // column). The result binds to a `let` and must be freed at scope exit
    // via the same `track_column_var` / tensor cleanup the binop results
    // use — this asserts no leak / double-free of the map-allocated
    // buffers. Looped so a per-iteration leak accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let d = c.map(|x| x * 2);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let e = t.map(|x| x + 1);
    d.sum() + e.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["680"], // (20 + 14) * 20
        "asan_column_tensor_map_freed_no_leak",
    );
}

#[test]
fn asan_column_tensor_zip_with_freed_no_leak() {
    // S6c-2b: `Column.zip_with` / `Tensor.zip_with` each allocate a FRESH
    // result container while READING (borrowing) both operands. This
    // asserts: (1) the fresh result binds to a `let` and frees at scope
    // exit (like the map / binop results); (2) the `other` operand — a
    // `ref` arg — is NOT double-freed (it's a borrow, freed once as its own
    // binding). Looped so any per-iteration leak accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let a: Column[i64] = Column.from_vec([1, 2, 3, 4]);
    let b: Column[i64] = Column.from_vec([10, 20, 30, 40]);
    let c = a.zip_with(b, |x, y| x + y);
    let t: Tensor[i64, [4]] = Tensor.from([1, 2, 3, 4]);
    let u: Tensor[i64, [4]] = Tensor.from([2, 2, 2, 2]);
    let v = t.zip_with(u, |x, y| x * y);
    c.sum() + v.sum()
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        &["2600"], // (110 + 20) * 20
        "asan_column_tensor_zip_with_freed_no_leak",
    );
}

#[test]
#[ignore = "B-2026-08-05-35 sweep: does not compile — no codegen handler for `argmax` on a non-identifier receiver. Silently SKIPPED until the harness learned to fail on a codegen error."]
fn asan_column_tensor_argmin_freed_no_leak() {
    // S6c: `Column.argmin`/`argmax` and `Tensor.argmin`/`argmax` return a
    // POD `Option[i64]` (no heap), but the receiver columns / tensors own
    // heap. This asserts the bound receivers free at scope exit AND that a
    // FRESH `Column.from_vec(...).argmin()` temp receiver is freed by the
    // owned-temp machinery (not leaked). Looped so a per-iteration leak
    // accumulates for LSan.
    assert_clean_asan_run(
        r#"
fn idx(o: Option[i64]) -> i64 {
    match o {
        Some(i) => i,
        None => -1,
    }
}
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let a = idx(c.argmin()) + idx(c.argmax());
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let b = idx(t.argmin()) + idx(t.argmax());
    // Fresh-temp receiver — must be freed by the owned-temp path.
    let d = idx(Column.from_vec([2, 8, 1, 8]).argmax());
    a + b + d
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        // c: 5+1=6; t: 1+4=5; d: argmax of [2,8,1,8] first 8 at idx 1 = 1.
        // (6 + 5 + 1) * 20 = 240.
        &["240"],
        "asan_column_tensor_argmin_freed_no_leak",
    );
}

#[test]
fn asan_column_tensor_sorted_argsort_freed_no_leak() {
    // S6c: `Column.sorted`/`argsort` and `Tensor.sorted`/`argsort` each
    // allocate a FRESH result `Vec` (a malloc'd buffer). Each result binds
    // to a `let` and must be freed at scope exit via the standard `Vec`
    // cleanup — this asserts no leak / double-free of the sort-allocated
    // buffers over a loop. Results are `let`-bound then indexed (the
    // standard `Stats.sort` idiom; the auto-par slot-published Column/Tensor
    // early-free that once corrupted fresh-temp-arg patterns is fixed —
    // B-2026-07-03-32).
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let c: Column[i64] = Column.from_vec([5, 9, 3, 3, 8, 1]);
    let cs: Vec[i64] = c.sorted();
    let ca: Vec[i64] = c.argsort();
    let mut n: Column[i64] = Column.with_capacity(5);
    n.push(10); n.push_null(); n.push(5); n.push_null(); n.push(20);
    let ns: Vec[i64] = n.sorted();
    let na: Vec[i64] = n.argsort();
    let t: Tensor[i64, [6]] = Tensor.from([4, 2, 7, 2, 9, 9]);
    let ts: Vec[i64] = t.sorted();
    let ta: Vec[i64] = t.argsort();
    cs[0] + ca[0] + ns[0] + na[0] + ts[0] + ta[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        // cs[0]=1, ca[0]=5, ns[0]=5, na[0]=2, ts[0]=2, ta[0]=1 → 16; *20 = 320.
        &["320"],
        "asan_column_tensor_sorted_argsort_freed_no_leak",
    );
}

#[test]
fn asan_tensor_sorted_argsort_narrow_widths_no_leak() {
    // B-2026-07-03-35 fixed: narrow-width `Tensor.sorted` mallocs a widened
    // 8-byte key copy, sorts it, narrows back into a `Vec[T]`-width buffer,
    // and frees the scratch; narrow `argsort` mallocs a widened key view and
    // frees it after the sort. Plus the tensor itself (`Tensor.from`) is a
    // malloc'd block freed at scope exit. This asserts no leak / double-free
    // of any of those over a loop — i32, u32, and f32 tensors. `let`-bound +
    // indexed idiom.
    assert_clean_asan_run(
        r#"
fn inner() -> i64 {
    let ti: Tensor[i32, [4]] = Tensor.from([40, 10, 30, 20]);
    let si: Vec[i32] = ti.sorted();
    let ai: Vec[i64] = ti.argsort();
    let tu: Tensor[u32, [3]] = Tensor.from([30, 10, 20]);
    let us: Vec[u32] = tu.sorted();
    let ua: Vec[i64] = tu.argsort();
    let tf: Tensor[f32, [4]] = Tensor.from([2.5, 0.5, 3.5, 1.5]);
    let fs: Vec[f32] = tf.sorted();
    let fa: Vec[i64] = tf.argsort();
    si.len() + ai[0] + us.len() + ua[0] + fs.len() + fa[0]
}
fn main() {
    let mut acc: i64 = 0;
    let mut i: i64 = 0;
    while i < 20 {
        acc = acc + inner();
        i = i + 1;
    }
    println(f"{acc}");
}
"#,
        // si.len()=4, ai[0]=1, us.len()=3, ua[0]=1, fs.len()=4, fa[0]=1 → 14;
        // *20 = 280.
        &["280"],
        "asan_tensor_sorted_argsort_narrow_widths_no_leak",
    );
}
