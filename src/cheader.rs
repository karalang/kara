//! C-header emitter for producer-mode library artifacts — the cbindgen
//! analogue (additive-interop Slice 3; design.md § Exported C ABI).
//!
//! Given a program built as a `--crate-type staticlib/cdylib`, this emits
//! a `.h` describing the exported C surface so a foreign caller
//! `#include`s it instead of hand-transcribing signatures. It is plain
//! data (AST → string, no `inkwell`), so it lives outside the
//! codegen-containment boundary and is callable on a non-`llvm` build.
//!
//! The exported surface is every `pub extern "C" fn` (see
//! [`is_exported`]); today Kāra emits bare, unmangled symbols for those,
//! so the C symbol name is the source name. The type mapping is the
//! honest v1 set from the spec: primitives + raw pointers + `#[repr(C)]`
//! structs cross transparently; everything else crosses as an opaque
//! `KaraHandle` (a `void*`), with the Kāra type named in a comment. The
//! ownership/boxing mechanics behind a handle are Slice 4 (`forget`) —
//! the header states the surface, not the handoff.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    EffectItem, EffectList, EffectVerbKind, EnumDef, Function, Item, Program, StructDef, TypeExpr,
    TypeKind,
};

/// True iff `func` is part of the exported C surface: a `pub extern "C"`
/// (or `"C-unwind"`) function *definition*. `abi` is `Some` only for
/// FFI-export definitions (a body callable from C), so this does not
/// match ordinary Kāra functions or foreign imports.
pub fn is_exported(func: &Function) -> bool {
    func.is_pub && matches!(func.abi.as_deref(), Some("C") | Some("C-unwind"))
}

/// A `pub extern "C" fn` whose non-transparent aggregate return is
/// auto-boxed for the C ABI (additive-interop Slice 4 Path B). Kāra returns
/// a `{data,len,cap}` value in registers, which doesn't match the SysV
/// struct-return ABI — so the export heap-boxes the value and returns an
/// opaque pointer to it; the C side reads the fields through the emitted
/// struct and frees via `karac_free_<name>`.
///
/// Covers `String`, `Vec[scalar]`, and — the Path-B follow-on — one level
/// of aggregate nesting (`Vec[String]`, `Vec[Vec[scalar]]`), whose elements
/// are themselves `{ptr,len,cap}` and need a per-element buffer drop before
/// the outer buffer. Deeper nesting / `enum` / user-struct returns stay
/// opaque-`KaraHandle`.
pub enum BoxedReturn {
    /// `String` or `Vec[scalar]`: `{elem_c* data; int64_t len; int64_t cap;}`,
    /// no per-element drop. `struct_name` = the C typedef, `elem_c` = the
    /// `data` element C type (`uint8_t` / `int64_t`).
    Flat {
        struct_name: String,
        elem_c: &'static str,
    },
    /// `Vec[String]` / `Vec[Vec[scalar]]`: each element is a `{ptr,len,cap}`
    /// needing a per-element buffer drop. `elem_struct` is the element's C
    /// typedef (`KaraString` / `KaraVec_int64_t`), `elem_elem_c` its own
    /// `data` element type, `outer_name` the `Vec[...]` C typedef.
    Nested {
        outer_name: String,
        elem_struct: String,
        elem_elem_c: &'static str,
    },
}

/// If `func` is an exported fn whose return needs C-ABI auto-boxing,
/// classify it. `None` for a transparent return (primitive / `#[repr(C)]`
/// struct / raw pointer / `()`), a deeper-nested / `enum` / user aggregate
/// (those stay opaque-`KaraHandle`), or any non-export.
pub fn boxed_return_of(func: &Function) -> Option<BoxedReturn> {
    if !func.is_pub || func.abi.as_deref() != Some("C") {
        return None;
    }
    boxed_shape_of(func.return_type.as_ref()?)
}

/// Classify a return `TypeExpr` as a boxable shape (`None` if it isn't).
fn boxed_shape_of(te: &TypeExpr) -> Option<BoxedReturn> {
    let TypeKind::Path(p) = &te.kind else {
        return None;
    };
    if p.segments.len() != 1 {
        return None;
    }
    match p.segments[0].as_str() {
        "String" => Some(BoxedReturn::Flat {
            struct_name: "KaraString".to_string(),
            elem_c: "uint8_t",
        }),
        "Vec" => {
            let elem = vec_elem(p)?;
            // Vec[scalar] → flat.
            if let TypeKind::Path(ep) = &elem.kind {
                if ep.segments.len() == 1 {
                    if let Some(c) = c_primitive(&ep.segments[0]) {
                        return Some(BoxedReturn::Flat {
                            struct_name: format!("KaraVec_{c}"),
                            elem_c: c,
                        });
                    }
                }
            }
            // Vec[String] / Vec[Vec[scalar]] → nested (element is an
            // aggregate `{ptr,len,cap}`).
            match boxed_shape_of(elem)? {
                BoxedReturn::Flat {
                    struct_name,
                    elem_c,
                } => Some(BoxedReturn::Nested {
                    outer_name: format!("KaraVec_{struct_name}"),
                    elem_struct: struct_name,
                    elem_elem_c: elem_c,
                }),
                // Deeper than one nesting level — not boxable (opaque).
                BoxedReturn::Nested { .. } => None,
            }
        }
        _ => None,
    }
}

/// The single `Type` generic arg of a `Vec[E]` path, else `None`.
fn vec_elem(p: &crate::ast::PathExpr) -> Option<&TypeExpr> {
    let args = p.generic_args.as_ref()?;
    if args.len() != 1 {
        return None;
    }
    match &args[0] {
        crate::ast::GenericArg::Type(elem) => Some(elem),
        _ => None,
    }
}

