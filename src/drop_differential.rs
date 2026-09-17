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
//! **What never reaches the comparison is reported, not dropped**
//! (B-2026-09-10-30). A program can leave the corpus three ways, and they are
//! three different facts: the front end rejected it ([`DiffOutcome::NotASubject`],
//! carrying which gate), codegen refused to lower it
//! ([`DiffOutcome::CodegenRefused`], carrying the diagnostic), or it is a known
//! capture edge ([`DiffOutcome::CaptureEdge`]). These used to be one silent
//! `Invalid`, which made `programs checked` a self-selecting denominator: every
//! shape codegen chokes on left the corpus, so the divergence ratio could sit at
//! 0 while the compiler got worse on exactly those shapes. The runner now prints
//! all three counts, and `--fail-on-codegen-refused` turns the second into a
//! gate.
//!
//! Only the **missing-drop (leak)** direction is checked. The extra-drop
//! (double-free) direction is not emit-time observable — codegen neutralizes a
//! moved-out value's drop with a runtime null/cap guard while keeping the
//! cleanup action, so a guarded no-op is indistinguishable from a real free at
//! emit time. The ASan/LSan fuzzer run stays the double-free authority.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

/// Whether `KARAC_DIFF_EXPLAIN=1` armed the side-by-side dump. Read once — env
/// is fixed for a process — mirroring `drop_obs::silenced`.
fn explain_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("KARAC_DIFF_EXPLAIN").as_deref() == Ok("1"))
}

use crate::ast::{Function, ImplItem, Item, Program};

/// One place where codegen's emitted drop set diverges from the oracle's
/// schedule (always a missing drop, in the direction this differential checks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Divergence {
    pub function: String,
    pub place: String,
}

