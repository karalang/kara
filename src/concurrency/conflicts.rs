//! Statement conflict analysis: the pairwise dependency tests
//! (`statements_conflict`, the effect-conflict lattice) and the
//! serialization-point detail reporting (`conflict_detail`).
//!
//! Extracted verbatim from `concurrency.rs`'s `ConcurrencyChecker` impl
//! (structural-debt extraction, slice 2). Sibling `impl super::…` block;
//! methods are `pub(super)`.

use super::*;

impl<'a> super::ConcurrencyChecker<'a> {
    /// Explain a single conflicting statement pair: the cause, the
    /// resource at issue, and (for an effect conflict) the callees whose
    /// effect on that resource forced the serialization. Mirrors the
    /// decision order of [`Self::statements_conflict`] and returns the
    /// first cause found. `statement_indices` is filled in by the caller.
    pub(super) fn conflict_detail(&self, a: &StmtInfo, b: &StmtInfo) -> Option<SerializationPoint> {
        let mk = |reason: String,
                  resource: String,
                  blocking_callees: Vec<String>,
                  cause: SerializationCause| {
            Some(SerializationPoint {
                statement_indices: Vec::new(),
                reason,
                resource,
                blocking_callees,
                cause,
            })
        };

        if a.is_seq || b.is_seq {
            return mk(
                "explicit seq ordering".to_string(),
                String::new(),
                Vec::new(),
                SerializationCause::SeqOrdering,
            );
        }

        // Data dependency: one reads a binding the other defines. `a` is the
        // earlier statement, `b` the later (the caller passes ascending
        // indices), so `a.defines ∩ b.reads` is read-after-write (a true flow
        // dependency) and `b.defines ∩ a.reads` is write-after-read (an
        // anti-dependency). RAW dominates the `kind` tag when both are present.
        let raw_present = a.defines.intersection(&b.reads).next().is_some();
        let mut dep: Vec<&String> = a.defines.intersection(&b.reads).collect();
        dep.extend(b.defines.intersection(&a.reads));
        if !dep.is_empty() {
            dep.sort();
            dep.dedup();
            let names = dep
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let vars = dep.iter().map(|s| (*s).clone()).collect();
            let kind = if raw_present {
                DataDepKind::Raw
            } else {
                DataDepKind::War
            };
            return mk(
                format!("data dependency on {names}"),
                String::new(),
                Vec::new(),
                SerializationCause::DataDependency { kind, vars },
            );
        }

        // Write-write on the same binding.
        let mut ww: Vec<&String> = a.defines.intersection(&b.defines).collect();
        if !ww.is_empty() {
            ww.sort();
            let names = ww
                .iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ");
            let vars = ww.iter().map(|s| (*s).clone()).collect();
            return mk(
                format!("both assign {names}"),
                String::new(),
                Vec::new(),
                SerializationCause::DataDependency {
                    kind: DataDepKind::WriteWrite,
                    vars,
                },
            );
        }

        // Polymorphic call: effects unknown at analysis time.
        if (a.calls_polymorphic && (b.calls_polymorphic || !b.effects.is_empty()))
            || (b.calls_polymorphic && !a.effects.is_empty())
        {
            return mk(
                "polymorphic-effect call — effects unknown at analysis time".to_string(),
                String::new(),
                Vec::new(),
                SerializationCause::PolymorphicEffect,
            );
        }

        // Effect conflict: find the conflicting effect pairs and attribute
        // them to the callees that contributed them.
        //
        // A2b-2 Phase 1/2: mirror `statements_conflict`'s network relaxations so
        // a reported cause is never a `Network`↔`Network` pair the grouper
        // actually treated as non-conflicting. For two ephemeral network
        // fan-outs, OR two method-call fan-outs on distinct receivers, the edge
        // that reached here must be a *non-Network* conflict, so skip
        // `Network`↔`Network` pairs and attribute the true cause.
        let distinct_method_fanout = match (
            &a.method_fanout_receiver_root,
            &b.method_fanout_receiver_root,
        ) {
            (Some(ra), Some(rb)) => ra != rb,
            _ => false,
        };
        let skip_network = (a.is_ephemeral_network_fanout && b.is_ephemeral_network_fanout)
            || distinct_method_fanout;
        let mut resource = String::new();
        let mut verbs: Option<(EffectVerbKind, EffectVerbKind)> = None;
        let mut callees: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for ae in &a.effects {
            for be in &b.effects {
                if skip_network && ae.resource == "Network" && be.resource == "Network" {
                    continue;
                }
                if self.two_effects_conflict(ae, be) {
                    if verbs.is_none() {
                        resource = ae.resource.clone();
                        verbs = Some((ae.verb.clone(), be.verb.clone()));
                    }
                    if ae.resource == be.resource && ae.resource == resource {
                        if let Some(c) = &ae.source_callee {
                            callees.insert(c.clone());
                        }
                        if let Some(c) = &be.source_callee {
                            callees.insert(c.clone());
                        }
                    }
                }
            }
        }
        if let Some((va, vb)) = verbs {
            let reason = format!(
                "{}({}) conflicts with {}({})",
                effect_verb_label(&va),
                resource,
                effect_verb_label(&vb),
                resource,
            );
            let cause = SerializationCause::EffectConflict {
                resource: resource.clone(),
                verbs: (va, vb),
            };
            return mk(reason, resource, callees.into_iter().collect(), cause);
        }

        None
    }

