//! Browser-WASM JS glue generation (`--target=wasm_browser`, phase-10
//! "`host fn` lowering — browser-WASM target").
//!
//! A `wasm_browser` build emits three artifacts: `<stem>.wasm` (a
//! wasm32-wasip1 command module — same flavor as `wasm_wasi`, see
//! design.md § Host Functions), `<stem>.js`, the ES-module glue this
//! module renders, and `<stem>.d.ts`, the TypeScript declarations for
//! that glue (see [`render_dts`]). The glue carries everything a JS
//! host needs to run the module with zero custom loader configuration:
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

/// Threaded-build parameters for the glue (phase-10 "WASM concurrency
/// lowering — `--features wasm-threads` opt-in"). Present only when the
/// build emitted the dual artifact; drives the glue's load-time pick
/// between the threaded module (`<stem>.threads.wasm`, a Web Worker
/// pool over SharedArrayBuffer) and the sequential one. Plain data —
/// the CLI assembles it from the flag, the `[wasm]` manifest knobs,
/// and the linked threaded module's own memory-import limits.
#[derive(Debug, Clone)]
pub struct WasmThreadsGlueConfig {
    /// Sibling threaded artifact's file name (`<stem>.threads.wasm`),
    /// resolved against `import.meta.url` like `WASM_FILENAME`.
    pub threads_filename: String,
    /// `[wasm] fallback = false`: when SAB/cross-origin-isolation are
    /// unavailable the glue hard-errors instead of console.warn +
    /// sequential.
    pub no_fallback: bool,
    /// `[wasm] pool-size`: worker-pool size baked into the glue's
    /// `KARAC_PAR_WORKERS` env injection. `None` = use
    /// `navigator.hardwareConcurrency` at load time.
    pub pool_size_override: Option<u32>,
    /// The threaded module's imported-memory limits (64 KiB pages),
    /// parsed out of the **linked** module by
    /// [`imported_memory_limits`] — wasm-ld computes `initial` from the
    /// data/stack layout, so it can't be predicted, only read back. The
    /// glue must create its `WebAssembly.Memory({shared: true})` with
    /// exactly these limits or instantiation fails the import match.
    pub mem_initial_pages: u32,
    pub mem_max_pages: u32,
}

/// Parse the `(import "env" "memory" (memory min max shared))` limits
/// out of a linked wasm binary — the threaded module declares its
/// memory as an import (`--import-memory --shared-memory`), and the
/// glue must mirror the limits exactly. Returns `(initial, maximum)`
/// in pages; `None` when the binary has no imported memory (or is
/// malformed) — the CLI treats that as a build error for the threaded
/// artifact. Minimal single-purpose reader, not a general wasm parser:
/// it walks sections to the import section (id 2) and scans entries.
pub fn imported_memory_limits(bytes: &[u8]) -> Option<(u32, u32)> {
    // Magic + version.
    if bytes.len() < 8 || &bytes[0..4] != b"\0asm" {
        return None;
    }
    let mut pos = 8usize;
    // LEB128 u32 decode.
    fn leb_u32(bytes: &[u8], pos: &mut usize) -> Option<u32> {
        let mut result: u32 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *bytes.get(*pos)?;
            *pos += 1;
            result |= u32::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
            shift += 7;
            if shift >= 35 {
                return None;
            }
        }
    }
    while pos < bytes.len() {
        let id = *bytes.get(pos)?;
        pos += 1;
        let size = leb_u32(bytes, &mut pos)? as usize;
        let section_end = pos.checked_add(size)?;
        if id != 2 {
            pos = section_end;
            continue;
        }
        // Import section: count, then (module, name, kind, …) entries.
        let count = leb_u32(bytes, &mut pos)?;
        for _ in 0..count {
            let mod_len = leb_u32(bytes, &mut pos)? as usize;
            let module = bytes.get(pos..pos + mod_len)?;
            pos += mod_len;
            let name_len = leb_u32(bytes, &mut pos)? as usize;
            let name = bytes.get(pos..pos + name_len)?;
            pos += name_len;
            let kind = *bytes.get(pos)?;
            pos += 1;
            match kind {
                0x00 => {
                    // function: typeidx
                    leb_u32(bytes, &mut pos)?;
                }
                0x01 => {
                    // table: reftype byte + limits
                    pos += 1;
                    let flags = *bytes.get(pos)?;
                    pos += 1;
                    leb_u32(bytes, &mut pos)?;
                    if flags & 0x01 != 0 {
                        leb_u32(bytes, &mut pos)?;
                    }
                }
                0x02 => {
                    // memory: limits — flags bit 0 = has-max, bit 1 = shared.
                    let flags = *bytes.get(pos)?;
                    pos += 1;
                    let min = leb_u32(bytes, &mut pos)?;
                    let max = if flags & 0x01 != 0 {
                        Some(leb_u32(bytes, &mut pos)?)
                    } else {
                        None
                    };
                    if module == b"env" && name == b"memory" {
                        // Shared memories always carry a max; mirror min
                        // when a (non-shared, theoretical) import lacks one.
                        return Some((min, max.unwrap_or(min)));
                    }
                }
                0x03 => {
                    // global: valtype + mutability
                    pos += 2;
                }
                _ => return None,
            }
        }
        return None; // import section scanned, no env.memory
    }
    None
}

