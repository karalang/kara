//! B-2026-10-01-49: a payload binding nested one tuple deeper at an arm tail lost its Drop body compiled.

use super::*;

/// B-2026-10-01-49 — the memory half of the codegen fixture of the same name:
/// the nested payload binding's body now runs once at the tuple's end, and the
/// payload's `String` is freed once.
#[test]
fn asan_nested_tuple_payload_binding_at_arm_tail_freed_once() {
    assert_clean_asan_run(
        r#"struct W1 { v: i64, s: String }
impl Drop for W1 { fn drop(mut ref self) { println(f"dW1_{self.v}") } }
fn mk(n: i64) -> W1 { return W1 { v: n, s: f"ssssssssssssssssssssssssssssss{n}" } }
fn a() { let o = Some(mk(1)); let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"a {t.0.0.v} {t.1}") }
fn b() { let o = Some(mk(2)); let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"b {t.1}") }
fn c(o: Option[W1]) { let t = match o { Some(w) => ((w, 1), 2), None => ((mk(0), 0), 0) }; println(f"c {t.1}") }
fn d() { let r: Result[W1, W1] = Ok(mk(4)); let t = match r { Ok(w) => ((w, 1), 2), Err(w) => ((w, 3), 4) }; println(f"d {t.1}") }
fn e() { let o = Some(mk(5)); let t = match o { Some(w) => (2, (w, 1)), None => (0, (mk(0), 0)) }; println(f"e {t.0}") }
fn g() { let o = Some(mk(6)); let t = match o { Some(w) => (((w, 1), 2), 3), None => (((mk(0), 0), 0), 0) }; println(f"g {t.1}") }
fn h() { let o = Some(mk(7)); let t = if let Some(w) = o { ((w, 1), 2) } else { ((mk(0), 0), 0) }; println(f"h {t.1}") }
fn main() {
  a(); b(); c(Some(mk(3))); d(); e(); g(); h()
  println("end")
}
"#,
        &[
            "a 1 2", "dW1_1", "b 2", "dW1_2", "c 2", "dW1_3", "d 2", "dW1_4", "e 2", "dW1_5",
            "g 3", "dW1_6", "h 2", "dW1_7", "end",
        ],
        "B-2026-10-01-49 nested tuple payload at arm tail",
    );
}