/// The outer C typedef name for a boxed return.
fn boxed_struct_name(b: &BoxedReturn) -> String {
    match b {
        BoxedReturn::Flat { struct_name, .. } => struct_name.clone(),
        BoxedReturn::Nested { outer_name, .. } => outer_name.clone(),
    }
}

/// True iff the boxed return's elements are themselves aggregates
/// (`{ptr,len,cap}`) that need a per-element buffer drop in the destructor.
pub fn boxed_return_elements_need_drop(func: &Function) -> bool {
    matches!(boxed_return_of(func), Some(BoxedReturn::Nested { .. }))
}

/// The full set of C symbol names an artifact must publish in its dynamic
/// symbol table, in the order a `.def`/`/EXPORT:` list wants them. Unix
/// shared objects export every default-visibility symbol automatically, so
/// this is only *needed* on Windows — a DLL exports nothing unless each
/// symbol is named (`link.exe` has no "export all" for C symbols the way
/// `ld` does). It comprises: every `pub extern "C" fn` (bare source name);
/// the auto-emitted `karac_free_<name>` destructor for each boxed-return
/// export (Slice 4 Path B); and the two runtime lifecycle entry points the
/// C header always declares. Kept here (plain-data, AST-derived) so the
/// codegen link path and the header stay in lockstep on the same rule.
pub fn export_symbols(program: &Program) -> Vec<String> {
    let mut syms = Vec::new();
    for it in &program.items {
        if let Item::Function(f) = it {
            if is_exported(f) {
                syms.push(f.name.clone());
                if boxed_return_of(f).is_some() {
                    syms.push(format!("karac_free_{}", f.name));
                }
            }
        }
    }
    syms.push("karac_runtime_init".to_string());
    syms.push("karac_runtime_shutdown".to_string());
    syms
}

/// True iff `te` crosses the C boundary *transparently by value* — a
/// primitive, a raw pointer, `()`, a `#[repr(C)]` struct named in `repr_c`,
/// or an all-unit `#[repr(C)]` enum named in `repr_c_enums` (its value is the
/// bare discriminant, an integer). These need no boxing; anything else is
/// either boxable (returns only) or unsupported.
fn is_transparent_boundary_type(
    te: &TypeExpr,
    repr_c: &std::collections::BTreeSet<String>,
    repr_c_enums: &std::collections::BTreeSet<String>,
) -> bool {
    match &te.kind {
        TypeKind::Unit => true,
        TypeKind::Tuple(elems) if elems.is_empty() => true,
        TypeKind::Pointer { .. } => true,
        TypeKind::Path(p) if p.segments.len() == 1 => {
            let n = &p.segments[0];
            c_primitive(n).is_some() || repr_c.contains(n) || repr_c_enums.contains(n)
        }
        _ => false,
    }
}

/// Validate every exported signature for C-ABI honesty (Slice 4). Returns
/// `(fn_name, reason)` for each export whose return or a param crosses the
/// boundary as neither a transparent-by-value type nor (for a return) a
/// boxable `Vec`/`String` — those would otherwise emit a dishonest
/// `KaraHandle` while codegen returns/expects a multi-register aggregate,
/// a silent miscompile. A library build turns a non-empty result into a
/// hard error.
pub fn validate_exports(program: &Program) -> Vec<(String, String)> {
    let repr_c: std::collections::BTreeSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::StructDef(s) if is_repr_c(s) => Some(s.name.clone()),
            _ => None,
        })
        .collect();
    // Every user struct (repr(C) or not) — lets a "you returned a plain
    // struct" case suggest the one-step `#[repr(C)]` fix instead of the
    // generic hint.
    let all_structs: std::collections::BTreeSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::StructDef(s) => Some(s.name.clone()),
            _ => None,
        })
        .collect();
    // User-defined enums — lets the hint name the enum-specific path (no
    // by-value C representation yet) instead of the generic fallback.
    let all_enums: std::collections::BTreeSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::EnumDef(e) => Some(e.name.clone()),
            _ => None,
        })
        .collect();
    // All-unit `#[repr(C)]` enums — cross transparently as a C integer
    // (Slice 1). A `#[repr(C)]` enum with a data-carrying variant is NOT in
    // this set and stays rejected (the tagged-union case is Slice 2).
    let repr_c_enums: std::collections::BTreeSet<String> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::EnumDef(e) if is_repr_c_all_unit_enum(e) => Some(e.name.clone()),
            _ => None,
        })
        .collect();
    let mut errs = Vec::new();
    for it in &program.items {
        let Item::Function(f) = it else { continue };
        if !is_exported(f) {
            continue;
        }
        // Return: transparent OR boxable.
        if let Some(rt) = &f.return_type {
            if !is_transparent_boundary_type(rt, &repr_c, &repr_c_enums)
                && boxed_return_of(f).is_none()
            {
                errs.push((
                    f.name.clone(),
                    format!(
                        "return type `{}` cannot cross the C ABI: it is neither transparent \
                         (primitive / raw pointer / `#[repr(C)]` struct / all-unit `#[repr(C)]` \
                         enum) nor an auto-boxable \
                         `Vec[scalar]` / `String` / `Vec[String]` / `Vec[Vec[scalar]]`.{}",
                        type_display(rt),
                        abi_fix_hint(
                            rt,
                            BoundaryPosition::Return,
                            &all_structs,
                            &all_enums,
                            &repr_c
                        )
                    ),
                ));
            }
        }
        // Params: transparent only (no boxing on the caller-provided side).
        for p in &f.params {
            if !is_transparent_boundary_type(&p.ty, &repr_c, &repr_c_enums) {
                errs.push((
                    f.name.clone(),
                    format!(
                        "parameter `{}: {}` cannot cross the C ABI by value: only transparent \
                         types (primitive / raw pointer / `#[repr(C)]` struct / all-unit \
                         `#[repr(C)]` enum) may be exported params.{}",
                        p.name().unwrap_or("_"),
                        type_display(&p.ty),
                        abi_fix_hint(
                            &p.ty,
                            BoundaryPosition::Param,
                            &all_structs,
                            &all_enums,
                            &repr_c
                        )
                    ),
                ));
            }
        }
    }
    errs
}

