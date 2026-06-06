//! Browser-WASM JS glue generation (`--target=wasm_browser`, phase-10
//! "`host fn` lowering — browser-WASM target").
//!
//! A `wasm_browser` build emits two artifacts: `<stem>.wasm` (a
//! wasm32-wasip1 command module — same flavor as `wasm_wasi`, see
//! design.md § Host Functions) and `<stem>.js`, the ES-module glue this
//! module renders. The glue carries everything a JS host needs to run
//! the module with zero custom loader configuration:
//!
//!   - the **`kara_host` import namespace**: every `host fn` in the
//!     program becomes a WASM import entry `kara_host.<name>` (codegen
//!     attaches `wasm-import-module` / `wasm-import-name` string
//!     attributes in `declare_one_extern_function`); the glue maps the
//!     user's implementation object onto that namespace and fails
//!     loudly at instantiation when an implementation is missing;
//!   - a **minimal WASI preview-1 polyfill** (console-backed fd_write,
//!     proc_exit, clock, randomness, args/environ negotiation) so the
//!     wasip1 module runs in browsers and node without a WASI host;
//!     un-polyfilled syscalls throw loudly by name via a Proxy;
//!   - a **default module loader** built on
//!     `new URL("<stem>.wasm", import.meta.url)` — the asset-reference
//!     pattern vite / webpack / esbuild / rollup understand natively
//!     (no custom loader), with a `node:fs` branch for `file:` URLs so
//!     the same glue runs under node ≥ 18.
//!
//! Calling-convention contract (stable, documented in the glue header
//! and design.md § Host Functions): numeric scalars pass by value;
//! `i64`/`u64`/`isize`/`usize` cross the JS boundary as `BigInt`;
//! opaque handles cross at their declared scalar width (an i32-field
//! handle is a JS number, an i64-field handle a BigInt); strings cross
//! as `(ptr, len)` pairs read with the exported `readString` helper.
//! Each host implementation additionally receives one trailing context
//! argument `{ memory, readString(ptr, len) }` so string params are
//! readable without plumbing the memory export by hand.
//!
//! This module is deliberately **inkwell-free** (codegen containment —
//! CLAUDE.md § Architecture): it consumes the plain AST and emits a
//! string. The CLI writes the file next to the `.wasm` artifact.

use crate::ast::{ExternFunction, TypeExpr};
use crate::ast::{Item, Program, TypeKind};
use std::collections::HashMap;

/// How a scalar crosses the wasm↔JS boundary: wasm `i64` arrives in JS
/// as `BigInt`; every other `host fn`-legal scalar is a JS number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsScalar {
    Number,
    BigInt,
}

/// One `host fn` parameter as the glue documents it.
#[derive(Debug, Clone)]
pub struct HostParam {
    pub name: String,
    /// Kāra-surface type rendering (`i64`, `*const u8`, `ElementHandle`).
    pub kara_ty: String,
    pub js: JsScalar,
}

/// One `host fn` signature, reduced to what the JS glue needs.
#[derive(Debug, Clone)]
pub struct HostFnSig {
    pub name: String,
    pub params: Vec<HostParam>,
    /// `None` for unit returns; otherwise the Kāra type rendering and
    /// its JS-boundary classification.
    pub ret: Option<(String, JsScalar)>,
}

/// Collect every `host fn` declaration in `program` (the `"host"` ABI
/// sentinel on `ExternFunction` — `host fn` never appears inside
/// `extern` blocks; the parser rejects `extern "host"`). Opaque-handle
/// widths are resolved against the program's own struct declarations:
/// a single-field struct crosses at its field's scalar width.
pub fn collect_host_fns(program: &Program) -> Vec<HostFnSig> {
    // Single-primitive-field structs: name → the field's JS classification.
    let handle_widths: HashMap<&str, JsScalar> = program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(s) if s.fields.len() == 1 => {
                Some((s.name.as_str(), js_scalar(&s.fields[0].ty, &HashMap::new())))
            }
            _ => None,
        })
        .collect();

    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::ExternFunction(ext) if ext.abi == "host" => {
                Some(host_fn_sig(ext, &handle_widths))
            }
            _ => None,
        })
        .collect()
}

fn host_fn_sig(ext: &ExternFunction, handles: &HashMap<&str, JsScalar>) -> HostFnSig {
    let params = ext
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| HostParam {
            name: p
                .name()
                .map(str::to_string)
                .unwrap_or_else(|| format!("arg{i}")),
            kara_ty: type_expr_display(&p.ty),
            js: js_scalar(&p.ty, handles),
        })
        .collect();
    let ret = ext.return_type.as_ref().and_then(|ty| match &ty.kind {
        TypeKind::Tuple(elems) if elems.is_empty() => None,
        _ => Some((type_expr_display(ty), js_scalar(ty, handles))),
    });
    HostFnSig {
        name: ext.name.clone(),
        params,
        ret,
    }
}

