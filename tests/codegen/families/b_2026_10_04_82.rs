//! B-2026-10-04-82: reassigned rebind of a by-value Option[Array] param leaks

use super::*;

/// B-2026-10-04-82: a `let mut` rebind of a by-value `Option`/`Result` param
/// whose payload boxes (an `Array` of Drop elements, a tuple) and that the
/// body then REASSIGNS. The rebind owns the value outright (the caller stands
/// down for it), but its box drop carried no interior drop, so the displaced
/// value's bodies ran and every element's heap leaked (2 blocks per element
/// pair at -O0). The cells cover reassignment to `None`, to a fresh `Some`,
/// `Result` to `Err`, a reassigned rebind that is returned, a tuple payload
/// (already clean), a reassignment after a print, and the local spelling
/// (already clean).
#[test]
fn e2e_reassigned_rebind_of_array_option_param_owns_its_interior() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn c1(x: Option[Array[R, 2]]) { let mut h = x; h = None; println("c1"); }
fn c2(x: Option[Array[R, 2]]) { let mut h = x; h = Some([mk(3), mk(4)]); println("c2"); }
fn c3(x: Result[Array[R, 2], String]) { let mut h = x; h = Err("e"); println("c3"); }
fn c5(x: Option[Array[R, 2]]) -> Option[Array[R, 2]] { let mut h = x; h = Some([mk(7), mk(8)]); return h; }
fn c6(x: Option[(R, R)]) { let mut h = x; h = None; println("c6"); }
fn c7(x: Option[Array[R, 2]]) { let mut h = x; println("c7a"); h = None; println("c7b"); }
fn c9() { let mut h: Option[Array[R, 2]] = Some([mk(91), mk(92)]); h = None; println("c9"); }
fn main() {
  c1(Some([mk(11), mk(12)])); println("l1")
  let a: Option[Array[R, 2]] = Some([mk(21), mk(22)]); c2(a); println("l2")
  c3(Ok([mk(31), mk(32)])); println("l3")
  let r = c5(Some([mk(51), mk(52)])); println("l5")
  c6(Some((mk(61), mk(62)))); println("l6")
  c7(Some([mk(71), mk(72)])); println("l7")
  c9(); println("l9")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "dR11\ndR12\nc1\nl1\ndR21\ndR22\ndR3\ndR4\nc2\nl2\ndR31\ndR32\nc3\nl3\ndR51\ndR52\ndR7\ndR8\nl5\ndR61\ndR62\nc6\nl6\nc7a\ndR71\ndR72\nc7b\nl7\ndR91\ndR92\nc9\nl9\nend\n", "got:\n{out}");
}
