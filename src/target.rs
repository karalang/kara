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
/// CPUs measured to execute scalar `f16` arithmetic in HARDWARE.
///
/// `bf16` IS NOT ON THIS AXIS and never consults these lists
/// (B-2026-08-22-30). karac widens bf16 arithmetic to `f32` and rounds back
/// unconditionally, on every backend, because LLVM 18's AArch64 ISel cannot
/// select scalar `bfloat` arithmetic (B-2026-07-22-1) — measured directly:
/// `llc -mtriple=arm64-apple-macosx -mcpu=apple-m1` on a bare `fadd bfloat`
/// dies with "Cannot select". So no CPU makes bf16 native, and treating the
/// two widths as one capability silently dropped the lint for bf16 on every
/// CPU listed here.
///
/// MEASURED, not inferred, with `llc -mcpu=<cpu>` on `fadd half` (LLVM 18):
/// a native CPU emits a half-width add (`fadd h0, h0, h1` on AArch64,
/// `vaddsh` on x86), an emulated one emits `fcvt`-to-`f32` promotion —
/// or, on wasm, `__extendhfsf2` / `__truncsfhf2` libcalls, which is
/// costlier still.
///
/// WHY AN EXPLICIT LIST rather than asking LLVM. There is no LLVM-C or
/// inkwell entry point that expands a CPU name to its resolved subtarget
/// features: `TargetMachine::get_feature_string` echoes back the string the
/// machine was CONSTRUCTED with, and `get_host_cpu_features` describes the
/// HOST, which is wrong under cross-compilation. Reading the feature string
/// alone is not a substitute — it is exactly the trap B-2026-08-22-7 was
/// filed on, because the aarch64 macOS baseline is `("apple-m1", "")`: an
/// EMPTY feature string with the whole capability carried by the CPU name.
const CPUS_WITH_NATIVE_F16: &[&str] = &["apple-m1", "sapphirerapids"];

/// CPUs measured to PROMOTE `f16` arithmetic to `f32`. Same `llc` method as
/// the list above, and the same `bf16` carve-out.
///
/// This list exists so the DEFAULT build path is decided by measurement
/// rather than by the fail-open fallback. `karac build` always installs a
/// CPU override — `resolve_native_cpu_baseline` applies `cpu-baseline =
/// "v3"` when nothing is configured — so "no explicit `--target-cpu`" is
/// not the same as "no CPU override set", and treating an unlisted CPU as
/// native would have silenced the lint on every ordinary build. Measured:
/// x86-64, -v2, -v3 and -v4 all promote (no `avx512fp16` at any level), as
/// does aarch64 `generic` at `+v8.2a` / `+v8.4a` / `+v8.6a` — an arch
/// version does not imply the OPTIONAL `FEAT_FP16`.
const CPUS_WITHOUT_NATIVE_F16: &[&str] = &[
    "x86-64",
    "x86-64-v2",
    "x86-64-v3",
    "x86-64-v4",
    "core2",
    "generic",
];

/// Feature flags that turn on hardware half-precision when named
/// explicitly via `--target-features`. Measured the same way: `+fullfp16`
/// on an AArch64 baseline flips `fadd s` to `fadd h`, and `+avx512fp16`
/// does the same on x86 **once its prerequisites are present** (alone it
/// stays emulated; with `+avx512f,+avx512vl` it emits `vaddsh`). Naming a
/// prerequisite-less `+avx512fp16` therefore under-reports rather than
/// over-reports, which is the safe direction.
const F16_FEATURE_FLAGS: &[&str] = &["+fullfp16", "+avx512fp16"];

