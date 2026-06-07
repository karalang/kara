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
//! The current target defaults to [`CURRENT_TARGET`] (`"native"`); the
//! `--target` flag (phase-10 WASM build path) swaps it process-wide via
//! [`set_active_target`] before any pass runs. All consumers — both
//! filter call sites (`cli::Pipeline::resolve` for single-file,
//! `module::build_program_tree` for project mode), the resolver's
//! tombstone diagnostics, and the effect checker's target gate — read
//! [`active_target`], so this stays the single source of truth.

use crate::ast::{Attribute, Expr, ExprKind, Item, Program};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The closed v1 target-name set. Order is the canonical listing used in
/// diagnostics.
pub const V1_TARGETS: &[&str] = &["native", "wasm_browser", "wasm_wasi", "gpu"];

/// The compilation target every karac build produces by default.
pub const CURRENT_TARGET: &str = "native";

/// Index into [`V1_TARGETS`] of the active compilation target. Index 0
/// is `"native"` — the default when `--target` is absent. Stored as an
/// index (not a string) so the getter can hand out `&'static str`
/// without leaks or locks.
static ACTIVE_TARGET_IDX: AtomicUsize = AtomicUsize::new(0);

/// The compilation target for this process. Defaults to
/// [`CURRENT_TARGET`]; `--target=<name>` swaps it once at CLI startup
/// (before any pipeline pass runs).
pub fn active_target() -> &'static str {
    V1_TARGETS[ACTIVE_TARGET_IDX.load(Ordering::Relaxed)]
}

/// Select the active compilation target by v1 name. Returns `Err` with
/// the closed-set listing for an unknown name — the CLI surfaces that
/// verbatim. One target per process: the compiler builds one artifact
/// per invocation (design.md § Cross-target Compilation — build-matrix
/// orchestration is a CI concern, not a compiler concern).
pub fn set_active_target(name: &str) -> Result<(), String> {
    match V1_TARGETS.iter().position(|t| *t == name) {
        Some(idx) => {
            ACTIVE_TARGET_IDX.store(idx, Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!(
            "unknown target '{}'. Valid targets: {}",
            name,
            V1_TARGETS.join(", ")
        )),
    }
}

/// Is the active target one of the two WASM module targets? Both
/// produce wasm32-wasip1 modules in v1 (`wasm_browser` is a wasip1
/// module whose WASI surface is polyfilled by the generated JS glue —
/// design.md § Host Functions), so the codegen driver's wasm decisions
/// (target machine, link path, allocator symbol, entry shim) key on
/// this predicate rather than on either name.
pub fn active_target_is_wasm() -> bool {
    matches!(active_target(), "wasm_wasi" | "wasm_browser")
}

/// Is `name` one of the closed v1 target names? The `--target` flag's
/// value space is shared with rustc-style triples (manifest
/// `[target.<triple>.*]` overlay selection); this predicate is how the
/// CLI tells the two apart.
pub fn is_v1_target_name(name: &str) -> bool {
    V1_TARGETS.contains(&name)
}

/// User-selected CPU baseline override (phase-10 `--target-cpu`;
/// design.md § CPU Baseline Targeting). `None` (the default) keeps the
/// per-target-triple table in `codegen/driver.rs::default_cpu_and_features`.
/// Set once at CLI startup by `cmd_build` / `cmd_build_project` after
/// resolving the precedence chain `--target-cpu` flag > `KARAC_TARGET_CPU`
/// env > `[release] target-cpu` in `kara.toml` — the codegen driver's
/// target-machine constructors are the only readers. Lives here (plain
/// string, no LLVM types) so the setter is reachable from non-llvm cfg
/// and the codegen-containment invariant holds.
static TARGET_CPU_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install the resolved `--target-cpu` override. One artifact per
/// invocation (the `set_active_target` posture), so first-set wins and
/// a second call is a no-op rather than an error.
pub fn set_target_cpu_override(cpu: &str) {
    let _ = TARGET_CPU_OVERRIDE.set(cpu.to_string());
}

/// The CPU baseline override for this process, if any.
pub fn target_cpu_override() -> Option<&'static str> {
    TARGET_CPU_OVERRIDE.get().map(|s| s.as_str())
}

/// User-selected feature-string override (phase-10 `--target-features`;
/// design.md § CPU Baseline Targeting > Feature-string override). The
/// sibling of [`TARGET_CPU_OVERRIDE`] with its own precedence chain
/// (`--target-features` flag > `KARAC_TARGET_FEATURES` env > `[release]
/// target-features`), resolved independently of the CPU chain. The
/// codegen driver *appends* this after the per-target default features —
/// LLVM resolves duplicate entries last-wins, so a user `-feat` can
/// disable a table default and the default can't silently re-override.
static TARGET_FEATURES_OVERRIDE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install the resolved `--target-features` override. First-set wins,
/// same as [`set_target_cpu_override`].
pub fn set_target_features_override(features: &str) {
    let _ = TARGET_FEATURES_OVERRIDE.set(features.to_string());
}

/// The feature-string override for this process, if any.
pub fn target_features_override() -> Option<&'static str> {
    TARGET_FEATURES_OVERRIDE.get().map(|s| s.as_str())
}

/// Package name under embedded-WIT component bindings (phase-10
/// "embedded-WIT migration"). Set by the CLI before codegen when the
/// effective `--bindings` mode is `component` on a wasm target; its
/// presence is what flips codegen's `host fn` import attachment from
/// the C-ABI `kara_host`/snake_case shape (browser glue, wasi
/// embedders, the deprecated paired form) to the canonical-ABI
/// `kara:<pkg>/host`/kebab-case shape `wasm-tools component embed`
/// resolves against (`wit::host_import_module` / `host_import_name` —
/// the single source of those strings). Lives here (plain string, no
/// LLVM types) for the same codegen-containment reason as the CPU
/// override above.
static WASM_COMPONENT_HOST_PACKAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install the component-bindings package name. First-set wins, same
/// as [`set_target_cpu_override`].
pub fn set_wasm_component_host_package(pkg: &str) {
    let _ = WASM_COMPONENT_HOST_PACKAGE.set(pkg.to_string());
}

/// The component-bindings package name for this process, if embedded
/// component bindings are active.
pub fn wasm_component_host_package() -> Option<&'static str> {
    WASM_COMPONENT_HOST_PACKAGE.get().map(|s| s.as_str())
}

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
