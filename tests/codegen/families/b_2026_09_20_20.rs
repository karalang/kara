//! B-2026-09-20-20 -- a by-value `Option`/`Result` argument that is not a
//! constructor (a call result, a passthrough, a generic callee's named local)
//! runs each payload part's `Drop` body once when the callee moves a part out.

use super::*;

/// B-2026-09-20-20 — `take(mkq())` over `fn mkq() -> Option[Q]` and a callee
/// arm that moves `t.r` into a local printed `r:50s:60 dR5v50 dR6v60 dR5v50`
/// on every compiled surface against the interpreter's correct
/// `r:50s:60 dR5v50 dR6v60`. The caller-side bodies gate asks which variant
/// the argument builds, and `ctor_variant_name_of_arg` answered the CALLEE of
/// any call, so a call result asked about variant `mkq`, which no escape map
/// flags, and the caller stood up the unmasked walk. The variant is now read
/// only from a real constructor of the param's head, or, for any other
/// argument, from the one variant whose payload carries a body.
///
/// The generic leg had two gaps of its own that the same cells reach: it
/// never asked the consumed-in-frame channel (so the fresh-temp and
/// call-result spellings of `Option[(T, T)]` lost element 1's body once the
/// variant was right), and it never masked a NAMED local's walk (so that
/// spelling doubled element 0's).
///
/// The `control:` cells were correct before and after.
#[test]
fn non_constructor_optres_argument_runs_each_part_body_once() {
    let cells: &[(&str, &str, &str)] = &[
        (
            "call",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"  r:{t.r.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { take(mkq()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "call-s",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn take2(o: Option[Q]) { match o { Option.Some(t) => { let x = t.s; println(f"  s:{x.v}") } Option.None => { println("  n") } } }
fn main() { take2(mkq()); println("end") }
"#,
            "  s:60\n  dR6v60\n  dR5v50\nend\n",
        ),
        (
            "call-twice",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { take(mkq()); take(mkq()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\n  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "call-nested",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn id(o: Option[Q]) -> Option[Q] { return o }
fn main() { take(id(mkq())); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "fncall-ctor",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn wrapq(q: Q) -> Option[Q] { return Option.Some(q) }
fn main() { take(wrapq(Q { r: mkr(5), s: mkr(6) })); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "passthru",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn id(o: Option[Q]) -> Option[Q] { return o }
fn main() { let a = mkq(); take(id(a)); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "method",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
struct H { k: i64 }
impl H { fn take(self, o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } } }
fn main() { let h = H { k: 1 }; h.take(mkq()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "assoc",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
struct H { k: i64 }
impl H { fn eat(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } } }
fn main() { H.eat(mkq()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "res-ok",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkok() -> Result[Q, i64] { return Result.Ok(Q { r: mkr(5), s: mkr(6) }) }
fn rtake(o: Result[Q, i64]) { match o { Result.Ok(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Result.Err(e) => { println(f"  e{e}") } } }
fn main() { rtake(mkok()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "res-both",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkok() -> Result[Q, R] { return Result.Ok(Q { r: mkr(5), s: mkr(6) }) }
fn rtake(o: Result[Q, R]) { match o { Result.Ok(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Result.Err(e) => { println(f"  e{e.v}") } } }
fn main() { rtake(mkok()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "tup-inline",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkt() -> Option[(R, i64)] { return Option.Some((mkr(5), 7)) }
fn ttake(o: Option[(R, i64)]) { match o { Option.Some(t) => { let x = t.0; println(f"  r:{x.v}k:{t.1}") } Option.None => { println("  n") } } }
fn main() { ttake(mkt()); println("end") }
"#,
            "  r:50k:7\n  dR5v50\nend\n",
        ),
        (
            "gtup",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkt() -> Option[(R, R)] { return Option.Some((mkr(5), mkr(6))) }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { let x = t.0; println("  g") } Option.None => { println("  n") } } }
fn main() { gt(mkt()); println("end") }
"#,
            "  dR5v50\n  g\n  dR6v60\nend\n",
        ),
        (
            "gtup-inline",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkt() -> Option[(R, i64)] { return Option.Some((mkr(5), 7)) }
fn gt[T](o: Option[(T, i64)]) { match o { Option.Some(t) => { let x = t.0; println(f"  k:{t.1}") } Option.None => { println("  n") } } }
fn main() { gt(mkt()); println("end") }
"#,
            "  dR5v50\n  k:7\nend\n",
        ),
        (
            "gtup-fresh2",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { let x = t.0; println("  g") } Option.None => { println("  n") } } }
fn main() { gt(Option.Some((mkr(5), mkr(6)))); println("end") }
"#,
            "  dR5v50\n  g\n  dR6v60\nend\n",
        ),
        (
            "gtup-named",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { let x = t.0; println("  g") } Option.None => { println("  n") } } }
fn main() { let a = Option.Some((mkr(5), mkr(6))); gt(a); println("end") }
"#,
            "  dR5v50\n  g\n  dR6v60\nend\n",
        ),
        (
            "gn-inline",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, i64)]) { match o { Option.Some(t) => { let x = t.0; println(f"  k:{t.1}") } Option.None => { println("  n") } } }
fn main() { let a = Option.Some((mkr(5), 7)); gt(a); println("end") }
"#,
            "  dR5v50\n  k:7\nend\n",
        ),
        (
            "gn-res",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Result[(T, T), i64]) { match o { Result.Ok(t) => { let x = t.1; println("  g") } Result.Err(e) => { println("  e") } } }
fn main() { let a: Result[(R, R), i64] = Result.Ok((mkr(5), mkr(6))); gt(a); println("end") }
"#,
            "  dR6v60\n  g\n  dR5v50\nend\n",
        ),
        (
            "gn-twice",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { let x = t.0; println("  g") } Option.None => { println("  n") } } }
fn main() { let a = Option.Some((mkr(5), mkr(6))); gt(a); let b = Option.Some((mkr(7), mkr(8))); gt(b); println("end") }
"#,
            "  dR5v50\n  g\n  dR6v60\n  dR7v70\n  g\n  dR8v80\nend\n",
        ),
        (
            "control:fresh",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"  r:{t.r.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { take(Option.Some(Q { r: mkr(5), s: mkr(6) })); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "control:named",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"  r:{t.r.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { let a = Option.Some(Q { r: mkr(5), s: mkr(6) }); take(a); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "control:named-call",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { let a = mkq(); take(a); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "control:peek",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn peek(o: Option[Q]) { match o { Option.Some(t) => { println(f"  r:{t.r.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { peek(mkq()); println("end") }
"#,
            "  r:50s:60\n  dR6v60\n  dR5v50\nend\n",
        ),
        (
            "control:call-none",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn main() { take(mkn()); println("end") }
"#,
            "  n\nend\n",
        ),
        (
            "control:res-err",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkerr() -> Result[Q, i64] { return Result.Err(3) }
fn rtake(o: Result[Q, i64]) { match o { Result.Ok(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Result.Err(e) => { println(f"  e{e}") } } }
fn main() { rtake(mkerr()); println("end") }
"#,
            "  e3\nend\n",
        ),
        (
            "control:tup-box",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn mkt() -> Option[(R, R)] { return Option.Some((mkr(5), mkr(6))) }
fn ttake(o: Option[(R, R)]) { match o { Option.Some(t) => { let x = t.0; println(f"  r:{x.v}s:{t.1.v}") } Option.None => { println("  n") } } }
fn main() { ttake(mkt()); println("end") }
"#,
            "  r:50s:60\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "control:generic",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gtake[T](o: Option[T]) { match o { Option.Some(t) => { println("  g") } Option.None => { println("  n") } } }
fn main() { gtake(mkq()); println("end") }
"#,
            "  g\n  dR6v60\n  dR5v50\nend\n",
        ),
        (
            "control:gn-peek",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { println("  g") } Option.None => { println("  n") } } }
fn main() { let a = Option.Some((mkr(5), mkr(6))); gt(a); println("end") }
"#,
            "  g\n  dR5v50\n  dR6v60\nend\n",
        ),
        (
            "control:gn-none",
            r#"struct R { id: i64, v: i64 }
impl Drop for R { fn drop(mut ref self) { println(f"  dR{self.id}v{self.v}") } }
struct Q { r: R, s: R }
fn mkr(k: i64) -> R { return R { id: k, v: k * 10 } }
fn mkq() -> Option[Q] { return Option.Some(Q { r: mkr(5), s: mkr(6) }) }
fn mkn() -> Option[Q] { return Option.None }
fn take(o: Option[Q]) { match o { Option.Some(t) => { let x = t.r; println(f"  r:{x.v}s:{t.s.v}") } Option.None => { println("  n") } } }
fn gt[T](o: Option[(T, T)]) { match o { Option.Some(t) => { let x = t.0; println("  g") } Option.None => { println("  n") } } }
fn main() { let a: Option[(R, R)] = Option.None; gt(a); println("end") }
"#,
            "  n\nend\n",
        ),
    ];
    for (label, prog, want) in cells {
        let (interp_out, interp_errs, _, _) = karac::run_program_full_checked(prog);
        assert!(
            interp_errs.is_empty(),
            "[{label}] interp errored: {interp_errs:?}"
        );
        assert_eq!(interp_out.join(""), *want, "[{label}] interpreter");
        let Some(aot) = run_program(prog) else {
            continue;
        };
        assert_eq!(aot, *want, "[{label}] AOT");
    }
}