    /// Check if two statements conflict (have a dependency requiring serialization).
    pub(super) fn statements_conflict(&self, a: &StmtInfo, b: &StmtInfo) -> bool {
        // If either is in a seq block, force serialization
        if a.is_seq || b.is_seq {
            return true;
        }

        // Data dependency: B reads something A defines, or A reads something B defines
        if !a.defines.is_disjoint(&b.reads) || !b.defines.is_disjoint(&a.reads) {
            return true;
        }

        // Write-write conflict on same variable
        if !a.defines.is_disjoint(&b.defines) {
            return true;
        }

        // Polymorphic calls have unknown effects at analysis time — conflict
        // with any other stmt that has effect activity.
        if a.calls_polymorphic && (b.calls_polymorphic || !b.effects.is_empty()) {
            return true;
        }
        if b.calls_polymorphic && !a.effects.is_empty() {
            return true;
        }

        // A2b-2 Phase 1: two *ephemeral* network fan-outs (borrow-free free-fn
        // network calls — e.g. `http_get("a"); http_get("b")`) open disjoint,
        // freshly-created connections, so their `Network`-resource effects
        // (`sends`/`receives`) do not conflict. Skip only `Network`↔`Network`
        // pairs; any *other* shared resource a callee touches still serializes
        // through `two_effects_conflict` (a data dependency was already ruled
        // out above). See `is_ephemeral_network_fanout` and
        // docs/spikes/network-resource-granularity.md.
        if a.is_ephemeral_network_fanout && b.is_ephemeral_network_fanout {
            return self.effects_conflict_excluding_network(&a.effects, &b.effects);
        }

        // A2b-2 Phase 2 Slice 2: two method-call network fan-outs on DISTINCT,
        // provably-non-aliasing receivers (`s1.read(); s2.read()`) touch distinct
        // connections, so their `Network` effects do not conflict. Same-root
        // calls never reach this relaxation: a `mut ref self` method defines its
        // receiver, so the write-write check above already serialized them, and a
        // `ref self` same-root pair is excluded by the `ra != rb` guard here.
        // Shared-type / `ref`-param receivers (which could alias under a distinct
        // name) are excluded from candidacy in `classify_method_fanout`.
        if let (Some(ra), Some(rb)) = (
            &a.method_fanout_receiver_root,
            &b.method_fanout_receiver_root,
        ) {
            if ra != rb {
                return self.effects_conflict_excluding_network(&a.effects, &b.effects);
            }
        }

        // Effect conflict
        self.effects_conflict(&a.effects, &b.effects)
    }

