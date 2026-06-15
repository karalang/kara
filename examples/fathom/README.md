# Fathom — a Mandelbrot explorer across every core, in a browser tab

Fathom is the front-end half of Kāra's "auto-concurrency without coloring"
thesis: a real-time fractal renderer where **one Kāra source** drives a render
loop whose per-frame compute fans out across a Web Worker pool — with no
`async`, no `await`, no callback coloring, and no hand-written threading code.

It is the smallest demo that still lands the headline: **real multi-core
compute in a browser, from one source** — and it is **interactive**: scroll to
zoom toward the cursor, drag to pan. (Its sibling **Plume** drives
the same live-input spine for a flow field; **Slipstream**'s wasm edition is the
full fluid-sim flagship on it — see [`docs/dogfooding.md`](../../docs/dogfooding.md).)

## What it shows

- The render loop is a plain blocking loop that also folds in live input —
  every frame it drains the latest scroll and pointer move with a non-blocking
  `try_recv`, with no `await` and no event callbacks:

  ```kara
  let frames = animation_frames();
  let wheels = wheel();           // Channel[WheelEvent]   — scroll
  let moves  = pointer_moves();   // Channel[PointerEvent] — pan
  loop {
      frames.recv();              // parks until the next frame — no await
      match wheels.try_recv() { Some(ev) => zoom_toward(ev), None => {} }
      match moves.try_recv()  { Some(p)  => grab_pan(p),     None => {} }
      render_frame(cx, cy, scale, workers);
  }
  ```

  `frames.recv()` blocks the worker until the host fires the next
  `requestAnimationFrame`; the worker parks and the host wakes it. The wheel and
  pointer events cross host→wasm as typed channel payloads (`WheelEvent` carries
  the cursor position, so a scroll can zoom *toward the cursor*). *Where is the
  `await`, the `addEventListener`, the SharedArrayBuffer juggling? There is
  none.*

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

## Interaction

- **Scroll** to zoom — `wheel()` (`std.web.events`) delivers a `WheelEvent`
  with the cursor position and scroll deltas. The loop scales the view while
  holding the complex point under the cursor fixed, so you zoom *into whatever
  you point at* (scroll up = in). Zoom is clamped to the full view at the top
  and to `f64`'s resolving power at the bottom.
- **Drag** to pan — `pointer_moves()` delivers a `PointerEvent` carrying the
  position *and* the held-buttons bitmask. The loop pans by the pointer's
  per-frame delta **only while a button is held** (`p.pressed()`), so a plain
  hover does nothing — true click-drag, not hover-pan. The last position is
  tracked on every move so a fresh drag never jumps.
- **Keyboard** — `keydown()` delivers a `KeyEvent` with the DOM `key_code()`:
  arrow keys pan, `+`/`-` zoom toward centre, `R` resets to the full view. The
  listener is on the window, so no canvas focus is needed.

The canvas is rendered 1:1 (CSS size == the wasm framebuffer size) so each
event's element-relative coordinates map straight onto internal pixels — no
rescale in the demo math.

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

### Verifying it actually works

A node mock-DOM harness (cf. `ssr_counter/run_browser.mjs`) can't exercise this
demo — it needs a real Worker pool, `SharedArrayBuffer`, a `<canvas>`, and live
input. `verify_browser.mjs` drives an actual headless Chrome over the DevTools
Protocol instead, and asserts the full interactive path:

```bash
./build.sh --build && node verify_browser.mjs
```

It loads the page cross-origin-isolated, checks the frame counter advances
(the blocking render loop is running on a host-woken worker), confirms the
canvas has real content, then **dispatches synthetic CDP wheel, pointer, and
keyboard events** — the real host-listener → channel → wasm `recv` path, not a
JS shim — and asserts: a scroll zooms, a buttonless **hover does NOT pan** (the
canvas is unchanged — the click-drag gate works), a **drag with a button held
does pan**, and an **ArrowRight keypress pans** (driving the `keydown`
producer). Exits `0` on PASS, `2` if no Chrome is found. (This is the methodology from
`reference_headless_browser_wasm_testing`: node E2E cannot catch browser-only
wasm-threads bugs, so the input path must be exercised in a real browser.)

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

The **interactive pan/zoom** slice first landed as pure demo source — it
consumes the already-shipped `std.web.events.wheel` / `pointer_moves` producers,
which is the point: the spine was built so a demo like this needs only
application code. That first cut surfaced one feature gap, now **closed by this
revision**:

5. **`PointerEvent.buttons`** — the pointer payload carried only `{ x, y }`, so
   the demo could only hover-pan. `PointerEvent` now also carries the DOM
   `MouseEvent.buttons` bitmask (`buttons()` / `pressed()` accessors): the host
   glue marshals it as an `i64` at byte 16 (24-byte payload now), so the demo
   gates panning on a held button for true click-drag. (`runtime/stdlib/web_events.kara`
   + `src/wasm_glue.rs`; round-trip pinned by
   `tests/cli.rs::wasm_threads_pointer_moves_payload_recv_e2e`, which now asserts
   the `buttons` field crosses host→wasm.)

6. **`std.web.events.keydown()`** — the third non-unit event-data producer
   (after `pointer_moves`/`wheel`), adding keyboard input. `keydown() ->
   Receiver[KeyEvent]` where `KeyEvent { key_code: i64 }` (8-byte payload), with
   a `key_code()` accessor. Same spine as its siblings: a `__schedule_keydown`
   builtin (`src/codegen/channel.rs`) emits the `__kara_keydown` import; the
   glue's `keydown` listener (`src/wasm_glue.rs`, on the window — keydown bubbles,
   so no focus needed) marshals `e.keyCode` as a little-endian i64 and
   `channel_send`s 8 bytes. Drives Fathom's keyboard controls (arrows / `+`-`-` /
   `R`). Pinned by `tests/cli.rs::{wasm_threads_keydown_payload_recv_e2e,
   wasm_keydown_sequential_target_rejected}` + the browser `verify_browser.mjs`
   keypress step. Touches only the karac binary + baked stdlib, no runtime `.a`.

Ops gotcha (still true): the `wasm` / `wasm-threads` runtime archives must be
rebuilt after any *runtime* change (the realloc-grow path `cda981bc` added
`__karac_realloc_or_panic64`, so a stale archive fails the link with
`undefined symbol` — rebuild per the repo-root `CLAUDE.md` archive recipe).
(This `buttons` change touches only the karac binary + baked stdlib, not the
runtime `.a`, so it needed a karac rebuild but no archive refresh.)
