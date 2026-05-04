// tests/rc_predicate_parity.rs
//
// Round 12.10: parity matrix for the formal RC-predicate pipeline
// (use classifier → CFG → dominator tree → predicate evaluator) vs.
// the legacy linear forward state machine in `src/ownership.rs`.
//
// Each test program is a verbatim shape from `tests/rc_fallback.rs`.
// We run both pipelines side-by-side and assert:
//
//   • Trigger 1 (branch-divergent re-use after consume) — predicate
//     fires; legacy fires. Sets must match.
//
//   • Trigger 2 (closure capture with outer use) — predicate fires
//     (round 12.11); legacy fires. Sets must match. The CFG lowers
//     the closure body into a sibling sink block of the creation
//     point, so capture-position consumes are dominance-incomparable
//     with subsequent outer uses.
//
//   • Trigger 3 (container store with subsequent use) — predicate
//     fires (round 12.12); legacy fires. Sets must match. The use
//     classifier marks each owned arg of a `mut ref self` method
//     call as a sink-arg; the CFG lowers those args into a sibling
//     sink block of the call site, so the consume site is
//     dominance-incomparable with subsequent outer uses.
//
//   • Negative shapes (sequential consume + use that's a real
//     use-after-move; read-only flows; @no_rc / #[no_rc]) — both
//     pipelines must agree that no RC fallback is recorded.
//
// Rounds 12.11 / 12.12 closed the trigger-2 / trigger-3 gaps via
// structural CFG fixes (sibling sink blocks for closure bodies and
// `mut ref self` container args, respectively). Round 12.13 closed
// the par-block CFG-walk completeness item by extending both
// `cfg.rs` and `use_classifier.rs` to walk transparent block-bodied
// forms (`par`, `seq`, `unsafe`, `lock`, `providers`).

use karac::cfg::ConsumeOrigin;
use karac::ownership::{OwnershipErrorKind, RcTrigger};
use karac::{
    ownershipcheck, parse, predicate_rc_candidates, predicate_uam_candidates, resolve, typecheck,
};
use std::collections::{HashMap, HashSet};

// ── Pipeline plumbing ─────────────────────────────────────────────

struct ParityRun {
    /// Function key → set of bindings flagged by the legacy pass,
    /// paired with the trigger label.
    legacy: HashMap<String, HashMap<String, RcTrigger>>,
    /// Function key → set of bindings flagged by the predicate pass.
    predicate: HashMap<String, HashSet<String>>,
    /// Function key → binding → origin tag on the predicate's witness.
    /// Round 12.14: lets parity tests cross-check that the predicate's
    /// flavor labeling lines up with the legacy `RcTrigger`.
    predicate_origins: HashMap<String, HashMap<String, ConsumeOrigin>>,
    /// Function key → set of bindings flagged by the direct-UAM
    /// predicate (round 12.15). Disjoint from `predicate` for any
    /// given binding because the UAM predicate excludes
    /// dominance-incomparable shapes (those are RC fallback) and
    /// the RC predicate excludes strict-dominance shapes (those are
    /// UAM).
    predicate_uam: HashMap<String, HashSet<String>>,
    /// Surfaced legacy errors (use-after-move, NoRcViolation, etc.).
    legacy_errors: Vec<OwnershipErrorKind>,
}

fn run(src: &str) -> ParityRun {
    let parsed = parse(src);
    assert!(
        parsed.errors.is_empty(),
        "parse errors: {:?}",
        parsed.errors
    );
    let resolved = resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "resolve errors: {:?}",
        resolved.errors
    );
    let tc = typecheck(&parsed.program, &resolved);
    let legacy_check = ownershipcheck(&parsed.program, &tc);

    let mut legacy: HashMap<String, HashMap<String, RcTrigger>> = HashMap::new();
    for (func_name, rc_map) in &legacy_check.rc_values {
        let mut entry = HashMap::new();
        for (binding, rc) in rc_map {
            entry.insert(binding.clone(), rc.trigger.clone());
        }
        legacy.insert(func_name.clone(), entry);
    }

    let predicate_raw = predicate_rc_candidates(&parsed.program, &tc);
    let mut predicate: HashMap<String, HashSet<String>> = HashMap::new();
    let mut predicate_origins: HashMap<String, HashMap<String, ConsumeOrigin>> = HashMap::new();
    for (fn_key, bindings) in predicate_raw {
        let mut keys = HashSet::new();
        let mut origins = HashMap::new();
        for (binding, witness) in bindings {
            keys.insert(binding.clone());
            origins.insert(binding, witness.consume_origin);
        }
        predicate.insert(fn_key.clone(), keys);
        predicate_origins.insert(fn_key, origins);
    }

    let uam_raw = predicate_uam_candidates(&parsed.program, &tc);
    let predicate_uam: HashMap<String, HashSet<String>> = uam_raw
        .into_iter()
        .map(|(fn_key, bindings)| (fn_key, bindings.into_keys().collect()))
        .collect();

    let legacy_errors = legacy_check.errors.iter().map(|e| e.kind.clone()).collect();

    ParityRun {
        legacy,
        predicate,
        predicate_origins,
        predicate_uam,
        legacy_errors,
    }
}