/// Which side of the boundary an offending type sits on — the fix differs
/// (a return can box or use an out-param; a param wants the `(ptr, len)` C
/// idiom).
#[derive(Clone, Copy, PartialEq, Eq)]
enum BoundaryPosition {
    Return,
    Param,
}

/// The actionable suffix for an `E_EXPORT_ABI` message — category-specific so
/// each rejected shape points at *its* real path across C, not a generic
/// catch-all. Order matters: the most specific match wins.
fn abi_fix_hint(
    te: &TypeExpr,
    pos: BoundaryPosition,
    all_structs: &std::collections::BTreeSet<String>,
    all_enums: &std::collections::BTreeSet<String>,
    repr_c: &std::collections::BTreeSet<String>,
) -> String {
    // Tuples have no C name at all.
    if let TypeKind::Tuple(elems) = &te.kind {
        if !elems.is_empty() {
            return " Tuples have no C representation — return a `#[repr(C)]` struct with named \
                    fields instead (or split into scalar out-params)."
                .to_string();
        }
    }
    if let TypeKind::Path(p) = &te.kind {
        if p.segments.len() == 1 {
            let n = &p.segments[0];
            // A user struct that merely lacks `#[repr(C)]` — the one-step fix.
            if all_structs.contains(n) && !repr_c.contains(n) {
                return format!(
                    " Add `#[repr(C)]` to `{n}` and it crosses transparently — the C header \
                     then declares the struct by value."
                );
            }
            // `Option[T]` — the discriminated-optional case. NULL is the
            // natural C sentinel for a pointer payload.
            if n == "Option" {
                return " `Option` has no by-value C representation: return a raw pointer whose \
                        NULL means `None`, or a `#[repr(C)]` struct with a present-flag plus the \
                        value. (repr(C) tagged-union enums are a planned follow-on.)"
                    .to_string();
            }
            // A user-defined enum (or `Result`) — a tagged value. An all-unit
            // `#[repr(C)]` enum already crosses as an integer (Slice 1); this
            // hint fires for the data-carrying / non-repr(C) case.
            if n == "Result" || all_enums.contains(n) {
                return " An all-unit enum crosses transparently once marked `#[repr(C)]` (its \
                        value is the discriminant). A data-carrying enum has no by-value C form \
                        yet: return a `#[repr(C)]` struct with a status/tag field plus the \
                        payload, or an opaque handle and accessor exports. (repr(C) tagged-union \
                        enums are a planned follow-on.)"
                    .to_string();
            }
            // An aggregate collection as a *param* — the C idiom is a pointer
            // and a length, not by-value (params never box).
            if pos == BoundaryPosition::Param && (n == "Vec" || n == "String") {
                return " Pass a raw pointer + length (the C `(ptr, len)` idiom) instead — \
                        aggregates cross by value only as auto-boxed *returns*, never as params."
                    .to_string();
            }
            // A `Vec` return past the boxable depth (one level of aggregate
            // nesting): the shape is right but too deep.
            if pos == BoundaryPosition::Return && n == "Vec" {
                return " Returns auto-box up to one level of aggregate nesting (`Vec[String]`, \
                        `Vec[Vec[scalar]]`); a deeper element type isn't boxed — flatten it, or \
                        return a raw pointer to a Kāra-owned box."
                    .to_string();
            }
        }
    }
    " Return/accept a `#[repr(C)]` struct or a raw pointer to a Kāra-owned box instead.".to_string()
}

