//! Monomorphization tracking — phase-7-codegen.md line 97. Plain-data
//! analyzer that walks the AST and the typechecker's per-call-site
//! type-parameter substitution table to enumerate every generic
//! instantiation in the program. Consumed by `karac query
//! monomorphization`; output schema lives at `design.md § Compiler
//! Query API > karac query monomorphization output schema`.
//!
//! ### Data sources
//!
//! - **`TypeCheckResult.call_type_subs`** — the typechecker records
//!   one entry per generic call site (call-expression span →
//!   typeparam-name → resolved type name). Non-generic calls are
//!   absent from the table (`record_call_type_subs` early-returns on
//!   empty solutions), so every entry we see is a real instantiation.
//! - **`TypeCheckResult.method_callee_types`** — resolves a
//!   `MethodCall` site to its `Type.method` string. Used to attribute
//!   method-call instantiations to a stable generic identity (the
//!   receiver type with its concrete generic args stripped).
//! - **`EffectCheckResult.call_effect_subs`** — the effect checker
//!   records, per generic call site, how each `with E` effect variable
//!   resolved (call-expression span → effect-variable name → concrete
//!   `Effect` set). We take the *union* of those sets — the call's
//!   effective effect set — and render it as the instance's `effects`.
//!   Optional: when no effect-check result is threaded (e.g. an earlier
//!   phase aborted), every instance's `effects` is empty.
//!
//! ### Effect-set identity
//!
//! Per `design.md § Effect Polymorphism > Monomorphization order for
//! compound polymorphism`, a call site is monomorphized on its resolved
//! `(T1..Tk, E1..Em)` tuple: two sites that agree on types but resolve
//! to different effect sets are distinct instances. The dedup key here
//! is therefore `(types, effects)` — see [`GroupAccum`]. We key on the
//! *union* effect set rather than per-variable bindings: which variable
//! carries an effect is codegen-irrelevant once the effective set is
//! fixed, and `design.md § Specification Layers > Compiler Query API`
//! explicitly permits the reported count to shrink when instances whose
//! effect sets coincide are merged. The guaranteed rule is the upper
//! bound (one instance per distinct tuple); reporting fewer is allowed.
//!
//! ### v1 limitations
//!
//! - **Param-order is alphabetical, not declaration-order.** The
//!   typechecker's `call_type_subs` is a `HashMap<String, String>`;
//!   we sort by param name so the `types` list is deterministic
//!   across runs. Mapping back to declaration order is a tooling
//!   concern (consumers read the function definition to learn the
//!   formal-param sequence).
//!
//! - **Receiver-type stripping is conservative.** `Vec[i64].push`
//!   and `Vec[String].push` deduplicate under the same generic
//!   identity `"Vec.push"` only if the receiver portion is a
//!   single-segment name with a `[…]` generic-args suffix. Path-
//!   shape receivers, nested generic args (`Map[i64, Vec[T]]`), and
//!   trait-bounded receivers fall back to the full `method_callee`
//!   string — they appear as one-instance "generics" until the
//!   stripping rule grows. v1 under-reports rather than over-reports.

use crate::ast::*;
use crate::effectchecker::{Effect, EffectCheckResult};
use crate::resolver::SpanKey;
use crate::token::Span;
use crate::typechecker::TypeCheckResult;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// One concrete instantiation of a generic — the resolved
/// `(T1..Tk)` tuple plus the call site that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// Resolved type names in sorted-by-param-name order. v1 emits
    /// the type names as the typechecker resolved them (e.g.
    /// `"i64"`, `"Vec[Order]"`, `"Wrapper"`).
    pub types: Vec<String>,
    /// The instance's effective effect set — the union of every `with E`
    /// effect-variable resolution at the call site, as sorted, de-duplicated
    /// `verb(resource)` labels (`reads(Log)`, `blocks`). Empty for a generic
    /// with no resolved compound-effect bindings, or when no effect-check
    /// result was threaded. See module doc § Effect-set identity.
    pub effects: Vec<String>,
    /// Source span of the first call site that produced this
    /// tuple. When multiple call sites share the same tuple, the
    /// first one in source order wins (stable by `Span.offset`).
    pub site: Span,
}