/// Render the complete ES-module glue file. `wasm_filename` is the
/// sibling `.wasm` artifact's file name (not path — the glue resolves
/// it against `import.meta.url`). `threads` is `Some` on a `--features
/// wasm-threads` dual-artifact build — the glue then picks the threaded
/// module at load time when SAB + cross-origin isolation are available.
pub fn render_glue(
    fns: &[HostFnSig],
    wasm_filename: &str,
    threads: Option<&WasmThreadsGlueConfig>,
) -> String {
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

    if threads.is_some() {
        out.push_str(
            "//\n\
             // wasm-threads build: a second, threaded module ships alongside\n\
             // (Web Worker pool + SharedArrayBuffer + atomics). run() picks it\n\
             // at load time when SAB + cross-origin isolation are available —\n\
             // deploy with COOP/COEP headers:\n\
             //   Cross-Origin-Opener-Policy: same-origin\n\
             //   Cross-Origin-Embedder-Policy: require-corp\n\
             // instantiate() always uses the sequential module (its exports\n\
             // run on the caller's thread, which must never block).\n",
        );
    }
    out.push('\n');

    out.push_str(&format!("const WASM_FILENAME = \"{wasm_filename}\";\n"));
    let names = fns
        .iter()
        .map(|s| format!("\"{}\"", s.name))
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("const DECLARED_IMPORTS = [{names}];\n"));
    // Threaded-build constants — rendered as inert nulls/zeros on a
    // sequential-only build so the static body references them
    // unconditionally.
    match threads {
        Some(cfg) => {
            out.push_str(&format!(
                "const WASM_THREADS_FILENAME = \"{}\";\n",
                cfg.threads_filename
            ));
            out.push_str(&format!(
                "const THREADS_NO_FALLBACK = {};\n",
                cfg.no_fallback
            ));
            out.push_str(&format!(
                "const THREADS_POOL_SIZE = {};\n",
                cfg.pool_size_override
                    .map_or("null".to_string(), |n| n.to_string())
            ));
            out.push_str(&format!(
                "const THREADS_MEM_INITIAL_PAGES = {};\n",
                cfg.mem_initial_pages
            ));
            out.push_str(&format!(
                "const THREADS_MEM_MAX_PAGES = {};\n",
                cfg.mem_max_pages
            ));
        }
        None => {
            out.push_str(
                "const WASM_THREADS_FILENAME = null;\n\
                 const THREADS_NO_FALLBACK = false;\n\
                 const THREADS_POOL_SIZE = null;\n\
                 const THREADS_MEM_INITIAL_PAGES = 0;\n\
                 const THREADS_MEM_MAX_PAGES = 0;\n",
            );
        }
    }

    // The static remainder: helpers, WASI polyfill, import-object
    // construction, default loader, threaded-path machinery, public
    // API. Kept as one literal so the emitted JS reads as a coherent
    // hand-written module.
    out.push_str(GLUE_STATIC_BODY);
    out
}

/// TypeScript-surface rendering of a JS-boundary classification.
fn ts_type(js: JsScalar) -> &'static str {
    match js {
        JsScalar::Number => "number",
        JsScalar::BigInt => "bigint",
    }
}