/// Emit the C header text for `program`'s exported surface. `lib_name` is
/// the bare library stem (no `lib` prefix / extension) — it names the
/// include guard and appears in the banner.
pub fn emit_c_header(program: &Program, lib_name: &str) -> String {
    let exports: Vec<&Function> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::Function(f) if is_exported(f) => Some(f),
            _ => None,
        })
        .collect();

    // Index of `#[repr(C)]` structs by name — the transparent aggregate
    // set. A default-layout struct is intentionally absent (no stable C
    // layout), so it maps to an opaque handle like every other aggregate.
    let repr_c_structs: BTreeMap<&str, &StructDef> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::StructDef(s) if is_repr_c(s) => Some((s.name.as_str(), s)),
            _ => None,
        })
        .collect();

    // Index of all-unit `#[repr(C)]` enums by name — the transparent
    // integer-enum set (Slice 1). Data-carrying repr(C) enums are absent
    // (rejected upstream), so they never reach the header.
    let repr_c_enums: BTreeMap<&str, &EnumDef> = program
        .items
        .iter()
        .filter_map(|it| match it {
            Item::EnumDef(e) if is_repr_c_all_unit_enum(e) => Some((e.name.as_str(), e)),
            _ => None,
        })
        .collect();

    // Walk every exported signature, mapping each type to C and recording
    // which repr(C) structs / enums and whether an opaque handle are referenced.
    let mut needed_structs: BTreeSet<String> = BTreeSet::new();
    let mut needed_enums: BTreeSet<String> = BTreeSet::new();
    let mut used_opaque = false;
    let mut protos: Vec<String> = Vec::with_capacity(exports.len());
    for f in &exports {
        protos.push(render_prototype(
            f,
            &repr_c_structs,
            &repr_c_enums,
            &mut needed_structs,
            &mut needed_enums,
            &mut used_opaque,
        ));
    }

    let guard = guard_macro(lib_name);
    let mut out = String::new();
    out.push_str(&format!(
        "/* Generated by karac — C ABI for the `{lib_name}` Kāra library.\n\
         * Producer-mode artifact (design.md \u{00a7} Exported C ABI). Do not edit by hand.\n\
         *\n\
         * Link: cc your_host.c -l{lib_name} -lpthread -lm -ldl -o your_host\n\
         * (the static archive bundles the Kāra runtime; a cdylib pulls it in too).\n\
         *\n\
         * Rust hosts: link the cdylib (.so/.dylib/.dll), NOT the static\n\
         * archive. The Kāra runtime is a Rust crate that bundles std, so the\n\
         * .a carries std symbols (rust_eh_personality, allocator shims, ...)\n\
         * that collide with the Rust host's own std at static-link time. A\n\
         * shared library encapsulates those internal symbols; only the\n\
         * exported entry points below are visible. C/C++ hosts have no std to\n\
         * clash with and may link either artifact.\n\
         */\n"
    ));
    out.push_str(&format!("#ifndef {guard}\n#define {guard}\n\n"));
    out.push_str("#include <stdint.h>\n#include <stddef.h>\n\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");

    // Runtime lifecycle — always surfaced (real symbols in the runtime
    // archive; no-ops at v1). A host calls init once before the first
    // exported call and shutdown at teardown.
    out.push_str(
        "/* Runtime lifecycle. Call karac_runtime_init() once before the first\n\
         * exported call, and karac_runtime_shutdown() at host teardown. */\n\
         void karac_runtime_init(void);\n\
         void karac_runtime_shutdown(void);\n\n",
    );

    if used_opaque {
        out.push_str(
            "/* Opaque handle to a K\u{101}ra-owned value (Vec/String/enum/non-repr(C)\n\
             * struct/...). Passed by pointer only; never dereferenced or free()d by\n\
             * the C side \u{2014} return it to a K\u{101}ra-provided destructor. */\n\
             typedef void* KaraHandle;\n\n",
        );
    }

    // C-ABI auto-boxed return structs (Slice 4 Path B): the transparent
    // `{data,len,cap}` view a boxed `Vec[...]` / `String` handle points at.
    // Nested returns (`Vec[String]`, `Vec[Vec[scalar]]`) need the element
    // struct defined *before* the outer one — collect into an ordered map
    // that places dependencies first.
    let mut boxed_defs: Vec<(String, String)> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let push_def = |name: String,
                    elem_c: String,
                    defs: &mut Vec<(String, String)>,
                    seen: &mut BTreeSet<String>| {
        if seen.insert(name.clone()) {
            defs.push((name, elem_c));
        }
    };
    for f in &exports {
        match boxed_return_of(f) {
            Some(BoxedReturn::Flat {
                struct_name,
                elem_c,
            }) => {
                push_def(struct_name, elem_c.to_string(), &mut boxed_defs, &mut seen);
            }
            Some(BoxedReturn::Nested {
                outer_name,
                elem_struct,
                elem_elem_c,
            }) => {
                // Element struct first (dependency), then the outer.
                push_def(
                    elem_struct.clone(),
                    elem_elem_c.to_string(),
                    &mut boxed_defs,
                    &mut seen,
                );
                push_def(outer_name, elem_struct, &mut boxed_defs, &mut seen);
            }
            None => {}
        }
    }
    for (name, elem) in &boxed_defs {
        out.push_str(&format!(
            "/* Boxed K\u{101}ra value. Read `data[0..len]`, then free the handle via\n\
             * the matching karac_free_* export (never plain free()). */\n\
             typedef struct {{ {elem}* data; int64_t len; int64_t cap; }} {name};\n\n"
        ));
    }

    // repr(C) struct definitions, dependency-ordered (a struct used
    // by-value inside another must be defined first). Built into a string
    // *before* the enum block is emitted: a struct field may reference an
    // all-unit repr(C) enum, which registers it into `needed_enums` here so
    // the typedef below covers the field-only case too.
    let mut struct_body = String::new();
    if !needed_structs.is_empty() {
        let mut emitted: BTreeSet<String> = BTreeSet::new();
        for name in &needed_structs {
            emit_struct(
                name,
                &repr_c_structs,
                &repr_c_enums,
                &needed_structs,
                &mut emitted,
                &mut needed_enums,
                &mut struct_body,
            );
        }
    }

    // All-unit `#[repr(C)]` enum definitions (Slice 1). The value is the
    // discriminant, so the type is `int64_t` (matching Kāra's i64 tag width —
    // a bare C `enum` is `int`/4 B and would mismatch the by-value ABI) with
    // the variant names as readable named constants. Emitted before the
    // struct bodies so an enum-typed struct field resolves.
    for name in &needed_enums {
        if let Some(e) = repr_c_enums.get(name.as_str()) {
            out.push_str(&format!(
                "/* {name} — an all-unit `#[repr(C)]` K\u{101}ra enum; values are the \
                 discriminants. */\ntypedef int64_t {name};\nenum {{"
            ));
            let consts: Vec<String> = e
                .variants
                .iter()
                .enumerate()
                .map(|(i, v)| format!(" {name}_{} = {i}", v.name))
                .collect();
            out.push_str(&consts.join(","));
            out.push_str(" };\n\n");
        }
    }
    out.push_str(&struct_body);

    for proto in &protos {
        out.push_str(proto);
        out.push('\n');
    }

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    out.push_str(&format!("#endif /* {guard} */\n"));
    out
}

