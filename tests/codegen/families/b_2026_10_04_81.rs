//! B-2026-10-04-81: fresh argument handed back by a by-value Option[Array] param

use super::*;

/// B-2026-10-04-81: a FRESH `Option`/`Result` argument (a constructor, or a
/// call that builds its own box) handed to a non-generic by-value param that
/// the callee returns. The callee takes the box and its interior over and
/// hands both back, but the caller's result binding registered a box-only
/// drop, so every element's heap leaked (2 blocks per call at -O0). The cells
/// cover a plain and a rebound hand-back, a conditional one, `Result`, a
/// moved local array, a named argument (already clean), a fresh call result,
/// a generic callee (already clean), a `match` on the result, an annotated
/// let, a chain, and a reassigned result.
#[test]
fn e2e_handed_back_fresh_array_option_arg_owns_its_interior() {
    let Some(out) = run_program(
        r#"struct R { id: i64, name: String }
impl Drop for R { fn drop(mut ref self) { println(f"dR{self.id}") } }
fn mk(i: i64) -> R { return R { id: i, name: f"h{i}" }; }
fn keep(x: Option[Array[R, 2]]) -> Option[Array[R, 2]] { println("keep"); return x; }
fn keep2(x: Option[Array[R, 2]]) -> Option[Array[R, 2]] { let h = x; println("keep2"); return h; }
fn midc(g: Option[Array[R, 2]], c: bool) -> Option[Array[R, 2]] { if c { return g } return Option.None }
fn keepr(x: Result[Array[R, 2], i64]) -> Result[Array[R, 2], i64] { println("keepr"); return x; }
fn keepg[T](x: Option[T]) -> Option[T] { println("keepg"); return x; }
fn mko(i: i64) -> Option[Array[R, 2]] { return Some([mk(i), mk(i + 1)]); }
fn main() {
  let r1 = keep(Some([mk(1), mk(2)])); println("a")
  let r2 = keep2(Some([mk(3), mk(4)])); println("b")
  let r3 = midc(Some([mk(5), mk(6)]), true); println("c")
  let r6 = keepr(Ok([mk(13), mk(14)])); println("f")
  let arr: Array[R, 2] = [mk(15), mk(16)]; let r7 = keep(Some(arr)); println("g")
  let o8: Option[Array[R, 2]] = Some([mk(17), mk(18)]); let r8 = keep(o8); println("h")
  let r9 = keep(mko(19)); println("i")
  let r10 = keepg(Some([mk(21), mk(22)])); println("j")
  let r11 = keep(Some([mk(23), mk(24)])); match r11 { Some(a) => println(f"u{a[1].id}"), None => {} } println("k")
  let r12 = keep(Some([mk(25), mk(26)])); println("l"); match r12 { Some(a) => println(f"w{a[0].id}"), None => {} }
  let r13: Option[Array[R, 2]] = keep(Some([mk(27), mk(28)])); println("m")
  let r14 = keep(Some([mk(29), mk(30)])); let q14 = keep(r14); println("n")
  let mut r15 = keep(Some([mk(31), mk(32)])); r15 = None; println("o")
  println("end")
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "keep\ndR1\ndR2\na\nkeep2\ndR3\ndR4\nb\ndR5\ndR6\nc\nkeepr\ndR13\ndR14\nf\nkeep\ndR15\ndR16\ng\nkeep\ndR17\ndR18\nh\nkeep\ndR19\ndR20\ni\nkeepg\ndR21\ndR22\nj\nkeep\nu24\ndR23\ndR24\nk\nkeep\nl\nw25\ndR25\ndR26\nkeep\ndR27\ndR28\nm\nkeep\nkeep\ndR29\ndR30\nn\nkeep\ndR31\ndR32\no\nend\n", "got:\n{out}");
}
