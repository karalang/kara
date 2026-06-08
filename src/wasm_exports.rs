//! WASM entry-point discovery (phase-10 "WASM entry-point discovery",
//! design.md § Entry point discovery).
//!
//! A user `fn` becomes a WASM module export when, for the target the
//! build is producing, all hold:
//!   1. it is `pub` (already drives `Linkage::External` in codegen, so
//!      the symbol survives DCE and carries its bare unmangled name);
//!   2. it carries a *positive* `#[target(wasm_browser)]` /
//!      `#[target(wasm_wasi)]` matching the build target.
//!
//! The "target-agnostic + transitively reachable from a tagged entry"
//! case (spec point 2's parenthetical) needs no compiler work — it falls
//! out of the linker's ordinary dead-code elimination once a tagged
//! entry pins it live. So discovery only collects the *explicit* entry
//! points: positively-tagged `pub fn`s.
//!
//! `main` is excluded — it is the `_start` entry, handled by the
//! `emit_wasm_entry_shim` path in codegen, not an additional export.
//!
//! This module is plain data (no `inkwell`), per the codegen-containment
//! invariant: it produces an [`ExportSig`] list that codegen (`--export=`
//! link flags), the browser glue (`wasm_glue`), and the WIT renderer
//! (`wit`) each consume.

use crate::ast::{Item, Program, TypeExpr, TypeKind};
use crate::target::target_spec_of;
use crate::wasm_glue::{handle_width_map, js_scalar, type_expr_display, JsScalar};
use std::collections::HashMap;

/// One field of a record-typed export param/return — a user struct with
/// all-scalar fields (sub-slice D). The Kāra struct layout (natural
/// alignment, declaration order) coincides with the Component Model
/// canonical record layout for scalar fields, so the trampoline can
/// relay an `sret` buffer directly.
#[derive(Debug, Clone)]
pub struct ExportField {
    pub name: String,
    pub kara_ty: String,
    pub js: JsScalar,
}

/// A param/return type of a discovered export, reduced to what the glue /
/// WIT renderers need.
#[derive(Debug, Clone)]
pub struct ExportType {
    /// Kāra-surface type rendering (`i32`, `Point`, `Option`, `*const u8`).
    pub kara_ty: String,
    /// JS-boundary scalar classification (meaningful only when `scalar`).
    pub js: JsScalar,
    /// `true` iff the type crosses the wasm boundary as a **bare scalar**
    /// (primitive, raw pointer, or single-field opaque-handle struct —
    /// confirmed by the empirical wasm ABI: small aggregates flatten /
    /// return via sret, scalars stay scalar).
    pub scalar: bool,
    /// `Some(fields)` when this is a user struct of all-scalar fields — a
    /// WIT `record` / JS object, marshalled via the export trampoline
    /// (sub-slice D). `None` for scalars and for not-yet-supported
    /// aggregates (`Option` / `Result` / `String` / `Vec` / nested).
    pub record_fields: Option<Vec<ExportField>>,
}

impl ExportType {
    /// A user struct of scalar fields — emittable as a WIT `record` and
    /// marshallable to/from a JS object (sub-slice D).
    pub fn is_record(&self) -> bool {
        self.record_fields.is_some()
    }

    /// Surface this slice can render/marshal today: bare scalars and flat
    /// records. (Option/Result/String/Vec/nested extend this as their
    /// sub-slices land.)
    pub fn is_marshallable(&self) -> bool {
        self.scalar || self.is_record()
    }
}

/// One parameter of a discovered wasm export.
#[derive(Debug, Clone)]
pub struct ExportParam {
    pub name: String,
    pub ty: ExportType,
}

/// One discovered wasm export.
#[derive(Debug, Clone)]
pub struct ExportSig {
    /// The Kāra function name — also the wasm export symbol (bare,
    /// unmangled — see `codegen::functions`).
    pub name: String,
    pub params: Vec<ExportParam>,
    /// `None` for unit returns; otherwise the return type.
    pub ret: Option<ExportType>,
    /// The wasm target this entry is tagged for (`wasm_browser` /
    /// `wasm_wasi`) — drives the binding-surface restriction and, later,
    /// the marshalling strategy.
    pub target: String,
}

