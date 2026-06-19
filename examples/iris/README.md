# Iris — one Kāra source, image filters native *and* in the browser

Iris is the cross-target demo: **one Kāra package** of image-processing kernels,
compiled with no change to a **native binary** and to **browser WASM**. The same
`karac build` on the same source tree produces both — only `--target` differs.
The browser build is an interactive editor (press 1–6 to switch filter, every
frame computed across a Web Worker pool with no `async` and no threading code);
the native build is a checksum oracle. A verification harness then proves the two
produce **byte-identical** output — the honest form of "no port."

It is the discrete sibling of the continuous-loop browser demos **Fathom** and
**Plume**: same framebuffer-blit + worker-pool spine, but the headline here is
the *one-source / two-target* story rather than a live simulation. See
[`docs/dogfooding.md`](../../docs/dogfooding.md).

## The "one source" structure

```
iris/
├── kara.toml
└── src/
    ├── filters.kara      # the shared kernel — target-agnostic, no host fns
    ├── main.kara         # the shared entry: `import host.{run}; fn main(){ run() }`
    ├── host_wasm.kara    # browser I/O   (selected on --target=wasm_browser)
    ├── host_macos.kara   # native I/O    (selected natively on macOS)
    └── host_linux.kara   # native I/O    (selected natively on Linux)
```

`filters.kara` and `main.kara` carry **no** target knowledge. The only
target-specific code is the `host` module, which exists in platform-suffixed
variants the compiler's module walker selects automatically per build target
(`docs/design.md § Module System — Conditional compilation`). `main.kara` calls
`host.run()` and never knows which `run` it got. That is the whole claim, made
structural: the pixels are computed by identical code on both targets.

The filters are ordinary 3×3 convolutions + per-pixel maps over an RGBA buffer —
non-trivial enough to be a real editor (box blur, sharpen, Sobel edge detect),
plus invert and grayscale — written as a pure per-row-band kernel so the browser
entry can fan the bands across the worker pool with no change to the math:

```kara
let handle: TaskHandle[Vec[u8]] = pool.spawn(|| render_band(y0, y1, filter_id));
```

The image being edited is a **procedural** source (a pure per-pixel function — a
filled disc, a checkerboard, diagonal stripes, a colour ramp), not a loaded
photo, so the source pixels are recomputed identically on every target and the
native-vs-wasm comparison is byte-exact. Loading a real photo needs an
image-decode host fn and is a tracked follow-up (below).

## Build & run

Needs `karac` with the threaded-WASM runtime archives (see the repo-root
`CLAUDE.md` one-time archive build) and `python3` for the server.

**Browser editor:**

```bash
./build.sh          # build wasm + serve on http://localhost:8000
./build.sh --build  # build only
```

Then open <http://localhost:8000> and press **1–6** (or click the buttons):
1 original · 2 blur · 3 sharpen · 4 edge · 5 invert · 6 grayscale. Each switch
fans the filter out across the worker pool in one parallel pass; the overlay
shows the active filter, the worker count, the render latency, and a render
counter. The image is static, so Iris renders **on filter change**, not every
frame — the renderer sits idle between switches rather than burning every core
on a frame that never changes.

**Cross-origin isolation is required.** `SharedArrayBuffer` and the Worker pool
only exist on a page served with `Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`. `serve.py` sets both; a plain
`python3 -m http.server` does **not**, and the threaded module silently falls
back to one core.

**Native oracle:**

```bash
karac build && ./iris      # or: ./build.sh --native
```

prints one checksum per filter — the ground truth for the browser build.

## Verifying native == wasm

`verify_browser.mjs` is the A/B proof. It runs the native binary to get a
checksum of every filtered image, then drives the browser build in headless
Chrome over the DevTools Protocol, switches through all six filters, reads the
canvas back, hashes it with the **identical** FNV-1a, and asserts every filter's
browser pixels hash to the exact native value:

```bash
./build.sh --build && node verify_browser.mjs
```

Exits `0` on PASS, `2` if Chrome or the artifacts are missing. (A node mock-DOM
harness can't run this — it needs a real Worker pool, `SharedArrayBuffer`, and a
`<canvas>`; the methodology is `reference_headless_browser_wasm_testing`.) Filter
switches are dispatched as synthetic `keydown`s through the real host-listener →
channel → `recv` path; synthetic (not CDP `Input.dispatchKeyEvent`) avoids the
headless-Chrome trusted-key flood the Fathom harness documents.

## Artifacts

`karac build --target=wasm_browser --features wasm-threads` emits under
`dist/wasm/` (copied next to `index.html` by `build.sh`):

- `iris.threads.wasm` — the threaded module (Worker pool + SAB)
- `iris.wasm` — sequential fallback (no cross-origin isolation)
- `iris.js` — the loader/glue (picks the right module at load time)
- `iris.d.ts` — TypeScript declarations

`karac build` (no target) emits the native `iris` binary.

## Compiler work this dogfood drove

Building Iris surfaced and closed a real `karac` gap (the dogfood's job — cf.
`feedback_no_workarounds_fix_compiler`):

1. **Project-mode `--target` did not drive platform-suffix module selection.**
   `cmd_build_project` always walked the source tree with `WalkerOpts::default()`
   (host platform), so `karac build --target=wasm_browser` selected the host's
   `_macos`/`_linux` modules and dropped the `_wasm` one — a project that swaps
   its host/IO layer per target via platform suffixes built the *wrong half*
   (here, the native checksum oracle compiled into the wasm module instead of the
   browser editor). Fixed in `src/cli.rs`: the project walker now takes
   `Platform::Wasm` for any `--target=wasm_*` build, matching single-file
   cross-target behavior. This is the gap that makes the whole one-source/
   two-target structure load-bearing — without it, platform-suffixed host layers
   silently don't work in project mode. Regression: `tests/cli.rs`
   `project_build_wasm_target_selects_wasm_platform_module`.

## Follow-ups (own slices)

- [ ] **Real-image load** — a `load_source(ptr, max_len) -> i64` host fn that
  copies a decoded `<img>`/`ImageData` into wasm memory, so the editor filters an
  actual photo rather than the procedural source. (The native A/B oracle would
  then read the same fixture image from a file via a `reads(FileSystem)` path.)
- [ ] **SIMD inner kernel** — vectorize the convolution/per-pixel kernels to
  `Vector[u8, 16]` / `Vector[i32, 4]` → WASM SIMD-128, as Fathom did for its
  escape-time loop. The image width is even to keep a two-pixel-per-lane loop
  aligned; the lowering already ships, so this is demo-source-only.
- [x] **Render-only-on-change — DONE.** The browser loop renders only when the
  filter actually changes (a `dirty` flag), keeping `frames.recv()` every tick
  for scheduling/input. The image is static, so this cuts idle CPU to ~zero
  between switches — and, by leaving the render thread idle, makes the CDP-driven
  `verify_browser.mjs` reliable (continuous full-frame rendering pegged the
  renderer and starved the harness's evals).
- [ ] **Histogram equalization** — a global two-pass filter (build the luminance
  histogram, then remap), which needs a parallel reduce + scan across bands
  rather than the embarrassingly-parallel per-band shape here. A good dogfood for
  cross-band reduction.
