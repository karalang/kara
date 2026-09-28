//! B-2026-09-28-47: a boxed Option reaching a Vec through a forwarding frame or a generic keeper.

use super::*;

/// B-2026-09-28-47 — a named boxed `Option[S]` (and a `Result`) handed to a
/// generic keeper that pushes its `t: T` whole, directly or through a generic
/// forward: the box moves into the `Vec`, so the binding keeps no second free.
#[test]
fn e2e_boxed_option_named_local_handed_to_a_generic_keeper_moves_its_box_into_the_vec() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn gkeep[T](t: T, v: mut ref Vec[T]) { v.push(t) }
fn gfw[T](t: T, v: mut ref Vec[T]) { gkeep(t, v) }
fn okeep(t: Option[S], v: mut ref Vec[Option[S]]) { v.push(t) }
fn ofw(t: Option[S], v: mut ref Vec[Option[S]]) { okeep(t, v) }
fn main() {
    let mut u: Vec[Option[S]] = Vec.new();
    let a = Option.Some(mks(1));
    gkeep(a, mut u);
    let b = Option.Some(mks(2));
    gfw(b, mut u);
    let mut w: Vec[Result[S, i64]] = Vec.new();
    let c: Result[S, i64] = Ok(mks(3));
    gkeep(c, mut w);
    println(f"n{u.len()}");
    println(f"w{w.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "n2\ndS1\ndS2\nw1\ndS3\nend\n", "got:\n{out}");
}

/// B-2026-09-28-47 — a frame that forwards its boxed `Option[S]` param whole
/// to a keeper, concrete or generic, stores it exactly as the keeper does, for
/// a temporary and a named argument alike.
#[test]
fn e2e_boxed_option_through_a_forwarding_frame_reaches_the_vec_once() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn gkeep[T](t: T, v: mut ref Vec[T]) { v.push(t) }
fn gfw[T](t: T, v: mut ref Vec[T]) { gkeep(t, v) }
fn okeep(t: Option[S], v: mut ref Vec[Option[S]]) { v.push(t) }
fn ofw(t: Option[S], v: mut ref Vec[Option[S]]) { okeep(t, v) }
fn f6(h: Option[S], v: mut ref Vec[Option[S]]) { okeep(h, v); println("after") }
fn f7(h: Option[S], v: mut ref Vec[Option[S]]) { gkeep(h, v); println("after") }
fn main() {
    let mut u: Vec[Option[S]] = Vec.new();
    ofw(Option.Some(mks(1)), mut u);
    let b = Option.Some(mks(2));
    ofw(b, mut u);
    f6(Option.Some(mks(3)), mut u);
    f7(Option.Some(mks(4)), mut u);
    let e = Option.Some(mks(5));
    f7(e, mut u);
    println(f"n{u.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "after\nafter\nafter\nn5\ndS1\ndS2\ndS3\ndS4\ndS5\nend\n",
        "got:\n{out}"
    );
}

/// B-2026-09-28-47 — the forward followed two frames deep, from a method,
/// and for `Result`.
#[test]
fn e2e_boxed_option_or_result_forwarded_through_two_frames_or_a_method() {
    let Some(out) = run_program(
        r#"struct S { id: i64, s: String }
impl Drop for S { fn drop(mut ref self) { println(f"dS{self.id}") } }
fn mks(i: i64) -> S { return S { id: i, s: "ab".to_string() + "cd" } }
fn gkeep[T](t: T, v: mut ref Vec[T]) { v.push(t) }
fn gfw[T](t: T, v: mut ref Vec[T]) { gkeep(t, v) }
fn okeep(t: Option[S], v: mut ref Vec[Option[S]]) { v.push(t) }
fn ofw(t: Option[S], v: mut ref Vec[Option[S]]) { okeep(t, v) }
fn f2(t: Option[S], v: mut ref Vec[Option[S]]) { ofw(t, v) }
struct K { z: i64 }
impl K { fn mf(ref self, t: Option[S], v: mut ref Vec[Option[S]]) { okeep(t, v) } }
fn rkeep(t: Result[S, i64], v: mut ref Vec[Result[S, i64]]) { v.push(t) }
fn rfw(t: Result[S, i64], v: mut ref Vec[Result[S, i64]]) { rkeep(t, v) }
fn main() {
    let k = K { z: 0 };
    let mut u: Vec[Option[S]] = Vec.new();
    k.mf(Option.Some(mks(1)), mut u);
    let a = Option.Some(mks(2));
    k.mf(a, mut u);
    f2(Option.Some(mks(3)), mut u);
    let b = Option.Some(mks(4));
    f2(b, mut u);
    let mut w: Vec[Result[S, i64]] = Vec.new();
    rfw(Ok(mks(5)), mut w);
    let c: Result[S, i64] = Ok(mks(6));
    rfw(c, mut w);
    println(f"n{u.len()}");
    println(f"w{w.len()}");
    println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out, "n4\ndS1\ndS2\ndS3\ndS4\nw2\ndS5\ndS6\nend\n",
        "got:\n{out}"
    );
}
