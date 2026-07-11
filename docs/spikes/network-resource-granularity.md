# Design spike — finer `Network` effect-resource granularity (A2b-2 Blocker A)

**Status:** design spike, 2026-07-09. **Phase 1 SHIPPED 2026-07-10** (the
scoped ephemeral-call conflict relaxation — see §8). **Phase 2 Slice 1
(associated-fn admission) + Slice 2 (method-call admission via receiver
distinctness) SHIPPED 2026-07-11:** the receiver-less openers
(`TcpStream.connect`) fan out via `resolve_assoc_callee`, and method calls
(`stream.read()`) on provably-distinct non-shared non-ref-param receivers fan
out via `classify_method_fanout` (threading the typecheck result through
`concurrency_analyze_typed`) — both reuse Phase 1's conflict-relaxation
machinery WITHOUT yet building the full connection-identity resource model.
**Phase 0 (conflict-table reconciliation, §4) and Slice 3 — full
parameterized-`Network` (borrow-param / connection-bound ops generally) —
remain open.** The original
proposal scoped the work into phases so it could be picked up incrementally;
Phase 1 + Phase 2 Slice 1 sidestep Phase 0 and the parameterized-resource model
by relaxing/extending only the narrow, provably-sound receiver-less cases rather
than changing the global network-verb semantics or the resource model.
**Context:** [`phase-5-diagnostics.md` § Auto-par conflict model / A2b-2](../implementation_checklist/phase-5-diagnostics.md).
A2b-2's arg-safe + borrow-param fan-out slices shipped (they lift the
coroutine-boundary *gate*), but the **headline** — "two independent network
fetches to different endpoints auto-parallelize" — is blocked one layer down,
in the *conflict model*. This spike is that blocker.

---

## 1. Problem

Auto-parallelization treats the `Network` resource as a single monolithic
nominal string. Real network I/O carries `sends(Network)` / `receives(Network)`
(HTTP GET sends a request and receives a response; `TcpListener.accept` is
`sends(Network) receives(Network)`, `runtime/stdlib/tcp.kara:162`). Two such
calls therefore collapse to the same `"Network"` resource and **falsely
conflict** in the auto-par edge builder — `src/concurrency.rs::two_effects_conflict`
short-circuits on `a.resource == b.resource` (`:2038-2044`) and then applies
`(Sends,Sends) => true` (`:2069`) / `(Receives,Receives) => true` (`:2071`). So
they serialize, even though at the client level they are independent sockets
the runtime already overlaps.

Concretely: `http_get("http://a"); http_get("http://b")` cannot fan out. The
A2b-2 fan-out tests only demonstrate the gate-lift using `reads(Network)`
(`(Reads,Reads) => false`, `:2062`) — a verb no real network primitive emits —
precisely because `sends`/`receives` still conflict. The gate-lift is necessary
but not sufficient; **this is the sufficient piece.**

## 2. Root cause

The resource model is **nominal-per-type**, not per-instance:

- A tracked resource is a flat `String` on both sides — `StmtEffect.resource`
  (`src/concurrency.rs:808-819`) and `Effect.resource` (`src/effectchecker.rs:54-59`).
  Conflict = string `==`.
- `Network` is registered as an ambient scope-0 resource (`src/prelude.rs:420-441`)
  and hardcoded onto stdlib method keys with the literal `resource: "Network"`
  (`src/effectchecker.rs:802-850`). Every connection, both directions, one string.
- **The per-instance slot exists in the AST but is discarded.** `Resource { path,
  param: Option<Box<Expr>>, span }` (`src/ast/items.rs:901-906`) — `param` is the
  parameterized-resource key — and the parser fills it (`src/parser/items_effects.rs:319-325`),
  but every `Resource → Effect.resource` conversion uses only `path.join(".")` and
  **drops `param`** (`src/effectchecker.rs:1104, :1255, :1482, :2014`).
- The concurrency pass copies the callee's static resource string verbatim
  (`src/concurrency.rs:3226-3235`); the receiver binding is *syntactically in
  hand* at the `MethodCall` effect-collection site (`:2985`) but never used to
  key the resource. So `stream_a.read()` and `stream_b.read()` both yield
  `resource = "Network"`.

So two operations on **different connections** are indistinguishable from two on
the **same** connection.

## 3. The design is already sanctioned — it's just unbuilt

Crucially, this is **not** a request to invent new language semantics.
`design.md § Parameterized Resources` (`:7067-7135`) already specifies exactly
the mechanism needed — "opt-in finer granularity":

