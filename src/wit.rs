//! WASM Component Model WIT descriptor emission (`--bindings component`,
//! phase-10 "WASM Component Model artifact emission").
//!
//! The v1 component-bindings output is the **paired form** design.md §
//! Target Build Artifacts sanctions until Component Model runtime
//! support is broadly stable: `<pkg>.wasm` (the C-ABI core module) plus
//! `<pkg>.component.wit` (the WIT interface this module renders) for
//! wrapping by external tools (`wasm-tools component embed/new`, jco).
//! The descriptor is plain text rendered in-compiler — karac takes no
//! dependency on the Component Model spec or its tooling; the
//! embedded-WIT migration (componentization via a pinned external tool,
//! `kara.toml` `[toolchain]` section) is the separate phase-10
//! follow-up entry, and downstream consumers of the paired shape get a
//! one-release deprecation notice before that swap.
//!
//! Mapping contract (also documented in the descriptor header):
//!
//!   - every `host fn` becomes a function in `interface host`; the
//!     core module imports it under the **`kara_host`** import module
//!     with its original snake_case name (WIT identifiers are
//!     kebab-case, so `log_str` renders as `log-str` — each doc
//!     comment carries the core import name);
//!   - the world export `run` is the core module's `_start` (WASI
//!     command) entry point — the only exported user entry until the
//!     phase-10 "WASM entry-point discovery" entry lands;
//!   - types map from the Kāra surface: `i8`..`i64`/`isize` ⇒
//!     `s8`..`s64`, `u8`..`u64`/`usize` ⇒ `u8`..`u64` (Kāra keeps
//!     64-bit `usize` semantics on wasm32), `f32`/`f64`, `bool`,
//!     `char`; raw pointers are wasm32 linear-memory addresses (`u32`)
//!     a wrapper must handle below the canonical ABI; single-field
//!     opaque handles cross at their field's scalar width.
//!
//! Like `wasm_glue`, this module is deliberately **inkwell-free**
//! (codegen containment — CLAUDE.md § Architecture): it consumes the
//! plain [`HostFnSig`] surface and emits a string. The CLI writes the
//! file next to the `.wasm` artifact.

use crate::wasm_glue::{HostFnSig, JsScalar};
use std::fmt::Write;

/// WIT keywords that need the `%`-escape when they collide with a
/// rendered identifier (a `host fn` named `record`, a package named
/// `result`). Subset of the WIT grammar's reserved words that a
/// kebab-cased Kāra identifier can actually collide with.
const WIT_KEYWORDS: &[&str] = &[
    "as",
    "async",
    "bool",
    "borrow",
    "char",
    "constructor",
    "export",
    "f32",
    "f64",
    "flags",
    "from",
    "func",
    "future",
    "import",
    "include",
    "interface",
    "list",
    "option",
    "own",
    "package",
    "record",
    "resource",
    "result",
    "s16",
    "s32",
    "s64",
    "s8",
    "static",
    "stream",
    "string",
    "tuple",
    "type",
    "u16",
    "u32",
    "u64",
    "u8",
    "use",
    "variant",
    "with",
    "world",
];

/// Lower a Kāra identifier (snake_case fn/param name, `kara.toml`
/// package name) to a valid WIT kebab-case identifier: ASCII-lowercase,
/// separators (`_` and anything non-alphanumeric) become `-`, runs
/// collapse, edges trim. A kebab *word* cannot start with a digit, so a
/// separator followed by a digit folds into the preceding word
/// (`vec_2d` ⇒ `vec2d`), and a leading digit gains a `p` prefix.
fn kebab_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending_sep = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() && !c.is_ascii_digit() {
                out.push('-');
            }
            pending_sep = false;
            if out.is_empty() && c.is_ascii_digit() {
                out.push('p');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            pending_sep = true;
        }
    }
    if out.is_empty() {
        out.push_str("pkg");
    }
    out
}

/// [`kebab_ident`] plus the `%`-escape for WIT-keyword collisions —
/// the form every *named-entity* position (function, param, package,
/// world) uses.
fn wit_ident(name: &str) -> String {
    let kebab = kebab_ident(name);
    if WIT_KEYWORDS.contains(&kebab.as_str()) {
        format!("%{kebab}")
    } else {
        kebab
    }
}

/// WIT type for a `host fn`-legal Kāra surface type. Unknown names are
/// the single-field opaque handles `collect_host_fns` resolved to their
/// field's scalar width — render at that width via the JS-boundary
/// classification (`BigInt` ⇔ wasm `i64`).
fn wit_type(kara_ty: &str, js: JsScalar) -> &'static str {
    match kara_ty {
        "i8" => "s8",
        "i16" => "s16",
        "i32" => "s32",
        "i64" | "isize" => "s64",
        "u8" => "u8",
        "u16" => "u16",
        "u32" => "u32",
        "u64" | "usize" => "u64",
        "f32" => "f32",
        "f64" => "f64",
        "bool" => "bool",
        "char" => "char",
        // Raw pointers are wasm32 linear-memory addresses — meaningful
        // only below the canonical ABI; the doc comment on the function
        // carries the Kāra-surface type for the wrapper author.
        ty if ty.starts_with('*') => "u32",
        _ => match js {
            JsScalar::Number => "s32",
            JsScalar::BigInt => "s64",
        },
    }
}