/// Render one exported function's prototype, including a Doxygen-style
/// `@effects` line (producer-side effects are KNOWN — checked against the
/// body — so the header states them precisely).
fn render_prototype(
    func: &Function,
    structs: &BTreeMap<&str, &StructDef>,
    enums: &BTreeMap<&str, &EnumDef>,
    needed: &mut BTreeSet<String>,
    needed_enums: &mut BTreeSet<String>,
    used_opaque: &mut bool,
) -> String {
    // C-ABI auto-boxed aggregate return (Slice 4 Path B): the export returns
    // an opaque pointer to a heap box holding a `{data,len,cap}` struct; the
    // C side reads its fields and frees it via `karac_free_<name>`.
    let boxed = boxed_return_of(func);
    let ret = match (&boxed, &func.return_type) {
        (Some(b), _) => format!("{}*", boxed_struct_name(b)),
        (None, None) => "void".to_string(),
        (None, Some(ty)) if is_unit(ty) => "void".to_string(),
        (None, Some(ty)) => c_type(ty, structs, enums, needed, needed_enums, used_opaque),
    };
    let params: Vec<String> = func
        .params
        .iter()
        .map(|p| {
            let cty = c_type(&p.ty, structs, enums, needed, needed_enums, used_opaque);
            match p.name() {
                Some(n) => format!("{cty} {n}"),
                None => cty,
            }
        })
        .collect();
    let param_list = if params.is_empty() {
        "void".to_string()
    } else {
        params.join(", ")
    };
    let mut s = String::new();
    if let Some(eff) = render_effects(func.effects.as_ref()) {
        s.push_str(&format!("/** @effects {eff} */\n"));
    }
    s.push_str(&format!("{ret} {}({param_list});", func.name));
    // Boxed return: emit the matching destructor prototype. The C caller
    // reads the returned handle's fields, then hands it back here to free
    // both the owned buffer and the box (never plain `free()`).
    if let Some(b) = &boxed {
        s.push_str(&format!(
            "\nvoid karac_free_{}({}* handle);",
            func.name,
            boxed_struct_name(b)
        ));
    }
    s
}

/// Map a Kāra type expression to its C rendering, recording referenced
/// repr(C) structs and all-unit repr(C) enums (into `needed` / `needed_enums`)
/// and whether an opaque handle was used.
fn c_type(
    ty: &TypeExpr,
    structs: &BTreeMap<&str, &StructDef>,
    enums: &BTreeMap<&str, &EnumDef>,
    needed: &mut BTreeSet<String>,
    needed_enums: &mut BTreeSet<String>,
    used_opaque: &mut bool,
) -> String {
    match &ty.kind {
        TypeKind::Unit => "void".to_string(),
        TypeKind::Tuple(elems) if elems.is_empty() => "void".to_string(),
        TypeKind::Pointer { is_mut, inner } => {
            let base = c_type(inner, structs, enums, needed, needed_enums, used_opaque);
            if *is_mut {
                format!("{base}*")
            } else {
                format!("const {base}*")
            }
        }
        TypeKind::Path(path) if path.segments.len() == 1 => {
            let name = &path.segments[0];
            if let Some(prim) = c_primitive(name) {
                prim.to_string()
            } else if structs.contains_key(name.as_str()) {
                needed.insert(name.clone());
                format!("struct {name}")
            } else if enums.contains_key(name.as_str()) {
                // All-unit `#[repr(C)]` enum → its `typedef int64_t <Name>`.
                needed_enums.insert(name.clone());
                name.clone()
            } else {
                *used_opaque = true;
                format!("KaraHandle /* {name} */")
            }
        }
        _ => {
            *used_opaque = true;
            format!("KaraHandle /* {} */", type_display(ty))
        }
    }
}

/// The C type for a Kāra primitive name, or `None` if `name` is not a
/// boundary-transparent primitive.
fn c_primitive(name: &str) -> Option<&'static str> {
    Some(match name {
        "i8" => "int8_t",
        "i16" => "int16_t",
        "i32" => "int32_t",
        "i64" => "int64_t",
        "u8" => "uint8_t",
        "u16" => "uint16_t",
        "u32" => "uint32_t",
        "u64" => "uint64_t",
        "f32" => "float",
        "f64" => "double",
        // C `_Bool` layout is not portably guaranteed across compilers;
        // `uint8_t` (0/1) is the stable boundary representation.
        "bool" => "uint8_t",
        // FFI-only size types (design.md \u{00a7} Numeric Semantics).
        "usize" => "size_t",
        "isize" => "ptrdiff_t",
        _ => return None,
    })
}

/// Emit a repr(C) struct definition into `body` in dependency order
/// (post-order DFS: a by-value struct field is defined before the struct
/// that uses it). `emitted` guards against re-emitting a shared struct.
fn emit_struct(
    name: &str,
    structs: &BTreeMap<&str, &StructDef>,
    enums: &BTreeMap<&str, &EnumDef>,
    needed: &BTreeSet<String>,
    emitted: &mut BTreeSet<String>,
    needed_enums: &mut BTreeSet<String>,
    body: &mut String,
) {
    if emitted.contains(name) {
        return;
    }
    let Some(def) = structs.get(name) else {
        return;
    };
    // Emit repr(C) struct dependencies referenced by-value first.
    for field in &def.fields {
        if let TypeKind::Path(p) = &field.ty.kind {
            if p.segments.len() == 1 {
                let dep = &p.segments[0];
                if structs.contains_key(dep.as_str()) && needed.contains(dep) {
                    emit_struct(dep, structs, enums, needed, emitted, needed_enums, body);
                }
            }
        }
    }
    emitted.insert(name.to_string());
    body.push_str(&format!("struct {name} {{\n"));
    for field in &def.fields {
        // Reuse the signature mapper; struct-emit-time opaque/needed-struct
        // recording is inert here (those header-level flags are already set
        // from signature mapping), so route through throwaway sinks — but a
        // field of all-unit repr(C) enum type must register into the REAL
        // `needed_enums` so its typedef is emitted even when no signature
        // references it directly.
        let mut throwaway_needed = BTreeSet::new();
        let mut throwaway_opaque = false;
        let cty = c_type(
            &field.ty,
            structs,
            enums,
            &mut throwaway_needed,
            needed_enums,
            &mut throwaway_opaque,
        );
        body.push_str(&format!("    {cty} {};\n", field.name));
    }
    body.push_str("};\n\n");
}

