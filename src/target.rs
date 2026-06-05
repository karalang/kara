//! `#[target(...)]` gating — phase-10, `syntax.md § 8` / `design.md §
//! Cross-target Compilation`.
//!
//! The attribute's argument grammar is a comma-separated list of bare
//! target names, each optionally wrapped in `not(...)` — no general
//! boolean logic, and mixing positive and negative names in one
//! attribute is rejected at parse (the combination has no defined
//! semantics in the v1 spec). Names come from the CLOSED v1 set
//! [`V1_TARGETS`]; unknown names are parse-level diagnostics (see
//! `parser/attributes.rs`).
//!
//! Semantics: an item whose `#[target(...)]` does not match the current
//! compilation target is **treated as absent at resolution time** — the
//! item (body included — it may reference target-specific names) never
//! reaches the resolver, typechecker, effect checker, interpreter, or
//! codegen. [`filter_inactive_items`] performs that removal and returns
//! tombstones (name → rendered spec) so the resolver can answer
//! references from active code with "not available on target X" instead
//! of a bare undefined-name error.
//!
//! The current target is [`CURRENT_TARGET`] (`"native"`) until the
//! `--target` cross-compile flag lands (separate phase-10 entry); both
//! filter call sites (`cli::Pipeline::resolve` for single-file,
//! `module::build_program_tree` for project mode) thread it from here so
//! the flag only has to swap this one source of truth.

use crate::ast::{Attribute, Expr, ExprKind, Item, Program};
use std::collections::HashMap;

/// The closed v1 target-name set. Order is the canonical listing used in
/// diagnostics.
pub const V1_TARGETS: &[&str] = &["native", "wasm_browser", "wasm_wasi", "gpu"];

/// The compilation target every karac build produces today.
pub const CURRENT_TARGET: &str = "native";

/// Parsed form of one `#[target(...)]` attribute. Per the no-boolean-
/// logic rule the list is either all positive or all negative — the
/// parser rejects mixed lists, so `negated` applies to the whole set.
#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub names: Vec<String>,
    pub negated: bool,
}

impl TargetSpec {
    /// Does this spec admit `target`? Positive list: membership.
    /// Negative list: non-membership.
    pub fn is_active_on(&self, target: &str) -> bool {
        let listed = self.names.iter().any(|n| n == target);
        if self.negated {
            !listed
        } else {
            listed
        }
    }

    /// Canonical rendering for diagnostics — `wasm_browser, wasm_wasi`
    /// or `not(gpu)`.
    pub fn render(&self) -> String {
        if self.negated {
            self.names
                .iter()
                .map(|n| format!("not({n})"))
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            self.names.join(", ")
        }
    }
}

/// Extract the `TargetSpec` from an item's attribute list. Assumes the
/// parser already validated the shape (closed set, no mixed lists, at
/// most one `#[target]` per item) — unparseable args are skipped here
/// rather than re-diagnosed, so error recovery doesn't double-report.
pub fn target_spec_of(attrs: &[Attribute]) -> Option<TargetSpec> {
    let attr = attrs.iter().find(|a| a.is_bare("target"))?;
    let mut names = Vec::new();
    let mut negated = false;
    for arg in &attr.args {
        match arg.value.as_ref().map(|v| &v.kind) {
            Some(ExprKind::Identifier(n)) => names.push(n.clone()),
            Some(ExprKind::Unary {
                op: crate::ast::UnaryOp::Not,
                operand,
            }) => {
                if let ExprKind::Identifier(n) = &operand.kind {
                    names.push(n.clone());
                    negated = true;
                }
            }
            _ => {}
        }
    }
    if names.is_empty() {
        return None;
    }
    Some(TargetSpec { names, negated })
}

/// Validation used by the parser: is `expr` a bare target name or a
/// `not(<target>)` wrap? Returns `(name, negated)` on shape match —
/// name-set membership is the caller's check so it can render the
/// closed-set diagnostic with the offending name.
pub fn classify_target_arg(expr: &Expr) -> Option<(String, bool)> {
    match &expr.kind {
        ExprKind::Identifier(n) => Some((n.clone(), false)),
        // `not` is the logical-not KEYWORD, so the surface form
        // `not(gpu)` parses as a unary expression over a (possibly
        // parenthesized) identifier — not as a call named "not".
        ExprKind::Unary {
            op: crate::ast::UnaryOp::Not,
            operand,
        } => match &operand.kind {
            ExprKind::Identifier(n) => Some((n.clone(), true)),
            _ => None,
        },
        _ => None,
    }
}

