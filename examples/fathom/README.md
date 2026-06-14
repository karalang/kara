# Fathom — a Mandelbrot explorer across every core, in a browser tab

Fathom is the front-end half of Kāra's "auto-concurrency without coloring"
thesis: a real-time fractal renderer where **one Kāra source** drives a render
loop whose per-frame compute fans out across a Web Worker pool — with no
`async`, no `await`, no callback coloring, and no hand-written threading code.

It is the smallest demo that still lands the headline: **real multi-core
compute in a browser, from one source.** (Its richer sibling **Plume** adds live
pointer input; **Slipstream**'s wasm edition is the full fluid-sim flagship on
the same spine — see [`docs/dogfooding.md`](../../docs/dogfooding.md).)

## What it shows

- The render loop is a plain blocking loop:

  ```kara
  let frames = animation_frames();
  loop {
      frames.recv();                 // parks until the next frame — no await
      render_frame(cx, cy, scale, workers);
  }
  ```

  `frames.recv()` blocks the worker until the host fires the next
  `requestAnimationFrame`; the worker parks and the host wakes it. *Where is the
  `await`? There isn't one.*

- Each frame's rows are split into bands and computed in parallel:

  ```kara
  let task: TaskHandle[Vec[u8]] = pool.spawn(|| render_rows(y0, y1, cx, cy, scale));
  ```

  Halve the available cores and the framerate visibly halves — the overlay shows
  live FPS, the worker count, and the frame counter.

- The finished framebuffer is blitted to a `<canvas>` through one host fn,
  handing JS the buffer's bytes directly out of the shared wasm memory:

  ```kara
  host fn put_pixels(ptr: *const u8, len: i64, w: i64, h: i64) with writes(Display);
  ...
  put_pixels(frame.as_ptr(), frame.len(), w, h);   // Vec[u8].as_ptr() FFI handoff
  ```

The view auto-zooms into the seahorse valley and resets when `f64` runs out of
precision — no input needed.

## Build & run

Needs `karac` with the threaded-WASM runtime archive (see the repo root
`CLAUDE.md` for the one-time archive build), plus `python3` for the server.

```bash
./build.sh          # build + serve on http://localhost:8000
./build.sh --build  # build only
```

Then open <http://localhost:8000>.

**Cross-origin isolation is required.** `SharedArrayBuffer` and the Worker pool
only exist on a page served with

```
Cross-Origin-Opener-Policy: same-origin
Cross-Origin-Embedder-Policy: require-corp
```

`serve.py` sets both. A plain `python3 -m http.server` does **not**, and the
threaded module will silently fall back to the single-thread build (still
renders, but on one core).

## Artifacts

`karac build mandelbrot.kara --target=wasm_browser --features wasm-threads`
emits:

- `mandelbrot.threads.wasm` — the threaded module (Worker pool + SAB)
- `mandelbrot.wasm` — sequential fallback (no cross-origin isolation)
- `mandelbrot.js` — the loader/glue (picks the right module at load time)
- `mandelbrot.d.ts` — TypeScript declarations

## Compiler work this dogfood drove

Building Fathom surfaced and closed real `karac` gaps (the dogfood's job — cf.
`feedback_no_workarounds_fix_compiler`):

1. **`std.web.time.animation_frames()`** — a multi-shot host-async channel
   producer (`requestAnimationFrame` re-arming itself each frame, coalesced to
   at most one un-drained tick). Sibling of `after`; same `--features
   wasm-threads` gate.
2. **`Vec[u8].as_ptr()` / `.as_mut_ptr()`** — the heap-buffer FFI handoff to a
   `host fn` (the framebuffer blit). Previously only `Array`/`CStr` had it; an
   `Array[u8, N]` of framebuffer size would overflow the wasm stack.
3. **`TaskHandle[T].join()` for non-scalar `T`** — `join` had returned `i64`
   unconditionally, so a `spawn` returning a `Vec`/`String`/struct came back as
   garbage and trapped. The typechecker now records each join's `T` and codegen
   sizes the cross-task transfer correctly.

It also surfaced a still-open codegen bug — a `for x in …` loop binding that
shares a name with an earlier same-function `let x` mis-resolves and
`TaskHandle.join` deadlocks on a stale handle (bug-ledger **B-2026-06-14-13**).
Fathom sidesteps it by binding the spawn result to `task` (distinct from the
`for handle in handles` join binding); see the comment in `mandelbrot.kara`.