/// Kāra-surface signature doc line (the source of truth the WIT types
/// were derived from), mirroring `render_dts`'s convention.
fn kara_signature(sig: &HostFnSig) -> String {
    let params = sig
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.kara_ty))
        .collect::<Vec<_>>()
        .join(", ");
    let ret = match &sig.ret {
        Some((ty, _)) => format!(" -> {ty}"),
        None => String::new(),
    };
    format!("host fn {}({params}){ret}", sig.name)
}

/// Render the `<pkg>.component.wit` descriptor for the paired
/// component-bindings shape. `package` is the `kara.toml` package name
/// (project mode) or the source-file stem (single-file mode);
/// `wasm_filename` is the sibling core module's file name, named in the
/// header so the pairing is self-describing.
pub fn render_component_wit(fns: &[HostFnSig], package: &str, wasm_filename: &str) -> String {
    let pkg = wit_ident(package);
    // Interfaces and worlds share a namespace within a WIT package — a
    // package literally named "host" must not collide with the `host`
    // interface its own descriptor declares.
    let world = if pkg == "host" {
        "host-world".to_string()
    } else {
        pkg.clone()
    };

    let mut out = String::with_capacity(2 * 1024);
    let _ = write!(
        out,
        "// Generated by karac for {wasm_filename} — WIT interface descriptor. DO NOT EDIT.\n\
         //\n\
         // Paired component-bindings shape (design.md § Target Build Artifacts):\n\
         // {wasm_filename} is a C-ABI core module (wasm32-wasip1 command); this\n\
         // descriptor declares its host surface for wrapping by external tools\n\
         // (wasm-tools component embed/new, jco). Until the embedded-WIT swap,\n\
         // the pairing contract is:\n\
         //\n\
         //   - each `interface host` function is a core-module import under the\n\
         //     `kara_host` import module, with the snake_case name its doc\n\
         //     comment records (WIT identifiers are kebab-case);\n\
         //   - the world export `run` is the core module's `_start` entry point;\n\
         //   - strings cross the core ABI as (ptr, len) pairs into linear\n\
         //     memory; raw-pointer parameters (rendered u32) are linear-memory\n\
         //     addresses a wrapper must bridge below the canonical ABI.\n\n"
    );
    let _ = writeln!(out, "package kara:{pkg};\n");

    if !fns.is_empty() {
        out.push_str(
            "/// Host functions the embedder provides (the core module's\n\
             /// `kara_host` import namespace).\n\
             interface host {\n",
        );
        for sig in fns {
            let _ = writeln!(
                out,
                "  /// `{}` — core import `kara_host.{}`",
                kara_signature(sig),
                sig.name
            );
            let params = sig
                .params
                .iter()
                .map(|p| format!("{}: {}", wit_ident(&p.name), wit_type(&p.kara_ty, p.js)))
                .collect::<Vec<_>>()
                .join(", ");
            let ret = match &sig.ret {
                Some((ty, js)) => format!(" -> {}", wit_type(ty, *js)),
                None => String::new(),
            };
            let _ = writeln!(out, "  {}: func({params}){ret};", wit_ident(&sig.name));
        }
        out.push_str("}\n\n");
    }

    let _ = writeln!(out, "world {world} {{");
    if !fns.is_empty() {
        out.push_str("  import host;\n");
    }
    out.push_str(
        "  /// The program entry point (core-module export `_start`).\n\
         \x20 export run: func();\n\
         }\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm_glue::HostParam;

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
    fn wit_maps_host_fns_per_boundary_contract() {
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
                    param("len", "usize", JsScalar::BigInt),
                ],
                None,
            ),
        ];
        let wit = render_component_wit(&fns, "webapp", "webapp.wasm");
        assert!(wit.contains("package kara:webapp;"));
        // 64-bit ints are s64/u64; pointers are wasm32 addresses (u32);
        // snake_case kebab-cases; unit returns drop the arrow.
        assert!(wit.contains("report: func(value: s64) -> s64;"));
        assert!(wit.contains("log-str: func(ptr: u32, len: u64);"));
        // Kāra-surface signature + core import name survive as doc lines.
        assert!(
            wit.contains("`host fn report(value: i64) -> i64` — core import `kara_host.report`")
        );
        // Host fns pull the interface import into the world.
        assert!(wit.contains("interface host {"));
        assert!(wit.contains("world webapp {"));
        assert!(wit.contains("import host;"));
        assert!(wit.contains("export run: func();"));
    }

    #[test]
    fn wit_without_host_fns_omits_the_host_interface() {
        let wit = render_component_wit(&[], "plain", "plain.wasm");
        assert!(!wit.contains("interface host {"));
        assert!(!wit.contains("import host;"));
        assert!(wit.contains("package kara:plain;"));
        assert!(wit.contains("world plain {"));
        assert!(wit.contains("export run: func();"));
    }

    #[test]
    fn wit_identifiers_escape_keywords_and_normalize_kebab() {
        // Keyword collision: a host fn named `record` must %-escape.
        let fns = vec![sig("record", vec![], None)];
        let wit = render_component_wit(&fns, "My_App", "my_app.wasm");
        assert!(wit.contains("%record: func();"));
        // Package/world names kebab-case (lowercase, `_` ⇒ `-`).
        assert!(wit.contains("package kara:my-app;"));
        assert!(wit.contains("world my-app {"));
        // A kebab word can't start with a digit: fold the separator.
        assert_eq!(kebab_ident("vec_2d"), "vec2d");
        assert_eq!(kebab_ident("2048"), "p2048");
        // A package named exactly `host` must not collide with the
        // descriptor's own `host` interface.
        let hosted = render_component_wit(&fns, "host", "host.wasm");
        assert!(hosted.contains("world host-world {"));
    }
}
