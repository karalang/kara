# Parallax

Full Parallax demo (Slice C, 2026-05-09). The canonical fan-out + join
workload that pairs Theme 6's `with_provider[R]` trait-method dispatch
with Slice A's per-branch return slots — the integration of two
recently-landed mechanisms in one source artifact, exercising the
"write sequential code, the compiler parallelizes it" promise on a
demo-shaped `get_dashboard(user_id) -> Dashboard` workload.

## What it demonstrates

The canonical Parallax fan-out + join shape (`docs/demo_ideas.md §
Demo 1`) — four typed effect resources, four provider impls, four
single-method traits, and a `get_dashboard(user_id)` workload whose
four `let` bindings auto-parallelize through `karac_par_run` and
join into a `Dashboard { profile, latest_order, top_notification,
top_recommendation }` aggregate.

```kara
pub fn get_dashboard(user_id: i64) -> Dashboard
    with reads(UserDB) reads(OrderDB) reads(NotifDB) reads(RecommendDB)
{
    let profile = fetch_profile(user_id);
    let latest_order = fetch_latest_order(user_id);
    let top_notification = fetch_top_notification(user_id);
    let top_recommendation = fetch_top_recommendation(user_id);
    Dashboard { profile, latest_order, top_notification, top_recommendation }
}
```

The auto-par analyzer (`src/concurrency.rs`) sees no conflict edges
between the four `let` statements (`reads(UserDB)` vs `reads(OrderDB)`
etc. — disjoint resources, no shared writes), so it groups them all
into a single `parallel_group`. The codegen path (`compile_function_body`
in `src/codegen.rs`) sees a non-trivial group with binding leak (each
`let` binding is consumed in the tail expression), and dispatches the
trio through Slice A's per-branch return-slot machinery: a
`__karac_ParGroup_<id>_Returns` struct sized to `(Profile, Order,
Notification, Recommendation)` is materialized in the parent's frame;
each branch fn writes its produced value to its slot; after the
`karac_par_run` barrier the parent reads four values back, and the
tail expression's `Dashboard { ... }` constructor reads the
in-scope bindings.

End result: `get_dashboard` runs four CPU-bound provider fetches
concurrently without any explicit `par {}` block in the source — the
compiler infers the parallel structure purely from effect annotations,
materializes the return-slot ABI, and threads the four-deep
`with_provider` chain through to each call's concrete impl.

## Differences from parallax-lite

Parallax-lite (`examples/parallax_lite/`) shipped the auto-par
*effect-only fan-out* shape — three `writes(R_i)` calls with no
joined return — as the v1 measurement workload. Slice 6's gate that
the canonical fan-out + join was blocked on (`group_defines_binding_used_outside`)
closed in Slice A (`ab611d3` 2026-05-08), and Slice C is the first
source artifact that exercises the full join shape.

Differences:
- **Four resources instead of three** (UserDB / OrderDB / NotifDB /
  RecommendDB) — exercises the four-deep `with_provider` chain in
  `main.kara`, one frame deeper than parallax-lite's three-deep nest.
- **Typed return values** (`Profile`, `Order`, `Notification`,
  `Recommendation`) instead of `()` — exercises Slice A's per-branch
  return slots through the slot ABI.
- **`reads(R)` not `writes(R)`** — read-only fetches with `ref self`
  receivers, the natural shape for a database-fetch workload.

## Demo-shape gap (Slice A return-shape)

The slice plan called for `Vec[Order] / Vec[Notification] /
Vec[Recommendation]` returns — repeated rows from each provider.
Verified at the `tests/par_codegen.rs::test_auto_par_vec_return_undefined_var_repro`
level that a `Vec[T]` typed return through Slice A's per-branch
return slots fails codegen with `Undefined variable '<binding>'` —
the slot ABI classifies `Vec` shapes correctly via
`llvm_type_for_type_expr` but the parent-side rebinding from
`slot_values` doesn't surface the slot binding for downstream stmts
in some shape path. Per the slice plan's hard-stop trigger 1 fallback
(b), the demo fixture uses single-row struct returns instead. The
demo's fan-out+join shape is preserved; the row-vs-rows shape change
is fixture-level only. Once the gap closes (filed as the Slice A
follow-up `tests/par_codegen.rs::test_auto_par_vec_return_undefined_var_repro`),
the fixture can grow back to `Vec[T]` returns additively.