/// Render a declared effect list as a compact `verb(Resource), ...`
/// string for the `@effects` comment, or `None` when the function
/// declares no effects (pure).
fn render_effects(effects: Option<&EffectList>) -> Option<String> {
    let list = effects?;
    if list.items.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for item in &list.items {
        match item {
            EffectItem::Verb(v) => {
                let verb = verb_name(&v.kind);
                if v.resources.is_empty() {
                    parts.push(verb.to_string());
                } else {
                    let res: Vec<String> = v.resources.iter().map(|r| r.path.join(".")).collect();
                    parts.push(format!("{verb}({})", res.join(", ")));
                }
            }
            EffectItem::Group(g) => parts.push(g.clone()),
            EffectItem::Polymorphic => parts.push("_".to_string()),
            EffectItem::Variable(e) => parts.push(e.clone()),
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

fn verb_name(kind: &EffectVerbKind) -> String {
    match kind {
        EffectVerbKind::Reads => "reads".to_string(),
        EffectVerbKind::Writes => "writes".to_string(),
        EffectVerbKind::Sends => "sends".to_string(),
        EffectVerbKind::Receives => "receives".to_string(),
        EffectVerbKind::Allocates => "allocates".to_string(),
        EffectVerbKind::Panics => "panics".to_string(),
        EffectVerbKind::Blocks => "blocks".to_string(),
        EffectVerbKind::Suspends => "suspends".to_string(),
        EffectVerbKind::UserDefined(n) => n.clone(),
    }
}

/// True iff `ty` is the unit type (`()`), which maps to a `void` return.
fn is_unit(ty: &TypeExpr) -> bool {
    matches!(&ty.kind, TypeKind::Unit) || matches!(&ty.kind, TypeKind::Tuple(e) if e.is_empty())
}

/// True iff the attribute list carries `#[repr(C)]`.
fn attrs_have_repr_c(attributes: &[crate::ast::Attribute]) -> bool {
    use crate::ast::ExprKind;
    attributes.iter().any(|a| {
        a.is_bare("repr")
            && a.args.iter().any(|arg| {
                if arg.name.is_some() {
                    return false;
                }
                match arg.value.as_ref().map(|e| &e.kind) {
                    Some(ExprKind::Identifier(s)) => s == "C",
                    Some(ExprKind::Path { segments, .. }) => {
                        segments.len() == 1 && segments[0] == "C"
                    }
                    _ => false,
                }
            })
    })
}

/// True iff a struct carries `#[repr(C)]`.
fn is_repr_c(def: &StructDef) -> bool {
    attrs_have_repr_c(&def.attributes)
}

/// True iff `e` is a `#[repr(C)]` enum whose every variant is a unit variant
/// (no payload) — the only enum shape that crosses the C ABI transparently at
/// v1 (see `spikes/repr-c-tagged-union-enums.md` Slice 1). Its value is the
/// bare discriminant, so it maps to a C integer with named constants. A
/// `#[repr(C)]` enum with any data-carrying variant is NOT this — it stays
/// rejected (a real tagged union is Slice 2).
fn is_repr_c_all_unit_enum(e: &EnumDef) -> bool {
    attrs_have_repr_c(&e.attributes)
        && !e.is_shared
        && !e.is_par
        && !e.variants.is_empty()
        && e.variants
            .iter()
            .all(|v| matches!(v.kind, crate::ast::VariantKind::Unit))
}

/// A best-effort Kāra-surface rendering of a type expression for the
/// opaque-handle comment (`KaraHandle /* Vec[i32] */`).
fn type_display(ty: &TypeExpr) -> String {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.join("."),
        TypeKind::Ref(inner) => format!("ref {}", type_display(inner)),
        TypeKind::MutRef(inner) => format!("mut ref {}", type_display(inner)),
        TypeKind::Pointer { is_mut, inner } => {
            let q = if *is_mut { "mut" } else { "const" };
            format!("*{q} {}", type_display(inner))
        }
        TypeKind::Tuple(elems) if elems.is_empty() => "()".to_string(),
        _ => "value".to_string(),
    }
}

/// The include-guard macro for `lib_name`: uppercased, non-alphanumerics
/// collapsed to `_`, wrapped as `LIB<NAME>_H`.
fn guard_macro(lib_name: &str) -> String {
    let mut s = String::from("LIB");
    for ch in lib_name.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_uppercase());
        } else {
            s.push('_');
        }
    }
    s.push_str("_H");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_for(src: &str, name: &str) -> String {
        let parsed = crate::parse(src);
        emit_c_header(&parsed.program, name)
    }

    #[test]
    fn primitive_and_pointer_signature() {
        let src = "pub extern \"C\" fn saxpy(n: i64, a: f32, x: *const f32, y: *mut f32) { }";
        let h = header_for(src, "kernels");
        assert!(
            h.contains("void saxpy(int64_t n, float a, const float* x, float* y);"),
            "unexpected header:\n{h}"
        );
        // Guard + lifecycle + extern-C wrapper are always present.
        assert!(h.contains("#ifndef LIBKERNELS_H"));
        assert!(h.contains("void karac_runtime_init(void);"));
        assert!(h.contains("extern \"C\""));
    }

    #[test]
    fn no_params_renders_void() {
        let src = "pub extern \"C\" fn tick() -> i32 { 0 }";
        let h = header_for(src, "t");
        assert!(h.contains("int32_t tick(void);"), "unexpected header:\n{h}");
    }

    #[test]
    fn non_exported_fns_are_excluded() {
        // Not pub, and a pub non-extern fn — neither is part of the C surface.
        let src = "extern \"C\" fn helper() { }\npub fn compute() -> i32 { 1 }";
        let h = header_for(src, "lib");
        assert!(!h.contains("helper"), "private extern leaked:\n{h}");
        assert!(!h.contains("compute"), "non-extern pub fn leaked:\n{h}");
    }

    #[test]
    fn repr_c_struct_is_emitted() {
        let src = "#[repr(C)]\npub struct Point { x: f64, y: f64 }\n\
                   pub extern \"C\" fn origin_dist(p: Point) -> f64 { 0.0 }";
        let h = header_for(src, "geo");
        assert!(h.contains("struct Point {"), "struct missing:\n{h}");
        assert!(h.contains("double x;"), "field missing:\n{h}");
        assert!(
            h.contains("double origin_dist(struct Point p);"),
            "proto missing:\n{h}"
        );
    }

    #[test]
    fn genuinely_opaque_type_becomes_handle() {
        // `Option[i32]` is non-transparent and NOT a boxable Vec/String, so
        // it maps to the generic opaque `KaraHandle`.
        let src = "pub extern \"C\" fn build() -> Option[i32] { }";
        let h = header_for(src, "lib");
        assert!(
            h.contains("typedef void* KaraHandle;"),
            "typedef missing:\n{h}"
        );
        assert!(h.contains("KaraHandle"), "opaque return missing:\n{h}");
    }

    #[test]
    fn vec_scalar_return_auto_boxes_with_destructor() {
        // A `Vec[scalar]` return (Slice 4 Path B) crosses as a boxed
        // `{data,len,cap}` handle + an auto-emitted `karac_free_<name>`.
        let src = "pub extern \"C\" fn make() -> Vec[i64] { }";
        let h = header_for(src, "lib");
        assert!(
            h.contains(
                "typedef struct { int64_t* data; int64_t len; int64_t cap; } KaraVec_int64_t;"
            ),
            "boxed struct typedef missing:\n{h}"
        );
        assert!(
            h.contains("KaraVec_int64_t* make(void);"),
            "boxed return proto missing:\n{h}"
        );
        assert!(
            h.contains("void karac_free_make(KaraVec_int64_t* handle);"),
            "destructor proto missing:\n{h}"
        );
    }

    #[test]
    fn string_return_auto_boxes() {
        let src = "pub extern \"C\" fn greet() -> String { }";
        let h = header_for(src, "lib");
        assert!(
            h.contains("typedef struct { uint8_t* data; int64_t len; int64_t cap; } KaraString;"),
            "KaraString typedef missing:\n{h}"
        );
        assert!(
            h.contains("KaraString* greet(void);"),
            "proto missing:\n{h}"
        );
        assert!(
            h.contains("void karac_free_greet(KaraString* handle);"),
            "destructor missing:\n{h}"
        );
    }

    #[test]
    fn export_symbols_lists_fns_destructors_and_lifecycle() {
        // A transparent export contributes only its bare name; a boxed-return
        // export additionally contributes its `karac_free_<name>` destructor.
        // The two runtime lifecycle entry points always trail.
        let src = "pub extern \"C\" fn saxpy(n: i64) { }\n\
                   pub extern \"C\" fn make() -> Vec[i64] { }\n\
                   pub fn helper() -> i32 { 1 }";
        let parsed = crate::parse(src);
        let syms = export_symbols(&parsed.program);
        assert_eq!(
            syms,
            vec![
                "saxpy".to_string(),
                "make".to_string(),
                "karac_free_make".to_string(),
                "karac_runtime_init".to_string(),
                "karac_runtime_shutdown".to_string(),
            ],
            "unexpected export symbol set: {syms:?}"
        );
        // A non-exported (non-extern) fn never appears.
        assert!(!syms.iter().any(|s| s == "helper"));
    }

    fn exported_fn(src: &str) -> &'static crate::ast::Function {
        // Leak the parsed program so we can hand a `&Function` to the
        // boxed-return predicates from a test.
        let parsed = Box::leak(Box::new(crate::parse(src)));
        parsed
            .program
            .items
            .iter()
            .find_map(|it| match it {
                crate::ast::Item::Function(f) if is_exported(f) => Some(f),
                _ => None,
            })
            .expect("one exported fn")
    }

    #[test]
    fn vec_string_return_nests_transparently() {
        // Path-B follow-on: `Vec[String]` → nested `{KaraString* data; ...}`,
        // element struct defined before the outer one.
        let src = "pub extern \"C\" fn names() -> Vec[String] { }";
        let h = header_for(src, "lib");
        let ks = h.find("} KaraString;").expect("KaraString def");
        let outer = h
            .find("} KaraVec_KaraString;")
            .expect("KaraVec_KaraString def");
        assert!(ks < outer, "element struct must precede outer:\n{h}");
        assert!(
            h.contains(
                "typedef struct { KaraString* data; int64_t len; int64_t cap; } KaraVec_KaraString;"
            ),
            "nested typedef missing:\n{h}"
        );
        assert!(
            h.contains("KaraVec_KaraString* names(void);"),
            "proto:\n{h}"
        );
        assert!(
            h.contains("void karac_free_names(KaraVec_KaraString* handle);"),
            "destructor:\n{h}"
        );
        assert!(boxed_return_elements_need_drop(exported_fn(src)));
    }

    #[test]
    fn vec_vec_scalar_return_nests() {
        let src = "pub extern \"C\" fn grid() -> Vec[Vec[i64]] { }";
        let h = header_for(src, "lib");
        assert!(
            h.contains("KaraVec_int64_t* data;"),
            "inner element pointer missing:\n{h}"
        );
        assert!(
            h.contains("KaraVec_KaraVec_int64_t* grid(void);"),
            "proto:\n{h}"
        );
    }

    #[test]
    fn non_boxable_return_and_param_are_flagged() {
        // The C-ABI honesty gate: `Option` return + `Vec` param each yield an
        // export error (else a dishonest `KaraHandle` would miscompile).
        let opt = crate::parse("pub extern \"C\" fn m() -> Option[i64] { }");
        assert!(
            validate_exports(&opt.program).iter().any(|(n, _)| n == "m"),
            "Option return not flagged"
        );
        let param = crate::parse("pub extern \"C\" fn t(v: Vec[i64]) -> i64 { 0 }");
        assert!(
            validate_exports(&param.program)
                .iter()
                .any(|(n, _)| n == "t"),
            "Vec param not flagged"
        );
        // A clean transparent + boxable surface flags nothing.
        let ok = crate::parse(
            "pub extern \"C\" fn a(x: i32) -> i32 { x }\n\
             pub extern \"C\" fn b() -> Vec[i64] { }",
        );
        assert!(
            validate_exports(&ok.program).is_empty(),
            "clean surface flagged"
        );
    }

    #[test]
    fn reject_hints_are_category_specific() {
        // Each rejected shape points at ITS real path, not the generic hint.
        let hint_for = |src: &str, fname: &str| -> String {
            let p = crate::parse(src);
            validate_exports(&p.program)
                .into_iter()
                .find(|(n, _)| n == fname)
                .map(|(_, r)| r)
                .unwrap_or_default()
        };

        // Option return → NULL-pointer / present-flag guidance.
        let opt = hint_for("pub extern \"C\" fn m() -> Option[i64] { }", "m");
        assert!(opt.contains("`Option` has no by-value"), "Option: {opt}");

        // Result / user enum return → all-unit repr(C) path + data-carrying
        // tag-field / accessor guidance.
        let res = hint_for("pub extern \"C\" fn r() -> Result[i64, i64] { }", "r");
        assert!(
            res.contains("data-carrying enum has no by-value"),
            "Result: {res}"
        );
        let en = hint_for(
            "pub enum Color { Red, Green }\npub extern \"C\" fn c() -> Color { }",
            "c",
        );
        assert!(
            en.contains("crosses transparently once marked `#[repr(C)]`"),
            "enum: {en}"
        );

        // Vec/String as a PARAM → (ptr, len) idiom (not the return box hint).
        let vp = hint_for("pub extern \"C\" fn t(v: Vec[i64]) -> i64 { 0 }", "t");
        assert!(vp.contains("`(ptr, len)` idiom"), "Vec param: {vp}");

        // Tuple return → repr(C) struct with named fields.
        let tup = hint_for("pub extern \"C\" fn p() -> (i64, i64) { }", "p");
        assert!(
            tup.contains("Tuples have no C representation"),
            "tuple: {tup}"
        );
    }

    #[test]
    fn all_unit_repr_c_enum_crosses_transparently() {
        // A `#[repr(C)]` all-unit enum is accepted as return AND param, and
        // the header emits `typedef int64_t <Name>` + named constants; the
        // prototype uses the enum name (an int64_t alias) by value.
        let src = "#[repr(C)]\npub enum Status { Ok, NotFound, Denied }\n\
                   pub extern \"C\" fn lookup(s: Status) -> Status { s }";
        let p = crate::parse(src);
        assert!(
            validate_exports(&p.program).is_empty(),
            "all-unit repr(C) enum should be accepted: {:?}",
            validate_exports(&p.program)
        );
        let h = emit_c_header(&p.program, "lib");
        assert!(
            h.contains("typedef int64_t Status;"),
            "enum typedef missing:\n{h}"
        );
        assert!(
            h.contains("Status_Ok = 0")
                && h.contains("Status_NotFound = 1")
                && h.contains("Status_Denied = 2"),
            "enum constants missing:\n{h}"
        );
        assert!(
            h.contains("Status lookup(Status s);"),
            "prototype should use the enum name by value:\n{h}"
        );
        // No opaque handle for this surface.
        assert!(!h.contains("KaraHandle"), "should not be opaque:\n{h}");
    }

    #[test]
    fn data_carrying_repr_c_enum_still_rejected() {
        // `#[repr(C)]` on a data-carrying enum is NOT transparent at v1 (the
        // tagged-union case is Slice 2) — it stays an export error.
        let src = "#[repr(C)]\npub enum Msg { Ping, Data(i64) }\n\
                   pub extern \"C\" fn recv() -> Msg { Msg.Ping }";
        let p = crate::parse(src);
        assert!(
            validate_exports(&p.program)
                .iter()
                .any(|(n, _)| n == "recv"),
            "data-carrying repr(C) enum must still be rejected"
        );
    }

    #[test]
    fn plain_struct_return_suggests_repr_c() {
        // A user struct that just lacks `#[repr(C)]` gets the one-step fix,
        // not the generic hint. Adding the attribute clears the error.
        let bad = crate::parse(
            "pub struct Point { x: f64, y: f64 }\n\
             pub extern \"C\" fn origin() -> Point { }",
        );
        let errs = validate_exports(&bad.program);
        let (_, reason) = errs.iter().find(|(n, _)| n == "origin").expect("flagged");
        assert!(
            reason.contains("Add `#[repr(C)]` to `Point`"),
            "expected repr(C) hint, got: {reason}"
        );

        let good = crate::parse(
            "#[repr(C)]\npub struct Point { x: f64, y: f64 }\n\
             pub extern \"C\" fn origin() -> Point { }",
        );
        assert!(
            validate_exports(&good.program).is_empty(),
            "repr(C) struct return should be accepted"
        );
    }

    #[test]
    fn declared_effects_render_as_doc_comment() {
        let src = "pub extern \"C\" fn touch(fd: i32) with writes(FileSystem), blocks { }";
        let h = header_for(src, "io");
        assert!(
            h.contains("@effects writes(FileSystem), blocks"),
            "effects comment missing:\n{h}"
        );
    }
}
