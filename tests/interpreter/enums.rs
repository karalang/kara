//! enum definitions, variants, discriminants, payloads -- fixtures for `tests/interpreter.rs`.
//!
//! Split out of `tests/interpreter.rs` on 2026-09-21. The TEST TARGET is
//! unchanged: this file is a module of that target, so
//! `cargo test --features llvm --test interpreter` still runs everything
//! and CI needs no edit. Run this area alone with:
//!
//!     cargo test --features llvm --test interpreter enums::
//!
//! New fixtures about enum definitions, variants, discriminants, payloads belong in this file.

use super::*;

/// B-2026-08-31-39 — the ORACLE for
/// B-2026-09-03-2 — the ORACLE twin of
/// `codegen_generic_forward_to_a_second_generic_resolves_the_payload`
/// (tests/codegen.rs), pinned to the same string.
///
/// The interpreter was correct throughout: it has no monomorphization, so a
/// generic body forwarding its `Option[T]` param to a second generic function
/// simply passes the value along and the arm binding is whatever was there.
/// Every compiled surface printed the payload BOX's address as an integer
/// instead — a different number on every run. Pinned here so the compiled fix
/// cannot later be "restored" by weakening this side to match a regressed
/// backend.
#[test]
fn test_generic_forward_to_a_second_generic_resolves_the_payload() {
    assert_eq!(
        run(
            r#"fn sink[T: Display](x: Option[T]) { match x { Some(t) => { println(f"s:{t}") } None => { println("n") } } }
fn fwd[T: Display](x: Option[T]) { sink(x) }
fn mid[T: Display](x: Option[T]) { sink(x) }
fn outer[T: Display](x: Option[T]) { mid(x) }
fn fwdU[U: Display](x: Option[U]) { sink(x) }
struct H { }
impl H {
    fn m[T: Display](ref self, x: Option[T]) { match x { Some(t) => { println(f"m:{t}") } None => { println("n") } } }
}
fn fwdm[T: Display](h: ref H, x: Option[T]) { h.m(x) }
fn main() {
    let a: Array[String, 2] = ["uv", "wx"];
    fwd(Some(a));
    fwd(Some(7));
    let b: Array[String, 2] = ["yz", "ab"];
    outer(Some(b));
    let c: Array[String, 2] = ["cd", "ef"];
    fwdU(Some(c));
    let h = H { };
    let d: Array[String, 2] = ["gh", "ij"];
    fwdm(h, Some(d));
    let e: Array[String, 2] = ["kl", "mn"];
    fwd(Some(e));
    let t: (i64, i64) = (5, 6);
    fwd(Some(t));
    let g: Array[i64, 3] = [1, 2, 3];
    fwd(Some(g));
    let sl: Slice[i64] = g[0..2];
    fwd(Some(sl));
}
"#
        ),
        "s:[uv, wx]\ns:7\ns:[yz, ab]\ns:[cd, ef]\nm:[gh, ij]\ns:[kl, mn]\ns:(5, 6)\ns:[1, 2, 3]\ns:[1, 2]\n"
    );
}

#[test]
fn test_method_on_enum_unit_variant_literal() {
    // B-2026-07-13-4: `Dir.North.code()` — a method on an enum unit-variant
    // LITERAL — parses as a 3-segment path call the interpreter had no
    // evaluation rule for ("path 'Dir.North.code' has no interpreter evaluation
    // rule"). The typechecker now types it and lowering materializes the
    // receiver into a fresh local, so the tree-walk interpreter dispatches it
    // like `let d = Dir.North; d.code()`.
    let src = "enum Dir { North, South, East, West }
        impl Dir {
            fn code(self) -> i64 {
                match self { Dir.North => 0, Dir.South => 1, Dir.East => 2, Dir.West => 3 }
            }
            fn opposite(self) -> i64 { self.code() + 100 }
        }
        fn main() {
            println(Dir.North.code().to_string());
            println(Dir.West.code().to_string());
            let z = Dir.East.code() + 5;
            println(z.to_string());
            println(Dir.South.opposite().to_string());
        }";
    assert_eq!(run_no_errors(src), "0\n3\n7\n101\n");
}

#[test]
fn enum_eq_is_not_gated_on_a_drop_classification() {
    // B-2026-08-27-19 — `==` on an enum was gated on a DROP classification, so
    // an enum the drop path deliberately declines to own compared its payload
    // WORDS.
    //
    // `enum_has_heap_payload` folds over `field_drop_kinds`, and
    // `enum_drop_kind_for_type_expr` refuses to classify a payload struct
    // `NestedStruct` unless it is word-ALIGNED — because the drop path and the
    // deep-copy-on-entry must stay symmetric, and classifying one the entry-copy
    // cannot duplicate turns a status-quo leak into a DOUBLE FREE. That `None` is
    // correct for ownership and wrong as an answer to "can these bytes be
    // compared as words". `subword` is the shape: `{ a: i32, b: i32, s: String }`
    // spends one word per field where LLVM packs the two `i32`s into eight bytes,
    // so it is not word-aligned, and `==` compared the `String`'s heap POINTER.
    //
    // `zeros` and `nan` are the SECOND symptom of the same gate, and the reason
    // the new predicate is not simply "owns heap". A float payload has no heap at
    // all, so no drop classification would ever have routed it — but bit equality
    // is not IEEE equality in exactly the two places the standard defines
    // specially: `0.0` and `-0.0` are equal with different bits, NaN is unequal
    // to itself with identical bits. Both answered backwards. The operands come
    // from opaque functions so LLVM cannot constant-fold the comparison and
    // answer correctly without running the emitted code.
    //
    // `scalarstruct` and `ints` are the controls: an all-scalar payload, struct
    // or not, is wholly inline and keeps the cheaper word compare. `AllScalar`
    // is deliberately three `i32`s — also not word-aligned, so it proves the
    // predicate keys on what the bytes MEAN rather than on alignment.
    //
    // The `-tag` lines guard the same dereference hazard B-2026-08-27-16 closed:
    // the payload walk must not run when the tags differ, since a unit variant's
    // uninitialised words become a wild pointer once a field is read through.
    //
    // Codegen twin: `test_e2e_enum_eq_is_not_gated_on_a_drop_classification`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct TwoNarrow { a: i32, b: i32, s: String }
#[derive(Hash, Eq, PartialEq)]
struct AllScalar { a: i32, b: i32, c: i32 }
#[derive(Hash, Eq, PartialEq)]
enum Holder { W(TwoNarrow), S(AllScalar), N }
#[derive(PartialEq)]
enum Num { F(f64), I(i64), N }

fn mk(n: i64) -> String { return f"v{n}"; }
fn nan64(z: f64) -> f64 { return z / z; }
fn negzero(z: f64) -> f64 { return -z; }

fn main() {
    let x: Holder = Holder.W(TwoNarrow { a: 1, b: 2, s: mk(1) });
    let y: Holder = Holder.W(TwoNarrow { a: 1, b: 2, s: mk(1) });
    println(f"subword={x == y}");
    println(f"subword-ne-s={x == Holder.W(TwoNarrow { a: 1, b: 2, s: mk(2) })}");
    println(f"subword-ne-a={x == Holder.W(TwoNarrow { a: 9, b: 2, s: mk(1) })}");
    println(f"subword-ne-b={x == Holder.W(TwoNarrow { a: 1, b: 9, s: mk(1) })}");
    println(f"subword-tag={x == Holder.N}");

    let p: Holder = Holder.S(AllScalar { a: 1, b: 2, c: 3 });
    println(f"scalarstruct={p == Holder.S(AllScalar { a: 1, b: 2, c: 3 })}");
    println(f"scalarstruct-ne={p == Holder.S(AllScalar { a: 1, b: 2, c: 4 })}");

    let z = 0.0;
    let neg = negzero(z);
    let nan = nan64(z);
    println(f"zeros={Num.F(0.0) == Num.F(neg)}");
    println(f"nan={Num.F(nan) == Num.F(nan)}");
    println(f"floats={Num.F(1.5) == Num.F(1.5)}");
    println(f"floats-ne={Num.F(1.5) == Num.F(2.5)}");
    println(f"ints={Num.I(7) == Num.I(7)}");
    println(f"ints-ne={Num.I(7) == Num.I(8)}");
    println(f"num-tag={Num.F(1.5) == Num.N}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "subword=true\nsubword-ne-s=false\nsubword-ne-a=false\n\
         subword-ne-b=false\nsubword-tag=false\n\
         scalarstruct=true\nscalarstruct-ne=false\nzeros=true\n\
         nan=false\nfloats=true\nfloats-ne=false\nints=true\n\
         ints-ne=false\nnum-tag=false\n"
    );
}

#[test]
fn assert_eq_compares_an_enum_by_content() {
    // B-2026-08-27-17 — `assert_eq` / `assert_ne` compared an ENUM by its
    // payload WORDS, so an assertion over two structurally-equal enums PASSED on
    // the interpreter and FAILED compiled. A test asserting enum equality got the
    // wrong verdict, which is worse than a wrong value: it makes a correct
    // program look broken, and for `assert_ne` a broken one look correct.
    //
    // `test_assert.rs` compiled its two operands and called `compile_binop`
    // directly. That dispatches an aggregate by shape and sent the enum to the
    // word-wise `compile_struct_eq` — a heap-pointer compare for any payload
    // owning heap. The `==` OPERATOR site had a structural path all along
    // (`compile_enum_eq`), selected by running `enum_name_of_expr` over the
    // operand EXPRESSIONS; the assertion helper simply never consulted it.
    //
    // The fix lifts that decision into one helper both sites call, so "how a
    // compiled `==` on an enum is decided" has a single definition. Unlike
    // `Vec.contains` (B-2026-08-27-13), this site HAS the operand expressions, so
    // it can reach the operator's own path rather than needing the pointer
    // comparator.
    //
    // EVERY PAYLOAD IS BUILT BY A FUNCTION CALL, never a literal: two identical
    // string literals can fold to one global, and a pointer compare would then
    // answer `true` and hide the defect. The struct, string and int lines are
    // controls that were already correct — `compile_struct_eq` recurses per
    // FIELD, and only an enum's fields are raw payload words.
    //
    // Codegen twin: `test_e2e_assert_eq_compares_an_enum_by_content`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
enum E { A(String), Pair(i64, String), B }
#[derive(Hash, Eq, PartialEq)]
struct S { a: i64, s: String }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    assert_eq(E.A(mk(1)), E.A(mk(1)));
    println(f"eq-payload ok");
    assert_ne(E.A(mk(1)), E.A(mk(2)));
    println(f"ne-payload ok");
    assert_eq(E.Pair(7, mk(3)), E.Pair(7, mk(3)));
    println(f"eq-tuple ok");
    assert_ne(E.Pair(7, mk(3)), E.Pair(7, mk(4)));
    println(f"ne-tuple ok");
    assert_eq(E.B, E.B);
    println(f"eq-unit ok");
    assert_ne(E.A(mk(1)), E.B);
    println(f"ne-tag ok");

    let a: Option[String] = Some(mk(5));
    let b: Option[String] = Some(mk(5));
    let c: Option[String] = Some(mk(6));
    assert_eq(a, b);
    println(f"eq-generic ok");
    assert_ne(a, c);
    println(f"ne-generic ok");

    assert_eq(S { a: 1, s: mk(7) }, S { a: 1, s: mk(7) });
    println(f"eq-struct ok");
    assert_eq(mk(8), mk(8));
    println(f"eq-string ok");
    assert_eq(1, 1);
    println(f"eq-int ok");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "eq-payload ok\nne-payload ok\neq-tuple ok\nne-tuple ok\n\
         eq-unit ok\nne-tag ok\neq-generic ok\nne-generic ok\n\
         eq-struct ok\neq-string ok\neq-int ok\n"
    );
}

#[test]
fn enum_eq_handles_a_boxed_or_wide_generic_payload() {
    // B-2026-08-27-16 — `==` on a generic enum whose argument OVERFLOWS its
    // erased payload allotment PANICKED THE COMPILER, and a wide inline payload
    // was compared by its words.
    //
    // `compile_enum_eq` packed offsets sequentially from the RESOLVED field
    // widths. That is right only while every field still fits the allotment the
    // erased layout reserved; when it does not,
    // `coerce_to_payload_words` heap-BOXED the field into one word at
    // construction, so the sequential sum indexed past an area that never held
    // those words and `build_extract_value` returned an unwrapped `Err`.
    // `boxed` and `boxed2` are that shape — `enum Box1[T] { One(T) }` allots `T`
    // ONE word, so a `String` does not fit. Before the fix this fixture did not
    // produce a wrong answer; `karac build` aborted.
    //
    // `wide` is the second defect: a field over three words fell to a
    // best-effort word-wise compare, so `struct Big { a: i64, b: i64, s: String }`
    // (five words) compared the `String`'s heap POINTER and `x == y` answered
    // `false` for equal contents.
    //
    // THE `-tag` LINES ARE THE SOUNDNESS CASE, and they are why the payload walk
    // is now gated on tag equality. The switch dispatches on the LEFT tag, so
    // with mismatched tags it still entered that variant's block and read the
    // RIGHT operand's words as that variant's payload. That was harmless while
    // those reads were integer extracts feeding an `icmp` — the tag conjunction
    // discarded the answer — but the fix DEREFERENCES a field (a boxed payload
    // is `inttoptr` + load, a rebuilt one is loaded through by its comparator),
    // and an uninitialised word from a unit variant is a wild pointer.
    // `Box1.One(s) == Box1.Nil` segfaulted under the JIT. One line per read
    // mode, since each dereferences differently.
    //
    // `opt-scalar` is the control: a payload that fits its allotment and is
    // wholly inline was correct before and must stay so.
    //
    // Codegen twin: `test_e2e_enum_eq_handles_a_boxed_or_wide_generic_payload`. The interpreter was already right,
    // so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
enum Box1[T] { One(T), Nil }
#[derive(Hash, Eq, PartialEq)]
enum Pair2[T] { P(T, i64), Nil }
#[derive(Hash, Eq, PartialEq)]
struct Big { a: i64, b: i64, s: String }
#[derive(Hash, Eq, PartialEq)]
enum H { W(Big), N }

fn mk(n: i64) -> String { return f"v{n}"; }

fn main() {
    let a: Box1[String] = Box1.One(mk(1));
    let b: Box1[String] = Box1.One(mk(1));
    let c: Box1[String] = Box1.One(mk(2));
    println(f"boxed={a == b}");
    println(f"boxed-ne={a == c}");
    println(f"boxed-tag={a == Box1.Nil}");

    let d: Pair2[String] = Pair2.P(mk(3), 7);
    let e: Pair2[String] = Pair2.P(mk(3), 7);
    let g: Pair2[String] = Pair2.P(mk(4), 7);
    let h: Pair2[String] = Pair2.P(mk(3), 8);
    println(f"boxed2={d == e}");
    println(f"boxed2-ne-s={d == g}");
    println(f"boxed2-ne-n={d == h}");
    println(f"boxed2-tag={d == Pair2.Nil}");

    let i: H = H.W(Big { a: 1, b: 2, s: mk(5) });
    let j: H = H.W(Big { a: 1, b: 2, s: mk(5) });
    let k: H = H.W(Big { a: 1, b: 2, s: mk(6) });
    let l: H = H.W(Big { a: 1, b: 9, s: mk(5) });
    println(f"wide={i == j}");
    println(f"wide-ne-s={i == k}");
    println(f"wide-ne-b={i == l}");
    println(f"wide-tag={i == H.N}");

    let m: Option[String] = Some(mk(7));
    let n: Option[String] = Some(mk(7));
    println(f"opt={m == n}");
    let q: Option[String] = None;
    println(f"opt-tag={m == q}");
    let o: Option[i64] = Some(5);
    let p: Option[i64] = Some(5);
    println(f"opt-scalar={o == p}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "boxed=true\nboxed-ne=false\nboxed-tag=false\n\
         boxed2=true\nboxed2-ne-s=false\nboxed2-ne-n=false\n\
         boxed2-tag=false\nwide=true\nwide-ne-s=false\n\
         wide-ne-b=false\nwide-tag=false\nopt=true\n\
         opt-tag=false\nopt-scalar=true\n"
    );
}

#[test]
fn a_generic_or_sub_word_enum_key_is_matched_by_content() {
    // B-2026-08-27-12 — the two payload shapes B-2026-08-27-6's structural enum
    // key walk DECLINED, so they kept the byte compare it replaced and stayed
    // wrong: a GENERIC enum key, and a payload struct whose word image is not its
    // LLVM layout.
    //
    // BOTH ARE ONE CAUSE — reading a payload by pointer-casting into its words
    // only works where the words ARE the value. Three ways they are not, and the
    // three read modes that answer them:
    //
    //   * `subword` — `struct { a: i32, b: i32, s: String }` spends three word
    //     runs where LLVM packs the two `i32`s into eight bytes, so `s` sits at
    //     byte 8 by the word stream and byte 8 by LLVM only by luck; field `b`
    //     does not. Rebuilt from its words through the same helper a match arm
    //     binds a payload with. `subword-ne-b` is the line that catches a walk
    //     reading `b` out of `a`'s padding.
    //   * `opt` / `res` — the payload type is the enum's PARAMETER, resolved by
    //     substituting the instantiation, exactly as `compile_enum_eq` does.
    //   * `boxed` / `boxed2` — `enum Box1[T] { One(T) }` allots `T` ONE word from
    //     its erased layout, so a `String` argument does not fit and construction
    //     heap-BOXES it. The word holds a box pointer, not the value.
    //
    // `inst-a` and `inst-b` are the collision guard. `mangled_type_name` drops
    // generic arguments, so `Option[String]` and `Option[i64]` would share one
    // `karac_eq_Option`; once the comparator resolves payloads per instantiation,
    // whichever was emitted first would answer for both. Two live maps of
    // different instantiations in one program is what makes that observable.
    //
    // The `-ne` lines are not padding: a comparator that answered `true`
    // unconditionally passes every positive assertion above them. `dedup` and
    // `removed` check that hash moved with eq — equal keys that hash differently
    // never meet in the same bucket.
    //
    // Codegen twin: `test_e2e_a_generic_or_sub_word_enum_key_is_matched_by_content`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct TwoNarrow { a: i32, b: i32, s: String }
#[derive(Hash, Eq, PartialEq)]
enum Holder { W(TwoNarrow), N }
#[derive(Hash, Eq, PartialEq)]
enum Box1[T] { One(T), Nil }
#[derive(Hash, Eq, PartialEq)]
enum Pair2[T] { P(T, i64), Nil }

fn main() {
    let mut m: Map[Holder, i64] = Map.new();
    m.insert(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zz" }), 7);
    println(f"subword={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zz" }))}");
    println(f"subword-ne-s={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 2, s: f"zy" }))}");
    println(f"subword-ne-b={m.contains_key(Holder.W(TwoNarrow { a: 1, b: 3, s: f"zz" }))}");

    let mut o: Map[Option[String], i64] = Map.new();
    o.insert(Some(f"kk"), 1);
    o.insert(None, 2);
    println(f"opt={o.contains_key(Some(f"kk"))}");
    println(f"opt-ne={o.contains_key(Some(f"kj"))}");
    println(f"opt-none={o.contains_key(None)}");

    let mut r: Map[Result[String, i64], i64] = Map.new();
    r.insert(Ok(f"rr"), 1);
    r.insert(Err(9), 2);
    println(f"res-ok={r.contains_key(Ok(f"rr"))}");
    println(f"res-ok-ne={r.contains_key(Ok(f"rq"))}");
    println(f"res-err={r.contains_key(Err(9))}");

    let mut b: Map[Box1[String], i64] = Map.new();
    b.insert(Box1.One(f"bb"), 1);
    println(f"boxed={b.contains_key(Box1.One(f"bb"))}");
    println(f"boxed-ne={b.contains_key(Box1.One(f"bc"))}");

    let mut p: Map[Pair2[String], i64] = Map.new();
    p.insert(Pair2.P(f"pp", 3), 1);
    println(f"boxed2={p.contains_key(Pair2.P(f"pp", 3))}");
    println(f"boxed2-ne-s={p.contains_key(Pair2.P(f"pq", 3))}");
    println(f"boxed2-ne-n={p.contains_key(Pair2.P(f"pp", 4))}");

    let mut i: Map[Option[i64], i64] = Map.new();
    i.insert(Some(5), 1);
    println(f"scalar={i.contains_key(Some(5))}");
    println(f"scalar-ne={i.contains_key(Some(6))}");

    let mut two: Map[Option[String], i64] = Map.new();
    two.insert(Some(f"aa"), 1);
    let mut three: Map[Option[i64], i64] = Map.new();
    three.insert(Some(7), 1);
    println(f"inst-a={two.contains_key(Some(f"aa"))}");
    println(f"inst-b={three.contains_key(Some(7))}");

    let mut s: Set[Box1[String]] = Set.new();
    s.insert(Box1.One(f"dup"));
    s.insert(Box1.One(f"dup"));
    println(f"dedup={s.len()}");
    b.remove(Box1.One(f"bb"));
    println(f"removed={b.len()}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "subword=true\nsubword-ne-s=false\nsubword-ne-b=false\n\
         opt=true\nopt-ne=false\nopt-none=true\nres-ok=true\n\
         res-ok-ne=false\nres-err=true\nboxed=true\n\
         boxed-ne=false\nboxed2=true\nboxed2-ne-s=false\n\
         boxed2-ne-n=false\nscalar=true\nscalar-ne=false\n\
         inst-a=true\ninst-b=true\ndedup=1\nremoved=0\n"
    );
}

#[test]
fn an_enum_key_with_a_heap_payload_is_matched_by_content() {
    // B-2026-08-27-6 — a plain (non-shared) ENUM key with a heap-bearing
    // payload was compared by its payload WORDS instead of recursing, so the
    // compiled backends missed a structurally-equal key the interpreter found.
    // `emit_eq_fn_for_type_expr` had arms for tuples, `Vec` and STRUCTS but
    // none for enums, and the byte-compare fallback it fell to reads a
    // `String` payload's two distinct heap POINTERS.
    //
    // THE SCALAR-PAYLOAD LEG IS THE CONTROL, and the reason this sat unseen:
    // a byte compare IS correct when the whole value is inline, so the common
    // enum key always worked.
    //
    // `Scalar(5)` and `Alt(5)` are the tag leg — identical payload words under
    // different discriminants. They must not collide, which is what a walk
    // that compared payloads without first gating on the tag would do.
    //
    // EQ AND HASH HAD TO MOVE TOGETHER; `dedup` and `removed` are what check
    // it. Equal keys that hash differently never land in the same bucket, so
    // an equality-only fix leaves every lookup missing exactly as before.
    //
    // THE LAST TWO LINES PIN THE TWO PATHS TOGETHER. `==` on an enum VALUE
    // does not use this comparator at all — the operator site routes a
    // heap-payload enum to `compile_enum_eq`, which has walked tags and
    // rebuilt payloads structurally all along. That asymmetry IS this bug:
    // `x == y` and `m.contains_key(y)` disagreed on the same two values,
    // because only one of the two enum comparators in this compiler knew how
    // to look past the payload words. They must keep agreeing.
    //
    // The `-ne` lines are not padding: a comparator that answered `true`
    // unconditionally passes every positive assertion above them.
    //
    // `shared` closes the loop with B-2026-08-27-4, whose shared-enum leg was
    // defined as "behaves as its non-shared twin does" and so inherited this
    // defect. Fixing the plain enum without it would have broken that parity
    // in the other direction.
    //
    // Codegen twin: `test_e2e_an_enum_key_with_a_heap_payload_is_matched_by_content`.
    // The interpreter was already right, so this pins it as the oracle.
    let src = r#"
#[derive(Hash, Eq, PartialEq)]
struct Inner { id: i64, tag: String }
#[derive(Hash, Eq, PartialEq)]
enum Shape {
    Unit,
    Scalar(i64),
    Alt(i64),
    Text { s: String },
    Pair(i64, String),
    Items(Vec[i64]),
    Nested(Inner),
    Narrow(u8, i64),
}
#[derive(Hash, Eq, PartialEq)]
shared enum Node { Named { s: String }, Anon }
fn main() {
    let mut m: Map[Shape, i64] = Map.new();
    m.insert(Shape.Unit, 1);
    m.insert(Shape.Scalar(5), 2);
    m.insert(Shape.Alt(5), 3);
    m.insert(Shape.Text { s: f"hello" }, 4);
    m.insert(Shape.Pair(9, f"pp"), 5);
    m.insert(Shape.Items(vec![1, 2, 3]), 6);
    m.insert(Shape.Nested(Inner { id: 7, tag: f"nn" }), 7);
    m.insert(Shape.Narrow(200, 11), 8);
    println(f"len={m.len()}");
    println(f"unit={m.contains_key(Shape.Unit)}");
    println(f"text={m.contains_key(Shape.Text { s: f"hello" })}");
    println(f"text-ne={m.contains_key(Shape.Text { s: f"hellp" })}");
    println(f"pair={m.contains_key(Shape.Pair(9, f"pp"))}");
    println(f"pair-ne={m.contains_key(Shape.Pair(9, f"pq"))}");
    println(f"items={m.contains_key(Shape.Items(vec![1, 2, 3]))}");
    println(f"items-ne={m.contains_key(Shape.Items(vec![1, 2, 4]))}");
    println(f"nested={m.contains_key(Shape.Nested(Inner { id: 7, tag: f"nn" }))}");
    println(f"nested-ne={m.contains_key(Shape.Nested(Inner { id: 7, tag: f"nm" }))}");
    println(f"narrow={m.contains_key(Shape.Narrow(200, 11))}");
    println(f"narrow-ne={m.contains_key(Shape.Narrow(201, 11))}");
    match m.get(Shape.Scalar(5)) { Some(v) => { println(f"tag-a={v}") } None => { println(f"tag-a=miss") } }
    match m.get(Shape.Alt(5)) { Some(v) => { println(f"tag-b={v}") } None => { println(f"tag-b=miss") } }

    let mut s: Set[Shape] = Set.new();
    s.insert(Shape.Text { s: f"dup" });
    s.insert(Shape.Text { s: f"dup" });
    println(f"dedup={s.len()}");
    m.remove(Shape.Text { s: f"hello" });
    println(f"removed={m.len()}");

    let mut n: Map[Node, i64] = Map.new();
    n.insert(Node.Named { s: f"aa" }, 1);
    println(f"shared={n.contains_key(Node.Named { s: f"aa" })}");
    println(f"shared-ne={n.contains_key(Node.Named { s: f"ab" })}");

    let p = Shape.Text { s: f"eq" };
    let q = Shape.Text { s: f"eq" };
    let r = Shape.Text { s: f"ne" };
    println(f"eq={p == q}");
    println(f"eq-ne={p == r}");
}
"#;
    assert_eq!(
        run_no_errors(src),
        "len=8\nunit=true\ntext=true\ntext-ne=false\npair=true\n\
         pair-ne=false\nitems=true\nitems-ne=false\nnested=true\n\
         nested-ne=false\nnarrow=true\nnarrow-ne=false\ntag-a=2\n\
         tag-b=3\ndedup=1\nremoved=7\nshared=true\n\
         shared-ne=false\neq=true\neq-ne=false\n"
    );
}

#[test]
fn test_legitimate_variant_matches_run() {
    // Parity guard for B-2026-07-17-6: the scrutinee-mismatch gate must not
    // disturb legitimate Option / Result / user-enum (bare + dotted) /
    // tuple-variant matches, which must still run and produce correct output.
    let out = run_no_errors(
        "enum Color { Red, Green, Blue }\n\
         enum Shape { Circle(i64), Square(i64) }\n\
         fn col(c: Color) -> i64 { match c { Red => 1, Color.Green => 2, _ => 3 } }\n\
         fn shp(s: Shape) -> i64 { match s { Shape.Circle(r) => r, Square(w) => w } }\n\
         fn opt(o: Option[i64]) -> i64 { match o { Some(v) => v, None => -1 } }\n\
         fn main() {\n\
             println(col(Color.Red));\n\
             println(col(Color.Green));\n\
             println(col(Color.Blue));\n\
             println(shp(Shape.Circle(7)));\n\
             println(shp(Shape.Square(4)));\n\
             println(opt(Some(9)));\n\
             println(opt(None));\n\
         }",
    );
    assert_eq!(out, "1\n2\n3\n7\n4\n9\n-1\n");
}

#[test]
fn test_tuple_variant_binding_shadows_unit_variant_local() {
    // Regression: a tuple/struct-variant pattern binding whose name collides
    // with an in-scope local that holds a UNIT enum variant must still bind the
    // payload, not misfire as a unit-variant test. The matcher's
    // bare-name→unit-variant heuristic previously consulted `env.get(name)` and
    // a local `c = Color.Green` (a unit variant value) made `Info(c)`'s binding
    // `c` look like a unit-variant pattern, so the arm failed and surfaced as a
    // spurious runtime "non-exhaustive match". Case-class (lowercase = binding)
    // disambiguates. (Pre-existing bug surfaced by user `impl Display`, GAP-W4.)
    let out = run("enum Color { Red, Green, Blue }\n\
         enum Msg { Info(i64), Quit }\n\
         fn main() {\n\
             let c = Color.Green;\n\
             let m = Msg.Info(7);\n\
             match m { Info(c) => println(f\"got {c}\"), Quit => println(\"quit\") }\n\
             match c { Green => println(\"green\"), _ => println(\"other\") }\n\
         }");
    assert_eq!(out, "got 7\ngreen\n");
}

#[test]
fn test_qualified_enum_variant_construction() {
    // `Enum.Variant(args)` (qualified) construction must work in the
    // interpreter, peer to the unqualified `Variant(args)` form. The
    // resolver and codegen accept the qualified form (Json/Ordering even
    // require it); the interpreter used to `eval_expr_inner` the callee path
    // `Enum.Variant` and panic ("path '…' not found"). Covers a user enum
    // tuple variant and the baked-stdlib `Result` / `Option`.
    assert_eq!(
        run("enum Color { Red, Blue(i64) }\n\
             fn main() {\n\
                 match Color.Blue(7) { Red => println(0), Blue(n) => println(n) }\n\
             }"),
        "7\n"
    );
    assert_eq!(
        run("fn main() {\n\
                 match Result.Ok(5) { Ok(n) => println(n), Err(e) => println(0) }\n\
             }"),
        "5\n"
    );
    assert_eq!(
        run("fn main() {\n\
                 match Option.Some(9) { Some(n) => println(n), None => println(0) }\n\
             }"),
        "9\n"
    );
}

#[test]
fn test_qualified_enum_variant_constructor_cross_boundary() {
    // A method returning a qualified-constructed `Result` whose value is
    // matched in the caller — the original repro for the interpreter panic.
    // Also pins that an enum's *associated fn* (`E.make`, not a variant) is
    // still dispatched as a call, not mistaken for a variant constructor.
    assert_eq!(
        run("enum E { A, B(i64) }\n\
             impl E { fn make() -> E { E.B(3) } }\n\
             struct W {}\n\
             impl W { fn g(self) -> Result[i64, String] { Result.Ok(42) } }\n\
             fn main() {\n\
                 match (W{}).g() { Ok(n) => println(n), Err(e) => println(0) }\n\
                 match E.make() { A => println(0), B(n) => println(n) }\n\
             }"),
        "42\n3\n"
    );
}

#[test]
fn test_user_enum_method_shadowing_builtin_name() {
    // B-2026-07-18-49: a USER enum/struct defining a method whose name collides
    // with an Option/Result builtin (`unwrap`/`expect`/`unwrap_err`) must
    // dispatch to the user impl, not the builtin. The Option/Result arms fell
    // through to `other => other.clone()` for a non-Option/Result receiver,
    // silently returning the receiver's Display instead of the user body — a
    // build-vs-interp divergence (codegen called the user method). Genuine
    // Option/Result unwrap must still use the builtin.
    assert_eq!(
        run("enum E { A(String) }\n\
             impl E { fn unwrap(self) -> String { match self { E.A(s) => s } } }\n\
             fn main() { let e = E.A(\"hi\".to_string()); println(e.unwrap()); }"),
        "hi\n"
    );
    // A user struct with a colliding `unwrap` name.
    assert_eq!(
        run("struct R { v: String }\n\
             impl R { fn unwrap(self) -> String { self.v } }\n\
             fn main() { let r = R { v: \"q\".to_string() }; println(r.unwrap()); }"),
        "q\n"
    );
    // A user enum with a colliding `expect` name (extra arg).
    assert_eq!(
        run("enum E { A(i64) }\n\
             impl E { fn expect(self, m: String) -> i64 { match self { E.A(n) => n } } }\n\
             fn main() { let e = E.A(9); println(e.expect(\"x\".to_string())); }"),
        "9\n"
    );
    // Genuine Option/Result builtins are unaffected.
    assert_eq!(
        run("fn main() {\n\
                 let o: Option[i64] = Some(42);\n\
                 println(o.unwrap());\n\
                 let r: Result[i64, String] = Err(\"bad\".to_string());\n\
                 println(r.unwrap_err());\n\
             }"),
        "42\nbad\n"
    );
}

#[test]
fn test_errdefer_with_binding_sees_err_payload() {
    // errdefer(e) binds `e` to the Err payload during the errdefer phase.
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 errdefer(e) { print(e); }\n\
                 return Err(\"oops\");\n\
             }\n\
             fn main() { let _ = body(); }"),
        "oops"
    );
}

// ── B-2026-07-01-8 enum-Drop + fresh-temp-arg parity (interpreter) ──

#[test]
fn test_user_drop_fires_for_value_enum_binding() {
    // Pre-fix the interpreter NEVER ran a user `impl Drop` for a value
    // ENUM binding in any position (`drop_target` resolved only
    // Value::Struct) — `karac run` was silent where `karac build`
    // printed one drop per value. NLL endpoint: `s` is unused after the
    // let, so the body fires immediately after it.
    let (output, _drops) = run_program_with_drops(
        "enum Sig { A(i64), B }\n\
         impl Drop for Sig {\n\
             fn drop(mut ref self) {\n\
                 println(7);\n\
             }\n\
         }\n\
         fn main() {\n\
             let s = Sig.A(1);\n\
             println(0);\n\
         }",
    );
    assert_eq!(
        output,
        vec!["7\n".to_string(), "0\n".to_string()],
        "expected the enum binding's user drop body to fire (NLL: right after the unused let); got {:?}",
        output
    );
}

#[test]
fn test_user_drop_fires_for_fresh_temp_enum_arg() {
    // The enum twin: tuple-variant ctor (`consume(Sig.A(1))`) and unit
    // variant (`consume(Sig.B)`) temps each fire once after their call.
    let (output, _drops) = run_program_with_drops(
        "enum Sig { A(i64), B }\n\
         impl Drop for Sig {\n\
             fn drop(mut ref self) {\n\
                 println(1);\n\
             }\n\
         }\n\
         fn consume(s: Sig) {\n\
             println(0);\n\
         }\n\
         fn main() {\n\
             consume(Sig.A(7));\n\
             consume(Sig.B);\n\
             println(99);\n\
         }",
    );
    assert_eq!(
        output,
        vec![
            "0\n".to_string(),
            "1\n".to_string(),
            "0\n".to_string(),
            "1\n".to_string(),
            "99\n".to_string()
        ],
        "expected one drop per fresh enum temp arg; got {:?}",
        output
    );
}

#[test]
fn test_user_drop_enum_move_fires_once() {
    // Move suppression parity for enums: a let-rebind (`let b = a;`) and
    // a tail return each transfer the drop obligation — exactly ONE body
    // firing per value (the suppressors previously matched only
    // Value::Struct, which would have double-fired once enum bindings
    // started dropping).
    let (output, _drops) = run_program_with_drops(
        "enum Sig { A(i64), B }\n\
         impl Drop for Sig {\n\
             fn drop(mut ref self) {\n\
                 println(1);\n\
             }\n\
         }\n\
         fn make() -> Sig {\n\
             let s = Sig.A(1);\n\
             s\n\
         }\n\
         fn main() {\n\
             let a = make();\n\
             let b = a;\n\
             println(0);\n\
         }",
    );
    let drop_count = output.iter().filter(|l| l.as_str() == "1\n").count();
    assert_eq!(
        drop_count, 1,
        "expected exactly one drop firing across tail-return + rebind moves; got {:?}",
        output
    );
}

// ── B-2026-07-03-6 value_compare Struct / EnumVariant arms ──

#[test]
fn test_struct_and_enum_ordering_no_dataloss_and_sorts() {
    // Pre-fix, two `Value::Struct` (or two `Value::EnumVariant`) fell to
    // value_compare's discriminant fallback (always Equal), so
    // `SortedSet[Struct]` / `SortedMap[Struct, _]` COLLAPSED distinct keys to
    // one (silent data loss) and `Vec[Struct].sort()` was a NO-OP. Verify the
    // count is preserved (no collapse) and the sort orders by fields.
    let output = run("#[derive(Eq, Ord)]\n\
         struct P { a: i64, b: i64 }\n\
         #[derive(Eq, Ord)]\n\
         enum E { Lo, Mid, Hi }\n\
         fn main() {\n\
             let mut ss: SortedSet[P] = SortedSet.new();\n\
             ss.insert(P { a: 2, b: 1 });\n\
             ss.insert(P { a: 1, b: 9 });\n\
             ss.insert(P { a: 1, b: 2 });\n\
             println(f\"{ss.len()}\");\n\
             println(f\"{ss.contains(P { a: 1, b: 9 })}\");\n\
             println(f\"{ss.contains(P { a: 5, b: 5 })}\");\n\
             let mut sm: SortedMap[P, i64] = SortedMap.new();\n\
             let _ = sm.insert(P { a: 1, b: 1 }, 10);\n\
             let _ = sm.insert(P { a: 2, b: 2 }, 20);\n\
             let _ = sm.insert(P { a: 3, b: 3 }, 30);\n\
             println(f\"{sm.len()}\");\n\
             let mut es: SortedSet[E] = SortedSet.new();\n\
             es.insert(E.Hi);\n\
             es.insert(E.Lo);\n\
             es.insert(E.Mid);\n\
             println(f\"{es.len()}\");\n\
             let mut v: Vec[P] = Vec.new();\n\
             v.push(P { a: 2, b: 0 });\n\
             v.push(P { a: 1, b: 9 });\n\
             v.push(P { a: 1, b: 2 });\n\
             v.sort();\n\
             let mut i = 0;\n\
             while i < v.len() { let p = v[i]; println(f\"{p.a},{p.b}\"); i = i + 1; };\n\
         }");
    // SortedSet len 3 (no collapse), membership correct; SortedMap len 3;
    // enum SortedSet len 3; Vec sorted by (a, b): (1,2) (1,9) (2,0).
    assert_eq!(output, "3\ntrue\nfalse\n3\n3\n1,2\n1,9\n2,0\n");
}

// ── B-2026-07-03-12: derived-Ord DECLARATION order, not alphabetical ──

