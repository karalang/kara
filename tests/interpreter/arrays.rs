//! fixed-size arrays and SoA element storage -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter arrays::
//!
//! New fixtures about fixed-size arrays and SoA element storage belong in this file.

use super::*;

/// B-2026-09-02-37 — the ORACLE for
/// `codegen_array_of_heap_elements_as_an_enum_payload` (tests/codegen.rs),
/// pinned to the same string.
///
/// The interpreter carries values rather than packing them into i64 payload
/// words, so it has no pack/unpack pair to disagree with itself and was correct
/// on every line — which is what made this an A/B divergence. Pinned here so
/// the compiled fix cannot later be "restored" by weakening this side.
#[test]
fn test_array_of_heap_elements_as_an_enum_payload() {
    assert_eq!(
        run(r#"struct P { s: String, n: i64 }
enum W { A(Array[String, 2]), B }
fn takeP(p: ref P) -> i64 { println(f"{p.s} {p.n}"); return p.n }
fn viaCall(x: Option[Array[String, 2]]) { match x { Some(t) => { println(f"{t}") } None => { println("n") } } }
fn generic[T: Display](x: Option[T]) { match x { Some(t) => { println(f"g:{t}") } None => { println("n") } } }
fn main() {
    let ctl: Array[String, 2] = ["ab", "cd"];
    println(f"{ctl}");

    let ai: Array[i64, 2] = [1, 2];
    let oi: Option[Array[i64, 2]] = Some(ai);
    match oi { Some(t) => { println(f"{t}") } None => { println("n") } }

    let a1: Array[String, 1] = ["one"];
    let o1: Option[Array[String, 1]] = Some(a1);
    println(f"{o1}");
    match o1 { Some(t) => { println(f"{t}") } None => { println("n") } }

    let a2: Array[String, 2] = ["ab", "cd"];
    let o2: Option[Array[String, 2]] = Some(a2);
    println(f"{o2}");
    match o2 { Some(t) => { println(f"{t}"); println(t[0]); println(t[1]) } None => { println("n") } }

    let a3: Array[String, 2] = ["ef", "gh"];
    viaCall(Some(a3));

    let a4: Array[String, 2] = ["ij", "kl"];
    let r: Result[Array[String, 2], String] = Ok(a4);
    match r { Ok(t) => { println(f"{t}") } Err(e) => { println(e) } }

    let a5: Array[String, 2] = ["mn", "op"];
    let w = W.A(a5);
    match w { W.A(t) => { println(f"{t}") } W.B => { println("b") } }

    let v1: Vec[i64] = [1, 2];
    let v2: Vec[i64] = [3];
    let av: Array[Vec[i64], 2] = [v1, v2];
    let ov: Option[Array[Vec[i64], 2]] = Some(av);
    match ov { Some(t) => { println(f"{t[0]}"); println(f"{t[1]}") } None => { println("n") } }

    let n1: Array[i64, 2] = [5, 6];
    let n2: Array[i64, 2] = [7, 8];
    let m: Array[Array[i64, 2], 2] = [n1, n2];
    let om: Option[Array[Array[i64, 2], 2]] = Some(m);
    match om { Some(t) => { let r0: Array[i64, 2] = t[0]; let r1: Array[i64, 2] = t[1]; println(f"{r0} {r1}") } None => { println("n") } }

    let ap: Array[P, 2] = [P { s: "qr", n: 1 }, P { s: "st", n: 2 }];
    let op: Option[Array[P, 2]] = Some(ap);
    match op { Some(t) => { let x = takeP(t[0]); let y = takeP(t[1]); println(f"{x}{y}") } None => { println("n") } }

    let a6: Array[String, 2] = ["uv", "wx"];
    generic(Some(a6));
}
"#),
        "[ab, cd]\n[1, 2]\nSome([one])\n[one]\nSome([ab, cd])\n[ab, cd]\nab\ncd\n[ef, gh]\n[ij, kl]\n[mn, op]\n[1, 2]\n[3]\n[5, 6] [7, 8]\nqr 1\nst 2\n12\ng:[uv, wx]\n"
    );
}

/// B-2026-08-31-29 (oracle half) — `let g = ref arr[i]` over an `Array[T, N]`
/// base reads the element.
///
/// The interpreter was RIGHT throughout, on every line of this fixture, which
/// is precisely what made the bug a run-vs-build divergence rather than a
/// design question: codegen's named-local `ref` arm dispatched no container
/// class at all and lowered an Array as though its slot were a `{ptr, i64,
/// i64}` Vec header, so `Array[i64, 3]` built cleanly and segfaulted while
/// `--interp` printed the right answer. This half is the oracle the compiled
/// side is asserted against.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_ref_binding_over_an_array_base_reads_the_element`, pinned to the same
/// string.
///
/// B-2026-08-31-36 added the four `slice*` lines, and this file is the half
/// that was WRONG for them — the reverse of the Array case above. `ref s[2]`
/// bound the whole `Slice` rather than the element, so it printed
/// `[10, 20, 30]` here against `30` on all three compiled surfaces, and
/// `slicearith` failed outright with "operator 'Add' is not defined for
/// operands of type 'Slice' and 'Int'". A slice element IS an element of the
/// backing array, so the fix binds the same `Value::ElemRef` the Vec path
/// binds, with the index shifted by the window's start — which is why
/// `slicealias` holds without any new machinery.
#[test]
fn test_ref_binding_over_an_array_base() {
    assert_eq!(
        run(r#"struct P { a: i64, b: i64 }

fn main() {
    let sa: Array[i64, 3] = Array[10, 20, 30];
    let g = ref sa[2];
    println(f"scalar {g}");

    let v: Vector[i64, 4] = Vector[i64, 4](1, 2, 3, 4);
    let va: Array[Vector[i64, 4], 3] = Array[v, v, v];
    let gv = ref va[2];
    println(f"vector {gv.reduce_sum()}");

    let pa: Array[P, 2] = Array[P { a: 1, b: 2 }, P { a: 3, b: 4 }];
    let gp = ref pa[1];
    println(f"struct {gp.a} {gp.b}");

    let mut ma: Array[i64, 3] = Array[1, 2, 3];
    let gm = ref ma[0];
    ma[0] = 99;
    println(f"alias {gm}");

    let vv: Vec[i64] = [7, 8, 9];
    let gc = ref vv[1];
    println(f"vec {gc}");

    let sv: Vec[i64] = [10, 20, 30];
    let sl: Slice[i64] = sv.as_slice();
    let gs = ref sl[2];
    println(f"slice {gs}");
    let ssum: i64 = gs + 5;
    println(f"slicearith {ssum}");

    let pv: Vec[P] = [P { a: 1, b: 2 }, P { a: 3, b: 4 }];
    let pl: Slice[P] = pv.as_slice();
    let gsp = ref pl[1];
    println(f"slicestruct {gsp.a} {gsp.b}");

    let mut mv: Vec[i64] = [1, 2, 3];
    let ml: Slice[i64] = mv.as_slice();
    let gsa = ref ml[0];
    mv[0] = 77;
    println(f"slicealias {gsa}");
}
"#),
        "scalar 30\nvector 10\nstruct 3 4\nalias 99\nvec 8\n\
             slice 30\nslicearith 35\nslicestruct 3 4\nslicealias 77\n"
    );
}

#[test]
fn array_ordering_in_the_interpreter() {
    // B-2026-08-27-42 -- the `Array[T, N]` sibling of the tuple test above,
    // and the same shape a level down: `type_supports_ord` recurses through
    // `Type::Array`, so `karac check` printed "All checks passed." while the
    // interpreter died on the catch-all arm whose message claims the
    // typechecker reports this as a hard error. It does not, and did not.
    //
    // Ordering goes through `value_compare`, which has had an Array arm since
    // B-2026-06-30-15 -- so unlike the compiled twin, which needed the
    // comparator WRITTEN, this side needed only the dispatch. Both order by
    // the same rule, which is what makes this the oracle for
    // `test_e2e_array_ordering_and_equality`.
    //
    // The last two rows are the deliberate NON-fix, pinned so a later widening
    // has to be deliberate. A NESTED array declines on both backends, and for
    // a reason specific to this backend: `Value::Array` is how a `Vec` is
    // represented here too, so no value-shape test can tell the two apart, and
    // admitting the element would make a tuple holding a `Vec` orderable here
    // while codegen still refuses it. Declining is what keeps the two gates
    // accepting the same set. A bare-float element declines for the sibling
    // reason the tuple test records: `value_compare` orders a float by
    // `total_cmp` (NaN last), which is a sort key's order, not `<`'s.
    let src = "fn main() {
            let a: Array[i64, 2] = Array[1, 2];
            let b: Array[i64, 2] = Array[1, 3];
            let c: Array[i64, 2] = Array[1, 2];
            println(f\"{a < b}\");
            println(f\"{b < a}\");
            println(f\"{a < c}\");
            println(f\"{a <= c}\");
            println(f\"{a >= c}\");
            println(f\"{b > a}\");
            println(f\"{a == c}\");
            let s: Array[String, 2] = Array[\"aa\", \"bb\"];
            let t: Array[String, 2] = Array[\"aa\", \"bc\"];
            println(f\"{s < t}\");
            println(f\"{t < s}\");
            let p: Array[i64, 3] = Array[2, 0, 0];
            let q: Array[i64, 3] = Array[1, 9, 9];
            println(f\"{p < q}\");
            let ta: Array[(i64, i64), 2] = Array[(1, 2), (3, 4)];
            let tb: Array[(i64, i64), 2] = Array[(1, 2), (3, 5)];
            println(f\"{ta < tb}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "true\nfalse\nfalse\ntrue\ntrue\ntrue\ntrue\n\
         true\nfalse\n\
         false\n\
         true\n"
    );
}

#[test]
fn array_equality_is_content_equality_in_the_interpreter() {
    // B-2026-08-27-25's oracle. The interpreter was ALREADY right here --
    // `Array[T, N]` is a `Value::Array` and took the `Vec` arm -- and that is
    // exactly why this test exists: the row is a run-vs-build split where
    // `karac run` works and `karac build` refuses, so the interpreter's answers
    // are the reference its E2E twin
    // (`test_e2e_array_equality_compares_contents`) is checked against. Pinning
    // them keeps the two halves from drifting apart in the other direction.
    let src = "#[derive(PartialEq)]
        struct WrapS { a: Array[String, 2] }
        fn main() {
            let x: Array[i64, 2] = Array[1, 2];
            let y: Array[i64, 2] = Array[1, 9];
            println(f\"{x == y}\");
            println(f\"{x != y}\");
            let p: Array[String, 2] = Array[\"ab\", \"cd\"];
            let q: Array[String, 2] = Array[\"ab\", \"cd\"];
            let r: Array[String, 2] = Array[\"ab\", \"zz\"];
            println(f\"{p == q}\");
            println(f\"{p == r}\");
            println(f\"{WrapS { a: Array[\"ab\", \"cd\"] } == WrapS { a: Array[\"ab\", \"cd\"] }}\");
            let n3: Array[Array[i64, 3], 2] = Array[Array[1, 2, 3], Array[4, 5, 6]];
            let m3: Array[Array[i64, 3], 2] = Array[Array[1, 2, 3], Array[4, 5, 7]];
            println(f\"{n3 == m3}\");
            let mut km: Map[Array[String, 2], i64] = Map.new();
            km.insert(Array[\"ab\", \"cd\"], 1);
            km.insert(Array[\"ab\", \"cd\"], 2);
            km.insert(Array[\"ab\", \"zz\"], 3);
            println(f\"{km.len()}\");
        }";
    assert_eq!(
        run_no_errors(src),
        "false\ntrue\ntrue\nfalse\ntrue\nfalse\n2\n"
    );
}

// ── Arrays ─────────────────────────────────────────────────────

#[test]
fn test_array_literal() {
    assert_eq!(
        run("fn main() {\n\
                 let arr = [10, 20, 30];\n\
                 println(arr[1]);\n\
             }"),
        "20\n"
    );
}

// ── Array methods ──────────────────────────────────────────────

#[test]
fn test_array_len() {
    assert_eq!(
        run("fn main() { let arr = [1, 2, 3]; println(arr.len()); }"),
        "3\n"
    );
}

#[test]
fn test_prefix_array_literal_runtime() {
    assert_eq!(
        run("fn main() { let a = Array[4, 5, 6]; println(a[0]); }"),
        "4\n"
    );
}

#[test]
fn test_repeat_literal_array_prefix_runtime() {
    assert_eq!(
        run("fn main() {
                 let a = Array[42; 3];
                 println(a[0]);
                 println(a[2]);
             }"),
        "42\n42\n"
    );
}

#[test]
fn test_a_boxed_array_payload_runs_its_element_drop_bodies() {
    // B-2026-09-10-27 (interpreter half) — an `Option`/`Result` payload that is
    // a fixed `Array[T, N]`.
    //
    // The row was an AGREED SILENCE: `Option[Array[R, 2]]` printed no body on
    // `--interp`, the JIT or either AOT lane. So neither backend could be fixed
    // alone, and b55b5e8 (B-2026-09-12-6) demonstrated it the hard way: its
    // codegen array arm reached the seeded head through a shared core, the
    // compiled surfaces started printing two bodies against the interpreter's
    // zero, and the two fixtures pinning the silence went red. It gated the arm
    // off rather than ship the divergence, and named this row as the owner of
    // "its own interpreter half to write". This is that half.
    //
    // TWO ARMS WERE NEEDED, not one, because the spellings arrive by different
    // routes — the same asymmetry B-2026-09-09-20 recorded for tuples:
    //
    //   * the registration gate (`optres_payload_te_runs_user_drop`) asked
    //     `type_expr_runs_user_drop` about the ARRAY, and `Array` is not a
    //     declared struct or enum, so a NAMED local registered nothing; and
    //   * a FRESH-TEMP argument has no binding to key a declared type on and
    //     goes through the value-driven arg path instead, which had no
    //     `Value::Array` case at all.
    //
    // Wiring only the first left the row's OWN cell (a fresh-temp argument)
    // silent, which is why both are here.
    //
    // The value-driven arm is scoped to the `Option`/`Result` payload entry
    // rather than added to `run_discarded_value_user_drops` beside its
    // `Value::Tuple` sibling. That asymmetry is deliberate and measured: a
    // discarded BARE array local already runs its element bodies through its own
    // registration, so widening the general walk would run them twice. The last
    // assertion below is that cell.
    const PRELUDE: &str = "struct Ar { id: i64 }\n\
         impl Drop for Ar { fn drop(mut ref self) { println(f\"dAr{self.id}\"); } }\n";

    // THE ROW'S OWN CELL: a fresh-temp argument to a by-value param.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn plainD(x: Option[Array[Ar, 2]]) {{\n\
             \x20   match x {{ Some(t) => {{ println(f\"s:{{t[0].id}}\") }} None => {{ println(\"n\") }} }}\n\
             }}\n\
             fn main() {{\n\
             \x20   plainD(Some([Ar {{ id: 1 }}, Ar {{ id: 2 }}]));\n\
             \x20   println(\"end\");\n}}\n"
        )),
        "s:1\ndAr1\ndAr2\nend\n"
    );

    // A NAMED `Option` local — the registration-gate half, a different route.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let a: Array[Ar, 2] = [Ar {{ id: 1 }}, Ar {{ id: 2 }}];\n\
             \x20   let o: Option[Array[Ar, 2]] = Some(a);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\nx\n"
    );

    // The `Result` twin on the `Ok` side.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let a: Array[Ar, 2] = [Ar {{ id: 1 }}, Ar {{ id: 2 }}];\n\
             \x20   let r: Result[Array[Ar, 2], i64] = Result.Ok(a);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\nx\n"
    );

    // N = 3, in index order.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let a: Array[Ar, 3] = [Ar {{ id: 1 }}, Ar {{ id: 2 }}, Ar {{ id: 3 }}];\n\
             \x20   let o: Option[Array[Ar, 3]] = Some(a);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\ndAr3\nx\n"
    );

    // CONTROL — a drop-free element stays silent, so the walk keys on the
    // element's own classification rather than on array-ness.
    assert_eq!(
        run("struct Pr { id: i64 }\n\
             fn main() {\n\
             \x20   let a: Array[Pr, 2] = [Pr { id: 1 }, Pr { id: 2 }];\n\
             \x20   let o: Option[Array[Pr, 2]] = Some(a);\n\
             \x20   println(\"x\");\n}\n"),
        "x\n"
    );

    // CONTROL, AND THE ONE THAT GUARDS THE SCOPING: a discarded BARE array
    // local already ran its bodies before this change. Exactly ONE pair — this
    // is what fails if the value-driven arm is widened into
    // `run_discarded_value_user_drops`.
    assert_eq!(
        run(&format!(
            "{PRELUDE}fn main() {{\n\
             \x20   let a: Array[Ar, 2] = [Ar {{ id: 1 }}, Ar {{ id: 2 }}];\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\nx\n"
    );

    // B-2026-09-12-24 — the MONOMORPHIC user enum at an `Array` payload, the
    // interpreter half of this row\'s sibling change in
    // `emit_user_enum_payload_bodies`. Both halves land together, for the
    // reason the header above records the hard way.
    //
    // The DECLARED head is the discriminator here and it has to be: the
    // interpreter represents `Array[T, N]` and `Vec[T]` with the same
    // `Value::Array`, so a value-shaped test cannot separate them. A
    // monomorphic declaration can (`Array` vs `Vec`); a generic one cannot,
    // and an earlier draft keyed on the value fired for `Slot[Vec[R]]` too —
    // silent on every compiled surface — printing two bodies against
    // codegen\'s zero. That is the divergence this row exists to avoid, made
    // in the opposite direction.
    assert_eq!(
        run(&format!(
            "{PRELUDE}enum Bin {{ Packed(Array[Ar, 2]), Bare }}\n\
             fn main() {{\n\
             \x20   let b: Bin = Bin.Packed([Ar {{ id: 1 }}, Ar {{ id: 2 }}]);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\nx\n"
    );

    // MATCHED OUT — the arm binding owns the elements, so exactly one pair.
    assert_eq!(
        run(&format!(
            "{PRELUDE}enum Bin {{ Packed(Array[Ar, 2]), Bare }}\n\
             fn main() {{\n\
             \x20   let b: Bin = Packed([Ar {{ id: 1 }}, Ar {{ id: 2 }}]);\n\
             \x20   match b {{ Packed(a) => {{ println(f\"s:{{a[0].id}}\") }} Bare => {{ println(\"n\") }} }}\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "s:1\ndAr1\ndAr2\nx\n"
    );

    // B-2026-09-10-20 — REPINNED, and this cell is why it was pinned. It read
    // `x` alone, as the BOUNDARY twin of the codegen cell
    // `boundary-mono-enum-vec-payload-stays-silent`, and its note asked that
    // the missing `Vec` arm "move both at once". That arm is written: the
    // name-keyed payload-bodies head in codegen now carries a `Vec` field row
    // beside its `Array` one, this file\'s `Some("Vec")` DECLARED-head arm is
    // the interpreter half, and the program prints `dAr1 dAr2 x` on
    // `--interp`, the JIT, `-O0` and `-O2` auto-par alike (valgrind at `-O0`:
    // 0 errors, nothing lost). The cell keeps its place so the boundary it
    // guards stays legible — what moved is which side of it this shape sits
    // on.
    assert_eq!(
        run(&format!(
            "{PRELUDE}enum Vbin {{ V(Vec[Ar]), Z }}\n\
             fn main() {{\n\
             \x20   let v: Vbin = Vbin.V(Vec[Ar {{ id: 1 }}, Ar {{ id: 2 }}]);\n\
             \x20   println(\"x\");\n}}\n"
        )),
        "dAr1\ndAr2\nx\n"
    );
}

/// B-2026-08-28-57, interpreter leg — the PARITY PIN for the compiled fix.
///
/// Unlike its siblings this one was green before the fix and after it: the
/// interpreter always ran a fixed array's element bodies, at the binding's
/// live-range end, and the whole divergence lived on the compiled side. It is
/// here because the compiled twin
/// (`codegen::e2e_fixed_array_elements_run_their_user_drop_bodies`) asserts the
/// same strings, so a future change that "fixes" a mismatch by moving the
/// INTERPRETER has to break this test to do it. The row it closes was a
/// placement divergence, and placement is what a one-sided edit would quietly
/// re-negotiate.
#[test]
fn fixed_array_elements_run_their_user_drop_bodies() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         enum H { A(R), B }\n\
         enum J { A(i64), B }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "enum-elems-mixed",
            "fn main() { let a: Array[E, 2] = [E.B, E.A(R { id: 3 })];\n\
             \x20            println(\"mid\"); println(\"end\"); }\n",
            "drop E\ndrop E\ndrop R3\nmid\nend\n",
        ),
        (
            "payload-only-enum-elem",
            "fn main() { let a: Array[H, 1] = [H.A(R { id: 4 })]; println(\"mid\"); }\n",
            "drop R4\nmid\n",
        ),
        (
            "struct-elems",
            "fn main() { let a: Array[R, 2] = [R { id: 1 }, R { id: 2 }];\n\
             \x20            println(\"mid\"); }\n",
            "drop R1\ndrop R2\nmid\n",
        ),
        (
            "moved-on-enum",
            "fn main() { let a: Array[E, 2] = [E.B, E.B]; let b = a; println(\"moved\"); }\n",
            "drop E\ndrop E\nmoved\n",
        ),
        (
            "nested-scope",
            "fn main() { { let a: Array[E, 1] = [E.B]; println(\"inner\") }\n\
             \x20            println(\"outer\"); }\n",
            "drop E\ninner\nouter\n",
        ),
        (
            "no-drop-elems",
            "fn main() { let a: Array[J, 2] = [J.A(1), J.B]; println(\"mid\"); }\n",
            "mid\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

/// B-2026-08-05: `File.read` / `BufReader.read` accept a fixed `Array[u8, N]`
/// as the `mut Slice[u8]` buffer, matching AOT.
///
/// The idiom is the one `examples/relay` ships for `TcpStream.read`:
///
///     let mut buf: Array[u8, N] = [0u8; N];
///     f.read(mut buf)
///
/// AOT coerces the array to the slice parameter and reads fine; the
/// interpreter rejected it with a runtime error, so the SAME program worked
/// under `karac build` and died under `karac run --interp`. `File.write`
/// already took a `Value::Array` deliberately ("be permissive at the
/// interpreter level — the typechecker enforces the declared shape"); read was
/// the inconsistent half. Found dogfooding the first file-I/O probe of the
/// Cumulus app.
#[test]
fn file_read_accepts_a_fixed_array_buffer_like_aot() {
    let dir = std::env::temp_dir().join("karac_file_read_array_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("probe.bin");
    let p = path.to_str().unwrap().replace('\\', "/");
    let src = format!(
        "fn main() with reads(FileSystem) writes(FileSystem) {{\n\
         let mut out: Vec[u8] = Vec.new();\n\
         for i in 0..8 {{ out.push((i * 17) as u8); }}\n\
         match File.create(\"{p}\") {{\n\
         Ok(f) => {{ match f.write(out.as_slice()) {{ Ok(n) => {{ }} Err(e) => {{ println(\"werr\"); }} }} }}\n\
         Err(e) => {{ println(\"cerr\"); }}\n\
         }}\n\
         match File.open(\"{p}\") {{\n\
         Ok(f) => {{\n\
         let mut buf: Array[u8, 8] = [0u8; 8];\n\
         match f.read(mut buf) {{\n\
         Ok(n) => {{ println(f\"read {{n}} first={{buf[0]}} last={{buf[7]}}\"); }}\n\
         Err(e) => {{ println(\"rerr\"); }}\n\
         }}\n\
         }}\n\
         Err(e) => {{ println(\"oerr\"); }}\n\
         }}\n\
         }}\n"
    );
    let out = run(&src);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        out, "read 8 first=0 last=119\n",
        "the interpreter must read through a fixed Array buffer exactly as AOT does; got: {out}"
    );
}

/// B-2026-09-15-26 — an `Array[T, N]`-typed STRUCT FIELD runs its elements'
/// `Drop` bodies on the tree-walk backend.
///
/// The interpreter twin of the codegen E2E
/// (`e2e_array_typed_struct_field_runs_its_element_drop_bodies`), which lives
/// behind `--features llvm` and so is invisible to the DEFAULT leg. Both
/// backends were silent here, so the row moved both gates in one commit; this
/// fixture is what keeps the interpreter half under the gate CI actually runs.
///
/// Two sites on this side, both keyed on a head name that `Array` is not: the
/// relevance gate `field_te_runs_user_drop`, and then the field WALK in
/// `drop_user_drop_fields_of_value` — `Vec` and `Array` are one runtime value
/// (`Value::Array`), so the field reached the `Vec` arm, failed its
/// declared-head gate, and fell through that arm's unconditional `continue`.
#[test]
fn test_array_typed_struct_field_runs_its_element_drop_bodies() {
    const H: &str = "struct D { id: i64, s: String }\n\
         impl Drop for D { fn drop(mut ref self) { println(f\"dD{self.id}\") } }\n\
         fn mkd(n: i64) -> D { return D { id: n, s: f\"ss{n}\" }; }\n";
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Array[D, 2] }}\n\
             fn main() {{\n\
             \x20   let h: H = H {{ f: [mkd(1), mkd(2)] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "the flat Array field"
    );
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Array[Vec[D], 1] }}\n\
             fn main() {{\n\
             \x20   let h: H = H {{ f: [[mkd(1), mkd(2)]] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "an Array field whose element is itself a container"
    );
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Array[D, 2] }}\n\
             impl Drop for H {{ fn drop(mut ref self) {{ println(\"dH\") }} }}\n\
             fn main() {{\n\
             \x20   let h: H = H {{ f: [mkd(1), mkd(2)] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dH\ndD1\ndD2\nend\n",
        "the holder's own body runs first, then the elements'"
    );
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Array[D, 2] }}\nstruct G {{ h: H }}\n\
             fn main() {{\n\
             \x20   let g: G = G {{ h: H {{ f: [mkd(1), mkd(2)] }} }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "one struct deeper"
    );
    // CONTROL — an Array field of non-Drop elements still runs nothing.
    assert_eq!(
        run("struct N { v: i64 }\n\
             struct H { f: Array[N, 2] }\n\
             fn main() {\n\
             \x20   let h: H = H { f: [N { v: 1 }, N { v: 2 }] };\n\
             \x20   println(\"end\");\n\
             }\n"),
        "end\n",
        "control: non-Drop elements"
    );
    // B-2026-09-12-21 — the array reaches the field by a MOVE out of a named
    // local rather than as a literal. Exactly one pair of bodies: the source
    // binding does not keep a second walk.
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Array[D, 2] }}\n             fn main() {{\n             \x20   let a: Array[D, 2] = [mkd(1), mkd(2)];\n             \x20   let h = H {{ f: a }};\n             \x20   println(\"end\");\n             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "the array is MOVED into the field from a local"
    );
    // B-2026-09-12-21, generic half. This arm is the one that fires
    // VALUE-driven off the declared array type, so it was already correct here
    // while codegen's mono selector asked about the bare `T` and declined —
    // the divergence B-2026-09-15-26 opened and the element-subst closed. Kept
    // as the interpreter side of that pair.
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: Array[T, 2] }}\n             fn main() {{\n             \x20   let g: G[D] = G {{ a: [mkd(1), mkd(2)] }};\n             \x20   println(\"end\");\n             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a generic parent whose field is Array[T, N]"
    );
    // B-2026-09-15-35 — a bare generic param bound to a CONTAINER. PINNED AT
    // SILENCE here while this row's selector resolved the array's ELEMENT
    // rather than the whole field TypeExpr, because the whole-TE substitution
    // reaches it on the codegen side and this walk gated on a DECLARED
    // container type that `Path("T")` is not — a divergence rather than a fix.
    //
    // -15-35 moved both gates in one commit, so the pins move with them: this
    // walk's `Vec`/`VecDeque` arm gained the bare-generic-param exception
    // B-2026-08-02-14 established for the plain-struct case, and codegen gained
    // a `bare_param_container` leg in `user_drop_field_indices_mono` (only the
    // GATE was missing there — its emitter arms already key off the
    // whole-TE-substituted field TE).
    //
    // ONE LEVEL, plain named element, on both sides. The first attempt here
    // routed through `run_discarded_value_user_drops`, which RECURSES, and
    // `T = Vec[Vec[D]]` then printed `dD1` on this backend alone — the nested
    // cell below is pinned against exactly that.
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n             fn main() {{\n             \x20   let g: G[Array[D, 2]] = G {{ a: [mkd(1), mkd(2)] }};\n             \x20   println(\"end\");\n             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a bare generic param bound to an Array (B-2026-09-15-35)"
    );
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n             fn main() {{\n             \x20   let g: G[Vec[D]] = G {{ a: [mkd(1), mkd(2)] }};\n             \x20   println(\"end\");\n             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a bare generic param bound to a Vec (B-2026-09-15-35)"
    );
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n\
             fn main() {{\n\
             \x20   let mut d: VecDeque[D] = VecDeque.new();\n\
             \x20   d.push_back(mkd(1));\n\
             \x20   d.push_back(mkd(2));\n\
             \x20   let g: G[VecDeque[D]] = G {{ a: d }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a bare generic param bound to a VecDeque (B-2026-09-15-35)"
    );
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n\
             fn main() {{\n\
             \x20   let v: Vec[D] = [mkd(1), mkd(2)];\n\
             \x20   let g: G[Vec[D]] = G {{ a: v }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "the container is MOVED into the bare-param field from a local (B-2026-09-15-35)"
    );
    // PINNED — a CONTAINER element under the bare param stays an agreed
    // silence: codegen's leg holds itself to a plain named element so the two
    // backends keep asking one question. B-2026-09-15-23's subject, one
    // position over.
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n\
             fn main() {{\n\
             \x20   let g: G[Vec[Vec[D]]] = G {{ a: [[mkd(1), mkd(2)]] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a bare param bound to Vec[Vec[D]] — the two rows' legs composing (B-2026-09-15-23)"
    );
    assert_eq!(
        run(&format!(
            "{H}struct G[T] {{ a: T }}\n\
             fn main() {{\n\
             \x20   let g: G[Array[Vec[D], 1]] = G {{ a: [[mkd(1)]] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\nend\n",
        "likewise a bare param bound to Array[Vec[D], 1] (B-2026-09-15-23)"
    );
    // PINNED — a nested container in a `Vec` field is still an agreed silence
    // on both backends (B-2026-09-15-23), so the two gates stay in step.
    assert_eq!(
        run(&format!(
            "{H}struct H {{ f: Vec[Array[D, 1]] }}\n\
             fn main() {{\n\
             \x20   let h: H = H {{ f: [[mkd(1)], [mkd(2)]] }};\n\
             \x20   println(\"end\");\n\
             }}\n"
        )),
        "dD1\ndD2\nend\n",
        "a Vec[Array[D, 1]] field runs its innermost bodies (B-2026-09-15-23)"
    );
}

/// B-2026-09-19-58 — the INTERPRETER twin of `tests/codegen.rs`'s
/// `e2e_named_array_local_into_seeded_match_scrutinee_has_one_owner`.
///
/// Byte-identical source and expectation. This side was already CORRECT at
/// every cell — the compiled surfaces ABORTED before printing anything — so it
/// is the oracle the compiled half was moved onto rather than a change of its
/// own, and its job here is to fail loudly if a later fix moves the oracle
/// instead of the backend.
///
/// The `b/` cells are the deliberate boundary; the INLINE-payload width
/// (`Array[S, 1]`, three words, never boxed) is deliberately absent because it
/// still aborts on both the `match` and the `let` spelling. See the codegen
/// twin's doc for the full cell-by-cell rationale.
#[test]
fn test_named_array_local_into_seeded_match_scrutinee_has_one_owner() {
    let out = run(r#"struct S { tag: String }
impl Drop for S { fn drop(mut ref self) { println(f"  dS{self.tag}") } }
struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct N { id: i64 }
impl Drop for N { fn drop(mut ref self) { println(f"  dN{self.id}") } }
enum W { P(Array[S, 2]), Q }
fn mka() -> Array[S, 2] { return [S { tag: f"gggggggg0" }, S { tag: f"gggggggg1" }] }
fn main() {
    println("m/arr");    { let a: Array[S, 2] = [S { tag: f"aaaaaaaa0" }, S { tag: f"aaaaaaaa1" }]; match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0].tag}") }, Option.None => { println("  n") } } }
    println("m/wild");   { let a: Array[S, 2] = [S { tag: f"bbbbbbbb0" }, S { tag: f"bbbbbbbb1" }]; match Option.Some(a) { Option.Some(_) => { println("  w") }, Option.None => { println("  n") } } }
    println("m/str");    { let a: Array[String, 2] = [f"cccccccc0", f"cccccccc1"]; match Option.Some(a) { Option.Some(v) => { println(f"  r:{v[0]}") }, Option.None => { println("  n") } } }
    println("m/res");    { let a: Array[S, 2] = [S { tag: f"dddddddd0" }, S { tag: f"dddddddd1" }]; match Result.Ok(a) { Result.Ok(v) => { println("  r") }, Result.Err(e) => { println("  n") } } }
    println("m/one");    { let a: Array[R, 1] = [R { id: 1, s: f"eeeeeeee0" }]; match Option.Some(a) { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/noheap"); { let a: Array[N, 2] = [N { id: 2 }, N { id: 3 }]; match Option.Some(a) { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/fresh");  { match Option.Some(mka()) { Option.Some(v) => { println(f"  r:{v[0].tag}") }, Option.None => { println("  n") } } }
    println("b/let");    { let a: Array[S, 2] = [S { tag: f"hhhhhhhh0" }, S { tag: f"hhhhhhhh1" }]; let o = Option.Some(a); match o { Option.Some(v) => { println("  r") }, Option.None => { println("  n") } } }
    println("b/mono");   { let a: Array[S, 2] = [S { tag: f"iiiiiiii0" }, S { tag: f"iiiiiiii1" }]; match W.P(a) { W.P(v) => { println("  r") }, W.Q => { println("  n") } } }
    println("end")
}
"#);
    assert_eq!(out, "m/arr\n  r:aaaaaaaa0\n  dSaaaaaaaa0\n  dSaaaaaaaa1\nm/wild\n  w\nm/str\n  r:cccccccc0\nm/res\n  r\n  dSdddddddd0\n  dSdddddddd1\nm/one\n  r\n  dR1\nb/noheap\n  r\n  dN2\n  dN3\nb/fresh\n  r:gggggggg0\n  dSgggggggg0\n  dSgggggggg1\nb/let\n  r\n  dShhhhhhhh0\n  dShhhhhhhh1\nb/mono\n  r\nend\n", "got:\n{out}");
}

#[test]
fn test_generic_enum_array_payload_runs_element_drop_bodies() {
    let hdr = "struct R { id: i64, s: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               enum G[T] { X(T), Y }\n";
    let target = run(&format!(
        "{hdr}fn main() {{ {{ let a: Array[R, 2] = [R {{ id: 21, s: f\"aaa\" }}, \
         R {{ id: 22, s: f\"bbb\" }}]; let w: G[Array[R, 2]] = G.X(a); println(\"mid\") }} \
         println(\"done\") }}"
    ));
    assert_eq!(
        target, "dR21\ndR22\nmid\ndone\n",
        "a generic enum's Array payload must run every element's body exactly once, \
         in the order the three compiled surfaces already use"
    );

    let k_vec = run(&format!(
        "{hdr}fn main() {{ {{ let v: Vec[R] = [R {{ id: 31, s: f\"aaa\" }}, \
         R {{ id: 32, s: f\"bbb\" }}]; let w: G[Vec[R]] = G.X(v); println(\"mid\") }} \
         println(\"done\") }}"
    ));
    assert_eq!(
        k_vec, "dR31\ndR32\nmid\ndone\n",
        "B-2026-09-20-62 RETIRED THIS CELL'S SILENCE: the generic Vec payload now runs \
         its elements' bodies on all four surfaces, so the agreement this pin was \
         protecting has moved to the correct answer rather than being broken"
    );

    let k_struct = run(&format!(
        "{hdr}fn main() {{ {{ let r: R = R {{ id: 41, s: f\"aaa\" }}; \
         let w: G[R] = G.X(r); println(\"mid\") }} println(\"done\") }}"
    ));
    assert_eq!(
        k_struct, "dR41\nmid\ndone\n",
        "a generic plain-struct payload was already correct and must not double"
    );

    // Its own literal rather than a `format!`: this cell interpolates nothing,
    // and `clippy::useless_format` is denied by the gate. Braces are single
    // here for the same reason.
    let k_ng = run("struct R { id: i64, s: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Ng { X(Array[R, 2]), Y }\n\
         fn main() { { let a: Array[R, 2] = [R { id: 71, s: f\"aaa\" }, \
         R { id: 72, s: f\"bbb\" }]; let w: Ng = Ng.X(a); println(\"mid\") } \
         println(\"done\") }");
    assert_eq!(
        k_ng, "dR71\ndR72\nmid\ndone\n",
        "the NON-generic Array payload reaches the same arm by its declared head \
         and must be untouched"
    );
}

/// B-2026-09-21-8 — a ONE-ELEMENT `Array` enum payload whose element holds a
/// `shared` handle is laid out the same way by the layout pass and by the pack.
///
/// `enum One { A(Array[Sd, 1]), N }` over `struct Sd { h: Inner }` and
/// `shared struct Inner { tag: String }` SEGFAULTED on jit, `-O0` and `-O2`
/// against a correct `--interp`, as a four-line whole program with no call, no
/// match and no `impl Drop` needed anywhere.
///
/// `payload_word_count_for_type_expr` answered a DIFFERENT WIDTH in its two
/// windows. It recognises a `shared` type as one pointer word through
/// `shared_types`, which the STRUCT LLVM build fills — after `declare_enums` has
/// run. So at layout time a shared type reached through a plain struct's FIELD
/// missed that arm, fell through to the struct recursion, and was sized by its own
/// fields: `Sd` measured 3 words at declare time and 1 word at compile time. The
/// `BoxedArray` pass compares `elem_words * n > field_words`, so 3 > 1 classified
/// the payload BOXED while the pack side — reading real LLVM widths — rode it
/// INLINE. `__karac_drop_E` then `inttoptr`'d the RC handle, walked it as an
/// `Array[Sd, 1]` and `free`d it, reading the refcount word as a pointer:
/// `Invalid read of size 8` at address 0x1.
///
/// The fix reads `shared_type_decl_names` beside `shared_types` — the name-only
/// set `register_struct_metadata` fills for exactly this window, which
/// `enum_drop_kind_for_type_expr`'s `SharedRc` arm already consults for the DIRECT
/// payload position (B-2026-09-10-11). The nested position was never wired to it.
///
/// THE CELLS THAT ARE NOT THE FAULT ARE MOST OF THIS FIXTURE. `g` is
/// `Array[Sd, 2]`, two words, which takes the boxed path and was always correct;
/// `h` is a direct `Sd` payload (`NestedStruct`); `i` is a heap-free element;
/// `d` is a `shared` struct that owns no heap, where the same width error sized
/// the payload at 1 and simply left it unclassified. `e` passes the value to a
/// by-value callee and `f` matches on it, the two spellings that carry the most
/// ownership machinery. All nine agree byte-for-byte across `--interp`,
/// `karac run`, `-O0` and `-O2`.
///
/// WHAT THIS FIXTURE DOES NOT PIN, deliberately: the RC box is still STRANDED on
/// the inline shapes (32 B for a heap-owning `Inner`, 16 B for a heap-free one).
/// An inline one-element array payload has no drop kind at all, which is this
/// row's second face and is split out as B-2026-09-21-9 with its three sites
/// named. No memory-sanitizer cell accompanies this fixture for that reason; what
/// it guards is the crash, and the crash is what the output can see.
#[test]
fn one_element_array_enum_payload_with_a_shared_handle_does_not_crash() {
    let src = r#"
shared struct Inner { tag: String }
shared struct Ik { k: i64 }
struct Sd { h: Inner }
impl Drop for Sd { fn drop(mut ref self) { println("dSd") } }
struct Sq { h: Inner }
struct Sk { h: Ik }
struct Sn { k: i64 }

enum One { A(Array[Sd, 1]), N }
enum Bare { A(Array[Sq, 1]), N }
enum Direct { A(Array[Inner, 1]), N }
enum Flat { A(Array[Ik, 1]), N }
enum Wide { A(Array[Sd, 2]), N }
enum Plain { A(Sd), N }
enum Scalar { A(Array[Sn, 1]), N }

fn eat(g: One) -> i64 { match g { One.A(x) => { return 7; } One.N => { return 0; } } }

fn a_bind() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); println("a"); }
fn b_nobody() { let g: Bare = Bare.A([Sq { h: Inner { tag: "q" } }]); println("b"); }
fn c_shared_elem() { let g: Direct = Direct.A([Inner { tag: "q" }]); println("c"); }
fn d_heapfree() { let g: Flat = Flat.A([Ik { k: 5 }]); println("d"); }
fn e_byvalue() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); println(f"e:{eat(g)}"); }
fn f_match() { let g: One = One.A([Sd { h: Inner { tag: "q" } }]); match g { One.A(x) => { println("f"); } One.N => { println("fn"); } } }
fn g_wide() { let g: Wide = Wide.A([Sd { h: Inner { tag: "q" } }, Sd { h: Inner { tag: "r" } }]); println("g"); }
fn h_direct() { let g: Plain = Plain.A(Sd { h: Inner { tag: "q" } }); println("h"); }
fn i_scalar() { let g: Scalar = Scalar.A([Sn { k: 5 }]); println("i"); }

fn main() {
    a_bind();
    b_nobody();
    c_shared_elem();
    d_heapfree();
    e_byvalue();
    f_match();
    g_wide();
    h_direct();
    i_scalar();
    println("end");
}
"#;
    assert_eq!(
        run(src),
        "dSd\na\nb\nc\nd\ne:7\ndSd\nf\ndSd\ndSd\ndSd\ng\ndSd\nh\ni\nend\n"
    );
}