/// The `(cpu, features)` baseline the active target compiles against.
///
/// Deliberately derived from `std::env::consts` for `native` rather than
/// from a triple: the triple is only resolved inside the LLVM driver, and
/// this has to be answerable from the TYPECHECKER, which is upstream of the
/// backend and where `#[allow(...)]` cascades already work. Kept in sync
/// with `codegen::driver::default_cpu_and_features` by
/// `native_baseline_matches_the_codegen_table`.
pub fn baseline_cpu_and_features() -> (&'static str, &'static str) {
    if active_target_is_wasm() {
        // No wasm proposal gives scalar half-precision arithmetic; LLVM
        // lowers it to `__extendhfsf2`/`__truncsfhf2` libcalls even with
        // `+simd128`.
        return ("generic", "+simd128");
    }
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => ("apple-m1", ""),
        ("aarch64", _) => ("generic", "+v8a,+outline-atomics"),
        ("x86_64", "macos") => ("core2", ""),
        ("x86_64", _) => ("x86-64", ""),
        _ => ("generic", ""),
    }
}

/// The CPU this build is actually judged against: the `--target-cpu` override
/// where the user set one, otherwise the per-target baseline.
///
/// Exists so a capability DECISION and the diagnostic that REPORTS it cannot
/// name different CPUs. They did (B-2026-08-22-30): `target_has_native_f16`
/// resolved the override while the `f16_software_emulated` message read
/// `baseline_cpu_and_features()` directly, so `KARAC_TARGET_CPU=generic` on an
/// Apple box correctly fired the lint and then blamed `apple-m1` — a CPU it had
/// not judged, and one whose real answer is the opposite. That is precisely
/// what `the_f16_lint_names_the_cpu_it_judged` was written to prevent, so the
/// two callers now read the same function.
pub fn resolved_cpu() -> &'static str {
    let (default_cpu, _) = baseline_cpu_and_features();
    target_cpu_override().unwrap_or(default_cpu)
}

/// Does the active target execute scalar `f16` arithmetic in hardware?
///
/// `false` means LLVM will promote every such operation to `f32` (or call
/// out to a libcall on wasm), which is what the `f16_software_emulated`
/// lint reports for `f16` — design.md:2347.
///
/// NOT a `bf16` predicate: that width is emulated on every target regardless
/// of the answer here, so the lint's bf16 arm does not call this
/// (B-2026-08-22-30). See [`CPUS_WITH_NATIVE_F16`].
///
/// UNKNOWN `--target-cpu` VALUES ANSWER `true` (i.e. do not lint). A user
/// naming a CPU this list has never measured is an expert action, and a
/// false "your f16 is emulated" on hardware that runs it natively is a
/// worse failure than a missing perf note: it sends someone rewriting code
/// that was already fast. Every CPU the default build path can install is
/// in one of the two measured lists, so fail-open is reached only when a
/// user names a CPU by hand that neither list has been measured against.
pub fn target_has_native_f16() -> bool {
    let cpu = resolved_cpu();
    let (_, default_features) = baseline_cpu_and_features();

    let mut features = String::from(default_features);
    if let Some(user) = target_features_override() {
        if !features.is_empty() {
            features.push(',');
        }
        features.push_str(user);
    }
    if F16_FEATURE_FLAGS.iter().any(|f| features.contains(f)) {
        return true;
    }
    if CPUS_WITH_NATIVE_F16.contains(&cpu) {
        return true;
    }
    if CPUS_WITHOUT_NATIVE_F16.contains(&cpu) {
        return false;
    }
    // An unmeasured CPU: fail open (see the doc comment above).
    true
}

pub fn active_target_is_wasm() -> bool {
    matches!(active_target(), "wasm_wasi" | "wasm_browser")
}

/// Whether this build has `--features wasm-threads` enabled. Unlike the
/// target name, the threads opt-in is a build *flag*, not a `V1_TARGETS`
/// entry — but checker-phase passes (the host-async target gate in
/// `effectchecker::target_gate`) need to know it to decide whether a
/// host-async producer (`std.web.time.after`) can be honored: on a
/// sequential wasm build it cannot (a single thread cannot both block in
/// `recv` and run the host event loop), so it is a hard error. Set once at
/// CLI startup alongside [`set_active_target`], before any pipeline pass.
static WASM_THREADS_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record whether `--features wasm-threads` is active for this build.
pub fn set_wasm_threads(enabled: bool) {
    WASM_THREADS_ENABLED.store(enabled, Ordering::Relaxed);
}