impl ExportSig {
    /// `true` iff every param and the return cross as bare scalars — the
    /// surface renderable without the export trampoline (sub-slice B).
    pub fn all_scalar(&self) -> bool {
        self.params.iter().all(|p| p.ty.scalar) && self.ret.as_ref().is_none_or(|r| r.scalar)
    }

    /// `true` iff every param and the return is either a scalar or a flat
    /// record — the surface sub-slice D can lower. `all_scalar` exports
    /// are a subset (they need no trampoline).
    pub fn is_marshallable(&self) -> bool {
        self.params.iter().all(|p| p.ty.is_marshallable())
            && self.ret.as_ref().is_none_or(|r| r.is_marshallable())
    }

    /// `true` iff some param or the return is a flat record — i.e. this
    /// export needs the sub-slice D trampoline (a pure-scalar export does
    /// not). Only meaningful together with [`Self::is_marshallable`].
    pub fn needs_trampoline(&self) -> bool {
        self.params.iter().any(|p| p.ty.is_record())
            || self.ret.as_ref().is_some_and(|r| r.is_record())
    }

    /// `true` iff the codegen export trampoline can lower this export for
    /// a **component** build today: all params are scalars and the return
    /// is a scalar or a flat record. (Record *params* — which need
    /// canonical param-flattening + reconstruction — and `Option` /
    /// `Result` / `String` / `Vec` extend this as their steps land.) A
    /// record return needs the trampoline; a pure-scalar export does not.
    pub fn component_lowerable(&self) -> bool {
        self.params.iter().all(|p| p.ty.scalar)
            && self.ret.as_ref().is_none_or(|r| r.scalar || r.is_record())
    }
}

/// Collect the explicit wasm export entry points in `program` for
/// `current_target` (`"wasm_browser"` / `"wasm_wasi"`). Assumes
/// `target::filter_inactive_items` has already pruned non-matching
/// `#[target(...)]` items, but re-checks the positive tag defensively so
/// the result is correct regardless of call order.
pub fn collect_wasm_exports(program: &Program, current_target: &str) -> Vec<ExportSig> {
    let handles = handle_width_map(program);
    let structs = struct_field_map(program);
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Function(f) if is_export_entry(f, current_target) => {
                let params = f
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| ExportParam {
                        name: p
                            .name()
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("arg{i}")),
                        ty: export_type(&p.ty, &handles, &structs),
                    })
                    .collect();
                let ret = f.return_type.as_ref().and_then(|ty| match &ty.kind {
                    TypeKind::Tuple(elems) if elems.is_empty() => None,
                    _ => Some(export_type(ty, &handles, &structs)),
                });
                Some(ExportSig {
                    name: f.name.clone(),
                    params,
                    ret,
                    target: current_target.to_string(),
                })
            }
            _ => None,
        })
        .collect()
}

/// The bare wasm export symbol names — the `--export=<name>` arguments
/// codegen's wasm link step needs.
pub fn export_names(sigs: &[ExportSig]) -> Vec<String> {
    sigs.iter().map(|s| s.name.clone()).collect()
}

/// The `--export=<symbol>` arguments for the wasm link, given the binding.
///
/// For a **component** build, a record-returning export is exported via
/// the codegen trampoline, whose symbol is the kebab WIT name
/// (`make_point` ⇒ `make-point`); `--export`-ing that keeps the
/// trampoline through wasm-ld's GC and surfaces it as the canonical
/// export. Every other export (scalars, and all exports on browser /
/// `--bindings none` builds) is the real function, exported under its
/// bare Kāra symbol name (a scalar component export is then renamed to
/// kebab by codegen's `wasm-export-name` attribute).
pub fn link_export_names(sigs: &[ExportSig], component: bool) -> Vec<String> {
    sigs.iter()
        .map(|s| {
            if component && s.component_lowerable() && s.needs_trampoline() {
                crate::wit::host_import_name(&s.name)
            } else {
                s.name.clone()
            }
        })
        .collect()
}

/// Map every user struct name to its fields — used to classify a Path
/// type as a WIT `record` (multi-field, all-scalar) export type.
fn struct_field_map(program: &Program) -> HashMap<&str, &[crate::ast::StructField]> {
    program
        .items
        .iter()
        .filter_map(|item| match item {
            Item::StructDef(s) => Some((s.name.as_str(), s.fields.as_slice())),
            _ => None,
        })
        .collect()
}

