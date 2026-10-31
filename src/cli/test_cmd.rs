//! `karac test` — discovery, the #[test] harness, and its reporting.
//!
//! Extracted verbatim from `cli.rs` (structural-debt extraction, slice 1).
//! Free functions are `pub(super)` — private plumbing of the CLI module.

use super::*;

// ── Tests ────────────────────────────────────────────────────────

/// Outcome of a single test: the runner turns it into a `test_pass` or
/// `test_fail` JSONL event.
#[derive(Debug, Clone)]
pub(super) struct TestOutcome {
    pub message: Option<String>,
    pub span: Option<crate::token::Span>,
    pub left: Option<String>,
    pub right: Option<String>,
    /// A failed assertion's source text (`x > 0`, `a == b`).
    pub assertion: Option<String>,
}

/// Emit one JSONL test-runner event on stdout. Schema documented in
/// `docs/design.md § Testing › Test runner output format`. The discriminator
/// key is `"type"`, matching the build pipeline's [`emit_jsonl_event`] —
/// JSONL clients consume one shape across all `karac` outputs.
pub(super) fn emit_test_event(event: &str, fields: &str) {
    if fields.is_empty() {
        println!("{{\"type\":{}}}", json_string(event));
    } else {
        println!("{{\"type\":{},{}}}", json_string(event), fields);
    }
}

/// Render a module path for the qualified test ID, e.g.
/// `db.connection::test_reconnect`. The crate-root module renders as
/// `<root>` so users can distinguish a test in the entry file from any
/// other.
pub(super) fn module_label(path: &[String]) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        path.join(".")
    }
}