```kara
effect resource UserDB[user_id: i64];
update_profile(42); update_settings(42);  // same key   → conflict, serialized
update_profile(42); update_settings(99);  // diff keys  → safe,     parallelized
```

with a defined **alias tri-state** (`:7083-7089`): proven-disjoint /
proven-identical / unproven-conservative, static distinctness rules (`:7099-7103`),
and a runtime `partition_by_key` guard (`:7105-7115`).

**But the conflict path does not honor it.** `roadmap.md:121,210,298` marks
parameterized distinctness `[x]` with a "conservative collapse", and in practice
that collapse is total: `param` is dropped (§2), there is **no** `partition_by_key`
/ distinctness / alias-tri-state code in `src/`, and **no test** distinguishes
`db1.query()` from `db2.query()` (`tests/concurrency.rs::test_different_resources_parallelizable`
parallelizes on *nominal name* difference `Db != Cache`, not instance). So the
building blocks exist on paper and in the AST, but the analysis is nominal-only.

**Two sub-problems, then:** (a) implement the specced parameterized-resource
distinctness at all; (b) make `Network` a parameterized resource keyed by
connection identity.

## 4. A telling clue — the two conflict tables already disagree

There are **two** conflict tables and they diverge on network verbs:

- Auto-par edge builder `src/concurrency.rs::two_effects_conflict` —
  `(Sends,Sends) => true`, `(Receives,Receives) => true` (`:2069/:2071`).
- Diagnostics `src/effectchecker.rs::effects_conflict` —
  `(Sends,Sends) => false`, `(Receives,Receives) => false` (`:2131/:2135`).

So the language's own *diagnostic* conflict semantics already consider two
independent network sends/receives **non-conflicting**; only the auto-par side
conservatively serializes them. The auto-par table is the outlier. This both
(a) suggests the intended end-state (network verbs are not blanket-conflicting)
and (b) is a latent inconsistency worth reconciling regardless of this spike.

**Phase 0 caveat — the diagnostics table is currently *unwired* (2026-07-10
recon).** `effectchecker.rs::effects_conflict` is called only by
`EffectChecker::find_conflicts`, which in turn has **no production caller** —
it is exercised solely by `tests/effectchecker.rs`. So the divergence has **no
live user-facing effect today**: no diagnostic actually consults the `=> false`
network rule. Two consequences for Phase 0: (i) reconciling the tables is
near-zero *risk* (nothing in the pipeline shifts) but also near-zero *value*
until `find_conflicts` is wired into a real diagnostic (e.g. validating an
explicit `par { … }` block); (ii) **do not "make them agree" by copying the
diagnostics `(Sends,Sends) => false` into auto-par.** Phase 1 established the
real answer is connection-identity-dependent — two *ephemeral* (borrow-free,
distinct-connection) sends don't conflict, but two sends on a *shared* borrowed
connection do. A flat `=> false` would greenlight the shared-connection race
(and if `find_conflicts` were later wired to gate `par {}` blocks, it would
wrongly accept `par { send(conn); send(conn); }`). The correct reconciliation
unifies **toward the Phase 2 connection-identity model**, not toward either
flat table. Phase 1's `effects_conflict_excluding_network` is the auto-par-side
down payment on that: it already encodes "ephemeral ⇒ non-conflicting, shared ⇒
conflicting" for the cases it can prove.

## 5. Proposed model — parameterize `Network` by connection identity

The resource for a network op should be the **connection**, not `"Network"`.
Two ops on different connections are independent; two on the same connection
conflict (they interleave on one socket — a real data race on stateful socket
buffers). Two op-classes, both statically determinable:

1. **Connection-bound op** — the network fn operates on an existing connection
   it receives as `self` or a parameter (`stream.read()`, `send_on(conn)`). The
   partition key is the **receiver/argument binding's identity** — the same
   per-binding identity the pass already computes for data dependencies.
   Different bindings → proven-disjoint → parallelize; same binding →
   proven-identical → conflict.

2. **Ephemeral op** — the fn creates + uses + drops a fresh connection
   internally, taking no connection in its signature (just value args like a
   URL: `http_get(url)`). Each call opens its **own** socket, so each call is a
   **fresh** connection identity → any two distinct ephemeral network calls are
   proven-disjoint → parallelize. This is the flagship shape.

This maps 1:1 onto the spec's alias tri-state: distinct bindings / distinct
ephemeral calls = proven-disjoint; same binding = proven-identical; a connection
aliased through opaque code = unproven → conservative (serialize). It is a
faithful application of § Parameterized Resources with the *key* being the
connection rather than a user-written `[user_id]`.

