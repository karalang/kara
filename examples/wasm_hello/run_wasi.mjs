// Run the wasm_wasi build under Node's built-in WASI host.
//   karac build examples/wasm_hello/main.kara --target=wasm_wasi
//   node examples/wasm_hello/run_wasi.mjs
// Expected output:  42  then  610  (byte-identical to the native build).
import { readFile } from 'node:fs/promises';
import { WASI } from 'node:wasi';

const wasi = new WASI({ version: 'preview1', args: [], env: {} });
const bytes = await readFile(new URL('./main.wasm', import.meta.url));
const mod = await WebAssembly.compile(bytes);
const inst = await WebAssembly.instantiate(mod, wasi.getImportObject());
wasi.start(inst);
