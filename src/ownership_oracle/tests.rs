//! Unit tests for the executable ownership/drop judgment.
//!
//! These pin the *model's answer* on the shapes that matter — including the two
//! sanity-check bug shapes the spike requires the judgment to explain as
//! one-line consequences (for-loop-element-escape, boxed-`Option` move-out). On
//! those valid programs the oracle reports **zero invariant violations** and a
//! drop schedule that includes the exact places codegen historically got wrong
//! (the reference codegen must match). Genuinely-invalid programs (use after
//! move) are flagged.

use super::*;

fn oracle(src: &str) -> OracleResult {
    let parsed = crate::parse(src);
    assert!(
        parsed.errors.is_empty(),
        "test source failed to parse: {:?}",
        parsed.errors
    );
    analyze(&parsed.program)
}

/// Names of places scheduled to drop in `func`.
fn drops_in(res: &OracleResult, func: &str) -> Vec<String> {
    res.function(func)
        .map(|f| f.drops.iter().map(|d| d.place.clone()).collect())
        .unwrap_or_default()
}

// ─────────────────────────── creation + scope drop ─────────────────────

#[test]
fn owned_string_drops_at_scope_exit() {
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    println(s.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    assert!(
        drops_in(&res, "main").contains(&"s".to_string()),
        "owned String `s` should be scheduled to drop; got {:?}",
        drops_in(&res, "main")
    );
}

#[test]
fn pod_local_does_not_drop() {
    let res = oracle(
        r#"
fn main() {
    let n: i64 = 5i64;
    println(n);
}
"#,
    );
    assert!(res.is_clean());
    assert!(
        drops_in(&res, "main").is_empty(),
        "a POD i64 must not be scheduled to drop"
    );
}

// ─────────────────────────── move disarms source ───────────────────────

#[test]
fn move_into_vec_disarms_source_string() {
    // `v.push(s)` escapes `s` into the container (§4 Escape) → `s` becomes
    // Moved and must NOT be scheduled to drop; only `v` drops.
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut v: Vec[String] = Vec.new();
    v.push(s);
    println(v.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(drops.contains(&"v".to_string()), "v should drop; {drops:?}");
    assert!(
        !drops.contains(&"s".to_string()),
        "s was moved into v — it must NOT drop again (double-free); {drops:?}"
    );
}

#[test]
fn use_after_move_is_flagged() {
    // Reading `s` after moving it into `v` is a use-after-move (invariant
    // clause 3). This is the one case the model rejects outright.
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut v: Vec[String] = Vec.new();
    v.push(s);
    println(s.len());
}
"#,
    );
    assert!(
        !res.is_clean(),
        "reading `s` after moving it must be a violation"
    );
    assert!(
        res.violations()
            .any(|vio| vio.place == "s" && vio.kind == ViolationKind::UseAfterMove),
        "expected a UseAfterMove on `s`; got {:?}",
        res.violations().collect::<Vec<_>>()
    );
}

// ─────────────────── borrow / caller-retains (NonConsuming) ─────────────

