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

- The inner escape-time kernel (`escape2`) computes **two pixels at once** in one
  `Vector[f64, 2]` lane pair. The `z = z^2 + c` arithmetic, the `|z|^2 <= 4`
  escape test, and the per-lane count update each lower to a single WASM SIMD-128
  (`v128`) instruction — same source, no flag:

  ```kara
  let active = mag <= four;                     // f64x2.le → Vector[bool, 2] mask
  counts = counts + active.select(one, zero);   // v128.bitselect — +1 per inside lane
  ```

  (Confirmed in the emitted module: `f64x2.mul`/`add`/`sub`/`splat`/`le`,
  `v128.bitselect`, `i64x2.add`/`extract_lane`.) Native single-thread A/B against
  the scalar loop: **~1.47× fewer instructions retired** for byte-identical
  output. In a browser the win is CPU headroom, not higher FPS — the frame rate
  is `requestAnimationFrame`/vsync-capped.

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
3. **`TaskHandle[T].join()` for non-scalar `T`** (B-2026-06-14-14) — `join` had
   returned `i64` unconditionally, so a `spawn` returning a `Vec`/`String`/struct
   came back as garbage and trapped. The typechecker now records each join's `T`
   and codegen sizes the cross-task transfer correctly.
4. **`for x in xs` loop-binding name collision** (B-2026-06-14-13) — a loop
   binding sharing a name with an earlier same-function `let x` (here `for handle
   in handles` after `let handle = spawn(...)`) was conflated by the ownership
   RC analysis, which inserted a spurious RC fallback; codegen then RC-boxed the
   binding and mis-lowered the plain loop element as an Rc pointer → segfault
   (native) / `join` deadlock (wasm-threads). Fixed by scoping the for-loop
   binding to a per-loop `@forN` rename frame in the CFG (`src/cfg.rs`), like
   match arms — so `render_frame` reuses the natural name in both places.