/// All distinct instantiations of one generic function.
#[derive(Debug, Clone)]
pub struct GenericRecord {
    /// Identity of the generic — free-function name (`"process"`),
    /// path (`"Module::process"`), or method (`"Vec.push"`).
    pub generic: String,
    /// One entry per distinct (types, effects) tuple, sorted by
    /// first-seen call-site offset.
    pub instances: Vec<Instance>,
}

/// Per-program monomorphization surface. The `karac query
/// monomorphization` renderer formats this into the JSON envelope
/// spelled out at `design.md § Compiler Query API`.
#[derive(Debug, Clone, Default)]
pub struct MonomorphizationTable {
    /// Sorted by `generic` name for deterministic rendering.
    pub by_generic: Vec<GenericRecord>,
}

/// Per-generic instantiation ceiling supplied by
/// `--monomorphization-budget=warn:N,error:M`. Both thresholds are
/// optional; an all-`None` budget is disabled. The v1 default is
/// disabled — picking concrete default thresholds is deferred to v1.x
/// pending codegen data (phase-7-codegen.md line 266).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MonomorphizationBudget {
    pub warn: Option<usize>,
    pub error: Option<usize>,
}

impl MonomorphizationBudget {
    /// True when at least one threshold is set. A disabled budget skips
    /// the check entirely.
    pub fn is_enabled(&self) -> bool {
        self.warn.is_some() || self.error.is_some()
    }
}

/// Severity of a budget violation. `Error` (count ≥ error threshold)
/// fails the build; `Warning` (count ≥ warn threshold, below error)
/// emits a note and continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLevel {
    Warning,
    Error,
}

/// One generic whose instantiation count met or exceeded a budget
/// threshold. Carries the first-seen call site so the diagnostic can
/// point at a concrete location.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetViolation {
    pub generic: String,
    pub count: usize,
    pub threshold: usize,
    pub level: BudgetLevel,
    pub site: Span,
}

impl MonomorphizationTable {
    pub fn generic_count(&self) -> usize {
        self.by_generic.len()
    }

    pub fn instance_count(&self) -> usize {
        self.by_generic.iter().map(|g| g.instances.len()).sum()
    }

    /// Compare each generic's instantiation count against `budget`. A
    /// generic that meets the error threshold yields a single `Error`
    /// violation (its warn threshold is not also reported); otherwise
    /// meeting the warn threshold yields a `Warning`. Result preserves
    /// `by_generic` order (already sorted by generic name).
    pub fn budget_violations(&self, budget: &MonomorphizationBudget) -> Vec<BudgetViolation> {
        let mut out = Vec::new();
        for g in &self.by_generic {
            let Some(first) = g.instances.first() else {
                continue;
            };
            let count = g.instances.len();
            if let Some(error) = budget.error {
                if count >= error {
                    out.push(BudgetViolation {
                        generic: g.generic.clone(),
                        count,
                        threshold: error,
                        level: BudgetLevel::Error,
                        site: first.site.clone(),
                    });
                    continue;
                }
            }
            if let Some(warn) = budget.warn {
                if count >= warn {
                    out.push(BudgetViolation {
                        generic: g.generic.clone(),
                        count,
                        threshold: warn,
                        level: BudgetLevel::Warning,
                        site: first.site.clone(),
                    });
                }
            }
        }
        out
    }
}

