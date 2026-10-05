//! B-2026-10-04-67 — a `shared enum` whose variant holds a tuple with an
//! `Option[shared]` element releases the handle and frees the payload.

use super::*;

/// B-2026-10-04-67 — `shared enum Est { A((Option[H], i64)), B }` held, read in
/// an arm (an element, an `unwrap` read, a destructure, a rebind), moved, and
/// stored in a `Vec`: each handle's `Drop` body runs once, after the read,
/// and nothing leaks. `Eu` beside it holds a bare `H` in the same shape and
/// must keep its bodies, because building `Est`'s payload drop before `H`'s
/// `Drop` impl was known cached a bare `free` for every later `H` release.
///
/// Before: `Est.A` leaked the 40 B payload box and the handle with no body,
/// and `Eu`'s bodies vanished once `Est`'s payload was classified.
#[test]
fn asan_shared_enum_tuple_payload_option_shared_released() {
    assert_clean_asan_run_min_allocs(
        r#"shared struct H { id: i64 }
impl Drop for H { fn drop(mut ref self) { println(f"dH{self.id}") } }
fn mk(i: i64) -> (Option[H], i64) { (Some(H { id: i }), i) }
fn mks(i: i64) -> (H, i64) { (H { id: i }, i) }
enum Et { A((Option[H], i64)), B }
shared enum Est { A((Option[H], i64)), B }
enum Eu { A((H, i64)), B }
fn main() {
    { let e = Est.A(mk(2)); println("x") }
    { let e = Est.A(mk(3)); match e { Est.A(t) => println(f"x{t.1}"), Est.B => {} } }
    { let e = Est.A(mk(4)); match e { Est.A(t) => println(f"x{t.0.unwrap().id}"), Est.B => {} } }
    { let e = Est.A(mk(7)); match e { Est.A((o, k)) => println(f"x{k}"), Est.B => {} } }
    { let e = Est.A(mk(8)); match e { Est.A(t) => { let u = t; println(f"x{u.1}") } Est.B => {} } }
    { let e = Est.A(mk(9)); let e2 = e; println("x") }
    { let e = Est.A(mk(10)); let v: Vec[Est] = [e]; println(f"x{v.len()}") }
    { let e = Est.B; println("x") }
    { let e = Est.A((None, 12)); println("x") }
    { let e = Eu.A(mks(5)); println("x") }
    { let e = Eu.A(mks(6)); match e { Eu.A(t) => println(f"x{t.0.id}"), Eu.B => {} } }
    println("end")
}"#,
        &[
            "x", "dH2", "x3", "dH3", "x4", "dH4", "x7", "dH7", "x8", "dH8", "x", "dH9", "x1",
            "dH10", "x", "x", "x", "dH5", "x6", "dH6", "end",
        ],
        "asan_shared_enum_tuple_payload_option_shared_released",
        8,
    );
}
