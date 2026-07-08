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
//! **Soundness — three alignment rules, each pinned by a false positive it
//! removed** (the corpus went 792 → 392 → 111 → 0 divergences as they went in):
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
//!  3. **Skip the §7 closure / cross-task capture edge.** The oracle walks a
//!     closure with Read role (conservative — never moves the captured parent),
//!     keeping a `spawn`/`par`-captured heap value Owned and scheduling a drop
//!     codegen elides (the task frees it). Documented model-conservatism, not a
//!     leak; such programs return [`DiffOutcome::CaptureEdge`] (counted, not
//!     silently dropped).
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
    /// Contains a closure / cross-task capture construct (`par {}` /
    /// `pool.spawn(|| …)`) — the oracle's §7 open edge, excluded from the gate.
    CaptureEdge,
    /// Checked: the oracle's local drop schedule was compared against codegen's
    /// emitted set. `divergences` is empty on agreement.
    Checked {
        /// Distinct scheduled local drop places checked against codegen.
        drops_checked: usize,
        divergences: Vec<Divergence>,
    },
}

/// Whether a program contains a capture construct the differential still skips.
/// `spawn` closures are now modelled (the oracle demotes a spawn-captured heap
/// binding to Borrowed — no scope drop, matching codegen's RC/join free — see
/// `ownership_oracle`'s `Closure` handling), so they are checked. `par {}`
/// blocks remain the open §7 edge: their captures interact with `shared struct`
/// RC promotion the oracle does not yet model, so those programs are still
/// skipped (and counted).
pub fn has_capture_construct(src: &str) -> bool {
    src.contains("par {")
}

/// Compile `src` in-process with the drop recorder armed and diff the oracle's
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
    if has_capture_construct(src) {
        return DiffOutcome::CaptureEdge;
    }

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
        // Distinct scheduled LOCAL places (dedup; params discharged caller-side).
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