/// Entry point. Walks `program` against `tc` and returns the
/// per-generic instantiation table.
/// Enumerate every generic instantiation in `program`. `ec` supplies the
/// per-call effect-variable resolutions used to populate each instance's
/// effective effect set; pass `None` (e.g. when an earlier phase aborted)
/// to leave every `effects` list empty.
pub fn analyze(
    program: &Program,
    tc: &TypeCheckResult,
    ec: Option<&EffectCheckResult>,
) -> MonomorphizationTable {
    let empty_subs: HashMap<SpanKey, HashMap<String, HashSet<Effect>>> = HashMap::new();
    let effect_subs = ec.map(|e| &e.call_effect_subs).unwrap_or(&empty_subs);
    let mut walker = Walker {
        tc,
        effect_subs,
        groups: BTreeMap::new(),
    };

    for item in &program.items {
        match item {
            Item::Function(f) => walker.walk_block(&f.body),
            Item::ImplBlock(imp) => {
                for impl_item in &imp.items {
                    if let ImplItem::Method(m) = impl_item {
                        walker.walk_block(&m.body);
                    }
                }
            }
            _ => {}
        }
    }

    let mut by_generic: Vec<GenericRecord> = walker
        .groups
        .into_iter()
        .map(|(generic, group)| {
            let mut instances: Vec<Instance> = group.instances.into_values().collect();
            instances.sort_by_key(|i| i.site.offset);
            GenericRecord { generic, instances }
        })
        .collect();
    by_generic.sort_by(|a, b| a.generic.cmp(&b.generic));

    MonomorphizationTable { by_generic }
}

struct GroupAccum {
    /// Dedup key is the `(types, effects)` tuple — `types` sorted by
    /// param name, `effects` the sorted effective effect set. Two call
    /// sites that agree on both axes share one instance; differing
    /// effect sets at the same types produce distinct instances
    /// (`design.md § Monomorphization identity`). First call-site for
    /// each tuple wins.
    instances: BTreeMap<(Vec<String>, Vec<String>), Instance>,
}

impl GroupAccum {
    fn new() -> Self {
        Self {
            instances: BTreeMap::new(),
        }
    }

    fn record(&mut self, types: Vec<String>, effects: Vec<String>, site: Span) {
        self.instances
            .entry((types.clone(), effects.clone()))
            .or_insert(Instance {
                types,
                effects,
                site,
            });
    }
}

struct Walker<'a> {
    tc: &'a TypeCheckResult,
    /// Per-call effect-variable resolutions, sourced from
    /// `EffectCheckResult.call_effect_subs` (empty when no effect-check
    /// result was threaded).
    effect_subs: &'a HashMap<SpanKey, HashMap<String, HashSet<Effect>>>,
    groups: BTreeMap<String, GroupAccum>,
}

impl Walker<'_> {
    fn record_call(&mut self, generic: String, span: &Span) {
        let span_key = SpanKey::from_span(span);
        let subs = match self.tc.call_type_subs.get(&span_key) {
            Some(s) => s,
            None => return,
        };
        if subs.is_empty() {
            return;
        }
        let types = ordered_types(subs);
        let effects = self
            .effect_subs
            .get(&span_key)
            .map(ordered_effects)
            .unwrap_or_default();
        self.groups
            .entry(generic)
            .or_insert_with(GroupAccum::new)
            .record(types, effects, span.clone());
    }

    fn walk_block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt);
        }
        if let Some(tail) = &block.final_expr {
            self.walk_expr(tail);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::MultiAssign { .. } => unreachable!(
                "StmtKind::MultiAssign is removed by the desugar pass before reaching this phase"
            ),
            StmtKind::Let { value, .. } | StmtKind::LetElse { value, .. } => {
                self.walk_expr(value);
            }
            StmtKind::LetUninit { .. } => {}
            StmtKind::Defer { body } | StmtKind::ErrDefer { body, .. } => {
                self.walk_block(body);
            }
            StmtKind::Assign { target, value } | StmtKind::CompoundAssign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(generic) = callee_generic_identity(callee) {
                    self.record_call(generic, &expr.span);
                }
                self.walk_expr(callee);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::MethodCall { object, args, .. } => {
                let span_key = SpanKey::from_span(&expr.span);
                if let Some(callee_path) = self.tc.method_callee_types.get(&span_key) {
                    let generic = strip_receiver_generic_args(callee_path);
                    self.record_call(generic, &expr.span);
                }
                self.walk_expr(object);
                for arg in args {
                    self.walk_expr(&arg.value);
                }
            }
            ExprKind::Binary { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Unary { operand, .. } | ExprKind::Question(operand) => {
                self.walk_expr(operand);
            }
            ExprKind::NilCoalesce { left, right } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::OptionalChain { object, args, .. } => {
                self.walk_expr(object);
                if let Some(args) = args {
                    for arg in args {
                        self.walk_expr(&arg.value);
                    }
                }
            }
            ExprKind::FieldAccess { object, .. } | ExprKind::TupleIndex { object, .. } => {
                self.walk_expr(object);
            }
            ExprKind::Index { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }
            ExprKind::Block(b) => self.walk_block(b),
            ExprKind::If {
                condition,
                then_block,
                else_branch,
            } => {
                self.walk_expr(condition);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::IfLet {
                value,
                then_block,
                else_branch,
                ..
            } => {
                self.walk_expr(value);
                self.walk_block(then_block);
                if let Some(eb) = else_branch {
                    self.walk_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.walk_expr(g);
                    }
                    self.walk_expr(&arm.body);
                }
            }
            ExprKind::While {
                condition, body, ..
            } => {
                self.walk_expr(condition);
                self.walk_block(body);
            }
            ExprKind::WhileLet { value, body, .. } => {
                self.walk_expr(value);
                self.walk_block(body);
            }
            ExprKind::For { iterable, body, .. } => {
                self.walk_expr(iterable);
                self.walk_block(body);
            }
            ExprKind::Loop { body, .. } => self.walk_block(body),
            ExprKind::LabeledBlock { body, .. } => self.walk_block(body),
            ExprKind::Closure { body, .. } => self.walk_expr(body),
            ExprKind::Return(Some(e)) => self.walk_expr(e),
            ExprKind::Break { value: Some(v), .. } => self.walk_expr(v),
            ExprKind::Tuple(items) | ExprKind::ArrayLiteral(items) => {
                for it in items {
                    self.walk_expr(it);
                }
            }
            ExprKind::PrefixCollectionLiteral { items, .. } => {
                for it in items {
                    self.walk_expr(it);
                }
            }
            _ => {}
        }
    }
}