fn legacy_trigger(run: &ParityRun, fn_key: &str, binding: &str) -> RcTrigger {
    run.legacy
        .get(fn_key)
        .unwrap_or_else(|| {
            panic!(
                "no legacy rc map for {fn_key}; have {:?}",
                run.legacy.keys()
            )
        })
        .get(binding)
        .cloned()
        .unwrap_or_else(|| panic!("legacy did not flag {binding} in {fn_key}"))
}

fn predicate_has(run: &ParityRun, fn_key: &str, binding: &str) -> bool {
    run.predicate
        .get(fn_key)
        .is_some_and(|s| s.contains(binding))
}

fn predicate_uam_has(run: &ParityRun, fn_key: &str, binding: &str) -> bool {
    run.predicate_uam
        .get(fn_key)
        .is_some_and(|s| s.contains(binding))
}

fn predicate_origin(run: &ParityRun, fn_key: &str, binding: &str) -> ConsumeOrigin {
    *run.predicate_origins
        .get(fn_key)
        .unwrap_or_else(|| {
            panic!(
                "no predicate origins for {fn_key}; have {:?}",
                run.predicate_origins.keys()
            )
        })
        .get(binding)
        .unwrap_or_else(|| panic!("predicate did not flag {binding} in {fn_key}"))
}

/// Map the legacy `RcTrigger` to the `ConsumeOrigin` the predicate
/// witness should carry for the same RC fallback shape.
fn expected_origin_for(trigger: &RcTrigger) -> ConsumeOrigin {
    match trigger {
        RcTrigger::DirectReuseAfterConsume => ConsumeOrigin::Direct,
        RcTrigger::ClosureCaptureWithOuterUse => ConsumeOrigin::ClosureCapture,
        RcTrigger::ContainerStoreWithSubsequentUse => ConsumeOrigin::ContainerStore,
    }
}

// ── Trigger 1: branch-divergent re-use after consume ──────────────

#[test]
fn parity_t1_if_branch_consume_then_outer_use() {
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) { }\n\
               fn process(cond: bool, d: Data) {\n\
                   if cond { consume(d); }\n\
                   use_d(d);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "process", "d"),
        RcTrigger::DirectReuseAfterConsume
    );
    assert!(
        predicate_has(&run, "process", "d"),
        "trigger 1 should fire under the predicate; got {:?}",
        run.predicate
    );
}

#[test]
fn parity_t1_match_arm_consume_then_outer_use() {
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) { }\n\
               fn process(d: Data) {\n\
                   match d.value {\n\
                       0 => consume(d),\n\
                       _ => {},\n\
                   }\n\
                   use_d(d);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "process", "d"),
        RcTrigger::DirectReuseAfterConsume
    );
    assert!(
        predicate_has(&run, "process", "d"),
        "match-arm trigger 1 should fire under the predicate"
    );
}

// ── Trigger 2: closure capture + outer use ────────────────────────
//
// Round 12.11 closed this gap. The CFG lowers a closure body into a
// sibling sink block of the creation point. The capture-position
// consume of `cfg` inside the closure body and the outer `log(cfg)`
// consume both descend from the creation block; neither dominates
// the other. The predicate fires.

#[test]
fn parity_t2_closure_capture_with_outer_use() {
    let src = "struct Config { name: i64 }\n\
               fn apply(c: Config) { }\n\
               fn log(c: Config) { }\n\
               fn make_handler(cfg: Config) {\n\
                   let h = || apply(cfg);\n\
                   log(cfg);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "make_handler", "cfg"),
        RcTrigger::ClosureCaptureWithOuterUse
    );
    assert!(
        predicate_has(&run, "make_handler", "cfg"),
        "trigger 2 should fire under the predicate (round 12.11); got {:?}",
        run.predicate
    );
}

// ── Trigger 3: container store + subsequent use ───────────────────
//
// Round 12.12 closed this gap structurally. The use classifier
// marks each owned (no-`mut`-marker) arg of a `mut ref self` method
// call as a sink-arg; the CFG `MethodCall` arm lowers those args
// into a sibling sink block of the call site. The consume of the
// owned arg lives in the sink block; subsequent outer uses live in
// the after-call block. Both descend from the call's pre-fork block
// but neither dominates the other — the predicate fires.