**Surface options for the key** (a design decision to settle):

- **(i) Implicit, compiler-derived.** The compiler keys `Network` effects by the
  receiver/first-connection-arg binding automatically; ephemeral calls (no
  connection in signature) mint a fresh key per call. No stdlib annotation
  change; most ergonomic; most "magic".
- **(ii) Explicit, spec-shaped.** Stdlib annotates `TcpStream.read(mut ref self)
  with receives(Network[self])`, `http_get(url) with receives(Network[fresh])`
  (or an implicit-fresh default when no key is written). Reuses the existing
  `Resource.param` slot verbatim; keeps the mechanism uniform with `UserDB[id]`;
  more explicit, requires a `[self]` / fresh-key vocabulary.

Recommendation leans **(ii)** for principled uniformity (it *is* the parameterized-
resource feature), with the implicit-fresh default so common code needs no
annotation.

## 6. Soundness

- **Same connection must still conflict.** Two `conn.read()` consume different
  bytes from one socket — order-dependent; two `conn.write()` interleave on one
  socket. The model preserves this: same binding → proven-identical → conflict.
  Note the pass *already* serializes same-binding network method calls via a
  second mechanism — `method_effects_imply_receiver_mutation` marks a receiver
  written for any non-pure verb (`sends`/`receives` qualify), so `conn.read();
  conn.read()` data-depend on `conn` (`src/concurrency.rs:2807-2809`). That is a
  useful backstop but is **not** sufficient on its own (see §7 Path A caveat).
- **Independent connections are sound to overlap.** Distinct sockets, distinct
  kernel buffers; the runtime already multiplexes them (the whole async-network
  substrate, `docs/spikes/network-async-coroutine-transform.md`). Client-side
  independence holds even for two fetches to the *same* URL (two client sockets);
  any server-side serialization is the server's concern, not modeled by the
  client's `Network` resource.
- **Scope boundary.** This models *transport-level* independence only. It does
  **not** model shared application state reachable through the network (a remote
  DB two fetches both mutate) — that is out of scope for a transport resource,
  exactly as the current `Network` resource already is.

## 7. Two implementation paths

### Path A — pragmatic, auto-par-local (fast flagship unblock)

Relax the auto-par table's network arms toward the diagnostics table, but only
where provably sound. The *cleanest sound* minimal rule is **not** a blanket
`(Sends,Sends) => false` — it is: **two network effects do not conflict when
they are on provably-different connections.** For the flagship (ephemeral,
no-connection-signature free-fn calls — exactly the shape `is_safe_network_fanout`
already identifies, `src/concurrency.rs:924-970`) two distinct calls are always
different connections, so their `sends`/`receives` can be treated as
non-conflicting in `two_effects_conflict`.

- **Pro:** small, analysis-local, reuses machinery already added for A2b-2;
  unblocks `http_get(a); http_get(b)`; does not touch the effect model or
  `design.md`.
- **Caveat (why a blanket flip is wrong):** a blanket `(Sends,Sends) => false`
  relying on data-dependency for same-connection safety has a hole — a
  free-function network op taking a connection by **`ref`** (not `mut ref`)
  does not get its arg marked written (the call-arg write detection only fires
  for `mut ref` / `mut Slice` / an explicit `mut` marker, `src/concurrency.rs:2839-2846`),
  so two `peek(conn)` with `receives(Network)` on a `ref Conn` param would
  wrongly parallelize despite mutating shared socket state. The scoped rule
  (ephemeral-only, no connection in signature) sidesteps this: with no
  connection param there is no shared connection to race.
- **Net:** ship Path A **scoped to ephemeral network calls** as the sound fast
  unblock; do **not** do the blanket flip.

### Path B — principled, full parameterized resources (the real answer)

Implement § Parameterized Resources for real and parameterize `Network`:

1. **Stop dropping `Resource.param`** — thread the key from AST through to a
   structured resource on `Effect`/`StmtEffect` (e.g. `resource: String` +
   `key: ResourceKey`), or synthesize a per-receiver / fresh-per-ephemeral key
   at the effect-collection site (`src/concurrency.rs:2985, :3226-3235`).
2. **Implement the alias tri-state** in `two_effects_conflict`: proven-disjoint
   (different bindings / distinct fresh ephemerals) → no conflict; proven-
   identical (same binding) → conflict; unproven → conflict (conservative).
   This is the currently-absent distinctness graph (`design.md:7083-7103`).