/// Free-function callee identity. `Identifier("name")` and
/// `Path([name])` collapse to `"name"`; multi-segment paths render as
/// `"seg::seg::…"`. Returns `None` for callee shapes we don't
/// attribute (closure invocations, `expr.method` style — those are
/// `MethodCall` and dispatched separately).
fn callee_generic_identity(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Identifier(n) => Some(n.clone()),
        ExprKind::Path { segments, .. } if !segments.is_empty() => Some(segments.join("::")),
        _ => None,
    }
}

/// Strip generic-args from the receiver portion of a `Type.method`
/// string so all instantiations of one generic method collapse to one
/// group. `"Vec[i64].push"` → `"Vec.push"`. The receiver portion is
/// everything up to the *last* `.` (Kāra identifiers don't contain
/// `.`, so a single rsplit is correct). Receivers without a `[` —
/// concrete primitive types like `i64.bit_count`, plain structs — pass
/// through unchanged.
fn strip_receiver_generic_args(callee: &str) -> String {
    let (receiver, method) = match callee.rsplit_once('.') {
        Some(pair) => pair,
        None => return callee.to_string(),
    };
    let bare_receiver = match receiver.find('[') {
        Some(idx) => &receiver[..idx],
        None => receiver,
    };
    format!("{}.{}", bare_receiver, method)
}

/// Sort `subs` (typeparam-name → resolved-type) by param name and
/// return the resolved types in that order. Alphabetical sort makes
/// the output deterministic across runs (the underlying `HashMap`
/// iteration order is not). Tools wanting declaration-order can map
/// back via the function definition.
fn ordered_types(subs: &HashMap<String, String>) -> Vec<String> {
    let names: BTreeSet<&String> = subs.keys().collect();
    names
        .into_iter()
        .filter_map(|k| subs.get(k).cloned())
        .collect()
}

/// Collapse a call site's per-effect-variable resolutions into the
/// instance's effective effect set: the union of every variable's
/// `Effect` set, rendered as sorted, de-duplicated `verb(resource)`
/// labels. The `BTreeSet` gives both the sort and the dedup, so two
/// variables that resolved to the same effect collapse to one label
/// (the effective set is what drives codegen, not the binding site).
fn ordered_effects(subs: &HashMap<String, HashSet<Effect>>) -> Vec<String> {
    let labels: BTreeSet<String> = subs.values().flatten().map(effect_label).collect();
    labels.into_iter().collect()
}

