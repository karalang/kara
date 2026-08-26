# `docs/wip/` — parked patches

Work that is written and verified but deliberately NOT landed, because landing
it alone would leave the tree worse than the status quo. Each patch names the
ledger row that tracks it; read that row before picking one up.

Regenerate context with `grep '<B-ID>' docs/bug-ledger.jsonl`.

| Patch | Row | Why it is parked |
|---|---|---|
| `b-2026-08-26-36-ref-bindings.patch` | B-2026-08-26-36 | Parser + typechecker for `ref` in expression position (`let r = ref v[i]`, a new `UnaryOp::Ref`). Parses and typechecks; **neither backend can execute it.** Landing this half alone is worse than the status quo: today the form fails cleanly at parse, half-implemented it would compile and then die in codegen. The real cost is the interpreter, which has no reference representation at all — see the row. |
| `b-2026-08-26-21-index-move-rejection.patch` | B-2026-08-26-21 | `E_INDEX_MOVE_NON_COPY` — rejects `let t = v[i]` / `v[i] = v[j]` for a non-`Copy` element, per design.md § "The index operator (`expr[i]`)". Complete and verified (fires exactly twice on the row's fixture, spares `Copy` elements, `E0282` wired into the JSON path `karac fix` reads). Blocked on `-36`: `ref` is the only fix-it for the 67 `Vec[Tensor]` sites in `std.autograd`, which have no `.clone()`. |
| `phase12-import-slice.patch` | — | Pre-existing; see `docs/implementation_checklist/phase-12-self-hosting.md`. |

**Apply order is `-36` first, then `-21`** — the rejection's diagnostic points at
the borrow spelling, so landing it before `ref` works hands 67 stdlib sites an
error they cannot satisfy.

Both were verified to apply cleanly to `5686fcb`. If they have since gone stale,
prefer rebuilding from the ledger rows (each records the design decisions and the
measurements) over force-fitting the diff.
