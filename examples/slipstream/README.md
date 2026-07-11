# Slipstream — an interactive wind tunnel in one Kāra source

A real-time 2D **Lattice-Boltzmann (D2Q9)** fluid solver. Air streams
left-to-right past a tilted wing; the colour is vorticity, so the von Kármán
wake lights up red/blue. Rotate the wing and watch the flow separate into
**stall**. The whole thing is one Kāra package compiled to two targets — a
native checksum oracle and a browser app — from the same source tree.

This is the Tier-1 flagship browser demo from
[`docs/dogfooding.md`](../../docs/dogfooding.md), built on the same wasm-threads
front-end spine as **Fathom** and **Plume** (`animation_frames` +
`TaskGroup.spawn` worker pool + `put_pixels` blit + event-data channels). Its
one new ingredient those demos don't have: **the simulation grid is state
carried and evolved across frames**, not recomputed from scratch each frame.

```
karac build                                                 # native oracle
karac build --target=wasm_browser --features wasm-threads   # browser tunnel
./build.sh            # build the browser app + serve it (COOP/COEP) on :8000
./build.sh --native   # build + run the native LBM checksum oracle
./build.sh --build    # build the browser app only
node verify_browser.mjs   # drive the real browser over CDP and assert it works
```

The browser needs cross-origin isolation (COOP/COEP headers) for
SharedArrayBuffer + the Web Worker pool; `serve.py` sets them, plain
`python3 -m http.server` does not.

## Controls

- **Slider** (`<input type=range>`) — set the wing's angle of attack directly;
  its live value streams in through the `std.web.events.input()` value channel.
- **↑ / ↓** (or **scroll**) — steepen / flatten the wing's angle of attack.
- **R** — reset to the default angle.

Push the angle past the stall point (~20°) and the attached flow breaks down: a
separation bubble peels off the upper surface and the wake explodes into
large-scale turbulence. Bring it back down and the flow reattaches.

## What it is, technically