/// The name a tombstone is filed under for a top-level item, alongside
/// the item's attribute list. Items that carry no attributes (imports,
/// use/alias decls, test cases, …) cannot be target-gated and return
/// `None`.
fn item_attrs_and_name(item: &Item) -> Option<(&[Attribute], Option<&str>)> {
    match item {
        Item::Function(f) => Some((&f.attributes, Some(&f.name))),
        Item::StructDef(s) => Some((&s.attributes, Some(&s.name))),
        Item::EnumDef(e) => Some((&e.attributes, Some(&e.name))),
        Item::TraitDef(t) => Some((&t.attributes, Some(&t.name))),
        Item::ConstDecl(c) => Some((&c.attributes, Some(&c.name))),
        Item::TypeAlias(t) => Some((&t.attributes, Some(&t.name))),
        Item::DistinctType(d) => Some((&d.attributes, Some(&d.name))),
        Item::ExternFunction(e) => Some((&e.attributes, Some(&e.name))),
        Item::ModuleBinding(b) => Some((&b.attributes, Some(&b.name))),
        // Impl blocks are target-gatable but nameless — dropping one
        // makes its methods absent, which surfaces through method
        // resolution rather than a named tombstone.
        Item::ImplBlock(i) => Some((&i.attributes, None)),
        _ => None,
    }
}

/// Remove every top-level item whose `#[target(...)]` does not admit
/// `current_target`. Returns tombstones: item name → rendered spec, for
/// resolver diagnostics at reference sites.
pub fn filter_inactive_items(
    program: &mut Program,
    current_target: &str,
) -> HashMap<String, String> {
    filter_inactive_items_in(&mut program.items, current_target)
}

/// Item-vec form of [`filter_inactive_items`] — used by
/// `module::build_program_tree`, which holds per-module item vecs
/// rather than a `Program`.
pub fn filter_inactive_items_in(
    items: &mut Vec<Item>,
    current_target: &str,
) -> HashMap<String, String> {
    let mut tombstones = HashMap::new();
    items.retain(|item| {
        let Some((attrs, name)) = item_attrs_and_name(item) else {
            return true;
        };
        let Some(spec) = target_spec_of(attrs) else {
            return true;
        };
        if spec.is_active_on(current_target) {
            return true;
        }
        if let Some(name) = name {
            tombstones.insert(name.to_string(), spec.render());
        }
        false
    });
    tombstones
}

// ── Target-provided resource sets (phase-10 target gate) ─────────
//
// Table per `design.md § Cross-target Compilation > Target-Provided
// Resource Sets`. Only HOST resources are listed — user-defined
// resources have no intrinsic target affinity (they exist wherever a
// provider exists) and the gate never examines them directly.
//
// `ProcessTable` is not in the design table (doc gap, noted in the
// phase-10 tracker entry): child-process spawning is native-only, so it
// gates like `Hardware`.

/// Is `resource` a host resource the target gate owns? Anything not in
/// this set is user-defined and exempt from target gating.
pub fn is_host_resource(resource: &str) -> bool {
    matches!(
        resource,
        "FileSystem"
            | "Stdin"
            | "Stdout"
            | "Stderr"
            | "Env"
            | "Network"
            | "Clock"
            | "RandomSource"
            | "Heap"
            | "Hardware"
            | "GpuBuffer"
            | "ProcessTable"
            | "Display"
            | "Storage"
            | "Console"
            | "Timer"
            | "Input"
    )
}

/// Does `target` provide `resource`? Callers must pre-check
/// [`is_host_resource`]; unknown resources return `false` here.
pub fn target_provides(target: &str, resource: &str) -> bool {
    let provided: &[&str] = match target {
        "native" => &[
            "FileSystem",
            "Stdin",
            "Stdout",
            "Stderr",
            "Env",
            "Network",
            "Clock",
            "RandomSource",
            "Heap",
            "Hardware",
            "GpuBuffer",
            "ProcessTable",
        ],
        "wasm_browser" => &[
            "Network",
            "Clock",
            "RandomSource",
            "Heap",
            "Display",
            "Storage",
            "Console",
            "Timer",
            "Input",
        ],
        "wasm_wasi" => &[
            "FileSystem",
            "Stdin",
            "Stdout",
            "Stderr",
            "Env",
            "Network",
            "Clock",
            "RandomSource",
            "Heap",
        ],
        "gpu" => &["GpuBuffer"],
        _ => &[],
    };
    provided.contains(&resource)
}