#[derive(Debug, Clone)]
pub(super) struct DiscoveredTest {
    pub(super) module_id: usize,
    pub(super) fn_name: String,
    pub(super) qualified: String,
    /// Fully-qualified resource paths (e.g. `"db.UserDB"`) the test
    /// declares via `#[test(requires = [...])]`. Empty when the test has
    /// no `requires` clause; the runner gates execution on the probe
    /// result for each entry.
    pub(super) requires: Vec<String>,
    /// `#[with_provider(resource_path, constructor_expr)]` fixtures on
    /// the test, preserved in source order (outer-to-inner). The runner
    /// evaluates each constructor before the test body and pushes a
    /// matching provider frame so resource-method calls inside the test
    /// resolve against the fixture. See design.md § Testing.
    pub(super) with_providers: Vec<WithProviderFixture>,
    /// Per-test timeout in seconds from `#[test(timeout_seconds = N)]`.
    /// `None` when the attribute is absent; the runner then falls back to
    /// the kara.toml `[test].timeout_seconds`, the `KARAC_TEST_TIMEOUT_SECS`
    /// env var, and finally the 30 s default (phase-7 line 847 sub-step 3).
    pub(super) timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
pub(super) struct WithProviderFixture {
    /// Fully-qualified resource path (e.g. `"Clock"` or `"db.UserDB"`).
    pub(super) resource_path: String,
}

/// Stable opaque interpreter-side identifier for an `Item::TestCase`.
/// The synthesized `Item::Function` (see [`lower_test_case_to_function`])
/// registers under this name so [`Interpreter::run_test_function`] can
/// dispatch through the regular `call_function` path with no extra
/// branching. Format: `__test_<sanitized-module-label>_<line>_<8-hex>`.
///
/// The hash prefix is `blake3(case_name)[..8]` — first 8 hex chars of
/// the case name's blake3 digest. Two cases at the same (module, line)
/// with different names can't both legally exist (one source line, one
/// item), so the line component already pins identity; the hash is a
/// belt-and-braces guard against module-path edge cases (synthetic
/// label collisions across re-export scaffolds, etc.) and gives the
/// mangled name a recognizable shape even when several cases share a
/// line through future macro expansion. Dots in the module label
/// become underscores so the mangled string stays a single contiguous
/// token in debugger / profiler views.
pub(super) fn mangled_test_function_name(
    module_label: &str,
    line: usize,
    case_name: &str,
) -> String {
    let label_safe: String = module_label
        .chars()
        .map(|c| if c == '.' || c == ':' { '_' } else { c })
        .collect();
    let digest = blake3::hash(case_name.as_bytes());
    let hex = digest.to_hex();
    format!("__test_{}_{}_{}", label_safe, line, &hex.as_str()[..8])
}

/// Synthesize an `Item::Function` shell from an `Item::TestCase` so
/// the regular resolve / typecheck / interpret pipeline can chew the
/// body without growing TestCase-specific arms in every phase. The
/// synthesized function has:
///
/// - the mangled name from [`mangled_test_function_name`]
/// - the case body, cloned verbatim
/// - no params, no self-param, no return type, no effects, no
///   contracts — the runner calls it as `call_function(name, &[])`
///   and inspects `runtime_errors` for failure details, so any
///   declared signature surface would be unused.
/// - `is_pub: false`, `is_private: false` — visibility is already
///   rejected at the parse site; the synthesized function is
///   module-internal regardless.
/// - the attribute list copied from the TestCase. Slice 4 lifts
///   `#[test(requires=[...])]` / `#[with_provider(...)]` extraction
///   onto `TestCase.attributes`; until then the field carries
///   whatever the parser attached without behavior change.
pub(super) fn lower_test_case_to_function(
    tc: &crate::ast::TestCase,
    mangled_name: String,
) -> Function {
    Function {
        span: tc.span,
        attributes: tc.attributes.clone(),
        doc_comment: tc.doc_comment.clone(),
        is_pub: false,
        is_private: false,
        is_unsafe: false,
        is_comptime: false,
        name: mangled_name,
        generic_params: None,
        params: Vec::new(),
        self_param: None,
        self_is_frozen: false,
        self_is_own: false,
        self_span: None,
        return_type: None,
        effects: None,
        requires: Vec::new(),
        ensures: Vec::new(),
        where_clause: None,
        body: tc.body.clone(),
        stdlib_origin: false,
        deprecation: None,
        unstable: None,
        is_track_caller: false,
        is_gpu: false,
        inline_hint: None,
        is_cold: false,
        lint_overrides: Vec::new(),
        profile_compat: Vec::new(),
        no_effect: Vec::new(),
        abi: None,
    }
}

/// Rewrite every `Item::TestCase` in the program tree to a
/// synthesized `Item::Function` *and* collect the parallel
/// `DiscoveredTest` list in one pass. The mangled function name on
/// each lowered `Item::Function` matches the `fn_name` field on the
/// returned `DiscoveredTest`, so the runner's later
/// `Interpreter::run_test_function(t.fn_name)` finds the entry the
/// standard `register_items` walk already registered.
///
/// Lowering happens *before* the resolver / typechecker run on the
/// program tree. Without that ordering, a typo or undefined-symbol
/// reference inside a test body would slip past name resolution (the
/// no-op `TestCase` arms in resolver / typechecker skip the body
/// unread) and only surface as a runtime error in the per-test loop —
/// breaking the contract that compile failures exit non-zero with no
/// test events emitted.
///
/// Test cases are structural: `Item::TestCase` entries from
/// `test "case" { body }` syntax per design.md § Testing. The
/// convention-based `fn test_*` discovery is gone — helper functions
/// in `_test.kara` files (any name, including `fn test_*`) stay
/// `Item::Function` and are never picked up as tests, closing the
/// silent-skip failure mode where a project written to the design
/// silently ran zero tests because the runner walked `fn test_*`
/// instead of `Item::TestCase`.
pub(super) fn lower_and_discover_test_cases(tree: &mut ProgramTree) -> Vec<DiscoveredTest> {
    let mut tests = Vec::new();
    for (mod_id, module) in tree.modules.iter_mut().enumerate() {
        if module.is_synthetic {
            continue;
        }
        if module.test_items_start.is_none() {
            continue;
        }
        let label = module_label(&module.path);
        let mut new_items: Vec<Item> = Vec::with_capacity(module.items.len());
        for item in module.items.drain(..) {
            match item {
                Item::TestCase(tc) => {
                    let mangled = mangled_test_function_name(&label, tc.name_span.line, &tc.name);
                    tests.push(DiscoveredTest {
                        module_id: mod_id,
                        fn_name: mangled.clone(),
                        // User-visible qualifier — design.md § Testing
                        // pins this to the case-name string verbatim:
                        // the string `--filter` matches against, the
                        // `test` field on every JSONL event.
                        qualified: tc.name.clone(),
                        requires: extract_requires(&tc.attributes),
                        with_providers: extract_with_providers(&tc.attributes),
                        timeout_seconds: extract_timeout_seconds(&tc.attributes),
                    });
                    new_items.push(Item::Function(lower_test_case_to_function(&tc, mangled)));
                }
                other => new_items.push(other),
            }
        }
        module.items = new_items;
    }
    tests
}

/// Pull resource paths out of a `#[test(requires = [a.b, c.d])]` attribute.
/// Other `#[test(...)]` arg shapes are tolerated and ignored, so future
/// slices can add new keys (e.g. `cases = N`) without breaking earlier
/// runners. Non-path expressions in the array are silently dropped — the
/// parser will already have errored if the attribute body is malformed
/// (the typechecker leaves attribute values alone, so what reaches us is
/// well-formed but possibly not a path).
pub(super) fn extract_requires(attributes: &[crate::ast::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attributes {
        if !attr.is_bare("test") {
            continue;
        }
        for arg in &attr.args {
            if arg.name.as_deref() != Some("requires") {
                continue;
            }
            let Some(value) = arg.value.as_ref() else {
                continue;
            };
            if let crate::ast::ExprKind::ArrayLiteral(elems) = &value.kind {
                for elem in elems {
                    if let Some(path) = expr_to_dotted_path(elem) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

/// Pull the per-test timeout from a `#[test(timeout_seconds = N)]` attribute
/// (phase-7 line 847 sub-step 3). Returns the first positive integer value
/// found across the test's attributes; `None` when absent. A non-positive or
/// non-integer value is ignored (it simply doesn't set a per-test override, so
/// the kara.toml / env / 30 s chain applies) — the parser already accepts any
/// expression in attribute args, and silently dropping a malformed value
/// matches `extract_requires`' tolerant stance toward unknown `#[test(...)]`
/// arg shapes.
pub(super) fn extract_timeout_seconds(attributes: &[crate::ast::Attribute]) -> Option<u64> {
    for attr in attributes {
        if !attr.is_bare("test") {
            continue;
        }
        for arg in &attr.args {
            if arg.name.as_deref() != Some("timeout_seconds") {
                continue;
            }
            if let Some(value) = arg.value.as_ref() {
                if let crate::ast::ExprKind::Integer(n, _) = &value.kind {
                    if *n > 0 {
                        return Some(*n as u64);
                    }
                }
            }
        }
    }
    None
}

/// Resolve the effective per-test timeout from the precedence chain
/// (phase-7 line 847): a per-test `#[test(timeout_seconds = N)]` attribute >
/// the kara.toml `[test].timeout_seconds` > the `KARAC_TEST_TIMEOUT_SECS` env
/// var > the built-in 30 s default. Each layer is an `Option<u64>` of seconds;
/// the first present wins.
pub(super) fn resolve_test_timeout(
    per_test: Option<u64>,
    manifest_default: Option<u64>,
    env_default: Option<u64>,
) -> std::time::Duration {
    let secs = per_test.or(manifest_default).or(env_default).unwrap_or(30);
    std::time::Duration::from_secs(secs)
}

/// Pull `#[with_provider(resource_path, constructor_expr)]` fixtures out
/// of a function's attribute list. Multiple attributes are preserved in
/// source order (outer-to-inner, matching design.md's stacking rule).
/// Attributes with fewer than two positional args are silently dropped —
/// the parser will already have reported a shape error if the attribute
/// body is malformed.
pub(super) fn extract_with_providers(
    attributes: &[crate::ast::Attribute],
) -> Vec<WithProviderFixture> {
    let mut out = Vec::new();
    for attr in attributes {
        if !attr.is_bare("with_provider") {
            continue;
        }
        if attr.args.len() < 2 {
            continue;
        }
        // Expect two positional args (name is None); tolerate named-
        // attribute shape by pulling values only when present.
        let Some(resource_expr) = attr.args[0].value.as_ref() else {
            continue;
        };
        if attr.args[1].value.is_none() {
            continue;
        }
        let Some(resource_path) = expr_to_dotted_path(resource_expr) else {
            continue;
        };
        out.push(WithProviderFixture { resource_path });
    }
    out
}

/// Reconstruct a dotted-path string from a parsed expression. The parser
/// breaks `db.UserDB` into `FieldAccess(Path(["db"]), "UserDB")` (and
/// deeper chains nest the same way), so walking the AST left-to-right
/// produces the original surface text. Returns `None` for anything
/// that is not a pure dotted identifier chain — such elements simply do
/// not contribute a resource entry.
pub(super) fn expr_to_dotted_path(expr: &crate::ast::Expr) -> Option<String> {
    use crate::ast::ExprKind;
    match &expr.kind {
        ExprKind::Identifier(name) => Some(name.clone()),
        ExprKind::Path { segments, .. } => {
            if segments.is_empty() {
                None
            } else {
                Some(segments.join("."))
            }
        }
        ExprKind::FieldAccess { object, field } => {
            let prefix = expr_to_dotted_path(object)?;
            Some(format!("{prefix}.{field}"))
        }
        _ => None,
    }
}

/// True iff the resource is reachable. Order of precedence matches
/// `docs/design.md § Testing › Resource availability probing`:
///   1. `[test.resources]` shell command — present iff the manifest
///      lists one for this resource path; available iff exit 0.
///   2. Env var `KARA_RESOURCE_<UPPER_DOT_SLASH_>` (dots → underscores)
///      — available iff set and non-empty.
pub(super) fn probe_resource(
    resource: &str,
    overrides: &std::collections::BTreeMap<String, String>,
) -> bool {
    if let Some(cmd) = overrides.get(resource) {
        return run_health_check(cmd);
    }
    let env_var = resource_env_var(resource);
    matches!(std::env::var(&env_var), Ok(v) if !v.is_empty())
}

/// Translate a dotted resource path into the env-var probe name. Matches
/// the design (`KARA_RESOURCE_DB_USERDB` from `db.UserDB`): the prefix is
/// fixed so the namespace is reserved, dots become underscores so the
/// shell can set the variable without quoting, and the result is upper-
/// cased so case-insensitive shells (Windows `cmd`) still hit it.
pub(super) fn resource_env_var(resource: &str) -> String {
    format!(
        "KARA_RESOURCE_{}",
        resource.replace('.', "_").to_uppercase()
    )
}

/// Run a shell health-check command and report whether it succeeded.
/// Uses `sh -c` so users can write the command exactly as they would
/// in a terminal (pipes, env-var interpolation, quoting). Stdout and
/// stderr are captured (not forwarded) so a noisy probe does not
/// pollute the JSONL stream — the only signal we care about is the
/// exit code.
pub(super) fn run_health_check(cmd: &str) -> bool {
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

/// What both test runners start from: the package's manifest, its module
/// tree with every `test` case lowered to a function, and the cases to run
/// (filtered, in a stable order). Every failure on the way is reported as a
/// JSONL event and exits 1 before any `run_start`.
pub(super) struct TestFront {
    pub mf: manifest::Manifest,
    pub tree: ProgramTree,
    pub tests: Vec<DiscoveredTest>,
}

/// `core_places`: the tests run on v2, so the tree check takes `ref` on any
/// named place ([`crate::lints::CliLintOverrides::core_places`]).
pub(super) fn test_front(filter: Option<&str>, core_places: bool) -> TestFront {
    match test_front_checked(filter, core_places) {
        Ok(front) => front,
        Err(f) => {
            for entry in parse_errors_jsonl(&f.parse_errors) {
                println!("{entry}");
            }
            for entry in cycles_jsonl(&f.cycles, &f.tree) {
                println!("{entry}");
            }
            for entry in resolve_errors_jsonl(&f.resolve_errors) {
                println!("{entry}");
            }
            for entry in type_errors_jsonl(&f.type_errors) {
                println!("{entry}");
            }
            process::exit(1);
        }
    }
}

/// The package's test files that failed to parse, resolve or check, which
/// `karac test` reports as JSONL and `karac check` in its own output mode.
pub(super) struct TestFrontErrors {
    pub tree: ProgramTree,
    pub parse_errors: Vec<ModuleParseErrors>,
    pub cycles: Vec<crate::module::Cycle>,
    pub resolve_errors: Vec<ModuleResolveErrors>,
    pub type_errors: Vec<ModuleTypeErrors>,
}

/// [`test_front`], returning the compile failure instead of reporting it.
pub(super) fn test_front_checked(
    filter: Option<&str>,
    core_places: bool,
) -> Result<TestFront, Box<TestFrontErrors>> {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            process::exit(1);
        }
    };

    let (root, mf) = match manifest::load_from_cwd(&cwd) {
        Ok(ok) => ok,
        Err(e) => {
            // Surface manifest errors as a JSONL diagnostic event so consumers
            // can recognize and act on them; then exit non-zero before any
            // run_start/summary so the schema stays clean (no half-runs).
            emit_test_event(
                "manifest_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };
    // Merge `[target.<triple>].dependencies` / `.dev-dependencies` overlays for
    // the host-default triple so per-target deps participate in test-mode
    // resolution exactly as they do under `karac build` (resolver follow-up
    // (e)). Applied before the `has_resolvable_deps` gate so a project whose
    // deps are declared *only* under `[target.*]` still resolves.
    let mf = manifest::merge_target_overlay(&mf, Some(&default_resolution_target(&mf)));

    // Toolchain pin (tracker line 892). Same enforcement as
    // cmd_build_project — runs before walk so a failing toolchain
    // gate halts before any test run_start lands in the stream.
    if !enforce_toolchain_pin(&root, OutputMode::Jsonl) {
        process::exit(1);
    }

    // Test-mode dep resolution (tracker line 884). Runs only when the
    // manifest declares at least one dep entry (regular, dev, or
    // workspace) — solo packages pay zero overhead. dev-dependencies
    // participate here (the test-vs-build split) so a test_dep declared
    // under `[dev-dependencies]` is resolved and recorded into the
    // lockfile alongside the build-mode deps. Errors surface as a
    // `dep_resolution_error` event and abort before any run_start.
    // The resolution is kept (resolver-block follow-up (f)): its
    // path-dep packages are walked below so root-package tests can
    // `import <pkg>.…` exactly as production code under `karac build`
    // can. `run_dep_resolution` emits through `emit_dep_diagnostic`,
    // whose Jsonl arm produces the same `dep_resolution_error`
    // envelope as before (registry/git unsupported still downgrade —
    // they surface as `dep_resolution_warning` and resolution is
    // skipped, matching the build flow).
    let has_resolvable_deps =
        !mf.dependencies.is_empty() || !mf.dev_dependencies.is_empty() || mf.kara_version.is_some();
    let dep_resolution: Option<crate::dep_resolver::Resolution> = if has_resolvable_deps {
        // `karac test` has no `--no-proxy` flag; the fetch path self-gates on
        // an explicitly-configured proxy (see `run_dep_resolution`), so a
        // registry dep is fetched only when the operator points at a real one.
        match run_dep_resolution(
            &root,
            mf.clone(),
            OutputMode::Jsonl,
            None,
            true,
            false,
            true,
        ) {
            Ok(r) => r,
            Err(()) => process::exit(1),
        }
    } else {
        None
    };

    let walk_opts = WalkerOpts {
        include_tests: true,
        ..WalkerOpts::default()
    };
    let walked = match walker::walk_project(&root, walk_opts) {
        Ok(w) => w,
        Err(e) => {
            emit_test_event(
                "walker_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };

    // Cross-package module loading for the test surface (phase-5 line
    // 898 follow-up iii / resolver-block follow-up (f)): walk each
    // resolved path-dep so its modules join the tree under package-
    // prefixed paths. Dep test companions stay excluded —
    // `dep_package_walks` walks deps with `include_tests: false`, so
    // `merge_test_companions` below only ever folds the *root*
    // package's `_test.kara` files; only the root package's tests run.
    let dep_walks = match dep_package_walks(
        &root,
        dep_resolution.as_ref(),
        walk_opts.target,
        OutputMode::Jsonl,
    ) {
        Ok(v) => v,
        Err(()) => process::exit(1),
    };

    let built = match module::build_program_tree_with_deps(
        &walked,
        &dep_walks,
        BuildTreeOpts {
            merge_test_companions: true,
        },
    ) {
        Ok(ok) => ok,
        Err(e) => {
            emit_test_event(
                "build_tree_error",
                &format!("\"message\":{}", json_string(&e.to_string())),
            );
            process::exit(1);
        }
    };

    let BuildTreeOk {
        mut tree,
        parse_errors,
    } = built;

    // Lower every `Item::TestCase` to a synthesized `Item::Function`
    // and collect the parallel `DiscoveredTest` list, *before* resolve
    // / typecheck run. Putting the lowering ahead of name resolution
    // is what gives the runner its compile-failure contract: an
    // undefined symbol inside a test body produces a resolve error
    // here at the global step, and the runner exits non-zero with no
    // test events. See `lower_and_discover_test_cases`.
    let discovered_tests = lower_and_discover_test_cases(&mut tree);

    let cycles = module::detect_cycles(&tree);

    // The v2 library a build appends to every module (B-2026-10-24-43), so
    // a test file sees `NetError` or `NetworkProvider` as the build does.
    let lib = if parse_errors.is_empty() && cycles.is_empty() {
        super::build_cmds::PackageLibrary::of(&tree)
    } else {
        None
    };
    let resolve_errors: Vec<ModuleResolveErrors> = if parse_errors.is_empty() && cycles.is_empty() {
        resolve_modules(&tree, lib.as_ref())
    } else {
        Vec::new()
    };

    // Phase-8 line 49 prereq 4 — mirror the build path: lift
    // `[lints].allow_unstable_api` from the manifest into the
    // per-module typecheck overrides so `karac test` honors the
    // global opt-in.
    let mut module_lint_overrides = crate::lints::CliLintOverrides::default();
    module_lint_overrides.apply_manifest_lints(&mf.lints);
    module_lint_overrides.core_places = core_places;
    let type_errors: Vec<ModuleTypeErrors> =
        if parse_errors.is_empty() && cycles.is_empty() && resolve_errors.is_empty() {
            typecheck_modules(&tree, &module_lint_overrides, lib.as_ref()).errors
        } else {
            Vec::new()
        };

    // The v2 runner (`core_places`) leaves a type refusal to the v2 check
    // of the combined source, which `karac check` and the build trust over
    // the per-module pass, so the three agree on what a package is
    // (B-2026-10-25-5, B-2026-10-30-4).
    let compile_failed = !parse_errors.is_empty()
        || !cycles.is_empty()
        || !resolve_errors.is_empty()
        || (!type_errors.is_empty() && !core_places);

    if compile_failed {
        return Err(Box::new(TestFrontErrors {
            tree,
            parse_errors,
            cycles,
            resolve_errors,
            type_errors,
        }));
    }

    // Apply filter to the discovery list built before resolve. Sort
    // by (module_id, fn_name) so order is stable across runs —
    // declaration order within a module (each case lives on a
    // distinct source line, and the mangled name embeds the line, so
    // sorting by mangled name matches source order), modules in walk
    // order. LLM consumers diffing two test runs depend on this.
    let mut tests = discovered_tests;
    if let Some(needle) = filter {
        tests.retain(|t| t.qualified.contains(needle));
    }
    tests.sort_by(|a, b| {
        a.module_id
            .cmp(&b.module_id)
            .then_with(|| a.fn_name.cmp(&b.fn_name))
    });
    Ok(TestFront { mf, tree, tests })
}

/// Subset of `requires` whose resources are NOT currently available.
/// Order is preserved from the source list so the diagnostic reads in
/// declaration order — the runner emits this slice into the
/// `resources` field of the `test_skip`/`test_fail` event.
pub(super) fn missing_resources(
    requires: &[String],
    overrides: &std::collections::BTreeMap<String, String>,
) -> Vec<String> {
    requires
        .iter()
        .filter(|r| !probe_resource(r, overrides))
        .cloned()
        .collect()
}

pub(super) fn test_skip_unsatisfied_requires_fields(
    t: &DiscoveredTest,
    missing: &[String],
) -> String {
    format!(
        "\"test\":{},\"reason\":\"unsatisfied_requires\",\"resources\":{}",
        json_string(&t.qualified),
        json_string_array(missing),
    )
}

pub(super) fn test_fail_unsatisfied_requires_fields(
    t: &DiscoveredTest,
    missing: &[String],
) -> String {
    // `--all` promotes the same condition to a failure. The shape mirrors a
    // normal `test_fail` (test, message) plus a `reason` + `resources` pair
    // so consumers that filter by `reason` work uniformly across skip- and
    // fail-events. `duration_ms` is 0 — the test never executed.
    let message = format!(
        "required resource{} unavailable: {}",
        if missing.len() == 1 { "" } else { "s" },
        missing.join(", "),
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"unsatisfied_requires\",\"resources\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string_array(missing),
        json_string(&message),
    )
}

/// Render a `Vec<String>` as a JSON array literal. Each element runs
/// through [`json_string`] for proper escaping.
pub(super) fn json_string_array(items: &[String]) -> String {
    let mut s = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&json_string(item));
    }
    s.push(']');
    s
}

pub(super) fn test_fail_fields(
    t: &DiscoveredTest,
    outcome: &TestOutcome,
    file_path: &str,
    duration_ms: u128,
) -> String {
    let mut s = format!(
        "\"test\":{},\"duration_ms\":{}",
        json_string(&t.qualified),
        duration_ms
    );
    if let Some(span) = &outcome.span {
        s.push_str(&format!(
            ",\"location\":{{\"file\":{},\"line\":{},\"col\":{}}}",
            json_string(file_path),
            span.line,
            span.column,
        ));
    }
    let message = outcome.message.as_deref().unwrap_or("test failed");
    s.push_str(&format!(",\"message\":{}", json_string(message)));
    // Typed fault category for contract failures (design.md § Contracts rule 2,
    // phase-9 step 7): so a consumer (CI / LLM) filters on a stable field rather
    // than string-matching the human message. Derived from the interpreter's
    // canonical fault text — the single source of truth that already
    // distinguishes the two categories (eval_call / method_call). Only emitted
    // for contract faults; ordinary assertion / panic failures carry no
    // `category`, same conditional-presence convention as `left`/`right`.
    if let Some(category) = contract_fault_category(message) {
        s.push_str(&format!(",\"category\":{}", json_string(category)));
    }
    if let Some(assertion) = &outcome.assertion {
        s.push_str(&format!(",\"assertion\":{}", json_string(assertion)));
    }
    if let Some(left) = &outcome.left {
        s.push_str(&format!(",\"left\":{}", json_string(left)));
    }
    if let Some(right) = &outcome.right {
        s.push_str(&format!(",\"right\":{}", json_string(right)));
    }
    s
}

/// Classify a test-failure message into a typed contract-fault category, or
/// `None` for a non-contract failure (assertion, plain panic, timeout, infra).
/// `contract predicate panicked` is checked **first**: a nested fault message
/// can read `contract predicate panicked: contract violated: …` (a contract
/// violation surfaced from inside a predicate's evaluation), which is a
/// predicate-panic, not a violation. The match strings are the canonical fault
/// names from design.md, emitted by both the interpreter (`eval_call` /
/// `method_call`) and codegen (`emit_panic`), so they don't drift.
pub(super) fn contract_fault_category(message: &str) -> Option<&'static str> {
    if message.contains("contract predicate panicked") {
        Some("contract_predicate_panicked")
    } else if message.contains("contract violated") {
        Some("contract_violated")
    } else {
        None
    }
}

/// Like `test_fail_fields` but also emits a `providers` array listing
/// the fully-qualified resource paths active for the test. Per design.md
/// § Testing, pass events stay lean; only failure events grow this
/// field so consumers reading pass/fail diffs can attribute the failure
/// to the fixture stack. Empty provider lists still emit the field for
/// shape consistency — it's `"providers":[]` in that case.
pub(super) fn test_fail_fields_with_providers(
    t: &DiscoveredTest,
    outcome: &TestOutcome,
    file_path: &str,
    duration_ms: u128,
    providers: &[String],
) -> String {
    let mut s = test_fail_fields(t, outcome, file_path, duration_ms);
    s.push_str(&format!(",\"providers\":{}", json_string_array(providers)));
    s
}

/// Intersection of `#[test(requires = [...])]` resources and
/// `#[with_provider(...)]` resource paths. Preserves `requires` order so
/// the conflict list reads in source declaration order.
pub(super) fn conflict_resources(
    requires: &[String],
    with_providers: &[WithProviderFixture],
) -> Vec<String> {
    let with_set: std::collections::BTreeSet<&str> = with_providers
        .iter()
        .map(|f| f.resource_path.as_str())
        .collect();
    requires
        .iter()
        .filter(|r| with_set.contains(r.as_str()))
        .cloned()
        .collect()
}

pub(super) fn test_fail_conflict_fields(t: &DiscoveredTest, conflicts: &[String]) -> String {
    let message = format!(
        "resource{} cannot appear in both `requires` and `with_provider`: {}",
        if conflicts.len() == 1 { "" } else { "s" },
        conflicts.join(", "),
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"requires_and_with_provider_conflict\",\"resources\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string_array(conflicts),
        json_string(&message),
    )
}

/// `test_fail` event for `provider_construction_failed` — constructor
/// expression panicked, returned `Err`, or otherwise did not complete
/// normally. `duration_ms` is 0 — the test body never ran. Includes the
/// resource path whose constructor failed and the diagnostic message so
/// CI / LLM consumers can distinguish construction failures from test-
/// body failures.
pub(super) fn test_fail_provider_construction_fields(
    t: &DiscoveredTest,
    resource: &str,
    message: &str,
) -> String {
    let wrapped = format!(
        "provider for `{}` failed to construct: {}",
        resource, message
    );
    format!(
        "\"test\":{},\"duration_ms\":0,\"reason\":\"provider_construction_failed\",\"resource\":{},\"message\":{}",
        json_string(&t.qualified),
        json_string(resource),
        json_string(&wrapped),
    )
}