#[test]
fn ref_param_call_retains_source() {
    // `peek(s)` where peek takes `ref String` is a borrow — `s` stays Owned and
    // still drops (§3.3). Reading `s` afterwards is fine.
    let res = oracle(
        r#"
fn peek(s: ref String) -> i64 { return s.len(); }
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let a: i64 = peek(s);
    let b: i64 = peek(s);
    println(a + b + s.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    assert!(
        drops_in(&res, "main").contains(&"s".to_string()),
        "borrowed `s` must still drop at scope exit"
    );
}

#[test]
fn owned_param_call_retains_caller_binding() {
    // The caller-retains rule (§4): passing `s` to a user fn with an OWNED
    // param is NonConsuming — the callee entry-copies, so the caller's `s`
    // stays Owned (drops) and reading it after the call is valid.
    let res = oracle(
        r#"
fn take(s: String) -> i64 { return s.len(); }
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let a: i64 = take(s);
    println(a + s.len());
}
"#,
    );
    assert!(
        res.is_clean(),
        "caller-retains: reading `s` after an owned-param call is valid; got {:?}",
        res.violations().collect::<Vec<_>>()
    );
    assert!(drops_in(&res, "main").contains(&"s".to_string()));
}

// ─────────────────── owned param inside the callee drops ────────────────

#[test]
fn owned_heap_param_drops_in_callee() {
    // An owned heap param is Owned in the callee (it owns its entry-copy) and
    // drops at the callee's scope exit.
    let res = oracle(
        r#"
fn take(s: String) -> i64 { return s.len(); }
fn main() { println(0); }
"#,
    );
    assert!(res.is_clean());
    assert!(
        drops_in(&res, "take").contains(&"s".to_string()),
        "owned heap param `s` should drop inside `take`; got {:?}",
        drops_in(&res, "take")
    );
}

#[test]
fn ref_param_does_not_drop_in_callee() {
    let res = oracle(
        r#"
fn peek(s: ref String) -> i64 { return s.len(); }
fn main() { println(0); }
"#,
    );
    assert!(res.is_clean());
    assert!(
        !drops_in(&res, "peek").contains(&"s".to_string()),
        "a `ref` param must NOT drop in the callee — it's a borrow"
    );
}

// ─────────────────────── sanity check 1: for-loop-element-escape ────────

#[test]
fn sanity_for_loop_element_escape_is_valid_and_vec_drops() {
    // B-2026-07-04-3: `for x in w.iter() { a.push((i, x)); }`. `x` is a Borrowed
    // alias of `w`'s buffer (§3.3); the push escapes a tuple containing it. The
    // MODEL says this is valid (no violation) and both `w` and `a` drop — the
    // codegen bug was aliasing `x` into the tuple instead of copying, which the
    // model forbids because escaping a Borrowed alias must deep-copy (§6). The
    // oracle's job here is to certify the source is valid and schedule w+a.
    let res = oracle(
        r#"
fn main() {
    let w: Vec[String] = Vec["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()];
    let mut a: Vec[(i64, String)] = Vec.new();
    let mut i: i64 = 0i64;
    for x in w.iter() { a.push((i, x)); i = i + 1i64; }
    println(a.len());
}
"#,
    );
    assert!(
        res.is_clean(),
        "for-loop-element-escape source is VALID; the bug was in codegen, not the model. got {:?}",
        res.violations().collect::<Vec<_>>()
    );
    let drops = drops_in(&res, "main");
    assert!(drops.contains(&"w".to_string()), "w should drop; {drops:?}");
    assert!(drops.contains(&"a".to_string()), "a should drop; {drops:?}");
}

// ─────────────────────── sanity check 2: boxed-Option move-out ──────────

#[test]
fn sanity_boxed_option_move_out_payload_drops() {
    // B-2026-07-03-31: `let A { value } = a; match value { Some(v) => take(v) }`
    // with `take(v: Val)` an OWNED param. Field move-out arms `value`'s / the
    // payload's obligation; `take(v)` is NonConsuming (owned param, §4), so `v`
    // stays Owned and MUST drop. The codegen bug disarmed `v`'s drop (mistaking
    // the call for an Escape) → leak. The model keeps `v`'s drop armed.
    let res = oracle(
        r#"
struct A { value: Option[String] }
fn take(v: String) -> i64 { return v.len(); }
fn use_a(a: A) -> i64 {
    let A { value } = a;
    match value {
        Some(v) => take(v),
        None => 0i64,
    }
}
fn main() { println(0); }
"#,
    );
    assert!(
        res.is_clean(),
        "boxed-Option move-out source is VALID; got {:?}",
        res.violations().collect::<Vec<_>>()
    );
    // `use_a`'s owned param `a` (a heap struct) is Owned; after destructure it
    // is Moved (fully consumed) and must NOT double-drop.
    let drops = drops_in(&res, "use_a");
    assert!(
        !drops.contains(&"a".to_string()),
        "`a` was destructured (moved out) — it must not drop again; {drops:?}"
    );
}

// ─────────────────────── index-store keeps the vec owned ────────────────

#[test]
fn index_store_vec_stays_owned() {
    let res = oracle(
        r#"
fn main() {
    let mut v: Vec[String] = Vec.new();
    v.push("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
    v[0i64] = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
    println(v.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected: {:?}", res.functions);
    assert!(drops_in(&res, "main").contains(&"v".to_string()));
}

// ─────────────────────── destructure moves the aggregate ────────────────

#[test]
fn destructure_moves_aggregate_binds_fields() {
    // `let Payload { name, items } = pl` moves `pl` (no double-drop) and binds
    // `name` (String, Owned→drops) and `items` (Vec, Owned→drops).
    let res = oracle(
        r#"
struct Payload { tag: i64, name: String, items: Vec[String] }
fn main() {
    let pl: Payload = Payload { tag: 1i64, name: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), items: Vec["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string()] };
    let Payload { tag, name, items } = pl;
    println(tag + name.len() + items.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(
        !drops.contains(&"pl".to_string()),
        "destructured `pl` must not double-drop; {drops:?}"
    );
    assert!(
        drops.contains(&"name".to_string()),
        "name should drop; {drops:?}"
    );
    assert!(
        drops.contains(&"items".to_string()),
        "items should drop; {drops:?}"
    );
    assert!(
        !drops.contains(&"tag".to_string()),
        "tag is POD (i64) — must not drop; {drops:?}"
    );
}

// ─────────────────────── whole-program clean-run smoke ──────────────────

#[test]
fn map_and_set_key_adoption_clean() {
    let res = oracle(
        r#"
fn main() {
    let mut m: Map[String, i64] = Map.new();
    m.insert("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(), 1i64);
    let mut st: Set[String] = Set.new();
    st.insert("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string());
    println(m.len() + st.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(drops.contains(&"m".to_string()) && drops.contains(&"st".to_string()));
}

// ─────────────────── conditional-move drop soundness ────────────────────

#[test]
fn conditional_move_keeps_source_drop_scheduled() {
    // `if cond { v.push(s); }` moves `s` only on the then-path. On the else-path
    // `s` survives and MUST be freed, so the model must keep `s` scheduled
    // (branch-state merge: Owned unless Moved on ALL paths). Under-scheduling
    // here would make a consuming codegen leak `s` when `!cond`.
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut v: Vec[String] = Vec.new();
    if v.len() == 0i64 { v.push(s); }
    println(v.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(
        drops.contains(&"s".to_string()),
        "conditionally-moved `s` must stay scheduled (else-path survives); got {drops:?}"
    );
    assert!(drops.contains(&"v".to_string()), "v should drop; {drops:?}");
}

#[test]
fn unconditional_move_both_branches_disarms_source() {
    // Moved on BOTH paths → gone on every path → the model may elide `s`'s drop.
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut v: Vec[String] = Vec.new();
    if v.len() == 0i64 { v.push(s); } else { v.push(s); }
    println(v.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(
        !drops.contains(&"s".to_string()),
        "`s` moved on both paths must NOT double-drop; {drops:?}"
    );
}

#[test]
fn match_move_in_one_arm_keeps_source_scheduled() {
    // `s` moved in one arm only → survives the other arm → stays scheduled.
    let res = oracle(
        r#"
fn main() {
    let s: String = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
    let mut v: Vec[String] = Vec.new();
    let o: Option[i64] = Some(1i64);
    match o {
        Some(k) => { v.push(s); }
        None => {}
    }
    println(v.len() + s.len());
}
"#,
    );
    assert!(res.is_clean(), "unexpected violations: {:?}", res.functions);
    let drops = drops_in(&res, "main");
    assert!(
        drops.contains(&"s".to_string()),
        "`s` moved in only one arm must stay scheduled; {drops:?}"
    );
}