/// True when the active build is `wasm_browser --features wasm-threads`
/// (the only configuration with a worker pool that can run a blocking
/// `recv` off the main thread). See [`set_wasm_threads`].
pub fn wasm_threads_enabled() -> bool {
    WASM_THREADS_ENABLED.load(Ordering::Relaxed)
}

/// Whether the active wasm build marshals rich entry-point exports
/// (phase-10 "WASM entry-point discovery"): true for `--bindings browser`
/// and `--bindings component` (both want idiomatic typed exports — the
/// browser glue / the component canonical ABI), false for `--bindings
/// none` (raw core exports, the user owns the ABI) and non-wasm builds.
/// When true, `codegen::cabi` emits canonical-ABI export trampolines;
/// `wasm_component_host_package().is_some()` additionally distinguishes
/// the component (kebab-named, WIT) path from the browser (Kāra-named,
/// JS-glue) path. Set at CLI startup alongside [`set_active_target`].
static WASM_EXPORT_MARSHALLING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Record whether the active build marshals rich exports (browser /
/// component bindings). See [`wasm_export_marshalling`].
pub fn set_wasm_export_marshalling(enabled: bool) {
    WASM_EXPORT_MARSHALLING.store(enabled, Ordering::Relaxed);
}

/// See [`set_wasm_export_marshalling`].
pub fn wasm_export_marshalling() -> bool {
    WASM_EXPORT_MARSHALLING.load(Ordering::Relaxed)
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

/// External native-library link directive resolved from `kara.toml`'s
/// `[link]` table (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking).
/// `search_paths` become `-L<path>` and `libs` become `-l<name>` on the
/// `cc` line in [`crate::codegen::driver::link_executable_impl`]. Lives here
/// — plain strings, no LLVM types — alongside the other build-wide codegen
/// knobs set once at CLI startup, so it is reachable from non-llvm cfg and
/// the codegen-containment invariant holds. The native link is the only
/// reader; wasm builds (wasm-ld) ignore it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLinkConfig {
    pub libs: Vec<String>,
    pub search_paths: Vec<String>,
}

/// Build-wide native-library link directive for this process. `OnceLock` so
/// first-set wins (the `set_target_cpu_override` posture — one artifact per
/// invocation). Unset (the common case) leaves the link line untouched.
static NATIVE_LINK_CONFIG: std::sync::OnceLock<NativeLinkConfig> = std::sync::OnceLock::new();

/// Install the resolved `[link]` directive. First-set wins; a no-op second
/// call mirrors [`set_target_cpu_override`]. Setting an all-empty config is
/// harmless — the link line then gains no flags.
pub fn set_native_link_config(libs: Vec<String>, search_paths: Vec<String>) {
    let _ = NATIVE_LINK_CONFIG.set(NativeLinkConfig { libs, search_paths });
}

/// The native-library link directive for this process, if any was set.
pub fn native_link_config() -> Option<&'static NativeLinkConfig> {
    NATIVE_LINK_CONFIG.get()
}

/// Is WASM SIMD-128 effectively enabled for the active wasm target?
/// `+simd128` is the wasm default feature (design.md § Portable SIMD —
/// "WebAssembly SIMD-128 is a first-class lowering target"; phase-10
/// WASM SIMD-128 entry), so this is `true` unless the
/// `--target-features` chain disables it with `-simd128`. The scan is
/// last-wins over the user list, mirroring how LLVM resolves the
/// combined feature string the codegen driver builds (per-target
/// default first, user override appended — see `combined_features` in
/// `codegen/driver.rs`). Only meaningful when [`active_target_is_wasm`];
/// native SIMD enablement is the driver's per-triple table. Read by
/// `simd_report::native_vector_bits` (the `#[require_simd]` /
/// `--simd-report` target model) — lives here, as plain data, so that
/// model needs no LLVM types (CLAUDE.md § Codegen containment).
pub fn wasm_simd128_enabled() -> bool {
    match target_features_override() {
        None => true,
        Some(features) => simd128_after_features(features),
    }
}