- **`src/sim.kara`** — the shared, pure fluid kernel. D2Q9 equilibrium, BGK
  collision, a pull-scheme streaming step with bounce-back off the wing and
  equilibrium inflow on the borders, and a vorticity (curl) renderer. No host
  imports — the *identical* code runs native and in the browser. The carried
  grid is a **`layout` block (SoA)**: the nine distribution functions split into
  two cache groups, so each buffer is two parallel arrays rather than an array of
  72-byte cells. Adding the layout block changes only the *physical* memory
  layout — the native oracle's milestone checksums are byte-identical to the
  array-of-structs build, and the browser flagship runs on SoA too (the
  per-layout-monomorphization spike's slice-6 proof).
- **`src/host_macos.kara` / `src/host_linux.kara`** — the native oracle: runs a
  fixed, deterministic substep schedule and prints framebuffer checksums + peak
  speed at three milestones.
- **`src/host_wasm.kara`** — the browser render loop: a plain blocking
  `loop { frames.recv(); /* drain input */ /* advance fluid */ put_pixels(); }`
  with the live angle-of-attack channels.
- **`src/main.kara`** — `import host.{run}; fn main { run() }`. The `host`
  module is platform-suffixed; the build walker picks `host_wasm` for a
  `wasm_*` target and `host_macos`/`host_linux` natively. This is the
  one-source/two-target structure **Iris** proves, applied to the flagship.

The wing is defined by a *slope* (rise/run), using only `sqrt`, to avoid the
tracked `sin`/`cos` stdlib gap — the same precedent Plume set with its rational
vortices.

## Where is the threading code?

There isn't any. Each LBM substep splits its collide and stream passes into
row-bands via `TaskGroup.spawn`, one task per band, and every worker reads the
**one shared grid**:

```kara
// sim.kara — the whole fan-out, no locks, no SharedArrayBuffer juggling.
while k < workers {
    // ... compute this band's row range [y0, y1) ...
    handles.push(pool.spawn(|| collide_band(grid, y0, y1)));   // shared read of `grid`
    k = k + 1;
}
```

`grid` is captured **read-only by every task at once** — the canonical
parallel-stencil shape. That capture is exactly what this dogfood drove a
compiler fix for (see below).

## The compiler bug this dogfood found

Building the worker-pool fan-out surfaced **B-2026-06-19-11** (see
[`docs/bug-ledger.jsonl`](../../docs/bug-ledger.jsonl)): a heap value captured
**read-only by multiple sibling `TaskGroup.spawn` tasks** while the parent still
owns it was miscompiled into a **double-free**. The spawn lowering treated every
capture as a by-move transfer, so each task re-registered a free of the one
shared buffer — N tasks freed it N times, producing wrong results (a 4-task
band-sum of `0..99` returned `328` instead of `4950`) and an allocator abort
(`failed to lock mutex: Invalid argument`).

The fix makes spawn lowering honour the ownership pass's per-capture mode: a
**borrowed** capture stays owned by the parent and is freed exactly once after
the structured-concurrency join barrier (the same `Copy`-capture rule a `par {}`
branch already uses, and the borrow design.md § Structured Concurrency Lifetime
Guarantees sanctions); only a **moved** capture transfers to the task. Per the
dogfooding charter (`feedback_no_workarounds_fix_compiler`), the fix went into
the compiler — the demo uses the natural shared-read fan-out, not a workaround.

## How it's verified

- **Native oracle (kernel correctness).** `./slipstream` prints framebuffer
  checksums at frames 30 / 60 / 120 plus peak speed. The checksums **differ**
  across milestones (the carried grid genuinely evolves), are **bit-identical
  run-to-run** (deterministic), and the peak speed holds ~0.17 — far below the
  lattice sound speed ~0.577 (stable, no blow-up). Crucially, the parallel
  fan-out's checksums are **byte-identical to the sequential kernel's**: the
  worker-pool decomposition changes nothing about the result, only how it is
  computed.
- **Browser (`verify_browser.mjs`).** Drives the real threaded wasm build in
  headless Chrome over CDP and asserts: cross-origin isolated, the blocking
  render loop advances, the canvas is non-uniform and **evolves** over time (the
  carried grid is integrating), it **soaks** to several hundred frames still
  advancing and non-uniform (no leak / OOM / deadlock — the classes that only
  surface in a real browser, cf. Fathom), and the **wheel angle control works**
  (scrolling steepens the wing — its grey-pixel height grows then shrinks).

## Honest cuts and follow-ups

- The **collide** pass — the pure per-cell BGK relaxation, the inner hot path —
  now runs as a `Vector[f64, 2]` SIMD-128 kernel (`collide2` in `sim.kara`): two
  horizontally-adjacent cells per lane pair, the same lowering Fathom's Mandelbrot
  kernel uses, with the scalar `if rho <= 0.0` guard as a per-lane mask/select.
  Output is **byte-identical** to the scalar kernel — the native-oracle checksums
  are unchanged (1582897806 / 793640938 / 680974524) and the built binary carries
  packed-double ops (`mulpd`/`addpd`/`cmplepd`/`unpcklpd`) — so the win is
  per-substep cost, not behaviour. The **stream** pass stays scalar: its per-cell
  solid/boundary bounce-back branches and data-dependent neighbour gathers don't
  map to a clean lane-pair pass. Building the SIMD collide surfaced and fixed a
  real compiler gap (the dogfood's job): **B-2026-07-11-1** — a `Vector[T, N]`
  wasn't classified `Copy` by the ownership checker, so aliasing a SIMD lane
  bundle for readability (`let e1 = ux;`) spuriously moved it; fixed in
  `src/ownership.rs` by adding the `Type::Vector` arm to `is_copy_type`.
- The wing angle is now driven by a real HTML **`<input type=range>` slider**
  (drag it to set the angle directly), alongside the arrow-keys / scroll controls.
  That drove a new `std.web.events` producer — **`input()`**, the DOM-element
  *value* channel: the slider's live `valueAsNumber` streams straight in as the
  slope, no `addEventListener` glue (`host_wasm.kara` drains `input().try_recv()`).
  It rides the same service-instance spine as `keydown`/`wheel`; the `slider` step
  in `verify_browser.mjs` drags the range input over CDP and asserts the wing
  responds (steep → flat). Producer round-trip pinned by
  `tests/cli.rs::wasm_threads_input_payload_recv_e2e`.
- The native **CPU SDL2** edition and the **GPU** path described in the roster
  remain Phase-11 / Phase-10 work; this is the browser edition, which the
  front-end spine already unblocks.
- `sin`/`cos`/`atan2` remain a tracked stdlib gap, so the wing is parameterised
  by slope rather than a true angle.