3. **Parameterize `Network`** — surface option (i) or (ii) from §5; annotate the
   stdlib network primitives (`src/effectchecker.rs:802-850`) accordingly.
4. **Reconcile the two conflict tables** (§4) so auto-par and diagnostics agree
   on network verbs.

- **Pro:** sound and general; covers connection-bound ops (`stream_a` vs
  `stream_b`), not just ephemerals; delivers the specced feature that also
  benefits `Db[id]` etc.; removes the auto-par/diagnostics divergence.
- **Con:** a real effect-model change touching AST → effectchecker → concurrency,
  plus `design.md` reconciliation; the largest single piece of remaining auto-par
  work.

## 8. Recommendation & phasing

1. **Phase 0 (independent cleanup, small — but lower-priority than first
   scoped):** reconcile the conflict-table divergence (§4) — decide the intended
   network-verb semantics and make the two tables agree. **2026-07-10 recon
   reprioritizes this:** the diagnostics table is *unwired* (only tests call
   `find_conflicts`), so reconciling it now is near-zero risk **and** near-zero
   value — there is no live diagnostic to fix. It becomes worthwhile only when
   paired with *wiring* `find_conflicts` into a real diagnostic (e.g. gating
   `par {}` blocks), and even then it must unify toward the Phase 2
   connection-identity answer, not a flat table (see §4 caveat). Treat Phase 0
   as a rider on Phase 2, not a standalone quick win.
2. **Phase 1 = Path A, scoped to ephemeral calls — ✅ SHIPPED 2026-07-10.** The
   sound fast unblock for the `http_get(a); http_get(b)` flagship. Ships the
   headline demo; contained; no effect-model change. Gated exactly on the
   ephemeral shape (`is_ephemeral_network_fanout` = a safe network fan-out whose
   callee declares no borrow param, so it cannot receive a shared connection and
   must open its own). `statements_conflict` skips `Network`↔`Network` conflicts
   for two such statements (`effects_conflict_excluding_network`); a borrow-param
   call stays serial, and any non-`Network` shared resource still conflicts. See
   the `[~]` A2b-2 sub-entry in `phase-5-diagnostics.md` for the full test list.