/// Build an [`ExportType`] from a param/return `TypeExpr`, classifying it
/// as a bare scalar, a flat record (multi-field user struct of scalar
/// fields), or neither.
fn export_type(
    ty: &TypeExpr,
    handles: &HashMap<&str, JsScalar>,
    structs: &HashMap<&str, &[crate::ast::StructField]>,
) -> ExportType {
    let record_fields = record_fields_of(ty, handles, structs);
    ExportType {
        kara_ty: type_expr_display(ty),
        js: js_scalar(ty, handles),
        scalar: is_scalar_surface(ty, handles),
        record_fields,
    }
}

/// `Some(fields)` when `ty` names a user struct with **more than one**
/// field, all of which are scalar-surface (single-field structs are
/// opaque handles — already scalar — and are not records). `None`
/// otherwise. Nested aggregates (a struct field that is itself a record /
/// `Vec` / `Option`) disqualify the record for this slice.
fn record_fields_of(
    ty: &TypeExpr,
    handles: &HashMap<&str, JsScalar>,
    structs: &HashMap<&str, &[crate::ast::StructField]>,
) -> Option<Vec<ExportField>> {
    let TypeKind::Path(p) = &ty.kind else {
        return None;
    };
    if p.segments.len() != 1 || p.generic_args.is_some() {
        return None;
    }
    let fields = structs.get(p.segments[0].as_str())?;
    if fields.len() < 2 || !fields.iter().all(|f| is_scalar_surface(&f.ty, handles)) {
        return None;
    }
    Some(
        fields
            .iter()
            .map(|f| ExportField {
                name: f.name.clone(),
                kara_ty: type_expr_display(&f.ty),
                js: js_scalar(&f.ty, handles),
            })
            .collect(),
    )
}

/// Does `ty` cross the wasm boundary as a bare scalar (so it needs no
/// export trampoline)? True for primitives, raw pointers, and
/// single-field opaque-handle structs (in `handles`); false for
/// multi-field structs, generic types (`Option[T]` / `Result[T,E]` /
/// `Vec[T]`), `String`, tuples, slices, and borrows.
fn is_scalar_surface(ty: &TypeExpr, handles: &HashMap<&str, JsScalar>) -> bool {
    match &ty.kind {
        TypeKind::Pointer { .. } => true,
        TypeKind::Path(p) if p.segments.len() == 1 && p.generic_args.is_none() => {
            let name = p.segments[0].as_str();
            is_primitive_scalar(name) || handles.contains_key(name)
        }
        _ => false,
    }
}

/// The built-in numeric / `bool` / `char` primitives that cross as a
/// single wasm scalar.
fn is_primitive_scalar(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16"
            | "i32"
            | "i64"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "isize"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
    )
}