/// `+simd128`-enabled state after applying a user feature list on top of
/// the wasm default (`+simd128`), last-wins. Split from
/// [`wasm_simd128_enabled`] so the resolution is testable without the
/// process-global override.
fn simd128_after_features(features: &str) -> bool {
    let mut enabled = true;
    for feat in features.split(',') {
        match feat.trim() {
            "+simd128" => enabled = true,
            "-simd128" => enabled = false,
            _ => {}
        }
    }
    enabled
}

#[cfg(test)]
mod simd128_feature_tests {
    use super::simd128_after_features;

    #[test]
    fn default_on_and_last_wins() {
        // No mention of simd128 → the wasm default (+simd128) stands.
        assert!(simd128_after_features(""));
        assert!(simd128_after_features("+bulk-memory,+sign-ext"));
        // A user `-simd128` disables it…
        assert!(!simd128_after_features("-simd128"));
        assert!(!simd128_after_features("+bulk-memory,-simd128"));
        // …and resolution over the user list is last-wins, mirroring
        // LLVM's handling of the combined string.
        assert!(simd128_after_features("-simd128,+simd128"));
        assert!(!simd128_after_features("+simd128,-simd128"));
        // Whitespace around entries is tolerated.
        assert!(!simd128_after_features(" -simd128 "));
    }
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

/// Which targets must be checked IN ADDITION to `base` for every
/// `#[target(...)]`-gated item in `items` to have its body examined at
/// all?
///
/// [`filter_inactive_items_in`] removes a non-admitted item *before
/// resolution*, body included — so under a single-target check a gated
/// body is not merely checked leniently, it is never seen. A file whose
/// only errors live in a `#[target(wasm_browser)]` fn therefore reports
/// "All checks passed" under the default `native` check, which is
/// B-2026-08-05-29: `karac check` is the Mend loop's front door, and it
/// was reporting success on source it had not read.
///
/// Deliberately shares [`item_attrs_and_name`] and [`TargetSpec::is_active_on`]
/// with the filter, so the two cannot disagree about which items are
/// gated or about what a spec admits — an item this returns nothing for
/// is exactly an item the filter keeps.
///
/// Returns [`V1_TARGETS`] order, deduped. Empty for a program with no
/// gated items, and empty for specs `base` already admits (`not(gpu)` on
/// a native check needs no second pass).
pub fn extra_check_targets_for(items: &[Item], base: &str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for item in items {
        let Some((attrs, _)) = item_attrs_and_name(item) else {
            continue;
        };
        let Some(spec) = target_spec_of(attrs) else {
            continue;
        };
        if spec.is_active_on(base) {
            continue;
        }
        for target in V1_TARGETS {
            if *target != base && spec.is_active_on(target) && !out.contains(target) {
                out.push(target);
            }
        }
    }
    out.sort_by_key(|t| V1_TARGETS.iter().position(|v| v == t).unwrap_or(usize::MAX));
    out
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

#[cfg(test)]
mod extra_check_target_tests {
    use super::extra_check_targets_for;
    use crate::parse;

    fn extra(src: &str) -> Vec<&'static str> {
        let parsed = parse(src);
        assert!(parsed.errors.is_empty(), "fixture must parse: {src}");
        extra_check_targets_for(&parsed.program.items, "native")
    }

    #[test]
    fn ungated_program_needs_no_second_pass() {
        assert!(extra("fn main() {}\n").is_empty());
    }

    #[test]
    fn positive_gate_names_the_target_to_check_under() {
        assert_eq!(
            extra("#[target(wasm_browser)]\npub fn f() {}\nfn main() {}\n"),
            vec!["wasm_browser"],
        );
    }

    #[test]
    fn multi_name_gate_yields_every_admitted_target_in_canonical_order() {
        // Both are admitted and the two differ in provided resources, so
        // checking under only one would leave the other unexamined.
        assert_eq!(
            extra("#[target(wasm_wasi, wasm_browser)]\npub fn f() {}\nfn main() {}\n"),
            vec!["wasm_browser", "wasm_wasi"],
        );
    }

    #[test]
    fn gate_admitting_native_needs_nothing() {
        // `not(gpu)` is active on native — the ordinary pass reads the
        // body already, so a second pass would be pure cost.
        assert!(extra("#[target(not(gpu))]\npub fn f() {}\nfn main() {}\n").is_empty());
    }

    #[test]
    fn negated_gate_excluding_native_covers_the_rest() {
        assert_eq!(
            extra("#[target(not(native))]\npub fn f() {}\nfn main() {}\n"),
            vec!["wasm_browser", "wasm_wasi", "gpu"],
        );
    }

    #[test]
    fn repeated_gates_dedupe() {
        assert_eq!(
            extra(
                "#[target(wasm_browser)]\npub fn a() {}\n\
                 #[target(wasm_browser)]\npub struct S { x: i32 }\nfn main() {}\n"
            ),
            vec!["wasm_browser"],
        );
    }
}

#[cfg(test)]
mod f16_capability_tests {
    use super::*;