#[test]
fn parity_t3_method_call_consume_with_outer_use() {
    let src = "struct Widget { value: i64 }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Widget) { }\n\
               }\n\
               fn audit(w: Widget) { }\n\
               fn register(w: Widget, bag: mut ref Bag) {\n\
                   bag.insert(0, w);\n\
                   audit(w);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "register", "w"),
        RcTrigger::ContainerStoreWithSubsequentUse
    );
    assert!(
        predicate_has(&run, "register", "w"),
        "trigger 3 should fire under the predicate (round 12.12); got {:?}",
        run.predicate
    );
}

#[test]
fn parity_t3_partial_move_through_field_projection() {
    let src = "struct Inner { n: i64 }\n\
               struct Outer { inner: Inner }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Inner) { }\n\
               }\n\
               fn audit(o: Outer) { }\n\
               fn register(o: Outer, bag: mut ref Bag) {\n\
                   bag.insert(0, o.inner);\n\
                   audit(o);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "register", "o"),
        RcTrigger::ContainerStoreWithSubsequentUse
    );
    assert!(
        predicate_has(&run, "register", "o"),
        "partial-move-through-field projection (round 12.12) should fire"
    );
}

#[test]
fn parity_t3_local_binding() {
    let src = "struct Widget { value: i64 }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Widget) { }\n\
               }\n\
               fn audit(w: Widget) { }\n\
               fn main() {\n\
                   let w = Widget { value: 42 };\n\
                   let mut bag = Bag { count: 0 };\n\
                   bag.insert(0, w);\n\
                   audit(w);\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "main", "w"),
        RcTrigger::ContainerStoreWithSubsequentUse
    );
    assert!(predicate_has(&run, "main", "w"));
}

// ── Negative parity: both pipelines agree on no-RC ────────────────

#[test]
fn parity_negative_clean_container_store() {
    // No subsequent use — neither pass records anything.
    let src = "struct Widget { value: i64 }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Widget) { }\n\
               }\n\
               fn register(w: Widget, bag: mut ref Bag) {\n\
                   bag.insert(0, w);\n\
               }";
    let run = run(src);
    assert!(run.legacy.get("register").is_none_or(|m| m.is_empty()));
    assert!(run.predicate.get("register").is_none_or(|s| s.is_empty()));
}

#[test]
fn parity_negative_owned_self_container_method_uam() {
    // Owned-self method consuming an owned arg → still a UAM error
    // under the legacy pass; predicate also produces no RC entry
    // because the consume + use are sequential.
    let src = "struct Widget { value: i64 }\n\
               impl Widget {\n\
                   fn merge(self, other: Widget) { }\n\
               }\n\
               fn audit(w: Widget) { }\n\
               fn register(a: Widget, b: Widget) {\n\
                   a.merge(b);\n\
                   audit(b);\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)));
    assert!(run
        .legacy
        .get("register")
        .is_none_or(|m| !m.contains_key("b")));
    assert!(run
        .predicate
        .get("register")
        .is_none_or(|s| !s.contains("b")));
    // Round 12.15: UAM predicate fires on the same shape.
    assert!(
        predicate_uam_has(&run, "register", "b"),
        "UAM predicate should fire on owned-self consume + outer use"
    );
}

#[test]
fn parity_negative_sequential_consume_is_uam() {
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   consume(d);\n\
                   consume(d);\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)));
    assert!(run.legacy.get("main").is_none_or(|m| m.is_empty()));
    assert!(run.predicate.get("main").is_none_or(|s| s.is_empty()));
    // Round 12.15: UAM predicate fires on the same shape.
    assert!(
        predicate_uam_has(&run, "main", "d"),
        "UAM predicate should fire on sequential consume-then-consume"
    );
}

#[test]
fn parity_negative_at_no_rc_struct_blocks_trigger() {
    // @no_rc Particle: legacy emits NoRcViolation rather than recording
    // an RC entry. Predicate still produces a witness (the predicate is
    // pure dataflow — it doesn't know about @no_rc); that's expected.
    // Both passes refuse to record RC; legacy errors out, predicate
    // returns the dataflow shape without enforcement.
    let src = "@no_rc\n\
               struct Particle { x: i64 }\n\
               fn consume(p: Particle) { }\n\
               fn use_p(p: Particle) { }\n\
               fn process(cond: bool, p: Particle) {\n\
                   if cond { consume(p); }\n\
                   use_p(p);\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::NoRcViolation)));
    // The predicate is enforcement-agnostic: it sees the same trigger-1
    // shape that drives the @no_rc error and produces a witness. Future
    // wiring will gate the @no_rc emission on the predicate witness.
    assert!(
        predicate_has(&run, "process", "p"),
        "predicate should still detect the trigger-1 shape under @no_rc"
    );
}

