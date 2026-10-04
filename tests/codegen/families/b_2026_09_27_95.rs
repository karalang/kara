//! B-2026-09-27-95: a by-value param's `let mut` rebind mutated in place

use super::*;

/// B-2026-09-27-95: `fn f(a: R) -> i64 { let mut c = a; c.id = c.id + 10; .. }` mutates the
/// callee's own copy of the param, so the caller's walk ran the `Drop` body over the value as
/// it was BEFORE the mutation (`dR1` where `dR11` is due), lost what the callee pushed onto a
/// `Vec` param, and ran a second body over what it popped off. The rebind now owns the value
/// and the caller stands down, as for a reassigned rebind; the body still runs at the end of
/// the call (rule 3), so it follows the callee's last line. Forwarding the param to such a
/// callee, or to one that keeps it in a local container, also stands the caller down.
#[test]
fn e2e_param_rebind_mutated_in_place_runs_the_mutated_body_once() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
struct H { r: R, s: String }
shared struct Q { mut id: i64 }
impl Drop for Q { fn drop(mut ref self) { println(f"dQ{self.id}") } }
struct Hs { q: Q, r: R }
struct K { k: i64 }
impl K {
    fn eat(ref self, a: R) -> i64 { let mut c = a; c.id = c.id + 10; println("in"); 5 }
}
fn held(a: R) -> i64 { let mut c = a; c.id = c.id + 10; println("in"); 5 }
fn rename(a: R) -> i64 { let mut c = a; c.name = "z"; c.id = 7; println(f"in {c.name}"); 5 }
fn grow(a: Vec[R]) -> i64 { let mut c = a; c.push(mk(99)); c.len() }
fn shrink(a: Vec[R]) -> i64 { let mut c = a; let r = c.pop(); println(f"in {r.is_some()}"); c.len() }
fn part(a: H) -> i64 { let mut c = a; c.r.id = 21; let x = c.r; println("in"); 5 }
fn swap(a: R) -> i64 { let mut c = a; c.id = 11; c = mk(9); println("in"); 5 }
fn fwd(a: R) -> i64 { held(a) }
fn keep(a: R) -> i64 { let mut v: Vec[R] = Vec.new(); v.push(a); 5 }
fn fwdk(a: R) -> i64 { keep(a) }
fn shared_field(a: Hs) -> i64 { let mut c = a; c.r.id = 11; c.q.id = 7; println("in"); 5 }
fn main() {
    let a = mk(1);
    println(f"k{held(a)}");
    println(f"k{held(mk(2))}");
    println(f"k{rename(mk(3))}");
    let v: Vec[R] = vec![mk(11)];
    println(f"k{grow(v)}");
    println(f"k{shrink(vec![mk(11), mk(12)])}");
    println(f"k{part(H { r: mk(4), s: "q" })}");
    println(f"k{swap(mk(5))}");
    println(f"k{fwd(mk(6))}");
    println(f"k{fwdk(mk(7))}");
    let k = K { k: 0 };
    println(f"k{k.eat(mk(8))}");
    let h = Hs { q: Q { id: 1 }, r: mk(2) };
    let n = shared_field(h);
    println(f"k{n}");
    println("end");
}
"#,
    ) else {
        return;
    };
    assert_eq!(
        out,
        "in\ndR11\nk5\nin\ndR12\nk5\nin z\ndR7\nk5\ndR11\ndR99\nk2\nin true\ndR12\ndR11\nk1\nin\ndR21\nk5\ndR11\ndR9\nin\nk5\nin\ndR16\nk5\ndR7\nk5\nin\ndR18\nk5\nin\ndR11\ndQ7\nk5\nend\n",
        "got:\n{out}"
    );
}
