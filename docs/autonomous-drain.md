# Autonomous ledger draining

Notes on running an unattended session that works open rows of
`docs/bug-ledger.jsonl`, and on why the default outcome — measured across four
parallel drain sessions on 2026-09-16 — is that a session closes one row, asks
whether to take another, and idles indefinitely.

Nothing here grants a session authority it did not already have. Whether a
given session runs unattended, and how wide its remit is, comes from the
operator who starts it (the kickoff prompt, or the Routine that wakes it) — not
from this file. What this file provides is the failure analysis and a template
the operator can fill in.

## Why a session stops, and why "keep going" in the kickoff prompt does not fix it

Three separate mechanisms, only one of which is about wording:

1. **Turn end is absolute.** A session that finishes its reply is not paused
   mid-thought; it is finished, and nothing re-invokes it. An instruction to
   "keep working" cannot execute after the turn it lives in has ended, because
   no execution context survives. Continuous operation is therefore not a
   prompting problem — it needs an **external wake**.
2. **The unit boundary reads as a decision point.** Closing a row is a natural
   completion, and picking the next one looks like a coordination act, since the
   claim protocol (CLAUDE.md § Claiming a bug) routes picks through shared
   state. So a session ends with *"take B-… next, or release and pick up
   something else?"*. Measured 2026-09-16: group D sat idle in exactly this
   state, with 63 other rows open.
3. **Instruction decay.** These sessions run to 600–760 k tokens. A kickoff
   prompt is thousands of turns and at least one compaction behind by the time
   the first row closes. Anything that must hold for the whole run cannot live
   only there — it has to be re-sent by each wake.

One mechanism per cause: a **Routine** for (1), an explicit **scope statement**
for (2), and putting that statement **in the wake prompt itself** for (3).

## Scope: what a drain task covers, and where it ends

The useful thing an operator can state up front is which boundaries are inside
the task and which are genuine escalations. For a drain task the natural split:

**Inside the unit of work** — the whole cycle, not one step of it: syncing to
`origin/main`; reading the session board and claiming a row per CLAUDE.md
§ Claiming a bug; reproducing; the fix and its regression fixture; the gates in
CLAUDE.md § Commands; `scripts/bug-close.py`; the push; releasing the tag; and
**then the next row**. Treating one row as the deliverable is what produces the
stall — the queue is the deliverable.

**Genuine escalations**, worth naming explicitly so they are not confused with
the above:

- a **language-design decision** — a change to `docs/design.md` semantics rather
  than its prose;
- a fix that would **break codegen containment** (CLAUDE.md § Architecture) or
  add a compiler phase;
- anything **irreversible or outside the repo** — deleting a remote ref, acting
  on another session's state, a blast radius past `main`;
- **rate-limit or disk exhaustion** (CLAUDE.md § "A red gate with no named test
  is probably a FULL DISK"); a session that retries through a limit starves the
  other three;
- **every open row already claimed** by a live session.

Distinct from those, and the ones that have actually ended drain sessions: a row
harder than it looked (split it, file the remainder as a fresh row), a gate gone
red from the session's own change (that is the work), a long context (compaction
is the harness's job), and reaching the end of the row set named at kickoff
(the ledger is the queue, not that set).

## The cycle

From a standing start each time, assuming nothing survives the wake:

1. `git fetch origin main && git reset --hard origin/main` — the ledger moves
   under you (CLAUDE.md § Claiming a bug, step 1).
2. Finish anything in flight before picking new work. A half-fixed claimed row
   *is* the iteration.
3. `list_sessions({mine: true, limit: 30})`; collect `kara-bug:*` tags. A tag on
   a RUNNING session, or an IDLE one touched within the day, is a live claim.
4. Pick the oldest unclaimed open row — `grep '"status": "open"'
   docs/bug-ledger.jsonl`. Oldest-first keeps parallel groups from colliding on
   whatever is newest.
5. Claim it, mirror the id into the title, re-list once, honor the `created_at`
   tiebreak.
6. Fix it: reproduce, minimal fix, regression fixture, the gates in CLAUDE.md
   § Commands — both clippy legs, both feature legs, and the two ASAN legs when
   a memory-class fixture is involved.
7. Re-read the row (`grep '<BUG-ID>' docs/bug-ledger.jsonl`); it may have been
   edited while held. Rebase, *then* read the fix SHA out of `git log`, then
   `scripts/bug-close.py <ID> --expect <substring> --sha <sha>`. Lint, push,
   release the tag.
8. Next row. A per-row report back to the operator is the stall in disguise: it
   reads as a handoff and invites a reply that may not come for hours. Report
   when an escalation above fires, or when the queue empties.

## Wiring the wake

A **Routine** (`create_trigger`, `claude-code-remote` MCP) is the external wake.
Minimum interval is hourly, which suits the work — the gate set alone is 25–40
minutes.

**Persistent-session heartbeat** (`persistent_session_id`) fires into an
existing session and keeps its accumulated context. Good for a session already
deep in one bug family; short shelf life, since these sessions saturate their
context window and a heartbeat cannot fix that.

**Fresh session per firing** (`create_new_session_on_fire: true`) starts clean
and re-reads CLAUDE.md and this file. That is the durable shape, *because* the
state that matters already lives outside the session: the ledger is the queue,
session tags are the lock, and a dead claim expires by the staleness rule in
CLAUDE.md § Claiming a bug. Its prompt inherits nothing, so it must be
standalone — and it should exit early and say so when
`grep '"status": "open"' docs/bug-ledger.jsonl` comes back empty, or it will
keep spawning against a drained queue.

Either way the prompt carries the scope statement inline, since by the time it
fires the kickoff prompt has decayed. A template for the operator to adapt:

    Continue the ledger drain described in docs/autonomous-drain.md, under the
    scope I am setting here: the unit of work is the queue, not one row — sync
    to origin/main, claim the oldest unclaimed open row per CLAUDE.md, fix it,
    close it with scripts/bug-close.py, push to main, then take the next row.
    Commit directly to main; do not create git branches. Come back to me for the
    escalations listed in docs/autonomous-drain.md § Scope, or when the queue is
    empty — not at each row boundary.

## Guardrails

Unattended operation compounds cost: a drain session runs several hundred
dollars and consumes the five-hour rate window every other session shares.

- Four concurrent drain sessions is the measured working point; beyond that they
  collide on the claim protocol rather than going faster.
- A fresh-session Routine with no supervision keeps spawning after the queue
  empties. Either give it a schedule that ends, or check the board on a cadence
  you keep.
- Review what landed. Autonomy moves the human from approving each step to
  auditing the diff; skipping both is how a bad fix reaches `main` sixty times.
