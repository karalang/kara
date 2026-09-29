//! B-2026-09-19-47 -- an `Option` / `Result` enum payload, declared or
//! generic, runs its elements' `Drop` bodies.

use super::*;

/// B-2026-09-19-47 — the memory-clean cells of the `Option` / `Result` enum
/// payload grid: a `Vec` inside an `Option` inside a generic payload, a
/// `None` payload, and an arm that moves the declared or generic `Option`
/// binding. The remaining cells hit the older boxed-payload leak of a user
/// enum over `Option` (the same bytes before and after this fix), which is its
/// own row, so they are pinned in the codegen and interpreter suites instead.
#[test]
fn asan_optres_enum_payload_clean_cells() {
    assert_clean_asan_run(
        r#"struct S1 { v: i64, s: String }
impl Drop for S1 { fn drop(mut ref self) { println(f"  dS{self.v}") } }
enum Ho { P(Option[S1]), Q }
enum G[T] { X(T), Y }
fn s(v: i64) -> S1 { S1 { v: v, s: f"ssssssss{v}" } }
fn main() {
    println("gvec"); { let mut w: Vec[S1] = []; w.push(s(2)); let o: Option[Vec[S1]] = Option.Some(w); let g = G.X(o); println("  x") }
    println("dnone"); { let h = Ho.P(Option.None); println("  x") }
    println("rebind"); { let h = Ho.P(Option.Some(s(3))); match h { Ho.P(o) => { let u = o; println("  in") } Ho.Q => {} } }
    println("grebind"); { let g = G.X(Option.Some(s(11))); match g { G.X(o) => { let u = o; println("  in") } G.Y => {} } }
    println("end")
}"#,
        &[
            "gvec", "  dS2", "  x", "dnone", "  x", "rebind", "  dS3", "  in", "grebind", "  dS11",
            "  in", "end",
        ],
        "b_2026_09_19_47_clean_cells",
    );
}
