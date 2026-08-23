//! Oracle↔codegen drop differential (ownership-model-mechanization Slice 4).
//!
//! Compares the Slice-3 ownership oracle's per-function *drop schedule*
//! ([`crate::ownership_oracle`]) against the drops codegen actually emits
//! (recorded by [`crate::codegen::drop_obs`]). A divergence in the direction we
//! check is a **missing drop**: the oracle schedules a drop for a place codegen
//! emitted no cleanup action for — i.e. a leak, localized to `(function,
//! place)`. This is the observability half of Slice 4; the structural half
//! (codegen *consuming* the schedule) lands behind it later, with this
//! differential as the regression net.
//!
//! Lives in the lib (behind `--features llvm`, since it drives codegen) so both
//! the `drop_fuzz --differential` corpus runner and `tests/drop_differential.rs`
//! (the standing gate over canonical heap-core shapes) share one implementation.
//!
//! **Soundness — four alignment rules, each pinned by a false positive it
//! removed** (the corpus went 792 → 392 → 111 → 0 divergences as rules 1-3
//! went in; rule 4 arrived later and is neutral on it):
//!
//! **The corpus number is a live gate, not a historical note.** It sat at 94
//! (186 at `--count 400`) for a while against a doc that still read 0
//! (B-2026-08-23-5) — a red gate nobody was reading, because the curated
//! `tests/drop_differential.rs` shapes stayed green. The cause was coverage,
//! and worth stating so it is not re-learned: **no curated shape declared an
//! `impl Drop`**, and every one of the 94 needed one. The corpus is back to 0
//! at both sizes, the two defects it was reporting are fixed (an unrecorded
//! NLL firing path, and the oracle reading an enum-variant constructor's
//! argument rather than moving it), and the curated file now carries
//! Drop-bearing cases so the standing gate covers the class too. If this
//! number and the corpus disagree again, believe the corpus.
//!
//!  1. **Oracle on the *surface* tree** (before `lower`). The oracle's model
//!     and its unit tests are defined over source syntax; `lower` desugars
//!     for-loops / matches / method chains into fresh-named temporaries the
//!     oracle would then schedule but codegen handles internally. Running
//!     `analyze` pre-`lower` keys the schedule on user source-binding names —
//!     the same names `create_entry_alloca` gives codegen's slots.
//!  2. **Local drops only, not parameters.** The oracle models an owned heap
//!     *param* as callee-owned (it drops at the callee's exit); codegen frees a
//!     bare `String`/`Vec`/`Map` param **caller-side** (caller-retains — the
//!     callee emits no cleanup). Both free exactly once, across the call
//!     boundary, so a per-callee comparison would false-positive on params.
//!  4. **Fixed-array locals are excluded, and this one is a KNOWN GAP rather
//!     than a clean alignment** (B-2026-08-23-2). The oracle schedules one
//!     array-keyed drop for an owned `Array[T, N]` local. Codegen frees the
//!     same element buffers, but never through an array-keyed action: it has
//!     an array drop fn (`synthesize_array_drop_fn_te`) and uses it only for
//!     PARAMS, so for a local the elements are discharged through whatever
//!     owns them individually — the source bindings, or the f-string / temp
//!     cleanups. Measured under LSan across four shapes (f-string elements,
//!     named bindings moved in, call-result elements, and an array returned
//!     out of its function): all clean, so this is a place-KEY mismatch, not a
//!     leak. Comparing by place name would false-positive on every one.
//!
//!     Unlike rules 1-3, this exclusion does NOT reflect a genuine ownership
//!     difference — it reflects codegen owning local arrays element-wise where
//!     it owns param arrays whole. Closing it means making codegen emit the
//!     array-keyed drop for locals and suppressing the element sources, at
//!     which point this filter comes out and the class becomes gated by name
//!     like every other. Tracked separately; the LSan array fixtures remain
//!     the gate on the actual leak until then.
//!
//!  3. **Captures are modelled, not skipped.** A `spawn`-closure capture escapes
//!     as an auto-promoted shared/RC reference, so the oracle demotes the
//!     captured heap binding to `Borrowed` (no scope drop — codegen frees it via
//!     the RC/join, not scope cleanup); a `par {}` block captures `shared struct`
//!     values whose scope-exit `RcDec` *is* the drop the oracle schedules. Both
//!     match codegen with 0 divergences over the whole corpus, so the differential
//!     checks 100% of generated programs (no capture skip). The general
//!     borrow-*escape decision procedure* for stored/heap-env closures is still
//!     open (judgment §7) but is not exercised by the fuzzer.
//!
//! Only the **missing-drop (leak)** direction is checked. The extra-drop
//! (double-free) direction is not emit-time observable — codegen neutralizes a
//! moved-out value's drop with a runtime null/cap guard while keeping the
//! cleanup action, so a guarded no-op is indistinguishable from a real free at
//! emit time. The ASan/LSan fuzzer run stays the double-free authority.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::ast::{Function, ImplItem, Item, Program};

/// One place where codegen's emitted drop set diverges from the oracle's
/// schedule (always a missing drop, in the direction this differential checks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub function: String,
    pub place: String,
}

/// The result of checking one program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffOutcome {
    /// Not a valid differential subject (parse / type / ownership error, or a
    /// codegen failure) — not counted toward coverage.
    Invalid,
    /// Reserved: a program the differential deliberately skips. No longer
    /// produced — both `spawn` closure captures (oracle demotes to Borrowed) and
    /// `par {}` shared-struct captures (freed via scope-exit `RcDec`, which the
    /// oracle schedules) are now modelled and checked with 0 divergences over
    /// the corpus. Retained so a future *known*-divergent capture shape can be
    /// routed here explicitly rather than surfacing as a spurious divergence.
    CaptureEdge,
    /// Checked: the oracle's local drop schedule was compared against codegen's
    /// emitted set. `divergences` is empty on agreement.
    Checked {
        /// Distinct scheduled local drop places checked against codegen.
        drops_checked: usize,
        divergences: Vec<Divergence>,
    },
}

