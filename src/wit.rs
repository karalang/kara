//! WASM Component Model WIT emission (`--bindings component`, phase-10
//! "WASM Component Model artifact emission" + "embedded-WIT
//! migration").
//!
//! [`render_embed_wit`] renders the WIT world `wasm-tools component
//! embed` bakes into the core module before `component new` wraps it
//! into a single self-describing component
//! (`componentize::componentize`). The world only *imports* `host`;
//! the preview1 command adapter contributes `export wasi:cli/run`
//! from `_start` at `component new` time, so no custom `run` export
//! appears here. The core module imports host fns as
//! `kara:<pkg>/host` / kebab-case names per the canonical ABI —
//! [`host_import_module`] / [`host_import_name`] are the single
//! source of those strings for codegen's attribute-attachment site.
//! (The pre-embedded-WIT **paired form** — C-ABI core module +
//! `<pkg>.component.wit` descriptor, `--bindings component-paired` —
//! was removed pre-first-release per the one-release deprecation
//! contract in design.md § Target Build Artifacts; no release ever
//! carried it.)
//!
//! Karac takes no dependency on the Component Model spec itself — the
//! world is plain text, and componentization is delegated to the
//! external `wasm-tools` binary pinned via `kara.toml` `[toolchain]`
//! (design.md § Component Model emission).
//!
//! Mapping contract:
//!
//!   - every `host fn` becomes a function in `interface host` (doc
//!     comments carry the core import name);
//!   - the program entry point is the core module's `_start` (WASI
//!     command) — surfaced as the adapter-synthesized `wasi:cli/run`
//!     in the embedded component — the only exported user entry until
//!     the phase-10 "WASM entry-point discovery" entry lands;
//!   - types map from the Kāra surface: `i8`..`i64`/`isize` ⇒
//!     `s8`..`s64`, `u8`..`u64`/`usize` ⇒ `u8`..`u64` (Kāra keeps
//!     64-bit `usize` semantics on wasm32), `f32`/`f64`, `bool`,
//!     `char`; raw pointers are wasm32 linear-memory addresses (`u32`)
//!     a wrapper must handle below the canonical ABI; single-field
//!     opaque handles cross at their field's scalar width.
//!
//! Like `wasm_glue`, this module is deliberately **inkwell-free**
//! (codegen containment — CLAUDE.md § Architecture): it consumes the
//! plain [`HostFnSig`] surface and emits strings.

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

/// Core-module import **module** string for a `host fn` under the
/// embedded component form: the canonical-ABI instance name
/// `kara:<pkg>/host`. The `%`-escape is a WIT *parser* device, not part
/// of the resolved identifier, so the module string uses the bare kebab
/// name even when the WIT text spells it `%record`. Single source for
/// codegen's `wasm-import-module` attribute — must agree with the
/// `package kara:<pkg>;` line [`render_embed_wit`] emits.
pub fn host_import_module(package: &str) -> String {
    format!("kara:{}/host", kebab_ident(package))
}

