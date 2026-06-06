# wasm_hello — compile Kāra to WebAssembly

A minimal compute-only program built for `wasm32-wasip1` and run under a
WASI host. Verified end-to-end: the wasm output prints the same bytes as
the native build.

```sh
cd examples/wasm_hello
karac build main.kara --target=wasm_wasi   # emits main.wasm in the CWD
node run_wasi.mjs                           # -> 42 \n 610
```

`karac build` writes `<stem>.wasm` to the current directory, so run the
build from inside this folder (or move `main.wasm` next to `run_wasi.mjs`).

## Toolchain prerequisites

- `karac` built with `--features llvm`.
- The wasm runtime archive, built once (see the repo `CLAUDE.md`):
  ```sh
  cargo rustc -p karac-runtime --release --target wasm32-wasip1 \
      --no-default-features --crate-type staticlib
  cp target/wasm32-wasip1/release/libkarac_runtime.a \
     target/release/libkarac_runtime_wasm.a
  ```
- A wasm linker. `wasm-ld` if you have it; otherwise point `KARAC_WASM_LD`
  at a `rust-lld` exposed under the name `wasm-ld`:
  ```sh
  ln -sf "$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/rust-lld" /tmp/wasm-ld
  export KARAC_WASM_LD=/tmp/wasm-ld
  ```
- Node 18+ (for the built-in `node:wasi` host) to run `run_wasi.mjs`.

## What works / what doesn't on WASM today

**Works (verified):** arithmetic, `Vec`, `Option`, `String`, `Map`, JSON,
structs/enums/`match`, recursion, float formatting, `println` to WASI
stdout, file read/write through WASI preopens — byte-identical to native.

**Not yet:** networking (the wasm archive is built `--no-default-features`
— no tokio/scheduler), auto-parallelism / `go` / `par` blocks (scheduler
symbols absent), SIMD-128 (falls back to scalar), and project-mode
(multi-file) wasm builds. WASM is **compute-only** for now. Full scope:
[`docs/implementation_checklist/phase-10-targets.md`](../../docs/implementation_checklist/phase-10-targets.md).
