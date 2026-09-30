//! B-2026-09-19-32 -- a non-`Drop` enum whose payload a by-value callee moves
//! out as its return value frees that payload exactly once.

use super::*;

/// B-2026-09-19-32 — `take(mkVe(i))` over `fn take(o: Ve) -> String { if let
/// Ve.A(s) = o { s } else { .. } }`, with `Ve` declaring no `impl Drop`, lost
/// 310 B in 20 blocks at `-O0` on both the `if let` and the `match`
/// spelling, while the same program with an `impl Drop for Ve` was clean.
///
/// Fixed by 388a4fb (B-2026-09-16-13's fix), which gave a call-returned enum
/// temp in argument position an owning slot in the caller. Bisected: the row's
/// program leaks under valgrind at 1058ec0 and is clean from 388a4fb on. This
/// pins the row's own shapes, which that commit's fixture does not cover: its
/// callees read or drop the payload, and none hands it back out.
#[test]
fn asan_non_drop_enum_payload_moved_out_as_return_frees_once() {
    assert_clean_asan_run(
        r#"
enum Ve { A(String), B }
enum Vv { A(Vec[i64]), B }
fn mkVe(n: i64) -> Ve { return Ve.A(f"payloadpayload{n}") }
fn mkVv(n: i64) -> Vv { let mut v: Vec[i64] = Vec.new(); v.push(n); v.push(n); return Vv.A(v) }
fn takeIf(o: Ve) -> String { if let Ve.A(s) = o { s } else { "none".to_string() } }
fn takeMatch(o: Ve) -> String { match o { Ve.A(s) => s, Ve.B => "none".to_string() } }
fn takeVec(o: Vv) -> Vec[i64] { if let Vv.A(s) = o { s } else { Vec.new() } }
fn main() {
    let mut i = 0; let mut t = 0; let mut u = 0; let mut w = 0;
    while i < 20 {
        t = t + takeIf(mkVe(i)).len();
        u = u + takeMatch(mkVe(i)).len();
        w = w + takeVec(mkVv(i)).len();
        i = i + 1;
    }
    println(f"t {t}");
    println(f"u {u}");
    println(f"w {w}");
    let v = mkVe(3);
    println(f"named {takeIf(v).len()}");
    println(f"none {takeIf(Ve.B).len()}");
}
"#,
        &["t 310", "u 310", "w 40", "named 15", "none 4"],
        "asan_non_drop_enum_payload_moved_out_as_return_frees_once",
    );
}