/// Is `f` an explicit wasm export entry for `current_target`?
/// `pub`, not `main`, no receiver, and *positively* tagged for this
/// target. A negated spec (`#[target(!native)]`) is an exclusion, not an
/// export intent — such fns reach wasm only via reachability/DCE.
///
/// Exposed for codegen, which attaches the canonical-ABI `wasm-export-name`
/// (kebab) attribute to these functions on component builds so the core
/// export name matches the embedded WIT.
pub fn is_export_entry(f: &crate::ast::Function, current_target: &str) -> bool {
    if !f.is_pub || f.self_param.is_some() || f.name == "main" {
        return false;
    }
    match target_spec_of(&f.attributes) {
        Some(spec) => !spec.negated && spec.names.iter().any(|n| n == current_target),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(src: &str) -> Program {
        crate::parse(src).program
    }

    #[test]
    fn discovers_only_positively_tagged_pub_fns() {
        let p = prog(
            r#"
            #[target(wasm_browser)] pub fn add(a: i32, b: i32) -> i32 { a + b }
            #[target(wasm_browser)] fn private_fn() {}
            #[target(native)] pub fn native_fn() {}
            pub fn untagged() {}
            #[target(wasm_browser)] pub fn main() {}
            "#,
        );
        let exports = collect_wasm_exports(&p, "wasm_browser");
        assert_eq!(export_names(&exports), vec!["add".to_string()]);
        assert_eq!(exports[0].params.len(), 2);
        assert!(exports[0].ret.is_some());
        assert_eq!(exports[0].target, "wasm_browser");
    }

    #[test]
    fn negated_tag_is_not_an_entry() {
        let p = prog(r#"#[target(!native)] pub fn f() {} fn main() {}"#);
        assert!(collect_wasm_exports(&p, "wasm_browser").is_empty());
    }

    #[test]
    fn discovery_is_keyed_on_the_build_target() {
        let p = prog(r#"#[target(wasm_wasi)] pub fn g(n: i64) -> i64 { n } fn main() {}"#);
        assert_eq!(
            export_names(&collect_wasm_exports(&p, "wasm_wasi")),
            vec!["g".to_string()]
        );
        // A wasm_wasi-tagged fn is not an export of a wasm_browser build.
        assert!(collect_wasm_exports(&p, "wasm_browser").is_empty());
    }

    #[test]
    fn unit_return_is_none() {
        let p = prog(r#"#[target(wasm_browser)] pub fn tick(n: i32) {} fn main() {}"#);
        let exports = collect_wasm_exports(&p, "wasm_browser");
        assert_eq!(exports.len(), 1);
        assert!(exports[0].ret.is_none());
    }

    #[test]
    fn scalars_and_handles_classify_as_scalar_surface() {
        let p = prog(
            r#"
            pub struct Handle { id: i64 }
            #[target(wasm_browser)] pub fn f(a: i32, b: f64, h: Handle) -> bool { true }
            fn main() {}
            "#,
        );
        let exports = collect_wasm_exports(&p, "wasm_browser");
        assert_eq!(exports.len(), 1);
        assert!(
            exports[0].all_scalar(),
            "primitives + single-field handle are scalar surface"
        );
    }

    #[test]
    fn aggregates_classify_as_non_scalar() {
        let p = prog(
            r#"
            #[derive(Copy, Clone)] pub struct Point { x: f64, y: f64 }
            #[target(wasm_browser)] pub fn a(p: Point) {}
            #[target(wasm_browser)] pub fn b() -> Option[i32] { Option.None }
            #[target(wasm_browser)] pub fn c(s: String) {}
            fn main() {}
            "#,
        );
        let exports = collect_wasm_exports(&p, "wasm_browser");
        for e in &exports {
            assert!(
                !e.all_scalar(),
                "{} should be non-scalar (aggregate)",
                e.name
            );
        }
    }

    #[test]
    fn multi_field_scalar_struct_classifies_as_record() {
        let p = prog(
            r#"
            #[derive(Copy, Clone)] pub struct Point { x: f64, y: f64 }
            pub struct Handle { id: i64 }
            #[target(wasm_wasi)] pub fn make_point(x: f64, y: f64) -> Point { Point { x: x, y: y } }
            #[target(wasm_wasi)] pub fn take_handle(h: Handle) -> i64 { 0 }
            fn main() {}
            "#,
        );
        let exports = collect_wasm_exports(&p, "wasm_wasi");
        let mk = exports.iter().find(|e| e.name == "make_point").unwrap();
        let ret = mk.ret.as_ref().unwrap();
        assert!(ret.is_record(), "Point return is a record");
        assert_eq!(ret.record_fields.as_ref().unwrap().len(), 2);
        assert!(!ret.scalar);
        // A record return with scalar params is component-lowerable and
        // needs the trampoline.
        assert!(mk.component_lowerable());
        assert!(mk.needs_trampoline());
        assert!(!mk.all_scalar());

        // A single-field struct is an opaque handle (scalar), NOT a record.
        let th = exports.iter().find(|e| e.name == "take_handle").unwrap();
        assert!(!th.params[0].ty.is_record());
        assert!(th.params[0].ty.scalar);
        assert!(th.all_scalar());
    }

    #[test]
    fn record_param_is_not_yet_component_lowerable() {
        // A record PARAM needs canonical param-flattening (a later step),
        // so it is not component_lowerable yet even though it is a record.
        let p = prog(
            r#"
            #[derive(Copy, Clone)] pub struct Point { x: f64, y: f64 }
            #[target(wasm_wasi)] pub fn sum(p: Point) -> f64 { p.x + p.y }
            fn main() {}
            "#,
        );
        let e = &collect_wasm_exports(&p, "wasm_wasi")[0];
        assert!(e.params[0].ty.is_record());
        assert!(!e.component_lowerable(), "record params not lowerable yet");
    }
}
