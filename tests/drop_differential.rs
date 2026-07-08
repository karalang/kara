//! Standing gate for the oracle↔codegen drop differential
//! (ownership-model-mechanization Slice 4 down-payment).
//!
//! Runs [`karac::drop_differential::differential_check`] over canonical
//! heap-core shapes and asserts codegen's emitted drop set covers the ownership
//! oracle's schedule on every function (zero missing-drop divergences). This is
//! the regression net the eventual structural refactor (codegen *consuming* the
//! oracle) lands behind: if a codegen change starts dropping a scheduled place
//! on the wrong path — or stops emitting it — one of these cases goes red.
//!
//! `#![cfg(feature = "llvm")]`: the differential drives codegen, so the whole
//! file compiles to nothing (and the CI llvm tier is where it runs) without the
//! feature. It needs **no runtime archives or `cc`** — nothing is linked or run,
//! only lowered to IR — so it is a cheap, pure-in-process gate.
//!
//! Non-vacuity (that the gate observes real drops rather than passing on an
//! empty comparison) is covered by the dedicated `schedule_is_nonvacuous` case
//! below (a shape whose oracle schedule is non-empty and fully covered by
//! codegen) and, at the corpus level, by the `drop_fuzz` binary's
//! `KARAC_DROPOBS_SILENCE=1` fault-injection knob (silencing the recorder turns
//! every scheduled drop into a reported divergence).

#![cfg(feature = "llvm")]

use karac::drop_differential::{
    differential_check, differential_check_on, DiffOutcome, OracleTree,
};

/// Assert `src` is a valid differential subject whose codegen drops cover the
/// oracle's whole local schedule (zero missing-drop divergences). Returns the
/// number of scheduled local drops checked — some shapes legitimately schedule
/// zero (e.g. everything moved out), so the count is returned for the caller to
/// assert on where non-vacuity is expected, not enforced here.
#[track_caller]
fn assert_clean(src: &str) -> usize {
    match differential_check(src) {
        DiffOutcome::Checked {
            drops_checked,
            divergences,
        } => {
            assert!(
                divergences.is_empty(),
                "codegen diverged from the oracle's schedule: {divergences:?}"
            );
            drops_checked
        }
        other => panic!("expected a checked program, got {other:?}"),
    }
}

const S: &str = "\"payload_bytes_kept_comfortably_long_enough_x\".to_string()";

#[test]
fn owned_string_local() {
    let src = format!("fn main() {{ let s: String = {S}; println(s.len()); }}");
    assert_clean(&src);
}

#[test]
fn move_into_vec_only_vec_drops() {
    // `s` is moved into `v` — codegen must drop only `v` (dropping `s` too would
    // double-free; not emitting `v` would leak). The oracle schedules just `v`.
    let src = format!(
        "fn main() {{ let s: String = {S}; let mut v: Vec[String] = Vec.new(); \
         v.push(s); println(v.len()); }}"
    );
    assert_clean(&src);
}

#[test]
fn struct_with_heap_fields() {
    let src = format!(
        "struct Payload {{ tag: i64, name: String, items: Vec[String] }}\n\
         fn main() {{ let p: Payload = Payload {{ tag: 1i64, name: {S}, items: Vec[{S}] }}; \
         println(p.tag + p.name.len() + p.items.len()); }}"
    );
    assert_clean(&src);
}

#[test]
fn map_and_set_locals() {
    let src = format!(
        "fn main() {{ \
         let mut m: Map[String, i64] = Map.new(); m.insert({S}, 1i64); \
         let mut st: Set[String] = Set.new(); st.insert({S}); \
         println(m.len() + st.len()); }}"
    );
    assert_clean(&src);
}

#[test]
fn destructure_moves_aggregate_binds_fields() {
    // `pl` is moved out by the destructure (must not drop again); `name` and
    // `items` become owned locals that must drop.
    let src = format!(
        "struct Payload {{ tag: i64, name: String, items: Vec[String] }}\n\
         fn main() {{ let pl: Payload = Payload {{ tag: 1i64, name: {S}, items: Vec[{S}] }}; \
         let Payload {{ tag, name, items }} = pl; \
         println(tag + name.len() + items.len()); }}"
    );
    assert_clean(&src);
}

#[test]
fn nested_vec_of_vecs() {
    let src = format!(
        "fn main() {{ let mut vv: Vec[Vec[String]] = Vec.new(); \
         vv.push(Vec[{S}, {S}]); \
         for iv in vv.iter() {{ for e in iv.iter() {{ println(e.len()); }} }} }}"
    );
    assert_clean(&src);
}

#[test]
fn option_string_match_is_clean() {
    // Documented oracle boundary: a `match o { Some(x) => … }` on an owned
    // `Option[String]` schedules **zero** local drops — the scrutinee `o` is
    // moved into the match and the payload binding `x` is modelled non-heap
    // (the oracle does not infer a match-arm payload's heap-ness; see
    // `ownership_oracle::bind_match_pattern_inner`). Codegen frees the payload
    // via `o`'s inline-Option slot, which the missing-drop direction correctly
    // does not flag. The assertion is that this is *clean* (no missing drop),
    // not that it schedules anything.
    let src = format!(
        "fn main() {{ let o: Option[String] = Some({S}); \
         match o {{ Some(x) => {{ println(x.len()); }}, None => {{}} }} }}"
    );
    assert_eq!(assert_clean(&src), 0);
}

