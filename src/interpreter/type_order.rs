//! Derived-`Ord` declaration-order registry for the tree-walk interpreter
//! (bug-ledger B-2026-07-03-12).
//!
//! `value_compare` (src/interpreter/helpers.rs) is a *free* function that also
//! backs `OrdValue::cmp`, which `BTreeMap` calls deep inside `insert` with no
//! `Interpreter` / type-registry handle. Before this module it ordered two
//! structs by ALPHABETICAL field name and two enum variants by variant NAME —
//! which preserves membership/dedup/count (the data-loss class fixed in
//! B-2026-07-03-6) but yields the WRONG observed SEQUENCE for
//! `Vec[Struct].sort()` / `SortedSet` / `SortedMap` iteration: a struct
//! `{ width, height }` would sort by `height` first, and an enum
//! `Priority { Low, Med, High }` would sort alphabetically (`High < Low < Med`)
//! instead of the derived-`Ord` declaration order `Low < Med < High`.
//!
//! The order is a static property of each type, held in
//! `TypeCheckResult.{struct_info,enum_info}` in declaration order (both are
//! `Vec`s). We can't thread that into the free `value_compare`, and we can't
//! embed it into `Value::Struct` / `Value::EnumVariant` without touching ~280
//! construction sites. Instead we stash a per-thread registry that
//! `value_compare` reads.
//!
//! **Installation chokepoint:** [`install`] runs from `Interpreter::new`, which
//! EVERY evaluation path funnels through — `run` / `run_test_function` / the
//! REPL / the comptime fold pass, and crucially each `par {}` branch (which
//! constructs its own branch `Interpreter::new(prog, tc)` *on its spawned
//! thread* in `eval_stmt.rs`). So a thread-local, set in `new`, is
//! automatically present on every thread that can reach `value_compare` — no
//! RAII guard and no cross-thread propagation needed. When the registry is
//! absent (a bare `value_compare` outside any interpreter run, or a type with
//! no entry) the comparators fall back to the prior alphabetical order, so the
//! no-data-loss guarantee from B-2026-07-03-6 is unconditional.

use std::collections::HashMap;
use std::rc::Rc;

use crate::typechecker::{TypeCheckResult, VariantTypeInfo};

/// Declaration-order lookup tables for user structs and enums.
#[derive(Debug, Default)]
pub(super) struct TypeOrderRegistry {
    /// struct name → (field name → declaration index)
    struct_fields: HashMap<String, HashMap<String, u32>>,
    /// enum name → (variant name → declaration index)
    enum_variants: HashMap<String, HashMap<String, u32>>,
    /// `"{enum}::{variant}"` → (field name → declaration index), for the
    /// struct-shaped enum variant payloads.
    variant_fields: HashMap<String, HashMap<String, u32>>,
}

impl TypeOrderRegistry {
    /// Field declaration-order map for a struct type, if known.
    pub(super) fn struct_field_order(&self, type_name: &str) -> Option<&HashMap<String, u32>> {
        self.struct_fields.get(type_name)
    }

    /// Variant declaration-order map for an enum type, if known.
    pub(super) fn enum_variant_order(&self, enum_name: &str) -> Option<&HashMap<String, u32>> {
        self.enum_variants.get(enum_name)
    }

    /// Field declaration-order map for a struct-shaped enum variant payload.
    pub(super) fn variant_field_order(
        &self,
        enum_name: &str,
        variant: &str,
    ) -> Option<&HashMap<String, u32>> {
        self.variant_fields.get(&variant_key(enum_name, variant))
    }
}

fn variant_key(enum_name: &str, variant: &str) -> String {
    format!("{enum_name}::{variant}")
}

thread_local! {
    /// The registry for the interpreter currently running on this thread. An
    /// `Rc` (not `Arc`) because it never crosses a thread boundary — each
    /// thread installs its own via `Interpreter::new`.
    static TYPE_ORDER: std::cell::RefCell<Option<Rc<TypeOrderRegistry>>> =
        const { std::cell::RefCell::new(None) };
}

/// Build the registry from a `TypeCheckResult` and install it as this thread's
/// active declaration-order source. Called from `Interpreter::new`.
pub(super) fn install(tc: &TypeCheckResult) {
    let reg = build(tc);
    TYPE_ORDER.with(|cell| *cell.borrow_mut() = Some(Rc::new(reg)));
}

/// Clone out this thread's active registry (cheap `Rc` refcount bump), so the
/// caller holds no `RefCell` borrow across the recursive `value_compare`.
pub(super) fn current() -> Option<Rc<TypeOrderRegistry>> {
    TYPE_ORDER.with(|cell| cell.borrow().clone())
}

fn build(tc: &TypeCheckResult) -> TypeOrderRegistry {
    let mut struct_fields = HashMap::new();
    for (name, info) in &tc.struct_info {
        let order = info
            .fields
            .iter()
            .enumerate()
            .map(|(i, (fname, _, _))| (fname.clone(), i as u32))
            .collect();
        struct_fields.insert(name.clone(), order);
    }

    let mut enum_variants = HashMap::new();
    let mut variant_fields = HashMap::new();
    for (name, info) in &tc.enum_info {
        let vorder = info
            .variants
            .iter()
            .enumerate()
            .map(|(i, (vname, _))| (vname.clone(), i as u32))
            .collect();
        enum_variants.insert(name.clone(), vorder);

        for (vname, vinfo) in &info.variants {
            if let VariantTypeInfo::Struct(fields) = vinfo {
                let forder = fields
                    .iter()
                    .enumerate()
                    .map(|(i, (fname, _))| (fname.clone(), i as u32))
                    .collect();
                variant_fields.insert(variant_key(name, vname), forder);
            }
        }
    }

    TypeOrderRegistry {
        struct_fields,
        enum_variants,
        variant_fields,
    }
}