/// Which tree the oracle analyzes. The comparison is sound either way (validated
/// 0-divergence on the corpus for both), and codegen's own inline self-check
/// (`KARAC_ORACLE_DROP_CHECK`) uses `Lowered` — it analyzes the tree it already
/// holds, which is why no surface tree needs threading into codegen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OracleTree {
    /// Analyze before `lower` — matches the oracle's model + unit tests.
    Surface,
    /// Analyze after `lower` — matches what codegen's inline self-check does.
    Lowered,
}

/// Compile `src` in-process with the drop recorder armed and diff the oracle's
/// per-function schedule against codegen's emitted drop set. See the module doc
/// for the three alignment rules that make this sound. Analyzes on the surface
/// tree; use [`differential_check_on`] to pick the tree.
pub fn differential_check(src: &str) -> DiffOutcome {
    differential_check_on(src, OracleTree::Surface)
}

/// [`differential_check`] with an explicit oracle-tree choice. Both trees are
/// validated to agree with codegen (0 divergences on the corpus); `Lowered`
/// mirrors codegen's inline self-check.
pub fn differential_check_on(src: &str, tree: OracleTree) -> DiffOutcome {
    let mut parsed = crate::parse(src);
    if !parsed.errors.is_empty() {
        return DiffOutcome::Invalid;
    }
    let resolved = crate::resolve(&parsed.program);
    let typed = crate::typecheck(&parsed.program, &resolved);
    if !typed.errors.is_empty() {
        return DiffOutcome::Invalid;
    }

    // On the SURFACE tree, analyze before lowering (rule 1 & 2).
    let surface = (tree == OracleTree::Surface).then(|| {
        (
            crate::ownership_oracle::analyze(&parsed.program),
            param_names_by_function(&parsed.program),
        )
    });

    // Lower + ownership-check for codegen (codegen consumes the lowered tree).
    crate::lower(&mut parsed.program, &typed);
    let ownership = crate::ownershipcheck(&parsed.program, &typed);
    if !ownership.errors.is_empty() {
        return DiffOutcome::Invalid;
    }

    // On the LOWERED tree, analyze after lowering — the tree codegen sees.
    let (oracle, params) = surface.unwrap_or_else(|| {
        (
            crate::ownership_oracle::analyze(&parsed.program),
            param_names_by_function(&parsed.program),
        )
    });

    // Seq surface (concurrency = None) to match the oracle's sequential model.
    // The recorder fires inside `compile_to_ir`'s cleanup drain; take
    // unconditionally so the thread-local sink resets even on codegen error.
    crate::codegen::drop_obs::begin();
    let ir = crate::codegen::compile_to_ir(&parsed.program, Some(&ownership), None);
    let recs = crate::codegen::drop_obs::take();
    if ir.is_err() {
        return DiffOutcome::Invalid;
    }

    // Codegen's emitted drop set, per function → distinct places.
    let mut cg: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in &recs {
        cg.entry(r.function.as_str())
            .or_default()
            .insert(r.place.as_str());
    }
    let empty: HashSet<String> = HashSet::new();

    let mut drops_checked = 0usize;
    let mut divergences = Vec::new();
    for f in &oracle.functions {
        let cg_places = cg.get(f.function.as_str());
        let fn_params = params.get(&f.function).unwrap_or(&empty);
        // Distinct scheduled LOCAL places (dedup; params discharged
        // caller-side — rule 2).
        //
        // Fixed arrays were excluded here too (rule 4) while codegen owned a
        // LOCAL one element-wise and a PARAM one whole: comparing by PLACE
        // NAME matched an oracle-scheduled `a` against a codegen-recorded
        // `x` / `y` / `fstr.acc` and reported a false divergence on every one.
        // B-2026-08-23-4 gave the local path the same array-keyed drop the
        // param path already had, so the class is gated by name like every
        // other and the rule is gone. Unlike rules 1-3 it never recorded two
        // correct models legitimately disagreeing — it was a known gap.
        let scheduled: BTreeSet<&str> = f
            .drops
            .iter()
            .map(|d| d.place.as_str())
            .filter(|p| !fn_params.contains(*p))
            .collect();
        for place in scheduled {
            drops_checked += 1;
            let emitted = cg_places.is_some_and(|s| s.contains(place));
            if !emitted {
                divergences.push(Divergence {
                    function: f.function.clone(),
                    place: place.to_string(),
                });
            }
        }
    }
    DiffOutcome::Checked {
        drops_checked,
        divergences,
    }
}

/// Parameter names of every free function and impl method in the surface tree,
/// keyed by function name — so the differential can exclude param-drop
/// obligations (discharged caller-side, not at the callee; rule 2).
pub fn param_names_by_function(program: &Program) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let mut add = |name: &str, f: &Function| {
        let ps = f
            .params
            .iter()
            .filter_map(|p| p.name().map(|s| s.to_string()))
            .collect();
        out.insert(name.to_string(), ps);
    };
    for item in &program.items {
        match item {
            Item::Function(f) => add(&f.name, f),
            Item::ImplBlock(b) => {
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        add(&m.name, m);
                    }
                }
            }
            _ => {}
        }
    }
    out
}