/// The result of checking one program.
///
/// The two non-subject variants are deliberately **not** one value
/// (B-2026-09-10-30). They used to be: a single `Invalid` covered parse,
/// typecheck, ownership *and* `compile_to_ir` failures alike, so a program
/// codegen could not lower left the corpus with the same silent status as one
/// that never typechecked. That is backwards for a gate whose job is to find
/// codegen defects — it went blind exactly where codegen is weakest, and
/// "codegen refuses this shape" is its own standing ledger class. Splitting
/// them costs one variant and turns a silent exclusion into a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffOutcome {
    /// The program never reached codegen, so there is nothing to compare: it
    /// failed to parse, typecheck, or ownership-check. Genuinely not a
    /// differential subject — not counted toward coverage, and not a finding.
    ///
    /// `reason` names which of the three gates rejected it (`"parse"`,
    /// `"typecheck"`, `"ownership"`), because a caller that cannot tell them
    /// apart cannot tell a generator bug from a compiler one.
    NotASubject { reason: &'static str },
    /// The program parsed, typechecked and ownership-checked — and then
    /// **codegen refused it** (`compile_to_ir` returned an error). Not counted
    /// toward coverage either (there are no emitted drops to compare against),
    /// but this is a *finding-shaped* skip rather than an uninteresting one:
    /// every such program is a shape the front end accepts and the backend
    /// cannot lower.
    ///
    /// `error` is the codegen diagnostic, kept so a runner can group refusals
    /// by message — the distinct messages are the actionable output, since one
    /// unlowerable shape reproduces across every generated program containing
    /// it.
    CodegenRefused { error: String },
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
        return DiffOutcome::NotASubject { reason: "parse" };
    }
    let resolved = crate::resolve(&parsed.program);
    let typed = crate::typecheck(&parsed.program, &resolved);
    if !typed.errors.is_empty() {
        return DiffOutcome::NotASubject {
            reason: "typecheck",
        };
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
        return DiffOutcome::NotASubject {
            reason: "ownership",
        };
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
    // B-2026-09-10-30: a refusal HERE is not the same event as a parse or type
    // error above. The program is well-formed by every front-end rule and the
    // backend still cannot lower it, which is the defect class this gate exists
    // to find — so it gets its own outcome and carries the diagnostic out,
    // rather than joining the front-end rejects in one silent bucket.
    if let Err(e) = ir {
        return DiffOutcome::CodegenRefused {
            error: e.to_string(),
        };
    }

    // Codegen's emitted drop set, per function → distinct places.
    let mut cg: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in &recs {
        cg.entry(r.function.as_str())
            .or_default()
            .insert(r.place.as_str());
    }
    let empty: HashSet<String> = HashSet::new();

    // `KARAC_DIFF_EXPLAIN=1` dumps both sides of the comparison to stderr, in
    // the same off-by-default style as `KARAC_DROPOBS_SILENCE`. Without it the
    // only observable is the verdict, and a `checked=0` verdict is ambiguous in
    // exactly the way that matters: it reads the same whether the oracle
    // scheduled nothing (a MODEL gap, B-2026-09-11-2) or scheduled something
    // codegen also emitted. Seeing which side is empty is the whole diagnosis,
    // so the knob prints the oracle's schedule and codegen's places side by
    // side rather than making the next reader re-derive them.
    if explain_enabled() {
        for f in &oracle.functions {
            eprintln!("[explain] fn {}", f.function);
            for d in &f.drops {
                match &d.via {
                    Some(v) => eprintln!("[explain]   oracle: `{}` (via `{v}`)", d.place),
                    None => eprintln!("[explain]   oracle: `{}`", d.place),
                }
            }
            match cg.get(f.function.as_str()) {
                Some(places) => {
                    for p in places {
                        eprintln!("[explain]   codegen: `{p}`");
                    }
                }
                None => eprintln!("[explain]   codegen: (no records)"),
            }
        }
    }

    // Rule 6's call-boundary inputs. Built from the same tree the oracle
    // analyzed, so a param's index and a call site's argument index agree.
    let params_order = param_order_by_function(&parsed.program);
    let call_sites = call_site_args(&parsed.program);

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
        //
        // Rule 5 — MATCH-PAYLOAD PLACE EQUIVALENCE (B-2026-09-10-31). A `match`
        // arm's payload binding owns the payload (§3.4's obligation split), so
        // the oracle schedules it under the BINDING's name; codegen discharges
        // it through the SCRUTINEE's slot. Measured on
        // `match ov { Some(x) => .. }`: codegen records `main::ov` and no
        // `main::x`, and the program is correct at runtime (the `Drop` body
        // runs exactly once, valgrind-clean). Two right answers, two names — so
        // the obligation is covered if EITHER is emitted, which the oracle
        // states per-event as `DropEvent::via`.
        //
        // This is deliberately an equivalence and not an exclusion: excluding
        // match payloads is what made the schedule empty for every matched
        // value in the first place, which is the blindness the row measured. If
        // codegen emits NEITHER name, that is still a divergence and still
        // reported.
        //
        // B-2026-09-11-2 leans on the same channel for a payload NO arm binds.
        // `_` discards the binding, not the obligation, so the oracle schedules
        // it under a synthetic place (`<discarded from ov>`) that is unspellable
        // in Kāra and therefore can never match `cg_places` by name — the match
        // is carried entirely by `via`. That is why the oracle only emits such
        // an event when it has a named scrutinee to put in `via`: without one
        // the event could never be covered and would be a guaranteed false
        // divergence rather than a finding.
        // RULE 2 COVERS THE PAYLOAD OF A MATCHED PARAM TOO, AND IT HAS TO BE
        // TESTED ON `via` RATHER THAN ON THE PLACE (B-2026-09-12-27). Once the
        // oracle learned parameter types, `match o { Some(x) => .. }` on a
        // by-value param began scheduling the payload under the ARM BINDING's
        // name (`x`, or `<discarded from o>` for a wildcard) with `via = o`.
        // Neither of those names is a parameter, so the place-keyed filter above
        // let them through — and they are exactly the obligations rule 2 exists
        // to exclude, discharged across the call boundary where a per-callee
        // comparison cannot see them.
        //
        // MEASURED, on `struct Held { tag: i64, buf: String }` with a user
        // `Drop`, matched out of `fn eat(o: Option[Held])` — all four cells emit
        // the IDENTICAL oracle event (`t` via `o`) and the identical empty
        // codegen record set for `eat`, because codegen discharges the payload
        // in the CALLER (`ot`, or `__optres_arg_bodies_tmp` for a fresh temp):
        //
        //   named local arg, arm reads `t.tag`   correct program  -> FALSE divergence
        //   named local arg, arm ignores `t`     correct program  -> FALSE divergence
        //   fresh temp arg,  arm ignores `t`     correct program  -> FALSE divergence
        //   fresh temp arg,  arm reads `t.tag`   Drop body LOST   -> true  divergence
        //
        // Three of the four are correct programs, and no per-function
        // comparison can separate them from the fourth: the discriminator lives
        // in another function's records. Reporting all four would redden the
        // gate on correct code, so the class is excluded on the same grounds as
        // the param itself. The corpus does not currently generate the shape (0
        // divergences over 1600 programs unfiltered), which is why this is a
        // guard against a future generator widening rather than a live fix.
        //
        // The cost is real and is not hidden: this takes the matched-param
        // population back out of the comparison, so the oracle's now-correct
        // schedule for it is unwatched. Closing that needs a CROSS-FUNCTION
        // discharge check (the callee's obligation covered by the caller's
        // record), which the per-function design cannot express today.
        let scheduled: BTreeSet<(&str, Option<&str>)> = f
            .drops
            .iter()
            .map(|d| (d.place.as_str(), d.via.as_deref()))
            .filter(|(p, _)| !fn_params.contains(*p))
            .collect();
        for (place, via) in scheduled {
            // RULE 6 — CROSS-FUNCTION DISCHARGE (B-2026-09-13-4), the successor
            // to rule 2's blanket exclusion of a matched PARAM's payload.
            //
            // Rule 2 excluded the whole population because the obligation is
            // discharged in the CALLER and the comparison is per-callee. That
            // was the only available answer while `param_names_by_function` was
            // the sole call-boundary information: it can say "this is a
            // parameter" and nothing about which caller-side place covers it.
            // `call_site_args` supplies the missing half, so the obligation can
            // be RESOLVED instead of dropped.
            //
            // THE DEFAULT IS STILL EXCLUSION, and that is the soundness
            // property rather than caution: every branch that cannot PROVE
            // coverage one way or the other falls back to today's behaviour.
            // A divergence is reported only when the call-site set is known
            // complete, every argument at that position is a nameable place,
            // and one of those places is absent from its own caller's records.
            if let Some(v) = via.filter(|v| fn_params.contains(*v)) {
                match cross_fn_discharge(&f.function, v, &params_order, &call_sites, &cg) {
                    CrossFn::Unresolvable => continue,
                    CrossFn::Covered => {
                        drops_checked += 1;
                        continue;
                    }
                    CrossFn::Uncovered => {
                        drops_checked += 1;
                        divergences.push(Divergence {
                            function: f.function.clone(),
                            place: place.to_string(),
                        });
                        continue;
                    }
                }
            }
            drops_checked += 1;
            let emitted =
                cg_places.is_some_and(|s| s.contains(place) || via.is_some_and(|v| s.contains(v)));
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

/// Rule 6's verdict for one callee obligation discharged across the call
/// boundary. See the comment at its use site for why `Unresolvable` is the
/// default rather than a failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CrossFn {
    /// Every call site passes a nameable place at this position, and every one
    /// of those places is recorded by its own caller. The obligation is met.
    Covered,
    /// Every call site passes a nameable place, and at least one caller emitted
    /// no record for it — a missing drop, localized to the callee's place.
    Uncovered,
    /// Nothing can be concluded: no call site was found (the callee is
    /// unreferenced, or reached only through a method call, which
    /// `call_site_args` deliberately does not collect), or some argument is a
    /// TEMPORARY, which codegen discharges through a synthesized place
    /// (`__optbox_arg_tmp0`) that no source name can match. Excluded exactly as
    /// rule 2 excluded it.
    Unresolvable,
}

/// Resolve a callee obligation whose `via` is the parameter `param` of
/// `callee`: is the place each caller passes at that position recorded as
/// dropped by that caller?
fn cross_fn_discharge(
    callee: &str,
    param: &str,
    params_order: &HashMap<String, Vec<String>>,
    call_sites: &HashMap<(String, usize), Vec<CallSiteArg>>,
    cg: &BTreeMap<&str, BTreeSet<&str>>,
) -> CrossFn {
    let Some(idx) = params_order
        .get(callee)
        .and_then(|ps| ps.iter().position(|p| p == param))
    else {
        return CrossFn::Unresolvable;
    };
    let Some(sites) = call_sites.get(&(callee.to_string(), idx)) else {
        return CrossFn::Unresolvable;
    };
    if sites.is_empty() {
        return CrossFn::Unresolvable;
    }
    let mut all_covered = true;
    for site in sites {
        let Some(root) = site.root.as_deref() else {
            return CrossFn::Unresolvable;
        };
        if !cg
            .get(site.caller.as_str())
            .is_some_and(|places| places.contains(root))
        {
            all_covered = false;
        }
    }
    if all_covered {
        CrossFn::Covered
    } else {
        CrossFn::Uncovered
    }
}

/// For each function, the parameter NAMES in declaration order — the index side
/// of [`param_names_by_function`]'s set, so a call site's argument position can
/// be matched to the parameter it binds.
fn param_order_by_function(program: &Program) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    let mut add = |name: &str, f: &Function| {
        out.insert(
            name.to_string(),
            f.params
                .iter()
                .map(|p| p.name().unwrap_or("").to_string())
                .collect(),
        );
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

/// One argument at one call site: which function contains the call, and the
/// caller-side ROOT PLACE the argument is rooted at (`None` for a temporary —
/// a constructor, a call result, a literal).
#[derive(Clone, Debug)]
struct CallSiteArg {
    caller: String,
    root: Option<String>,
}

/// Every `f(..)` call site in the surface tree, keyed by
/// `(callee name, argument index)`.
///
/// B-2026-09-13-4 — the call-boundary information rule 2's successor needs.
/// `param_names_by_function` could only answer "is this place a parameter",
/// which is why the matched-param population could only be EXCLUDED: the
/// obligation is discharged in the caller, and nothing here could say which
/// caller-side place discharged it.
///
/// COMPLETENESS IS THE SOUNDNESS CONDITION, and it is why this reuses
/// `codegen::param_transfer::visit_block` rather than a local walker. A MISSED
/// call site reads as "this obligation has no caller that covers it", i.e. a
/// false divergence on correct code — the exact failure the exclusion existed to
/// avoid. That walker is exhaustive over `ExprKind` by construction (no `_`
/// arm, so a new variant fails the build there), so the set is complete or the
/// compiler says so.
///
/// Method calls are NOT collected: resolving a receiver to an impl block needs
/// the type, which this pass does not carry, and a wrongly-attributed call site
/// is the unsound direction. They stay excluded, as they are today.
fn call_site_args(program: &Program) -> HashMap<(String, usize), Vec<CallSiteArg>> {
    use crate::codegen::param_transfer::{place_root, visit_block, Node};
    let mut out: HashMap<(String, usize), Vec<CallSiteArg>> = HashMap::new();
    let mut collect = |caller: &str, body: &crate::ast::Block| {
        visit_block(body, &mut |n| {
            let Node::Expr(e) = n else { return };
            let crate::ast::ExprKind::Call { callee, args } = &e.kind else {
                return;
            };
            let crate::ast::ExprKind::Identifier(name) = &callee.kind else {
                return;
            };
            for (i, a) in args.iter().enumerate() {
                out.entry((name.clone(), i)).or_default().push(CallSiteArg {
                    caller: caller.to_string(),
                    root: place_root(&a.value).map(|s| s.to_string()),
                });
            }
        });
    };
    for item in &program.items {
        match item {
            Item::Function(f) => collect(&f.name, &f.body),
            Item::ImplBlock(b) => {
                for it in &b.items {
                    if let ImplItem::Method(m) = it {
                        collect(&m.name, &m.body);
                    }
                }
            }
            _ => {}
        }
    }
    out
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