/// Core-module import **name** string for a `host fn` under the
/// embedded component form: the kebab-cased function name (canonical
/// ABI; bare — see [`host_import_module`] on `%`-escapes). Single
/// source for codegen's `wasm-import-name` attribute — must agree with
/// the `interface host` function names [`render_embed_wit`] emits.
pub fn host_import_name(fn_name: &str) -> String {
    kebab_ident(fn_name)
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

/// World name for a package: the kebab package name, except a package
/// literally named "host" — interfaces and worlds share a namespace
/// within a WIT package, so it must not collide with the `host`
/// interface the same file declares.
fn world_name(pkg: &str) -> String {
    if pkg == "host" {
        "host-world".to_string()
    } else {
        pkg.to_string()
    }
}

/// Append the `interface host { ... }` block — one function per
/// `host fn`, each with a doc line carrying the Kāra-surface signature
/// and the canonical-ABI core import path
/// (`kara:<pkg>/host.<kebab-name>`, the strings codegen attaches).
fn push_host_interface(out: &mut String, fns: &[HostFnSig], doc_module: &str) {
    out.push_str(
        "/// Host functions the embedder provides (the core module's\n\
         /// host-import namespace).\n\
         interface host {\n",
    );
    for sig in fns {
        let _ = writeln!(
            out,
            "  /// `{}` — core import `{doc_module}.{}`",
            kara_signature(sig),
            host_import_name(&sig.name)
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

/// Render the WIT world `wasm-tools component embed` bakes into the
/// core module on the embedded component path (`--bindings component`).
/// Returns `(wit_text, world_name)` — the world name is what
/// `componentize` passes as `--world`.
///
/// The world only **imports** `host` (when host fns exist): the
/// preview1 command adapter synthesizes `export wasi:cli/run` from the
/// module's `_start` at `component new` time, so declaring a custom
/// `run` export here would demand a core export that doesn't exist.
/// Core import naming must match what codegen attached —
/// [`host_import_module`] / [`host_import_name`].
pub fn render_embed_wit(fns: &[HostFnSig], package: &str) -> (String, String) {
    let pkg = wit_ident(package);
    let world = world_name(&pkg);
    let module = host_import_module(package);

    let mut out = String::with_capacity(1024);
    let _ = write!(
        out,
        "// Generated by karac — embed input for `wasm-tools component embed`.\n\
         // The world's host imports correspond to core-module imports under\n\
         // the `{module}` import module (canonical ABI); `wasi:cli/run` is\n\
         // contributed by the preview1 command adapter from `_start`.\n\n"
    );
    let _ = writeln!(out, "package kara:{pkg};\n");
    if !fns.is_empty() {
        push_host_interface(&mut out, fns, &module);
    }
    let _ = writeln!(out, "world {world} {{");
    if !fns.is_empty() {
        out.push_str("  import host;\n");
    }
    out.push_str("}\n");
    (out, world)
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
        let (wit, _) = render_embed_wit(&fns, "webapp");
        assert!(wit.contains("package kara:webapp;"));
        // 64-bit ints are s64/u64; pointers are wasm32 addresses (u32);
        // snake_case kebab-cases; unit returns drop the arrow.
        assert!(wit.contains("report: func(value: s64) -> s64;"));
        assert!(wit.contains("log-str: func(ptr: u32, len: u64);"));
        // Kāra-surface signature + core import name survive as doc lines.
        assert!(wit.contains(
            "`host fn report(value: i64) -> i64` — core import `kara:webapp/host.report`"
        ));
        // Host fns pull the interface import into the world.
        assert!(wit.contains("interface host {"));
        assert!(wit.contains("world webapp {"));
        assert!(wit.contains("import host;"));
    }

    #[test]
    fn embed_wit_imports_host_and_leaves_run_to_the_adapter() {
        let fns = vec![sig(
            "log_str",
            vec![
                param("ptr", "*const u8", JsScalar::Number),
                param("len", "usize", JsScalar::BigInt),
            ],
            None,
        )];
        let (wit, world) = render_embed_wit(&fns, "webapp");
        assert_eq!(world, "webapp");
        assert!(wit.contains("package kara:webapp;"));
        assert!(wit.contains("interface host {"));
        assert!(wit.contains("log-str: func(ptr: u32, len: u64);"));
        assert!(wit.contains("import host;"));
        // The embedded core module imports under the canonical-ABI
        // instance name, kebab-cased — the doc line records it.
        assert!(wit.contains("core import `kara:webapp/host.log-str`"));
        // No custom run export — `wasi:cli/run` is the adapter's job;
        // declaring it here would demand a nonexistent core export.
        assert!(!wit.contains("export run"));
    }

    #[test]
    fn embed_wit_without_host_fns_is_an_empty_world() {
        // The CLI skips the embed step entirely for host-fn-free
        // programs (`component new` direct), but the renderer stays
        // total: empty world, no host interface.
        let (wit, world) = render_embed_wit(&[], "plain");
        assert_eq!(world, "plain");
        assert!(!wit.contains("interface host"));
        assert!(!wit.contains("import host;"));
        assert!(wit.contains("world plain {\n}\n"));
    }

    #[test]
    fn host_import_naming_matches_embed_wit_text() {
        // The codegen attribute strings and the WIT text must agree —
        // these helpers are the single source for both.
        assert_eq!(host_import_module("My_App"), "kara:my-app/host");
        assert_eq!(host_import_name("log_str"), "log-str");
        // %-escape is parser-level only: the resolved identifier (and
        // therefore the core import string) is bare.
        assert_eq!(host_import_module("record"), "kara:record/host");
        assert_eq!(host_import_name("record"), "record");
        let (wit, _) = render_embed_wit(&[sig("record", vec![], None)], "record");
        assert!(wit.contains("package kara:%record;"));
        assert!(wit.contains("%record: func();"));
    }

    #[test]
    fn wit_identifiers_escape_keywords_and_normalize_kebab() {
        // Keyword collision: a host fn named `record` must %-escape.
        let fns = vec![sig("record", vec![], None)];
        let (wit, world) = render_embed_wit(&fns, "My_App");
        assert!(wit.contains("%record: func();"));
        // Package/world names kebab-case (lowercase, `_` ⇒ `-`).
        assert!(wit.contains("package kara:my-app;"));
        assert!(wit.contains("world my-app {"));
        assert_eq!(world, "my-app");
        // A kebab word can't start with a digit: fold the separator.
        assert_eq!(kebab_ident("vec_2d"), "vec2d");
        assert_eq!(kebab_ident("2048"), "p2048");
        // A package named exactly `host` must not collide with the
        // world file's own `host` interface.
        let (hosted, hosted_world) = render_embed_wit(&fns, "host");
        assert!(hosted.contains("world host-world {"));
        assert_eq!(hosted_world, "host-world");
    }
}
