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
    let handle_widths = handle_width_map(program);

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

/// Single-primitive-field structs in `program`, mapping each name to its
/// field's JS-boundary classification — the opaque-handle width table
/// shared by [`collect_host_fns`] and wasm-export discovery
/// (`crate::wasm_exports`). A one-field struct crosses the boundary as
/// its field's scalar.
pub(crate) fn handle_width_map(program: &Program) -> HashMap<&str, JsScalar> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(s) if s.fields.len() == 1 => {
                Some((s.name.as_str(), js_scalar(&s.fields[0].ty, &HashMap::new())))
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
pub(crate) fn js_scalar(ty: &TypeExpr, handles: &HashMap<&str, JsScalar>) -> JsScalar {
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
pub(crate) fn type_expr_display(ty: &TypeExpr) -> String {
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
/// Emit the JS marshalling-descriptor literal for one export param/return
/// type (phase-10 WASM entry-point discovery, sub-slice D.4). The static
/// glue body's `karaLift`/`karaLowerParam` interpret it. Tags: 0 scalar,
/// 1 record, 2 option, 3 result, 4 string, 5 list. Offsets are the
/// canonical-ABI layout (shared `wasm_exports::scalar_size_align` /
/// `variant_layout`), so they agree with the codegen trampoline.
fn js_shape(ty: &crate::wasm_exports::ExportType) -> String {
    use crate::wasm_exports::{scalar_size_align, variant_layout, VariantShape};
    if let Some(fields) = &ty.record_fields {
        // Natural-alignment record layout (matches the LLVM struct the
        // trampoline writes for scalar fields).
        let mut off = 0u64;
        let mut max_align = 1u32;
        let parts = fields
            .iter()
            .map(|f| {
                let (sz, al) = scalar_size_align(&f.kara_ty);
                off = off.next_multiple_of(al as u64);
                let here = off;
                off += sz;
                max_align = max_align.max(al);
                format!("{{ n: \"{}\", o: {here}, ty: \"{}\" }}", f.name, f.kara_ty)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _size = off.next_multiple_of(max_align as u64);
        format!("{{ k: 1, f: [{parts}] }}")
    } else if let Some(v) = &ty.variant {
        match v {
            VariantShape::Option(t) => {
                let (sz, al) = scalar_size_align(&t.kara_ty);
                let (po, _, _) = variant_layout(sz, al);
                format!("{{ k: 2, po: {po}, p: {} }}", js_shape(t))
            }
            VariantShape::Result(t, e) => {
                let (ts, ta) = scalar_size_align(&t.kara_ty);
                let (es, ea) = scalar_size_align(&e.kara_ty);
                let (po, _, _) = variant_layout(ts.max(es), ta.max(ea));
                format!(
                    "{{ k: 3, po: {po}, ok: {}, err: {} }}",
                    js_shape(t),
                    js_shape(e)
                )
            }
        }
    } else if ty.is_string() {
        "{ k: 4 }".to_string()
    } else if let Some(elem) = &ty.list_elem {
        let (es, _) = scalar_size_align(&elem.kara_ty);
        format!("{{ k: 5, es: {es}, e: {} }}", js_shape(elem))
    } else {
        // Scalar (primitive / opaque handle). `ty.kara_ty` is the Kāra
        // scalar name the JS reader/writer keys off.
        format!("{{ k: 0, ty: \"{}\" }}", ty.kara_ty)
    }
}

pub fn render_glue(
    fns: &[HostFnSig],
    exports: &[crate::wasm_exports::ExportSig],
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
    // Per-fn JS-scalar marshalling table for the threaded host-fn
    // worker→main proxy (`makeHostProxy` / `startHostService` in the
    // static body): each entry says how to read each scalar arg and the
    // return out of the shared control block (a `bigint` slot vs a
    // `number` slot). Always emitted (`[]` for a host-fn-free program)
    // so the static body references it unconditionally; only the
    // threaded path with declared host fns actually consults it. The
    // sequential `kara_host` import wiring (`buildImports`) calls the
    // user closures directly and needs no table.
    let sigs = fns
        .iter()
        .map(|s| {
            let params = s
                .params
                .iter()
                .map(|p| match p.js {
                    JsScalar::BigInt => "\"bigint\"",
                    JsScalar::Number => "\"number\"",
                })
                .collect::<Vec<_>>()
                .join(", ");
            let ret = match &s.ret {
                Some((_, JsScalar::BigInt)) => "\"bigint\"",
                Some((_, JsScalar::Number)) => "\"number\"",
                None => "\"void\"",
            };
            format!("{{ name: \"{}\", params: [{params}], ret: {ret} }}", s.name)
        })
        .collect::<Vec<_>>();
    // Compiler-emitted builtin host fns (phase-10 host-async producers).
    // These are NOT user `host fn`s — they never appear in DECLARED_IMPORTS
    // (the user contract / missing-impl check) or the `HostImpls` d.ts; the
    // glue supplies their implementation itself. They DO ride the same
    // worker→main proxy machinery (a producer call originates in the
    // primary worker), so they are appended to HOST_FN_SIGS — the proxy and
    // service loop key off this array. Only emitted on threaded builds: a
    // sequential WASM target can't host them (the codegen gate rejects the
    // producer pre-link), and a native/sequential build never wires a
    // service instance. `__kara_timer_after(chPtr: i64, ms: i64) -> ()`
    // backs `std.web.time.after`; `__kara_timer_every(chPtr: i64, ms: i64) ->
    // ()` backs `std.web.time.every` (a multi-shot `setInterval` loop);
    // `__kara_animation_frames(chPtr: i64) -> ()`
    // backs `std.web.time.animation_frames` (a multi-shot rAF loop);
    // `__kara_pointer_moves(chPtr: i64) -> ()` backs
    // `std.web.events.pointer_moves` — the first non-unit producer: it sends a
    // `PointerEvent` payload (not a `()` tick) across the service instance;
    // `__kara_wheel(chPtr: i64) -> ()` backs `std.web.events.wheel` (same
    // non-unit spine, a 32-byte `WheelEvent` payload); `__kara_keydown(chPtr:
    // i64) -> ()` backs `std.web.events.keydown` (an 8-byte `KeyEvent` payload);
    // `__kara_keyup(chPtr: i64) -> ()` backs `std.web.events.keyup` (the
    // key-release sibling, the same 8-byte `KeyEvent` payload); `__kara_clicks(
    // chPtr: i64) -> ()` backs `std.web.events.clicks` (the discrete
    // click-position sibling of `pointer_moves`, a 16-byte `ClickEvent` payload);
    // `__kara_dblclick(chPtr: i64) -> ()` backs `std.web.events.dblclick` (the
    // double-press sibling of `clicks`, the same 16-byte `ClickEvent` payload);
    // `__kara_resize(chPtr: i64) -> ()` backs `std.web.events.resize` (a 16-byte
    // `ResizeEvent` of two `i64`s read off the window's `innerWidth`/`Height`);
    // `__kara_contextmenu(chPtr: i64) -> ()` backs `std.web.events.contextmenu`
    // (the right-click sibling of `clicks`, the same 16-byte `ClickEvent`
    // payload; its listener preventDefaults the native menu); `__kara_focus(
    // chPtr: i64) -> ()` / `__kara_blur(chPtr: i64) -> ()` back
    // `std.web.events.focus` / `.blur` — the first UNIT-payload `events.*`
    // producers (a 0-byte `()` token per focus/blur edge, no event-scratch).
    let builtin_sigs: &[&str] = if threads.is_some() {
        &[
            "{ name: \"__kara_timer_after\", params: [\"bigint\", \"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_timer_every\", params: [\"bigint\", \"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_animation_frames\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_pointer_moves\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_wheel\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_keydown\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_keyup\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_clicks\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_dblclick\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_resize\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_contextmenu\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_focus\", params: [\"bigint\"], ret: \"void\" }",
            "{ name: \"__kara_blur\", params: [\"bigint\"], ret: \"void\" }",
        ]
    } else {
        &[]
    };
    let builtin_names: &[&str] = if threads.is_some() {
        &[
            "\"__kara_timer_after\"",
            "\"__kara_timer_every\"",
            "\"__kara_animation_frames\"",
            "\"__kara_pointer_moves\"",
            "\"__kara_wheel\"",
            "\"__kara_keydown\"",
            "\"__kara_keyup\"",
            "\"__kara_clicks\"",
            "\"__kara_dblclick\"",
            "\"__kara_resize\"",
            "\"__kara_contextmenu\"",
            "\"__kara_focus\"",
            "\"__kara_blur\"",
        ]
    } else {
        &[]
    };
    let all_sigs = sigs
        .iter()
        .map(String::as_str)
        .chain(builtin_sigs.iter().copied())
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("const HOST_FN_SIGS = [{all_sigs}];\n"));
    out.push_str(&format!(
        "const BUILTIN_HOST_FNS = [{}];\n",
        builtin_names.join(", ")
    ));
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

    // Per-export marshalling descriptors (phase-10 WASM entry-point
    // discovery, sub-slice D.4). Only RICH exports (record / option /
    // result / string / list params or returns — `needs_trampoline`)
    // appear: they are exported as canonical-ABI trampolines, so the glue
    // wraps them to marshal JS values ↔ the canonical layout
    // (`karaBuildExports` in the static body). Scalar exports pass through
    // `instance.exports` unwrapped. `[]` when there are none.
    let export_descs = exports
        .iter()
        .filter(|e| e.component_lowerable() && e.needs_trampoline())
        .map(|e| {
            let params = e
                .params
                .iter()
                .map(|p| js_shape(&p.ty))
                .collect::<Vec<_>>()
                .join(", ");
            let ret = match &e.ret {
                Some(t) => js_shape(t),
                None => "null".to_string(),
            };
            format!("{{ name: \"{}\", params: [{params}], ret: {ret} }}", e.name)
        })
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!("const KARA_EXPORTS = [{export_descs}];\n"));

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

/// TypeScript type for an export param/return — the JS shape the glue
/// marshaller (`karaLift`/`karaLowerParam`) produces/consumes: scalars
/// are `number`/`bigint`; a record is an object type; `Option[T]` is
/// `T | null`; `Result[T,E]` is `{ ok: T } | { err: E }`; `String` is
/// `string`; `Vec[T]` is `T[]`.
fn ts_export_type(ty: &crate::wasm_exports::ExportType) -> String {
    use crate::wasm_exports::VariantShape;
    if let Some(fields) = &ty.record_fields {
        let body = fields
            .iter()
            .map(|f| format!("{}: {}", f.name, ts_type(f.js)))
            .collect::<Vec<_>>()
            .join("; ");
        format!("{{ {body} }}")
    } else if let Some(v) = &ty.variant {
        match v {
            VariantShape::Option(t) => format!("{} | null", ts_export_type(t)),
            VariantShape::Result(t, e) => {
                format!(
                    "{{ ok: {} }} | {{ err: {} }}",
                    ts_export_type(t),
                    ts_export_type(e)
                )
            }
        }
    } else if ty.is_string() {
        "string".to_string()
    } else if let Some(elem) = &ty.list_elem {
        format!("{}[]", ts_export_type(elem))
    } else {
        ts_type(ty.js).to_string()
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
/// Discovered WASM entry-point exports (phase-10 "WASM entry-point
/// discovery") are typed on the handle's `exports` via a `KaraExports`
/// interface — `instantiate()` returns a handle whose `exports.<name>(…)`
/// carries the per-export signature. Sub-slice B covers the **scalar**
/// surface (primitives / single-field opaque handles cross as bare
/// `number`/`bigint`); rich shapes (`Result`/`Option`/structs via the
/// export trampoline) extend `ts_export_type` when sub-slice D lands, so
/// only `all_scalar` exports are typed here today.
///
/// `threaded` mirrors [`render_glue`]'s `threads.is_some()`: a
/// wasm-threads build's declarations additionally carry
/// `KaraThreadedHandle` (what `run()` resolves with when the threaded
/// module was picked — the instance lives in the primary worker, so
/// only the shared memory crosses back) and the widened `run()` return
/// type; a sequential-only build's declarations are unchanged.
pub fn render_dts(
    fns: &[HostFnSig],
    exports: &[crate::wasm_exports::ExportSig],
    wasm_filename: &str,
    threaded: bool,
) -> String {
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

    // Per-export typed surface (phase-10 WASM entry-point discovery).
    // Every export the glue can marshal (`component_lowerable`) is typed:
    // scalars cross as `number`/`bigint`; records as object types,
    // `Option[T]` as `T | null`, `Result[T,E]` as `{ ok: T } | { err: E }`,
    // `String` as `string`, `Vec[T]` as `T[]` — see [`ts_export_type`].
    out.push_str("export interface KaraExports {\n  _start(): void;\n");
    for e in exports.iter().filter(|e| e.component_lowerable()) {
        let params = e
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, ts_export_type(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = match &e.ret {
            Some(t) => ts_export_type(t),
            None => "void".to_string(),
        };
        let _ = writeln!(out, "  {}({params}): {ret};", e.name);
    }
    out.push_str("}\n\n");

    out.push_str(
        "export interface KaraHandle {\n\
         \x20 instance: WebAssembly.Instance;\n\
         \x20 exports: WebAssembly.Exports & KaraExports;\n\
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
/** Decode a (ptr, len) UTF-8 string out of the module's linear memory.
 * `.slice()` copies into a fresh non-shared buffer first: on the threaded
 * (`--features wasm-threads`) build `memory.buffer` is a SharedArrayBuffer, and
 * browser `TextDecoder.decode` rejects shared views ("The provided
 * ArrayBufferView value must not be shared."). This helper is the single
 * string-decode funnel — it backs `ctx.readString` in the main-thread
 * host-service loop (a `host fn` taking a `string` arg) and the rich
 * string-export lift — so the copy must live here, not just at the fd_write
 * call site. B-2026-06-14-22 follow-up: 69c49ec0 fixed fd_write + random_get
 * but missed this helper; node's lenient TextDecoder hid it. Harmless (a copy)
 * on the sequential path where the buffer is already non-shared. */
export function readString(memory, ptr, len) {
  return new TextDecoder("utf-8").decode(
    new Uint8Array(memory.buffer, Number(ptr), Number(len)).slice(),
  );
}

// ── Rich-export marshalling (phase-10 WASM entry-point discovery) ──────
// KARA_EXPORTS (emitted above) describes each record/option/result/
// string/list export; the entries are interpreted here to marshal JS
// values across the canonical-ABI trampolines. Shape tags: 0 scalar,
// 1 record, 2 option, 3 result, 4 string, 5 list.

function karaScalarRead(dv, off, ty) {
  switch (ty) {
    case "i8": return dv.getInt8(off);
    case "u8": case "bool": return dv.getUint8(off);
    case "i16": return dv.getInt16(off, true);
    case "u16": return dv.getUint16(off, true);
    case "i32": case "char": return dv.getInt32(off, true);
    case "u32": return dv.getUint32(off, true);
    case "f32": return dv.getFloat32(off, true);
    case "f64": return dv.getFloat64(off, true);
    case "i64": case "isize": return dv.getBigInt64(off, true);
    case "u64": case "usize": return dv.getBigUint64(off, true);
    default: return dv.getInt32(off, true);
  }
}

function karaScalarWrite(dv, off, ty, v) {
  switch (ty) {
    case "i8": dv.setInt8(off, v); break;
    case "u8": case "bool": dv.setUint8(off, v); break;
    case "i16": dv.setInt16(off, v, true); break;
    case "u16": dv.setUint16(off, v, true); break;
    case "i32": case "char": dv.setInt32(off, v, true); break;
    case "u32": dv.setUint32(off, v, true); break;
    case "f32": dv.setFloat32(off, v, true); break;
    case "f64": dv.setFloat64(off, v, true); break;
    case "i64": case "isize": dv.setBigInt64(off, BigInt(v), true); break;
    case "u64": case "usize": dv.setBigUint64(off, BigInt(v), true); break;
    default: dv.setInt32(off, v, true); break;
  }
}

// Lift a canonical value at `ptr` (a return-area / inner pointer) into a
// JS value per `shape`.
function karaLift(memory, shape, ptr) {
  const dv = new DataView(memory.buffer);
  switch (shape.k) {
    case 0: return karaScalarRead(dv, ptr, shape.ty);
    case 1: {
      const o = {};
      for (const f of shape.f) o[f.n] = karaScalarRead(dv, ptr + f.o, f.ty);
      return o;
    }
    case 2:
      return dv.getUint8(ptr) === 1 ? karaLift(memory, shape.p, ptr + shape.po) : null;
    case 3:
      return dv.getUint8(ptr) === 0
        ? { ok: karaLift(memory, shape.ok, ptr + shape.po) }
        : { err: karaLift(memory, shape.err, ptr + shape.po) };
    case 4: {
      const sp = dv.getInt32(ptr, true), sl = dv.getInt32(ptr + 4, true);
      return readString(memory, sp, sl);
    }
    case 5: {
      const lp = dv.getInt32(ptr, true), lc = dv.getInt32(ptr + 4, true);
      const out = [];
      for (let i = 0; i < lc; i++) out.push(karaLift(memory, shape.e, lp + i * shape.es));
      return out;
    }
  }
}

// Lower a JS value to the trampoline's canonical flat params, pushing
// onto `flats`. `alloc(size, align)` reserves guest memory (cabi_realloc).
function karaLowerParam(memory, alloc, shape, val, flats) {
  switch (shape.k) {
    case 0:
      flats.push(val);
      break;
    case 1: // record → flattened field values, in declaration order
      for (const f of shape.f) flats.push(val[f.n]);
      break;
    case 4: { // string → (ptr, len)
      const bytes = new TextEncoder().encode(val);
      const p = alloc(bytes.length, 1);
      new Uint8Array(memory.buffer, p, bytes.length).set(bytes);
      flats.push(p, bytes.length);
      break;
    }
    case 5: { // list → (ptr, count)
      const n = val.length;
      const p = alloc(n * shape.es, shape.es);
      const dv = new DataView(memory.buffer);
      for (let i = 0; i < n; i++) karaScalarWrite(dv, p + i * shape.es, shape.e.ty, val[i]);
      flats.push(p, n);
      break;
    }
    default:
      throw new Error("kara: unsupported export param shape " + shape.k);
  }
}

// Build the handle's `exports`: rich exports (in KARA_EXPORTS) are wrapped
// to marshal JS values; everything else passes through `instance.exports`.
function karaBuildExports(instance) {
  const raw = instance.exports;
  const memory = raw.memory;
  // `instance.exports` is frozen (its props are read-only), so copy into
  // a writable object before overriding the rich exports with wrappers.
  const wrapped = Object.assign({}, raw);
  for (const sig of KARA_EXPORTS) {
    const fn = raw[sig.name];
    wrapped[sig.name] = (...args) => {
      const flats = [];
      const alloc = (sz, al) => raw.cabi_realloc(0, 0, al, sz);
      for (let i = 0; i < sig.params.length; i++) {
        karaLowerParam(memory, alloc, sig.params[i], args[i], flats);
      }
      const r = fn(...flats);
      if (sig.ret == null) return undefined;
      // A scalar return comes back by value; a record/option/result/
      // string/list return comes back as a canonical return-area pointer.
      return sig.ret.k === 0 ? r : karaLift(memory, sig.ret, r);
    };
  }
  return wrapped;
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
        // `.slice()` copies into a fresh non-shared ArrayBuffer: on the
        // threaded build `getMemory().buffer` is a SharedArrayBuffer, and
        // TextDecoder.decode rejects a shared-backed view ("The provided
        // ArrayBufferView value must not be shared."). Any stdout/stderr
        // write from a threaded browser program — a print, a panic, the
        // alloc-error abort path — hit this without the copy (B-2026-06-14-22).
        text += new TextDecoder("utf-8").decode(
          new Uint8Array(getMemory().buffer, ptr, len).slice(),
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
      // crypto.getRandomValues also rejects a SharedArrayBuffer-backed
      // view (threaded build), so fill a fresh non-shared buffer and copy
      // the bytes into linear memory.
      const tmp = new Uint8Array(len);
      globalThis.crypto.getRandomValues(tmp);
      new Uint8Array(getMemory().buffer, ptr, len).set(tmp);
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

// ── pthread-spawn worker→main proxy (browser only) ──
//
// In browsers, a nested dedicated worker's module script is fetched on its
// CREATING agent's event loop. A wasm pthread that spawns a sibling then
// immediately blocks (`memory.atomic.wait32` in `join`/`recv`) never turns
// its loop again, so the sibling's script never loads — it deadlocks. The
// fix mirrors Emscripten's PROXY_TO_PTHREAD: every worker routes
// `thread-spawn` to the MAIN thread (the only always-live agent), which
// creates the Worker. `thread-spawn` still returns the new tid
// synchronously (the wasi-threads ABI requires it); only the Worker
// construction is deferred to the main thread. node's worker_threads has
// no such constraint, so it keeps spawning siblings directly.
const SP_LOCK = 0; // i32: mutex over the ring write side
const SP_SEQ = 1; // i32: doorbell — bumped per enqueue
const SP_WRITE = 2; // i32: ring write cursor (producer, under SP_LOCK)
const SP_RING_OFF = 8; // i32 index where the (tid,startArg) ring begins
const SP_RING_CAP = 1024; // max in-flight spawn requests (pairs)
const SPAWN_CTL_INTS = SP_RING_OFF + SP_RING_CAP * 2;
const SPAWN_CTL_BYTES = SPAWN_CTL_INTS * 4;

/** Worker side: enqueue a {tid, startArg} spawn request for the main
 * thread and ring its doorbell. Never blocks on the spawn completing —
 * the new thread runs `wasi_thread_start(tid, startArg)` once the main
 * thread has created its Worker. */
function requestMainThreadSpawn(spawnCtl, tid, startArg) {
  const c = new Int32Array(spawnCtl);
  while (Atomics.compareExchange(c, SP_LOCK, 0, 1) !== 0) {
    Atomics.wait(c, SP_LOCK, 1);
  }
  try {
    const wcur = Atomics.load(c, SP_WRITE);
    const slot = SP_RING_OFF + wcur * 2;
    Atomics.store(c, slot, tid | 0);
    Atomics.store(c, slot + 1, startArg | 0);
    Atomics.store(c, SP_WRITE, (wcur + 1) % SP_RING_CAP);
    Atomics.add(c, SP_SEQ, 1);
    Atomics.notify(c, SP_SEQ);
  } finally {
    Atomics.store(c, SP_LOCK, 0);
    Atomics.notify(c, SP_LOCK, 1);
  }
}

/** Main side: drain spawn requests and create each pthread Worker on the
 * main thread's (always-live) event loop. `baseData` is the primary's
 * protocol record; each spawn overrides role/tid/startArg. Returns a
 * handle whose stop() ends the loop. */
function startSpawnService(spawnCtl, baseData) {
  const c = new Int32Array(spawnCtl);
  let read = 0;
  let last = Atomics.load(c, SP_SEQ);
  let running = true;
  const hasWaitAsync = typeof Atomics.waitAsync === "function";
  function drain() {
    const write = Atomics.load(c, SP_WRITE);
    while (read !== write) {
      const slot = SP_RING_OFF + read * 2;
      const tid = Atomics.load(c, slot);
      const startArg = Atomics.load(c, slot + 1) >>> 0;
      read = (read + 1) % SP_RING_CAP;
      spawnKaraWorkerSync(
        { ...baseData, role: "pthread", tid, startArg },
        { unref: true },
      );
    }
  }
  const done = (async () => {
    for (;;) {
      let cur = Atomics.load(c, SP_SEQ);
      if (cur === last) {
        if (hasWaitAsync) {
          const w = Atomics.waitAsync(c, SP_SEQ, last);
          if (w.async) await w.value;
        } else {
          await new Promise((r) => setTimeout(r, 0));
        }
        cur = Atomics.load(c, SP_SEQ);
      }
      if (!running) break;
      last = cur;
      drain();
    }
  })();
  return {
    stop() {
      running = false;
      Atomics.add(c, SP_SEQ, 1);
      Atomics.notify(c, SP_SEQ);
      return done;
    },
  };
}

// ── host-fn worker→main proxy (threaded builds with host fns) ──
//
// The program runs in a worker (PROXY_TO_PTHREAD), but `host fn`
// implementations are user-supplied MAIN-THREAD closures. A worker that
// calls a host fn must round-trip to the main thread synchronously: it
// publishes the call into a shared control block, wakes the main
// thread, then `Atomics.wait`s for the result. The main thread is the
// only blockable-by-design agent here (its service loop yields to the
// event loop via Atomics.waitAsync), so it executes the closure and
// notifies. One SharedArrayBuffer holds: a mutex (only one host call in
// flight — the main thread services serially), a request doorbell the
// main thread waits on, a done flag, and an 8-byte-slot payload region
// for scalar args + the return. Strings cross as (ptr, len) scalars and
// live in the shared linear memory — the main side reads them directly.
const HC_LOCK = 0; // i32: mutex (0 free, 1 held)
const HC_SEQ = 1; // i32: request counter / doorbell
const HC_DONE = 2; // i32: result-ready flag for the current request
const HC_FN = 3; // i32: HOST_FN_SIGS index of the call
const HC_ARGC = 4; // i32: argument count
const HC_PAYLOAD_OFF = 64; // 8-aligned; clears the i32 control cells
const HC_MAX_ARGS = 16; // scalar-arg ceiling (host fns take a handful)
const HC_RET_SLOT = HC_MAX_ARGS; // return value lives just past the args
const HOST_CTL_BYTES = HC_PAYLOAD_OFF + (HC_MAX_ARGS + 1) * 8;

/** Worker side: a `kara_host` import namespace whose entries marshal
 * the call across the shared control block and block until the main
 * thread writes the result. Built per worker (primary and pthreads),
 * all bound to the same shared `hostCtl`. */
function makeHostProxy(hostCtl) {
  const c = new Int32Array(hostCtl);
  const f = new Float64Array(hostCtl, HC_PAYLOAD_OFF);
  const b = new BigInt64Array(hostCtl, HC_PAYLOAD_OFF);
  const kara_host = {};
  for (let idx = 0; idx < HOST_FN_SIGS.length; idx++) {
    const sig = HOST_FN_SIGS[idx];
    kara_host[sig.name] = (...args) => {
      // Acquire the host-call mutex (block on contention).
      while (Atomics.compareExchange(c, HC_LOCK, 0, 1) !== 0) {
        Atomics.wait(c, HC_LOCK, 1);
      }
      try {
        for (let k = 0; k < args.length; k++) {
          if (sig.params[k] === "bigint") b[k] = BigInt(args[k]);
          else f[k] = Number(args[k]);
        }
        Atomics.store(c, HC_FN, idx);
        Atomics.store(c, HC_ARGC, args.length);
        Atomics.store(c, HC_DONE, 0);
        // Publish (release) and ring the doorbell.
        Atomics.add(c, HC_SEQ, 1);
        Atomics.notify(c, HC_SEQ);
        // Block this worker until the main thread posts the result.
        while (Atomics.load(c, HC_DONE) === 0) {
          Atomics.wait(c, HC_DONE, 0);
        }
        if (sig.ret === "bigint") return b[HC_RET_SLOT];
        if (sig.ret === "number") return f[HC_RET_SLOT];
        return undefined;
      } finally {
        Atomics.store(c, HC_LOCK, 0);
        Atomics.notify(c, HC_LOCK, 1);
      }
    };
  }
  return kara_host;
}

/** Main side: service host-fn calls posted by workers. Runs on the
 * main thread's event loop (Atomics.waitAsync — the main agent may be
 * non-blockable), executing each user closure with the shared memory in
 * scope so (ptr, len) string args read directly. Returns a handle whose
 * stop() unblocks and ends the loop once the program has exited. */
function startHostService(hostCtl, memory, hostImpls) {
  const c = new Int32Array(hostCtl);
  const f = new Float64Array(hostCtl, HC_PAYLOAD_OFF);
  const b = new BigInt64Array(hostCtl, HC_PAYLOAD_OFF);
  const ctx = {
    get memory() {
      return memory;
    },
    readString: (ptr, len) => readString(memory, ptr, len),
  };
  let last = Atomics.load(c, HC_SEQ);
  let running = true;
  const hasWaitAsync = typeof Atomics.waitAsync === "function";
  function serveOne() {
    const idx = Atomics.load(c, HC_FN);
    const argc = Atomics.load(c, HC_ARGC);
    const sig = HOST_FN_SIGS[idx];
    const args = [];
    for (let k = 0; k < argc; k++) {
      args.push(sig.params[k] === "bigint" ? b[k] : f[k]);
    }
    let ret;
    try {
      ret = hostImpls[sig.name](...args, ctx);
    } catch (e) {
      // A throwing host impl must not deadlock the worker — log and
      // return a zero result so the round-trip completes.
      console.error("kara: host fn `" + sig.name + "` threw:", e);
      ret = undefined;
    }
    if (sig.ret === "bigint") b[HC_RET_SLOT] = BigInt(ret ?? 0n);
    else if (sig.ret === "number") f[HC_RET_SLOT] = Number(ret ?? 0);
    Atomics.store(c, HC_DONE, 1);
    Atomics.notify(c, HC_DONE);
  }
  const done = (async () => {
    for (;;) {
      let cur = Atomics.load(c, HC_SEQ);
      if (cur === last) {
        if (hasWaitAsync) {
          const w = Atomics.waitAsync(c, HC_SEQ, last);
          if (w.async) await w.value;
        } else {
          // Engines without waitAsync: poll, yielding to the event loop
          // (the main agent can't Atomics.wait).
          await new Promise((r) => setTimeout(r, 0));
        }
        cur = Atomics.load(c, HC_SEQ);
      }
      if (!running) break;
      if (cur === last) continue; // spurious wake
      last = cur;
      serveOne();
    }
  })();
  return {
    stop() {
      running = false;
      // Wake the loop so it observes !running and returns.
      Atomics.add(c, HC_SEQ, 1);
      Atomics.notify(c, HC_SEQ);
      return done;
    },
  };
}

/** Worker-side protocol: instantiate the shared-memory module and
 * either run `_start` (primary) or `wasi_thread_start` (pthread). */
async function karaThreadWorkerMain(data, postMessageFn) {
  const { role, module, memory, tidCounter, env, tid, startArg, hostCtl, spawnCtl } =
    data;
  const imports = {
    env: { memory },
    wasi: {
      "thread-spawn": (arg) => {
        const newTid = Atomics.add(new Int32Array(tidCounter), 0, 1);
        if (nodeWorkerThreads) {
          // node: nested worker_threads start independently of the
          // (blocked) parent — spawn the sibling directly.
          spawnKaraWorkerSync(
            { ...data, role: "pthread", tid: newTid, startArg: arg },
            { unref: true },
          );
        } else {
          // browser: defer Worker construction to the main thread, which
          // is the only agent whose event loop is guaranteed live (this
          // worker is about to block in `join`/`recv`).
          requestMainThreadSpawn(spawnCtl, newTid, arg);
        }
        return newTid;
      },
    },
    wasi_snapshot_preview1: makeWasiPolyfill(() => memory, env),
  };
  // host fns: proxy each call to the main thread's service loop. hostCtl
  // is null for host-fn-free programs (no proxy needed).
  if (hostCtl) {
    imports.kara_host = makeHostProxy(hostCtl);
  }
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
async function runThreaded(hostImpls = {}, opts = {}) {
  // host fn implementations are main-thread closures bridged into the
  // worker via the synchronous proxy below — validate them up front, the
  // same contract the sequential `buildImports` enforces.
  const missing = DECLARED_IMPORTS.filter(
    (n) => typeof hostImpls?.[n] !== "function",
  );
  if (missing.length > 0) {
    throw new Error(
      "missing host fn implementation(s): " + missing.join(", "),
    );
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
  // Builtin (compiler-emitted) host fns — phase-10 host-async producers
  // (`std.web.time.after`). These run on the main thread like user host
  // fns, but their implementation is supplied here, not by the caller, and
  // they operate over the shared linear memory through a SECOND "service"
  // instance of the same module. The service instance never runs
  // `_start`/`wasi_thread_start`; its only job is to call the channel
  // externs from a host event callback so a worker parked in `recv` wakes.
  let serviceInstance = null;
  const builtinHostImpls = {};
  if (BUILTIN_HOST_FNS.length > 0) {
    const serviceImports = {
      env: { memory },
      wasi: {
        "thread-spawn": () => {
          throw new Error("kara: service instance must not spawn threads");
        },
      },
      wasi_snapshot_preview1: makeWasiPolyfill(() => memory),
      // The service never executes program code, so it never calls a host
      // fn — but instantiation must still satisfy every kara_host import.
      kara_host: Object.fromEntries(
        HOST_FN_SIGS.map((s) => [
          s.name,
          () => {
            throw new Error("kara: service instance host fn unreachable: " + s.name);
          },
        ]),
      ),
    };
    serviceInstance = await WebAssembly.instantiate(module, serviceImports);
    // Retarget the service instance's shadow stack onto a dedicated buffer
    // so its channel_send frames can't clobber the primary worker's live
    // (parked-in-recv) frames — both default to the same linker stack.
    serviceInstance.exports.__stack_pointer.value =
      serviceInstance.exports.karac_runtime_service_stack_top();
    // __kara_timer_after(chPtr: i64 [BigInt], ms: i64 [BigInt]) -> (): the
    // channel pointer is a wasm32 address (fits a JS number); channel_send
    // takes (i32 ptr, i32 val_ptr, i64 elem_size) — a unit channel sends 0
    // bytes, so val_ptr=0, elem_size=0n. After the send we drop the sender
    // ref the producer cloned for us, closing the channel.
    builtinHostImpls["__kara_timer_after"] = (chPtr, ms) => {
      const ptr = Number(chPtr);
      setTimeout(() => {
        serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);
        serviceInstance.exports.karac_runtime_channel_drop_sender(ptr);
      }, Number(ms));
    };
    // __kara_timer_every(chPtr: i64 [BigInt], ms: i64 [BigInt]) -> (): a
    // MULTI-SHOT setInterval loop — the `after` arg-shape with the
    // animation_frames lifetime. Each interval, if the worker has drained the
    // previous tick (channel_pending == 0n), feed a fresh `()`. The pending
    // probe coalesces — the channel holds at most one un-consumed tick — so a
    // consumer slower than the period drops backlog rather than growing
    // unbounded lag. The cloned sender is owned by the interval for its
    // lifetime (never dropped, unlike __kara_timer_after's single fire).
    builtinHostImpls["__kara_timer_every"] = (chPtr, ms) => {
      const ptr = Number(chPtr);
      setInterval(() => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) === 0) {
          serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);
        }
      }, Number(ms));
    };
    // __kara_animation_frames(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT
    // requestAnimationFrame loop. Each frame, if the worker has drained the
    // previous tick (channel_pending == 0n), feed a fresh `()`; then re-arm.
    // The pending probe coalesces — the channel holds at most one un-consumed
    // frame token — so a slow consumer drops backlog rather than growing
    // unbounded lag. The cloned sender is owned by this loop for its lifetime
    // (never dropped: the render loop lives as long as the page). Outside a
    // browser (node tests) there is no requestAnimationFrame, so fall back to
    // a ~16ms setTimeout cadence — the channel semantics are identical.
    builtinHostImpls["__kara_animation_frames"] = (chPtr) => {
      const ptr = Number(chPtr);
      const raf = globalThis.requestAnimationFrame
        ? globalThis.requestAnimationFrame.bind(globalThis)
        : (cb) => setTimeout(cb, 16);
      const tick = () => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) === 0) {
          serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);
        }
        raf(tick);
      };
      raf(tick);
    };
    // __kara_pointer_moves(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT pointer
    // input stream — the first NON-UNIT host-async producer. Registers a
    // "pointermove" listener; each event marshals (clientX, clientY) as two
    // little-endian f64s into the service instance's reusable event-scratch
    // buffer in shared memory, then channel_send copies the 16-byte
    // PointerEvent payload into the queue (vs the 0-byte `()` a timer/frame
    // producer sends — `val_ptr=scratch, elem_size=16n`). Coalesces like
    // animation_frames: feed a fresh position only once the worker has drained
    // the previous one (channel_pending == 0n), so a burst of moves collapses
    // to a sample rather than growing unbounded backlog. The cloned sender is
    // owned by the listener for its lifetime (never dropped; the listener
    // lives as long as the page). Event source: `opts.pointerTarget` (a
    // browser passes the canvas element; a node test passes an EventTarget it
    // dispatches synthetic moves on), else `globalThis` if it can
    // addEventListener. No source → no-op (the channel never fills; recv
    // blocks, exactly as a tab receiving no pointer input).
    builtinHostImpls["__kara_pointer_moves"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.pointerTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // PointerEvent layout: { x: f64 @ 0, y: f64 @ 8, buttons: i64 @ 16 } = 24
      // bytes — kept in sync with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onMove = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 24);
        // Prefer element-relative coordinates (what a canvas listener wants)
        // and fall back to viewport-relative when a synthetic event omits them.
        dv.setFloat64(0, e.offsetX ?? e.clientX ?? 0, true);
        dv.setFloat64(8, e.offsetY ?? e.clientY ?? 0, true);
        // `MouseEvent.buttons` bitmask held during the move (lets the guest gate
        // on a held button for click-drag); 0 when a synthetic event omits it.
        dv.setBigInt64(16, BigInt(e.buttons ?? 0), true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 24n);
      };
      target.addEventListener("pointermove", onMove, { passive: true });
    };

    // __kara_wheel(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT wheel/scroll
    // stream, sibling of __kara_pointer_moves. Each "wheel" event marshals
    // (offsetX/clientX, offsetY/clientY, deltaX, deltaY) as four little-endian
    // f64s into the event-scratch buffer, then channel_send copies the 32-byte
    // WheelEvent payload into the queue (val_ptr=scratch, elem_size=32n).
    // Coalesces like pointer_moves (feed a fresh event only once the worker
    // drained the previous one). Registered `{ passive: true }` — the producer
    // reads the deltas but does NOT preventDefault, so it never silently
    // hijacks the page's scroll; a wheel-to-zoom demo suppresses scroll via
    // CSS (overscroll-behavior / touch-action) on its own element. Event
    // source: `opts.wheelTarget` (browser: the canvas; node test: an
    // EventTarget it dispatches synthetic wheels on), else `globalThis` if it
    // can addEventListener; no source → no-op (recv just blocks).
    builtinHostImpls["__kara_wheel"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.wheelTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // WheelEvent layout: { x @ 0, y @ 8, delta_x @ 16, delta_y @ 24 } = 32
      // bytes — kept in sync with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onWheel = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 32);
        dv.setFloat64(0, e.offsetX ?? e.clientX ?? 0, true);
        dv.setFloat64(8, e.offsetY ?? e.clientY ?? 0, true);
        dv.setFloat64(16, e.deltaX ?? 0, true);
        dv.setFloat64(24, e.deltaY ?? 0, true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 32n);
      };
      target.addEventListener("wheel", onWheel, { passive: true });
    };

    // __kara_keydown(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT keyboard stream,
    // sibling of __kara_pointer_moves / __kara_wheel. Each "keydown" event
    // marshals the numeric `keyCode` as one little-endian i64 into the
    // event-scratch buffer, then channel_send copies the 8-byte KeyEvent payload
    // (val_ptr=scratch, elem_size=8n). Coalesces like the others (feed a fresh
    // event only once the worker drained the previous one). Event source:
    // `opts.keyTarget` (browser: typically window/document — keydown bubbles, so
    // no element focus is needed; node test: an EventTarget it dispatches
    // synthetic keydowns on), else `globalThis` if it can addEventListener; no
    // source → no-op (recv just blocks). Not `{ passive }` (a key handler may
    // legitimately want to preventDefault), but this listener never does.
    builtinHostImpls["__kara_keydown"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.keyTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // KeyEvent layout: { key_code: i64 @ 0 } = 8 bytes — kept in sync with
      // runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onKey = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 8);
        dv.setBigInt64(0, BigInt(e.keyCode ?? e.which ?? 0), true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 8n);
      };
      target.addEventListener("keydown", onKey);
    };
    // __kara_keyup(chPtr: i64 [BigInt]) -> (): the key-RELEASE sibling of
    // __kara_keydown — identical marshalling (the same 8-byte KeyEvent
    // payload), only the DOM event differs ("keyup" vs "keydown"). The same
    // `opts.keyTarget` serves both (distinct event types never cross-fire), so
    // a program draining both keydown() and keyup() shares one source. The
    // main-thread event-scratch buffer is reused per event and copied out
    // synchronously by channel_send, so the two listeners never race over it.
    builtinHostImpls["__kara_keyup"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.keyTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // KeyEvent layout: { key_code: i64 @ 0 } = 8 bytes — kept in sync with
      // runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onKey = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 8);
        dv.setBigInt64(0, BigInt(e.keyCode ?? e.which ?? 0), true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 8n);
      };
      target.addEventListener("keyup", onKey);
    };
    // __kara_clicks(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT click-position
    // stream — the discrete "where did the user click" sibling of the continuous
    // __kara_pointer_moves. Registers a "click" listener; each event marshals
    // (offsetX/clientX, offsetY/clientY) as two little-endian f64s into the
    // event-scratch buffer, then channel_send copies the 16-byte ClickEvent
    // payload (val_ptr=scratch, elem_size=16n) — the same coordinate frame
    // pointer_moves reports, so a place-on-click demo shares its steering frame.
    // Coalesces like the others (feed a fresh event only once the worker drained
    // the previous one); clicks rarely burst, but a double/triple click still
    // collapses to a sample. Event source: `opts.clickTarget` (browser: the
    // canvas; node test: an EventTarget it dispatches synthetic clicks on), else
    // `globalThis` if it can addEventListener; no source → no-op (recv just
    // blocks). Not `{ passive }` — a click handler may legitimately want to
    // preventDefault — but this listener never does.
    builtinHostImpls["__kara_clicks"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.clickTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // ClickEvent layout: { x: f64 @ 0, y: f64 @ 8 } = 16 bytes — kept in sync
      // with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onClick = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 16);
        dv.setFloat64(0, e.offsetX ?? e.clientX ?? 0, true);
        dv.setFloat64(8, e.offsetY ?? e.clientY ?? 0, true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 16n);
      };
      target.addEventListener("click", onClick);
    };
    // __kara_dblclick(chPtr: i64 [BigInt]) -> (): the double-press sibling of
    // __kara_clicks — identical marshalling (the same 16-byte ClickEvent x,y
    // payload), only the DOM event differs ("dblclick" vs "click"). Event source:
    // `opts.dblclickTarget` (browser: the canvas; node test: an EventTarget it
    // dispatches synthetic dblclicks on), else `globalThis` if it can
    // addEventListener; no source → no-op (recv just blocks). A browser fires
    // `dblclick` alongside the two `click`s, so a program draining both shares
    // neither listener nor scratch contention (each producer reads the scratch
    // synchronously inside channel_send). Not `{ passive }`; never preventDefaults.
    builtinHostImpls["__kara_dblclick"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.dblclickTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // ClickEvent layout: { x: f64 @ 0, y: f64 @ 8 } = 16 bytes — kept in sync
      // with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onDblClick = (e) => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 16);
        dv.setFloat64(0, e.offsetX ?? e.clientX ?? 0, true);
        dv.setFloat64(8, e.offsetY ?? e.clientY ?? 0, true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 16n);
      };
      target.addEventListener("dblclick", onDblClick);
    };
    // __kara_resize(chPtr: i64 [BigInt]) -> (): a MULTI-SHOT window-dimension
    // stream. Registers a "resize" listener; UNLIKE the pointer/click producers
    // the payload does NOT come from the event object (a DOM "resize" event
    // carries no coordinates) — it reads the *current* innerWidth/innerHeight off
    // the target at dispatch time and marshals them as two little-endian i64s
    // into the event-scratch buffer, then channel_send copies the 16-byte
    // ResizeEvent payload (val_ptr=scratch, elem_size=16n). Coalesces like the
    // others (feed a fresh size only once the worker drained the previous one),
    // so a drag-resize burst collapses to the latest settled dimensions. Event
    // source: `opts.resizeTarget` (browser: window; node test: an object with
    // innerWidth/innerHeight it dispatches synthetic resizes on), else
    // `globalThis` if it can addEventListener; no source → no-op (recv just
    // blocks). Reads dims from the target, falling back to globalThis then 0.
    builtinHostImpls["__kara_resize"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.resizeTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // ResizeEvent layout: { width: i64 @ 0, height: i64 @ 8 } = 16 bytes —
      // kept in sync with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onResize = () => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 16);
        // Dimensions come from the window/target, not the event (resize carries
        // none); BigInt-floor to i64. Prefer the listening target's own dims.
        const w = target.innerWidth ?? globalThis.innerWidth ?? 0;
        const h = target.innerHeight ?? globalThis.innerHeight ?? 0;
        dv.setBigInt64(0, BigInt(Math.trunc(w)), true);
        dv.setBigInt64(8, BigInt(Math.trunc(h)), true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 16n);
      };
      target.addEventListener("resize", onResize);
    };
    // __kara_contextmenu(chPtr: i64 [BigInt]) -> (): the right-click sibling of
    // __kara_clicks — identical marshalling (the same 16-byte ClickEvent x,y
    // payload), only the DOM event differs ("contextmenu" vs "click"). The one
    // behavioral difference: the listener calls e.preventDefault() so the
    // browser's native context menu does NOT pop up over the canvas — suppressing
    // it is the whole point of capturing right-click in an app (the click/dblclick
    // listeners never preventDefault). Event source: `opts.contextmenuTarget`
    // (browser: the canvas; node test: an EventTarget it dispatches synthetic
    // contextmenus on), else `globalThis` if it can addEventListener; no source →
    // no-op (recv just blocks).
    builtinHostImpls["__kara_contextmenu"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.contextmenuTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      // ClickEvent layout: { x: f64 @ 0, y: f64 @ 8 } = 16 bytes — kept in sync
      // with runtime/stdlib/web_events.kara.
      const scratch = serviceInstance.exports.karac_runtime_event_scratch();
      const onContextMenu = (e) => {
        // Suppress the native menu regardless of coalescing — a right-click that
        // arrives while the worker is still draining the previous one must still
        // not pop the OS menu, so preventDefault BEFORE the pending-probe early-out.
        if (typeof e.preventDefault === "function") e.preventDefault();
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        // Re-derive the view each event: a shared Memory's `.buffer` may be
        // replaced on grow, so a cached DataView could go stale.
        const dv = new DataView(memory.buffer, scratch, 16);
        dv.setFloat64(0, e.offsetX ?? e.clientX ?? 0, true);
        dv.setFloat64(8, e.offsetY ?? e.clientY ?? 0, true);
        serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 16n);
      };
      target.addEventListener("contextmenu", onContextMenu);
    };
    // __kara_focus(chPtr: i64 [BigInt]) -> (): the first UNIT-payload event
    // producer. Registers a "focus" listener; each focus edge sends a 0-byte
    // `()` token (channel_send(ptr, 0, 0n) — NO event-scratch, like
    // animation_frames/every, vs the click/pointer producers' scratch marshal).
    // Coalesces via the pending-probe so a focus flurry collapses to one edge.
    // Event source: `opts.focusTarget` (browser: window; node test: an
    // EventTarget it dispatches synthetic focus events on), else `globalThis` if
    // it can addEventListener; no source → no-op (recv just blocks).
    builtinHostImpls["__kara_focus"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.focusTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      const onFocus = () => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);
      };
      target.addEventListener("focus", onFocus);
    };
    // __kara_blur(chPtr: i64 [BigInt]) -> (): the focus-LOST sibling of
    // __kara_focus — identical (a 0-byte `()` token per edge), only the DOM event
    // differs ("blur" vs "focus"). `focus` and `blur` are distinct event types,
    // so the two listeners never cross-fire on a shared target. Event source:
    // `opts.blurTarget`, else `globalThis`; no source → no-op.
    builtinHostImpls["__kara_blur"] = (chPtr) => {
      const ptr = Number(chPtr);
      const target =
        (opts && opts.blurTarget) ||
        (typeof globalThis.addEventListener === "function" ? globalThis : null);
      if (target === null) return;
      const onBlur = () => {
        if (Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) !== 0) return;
        serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);
      };
      target.addEventListener("blur", onBlur);
    };
  }
  // host fns: stand up the worker→main proxy (shared control block + the
  // main-thread service loop). Needed when the program declares user host
  // fns OR a builtin producer is wired. The service loop dispatches both
  // (user impls + builtinHostImpls), but only user impls are contract-
  // checked above.
  let hostCtl = null;
  let hostService = null;
  if (DECLARED_IMPORTS.length > 0 || BUILTIN_HOST_FNS.length > 0) {
    hostCtl = new SharedArrayBuffer(HOST_CTL_BYTES);
    hostService = startHostService(hostCtl, memory, {
      ...hostImpls,
      ...builtinHostImpls,
    });
  }
  const poolSize =
    THREADS_POOL_SIZE ??
    (globalThis.navigator && navigator.hardwareConcurrency) ??
    4;
  const env = ["KARAC_PAR_WORKERS=" + poolSize];
  // Browser: pthread Workers are created on the main thread (the only
  // always-live agent) via a shared spawn-request ring — a blocked
  // worker cannot load nested workers. node spawns siblings directly.
  let spawnCtl = null;
  let spawnService = null;
  if (!isNode) {
    spawnCtl = new SharedArrayBuffer(SPAWN_CTL_BYTES);
  }
  const data = {
    __karaThreads: true,
    role: "primary",
    module,
    memory,
    tidCounter,
    env,
    tid: 0,
    startArg: 0,
    hostCtl,
    spawnCtl,
  };
  if (spawnCtl) spawnService = startSpawnService(spawnCtl, data);
  try {
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
  } finally {
    if (hostService) await hostService.stop();
    if (spawnService) await spawnService.stop();
  }
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
  return { instance, exports: karaBuildExports(instance), memory };
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
      return await runThreaded(hostImpls, opts);
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
        let dts = render_dts(&fns, &[], "app.wasm", false);
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
    fn dts_types_rich_exports() {
        use crate::wasm_exports::{ExportField, ExportParam, ExportSig, ExportType, VariantShape};
        let i32t = || ExportType {
            kara_ty: "i32".to_string(),
            js: JsScalar::Number,
            scalar: true,
            record_fields: None,
            variant: None,
            string: false,
            list_elem: None,
        };
        let f64f = |n: &str| ExportField {
            name: n.to_string(),
            kara_ty: "f64".to_string(),
            js: JsScalar::Number,
        };
        let rich = |kara: &str, record_fields, variant, string, list_elem| ExportType {
            kara_ty: kara.to_string(),
            js: JsScalar::Number,
            scalar: false,
            record_fields,
            variant,
            string,
            list_elem,
        };
        let exports = vec![
            // fn mk(...) -> Point  (record return)
            ExportSig {
                name: "mk".to_string(),
                params: vec![],
                ret: Some(rich(
                    "Point",
                    Some(vec![f64f("x"), f64f("y")]),
                    None,
                    false,
                    None,
                )),
                target: "wasm_browser".to_string(),
            },
            // fn find(...) -> Option[i32]
            ExportSig {
                name: "find".to_string(),
                params: vec![],
                ret: Some(rich(
                    "Option",
                    None,
                    Some(VariantShape::Option(Box::new(i32t()))),
                    false,
                    None,
                )),
                target: "wasm_browser".to_string(),
            },
            // fn run(...) -> Result[i32, i32]
            ExportSig {
                name: "run".to_string(),
                params: vec![],
                ret: Some(rich(
                    "Result",
                    None,
                    Some(VariantShape::Result(Box::new(i32t()), Box::new(i32t()))),
                    false,
                    None,
                )),
                target: "wasm_browser".to_string(),
            },
            // fn nums(xs: Vec[i32]) -> String
            ExportSig {
                name: "nums".to_string(),
                params: vec![ExportParam {
                    name: "xs".to_string(),
                    ty: rich("Vec", None, None, false, Some(Box::new(i32t()))),
                }],
                ret: Some(rich("String", None, None, true, None)),
                target: "wasm_browser".to_string(),
            },
        ];
        let dts = render_dts(&[], &exports, "app.wasm", false);
        assert!(dts.contains("mk(): { x: number; y: number };"), "{dts}");
        assert!(dts.contains("find(): number | null;"), "{dts}");
        assert!(
            dts.contains("run(): { ok: number } | { err: number };"),
            "{dts}"
        );
        assert!(dts.contains("nums(xs: number[]): string;"), "{dts}");
    }

    #[test]
    fn dts_types_scalar_exports_on_handle() {
        use crate::wasm_exports::{ExportParam, ExportSig, ExportType};
        let scalar = |kara: &str, js| ExportType {
            kara_ty: kara.to_string(),
            js,
            scalar: true,
            record_fields: None,
            variant: None,
            string: false,
            list_elem: None,
        };
        let exports = vec![
            ExportSig {
                name: "add".to_string(),
                params: vec![
                    ExportParam {
                        name: "a".to_string(),
                        ty: scalar("i32", JsScalar::Number),
                    },
                    ExportParam {
                        name: "b".to_string(),
                        ty: scalar("i32", JsScalar::Number),
                    },
                ],
                ret: Some(scalar("i32", JsScalar::Number)),
                target: "wasm_browser".to_string(),
            },
            // Aggregate export: omitted from the typed surface in sub-slice B.
            ExportSig {
                name: "mk_point".to_string(),
                params: vec![],
                ret: Some(ExportType {
                    kara_ty: "Point".to_string(),
                    js: JsScalar::Number,
                    scalar: false,
                    record_fields: None,
                    variant: None,
                    string: false,
                    list_elem: None,
                }),
                target: "wasm_browser".to_string(),
            },
        ];
        let dts = render_dts(&[], &exports, "app.wasm", false);
        assert!(dts.contains("export interface KaraExports {"));
        assert!(dts.contains("_start(): void;"));
        assert!(dts.contains("add(a: number, b: number): number;"));
        assert!(dts.contains("exports: WebAssembly.Exports & KaraExports;"));
        // The non-scalar export is not typed yet (export trampoline pending).
        assert!(
            !dts.contains("mk_point("),
            "aggregate export must be omitted"
        );
    }

    #[test]
    fn dts_with_no_host_fns_makes_host_impls_optional() {
        let dts = render_dts(&[], &[], "plain.wasm", false);
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
        let glue = render_glue(&[], &[], "app.wasm", Some(&cfg));
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
            // pthread-spawn → main-thread proxy: browsers can't load a
            // nested worker while its creator is blocked, so Worker
            // construction is routed to the always-live main thread.
            "function requestMainThreadSpawn(spawnCtl, tid, startArg)",
            "function startSpawnService(spawnCtl, baseData)",
            "spawnCtl = new SharedArrayBuffer(SPAWN_CTL_BYTES);",
            "requestMainThreadSpawn(spawnCtl, newTid, arg);",
            "if (spawnService) await spawnService.stop();",
            // B-2026-06-14-22: shared-memory I/O must copy out of the SAB
            // before TextDecoder / crypto (which reject shared-backed views).
            "new Uint8Array(getMemory().buffer, ptr, len).slice()",
            "globalThis.crypto.getRandomValues(tmp);",
            // B-2026-06-14-22 follow-up: the exported readString funnel (backs
            // the threaded host-service `ctx.readString` string-arg path + the
            // rich string-export lift) must also copy before decode.
            "new Uint8Array(memory.buffer, Number(ptr), Number(len)).slice()",
        ] {
            assert!(glue.contains(needle), "missing in threaded glue: {needle}");
        }
        // node keeps spawning siblings directly (no proxy hop).
        assert!(glue.contains("if (nodeWorkerThreads) {"));
        // Sequential-only build renders the constants inert (the static
        // body references them unconditionally).
        let seq = render_glue(&[], &[], "app.wasm", None);
        assert!(seq.contains("const WASM_THREADS_FILENAME = null;"));
        assert!(seq.contains("const THREADS_POOL_SIZE = null;"));
    }

    #[test]
    fn threaded_glue_renders_host_fn_proxy() {
        let cfg = WasmThreadsGlueConfig {
            threads_filename: "app.threads.wasm".to_string(),
            no_fallback: false,
            pool_size_override: None,
            mem_initial_pages: 17,
            mem_max_pages: 16384,
        };
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
        let glue = render_glue(&fns, &[], "app.wasm", Some(&cfg));
        // Signature-driven marshalling table: per-fn arg/ret JS kinds. On a
        // threaded build the compiler-emitted `__kara_timer_after` builtin
        // is appended (it rides the same proxy machinery), but it is NOT a
        // user-facing import.
        assert!(glue.contains(
            "const HOST_FN_SIGS = [{ name: \"report\", params: [\"bigint\"], ret: \"bigint\" }, \
             { name: \"log_str\", params: [\"number\", \"bigint\"], ret: \"void\" }, \
             { name: \"__kara_timer_after\", params: [\"bigint\", \"bigint\"], ret: \"void\" }, \
             { name: \"__kara_timer_every\", params: [\"bigint\", \"bigint\"], ret: \"void\" }, \
             { name: \"__kara_animation_frames\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_pointer_moves\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_wheel\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_keydown\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_keyup\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_clicks\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_dblclick\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_resize\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_contextmenu\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_focus\", params: [\"bigint\"], ret: \"void\" }, \
             { name: \"__kara_blur\", params: [\"bigint\"], ret: \"void\" }];"
        ));
        // The builtins are registered as builtins, and excluded from the
        // user contract (DECLARED_IMPORTS drives the missing-impl check and
        // the d.ts).
        assert!(glue.contains(
            "const BUILTIN_HOST_FNS = [\"__kara_timer_after\", \"__kara_timer_every\", \
             \"__kara_animation_frames\", \"__kara_pointer_moves\", \"__kara_wheel\", \
             \"__kara_keydown\", \"__kara_keyup\", \"__kara_clicks\", \
             \"__kara_dblclick\", \"__kara_resize\", \"__kara_contextmenu\", \
             \"__kara_focus\", \"__kara_blur\"];"
        ));
        assert!(glue.contains("const DECLARED_IMPORTS = [\"report\", \"log_str\"];"));
        // Both halves of the proxy plus the worker-side wiring.
        for needle in [
            "function makeHostProxy(hostCtl)",
            "function startHostService(hostCtl, memory, hostImpls)",
            "imports.kara_host = makeHostProxy(hostCtl);",
            "Atomics.waitAsync",
            "Atomics.compareExchange(c, HC_LOCK",
            "return await runThreaded(hostImpls, opts);",
            // Host-async service instance + builtin timer impl.
            "serviceInstance = await WebAssembly.instantiate(module, serviceImports);",
            "serviceInstance.exports.karac_runtime_service_stack_top()",
            "builtinHostImpls[\"__kara_timer_after\"] = (chPtr, ms) =>",
            "serviceInstance.exports.karac_runtime_channel_send(ptr, 0, 0n);",
            // Multi-shot setInterval producer (sibling of after; coalesced).
            "builtinHostImpls[\"__kara_timer_every\"] = (chPtr, ms) =>",
            // Multi-shot rAF producer + its coalescing pending-probe.
            "builtinHostImpls[\"__kara_animation_frames\"] = (chPtr) =>",
            "Number(serviceInstance.exports.karac_runtime_channel_pending(ptr)) === 0",
            // Non-unit event-data producer: marshals a PointerEvent payload
            // (x, y, buttons) into the event-scratch buffer and sends 24 bytes.
            "builtinHostImpls[\"__kara_pointer_moves\"] = (chPtr) =>",
            "serviceInstance.exports.karac_runtime_event_scratch()",
            "serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 24n);",
            // Sibling non-unit producer: wheel/scroll, 32-byte WheelEvent.
            "builtinHostImpls[\"__kara_wheel\"] = (chPtr) =>",
            "serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 32n);",
            "target.addEventListener(\"wheel\", onWheel, { passive: true });",
            // Sibling non-unit producer: keydown, 8-byte KeyEvent.
            "builtinHostImpls[\"__kara_keydown\"] = (chPtr) =>",
            "serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 8n);",
            "target.addEventListener(\"keydown\", onKey);",
            // Key-release sibling: keyup, same 8-byte KeyEvent payload.
            "builtinHostImpls[\"__kara_keyup\"] = (chPtr) =>",
            "target.addEventListener(\"keyup\", onKey);",
            // Discrete click-position sibling of pointer_moves: 16-byte
            // ClickEvent (x, y).
            "builtinHostImpls[\"__kara_clicks\"] = (chPtr) =>",
            "serviceInstance.exports.karac_runtime_channel_send(ptr, scratch, 16n);",
            "target.addEventListener(\"click\", onClick);",
            // Double-press sibling: dblclick, same 16-byte ClickEvent payload.
            "builtinHostImpls[\"__kara_dblclick\"] = (chPtr) =>",
            "target.addEventListener(\"dblclick\", onDblClick);",
            // Window-dimension producer: resize, 16-byte ResizeEvent (two i64s
            // read off the window, not the event).
            "builtinHostImpls[\"__kara_resize\"] = (chPtr) =>",
            "target.addEventListener(\"resize\", onResize);",
            // Right-click sibling: contextmenu, same 16-byte ClickEvent payload;
            // its listener preventDefaults the native menu.
            "builtinHostImpls[\"__kara_contextmenu\"] = (chPtr) =>",
            "if (typeof e.preventDefault === \"function\") e.preventDefault();",
            "target.addEventListener(\"contextmenu\", onContextMenu);",
            // First unit-payload event producers: focus/blur, 0-byte () token
            // (no event-scratch).
            "builtinHostImpls[\"__kara_focus\"] = (chPtr) =>",
            "target.addEventListener(\"focus\", onFocus);",
            "builtinHostImpls[\"__kara_blur\"] = (chPtr) =>",
            "target.addEventListener(\"blur\", onBlur);",
        ] {
            assert!(glue.contains(needle), "missing proxy machinery: {needle}");
        }
        // The dead build-time rejection in runThreaded is gone.
        assert!(!glue.contains("host fns are not supported with wasm-threads"));
        // The builtin must NOT leak into the user-facing TypeScript surface.
        let dts = render_dts(&fns, &[], "app.wasm", true);
        assert!(
            !dts.contains("__kara_timer_after")
                && !dts.contains("__kara_timer_every")
                && !dts.contains("__kara_animation_frames")
                && !dts.contains("__kara_pointer_moves")
                && !dts.contains("__kara_wheel")
                && !dts.contains("__kara_keydown")
                && !dts.contains("__kara_keyup")
                && !dts.contains("__kara_clicks")
                && !dts.contains("__kara_dblclick")
                && !dts.contains("__kara_resize")
                && !dts.contains("__kara_contextmenu")
                && !dts.contains("__kara_focus")
                && !dts.contains("__kara_blur"),
            "builtin leaked into d.ts"
        );
    }

    #[test]
    fn host_fn_sigs_table_is_empty_without_host_fns() {
        // The static body references HOST_FN_SIGS unconditionally, so it
        // must render even on a host-fn-free build — as an empty array.
        let glue = render_glue(&[], &[], "plain.wasm", None);
        assert!(glue.contains("const HOST_FN_SIGS = [];"));
    }

    #[test]
    fn threaded_dts_declares_threaded_surface() {
        let dts = render_dts(&[], &[], "app.wasm", true);
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