/// Render the TypeScript declaration file (`<stem>.d.ts`) for the glue
/// module [`render_glue`] emits. Declares the glue's full public
/// surface — `readString`, `KaraExit`, `instantiate`, `run` — plus a
/// `HostImpls` interface typing every declared `host fn`
/// implementation on the JS side per the boundary contract
/// (`i64`/`u64`/`isize`/`usize` ⇒ `bigint`, everything else ⇒
/// `number`, trailing `HostCtx` argument). When the program declares
/// host fns, `hostImpls` is a *required* parameter — the glue throws
/// at instantiation on a missing implementation, so the declarations
/// surface that contract at compile time.
///
/// Per-export signatures beyond `_start` (including `Result`/`Option`
/// shapes and exported structs) extend this generator when the
/// phase-10 "WASM entry-point discovery" entry lands — today the only
/// wasm-exported user entry point is `main` via `_start`.
///
/// `threaded` mirrors [`render_glue`]'s `threads.is_some()`: a
/// wasm-threads build's declarations additionally carry
/// `KaraThreadedHandle` (what `run()` resolves with when the threaded
/// module was picked — the instance lives in the primary worker, so
/// only the shared memory crosses back) and the widened `run()` return
/// type; a sequential-only build's declarations are unchanged.
pub fn render_dts(fns: &[HostFnSig], wasm_filename: &str, threaded: bool) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(4 * 1024);

    let _ = write!(
        out,
        "// Generated by karac for {wasm_filename} — TypeScript declarations for\n\
         // the browser-WASM glue module. DO NOT EDIT.\n\n"
    );

    out.push_str(
        "/** Trailing context argument passed to every host fn implementation. */\n\
         export interface HostCtx {\n\
         \x20 readonly memory: WebAssembly.Memory;\n\
         \x20 /** Decode a (ptr, len) UTF-8 string out of the module's linear memory. */\n\
         \x20 readString(ptr: number | bigint, len: number | bigint): string;\n\
         }\n\n",
    );

    out.push_str(
        "/**\n\
         \x20* Host fn implementations (the `kara_host` import namespace).\n\
         \x20* i64/u64/isize/usize cross the boundary as bigint; every other\n\
         \x20* host-fn-legal scalar is a number; strings arrive as (ptr, len)\n\
         \x20* pairs — decode with `ctx.readString`.\n\
         \x20*/\n",
    );
    if fns.is_empty() {
        out.push_str(
            "// This program declares no host fns.\n\
             // eslint-disable-next-line @typescript-eslint/no-empty-object-type\n\
             export interface HostImpls {}\n\n",
        );
    } else {
        out.push_str("export interface HostImpls {\n");
        for sig in fns {
            // Kāra-surface signature as the doc line (the source of truth
            // the TS types were derived from).
            let kara_params = sig
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, p.kara_ty))
                .collect::<Vec<_>>()
                .join(", ");
            let kara_ret = match &sig.ret {
                Some((ty, _)) => format!(" -> {ty}"),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "  /** `host fn {}({kara_params}){kara_ret}` */",
                sig.name
            );
            let ts_params = sig
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type(p.js)))
                .collect::<Vec<_>>()
                .join(", ");
            let sep = if ts_params.is_empty() { "" } else { ", " };
            let ts_ret = match &sig.ret {
                Some((_, js)) => ts_type(*js),
                None => "void",
            };
            let _ = writeln!(
                out,
                "  {}({ts_params}{sep}ctx: HostCtx): {ts_ret};",
                sig.name
            );
        }
        out.push_str("}\n\n");
    }

    if threaded {
        out.push_str(
            "export interface InstantiateOpts {\n\
             \x20 /** Pre-compiled module — bypasses the default loader.\n\
             \x20  * Feeds the sequential path only. */\n\
             \x20 module?: WebAssembly.Module;\n\
             \x20 /** Raw module bytes — bypasses the default loader.\n\
             \x20  * Feeds the sequential path only. */\n\
             \x20 bytes?: BufferSource;\n\
             \x20 /** Skip the threaded-module pick in run() and use the\n\
             \x20  * sequential module unconditionally. */\n\
             \x20 forceSequential?: boolean;\n\
             }\n\n",
        );
    } else {
        out.push_str(
            "export interface InstantiateOpts {\n\
             \x20 /** Pre-compiled module — bypasses the default loader. */\n\
             \x20 module?: WebAssembly.Module;\n\
             \x20 /** Raw module bytes — bypasses the default loader. */\n\
             \x20 bytes?: BufferSource;\n\
             }\n\n",
        );
    }

    out.push_str(
        "export interface KaraHandle {\n\
         \x20 instance: WebAssembly.Instance;\n\
         \x20 exports: WebAssembly.Exports & { _start(): void };\n\
         \x20 memory: WebAssembly.Memory;\n\
         }\n\n",
    );
    if threaded {
        out.push_str(
            "/** Resolved by run() when the THREADED module was picked: the\n\
             \x20* program ran on the Web Worker pool (its instance lives in the\n\
             \x20* primary worker); only the shared memory crosses back. */\n\
             export interface KaraThreadedHandle {\n\
             \x20 memory: WebAssembly.Memory;\n\
             \x20 threaded: true;\n\
             }\n\n",
        );
    }
    out.push_str(
        "/** Decode a (ptr, len) UTF-8 string out of the module's linear memory. */\n\
         export function readString(\n\
         \x20 memory: WebAssembly.Memory,\n\
         \x20 ptr: number | bigint,\n\
         \x20 len: number | bigint,\n\
         ): string;\n\n\
         /** Thrown by the WASI polyfill's proc_exit; run() swallows exit code 0. */\n\
         export class KaraExit extends Error {\n\
         \x20 code: number;\n\
         \x20 constructor(code: number);\n\
         }\n\n",
    );

    // `hostImpls` optionality mirrors the runtime contract: with declared
    // host fns the glue throws on a missing implementation before any
    // wasm runs, so the parameter is required at the type level.
    let host_param = if fns.is_empty() {
        "hostImpls?: HostImpls"
    } else {
        "hostImpls: HostImpls"
    };
    let run_ret = if threaded {
        "Promise<KaraHandle | KaraThreadedHandle>"
    } else {
        "Promise<KaraHandle>"
    };
    let instantiate_doc = if threaded {
        "/**\n\
         \x20* Compile + instantiate the SEQUENTIAL module (its exports run on\n\
         \x20* the caller's thread, which must never block — the threaded\n\
         \x20* module is only ever driven by run()'s worker pool). Missing\n\
         \x20* host fn implementations throw before any wasm runs.\n\
         \x20*/\n"
    } else {
        "/**\n\
         \x20* Compile + instantiate the module. Missing host fn implementations\n\
         \x20* throw before any wasm runs.\n\
         \x20*/\n"
    };
    let run_doc = if threaded {
        "/**\n\
         \x20* Run the program: picks the THREADED module when SharedArrayBuffer\n\
         \x20* + cross-origin isolation are available (deploy with COOP/COEP\n\
         \x20* headers), else console.warns and falls back to the sequential\n\
         \x20* module (or throws, when the build disabled fallback). A clean\n\
         \x20* exit resolves normally; a nonzero exit code rejects with KaraExit.\n\
         \x20*/\n"
    } else {
        "/**\n\
         \x20* Instantiate and run the program's entry point (`_start`). A clean\n\
         \x20* exit resolves normally; a nonzero exit code rejects with KaraExit.\n\
         \x20*/\n"
    };
    let _ = write!(
        out,
        "{instantiate_doc}\
         export function instantiate(\n\
         \x20 {host_param},\n\
         \x20 opts?: InstantiateOpts,\n\
         ): Promise<KaraHandle>;\n\n\
         {run_doc}\
         export function run(\n\
         \x20 {host_param},\n\
         \x20 opts?: InstantiateOpts,\n\
         ): {run_ret};\n"
    );

    out
}