// ── Trigger 1 with par {} — par-body use is now visible ───────────
//
// Round 12.13: `cfg.rs` and `use_classifier.rs` now both walk the
// transparent block-bodied forms (`par`, `seq`, `unsafe`, `lock`,
// `providers`). Uses inside `par { ... }` become CFG `UseSite`s and
// are classified normally, so the predicate fires on the same shape
// the legacy pass detects.

#[test]
fn parity_t1_in_par_block_predicate_matches_legacy() {
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) { }\n\
               fn process(cond: bool, d: Data) {\n\
                   if cond { consume(d); }\n\
                   par { use_d(d); }\n\
               }";
    let run = run(src);
    assert_eq!(
        legacy_trigger(&run, "process", "d"),
        RcTrigger::DirectReuseAfterConsume
    );
    assert!(
        predicate_has(&run, "process", "d"),
        "predicate should detect trigger-1 reuse-after-consume when the \
         second use lives inside a `par` block (round 12.13)"
    );
}

// ── Round 12.14: predicate witness origin matches legacy trigger ──
//
// The use classifier tags each Consume identifier-leaf with a
// `ConsumeOrigin` based on its surrounding AST context (closure body
// → ClosureCapture; sink-arg of a `mut ref self` method call →
// ContainerStore; otherwise Direct). The CFG threads that tag through
// `UseSite`, and the predicate evaluator surfaces it on each
// `RcWitness` so the eventual in-place integration into
// `OwnershipChecker::check_function_body` can map flavor labels back
// onto `RcEntry::trigger` without a second AST walk. These tests
// cross-check that the predicate's origin agrees with the legacy
// pass's `RcTrigger` for each of the three RC fallback shapes.

#[test]
fn parity_origin_t1_branch_divergent_is_direct() {
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) { }\n\
               fn process(cond: bool, d: Data) {\n\
                   if cond { consume(d); }\n\
                   use_d(d);\n\
               }";
    let run = run(src);
    let trigger = legacy_trigger(&run, "process", "d");
    assert_eq!(trigger, RcTrigger::DirectReuseAfterConsume);
    assert_eq!(
        predicate_origin(&run, "process", "d"),
        expected_origin_for(&trigger),
        "trigger-1 witness should carry origin Direct"
    );
}

#[test]
fn parity_origin_t2_closure_capture() {
    let src = "struct Config { name: i64 }\n\
               fn apply(c: Config) { }\n\
               fn log(c: Config) { }\n\
               fn make_handler(cfg: Config) {\n\
                   let h = || apply(cfg);\n\
                   log(cfg);\n\
               }";
    let run = run(src);
    let trigger = legacy_trigger(&run, "make_handler", "cfg");
    assert_eq!(trigger, RcTrigger::ClosureCaptureWithOuterUse);
    assert_eq!(
        predicate_origin(&run, "make_handler", "cfg"),
        expected_origin_for(&trigger),
        "trigger-2 witness should carry origin ClosureCapture"
    );
}

#[test]
fn parity_origin_t3_container_store() {
    let src = "struct Widget { value: i64 }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Widget) { }\n\
               }\n\
               fn audit(w: Widget) { }\n\
               fn register(w: Widget, bag: mut ref Bag) {\n\
                   bag.insert(0, w);\n\
                   audit(w);\n\
               }";
    let run = run(src);
    let trigger = legacy_trigger(&run, "register", "w");
    assert_eq!(trigger, RcTrigger::ContainerStoreWithSubsequentUse);
    assert_eq!(
        predicate_origin(&run, "register", "w"),
        expected_origin_for(&trigger),
        "trigger-3 witness should carry origin ContainerStore"
    );
}

// ── Round 12.15: direct-UAM predicate parity ─────────────────────
//
// `direct_uam_candidates` covers the *error* case the formal RC
// predicate filters out: consume strictly precedes another use
// (same-block source order or cross-block dominance). Together with
// `rc_candidates`, the two predicates partition every binding's use
// pairs into RC fallback / direct UAM / sequentially fine. Tests
// here cross-check that:
//   • Every shape that errors with `UseAfterMove` under the legacy
//     pass also fires the UAM predicate for the same binding.
//   • Every RC fallback shape (t1/t2/t3) does NOT fire the UAM
//     predicate (mutual exclusivity).
//   • Read-only and clean-sequential shapes fire neither.

#[test]
fn parity_uam_legacy_uam_implies_predicate_uam() {
    // Linear sequential consume of a non-Copy value → legacy pass
    // emits UseAfterMove; UAM predicate fires.
    let src = "struct Data { value: i64 }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   let _a = d;\n\
                   let _b = d;\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)));
    assert!(predicate_uam_has(&run, "main", "d"));
    assert!(
        !predicate_has(&run, "main", "d"),
        "UAM shape must not fire RC predicate"
    );
}

