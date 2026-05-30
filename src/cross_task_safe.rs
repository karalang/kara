//! Cross-task-safe type-tree walker.
//!
//! Phase 6 line 170 slices 1 + 2 — the closed cross-task-unsafe
//! enumeration + transitive walker that boundary-site enforcement
//! (slice 3) consults at every `spawn(closure)` / `par {}` /
//! `TaskGroup.spawn(closure)` / `Channel.send(val)` /
//! `with_provider(provider, closure)` call site.
//!
//! Spec at `design.md § Structured Concurrency Lifetime Guarantees`
//! (v60 item 48). Replaces the deferred Send + Sync auto-trait
//! approach with a closed structural list — no user-extensible
//! auto-trait inference at v1.
//!
//! ## v1 unsafe set
//!
//! Five hardcoded type shapes are NOT cross-task-safe:
//!
//! - **`shared struct S { ... }`** (any S) — `Rc`-rooted reachability
//!   means a sibling task can observe a partially-cleaned-up handle
//!   if drop fires under cancellation. Fix-it: `par struct S`.
//! - **`shared enum E { ... }`** (any E) — symmetric.
//! - **`Rc[T]`** — single-task RC, not atomic. Fix-it: `Arc[T]`.
//! - **`OnceCell[T]`** — single-task once-init. Fix-it: `OnceLock[T]`.
//! - **`*const T` / `*mut T`** — raw pointers carry no ownership
//!   tracking. Fix-it: wrap in `Atomic[*mut T]` or transfer through a
//!   channel.
//!
//! ## Walker behavior
//!
//! [`is_cross_task_safe`] walks the type tree depth-first. On the first
//! unsafe leaf it returns `Err(CrossTaskUnsafePath)` carrying the
//! field/element/variant path from the root to the leaf. The path
//! reads naturally in a diagnostic (`type 'Foo' contains 'Rc[String]'
//! at field 'cache'`).
//!
//! Recurses into:
//! - `Tuple` components
//! - `Array` / `Slice` element type
//! - `Named { name, args }` type args (so `Vec[Rc[T]]` catches the
//!   inner Rc) + user struct fields / enum variant payloads via
//!   `TypeCheckResult.struct_info` / `enum_info`
//! - `Arc(inner)` — Arc itself is safe but its contents might not be
//!   (`Arc[Rc[T]]` still has Rc inside)
//! - `Ref(inner)` / `MutRef(inner)` / `Weak(inner)` — the borrow target's
//!   transitivity matters
//!
//! Does NOT recurse into:
//! - `Function` / `OnceFunction` — closures are checked at their own
//!   boundary site (their capture set), not transitively through the
//!   function type
//! - `ImplTrait` / `Dyn` — unresolved at the type layer
//! - `TypeParam` / `TypeVar` / `AssocProjection` — conservatively
//!   accepted; the check fires post-monomorphization in slice 3, so
//!   unresolved type parameters at this layer can't carry an unsafe
//!   leaf yet
//!
//! ## What this module does NOT do (yet)
//!
//! Slices 3–6 of the line-170 entry: boundary-site enforcement at the
//! five call shapes, the `E_NOT_CROSS_TASK` diagnostic + cli
//! integration, borrow-rule consolidation, full test coverage. This
//! slice ships the standalone walker + unit tests so the integration
//! slice has a stable input contract.

use crate::typechecker::env::{EnumInfo, StructInfo};
use crate::typechecker::types::{type_display, Type};
use crate::typechecker::TypeCheckResult;
use std::collections::HashMap;

/// Diagnostic-shaped path through the type tree from a root binding's
/// type to the cross-task-unsafe leaf that's transitively reachable.
///
/// `root` is the formatted type name of the original binding. `path` is
/// the chain of field / element / variant labels from `root` down to
/// the unsafe leaf — empty when the root itself is unsafe.
///
/// `unsafe_leaf` is the formatted name of the unsafe type at the leaf.
/// `fix_it` carries the canonical replacement to suggest in the
/// `E_NOT_CROSS_TASK` diagnostic; the cli formatter dispatches on it.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossTaskUnsafePath {
    pub root: String,
    pub path: Vec<String>,
    pub unsafe_leaf: String,
    pub fix_it: CrossTaskUnsafeFixIt,
}

/// Hardcoded replacement suggestion per the spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossTaskUnsafeFixIt {
    /// `Rc[T]` → `Arc[T]`.
    RcToArc,
    /// `shared struct X` / `shared enum X` → `par struct X` / `par enum X`.
    SharedToPar,
    /// `OnceCell[T]` → `OnceLock[T]`.
    OnceCellToLock,
    /// Raw pointers — wrap in `Atomic[*mut T]` or transfer through a channel.
    RawPointer,
}