/// JS-boundary classification for a `host fn`-legal type. 64-bit
/// integers lower to wasm `i64` (`llvm_type_for_name` maps
/// `i64`/`u64`/`isize`/`usize` to LLVM i64 — Kāra keeps 64-bit `usize`
/// semantics on wasm32) and cross as `BigInt`; pointers are wasm32
/// addresses (i32 → number); single-field handle structs cross at
/// their field's width. Anything unrecognized is documented as a
/// number — the classification feeds doc comments, not codegen.
fn js_scalar(ty: &TypeExpr, handles: &HashMap<&str, JsScalar>) -> JsScalar {
    match &ty.kind {
        TypeKind::Pointer { .. } => JsScalar::Number,
        TypeKind::Path(path) if path.segments.len() == 1 => {
            let name = path.segments[0].as_str();
            match name {
                "i64" | "u64" | "isize" | "usize" => JsScalar::BigInt,
                _ => handles.get(name).copied().unwrap_or(JsScalar::Number),
            }
        }
        _ => JsScalar::Number,
    }
}

/// Kāra-surface rendering of a `host fn`-legal type for the glue's doc
/// comments. The boundary restriction keeps the shapes simple: paths,
/// raw pointers, `()`.
fn type_expr_display(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeKind::Path(path) => path.segments.join("."),
        TypeKind::Pointer { is_mut, inner } => {
            let qual = if *is_mut { "mut" } else { "const" };
            format!("*{qual} {}", type_expr_display(inner))
        }
        TypeKind::Tuple(elems) if elems.is_empty() => "()".to_string(),
        _ => "?".to_string(),
    }
}

/// One `//   name(params) -> ret` doc line per host fn, with the JS
/// arrival type noted wherever it is `BigInt` (the surprising case).
fn signature_doc_line(sig: &HostFnSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| {
            let bigint = if p.js == JsScalar::BigInt {
                " [BigInt]"
            } else {
                ""
            };
            format!("{}: {}{}", p.name, p.kara_ty, bigint)
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match &sig.ret {
        Some((ty, JsScalar::BigInt)) => format!(" -> {ty} [return a BigInt]"),
        Some((ty, JsScalar::Number)) => format!(" -> {ty}"),
        None => String::new(),
    };
    format!("//   {}({params}){ret}", sig.name)
}

/// Render the complete ES-module glue file. `wasm_filename` is the
/// sibling `.wasm` artifact's file name (not path — the glue resolves
/// it against `import.meta.url`).
pub fn render_glue(fns: &[HostFnSig], wasm_filename: &str) -> String {
    let mut out = String::with_capacity(8 * 1024);

    out.push_str(&format!(
        "// Generated by karac for {wasm_filename} — browser-WASM glue. DO NOT EDIT.\n"
    ));
    out.push_str(
        "//\n\
         // The module is a wasm32-wasip1 command module; this file supplies a\n\
         // minimal console-backed WASI preview-1 polyfill plus the `kara_host`\n\
         // import namespace where `host fn` implementations live (a stable\n\
         // contract — hand-rolled hosts instantiate with\n\
         // `{ kara_host: {...}, wasi_snapshot_preview1: {...} }`).\n\
         //\n\
         // Boundary conventions: numeric scalars pass by value; i64/u64/\n\
         // isize/usize cross as BigInt; opaque handles cross at their declared\n\
         // scalar width; strings cross as (ptr, len) pairs — read them with\n\
         // readString. Each implementation receives one trailing context\n\
         // argument `{ memory, readString(ptr, len) }`.\n",
    );
    if fns.is_empty() {
        out.push_str("//\n// This program declares no host fns.\n");
    } else {
        out.push_str(
            "//\n// Declared host fns (implement each in the object passed to\n\
             // run/instantiate):\n",
        );
        for sig in fns {
            out.push_str(&signature_doc_line(sig));
            out.push('\n');
        }
    }
    out.push('\n');

    out.push_str(&format!("const WASM_FILENAME = \"{wasm_filename}\";\n"));
    let names = fns
        .iter()
        .map(|s| format!("\"{}\"", s.name))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("const DECLARED_IMPORTS = [{names}];\n"));

    // The static remainder: helpers, WASI polyfill, import-object
    // construction, default loader, public API. Kept as one literal so
    // the emitted JS reads as a coherent hand-written module.
    out.push_str(GLUE_STATIC_BODY);
    out
}

/// The host-fn-independent remainder of the glue file.
const GLUE_STATIC_BODY: &str = r#"
/** Decode a (ptr, len) UTF-8 string out of the module's linear memory. */
export function readString(memory, ptr, len) {
  return new TextDecoder("utf-8").decode(
    new Uint8Array(memory.buffer, Number(ptr), Number(len)),
  );
}

/** Thrown by the WASI polyfill's proc_exit; run() swallows exit code 0. */
export class KaraExit extends Error {
  constructor(code) {
    super(`proc_exit(${code})`);
    this.code = code;
  }
}