#[test]
fn test_struct_enum_ordering_is_declaration_order_not_alphabetical() {
    // B-2026-07-03-6 fixed the data loss but ordered struct fields / enum
    // variants ALPHABETICALLY. This locks the derived-`Ord` DECLARATION order
    // recovered from the per-thread `type_order` registry:
    //   - struct `Rect { width, height }` sorts by `width` FIRST — alphabetical
    //     would compare `height` first and flip the two rows.
    //   - enum `Priority { Low, Med, High }` sorts `Low < Med < High` — the
    //     "more visible" case: alphabetical would give `High < Low < Med`.
    // Both the `Vec.sort()` path (value_compare direct) and the SortedSet
    // path (OrdValue::cmp deep inside BTreeMap, no interpreter handle) must
    // agree, so the test exercises both.
    let output = run_no_errors(
        "#[derive(Eq, Ord)]\n\
         struct Rect { width: i64, height: i64 }\n\
         #[derive(Eq, Ord)]\n\
         enum Priority { Low, Med, High }\n\
         fn rank(p: Priority) -> i64 {\n\
             match p { Priority.Low => 0, Priority.Med => 1, Priority.High => 2 }\n\
         }\n\
         fn main() {\n\
             let mut v: Vec[Rect] = Vec.new();\n\
             v.push(Rect { width: 2, height: 1 });\n\
             v.push(Rect { width: 1, height: 9 });\n\
             v.sort();\n\
             let mut i = 0;\n\
             while i < v.len() { let r = v[i]; println(f\"{r.width},{r.height}\"); i = i + 1; };\n\
             let mut ss: SortedSet[Priority] = SortedSet.new();\n\
             ss.insert(Priority.High);\n\
             ss.insert(Priority.Low);\n\
             ss.insert(Priority.Med);\n\
             for p in ss { println(f\"{rank(p)}\"); };\n\
         }",
    );
    // Struct sorted by width (declaration order): (1,9) then (2,1).
    // Enum SortedSet iterates in declaration order: Low(0) Med(1) High(2).
    assert_eq!(output, "1,9\n2,1\n0\n1\n2\n");
}

#[test]
fn test_e2e_enum_state_machine() {
    assert_eq!(
        run("enum State { Idle, Running(i64), Done }\n\
             fn step(s: State) -> State {\n\
                 match s {\n\
                     Idle => Running(0),\n\
                     Running(n) => {\n\
                         if n >= 3 { Done } else { Running(n + 1) }\n\
                     },\n\
                     Done => Done,\n\
                 }\n\
             }\n\
             fn is_done(s: State) -> bool {\n\
                 match s {\n\
                     Done => true,\n\
                     _ => false,\n\
                 }\n\
             }\n\
             fn main() {\n\
                 let s1 = step(Idle);\n\
                 println(is_done(s1));\n\
                 let s2 = step(s1);\n\
                 let s3 = step(s2);\n\
                 let s4 = step(s3);\n\
                 let s5 = step(s4);\n\
                 println(is_done(s5));\n\
             }"),
        "false\ntrue\n"
    );
}

// ── Ordering / MemoryOrdering Enums ───────────────────────────

#[test]
fn test_ordering_variants() {
    // Comparison-Ordering variants (Less / Equal / Greater)
    let output = run("fn main() {\n\
             let lt = Ordering.Less;\n\
             let eq = Ordering.Equal;\n\
             let gt = Ordering.Greater;\n\
             println(lt);\n\
             println(eq);\n\
             println(gt);\n\
         }");
    assert!(output.contains("Less"));
    assert!(output.contains("Equal"));
    assert!(output.contains("Greater"));
}

#[test]
fn test_memory_ordering_variants() {
    let output = run("fn main() {\n\
             let r = MemoryOrdering.Relaxed;\n\
             let a = MemoryOrdering.Acquire;\n\
             let rel = MemoryOrdering.Release;\n\
             println(r);\n\
             println(a);\n\
             println(rel);\n\
         }");
    assert!(output.contains("Relaxed"));
    assert!(output.contains("Acquire"));
    assert!(output.contains("Release"));
}