impl CrossTaskUnsafeFixIt {
    /// Render the replacement-suggestion text for the help line of the
    /// `E_NOT_CROSS_TASK` diagnostic.
    pub fn help_text(&self, unsafe_leaf: &str) -> String {
        match self {
            CrossTaskUnsafeFixIt::RcToArc => format!(
                "replace `{}` with `Arc[...]` to allow cross-task sharing",
                unsafe_leaf
            ),
            CrossTaskUnsafeFixIt::SharedToPar => format!(
                "replace `{}` with the `par` form of the same type to allow cross-task sharing",
                unsafe_leaf
            ),
            CrossTaskUnsafeFixIt::OnceCellToLock => format!(
                "replace `{}` with `OnceLock[...]` to allow cross-task sharing",
                unsafe_leaf
            ),
            CrossTaskUnsafeFixIt::RawPointer => format!(
                "wrap `{}` in `Atomic[*mut T]` or transfer ownership through a channel",
                unsafe_leaf
            ),
        }
    }
}

/// Walk `ty` against the cross-task-unsafe set. Returns `Ok(())` when
/// every reachable leaf is safe; returns `Err(path)` on the first
/// unsafe leaf encountered.
///
/// `types` is consulted for struct field / enum variant transitivity:
/// `Named { name, args }` shapes that resolve to a user-defined struct
/// in `types.struct_info` walk each field's type; same for `enum_info`.
pub fn is_cross_task_safe(ty: &Type, types: &TypeCheckResult) -> Result<(), CrossTaskUnsafePath> {
    is_cross_task_safe_with(ty, &types.struct_info, &types.enum_info)
}

/// Variant of [`is_cross_task_safe`] that takes the struct / enum index
/// maps directly. Used by the boundary-site enforcement check (slice 3)
/// which runs mid-typecheck — the canonical `TypeCheckResult` hasn't
/// been materialised yet, but `TypeChecker.env.structs` / `env.enums`
/// hold the same shape data.
pub fn is_cross_task_safe_with(
    ty: &Type,
    struct_info: &HashMap<String, StructInfo>,
    enum_info: &HashMap<String, EnumInfo>,
) -> Result<(), CrossTaskUnsafePath> {
    let root = type_display(ty);
    let mut path: Vec<String> = Vec::new();
    walk(ty, struct_info, enum_info, &mut path, &root)
}