#[test]
fn parity_uam_cross_block_strict_dominance_fires() {
    // Pre-loop consume + in-loop use: consume strictly dominates the
    // in-loop use → UAM. Legacy pass currently routes this as a
    // direct error too.
    let src = "struct Data { value: i64 }\n\
               fn use_d(d: Data) { }\n\
               fn process(d: Data) {\n\
                   let _pre = d;\n\
                   while true { use_d(d); }\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)));
    assert!(predicate_uam_has(&run, "process", "d"));
    assert!(!predicate_has(&run, "process", "d"));
}

#[test]
fn parity_uam_t1_branch_divergent_does_not_fire() {
    // Trigger 1 RC fallback shape — RC predicate fires; UAM must NOT.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) { }\n\
               fn process(cond: bool, d: Data) {\n\
                   if cond { consume(d); }\n\
                   use_d(d);\n\
               }";
    let run = run(src);
    assert!(predicate_has(&run, "process", "d"));
    assert!(
        !predicate_uam_has(&run, "process", "d"),
        "trigger-1 shape is RC fallback, not UAM"
    );
}

#[test]
fn parity_uam_t2_closure_capture_does_not_fire() {
    // Trigger 2 RC fallback shape — RC predicate fires; UAM must NOT.
    let src = "struct Config { name: i64 }\n\
               fn apply(c: Config) { }\n\
               fn log(c: Config) { }\n\
               fn make_handler(cfg: Config) {\n\
                   let h = || apply(cfg);\n\
                   log(cfg);\n\
               }";
    let run = run(src);
    assert!(predicate_has(&run, "make_handler", "cfg"));
    assert!(
        !predicate_uam_has(&run, "make_handler", "cfg"),
        "trigger-2 shape is RC fallback, not UAM"
    );
}

#[test]
fn parity_uam_t3_container_store_does_not_fire() {
    // Trigger 3 RC fallback shape — RC predicate fires; UAM must NOT.
    let src = "struct Widget { value: i64 }\n\
               struct Bag { count: i64 }\n\
               impl Bag {\n\
                   fn insert(mut ref self, key: i64, value: Widget) { }\n\
               }\n\
               fn audit(w: Widget) { }\n\
               fn register(w: Widget, bag: mut ref Bag) {\n\
                   bag.insert(0, w);\n\
                   audit(w);\n\
               }";
    let run = run(src);
    assert!(predicate_has(&run, "register", "w"));
    assert!(
        !predicate_uam_has(&run, "register", "w"),
        "trigger-3 shape is RC fallback, not UAM"
    );
}

#[test]
fn parity_uam_clean_sequential_fires_nothing() {
    // Single non-Copy consume, no other use — neither RC nor UAM.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   consume(d);\n\
               }";
    let run = run(src);
    assert!(run.legacy_errors.is_empty());
    assert!(run.predicate.get("main").is_none_or(|s| s.is_empty()));
    assert!(run.predicate_uam.get("main").is_none_or(|s| s.is_empty()));
}

#[test]
fn parity_uam_read_then_consume_is_clean() {
    // Read-then-consume — sequentially fine; neither predicate fires.
    let src = "struct Data { value: i64 }\n\
               fn read(d: ref Data) { }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   read(d);\n\
                   consume(d);\n\
               }";
    let run = run(src);
    assert!(run.legacy_errors.is_empty());
    assert!(run.predicate.get("main").is_none_or(|s| s.is_empty()));
    assert!(run.predicate_uam.get("main").is_none_or(|s| s.is_empty()));
}

// ── Round 12.19: reassignment-resets-state parity ───────────────────
//
// The legacy `OwnershipChecker` resets a binding's state to `Live` on
// every `name = expr;` assignment to a bare-identifier LHS (see
// `ownership.rs:866`). Round 12.19 lifts this into the predicate
// pipeline by emitting `UseKind::Reassign` markers at those LHS
// positions and applying a kill-check to the UAM predicate. These
// tests pin the parity between the two pipelines.

#[test]
fn parity_reassign_kills_uam_in_straight_line() {
    // The documented gap from round 12.18:
    //   let mut d; consume(d); d = ...; consume(d);
    // Legacy: no error (reassign resets state). Predicate before round
    // 12.19: UAM fires (linear sequential consume). Predicate now: UAM
    // killed by the Reassign marker between the two consumes.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let mut d = Data { value: 1 };\n\
                   consume(d);\n\
                   d = Data { value: 2 };\n\
                   consume(d);\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy must accept reassign-reset; got {:?}",
        run.legacy_errors
    );
    assert!(
        !predicate_uam_has(&run, "main", "d"),
        "predicate UAM must be killed by the reassign between consumes; got UAM hit"
    );
    assert!(
        !predicate_has(&run, "main", "d"),
        "no RC fallback expected either"
    );
}