    /// Like [`Self::effects_conflict`] but ignores every effect pair where
    /// BOTH sides touch the `Network` resource. Used only for two ephemeral
    /// network fan-outs (see [`Self::statements_conflict`]): their network I/O
    /// is on disjoint fresh connections, so `Network`↔`Network` is safe, while
    /// any non-`Network` resource conflict they carry is still honored.
    pub(super) fn effects_conflict_excluding_network(
        &self,
        a_effects: &[StmtEffect],
        b_effects: &[StmtEffect],
    ) -> bool {
        for a in a_effects {
            for b in b_effects {
                if a.resource == "Network" && b.resource == "Network" {
                    continue;
                }
                if self.two_effects_conflict(a, b) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if two sets of effects have a conflict.
    pub(super) fn effects_conflict(
        &self,
        a_effects: &[StmtEffect],
        b_effects: &[StmtEffect],
    ) -> bool {
        for a in a_effects {
            for b in b_effects {
                if self.two_effects_conflict(a, b) {
                    return true;
                }
            }
        }
        false
    }

    /// Check if two individual effects conflict.
    ///
    /// Conflict rules:
    /// - Same resource:
    ///   - reads + reads = NO conflict
    ///   - reads + writes = CONFLICT
    ///   - writes + writes = CONFLICT
    ///   - sends + sends = CONFLICT
    ///   - receives + receives = CONFLICT
    ///   - allocates + allocates = CONFLICT (same resource)
    ///   - panics + panics = CONFLICT
    ///   - blocks + blocks = NO conflict — execution verb drives placement, not
    ///     conflict (A1, 2026-06-10; design.md:5907/:5920)
    ///   - suspends + suspends = NO conflict — execution verb, same as blocks
    ///     (design.md:5907/:5920). Only matters for stmts the boundary gate lets
    ///     through, i.e. standalone `sleep_ms` timer waits (A2b, 2026-06-10):
    ///     two overlap via the par thread-block path. Channel `recv` / network
    ///     parks also carry `suspends` but are excluded upstream by
    ///     `effects_mark_coroutine_boundary` before this check is reached.
    ///   - Cross-category (e.g. reads + sends) = NO conflict even on same resource
    /// - Different resources = NO conflict regardless of verbs
    pub(super) fn two_effects_conflict(&self, a: &StmtEffect, b: &StmtEffect) -> bool {
        // Different resources never conflict
        if a.resource != b.resource {
            return false;
        }

        // A2b-2 Phase 2 Slice 3: parameterized-resource PARTITION KEYS. Two
        // accesses to the same resource with DISTINCT compile-time-literal keys
        // touch different partitions (`writes(Db[1])` vs `writes(Db[2])`) and
        // never conflict — the `design.md § Parameterized Resources`
        // proven-disjoint case. Any other combination (equal keys =
        // proven-identical; a `None` key = unparameterized or a non-literal
        // "unproven" arg) falls through to the verb-based check below, so it
        // conservatively conflicts — "silent under-serialization is never
        // accepted". `key` is only ever `Some` for a resource declared with a
        // `[param]`, so unparameterized effects are unaffected (additive).
        if let (Some(ka), Some(kb)) = (&a.key, &b.key) {
            if ka != kb {
                return false;
            }
        }

        // Same resource: check verb categories
        use EffectVerbKind::*;

        // Group 1: reads/writes — same category
        // Group 2: sends/receives — same category
        // Group 3: allocates — informational, NOT a conflict (A3a; design.md)
        // Group 4: panics — informational, NOT a conflict (A3b; design.md)
        // Group 5: blocks — execution verb, NOT a conflict (A1; design.md:5907)
        // Group 6: suspends — execution verb, NOT a conflict (A2b; the
        //          `(Suspends,Suspends) => false` arm below). General
        //          suspends/network still serialize, but via the upstream
        //          `effects_mark_coroutine_boundary` gate in
        //          `find_parallel_groups`, not this conflict arm — only
        //          `sleep_ms` timer waits (which clear the gate) reach here.
        // Cross-group: no conflict

        match (&a.verb, &b.verb) {
            // reads + reads = safe
            (Reads, Reads) => false,
            // reads + writes or writes + reads = CONFLICT
            (Reads, Writes) | (Writes, Reads) => true,
            // writes + writes = CONFLICT
            (Writes, Writes) => true,

            // sends + sends = CONFLICT
            (Sends, Sends) => true,
            // receives + receives = CONFLICT
            (Receives, Receives) => true,
            // sends + receives = safe (same resource, different direction)
            (Sends, Receives) | (Receives, Sends) => false,

            // allocates + allocates = NO conflict. `allocates` is an
            // *informational* resource verb (design.md: only reads/writes +
            // sends/receives drive conflict) — the heap allocator is
            // thread-safe, so two independent allocating statements may run
            // concurrently. The diagnostics-side `effectchecker.rs::two_effects_conflict`
            // already returns `false` here ("allocates, panics are
            // informational"); this aligns the auto-par conflict model with it.
            // Unlike `suspends`/network, `allocates` is NOT a coroutine
            // boundary (`effects_mark_coroutine_boundary` excludes it), so the
            // by-value double-drop hazard does not apply — the same reasoning
            // that made the A1 `blocks` flip safe. Lifted in A3 (2026-06-19);
            // see phase-5-diagnostics.md.
            (Allocates, Allocates) => false,
            // panics + panics = NO conflict. `panics` is *informational* too
            // (design.md: only reads/writes + sends/receives drive conflict),
            // and `effectchecker.rs::effects_conflict` already treats it as
            // non-conflicting. This unblocks auto-par for ordinary arithmetic:
            // `/` and `%` infer `panics` (the div/rem-by-zero guard), which is
            // why `examples/parallax_lite` had to avoid them to keep its groups.
            // Safe because a Kāra panic lowers to `emit_panic` = `printf` +
            // `exit(1)` (`src/codegen/runtime.rs`), a direct process exit — NOT
            // a Rust unwind. So a panic inside a `par_run` worker terminates the
            // whole process fail-fast (identical to a sequential panic: the
            // release runtime is built `panic = "abort"`, and worker-panic →
            // process-abort is the documented intended `par {}` semantics, see
            // the `[profile.release]` comment in Cargo.toml). No unwinding means
            // no double-drop and nothing to "propagate" — the same worker-exit
            // path already runs for explicit `par {}` and the A1/A3a groups.
            // Like `allocates`, `panics` is NOT a coroutine boundary. The
            // common case — a `/`/`%` that does not actually divide by zero —
            // simply computes concurrently. Lifted in A3b (2026-06-19);
            // see phase-5-diagnostics.md.
            (Panics, Panics) => false,
            // blocks + blocks = NO conflict. Execution verbs answer PLACEMENT,
            // not conflict (design.md:5907/:5920) — two independent blocking
            // calls overlap on the blocking pool via the same `emit_par_run`
            // fan-out that explicit `par {}` uses. Lifted in A1 (2026-06-10);
            // see phase-5-diagnostics.md and bench/auto_par_io/.
            (Blocks, Blocks) => false,
            // suspends + suspends = NO conflict. Like `blocks`, `suspends` is an
            // execution verb that answers PLACEMENT, not conflict
            // (design.md:5907/:5920). This arm is reached ONLY for stmts that
            // clear `effects_mark_coroutine_boundary` — i.e. standalone
            // `sleep_ms` timer waits (`stmt_is_timer_suspend`), the one
            // `suspends` form proven independent (a bare timer park, no by-value
            // `Drop` params). Two of them overlap via the `emit_par_run`
            // thread-block path exactly like `blocks` (A2b, 2026-06-10).
            // Channel `recv` and network parks also carry `suspends` but never
            // reach here — the boundary gate excludes them upstream (a channel
            // recv has a happens-before with its producer; lifting it deadlocks,
            // and a network coroutine owns + drops by-value params; the network
            // fan-out is A2b-2).
            (Suspends, Suspends) => false,

            // User-defined verbs: conflict if same verb on same resource
            (UserDefined(va), UserDefined(vb)) => va == vb,

            // Cross-category: no conflict
            _ => false,
        }
    }
}