## How to run

The example uses the multi-file project shape (`kara.toml` +
`src/*.kara`). v1 project-mode build (`karac build` from inside
`examples/parallax/`) typechecks across modules but doesn't yet
emit a binary — full multi-file codegen is CR-24. To exercise the
auto-par + with_provider path end-to-end today, concatenate the
sources into a single file and build it with `karac build`:

```sh
# concat the canonical workload + main into a single .kara file,
# stripping cross-module `import` lines
( cat examples/parallax/src/types.kara                                 \
       examples/parallax/src/traits.kara                               \
       examples/parallax/src/resources.kara                            \
       examples/parallax/src/providers.kara                            \
  ; grep -v '^import ' examples/parallax/src/workload.kara              \
  ; grep -v '^import ' examples/parallax/src/main.kara                  \
) > /tmp/parallax.kara

# auto-par on (default)
karac build /tmp/parallax.kara
./parallax

# sequential baseline (slice 6 codegen gate)
KARAC_AUTO_PAR=0 karac build /tmp/parallax.kara
./parallax
```

`KARAC_AUTO_PAR=0` flips the slice 6 codegen gate in `src/codegen.rs`
(`Codegen::auto_par_disabled`), short-circuiting all parallel-group
dispatch back to plain sequential `compile_block` without changing
the source. Default is auto-par on. The user-facing `--sequential`
CLI flag is a Phase 8.5 Track 2 deliverable (slice C stays inside the
codegen entry-point arg budget).

The `karac build --concurrency-report /tmp/parallax.kara` pipeline
emits the demo-storyboard text shape pinning what auto-parallelizes
and why; the four-call group on `get_dashboard` is the load-bearing
output.

## Files

- `kara.toml` — project manifest (`name = "parallax"`, `edition =
  "2026"`, no dependencies).
- `src/types.kara` — five owned data types: `Profile`, `Order`,
  `Notification`, `Recommendation`, `Dashboard`.
- `src/traits.kara` — four single-method provider traits
  (`UserDatabase`, `OrderDatabase`, `NotificationDatabase`,
  `RecommendationDatabase`), each with one `fetch_*` method
  taking `ref self`.
- `src/resources.kara` — four typed effect resources
  (`UserDB / OrderDB / NotifDB / RecommendDB`) bound to their
  respective traits.
- `src/providers.kara` — four `InMemory*DB` provider structs +
  impls. Each `fetch_*` runs a CPU-bound additive busy-compute
  kernel sized proportionally to "simulated I/O latency" (10M /
  30M / 15M / 20M iters per fetch), then constructs and returns
  the typed result. Each provider carries a unique tag-field shape
  to distinguish it in the codegen's LLVM-struct-identity reverse
  lookup.
- `src/workload.kara` — four `fetch_*` driver wrappers (each
  declared `with reads(R)` so the analyzer's effect-collection
  picks them up by name) + the canonical `get_dashboard(user_id)`
  workload whose four `let` bindings auto-parallelize.
- `src/main.kara` — entry point. Four-deep nested `with_provider`
  chain wraps a single `get_dashboard(42)` call.

## See also

- `docs/implementation_checklist/phase-8-stdlib-floor.md § Provider
  Implementations` — the slice C plan and close-out.
- `docs/demo_ideas.md § Demo 1` — the canonical Parallax demo
  storyboard this workload renders against.
- `examples/parallax_lite/` — sister project, three-resource
  effect-only fan-out (without join). Same multi-file shape, lighter
  surface, ships the v1 measurement workload.
- `tests/parallax.rs` — IR-shape, concurrency, dispatch e2e,
  return-slot e2e, concurrency-report cross-check, and
  (`#[ignore]`-gated) wall-clock benchmark coverage.
- `tests/par_codegen.rs::test_auto_par_vec_return_undefined_var_repro`
  — the Slice A follow-up regression test for the Vec-return shape
  gap that this slice deviated from.