#[test]
fn parity_reassign_with_consume_in_value_position_does_not_kill_self() {
    // `d = f(d);` — the RHS reads `d` (consume), THEN the LHS rebinds
    // `d`. The use-classifier walks the value first, so the RHS's
    // consume of `d` is recorded before the LHS's Reassign marker.
    // Without a SECOND use of `d` after the reassign, no UAM partner
    // exists and the predicate stays silent. Pin that the predicate
    // doesn't get confused by the self-referential rebind.
    let src = "struct Data { value: i64 }\n\
               fn rotate(d: Data) -> Data { d }\n\
               fn main() {\n\
                   let mut d = Data { value: 1 };\n\
                   d = rotate(d);\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy must accept self-referential reassign; got {:?}",
        run.legacy_errors
    );
    assert!(
        !predicate_uam_has(&run, "main", "d"),
        "single consume in RHS + rebind = no UAM"
    );
}

#[test]
fn parity_reassign_does_not_kill_unrelated_consume_after_reassign() {
    // Sentinel: the kill check must not over-trigger. `consume(d);
    // d = ...; consume(d); consume(d);` — the FIRST post-reassign
    // consume is fine (kill applies); the SECOND post-reassign consume
    // is a UAM after the first post-reassign consume. Predicate must
    // fire on that pair.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let mut d = Data { value: 1 };\n\
                   consume(d);\n\
                   d = Data { value: 2 };\n\
                   consume(d);\n\
                   consume(d);\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors
            .iter()
            .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)),
        "legacy must flag the second post-reassign consume as UAM"
    );
    assert!(
        predicate_uam_has(&run, "main", "d"),
        "predicate must flag the second post-reassign consume as UAM"
    );
}

// ── Round 12.20: once-callable closure call-site consume parity ─────
//
// Legacy `OwnershipChecker` formerly tracked once-callable closures
// via `once_callable_closures: HashSet<String>`, populated when a
// let-RHS closure body produced a `MoveKind::ClosureCapture` consume
// on a pre-Live outer binding (round 12.42 collapsed `MoveKind` into
// a single `Moved` state — the legacy state machine no longer drives
// any RC-flavor distinction). At call sites, `check_callee` consumed
// the closure binding so a second `f()` errored as UseAfterMove.
// Round 12.20 lifts this into the use-classifier so the UAM predicate
// fires on `f(); f();` shapes, mirroring the legacy diagnostic;
// round 12.38 removed the legacy state-machine bookkeeping.

#[test]
fn parity_uam_once_callable_closure_second_call_is_uam() {
    // Bare-form closure capturing a non-Copy outer binding by
    // ownership → once-callable → second `f()` is UAM under both
    // legacy and predicate.
    let src = "struct Config { name: i64 }\n\
               fn apply(c: Config) { }\n\
               fn main() {\n\
                   let cfg = Config { name: 1 };\n\
                   let f = || apply(cfg);\n\
                   f();\n\
                   f();\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors
            .iter()
            .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)),
        "legacy must emit UseAfterMove on second f(); got {:?}",
        run.legacy_errors
    );
    assert!(
        predicate_uam_has(&run, "main", "f"),
        "predicate UAM must fire for once-callable f's second call"
    );
}

#[test]
fn parity_repeatable_closure_multi_call_is_clean() {
    // Closure body only reads a Copy field — no ClosureCapture origin
    // → not once-callable → multi-call is fine. Neither predicate
    // fires for the closure binding; legacy accepts.
    let src = "struct Config { value: i64 }\n\
               fn main() {\n\
                   let cfg = Config { value: 1 };\n\
                   let f = || cfg.value;\n\
                   let _a = f();\n\
                   let _b = f();\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy must accept repeatable closure multi-call; got {:?}",
        run.legacy_errors
    );
    assert!(
        !predicate_uam_has(&run, "main", "f"),
        "predicate must not flag UAM on repeatable closure binding"
    );
}

#[test]
fn parity_once_callable_closure_single_call_is_clean() {
    // Once-callable closure called exactly once — no UAM. The legacy
    // pass and the predicate must agree on this single-call shape.
    let src = "struct Config { name: i64 }\n\
               fn apply(c: Config) { }\n\
               fn main() {\n\
                   let cfg = Config { name: 1 };\n\
                   let f = || apply(cfg);\n\
                   f();\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "single call of a once-callable closure is fine; legacy err {:?}",
        run.legacy_errors
    );
    assert!(
        !predicate_uam_has(&run, "main", "f"),
        "single call must not fire UAM"
    );
}