3. **Phase 2 = Path B**, taken incrementally:
   - **Slice 1 — associated-fn admission — ✅ SHIPPED 2026-07-11.** The
     receiver-less openers (`TcpStream.connect` / `TlsStream.connect`, 2-segment
     `Type.method(...)` with `self_param.is_none()`) fan out via
     `resolve_assoc_callee`, reusing Phase 1's gates. No new type info needed —
     an associated fn has no receiver, so it is structurally a free function.
   - **Slice 2 — METHOD-call admission — ✅ SHIPPED 2026-07-11.** Method calls
     with a `self` receiver (`stream.read()`, `client.get()`) fan out when the
     two receivers are provably distinct. `concurrency_analyze_typed` threads
     the typecheck result in; `classify_method_fanout` records the receiver root
     when the receiver is a plain non-`ref`-param local of a non-`shared` type
     and the method borrows `self` (not consuming); `statements_conflict` relaxes
     `Network`↔`Network` for two candidates with distinct roots. Same-root is
     serialized by the write-write dep (a `mut ref self` method defines its
     receiver), shared/ref-param receivers are excluded, and codegen already
     falls back to sequential when a mutated receiver is observed after the group
     (`stmts.rs:914`, so no silent-wrong-value). Verified end-to-end + LSan/ASan-
     clean; see the `[~]` A2b-2 sub-entry in `phase-5-diagnostics.md` for the
     full test list. The scoping below is retained as the design record.

     A method's receiver *is* the
     shared connection, so `a.read(); a.read()` on one stream must serialize
     while `a.read(); b.read()` on distinct streams may fan out — soundness
     hinges on proving the two receivers don't alias. Empirical probes settled
     the landscape: **(i)** Kāra has **no body-level `ref`-binding** (`let b = ref a`
     is a parse error — borrows are signature-level), so two distinct `let`
     locals in a body **cannot alias via borrows** — good. **(ii)** BUT a
     `shared` (RC) receiver aliases via plain assignment: `shared struct Conn` +
     `let b = a` makes `a`/`b` the *same* object (verified: `a.bump()`→1,
     `b.bump()`→2, `a.n`→2) and **checks clean** — so distinct roots do NOT imply
     distinct objects for shared types. **(iii)** A `ref`/`mut ref` *param*
     receiver may be aliased by the caller (cross-function). The AST-only
     concurrency pass (`concurrency_analyze(program, effects)` — no type info)
     cannot detect (ii)/(iii). **Sound Slice 2 therefore requires: a
     `ref`/`mut ref self` method (borrowed receiver — no consuming-`self`
     double-drop) + distinct receiver roots + a receiver that is provably
     NON-shared and NOT a `ref`/`mut ref` param** — the last two need receiver-
     type info threaded into the pass (a new `&TypeCheckResult`-shaped input
     across ~10 call sites; the receiver's type comes from `expr_types`, its
     shared-ness from the AST decl's `is_shared` flag). **Codegen is NOT a
     blocker (verified 2026-07-11).** Network methods are `mut ref self` (a read
     advances the stream), but auto-par already handles a mutated captured
     receiver correctly: `src/codegen/stmts.rs:914` falls back to sequential when
     a captured-mutation name (`method_effects_imply_receiver_mutation` feeds
     `StmtInfo.defines`) is read outside the group ("parallelization is an
     optimization hint, not a semantic requirement"), and fans out only when the
     receiver is unobserved after (the common `let d1 = s1.read(); let d2 =
     s2.read(); use(d1); use(d2)` shape) — so a mutated receiver never yields a
     silent-wrong-value. (An earlier probe of *explicit* `par { x = 1 }` showed
     the mutation dropped — that is `par {}`'s deliberate branch-isolation
     semantic, NOT the guarded auto-par path.) The residual risk is therefore
     purely the concurrent-access RACE on an *aliased* receiver, which the
     shared/ref-param exclusions rule out; the admitted case (two distinct
     non-shared, non-param local roots) is provably distinct given finding (i)
     (a non-shared `let b = a` MOVES `a`, and there is no `ref`-binding). Net:
     buildable, but a race-sensitive multi-step slice (plumbing + admission +
     conflict relaxation) that must land with full ASAN + shared-receiver /
     ref-param / same-root serial pins. Schedule as a focused effort; do not rush.
   - **Slice 3 — full parameterized-`Network`:** the principled end-state that
     subsumes Slices 1–2, covers borrow-param connection-bound ops generally, and
     lands the long-specced parameterized-resource feature. The "correct" model;
     take on deliberately as its own project.

Phase 1 + Phase 2 Slice 1 already deliver the flagship value (free-fn `http_get`
fan-out + associated `connect` openers); Slices 2–3 extend reach to method /
connection-bound ops.

## 9. Effort / risk

- **Phase 0:** ~small. Risk: changing the diagnostics conflict semantics could
  shift user-facing effect-conflict diagnostics — needs a test pass.
- **Phase 1:** ✅ shipped. Was ~small–medium, analysis-only, memory-safety-neutral
  (no codegen) — as predicted, it reused the A2b-2 arg-safety scaffolding
  (`is_safe_network_fanout`) and added only the borrow-free-callee predicate; the
  fan-out rides the existing return-slot `par_run` path, so the codegen surface
  was untouched and the ASAN suite stayed green.
- **Phase 2:** medium–large. Touches AST→effectchecker→concurrency + `design.md`.
  Risk: soundness of the distinctness graph (must be conservative on aliasing);
  the key-surface decision (§5 (i) vs (ii)); interaction with provider-rooted
  resources (`design.md:7137-7226`, which stays nominal per-base-name).

## 10. Open questions

1. **Key surface (§5):** implicit compiler-derived vs explicit `Network[self]` /
   fresh. The explicit form is spec-uniform but needs a `[self]`/fresh vocabulary
   the parameterized-resource spec doesn't yet name.
2. **Ephemeral detection:** "no connection in the signature" is the proposed
   proxy for "opens a fresh connection". Is there a shape where a fn with no
   connection param nonetheless touches a *shared* connection (e.g. an ambient/
   global socket via a provider)? If so the proxy needs tightening (exclude
   provider-rooted network resources).
3. **Direction split:** `Network` unifies `sends` + `receives` today
   (`runtime/stdlib/http.kara:183-189`). Per-connection keying makes
   `conn.write()` (send) and `conn.read()` (receive) on the *same* conn share a
   key → conflict, which is correct (full-duplex on one fd still shares socket
   state at this granularity). Confirm that is the desired semantics.
4. **Same-URL fetches:** the model parallelizes two `http_get(same_url)` calls
   (two client sockets). Confirm that is intended (it is client-correct; a user
   wanting them serialized would sequence them explicitly).
