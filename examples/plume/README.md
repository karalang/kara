# Plume — a pointer-steered flow field across every core, in a browser tab

Plume is the interactive half of Kāra's "auto-concurrency without coloring"
thesis: a real-time particle-flow field where **one Kāra source** drives a
render loop whose per-frame compute fans out across a Web Worker pool — and the
**mouse steers the flow**, arriving as a typed channel the loop drains with a
plain `try_recv`. No `async`, no `await`, no callback coloring, no hand-written
threading code, and no `addEventListener` in the Kāra source.

It is [Fathom](../fathom/)'s interactive sibling — the same blocking-loop +
Worker-pool + framebuffer-blit spine, plus **live input** as the new ingredient.
(Slipstream's wasm edition is the full fluid-sim flagship on this spine — see
[`docs/dogfooding.md`](../../docs/dogfooding.md).)

## What it shows

- The render loop is a plain blocking loop — frame clock **and** input, both as
  channels, no `await` anywhere:

  ```kara
  let frames = animation_frames();      // std.web.time
  let moves  = pointer_moves();         // std.web.events — Channel[PointerEvent]
  loop {
      frames.recv();                    // parks until the next frame
      match moves.try_recv() {          // latest pointer, non-blocking
          Some(p) => { pu = p.x() / W; pv = p.y() / H; }
          None    => {}
      }
      render_frame(pu, pv, t, workers);
  }
  ```

  The pointer position crosses host→wasm as a **real struct payload**
  (`PointerEvent { x: f64, y: f64 }`), not a bare tick — the
  `Channel[T]`-for-`T != ()` event-data path. *Where is the event listener? It's
  a channel; the host fills it.*

- Each frame's rows are split into bands and computed in parallel:

  ```kara
  let task: TaskHandle[Vec[u8]] = pool.spawn(|| render_rows(y0, y1, pu, pv, t));
  ```

  Halve the available cores and the framerate visibly halves — the overlay shows
  live FPS, worker count, and frame counter.

- The finished framebuffer is blitted to a `<canvas>` through one host fn,
  handing JS the bytes directly out of shared wasm memory:

  ```kara
  host fn put_pixels(ptr: *const u8, len: i64, w: i64, h: i64) with writes(Display);
  ...
  put_pixels(frame.as_ptr(), frame.len(), w, h);   // Vec[u8].as_ptr() FFI handoff
  ```

The flow is a line-integral convolution: each pixel walks *upstream* through a
velocity field (rightward drift + fixed background vortices + the pointer's
steerable vortex) and averages a noise texture along the path, so bright streaks
ride the streamlines. Pure `f64` + `sqrt` — no trig, no GPU. **Move the mouse
over the canvas** and the flow bends around the cursor.

## Build & run

Needs `karac` with the threaded-WASM runtime archive (see the repo root
`CLAUDE.md` for the one-time archive build), plus `python3` for the server.

```bash
./build.sh          # build + serve on http://localhost:8000
./build.sh --build  # build only
```

Then open <http://localhost:8000> and move the mouse over the canvas.

**Cross-origin isolation is required.** `SharedArrayBuffer` and the Worker pool
only exist on a page served with

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

`serve.py` sets both. A plain `python3 -m http.server` does **not**, and the
threaded module will silently fall back to the single-thread build (still
renders and steers, but on one core).

## Artifacts

`karac build plume.kara --target=wasm_browser --features wasm-threads` emits:

- `plume.threads.wasm` — the threaded module (Worker pool + SAB)
- `plume.wasm` — sequential fallback (no cross-origin isolation)
- `plume.js` — the loader/glue (picks the right module at load time)
- `plume.d.ts` — TypeScript declarations

## Compiler work this dogfood drove

Building Plume surfaced and closed real `karac` gaps (the dogfood's job — cf.
`feedback_no_workarounds_fix_compiler`):

1. **Event-data channels — `std.web.events.pointer_moves()`** (the
   `Channel[T]`-for-`T != ()` slice). The first *non-unit* host-async producer:
   the host marshals each `pointermove`'s `(x, y)` into a service-instance
   scratch buffer (`karac_runtime_event_scratch`) and `channel_send`s a 16-byte
   `PointerEvent` payload — vs the 0-byte `()` of a timer/frame producer. Built
   end to end across stdlib / codegen / runtime / glue (see
   [`docs/implementation_checklist/phase-10-targets.md`](../../docs/implementation_checklist/phase-10-targets.md)).
2. **`f64.sqrt()`** — the first piece of a numeric math surface, lowered to the
   `llvm.sqrt` intrinsic (a single `f64.sqrt` on wasm, no libm), used to
   normalize the flow velocity so streamline steps are constant-length. `sin` /
   `cos` / `atan2` remain a tracked gap — Plume's field is built from rational
   vortices precisely because there is no trig yet.

The render-loop / Worker-pool / `put_pixels` / `Vec.as_ptr` spine is shared with
**Fathom**, which drove those pieces first; Plume adds the live-input leg.

## Verified by

`tests/cli.rs::plume_example_pointer_steered_flow_e2e` builds this exact source
for `wasm_browser --features wasm-threads` and runs it under node: it asserts
frames render, the framebuffer is non-uniform, and the pointer's warm-tint
region tracks a fed cursor position (near-cursor R channel ≫ far corner) — a
bit-rot guard on the demo and a proof that pointer steering reaches the kernel.