#[test]
fn parity_field_assign_does_not_kill_consume() {
    // `s.value = ...;` is a partial mutation, not a rebind. The
    // classifier does not emit a Reassign marker for projection
    // targets, so a prior consume of `s` still flags UAM on a later
    // use. Cross-checks legacy behavior (where field-assign on a
    // moved value would itself error, but the rule we care about
    // here is the predicate-pipeline marker emission).
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let mut s = Data { value: 1 };\n\
                   consume(s);\n\
                   s.value = 2;\n\
                   consume(s);\n\
               }";
    let run = run(src);
    assert!(
        !run.legacy_errors.is_empty(),
        "legacy must error on field-assign and consume of moved s"
    );
    assert!(
        predicate_uam_has(&run, "main", "s"),
        "predicate must fire UAM — field-assign is not a rebind"
    );
}

// ── Round 12.22: loop-of-consume rule ─────────────────────────────
//
// The formal RC predicate cannot fire when only one Consume site
// exists in source order — there is no second use site U to pair
// against. A consume inside a loop body is exactly that shape: the
// "second use" is the implicit next-iteration revisit of the same
// site. Round 12.22 supplies that witness via
// `loop_of_consume_candidates`, merged into
// `run_predicate_for_function`'s output.
//
// These shapes are NEW capability over the legacy linear forward
// state machine, which walks a loop body once and never paired the
// in-loop consume with the implicit next-iteration partner. Post
// round 12.21, `OwnershipChecker::populate_predicate_outputs` is
// the sole source of RC entries — so `rc_values` reflects the
// merged predicate output (formal RC + loop-of-consume), and the
// parity-run `legacy` map shows the predicate's view too. The
// tests pin (a) the predicate fires the new RC, (b) the legacy
// state machine itself raises no errors (no UAM hard-error for the
// in-loop consume), and (c) the witness carries the `Direct`
// flavor (the `RcTrigger::DirectReuseAfterConsume` shape).

#[test]
fn parity_loop_of_consume_predicate_fires_legacy_silent() {
    // The motivating shape: outer-scope binding, single Consume in
    // a `while` body, no other use, no in-loop reassign. Legacy
    // state machine is silent (single body walk → Moved, no error
    // because no further use); predicate fires loop-of-consume RC,
    // and post-12.21 routing makes `rc_values` agree.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   let mut i = 0;\n\
                   while i < 3 { consume(d); i = i + 1; }\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy state machine must not raise UAM on single in-loop consume; got {:?}",
        run.legacy_errors
    );
    assert!(
        predicate_has(&run, "main", "d"),
        "predicate loop-of-consume rule must fire for in-loop single consume; got {:?}",
        run.predicate.get("main")
    );
    assert_eq!(
        legacy_trigger(&run, "main", "d"),
        RcTrigger::DirectReuseAfterConsume,
        "post-12.21 routing carries the Direct flavor through to rc_values"
    );
    assert_eq!(
        predicate_origin(&run, "main", "d"),
        ConsumeOrigin::Direct,
        "loop-of-consume witness carries the Direct flavor"
    );
    assert!(
        !predicate_uam_has(&run, "main", "d"),
        "loop-of-consume is RC fallback, not UAM"
    );
}

#[test]
fn parity_loop_of_consume_for_loop_body() {
    // Same shape with a `for ... in ... { ... }` driver.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   for _i in 0..3 { consume(d); }\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy must not error on single-walk for-body consume; got {:?}",
        run.legacy_errors
    );
    assert!(
        predicate_has(&run, "main", "d"),
        "predicate loop-of-consume must fire on for-body consume"
    );
    assert!(!predicate_uam_has(&run, "main", "d"));
}

#[test]
fn parity_loop_of_consume_suppressed_by_in_loop_rebind() {
    // `let mut x; while c { consume(x); x = next(); }` — the rebind
    // closes the next-iteration gap. v1 coarse rule suppresses on
    // any in-loop reassign of the same binding, regardless of
    // position. Legacy also accepts this program (rebind resets
    // state to Live). Both must remain silent.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn fresh() -> Data { Data { value: 1 } }\n\
               fn main() {\n\
                   let mut d = Data { value: 1 };\n\
                   let mut i = 0;\n\
                   while i < 3 { consume(d); d = fresh(); i = i + 1; }\n\
               }";
    let run = run(src);
    assert!(
        run.legacy_errors.is_empty(),
        "legacy must accept rebind-in-loop; got {:?}",
        run.legacy_errors
    );
    assert!(
        run.predicate.get("main").is_none_or(|s| !s.contains("d")),
        "in-loop reassign must suppress loop-of-consume; got {:?}",
        run.predicate.get("main")
    );
    assert!(!predicate_uam_has(&run, "main", "d"));
}