fn walk(
    ty: &Type,
    struct_info: &HashMap<String, StructInfo>,
    enum_info: &HashMap<String, EnumInfo>,
    path: &mut Vec<String>,
    root: &str,
) -> Result<(), CrossTaskUnsafePath> {
    // Immediate-hit checks — these are the v1 unsafe set's leaf shapes.
    match ty {
        Type::Shared(name) => {
            return Err(CrossTaskUnsafePath {
                root: root.to_string(),
                path: path.clone(),
                unsafe_leaf: name.clone(),
                fix_it: CrossTaskUnsafeFixIt::SharedToPar,
            });
        }
        Type::Rc(_) => {
            return Err(CrossTaskUnsafePath {
                root: root.to_string(),
                path: path.clone(),
                unsafe_leaf: type_display(ty),
                fix_it: CrossTaskUnsafeFixIt::RcToArc,
            });
        }
        Type::Pointer { .. } => {
            return Err(CrossTaskUnsafePath {
                root: root.to_string(),
                path: path.clone(),
                unsafe_leaf: type_display(ty),
                fix_it: CrossTaskUnsafeFixIt::RawPointer,
            });
        }
        Type::Named { name, .. } if name == "OnceCell" => {
            return Err(CrossTaskUnsafePath {
                root: root.to_string(),
                path: path.clone(),
                unsafe_leaf: type_display(ty),
                fix_it: CrossTaskUnsafeFixIt::OnceCellToLock,
            });
        }
        _ => {}
    }

    // Recursive cases — walk every reachable sub-type.
    match ty {
        Type::Tuple(elems) => {
            for (i, elem) in elems.iter().enumerate() {
                path.push(format!("tuple element {}", i));
                walk(elem, struct_info, enum_info, path, root)?;
                path.pop();
            }
        }
        Type::Array { element, .. } => {
            path.push("array element".to_string());
            walk(element, struct_info, enum_info, path, root)?;
            path.pop();
        }
        Type::Slice { element, .. } => {
            path.push("slice element".to_string());
            walk(element, struct_info, enum_info, path, root)?;
            path.pop();
        }
        Type::Arc(inner) => {
            // Arc itself is safe but its inner might transitively reach
            // an unsafe leaf — `Arc[Rc[T]]` is still bad.
            path.push("Arc inner".to_string());
            walk(inner, struct_info, enum_info, path, root)?;
            path.pop();
        }
        Type::Ref(inner) | Type::MutRef(inner) | Type::Weak(inner) => {
            walk(inner, struct_info, enum_info, path, root)?;
        }
        // A refinement is structurally its base — walk through to catch an
        // unsafe leaf reachable via the refined type's base (e.g. a
        // refinement over `Rc[T]`).
        Type::Refinement { base, .. } => {
            walk(base, struct_info, enum_info, path, root)?;
        }
        Type::Named { name, args } => {
            // Walk type args first — `Vec[Rc[T]]` catches Rc here.
            for (i, arg) in args.iter().enumerate() {
                path.push(format!("`{}` arg {}", name, i));
                walk(arg, struct_info, enum_info, path, root)?;
                path.pop();
            }
            // Then transitively walk user struct fields / enum variants.
            if let Some(info) = struct_info.get(name) {
                if info.is_shared {
                    return Err(CrossTaskUnsafePath {
                        root: root.to_string(),
                        path: path.clone(),
                        unsafe_leaf: format!("shared struct {}", name),
                        fix_it: CrossTaskUnsafeFixIt::SharedToPar,
                    });
                }
                for (field_name, field_ty, _is_pub) in &info.fields {
                    path.push(format!("field '{}'", field_name));
                    walk(field_ty, struct_info, enum_info, path, root)?;
                    path.pop();
                }
            } else if let Some(info) = enum_info.get(name) {
                if info.is_shared {
                    return Err(CrossTaskUnsafePath {
                        root: root.to_string(),
                        path: path.clone(),
                        unsafe_leaf: format!("shared enum {}", name),
                        fix_it: CrossTaskUnsafeFixIt::SharedToPar,
                    });
                }
                for (variant_name, variant_info) in &info.variants {
                    use crate::typechecker::types::VariantTypeInfo;
                    match variant_info {
                        VariantTypeInfo::Unit => {}
                        VariantTypeInfo::Tuple(fields) => {
                            for (i, field_ty) in fields.iter().enumerate() {
                                path.push(format!("variant '{}' payload {}", variant_name, i));
                                walk(field_ty, struct_info, enum_info, path, root)?;
                                path.pop();
                            }
                        }
                        VariantTypeInfo::Struct(fields) => {
                            for (field_name, field_ty) in fields {
                                path.push(format!(
                                    "variant '{}' field '{}'",
                                    variant_name, field_name
                                ));
                                walk(field_ty, struct_info, enum_info, path, root)?;
                                path.pop();
                            }
                        }
                    }
                }
            }
        }
        // Safe leaves — primitives + Unit + Never.
        Type::Int(_)
        | Type::UInt(_)
        | Type::Float(_)
        | Type::Bool
        | Type::Char
        | Type::Str
        | Type::Unit
        | Type::Never => {}
        // Conservatively safe — these don't carry a resolved unsafe
        // leaf yet at this layer. Boundary-site enforcement (slice 3)
        // fires post-monomorphization so concrete substitutions ARE
        // walked.
        Type::TypeParam(_) | Type::TypeVar(_) | Type::AssocProjection { .. } => {}
        // Opaque — walker doesn't recurse. The function value's
        // capture set is checked at the boundary site where the
        // function is constructed (the closure literal), not at every
        // place the function-typed value flows.
        Type::Function { .. } | Type::OnceFunction { .. } => {}
        // Unresolved existential trait shape — accepted at this
        // layer; concrete witnesses are walked when they're
        // substituted in.
        Type::Existential { .. } => {}
        // Already returned via early-hit match above.
        Type::Shared(_) | Type::Rc(_) | Type::Pointer { .. } => unreachable!(),
        // Type::Error — accepted (downstream diagnostics already cover
        // the malformed-type case).
        Type::Error => {}
    }
    Ok(())
}

// Unit tests are at the integration level: `tests/cross_task_safe.rs`
// drives the walker against types produced by the real `karac::typecheck`
// pipeline. The walker itself is small + self-contained so the
// integration-level tests cover both direct-hit and transitive cases
// without a parallel hand-built `TypeCheckResult` fixture. This file
// intentionally has no `#[cfg(test)] mod tests` block — `TypeCheckResult`
// doesn't derive `Default` (it carries 30+ pipeline-populated fields)
// so fixture-based unit tests would be either fragile (full manual
// construction) or duplicative (a parallel default-impl).