/// B-2026-08-10-3 — `SeekFrom` is a PRELUDE enum, and prelude enums reach the
/// typechecker through `STDLIB_PROGRAMS` but never codegen's `declare_enums`.
/// This pins the interpreter side of the three-way agreement; the codegen twin
/// (`test_e2e_seek_from_prelude_enum_discriminates`) is the one that would have
/// caught the seed being missing.
#[test]
fn test_seek_from_variants_discriminate() {
    let src = "fn name(w: SeekFrom) -> String {
                   match w {
                       SeekFrom.Start => { return \"start\"; }
                       SeekFrom.Current => { return \"current\"; }
                       SeekFrom.End => { return \"end\"; }
                   }
               }
               fn main() {
                   println(name(SeekFrom.Start));
                   println(name(SeekFrom.Current));
                   println(name(SeekFrom.End));
               }";
    assert_eq!(run_no_errors(src), "start\ncurrent\nend\n");
}

#[test]
fn test_oncecell_get_or_init_struct_payload() {
    // The canonical lazy-init use case: a struct built on first access
    // and read back by field. `get_or_init` returns `ref T`; field access
    // auto-derefs, so `c.port` works without an explicit `*`. `T` is
    // inferred from the receiver `OnceCell[Cfg]` (the value-receiver
    // generic dispatch binds impl generics from the receiver's type args).
    let output = run(r#"struct Cfg { port: i64, name: String }
         fn main() {
             let cell: OnceCell[Cfg] = OnceCell.new();
             let c = cell.get_or_init(|| Cfg { port: 8080, name: "svc" });
             println(c.port);
             println(c.name);
             let c2 = cell.get_or_init(|| Cfg { port: 1, name: "other" });
             println(c2.port);
         }"#);
    assert_eq!(output, "8080\nsvc\n8080\n");
}

#[test]
fn test_a_discarded_enum_statement_runs_its_payloads_drop_body() {
    // B-2026-09-02-13 — `mk(1);` ran the enum's own `Drop` body but not its
    // payload's, where `let x = mk(1);` — the identical value one spelling
    // away — runs both. The bound local is the oracle: the two differ in
    // nothing but whether the value gets a name.
    //
    // The interpreter's half was to WIDEN gates, not to add walks. Each discard
    // site's payload-walk companion is gated to admit exactly the producers
    // codegen walked, and codegen's registrar reached its enum payload walker
    // only on the no-own-`Drop` leg — so a CALL returning an own-`Drop` enum was
    // outside every gate. Adding a second walk instead double-fires: measured
    // `dB dW7 dW7` twice while building this, once by widening the shared
    // `run_discarded_value_user_drops` and once by stacking a walk beside the
    // wildcard-`let` site's existing companion.
    //
    // On the DEFAULT leg because half the fix is the interpreter's, and this
    // carries the NESTED case (`Bn -> Mid -> Leaf`) that the cross-backend
    // matrix `e2e_a_discarded_enum_statement_runs_its_payloads_drop_body`
    // cannot express under its single prelude.
    const PRELUDE: &str = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
         enum Bx { Full(W), Empty(W) }\n\
         impl Drop for Bx { fn drop(mut ref self) { println(\"dB\"); } }\n\
         fn mk(n: i64) -> Bx {\n\
         if n < 1 { return Bx.Full(W { id: n }); }\n\
         return Bx.Empty(W { id: 7 });\n\
         }\n";
    let wrap =
        |body: &str| format!("{PRELUDE}fn main() {{\n    {body}\n    println(\"mid\");\n}}\n");

    // THE ORACLE, then the row.
    assert_eq!(run(&wrap("let x = mk(1);")), "dB\ndW7\nmid\n");
    assert_eq!(run(&wrap("mk(1);")), "dB\ndW7\nmid\n");
    assert_eq!(run(&wrap("let _ = mk(1);")), "dB\ndW7\nmid\n");

    // THE ROW'S THIRD NOT-MEASURED ITEM: the payload's own nested Drop-bearing
    // field is reached once the walker is registered, and in declaration order.
    let nested = "struct Leaf { id: i64 }\n\
         impl Drop for Leaf { fn drop(mut ref self) { println(f\"dL{self.id}\"); } }\n\
         struct Mid { leaf: Leaf }\n\
         impl Drop for Mid { fn drop(mut ref self) { println(\"dM\"); } }\n\
         enum Bn { Full(Mid), Empty(Mid) }\n\
         impl Drop for Bn { fn drop(mut ref self) { println(\"dBn\"); } }\n\
         fn mkn(n: i64) -> Bn {\n\
         if n < 1 { return Bn.Full(Mid { leaf: Leaf { id: n } }); }\n\
         return Bn.Empty(Mid { leaf: Leaf { id: 7 } });\n\
         }\n";
    assert_eq!(
        run(&format!(
            "{nested}fn main() {{\n    let x = mkn(1);\n    println(\"mid\");\n}}\n"
        )),
        "dBn\ndM\ndL7\nmid\n"
    );
    assert_eq!(
        run(&format!(
            "{nested}fn main() {{\n    mkn(1);\n    println(\"mid\");\n}}\n"
        )),
        "dBn\ndM\ndL7\nmid\n"
    );

    // THE TWO DOUBLE-FIRE HAZARDS the row records, each measured at `dB dW7 dW7`
    // while building this. The first is the wildcard-destructure leaf, which
    // calls the shared walker AND adds its own payload walk (B-2026-08-28-40);
    // the second is the wildcard-`let` site's own `inline_ctor`-gated companion
    // (B-2026-08-28-39). Neither may gain a second walk from this fix.
    assert_eq!(run(&wrap("let (_, _) = (mk(1), 5);")), "dB\ndW7\nmid\n");
    assert_eq!(
        run(&wrap("let _ = Bx.Empty(W { id: 7 });")),
        "dB\ndW7\nmid\n"
    );

    // CONTROLS, unchanged by the fix and both correct before it: an enum with
    // no own `Drop` walks its payload through the field-bodies leg, and an
    // own-`Drop` enum whose payload has none owes only the own body.
    let noown = "struct W { id: i64 }\n\
         impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.id}\"); } }\n\
         enum NoOwn { Full(W), Empty(W) }\n\
         fn mk2(n: i64) -> NoOwn {\n\
         if n < 1 { return NoOwn.Full(W { id: n }); }\n\
         return NoOwn.Empty(W { id: 7 });\n\
         }\n";
    assert_eq!(
        run(&format!(
            "{noown}fn main() {{\n    mk2(1);\n    println(\"mid\");\n}}\n"
        )),
        "dW7\nmid\n"
    );
    let ownonly = "enum OwnOnly { A(i64), B(i64) }\n\
         impl Drop for OwnOnly { fn drop(mut ref self) { println(\"dO\"); } }\n\
         fn mk3(n: i64) -> OwnOnly {\n\
         if n < 1 { return OwnOnly.A(n); }\n\
         return OwnOnly.B(7);\n\
         }\n";
    assert_eq!(
        run(&format!(
            "{ownonly}fn main() {{\n    mk3(1);\n    println(\"mid\");\n}}\n"
        )),
        "dO\nmid\n"
    );
}

#[test]
fn test_freshtemp_enum_scrutinee_runs_user_drop() {
    // B-2026-07-11-26: a fresh-temp enum scrutinee whose type has a user
    // `impl Drop` must RUN that Drop under the interpreter too (run/build
    // parity) — pre-fix it was silently skipped in if-let/while-let/let-else/
    // match. The interpreter runs the body at the statement/arm boundary
    // (codegen defers to enclosing-scope exit — the deferred slice-3 timing
    // difference); both run it exactly once.
    //
    // B-2026-09-01-42 — the FOURTH `DROP` is the temporary that ends the
    // `while let`. `next` is called four times (i = 0, 1, 2 yield; i = 3
    // stops), so four temporaries exist and four bodies are due; this
    // assertion carried three, pinning the loop-exit residual as expected
    // output on the interpreter exactly as its codegen twin did on the
    // compiled side. Both were updated in the same commit, since the two
    // backends have to agree here.
    let out = run(r#"enum Step { Yield(i64), Stop }
        impl Drop for Step { fn drop(mut ref self) { println("DROP"); } }
        fn next(i: i64) -> Step { if i < 3 { Step.Yield(i) } else { Step.Stop } }
        fn main() {
            if let Step.Yield(v) = next(0) { println(f"Y {v}"); } else { println("MISS"); }
            println("AFTER");
            let mut i: i64 = 0;
            while let Step.Yield(v) = next(i) { println(f"I {v}"); i = i + 1; }
            println("DONE");
        }"#);
    assert_eq!(
        out.trim(),
        "Y 0\nDROP\nAFTER\nI 0\nDROP\nI 1\nDROP\nI 2\nDROP\nDROP\nDONE"
    );
}

#[test]
fn test_derive_default_enum_marked_variant() {
    // `#[derive(Default)]` constructs the `#[default]`-marked variant —
    // not necessarily the first declared one. Here the marker is on a
    // later variant to prove declaration order is irrelevant.
    let output = run(r#"
#[derive(Default)]
enum Mode { Running(i64), #[default] Idle, Custom { level: i64 } }

fn main() {
    let m = Mode.default();
    match m {
        Mode.Idle => { println("idle"); }
        Mode.Running(n) => { println(n); }
        Mode.Custom { level } => { println(level); }
    }
}
"#);
    assert_eq!(output, "idle\n");
}

#[test]
fn test_derive_default_enum_marked_unit_variant() {
    // The marked variant must be field-less; here `Empty` is the
    // default even though a payload-carrying variant precedes it.
    let output = run(r#"
#[derive(Default)]
enum Wrapped { Pair(i64, bool), #[default] Empty }

fn main() {
    let w = Wrapped.default();
    match w {
        Wrapped.Pair(n, b) => { println(n); println(b); }
        Wrapped.Empty => { println("empty"); }
    }
}
"#);
    assert_eq!(output, "empty\n");
}

#[test]
fn test_derive_default_enum_non_exhaustive_compose() {
    // `#[non_exhaustive]` and `#[derive(Default)]` are orthogonal — the
    // variant set may grow without changing the marked default.
    let output = run(r#"
#[non_exhaustive]
#[derive(Default)]
pub enum M { #[default] A, B }

fn main() {
    let m = M.default();
    match m {
        M.A => { println("a"); }
        M.B => { println("b"); }
    }
}
"#);
    assert_eq!(output, "a\n");
}

// ── Contracts — struct invariants at pub method exits ──────────────
//
// design.md § Contracts rule 3: a type with an `invariant` block re-checks
// it at the exit of every pub method (private methods do not check). v1
// covers pub instance methods.

#[test]
fn test_contract_invariant_holds_runs() {
    // `inc` keeps `self.n >= 0`, so the pub-method-exit check passes.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn inc(mut ref self) { self.n = self.n + 1; } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.inc(); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied invariant must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_invariant_violation_faults() {
    // `dec` drives `self.n` to -1, violating `self.n >= 0` at method exit.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn dec(mut ref self) { self.n = self.n - 1; } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.dec(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` invariant fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_invariant_private_method_not_checked() {
    // A private method may transiently break the invariant; only the
    // outer pub method's exit checks it. Here the pub method restores
    // nothing but never lets a violated state reach a pub exit, so no
    // fault fires (the private `break_priv` result is discarded).
    let errors = runtime_errors(
        "struct DateRange { start: i64, end: i64, invariant self.start <= self.end }\n\
         impl DateRange {\n\
             fn break_priv(self) -> DateRange { DateRange { start: 10, end: 0 } }\n\
             pub fn via(self) -> i64 { let _b = self.break_priv(); 99 }\n\
         }\n\
         fn main() { let r = DateRange { start: 1, end: 5 }; let _ = r.via(); }",
    );
    assert!(
        errors.is_empty(),
        "a private method must not trigger the invariant check, got: {errors:?}"
    );
}

// ── Contracts — constructor invariants (pub assoc fn returning Self) ──
//
// design.md § Contracts: "Constructors (pub associated functions that return
// `Self`) also check the invariant at their return point." The return value
// is bound as `self` and the type's invariants are re-checked — the
// construction boundary, alongside the pub-method-exit checks above.

#[test]
fn test_contract_constructor_invariant_holds() {
    // A pub constructor that produces a valid instance must not fault.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn make() -> Self { Counter { n: 7 } } }\n\
         fn main() { let _c = Counter.make(); }",
    );
    assert!(
        errors.is_empty(),
        "a valid constructor must not fault, got: {errors:?}"
    );
}

#[test]
fn test_contract_constructor_invariant_violation_faults() {
    // The constructor builds `n = -5`, violating `self.n >= 0` at its return
    // point — the construction boundary aborts even though no method ran.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter { pub fn bad() -> Self { Counter { n: 0 - 5 } } }\n\
         fn main() { let _c = Counter.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected a `contract violated` invariant fault at construction, got: {errors:?}"
    );
}

#[test]
fn test_contract_constructor_impl_invariant_faults() {
    // `impl invariant` fires at the constructor return point too (it fires at
    // every method exit, and a constructor is a return boundary). The explicit
    // `-> Counter` return-type form (not `Self`) is also recognized.
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter { pub fn bad() -> Counter { Counter { n: 0 - 1 } } }\n\
         fn main() { let _c = Counter.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "an `impl invariant` must fire at the constructor return, got: {errors:?}"
    );
}

#[test]
fn test_contract_shared_constructor_invariant_faults() {
    // Constructor invariants fire on shared (RC) structs too — construction
    // doesn't involve the shared-mutation path, so this is a clean check.
    let errors = runtime_errors(
        "shared struct Scell { n: i64, invariant self.n >= 0 }\n\
         impl Scell { pub fn bad() -> Self { Scell { n: 0 - 1 } } }\n\
         fn main() { let _c = Scell.bad(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "a shared-struct constructor must check its invariant, got: {errors:?}"
    );
}

#[test]
fn test_contract_shared_method_invariant_holds() {
    // Shared-struct pub-method invariants are already plumbed (the check is
    // receiver-kind-agnostic): a non-mutating pub method with a held invariant
    // runs clean. (A *violating* shared method needs field mutation through
    // `mut ref self`, which has an orthogonal constraint tracked separately;
    // this pins that the invariant dispatch itself fires without spurious
    // faults for a shared receiver.)
    let errors = runtime_errors(
        "shared struct Scell { n: i64, invariant self.n >= 0 }\n\
         impl Scell { pub fn get(ref self) -> i64 { self.n } }\n\
         fn main() { let c = Scell { n: 5 }; let _ = c.get(); }",
    );
    assert!(
        errors.is_empty(),
        "a held shared-method invariant must not fault, got: {errors:?}"
    );
}

// ── Contracts — impl invariant (step 5b, all-method scope) ─────────
//
// design.md § Contracts — `impl invariant` fires at every method exit
// (pub and private); plain `invariant` only at pub method exits.

#[test]
fn test_impl_invariant_fires_at_private_method_exit() {
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter {\n\
             fn dec_priv(mut ref self) { self.n = self.n - 1; }\n\
             pub fn run(mut ref self) -> i64 { self.dec_priv(); 0 }\n\
         }\n\
         fn main() { let mut c = Counter { n: 0 }; let _ = c.run(); }",
    );
    assert!(
        errors
            .iter()
            .any(|e| e.message.contains("contract violated")),
        "expected `impl invariant` to fault at the private method exit, got: {errors:?}"
    );
}

#[test]
fn test_plain_invariant_not_checked_at_private_method_exit() {
    // The private helper transiently breaks the plain invariant; the pub
    // method restores it before its own exit, so no fault fires.
    let errors = runtime_errors(
        "struct Counter { n: i64, invariant self.n >= 0 }\n\
         impl Counter {\n\
             fn dec_priv(mut ref self) { self.n = self.n - 1; }\n\
             pub fn run(mut ref self) -> i64 { self.dec_priv(); self.n = self.n + 1; 0 }\n\
         }\n\
         fn main() { let mut c = Counter { n: 0 }; let _ = c.run(); }",
    );
    assert!(
        errors.is_empty(),
        "plain invariant must not fire at a private method exit, got: {errors:?}"
    );
}

#[test]
fn test_impl_invariant_holds_runs() {
    let errors = runtime_errors(
        "struct Counter { n: i64, impl invariant self.n >= 0 }\n\
         impl Counter { fn inc_priv(mut ref self) { self.n = self.n + 1; } pub fn run(mut ref self) { self.inc_priv(); } }\n\
         fn main() { let mut c = Counter { n: 0 }; c.run(); }",
    );
    assert!(
        errors.is_empty(),
        "a satisfied impl invariant must not fault, got: {errors:?}"
    );
}

// ── Enum / struct structural equality (`==` / `!=`) ───────────────
// Regression coverage for the interpreter gap where `==` on any enum
// variant or struct hit `eval_ops`'s `unreachable!` (every enum, incl.
// Option/Result/Ordering, panicked on `==`). `Value`'s `PartialEq`
// already compared these structurally; the fix wires `eval_binary` to it.

#[test]
fn enum_equality_unit_and_payload_variants() {
    let output = run(r#"
#[derive(Eq)]
enum Color { Red, Green, Blue }

#[derive(Eq)]
enum Tagged { N(i64), Z }

fn main() {
    println(f"{Color.Red == Color.Red}");
    println(f"{Color.Red == Color.Blue}");
    println(f"{Color.Red != Color.Blue}");
    println(f"{Tagged.N(7) == Tagged.N(7)}");
    println(f"{Tagged.N(7) == Tagged.N(9)}");
    println(f"{Tagged.N(7) == Tagged.Z}");
}
"#);
    assert_eq!(output, "true\nfalse\ntrue\ntrue\nfalse\nfalse\n");
}

#[test]
fn size_of_enum_tagged_word_layout() {
    // Shape = {i64 tag, 3 × i64 payload} = 32 bytes (Label(String) is
    // the widest variant), 8-aligned.
    let out = run_no_errors(
        "enum Shape { Dot, Line(i64, i64), Label(String) }\n\
         fn main() { println(size_of[Shape]()); println(align_of[Shape]()); }",
    );
    assert_eq!(out, "32\n8\n");
}

// B-2026-07-05-2 run==build parity: moving a for-loop `Vec[<user enum>]` element
// whole into a new owner (`let x = a`) is value-semantics-clean in the interp (no
// aliasing double-free). The codegen fix (81ad98c4) makes the build surface match;
// these pin the RUN surface to the same output (the ASAN cases in
// `tests/memory_sanitizer.rs` cover the double-free, but neither the interp nor the
// non-ASAN codegen surface was locked). Covers a `VecOrString` payload and a
// `NestedStruct` payload.
#[test]
fn forloop_enum_element_whole_move_runs() {
    let src = "enum Tok { Empty, Word(String) }\n\
               fn build() -> Vec[Tok] {\n\
                   let mut v: Vec[Tok] = Vec.new();\n\
                   let mut i = 0;\n\
                   while i < 6 { v.push(Tok.Word(\"forloop_enum_element_whole_move_payload_theta_xx\".to_string())); i = i + 1; }\n\
                   v\n\
               }\n\
               fn main() {\n\
                   let items = build();\n\
                   let mut n: i64 = 0;\n\
                   for a in items {\n\
                       let x = a;\n\
                       match x { Tok.Word(s) => { n = n + s.len(); } Tok.Empty => {} }\n\
                   }\n\
                   println(n);\n\
               }";
    assert_eq!(run(src), "288\n");
}

#[test]
fn forloop_enum_element_nested_struct_payload_runs() {
    let src = "struct Inner { s: String }\n\
               enum Node { Leaf, Wrap(Inner) }\n\
               fn build() -> Vec[Node] {\n\
                   let mut v: Vec[Node] = Vec.new();\n\
                   let mut i = 0;\n\
                   while i < 5 { v.push(Node.Wrap(Inner { s: \"forloop_enum_nested_struct_payload_iota_field_yy\".to_string() })); i = i + 1; }\n\
                   v\n\
               }\n\
               fn main() {\n\
                   let items = build();\n\
                   let mut n: i64 = 0;\n\
                   for a in items {\n\
                       let x = a;\n\
                       match x { Node.Wrap(inner) => { n = n + inner.s.len(); } Node.Leaf => {} }\n\
                   }\n\
                   println(n);\n\
               }";
    assert_eq!(run(src), "240\n");
}

/// B-2026-07-31-45 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_let_else_moved_payload_single_drop`, same source and expected
/// string. The pre-fix interpreter printed the body TWICE (drop 52 before
/// `bound 52` via the un-disarmed source walk, and again after).
#[test]
fn test_let_else_moved_payload_single_drop() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn check(w: Box2) {\n\
                 let Full(r2) = w else {\n\
                     println(\"nope\")\n\
                     return\n\
                 }\n\
                 println(f\"bound {r2.id}\")\n\
             }\n\
             fn main() {\n\
                 let w = Box2.Full(Res { id: 52 });\n\
                 println(\"a\");\n\
                 let Full(r2) = w else {\n\
                     println(\"nope\")\n\
                     return\n\
                 }\n\
                 println(f\"bound {r2.id}\");\n\
                 println(\"b\");\n\
                 check(Box2.Full(Res { id: 53 }));\n\
                 println(\"c\");\n\
                 check(Box2.Empty);\n\
                 println(\"end\");\n\
             }\n"),
        "a\nbound 52\ndrop 52\nb\nbound 53\ndrop 53\nc\nnope\nend\n"
    );
}

/// B-2026-07-30-11 (enum-assign displacement) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_enum_assign_displacement_runs_payload_body`,
/// same source and expected string. Pre-fix, overwriting an enum binding
/// (`b = Box2.Empty;` over `Full(Res{5})`) fired NO payload body in either
/// backend — the displaced-value hook matched only struct old values. Four
/// shapes: full→empty (the silence), full→full (old body at the
/// assignment, new at scope exit), empty→full (no old body), and a
/// moved-out payload then reassign (the arm binding owns the body; the
/// reassign must not replay it).
#[test]
fn test_enum_assign_displacement_runs_payload_body() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
                 println(\"a: full -> empty\");\n\
                 let mut b = Box2.Full(Res { id: 5 });\n\
                 b = Box2.Empty;\n\
                 println(\"b: full -> full\");\n\
                 let mut c = Box2.Full(Res { id: 6 });\n\
                 c = Box2.Full(Res { id: 7 });\n\
                 println(\"c: empty -> full (no old body)\");\n\
                 let mut d = Box2.Empty;\n\
                 d = Box2.Full(Res { id: 8 });\n\
                 println(\"d: moved-out then reassign (no double)\");\n\
                 let mut e = Box2.Full(Res { id: 9 });\n\
                 match e {\n\
                     Box2.Full(r) => { println(f\"took {r.id}\"); }\n\
                     Box2.Empty => {}\n\
                 }\n\
                 e = Box2.Empty;\n\
                 println(\"end\");\n\
             }\n"),
        "a: full -> empty\ndrop 5\nb: full -> full\ndrop 6\ndrop 7\n\
         c: empty -> full (no old body)\ndrop 8\n\
         d: moved-out then reassign (no double)\ntook 9\ndrop 9\nend\n"
    );
}

#[test]
fn test_enum_return_discard_runs_payload_body() {
    // B-2026-09-10-2 — the `e` quadrant's ORACLE CHANGED. The note above
    // recorded that an erased-generic payload is one "codegen structurally
    // cannot see", so both backends were made silent there and this string
    // omitted `drop 7 g7` / `drop 8 g8`. Codegen can see it now: the
    // instantiation-keyed walker resolves `MyBox[Res]` through the enum's own
    // substitution, so a discarded generic return runs its payload body on all
    // three backends and no longer strands 32 B per call. Same move
    // B-2026-08-02-14 made one container over for a generic STRUCT FIELD —
    // parity holds in the FIRING direction now, not the silent-leak one.
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             enum MyBox[T] { Wrap(T), Nil }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
                 fn drop(mut ref self) {\n\
                     println(\"loud drop\")\n\
                 }\n\
             }\n\
             struct Fac { tag: i64 }\n\
             impl Fac {\n\
                 fn make(ref self, n: i64) -> Box2 {\n\
                     return Box2.Full(Res { id: n, name: f\"m{n}\" });\n\
                 }\n\
             }\n\
             fn mk_enum(n: i64) -> Box2 {\n\
                 return Box2.Full(Res { id: n, name: f\"h{n}\" });\n\
             }\n\
             fn mk_gen(n: i64) -> MyBox[Res] {\n\
                 return MyBox.Wrap(Res { id: n, name: f\"g{n}\" });\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
                 return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn mk_empty() -> Box2 {\n\
                 return Box2.Empty;\n\
             }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 let _ = mk_enum(1);\n\
                 println(\"b\");\n\
                 mk_enum(2);\n\
                 println(\"c\");\n\
                 let f = Fac { tag: 0 };\n\
                 let _ = f.make(3);\n\
                 f.make(4);\n\
                 println(\"d\");\n\
                 let _ = mk_loud(5);\n\
                 mk_loud(6);\n\
                 println(\"e\");\n\
                 let _ = mk_gen(7);\n\
                 mk_gen(8);\n\
                 println(\"f\");\n\
                 let _ = mk_empty();\n\
                 mk_empty();\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 1 h1\nb\ndrop 2 h2\nc\ndrop 3 m3\ndrop 4 m4\nd\nloud drop\ndrop 5 l5\nloud drop\ndrop 6 l6\ne\ndrop 7 g7\ndrop 8 g8\nf\nend\n"
    );
}

/// B-2026-07-30-11 (own-Drop enum reassign leg) — interpreter twin of
/// `tests/codegen.rs`'s `e2e_own_drop_enum_reassign_sequencing`, same source
/// and expected string. Pre-fix the interpreter's Assign displacement arm
/// excluded own-`impl Drop` enums entirely ("sequencing unsettled"), so the
/// overwritten value's own body and payload body were silently lost (m1/m5).
#[test]
fn test_own_drop_enum_reassign_sequencing() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
                 fn drop(mut ref self) {\n\
                     println(\"loud drop\")\n\
                 }\n\
             }\n\
             fn mk_loud(n: i64) -> Loud {\n\
                 return Loud.Hold(Res { id: n, name: f\"l{n}\" });\n\
             }\n\
             fn use_res(r: Res) {\n\
                 println(f\"took {r.id}\");\n\
             }\n\
             fn pass(b: Loud) -> Loud {\n\
                 return b;\n\
             }\n\
             fn main() {\n\
                 println(\"m1: plain reassign\");\n\
                 let mut a = mk_loud(1);\n\
                 a = mk_loud(2);\n\
                 println(\"m1 end\");\n\
                 println(\"m2: reassign after whole-value move\");\n\
                 let mut b = mk_loud(3);\n\
                 let c = b;\n\
                 b = mk_loud(4);\n\
                 println(\"m2 end\");\n\
                 println(\"m3: reassign after payload move-out\");\n\
                 let mut d = mk_loud(5);\n\
                 match d {\n\
                     Loud.Hold(r) => { use_res(r); }\n\
                     Loud.Quiet => {}\n\
                 }\n\
                 d = mk_loud(6);\n\
                 println(\"m3 end\");\n\
                 println(\"m4: self-mention\");\n\
                 let mut e = mk_loud(7);\n\
                 e = pass(e);\n\
                 println(\"m4 end\");\n\
                 println(\"m5: quiet-to-full and full-to-quiet\");\n\
                 let mut g = mk_loud(8);\n\
                 g = Loud.Quiet;\n\
                 g = mk_loud(9);\n\
                 println(\"end\");\n\
             }\n"),
        "m1: plain reassign\nloud drop\ndrop 1 l1\nloud drop\ndrop 2 l2\nm1 end\n\
         m2: reassign after whole-value move\nloud drop\ndrop 3 l3\nm2 end\n\
         m3: reassign after payload move-out\ntook 5\ndrop 5 l5\nloud drop\ndrop 6 l6\nm3 end\n\
         m4: self-mention\nloud drop\ndrop 7 l7\nm4 end\n\
         m5: quiet-to-full and full-to-quiet\nloud drop\ndrop 8 l8\nloud drop\nloud drop\ndrop 9 l9\nend\n"
    );
}

/// B-2026-08-29-37 (interpreter twin / ORACLE) — the interpreter never copies a
/// match scrutinee, so every `Drop` body here fires exactly as often as the
/// source program says. That is what makes it the oracle for
/// `tests/codegen.rs`'s
/// `e2e_defensive_scrutinee_copy_does_not_rerun_the_enum_own_drop_body`, which
/// is pinned to this same string.
///
/// The compiled backends stage the scrutinee into a defensive copy for four
/// distinct shapes and used to run the enum's own body on that copy as well as
/// on the source, so `refchain` / `loop` / `index` each carried an extra `dE`.
/// Recording the expectation on BOTH sides means a future change that alters
/// the interpreter's count has to argue with a test rather than silently
/// re-baseline the codegen twin.
///
/// B-2026-09-02-11 migrated the `index` leg to a READ-ONLY arm, so this program
/// is now byte-identical to the codegen twin's (it had already migrated for
/// B-2026-08-31-3, which rejects a consume out of a `v[i]` scrutinee — and
/// `let m = r` is a consume). The leg's expectation drops one `dR3` with it:
/// nothing is moved out of the container, so the element is destroyed exactly
/// once, at `v`'s NLL death. The compiled backends printed the extra one until
/// B-2026-09-02-11 — the clone's payload binding ran a body the container was
/// already going to run.
#[test]
fn test_scrutinee_clone_does_not_rerun_the_enum_own_drop_body() {
    assert_eq!(
        run(r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct S { e: E }

fn mk(n: i64) -> E {
    let mut v = Vec.new();
    v.push(n);
    return E.A(R { id: n, v: v })
}

fn via_ref(s: ref S) -> i64 {
    match s.e { E.A(r) => { let m = r; return m.id + m.v.len() } E.B => { return 0 } }
}

fn leg_refchain() {
    println("refchain");
    let s = S { e: mk(1) };
    println(f"got {via_ref(s)}");
    println("refchain end");
}

fn leg_loop_elem() {
    println("loop");
    let mut v = Vec.new();
    v.push(mk(2));
    for p in v {
        match p { E.A(r) => { let m = r; println(f"got {m.id + m.v.len()}"); } E.B => { } }
    }
    println("loop end");
}

fn leg_vec_index() {
    println("index");
    let mut v = Vec.new();
    v.push(mk(3));
    match v[0] { E.A(r) => { println(f"got {r.id + r.v.len()}"); } E.B => { } }
    println("index end");
}

fn leg_fresh_temp() {
    println("fresh");
    match mk(4) { E.A(r) => { let m = r; println(f"got {m.id + m.v.len()}"); } E.B => { } }
    println("fresh end");
}

fn main() {
    leg_refchain();
    leg_loop_elem();
    leg_vec_index();
    leg_fresh_temp();
    println("end");
}
"#),
        r#"refchain
dR1
got 2
dE
dR1
refchain end
loop
got 3
dR2
dE
dR2
loop end
index
got 4
dE
dR3
index end
fresh
got 5
dR4
dE
fresh end
end
"#
    );
}

/// B-2026-09-02-17 — THE `let … else` SPELLING OVER AN INDEXED ELEMENT RUNS
/// EXACTLY ONE PAYLOAD `Drop` BODY, AND IT IS THE CONTAINER'S.
///
/// The last member of the `v[i]` family to get a fixture, and the only one that
/// diverged. `v[i]` evaluates to `ref T`, so a binding taken out of one is a
/// view of a defensive clone (B-2026-09-02-11) and the container runs the body
/// at its own NLL death. `match` / `if let` / `while let` all reach that answer
/// through `scrutinee_expr_is_consuming`, which has no `Index` arm and so
/// answers false. `let … else` never asked: it binds through `bind_pattern`
/// directly and `push_drops_for_stmt` registered a real slot per name, so the
/// body ran TWICE — once via the container's still-armed walk, once via the
/// binding's own slot.
///
/// WHICH COUNT IS RIGHT WAS THE ROW'S OPEN QUESTION, and `liveafter` is the leg
/// that answers it. The row's second reading was that `let … else` binds into
/// the ENCLOSING scope, so `r` outlives the container's death and "a value the
/// program observes arguably earns its own body". `liveafter` reads `v` AFTER
/// `r`, so the container outlives the binding and no such escape exists — and
/// the interpreter doubled there too, pre-fix. The extra body was never the
/// escape's consequence, which also retires the row's third reading (reject the
/// form, per B-2026-08-31-3): there is nothing to reject in a program whose
/// borrow never dangles.
///
/// `temp` IS THE CONTROL THAT SHAPED THE GATE. Over a TEMPORARY container
/// (`mkv(8)[0]`) nothing else owns the element, every surface already ran one
/// body, and that body is the BINDING's — `got 9` then `dR8`. Suppressing there
/// would lose it entirely rather than merely mistime it, so the gate reuses
/// `place_walk_is_retractable`, the same "is there an owner to hand back to"
/// walk the disarm family uses: an identifier root or a one-hop field chain,
/// and nothing else.
///
/// THE DEFECT IS THE BINDING SLOT, NOT THE ENUM PAYLOAD. `struct` — a bare
/// `let W { r, k } = v[0] else { … }` — carries it with no enum anywhere, and
/// `opt` carries it on the seeded pair. `field` (`h.xs[0]`) and `two` (`v[1]`
/// of two elements, where the container walks BOTH) were not named in the row.
///
/// Pre-fix, measured per leg and on `--interp` ONLY: `bare` `dE dR1 got 2 dR1`,
/// `liveafter` `got 3 dR2 len 1 dE dR2`, `field` `dE dR3 got 4 dR3`, `two`
/// `dE dR4 dE dR5 got 6 dR5`, `opt` `dR6 got 7 dR6`, `struct` `dR7 got 14 dR7`
/// — one extra body each. `temp`, `iflet` and `elsetaken` were identical before
/// and after, and all three compiled surfaces were correct throughout.
///
/// Pinned to the same program and the same string as `tests/codegen.rs`'s
/// `e2e_let_else_over_an_indexed_element_runs_one_payload_body`.
#[test]
fn test_let_else_over_an_indexed_element_runs_one_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, v: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct H { xs: Vec[E] }
struct W { r: R, k: i64 }

fn mkr(n: i64) -> R { let mut v: Vec[i64] = Vec.new(); v.push(n); return R { id: n, v: v } }
fn mk(n: i64) -> E { return E.A(mkr(n)) }
fn mkw(n: i64) -> W { return W { r: mkr(n), k: n } }
fn mkv(n: i64) -> Vec[E] { let mut v: Vec[E] = Vec.new(); v.push(mk(n)); return v }

fn leg_bare() {
    println("bare");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(1));
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("bare end");
}

fn leg_live_after() {
    println("liveafter");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(2));
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println(f"len {v.len()}");
    println("liveafter end");
}

fn leg_field() {
    println("field");
    let mut xs: Vec[E] = Vec.new();
    xs.push(mk(3));
    let h = H { xs: xs };
    let E.A(r) = h.xs[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("field end");
}

fn leg_two() {
    println("two");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(4));
    v.push(mk(5));
    let E.A(r) = v[1] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("two end");
}

fn leg_opt() {
    println("opt");
    let mut v: Vec[Option[R]] = Vec.new();
    v.push(Option.Some(mkr(6)));
    let Option.Some(r) = v[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("opt end");
}

fn leg_struct() {
    println("struct");
    let mut v: Vec[W] = Vec.new();
    v.push(mkw(7));
    let W { r, k } = v[0] else { println("no"); return }
    println(f"got {r.id + k}");
    println("struct end");
}

fn leg_temp() {
    println("temp");
    let E.A(r) = mkv(8)[0] else { println("no"); return }
    println(f"got {r.id + r.v.len()}");
    println("temp end");
}

fn leg_iflet() {
    println("iflet");
    let mut v: Vec[E] = Vec.new();
    v.push(mk(9));
    if let E.A(r) = v[0] { println(f"got {r.id + r.v.len()}") }
    println("iflet end");
}

fn leg_elsetaken() {
    println("elsetaken");
    let mut v: Vec[E] = Vec.new();
    v.push(E.B);
    let E.A(r) = v[0] else { println("no"); return }
    println(f"got {r.id}");
}

fn main() {
    leg_bare();
    leg_live_after();
    leg_field();
    leg_two();
    leg_opt();
    leg_struct();
    leg_temp();
    leg_iflet();
    leg_elsetaken();
    println("end");
}
"#),
        r#"bare
dE
dR1
got 2
bare end
liveafter
got 3
len 1
dE
dR2
liveafter end
field
dE
dR3
got 4
field end
two
dE
dR4
dE
dR5
got 6
two end
opt
dR6
got 7
opt end
struct
dR7
got 14
struct end
temp
got 9
dR8
temp end
iflet
got 10
dE
dR9
iflet end
elsetaken
no
dE
end
"#
    );
}

/// B-2026-08-31-1 — A PAYLOAD BOUND OUT OF A *PROJECTION* OF AN OWNED PARAM IS
/// A VIEW, AND THE VIEW-NESS MUST PROPAGATE THROUGH A REBIND.
///
/// Under caller-retains (B-2026-08-01-13), a payload destructured from an owned
/// by-value param belongs to the CALLER's fire, so the arm binding registers no
/// Drop slot of its own. B-2026-08-29-17 made that view-ness propagate through
/// `let m = r`, but keyed the propagation on the scrutinee being a bare
/// `Identifier` — so `match s.e { E.A(r) => { let m = r; ... } }` inside
/// `fn take(s: S)` left `r` out of the view set, `m` took a slot, and this
/// backend ran the body the caller was already running.
///
/// Codegen never had the hole: its twin predicate walks field and tuple-index
/// hops to the root (B-2026-08-03-3 leg B), so the two sides now ask the same
/// question. Pre-fix this program printed FIVE extra `dR` lines against both
/// compiled backends — one each for `enum`, `opt`, `res`, `two` and `sv`.
///
/// THE FIVE CONTROLS ARE THE POINT, because the fix withholds a body and the
/// failure mode of over-reaching is a body that never runs at all:
/// - `tup` — a plain TUPLE pattern over an owned param's tuple field. NO LONGER A
///   CONTROL. It ran two bodies on every surface when this test was written, and
///   this bullet recorded that -31-1 deliberately left it alone: codegen
///   view-marked only VARIANT payload bindings, so withholding on the interpreter
///   alone would have turned an agreed answer into a new divergence.
///   B-2026-08-31-7 repaired the codegen side instead —
///   `collect_bare_tuple_binding_names` marks a bare-tuple element bound out of an
///   owned-param scrutinee as a param VIEW too, without routing it to the arm
///   channel a variant payload takes — so both columns moved to ONE body together
///   and the expectation below now carries `dR6` once.
/// - `ref` — a `ref` param, not owned, so no caller-retains: the arm keeps its slot.
/// - `local` — an owned LOCAL projection, where the arm binding really is the owner.
/// - `fresh` — a fresh-temp scrutinee, likewise.
/// - `norebind` — the same owned-param projection WITHOUT the rebind, which was
///   already correct and must stay so.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_owned_param_projection_payload_rebind_is_a_view`, pinned to this string.
#[test]
fn test_owned_param_projection_payload_rebind_is_a_view() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
enum Sv { Hold { inner: R }, Nil }
impl Drop for Sv { fn drop(mut ref self) { println("dSv") } }

struct S { e: E }
struct Ob { o: Option[R] }
struct Rb { r: Result[R, i64] }
struct Tb { t: (R, i64) }
struct W2 { s: S }
struct Hs { v: Sv }

fn mk(n: i64) -> E { return E.A(R { id: n }) }

fn f_enum(s: S)  { match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_opt(s: Ob)  { match s.o { Option.Some(r) => { let m = r; println(f"  b{m.id}"); } Option.None => { } } }
fn f_res(s: Rb)  { match s.r { Result.Ok(r) => { let m = r; println(f"  b{m.id}"); } Result.Err(e) => { } } }
fn f_two(w: W2)  { match w.s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_sv(h: Hs)   { match h.v { Sv.Hold { inner } => { let m = inner; println(f"  b{m.id}"); } Sv.Nil => { } } }
fn f_tup(s: Tb)  { match s.t { (r, k) => { let m = r; println(f"  b{m.id}"); } } }
fn f_ref(s: ref S) { match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_norebind(s: S) { match s.e { E.A(r) => { println(f"  b{r.id}"); } E.B => { } } }
fn f_local() { let s = S { e: mk(8) }; match s.e { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }
fn f_fresh() { match mk(9) { E.A(r) => { let m = r; println(f"  b{m.id}"); } E.B => { } } }

fn main() {
    println("enum");     f_enum(S { e: mk(1) });                              println("enum end");
    println("opt");      f_opt(Ob { o: Option.Some(R { id: 2 }) });           println("opt end");
    println("res");      f_res(Rb { r: Result.Ok(R { id: 3 }) });             println("res end");
    println("two");      f_two(W2 { s: S { e: mk(4) } });                     println("two end");
    println("sv");       f_sv(Hs { v: Sv.Hold { inner: R { id: 5 } } });      println("sv end");
    println("tup");      f_tup(Tb { t: (R { id: 6 }, 0) });                   println("tup end");
    println("ref");      let a = S { e: mk(7) }; f_ref(a);                    println("ref end");
    println("local");    f_local();                                           println("local end");
    println("fresh");    f_fresh();                                           println("fresh end");
    println("norebind"); f_norebind(S { e: mk(10) });                         println("norebind end");
    println("done");
}
"#),
        r#"enum
  b1
dE
dR1
enum end
opt
  b2
dR2
opt end
res
  b3
dR3
res end
two
  b4
dE
dR4
two end
sv
  b5
dSv
dR5
sv end
tup
  b6
dR6
tup end
ref
  b7
dR7
dE
dR7
ref end
local
  b8
dR8
dE
local end
fresh
  b9
dR9
dE
fresh end
norebind
  b10
dE
dR10
norebind end
done
"#
    );
}

/// B-2026-08-01-10 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_bare_user_enum_ctor_discard`, same source and expected string. The
/// interpreter's bare Path-ctor discard leg (added with the
/// B-2026-07-30-11 optres bare-statement work) already fired this shape —
/// the pin is the parity target the bare arm's new codegen ctor channel
/// now meets.
#[test]
fn test_bare_user_enum_ctor_discard() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 Box2.Full(Res { id: 24, name: f\"h{24}\" });\n\
                 println(\"b\");\n\
                 Box2.Empty;\n\
                 println(\"end\");\n\
             }\n"),
        "a\ndrop 24 h24\nb\nend\n"
    );
}

/// B-2026-08-29-10, interpreter leg — a METHOD whose owned enum param has its
/// payload bound out and NOT returned runs that payload's `Drop` body once.
///
/// The arm-stash gate `scrutinee_expr_is_consuming` declines an owned-param
/// scrutinee because the payload's observability "belongs to the CALLER
/// (caller-retains)". That hand-off is real for a FREE FUNCTION and vacuous for
/// a METHOD, whose arguments reach no caller-side fire — the same asymmetry
/// `owned_param_frame_is_method` already records for the `let`-destructure
/// gates. So a method frame's owned-param scrutinee IS consuming.
///
/// Pre-fix the user-enum case printed `v=7` alone here against `drop 7` / `v=7`
/// on all three compiled backends — a run-vs-build divergence — while both
/// free-function oracles printed one body on all four surfaces, which is what
/// makes one the right answer.
///
/// SCOPE: the `Option` spelling is NOT fixed by this. Its body is missed on the
/// CODEGEN side too (the caller arms the payload-bodies walk only for a tracked
/// VALUE enum, and `Option`/`Result` carry their own machinery), so before this
/// change it was an agreed silence on both backends and after it the
/// interpreter is correct while codegen still misses. That is the remaining
/// half of the row and is deliberately not pinned here to either answer.
#[test]
fn test_method_owned_enum_param_payload_body_runs_once() {
    const DROPPER: &str = "struct Res { id: i64, name: String }\n\
         impl Drop for Res { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n";
    for (label, body, want) in [
        // The shape this fixes: a METHOD taking an owned user enum, binding the
        // payload out and returning something else.
        (
            "method-user-enum-param",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> i64 \
             { match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let v: i64 = t.take(b); println(f\"v={v}\") }\n",
            "drop 7\nv=7\n",
        ),
        // CONTROLS — the free-function twins, correct before and after on all
        // four surfaces. They are the oracles the method case is measured
        // against, so a change that "fixed" methods by moving free functions
        // would fail here.
        (
            "free-fn-oracle-user-enum",
            "enum Box2 { Full(Res), Empty }\n\
             fn takef(b: Box2) -> i64 \
             { match b { Box2.Full(r) => { return r.id; } Box2.Empty => { return 0; } } }\n\
             fn main() { let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let v: i64 = takef(b); println(f\"v={v}\") }\n",
            "drop 7\nv=7\n",
        ),
        (
            "free-fn-oracle-option",
            "fn takef(b: Option[Res]) -> i64 \
             { match b { Option.Some(r) => { return r.id; } Option.None => { return 0; } } }\n\
             fn main() { let b: Option[Res] = Option.Some(Res { id: 7, name: f\"e7\" }); \
             let v: i64 = takef(b); println(f\"v={v}\") }\n",
            "drop 7\nv=7\n",
        ),
        // BOUNDARY — the payload IS returned, so the caller's binding owns it
        // and the arm stash must stay silent. This is B-2026-08-29-9's shape:
        // making the scrutinee consuming must not resurrect that double body.
        (
            "payload-returned-stays-single",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(b); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-9 — a METHOD that RETURNS a payload bound out of its owned
/// enum / `Option` param runs that payload's `Drop` body ONCE.
///
/// Exactly one value is constructed and one reaches the caller's binding, so
/// one body is correct; the compiled backends always had it. 277621a made the
/// interpreter run it twice — once at the method's own exit and once for the
/// caller's binding — by isolating the callee frame's moved-out sets.
///
/// The mechanism is worth stating, because it is not obvious and the fix is a
/// REVERT rather than an addition. Those sets are keyed by BINDING NAME with no
/// frame qualifier. Nothing inside the callee marks an escaping payload
/// moved-out; the suppression comes entirely from the CALLER's mark on the
/// argument leaking into the callee under the shared name. Isolating the frame
/// removed that leak and with it the only suppression this shape had.
///
/// RESOLVED by B-2026-08-29-11, and the account above is worth keeping because
/// its conclusion was wrong in an instructive way. The leak was called
/// load-bearing on the strength of this fixture, and this fixture spells the
/// caller's binding `b` — the same as the param. Rename it and the double body
/// was already there, with the leak fully intact. So the leak was not holding
/// the shape up, it was holding up one SPELLING of it.
///
/// What actually owns the suppression now is a rule the method path never had:
/// a method frame owns its arguments (`owned_param_frame_is_method`), so the
/// CALLER stands down on every by-value arg it hands over, whatever either side
/// calls it. With that in place the frame isolation removes a genuine leak, and
/// both cases below hold at one body — as does the renamed spelling, in
/// `test_method_frame_marks_do_not_leak_across_frames`.
///
/// Isolated by disabling each half of 277621a's interpreter change
/// independently: with the param registration off the double body REMAINED,
/// with the isolation off it went away and the registration still on. Both
/// legs below therefore also serve as controls that the registration is not
/// what regressed.
#[test]
fn test_method_returned_enum_param_payload_body_runs_once() {
    for (label, decls, want) in [
        // A USER value enum.
        (
            "user-enum-param",
            "enum Box2 { Full(Res), Empty }\n\
             struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Box2) -> Res \
             { match b { Box2.Full(r) => { return r; } \
             Box2.Empty => { return Res { id: 0, name: f\"z\" }; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Box2 = Box2.Full(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(b); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
        // `Option` behaves identically, which is what shows this is the METHOD
        // spelling of the shape rather than anything about the built-ins.
        (
            "option-param",
            "struct T { n: i64 }\n\
             impl T { fn take(ref self, b: Option[Res]) -> Res \
             { match b { Option.Some(r) => { return r; } \
             Option.None => { return Res { id: 0, name: f\"z\" }; } } } }\n\
             fn main() { let t = T { n: 1 }; \
             let b: Option[Res] = Option.Some(Res { id: 7, name: f\"e7\" }); \
             let r: Res = t.take(b); println(f\"got {r.id}\") }\n",
            "got 7\ndrop 7 e7\n",
        ),
    ] {
        const DROPPER: &str = "struct Res { id: i64, name: String }\n\
             impl Drop for Res { fn drop(mut ref self) { println(f\"drop {self.id} {self.name}\") } }\n";
        assert_eq!(run(&format!("{DROPPER}{decls}")), want, "{label}");
    }
}

/// B-2026-08-29-4, interpreter leg — a METHOD that hands an inline
/// `Option`/`Result` argument back yields the payload exactly once.
///
/// The twin of `e2e_method_passthrough_arg_aliases_the_source_payload` in
/// `tests/codegen.rs`, carrying the same cases with the same expected output.
/// Keep the two in step verbatim.
///
/// This backend was CORRECT before the fix, so every case here is a control
/// rather than a reproduction: the defect was a compiled-only double free (one
/// allocation, two frees, process abort), and what this pins is that the
/// codegen fix moved codegen ONTO the interpreter's answer rather than moving
/// both. A regression that "fixed" the double free by changing what the
/// program prints would fail here instead.
#[test]
fn test_method_passthrough_arg_aliases_the_source_payload() {
    const DECLS: &str = "struct Bx { n: i64 }\n\
         impl Bx {\n\
         fn take(ref self, o: Option[String]) -> Option[String] { o }\n\
         fn second(ref self, a: Option[String], b: Option[String]) -> Option[String] { b }\n\
         fn first(ref self, a: Option[String], b: Option[String]) -> Option[String] { a }\n\
         fn fresh(ref self, o: Option[String]) -> Option[String] { Option.Some(f\"fresh{self.n}\") }\n\
         }\n";
    for (label, body, want) in [
        (
            "single-arg-passthrough",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"a{b.n}\"); \
             let o = b.take(s); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got a1\n",
        ),
        (
            "two-args-second-returned",
            "fn main() { let b = Bx { n: 1 }; let p = Option.Some(f\"d{b.n}\"); \
             let q = Option.Some(f\"e{b.n}\"); let o = b.second(p, q); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got e1\n",
        ),
        (
            "two-args-first-returned",
            "fn main() { let b = Bx { n: 1 }; let p = Option.Some(f\"f{b.n}\"); \
             let q = Option.Some(f\"g{b.n}\"); let o = b.first(p, q); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got f1\n",
        ),
        (
            "no-passthrough-keeps-fresh-owner",
            "fn main() { let b = Bx { n: 1 }; let s = Option.Some(f\"h{b.n}\"); \
             let o = b.fresh(s); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got fresh1\n",
        ),
        (
            "free-fn-oracle-passthrough",
            "fn takef(o: Option[String]) -> Option[String] { o }\n\
             fn main() { let n = 1; let s = Option.Some(f\"a{n}\"); let o = takef(s); \
             match o { Option.Some(v) => { println(f\"got {v}\") } \
             Option.None => { println(\"none\") } } }\n",
            "got a1\n",
        ),
    ] {
        assert_eq!(run(&format!("{DECLS}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-17, interpreter leg — a `match` / `if let` arm that REBINDS a
/// payload bound out of an OWNED-PARAM scrutinee runs that payload's `Drop`
/// body exactly once.
///
/// The interpreter twin of `test_e2e_rebound_owned_param_payload_drops_once`
/// and `test_e2e_rebound_non_param_payload_still_fires_once` in
/// `tests/codegen.rs`, carrying the same cases with the same expected output.
/// Keep the two in step verbatim.
///
/// BOTH FILES NEED THE FULL MATRIX rather than half each, and for a sharper
/// reason than usual here: all three backends AGREED on the doubled body, so
/// parity could not see the defect and fixing one side alone would have
/// manufactured a divergence. That happened mid-fix — the codegen half landed
/// first and turned `dR1 dR1` into an interp-vs-compiled split until the
/// interpreter half followed. These absolute expectations are what would catch
/// a future one-sided change.
#[test]
fn test_rebound_owned_param_payload_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Box2 { Full(R), Empty }\n";
    for (label, body, want) in [
        // The defect: owned-param scrutinee, rebind, payload does NOT escape.
        // Pre-fix `dR1 / dR1 / v=1` — one body too many, on every backend.
        (
            "rebind-nonescaping",
            "fn take(o: Box2) -> i64 \
             { match o { Box2.Full(r) => { let m = r; m.id } Box2.Empty => { 0 } } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let v = take(o); \
             println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // `if let` — a separate binding path in this backend, and it needed its
        // own leg; the `match` spelling was fixed first and this one still
        // doubled.
        (
            "rebind-iflet",
            "fn take(o: Box2) -> i64 \
             { if let Box2.Full(r) = o { let m = r; m.id } else { 0 } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let v = take(o); \
             println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // TRANSITIVE — two levels of rebind.
        (
            "rebind-two-levels",
            "fn take(o: Box2) -> i64 \
             { match o { Box2.Full(r) => { let m = r; let n = m; n.id } Box2.Empty => { 0 } } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let v = take(o); \
             println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // GUARD RAIL — a LOCAL scrutinee has no caller behind it, so the arm
        // binding really is the only owner and the rebind must keep the body.
        (
            "guard-local-scrutinee",
            "fn main() { let o: Box2 = Box2.Full(R { id: 1 }); \
             let v = match o { Box2.Full(r) => { let m = r; m.id } Box2.Empty => { 0 } }; \
             println(f\"v={v}\"); println(\"end\") }\n",
            "dR1\nv=1\nend\n",
        ),
        // GUARD RAIL — a FRESH-TEMP scrutinee, owned outright by the match.
        (
            "guard-fresh-temp",
            "fn mk() -> Box2 { Box2.Full(R { id: 1 }) }\n\
             fn main() { let v = match mk() \
             { Box2.Full(r) => { let m = r; m.id } Box2.Empty => { 0 } }; \
             println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // GUARD RAIL — a second owned param that is never matched keeps its own
        // caller-side fire. Two values are constructed, so two bodies are right,
        // and the propagation has to be per-binding rather than per-frame.
        (
            "guard-unmatched-second-param",
            "fn take(o: Box2, p: R) -> i64 \
             { let s = match o { Box2.Full(r) => { let m = r; m.id } Box2.Empty => { 0 } }; \
             s + p.id }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); \
             let v = take(o, R { id: 2 }); println(f\"v={v}\") }\n",
            "dR2\ndR1\nv=3\n",
        ),
        // GUARD RAIL — the rebind ESCAPES as the return value, so the caller's
        // binding owns it and fires once, there.
        (
            "guard-rebind-escapes",
            "fn take(o: Box2) -> R \
             { match o { Box2.Full(r) => { let m = r; m } Box2.Empty => { R { id: 0 } } } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let k = take(o); \
             println(f\"k={k.id}\") }\n",
            "k=1\ndR1\n",
        ),
    ] {
        assert_eq!(run(&format!("{DROPPER}{body}")), want, "{label}");
    }
}

/// B-2026-08-29-19 — WRAPPING a param view into a fresh enum ran the payload's
/// `Drop` body TWICE in this backend too, so the shape agreed with both
/// compiled backends on the wrong answer and no A/B gate could see it.
///
/// The rule is B-2026-08-01-15's, one level of syntax up: a value moved out of
/// an owned param is a VIEW whose body the caller runs, and that survives a
/// CONSTRUCTOR exactly as it survives `let m = r;`. The interpreter half is
/// `let_ctor_payloads_are_param_views`, gating the binding's Drop slot the same
/// way codegen gates its `__karac_dropelems_enum_<E>` walk.
///
/// The last three cases are guard rails against over-suppression, which here
/// means a MISSING body — the same severity as the double being fixed, so the
/// fix is deliberately conditional rather than a blanket retraction. The mixed
/// case is pinned at the defect's three bodies for that reason (B-2026-08-29-24);
/// so is `Some(r)`, whose payload rides the `optres_*` leg on the compiled side
/// where no walker exists to withhold — fixing only this backend there turned
/// an agreed defect into a run-vs-build divergence, measured.
#[test]
fn test_wrapped_owned_param_payload_drop_body_runs_once() {
    const DROPPER: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Box2 { Full(R), Empty }\n\
         enum W { One(R), None2 }\n";
    for (label, body, want) in [
        // The row's shape: rebind, re-wrap, re-match.
        (
            "wrap-rebind-rematch",
            "fn take(o: Box2) -> i64 { match o { Box2.Full(r) => { let m = r; \
             let w = W.One(m); match w { W.One(z) => { z.id } W.None2 => { 0 } } } \
             Box2.Empty => { 0 } } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let v = take(o); \
             println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // Neither the rebind nor the second match is load-bearing.
        (
            "wrap-only",
            "fn take(o: Box2) -> i64 { match o { Box2.Full(r) => { let w = W.One(r); 7 } \
             Box2.Empty => { 0 } } }\n\
             fn main() { let o: Box2 = Box2.Full(R { id: 1 }); let v = take(o); \
             println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
        // The minimal shape — no enum payload, no `match` anywhere.
        (
            "wrap-plain-param",
            "fn take(r: R) -> i64 { let w = W.One(r); 7 }\n\
             fn main() { let v = take(R { id: 1 }); println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
        // View-ness must propagate THROUGH the wrap.
        (
            "wrap-then-rebind",
            "fn take(r: R) -> i64 { let w = W.One(r); let w2 = w; 7 }\n\
             fn main() { let v = take(R { id: 1 }); println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
        // A METHOD frame registers its own slots (B-2026-08-27-48: a method's
        // arguments reach no caller-side fire), and one body is still correct.
        (
            "wrap-in-method",
            "struct T { tag: i64 }\n\
             impl T { fn take(ref self, r: R) -> i64 { let w = W.One(r); 7 } }\n\
             fn main() { let t = T { tag: 0 }; let v = t.take(R { id: 1 }); \
             println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
        // GUARD RAIL — a genuine LOCAL wrapped: the callee owns it and must fire.
        (
            "guard-local-wrapped",
            "fn take() -> i64 { let m = R { id: 1 }; let w = W.One(m); \
             match w { W.One(z) => { z.id } W.None2 => { 0 } } }\n\
             fn main() { let v = take(); println(f\"v={v}\") }\n",
            "dR1\nv=1\n",
        ),
        // GUARD RAIL — a payload constructed FRESH inside the wrap.
        (
            "guard-fresh-payload",
            "fn take(k: i64) -> i64 { let w = W.One(R { id: k }); 7 }\n\
             fn main() { let v = take(1); println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
        // MIXED payloads shared one walk, so this row could only be armed or
        // withheld whole and stayed at three bodies. B-2026-08-29-24 masks the
        // view SLOT out of the walk, so the fresh payload keeps its body and the
        // view's goes back to the caller: `dR2` from the walk, `dR1` from the
        // caller's fire. Was `dR1 dR2 dR1`.
        (
            "mixed-payloads",
            "enum W2 { Two(R, R), None3 }\n\
             fn take(r: R) -> i64 { let w = W2.Two(r, R { id: 2 }); 7 }\n\
             fn main() { let v = take(R { id: 1 }); println(f\"v={v}\") }\n",
            "dR2\ndR1\nv=7\n",
        ),
        // `Some(r)` was excluded HERE to hold parity with codegen, which had no
        // `optres_*` withholding of its own; B-2026-08-29-24 landed that leg and
        // deleted the exclusion in the same commit. Was `dR1 dR1`.
        (
            "option-wrap",
            "fn take(r: R) -> i64 { let q = Some(r); 7 }\n\
             fn main() { let v = take(R { id: 1 }); println(f\"v={v}\") }\n",
            "dR1\nv=7\n",
        ),
    ] {
        let src = format!("{DROPPER}{body}");
        assert_eq!(run(&src), want, "case {label}");
    }
}

/// B-2026-08-29-24, interpreter leg — the wrap kinds and MIXED wraps that
/// B-2026-08-29-19 could not reach.
///
/// That fix covered the variant constructor only; a struct literal, a tuple
/// literal and `Some(..)` doubled a param view's `Drop` body identically, and a
/// MIXED wrap of any kind doubled it while the fresh payload beside it still
/// needed its own body. This backend masks each case through the same per-slot
/// sets a MOVE-OUT already used — `moved_out_struct_field_bodies`,
/// `moved_out_tuple_elem_bodies`, and the new `moved_out_enum_payload_slots` —
/// because the destination is the same: a body this binding must stop running
/// because something else runs it.
///
/// Every case here has a codegen twin in
/// `e2e_wrapped_param_view_nonenum_and_mixed_wraps_drop_once` with the same
/// expected output. That is the whole point: while wrong, all of these AGREED
/// across the three backends, so no A/B parity gate could see any of it and
/// only an absolute expectation catches a regression.
#[test]
fn test_wrapped_param_view_nonenum_and_mixed_wraps_drop_once() {
    const HDR: &str = "struct R { id: i64 }\n\
                       impl Drop for R {\n\
                       \x20   fn drop(mut ref self) { println(f\"dR{self.id}\") }\n\
                       }\n\
                       enum W2 { Two(R, R), None3 }\n\
                       struct S { r: R }\n\
                       struct S2 { r: R, k: i64 }\n\
                       struct S3 { a: R, b: R }\n\
                       struct Sd { r: R }\n\
                       impl Drop for Sd {\n\
                       \x20   fn drop(mut ref self) { println(\"dSd\") }\n\
                       }\n";
    for (label, body, want) in [
        // The wrap kinds B-2026-08-29-19 left doubling.
        ("struct-literal", "let s = S { r: r };", "dR1\nv=7\n"),
        (
            "struct-literal-inert-field",
            "let s = S2 { r: r, k: 9 };",
            "dR1\nv=7\n",
        ),
        ("tuple-literal", "let t = (r, 5);", "dR1\nv=7\n"),
        // The `Option` exclusion this backend carried to hold parity is gone,
        // because codegen's `optres_*` leg landed in the same commit.
        ("option-wrap", "let q = Some(r);", "dR1\nv=7\n"),
        (
            "owndrop-struct-all-views",
            "let s = Sd { r: r };",
            "dSd\ndR1\nv=7\n",
        ),
        // MIXED: the fresh payload keeps its body, the view's goes back to the
        // caller. Suppressing wholesale here is the failure in the OTHER
        // direction, and is what B-2026-08-29-19 declined to risk.
        (
            "mixed-enum",
            "let w = W2.Two(r, R { id: 2 });",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "mixed-struct",
            "let s = S3 { a: r, b: R { id: 2 } };",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "mixed-tuple",
            "let t = (r, R { id: 2 });",
            "dR2\ndR1\nv=7\n",
        ),
        (
            "struct-then-rebind",
            "let s = S { r: r }; let s2 = s;",
            "dR1\nv=7\n",
        ),
    ] {
        let src = format!(
            "{HDR}fn take(r: R) -> i64 {{ {body} 7 }}\n\
             fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\") }}\n"
        );
        assert_eq!(run(&src), want, "case {label}");
    }
    // GUARD RAILS — a FRESH payload in each wrap kind. No caller owns it, so
    // the callee's walk is the only fire; an over-broad mask silences these.
    for (label, body, want) in [
        (
            "guard-fresh-struct",
            "let s = S { r: R { id: 3 } };",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-tuple",
            "let t = (R { id: 3 }, 5);",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-option",
            "let q = Some(R { id: 3 });",
            "dR3\nv=7\n",
        ),
        (
            "guard-fresh-owndrop",
            "let s = Sd { r: R { id: 3 } };",
            "dSd\ndR3\nv=7\n",
        ),
    ] {
        let src = format!(
            "{HDR}fn take(k: i64) -> i64 {{ {body} 7 }}\n\
             fn main() {{ let v = take(3); println(f\"v={{v}}\") }}\n"
        );
        assert_eq!(run(&src), want, "case {label}");
    }
    // GUARD RAILS — a genuine LOCAL wrapped: the rule keys on an owned-param
    // source, which is exactly when a caller fires instead.
    for (label, body, want) in [
        ("guard-local-struct", "let s = S { r: m };", "dR4\nv=7\n"),
        ("guard-local-tuple", "let t = (m, 5);", "dR4\nv=7\n"),
        ("guard-local-option", "let q = Some(m);", "dR4\nv=7\n"),
    ] {
        let src = format!(
            "{HDR}fn take() -> i64 {{ let m = R {{ id: 4 }}; {body} 7 }}\n\
             fn main() {{ let v = take(); println(f\"v={{v}}\") }}\n"
        );
        assert_eq!(run(&src), want, "case {label}");
    }
    // PINNED AT THE DEFECT, each agreed with both compiled backends.
    for (label, src, want) in [
        // FIXED (B-2026-08-29-43). This backend always COULD mask a mixed
        // literal per field; what it lacked was a codegen counterpart, so it
        // declined on purpose rather than open a run-vs-build divergence.
        // Codegen has one now (`emit_user_drop_wrapper_skipping`, masking the
        // body step only), and the bail came out on both backends in a single
        // commit — so this flips to the one body that was always due.
        (
            "pin-owndrop-mixed",
            format!(
                "{HDR}struct Sd3 {{ a: R, b: R }}\n\
                 impl Drop for Sd3 {{ fn drop(mut ref self) {{ println(\"dSd3\") }} }}\n\
                 fn take(r: R) -> i64 {{ let s = Sd3 {{ a: r, b: R {{ id: 2 }} }}; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dSd3\ndR2\ndR1\nv=7\n",
        ),
        // A whole-value rebind after a MIXED wrap used to re-arm the full walk — the
        // mask is keyed on the binding and the destination registers afresh.
        // FIXED by B-2026-08-31-50: the enum-ctor slot mask is stored per
        // binding on both backends now and a rebind inherits it.
        (
            "pin-mixed-then-rebind",
            format!(
                "{HDR}fn take(r: R) -> i64 {{ let w = W2.Two(r, R {{ id: 2 }}); let w2 = w; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
        // A `Vec` literal doubled too, but a plain LOCAL moved into one doubled
        // identically — so it was a move-suppression hole, not this rule. Both
        // spellings stay pinned, plus the FRESH-element case, since the three
        // together are what identified the defect.
        //
        // NOW FIXED under B-2026-08-29-45, which needed both halves: the
        // move-suppression retraction for the local source and a caller-retains
        // mask (`mask_param_view_container_literal_elems`) for the param one.
        // These two expectations were the known-bad output recorded on purpose;
        // they are now the correct single body. The MIXED literal stays unfixed
        // by design — a `Vec`'s arity is not fixed, so it cannot carry the
        // per-slot mask a tuple can, and both backends decline it together.
        (
            "pin-vec-param-view",
            format!(
                "{HDR}fn take(r: R) -> i64 {{ let v2 = [r]; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR1\nv=7\n",
        ),
        (
            "pin-vec-local",
            format!(
                "{HDR}fn take() -> i64 {{ let m = R {{ id: 4 }}; let v2 = [m]; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\") }}\n"
            ),
            "dR4\nv=7\n",
        ),
        (
            "vec-fresh-elem-is-correct",
            format!(
                "{HDR}fn take() -> i64 {{ let v2 = [R {{ id: 4 }}]; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\") }}\n"
            ),
            "dR4\nv=7\n",
        ),
        // Moving a param view back OUT of the struct it was wrapped into: the
        // destination used to register a body the caller also runs. FIXED
        // (B-2026-08-29-47), in the same commit as the codegen twin — this
        // expectation was the recorded known-bad `dR1 dR1`. The local-source
        // twin beside it never moved, which isolated the cause.
        (
            "view-field-moved-out",
            format!(
                "{HDR}fn take(r: R) -> i64 {{ let s = S {{ r: r }}; let x = s.r; 7 }}\n\
                 fn main() {{ let v = take(R {{ id: 1 }}); println(f\"v={{v}}\") }}\n"
            ),
            "dR1\nv=7\n",
        ),
        (
            "local-field-moved-out-is-correct",
            format!(
                "{HDR}fn take() -> i64 {{ let s = S {{ r: R {{ id: 1 }} }}; let x = s.r; 7 }}\n\
                 fn main() {{ let v = take(); println(f\"v={{v}}\") }}\n"
            ),
            "dR1\nv=7\n",
        ),
        // TWO owned params, no wrap: both backends now drop the caller's fresh
        // temps in REVERSE argument order. This case pinned the divergence that
        // was B-2026-08-29-46 (this backend forward, both compiled backends
        // reverse) and is kept as the wrap-free neighbour of the cases above —
        // the full surface lives in
        // `test_owned_param_temps_drop_in_reverse_argument_order`.
        (
            "two-owned-params-drop-order",
            format!(
                "{HDR}fn take(r: R, q: R) -> i64 {{ 7 }}\n\
                 fn main() {{\n\
                 \x20   let v = take(R {{ id: 1 }}, R {{ id: 2 }});\n\
                 \x20   println(f\"v={{v}}\")\n\
                 }}\n"
            ),
            "dR2\ndR1\nv=7\n",
        ),
    ] {
        assert_eq!(run(&src), want, "case {label}");
    }
}

/// B-2026-08-28-39 — `let _ = <own-Drop enum ctor>` runs the enum's own body
/// AND its payload's, matching both compiled backends.
///
/// Two spellings of one construction were wrong in two different ways, which is
/// what made the shape read as a single payload-only gap:
///
///   * QUALIFIED (`let _ = E.A(R { .. })`) ran the OWN body and stopped, 1
///     against compiled's 2. `run_discarded_value_user_drops` runs an
///     own-`impl Drop` enum's own body and deliberately does not walk the
///     payload, on the stated ground that firing payloads "would print bodies
///     `karac build` does not" (the B-2026-08-01-2 p3 probe). That premise had
///     since stopped holding at this site: compiled now prints both.
///   * BARE (`let _ = A(R { .. })`) ran NOTHING — 0 against compiled's 2 —
///     because the discard gate's `Identifier` arm looks for a program
///     FUNCTION and a variant name is not one, so the whole discard was
///     declined. The qualified spelling was admitted by the `Path` arm all
///     along, which is exactly why only the payload looked missing.
///
/// THE PAYLOAD WALK IS SITE-LOCAL, and that is the care in this fix rather than
/// an implementation detail. `run_discarded_value_user_drops` has 31 callers,
/// and widening it moves every one of them at once. Measured, not assumed: the
/// first cut of this fix did exactly that, and `call-source-control` below is
/// the row that caught it and still holds the gate.
///
/// The leaf that the first cut ALSO disturbed — the wildcard-destructure path
/// (B-2026-08-28-12), then one body against this site's two — has since moved to
/// two on all three backends under B-2026-08-28-40, by the same site-local
/// method rather than by widening the shared walker. `wildcard-leaf-control`
/// tracks it at its new value.
///
/// `call-source-control` is the second thing measurement forced. Firing for any
/// own-`Drop` enum value here took `let _ = mk()` from 1/1/1 to 2/1/1 — a
/// divergence introduced by the fix — because compiled walks the payload for a
/// variant built AT the discard and not for one arriving from a call. The gate
/// is therefore on an INLINE construction, and this row is what holds it there.
#[test]
fn test_discarded_own_drop_enum_ctor_runs_own_and_payload_bodies() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "qualified-ctor",
            "fn main() { let _ = E.A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        (
            "bare-ctor",
            "fn main() { let _ = A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        // CONTROL — a CALL source. Compiled runs the own body alone here, so
        // this must stay at one; the inline-construction gate is what keeps it.
        (
            "call-source-control",
            "fn mk() -> E { E.A(R { id: 41 }) }\n\
             fn main() { let _ = mk(); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        // CONTROL — the wildcard LEAF one level in. Its own site-local walk
        // (B-2026-08-28-40) brought it to two bodies on every backend; it is
        // pinned here so the two discard sites stay in step.
        (
            "wildcard-leaf-control",
            "fn main() { let p = (E.A(R { id: 41 }), 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\ndrop R41\n1\n",
        ),
        // B-2026-08-28-48 — the BARE STATEMENT position of both spellings, the
        // third site in this sequence. The same two-spellings-two-failures
        // split this test documents for `let _ =` was present here and had
        // never been fixed:
        //
        //   qualified `E.A(..);`  ran the own body and stopped   (1 vs 2)
        //   bare      `A(..);`    ran NOTHING at all             (0 vs 2)
        //
        // Both now match the `let _ =` rows above and the bound yardstick.
        (
            "qualified-ctor-stmt",
            "fn main() { E.A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        (
            "bare-ctor-stmt",
            "fn main() { A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        // CONTROL — a CALL source in STATEMENT position. This is the arm the
        // bare-spelling fix sits beside, and compiled runs the own body alone
        // here exactly as it does for `let _ = mk()`. It fails the moment the
        // variant check is loosened into a general "any Identifier callee".
        (
            "call-source-stmt-control",
            "fn mk() -> E { E.A(R { id: 41 }) }\n\
             fn main() { mk(); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
        // CONTROL — a payloadless variant in statement position, both
        // spellings. One body is correct; these pin that the new walk adds
        // nothing where there is no payload.
        (
            "unit-variant-stmt",
            "fn main() { E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "bare-unit-variant-stmt",
            "fn main() { B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }

    // CONTROL — a payload-only enum (no own `impl Drop`) keeps its existing
    // single payload body; the new walk must not double it. Carries its own
    // header, which is why it sits outside the table above.
    assert_eq!(
        run("enum E { A(R), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { let _ = E.A(R { id: 41 }); println(\"end\") }\n"),
        "drop R41\nend\n",
        "payload-only-control"
    );
    // CONTROL — the same payload-only enum in STATEMENT position. Its single
    // payload body comes from the shared discard walk, not from the own-`Drop`
    // gated line B-2026-08-28-48 added, so this row is what shows that line is
    // additive rather than a replacement.
    assert_eq!(
        run("enum E { A(R), B }\n\
             struct R { id: i64 }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n\
             fn main() { E.A(R { id: 41 }); println(\"end\") }\n"),
        "drop R41\nend\n",
        "payload-only-stmt-control"
    );
}

/// B-2026-08-28-43, interpreter leg — the BARE spelling of a unit variant runs
/// its enum's `Drop` body as a fresh ARGUMENT, and both spellings run it in
/// bare STATEMENT position.
///
/// The two halves fail for different reasons and only one is shared with
/// codegen:
///
///   * ARGUMENT (`take(B)`) — zero here, and the cause is a lookup that cannot
///     answer the question it is asked. `run_fresh_temp_arg_drops` excluded a
///     named binding on `env.get(v).is_none()`, but the item pass seeds EVERY
///     unit variant into the outermost scope as a constant, so `env.get("B")`
///     answers `Some` for the variant exactly as it does for a local. The
///     exclusion meant for bindings therefore swallowed the variant, and
///     `take(B)` ran no body while `take(E.B)` ran one.
///   * STATEMENT (`E.B;` / `B;`) — zero for both spellings, because the
///     statement-expression arm dispatches on `Call` / `MethodCall` and a unit
///     variant is neither. The `let _ =` sibling of this was B-2026-08-28-41.
///
/// The fix is `fresh_bare_unit_variant_enum`, which asks whether any INNER
/// scope binds the name rather than whether the name is bound at all.
/// `bound-then-passed-control` is the row that would catch the permissive
/// version: a local really is owned by its binding, and firing here too doubles
/// its body.
#[test]
fn test_bare_unit_variant_of_own_drop_enum_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "argument-bare",
            "fn take(e: E) -> i64 { 7 }\n\
             fn main() { let x = take(B); println(f\"{x}\") }\n",
            "drop E\n7\n",
        ),
        // Correct here all along — the asymmetry that made the family look
        // spelling-dependent rather than lookup-dependent.
        (
            "argument-qualified",
            "fn take(e: E) -> i64 { 7 }\n\
             fn main() { let x = take(E.B); println(f\"{x}\") }\n",
            "drop E\n7\n",
        ),
        (
            "statement-bare",
            "fn main() { B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "statement-qualified",
            "fn main() { E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        // Already correct on this backend, and pinned because the compiled
        // sibling of each was not.
        (
            "let-discard-bare",
            "fn main() { let _ = B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "tuple-leaf-bare",
            "fn main() { let p = (B, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        // CONTROL — BOUND, the yardstick.
        (
            "bound-control",
            "fn main() { let e = B; println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
        // CONTROL, the DOUBLE-FREE direction. A local named for a variant is
        // owned by its binding; the argument arm must not fire for it as well.
        // This is the row that fails if the new lookup asks "is this a variant
        // name" instead of "is this shadowed".
        (
            "bound-then-passed-control",
            "fn take(e: E) -> i64 { 7 }\n\
             fn main() { let e = B; println(f\"{take(e)}\") }\n",
            "7\ndrop E\n",
        ),
        // CONTROL — a PASSTHROUGH callee owns the drop through its result.
        (
            "passthrough-control",
            "fn pass(e: E) -> E { e }\n\
             fn main() { let y = pass(B); println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
    // BOUNDARY — a bare `None`. The scan reads `program.items`, which carries
    // the user program and not the baked stdlib, so a seeded variant is out of
    // reach by construction; codegen filters `seeded_enum_names` to land in the
    // same place.
    assert_eq!(
        run("fn take(o: Option[String]) -> i64 { 7 }\n\
             fn main() { let x = take(None); println(f\"{x}\") }\n"),
        "7\n",
        "bare-None-control"
    );
}

/// B-2026-08-28-41, interpreter leg — `let _ = E.B`, a payloadless variant of
/// an own-`impl Drop` enum, runs that enum's body.
///
/// Pre-fix zero, on this backend and both compiled ones. Here the cause is
/// `discard_rhs_produces_owned_value`, whose arms are `StructLiteral`,
/// `Identifier`, `Tuple`, `Call` and `MethodCall`: a QUALIFIED unit variant is
/// a bare `Path` and matched none of them, so the discard never fired.
///
/// The BARE spelling was admitted by the `Identifier` arm all along and is
/// already correct on this backend — which is exactly what made the family look
/// spelling-dependent rather than site-dependent. It is still zero on the
/// COMPILED backends, for an unrelated reason on that side, and is filed
/// separately.
#[test]
fn test_discarded_unit_variant_of_own_drop_enum_runs_its_body() {
    const H: &str = "enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"drop E\") } }\n\
         struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"drop R{self.id}\") } }\n";
    for (label, body, want) in [
        (
            "qualified-unit-variant",
            "fn main() { let _ = E.B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        // Already correct on this backend via the `Identifier` arm; pinned so
        // the two spellings stay in step here.
        (
            "bare-unit-variant",
            "fn main() { let _ = B; println(\"end\") }\n",
            "drop E\nend\n",
        ),
        (
            "bound-control",
            "fn main() { let e = E.B; println(\"mid\") }\n",
            "drop E\nmid\n",
        ),
        (
            "wildcard-leaf-control",
            "fn main() { let p = (E.B, 1); let (_, n) = p; println(f\"{n}\") }\n",
            "drop E\n1\n",
        ),
        (
            "payload-ctor-control",
            "fn main() { let _ = E.A(R { id: 41 }); println(\"end\") }\n",
            "drop E\ndrop R41\nend\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

/// B-2026-08-14-10 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_user_enum_shadowing_builtin_variant_names`, same source and
/// expected string.
///
/// The interpreter does its own resolution and ran these correctly even on the
/// runs the typechecker rejected, which is what made the bug a three-way
/// disagreement rather than a plain divergence. Pinning the twin holds the
/// resolution rule across all three surfaces.
#[test]
fn test_user_enum_shadowing_builtin_variant_names() {
    assert_eq!(
        run("enum Sink { None, Open(i64) }\n\
            enum Slot { Ok, Err, Eof }\n\
            enum MyIoErr { Other(i64), Eof }\n\
            fn make(x: i64) -> Option[i64] { if x > 0 { Some(x) } else { None } }\n\
            fn div(x: i64) -> Result[i64, i64] { if x > 0 { Ok(x) } else { Err(0 - 1) } }\n\
            fn main() {\n\
                match make(1) { Some(v) => println(v), None => println(-1) }\n\
                match make(0) { Some(v) => println(v), None => println(-1) }\n\
                match div(2) { Ok(v) => println(v), Err(e) => println(e) }\n\
                match div(0) { Ok(v) => println(v), Err(e) => println(e) }\n\
                let s: Sink = Sink.None;\n\
                match s { None => println(-2), Open(v) => println(v) }\n\
                let e = Other(7);\n\
                match e { Other(v) => println(v), Eof => println(-3) }\n\
            }"),
        "1\n-1\n2\n-1\n-2\n7\n"
    );
}

/// B-2026-08-01-13 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_owned_enum_arg_payload_body_single_caller_fire`, same source and
/// expected string. Pre-fix the interpreter was silent for the
/// whole-drop shapes (no payload walk in the fresh-arg hook) and DOUBLE
/// for the identifier-arg match/if-let shapes (arm stash + caller NLL);
/// the param-scrutinee gate plus the fresh-arg payload walk leave exactly
/// the caller's single fire everywhere.
#[test]
fn test_owned_enum_arg_payload_body_single_caller_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Loud { Hold(Res), Quiet }\n\
             impl Drop for Loud {\n\
                 fn drop(mut ref self) {\n\
                     println(\"loud drop\")\n\
                 }\n\
             }\n\
             enum E2 { B(Res), Empty }\n\
             fn check(w: E2) { println(\"checked\"); }\n\
             fn check_match(w: E2) {\n\
                 match w {\n\
                     E2.B(r2) => { println(f\"got {r2.id}\"); }\n\
                     E2.Empty => { println(\"none\"); }\n\
                 }\n\
                 println(\"match done\");\n\
             }\n\
             fn check_iflet(w: E2) {\n\
                 if let E2.B(r2) = w {\n\
                     println(f\"if {r2.id}\");\n\
                 }\n\
                 println(\"iflet done\");\n\
             }\n\
             fn check_loud(w: Loud) { println(\"loudcheck\"); }\n\
             fn mk(n: i64) -> E2 { return E2.B(Res { id: n, name: f\"x{n}\" }); }\n\
             fn main() {\n\
                 println(\"a\");\n\
                 check(E2.B(Res { id: 55, name: f\"x{55}\" }));\n\
                 println(\"b\");\n\
                 check_loud(Loud.Hold(Res { id: 9, name: f\"l{9}\" }));\n\
                 println(\"c\");\n\
                 let e = mk(53);\n\
                 check_match(e);\n\
                 println(\"d\");\n\
                 check_iflet(E2.B(Res { id: 6, name: f\"x{6}\" }));\n\
                 println(\"end\");\n\
             }\n"),
        "a\nchecked\ndrop 55 x55\nb\nloudcheck\nloud drop\ndrop 9 l9\nc\ngot 53\n\
         match done\ndrop 53 x53\nd\nif 6\niflet done\ndrop 6 x6\nend\n"
    );
}

/// B-2026-08-01-7 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_owned_self_enum_receiver_single_fire`, same source and expected
/// string. Pre-fix both backends double-fired the match-consume shape
/// identically (arm channel + the receiver binding's still-armed walk).
///
/// B-2026-09-06-39 — CELL `b` WAS PINNING A LOST BODY, and its own label said
/// so: `just_three(self)` never destructures `self`, and this fixture recorded
/// `w=3` with no `drop 8 e8` under the words "consumed silently". The disarm
/// cell `a` needed is a HAND-OFF to the arm channel, and cell `b` has no arm —
/// so the hand-off reached nobody and the payload's body was lost on every
/// surface. Both cells now fire exactly once: `a` from the arm, `b` from the
/// caller's restored walk. The label is corrected to match.
#[test]
fn test_owned_self_enum_receiver_single_fire() {
    assert_eq!(
        run("struct Res { id: i64, name: String }\n\
             impl Drop for Res {\n\
                 fn drop(mut ref self) {\n\
                     println(f\"drop {self.id} {self.name}\")\n\
                 }\n\
             }\n\
             enum Box2 { Full(Res), Empty }\n\
             impl Box2 {\n\
                 fn into_id(self) -> i64 {\n\
                     match self {\n\
                         Box2.Full(r) => { return r.id; }\n\
                         Box2.Empty => { return 0; }\n\
                     }\n\
                 }\n\
                 fn just_three(self) -> i64 {\n\
                     return 3;\n\
                 }\n\
             }\n\
             fn mk_e(n: i64) -> Box2 {\n\
                 return Box2.Full(Res { id: n, name: f\"e{n}\" });\n\
             }\n\
             fn main() {\n\
                 println(\"a: owned-self match-consume fires once via the arm\");\n\
                 let b = mk_e(7);\n\
                 let v = b.into_id();\n\
                 println(f\"v={v}\");\n\
                 println(\"b: owned-self non-consuming — runs its payload body\");\n\
                 let c = mk_e(8);\n\
                 let w = c.just_three();\n\
                 println(f\"w={w}\");\n\
                 println(\"end\");\n\
             }\n"),
        "a: owned-self match-consume fires once via the arm\ndrop 7 e7\nv=7\n\
         b: owned-self non-consuming — runs its payload body\ndrop 8 e8\nw=3\nend\n"
    );
}

/// B-2026-07-30-11 (enum leg) — the interpreter half: a value enum's
/// live-variant payload runs its user `impl Drop` body when the enum binding
/// dies. Same source and expected output as `tests/codegen.rs`'s
/// `e2e_enum_payload_runs_user_drop_bodies`.
///
/// Both backends were silent here, so this pair is what makes the new behaviour
/// a shared contract rather than one backend drifting ahead of the other.
#[test]
fn test_enum_payload_runs_user_drop_bodies() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             struct W { r: Res }\n\
             enum Slot { Empty, Full(Res) }\n\
             enum Named { None0, One { r: Res, tag: i64 } }\n\
             enum Pair { Zero, Two(Res, Res) }\n\
             enum Nest { Nil, Wrap(W) }\n\
             enum Plain { A, B(i64) }\n\
             fn main() {\n\
                 { let s = Slot.Full(Res { id: 21 }); println(1); }\n\
                 { let n = Named.One { r: Res { id: 22 }, tag: 5 }; println(2); }\n\
                 { let p = Pair.Two(Res { id: 23 }, Res { id: 24 }); println(3); }\n\
                 { let w = Nest.Wrap(W { r: Res { id: 25 } }); println(4); }\n\
                 { let q = Plain.B(7); println(5); }\n\
                 println(999);\n\
             }\n"),
        "21\n1\n22\n2\n24\n23\n3\n25\n4\n5\n999\n"
    );
}

/// B-2026-07-30-11 (enum leg) — the interpreter half of the move-out disarm.
/// Same source and expected output as `tests/codegen.rs`'s
/// `e2e_enum_payload_move_out_disarms_source_drop`.
///
/// The interpreter's disarm has to be as COARSE as codegen's compile-time
/// retraction: it fires when any arm consumes the payload, not only the arm
/// taken. Anything finer would print a body `karac build` does not.
#[test]
fn test_enum_payload_move_out_disarms_source_drop() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             enum Slot { Empty, Full(Res) }\n\
             fn main() {\n\
                 { let s = Slot.Full(Res { id: 31 });\n\
                   match s { Slot.Full(r) => println(r.id), Slot.Empty => println(0) } }\n\
                 { let t = Slot.Full(Res { id: 32 });\n\
                   match t { Slot.Full(_) => println(100), Slot.Empty => println(0) } }\n\
                 { let u = Slot.Full(Res { id: 33 });\n\
                   if let Slot.Full(q) = u { println(q.id) } }\n\
                 println(999);\n\
             }\n"),
        "31\n31\n100\n32\n33\n33\n999\n"
    );
}

/// B-2026-07-31-5 — the interpreter half: an enum's OWN `impl Drop` body at
/// the NLL live-range end, then its payload's. Same source and expected output
/// as `tests/codegen.rs`'s `e2e_enum_own_drop_body_fires_at_nll_end`. The
/// interpreter was already right here; the twin is what pins codegen to it.
#[test]
fn test_enum_own_drop_body_fires_at_nll_end() {
    assert_eq!(
        run("struct Res { id: i64 }\n\
             impl Drop for Res { fn drop(mut ref self) { println(self.id); } }\n\
             enum Bare { Empty, Full(i64) }\n\
             impl Drop for Bare { fn drop(mut ref self) { println(1); } }\n\
             enum Both { Nil, Held(Res) }\n\
             impl Drop for Both { fn drop(mut ref self) { println(2); } }\n\
             fn main() {\n\
                 { let b = Bare.Full(7); println(50); }\n\
                 { let h = Both.Held(Res { id: 41 }); println(51); }\n\
                 println(999);\n\
             }\n"),
        "1\n50\n2\n41\n51\n999\n"
    );
}

/// The oracle twin for B-2026-08-15-12. A user enum shadowing a prelude handle
/// type ran CORRECTLY here throughout the bug's life — codegen was the side
/// that refused to build it (`Undefined variable 'x'`), which is what made the
/// row a run-vs-build divergence rather than a wrong answer.
///
/// Pinned so the parity is asserted from both ends: `tests/codegen.rs`'s
/// `test_e2e_user_enum_shadowing_a_prelude_handle_type_reads_its_payload`
/// asserts the same values out of a built binary, and neither side can drift
/// without one of them failing.
#[test]
fn test_user_enum_shadowing_a_prelude_handle_type() {
    for name in [
        "Map",
        "Set",
        "SortedMap",
        "SortedSet",
        "Tensor",
        "Column",
        "DataFrame",
        "Interner",
        "Arena",
        "Request",
        "File",
        "Sender",
        "Receiver",
        "Channel",
    ] {
        assert_eq!(
            run(&format!(
                "enum {name} {{ A(i64), B }}\n\
                 fn f(e: ref {name}) -> i64 {{ match e {{ A(x) => x, B => 0 }} }}\n\
                 fn g(e: {name}) -> i64 {{ match e {{ A(x) => x + 1, B => 0 }} }}\n\
                 fn main() {{ println(f({name}.A(4))); println(g({name}.A(4))); \
                     println(f({name}.B)); }}\n"
            )),
            "4\n5\n0\n",
            "{name}: the interpreter is the oracle here and must not drift",
        );
    }
}

#[test]
fn test_prelude_colliding_variant_ctor_oracle() {
    // Oracle twin of `tests/codegen.rs`'s
    // `test_e2e_prelude_colliding_variant_constructs_bare` (B-2026-08-17-7).
    // `Request`/`Response`/`File` are prelude names; the bare constructor in
    // value position must mean the USER's variant on all three backends.
    let out = run("\n\
         enum Ev { Request(String), Response(i64), Idle }\n\
         enum Mode { Fast(i64), File }\n\
         fn describe(e: Ev) -> String {\n\
             match e {\n\
                 Request(p) => f\"req {p}\",\n\
                 Response(c) => f\"resp {c}\",\n\
                 Idle => \"idle\",\n\
             }\n\
         }\n\
         fn main() {\n\
             let a = Request(\"/users\");\n\
             let b = Response(200);\n\
             let c = Idle;\n\
             println(describe(a));\n\
             println(describe(b));\n\
             println(describe(c));\n\
             let m = File;\n\
             match m { Mode.File => println(\"file\"), _ => println(\"fast\") }\n\
         }\n");
    assert_eq!(out, "req /users\nresp 200\nidle\nfile\n");
}

/// The interpreter twin of `e2e_generic_enum_display_renders_at_its_instantiation`
/// (tests/codegen.rs), B-2026-08-19-28.
///
/// The interpreter was already correct here — it renders from the VALUE, which
/// carries its payload directly — so it is the reference the codegen fix was
/// measured against. Pinning it is what stops the pair drifting: codegen had to
/// learn to substitute a generic enum's parameters from the use site, and the
/// expected strings below are what it must keep producing.
#[test]
fn a_generic_enum_renders_at_its_instantiation() {
    assert_eq!(
        run_no_errors(
            "#[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             fn main() {\n\
             let mut a: Vec[Option[i64]] = vec![];\n\
             a.push(Some(5i64));\n\
             a.push(None);\n\
             println(a);\n\
             let mut b: Vec[Result[i64, String]] = vec![];\n\
             b.push(Ok(5i64));\n\
             b.push(Err(\"bad\"));\n\
             println(b);\n\
             let mut m: Map[String, Option[i64]] = Map.new();\n\
             m.insert(\"k\", Some(5i64));\n\
             println(m);\n\
             let mut g: Vec[MyOpt[i64]] = vec![];\n\
             g.push(MyOpt.Has(5i64));\n\
             g.push(MyOpt.Empty);\n\
             println(g);\n\
             let mut h: Vec[MyOpt[String]] = vec![];\n\
             h.push(MyOpt.Has(\"hello\"));\n\
             println(h);\n\
             let mut i: Vec[i64] = vec![];\n\
             i.push(7i64);\n\
             let mut v: Vec[MyOpt[Vec[i64]]] = vec![];\n\
             v.push(MyOpt.Has(i));\n\
             println(v);\n\
             }"
        ),
        "[Some(5), None]\n\
         [Ok(5), Err(bad)]\n\
         {k: Some(5)}\n\
         [Has(5), Empty]\n\
         [Has(hello)]\n\
         [Has([7])]\n"
    );
}

/// The interpreter twin of `e2e_generic_enum_display_direct_spellings`
/// (tests/codegen.rs), B-2026-08-19-30. The interpreter renders from the VALUE,
/// so it was correct throughout and is the reference codegen was measured
/// against; pinning it keeps the pair from drifting.
#[test]
fn a_generic_enum_renders_directly_at_its_instantiation() {
    assert_eq!(
        run_no_errors(
            "#[derive(Display)]\n\
             enum MyOpt[T] { Has(T), Empty }\n\
             #[derive(Display)]\n\
             enum Pair2[A, B] { Both(A, B), Left(A), Neither }\n\
             fn main() {\n\
             let a: MyOpt[i64] = MyOpt.Has(5i64);\n\
             println(a);\n\
             let b: MyOpt[String] = MyOpt.Has(\"hi\");\n\
             println(b);\n\
             let d: MyOpt[u64] = MyOpt.Has(18446744073709551615u64);\n\
             println(d);\n\
             let e: Pair2[i64, String] = Pair2.Both(7i64, \"s\");\n\
             println(e);\n\
             println(f\"{a} {b}\");\n\
             println(a.to_string());\n\
             }"
        ),
        "Has(5)\n\
         Has(hi)\n\
         Has(18446744073709551615)\n\
         Both(7, s)\n\
         Has(5) Has(hi)\n\
         Has(5)\n"
    );
}

// ── B-2026-08-19-16: qualified unit-variant paths across colliding enums ──

#[test]
fn qualified_unit_variant_path_picks_its_own_enum() {
    // B-2026-08-19-16. `register_items` bound user-enum unit variants under
    // the BARE name only, and `eval_expr`'s Path arm falls back to the last
    // segment when the qualified key misses. So two enums sharing a variant
    // name both wrote `env["A"]`, the later declaration won, and `First.A`
    // evaluated to a `Second` value — which method dispatch then read
    // `enum_name` off, calling the wrong impl.
    //
    // Everything else agreed, which is what made it silent: the static type
    // was `First`, and the pattern matcher compares only the variant name.
    assert_eq!(
        run("enum First { A, B }
             enum Second { A, B }
             impl First { fn tag(ref self) -> i64 { 1 } }
             impl Second { fn tag(ref self) -> i64 { 2 } }
             fn main() {
                 println(First.A.tag());
                 println(Second.A.tag());
             }"),
        "1\n2\n"
    );
}

#[test]
fn qualified_unit_variant_through_a_let_binding() {
    // Same defect reached through a binding rather than a direct path — the
    // wrong `enum_name` travels with the value, so the call site is not what
    // matters.
    assert_eq!(
        run("enum First { A, B }
             enum Second { A, B }
             impl First { fn tag(ref self) -> i64 { 4 } }
             impl Second { fn tag(ref self) -> i64 { 5 } }
             fn main() {
                 let d = First.A;
                 println(d.tag());
             }"),
        "4\n"
    );
}

#[test]
fn colliding_tuple_and_struct_variants_keep_their_enum() {
    // The non-unit variants were ALREADY correct — they are constructed at
    // call sites where the enum name is in hand, never through the bare-name
    // env binding. Pinned so a future refactor of variant construction cannot
    // regress them into the unit variants' old behaviour.
    assert_eq!(
        run("enum First { W(i64), X }
             enum Second { W(i64), X }
             impl First { fn tag(ref self) -> i64 { 1 } }
             impl Second { fn tag(ref self) -> i64 { 2 } }
             fn main() {
                 println(First.W(7).tag());
                 println(Second.W(7).tag());
             }"),
        "1\n2\n"
    );
}

#[test]
fn bare_unit_variant_still_resolves_when_unambiguous() {
    // The fix ADDS the qualified binding; the bare one stays, because the
    // pattern matcher classifies a bare PascalCase identifier as a unit-variant
    // pattern only when `env.get(name)` returns a unit `EnumVariant` (see the
    // `Ordering` registration comment in `register_items`). Both the expression
    // and the match arm must keep working.
    assert_eq!(
        run("enum Only { A, B }
             impl Only { fn tag(ref self) -> i64 { 42 } }
             fn main() {
                 let x = A;
                 println(x.tag());
                 match x { A => println(1), B => println(2), }
             }"),
        "42\n1\n"
    );
}

// ── B-2026-08-19-17 (a): interpreter follows the typechecker's variant pick ──

#[test]
fn bare_variant_resolution_matches_the_typechecker_not_declaration_order() {
    // B-2026-08-19-17 (a). `register_items` binds a bare variant name by plain
    // last-write-wins, so the LATER-declared enum owned `env["A"]`. The
    // typechecker instead uses B-2026-08-14-10's two-tier sorted scan, and
    // codegen follows the typechecker — so `karac run` and `karac build` handed
    // out different enums for the same bare name (measured: build printed 1,
    // run printed 2).
    //
    // Here `Alpha` is declared SECOND but wins the typechecker's sort, so
    // last-write-wins and the real rule disagree. The interpreter must follow
    // the typechecker.
    //
    // The program is only reachable because a user-vs-user collision is
    // rejected in EXPRESSION position (B-2026-08-19-17 (b)) — this one resolves
    // through a pattern, which stays scrutinee-typed and legal.
    assert_eq!(
        run("enum Zebra { A, B }
             enum Alpha { A, C }
             fn main() {
                 let z = Zebra.A;
                 match z { A => println(1), B => println(2), }
                 let a = Alpha.A;
                 match a { A => println(3), C => println(4), }
             }"),
        "1\n3\n"
    );
}

#[test]
fn user_enum_does_not_hijack_the_builtin_bare_none() {
    // The typechecker pins `Some`/`None`/`Ok`/`Err` to their builtin owner so a
    // user enum cannot capture a bare `None`. The interpreter's env had no such
    // rule — the user enum registered after the prelude and simply overwrote
    // `env["None"]`. Masked in most probes because a `match` arm compares only
    // the VARIANT name, so a wrongly-tagged `Cache.None` still hits the `None`
    // arm; the retag is what makes the value itself right.
    assert_eq!(
        run("enum Cache { None, Warm }
             fn take(o: Option[i64]) -> i64 { match o { Some(v) => v, None => -1, } }
             fn main() { println(take(None)); println(take(Some(9))); }"),
        "-1\n9\n"
    );
}

#[test]
fn variant_literal_receiver_survives_a_method_returning_a_colliding_enum() {
    // The same defect with `cmp` swapped out, which is what shows it was never
    // about `cmp`: the trigger is a method on a variant literal whose RETURN
    // type is a `Named` type. Here `Color.Red.to_fruit()` records `Fruit` at
    // the callee span, and `Fruit` also declares `Red` — so the receiver was
    // retagged to `Fruit.Red` and dispatch then failed outright with
    // "method 'to_fruit' not found on type 'Fruit'". Loud rather than silent,
    // but the same single cause.
    //
    // Both variants are exercised because the retag rewrote the enum name and
    // kept the variant, so a fixture using only one variant cannot tell a
    // correct receiver from a retagged one that happens to dispatch.
    assert_eq!(
        run_no_errors(
            "enum Color { Red, Green }
             enum Fruit { Red, Apple }
             impl Color {
                 fn to_fruit(self) -> Fruit {
                     match self { Red => return Fruit.Apple, Green => return Fruit.Red, }
                 }
             }
             fn fruit_tag(f: Fruit) -> i64 {
                 match f { Red => return 0, Apple => return 1, }
             }
             fn main() {
                 println(fruit_tag(Color.Red.to_fruit()));
                 println(fruit_tag(Color.Green.to_fruit()));
             }"
        ),
        "1\n0\n"
    );
}

#[test]
fn variant_literal_receiver_with_a_struct_returning_method_does_not_ice() {
    // The third shape the same cause took, and the worst-behaved: when the
    // method returns a STRUCT, the recorded `Named` type names no enum at all,
    // so the receiver was retagged to an enum name that does not exist, the
    // call produced `Value::Unit`, and the following field read PANICKED the
    // interpreter — `internal error: entered unreachable code: field access
    // ...: receiver was Value::Unit not Struct/SharedStruct`, a message that
    // blames the typechecker or a wrong-variant codepath when both were right.
    // A raw panic also violates the standing never-panic diagnostics rule, so
    // this leg is worth pinning on its own rather than folding into the enum
    // one above.
    assert_eq!(
        run_no_errors(
            "enum Color { Red, Green }
             struct Tag { n: i64 }
             impl Color {
                 fn to_tag(self) -> Tag {
                     match self { Red => return Tag { n: 1 }, Green => return Tag { n: 2 }, }
                 }
             }
             fn main() {
                 println(Color.Red.to_tag().n);
                 println(Color.Green.to_tag().n);
             }"
        ),
        "1\n2\n"
    );
}

#[test]
fn variant_literal_receiver_with_a_primitive_returning_method_still_works() {
    // The leg that always worked, kept as the boundary marker: a primitive
    // return type records `Type::Int`, which is not `Named`, so the retag
    // declined and the receiver survived. This is why B-2026-07-13-4 — the
    // commit that introduced the literal-receiver lowering, with a fixture
    // whose method returns `i64` — passed while three sibling return types
    // were broken. It passes both before and after the fix by design; its job
    // is to stop a future narrowing of the retag from being mistaken for a
    // complete answer to this bug.
    assert_eq!(
        run_no_errors(
            "enum Color { Red, Green }
             impl Color {
                 fn code(self) -> i64 {
                     match self { Red => return 10, Green => return 20, }
                 }
             }
             fn main() { println(Color.Red.code()); println(Color.Green.code()); }"
        ),
        "10\n20\n"
    );
}

// ── B-2026-08-21-10: C-like enum `.discriminant()` ──────────────
//
// design.md § Enum Discriminant Runtime Surface. The values come from the
// typechecker's FOLDED table, which is what keeps the two backends from
// disagreeing about a declared `Audio = BASE + 1`.

#[test]
fn discriminant_reads_the_declared_value_not_the_tag() {
    // The spec's own example. Tags are 0/1/2 by declaration position; the
    // answers must be the DECLARED 1/3/8.
    let out = run("#[repr(u8)]\n\
    enum UsbClass { Audio = 0x01, Hid = 0x03, MassStorage = 0x08 }\n\
    fn main() {\n\
        let c = UsbClass.Hid;\n\
        let byte: u8 = c.discriminant();\n\
        println(f\"{byte} {UsbClass.Audio.discriminant()} {UsbClass.MassStorage.discriminant()}\");\n\
    }");
    assert_eq!(out, "3 1 8\n");
}

#[test]
fn discriminant_falls_back_to_declaration_position() {
    // No declared values: each variant's discriminant is its position, as in C.
    let out = run("enum Plain { A, B, C }\n\
    fn main() {\n\
        println(f\"{Plain.A.discriminant()} {Plain.B.discriminant()} {Plain.C.discriminant()}\");\n\
    }");
    assert_eq!(out, "0 1 2\n");
}

#[test]
fn discriminant_carries_a_signed_repr() {
    let out = run("#[repr(i8)]\n\
    enum Neg { Down = -128, Up = 127 }\n\
    fn main() {\n\
        let d: i8 = Neg.Down.discriminant();\n\
        println(f\"{d} {Neg.Up.discriminant()}\");\n\
    }");
    assert_eq!(out, "-128 127\n");
}

#[test]
fn discriminant_folds_a_constant_expression() {
    // The value need not be a literal — it is folded once, in the typechecker,
    // precisely so the interpreter and codegen cannot fold it differently.
    let out = run("const BASE: i64 = 16;\n\
    #[repr(u8)]\n\
    enum Op { Add = BASE + 1, Sub = BASE + 2 }\n\
    fn main() {\n\
        println(f\"{Op.Add.discriminant()} {Op.Sub.discriminant()}\");\n\
    }");
    assert_eq!(out, "17 18\n");
}

#[test]
fn discriminant_reaches_every_receiver_shape() {
    let out = run("#[repr(u8)]\n\
    enum Big { Lo = 0, Hi = 255 }\n\
    struct Holder { kind: Big }\n\
    #[repr(u8)]\n\
    enum Op { Add = 1, Sub = 2 }\n\
    impl Op {\n\
        fn code(ref self) -> u8 { self.discriminant() }\n\
    }\n\
    fn as_u8(k: Big) -> u8 { k.discriminant() }\n\
    fn main() {\n\
        let h = Holder { kind: Big.Hi };\n\
        let o = Op.Add;\n\
        println(f\"{h.kind.discriminant()} {as_u8(Big.Hi)} {Op.Sub.code()} {o.code()}\");\n\
    }");
    // Field access, a by-value parameter, a path receiver, and `self` inside
    // an impl all resolve to the same generated method.
    assert_eq!(out, "255 255 2 1\n");
}

#[test]
fn test_a_lowercase_sub_binding_is_still_a_binding_not_a_variant_test() {
    // The hazard the PascalCase gate exists for, kept as a negative control
    // across the predicate's extraction: an ordinary local holding a
    // unit-variant VALUE (`c`) must not turn a constructor's sub-binding of
    // the same name into a variant test. Before that gate this surfaced as a
    // spurious "non-exhaustive match" — the arm neither matched nor bound.
    let out = run("enum Color { Red, Green, Blue }\n\
        enum Msg { Info(Color), Quiet }\n\
        fn main() {\n\
            let c = Color.Green;\n\
            let m = Msg.Info(Color.Red);\n\
            match m {\n\
                Msg.Info(c) => match c { Red => println(\"payload=Red\"), Green => println(\"payload=Green\"), Blue => println(\"payload=Blue\") },\n\
                Msg.Quiet => println(\"quiet\"),\n\
            }\n\
            match c { Red => println(\"outer=Red\"), Green => println(\"outer=Green\"), Blue => println(\"outer=Blue\") }\n\
        }");
    assert_eq!(out, "payload=Red\nouter=Green\n");
}

#[test]
fn test_a_pascalcase_name_that_is_not_the_scrutinees_variant_still_binds() {
    // How NARROW the fix is: the scrutinee-directed lookup fires only when the
    // scrutinee's own enum declares that variant. A PascalCase name that does
    // not name one of its variants keeps the pre-existing catch-all-binding
    // behaviour, so nothing outside the reported shape moved. (Whether such a
    // pattern should be a resolve error at all is a separate question this fix
    // deliberately does not answer.)
    let out = run("enum Color { Red, Green, Blue }\n\
        fn main() {\n\
            let c = Color.Blue;\n\
            match c { Red => println(\"Red\"), NotAVariantOfColor => println(\"caught\") }\n\
        }");
    assert_eq!(out, "caught\n");
}

/// The binding form on the tail path, and the reason the interpreter can be
/// exact here: the payload comes from the ALREADY EVALUATED tail value, so the
/// argument expression is never re-run. (Codegen has no value in hand at that
/// point and must re-compile or word-extract it, which is its own problem.)
#[test]
fn test_errdefer_binding_sees_the_tail_err_payload() {
    assert_eq!(
        run("fn body() -> Result[i64, String] {\n\
                 errdefer(e) { print(e); }\n\
                 Err(\"payload\".to_string())\n\
             }\n\
             fn main() { let _ = body(); }"),
        "payload"
    );
}

/// B-2026-09-06-18 — the INTERPRETER twin of
/// `e2e_param_wrapped_in_returned_enum_variant_on_some_paths_has_one_owner`,
/// same program and the same expected string: the hand-back path doubled on
/// all four surfaces (the method spelling on the compiled ones only), and the
/// one predicate both backends read now settles every cell.
#[test]
fn test_param_wrapped_in_returned_enum_variant_on_some_paths_has_one_owner() {
    assert_eq!(
        run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
enum Slot { Held(R), Pair(R, i64), Boxed { r: R, n: i64 }, Empty }
enum Tagged { Held(R), Empty }
impl Drop for Tagged { fn drop(mut ref self) { println("dT") } }
struct H { n: i64 }
fn mk(i: i64) -> String { return f"pay-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; }
fn mr(i: i64) -> R { return R { id: i, s: mk(i) }; }
fn show(x: Slot) { match x { Slot.Held(v) => println(f"C{v.id}"), Slot.Pair(v, n) => println(f"P{v.id}"), Slot.Boxed { r, n } => println(f"X{r.id}"), Slot.Empty => println("CE") } }
fn fslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
fn fpair(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Pair(r, 1); }
fn fboxed(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Boxed { r: r, n: 1 }; }
fn ffresh(r: R, k: bool) -> Slot { if k { return Slot.Held(mr(90)); } return Slot.Held(r); }
fn ftail(r: R, k: bool) -> Slot { if k { Slot.Empty } else { Slot.Held(r) } }
fn fnest(r: R, k: bool, j: bool) -> Slot { if k { if j { return Slot.Held(r); } } return Slot.Empty; }
fn ftag(r: R, k: bool) -> Tagged { if k { return Tagged.Empty; } return Tagged.Held(r); }
impl H {
    fn aslot(r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
    fn mslot(ref self, r: R, k: bool) -> Slot { if k { return Slot.Empty; } return Slot.Held(r); }
}
fn main() {
    let h = H { n: 0 };
    println("st"); { let x = fslot(mr(1), true); show(x); }
    println("sf"); { let x = fslot(mr(2), false); show(x); }
    println("pt"); { let x = fpair(mr(3), true); show(x); }
    println("pf"); { let x = fpair(mr(4), false); show(x); }
    println("bt"); { let x = fboxed(mr(5), true); show(x); }
    println("bf"); { let x = fboxed(mr(6), false); show(x); }
    println("frt"); { let x = ffresh(mr(7), true); show(x); }
    println("frf"); { let x = ffresh(mr(8), false); show(x); }
    println("tlt"); { let x = ftail(mr(9), true); show(x); }
    println("tlf"); { let x = ftail(mr(10), false); show(x); }
    println("ntt"); { let x = fnest(mr(11), true, true); show(x); }
    println("ntf"); { let x = fnest(mr(12), true, false); show(x); }
    println("nff"); { let x = fnest(mr(13), false, false); show(x); }
    println("tgt"); { let x = ftag(mr(14), true); match x { Tagged.Held(v) => println(f"C{v.id}"), Tagged.Empty => println("CE") } }
    println("tgf"); { let x = ftag(mr(15), false); match x { Tagged.Held(v) => println(f"C{v.id}"), Tagged.Empty => println("CE") } }
    println("at"); { let x = H.aslot(mr(16), true); show(x); }
    println("af"); { let x = H.aslot(mr(17), false); show(x); }
    println("mt"); { let x = h.mslot(mr(18), true); show(x); }
    println("mf"); { let x = h.mslot(mr(19), false); show(x); }
    println("nt"); { let a = mr(20); let x = fslot(a, true); show(x); }
    println("nf"); { let b = mr(21); let x = fslot(b, false); show(x); }
    println("end");
}"#),
        "st\nd1\nCE\nsf\nC2\nd2\npt\nd3\nCE\npf\nP4\nd4\nbt\nd5\nCE\nbf\nX6\nd6\nfrt\nd7\nC90\nd90\nfrf\nC8\nd8\ntlt\nd9\nCE\ntlf\nC10\nd10\nntt\nC11\nd11\nntf\nd12\nCE\nnff\nd13\nCE\ntgt\nd14\nCE\ndT\ntgf\nC15\ndT\nd15\nat\nd16\nCE\naf\nC17\nd17\nmt\nd18\nCE\nmf\nC19\nd19\nnt\nd20\nCE\nnf\nC21\nd21\nend\n",
        "a param wrapped in a returned enum variant on some paths has one owner per path"
    );
}

/// B-2026-09-01-39 — the INTERPRETER twin of
/// `e2e_live_local_handed_out_of_a_discarded_branch_runs_payload_body_once`,
/// same program and the same expected string; this backend lost the payload
/// body on the `if` spelling and both backends lost it on the direct
/// `let _ = e;`.
#[test]
fn test_live_local_handed_out_of_a_discarded_branch_runs_payload_body_once() {
    assert_eq!(
        run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("dE") } }
struct W { r: R, n: i64 }
impl Drop for W { fn drop(mut ref self) { println("dW") } }
struct P { r: R, n: i64 }
enum E2 { A(R), B }
fn mk(n: i64) -> R { return R { id: n }; }
fn if_let_e2(c: bool) { let e = E2.A(mk(21)); let _ = if c { E2.A(mk(8)) } else { e }; println("mid") }
fn direct_let_e2() { let e = E2.A(mk(22)); let _ = e; println("mid") }
fn direct_bare_e2() { let e = E2.A(mk(23)); e; println("mid") }
fn direct_let_read(c: bool) { let e = E.A(mk(24)); let _ = e; println("mid") }
fn direct_let_e_unit() { let e = E.B; let _ = e; println("mid") }
fn if_let(c: bool) { let e = E.A(mk(1)); let _ = if c { E.A(mk(8)) } else { e }; println("mid") }
fn if_bare(c: bool) { let e = E.A(mk(2)); if c { E.A(mk(8)) } else { e }; println("mid") }
fn match_let(c: bool) { let e = E.A(mk(3)); let _ = match c { true => E.A(mk(8)), _ => e }; println("mid") }
fn match_bare(c: bool) { let e = E.A(mk(4)); match c { true => E.A(mk(8)), _ => e }; println("mid") }
fn direct_let() { let e = E.A(mk(5)); let _ = e; println("mid") }
fn direct_bare() { let e = E.A(mk(6)); e; println("mid") }
fn if_let_w(c: bool) { let w = W { r: mk(11), n: 1 }; let _ = if c { W { r: mk(8), n: 2 } } else { w }; println("mid") }
fn direct_let_w() { let w = W { r: mk(12), n: 1 }; let _ = w; println("mid") }
fn if_let_p(c: bool) { let p = P { r: mk(13), n: 1 }; let _ = if c { P { r: mk(8), n: 2 } } else { p }; println("mid") }
fn direct_let_p() { let p = P { r: mk(14), n: 1 }; let _ = p; println("mid") }
fn if_let_r(c: bool) { let r = mk(15); let _ = if c { mk(8) } else { r }; println("mid") }
fn direct_let_r() { let r = mk(16); let _ = r; println("mid") }
fn if_let_nested(c: bool) { let e = E.A(mk(17)); let _ = if c { E.A(mk(8)) } else { if c { E.B } else { e } }; println("mid") }
fn main() {
    println("if_let-f"); if_let(false);
    println("if_let-t"); if_let(true);
    println("if_bare-f"); if_bare(false);
    println("if_bare-t"); if_bare(true);
    println("match_let-f"); match_let(false);
    println("match_let-t"); match_let(true);
    println("match_bare-f"); match_bare(false);
    println("direct_let"); direct_let();
    println("direct_bare"); direct_bare();
    println("if_let_w-f"); if_let_w(false);
    println("if_let_w-t"); if_let_w(true);
    println("direct_let_w"); direct_let_w();
    println("if_let_p-f"); if_let_p(false);
    println("if_let_p-t"); if_let_p(true);
    println("direct_let_p"); direct_let_p();
    println("if_let_r-f"); if_let_r(false);
    println("direct_let_r"); direct_let_r();
    println("if_let_nested-f"); if_let_nested(false);
    println("if_let_e2-f"); if_let_e2(false);
    println("direct_let_e2"); direct_let_e2();
    println("direct_bare_e2"); direct_bare_e2();
    println("direct_let_e_unit"); direct_let_e_unit();
    println("end");
}"#),
        "if_let-f\ndE\ndR1\nmid\nif_let-t\ndE\ndR8\ndE\ndR1\nmid\nif_bare-f\ndE\ndR2\nmid\nif_bare-t\ndE\ndR8\ndE\ndR2\nmid\nmatch_let-f\ndE\ndR3\nmid\nmatch_let-t\ndE\ndR8\ndE\ndR3\nmid\nmatch_bare-f\ndE\ndR4\nmid\ndirect_let\ndE\ndR5\nmid\ndirect_bare\ndE\ndR6\nmid\nif_let_w-f\ndW\ndR11\nmid\nif_let_w-t\ndW\ndR8\ndW\ndR11\nmid\ndirect_let_w\ndW\ndR12\nmid\nif_let_p-f\ndR13\nmid\nif_let_p-t\ndR8\ndR13\nmid\ndirect_let_p\ndR14\nmid\nif_let_r-f\ndR15\nmid\ndirect_let_r\ndR16\nmid\nif_let_nested-f\ndE\ndR17\nmid\nif_let_e2-f\ndR21\nmid\ndirect_let_e2\ndR22\nmid\ndirect_bare_e2\ndR23\nmid\ndirect_let_e_unit\ndE\nmid\nend\n",
        "a live local handed out of a discarded branch runs its payload body once"
    );
}

/// B-2026-09-06-17 — a payload bound out of a PROJECTED enum and handed back
/// (`fn out(self) -> R { match self.e { E.A(r) => return r, .. } }`, and the
/// free-function twin `fn p_out(h: H1) -> R { match h.e { .. } }`) ran its `Drop`
/// body in the caller's walk over the argument as well as at the result's own
/// death, on a named local and a fresh temp alike, agreed on every surface. The
/// whole-param scanner keys on the bare parameter (`e_out`, one body throughout)
/// and the part-path scanner denotes returned PLACES; neither covered a projected
/// enum's payload. `fn_escaping_param_field_payload_paths` (and its owned-`self`
/// form) now reports the field path, and the callers mask that field's payload
/// bodies — in place on a named binding, payload-only in a fresh temp's walker.
///
/// `read` / `p_read` are the read-only arms (unchanged, one body); `out_some/not`
/// is the conservative-any-variant trade, where the un-taken hand-out path still
/// runs the payload's body once at the caller (`dR6` before `got1`); `out/temp`
/// is the fresh-temp RECEIVER, whose payload is one body but whose enum shell's
/// `dE` is B-2026-09-04-30's pre-existing loss, pinned as it stands.
///
/// Twin of `tests/codegen.rs`'s `e2e_projected_enum_payload_handed_out_runs_one_body`, pinned to the same string.
#[test]
fn test_projected_enum_payload_handed_out_runs_one_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H1 { e: E }
struct H2 { s: S }

impl H1 {
    fn out(self) -> R { match self.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
    fn out_iflet(self) -> R { if let E.A(r) = self.e { return r; } else { return mk(0); } }
    fn out_tail(self) -> R { match self.e { E.A(r) => r, E.B => mk(0) } }
    fn out_some(self, k: bool) -> R { match self.e { E.A(r) => { if k { return r; } return mk(1); } E.B => { return mk(0); } } }
    fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl H2 { fn out2(self) -> R { match self.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } } }
fn p_out(h: H1) -> R { match h.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_out2(h: H2) -> R { match h.s.e { E.A(r) => { return r; } E.B => { return mk(0); } } }
fn p_read(h: H1) -> i64 { match h.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
fn e_out(b: E) -> R { match b { E.A(r) => { return r; } E.B => { return mk(0); } } }

fn main() {
    println("out/local"); let a1 = H1 { e: E.A(mk(1)) }; let r1 = a1.out(); println(f"  got{r1.id}");
    println("out/temp"); let r2 = H1 { e: E.A(mk(2)) }.out(); println(f"  got{r2.id}");
    println("out_iflet/local"); let a3 = H1 { e: E.A(mk(3)) }; let r3 = a3.out_iflet(); println(f"  got{r3.id}");
    println("out_tail/local"); let a4 = H1 { e: E.A(mk(4)) }; let r4 = a4.out_tail(); println(f"  got{r4.id}");
    println("out_some/taken"); let a5 = H1 { e: E.A(mk(5)) }; let r5 = a5.out_some(true); println(f"  got{r5.id}");
    println("out_some/not"); let a6 = H1 { e: E.A(mk(6)) }; let r6 = a6.out_some(false); println(f"  got{r6.id}");
    println("read/local"); let a7 = H1 { e: E.A(mk(7)) }; let x7 = a7.read(); println(f"  r{x7}");
    println("out2/local"); let a8 = H2 { s: S { e: E.A(mk(8)) } }; let r8 = a8.out2(); println(f"  got{r8.id}");
    println("p_out/local"); let a9 = H1 { e: E.A(mk(9)) }; let r9 = p_out(a9); println(f"  got{r9.id}");
    println("p_out/temp"); let r10 = p_out(H1 { e: E.A(mk(10)) }); println(f"  got{r10.id}");
    println("p_out2/local"); let a11 = H2 { s: S { e: E.A(mk(11)) } }; let r11 = p_out2(a11); println(f"  got{r11.id}");
    println("p_read/local"); let a12 = H1 { e: E.A(mk(12)) }; let x12 = p_read(a12); println(f"  r{x12}");
    println("e_out/local"); let a13 = E.A(mk(13)); let r13 = e_out(a13); println(f"  got{r13.id}");
    println("end");
}
"#),
        r#"out/local
  dE
  got1
  dR1
out/temp
  got2
  dR2
out_iflet/local
  dE
  got3
  dR3
out_tail/local
  dE
  got4
  dR4
out_some/taken
  dE
  got5
  dR5
out_some/not
  dE
  got1
  dR1
read/local
  dE
  dR7
  r7
out2/local
  dE
  got8
  dR8
p_out/local
  dE
  got9
  dR9
p_out/temp
  dE
  got10
  dR10
p_out2/local
  dE
  got11
  dR11
p_read/local
  dE
  dR12
  r12
e_out/local
  dE
  got13
  dR13
end
"#
    );
}

/// B-2026-09-07-13 — the interpreter was correct on every cell of this row; the
/// twin holds the compiled string to it.
///
/// The defect was caller-side only and codegen-only: a CALL-produced enum
/// argument that the callee stores into an outliving place kept a second `Drop`
/// owner in the caller, so six of the eight cells printed their body twice on
/// every compiled surface. `--interp` printed each once, which is what this pins
/// — and pinning it here is what makes a future regression on either backend a
/// visible DIVERGENCE rather than a silently agreed-upon wrong answer.
///
/// Twin of `tests/codegen.rs`'s
/// `test_e2e_stored_enum_argument_is_owned_by_its_new_home_not_the_caller`,
/// pinned to the same string.
#[test]
fn test_stored_enum_argument_is_owned_by_its_new_home_not_the_caller() {
    assert_eq!(
        run(r#"shared struct In2 { v: i64 }
enum Ev { A(i64, In2), B }
impl Drop for Ev { fn drop(mut ref self) { println("dEv") } }
fn mke(i: i64) -> Ev { return Ev.A(i, In2 { v: i }); }

enum Es { A(String), B }
impl Drop for Es { fn drop(mut ref self) { println("dEs") } }
fn mkes(i: i64) -> Es { return Es.A(f"e{i}"); }
impl Es { fn is_a(ref self) -> i64 { match self { Es.A(s) => { return 1; } Es.B => { return 0; } } } }

struct BoxE { mut xs: Vec[Ev] }
impl BoxE {
    fn put(mut ref self, e: Ev) { self.xs.push(e); }
    fn stash(b: mut ref BoxE, e: Ev) { b.xs.push(e); }
}
struct BoxS { mut ys: Vec[Es] }
impl BoxS { fn puts(mut ref self, e: Es) { self.ys.push(e); } }

fn pute(b: mut ref BoxS, e: Es) { b.ys.push(e); }
fn stashg[T](v: mut ref Vec[T], x: T) { v.push(x); }
fn passe(e: Es) -> Es { return e; }

fn c_meth()   { let mut b = BoxE { xs: Vec.new() }; b.put(mke(60)); println(f"a{b.xs.len()}"); }
fn c_ctor()   { let mut b = BoxE { xs: Vec.new() }; b.put(Ev.A(77, In2 { v: 1 })); println(f"b{b.xs.len()}"); }
fn c_meths()  { let mut d = BoxS { ys: Vec.new() }; d.puts(mkes(61)); println(f"c{d.ys.len()}"); }
fn c_free()   { let mut d = BoxS { ys: Vec.new() }; pute(mut d, mkes(76)); println(f"d{d.ys.len()}"); }
fn c_assoc()  { let mut b = BoxE { xs: Vec.new() }; BoxE.stash(mut b, mke(78)); println(f"e{b.xs.len()}"); }
fn c_generic(){ let mut v: Vec[Es] = Vec.new(); stashg(mut v, mkes(75)); println(f"f{v.len()}"); }
fn c_ret()    { let z = passe(mkes(71)); println(f"g{z.is_a()}"); }
fn c_plain()  { let e = mkes(79); println(f"h{e.is_a()}"); }

fn main() {
    c_meth(); c_ctor(); c_meths(); c_free(); c_assoc(); c_generic(); c_ret(); c_plain();
    println("end");
}
"#),
        "a1\ndEv\nb1\ndEv\nc1\ndEs\nd1\ndEs\ne1\ndEv\nf1\ndEs\ng1\ndEs\nh1\ndEs\nend\n"
    );
}

/// B-2026-08-31-43 — a `match` / `if let` / `let … else` / `while let` over a
/// PROJECTION off an OWNED `self` receiver (`match self.e { E.A(r) => { let m = r;
/// .. } }`, one and two hops) ran the payload's `Drop` body twice on every surface:
/// the owned-param-root walk in each backend (codegen's
/// `scrutinee_is_owned_param_binding`, the interpreter's `place_root_is_owned_param`)
/// stopped at `ExprKind::Identifier`, `self` is `ExprKind::SelfValue`, so the arm's
/// binding was never a view of the caller-retained value and took a body beside the
/// caller's walk. A named by-value param in the same position (`p_take`, `p_iflet`)
/// was one body throughout — the control.
///
/// The fresh-temp receiver (`take/temp`, `viacall/temp`, `iflet/temp`) had been
/// losing the enum shell's `dE` all along, because the B-2026-09-04-30 gate
/// declined to retain a temp receiver's bodies caller-side for any method that
/// binds a part of `self` out; a projection scrutinee no longer counts as one. The
/// `read` cells are the read-only arms (`read2/local` was a compiled-only double at
/// two hops); `borrowed/local` is `mut ref self`, where the second body is the
/// documented copy (design.md "A projection off a borrow is an implicit copy") and
/// must stay at two.
///
/// Twin of `tests/codegen.rs`'s `e2e_owned_self_projection_scrutinee_runs_one_payload_body`, pinned to the same string.
#[test]
fn test_owned_self_projection_scrutinee_runs_one_payload_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct S { e: E }
struct H1 { e: E }
struct H2 { s: S }
fn consume(x: R) -> i64 { return x.id }

impl H1 {
    fn take(self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn read(self) -> i64 { match self.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn viacall(self) -> i64 { match self.e { E.A(r) => { return consume(r); } E.B => { return 0; } } }
    fn iflet(self) -> i64 { if let E.A(r) = self.e { let m = r; return m.id; } else { return 0; } }
    fn letelse(self) -> i64 { let E.A(r) = self.e else { return 0; }; let m = r; return m.id; }
    fn whilelet(self) -> i64 { while let E.A(r) = self.e { let m = r; return m.id; } return 0; }
    fn borrowed(mut ref self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
}
impl H2 {
    fn take2(self) -> i64 { match self.s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
    fn read2(self) -> i64 { match self.s.e { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
fn p_take(h: H1) -> i64 { match h.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } }
fn p_iflet(h: H1) -> i64 { if let E.A(r) = h.e { let m = r; return m.id; } else { return 0; } }

fn main() {
    println("take/local"); let a1 = H1 { e: E.A(mk(1)) }; let x1 = a1.take(); println(f"  r{x1}");
    println("take/temp"); let x2 = H1 { e: E.A(mk(2)) }.take(); println(f"  r{x2}");
    println("take2/local"); let a3 = H2 { s: S { e: E.A(mk(3)) } }; let x3 = a3.take2(); println(f"  r{x3}");
    println("read/local"); let a4 = H1 { e: E.A(mk(4)) }; let x4 = a4.read(); println(f"  r{x4}");
    println("read2/local"); let a5 = H2 { s: S { e: E.A(mk(5)) } }; let x5 = a5.read2(); println(f"  r{x5}");
    println("viacall/local"); let a6 = H1 { e: E.A(mk(6)) }; let x6 = a6.viacall(); println(f"  r{x6}");
    println("viacall/temp"); let x7 = H1 { e: E.A(mk(7)) }.viacall(); println(f"  r{x7}");
    println("iflet/local"); let a8 = H1 { e: E.A(mk(8)) }; let x8 = a8.iflet(); println(f"  r{x8}");
    println("iflet/temp"); let x9 = H1 { e: E.A(mk(9)) }.iflet(); println(f"  r{x9}");
    println("letelse/local"); let a10 = H1 { e: E.A(mk(10)) }; let x10 = a10.letelse(); println(f"  r{x10}");
    println("whilelet/local"); let a11 = H1 { e: E.A(mk(11)) }; let x11 = a11.whilelet(); println(f"  r{x11}");
    println("borrowed/local"); let mut a12 = H1 { e: E.A(mk(12)) }; let x12 = a12.borrowed(); println(f"  r{x12}");
    println("p_take/local"); let a13 = H1 { e: E.A(mk(13)) }; let x13 = p_take(a13); println(f"  r{x13}");
    println("p_iflet/local"); let a14 = H1 { e: E.A(mk(14)) }; let x14 = p_iflet(a14); println(f"  r{x14}");
    println("end");
}
"#),
        r#"take/local
  dE
  dR1
  r1
take/temp
  dE
  dR2
  r2
take2/local
  dE
  dR3
  r3
read/local
  dE
  dR4
  r4
read2/local
  dE
  dR5
  r5
viacall/local
  dE
  dR6
  r6
viacall/temp
  dE
  dR7
  r7
iflet/local
  dE
  dR8
  r8
iflet/temp
  dE
  dR9
  r9
letelse/local
  dE
  dR10
  r10
whilelet/local
  dE
  dR11
  r11
borrowed/local
  dR12
  dE
  dR12
  r12
p_take/local
  dE
  dR13
  r13
p_iflet/local
  dE
  dR14
  r14
end
"#
    );
}

/// B-2026-09-06-36 — a `match` / `if let` over a LOCAL struct scrutinee whose
/// ENUM leaf the arm never consumes lost that leaf's `Drop` body on every
/// compiled backend: `let c = H1 { e: E.A(mk(1)) }; match c { H1 { e } => { .. } }`
/// with `e` untouched printed `m` alone under `karac run` / `karac build` /
/// `KARAC_AUTO_PAR=0`, against `m dE dR1` on `--interp`. Memory was balanced
/// throughout — a lost BODY, not a leak.
///
/// `disarm_arm_destructured_struct_field_bodies` masked a field's bodies
/// whenever its sub-pattern was a bare binding, without asking whether the arm
/// used it. That mask is a HANDOVER — its own doc says it exists so a moved-out
/// field is not walked twice — and with nothing moved out there is nobody to
/// hand to.
///
/// The repair registers a bodies-only walker on the BINDING rather than
/// declining the mask, because the MEMORY half beside it has already given the
/// binding the field's heap ("the binding owns the field's entire heap
/// subtree"). Leaving the body with the source ran it over the husk the
/// cap-zeroing left: measured `dR0` where `dR34` was due — the exact symptom
/// the disarm's own doc records. Body and memory stay with one owner.
///
/// TWO EXCLUSIONS, each established by a measured double rather than by
/// argument, and each pinned by a cell here:
///   * an owned-PARAM scrutinee (`by_value_param`, and the `self`-receiver) —
///     the leaf is a view of the callee's entry copy whose body the CALLER runs
///     (caller-retains). Registering here too gave `dE dR7 dE dR7`.
///   * a STRUCT leaf (`struct_leaf`) — already covered by the source's own
///     field-bodies walker; registering gave `s dR8 dR8`. The row scoped itself
///     to an ENUM leaf and listed the struct leaf as NOT MEASURED. This is that
///     measurement, and it says leave it alone.
///
/// `let … else` is deliberately untouched: its binding escapes into the
/// enclosing block, so there is no scope to classify it against, and it keeps
/// today's mask — the same `scope: None` convention the interpreter's twin
/// states.
///
/// CELLS: `unread` (the row's own shape), `bound_result` (match result bound
/// rather than discarded — the row notes the discard/bound distinction is not
/// the discriminator), `iflet` (the `if let` spelling the row lists as NOT
/// MEASURED, which diverged identically), `read_only` (leaf bound beside a
/// scalar the arm returns), `consumed` (the arm hands the leaf to a call, which under
/// caller-retains is NOT a transfer, so the binding still owes the body —
/// the cell is named for its shape, not for a handover), `by_value_param` and
/// `struct_leaf` (the two
/// exclusions), `wildcard` (`e: _`, which never masked and was always right).
///
/// All four surfaces — `--interp`, `karac run`, `KARAC_AUTO_PAR=0` and the
/// default auto-par build — now print this string byte-identically, so the
/// twinned pair is pinned to ONE expected output rather than two.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_unconsumed_enum_leaf_of_a_local_struct_scrutinee_runs_its_body`, pinned
/// to the same string.
#[test]
fn test_unconsumed_enum_leaf_of_a_local_struct_scrutinee_runs_its_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct H1 { e: E }
struct H2 { r: R }
struct H3 { e: E, k: i64 }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
fn eat(e: E) -> i64 { match e { E.A(r) => { return r.id; } E.B => { return 0; } } }

fn unread() { let c: H1 = H1 { e: E.A(mk(1)) }; match c { H1 { e } => { println("  m"); } } }
fn bound_result() -> i64 { let c: H1 = H1 { e: E.A(mk(2)) }; let k: i64 = match c { H1 { e } => { 9 } }; return k; }
fn iflet() { let c: H1 = H1 { e: E.A(mk(3)) }; if let H1 { e } = c { println("  i"); } }
fn read_only() -> i64 { let c: H3 = H3 { e: E.A(mk(4)), k: 5 }; match c { H3 { e, k } => { return k; } } }
fn consumed() -> i64 { let c: H1 = H1 { e: E.A(mk(6)) }; match c { H1 { e } => { return eat(e); } } }
fn by_value_param(h: H1) -> i64 { match h { H1 { e } => { return 9; } } }
fn struct_leaf() { let c: H2 = H2 { r: mk(8) }; match c { H2 { r } => { println("  s"); } } }
fn wildcard() { let c: H1 = H1 { e: E.A(mk(9)) }; match c { H1 { e: _ } => { println("  w"); } } }

fn main() {
    println("unread"); unread();
    println("bound_result"); let a: i64 = bound_result(); println(f"  ={a}");
    println("iflet"); iflet();
    println("read_only"); let b: i64 = read_only(); println(f"  ={b}");
    println("consumed"); let c2: i64 = consumed(); println(f"  ={c2}");
    println("by_value_param"); let d: i64 = by_value_param(H1 { e: E.A(mk(7)) }); println(f"  ={d}");
    println("struct_leaf"); struct_leaf();
    println("wildcard"); wildcard();
    println("end");
}
"#),
        r#"unread
  m
  dE
  dR1
bound_result
  dE
  dR2
  =9
iflet
  i
  dE
  dR3
read_only
  dE
  dR4
  =5
consumed
  dE
  dR6
  =6
by_value_param
  dE
  dR7
  =9
struct_leaf
  s
  dR8
wildcard
  w
  dE
  dR9
end
"#
    );
}

/// B-2026-09-06-38 — a FRESH-TEMP owned ENUM receiver lost the enum SHELL's own
/// `Drop` body on every surface: `E.A(mk(2)).m_read()` printed `dR2 x2` and never
/// `dE`, where the named local `let a = E.A(mk(1)); a.m_read()` printed `dR1 dE x1`.
/// B-2026-09-04-30's receiver-temp registrar kept the value-enum arm memory-only
/// (B-2026-08-01-5's reasoning: a ref-self method binding the payload fired the
/// interpreter's arm channel, so a walk here would double it). That covered the
/// PAYLOAD; the shell's own body has no arm to fire from and had no owner for a
/// temp. Measured wider, a `ref self` temp (`E.A(mk(6)).m_ref()`) fired NOTHING —
/// neither payload nor shell — on all four surfaces, the arm channel having stood
/// down on a borrowed receiver since B-2026-08-28-67's read-through gate.
///
/// Both registrars now give an enum receiver temp its bodies at the statement's
/// end, in two shapes: a `ref self` / `mut ref self` method BORROWED the temp, so
/// the caller owns the whole value — shell body, then the payload walk (`ref/temp`
/// `dE dR6 x6`, the order `ref/local` prints; `refnoshell/temp` `dR13`); an owned
/// `self` CONSUMED it and the arm channel runs the payload (B-2026-09-06-27, -37),
/// so the caller registers the shell's body ALONE (`read/temp` `dR2 dE x2`,
/// `print/temp` `p5 dR5 dE`, `iflet/temp` `dR7 dE x7`). The owned gate is
/// `owned_self_return_cannot_carry_receiver`, the enum-specific form of the struct
/// arm's opacity gate: it declines only a return that can carry the WHOLE receiver
/// (`-> E`, `-> Self`, `-> W { e: E }`, `-> Option[E]`), the one shape whose result
/// binding would run the shell body a second time — `me/temp`, `wrap/temp`,
/// `optself/temp` keep their single `dE` — and admits a payload hand-back (`-> R`,
/// `-> Option[R]`), which doubles nothing: `r/temp` `dE y4 dR4` now matches
/// `r/local` `dE y3 dR3`. Memory is untouched (the new registrations are
/// bodies-only fns behind the unchanged `track_enum_var` free). The chain link
/// (`E.A(mk(11)).me().m_read()`, `chain/temp`) stays shell-less, the struct side's
/// recorded residual; `unit/temp` (`E.B.m_read()`) was already right.
///
/// B-2026-09-06-39 — REPINNED. `read/*`, `print/temp` and `iflet/temp` now print
/// the shell's body before the payload's (`dE dR1` for `dR1 dE`), the design.md
/// § Part 8 order: a read-only bare-`self` arm over an enum with its own `Drop`
/// binds VIEWS now and the caller owns the payload's body. `r/*` (the arm hands
/// the payload back) and `chain/temp` are unchanged — the first because the arm
/// really does take it, the second because the fresh-temp registrar was widened
/// to admit a chain-link receiver for that walk rather than lose it.
///
/// Twin of `tests/codegen.rs`'s `e2e_fresh_temp_owned_enum_receiver_runs_the_shell_body`, pinned to the same string.
#[test]
fn test_fresh_temp_owned_enum_receiver_runs_the_shell_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }
enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
struct W { e: E }
enum F { A(R), B }
impl E {
    fn m_read(self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_r(self) -> R { match self { E.A(r) => { return r; } E.B => { return mk(0); } } }
    fn m_print(self) { match self { E.A(r) => { println(f"  p{r.id}"); } E.B => { println("  pB"); } } }
    fn m_ref(ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
    fn m_iflet(self) -> i64 { if let E.A(r) = self { return r.id; } else { return 0; } }
    fn me(self) -> E { return self; }
    fn wrap(self) -> W { return W { e: self }; }
    fn m_opt(self) -> Option[R] { match self { E.A(r) => { return Some(r); } E.B => { return None; } } }
    fn m_optself(self) -> Option[E] { return Some(self); }
    fn m_mut(mut ref self) -> i64 { match self { E.A(r) => { return r.id; } E.B => { return 0; } } }
}
impl F {
    fn m_read(self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
    fn m_ref(ref self) -> i64 { match self { F.A(r) => { return r.id; } F.B => { return 0; } } }
}
fn main() {
    println("read/local"); let a = E.A(mk(1)); let x = a.m_read(); println(f"  x{x}");
    println("read/temp"); let x2 = E.A(mk(2)).m_read(); println(f"  x{x2}");
    println("r/local"); let b = E.A(mk(3)); let y = b.m_r(); println(f"  y{y.id}");
    println("r/temp"); let y2 = E.A(mk(4)).m_r(); println(f"  y{y2.id}");
    println("print/temp"); E.A(mk(5)).m_print(); println("  after");
    println("ref/temp"); let x6 = E.A(mk(6)).m_ref(); println(f"  x{x6}");
    println("iflet/temp"); let x7 = E.A(mk(7)).m_iflet(); println(f"  x{x7}");
    println("unit/temp"); let x8 = E.B.m_read(); println(f"  x{x8}");
    println("noshell/temp"); let x9 = F.A(mk(8)).m_read(); println(f"  x{x9}");
    println("me/temp"); let e = E.A(mk(9)).me(); println("  held");
    println("wrap/temp"); let w = E.A(mk(10)).wrap(); println("  held");
    println("chain/temp"); let x11 = E.A(mk(11)).me().m_read(); println(f"  x{x11}");
    println("ref/local"); let c = E.A(mk(12)); let x12 = c.m_ref(); println(f"  x{x12}");
    println("refnoshell/temp"); let x13 = F.A(mk(13)).m_ref(); println(f"  x{x13}");
    println("refnoshell/local"); let d = F.A(mk(14)); let x14 = d.m_ref(); println(f"  x{x14}");
    println("opt/temp"); let o16 = E.A(mk(16)).m_opt(); println("  held");
    println("optself/temp"); let o17 = E.A(mk(17)).m_optself(); println("  held");
    println("mut/temp"); let x18 = E.A(mk(18)).m_mut(); println(f"  x{x18}");
    println("end");
}
"#),
        r#"read/local
  dE
  dR1
  x1
read/temp
  dE
  dR2
  x2
r/local
  dE
  y3
  dR3
r/temp
  dE
  y4
  dR4
print/temp
  p5
  dE
  dR5
  after
ref/temp
  dE
  dR6
  x6
iflet/temp
  dE
  dR7
  x7
unit/temp
  dE
  x0
noshell/temp
  dR8
  x8
me/temp
  dE
  dR9
  held
wrap/temp
  dE
  dR10
  held
chain/temp
  dR11
  x11
ref/local
  dE
  dR12
  x12
refnoshell/temp
  dR13
  x13
refnoshell/local
  dR14
  x14
opt/temp
  dE
  dR16
  held
optself/temp
  dE
  dR17
  held
mut/temp
  dE
  dR18
  x18
end
"#
    );
}

#[test]
fn test_deep_projection_scrutinee_runs_one_payload_body() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               enum E { A(R), B }\n\
               impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
               struct S { e: E }\n\
               struct W { s: S }\n\
               struct X { w: W }\n\
               struct Sd { e: E }\n\
               impl Drop for Sd { fn drop(mut ref self) { println(\"dSd\") } }\n\
               struct Wd { s: Sd }\n\
               struct S2 { n: i64, e: E }\n\
               struct W2 { k: R, s: S2 }\n\
               struct Coll { e: E, s: S }\n\
               struct Two { a: S, b: S }\n";
    for (label, body, want) in [
        (
            "control: one hop",
            "let s = S { e: E.A(R { id: 8 }) };\n\
             match s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndE\n",
        ),
        (
            "two hops — the row",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
             match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndE\n",
        ),
        (
            "three hops",
            "let x = X { w: W { s: S { e: E.A(R { id: 8 }) } } };\n\
             match x.w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndE\n",
        ),
        (
            "two hops, intermediate struct has its OWN Drop",
            "let w = Wd { s: Sd { e: E.A(R { id: 8 }) } };\n\
             match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndSd\ndE\n",
        ),
        (
            "two hops, sibling fields still walked",
            "let w = W2 { k: R { id: 3 }, s: S2 { n: 1, e: E.A(R { id: 8 }) } };\n\
             match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndE\ndR3\n",
        ),
        (
            "two hops, `if let` spelling",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
             if let E.A(r) = w.s.e { let m = r; println(f\"m{m.id}\") }",
            "m8\ndR8\ndE\n",
        ),
        (
            "two hops, a statement follows",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
             match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }\n\
             println(\"after\");",
            "m8\ndR8\ndE\nafter\n",
        ),
        // CONTROLS the mask must keep DECLINING. A non-consuming arm did
        // not take the payload, so the owner still owes that body; the
        // `B` arm holds no payload at all.
        (
            "control: two hops, arm does NOT consume",
            "let w = W { s: S { e: E.A(R { id: 8 }) } };\n\
             match w.s.e { E.A(r) => { println(f\"n{r.id}\") } E.B => {} }",
            "n8\ndE\ndR8\n",
        ),
        (
            "control: two hops, payload-less arm taken",
            "let w = W { s: S { e: E.B } };\n\
             match w.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => { println(\"bee\") } }",
            "bee\ndE\n",
        ),
        // COMPOSITION. A path mask must land on the level that owns the
        // enum and nowhere else — these are the rows that fail if the
        // nesting is built or consumed at the wrong depth.
        (
            "name collision: masking the INNER `e` leaves the outer's payload",
            "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
             match c.s.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m8\ndR8\ndE\ndE\ndR1\n",
        ),
        (
            "name collision: masking the OUTER `e` leaves the inner's payload",
            "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
             match c.e { E.A(r) => { let m = r; println(f\"m{m.id}\") } E.B => {} }",
            "m1\ndR1\ndE\ndR8\ndE\n",
        ),
        (
            "compose: a one-hop and a two-hop mask on the same variable",
            "let c = Coll { e: E.A(R { id: 1 }), s: S { e: E.A(R { id: 8 }) } };\n\
             match c.e { E.A(r) => { let m = r; println(f\"o{m.id}\") } E.B => {} }\n\
             match c.s.e { E.A(r) => { let m = r; println(f\"i{m.id}\") } E.B => {} }",
            "o1\ndR1\ni8\ndR8\ndE\ndE\n",
        ),
        (
            "compose: both sibling subtrees masked",
            "let t = Two { a: S { e: E.A(R { id: 1 }) }, b: S { e: E.A(R { id: 2 }) } };\n\
             match t.a.e { E.A(r) => { let m = r; println(f\"a{m.id}\") } E.B => {} }\n\
             match t.b.e { E.A(r) => { let m = r; println(f\"b{m.id}\") } E.B => {} }",
            "a1\ndR1\nb2\ndR2\ndE\ndE\n",
        ),
        (
            "compose: only ONE sibling subtree masked",
            "let t = Two { a: S { e: E.A(R { id: 1 }) }, b: S { e: E.A(R { id: 2 }) } };\n\
             match t.a.e { E.A(r) => { let m = r; println(f\"a{m.id}\") } E.B => {} }",
            "a1\ndR1\ndE\ndR2\ndE\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{body}\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
    // A `self`-ROOTED projection scrutinee through a BORROWED receiver
    // (`mut ref self`) runs the payload's body twice, and that is the
    // documented copy, not a gap: design.md "A projection off a borrow is an
    // implicit copy, and that is a stopgap" -- the match copies `E.A(R)` out
    // of the borrow, `m` owns the copy and runs its body, and the caller's
    // receiver still owns the original and runs its own. The explicit
    // spelling `let e = h.e` through `ref h` prints `dE dR dE dR` on every
    // surface (valgrind-clean with a heap-carrying payload) and carries
    // W0299 `borrow_projection_copy` saying so. B-2026-08-31-43 first pinned
    // this as a KNOWN GAP; its fix covers the OWNED receiver (`fn take(self)`),
    // whose payload was a genuine double -- see
    // `test_owned_self_projection_scrutinee_runs_one_payload_body`. Kept at
    // the copy's transcript so a change in that stopgap fails loudly here.
    let selfrooted = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct S { e: E }\n\
         struct H1 { e: E }\n\
         impl H1 { fn take(mut ref self) -> i64 { match self.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } }\n\
         struct H2 { s: S }\n\
         impl H2 { fn take(mut ref self) -> i64 { match self.s.e { E.A(r) => { let m = r; return m.id; } E.B => { return 0; } } } }\n\
         fn main() {\n\
         \x20   let mut a = H1 { e: E.A(R { id: 1 }) };\n\
         \x20   println(a.take());\n\
         \x20   let mut b = H2 { s: S { e: E.A(R { id: 2 }) } };\n\
         \x20   println(b.take());\n\
         }\n";
    assert_eq!(
        run(selfrooted),
        "dR1\n1\ndE\ndR1\ndR2\n2\ndE\ndR2\n",
        "[borrowed receiver: the projection copies, one hop and two]"
    );
}

/// B-2026-09-12-17 / B-2026-09-13-24 — a GENERIC user enum's payload `Drop` body
/// runs for a fresh-temp constructor argument, and the axis is GENERICITY rather
/// than "user enum vs the seeded pair".
///
/// The name-keyed walker `emit_enum_payload_user_drop_bodies_fn` skips a payload
/// declared as one of the enum's own generic params (B-2026-08-03-5's guard), so
/// for `enum Ho[T] { Full(T) }` every slot was skipped, it returned `None`, and a
/// fresh temp — which has no `let` site to fall back on — had no owner for its
/// payload body at all. The instantiation-keyed walker
/// `emit_generic_enum_payload_user_drop_bodies_fn` (B-2026-09-10-2) is now tried
/// when the name-keyed one declines.
///
/// CELLS 4-9 ARE THE CONTROLS THAT MAKE THE FIX A PARTITION RATHER THAN A
/// WIDENING. The monomorphic enum reaches the name-keyed walker and the seeded
/// `Option` reaches its own instantiation-keyed gate; both were already correct,
/// and neither may now DOUBLE. The generic named-local cell is correct through its
/// `let` site, which is the asymmetry that localised this in the first place, and
/// the payload-less variant pins that the tag switch still selects nothing to run.
///
/// INLINE PAYLOADS ONLY, and cells 10-12 are why. When the instantiated payload
/// outgrows the erased one-word payload area it is heap-BOXED, and the box's own
/// interior drop already runs the body (B-2026-09-10-2) -- so registering here as
/// well gives it TWO owners. The first pass at this fix did exactly that and
/// doubled the body for a three-`String` payload, caught by
/// `asan_generic_enum_payload_runs_its_drop_and_frees_its_interior` rather than by
/// anything here: every cell in this table used a ONE-WORD payload, so the table
/// could not see the boxing axis at all. Cell 10 is that shape, pinned.
///
/// ASAN STAYED CLEAN THROUGH THAT REGRESSION -- a duplicated body is not a double
/// free -- so only an output comparison catches this class. That is the argument
/// for pinning cell 10 here rather than relying on the sanitizer suite.
///
/// CELL 3 WAS THE PINNED GAP AND IS NOW FIXED (B-2026-09-13-24). The spelling
/// qualified WITH generic args (`Ho[R].Full(..)`) parses as a `MethodCall` whose
/// receiver is a one-segment path carrying generic args, and neither fresh-temp
/// registrar had an arm for that node: `enum_name_of_expr` has none, and the
/// interpreter's `fresh_temp_arg_type_name` had none either. Both gained one, in
/// the same commit, because the cell was AGREED-silent and repairing either
/// backend alone would have converted an agreed gap into a fresh divergence —
/// which is precisely what the pin was here to catch, and it did: the codegen
/// half alone turned this cell red.
///
/// CELL 13 IS THE BOXED TWIN OF CELL 3, and measuring it corrected the row. The
/// "runs on NO surface" framing is true only for the INLINE payload: with a
/// three-`String` payload the box's own interior drop already carried the body on
/// the compiled backends, so `holdw(Ho[W].Full(mkw()))` was `hw / end` on the
/// interpreter against `hw / dW4 / end` compiled — a live run-vs-build divergence,
/// not an agreed gap. The interpreter arm repairs it, and the cell pins that the
/// codegen arm did NOT add a second owner on top of the box route (the doubling
/// that cells 10-12 exist for).
///
/// CELL 14 IS THE OVER-CLAIM CONTROL. `Ho[R].mk(R { id: 7 })` — an associated
/// function on the same generic enum — has the IDENTICAL node shape to cell 3 and
/// must not be claimed by either registrar, since its result is owned by whatever
/// the callee returns. Both predicates ask `qualified_enum_variant_is_unit`, which
/// answers `None` for a name that is not a variant, and this cell is what holds
/// that distinction in place.
///
/// B-2026-09-25-16 FLIPPED ITS PIN. It read `dR7 f:7 end`: the body ran INSIDE
/// `mk`, before the payload was used, because `mk` dropped its own param instead
/// of handing it back through `Ho.Full(v)`. Once `mk` hands it back the result is
/// a fresh temp with no owner, so both registrars now claim it as a CALL RESULT
/// (the `Ho.mk(..)` twin's route) -- still not as a constructor.
///
/// Twin in the other backend's suite under the same name, same table.
#[test]
fn test_generic_enum_ctor_temp_arg_runs_its_payload_drop_body() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               struct W { a: String, b: String, c: String }\n\
               impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a.len()}\") } }\n\
               fn mkw() -> W { return W { a: f\"aaa{1}\", b: f\"bbb{1}\", c: f\"ccc{1}\" }; }\n\
               enum Ho[T] { Full(T), Empty }\n\
                   impl[T] Ho[T] { fn mk(v: T) -> Ho[T] { return Ho.Full(v); } }\n\
               enum Mo { Whole(R), Nil }\n\
               enum Bo { Wide(W), Nil }\n\
               fn takeit(x: Ho[R]) { match x { Full(r) => { println(f\"f:{r.id}\") } Empty => { println(\"e\") } } }\n\
               fn takemo(x: Mo) { match x { Whole(r) => { println(f\"f:{r.id}\") } Nil => { println(\"e\") } } }\n\
               fn taken(o: Option[R]) { match o { Some(r) => { println(f\"f:{r.id}\") } None => { println(\"e\") } } }\n\
               fn takebo(x: Bo) { match x { Wide(r) => { println(f\"w:{r.a.len()}\") } Nil => { println(\"e\") } } }\n\
               fn holdw(x: Ho[W]) { println(\"hw\"); }\n\
               fn holdbo(x: Bo) { println(\"hb\"); }\n";
    for (label, stmts, want) in [
        (
            "generic enum, bare ctor temp",
            "takeit(Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "generic enum, qualified-without-args ctor temp",
            "takeit(Ho.Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "generic enum, ctor qualified WITH generic args",
            "takeit(Ho[R].Full(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: monomorphic enum, bare ctor temp",
            "takemo(Whole(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: monomorphic enum, qualified ctor temp",
            "takemo(Mo.Whole(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: the seeded Option oracle",
            "taken(Some(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: the seeded Option oracle, qualified",
            "taken(Option[R].Some(R { id: 5 }));",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: generic enum through a NAMED LOCAL",
            "let h: Ho[R] = Full(R { id: 5 });\n\
             takeit(h);",
            "f:5\ndR5\nend\n",
        ),
        (
            "control: generic enum, payload-less variant",
            "takeit(Empty);",
            "e\nend\n",
        ),
        (
            "REGRESSION GUARD: boxed generic payload, callee does not bind it",
            "holdw(Ho.Full(mkw()));",
            "hw\ndW4\nend\n",
        ),
        (
            "control: boxed payload in a MONOMORPHIC enum, callee does not bind",
            "holdbo(Bo.Wide(mkw()));",
            "hb\ndW4\nend\n",
        ),
        (
            "control: boxed payload in a MONOMORPHIC enum, arm binds it",
            "takebo(Bo.Wide(mkw()));",
            "w:4\ndW4\nend\n",
        ),
        (
            "the BOXED twin of cell 3 — was interp-vs-compiled divergent",
            "holdw(Ho[W].Full(mkw()));",
            "hw\ndW4\nend\n",
        ),
        (
            "control: an ASSOC FN with the same node shape is not a ctor",
            "takeit(Ho[R].mk(R { id: 7 }));",
            "f:7\ndR7\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-14-22 — a GENERIC enum's BOXED payload keeps its `Drop` body when the
/// consuming arm BINDS that payload read-only.
///
/// `suppress_destructured_enum_payload_cleanup` masks the scrutinee's payload-BODIES
/// walk for every position the arm consumes, on the premise that the arm's binding
/// now owns the body. For a GENERIC payload behind a BY-VALUE PARAM scrutinee that
/// premise fails on both halves at once: the instantiation-keyed walker
/// `__karac_dropelems_genum_<te>` is the only bodies channel a generic payload has
/// (the name-keyed one skips a payload declared as the enum's own parameter —
/// B-2026-08-03-5's guard), and a param scrutinee's arm binds a VIEW whose drop is
/// the memory-only `__karac_drop_struct_<T>`. So the mask retracted the one channel
/// and handed the body to a binding that runs none: `w:4 / end` compiled against
/// `w:4 / dW4 / end` on the interpreter, a live run-vs-build divergence.
///
/// The gate is THREE predicates and every one of them earns its place, each pinned
/// by a cell that went wrong when it was missing:
///
/// * read-only arm (`binding_only_borrowed`) — cell 10 is the consuming arm, still a
///   gap and pinned as one below.
/// * generic payload (`arm_consumes_only_generic_payload`) — cells 4-5 and 8 are the
///   MONOMORPHIC twin, which has a SECOND channel in the enum's own
///   `__karac_drop_<E>` switch; skipping the mask there DOUBLES the body.
/// * param scrutinee (`scrutinee_is_owned_param_binding`) — cells 6-7 match a NAMED
///   LOCAL, whose arm binding gets its own body-running `karac_drop_<T>`; skipping
///   the mask there printed `w:4 / dW4 / dW4 / end`, measured.
///
/// AND THE MEMORY HALF OF THE SAME BLOCK IS NOT GATED. `clear_boxed_enum_inner_drop`
/// is what stops the box drop and the binding from both freeing a boxed payload; the
/// first cut of this fix skipped the whole block and turned cells 6-7 into
/// `free(): double free detected in tcache 2`, exit 134. Only the BODIES mask is
/// gated — hence the inner `if` rather than a condition on the outer one.
///
/// INLINE PAYLOADS ARE A DIFFERENT CELL ALREADY GREEN, which is why this row is
/// separate from `(test|e2e)_generic_enum_ctor_temp_arg_runs_its_payload_drop_body`:
/// that table's one-word `R` payload rides the inline route and was never masked.
/// Three `String`s (9 words) outgrow the erased one-word payload area and heap-box,
/// which is the whole of this row.
///
/// CELL 9 IS THE TWO-PARAMETER DECLARATION (`enum Ro[T, E]`), which the row listed as
/// NOT MEASURED: divergent before the fix, correct on all four surfaces after, so the
/// gate keys on the payload rather than on the enum's arity.
///
/// CELL 10 IS THE REMAINING GAP, split out as its own open row rather than buried
/// here: an arm that CONSUMES its binding (`let z = r`) still loses the body on every
/// compiled surface. It is pinned at the divergent value in this suite and at the
/// correct one in the interpreter's twin, deliberately, so the split is visible in
/// both tables rather than quietly absent from one.
///
/// Twin in the other backend's suite under the same name, same table.
#[test]
fn test_generic_boxed_enum_payload_body_survives_a_read_only_binding_arm() {
    let hdr = "struct W { a: String, b: String, c: String }\n\
              impl Drop for W { fn drop(mut ref self) { println(f\"dW{self.a.len()}\") } }\n\
              fn mkw() -> W { return W { a: f\"aaa{1}\", b: f\"bbb{1}\", c: f\"ccc{1}\" }; }\n\
              enum Ho[T] { Full(T), Empty }\n\
              enum Mo { Full(W), Empty }\n\
              enum Ro[T, E] { Good(T), Bad(E) }\n\
              fn taker(x: Ro[W, i64]) { match x { Good(r) => { println(f\"w:{r.a.len()}\") } Bad(n) => { println(f\"b{n}\") } } }\n\
              fn takew(x: Ho[W]) { match x { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } } }\n\
              fn holdw(x: Ho[W]) { match x { Full(_) => { println(\"w\") } Empty => { println(\"e\") } } }\n\
              fn takem(x: Mo) { match x { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } } }\n\
              fn holdm(x: Mo) { match x { Full(_) => { println(\"w\") } Empty => { println(\"e\") } } }\n\
              fn consumew(x: Ho[W]) { match x { Full(r) => { let z = r; println(f\"w:{z.a.len()}\") } Empty => { println(\"e\") } } }\n\
              fn mkho() -> Ho[W] { return Ho.Full(mkw()); }\n\
              fn mkmo() -> Mo { return Mo.Full(mkw()); }\n";
    for (label, stmts, want) in [
        (
            "generic, boxed payload, read-only binding arm, fresh ctor temp",
            "takew(Ho.Full(mkw()));",
            "w:4\ndW4\nend\n",
        ),
        (
            "generic, same arm, a named local moved into the callee",
            "let g: Ho[W] = Ho.Full(mkw());\ntakew(g);",
            "w:4\ndW4\nend\n",
        ),
        (
            "control: the arm does NOT bind (wildcard) — never masked, never lost",
            "holdw(Ho.Full(mkw()));",
            "w\ndW4\nend\n",
        ),
        (
            "control: MONOMORPHIC twin, binding arm — has a second channel, must not double",
            "takem(Mo.Full(mkw()));",
            "w:4\ndW4\nend\n",
        ),
        (
            "control: MONOMORPHIC twin, non-binding arm",
            "holdm(Mo.Full(mkw()));",
            "w\ndW4\nend\n",
        ),
        (
            "control: generic, NAMED LOCAL scrutinee from a call — arm owns, must not double",
            "let g = mkho();\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
            "w:4\ndW4\nend\n",
        ),
        (
            "control: generic, NAMED LOCAL scrutinee from a ctor",
            "let g: Ho[W] = Ho.Full(mkw());\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
            "w:4\ndW4\nend\n",
        ),
        (
            "control: MONOMORPHIC named local scrutinee from a call",
            "let g = mkmo();\nmatch g { Full(r) => { println(f\"w:{r.a.len()}\") } Empty => { println(\"e\") } }",
            "w:4\ndW4\nend\n",
        ),
        (
            "generic with TWO parameters — the `Result`-shaped declaration",
            "taker(Ro.Good(mkw()));",
            "w:4\ndW4\nend\n",
        ),
        (
            "PINNED GAP: the arm CONSUMES its binding (`let z = r`) — body lost when compiled",
            "consumew(Ho.Full(mkw()));",
            "w:4\ndW4\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-08-30-55 — a method frame owns its by-value ENUM argument.
///
/// `method_param_drop_names` admitted `Value::Struct` only, and a method frame
/// stands its caller down, so an owned enum argument had NO owner on either
/// side: `t.eat(E.A(R { .. }))` ran ZERO `Drop` bodies here — neither the
/// enum's own nor its payload's — against two on both compiled backends.
///
/// Every row is a body COUNT, and each of the four is a different answer to
/// "who owns this argument", which is the one question the fix turns on:
///
/// * `temp` — no caller binding exists, so the frame must claim it. The
///   reported hole.
/// * `named` — the caller's binding fires, so the frame must NOT claim it.
///   Claiming unconditionally was the obvious widening and it doubles this row.
/// * `named-struct` — the row recorded its struct twin as a working control.
///   That holds only for the fresh-temp spelling; this one ran `dR8` TWICE
///   before the fix, so the defect was never enum-specific — only
///   enum-visible in the shape probed.
/// * `free-fn` — the control that localizes it: the caller still fires, and
///   this spelling was correct throughout.
#[test]
fn method_frame_owns_its_by_value_enum_argument() {
    const H: &str = "struct R { id: i64, tag: String }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum E { A(R), B }\n\
         impl Drop for E { fn drop(mut ref self) { println(\"dE\") } }\n\
         struct T { n: i64 }\n\
         impl T { fn eat(ref self, b: E) -> i64 { return 3; } }\n\
         impl T { fn eats(ref self, r: R) -> i64 { return 3; } }\n\
         fn eatf(b: E) -> i64 { return 3; }\n";
    for (label, body, want) in [
        (
            "temp",
            "fn main() { let t: T = T { n: 1 };\n\
             let v: i64 = t.eat(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
        (
            "named",
            "fn main() { let t: T = T { n: 1 }; let c: E = E.A(R { id: 8, tag: f\"t8\" });\n\
             let v: i64 = t.eat(c); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
        (
            "named-struct",
            "fn main() { let t: T = T { n: 1 }; let c: R = R { id: 8, tag: f\"t8\" };\n\
             let v: i64 = t.eats(c); println(f\"v{v}\") }\n",
            "dR8\nv3\n",
        ),
        (
            "free-fn",
            "fn main() { let v: i64 = eatf(E.A(R { id: 8, tag: f\"t8\" })); println(f\"v{v}\") }\n",
            "dE\ndR8\nv3\n",
        ),
    ] {
        assert_eq!(run(&format!("{H}{body}")), want, "{label}");
    }
}

/// B-2026-08-31-47, shape 2 — the two DISCARD SPELLINGS of one call agree.
///
/// `k.f(mk(4), true);` ran no `Drop` body at all while
/// `let _ = k.f(mk(4), true);` ran one. The bare arm took the DECLARED-TYPE
/// payload walk, and `Option`/`Result` are built-ins with no source-level
/// `EnumDef`, so that walk answers nothing for them; the `let _` spelling
/// reaches the shared discard walker, which carries its own value-driven
/// `Option`/`Result` arm.
///
/// The matrix is 2x2 on purpose — (user enum, `Option`) x (bare, `let _`) —
/// because only ONE of the four cells was wrong, and a fixture covering just
/// the failing spelling would not have shown that the user-enum sibling is the
/// control rather than a second bug. The user-enum rows also guard the fix's
/// blast radius: they must keep running exactly one body, since only the
/// built-ins were rerouted.
///
/// Bodies only. The COMPILED backends run two for the escaping call here, which
/// is B-2026-08-29-38's family — over-firing, the opposite direction — and is
/// deliberately not this row's, so there is no codegen twin to share.
#[test]
fn discard_spellings_of_a_method_call_agree_on_the_payload_body() {
    let src = "struct R { id: i64, name: String }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"drop {self.id}\") } }\n\
               enum MyBox { Full(R), Empty }\n\
               struct K { n: i64 }\n\
               fn mk(i: i64) -> R { return R { id: i, name: f\"h{i}\" }; }\n\
               impl K {\n\
               \x20   fn userenum(ref self, r: R) -> MyBox { return MyBox.Full(r); }\n\
               \x20   fn opt(ref self, r: R) -> Option[R] { return Option.Some(r); }\n\
               }\n\
               fn main() {\n\
               \x20   let k: K = K { n: 1 };\n\
               \x20   k.userenum(mk(1));\n\
               \x20   let _ = k.userenum(mk(2));\n\
               \x20   k.opt(mk(3));\n\
               \x20   let _ = k.opt(mk(4));\n\
               \x20   println(\"end\");\n\
               }\n";
    assert_eq!(
        run(src),
        "drop 1\ndrop 2\ndrop 3\ndrop 4\nend\n",
        "each of the four discards owns exactly one body; `drop 3` is the cell \
         that ran none before this row — a bare-statement discard of a method \
         returning `Option`"
    );
}

/// B-2026-09-01-33 — interpreter twin of `tests/codegen.rs`'s
/// `test_e2e_producer_call_arg_runs_the_enum_payload_body`, same programs and
/// expectations.
///
/// This backend was CORRECT on every row; both compiled backends lost the
/// payload body for an enum temp returned from a call. The twin pins the
/// transcript the fix had to converge ON, and keeps the two spellings and the
/// three control positions asserted on both sides.
///
/// Note the DIRECTION: this is the opposite of B-2026-08-31-8, where the
/// interpreter was the wrong side for a neighbouring shape. That is why the
/// producer-call case was deliberately excluded from that row's fixtures --
/// pinning it then would have asserted one side of a live disagreement.
#[test]
fn producer_call_arg_runs_the_enum_payload_body() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         enum Tv { A(R), Nil }\n\
         impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
         struct Mk { z: i64 }\n\
         impl Mk { fn s(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; } }\n\
         fn eatS(v: Sv) { println(\"e\") }\n\
         fn eatT(v: Tv) { println(\"e\") }\n\
         fn mkS(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n\
         fn mkT(i: i64) -> Tv { return Tv.A(R { id: i }); }\n";
    for (label, body, want) in [
        (
            "producer fn, struct variant",
            "eatS(mkS(5))",
            "e\ndSv\ndR5\n",
        ),
        (
            "producer fn, tuple variant",
            "eatT(mkT(6))",
            "e\ndTv\ndR6\n",
        ),
        (
            "via named local, struct variant (control)",
            "let x = mkS(7); eatS(x)",
            "e\ndSv\ndR7\n",
        ),
        (
            "via named local, tuple variant (control)",
            "let y = mkT(8); eatT(y)",
            "e\ndTv\ndR8\n",
        ),
        (
            "inline constructor argument (control)",
            "eatS(Sv.Hold { inner: R { id: 9 } })",
            "e\ndSv\ndR9\n",
        ),
        ("associated producer fn", "eatS(Mk.s(10))", "e\ndSv\ndR10\n"),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }
}

/// B-2026-09-01-31 — a DISCARDED enum struct-variant literal runs its payload's
/// `Drop` body.
///
/// `let _ = Sv.Hold { inner: R { .. } }` is spelled as a struct literal, so
/// `discard_producer_runs_payload_walk` -- which had a `Call` arm for
/// `Tv.A(r)` and none for a struct literal -- answered `false`, the discard
/// took the own-`Drop`-enum branch, and the payload's body never ran. Measured
/// `dSv` against `dSv dR1` on all three compiled surfaces.
///
/// `tuple-variant` is the control that localizes it: the same discard one
/// spelling over was correct in every shape. `no-own-drop-enum` is the second
/// control, and it is the sharper one -- a struct-variant whose ENUM declares
/// no `Drop` of its own takes the payload-walk branch and was always correct,
/// so the bug needed BOTH an own-`Drop` enum and a struct-shaped variant.
///
/// `block-peel` and `no-else-if` are the wrapper positions the same predicate
/// reaches by recursion, and both compiled backends run the payload there too.
///
/// `producer-call` WAS the row a later widening would break: both backends ran
/// the enum's own body ALONE for `let _ = mk(6)`, so firing the payload here
/// would have been a fresh divergence rather than a fix.
///
/// B-2026-09-02-13 removed that premise at its source. Codegen's discard
/// registrar reached its enum payload walker only on the no-own-`Drop` leg, so
/// a CALL producing an own-`Drop` enum got the `karac_drop_<E>` wrapper alone —
/// and the wrapper's field-cleanup half is a no-op for an enum name, so the
/// payload body ran nowhere. Against the bound-local oracle (`let x = mk(6);`,
/// which prints `dSv dR6` on every surface) that was the under-drop. The
/// registrar now registers both, this row reads `dSv dR6`, and the widening it
/// used to forbid is what closed the gap.
///
/// `two-tail-if` and `match` were ABSENT here until B-2026-09-01-34 landed, and
/// their history is the point. The compiled backends used to emit NO body at
/// all for a struct-variant literal in those positions -- not even the enum's
/// own -- so this backend's `dSv` already disagreed, and widening it to
/// `dSv dR` would have disagreed at TWO bodies instead of one.
/// `discard_arm_yields_fresh_enum` carried an explicit exclusion to hold that
/// line. -34 gave codegen the owner it was missing, so the exclusion came out
/// and both rows are now asserted on both sides.
#[test]
fn discarded_enum_struct_variant_literal_runs_its_payload_body() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         enum Tv { A(R), Nil }\n\
         impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
         enum Nd { H { inner: R }, Nil }\n\
         fn mk(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n";
    for (label, body, want) in [
        (
            "struct-variant literal",
            "let _ = Sv.Hold { inner: R { id: 1 } }; println(\"d\")",
            "dSv\ndR1\nd\n",
        ),
        (
            "tuple-variant literal (control)",
            "let _ = Tv.A(R { id: 2 }); println(\"d\")",
            "dTv\ndR2\nd\n",
        ),
        (
            "no-own-drop-enum (control)",
            "let _ = Nd.H { inner: R { id: 3 } }; println(\"d\")",
            "dR3\nd\n",
        ),
        (
            "block-peel",
            "let _ = { Sv.Hold { inner: R { id: 4 } } }; println(\"d\")",
            "dSv\ndR4\nd\n",
        ),
        (
            "no-else-if",
            "let c = true; let _ = if c { Sv.Hold { inner: R { id: 5 } } }; println(\"d\")",
            "dSv\ndR5\nd\n",
        ),
        (
            // B-2026-09-02-13 — no longer "own body alone": a call producer
            // runs the payload body too, matching the bound local. Kept as
            // the producer-call control; see the codegen twin.
            "producer-call (control: a call producer, own body + payload)",
            "let _ = mk(6); println(\"d\")",
            "dSv\ndR6\nd\n",
        ),
        (
            "two-tail-if",
            "let c = true; let _ = if c { Sv.Hold { inner: R { id: 7 } } } else { Sv.Nil }; println(\"d\")",
            "dSv\ndR7\nd\n",
        ),
        (
            "match",
            "let n = 1; let _ = match n { 1 => { Sv.Hold { inner: R { id: 8 } } } _ => { Sv.Nil } }; println(\"d\")",
            "dSv\ndR8\nd\n",
        ),
        (
            "two-tail-if, tuple variant (control)",
            "let c = true; let _ = if c { Tv.A(R { id: 9 }) } else { Tv.Nil }; println(\"d\")",
            "dTv\ndR9\nd\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }

    // A heap-carrying payload, so the fix is pinned on the shape that actually
    // owns memory rather than only on a scalar one.
    assert_eq!(
        run("struct R { id: i64, xs: Vec[i64] }\n\
             impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}:{self.xs.len()}\") } }\n\
             enum Sv { Hold { inner: R }, Nil }\n\
             impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
             fn main() {\n\
             \x20   let _ = Sv.Hold { inner: R { id: 1, xs: [1, 2, 3] } };\n\
             \x20   println(\"d\")\n\
             }\n"),
        "dSv\ndR1:3\nd\n"
    );
}

/// B-2026-09-01-32 — the UNQUALIFIED enum struct-variant literal
/// `Hold { .. }` runs its `Drop` bodies.
///
/// The sibling of B-2026-08-31-8, and the spelling that row's fix deliberately
/// declined. It was wrong on ALL FOUR surfaces at once, so no A/B parity gate
/// could see it: every backend agreed, and every backend ran neither the enum's
/// own body nor its payload's. Fixing the interpreter alone would have turned
/// that agreed-but-wrong answer into a fresh run-vs-build divergence — the
/// worse of the two — which is why both halves landed together.
///
/// The two CONTROLS are what localize it to the brace spelling. The qualified
/// form `Sv.Hold { .. }` was correct (B-2026-08-31-8 for the interpreter;
/// codegen always), and the unqualified TUPLE form `A(r)` was correct on every
/// surface throughout — it parses as an `ExprKind::Call` and reaches
/// `enum_name_for_variant_ctor`, the very helper the brace arm now shares.
///
/// `struct wins over a same-named variant` pins the PRECEDENCE, which is the
/// one way this fix could have broken working programs: both the interpreter
/// and codegen check for a real struct of that name before reading the segment
/// as a variant, so an ordinary literal keeps its own drop in a program that
/// also declares a variant of the same name. Without this row a fix that
/// dropped the struct check would still pass every other row here.
///
/// The two order rows pin the SEQUENCE, not just the count: argument temps are
/// introduced left to right and pop right to left (B-2026-08-29-46), so a fix
/// that fired them forward would agree on every count and differ only here.
/// The mixed row additionally proves the two spellings share one owner
/// mechanism rather than two that happen to agree.
#[test]
fn fresh_temp_unqualified_struct_variant_arg_runs_its_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         enum Tv { A(R), Nil }\n\
         impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
         struct Dup { id: i64 }\n\
         impl Drop for Dup { fn drop(mut ref self) { println(f\"dDup{self.id}\") } }\n\
         enum Ev { Dup { inner: R }, Nil }\n\
         impl Drop for Ev { fn drop(mut ref self) { println(\"dEv\") } }\n\
         fn eat(v: Sv) { println(\"e\") }\n\
         fn eatt(v: Tv) { println(\"e\") }\n\
         fn eatd(v: Dup) { println(\"e\") }\n\
         fn eat2(v: Sv, w: Sv) { println(\"e2\") }\n\
         struct Rh { id: i64, tag: String, buf: Vec[i64] }\n\
         impl Drop for Rh { fn drop(mut ref self) { println(f\"dRh{self.id}:{self.buf.len()}\") } }\n\
         enum Svh { HoldH { inner: Rh }, Nil }\n\
         impl Drop for Svh { fn drop(mut ref self) { println(\"dSvh\") } }\n\
         fn eath(v: Svh) { println(\"e\") }\n";
    for (label, body, want) in [
        (
            "unqualified struct-variant fresh temp",
            "eat(Hold { inner: R { id: 1 } })",
            "e\ndSv\ndR1\n",
        ),
        (
            "qualified struct-variant (control, B-2026-08-31-8)",
            "eat(Sv.Hold { inner: R { id: 2 } })",
            "e\ndSv\ndR2\n",
        ),
        (
            "unqualified tuple-variant (control)",
            "eatt(A(R { id: 3 }))",
            "e\ndTv\ndR3\n",
        ),
        (
            "named-local unqualified (control)",
            "let a = Hold { inner: R { id: 4 } }; eat(a)",
            "e\ndSv\ndR4\n",
        ),
        (
            "struct wins over a same-named variant",
            "eatd(Dup { id: 7 })",
            "e\ndDup7\n",
        ),
        (
            "unqualified, HEAP-carrying payload",
            "eath(HoldH { inner: Rh { id: 8, tag: \"ab\", buf: [1, 2, 3] } })",
            "e\ndSvh\ndRh8:3\n",
        ),
        (
            "two-fresh unqualified, reverse order",
            "eat2(Hold { inner: R { id: 5 } }, Hold { inner: R { id: 6 } })",
            "e2\ndSv\ndR6\ndSv\ndR5\n",
        ),
        (
            "mixed qualified + unqualified, reverse order",
            "eat2(Sv.Hold { inner: R { id: 8 } }, Hold { inner: R { id: 9 } })",
            "e2\ndSv\ndR9\ndSv\ndR8\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }
}

/// B-2026-08-31-8 — an enum STRUCT-VARIANT literal passed as a fresh-temp
/// argument runs its `Drop` bodies.
///
/// `eat(Sv.Hold { inner: R { .. } })` is spelled as a struct literal whose path
/// ends in the VARIANT, so the fresh-temp argument classifier's
/// `find_struct_def(path.last())` answered `None` and the walk claimed no owner
/// at all: NEITHER the enum's own body nor its payload's ran, against both
/// compiled backends running both. A `Drop` that closes a handle or releases a
/// lock simply never fired on this backend.
///
/// `tuple-variant` is the control that localizes it, and it is the whole point
/// of the row: the identical program spelled `Tv.A(R { .. })` is an
/// `ExprKind::Call` whose callee path resolves through the enum, and was
/// correct throughout. The spelling was the only variable.
///
/// `named-local` is the row a later widening would break: it already had an
/// owner, and claiming one here too would double the body.
///
/// The PRODUCER-FN spelling (`eat(mk(4))`) is deliberately absent. It diverges
/// the other way on this header — the interpreter runs `dSv dR4`, both compiled
/// backends run `dSv` alone — so pinning it here would assert one side of a
/// live disagreement. Filed as its own row; when it closes, it belongs in this
/// table and its twin.
///
/// `two-fresh` pins the ORDER as well as the count — argument temps are
/// introduced left to right and pop right to left (B-2026-08-29-46), so a fix
/// that fired them forward would agree on the count and diverge on the order,
/// which no count-only assertion could see.
#[test]
fn fresh_temp_struct_variant_arg_runs_its_drop_bodies() {
    const H: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         enum Tv { A(R), Nil }\n\
         impl Drop for Tv { fn drop(mut ref self) { println(\"dTv\") } }\n\
         fn eat(v: Sv) { println(\"e\") }\n\
         fn eatt(v: Tv) { println(\"e\") }\n\
         fn eat2(v: Sv, w: Sv) { println(\"e2\") }\n\
         fn mk(i: i64) -> Sv { return Sv.Hold { inner: R { id: i } }; }\n";
    for (label, body, want) in [
        (
            "struct-variant fresh temp",
            "eat(Sv.Hold { inner: R { id: 1 } })",
            "e\ndSv\ndR1\n",
        ),
        (
            "tuple-variant fresh temp (control)",
            "eatt(Tv.A(R { id: 2 }))",
            "e\ndTv\ndR2\n",
        ),
        (
            "named-local (control)",
            "let a = Sv.Hold { inner: R { id: 3 } }; eat(a)",
            "e\ndSv\ndR3\n",
        ),
        (
            "two-fresh, reverse order",
            "eat2(Sv.Hold { inner: R { id: 5 } }, Sv.Hold { inner: R { id: 6 } })",
            "e2\ndSv\ndR6\ndSv\ndR5\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }

    // The shape the row was FILED as: a callee that `match`es the owned param.
    // The match machinery reached the identical decisions for both spellings
    // (measured), so the loss was always the caller-side classifier — with and
    // without the arm's rebind, and with the tuple-variant twin beside it.
    const M: &str = "struct R { id: i64 }\n\
         impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
         enum Sv { Hold { inner: R }, Nil }\n\
         impl Drop for Sv { fn drop(mut ref self) { println(\"dSv\") } }\n\
         fn s2(v: Sv) { match v { Sv.Hold { inner } => { let m = inner; println(f\"b{m.id}\") } Sv.Nil => { } } }\n\
         fn s2b(v: Sv) { match v { Sv.Hold { inner } => { println(f\"b{inner.id}\") } Sv.Nil => { } } }\n";
    for (label, body, want) in [
        (
            "match, with rebind",
            "s2(Sv.Hold { inner: R { id: 7 } })",
            "b7\ndSv\ndR7\n",
        ),
        (
            "match, no rebind",
            "s2b(Sv.Hold { inner: R { id: 8 } })",
            "b8\ndSv\ndR8\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{M}fn main() {{\n{body}\n}}\n")),
            want,
            "{label}"
        );
    }
}

/// B-2026-09-10-25 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_bare_variant_ctor_tuple_elem_runs_its_drop_body`, same program, pinned
/// to the same string.
///
/// A BARE enum-variant constructor as a tuple element (`(Some(mk(11)), 7)`)
/// ran no `Drop` work on ANY surface, where the qualified `Option.Some(..)`
/// twin one cell down was correct on all four — so the discriminator is the
/// CONSTRUCTOR SPELLING, not the tuple-argument position the filing row
/// blamed. `discard_tuple_elem_is_fresh` admitted a `Path` callee and, for an
/// `Identifier` callee, only a user FUNCTION name, and ONE non-fresh element
/// disqualifies the whole literal — which is why `mixed` lost its plain-struct
/// element's body as well.
///
/// The lookup behind the new arm scans the BAKED STDLIB as well as
/// `program.items`, and that is not incidental: `Some` / `Ok` / `Err` are
/// declared there, so a user-program-only scan answers `None` for the
/// commonest spelling the arm exists to admit, while codegen's
/// `enum_name_for_variant_ctor` reads `enum_layouts` and answers `Option`.
/// Measured in exactly that state — `mixed` printed `dR19` alone compiled and
/// both bodies here — which is a run-vs-build divergence rather than a fix.
///
/// `discard` covers the second POSITION the same gate governs; `movedplace`
/// pins exactly ONE body when the payload is a moved binding; `barenone` that
/// a payload-free variant runs nothing.
#[test]
fn test_bare_variant_ctor_tuple_elem_runs_its_drop_body() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
enum W { A(R), N }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }

fn optArg(t: (Option[R], i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn twoArg(t: (Option[R], Option[R])) -> i64 { println("  in2"); return 0; }
fn sndArg(t: (i64, Option[R])) -> i64 { println(f"  in{t.0}"); return 0; }
fn uvArg(t: (W, i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn mixArg(t: (Option[R], R)) -> i64 { println(f"  in{t.1.id}"); return 0; }
fn nestArg(t: ((Option[R], i64), i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn resArg(t: (Result[R, i64], i64)) -> i64 { println(f"  in{t.1}"); return 0; }

fn main() {
    println("baretemp");   let _ = optArg((Some(mk(11)), 7));                println("baretemp end")
    println("qualtemp");   let _ = optArg((Option.Some(mk(12)), 7));         println("qualtemp end")
    println("twobare");    let _ = twoArg((Some(mk(13)), Some(mk(14))));     println("twobare end")
    println("secondpos");  let _ = sndArg((7, Some(mk(15))));                println("secondpos end")
    println("bareuv");     let _ = uvArg((A(mk(16)), 7));                    println("bareuv end")
    println("qualuv");     let _ = uvArg((W.A(mk(17)), 7));                  println("qualuv end")
    println("mixed");      let _ = mixArg((Some(mk(18)), mk(19)));           println("mixed end")
    println("nested");     let _ = nestArg(((Some(mk(20)), 7), 9));          println("nested end")
    println("bareok");     let _ = resArg((Ok(mk(21)), 7));                  println("bareok end")
    println("barenone");   let _ = optArg((None, 7));                        println("barenone end")
    println("discard");    let _ = (Some(mk(22)), 7);                        println("discard end")
    println("movedplace"); let r = mk(23); let _ = optArg((Some(r), 7));     println("movedplace end")
    println("done")
}
"#),
        r#"baretemp
  in7
dR11/t11
baretemp end
qualtemp
  in7
dR12/t12
qualtemp end
twobare
  in2
dR13/t13
dR14/t14
twobare end
secondpos
  in7
dR15/t15
secondpos end
bareuv
  in7
dR16/t16
bareuv end
qualuv
  in7
dR17/t17
qualuv end
mixed
  in19
dR18/t18
dR19/t19
mixed end
nested
  in9
dR20/t20
nested end
bareok
  in7
dR21/t21
bareok end
barenone
  in7
barenone end
discard
dR22/t22
discard end
movedplace
  in7
dR23/t23
movedplace end
done
"#
    );
}

/// B-2026-09-10-25's CARVE-OUT — interpreter twin of `tests/codegen.rs`'s
/// `e2e_bare_shared_enum_tuple_elem_stays_silent`, same program and string.
///
/// A `shared` / `par` enum's drop is refcount-driven, and the two backends do
/// not agree about the shape: the QUALIFIED `(Sh.S(mk(1)), 7)` runs the payload
/// body HERE and on no compiled surface, in the argument and `let _ =`
/// positions alike. That divergence predates this row and is B-2026-09-17-19,
/// which owns the same lost body one wrapping in (a BARE `shared enum` local);
/// these tuple-element cells are a second repro of it, not a separate defect.
/// The BARE spelling is agreed-silent on all four, so the gate widened by
/// B-2026-09-10-25 excludes shared and `par` heads rather than converting an
/// agreed gap into a second divergence.
///
/// This pins a KNOWN GAP, not correct behaviour. When the qualified divergence
/// is fixed, both halves move together and this expectation is what changes.
#[test]
fn test_bare_shared_enum_tuple_elem_stays_silent() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}/{self.tag}") } }
shared enum Sh { S(R), Z }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}" }; }
fn shArg(t: (Sh, i64)) -> i64 { println(f"  in{t.1}"); return 0; }
fn main() {
    println("bare"); let _ = shArg((S(mk(1)), 7)); println("bare end")
    println("done")
}
"#),
        "bare\n  in7\nbare end\ndone\n"
    );
}

/// B-2026-09-05-26 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_user_enum_struct_payload_owns_its_heap`, same program and string. The
/// interpreter was the correct reference throughout (the leak and the garbage
/// `id` were codegen's alone).
#[test]
fn test_user_enum_struct_payload_owns_its_heap() {
    assert_eq!(
        run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
struct Two { a: R, b: R }
struct Ho2 { a: R, b: Option[R] }
enum Wrap { W(Ho2), T(Two), N }
enum E { V(R), N }
fn mk(k: i64) -> R { return R { id: k, tag: f"t{k}", xs: [k] } }
fn main() {
    { let w: Wrap = Wrap.T(Two { a: mk(1), b: mk(101) }); println("one") }
    { let w: Wrap = Wrap.T(Two { a: mk(2), b: mk(102) }); match w { _ => println("n") } println("two") }
    { let w: Wrap = Wrap.T(Two { a: mk(3), b: mk(103) }); match w { Wrap.T(h) => { println("in") }, _ => println("n") } println("three") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(4), b: Option.Some(mk(104)) }); println("four") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(5), b: Option.Some(mk(105)) }); match w { _ => println("n") } println("five") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(6), b: Option.Some(mk(106)) }); match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("six") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(7), b: Option.Some(mk(107)) }); match w { Wrap.W(h) => { let Ho2 { a, b } = h; println("in") }, _ => println("n") } println("seven") }
    { let w: Wrap = Wrap.W(Ho2 { a: mk(8), b: Option.None }); match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("eight") }
    { let e: E = E.V(mk(9)); println("nine") }
    { let w: Wrap = Wrap.N; match w { Wrap.W(h) => { println("in") }, _ => println("n") } println("ten") }
    println("end")
}
"#),
        "dR101\ndR1\none\nn\ndR102\ndR2\ntwo\nin\ndR103\ndR3\nthree\ndR104\ndR4\nfour\nn\ndR105\ndR5\nfive\nin\ndR106\ndR6\nsix\ndR107\ndR7\nin\nseven\nin\ndR8\neight\ndR9\nnine\nn\nten\nend\n"
    );
}

/// B-2026-08-31-50 — interpreter twin of `tests/codegen.rs`'s
/// `e2e_enum_ctor_mixed_wrap_rebind_inherits_the_slot_mask`, same program
/// and string. This backend's transfer of `moved_out_enum_payload_slots` on
/// a rebind was written during B-2026-08-29-44 and withheld until codegen
/// could inherit the same mask; both landed in one commit, and this pins
/// the five shapes from this side.
#[test]
fn test_enum_ctor_mixed_wrap_rebind_inherits_the_slot_mask() {
    assert_eq!(
        run(r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"n{i}" }; }
enum W2 { Two(R, R), None2 }
enum W1 { One(R), None1 }
fn take(r: R) -> i64 { let w: W2 = W2.Two(r, mk(2)); let w2: W2 = w; return 7; }
fn take_ctl(r: R) -> i64 { let w: W2 = W2.Two(r, mk(4)); return 7; }
fn take1(r: R) -> i64 { let w: W1 = W1.One(r); let w2: W1 = w; return 7; }
fn take_twice(r: R) -> i64 { let w: W2 = W2.Two(r, mk(8)); let w2: W2 = w; let w3: W2 = w2; return 7; }
fn take_swap(r: R) -> i64 { let w: W2 = W2.Two(mk(10), r); let w2: W2 = w; return 7; }
fn main() {
    { let v: i64 = take(mk(1)); println(f"v={v}"); println("one") }
    { let v: i64 = take_ctl(mk(3)); println(f"v={v}"); println("two") }
    { let v: i64 = take1(mk(5)); println(f"v={v}"); println("three") }
    { let v: i64 = take_twice(mk(7)); println(f"v={v}"); println("four") }
    { let v: i64 = take_swap(mk(9)); println(f"v={v}"); println("five") }
    println("end")
}
"#),
        "dR2\ndR1\nv=7\none\ndR4\ndR3\nv=7\ntwo\ndR5\nv=7\nthree\ndR8\ndR7\nv=7\nfour\ndR10\ndR9\nv=7\nfive\nend\n"
    );
}

/// B-2026-09-15-14 — the interpreter half. An enum element of a `Map`/`Set`
/// ran no user `Drop` body at all, because a plain enum variant fell past the
/// `Value::Struct` destructure in both map walks — the twin of codegen's
/// `struct_types`-only gate, silent in exactly the same way on both backends.
///
/// The codegen twin is `tests/codegen.rs`'s
/// `e2e_hash_container_enum_element_runs_its_body`, which asserts the same
/// cells through `karac build`; both must stay in step, since the whole defect
/// was the two agreeing on ZERO.
#[test]
fn test_hash_container_enum_element_runs_its_body() {
    const H: &str = "#[derive(Hash, Eq, PartialEq)]\n\
         enum Tg { Named { s: String }, Num { n: i64 } }\n\
         impl Drop for Tg { fn drop(mut ref self) { println(\"dD\") } }\n\
         fn mkd(n: i64) -> Tg { return Tg.Named { s: f\"aaaaaaaaaaaaaaaa-{n}\" }; }\n";
    for (label, body) in [
        (
            "set-element",
            "fn main() {\n\
             \x20   let mut s: Set[Tg] = Set.new();\n\
             \x20   s.insert(mkd(0i64));\n\
             \x20   println(f\"len:{s.len()}\");\n\
             }\n",
        ),
        (
            "map-key",
            "fn main() {\n\
             \x20   let mut m: Map[Tg, i64] = Map.new();\n\
             \x20   m.insert(mkd(0i64), 7i64);\n\
             \x20   println(f\"len:{m.len()}\");\n\
             }\n",
        ),
        (
            "map-value",
            "fn main() {\n\
             \x20   let mut m: Map[i64, Tg] = Map.new();\n\
             \x20   m.insert(3i64, mkd(0i64));\n\
             \x20   println(f\"len:{m.len()}\");\n\
             }\n",
        ),
        // The struct-FIELD spelling goes through `run_field_map_half_user_drops`
        // rather than `run_map_half_user_drops`. The interpreter has two arms
        // where codegen has one walker, so fixing only the binding arm left
        // this cell reading `interp=0 build=1` — a run-vs-build divergence
        // traded for the agreement it replaced. Both arms are patched; this
        // pins the second.
        (
            "set-in-struct-field",
            "struct Holder { mut s: Set[Tg] }\n\
             fn main() {\n\
             \x20   let mut h = Holder { s: Set.new() };\n\
             \x20   h.s.insert(mkd(0i64));\n\
             \x20   println(f\"len:{h.s.len()}\");\n\
             }\n",
        ),
    ] {
        assert_eq!(
            run(&format!("{H}{body}")),
            "len:1\ndD\n",
            "{label}: one body is owed at the container's destruction"
        );
    }
    // CONTROLS — correct before the fix, and what isolate the axis: a `Vec` of
    // the same enum says the axis is the hash container, a STRUCT element in
    // the same `Map` says the other half is the element being an enum. A
    // widening that fired per-container rather than per-element doubles these.
    assert_eq!(
        run(&format!(
            "{H}fn main() {{\n\
             \x20   let mut v: Vec[Tg] = Vec.new();\n\
             \x20   v.push(mkd(0i64));\n\
             \x20   println(f\"len:{{v.len()}}\");\n\
             }}\n"
        )),
        "len:1\ndD\n",
        "Vec of the same enum"
    );
    assert_eq!(
        run("#[derive(Hash, Eq, PartialEq)]\n\
             struct Dk { s: String }\n\
             impl Drop for Dk { fn drop(mut ref self) { println(\"dD\") } }\n\
             fn mks(n: i64) -> Dk { return Dk { s: f\"aaaaaaaaaaaaaaaa-{n}\" }; }\n\
             fn main() {\n\
             \x20   let mut m: Map[Dk, i64] = Map.new();\n\
             \x20   m.insert(mks(0i64), 7i64);\n\
             \x20   println(f\"len:{m.len()}\");\n\
             }\n"),
        "len:1\ndD\n",
        "struct element in the same Map"
    );
}

/// B-2026-09-16-31 — the INTERPRETER twin of `tests/codegen.rs`'s
/// `e2e_generic_enum_with_generic_drop_impl_survives_an_owned_self_method`,
/// byte-identical source and expectation.
///
/// Every cell here already passed on `--interp` when the row was filed: the
/// crash and the three lost/doubled bodies it turned out to be hiding were all
/// on the compiled side. That is exactly why the twin is worth having — this
/// side is the ORACLE the compiled transcript is measured against, and without
/// it in the tree nothing stops a later change moving both halves together
/// into a new agreed gap.
#[test]
fn test_generic_enum_with_generic_drop_impl_survives_an_owned_self_method() {
    let out = run(r#"struct R { id: i64, tag: String, xs: Vec[i64] }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, tag: f"t{i}", xs: [i] } }

enum G[T] { X(T), Y }
impl[T] Drop for G[T] { fn drop(mut ref self) { println("  dG") } }
impl[T] G[T] {
    fn gread(self) -> i64 { match self { G.X(t) => { return 1; } G.Y => { return 0; } } }
    fn gnone(self) -> i64 { return 5 }
}

enum K[T] { X(T), Y }
impl Drop for K[R] { fn drop(mut ref self) { println("  dK") } }
impl[T] K[T] { fn kread(self) -> i64 { match self { K.X(t) => { return 1; } K.Y => { return 0; } } } }

enum H[T] { X(T), Y }
impl[T] H[T] {
    fn hnone(self) -> i64 { return 5 }
    fn hread(self) -> i64 { match self { H.X(t) => { return 1; } H.Y => { return 0; } } }
}

enum P[T] { X(T), Y }
impl P[R] { fn pnone(self) -> i64 { return 5 } }
fn mkp(i: i64) -> P[R] { return P.X(mk(i)) }

enum E { A(R), B }
impl Drop for E { fn drop(mut ref self) { println("  dE") } }
impl E {
    fn eread(self) -> i64 { match self { E.A(t) => { return 1; } E.B => { return 0; } } }
    fn enone(self) -> i64 { return 5 }
}

struct S[T] { v: T }
impl[T] Drop for S[T] { fn drop(mut ref self) { println("  dS") } }
impl[T] S[T] { fn snone(self) -> i64 { return 5 } }

fn main() {
    println("repro");  { let g: G[R] = G.X(mk(20)); println(f"  x{g.gread()}") }
    println("gnone");  { let g: G[R] = G.X(mk(21)); println(f"  x{g.gnone()}") }
    println("glocal"); { let g: G[i64] = G.X(7); println("  x1") }
    println("gnarrow");{ let g: G[i64] = G.X(7); println(f"  x{g.gnone()}") }
    println("kread");  { let k: K[R] = K.X(mk(22)); println(f"  x{k.kread()}") }
    println("hnone");  { let h: H[R] = H.X(mk(23)); println(f"  x{h.hnone()}") }
    println("hread");  { let h: H[R] = H.X(mk(24)); println(f"  x{h.hread()}") }
    println("htemp");  { println(f"  x{H.X(mk(25)).hnone()}") }
    println("pnone");  { let p: P[R] = P.X(mk(26)); println(f"  x{p.pnone()}") }
    println("ptemp");  { println(f"  x{P.X(mk(27)).pnone()}") }
    println("pcall");  { println(f"  x{mkp(28).pnone()}") }
    println("twomono");{ let a: G[R] = G.X(mk(29)); println(f"  x{a.gnone()}"); let b: G[i64] = G.X(7); println(f"  y{b.gnone()}") }
    println("enone");  { let e: E = E.A(mk(30)); println(f"  x{e.enone()}") }
    println("eread");  { let e: E = E.A(mk(31)); println(f"  x{e.eread()}") }
    println("snone");  { let s: S[R] = S { v: mk(32) }; println(f"  x{s.snone()}") }
    println("end")
}
"#);
    assert_eq!(out, "repro\n  x1\n  dG\n  dR20\ngnone\n  x5\n  dG\n  dR21\nglocal\n  dG\n  x1\ngnarrow\n  x5\n  dG\nkread\n  x1\n  dK\n  dR22\nhnone\n  x5\n  dR23\nhread\n  dR24\n  x1\nhtemp\n  dR25\n  x5\npnone\n  x5\n  dR26\nptemp\n  dR27\n  x5\npcall\n  dR28\n  x5\ntwomono\n  x5\n  dG\n  dR29\n  y5\n  dG\nenone\n  x5\n  dE\n  dR30\neread\n  x1\n  dE\n  dR31\nsnone\n  x5\n  dS\n  dR32\nend\n", "got:\n{out}");
}

/// B-2026-09-19-12 — an ELEMENT moved out of a boxed tuple payload must not
/// have its `Drop` body run again by the envelope's walk.
///
/// `Some(t) => { let x = t.0 }` reads THROUGH `t` — a projection is a read of
/// the binding — so `optres_arm_takes_whole_payload` answers false and the
/// scrutinee keeps its element walk. That is right about `t` and wrong about
/// element 0, which `x` now owns: both ran its body, the second time over a
/// husk. The codegen twin masks per element at the move site
/// (B-2026-09-19-9); this is the interpreter's copy of that mask, asked of the
/// SAME shared predicate one hop deeper, so the two cannot drift.
///
/// The cells, and what each one is for:
///
///   one        the row's own spelling, one `Drop` element moved.
///   two        a SIBLING the arm never touched, which must still run — the
///              cell that forbids retracting the walk outright.
///   both       every element moved, where the walk owes nothing.
///   second     only element 1 moved, so the mask must be per element and not
///              a high-water mark. Element 0's body is due AFTER `mid`.
///   readonly   an arm that reads and moves nothing: the walk is the sole
///              owner of both elements.
///   inline     an INLINE scrutinee (`match mko()`), which was already correct
///              and stays so — the row's narrowing said the defect was the
///              LOCAL spelling only.
///   struct     a struct payload, the shape B-2026-09-17-34 already covered.
///   none       the `None` arm, where nothing is bound at all.
///
/// Twin of `tests/codegen.rs`'s
/// `e2e_boxed_tuple_payload_elem_move_runs_each_body_once`, byte-identical
/// source and expectation. This side was the only wrong one, but the fixture
/// is paired anyway: the codegen half is what holds the agreement, and a
/// one-sided fixture cannot see an A/B divergence reopen.
#[test]
fn test_boxed_tuple_payload_elem_move_runs_each_body_once() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct W { r: R, n: i64, p: i64, q: i64 }
fn mk(i: i64) -> R { return R { id: i } }
fn mko() -> Option[(R, i64, i64, i64)] { return Some((mk(40), 1, 2, 3)) }

fn main() {
    println("one");      { let o: Option[(R, i64, i64, i64)] = Some((mk(1), 1, 2, 3)); match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
    println("two");      { let o: Option[(R, R, i64, i64)] = Some((mk(2), mk(3), 2, 3)); match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
    println("both");     { let o: Option[(R, R, i64, i64)] = Some((mk(4), mk(5), 2, 3)); match o { Some(t) => { let x = t.0; let y = t.1; println("  mid") } None => { println("  n") } } println("  end") }
    println("second");   { let o: Option[(R, R, i64, i64)] = Some((mk(6), mk(7), 2, 3)); match o { Some(t) => { let y = t.1; println("  mid") } None => { println("  n") } } println("  end") }
    println("readonly"); { let o: Option[(R, R, i64, i64)] = Some((mk(9), mk(10), 2, 3)); match o { Some(t) => { println(f"  mid{t.0.id}") } None => { println("  n") } } println("  end") }
    println("inline");   { match mko() { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
    println("struct");   { let o: Option[W] = Some(W { r: mk(12), n: 1, p: 2, q: 3 }); match o { Some(t) => { let x = t.r; println("  mid") } None => { println("  n") } } println("  end") }
    println("none");     { let o: Option[(R, i64, i64, i64)] = None; match o { Some(t) => { let x = t.0; println("  mid") } None => { println("  n") } } println("  end") }
    println("end")
}
"#);
    assert_eq!(out, "one\n  dR1\n  mid\n  end\ntwo\n  dR2\n  mid\n  dR3\n  end\nboth\n  dR4\n  dR5\n  mid\n  end\nsecond\n  dR7\n  mid\n  dR6\n  end\nreadonly\n  mid9\n  dR9\n  dR10\n  end\ninline\n  dR40\n  mid\n  end\nstruct\n  dR12\n  mid\n  end\nnone\n  n\n  end\nend\n", "got:\n{out}");
}

/// B-2026-09-17-22 — A `shared enum`'s UNIT VARIANT STRANDS ITS RC SHELL,
/// AND WITH IT THE `Drop` BODY THAT SHELL WAS SUPPOSED TO RUN.
///
/// This is the INTERPRETER side of the fix, and every line of it was already
/// correct: the interpreter has always run these bodies. What it pins is that
/// it still does, unchanged, now that the compiled backends agree — so a later
/// change on either side cannot quietly re-open the gap by moving this half.
///
/// The compiled half lost the whole `{ i64 rc, i64 tag, .. }` allocation on
/// `{ let s = U.Ua; }`, because the `let` site retained a value
/// `emit_rc_alloc` had already set to `rc = 1` — `rhs_yields_fresh_ref` matched
/// `Call` / `MethodCall` / `StructLiteral`, and a unit variant is the one
/// fresh-ref source spelled as a `Path` (`U.Ua`) or a bare `Identifier`
/// (`Ub`). The count never reached 0, so the free never ran, so the body never
/// ran: six of these cells printed nothing at all on every compiled surface.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_shared_enum_unit_variant_runs_its_drop_body`, byte-identical source and
/// expectation.
#[test]
fn test_shared_enum_unit_variant_runs_its_drop_body() {
    let out = run(r#"shared enum U { Ua, Ub }
impl Drop for U { fn drop(mut ref self) { println(f"  dU") } }
shared enum V { Va, Vb }
par enum W { Wa, Wb }
impl Drop for W { fn drop(mut ref self) { println(f"  dW") } }
enum Plain { Pa, Pb }
impl Drop for Plain { fn drop(mut ref self) { println(f"  dP") } }
fn tag(u: ref U) -> i64 { match u { U.Ua => { return 1 } U.Ub => { return 0 } } }
fn main() {
    println("qual");     { let s = U.Ua; println("  mid") } println("  out")
    println("bare");     { let s = Ub; println("  mid") } println("  out")
    println("two");      { let s = U.Ua; let t = U.Ub; println("  mid") } println("  out")
    println("alias");    { let s = U.Ua; let t = s; println("  mid") } println("  out")
    println("uselater"); { let s = U.Ua; println("  mid"); println(f"  t{tag(s)}") } println("  out")
    println("reassign"); { let mut s = U.Ua; s = U.Ub; println("  mid") } println("  out")
    println("nodrop");   { let s = V.Va; println("  mid") } println("  out")
    println("par");      { let s = W.Wa; println("  mid") } println("  out")
    println("plain");    { let s = Plain.Pa; println("  mid") } println("  out")
    println("end")
}
"#);
    assert_eq!(out, "qual\n  dU\n  mid\n  out\nbare\n  dU\n  mid\n  out\ntwo\n  dU\n  dU\n  mid\n  out\nalias\n  dU\n  mid\n  out\nuselater\n  mid\n  t1\n  dU\n  out\nreassign\n  dU\n  dU\n  mid\n  out\nnodrop\n  mid\n  out\npar\n  dW\n  mid\n  out\nplain\n  dP\n  mid\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-17-19 — A `shared enum`'s VARIANT PAYLOAD RUNS ITS `Drop` BODY,
/// AND THE RELEASE LANDS AT THE BINDING'S LIVE-RANGE END.
///
/// `let s: SMono = SMono.P(mkr(1));` over `shared enum SMono {{ P(R2), Q }}` with
/// `impl Drop for R2` printed `d2:9` under `--interp` and NOTHING on jit /
/// `karac build` / `KARAC_AUTO_PAR=0 build`. `emit_enum_payload_user_drop_bodies_fn_skipping`
/// defers a shared enum to "the RC machinery", and the RC machinery ran the
/// enum's own body and the memory walk but never the payload's — so no pass
/// owned it.
///
/// `live` and `uselater` are the second half and do not follow from the first:
/// once the bodies ran, they ran at the CLOSING BRACE while `--interp` ran them
/// at `s`'s last use. `uselater` is the cell that PINS that — with a use after
/// the `let`, the body lands after the use, which separates live-range-end
/// firing from firing at the construction statement (the two coincide in every
/// other cell here, and the plain-enum convention this tree already had is the
/// latter). B-2026-09-04-13 made the same admission for a shared
/// STRUCT's plain fields and wrote down this exact consequence — a holder
/// "invisible while its field bodies never ran" that "became a run/build
/// divergence the moment they did". `noheap` is what forces the whole-fn gate
/// to widen rather than just the per-variant one: a payload that owns a body
/// but no heap is not walkable, so the drop fn used to decline outright.
///
/// `psh` is a separate defect the same work uncovered, on the INTERPRETER
/// side: the shared-release walk covered struct, tuple and array holders and
/// stopped at an enum, so a `shared struct` in a plain enum's payload released
/// nothing here while every compiled surface ran its body. `control`, `qvar`
/// and `unitvar` are the negative cells — a plain enum, a payload-free variant
/// of a payload-carrying enum, and an enum with no payload anywhere.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_shared_enum_payload_runs_its_drop_body`, byte-identical source and
/// expectation.
#[test]
fn test_shared_enum_payload_runs_its_drop_body() {
    let out = run(r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"ssssssss{i}", t: f"tttttttt{i}", u: f"uuuuuuuu{i}" } }
struct Z { n: i64 }
impl Drop for Z { fn drop(mut ref self) { println(f"  dZ{self.n}") } }
shared struct Sr { s: String }
impl Drop for Sr { fn drop(mut ref self) { println(f"  dS{self.s.len()}") } }
enum Mono { P(R2), Q }
shared enum SMono { P(R2), Q }
shared enum Sz { P(Z), Q }
shared enum Unit { P, Q }
enum HoldSr { P(Sr), Q }
fn tag(e: ref SMono) -> i64 { match e { SMono.P(_) => { return 1 } SMono.Q => { return 0 } } }

fn main() {
    println("temp");    { let s: SMono = SMono.P(mkr(1)); } println("  out")
    println("named");   { let r = mkr(2); let s: SMono = SMono.P(r); } println("  out")
    println("two");     { let a: SMono = SMono.P(mkr(3)); let b = a; } println("  out")
    println("live");    { let s: SMono = SMono.P(mkr(4)); println("  mid") } println("  out")
    println("noheap");  { let s: Sz = Sz.P(Z { n: 5 }); println("  mid") } println("  out")
    println("unitvar"); { let s: Unit = Unit.Q; println("  mid") } println("  out")
    println("qvar");    { let s: SMono = SMono.Q; println("  mid") } println("  out")
    println("uselater"); { let s: SMono = SMono.P(mkr(9)); println("  mid"); println(f"  t{tag(s)}") } println("  out")
    println("psh");     { let h: HoldSr = HoldSr.P(Sr { s: "sssss" }); println("  mid") } println("  out")
    println("control"); { let m: Mono = Mono.P(mkr(8)); println("  mid") } println("  out")
    println("end")
}
"#);
    assert_eq!(out, "temp\n  d2:9\n  out\nnamed\n  d2:9\n  out\ntwo\n  d2:9\n  out\nlive\n  d2:9\n  mid\n  out\nnoheap\n  dZ5\n  mid\n  out\nunitvar\n  mid\n  out\nqvar\n  mid\n  out\nuselater\n  mid\n  t1\n  d2:9\n  out\npsh\n  mid\n  dS5\n  out\ncontrol\n  d2:9\n  mid\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-19-17 — A `shared` OR `par` ENUM HELD IN A PLAIN ENUM'S PAYLOAD
/// RUNS ITS PAYLOAD'S `Drop` BODY, once, UNDER `--interp`.
///
/// `enum H3 { P(SMono), Q }` over `shared enum SMono { P(R2), Q }` ran NOTHING
/// here while every compiled surface ran `R2`'s body — the one cell
/// B-2026-09-17-19 moved from AGREED to DIVERGENT and filed rather than fixed,
/// on the reading that the interpreter half needed a refcounted representation
/// for a `shared enum` (which is a plain `Value::EnumVariant` with no `Arc`,
/// unlike `shared struct`'s `Value::SharedStruct(Arc<..>)`).
///
/// IT NEEDED NO REFCOUNT, and the cheap question that row named first is the
/// one that answered it. The four SIBLING holders of a shared enum — a struct
/// FIELD, a tuple ELEMENT, a `Vec` ELEMENT and an `Option` payload — already
/// fire the payload body on their holder's death with no count consulted, and
/// already agree with all three compiled surfaces on the COUNT. The enum
/// holder was the one position whose walk stopped short:
/// `run_enum_payload_user_drops_value` destructured a `Value::Struct` payload
/// and let an enum payload fall through to `continue`.
///
/// `E_ENUM_NESTED_ENUM_PAYLOAD` IS WHAT MAKES THE NEW ARM SAFE without a
/// `shared_types` gate of its own, and `par` is here to pin that. A PLAIN enum
/// cannot be an enum variant's payload at all — "v1 only supports up to one
/// level of enum nesting; ... mark the inner enum as `shared` (RC pointer) or
/// `par` (cross-task pointer)" — and `Option` / `Result` route through their
/// own instantiation-driven arm before this one, so the only values the arm
/// can ever see are the two spellings that diagnostic prescribes.
///
/// `owndrop` pins the ORDER (the holder's own body, then its payload's —
/// design.md § Part 8), `noheap` a payload that owns a body but no heap,
/// `reclist` a self-referential `shared enum` whose recursion this arm has to
/// terminate (and which moved from ONE body to TWO, matching the compiled
/// side exactly). `qvar`, `unitvar` and `control` are the negative cells: a
/// payload-free variant of a payload-carrying shared enum, a payload-free
/// holder, and a plain struct payload that was always right.
///
/// THE CODEGEN TWIN IS NOT BYTE-IDENTICAL, deliberately, and that is the
/// remaining half rather than a defect in either side. The four holder cells
/// print their body BEFORE `mid` here and after it there: one body either way,
/// at the binding's live-range end on this side and at lexical scope exit on
/// that one. That is B-2026-09-19-18, which this row joins as a fourth
/// spelling; design.md `:866` ("Destructor calls ... fire at each binding's
/// LIVE-RANGE END") puts the correct point on this side.
#[test]
fn test_shared_enum_in_plain_enum_payload_runs_its_drop_body() {
    let out = run(r#"struct R2 { s: String, t: String, u: String }
impl Drop for R2 { fn drop(mut ref self) { println(f"  d2:{self.s.len()}") } }
fn mkr(i: i64) -> R2 { return R2 { s: f"aaaaaaaaa", t: f"b", u: f"c" } }
struct Z { n: i64 }
impl Drop for Z { fn drop(mut ref self) { println(f"  dZ{self.n}") } }
shared enum SMono { P(R2), Q }
shared enum Sdd { P(R2), Q }
impl Drop for Sdd { fn drop(mut ref self) { println("  dSdd") } }
shared enum Sz { P(Z), Q }
par enum PMono { P(R2), Q }
enum H3 { P(SMono), Q }
enum Hd { P(Sdd), Q }
enum Hz { P(Sz), Q }
enum Hp { P(PMono), Q }
enum Hn { P(SMono), Q }
shared enum Lst { Cons(R2, Lst), Nil }
enum Plain { P(R2), Q }

fn main() {
    println("nested");   { let h: H3 = H3.P(SMono.P(mkr(1))); println("  mid") } println("  out")
    println("par");      { let h: Hp = Hp.P(PMono.P(mkr(2))); println("  mid") } println("  out")
    println("owndrop");  { let h: Hd = Hd.P(Sdd.P(mkr(3))); println("  mid") } println("  out")
    println("noheap");   { let h: Hz = Hz.P(Sz.P(Z { n: 4 })); println("  mid") } println("  out")
    println("qvar");     { let h: Hn = Hn.P(SMono.Q); println("  mid") } println("  out")
    println("unitvar");  { let h: Hn = Hn.Q; println("  mid") } println("  out")
    println("reclist");  { let a: Lst = Lst.Cons(mkr(5), Lst.Nil); let b: Lst = Lst.Cons(mkr(6), a); println("  mid") } println("  out")
    println("control");  { let p: Plain = Plain.P(mkr(7)); println("  mid") } println("  out")
    println("end")
}
"#);
    assert_eq!(out, "nested\n  d2:9\n  mid\n  out\npar\n  d2:9\n  mid\n  out\nowndrop\n  dSdd\n  d2:9\n  mid\n  out\nnoheap\n  dZ4\n  mid\n  out\nqvar\n  mid\n  out\nunitvar\n  mid\n  out\nreclist\n  d2:9\n  d2:9\n  mid\n  out\ncontrol\n  d2:9\n  mid\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-10-20 — an enum's CONTAINER payload held in a STRUCT FIELD.
///
/// The sibling of `tests/codegen.rs`'s
/// `e2e_enum_container_payload_in_struct_field_runs_element_drop_bodies`, on the axis
/// that row's earlier sessions never varied: POSITION. Both of its previous
/// fixes were measured across payload TYPES — `Vec` vs `Array` vs tuple,
/// declared vs generic — with every cell written at the same `let`-bound
/// position, and both were reverted. A cell matrix that varies only the type
/// cannot see a defect whose whole shape is which registration site fires.
///
/// `f-arr` / `f-vec` / `f-venum` are the fixed cells: an `Array[R, 2]`,
/// `Vec[R]` or `Vec[Mono]` payload whose enum sits in a struct field ran its
/// element bodies under `--interp` and on NO compiled surface — three
/// run-vs-build divergences from ONE gate. `type_runs_user_drop`'s enum leg
/// reads the payload's HEAD NAME, and the head of `Array[R, 2]` is `Array`,
/// so `user_drop_field_indices_mono` never admitted the field, the parent's
/// bodies walker was declined for an empty set, and the field's enum arm —
/// which already calls `emit_enum_payload_user_drop_bodies_fn` — was never
/// reached. Only the gate was missing.
///
/// `f-bind` varies how the field is INITIALIZED (a named local rather than a
/// fresh temp) and `f-second` puts the enum at field index 1 behind a scalar,
/// which is what pins the GEP rather than the admission.
///
/// `l-arr` / `l-vec` are the non-regression controls: the same payloads at the
/// `let`-bound position, correct since B-2026-09-12-6 / B-2026-09-13-29 and
/// unmoved by this commit.
///
/// THE THREE `b-` CELLS ARE THE DELIBERATE BOUNDARY AND MUST STAY SILENT.
/// `b-arrenum` (`Array[Mono, 1]`, a user ENUM element) and `b-tuple` are
/// AGREED silences today, and the gate is narrowed to mirror
/// `run_enum_payload_user_drops_value`'s element dispatch arm for arm so they
/// stay that way: its declared-`Array` arm takes a `Value::Struct` element
/// only, while its declared-`Vec` arm takes a struct OR a non-shared user
/// enum. Asking the emitter's wider `elem_te_runs_user_drop` here instead was
/// measured to make `b-arrenum` print on all three compiled surfaces and
/// nowhere under `--interp` — one fresh divergence bought for three closed,
/// which is the trade 0eba4d1's revert was about. `b-arrenum`'s family is the
/// row's remainder; `b-tuple` is B-2026-09-19-46.
///
/// Byte-identical source and expectation; this side was already CORRECT at
/// every cell, so it is the oracle the compiled half was moved onto rather
/// than a change of its own.
#[test]
fn test_enum_container_payload_in_struct_field_runs_element_drop_bodies() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"d{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"aaa" } }
enum Mono { P(R), Q }
enum Ea { P(Array[R, 2]), Q }
enum Ev { P(Vec[R]), Q }
enum Em { P(Vec[Mono]), Q }
enum En { P(Array[Mono, 1]), Q }
enum Et { P((R, R)), Q }
struct Ha { h: Ea }
struct Hv { h: Ev }
struct Hm { h: Em }
struct Hn { h: En }
struct Ht { h: Et }
struct Hw { lead: i64, h: Ea }

fn main() {
    println("f-arr");   { let a: Array[R, 2] = [mkr(1), mkr(2)]; let g = Ha { h: Ea.P(a) }; println("m") }
    println("f-vec");   { let mut w: Vec[R] = []; w.push(mkr(3)); let g = Hv { h: Ev.P(w) }; println("m") }
    println("f-venum"); { let mut w: Vec[Mono] = []; w.push(Mono.P(mkr(4))); let g = Hm { h: Em.P(w) }; println("m") }
    println("f-bind");  { let a: Array[R, 2] = [mkr(5), mkr(6)]; let h = Ea.P(a); let g = Ha { h: h }; println("m") }
    println("f-second"); { let a: Array[R, 2] = [mkr(7), mkr(8)]; let g = Hw { lead: 9, h: Ea.P(a) }; println("m") }
    println("l-arr");   { let a: Array[R, 2] = [mkr(10), mkr(11)]; let h = Ea.P(a); println("m") }
    println("l-vec");   { let mut w: Vec[R] = []; w.push(mkr(12)); let h = Ev.P(w); println("m") }
    println("b-arrenum"); { let a: Array[Mono, 1] = [Mono.P(mkr(13))]; let g = Hn { h: En.P(a) }; println("m") }
    println("b-tuple");  { let g = Ht { h: Et.P((mkr(14), mkr(15))) }; println("m") }
    println("b-unit");   { let g = Ha { h: Ea.Q }; println("m") }
    println("end")
}
"#);
    assert_eq!(out, "f-arr\nd1\nd2\nm\nf-vec\nd3\nm\nf-venum\nd4\nm\nf-bind\nd5\nd6\nm\nf-second\nd7\nd8\nm\nl-arr\nd10\nd11\nm\nl-vec\nd12\nm\nb-arrenum\nm\nb-tuple\nm\nb-unit\nm\nend\n", "got:\n{out}");
}

/// B-2026-09-17-15 — A GENERIC ENUM'S `shared` PAYLOAD IS NOW RC-RELEASED,
/// AND EVERY CELL IS PINNED BESIDE ITS CONCRETE TWIN, which is the whole
/// design of this fixture rather than decoration.
///
/// `enum Box2[T] { V(T), N }` at `T = shared struct Sh` stranded one 16 B
/// RC control block per value at `-O0` and LOST the payload's `Drop` body
/// on all three compiled surfaces, where `--interp` ran it — a run-vs-build
/// split, not the leak the row was filed as. The cause: `field_drop_kinds`
/// is written once per enum NAME in `declare_enums`, so the payload
/// classified is the bare `T` and takes `enum_drop_kind_for_type_expr`'s
/// `_ => None` tail, which all four of B-2026-09-10-11's gates then read.
///
/// WHY EVERY `G-` CELL HAS A `C-` TWIN. The row's own control is the
/// CONCRETE enum `Et { A(Sh), B }`, correct since that parent fix. Pinning
/// the two spellings in ONE program makes the test assert the property that
/// actually matters — the generic spelling behaves as the concrete one —
/// rather than a transcript somebody re-measured after the fact. Five of the
/// six pairs are byte-identical here and on `--interp`.
///
/// THE TWO PLACES `--interp` DIFFERS ARE PRE-EXISTING AND SHARED BY BOTH
/// SPELLINGS, which is why they are pinned rather than fixed: `G-temp` /
/// `C-temp` both lose the body under `--interp` (a fresh-temp argument to a
/// by-value param), and `G-named` / `C-named` both place it at the end of
/// `main` there instead of at the callee's exit. Measured on the CONCRETE
/// cells alone before this fix, so neither was opened by it. This is the
/// `--interp` half of that pin; `e2e_generic_enum_shared_payload_is_rc_released`
/// in `tests/codegen.rs` runs the same program on the other three surfaces.
///
/// `G-alias` IS THE ONE CELL WHERE THE TWO SPELLINGS STILL DISAGREE, and it
/// is the fix's measured remainder. `let gf = ge` registers nothing: the
/// let-site gate wants a FRESH-owned RHS (`rhs_is_fresh_inline_enum`) and a
/// bound identifier is a move, while `ge` itself is excluded for escaping
/// into `gf`. Registering either without defusing the other is a double
/// free, and the suppression channel that would do it
/// (`var_option_shared_heap`-keyed) is `Option`-only — which is why the
/// `Result[shared]` sibling carries the identical residual. `C-alias` prints
/// `dSh6` because the concrete path is a TYPE-keyed drop fn rather than a
/// per-binding cleanup, so it fires wherever the value lives.
///
/// `sharedenum` IS A DELIBERATELY-HELD AGREED GAP, NOT AN OVERSIGHT.
/// B-2026-09-19-17 withheld the interpreter's own-generic-param exception
/// from an ENUM payload precisely so this cell stays silent on both sides;
/// admitting it in codegen would close a 24 B leak by opening a divergence,
/// and the dec cannot be taken without the body, since releasing the last
/// ref is what reaches `emit_shared_enum_rc_drop_fn`. The
/// `generic_enum_shared_payload_arms` query excludes it for that reason and
/// says so at the site.
///
/// The remaining cells are controls that place the fault: `string` (3 words,
/// so the BOXED path already owned it), `scalar` (fits the area, owns
/// nothing), `unit`, `otherarm` (the non-shared arm of the same two-param
/// enum, which the per-arm tag guard must leave untouched), `plain` (a
/// non-shared struct payload, which rides the bodies walker) and `par`
/// (`shared_types` records `is_par` too, and a fix keyed on `shared` alone
/// would leave it leaking).
#[test]
fn test_generic_enum_shared_payload_is_rc_released() {
    let out = run(r#"shared struct Sh { n: i64 }
impl Drop for Sh { fn drop(mut ref self) { println(f"  dSh{self.n}") } }
par struct Pa { n: i64 }
impl Drop for Pa { fn drop(mut ref self) { println(f"  dPa{self.n}") } }
struct Pl { n: i64 }
impl Drop for Pl { fn drop(mut ref self) { println(f"  dPl{self.n}") } }
shared enum Sen { P(Pl), Q }
enum Box2[T] { V(T), N }
enum Pair[A, B] { L(A), R(B) }
enum Et { A(Sh), B }

fn pg(b: Box2[Sh]) -> Box2[Sh] { return b }
fn pc(b: Et) -> Et { return b }
fn eg(b: Box2[Sh]) { println("  eaten") }
fn ec(b: Et) { println("  eaten") }

fn main() {
    println("G-bare"); { let ga: Box2[Sh] = Box2.V(Sh { n: 1 }); println("  x") }
    println("C-bare"); { let ca: Et = Et.A(Sh { n: 1 }); println("  x") }
    println("G-round"); { let gb = pg(Box2.V(Sh { n: 2 })); println("  x") }
    println("C-round"); { let cb = pc(Et.A(Sh { n: 2 })); println("  x") }
    println("G-arm"); { let gc: Box2[Sh] = Box2.V(Sh { n: 3 }); match gc { Box2.V(s) => { println(f"  got{s.n}") } Box2.N => { println("  no") } } }
    println("C-arm"); { let cc: Et = Et.A(Sh { n: 3 }); match cc { Et.A(s) => { println(f"  got{s.n}") } Et.B => { println("  no") } } }
    println("G-temp"); { eg(Box2.V(Sh { n: 4 })); println("  x") }
    println("C-temp"); { ec(Et.A(Sh { n: 4 })); println("  x") }
    println("G-named"); { let gd: Box2[Sh] = Box2.V(Sh { n: 5 }); eg(gd); println("  x") }
    println("C-named"); { let cd: Et = Et.A(Sh { n: 5 }); ec(cd); println("  x") }
    println("G-alias"); { let ge: Box2[Sh] = Box2.V(Sh { n: 6 }); let gf = ge; println("  x") }
    println("C-alias"); { let ci: Et = Et.A(Sh { n: 6 }); let cj = ci; println("  x") }
    println("par"); { let pz: Box2[Pa] = Box2.V(Pa { n: 7 }); println("  x") }
    println("twoparam"); { let tp: Pair[Sh, i64] = Pair.L(Sh { n: 8 }); println("  x") }
    println("otherarm"); { let oa: Pair[Sh, i64] = Pair.R(9); println("  x") }
    println("unit"); { let uz: Box2[Sh] = Box2.N; println("  x") }
    println("string"); { let sz: Box2[String] = Box2.V("aaaaaaaaa"); println("  x") }
    println("scalar"); { let iz: Box2[i64] = Box2.V(11); println("  x") }
    println("plain"); { let lz: Box2[Pl] = Box2.V(Pl { n: 13 }); println("  x") }
    println("sharedenum"); { let ez: Box2[Sen] = Box2.V(Sen.P(Pl { n: 14 })); println("  x") }
    println("end")
}
"#);
    assert_eq!(out, "G-bare\n  x\n  dSh1\nC-bare\n  x\n  dSh1\nG-round\n  x\n  dSh2\nC-round\n  x\n  dSh2\nG-arm\n  got3\n  dSh3\nC-arm\n  got3\n  dSh3\nG-temp\n  eaten\n  x\nC-temp\n  eaten\n  x\nG-named\n  eaten\n  x\n  dSh5\nC-named\n  eaten\n  x\n  dSh5\nG-alias\n  x\n  dSh6\nC-alias\n  x\n  dSh6\npar\n  x\n  dPa7\ntwoparam\n  x\n  dSh8\notherarm\n  x\nunit\n  x\nstring\n  x\nscalar\n  x\nplain\n  dPl13\n  x\nsharedenum\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-19-21 — a generic callee that hands its boxed payload back
/// INSIDE AN AGGREGATE freed the box twice.
///
/// `fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return
/// H { g: G1.N } }` over `struct H[T] { g: G1[T] }` is the mixed-path callee
/// B-2026-09-17-7 fixed, one wrapping out: the same box comes back, but inside
/// a struct rather than as the return value. That row's runtime compare looks
/// at word 1 of the RETURN, so with a return type of `H[T]` the two shapes did
/// not even agree in type, the compare declined by construction, and the
/// argument kept a box drop the wrapper's field now also owned. Measured
/// `free(): double free detected in tcache 2` against a correct `--interp`, and
/// under valgrind `Invalid read of size 8` then `Invalid free()`.
///
/// `tuple` and `nested` are the same defect through the other two aggregate
/// shapes — a `(G1[T], i64)` return and a struct inside a struct — and both
/// died the same way. They were this row's own NOT MEASURED list and are cells
/// rather than a follow-up because one scan covers all three.
///
/// THE SCAN IS TYPE-IDENTITY-BOUNDED, and `allpaths` is what says the bound is
/// not merely cautious. The disarm now compares against every position inside
/// the returned aggregate whose LLVM type is the argument's own enum type,
/// which is a strictly wider `%same` — and a wider disarm is the direction that
/// STRANDS boxes. `allpaths` (the static all-paths spelling), `structF`,
/// `tupleF` (the dies-inside legs, which return a payload-free variant whose
/// box word is zero), `bare` / `bareF` (the sibling row's own cells, which must
/// not move) and `discard` (the result consumed by nobody, where a disarm would
/// strand the box outright) are all here for that direction. A cell that starts
/// leaking fails the memory twin rather than this one.
///
/// Valgrind on this program, `-O0` / `KARAC_AUTO_PAR=0`: 12 errors from 12
/// contexts before, `no leaks are possible` after.
///
/// This side was correct throughout — it is the oracle the compiled cells are
/// measured against — so this fixture pins the expectation rather than a fix.
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_generic_callee_hands_its_boxed_payload_back_inside_an_aggregate`,
/// byte-identical source and expectation.
#[test]
fn test_generic_callee_hands_its_boxed_payload_back_inside_an_aggregate() {
    let out = run(r#"enum G1[T] { Y(T), N }
struct H[T] { g: G1[T] }
struct H2[T] { h: H[T] }
fn wrap[T](g: G1[T], c: bool) -> H[T] { if c { return H { g: g } } return H { g: G1.N } }
fn wrapAll[T](g: G1[T]) -> H[T] { return H { g: g } }
fn wrapTup[T](g: G1[T], c: bool) -> (G1[T], i64) { if c { return (g, 7) } return (G1.N, 7) }
fn wrapNest[T](g: G1[T], c: bool) -> H2[T] { if c { return H2 { h: H { g: g } } } return H2 { h: H { g: G1.N } } }
fn bare[T](g: G1[T], c: bool) -> G1[T] { if c { return g } return G1.N }
fn shw(g: G1[String]) { match g { G1.Y(v) => { println(f"  mx {v.len()}") } G1.N => { println("  mx 0") } } }

fn main() {
    println("struct");  { let g: G1[String] = G1.Y(f"aaaaaaaa-1"); let h = wrap(g, true); shw(h.g) }
    println("structF"); { let g: G1[String] = G1.Y(f"aaaaaaaa-2"); let h = wrap(g, false); shw(h.g) }
    println("allpaths");{ let g: G1[String] = G1.Y(f"aaaaaaaa-3"); let h = wrapAll(g); shw(h.g) }
    println("tuple");   { let g: G1[String] = G1.Y(f"aaaaaaaa-4"); let t = wrapTup(g, true); shw(t.0) }
    println("tupleF");  { let g: G1[String] = G1.Y(f"aaaaaaaa-5"); let t = wrapTup(g, false); shw(t.0) }
    println("nested");  { let g: G1[String] = G1.Y(f"aaaaaaaa-6"); let h = wrapNest(g, true); shw(h.h.g) }
    println("bare");    { let g: G1[String] = G1.Y(f"aaaaaaaa-7"); let b = bare(g, true); shw(b) }
    println("bareF");   { let g: G1[String] = G1.Y(f"aaaaaaaa-8"); let b = bare(g, false); shw(b) }
    println("discard"); { let g: G1[String] = G1.Y(f"aaaaaaaa-9"); wrap(g, true); println("  x") }
    println("end")
}
"#);
    assert_eq!(out, "struct\n  mx 10\nstructF\n  mx 0\nallpaths\n  mx 10\ntuple\n  mx 10\ntupleF\n  mx 0\nnested\n  mx 10\nbare\n  mx 10\nbareF\n  mx 0\ndiscard\n  x\nend\n", "got:\n{out}");
}

/// B-2026-09-17-7 — a generic callee that MAY hand its boxed payload back
/// freed the box twice, and the check that stops it was blind inside braces.
///
/// `fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.N }`
/// returns its own parameter on one leg and lets it die inside on the other.
/// Both legs belong to ONE call site, so no static answer there is right for
/// both: disarming the argument strands the box when the callee kept it, and
/// leaving it armed frees the box twice when the callee handed it back. The
/// `handback` cell measured the second — no stdout at all, `Invalid read of
/// size 8`, the caller freeing `a`'s box after `b`'s box, one pointer.
///
/// The answer is a runtime compare, the cheapest dynamic form: after the call
/// the caller zeroes the argument's slot only when the returned box word IS
/// the word that went in. It cannot be wrong in the suppressing direction,
/// which is what makes the UNION predicate `fn_returns_param` usable here
/// where the all-paths one was needed before — a return that merely moves the
/// parameter into a new value carries a different word, the compare fails, and
/// the argument keeps the drop it has today. `keptinside`, `diesinside` and
/// `mono` are the negative cells.
///
/// The second half is why every cell here is BRACED. The window that tells
/// that disarm "nothing consumes this result" was armed over a discarded
/// statement's whole expression, so a braced block written as a statement
/// covered every call inside it — including calls whose result a `let` binding
/// owns outright. The flat twin of `handback` was clean while the braced one
/// died at the block's exit. The window now follows a block to its tail, the
/// only part of it whose value the statement throws away; `discarded` and
/// `blocktail` are the cells that keep it armed where it does belong, and each
/// leaks one box if it stops firing.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_generic_callee_may_hand_its_boxed_payload_back`, byte-identical source and expectation.
#[test]
fn test_generic_callee_may_hand_its_boxed_payload_back() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
enum G[T] { Y(T), N }
enum M { Y(String), N }
fn mid[T](g: G[T], c: bool) -> G[T] { if c { return g } return G.N }
fn allpaths[T](g: G[T]) -> G[T] { return g }
fn diesinside[T](g: G[T]) -> G[T] { return G.N }
fn tailmatch[T](g: G[T], c: i64) -> G[T] { match c { 1 => { return g } _ => { return G.N } } }
fn pick[T](a: G[T], b: G[T], c: bool) -> G[T] { if c { return a } return G.N }
fn midmono(g: M, c: bool) -> M { if c { return g } return M.N }
fn show[T](g: G[String]) { match g { G.Y(v) => { println(f"  {v.len()}") } G.N => { println("  none") } } }

fn main() {
    println("handback");  { let a: G[String] = G.Y(f"aaaaaaaa-1"); let b = mid(a, true); show(b) }
    println("keptinside");{ let a: G[String] = G.Y(f"bbbbbbbb-2"); let b = mid(a, false); show(b) }
    println("allpaths");  { let a: G[String] = G.Y(f"cccccccc-3"); let b = allpaths(a); show(b) }
    println("diesinside");{ let a: G[String] = G.Y(f"dddddddd-4"); let b = diesinside(a); show(b) }
    println("discarded"); { let a: G[String] = G.Y(f"eeeeeeee-5"); mid(a, true); println("  out") }
    println("unread");    { let a: G[String] = G.Y(f"ffffffff-6"); let b = mid(a, true); println("  out") }
    println("userdrop");  { let a: G[R] = G.Y(R { id: 7, s: f"ssssssss" }); let b = mid(a, true); println("  out") }
    println("i64");       { let a: G[i64] = G.Y(5); let b = mid(a, true); match b { G.Y(v) => { println(f"  {v}") } G.N => { println("  none") } } }
    println("twoargs");   { let x: G[String] = G.Y(f"iiiiiiii-9"); let y: G[String] = G.Y(f"jjjjjjjj-10"); let b = pick(x, y, true); show(b) }
    println("vecpayload");{ let a: G[Vec[String]] = G.Y([f"kkkkkkkk-11", f"llllllll-12"]); let b = mid(a, true); match b { G.Y(v) => { println(f"  {v.len()}") } G.N => { println("  none") } } }
    println("arrpayload");{ let a: G[Array[String, 2]] = G.Y([f"mmmmmmmm-13", f"nnnnnnnn-14"]); let b = mid(a, true); match b { G.Y(v) => { println("  arr") } G.N => { println("  none") } } }
    println("matchtail"); { let a: G[String] = G.Y(f"oooooooo-15"); let b = tailmatch(a, 1); show(b) }
    println("chain");     { let a: G[String] = G.Y(f"pppppppp-16"); let b = mid(mid(a, true), true); show(b) }
    println("blocktail"); { let a: G[String] = G.Y(f"rrrrrrrr-18"); mid(a, true) }; println("  out")
    println("mono");      { let a: M = M.Y(f"qqqqqqqq-17"); let b = midmono(a, true); match b { M.Y(v) => { println(f"  {v.len()}") } M.N => { println("  none") } } }
    println("end")
}
"#);
    assert_eq!(out, "handback\n  10\nkeptinside\n  none\nallpaths\n  10\ndiesinside\n  none\ndiscarded\n  out\nunread\n  out\nuserdrop\n  dR7\n  out\ni64\n  5\ntwoargs\n  10\nvecpayload\n  2\narrpayload\n  arr\nmatchtail\n  11\nchain\n  11\nblocktail\n  out\nmono\n  11\nend\n", "got:\n{out}");
}

/// B-2026-09-17-8 — compiling a monomorph mid-caller wiped the CALLER's
/// payload-ownership records, and the caller then gave the same payload two
/// owners.
///
/// Nine registries are cleared at the mono body entry and none of them was
/// swapped out around the nested compile, so the clear was one-way.
/// `let b = idOpt(a);` over `fn idOpt[T](g: Option[T]) -> Option[T] { return
/// g }` records `passthrough_owner_alias[b] = a` and registers no owner for
/// `b`, on B-2026-08-06-27's rule that the source stays sole owner — but only
/// while `a` is still in `inline_option_payload_vars` when that `let`
/// compiles. The monomorph compiled for that very call had emptied the set, so
/// the alias was never recorded and `b` took a second registration over `a`'s
/// payload: `free(): double free detected in tcache 2`, 11 allocs / 12 frees.
/// `monoinline`, `monobody` and `nocall` are the cells that were always clean,
/// and the first two are clean for one reason — nothing clears the set.
///
/// The second half is the BODIES channel, which the memory fix exposed rather
/// than caused. `compile_call` has retracted a handed-back argument's payload
/// walk since B-2026-08-09-15; `compile_generic_call` never did, so with the
/// source correctly registered again both `a` and `b` fired it — `dD / got 6 /
/// dD` against the interpreter's `got 6 / dD`, on balanced memory. `body`,
/// `bodyboxed` and `bodynoarm` are the three cells that double without it, and
/// they double across both payload classes: `D` is one word and rides the
/// inline channel, `W` is four and boxes.
///
/// `twocalls` is the cell that refutes the argument-side disarm this row first
/// reached for: standing the argument down fixes the first call to a monomorph
/// and leaks every later one, because the inline channel's contract is the
/// opposite of the boxed one — there the source is the sole owner, not the
/// second.
///
/// The CODEGEN twin is `tests/codegen.rs`'s
/// `e2e_generic_call_keeps_the_callers_payload_ownership`, byte-identical source and expectation.
#[test]
fn test_generic_call_keeps_the_callers_payload_ownership() {
    let out = run(r#"struct D { id: i64 }
impl Drop for D { fn drop(mut ref self) { println(f"  dD{self.id}") } }
struct W { id: i64, s: String }
impl Drop for W { fn drop(mut ref self) { println(f"  dW{self.id}") } }
fn idOpt[T](g: Option[T]) -> Option[T] { return g }
fn idRes[T](g: Result[T, i64]) -> Result[T, i64] { return g }
fn idOptMono(g: Option[String]) -> Option[String] { return g }
fn idDMono(g: Option[D]) -> Option[D] { return g }

fn main() {
    println("inline");   { let a: Option[String] = Option.Some(f"aaaaaaaa-1"); let b = idOpt(a); match b { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("result");   { let a: Result[String, i64] = Result.Ok(f"bbbbbbbb-2"); let b = idRes(a); match b { Result.Ok(v) => { println(f"  {v.len()}") } Result.Err(e) => { println("  err") } } }
    println("noarm");    { let a: Option[String] = Option.Some(f"cccccccc-3"); let b = idOpt(a); println("  out") }
    println("twocalls"); { let a: Option[String] = Option.Some(f"dddddddd-4"); let x = idOpt(a); let c: Option[String] = Option.Some(f"eeeeeeee-5"); let y = idOpt(c); match x { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } match y { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("body");     { let a: Option[D] = Option.Some(D { id: 6 }); let b = idOpt(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("bodyboxed");{ let a: Option[W] = Option.Some(W { id: 7, s: f"ssssssss" }); let b = idOpt(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("bodynoarm");{ let a: Option[W] = Option.Some(W { id: 8, s: f"tttttttt" }); let b = idOpt(a); println("  out") }
    println("monoinline"); { let a: Option[String] = Option.Some(f"ffffffff-9"); let b = idOptMono(a); match b { Option.Some(v) => { println(f"  {v.len()}") } Option.None => { println("  none") } } }
    println("monobody");   { let a: Option[D] = Option.Some(D { id: 10 }); let b = idDMono(a); match b { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("nocall");     { let a: Option[D] = Option.Some(D { id: 11 }); match a { Option.Some(v) => { println(f"  got {v.id}") } Option.None => { println("  none") } } }
    println("end")
}
"#);
    assert_eq!(out, "inline\n  10\nresult\n  10\nnoarm\n  out\ntwocalls\n  10\n  10\nbody\n  got 6\n  dD6\nbodyboxed\n  got 7\n  dW7\nbodynoarm\n  dW8\n  out\nmonoinline\n  10\nmonobody\n  got 10\n  dD10\nnocall\n  got 11\n  dD11\nend\n", "got:\n{out}");
}

/// B-2026-09-17-31 — the interpreter half of the method-call payload-part
/// stand-down, and the half that actually moved: every `method-*` cell below
/// printed the handed-out part's body and NOT the untouched sibling's, where
/// `free-fn` — the identical body as a free function — was correct. The
/// compiled twin is `tests/codegen.rs`'s
/// `e2e_method_call_keeps_an_unmoved_payload_parts_drop_body`, which asserts
/// the same shapes against both backends.
///
/// `ctl-whole` is the control with teeth: the callee returns the ARGUMENT, so
/// the whole-argument stand-down the fix narrows must still fire, and both
/// bodies must stay with the result rather than doubling here.
#[test]
fn test_method_call_keeps_an_unmoved_payload_parts_drop_body() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct Q { r: R, s: R }
struct H { n: i64 }
impl H {
  fn mref(ref self, o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } }
  fn mown(self, o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } }
  fn mres(ref self, o: Result[(R, R), i64]) -> R { match o { Result.Ok(t) => { return t.0 } Result.Err(e) => { return R { id: 0 } } } }
  fn msnd(ref self, o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.1 } Option.None => { return R { id: 0 } } } }
  fn mstr(ref self, o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } }
  fn mdes(ref self, o: Option[(R, R)]) -> R { match o { Option.Some((a, b)) => { return a } Option.None => { return R { id: 0 } } } }
  fn mnom(ref self, o: Option[(R, R)]) { match o { Option.Some(t) => { println("  mid") } Option.None => { println("  n") } } }
  fn mwho(ref self, o: Option[(R, R)]) -> Option[(R, R)] { return o }
}
struct A {}
impl A { fn atup(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } } }
fn ffn(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } }

fn main() {
  println("free-fn");       { let g = ffn(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("method-ref");    { let h = H { n: 1 }; let g = h.mref(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("method-owned");  { let h = H { n: 1 }; let g = h.mown(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("method-result"); { let h = H { n: 1 }; let g = h.mres(Result.Ok((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("method-second"); { let h = H { n: 1 }; let g = h.msnd(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("method-struct"); { let h = H { n: 1 }; let g = h.mstr(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("method-destr");  { let h = H { n: 1 }; let g = h.mdes(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("ctl-nomove");    { let h = H { n: 1 }; h.mnom(Option.Some((R { id: 5 }, R { id: 6 }))) } println("  out")
  println("ctl-whole");     { let h = H { n: 1 }; let g = h.mwho(Option.Some((R { id: 5 }, R { id: 6 }))); println("  kept") } println("  out")
  println("ctl-assoc");     { let g = A.atup(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("end")
}
"#);
    assert_eq!(out, "free-fn\n  dR6\n  got5\n  dR5\n  out\nmethod-ref\n  dR6\n  got5\n  dR5\n  out\nmethod-owned\n  dR6\n  got5\n  dR5\n  out\nmethod-result\n  dR6\n  got5\n  dR5\n  out\nmethod-second\n  dR5\n  got6\n  dR6\n  out\nmethod-struct\n  dR6\n  got5\n  dR5\n  out\nmethod-destr\n  dR6\n  got5\n  dR5\n  out\nctl-nomove\n  mid\n  dR5\n  dR6\n  out\nctl-whole\n  dR5\n  dR6\n  kept\n  out\nctl-assoc\n  dR6\n  got5\n  dR5\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-19-55 — the interpreter half of the duplicate-associated-name
/// payload-part fix, and the half that actually moved: every `dup-*` cell below
/// ran the UNTAKEN field's `Drop` body twice, where the same program with a
/// unique name, or with a free function, or with the instance-method spelling,
/// printed it once. The compiled twin is `tests/codegen.rs`'s
/// `e2e_duplicate_assoc_fn_name_keeps_one_payload_part_drop_body`, which
/// asserts the same shapes against both backends.
///
/// `peer` is the control with teeth: the two same-named functions hand out
/// DIFFERENT fields, so a resolution that merely stopped failing — rather than
/// resolving exactly — would keep the wrong sibling alive and be visible here.
#[test]
fn test_duplicate_assoc_fn_name_keeps_one_payload_part_drop_body() {
    let out = run(r#"struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}") } }
struct Q { r: R, s: R }
struct Bx { v: R }
struct A {}
struct B {}
struct C {}
struct Z {}
struct H { n: i64 }
impl A { fn dup(o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } } }
impl B { fn dup(o: Option[Q]) -> R { match o { Option.Some(t) => { return t.s } Option.None => { return R { id: 0 } } } } }
impl C { fn dup(o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } } }
impl A { fn tdup(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } } }
impl B { fn tdup(o: Option[(R, R)]) -> R { match o { Option.Some(t) => { return t.0 } Option.None => { return R { id: 0 } } } } }
impl A { fn rdup(o: Result[Q, i64]) -> R { match o { Result.Ok(t) => { return t.r } Result.Err(e) => { return R { id: 0 } } } } }
impl B { fn rdup(o: Result[Q, i64]) -> R { match o { Result.Ok(t) => { return t.r } Result.Err(e) => { return R { id: 0 } } } } }
impl A { fn whole(o: Option[Q]) -> Option[Q] { o } }
impl B { fn whole(o: Option[Q]) -> Option[Q] { o } }
impl H { fn dup(ref self, o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } } }
impl Z { fn zdup(o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } } }
fn dup(o: Option[Q]) -> R { match o { Option.Some(t) => { return t.r } Option.None => { return R { id: 0 } } } }
fn sink(r: R) { println(f"  sank{r.id}") }
fn main() {
  println("dup-a");      { let g = A.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("dup-c");      { let g = C.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("peer");       { let g = B.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("dup-tuple");  { let g = A.tdup(Option.Some((R { id: 5 }, R { id: 6 }))); println(f"  got{g.id}") } println("  out")
  println("dup-result"); { let g = A.rdup(Result.Ok(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("discard");    { A.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println("  after") } println("  out")
  println("field");      { let b = Bx { v: A.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })) }; println(f"  in{b.v.id}") } println("  out")
  println("to-callee");  { sink(A.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } }))) } println("  out")
  println("loop");       { let mut i = 0; while i < 2 { let g = A.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  it{g.id}"); i = i + 1; } } println("  out")
  println("method");     { let h = H { n: 1 }; let g = h.dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("ctl-uniq");   { let g = Z.zdup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("ctl-free");   { let g = dup(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println(f"  got{g.id}") } println("  out")
  println("ctl-whole");  { let g = A.whole(Option.Some(Q { r: R { id: 5 }, s: R { id: 6 } })); println("  kept") } println("  out")
  println("end")
}
"#);
    assert_eq!(out, "dup-a\n  dR6\n  got5\n  dR5\n  out\ndup-c\n  dR6\n  got5\n  dR5\n  out\npeer\n  dR5\n  got6\n  dR6\n  out\ndup-tuple\n  dR6\n  got5\n  dR5\n  out\ndup-result\n  dR6\n  got5\n  dR5\n  out\ndiscard\n  dR6\n  dR5\n  after\n  out\nfield\n  dR6\n  in5\n  dR5\n  out\nto-callee\n  dR6\n  sank5\n  dR5\n  out\nloop\n  dR6\n  it5\n  dR5\n  dR6\n  it5\n  dR5\n  out\nmethod\n  dR6\n  got5\n  dR5\n  out\nctl-uniq\n  dR6\n  got5\n  dR5\n  out\nctl-free\n  dR6\n  got5\n  dR5\n  out\nctl-whole\n  dR6\n  dR5\n  kept\n  out\nend\n", "got:\n{out}");
}

/// B-2026-09-20-63 (interpreter twin) — the reference answers for a
/// constructor used DIRECTLY as a `match` scrutinee, pinned so the codegen fix
/// beside it cannot drift onto one backend alone.
///
/// Two of these cells are AGREED GAPS rather than correct answers, and they are
/// pinned as such on purpose. An arm that DISCARDS its payload runs no element
/// body on any surface, and the declared-container spelling
/// `enum Ev { V(Vec[R]), N }` runs none either; arming the compiled side of
/// either would trade an agreement for a run-vs-build divergence, which is
/// strictly worse. The `Vec`-at-a-binding-arm cell is the reverse: the
/// interpreter is CORRECT there and the compiled surfaces are not, which is the
/// half of B-2026-09-20-63 its fix does not close.
#[test]
fn test_freshtemp_generic_enum_scrutinee_payload_drop_bodies() {
    let hdr = "struct R { id: i64 }\n\
               impl Drop for R { fn drop(mut ref self) { println(f\"dR{self.id}\") } }\n\
               fn mkr(i: i64) -> R { return R { id: i }; }\n\
               enum Slot[T] { S(T), N }\n\
               enum Ev { V(Vec[R]), N }\n";
    for (label, stmts, want) in [
        (
            "Array payload, fresh ctor temp, binding arm — the cell the codegen \
             half of B-2026-09-20-63 brings the compiled surfaces up to",
            "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
            "x1\ndR1\ndR2\nend\n",
        ),
        (
            "the same, with a GUARD and two binding arms",
            "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) {\n\
               Slot.S(v) if v[0].id > 99 => { println(f\"big{v[0].id}\") }\n\
               Slot.S(w) => { println(f\"x{w[1].id}\") }\n\
               Slot.N => { println(\"no\") }\n\
             }",
            "x2\ndR1\ndR2\nend\n",
        ),
        (
            "AGREED GAP: an arm that DISCARDS its payload runs no element body on \
             any surface. Pinned at the agreed answer, not at the due one",
            "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) { Slot.S(_) => { println(\"x\") } Slot.N => { println(\"no\") } }",
            "x\nend\n",
        ),
        (
            "the Vec spelling. This was a PINNED DIVERGENCE when the fixture was \
             written — correct here, silent on the three compiled surfaces — and \
             B-2026-09-21-11 closed it by giving the bodies to the arm's binding, \
             so the four now agree at this value",
            "let a: Vec[R] = [mkr(1), mkr(2)];\n\
             match Slot.S(a) { Slot.S(v) => { println(f\"x{v[0].id}\") } Slot.N => { println(\"no\") } }",
            "x1\ndR1\ndR2\nend\n",
        ),
        (
            "AGREED GAP: the DECLARED-container spelling at the same position",
            "let a: Vec[R] = [mkr(1), mkr(2)];\n\
             match Ev.V(a) { Ev.V(v) => { println(f\"x{v[0].id}\") } Ev.N => { println(\"no\") } }",
            "x1\nend\n",
        ),
        (
            "control: a whole-value discard of the same ctor, correct everywhere",
            "let a: Array[R, 2] = [mkr(1), mkr(2)];\n\
             let _ = Slot.S(a);",
            "dR1\ndR2\nend\n",
        ),
    ] {
        let src = format!("{hdr}fn main() {{\n{stmts}\nprintln(\"end\");\n}}\n");
        assert_eq!(run(&src), want, "[{label}]");
    }
}

/// B-2026-09-20-12 — a by-value enum argument spelled as a FIELD
/// PROJECTION runs its payload's `Drop` body ONCE.
///
/// `eat(b.w)` over an enum the callee owns BY TRANSFER fired the payload's
/// body twice: once in the callee, which owns it, and once again in the
/// caller when the holder died. The discriminator was a SIBLING VARIANT
/// that is never constructed, never matched and never passed — `Wpr`'s
/// `S(Inner)` classifies `SharedRc`, which makes
/// `enum_param_owned_by_transfer` answer TRUE for the whole TYPE, so a
/// value whose live variant is the inline `A(Array[Sp, 1])` is declared
/// callee-owned. `Wps`, identical but for that variant, is correct and
/// stays correct here (`b:7 dSp41`).
///
/// TWO CHANNELS, AND ONLY ONE WAS STOOD DOWN. B-2026-09-19-51 already
/// zeroes the handed-over field's payload words, which neutralizes
/// `__karac_drop_struct_<S>`'s FREE of it. The holder's field BODIES live
/// in a separate per-binding `__karac_dropbodies_<S>` walker fired from its
/// own `CleanupAction`, which the zero cannot reach — so the second body
/// still ran, over the zeroed words, printing `dSp0`. On an element one
/// word wide that OWNS heap it would read a live pointer instead, which is
/// why the row is filed as a use-after-free rather than as noise.
/// `zero_transfer_owned_enum_field_arg` now masks the field out of that
/// walker as well, through the same `disarm_struct_field_bodies_at` the
/// `let x = h.o` move-out sites use.
///
/// THE MASK IS PER FIELD, and four cells here exist to pin that rather
/// than to restate the fault: a SIBLING enum field (`e`), a plain
/// `Drop`-bearing field (`h`), a NON-transfer enum field beside a transfer
/// one (`i`), and both fields handed over in turn (`f`/`g`). Each
/// survivor's body still runs exactly once. Before the fix every one of
/// these carried a trailing `dSp0`; `j`, where nothing is transfer-owned,
/// is byte-identical on both arms and is the control.
///
/// ORDER IS NOT THIS ROW. The three compiled surfaces agree exactly, and
/// `--interp` prints the same 25 lines in a different order: a
/// transfer-owned argument is dropped by the CALLEE on the compiled
/// backends and at the caller's statement end under `--interp`. That split
/// predates this fix — it is `named-local` (`c`) and `temporary` (`d`)
/// here, both of which this row calls correct — and it is B-2026-09-15-17,
/// whose own prose names the same lever. The multiset of lines is equal on
/// all four surfaces; only the sequence differs, so the twin in
/// `tests/interpreter.rs` pins the interpreter's order deliberately.
#[test]
fn transfer_owned_enum_field_arg_runs_its_payload_body_once() {
    let src = r#"
struct Sp { v: i64 }
impl Drop for Sp { fn drop(mut ref self) { println(f"dSp{self.v}") } }
shared struct Inner { tag: String }

enum Wpr { A(Array[Sp, 1]), S(Inner), N }
enum Wps { A(Array[Sp, 1]), N }

struct Br { w: Wpr, n: i64 }
struct Bs { w: Wps, n: i64 }
struct Two { w: Wpr, u: Wpr, n: i64 }
struct Mix { w: Wpr, s: Sp, n: i64 }
struct MixNT { w: Wpr, v: Wps, n: i64 }

fn eat_r(g: Wpr) -> i64 { match g { Wpr.A(x) => { return 7; } Wpr.S(i) => { return 1; } Wpr.N => { return 0; } } }
fn eat_s(g: Wps) -> i64 { match g { Wps.A(x) => { return 7; } Wps.N => { return 0; } } }

fn proj_sib() { let b: Br = Br { w: Wpr.A([Sp { v: 42 }]), n: 1 }; println(f"a:{eat_r(b.w)}"); }
fn proj_nosib() { let b: Bs = Bs { w: Wps.A([Sp { v: 41 }]), n: 1 }; println(f"b:{eat_s(b.w)}"); }
fn named_local() { let b: Br = Br { w: Wpr.A([Sp { v: 43 }]), n: 1 }; let w2: Wpr = b.w; println(f"c:{eat_r(w2)}"); }
fn temporary() { println(f"d:{eat_r(Wpr.A([Sp { v: 44 }]))}"); }
fn sibling_field() { let t: Two = Two { w: Wpr.A([Sp { v: 51 }]), u: Wpr.A([Sp { v: 52 }]), n: 1 }; println(f"e:{eat_r(t.w)}"); }
fn both_handed() { let t: Two = Two { w: Wpr.A([Sp { v: 53 }]), u: Wpr.A([Sp { v: 54 }]), n: 1 }; println(f"f:{eat_r(t.w)}"); println(f"g:{eat_r(t.u)}"); }
fn plain_field() { let m: Mix = Mix { w: Wpr.A([Sp { v: 55 }]), s: Sp { v: 56 }, n: 1 }; println(f"h:{eat_r(m.w)}"); }
fn nt_sibling() { let m: MixNT = MixNT { w: Wpr.A([Sp { v: 57 }]), v: Wps.A([Sp { v: 58 }]), n: 1 }; println(f"i:{eat_r(m.w)}"); }
fn nt_only() { let m: MixNT = MixNT { w: Wpr.A([Sp { v: 60 }]), v: Wps.A([Sp { v: 61 }]), n: 1 }; println(f"j:{eat_s(m.v)}"); }

fn main() {
    proj_sib();
    proj_nosib();
    named_local();
    temporary();
    sibling_field();
    both_handed();
    plain_field();
    nt_sibling();
    nt_only();
    println("end");
}
"#;
    assert_eq!(
        run(src),
        "a:7\ndSp42\nb:7\ndSp41\nc:7\ndSp43\ndSp44\nd:7\ne:7\ndSp52\ndSp51\nf:7\ng:7\ndSp54\ndSp53\nh:7\ndSp56\ndSp55\ni:7\ndSp58\ndSp57\nj:7\ndSp61\ndSp60\nend\n"
    );
}

/// B-2026-09-21-5 — interpreter twin of
/// `test_e2e_generic_ref_param_over_generic_enum_reads_its_payload`.
///
/// The interpreter is the ORACLE for that row rather than a second suspect:
/// the fault is memory-balanced and exits 0 on every compiled surface, so an
/// A/B against this backend is the only instrument that can see it. This twin
/// is not therefore vacuous — it pins the oracle, so a later change that moves
/// the interpreter would be caught here instead of silently redefining what
/// "correct" means for the codegen cell. Same program, same expectation,
/// byte-for-byte.
#[test]
fn generic_ref_param_over_generic_enum_reads_its_payload() {
    let src = r#"
trait Num {
    fn get(ref self) -> i64;
}
struct W { n: i64 }
impl Num for W {
    fn get(ref self) -> i64 { self.n }
}

enum G1[T] { Y(T), N }

fn shr[T](g: ref G1[T]) { match g { G1.Y(v) => { println(f"  rx {v}") } G1.N => { println("  rx NONE") } } }
fn shm[T](g: mut ref G1[T]) { match g { G1.Y(v) => { println(f"  mx {v}") } G1.N => { println("  mx NONE") } } }
fn shg[T](g: G1[T]) { match g { G1.Y(v) => { println(f"  vx {v}") } G1.N => { println("  vx NONE") } } }
fn shw[T: Num](g: ref G1[T]) { match g { G1.Y(v) => { println(f"  wx {v.get()}") } G1.N => { println("  wx NONE") } } }

impl[T] G1[T] {
    fn shs(ref self) -> i64 { match self { G1.Y(v) => { println(f"  sx {v}"); 1 } G1.N => { println("  sx NONE"); 0 } } }
}

fn a_ref_string() { println("a"); let g: G1[String] = G1.Y(f"pa"); shr(g); shr(g) }
fn b_mutref_string() { println("b"); let mut g: G1[String] = G1.Y(f"pb"); shm(mut g) }
fn c_value_string() { println("c"); let g: G1[String] = G1.Y(f"pc"); shg(g) }
fn d_ref_i64() { println("d"); let g: G1[i64] = G1.Y(77); shr(g); shr(g) }
fn e_refself_string() { println("e"); let g: G1[String] = G1.Y(f"pe"); let r = g.shs(); println(f"  r={r}") }
fn f_ref_struct() { println("f"); let g: G1[W] = G1.Y(W { n: 5 }); shw(g); shw(g) }
fn g_ref_none() { println("g"); let g: G1[String] = G1.N; shr(g) }

fn main() {
    a_ref_string();
    b_mutref_string();
    c_value_string();
    d_ref_i64();
    e_refself_string();
    f_ref_struct();
    g_ref_none();
    println("end");
}
"#;
    assert_eq!(run(src), "a\n  rx pa\n  rx pa\nb\n  mx pb\nc\n  vx pc\nd\n  rx 77\n  rx 77\ne\n  sx pe\n  r=1\nf\n  wx 5\n  wx 5\ng\n  rx NONE\nend\n");
}

/// B-2026-09-20-62 — the interpreter twin of
/// `e2e_generic_enum_container_payload_positions` in `tests/codegen.rs`.
///
/// GENERATED FROM THAT FIXTURE'S OWN TEXT rather than typed beside it, so the
/// two files are one transcription and cannot drift on a program or an
/// expectation. Every cell below ran on all four surfaces and they agreed
/// byte for byte; this file is what holds `--interp` to that agreement, since
/// the codegen fixture's own `run_program` never reaches the interpreter.
///
/// The two SILENT cells matter here most: three-deep nesting and a two-field
/// variant are silent on every surface by design, and the interpreter is the
/// side that would most easily start printing — it holds the concrete value
/// and needs no instantiation to walk it. If either fires here, this side has
/// been widened past the compiled one.
#[test]
fn generic_enum_container_payload_positions() {
    for (label, src, want) in [
        (
            "a DISCARDED value — no binding at all, so no instantiation is recorded for one (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let _ = Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "a BARE STATEMENT of the same constructor (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "a BLOCK-scoped binding: the bodies land at the live-range end, before the block's own output (before: in|mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
{ let s: Slot[Vec[R]] = Slot.S(a); println("in"); }
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nin\nmid\nend\n",
        ),
        (
            "a BY-VALUE PARAM scrutinee at a read-only arm — the binding holds the buffer here too (before: x1|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
seenv(Slot.S(a));
println("end");
}
"#,
            "x1\ndR1\ndR2\nend\n",
        ),
        (
            "ENUM elements: own body first, then the element's payload (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[Ew] = [Ew.Z(mkr(1)), Ew.Z(mkr(2))];
let s: Slot[Vec[Ew]] = Slot.S(a);
println("mid");
println("end");
}
"#,
            "dER\ndR1\ndER\ndR2\nmid\nend\n",
        ),
        (
            "one container NESTED inside another (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let i1: Vec[R] = [mkr(1)];
let i2: Vec[R] = [mkr(2)];
let a: Vec[Vec[R]] = [i1, i2];
let s: Slot[Vec[Vec[R]]] = Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "MIXED-WIDTH enum, Vec instantiation: the variant's own width decides boxing, not the enum's area (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let s: Mix[Vec[R]] = Mix.A(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "MIXED-WIDTH enum, Array instantiation — printed an ASLR-varying id before this fix (before: interp dR1|dR2|mid|end| vs compiled dR<addr>|dR0|mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
let s: Mix[Array[R, 2]] = Mix.A(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "CONTROL, the Array twin of the discard cell: compiled-correct before, interpreted-silent (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
let _ = Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "CONTROL, the Array twin of the bare statement (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let a: Array[R, 2] = [mkr(1), mkr(2)];
Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "CONTROL, the Array twin of the nesting cell (before: interp mid|end| vs compiled dR1|dR2|mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }
enum Mix[T] { A(T), B(Vec[T]), N }

fn main() {
let i1: Array[R, 1] = [mkr(1)];
let i2: Array[R, 1] = [mkr(2)];
let a: Array[Array[R, 1], 2] = [i1, i2];
let s: Slot[Array[Array[R, 1], 2]] = Slot.S(a);
println("mid");
println("end");
}
"#,
            "dR1\ndR2\nmid\nend\n",
        ),
        (
            "AGREED SILENCE, three deep: `elem_te_runs_user_drop` stops at one level and so does the walk (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
enum Slot[T] { S(T), N }

fn main() {
let a1: Vec[R] = [mkr(1)];
let b1: Vec[Vec[R]] = [a1];
let c1: Vec[Vec[Vec[R]]] = [b1];
let s: Slot[Vec[Vec[Vec[R]]]] = Slot.S(c1);
println("mid");
println("end");
}
"#,
            "mid\nend\n",
        ),
        (
            "AGREED SILENCE, a TWO-FIELD variant: the walker head skips it, so neither side may fire (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[R] = [mkr(1), mkr(2)];
let s: G2[Vec[R]] = G2.X(a, 7);
println("mid");
println("end");
}
"#,
            "mid\nend\n",
        ),
        (
            "CONTROL, elements with no body: nothing is due and nothing runs (before: mid|end|)",
            r#"
struct R { id: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i }; }
struct P { id: i64 }
enum Ew { Z(R), N }
impl Drop for Ew { fn drop(mut ref self) { println("dER") } }
enum Slot[T] { S(T), N }
enum EVecG[T] { V(Vec[T]), N }
enum Mix[T] { A(T), B(Vec[T]), N }
enum G2[T] { X(T, i64), Y }
fn seenv(x: Slot[Vec[R]]) { match x { Slot.S(v) => { println(f"x{v[0].id}") } Slot.N => { println("no") } } }

fn main() {
let a: Vec[P] = [P { id: 1 }, P { id: 2 }];
let s: Slot[Vec[P]] = Slot.S(a);
println("mid");
println("end");
}
"#,
            "mid\nend\n",
        ),
    ] {
        assert_eq!(run(src), want, "[{label}]");
    }
}

/// B-2026-09-25-16 — a GENERIC user enum returned by a call and passed straight
/// on (`takeit(mkg(mkr(9)))` over `fn mkg(v: R) -> Ho[R] { return Ho.Full(v) }`)
/// ran its payload's `Drop` body TWICE on every surface: once inside `mkg`, which
/// dropped its own param instead of handing it back through `Ho.Full(v)`, and
/// once from the result's owner. With the hand-back recognised, a call result fed
/// straight into a by-value param had no owner at all, so both backends now
/// claim it as a fresh temp, as they already did for the constructor. Cells: a
/// destructuring and a holding callee, an inline and a boxed payload, a free,
/// an associated and a fresh-producing callee, a passthrough of a binding and of
/// a temp, a discarded result, and a bound result.
#[test]
fn interp_generic_enum_call_result_arg_runs_its_payload_body_once() {
    let out = run(r#"struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mkr(i: i64) -> R { return R { id: i, s: f"x{i}" } }
struct W { a: String, b: String, c: String }
impl Drop for W { fn drop(mut ref self) { println(f"dW{self.a.len()}") } }
fn mkw() -> W { return W { a: f"aaa{1}", b: f"bbb{1}", c: f"ccc{1}" } }
enum Ho[T] { Full(T), Empty }
impl[T] Ho[T] { fn mk(v: T) -> Ho[T] { return Ho.Full(v); } }
fn mkg(v: R) -> Ho[R] { return Ho.Full(v) }
fn mkf() -> Ho[R] { return Ho.Full(mkr(30)) }
fn mkgw(v: W) -> Ho[W] { return Ho.Full(v) }
fn idh(h: Ho[R]) -> Ho[R] { h }
fn takeit(x: Ho[R]) { match x { Full(r) => { println(f"f:{r.id}") } Empty => { println("e") } } }
fn takew(x: Ho[W]) { match x { Full(w) => { println(f"w:{w.a.len()}") } Empty => { println("e") } } }
fn hold(x: Ho[R]) { println("h") }
fn holdw(x: Ho[W]) { println("hw") }
fn main() {
    { takeit(mkg(mkr(9))); println("k1") }
    { takeit(mkf()); println("k2") }
    { takeit(Ho.mk(mkr(31))); println("k3") }
    { hold(mkg(mkr(32))); println("k4") }
    { takew(mkgw(mkw())); println("k5") }
    { holdw(mkgw(mkw())); println("k6") }
    { let h = mkg(mkr(33)); takeit(idh(h)); println("k7") }
    { takeit(idh(mkg(mkr(34)))); println("k8") }
    { mkg(mkr(35)); println("k9") }
    { let a = mkg(mkr(36)); takeit(a); println("k10") }
    println("end")
}"#);
    assert_eq!(out, "f:9\ndR9\nk1\nf:30\ndR30\nk2\nf:31\ndR31\nk3\nh\ndR32\nk4\nw:4\ndW4\nk5\nhw\ndW4\nk6\nf:33\ndR33\nk7\nf:34\ndR34\nk8\ndR35\nk9\nf:36\ndR36\nk10\nend\n", "got:\n{out}");
}