/// The host-fn-independent remainder of the glue file. (`r##` raw
/// delimiter: the JS contains `"#kara-thread-worker"`, whose `"#`
/// sequence would close a plain `r#` literal.)
const GLUE_STATIC_BODY: &str = r##"
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
// `env` is a list of "KEY=value" strings serialized through
// environ_get — the threaded path injects KARAC_PAR_WORKERS there (the
// runtime's pool-size knob); the sequential path passes none.
function makeWasiPolyfill(getMemory, env = []) {
  const view = () => new DataView(getMemory().buffer);
  const envBytes = env.map((s) => new TextEncoder().encode(s + "\0"));
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
      if (nodeFsSync) {
        // Threaded path under node: workers' console pipes to the main
        // process asynchronously and an unref'd pthread worker dying at
        // process exit drops anything unflushed — writeSync is the
        // loss-proof channel. Only ever set on the threaded path; the
        // sequential glue keeps the console behavior byte-for-byte.
        nodeFsSync.writeSync(fd === 2 ? 2 : 1, text);
      } else {
        (fd === 2 ? console.error : console.log)(text.replace(/\n$/, ""));
      }
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
      view().setUint32(countPtr, envBytes.length, true);
      view().setUint32(
        bufSizePtr,
        envBytes.reduce((a, b) => a + b.length, 0),
        true,
      );
      return 0;
    },
    environ_get(environPtr, bufPtr) {
      const dv = view();
      const mem = new Uint8Array(getMemory().buffer);
      let p = bufPtr;
      envBytes.forEach((b, i) => {
        dv.setUint32(environPtr + 4 * i, p, true);
        mem.set(b, p);
        p += b.length;
      });
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
async function defaultSource(filename = WASM_FILENAME) {
  const url = new URL(filename, import.meta.url);
  if (url.protocol === "file:") {
    const [{ readFile }, { fileURLToPath }] = await Promise.all([
      import("node:fs/promises"),
      import("node:url"),
    ]);
    return await readFile(fileURLToPath(url));
  }
  return await fetch(url);
}

// ── wasm-threads machinery (inert when WASM_THREADS_FILENAME is null) ──
//
// The threaded module is a wasm32-wasip1-threads build: shared imported
// memory (`env.memory`), pthreads riding the wasi-threads ABI — the
// module imports `wasi.thread-spawn` and exports `wasi_thread_start`;
// this glue services thread-spawn by spawning a Web Worker (browser) /
// worker_threads Worker (node) ON THIS SAME MODULE FILE, which detects
// the worker role at load and runs the worker protocol below.
//
// The program's `_start` itself runs in a "primary" worker — never on
// the page's main thread — because every blocking primitive in the
// threaded runtime bottoms out in `memory.atomic.wait32`, which traps
// on non-blockable agents (the browser main thread). The classic
// PROXY_TO_PTHREAD model.

/** SAB + cross-origin isolation feature detection. node has SAB
 * unconditionally and no crossOriginIsolated — treat undefined as
 * isolated. */
function threadsSupported() {
  if (typeof SharedArrayBuffer === "undefined") return false;
  return globalThis.crossOriginIsolated ?? true;
}

// node:worker_threads module, resolved once per agent (main thread at
// runThreaded; workers at bootstrap) so thread-spawn can create
// siblings synchronously.
let nodeWorkerThreads = null;
// node:fs, resolved alongside — gives the threaded path's fd_write a
// synchronous stdout/stderr (see the polyfill). Stays null on the
// sequential path and in browsers.
let nodeFsSync = null;

/** Spawn a kara thread worker on this module file. `data` is the
 * worker-protocol record (structured-cloneable: WebAssembly.Module,
 * shared Memory, SAB tid counter). Synchronous — wasi.thread-spawn
 * must return the new tid without awaiting. */
function spawnKaraWorkerSync(data, opts = {}) {
  if (nodeWorkerThreads) {
    const w = new nodeWorkerThreads.Worker(new URL(import.meta.url), {
      workerData: data,
    });
    // pthread workers must not hold the node process open — the pool's
    // worker_loop never returns by design (parity with the native
    // daemon-thread pool). The primary stays ref'd: its exit message is
    // the program result.
    if (opts.unref) w.unref();
    return w;
  }
  // The #kara-thread-worker fragment is the worker-role marker: only a
  // module loaded under it consumes its first message as the protocol
  // record, so a user importing this glue inside their own worker is
  // never disturbed.
  const w = new Worker(new URL("#kara-thread-worker", import.meta.url), {
    type: "module",
  });
  w.postMessage(data);
  return w;
}

/** Worker-side protocol: instantiate the shared-memory module and
 * either run `_start` (primary) or `wasi_thread_start` (pthread). */
async function karaThreadWorkerMain(data, postMessageFn) {
  const { role, module, memory, tidCounter, env, tid, startArg } = data;
  const imports = {
    env: { memory },
    wasi: {
      "thread-spawn": (arg) => {
        const newTid = Atomics.add(new Int32Array(tidCounter), 0, 1);
        spawnKaraWorkerSync(
          { ...data, role: "pthread", tid: newTid, startArg: arg },
          { unref: true },
        );
        return newTid;
      },
    },
    wasi_snapshot_preview1: makeWasiPolyfill(() => memory, env),
  };
  const instance = await WebAssembly.instantiate(module, imports);
  if (role === "primary") {
    let code = 0;
    try {
      instance.exports._start();
    } catch (e) {
      if (e instanceof KaraExit) {
        code = e.code;
      } else {
        postMessageFn({
          __karaThreads: "error",
          message: String(e && e.stack ? e.stack : e),
        });
        return;
      }
    }
    postMessageFn({ __karaThreads: "exit", code });
  } else {
    instance.exports.wasi_thread_start(tid, startArg);
  }
}

/** Detect "this module was loaded as a kara thread worker" and run the
 * protocol. No-op on the main thread, in non-kara workers, and on
 * sequential-only builds. */
async function karaMaybeRunAsThreadWorker() {
  if (WASM_THREADS_FILENAME === null) return;
  const isNode =
    typeof process !== "undefined" && !!(process.versions && process.versions.node);
  if (isNode) {
    const wt = await import("node:worker_threads");
    if (wt.isMainThread) return;
    const data = wt.workerData;
    if (!data || data.__karaThreads !== true) return;
    nodeWorkerThreads = wt;
    nodeFsSync = await import("node:fs");
    await karaThreadWorkerMain(data, (m) => wt.parentPort.postMessage(m));
    return;
  }
  const isWorkerScope =
    typeof WorkerGlobalScope !== "undefined" &&
    globalThis instanceof WorkerGlobalScope;
  if (!isWorkerScope || self.location.hash !== "#kara-thread-worker") return;
  const data = await new Promise((resolve) => {
    globalThis.addEventListener("message", (e) => resolve(e.data), {
      once: true,
    });
  });
  if (!data || data.__karaThreads !== true) return;
  await karaThreadWorkerMain(data, (m) => globalThis.postMessage(m));
  // A finished pthread's worker has nothing left to do (pool workers
  // never reach here — worker_loop doesn't return).
  if (data.role === "pthread") globalThis.close();
}
await karaMaybeRunAsThreadWorker();

/** Main-thread side of the threaded run: compile the threaded module,
 * create the shared memory (limits mirror the module's import
 * declaration exactly), spawn the primary worker, await its exit. */
async function runThreaded() {
  if (DECLARED_IMPORTS.length > 0) {
    // karac rejects host fns + wasm-threads at build time (their
    // implementations are main-thread closures; the program runs in a
    // worker). Defensive — this glue shape is unreachable.
    throw new Error("host fns are not supported with wasm-threads");
  }
  const isNode =
    typeof process !== "undefined" && !!(process.versions && process.versions.node);
  if (isNode) {
    nodeWorkerThreads = await import("node:worker_threads");
    nodeFsSync = await import("node:fs");
  }
  const src = await defaultSource(WASM_THREADS_FILENAME);
  const bytes =
    typeof Response !== "undefined" && src instanceof Response
      ? await src.arrayBuffer()
      : src;
  const module = await WebAssembly.compile(bytes);
  const memory = new WebAssembly.Memory({
    initial: THREADS_MEM_INITIAL_PAGES,
    maximum: THREADS_MEM_MAX_PAGES,
    shared: true,
  });
  const tidCounter = new SharedArrayBuffer(4);
  new Int32Array(tidCounter)[0] = 1;
  const poolSize =
    THREADS_POOL_SIZE ??
    (globalThis.navigator && navigator.hardwareConcurrency) ??
    4;
  const env = ["KARAC_PAR_WORKERS=" + poolSize];
  const data = {
    __karaThreads: true,
    role: "primary",
    module,
    memory,
    tidCounter,
    env,
    tid: 0,
    startArg: 0,
  };
  const code = await new Promise((resolve, reject) => {
    const w = spawnKaraWorkerSync(data);
    const onMessage = (msg) => {
      if (msg && msg.__karaThreads === "exit") resolve(msg.code);
      else if (msg && msg.__karaThreads === "error")
        reject(new Error(msg.message));
    };
    if (nodeWorkerThreads) {
      w.on("message", onMessage);
      w.on("error", reject);
    } else {
      w.addEventListener("message", (e) => onMessage(e.data));
      w.addEventListener("error", (e) =>
        reject(e.error ?? new Error("kara thread worker error")),
      );
    }
  });
  if (code !== 0) throw new KaraExit(code);
  return { memory, threaded: true };
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
 *
 * On a wasm-threads build this picks the THREADED module when
 * SharedArrayBuffer + cross-origin isolation are available (running
 * `_start` in a primary worker; the resolved handle then carries only
 * `{ memory, threaded: true }` — the instance lives in the worker).
 * Otherwise it console.warns and falls back to the sequential module —
 * or throws, when the build's manifest set `[wasm] fallback = false`.
 * `opts.forceSequential` skips the pick (useful for A/B runs and
 * tests); `opts.module` / `opts.bytes` only ever feed the sequential
 * path.
 */
export async function run(hostImpls = {}, opts = {}) {
  if (WASM_THREADS_FILENAME !== null && !opts.forceSequential) {
    if (threadsSupported()) {
      return await runThreaded();
    }
    if (THREADS_NO_FALLBACK) {
      throw new Error(
        "karac: this build requires wasm-threads (SharedArrayBuffer + " +
          "cross-origin isolation) but they are unavailable. Serve with " +
          "COOP/COEP headers (Cross-Origin-Opener-Policy: same-origin; " +
          "Cross-Origin-Embedder-Policy: require-corp). Sequential " +
          "fallback was disabled by `[wasm] fallback = false` in kara.toml.",
      );
    }
    console.warn(
      "karac: SharedArrayBuffer/cross-origin isolation unavailable; " +
        "falling back to the sequential (single-threaded) module",
    );
  }
  const handle = await instantiate(hostImpls, opts);
  try {
    handle.exports._start();
  } catch (e) {
    if (!(e instanceof KaraExit) || e.code !== 0) throw e;
  }
  return handle;
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(name: &str, params: Vec<HostParam>, ret: Option<(String, JsScalar)>) -> HostFnSig {
        HostFnSig {
            name: name.to_string(),
            params,
            ret,
        }
    }

    fn param(name: &str, kara_ty: &str, js: JsScalar) -> HostParam {
        HostParam {
            name: name.to_string(),
            kara_ty: kara_ty.to_string(),
            js,
        }
    }

    #[test]
    fn dts_types_host_fns_per_boundary_contract() {
        let fns = vec![
            sig(
                "report",
                vec![param("value", "i64", JsScalar::BigInt)],
                Some(("i64".to_string(), JsScalar::BigInt)),
            ),
            sig(
                "log_str",
                vec![
                    param("ptr", "*const u8", JsScalar::Number),
                    param("len", "i64", JsScalar::BigInt),
                ],
                None,
            ),
        ];
        let dts = render_dts(&fns, "app.wasm", false);
        // i64 params/returns are bigint; pointers are numbers; unit
        // returns are void; every impl takes the trailing HostCtx.
        assert!(dts.contains("report(value: bigint, ctx: HostCtx): bigint;"));
        assert!(dts.contains("log_str(ptr: number, len: bigint, ctx: HostCtx): void;"));
        // Kāra-surface signature is preserved as the doc line.
        assert!(dts.contains("`host fn report(value: i64) -> i64`"));
        // Declared host fns make the hostImpls parameter required.
        assert!(dts.contains("hostImpls: HostImpls"));
        assert!(!dts.contains("hostImpls?: HostImpls"));
    }

    #[test]
    fn dts_with_no_host_fns_makes_host_impls_optional() {
        let dts = render_dts(&[], "plain.wasm", false);
        assert!(dts.contains("export interface HostImpls {}"));
        assert!(dts.contains("hostImpls?: HostImpls"));
        // The glue module's own surface is always declared.
        for decl in [
            "export interface HostCtx",
            "export interface InstantiateOpts",
            "export interface KaraHandle",
            "export function readString",
            "export class KaraExit",
            "export function instantiate",
            "export function run",
        ] {
            assert!(dts.contains(decl), "missing declaration: {decl}");
        }
        // Sequential-only declarations carry none of the threaded surface.
        assert!(!dts.contains("KaraThreadedHandle"));
        assert!(!dts.contains("forceSequential"));
    }

    #[test]
    fn threaded_glue_renders_pick_constants_and_machinery() {
        let cfg = WasmThreadsGlueConfig {
            threads_filename: "app.threads.wasm".to_string(),
            no_fallback: false,
            pool_size_override: Some(6),
            mem_initial_pages: 18,
            mem_max_pages: 16384,
        };
        let glue = render_glue(&[], "app.wasm", Some(&cfg));
        for needle in [
            "const WASM_THREADS_FILENAME = \"app.threads.wasm\";",
            "const THREADS_NO_FALLBACK = false;",
            "const THREADS_POOL_SIZE = 6;",
            "const THREADS_MEM_INITIAL_PAGES = 18;",
            "const THREADS_MEM_MAX_PAGES = 16384;",
            // Machinery + protocol pieces the threaded path depends on.
            "function threadsSupported()",
            "\"thread-spawn\"",
            "wasi_thread_start",
            "#kara-thread-worker",
            "KARAC_PAR_WORKERS=",
            "forceSequential",
        ] {
            assert!(glue.contains(needle), "missing in threaded glue: {needle}");
        }
        // Sequential-only build renders the constants inert (the static
        // body references them unconditionally).
        let seq = render_glue(&[], "app.wasm", None);
        assert!(seq.contains("const WASM_THREADS_FILENAME = null;"));
        assert!(seq.contains("const THREADS_POOL_SIZE = null;"));
    }

    #[test]
    fn threaded_dts_declares_threaded_surface() {
        let dts = render_dts(&[], "app.wasm", true);
        assert!(dts.contains("export interface KaraThreadedHandle"));
        assert!(dts.contains("forceSequential?: boolean;"));
        assert!(dts.contains("Promise<KaraHandle | KaraThreadedHandle>"));
        // instantiate() stays sequential-typed.
        assert!(dts.contains("): Promise<KaraHandle>;"));
    }

    #[test]
    fn imported_memory_limits_parses_shared_memory_import() {
        // Hand-assembled minimal module: magic+version, then an import
        // section with exactly (import "env" "memory" (memory 17 16384
        // shared)) — flags 0x03 = has-max | shared; 16384 = LEB 0x80 0x80
        // 0x01.
        let mut m = Vec::new();
        m.extend_from_slice(b"\0asm");
        m.extend_from_slice(&[1, 0, 0, 0]);
        let body: &[u8] = &[
            0x01, // one import
            0x03, b'e', b'n', b'v', // module "env"
            0x06, b'm', b'e', b'm', b'o', b'r', b'y', // name "memory"
            0x02, // kind: memory
            0x03, // limits flags: has-max | shared
            0x11, // min = 17
            0x80, 0x80, 0x01, // max = 16384
        ];
        m.push(0x02); // import section id
        m.push(body.len() as u8);
        m.extend_from_slice(body);
        assert_eq!(imported_memory_limits(&m), Some((17, 16384)));
    }

    #[test]
    fn imported_memory_limits_skips_non_memory_imports_and_handles_absence() {
        // A function import before the memory import must be skipped
        // correctly (typeidx consumed), and a module with no env.memory
        // import returns None.
        let mut m = Vec::new();
        m.extend_from_slice(b"\0asm");
        m.extend_from_slice(&[1, 0, 0, 0]);
        let body: &[u8] = &[
            0x02, // two imports
            0x04, b'w', b'a', b's', b'i', // module "wasi"
            0x0c, b't', b'h', b'r', b'e', b'a', b'd', b'-', b's', b'p', b'a', b'w',
            b'n', // name "thread-spawn"
            0x00, // kind: function
            0x05, // typeidx 5
            0x03, b'e', b'n', b'v', // module "env"
            0x06, b'm', b'e', b'm', b'o', b'r', b'y', // name "memory"
            0x02, // kind: memory
            0x01, // limits flags: has-max (non-shared)
            0x02, // min = 2
            0x0a, // max = 10
        ];
        m.push(0x02);
        m.push(body.len() as u8);
        m.extend_from_slice(body);
        assert_eq!(imported_memory_limits(&m), Some((2, 10)));

        // No import section at all → None.
        assert_eq!(imported_memory_limits(b"\0asm\x01\x00\x00\x00"), None);
        // Truncated/garbage input → None, never a panic.
        assert_eq!(imported_memory_limits(b"\0asm"), None);
        assert_eq!(imported_memory_limits(b"not wasm at all"), None);
    }
}