#[test]
fn parity_loop_of_consume_yields_to_uam() {
    // Pre-loop consume + in-loop consume → formal UAM fires for the
    // in-loop site (cross-block strict dominance). Legacy also fires
    // UAM (linear forward state machine sees second consume after
    // the first is Moved). Loop-of-consume must yield — RC must NOT
    // be added on a binding the UAM predicate already flags.
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 1 };\n\
                   consume(d);\n\
                   while true { consume(d); }\n\
               }";
    let run = run(src);
    assert!(run
        .legacy_errors
        .iter()
        .any(|e| matches!(e, OwnershipErrorKind::UseAfterMove)));
    assert!(predicate_uam_has(&run, "main", "d"));
    assert!(
        run.predicate.get("main").is_none_or(|s| !s.contains("d")),
        "RC must not fire on a binding the UAM predicate already flags; got {:?}",
        run.predicate.get("main")
    );
}

// ── defer / errdefer cleanup-edge lowering (round 12.41) ──────────
//
// The CFG no longer walks `defer` / `errdefer` bodies inline at the
// declaration site; bodies are re-lowered on every exit edge that
// crosses the scope, with `errdefer` items firing only on error
// paths (?-error, return). This closes a soundness gap in the
// legacy state machine, which checked the body in a *cloned* state
// at the declaration site (`ownership.rs:1619-1622`) — meaning a
// subsequent consume of an outer binding read by the defer body was
// not detected as a use-after-move. The new predicate pipeline
// detects the UAM correctly. The legacy pass remains silent on this
// shape; the parity tests below assert the predicate fires while
// noting the legacy gap as a known limitation closed by the
// in-place integration when the predicate becomes authoritative.

#[test]
fn parity_defer_then_consume_predicate_fires_uam() {
    // `let x; defer use(x); consume(x);` — per design.md "bindings
    // used in a defer/errdefer block must remain valid through the
    // scope exit", consuming x before scope exit makes the defer
    // body's read invalid. The CFG places the defer body's read on
    // every exit edge (here: fall-through), strictly downstream of
    // the consume, so the UAM predicate fires.
    let src = "struct Data { value: i64 }\n\
               fn use_d(d: ref Data) { }\n\
               fn drop_d(d: Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 0 };\n\
                   defer { use_d(d); }\n\
                   drop_d(d);\n\
               }";
    let run = run(src);
    assert!(
        predicate_uam_has(&run, "main", "d"),
        "defer body read after scope-local consume must fire UAM; got {:?}",
        run.predicate_uam.get("main")
    );
    // RC must NOT also fire — UAM and RC are mutually exclusive per
    // binding (the cleanup chain strictly post-dominates the
    // consume, so the pair is dominance-comparable).
    assert!(
        run.predicate.get("main").is_none_or(|s| !s.contains("d")),
        "UAM shape must not also fire RC; got {:?}",
        run.predicate.get("main")
    );
}

#[test]
fn parity_defer_no_consume_is_clean() {
    // `let x; defer use(x);` with no subsequent consume — clean.
    // The defer body fires at scope exit reading a still-live
    // binding; predicate must not flag anything.
    let src = "struct Data { value: i64 }\n\
               fn use_d(d: ref Data) { }\n\
               fn main() {\n\
                   let d = Data { value: 0 };\n\
                   defer { use_d(d); }\n\
               }";
    let run = run(src);
    assert!(
        run.predicate.get("main").is_none_or(|s| s.is_empty()),
        "clean defer-then-end shape must not fire RC; got {:?}",
        run.predicate.get("main")
    );
    assert!(
        run.predicate_uam.get("main").is_none_or(|s| s.is_empty()),
        "clean defer-then-end shape must not fire UAM; got {:?}",
        run.predicate_uam.get("main")
    );
}

#[test]
fn parity_errdefer_on_question_error_path_predicate_fires_uam() {
    // `let x; errdefer use(x); let _ = try_op()?; consume(x);` —
    // on the ?-error path the errdefer body fires before exit,
    // reading x. On the success path, control reaches consume(x)
    // and then fall-through (no errdefer on success). The error
    // path's read is dominance-incomparable with consume (different
    // path), but on the success path's fall-through cleanup the
    // errdefer does not fire — so no read pairs with consume.
    //
    // The interesting case is the pre-question consume: putting
    // consume BEFORE the `?` makes the errdefer body's read on the
    // ?-error path strictly dominated by consume → UAM fires.
    let src = "struct Data { value: i64 }\n\
               struct E { msg: i64 }\n\
               struct T { v: i64 }\n\
               fn use_d(d: ref Data) { }\n\
               fn drop_d(d: Data) { }\n\
               fn try_op() -> Result[T, E] { Ok(T { v: 0 }) }\n\
               fn main() -> Result[T, E] {\n\
                   let d = Data { value: 0 };\n\
                   errdefer { use_d(d); }\n\
                   drop_d(d);\n\
                   let _y = try_op()?;\n\
                   Ok(T { v: 0 })\n\
               }";
    let run = run(src);
    assert!(
        predicate_uam_has(&run, "main", "d"),
        "consume before ? with errdefer reading the consumed binding must fire UAM; got {:?}",
        run.predicate_uam.get("main")
    );
}