// Minimal console-backed WASI preview-1 polyfill — just enough for the
// karac wasm runtime archive (stdout/stderr writes, clock, randomness,
// args/environ negotiation). Unknown syscalls throw loudly by name.
function makeWasiPolyfill(getMemory) {
  const view = () => new DataView(getMemory().buffer);
  const impl = {
    fd_write(fd, iovsPtr, iovsLen, nwrittenPtr) {
      const dv = view();
      let written = 0;
      let text = "";
      for (let i = 0; i < iovsLen; i++) {
        const base = iovsPtr + i * 8;
        const ptr = dv.getUint32(base, true);
        const len = dv.getUint32(base + 4, true);
        text += new TextDecoder("utf-8").decode(
          new Uint8Array(getMemory().buffer, ptr, len),
        );
        written += len;
      }
      dv.setUint32(nwrittenPtr, written, true);
      (fd === 2 ? console.error : console.log)(text.replace(/\n$/, ""));
      return 0;
    },
    fd_close() {
      return 0;
    },
    fd_seek() {
      return 70; // ESPIPE — the console streams are not seekable.
    },
    fd_fdstat_get(_fd, outPtr) {
      new Uint8Array(getMemory().buffer, outPtr, 24).fill(0);
      view().setUint8(outPtr, 2); // filetype: character_device
      return 0;
    },
    proc_exit(code) {
      throw new KaraExit(code);
    },
    clock_time_get(_id, _precision, outPtr) {
      const ns = BigInt(Math.round(performance.now() * 1e6));
      view().setBigUint64(outPtr, ns, true);
      return 0;
    },
    random_get(ptr, len) {
      globalThis.crypto.getRandomValues(
        new Uint8Array(getMemory().buffer, ptr, len),
      );
      return 0;
    },
    args_sizes_get(argcPtr, argvBufSizePtr) {
      view().setUint32(argcPtr, 0, true);
      view().setUint32(argvBufSizePtr, 0, true);
      return 0;
    },
    args_get() {
      return 0;
    },
    environ_sizes_get(countPtr, bufSizePtr) {
      view().setUint32(countPtr, 0, true);
      view().setUint32(bufSizePtr, 0, true);
      return 0;
    },
    environ_get() {
      return 0;
    },
    sched_yield() {
      return 0;
    },
  };
  return new Proxy(impl, {
    get(target, prop) {
      if (prop in target) return target[prop];
      return () => {
        throw new Error(
          `WASI syscall not polyfilled by this glue: ${String(prop)}`,
        );
      };
    },
  });
}

function buildImports(hostImpls, getMemory) {
  const missing = DECLARED_IMPORTS.filter(
    (n) => typeof hostImpls?.[n] !== "function",
  );
  if (missing.length > 0) {
    throw new Error(
      "missing host fn implementation(s): " + missing.join(", "),
    );
  }
  const ctx = {
    get memory() {
      return getMemory();
    },
    readString: (ptr, len) => readString(getMemory(), ptr, len),
  };
  const kara_host = {};
  for (const name of DECLARED_IMPORTS) {
    kara_host[name] = (...args) => hostImpls[name](...args, ctx);
  }
  return { kara_host, wasi_snapshot_preview1: makeWasiPolyfill(getMemory) };
}

// Default loader: `new URL(..., import.meta.url)` is the asset-reference
// pattern bundlers (vite / webpack / esbuild / rollup) rewrite natively —
// no custom loader configuration. Under node (file: URL) read from disk.
async function defaultSource() {
  const url = new URL(WASM_FILENAME, import.meta.url);
  if (url.protocol === "file:") {
    const [{ readFile }, { fileURLToPath }] = await Promise.all([
      import("node:fs/promises"),
      import("node:url"),
    ]);
    return await readFile(fileURLToPath(url));
  }
  return await fetch(url);
}

/**
 * Compile + instantiate the module. `hostImpls` maps each declared
 * host fn name to its implementation; missing names throw before any
 * wasm runs. `opts.module` (a WebAssembly.Module) or `opts.bytes`
 * (BufferSource) bypass the default loader.
 * Returns { instance, exports, memory }.
 */
export async function instantiate(hostImpls = {}, opts = {}) {
  let memory;
  const imports = buildImports(hostImpls, () => memory);
  const src = opts.module ?? opts.bytes ?? (await defaultSource());
  let instance;
  if (typeof Response !== "undefined" && src instanceof Response) {
    try {
      ({ instance } = await WebAssembly.instantiateStreaming(
        src.clone(),
        imports,
      ));
    } catch {
      // Wrong Content-Type from the server — compile from bytes instead.
      const mod = await WebAssembly.compile(await src.arrayBuffer());
      instance = await WebAssembly.instantiate(mod, imports);
    }
  } else {
    const mod =
      src instanceof WebAssembly.Module ? src : await WebAssembly.compile(src);
    instance = await WebAssembly.instantiate(mod, imports);
  }
  memory = instance.exports.memory;
  return { instance, exports: instance.exports, memory };
}

/**
 * Instantiate and run the program's entry point (`_start`). A clean
 * exit (proc_exit(0) or main returning) resolves normally; a nonzero
 * exit code rejects with KaraExit.
 */
export async function run(hostImpls = {}, opts = {}) {
  const handle = await instantiate(hostImpls, opts);
  try {
    handle.exports._start();
  } catch (e) {
    if (!(e instanceof KaraExit) || e.code !== 0) throw e;
  }
  return handle;
}
"#;
