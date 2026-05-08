# WIP — List 1 (serial work, this session)

When picking up work, also mirror the bullet (with the box checked off as
work progresses) into the relevant `phase-N-*.md` tracker so the durable
record lives alongside every other completed phase entry.

---

## Theme: small/contained checklist items (2026-05-05)

Picking up a sequence of small, contained checklist items so each can
ship as its own commit. Original tracker entries get checked off as
each one closes.


- [x] **Bench script: wire `build_kara` step into `bench/bench.sh`.** Add a `build_kara()` function next to the existing `build_rust()` in `kata-katas/leetcode/1-100/1-two-sum/bench/bench.sh`, invoke `karac build` to produce native binaries, add `kara brute_force (codegen)` and `kara hash_map (codegen)` rows to the hyperfine command. Source: v62 brainstorm decision #8 (execution-only, no design fork).

- [x] **README §Benchmarks rewrite.** Once the bench script change above lands, rewrite `kata-katas/leetcode/1-100/1-two-sum/README.md` §Benchmarks to lead with codegen numbers; demote interpreter row to "what the compile pipeline costs today" context. Source: v62 brainstorm decision #9.

- [x] **N=5000 bump in all five bench files.** Change the `N=200` constant to `N=5000` in all five bench source files in `kata-katas/leetcode/1-100/1-two-sum/bench/` (`hash_map.{rs,py,kara}`, `brute_force.{rs,py,kara}` — adjust list to actual files). Single commit. Real N=5000 numbers already captured in v62 doc: kara_brute_force 97.5 ± 0.2 ms (rust 32.8 ± 0.5 ms — 3.0× gap); kara_hash_map 2.8 ± 0.1 ms (rust 2.1 ± 0.2 ms — 1.4× gap). Source: v62 brainstorm decision #10.

---

## Theme: method-resolution slices 3 + 3.5 (autonomous queue, 2026-05-07)

Two consecutive slices closing the autonomous-friendly tail of the
method-resolution CR (`phase-4-interpreter.md` § "TypeChecker: implement
full method resolution algorithm"). Per-slice plans are drafted under
their respective items in that tracker; this list is the execution
order + checkbox mirror.

Slice 4 (`impl Option[Ordering]` storage-shape change) and the parser
CR for concrete-type UFCS stay in **discussion mode** — not in this
queue. Both have too many design forks for autonomous run.

Run-time rules (per the parent CR roadmap entry's "Autonomous queue"
note):
- Per-slice commit: plan + impl combined (the plan already lives in the
  phase tracker by virtue of being drafted before the autonomous run).
- Between slices: `cargo test`, `cargo test --features llvm`,
  `cargo clippy --all --tests -- -D warnings`, `cargo fmt --check` all
  clean.
- Hard-stop on: pre-existing test breakage requiring a design fork;
  parser/AST shape changes; effect-checker or ownership-checker
  invariants turning out load-bearing in unanticipated ways.
- Soft-stop on: clippy lint corner cases, doc placement nits.

Slice ordering is sequential (3 → 3.5) — both touch
`infer_method_call`'s `Type::TypeParam` arm, so serial execution keeps
merge surface tight and lets 3.5 reuse 3's `AmbiguousMethod` variant.

- [ ] **Slice 3 — Ambiguity detection on receiver form (item 4).** When more than one user-impl method survives the inherent-beats-trait priority filter at a receiver-form call, emit `AmbiguousMethod` listing all candidates with UFCS hints instead of silent first-match. Inherent-beats-trait priority preserved (item 3 unchanged). Plan: `phase-4-interpreter.md` item 4 § "Slice 3 plan". Source: parent CR roadmap.

- [ ] **Slice 3.5 — Self-receiver dispatch (item 8 follow-up).** `self.method()` inside a trait default body resolves through the enclosing trait's own methods + supertrait closure (currently silent fallthrough). Closes the `name != "Self"` exclusion slice 2 left in place. Five pre-existing tests get a real resolution path; new negative test pins the closed silent-fallthrough hole. Plan: `phase-4-interpreter.md` item 8 § "Slice 3.5 plan". Source: slice 2 deferred item.
