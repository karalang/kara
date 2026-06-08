# ssr_counter — server-side rendering with provider injection

One Kāra source file, two targets, one shared component. The server
renders the component to HTML; the browser hydrates it and drives live
DOM updates — running the **same** component function on both sides.

This is the worked example for the SSR / isomorphic-rendering pattern in
[design.md § Cross-target Compilation](../../docs/design.md#cross-target-compilation)
and the book chapter
[Server-Side Rendering](../../docs/book/src/ch17-ssr.md).

## The pattern in one sentence

The component (`render_counter`) is an ordinary **target-agnostic**
function — no `#[target(...)]`, no `#[cfg]`. The server and the client
differ only in which **provider** they bind for the abstract `Sink`
resource; the `#[target(...)]` attributes sit solely on the two entry
points (`main`, `hydrate`), which is the one place they belong.

| | server (`native`) | client (`wasm_browser`) |
|---|---|---|
| entry point | `#[target(native)] fn main` | `#[target(wasm_browser)] pub fn hydrate` |
| `Sink` provider | `StringSink` → HTML on stdout | `DomSink` → DOM via host fns |
| static chrome (`heading`) | emitted as HTML | already in the SSR'd page → no-op |
| dynamic state (`count`, `parity`) | formatted into HTML | crosses as `i64` to JS, mutates the DOM |

Only plain `i64`s cross the host boundary — exactly the shape host fns
allow. Text the component "owns" is emitted by the server and is already
present in the page the client hydrates, so it never has to cross.

## Run the server leg

```sh
cd examples/ssr_counter
karac build ssr_counter.kara          # emits ./ssr_counter
./ssr_counter
# <h1>Kāra SSR Counter</h1><output id="count">42</output><span id="parity">even</span>
```

That stdout is the page body a real server would write into its HTTP
response. `index.html` shows it inlined inside `<div id="app">`.

## Run the client leg

Under node, with a mock DOM (this is also the CI E2E):

```sh
karac build ssr_counter.kara --target=wasm_browser   # emits .wasm + .js + .d.ts
node run_browser.mjs
# HYDRATED {"count":10,"parity":"even"}
```

In a real browser, against a live `document`:

```sh
karac build ssr_counter.kara --target=wasm_browser
python3 -m http.server          # serve this directory over http
# open http://localhost:8000/ and click "increment"
```

The page arrives server-rendered; the inline module script in
`index.html` instantiates the wasm, supplies the real DOM host fns, and
calls `hydrate` — first to adopt the current value, then on every click.

## Toolchain prerequisites

- `karac` built with `--features llvm`.
- The native runtime archive (`libkarac_runtime.a`) and the wasm runtime
  archive (`libkarac_runtime_wasm.a`) — see the repo `CLAUDE.md` for the
  one-time build recipe.
- A wasm linker for the browser leg (`wasm-ld`, or a `rust-lld` exposed as
  `wasm-ld` via `KARAC_WASM_LD`; see `examples/wasm_hello/README.md`).
- `node` ≥ 18 for `run_browser.mjs`.

## Files

| file | role |
|---|---|
| `ssr_counter.kara` | the dual-target source: component, two providers, two entry points |
| `index.html` | browser harness — SSR'd skeleton + hydration script |
| `run_browser.mjs` | node harness — hydration against a mock DOM |