#[test]
fn schedule_is_nonvacuous() {
    // Non-vacuity anchor: a shape whose oracle schedule is provably non-empty
    // and fully covered by codegen — so `assert_clean`'s zero-divergence check
    // is checking real drops, not passing on an empty comparison. The
    // destructure binds two owned heap locals (`name`, `items`) that both drop.
    let src = format!(
        "struct Payload {{ tag: i64, name: String, items: Vec[String] }}\n\
         fn main() {{ let pl: Payload = Payload {{ tag: 1i64, name: {S}, items: Vec[{S}] }}; \
         let Payload {{ tag, name, items }} = pl; \
         println(tag + name.len() + items.len()); }}"
    );
    assert!(
        assert_clean(&src) >= 2,
        "expected ≥2 scheduled drops (name, items) covered by codegen"
    );
}

#[test]
fn borrow_param_source_still_drops() {
    // `peek` borrows `s` (ref param) — the callee must NOT drop it; the caller's
    // `s` stays owned and drops. Covers the caller-retains / param-exclusion rule.
    let src = format!(
        "fn peek(s: ref String) -> i64 {{ return s.len(); }}\n\
         fn main() {{ let s: String = {S}; let a: i64 = peek(s); println(a + s.len()); }}"
    );
    assert_clean(&src);
}

#[test]
fn lowered_oracle_agrees_with_codegen() {
    // Locks the architectural assumption codegen's inline self-check
    // (`KARAC_ORACLE_DROP_CHECK`) relies on: the oracle run on the *lowered*
    // tree — the tree codegen actually holds — covers codegen's emitted drops
    // just as the surface-tree run does. If lowering ever introduced a droppable
    // temporary the oracle scheduled but codegen didn't emit (or vice versa),
    // this goes red and the self-check's no-plumbing design would need revisiting.
    let shapes = [
        format!("fn main() {{ let s: String = {S}; println(s.len()); }}"),
        format!(
            "fn main() {{ let s: String = {S}; let mut v: Vec[String] = Vec.new(); \
             v.push(s); println(v.len()); }}"
        ),
        format!(
            "struct P {{ tag: i64, name: String, items: Vec[String] }}\n\
             fn main() {{ let p: P = P {{ tag: 1i64, name: {S}, items: Vec[{S}] }}; \
             let P {{ tag, name, items }} = p; println(tag + name.len() + items.len()); }}"
        ),
        format!(
            "fn main() {{ let mut m: Map[String, i64] = Map.new(); m.insert({S}, 1i64); \
             let mut st: Set[String] = Set.new(); st.insert({S}); println(m.len() + st.len()); }}"
        ),
        format!(
            "fn main() {{ let mut vv: Vec[Vec[String]] = Vec.new(); vv.push(Vec[{S}, {S}]); \
             for iv in vv.iter() {{ for e in iv.iter() {{ println(e.len()); }} }} }}"
        ),
    ];
    for src in &shapes {
        match differential_check_on(src, OracleTree::Lowered) {
            DiffOutcome::Checked { divergences, .. } => assert!(
                divergences.is_empty(),
                "lowered-oracle divergence: {divergences:?} in:\n{src}"
            ),
            other => panic!("expected Checked on lowered path, got {other:?} for:\n{src}"),
        }
    }
}

#[test]
fn spawn_capture_is_checked_clean() {
    // A `spawn`-captured heap Vec is now modelled: the oracle demotes it to
    // Borrowed (escapes as a shared/RC ref — no scope drop), matching codegen's
    // RC/join free. So the program is CHECKED (not skipped) with no divergence —
    // `v` is neither scheduled by the oracle nor scope-dropped by codegen.
    let src = format!(
        "fn band(data: Vec[String]) -> i64 {{ let mut a: i64 = 0i64; \
         for e in data.iter() {{ a = a + e.len(); }} return a; }}\n\
         fn main() {{ let v: Vec[String] = Vec[{S}]; \
         let mut pool: TaskGroup = TaskGroup.new(); \
         let mut hs: Vec[TaskHandle[i64]] = Vec.new(); \
         hs.push(pool.spawn(|| band(v))); \
         for h in hs {{ println(h.join()); }} }}"
    );
    match differential_check(&src) {
        DiffOutcome::Checked { divergences, .. } => assert!(
            divergences.is_empty(),
            "spawn capture should be clean, got {divergences:?}"
        ),
        other => panic!("spawn capture should be Checked now, got {other:?}"),
    }
}

#[test]
fn par_block_shared_capture_is_checked_clean() {
    // `par {}` captures `shared struct` values (`ha`/`hb`); those are freed at
    // scope exit via `RcDec`, which is exactly the drop the oracle schedules —
    // so codegen and the oracle agree and the program is CHECKED clean (no
    // capture skip needed once spawn captures are modelled).
    let src = format!(
        "shared struct Holder {{ s: String }}\n\
         fn hold_len(h: Holder) -> i64 {{ return h.s.len(); }}\n\
         fn main() {{ let a: String = {S}; let b: String = {S}; \
         let ha: Holder = Holder {{ s: a }}; let hb: Holder = Holder {{ s: b }}; \
         par {{ hold_len(ha); hold_len(hb); }} }}"
    );
    match differential_check(&src) {
        DiffOutcome::Checked { divergences, .. } => assert!(
            divergences.is_empty(),
            "par shared-struct capture should be clean, got {divergences:?}"
        ),
        other => panic!("par program should be Checked now, got {other:?}"),
    }
}

#[test]
fn ownership_error_is_invalid_not_a_divergence() {
    // Use-after-move: `karac check` rejects it, so it is not a codegen question.
    let src = format!(
        "fn main() {{ let s: String = {S}; let mut v: Vec[String] = Vec.new(); \
         v.push(s); println(s.len()); }}"
    );
    assert_eq!(differential_check(&src), DiffOutcome::Invalid);
}