/// Render one `Effect` as `verb(resource)`, dropping the parens for the
/// resourceless execution verbs (`blocks`, `suspends`). Shares
/// `effectchecker::verb_name` so the spelling matches the effect
/// checker's own diagnostics and `karac query effects`.
fn effect_label(effect: &Effect) -> String {
    let verb = crate::effectchecker::verb_name(&effect.verb);
    if effect.resource.is_empty() {
        verb
    } else {
        format!("{}({})", verb, effect.resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_generic_args_strips_brackets_from_receiver() {
        assert_eq!(
            strip_receiver_generic_args("Vec[i64].push"),
            "Vec.push".to_string()
        );
    }

    #[test]
    fn strip_generic_args_passthrough_when_no_brackets() {
        assert_eq!(
            strip_receiver_generic_args("i64.bit_count"),
            "i64.bit_count".to_string()
        );
    }

    #[test]
    fn strip_generic_args_keeps_method_intact_after_strip() {
        assert_eq!(
            strip_receiver_generic_args("Map[i64, String].get"),
            "Map.get".to_string()
        );
    }

    #[test]
    fn strip_generic_args_handles_no_dot() {
        assert_eq!(strip_receiver_generic_args("orphan"), "orphan".to_string());
    }

    #[test]
    fn ordered_types_is_alphabetical_by_param_name() {
        let mut subs: HashMap<String, String> = HashMap::new();
        subs.insert("U".to_string(), "Receipt".to_string());
        subs.insert("T".to_string(), "Order".to_string());
        // Sorted by name (T, U) → values in that order.
        assert_eq!(
            ordered_types(&subs),
            vec!["Order".to_string(), "Receipt".to_string()]
        );
    }

    #[test]
    fn empty_table_reports_zero_counts() {
        let table = MonomorphizationTable::default();
        assert_eq!(table.generic_count(), 0);
        assert_eq!(table.instance_count(), 0);
    }

    /// Build a table with one generic carrying `n` distinct instances.
    fn table_with(generic: &str, n: usize) -> MonomorphizationTable {
        let instances = (0..n)
            .map(|i| Instance {
                types: vec![format!("T{i}")],
                effects: Vec::new(),
                site: Span::default(),
            })
            .collect();
        MonomorphizationTable {
            by_generic: vec![GenericRecord {
                generic: generic.to_string(),
                instances,
            }],
        }
    }

    #[test]
    fn budget_disabled_reports_no_violations() {
        let table = table_with("process", 9);
        assert!(table
            .budget_violations(&MonomorphizationBudget::default())
            .is_empty());
    }

    #[test]
    fn budget_below_threshold_is_silent() {
        let table = table_with("process", 1);
        let budget = MonomorphizationBudget {
            warn: Some(2),
            error: Some(4),
        };
        assert!(table.budget_violations(&budget).is_empty());
    }

    #[test]
    fn budget_warn_threshold_trips_at_equality() {
        let table = table_with("process", 3);
        let budget = MonomorphizationBudget {
            warn: Some(3),
            error: None,
        };
        let v = table.budget_violations(&budget);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].level, BudgetLevel::Warning);
        assert_eq!(v[0].count, 3);
        assert_eq!(v[0].threshold, 3);
        assert_eq!(v[0].generic, "process");
    }

    #[test]
    fn budget_error_supersedes_warn_for_same_generic() {
        let table = table_with("process", 5);
        let budget = MonomorphizationBudget {
            warn: Some(2),
            error: Some(5),
        };
        let v = table.budget_violations(&budget);
        // A single Error violation — the warn level is not also reported.
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].level, BudgetLevel::Error);
        assert_eq!(v[0].threshold, 5);
    }

    #[test]
    fn budget_error_only_ignores_sub_error_counts() {
        let table = table_with("process", 3);
        let budget = MonomorphizationBudget {
            warn: None,
            error: Some(4),
        };
        assert!(table.budget_violations(&budget).is_empty());
    }
}