    /// The two baseline tables must agree, or the lint judges a different
    /// machine than the one codegen builds for.
    ///
    /// `target::baseline_cpu_and_features` exists because the TYPECHECKER
    /// cannot reach the codegen driver (containment, and the driver is
    /// `#[cfg(feature = "llvm")]`), so it classifies by
    /// `std::env::consts` instead of by an LLVM-resolved triple. Two
    /// producers is exactly the drift this codebase keeps paying for, so
    /// they are pinned against each other here rather than trusted to stay
    /// in sync.
    #[cfg(feature = "llvm")]
    #[test]
    fn native_baseline_matches_the_codegen_table() {
        let host_triple = format!(
            "{}-{}",
            std::env::consts::ARCH,
            if std::env::consts::OS == "macos" {
                "apple-darwin"
            } else {
                "unknown-linux-gnu"
            }
        );
        let from_codegen = crate::codegen::driver::default_cpu_and_features(&host_triple);
        let from_target = baseline_cpu_and_features();
        assert_eq!(
            from_target, from_codegen,
            "the typechecker's baseline ({from_target:?}) disagrees with the codegen \
             table ({from_codegen:?}) for {host_triple}"
        );
    }

    #[test]
    fn every_default_baseline_cpu_is_measured() {
        // The fail-open branch must be unreachable on a default build: if a
        // CPU the build path installs is in NEITHER measured list, the lint
        // silently stops firing. Covers the `cpu-baseline` levels
        // `resolve_native_cpu_baseline` can pick, plus the per-OS defaults.
        for cpu in [
            "x86-64",
            "x86-64-v2",
            "x86-64-v3",
            "x86-64-v4",
            "core2",
            "generic",
            "apple-m1",
        ] {
            assert!(
                CPUS_WITH_NATIVE_F16.contains(&cpu) || CPUS_WITHOUT_NATIVE_F16.contains(&cpu),
                "`{cpu}` is reachable from the default build path but is in neither \
                 measured list, so the lint would fail open on it"
            );
        }
    }

    #[test]
    fn the_two_measured_lists_are_disjoint() {
        for cpu in CPUS_WITH_NATIVE_F16 {
            assert!(
                !CPUS_WITHOUT_NATIVE_F16.contains(cpu),
                "`{cpu}` is listed as both native and emulated"
            );
        }
    }
}
