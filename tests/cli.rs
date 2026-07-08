//! CLI and diagnostic output integration tests.
//!
//! These are golden-file snapshot tests that freeze the diagnostic output format.
//! If a test fails, either the output format has regressed (fix the code) or
//! the format intentionally changed (update the expected output).

mod common;

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};

/// Wrapper around `Command` that routes `.output()` through the
/// shared `output_with_hang_watchdog` helper (15 s timeout per call)
/// so a hung `karac` invocation can't lock up the cli test suite — same
/// concurrent-spawn-deadlock defense the codegen suite picked up in
/// commit `62af025`. Builder methods (`.arg`, `.args`, `.current_dir`,
/// `.env`, `.env_remove`) delegate to the inner `Command` so call sites
/// keep their existing chain shape verbatim — only the `karac_bin()`
/// return type changed. Each cli test does at most a handful of fast
/// karac invocations, so the per-spawn cost of one extra watchdog
/// thread is invisible at suite scale.
struct KaracBin(Command);

impl KaracBin {
    fn arg<S: AsRef<OsStr>>(mut self, arg: S) -> Self {
        self.0.arg(arg);
        self
    }

    fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.0.args(args);
        self
    }

    fn current_dir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.0.current_dir(dir);
        self
    }

    fn env<K: AsRef<OsStr>, V: AsRef<OsStr>>(mut self, k: K, v: V) -> Self {
        self.0.env(k, v);
        self
    }

    fn env_remove<K: AsRef<OsStr>>(mut self, k: K) -> Self {
        self.0.env_remove(k);
        self
    }

    fn output(self) -> std::io::Result<Output> {
        common::output_with_hang_watchdog(self.0, std::time::Duration::from_secs(15))
            .ok_or_else(|| std::io::Error::other("karac child spawn failed"))
    }
}

fn karac_bin() -> KaracBin {
    KaracBin(Command::new(env!("CARGO_BIN_EXE_karac")))
}

// ── Help & Version ──────────────────────────────────────────────

#[test]
fn test_help_output() {
    let out = karac_bin().arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac - The Kara language compiler"));
    assert!(stdout.contains("COMMANDS:"));
    assert!(stdout.contains("run"));
    assert!(stdout.contains("check"));
    assert!(stdout.contains("build"));
    assert!(stdout.contains("query"));
    assert!(stdout.contains("fmt"));
}

#[test]
fn test_version_output() {
    let out = karac_bin().arg("version").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac 0.1.0"));
}

#[test]
fn test_no_args_shows_help() {
    let out = karac_bin().output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("COMMANDS:"));
}

// ── Subcommand-scoped --help ────────────────────────────────────

#[test]
fn test_subcommand_help_init() {
    for flag in ["--help", "-h"] {
        let out = karac_bin().args(["init", flag]).output().unwrap();
        assert!(out.status.success(), "`karac init {flag}` should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("karac init"));
        assert!(stdout.contains("--bin"));
        assert!(stdout.contains("--lib"));
        assert!(stdout.contains("--force"));
    }
}

#[test]
fn test_subcommand_help_run() {
    let out = karac_bin().args(["run", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac run"));
    assert!(stdout.contains("--sequential"));
}

#[test]
fn test_subcommand_help_check() {
    let out = karac_bin().args(["check", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac check"));
    assert!(stdout.contains("--output=json"));
}

#[test]
fn test_subcommand_help_build() {
    let out = karac_bin().args(["build", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac build"));
    assert!(stdout.contains("kara.toml"));
    // Phase-7 line 5 sub-item 1 — flag is listed in build's help.
    assert!(stdout.contains("--enable-hot-swap"));
    // Tracker line 880 — --offline + OFFLINE section documented.
    assert!(stdout.contains("--offline"));
    assert!(stdout.contains("OFFLINE:"));
    assert!(stdout.contains("E_OFFLINE_NO_VENDOR_DIR"));
    // Tracker line 882 — --target + TARGETS section documented.
    assert!(stdout.contains("--target=<triple>"));
    assert!(stdout.contains("TARGETS:"));
}

#[test]
fn test_subcommand_help_query() {
    let out = karac_bin().args(["query", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac query"));
    assert!(stdout.contains("effects"));
    assert!(stdout.contains("ownership"));
    assert!(stdout.contains("concurrency"));
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 3 —
    // the `queries` kind shows up in help.
    assert!(stdout.contains("queries"));
}

#[test]
fn test_query_queries_empty_envelope() {
    // Phase-8 stdlib-floor § Compiler queries channel sub-item 3.
    // v1 catalogue is empty; the envelope shape `{"queries":[]}`
    // is the contract for external tooling pinning to the command.
    // Surface lock — populating queries from any phase must not
    // break the envelope schema.
    let out = karac_bin()
        .args(["query", "queries", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0 on a clean program; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"queries\":[]"),
        "expected empty queries envelope; got stdout={stdout}",
    );
}

#[test]
fn test_query_queries_populated_envelope_has_inlining_query() {
    // Phase-7 line 25 (P1.3 codegen queries) — the codegen-queries
    // analyzer surfaces an inlining-decision query for a pub fn
    // that's called from three hot-looking sites. The CLI must
    // serialize it into the envelope with the expected `kind`,
    // `id`, and resolution surface so external tooling can render
    // the fix without parsing the body.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-populated-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("hot.kara");
    let src = r#"
pub fn step(x: i64) -> i64 { x + 1 }
fn main() {
    let mut acc: i64 = 0;
    for i in 0..100 {
        acc = step(acc);
        acc = step(acc);
        acc = step(acc);
    }
    println(f"{acc}");
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "queries", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"inlining_decision\""),
        "expected an inlining_decision entry; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"id\":\"step\""),
        "expected query id `step`; got stdout={stdout}",
    );
    // Both halves of the resolution surface are exposed so tools can
    // pick either direction as the suggested fix.
    assert!(
        stdout.contains("\"inline\""),
        "expected `inline` in resolution_surface; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"inline(never)\""),
        "expected `inline(never)` in resolution_surface; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"cross_phase_origin\":\"codegen\""),
        "expected codegen-origin tag; got stdout={stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_queries_populated_envelope_has_specialization_query() {
    // Phase-8 (P1.2 specialization queries) — the specialization-queries
    // analyzer surfaces one fan-out query for a generic free function
    // monomorphized into four distinct type tuples. The CLI must
    // serialize it into the same `{"queries":[…]}` envelope with the
    // `specialization_decision` kind and the `specialize` resolution
    // surface, alongside any codegen (P1.3) queries.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-spec-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("fanout.kara");
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(2i32);
    let _ = identity(3u8);
    let _ = identity(4u64);
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "queries", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"specialization_decision\""),
        "expected a specialization_decision entry; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"id\":\"identity\""),
        "expected query id `identity`; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"specialize\""),
        "expected `specialize` in resolution_surface; got stdout={stdout}",
    );
    // Fan-out folded into options: a per-tuple option plus the count in
    // the default note.
    assert!(
        stdout.contains("\"specialize_i64\"") && stdout.contains("4 distinct type tuples"),
        "expected per-tuple options + fan-out count; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"cross_phase_origin\":\"typechecker\""),
        "expected typechecker-origin tag; got stdout={stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_queries_populated_envelope_has_rc_fallback_query() {
    // Phase-8 (P1.1 RC-fallback queries) — the ownership pass RC-promotes
    // `o` (captured by-value into a closure, used again after), and the
    // rc-fallback-queries analyzer surfaces one query for it. The CLI must
    // serialize it into the `{"queries":[…]}` envelope with the
    // `rc_fallback_decision` kind and the `no_rc`/`prefer_rc` surface.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-rc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("rc.kara");
    let src = r#"
struct Owned { x: i64 }
fn take(o: Owned) { }
fn main() {
    let o = Owned { x: 1 };
    let _f = || take(o);
    let _u = o;
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "queries", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"rc_fallback_decision\""),
        "expected an rc_fallback_decision entry; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"no_rc\"") && stdout.contains("\"prefer_rc\""),
        "expected the no_rc/prefer_rc resolution surface; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"cross_phase_origin\":\"ownership\""),
        "expected ownership-origin tag; got stdout={stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_queries_populated_envelope_has_fork_threshold_query() {
    // Phase-8 (P1.6 fork-threshold queries) — two effectful calls on
    // independent resources form a non-trivial parallel group the
    // auto-parallelizer forks; the fork-threshold analyzer surfaces one
    // query advertising `#[fork_at]`.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-fork-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("fork.kara");
    let src = r#"
effect resource R1;
effect resource R2;
fn w1() writes(R1) {}
fn w2() writes(R2) {}
fn main() {
    w1();
    w2();
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "queries", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"fork_threshold_decision\""),
        "expected a fork_threshold_decision entry; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"fork_at\""),
        "expected the fork_at resolution surface; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"cross_phase_origin\":\"concurrency\""),
        "expected concurrency-origin tag; got stdout={stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_queries_populated_envelope_has_layout_query() {
    // Phase-8 (P1.5 layout-choice queries) — a loop over `Vec[Entity]`
    // that reads a strict subset of the struct's fields is a
    // struct-of-arrays candidate; the layout analyzer surfaces one query.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-layout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("layout.kara");
    let src = r#"
struct Entity { x: f64, y: f64, hp: i64 }
fn sum_x(entities: Vec[Entity]) -> f64 {
    let mut total: f64 = 0.0;
    for e in entities {
        total = total + e.x;
    }
    total
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "queries", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac query queries should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"layout_choice\""),
        "expected a layout_choice entry; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"id\":\"sum_x") && stdout.contains("group_hot_fields"),
        "expected the sum_x layout query with a group option; got stdout={stdout}",
    );
    assert!(
        stdout.contains("\"cross_phase_origin\":\"codegen\""),
        "expected codegen-origin tag; got stdout={stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_subcommand_help_fmt() {
    let out = karac_bin().args(["fmt", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac fmt"));
}

#[test]
fn test_subcommand_help_does_not_scaffold() {
    // Guard against regressions where `karac init --help` would fall through
    // and actually run the scaffolder in the CWD.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-init-help-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let out = karac_bin()
        .args(["init", "--help"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!tmp.join("kara.toml").exists());
    assert!(!tmp.join("src").exists());
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Run ─────────────────────────────────────────────────────────

#[test]
fn test_run_clean_program() {
    let out = karac_bin()
        .args(["run", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("x = 42"));
}

#[test]
fn test_run_bare_file_shorthand() {
    let out = karac_bin()
        .arg("tests/snapshots/clean.kara")
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("x = 42"));
}

#[test]
fn test_run_resolve_error_exits_nonzero() {
    let out = karac_bin()
        .args(["run", "tests/snapshots/resolve_error.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[resolve]"));
    assert!(stderr.contains("undefined name"));
}

// ── Check ───────────────────────────────────────────────────────

#[test]
fn test_check_clean_program() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("All checks passed"));
}

#[test]
fn test_check_parse_error() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/parse_error.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[parse]"));
    assert!(stderr.contains("2:13")); // line:column
}

#[test]
fn test_check_type_error() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/type_error.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("error[typecheck]"));
    assert!(stderr.contains("expected 2 argument(s), found 3"));
}

// `Pipeline::has_fatal_errors` includes typecheck errors so `karac build`
// stops at the typechecker's diagnostic instead of proceeding into
// codegen and emitting a misleading "no handler for method 'unwrap'"
// downstream. Pins the user-visible behavior change — without the
// `has_type_errors` extension, this test would see the codegen error
// in stderr instead of the typecheck one. Surfaced 2026-05-22 building
// the kata-91 bench mirror.
#[cfg(feature = "llvm")]
#[test]
fn test_build_typecheck_error_does_not_fall_through_to_codegen() {
    let out = karac_bin()
        .args(["build", "tests/snapshots/undeclared_assoc_method.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "build should exit nonzero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[typecheck]"),
        "expected typecheck error in stderr, got: {stderr}"
    );
    assert!(
        stderr.contains("no associated function 'from_utf16' on type 'String'"),
        "expected the new NoMethodFound diagnostic text, got: {stderr}"
    );
    assert!(
        !stderr.contains("codegen failed"),
        "build should stop at typecheck — codegen must not run for this input. \
         stderr: {stderr}"
    );
    assert!(
        !stderr.contains("no handler for method 'unwrap'"),
        "the old misleading codegen diagnostic must not surface. stderr: {stderr}"
    );
}

#[test]
fn test_check_provider_escape_error() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/provider_escape_error.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[provider_escape]"),
        "expected provider_escape error, got: {}",
        stderr
    );
    assert!(
        stderr.contains("Clock"),
        "expected resource name Clock in message, got: {}",
        stderr
    );
    assert!(
        stderr.contains("escapes its provider scope"),
        "expected escape message, got: {}",
        stderr
    );
}

#[test]
fn test_check_provider_escape_error_json() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/provider_escape_error.kara",
            "--output=json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"phase\":\"provider_escape\""),
        "missing phase in JSON output: {}",
        stdout
    );
    assert!(
        stdout.contains("\"code\":\"E0600\""),
        "missing code E0600 in JSON output: {}",
        stdout
    );
    assert!(
        stdout.contains("Clock"),
        "missing resource name in JSON output: {}",
        stdout
    );
}

#[test]
fn test_run_rejects_provider_escape() {
    // `karac run` must abort on escape errors — the spec's "cannot escape"
    // rule breaks test isolation if we silently run. Post-Slice-6 (run-leniency
    // stripped), run rejects the same set `check` rejects: this fixture's
    // escape ALSO manifests as a type error (`expected '()', found 'Fn(...)'`
    // — the closure returns a Clock-bound function), and `karac check` reports
    // both. Run now aborts at the type gate with `error[typecheck]` before the
    // provider_escape gate is reached; the essential property (run does NOT
    // silently execute an escaping program) holds. The provider_escape-specific
    // diagnostic stays covered by `test_check_provider_escape_error*` (which
    // exercise `karac check`, where all diagnostics are collected together).
    let out = karac_bin()
        .args(["run", "tests/snapshots/provider_escape_error.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[typecheck]") || stderr.contains("error[provider_escape]"),
        "run must reject the escaping program with a hard error, got: {}",
        stderr
    );
}

// ── JSON Output Snapshots ───────────────────────────────────────

#[test]
fn test_json_clean_program() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara", "--output=json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    // Verify structure
    assert!(json.starts_with('{'));
    assert!(json.contains("\"program_effects\""));
    assert!(json.contains("\"diagnostics\":[]"));
}

#[test]
fn test_json_parse_error_snapshot() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/parse_error.kara", "--output=json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    // Verify diagnostic fields per spec
    assert!(json.contains("\"severity\":\"error\""));
    assert!(json.contains("\"primary\":true"));
    assert!(json.contains("\"phase\":\"parse\""));
    assert!(json.contains("\"code\":\"E0001\""));
    assert!(json.contains("\"line\":2"));
    assert!(json.contains("\"column\":13"));
    assert!(json.contains("\"message\":"));
}

#[test]
fn test_json_resolve_error_snapshot() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/resolve_error.kara",
            "--output=json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"phase\":\"resolve\""));
    assert!(json.contains("\"code\":\"E0100\""));
    assert!(json.contains("undefined name"));
}

#[test]
fn test_json_type_error_snapshot() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/type_error.kara", "--output=json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"phase\":\"typecheck\""));
    assert!(json.contains("\"code\":\"E0202\""));
    assert!(json.contains("expected 2 argument(s), found 3"));
    // Line 619 slice 2 added a `class` field to typecheck diagnostic
    // records; WrongNumberOfArgs maps to WRONG_NUMBER_OF_ARGS.
    assert!(
        json.contains("\"class\":\"WRONG_NUMBER_OF_ARGS\""),
        "JSON output should carry the class field; got: {}",
        json
    );
}

#[test]
fn test_json_type_mismatch_carries_class_and_typed_fields() {
    // Line 619 slice 4: a TypeMismatch diagnostic emitted via
    // `--output=json` must carry `class`, `expected`, and `got` as
    // structured fields. Write an inline fixture rather than reuse a
    // snapshot so the test owns its trigger shape.
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_test_type_mismatch_json.kara");
    std::fs::write(&fixture, "fn main() {\n    let x: i32 = \"hello\";\n}\n")
        .expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"class\":\"TYPE_MISMATCH\""),
        "JSON output should carry TYPE_MISMATCH class; got: {}",
        stdout
    );
    assert!(
        stdout.contains("\"expected\":\"i32\""),
        "JSON output should carry expected field with 'i32'; got: {}",
        stdout
    );
    assert!(
        stdout.contains("\"got\":"),
        "JSON output should carry got field; got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_json_multiple_errors_snapshot() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/multiple_errors.kara",
            "--output=json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    // Should have 3 diagnostics (d1, d2, d3)
    assert!(json.contains("\"id\":\"d1\""));
    assert!(json.contains("\"id\":\"d2\""));
    assert!(json.contains("\"id\":\"d3\""));
    // All are resolve errors
    assert!(json.matches("\"phase\":\"resolve\"").count() == 3);
}

#[test]
fn test_json_suggestion_in_hints() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/multiple_errors.kara",
            "--output=json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Suggestions should be present as hints
    assert!(stdout.contains("\"hints\":[{\"description\":"));
}

// ── RC-fallback note reaches the default text surface (B-2026-06-13-3) ──

/// Regression: the RC-fallback perf note must render in the default
/// `karac check` *text* output, not only in `--output=json`/LSP.
/// `render_text_diagnostics` (src/cli.rs) once iterated only `o.errors`
/// and silently dropped `o.notes`, so `karac build`/`check` in a terminal
/// said nothing about RC fallback — breaking design.md § Part 4's "the
/// note fires by default" guarantee (the "RC overhead is visible" pillar).
/// Trigger shape mirrors `tests/rc_fallback.rs::trigger1`: a value consumed
/// on one branch and re-consumed after the branch → RC fallback, not error.
#[test]
fn test_rc_fallback_note_renders_in_text_and_json() {
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_test_rc_fallback_note.kara");
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) { }\n\
               fn use_d(d: Data) -> i64 { d.value }\n\
               fn process(cond: bool, d: Data) -> i64 {\n\
               \x20   if cond { consume(d); }\n\
               \x20   use_d(d)\n\
               }\n\
               fn main() {\n\
               \x20   let d = Data { value: 7 };\n\
               \x20   println(f\"{process(false, d)}\");\n\
               }\n";
    std::fs::write(&fixture, src).expect("write fixture");

    // Default text surface — the regression target. RC fallback is a perf
    // note, not an error, so the check still succeeds.
    let text = karac_bin()
        .args(["check", fixture.to_str().unwrap()])
        .output()
        .unwrap();
    let text_out = String::from_utf8_lossy(&text.stdout);
    let text_err = String::from_utf8_lossy(&text.stderr);
    let combined = format!("{text_out}{text_err}");
    assert!(
        combined.contains("perf[rc-fallback]:") && combined.contains("RC fallback inserted for 'd'"),
        "text output must surface the RC-fallback note; got stdout=[{text_out}] stderr=[{text_err}]"
    );
    assert!(
        combined.contains("help: restructure to a single ownership path"),
        "text output must include the RC-fallback help line; got [{combined}]"
    );

    // JSON surface must still carry it (severity note, code N0503).
    let json = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let json_out = String::from_utf8_lossy(&json.stdout);
    assert!(
        json_out.contains("\"code\":\"N0503\"") && json_out.contains("\"severity\":\"note\""),
        "JSON output must still carry the N0503 RC-fallback note; got: {json_out}"
    );

    // `#[allow(rc_fallback)]` suppresses the note at every surface
    // (suppression is applied upstream in `emit_rc_fallback_notes`).
    let allow_src = src.replace(
        "fn process(cond: bool, d: Data) -> i64 {",
        "#[allow(rc_fallback)]\nfn process(cond: bool, d: Data) -> i64 {",
    );
    std::fs::write(&fixture, &allow_src).expect("rewrite fixture");
    let allowed = karac_bin()
        .args(["check", fixture.to_str().unwrap()])
        .output()
        .unwrap();
    let allowed_combined = format!(
        "{}{}",
        String::from_utf8_lossy(&allowed.stdout),
        String::from_utf8_lossy(&allowed.stderr)
    );
    assert!(
        !allowed_combined.contains("perf[rc-fallback]:"),
        "#[allow(rc_fallback)] must suppress the note; got [{allowed_combined}]"
    );

    let _ = std::fs::remove_file(&fixture);
}

// ── Signature-from-call-site stub diagnostic (line 633 slice 3) ────

#[test]
fn test_json_stub_hint_emits_diff_in_test_file() {
    // An unresolved-call in a `_test.kara` file must produce a
    // `hints[].diff` entry pointing at the sibling production file
    // with the rendered stub source. Slice 2 inference flows through
    // — unsuffixed int literals land as `i64` in the diff body.
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_test_stub_hint_test.kara");
    std::fs::write(
        &fixture,
        "fn test_calls_missing() {\n    let _ = add(1, 2);\n}\n",
    )
    .expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_file(&fixture);
    assert!(!out.status.success());
    assert!(
        stdout.contains("\"diff\":{"),
        "expected hints[].diff entry; got: {}",
        stdout
    );
    assert!(
        stdout.contains("stub `add`"),
        "expected stub description; got: {}",
        stdout
    );
    // The sibling production file path is the `_test.kara` →
    // `.kara` munge. Pin the body shape too — i64 args + todo() body.
    assert!(
        stdout.contains("karac_test_stub_hint.kara"),
        "expected sibling production filename in diff; got: {}",
        stdout
    );
    assert!(
        stdout.contains("fn add(arg0: i64, arg1: i64) -> _"),
        "expected i64-inferred stub body; got: {}",
        stdout
    );
    assert!(
        stdout.contains("todo()"),
        "expected todo() body; got: {}",
        stdout
    );
}

#[test]
fn test_json_stub_hint_absent_for_production_file() {
    // Activation gate: a production-file (`*.kara`, not `*_test.kara`)
    // unresolved-call must NOT carry a stub-hint diff. Mirrors the
    // resolver-level gate test; this confirms the CLI emitter
    // respects the absence too.
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_test_stub_hint_prod.kara");
    std::fs::write(&fixture, "fn main() {\n    let _ = add(1, 2);\n}\n").expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_file(&fixture);
    assert!(!out.status.success());
    assert!(
        !stdout.contains("\"diff\":"),
        "production-file diagnostic must not carry hints[].diff; got: {}",
        stdout
    );
}

// ── JSONL Output Snapshots ──────────────────────────────────────

#[test]
fn test_jsonl_clean_program() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara", "--output=jsonl"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.trim().lines().collect();

    // First line: build_start
    assert!(lines[0].contains("\"type\":\"build_start\""));

    // Should have phase_start/phase_complete pairs for each phase
    let phase_starts: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("\"phase_start\""))
        .collect();
    assert!(phase_starts.len() >= 5); // lex, parse, resolve, typecheck, effect, ownership

    // Last line: build_complete
    let last = lines.last().unwrap();
    assert!(last.contains("\"type\":\"build_complete\""));
    assert!(last.contains("\"success\":true"));
    assert!(last.contains("\"total_errors\":0"));
}

#[test]
fn test_jsonl_parse_error_skips_phases() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/parse_error.kara",
            "--output=jsonl",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Should have phase_skipped for resolve, typecheck, effect, ownership
    assert!(stdout.contains("\"type\":\"phase_skipped\""));
    let skipped_count = stdout.matches("\"phase_skipped\"").count();
    assert_eq!(skipped_count, 4);

    // build_complete should show failure
    assert!(stdout.contains("\"success\":false"));
}

#[test]
fn test_jsonl_resolve_error_skips_later_phases() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/resolve_error.kara",
            "--output=jsonl",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // resolve should complete (not be skipped)
    assert!(stdout.contains("\"phase\":\"resolve\""));
    // typecheck, effect, ownership should be skipped
    let skipped_count = stdout.matches("\"phase_skipped\"").count();
    assert_eq!(skipped_count, 3);
}

// ── Query ───────────────────────────────────────────────────────

#[test]
fn test_query_effects_pure_function() {
    let out = karac_bin()
        .args(["query", "effects", "tests/snapshots/type_error.kara.add"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"function\":\"add\""));
    assert!(stdout.contains("\"inferred_effects\":[]"));
    assert!(stdout.contains("\"declared_effects\":null"));
}

#[test]
fn test_query_effects_resolves_instance_method_network_effect() {
    // Phase-8 line 101 regression. Before the fix, the Effects query arm
    // ran `effectcheck()` without `typecheck()`, so `method_callee_types`
    // was empty and an effect reaching a caller *through an instance method*
    // (`c.get(...)`) was invisible — `inferred_effects` came back `[]` even
    // though `build` / `test` correctly propagate `sends`/`receives(Network)`.
    // The query now typechecks first, so the network pair surfaces here too.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-effects-instance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("svc.kara");
    let src = "fn fetch() {\n\
               \x20   let c = Client.new();\n\
               \x20   let r = c.get(\"http://example.com\");\n\
               \x20   match r { Result.Ok(_) => {} Result.Err(_) => {} }\n\
               }\n";
    std::fs::write(&path, src).unwrap();

    let target = format!("{}.fetch", path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "effects", &target])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"function\":\"fetch\""));
    assert!(
        stdout.contains("\"resource\":\"Network\""),
        "instance-method network effect should surface under `query effects`; got: {stdout}",
    );
    assert!(stdout.contains("\"verb\":\"sends\""));
    assert!(stdout.contains("\"verb\":\"receives\""));
}

#[test]
fn test_query_effects_whole_program_emits_nodes_and_call_edges() {
    // A bare `<file>.kara` target (no trailing `.function`) emits the
    // whole-program effect graph: a `functions` array with one node per
    // source function (effects + source line) plus a `calls` array of
    // directed call-graph edges. This is the Cartographer artifact.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-effects-whole-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("graph.kara");
    // `leaf` is pure; `root` calls it twice (a call edge) and reads a
    // user resource through an instance method so a node carries a
    // non-empty effect.
    let src = "fn leaf() -> i64 { 0 }\n\
               fn root() -> i64 {\n\
               \x20   let a = leaf();\n\
               \x20   let b = leaf();\n\
               \x20   a + b\n\
               }\n";
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "effects", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"scope\":"),
        "whole-program envelope; got: {stdout}"
    );
    assert!(stdout.contains("\"functions\":["));
    assert!(stdout.contains("\"function\":\"leaf\""));
    assert!(stdout.contains("\"function\":\"root\""));
    assert!(stdout.contains("\"line\":"));
    assert!(
        stdout.contains("\"caller\":\"root\",\"callee\":\"leaf\""),
        "call edge root->leaf should appear in `calls`; got: {stdout}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_cartograph_json_matches_cli_query_output() {
    // The browser-studio library entry point `karac::effect_graph::cartograph_json`
    // and the CLI `query effects`/`query concurrency` whole-program
    // emitters share the same Pipeline + JSON builders, so the graph must
    // be byte-identical across the two surfaces (the studio can't drift
    // from the CLI). Also pins the result envelope contract.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cartograph-json-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("svc.kara");
    let src = "fn leaf() -> i64 { 0 }\n\
               fn root() -> i64 {\n\
               \x20   let a = leaf();\n\
               \x20   let b = leaf();\n\
               \x20   a + b\n\
               }\n";
    std::fs::write(&path, src).unwrap();
    let target = path.to_str().unwrap();

    let result = karac::effect_graph::cartograph_json(src, target);
    assert!(result.ok, "clean program should produce a graph");
    assert!(result.diagnostics.is_empty());
    assert!(result
        .effects_json
        .contains("\"caller\":\"root\",\"callee\":\"leaf\""));
    assert!(result.concurrency_json.contains("\"function\":\"root\""));

    // Byte-identical to the CLI emitters.
    let cli_effects = karac_bin()
        .args(["query", "effects", target])
        .output()
        .unwrap();
    let cli_conc = karac_bin()
        .args(["query", "concurrency", target])
        .output()
        .unwrap();
    assert_eq!(
        result.effects_json,
        String::from_utf8_lossy(&cli_effects.stdout).trim_end(),
        "cartograph_json effects must match `query effects` byte-for-byte",
    );
    assert_eq!(
        result.concurrency_json,
        String::from_utf8_lossy(&cli_conc.stdout).trim_end(),
        "cartograph_json concurrency must match `query concurrency` byte-for-byte",
    );

    // Fatal parse error → no graph, diagnostics populated.
    let bad = karac::effect_graph::cartograph_json("fn main( {", "bad.kara");
    assert!(!bad.ok);
    assert!(bad.effects_json.is_empty());
    assert!(!bad.diagnostics.is_empty());
    assert_eq!(bad.diagnostics[0].phase, "parse");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_whole_program_generic_receiver_method_joins_keys() {
    // B-2026-06-14-3 regression. The whole-program emitters look effects /
    // concurrency up by the call-graph node key, but the call graph keyed
    // an impl method by the rendered receiver (`Box[T].sizes`) while the
    // effect checker and concurrency analysis key by the bare base name
    // (`Box.sizes`). For a GENERIC receiver the keys diverged, so the
    // method's effects came back empty under `query effects` and the
    // method vanished entirely from `query concurrency`. Both must now
    // join: the node is keyed `Box.sizes`, carries its `allocates(Heap)`
    // effect, and appears in the concurrency report.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-query-generic-join-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("gen.kara");
    let src = "struct Box[T] { value: T }\n\
               impl[T] Box[T] {\n\
               \x20   fn sizes(ref self) -> i64 {\n\
               \x20       let a: Vec[i64] = Vec.new();\n\
               \x20       let b: Vec[i64] = Vec.new();\n\
               \x20       a.len() + b.len()\n\
               \x20   }\n\
               }\n\
               fn main() {\n\
               \x20   let bx = Box { value: 5 };\n\
               \x20   println(f\"{bx.sizes()}\");\n\
               }\n";
    std::fs::write(&path, src).unwrap();
    let target = path.to_str().unwrap();

    let eff = karac_bin()
        .args(["query", "effects", target])
        .output()
        .unwrap();
    assert!(eff.status.success());
    let eff_out = String::from_utf8_lossy(&eff.stdout);
    // Bare base-name key, not the rendered generic form.
    assert!(
        eff_out.contains("\"function\":\"Box.sizes\""),
        "node should key by bare base name `Box.sizes`; got: {eff_out}",
    );
    assert!(
        !eff_out.contains("Box[T].sizes"),
        "node must not key by the rendered generic form; got: {eff_out}",
    );
    // Effect lookup must join — the method allocates, so the node is not pure.
    assert!(
        eff_out.contains("\"verb\":\"allocates\",\"resource\":\"Heap\""),
        "generic-receiver method's effect should join, not report empty; got: {eff_out}",
    );

    let conc = karac_bin()
        .args(["query", "concurrency", target])
        .output()
        .unwrap();
    assert!(conc.status.success());
    let conc_out = String::from_utf8_lossy(&conc.stdout);
    assert!(
        conc_out.contains("\"function\":\"Box.sizes\""),
        "generic-receiver method must appear in the concurrency report, not be dropped; got: {conc_out}",
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_ownership() {
    let out = karac_bin()
        .args(["query", "ownership", "tests/snapshots/type_error.kara.add"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"function\":\"add\""));
    assert!(stdout.contains("\"parameters\":["));
    assert!(stdout.contains("\"name\":\"a\""));
    assert!(stdout.contains("\"name\":\"b\""));
    // Round 12.25: closures array is always present (empty for
    // functions without closures).
    assert!(stdout.contains("\"closures\":[]"));
}

#[test]
fn test_query_ownership_surfaces_closures() {
    // Round 12.25: closures created inside the queried function are
    // listed with their inferred param modes and captures. The
    // snapshot has one consume-param closure (no capture) and one
    // bare-capture closure (no param).
    let out = karac_bin()
        .args([
            "query",
            "ownership",
            "tests/snapshots/closure_query.kara.main",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"function\":\"main\""));
    // First closure: |x: Data| take(x) — consume-param, no capture.
    assert!(
        stdout.contains("\"name\":\"x\",\"mode\":\"own\""),
        "expected consume-param entry; got: {stdout}"
    );
    // Second closure: || d.v — no params, ref-capture of d.
    assert!(
        stdout.contains("\"name\":\"d\",\"mode\":\"ref\""),
        "expected ref-capture entry; got: {stdout}"
    );
    // Each closure's source location is surfaced.
    assert!(
        stdout.contains("\"line\":7"),
        "expected line 7 for first closure; got: {stdout}"
    );
    assert!(
        stdout.contains("\"line\":8"),
        "expected line 8 for second closure; got: {stdout}"
    );
}

#[test]
fn test_query_ownership_closure_array_filtered_per_function() {
    // Querying a function with no closures must still emit the
    // closures array as `[]` even though the program contains
    // closures elsewhere (in `main`). Pins the per-function filter.
    let out = karac_bin()
        .args([
            "query",
            "ownership",
            "tests/snapshots/closure_query.kara.take",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"function\":\"take\""));
    assert!(
        stdout.contains("\"closures\":[]"),
        "take has no closures; expected empty array; got: {stdout}"
    );
}

// ── Formatter ───────────────────────────────────────────────────

#[test]
fn test_fmt_idempotent() {
    let out1 = karac_bin()
        .args(["fmt", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(out1.status.success());
    let formatted = String::from_utf8_lossy(&out1.stdout);
    // Should contain the key constructs
    assert!(formatted.contains("fn main()"));
    assert!(formatted.contains("let x = 42"));
}

// ── Error Cases ─────────────────────────────────────────────────

#[test]
fn test_unknown_command() {
    let out = karac_bin().arg("foobar").output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown command"));
}

#[test]
fn test_missing_file() {
    let out = karac_bin()
        .args(["run", "nonexistent.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot read"));
}

#[test]
fn test_unknown_output_mode() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara", "--output=xml"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown output mode"));
}

// ── Project-mode build (CR-24 slices 2 + 3) ─────────────────────

fn scratch_project(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-build-project-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

fn write(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

#[test]
fn test_build_project_lists_discovered_modules() {
    let tmp = scratch_project("lists-modules");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(&tmp.join("src/greet.kara"), "pub fn greet() {}\n");
    write(&tmp.join("src/db/connection.kara"), "pub fn open() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "build failed: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("project: demo"));
    assert!(stdout.contains("entry:   bin"));
    assert!(stdout.contains("modules: 3"));
    assert!(stdout.contains("greet"));
    assert!(stdout.contains("db.connection"));
    assert!(stdout.contains("<crate root>"));
}

#[test]
fn test_build_project_resolves_imported_type_associated_fn() {
    // B-2026-07-08-20: project-mode `karac build` ran a PER-MODULE typecheck
    // that could not see an imported type's `impl`-block associated function —
    // `import lib.{Widget}` then `Widget.new()` failed with E0200 "no
    // associated function 'new' on type 'Widget'", even though `karac run`
    // (which typechecks the merged super-program) resolved it. The fix pulls
    // the imported type's impl blocks from its defining module into the
    // per-module env. Build must now type-check past the assoc-fn call.
    let tmp = scratch_project("imported-assoc-fn");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/widgets.kara"),
        "pub struct Widget { pub id: i64 }\n\
         impl Widget {\n\
             pub fn new(v: i64) -> Widget { Widget { id: v } }\n\
         }\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import widgets.{Widget};\n\
         fn main() {\n\
             let w = Widget.new(41);\n\
             println(w.id);\n\
         }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stderr = String::from_utf8_lossy(&out.stderr);
    // No llvm in the test env falls back to type-check; either way the
    // per-module typecheck must NOT reject the cross-module associated fn.
    assert!(
        !stderr.contains("E0200") && !stderr.contains("no associated function"),
        "project build wrongly rejected the imported assoc fn: {stderr}"
    );
    assert!(
        out.status.success(),
        "project build failed: stderr={stderr}",
    );
}

#[test]
fn test_single_file_build_refuses_package_member() {
    // B-2026-07-08-19: `karac build <pkg>/src/main.kara` (single-file) silently
    // dropped the package's sibling modules and emitted a truncated binary.
    // It must now refuse with actionable guidance pointing at project mode.
    let tmp = scratch_project("pkg-member-refusal");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/helper.kara"), "pub fn help() -> i64 { 7 }\n");
    write(
        &tmp.join("src/main.kara"),
        "import helper.{help};\nfn main() { println(help()); }\n",
    );
    // Invoke single-file build by naming the file explicitly.
    let out = karac_bin()
        .arg("build")
        .arg(tmp.join("src/main.kara"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "single-file build of a package member should fail, not silently succeed",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("source file of the package") && stderr.contains("karac build"),
        "expected a package-member refusal message, got: {stderr}"
    );
}

/// Project-mode platform-suffix selection must follow the `--target`, not the
/// host. A `--target=wasm_*` build selects `_wasm` modules and drops the
/// `_macos`/`_linux` siblings — so an example that swaps its host/IO layer per
/// target (e.g. `examples/iris`) builds the browser half, not the native half.
/// Regression for the `cmd_build_project` walker target (was always
/// `Platform::host()`). The "modules:" header is emitted right after the walk
/// and before codegen, so this asserts the *selection* without depending on a
/// wasm toolchain — the build itself may not complete in a minimal environment.
#[test]
fn project_build_wasm_target_selects_wasm_platform_module() {
    let tmp = scratch_project("wasm-platform-select");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import host.{run};\nfn main() { run(); }\n",
    );
    write(&tmp.join("src/host_wasm.kara"), "pub fn run() {}\n");
    write(&tmp.join("src/host_macos.kara"), "pub fn run() {}\n");
    write(&tmp.join("src/host_linux.kara"), "pub fn run() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--target=wasm_browser"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    // The wasm host is selected; the native siblings are filtered out.
    assert!(
        stdout.contains("host [wasm]"),
        "wasm build should select host_wasm; stdout=\n{stdout}",
    );
    assert!(
        !stdout.contains("host [macos]") && !stdout.contains("host [linux]"),
        "wasm build must not select native host modules; stdout=\n{stdout}",
    );
}

#[test]
fn test_build_project_mixed_entry_files_fails() {
    let tmp = scratch_project("mixed-entry");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(&tmp.join("src/lib.kara"), "pub fn add() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot contain both"));
}

#[test]
fn test_build_project_rejects_mod_kara() {
    let tmp = scratch_project("mod-kara");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(&tmp.join("src/db/mod.kara"), "\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mod.kara"));
}

#[test]
fn test_build_project_missing_src_dir_fails() {
    let tmp = scratch_project("no-src");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    // No src/ directory.

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no `src/` directory"));
}

#[test]
fn test_build_project_json_output() {
    let tmp = scratch_project("json");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(&tmp.join("src/greet.kara"), "pub fn greet() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--output=json"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"status\":\"ok\""));
    assert!(stdout.contains("\"entry\":\"bin\""));
    assert!(stdout.contains("\"modules\":["));
    assert!(stdout.contains("\"role\":\"entry\""));
    assert!(stdout.contains("\"role\":\"ordinary\""));
    assert!(stdout.contains("\"path\":\"greet\""));
}

// ── Slice 4 integration: parse errors + cycle detection hooks ───

#[test]
fn test_build_project_surfaces_per_file_parse_errors() {
    let tmp = scratch_project("parse-errors");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    // Intentional parse error: unterminated function signature.
    write(&tmp.join("src/broken.kara"), "fn oops(\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[parse]"),
        "expected parse error diagnostic, got stderr={stderr}",
    );
    assert!(
        stderr.contains("broken.kara"),
        "parse error should identify the offending file, got stderr={stderr}",
    );
}

#[test]
fn test_build_project_requires_entry_file() {
    let tmp = scratch_project("no-entry");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    // Only a nested module, no main.kara / lib.kara.
    write(&tmp.join("src/helper.kara"), "pub fn hi() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no entry file"),
        "expected 'no entry file' diagnostic, got stderr={stderr}",
    );
}

#[test]
fn test_build_project_empty_src_dir_fails() {
    let tmp = scratch_project("empty-src");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    std::fs::create_dir_all(tmp.join("src")).unwrap();

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no compilable"),
        "expected 'no compilable files' diagnostic, got stderr={stderr}",
    );
}

#[test]
fn test_build_project_emits_built_line_or_no_llvm_note() {
    // Theme 4 (2026-05-10): the project-mode build now drives codegen
    // through to a linked executable. Under `cfg(feature = "llvm")`,
    // stdout carries `Built: <path>`; without the llvm feature, stderr
    // carries the no-llvm fallback note. Either way, the build should
    // report a successful exit for a trivially-correct project.
    let tmp = scratch_project("built-line");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let built = stdout.contains("Built: ") || stderr.contains("requires the llvm feature");
    assert!(
        built,
        "expected `Built: ...` (llvm path) or no-llvm fallback note; stdout={stdout} stderr={stderr}",
    );
}

// ── Phase-7 line 5 slice 1: --enable-hot-swap plumbing + gating ─

#[test]
fn test_build_project_hot_swap_rejected_with_embedded_profile() {
    let tmp = scratch_project("hotswap-embedded");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\nprofile = \"embedded\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--enable-hot-swap"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--enable-hot-swap is incompatible with"),
        "expected gating diagnostic, got stderr={stderr}",
    );
    assert!(stderr.contains("embedded"));
}

#[test]
fn test_build_project_hot_swap_rejected_with_kernel_profile() {
    let tmp = scratch_project("hotswap-kernel");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\nprofile = \"kernel\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--enable-hot-swap"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--enable-hot-swap is incompatible with"),
        "expected gating diagnostic, got stderr={stderr}",
    );
    assert!(stderr.contains("kernel"));
}

#[test]
fn test_build_project_hot_swap_accepted_with_default_profile() {
    let tmp = scratch_project("hotswap-default");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--enable-hot-swap"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "expected success on default profile, got stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn test_build_bare_file_hot_swap_accepted() {
    // Bare-file build has no manifest → no profile → no gating.
    // The flag is accepted by the parser; under cfg(feature="llvm")
    // the build runs through codegen with indirection wired, under
    // the no-llvm fallback `cmd_build` routes through `cmd_check`.
    // Either way the parser should not have rejected the flag and
    // no gating error should fire.
    let out = karac_bin()
        .args(["build", "tests/snapshots/clean.kara", "--enable-hot-swap"])
        .output()
        .unwrap();
    // `karac build` derives the output executable name from the source
    // file_stem and writes it to CWD on the llvm-feature path; remove the
    // `clean` (or `clean.exe`) artifact so a `--features llvm` run doesn't
    // litter the working tree. Mirrors test_lint_cli_allow_via_build_subcommand.
    let _ = std::fs::remove_file(if cfg!(windows) { "clean.exe" } else { "clean" });
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "expected --enable-hot-swap to parse, got stderr={stderr}",
    );
    assert!(
        !stderr.contains("incompatible with"),
        "expected no profile gating for bare-file build, got stderr={stderr}",
    );
}

// ── Phase-9: `karac build --release` contract stripping ─────────
//
// `--release` strips debug-only runtime checks (contracts today) from the
// emitted binary, per design.md § Contracts ("checked at runtime in debug
// builds, stripped in release"). Wired in both single-file and project mode;
// OR-composes with the `KARAC_STRIP_CONTRACTS` env var.

#[test]
fn test_build_release_flag_accepted_single_file() {
    // The parser must recognise `--release` on a single-file build — no
    // "unknown flag" rejection. (Under no-llvm the build falls back to a
    // type check; either way the flag parses.)
    let out = karac_bin()
        .args(["build", "tests/snapshots/clean.kara", "--release"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(if cfg!(windows) { "clean.exe" } else { "clean" });
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unknown flag"),
        "expected --release to parse, got stderr={stderr}",
    );
}

#[test]
fn test_build_release_project_mode_accepted() {
    // `--release` with no file argument is project mode, which is now wired
    // for stripping. The parser must forward it (no "unknown flag", no the
    // old "only supported in single-file" rejection); the build then fails
    // only because there's no manifest in the test's CWD — which is fine, we
    // assert solely that the flag parsed and forwarded.
    let out = karac_bin().args(["build", "--release"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("only supported in single-file"),
        "project-mode --release must no longer be rejected, got stderr={stderr}",
    );
    assert!(
        !stderr.contains("unknown flag"),
        "expected --release to parse in project mode, got stderr={stderr}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_release_strips_contracts_e2e() {
    use std::io::Write;
    // `checked(5)` violates `requires x > 100`. A debug build aborts at
    // runtime with `contract violated`; `--release` strips the check so the
    // program runs to completion and prints 5. Building into a temp CWD keeps
    // the produced binary out of the worktree (the runtime archive resolves
    // via CARGO_MANIFEST_DIR, so the CWD change is safe).
    let src = r#"
fn checked(x: i64) -> i64 requires x > 100 { x }
fn main() { println(checked(5)); }
"#;
    let dir = std::env::temp_dir().join(format!("karac_release_e2e_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let kara_path = dir.join("relprog.kara");
    {
        let mut f = std::fs::File::create(&kara_path).expect("write temp .kara");
        f.write_all(src.as_bytes()).expect("write src");
    }
    let exe = dir.join("relprog");

    // Build (with/without --release) into the temp dir, then run the binary.
    // Returns None to soft-skip when the no-llvm fallback fires or linking
    // can't find the runtime archive (so the test passes vacuously in those
    // environments rather than failing on an unrelated cause).
    let build_and_run = |release: bool| -> Option<(String, Option<i32>)> {
        let mut args: Vec<&str> = vec!["build", "relprog.kara"];
        if release {
            args.push("--release");
        }
        let build = karac_bin().current_dir(&dir).args(args).output().unwrap();
        let berr = String::from_utf8_lossy(&build.stderr);
        if berr.contains("requires the llvm feature") || !exe.exists() {
            return None;
        }
        let run = common::output_with_hang_watchdog(
            std::process::Command::new(&exe),
            std::time::Duration::from_secs(15),
        )?;
        let out = String::from_utf8_lossy(&run.stdout).to_string();
        let code = run.status.code();
        let _ = std::fs::remove_file(&exe);
        Some((out, code))
    };

    if let Some((out, code)) = build_and_run(false) {
        assert!(
            out.contains("contract violated"),
            "debug build must abort on the requires violation, got stdout={out:?}",
        );
        assert_ne!(code, Some(0), "debug build must exit nonzero on the abort");
    }
    if let Some((out, code)) = build_and_run(true) {
        assert!(
            !out.contains("contract violated"),
            "--release must strip the contract, got stdout={out:?}",
        );
        assert_eq!(
            out.trim(),
            "5",
            "stripped build runs the body to completion"
        );
        assert_eq!(code, Some(0), "stripped build exits cleanly");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_release_project_mode_strips_contracts_e2e() {
    // Same debug-vs-release contrast as the single-file E2E, but through the
    // project-mode path (`cmd_build_project` → `run_multi_file_codegen` →
    // `compile_to_object_with_hot_swap`). A project with a `requires`-violating
    // call aborts on a debug build and runs to completion (prints 5) under
    // `--release`. Proves the flag is threaded all the way to codegen, not just
    // parsed.
    let src = "fn checked(x: i64) -> i64 requires x > 100 { x }\n\
               fn main() { println(checked(5)); }\n";

    // Build the project (with/without --release), then run the emitted binary.
    // Soft-skip (None) on the no-llvm fallback or a missing exe so the test
    // passes vacuously in those environments rather than failing on an
    // unrelated cause — same discipline as the single-file E2E above.
    let build_and_run = |release: bool| -> Option<(String, Option<i32>)> {
        let tmp = scratch_project(if release {
            "release-strip"
        } else {
            "release-debug"
        });
        write(&tmp.join("kara.toml"), "[package]\nname = \"relproj\"\n");
        write(&tmp.join("src/main.kara"), src);

        let mut args: Vec<&str> = vec!["build"];
        if release {
            args.push("--release");
        }
        let build = karac_bin().current_dir(&tmp).args(args).output().unwrap();
        let berr = String::from_utf8_lossy(&build.stderr);
        let exe = tmp.join("relproj");
        if berr.contains("requires the llvm feature") || !exe.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
        let run = common::output_with_hang_watchdog(
            std::process::Command::new(&exe),
            std::time::Duration::from_secs(15),
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let run = run?;
        Some((
            String::from_utf8_lossy(&run.stdout).to_string(),
            run.status.code(),
        ))
    };

    if let Some((out, code)) = build_and_run(false) {
        assert!(
            out.contains("contract violated"),
            "debug project build must abort on the requires violation, got stdout={out:?}",
        );
        assert_ne!(
            code,
            Some(0),
            "debug project build must exit nonzero on the abort"
        );
    }
    if let Some((out, code)) = build_and_run(true) {
        assert!(
            !out.contains("contract violated"),
            "--release must strip the contract in project mode, got stdout={out:?}",
        );
        assert_eq!(
            out.trim(),
            "5",
            "stripped project build runs the body to completion"
        );
        assert_eq!(code, Some(0), "stripped project build exits cleanly");
    }
}

// ── `kara.toml` `[link]` native-library directive ───────────────────
//
// The `[link]` table appends `-L<search-path>` / `-l<lib>` to the native
// `cc` line (`src/codegen/driver.rs` `link_executable_impl`). It is the
// self-hosting prerequisite that lets the Kāra-written codegen module link
// `libLLVM-18` to call the LLVM-C API
// (`docs/spikes/self-hosting-llvm-c-ffi.md` § Linking). Both tests are
// gated on `--features llvm` (no native link without codegen) and
// soft-skip when the no-llvm fallback fires.

/// Negative proof that the `-l<lib>` flag actually reaches the linker: a
/// `[link]` table naming a library that cannot exist makes the build fail
/// at link, and the linker's "library not found" error names that exact
/// library. That the linker even saw the name is the proof the directive
/// injected `-lkarac_link_directive_absent_xyz` onto the `cc` line. (A
/// missing runtime archive would fail the link for an unrelated reason
/// *without* naming our library — so the lib-name check both proves
/// injection and distinguishes it from an environment gap, which
/// soft-skips.)
#[cfg(feature = "llvm")]
#[test]
fn test_link_directive_injects_lib_flag_e2e() {
    const ABSENT_LIB: &str = "karac_link_directive_absent_xyz";
    let tmp = scratch_project("link-missing");
    write(
        &tmp.join("kara.toml"),
        &format!("[package]\nname = \"linkprog\"\n\n[link]\nlibs = [\"{ABSENT_LIB}\"]\n"),
    );
    write(&tmp.join("src/main.kara"), "fn main() { println(1); }\n");

    let build = karac_bin()
        .current_dir(&tmp)
        .args(["build"])
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let exe = tmp.join("linkprog");
    if berr.contains("requires the llvm feature") {
        eprintln!("skip: test_link_directive_injects_lib_flag_e2e — no llvm feature");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    // The named library cannot resolve, so the build must NOT produce a
    // binary regardless of environment.
    assert!(
        !exe.exists(),
        "a `[link]` lib that cannot exist must fail the build, but an executable was produced",
    );
    if berr.contains(ABSENT_LIB) {
        // Proof: the directive's `-l<lib>` reached the system linker.
        assert!(
            !build.status.success(),
            "build naming the missing library must exit nonzero",
        );
    } else {
        // The link step never reached our flag (e.g. the runtime archive is
        // absent in this environment) — nothing to prove here, skip.
        eprintln!(
            "skip: test_link_directive_injects_lib_flag_e2e — link did not reach the directive \
             (likely missing runtime archive); stderr:\n{berr}"
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Positive proof through a real library: a program that calls
/// `zlibVersion` (from the ubiquitous system zlib). That symbol resolves
/// *only* when `-lz` is on the link line. The contrast is the proof — the
/// same program builds and runs WITH `[link] libs = ["z"]` and fails to
/// link WITHOUT it (undefined `zlibVersion`). The contrast attributes the
/// resolution to the directive, not to the environment. Soft-skips when
/// the "with" build can't link zlib at all (no zlib-dev present), so a bare
/// CI box passes vacuously rather than failing on an unrelated cause.
#[cfg(feature = "llvm")]
#[test]
fn test_link_directive_resolves_real_library_e2e() {
    let src = "unsafe extern \"C\" { fn zlibVersion() -> *const u8; }\n\
               fn main() {\n\
               \x20   let v = unsafe { zlibVersion() };\n\
               \x20   if unsafe { ptr.addr(v) } != 0 { println(\"zlib-linked\") } else { println(\"null\") }\n\
               }\n";

    // Build (optionally with the `[link]` table) and run. Returns the run
    // output, or None to soft-skip (no-llvm fallback, or the build produced
    // no binary — including the legitimate "zlib not installed here" case).
    let build_and_run = |with_link: bool| -> Option<(String, Option<i32>)> {
        let tmp = scratch_project(if with_link {
            "zlib-with"
        } else {
            "zlib-without"
        });
        let manifest = if with_link {
            "[package]\nname = \"zprog\"\n\n[link]\nlibs = [\"z\"]\n"
        } else {
            "[package]\nname = \"zprog\"\n"
        };
        write(&tmp.join("kara.toml"), manifest);
        write(&tmp.join("src/main.kara"), src);
        let build = karac_bin()
            .current_dir(&tmp)
            .args(["build"])
            .env_remove("KARAC_RUNTIME")
            .output()
            .unwrap();
        let berr = String::from_utf8_lossy(&build.stderr).to_string();
        let exe = tmp.join("zprog");
        if berr.contains("requires the llvm feature") || !exe.exists() {
            let _ = std::fs::remove_dir_all(&tmp);
            return None;
        }
        let run = common::output_with_hang_watchdog(
            std::process::Command::new(&exe),
            std::time::Duration::from_secs(15),
        );
        let _ = std::fs::remove_dir_all(&tmp);
        let run = run?;
        Some((
            String::from_utf8_lossy(&run.stdout).to_string(),
            run.status.code(),
        ))
    };

    // The `[link]`-equipped build is the gate: only assert the contrast when
    // zlib actually links here. If it doesn't (no zlib-dev), the whole test
    // soft-skips — we never reach the "without" side.
    let Some((out, code)) = build_and_run(true) else {
        eprintln!("skip: test_link_directive_resolves_real_library_e2e — could not link zlib here");
        return;
    };
    assert_eq!(
        out.trim(),
        "zlib-linked",
        "the `[link]`-equipped build must resolve zlibVersion and run",
    );
    assert_eq!(code, Some(0), "the zlib-linked program exits cleanly");

    // Contrast: without the directive the very same program must fail to
    // link (undefined `zlibVersion`) — so `build_and_run(false)` produces no
    // binary and returns None. A `Some` here would mean zlib was linked
    // without the directive, which would falsify the attribution.
    assert!(
        build_and_run(false).is_none(),
        "without `[link]`, `zlibVersion` is undefined and the build must not produce a binary",
    );
}

// ── Theme 4: multi-file project-mode codegen ────────────────────
//
// Theme 4 wires `cmd_build_project` through the existing single-file
// codegen path by concatenating all module items (in topological order,
// dropping `import` declarations + the synthetic prelude) into a single
// super-program and driving it through `lower` → `effect` → `ownership`
// → `concurrency` → codegen → link. Symbol mangling is deferred to v2;
// cross-module function-name collisions surface as resolve-time errors
// against the merged super-program (clear diagnostic, no mangling
// ambiguity).
//
// All four tests below are gated `#[cfg(feature = "llvm")]` because the
// codegen output they verify only exists when llvm is built in.

#[test]
fn test_run_project_multi_module_loads_siblings() {
    // GAP-W3: `karac run <entry>` in a multi-module project must load sibling
    // modules into the interpreter — both a cross-module free function AND a
    // cross-module associated function. Before this, only the entry file's
    // items were registered, so these failed at runtime ("variable not found" /
    // "no interpreter evaluation rule") even though resolve + typecheck passed.
    // Interpreter path — no `llvm` feature needed.
    let tmp = scratch_project("run-multi-module");
    write(&tmp.join("kara.toml"), "[package]\nname = \"mm_run\"\n");
    write(
        &tmp.join("src/util.kara"),
        "pub struct Rates { rate: f64 }\n\
         impl Rates {\n\
             pub fn new(r: f64) -> Rates { Rates { rate: r } }\n\
             pub fn get(ref self) -> f64 { self.rate }\n\
         }\n\
         pub fn greet(n: String) -> String { f\"hi {n}\" }\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import util.{Rates, greet};\n\
         fn main() {\n\
             println(greet(\"world\"));\n\
             let r = Rates.new(2.5);\n\
             println(f\"r={r.get()}\");\n\
         }\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["run", "src/main.kara"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "run failed: stdout={stdout} stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("hi world"),
        "cross-module free fn not loaded: {stdout}",
    );
    assert!(
        stdout.contains("r=2.5"),
        "cross-module associated fn not loaded: {stdout}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_codegen_two_files_runs() {
    let tmp = scratch_project("codegen-two-files");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"two_file_demo\"\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import greet.add;\n\
         fn main() {\n    println(add(2, 3));\n}\n",
    );
    write(
        &tmp.join("src/greet.kara"),
        "pub fn add(x: i64, y: i64) -> i64 { x + y }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    assert!(
        out.status.success(),
        "build failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Built: "),
        "expected `Built: ...` line; stdout={stdout}",
    );

    let exe_path = tmp.join("two_file_demo");
    let run = common::output_with_hang_watchdog(
        std::process::Command::new(&exe_path),
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    let run = run.expect("executable should be runnable");
    assert!(
        run.status.success(),
        "executable failed: stderr={}",
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "5");
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_codegen_three_module_chain_runs() {
    // Pins topological emission order — `db.users` must declare its
    // symbols before `db` references them, which must declare before
    // `main`. The super-program builder concatenates in
    // dependency-first order via `module::emission_order`.
    let tmp = scratch_project("codegen-three-chain");
    write(&tmp.join("kara.toml"), "[package]\nname = \"chain_demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import db.fetch_count;\n\
         fn main() { println(fetch_count()); }\n",
    );
    write(
        &tmp.join("src/db.kara"),
        "import db.users.user_count;\n\
         pub fn fetch_count() -> i64 { user_count() + 1 }\n",
    );
    write(
        &tmp.join("src/db/users.kara"),
        "pub fn user_count() -> i64 { 42 }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    assert!(
        out.status.success(),
        "build failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let exe_path = tmp.join("chain_demo");
    let run = common::output_with_hang_watchdog(
        std::process::Command::new(&exe_path),
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    let run = run.expect("executable should be runnable");
    assert!(
        run.status.success(),
        "executable failed: stderr={}",
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "43");
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_codegen_cross_module_collision_diagnostic() {
    // Symbol mangling is deferred to v2 (per the Theme 4 plan revision);
    // for now, cross-module function-name collisions fall out as a
    // resolve-time error against the merged super-program. The
    // diagnostic is structured (mentions the colliding name) but
    // file-context is absent until the v2 follow-up adds per-module
    // span threading. Pins the no-silent-overwrite invariant.
    let tmp = scratch_project("codegen-collision");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"collide_demo\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/a.kara"),
        "pub fn shared_name() -> i64 { 1 }\n",
    );
    write(
        &tmp.join("src/b.kara"),
        "pub fn shared_name() -> i64 { 2 }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "build should fail on cross-module collision",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("shared_name") && stderr.contains("already defined"),
        "expected collision diagnostic naming `shared_name`; stderr={stderr}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_late_phase_diagnostic_includes_file_context() {
    // 2026-05-12 close-out for the wip-staging carry-forward
    // "Per-module diagnostics for late-phase failures in
    // `cmd_build_project`". The super-program approach concatenates
    // all module items, so post-typecheck phases lose the file-of-
    // origin context that the per-module typecheck path retains.
    // The `ModuleSpanTable` built at concat time in
    // `run_multi_file_codegen` (src/span_visitor.rs) restores that
    // context: each error's span is looked up against the table
    // and, when it resolves to exactly one module, the diagnostic
    // line is prefixed with `file:line:col`. This test triggers an
    // ownership use-after-move in a helper module and asserts the
    // build diagnostic surfaces `src/helper.kara` in the error
    // output. Use-after-move fires only in the ownership pass,
    // which runs over the super-program — typecheck-per-module
    // upstream lets the program through.
    let tmp = scratch_project("late-phase-file-context");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"late_phase_demo\"\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import helper.bad;\n\
         fn main() { let _ = bad(); }\n",
    );
    write(
        &tmp.join("src/helper.kara"),
        "struct Data { value: i64 }\n\
         fn consume(d: Data) -> i64 { d.value }\n\
         pub fn bad() -> i64 {\n\
             let d = Data { value: 7 };\n\
             let a = consume(d);\n\
             let b = consume(d);\n\
             a + b\n\
         }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "build should fail on use-after-move in helper.kara",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("src/helper.kara") || stderr.contains("helper.kara"),
        "expected helper.kara file context in stderr; stderr={stderr}",
    );
    // Sanity: also confirm the diagnostic phase name is included.
    assert!(
        stderr.contains("ownership") || stderr.contains("moved"),
        "expected an ownership-shaped diagnostic; stderr={stderr}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_codegen_providers_as_module_name() {
    // Theme 4 follow-up (2026-05-10): the lexer no longer reserves
    // `providers` as a global keyword. The bareword now lexes as a
    // regular identifier, so module names like `src/providers.kara`
    // and import paths `import providers.{...}` parse cleanly. The
    // parser dispatches to the `providers { R => e } in { body }`
    // block contextually — only when `providers` (as an Identifier
    // expression) is followed by `{`. This pins the demo path that
    // motivated the contextual-keyword change (`examples/parallax/`).
    let tmp = scratch_project("providers-module-name");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"providers_module\"\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import providers.canned_value;\n\
         fn main() { println(canned_value()); }\n",
    );
    write(
        &tmp.join("src/providers.kara"),
        "pub fn canned_value() -> i64 { 314 }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    assert!(
        out.status.success(),
        "build failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let exe_path = tmp.join("providers_module");
    let run = common::output_with_hang_watchdog(
        std::process::Command::new(&exe_path),
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    let run = run.expect("executable should be runnable");
    assert!(
        run.status.success(),
        "executable failed: stderr={}",
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "314");
}

#[cfg(feature = "llvm")]
#[test]
fn test_build_project_codegen_manifest_name_becomes_binary_name() {
    let tmp = scratch_project("codegen-binname");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"my_renamed_app\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() { println(7); }\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    assert!(
        out.status.success(),
        "build failed: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );

    let exe_path = tmp.join("my_renamed_app");
    let exe_exists = exe_path.exists();
    let run = common::output_with_hang_watchdog(
        std::process::Command::new(&exe_path),
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        exe_exists,
        "expected binary at <project>/my_renamed_app derived from manifest name",
    );
    let run = run.expect("executable should be runnable");
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "7");
}

#[test]
fn test_build_project_surfaces_unknown_module_e0224() {
    let tmp = scratch_project("unknown-module");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import greeet.hello;\nfn main() {}\n",
    );
    write(&tmp.join("src/greet.kara"), "pub fn hello() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "build should fail on unknown module");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[E0224]"),
        "expected E0224 in stderr, got {stderr}",
    );
    assert!(
        stderr.contains("greet"),
        "expected 'did you mean greet' suggestion, got {stderr}",
    );
}

#[test]
fn test_build_project_surfaces_unknown_item_e0225() {
    let tmp = scratch_project("unknown-item");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import greet.helllo;\nfn main() {}\n",
    );
    write(&tmp.join("src/greet.kara"), "pub fn hello() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "build should fail on unknown item");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[E0225]"),
        "expected E0225 in stderr, got {stderr}",
    );
}

#[test]
fn test_build_project_json_includes_resolve_diagnostics() {
    let tmp = scratch_project("json-resolve");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import nope.thing;\nfn main() {}\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .arg("build")
        .arg("--output=json")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"E0224\""),
        "JSON output should carry E0224, got {stdout}",
    );
    assert!(
        stdout.contains("\"phase\":\"resolve\""),
        "JSON diagnostic should tag phase=resolve, got {stdout}",
    );
}

#[test]
fn test_build_project_surfaces_e0222_across_directories() {
    let tmp = scratch_project("private-cross-dir");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import db.helper.secret;\nfn main() {}\n",
    );
    write(&tmp.join("src/db/helper.kara"), "private fn secret() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "build should fail on cross-dir private access",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[E0222]"),
        "expected E0222 in stderr, got {stderr}",
    );
}

// ── karac test (CR-24 follow-up slice 1) ────────────────────────

/// Filter stdout to test-runner JSONL events. Build-pipeline diagnostics
/// share the same `"type"` discriminator key, so the filter restricts to
/// the five test-runner event types defined at
/// `docs/design.md § Testing › Test runner output format`.
fn jsonl_lines(s: &str) -> Vec<&str> {
    s.lines()
        .filter(|l| {
            matches!(
                event_kind(l),
                Some(
                    "run_start"
                        | "test_pass"
                        | "test_fail"
                        | "test_skip"
                        | "test_timeout"
                        | "summary"
                )
            )
        })
        .collect()
}

fn event_kind(line: &str) -> Option<&str> {
    // Lines look like `{"type":"test_pass",...}` — slice between the first
    // pair of double-quotes after `:`.
    let after_event = line.strip_prefix("{\"type\":\"")?;
    let end = after_event.find('"')?;
    Some(&after_event[..end])
}

#[test]
fn test_subcommand_help_test() {
    let out = karac_bin().args(["test", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac test"));
    assert!(stdout.contains("JSONL"));
    assert!(stdout.contains("EXIT CODE"));
}

#[test]
fn test_test_all_passing() {
    let tmp = scratch_project("test-all-pass");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"add\" { assert_eq(add(1, 2), 3); }\ntest \"zero\" { assert_eq(add(0, 0), 0); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert_eq!(event_kind(lines[0]), Some("run_start"));
    assert!(lines[0].contains("\"total_tests\":2"));
    let pass_count = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .count();
    assert_eq!(pass_count, 2, "expected 2 test_pass events; got: {lines:?}");
    let summary = lines.last().unwrap();
    assert_eq!(event_kind(summary), Some("summary"));
    assert!(summary.contains("\"passed\":2"));
    assert!(summary.contains("\"failed\":0"));
}

#[test]
fn test_test_failure_emits_left_right_and_location() {
    let tmp = scratch_project("test-failure-detail");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    // `assert_eq(add(2, 2), 5)` — left=4, right=5.
    write(
        &tmp.join("src/main_test.kara"),
        "test \"failing\" { assert_eq(add(2, 2), 5); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected a test_fail event in:\n{lines:?}"));
    assert!(fail_line.contains("\"test\":\"failing\""));
    assert!(fail_line.contains("\"left\":\"4\""));
    assert!(fail_line.contains("\"right\":\"5\""));
    assert!(fail_line.contains("\"location\":{\"file\":"));
    assert!(fail_line.contains("main_test.kara"));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"failed\":1"));
}

#[test]
fn test_test_failure_emits_contract_fault_category() {
    // phase-9 step 7: a `test_fail` event carries a typed `category` field for
    // contract faults — `contract_violated` (false predicate) vs
    // `contract_predicate_panicked` (predicate evaluation faults) — so a
    // consumer filters on a stable field, not the human message. A plain
    // assertion failure carries NO `category` (conditional-presence, like
    // left/right). Runs on the interpreter path, so no llvm needed.
    let tmp = scratch_project("test-contract-category");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\n\
         fn checked(x: i64) -> i64 requires x > 100 { x }\n\
         fn deref_at(v: ref Vec[i64], i: i64) -> bool { v[i] >= 0 }\n\
         fn at(v: ref Vec[i64], i: i64) -> i64 requires deref_at(v, i) { 0 }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"violated\" { let _ = checked(5); }\n\
         test \"pred_panicked\" {\n\
         \x20   let mut v: Vec[i64] = Vec.new();\n\
         \x20   v.push(1);\n\
         \x20   let _ = at(v, 99);\n\
         }\n\
         test \"plain_assert\" { assert_eq(1, 2); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail_line = |name: &str| -> String {
        lines
            .iter()
            .find(|l| {
                event_kind(l) == Some("test_fail") && l.contains(&format!("\"test\":\"{name}\""))
            })
            .unwrap_or_else(|| panic!("expected a test_fail for `{name}` in:\n{lines:?}"))
            .to_string()
    };

    let violated = fail_line("violated");
    assert!(
        violated.contains("\"category\":\"contract_violated\""),
        "false predicate must tag `contract_violated`; got: {violated}"
    );
    assert!(
        !violated.contains("predicate_panicked"),
        "a violation must not tag panicked; got: {violated}"
    );

    let panicked = fail_line("pred_panicked");
    assert!(
        panicked.contains("\"category\":\"contract_predicate_panicked\""),
        "a cross-call predicate panic must tag `contract_predicate_panicked`; got: {panicked}"
    );

    let plain = fail_line("plain_assert");
    assert!(
        !plain.contains("\"category\":"),
        "a non-contract failure must carry no `category` field; got: {plain}"
    );
}

// ── Slice c.3 — `karac test` JIT subprocess dispatch ──
//
// With `KARAC_TEST_JIT=1` set (and `--features lljit_prototype` built),
// `cmd_test` shells each test out to `karac_jit_runner` instead of
// invoking the tree-walk interpreter. These tests pin that the
// surface event JSONL (test_pass / test_fail / summary, including
// the structured `left` / `right` / `location` fields on failures)
// stays byte-identical to the interpreter-path equivalents above —
// the JIT cutover is supposed to be invisible to consumers.

#[cfg(feature = "llvm")]
#[test]
fn test_test_jit_all_passing() {
    let tmp = scratch_project("test-jit-all-pass");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"add\" { assert_eq(add(1, 2), 3); }\ntest \"zero\" { assert_eq(add(0, 0), 0); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert_eq!(event_kind(lines[0]), Some("run_start"));
    assert!(lines[0].contains("\"total_tests\":2"));
    let pass_count = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .count();
    assert_eq!(pass_count, 2, "expected 2 test_pass events; got: {lines:?}");
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":2"));
    assert!(summary.contains("\"failed\":0"));
}

#[cfg(feature = "llvm")]
#[test]
fn test_test_jit_failure_emits_left_right_and_location() {
    let tmp = scratch_project("test-jit-failure-detail");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    // Same kara source as the interpreter-path test above; the JIT
    // outcome should emit the same fields (left=4, right=5, location
    // pointing at the assert_eq call site).
    write(
        &tmp.join("src/main_test.kara"),
        "test \"failing\" { assert_eq(add(2, 2), 5); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected a test_fail event in:\n{lines:?}"));
    assert!(fail_line.contains("\"test\":\"failing\""));
    assert!(fail_line.contains("\"left\":\"4\""));
    assert!(fail_line.contains("\"right\":\"5\""));
    assert!(fail_line.contains("\"location\":{\"file\":"));
    assert!(fail_line.contains("main_test.kara"));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"failed\":1"));
}

#[cfg(feature = "llvm")]
#[test]
fn test_test_jit_bare_assert_false_emits_null_left_right() {
    // `assert(false)` failures (no left/right operands) must still
    // surface a test_fail event with a message and no operand fields.
    // Mirrors the interpreter-path behavior — c.1's runtime fn emits
    // `"left":null,"right":null` and the parser maps that to
    // `TestOutcome { left: None, right: None }`, which the runner's
    // emitter then omits from the JSON output.
    let tmp = scratch_project("test-jit-bare-assert");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"bare\" { assert(false); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "expected non-zero exit");
    let lines = jsonl_lines(&stdout);
    let fail_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected a test_fail event in:\n{lines:?}"));
    assert!(fail_line.contains("\"message\":\"assertion failed\""));
    // No left / right fields on bare assert(cond) — the emitter
    // suppresses them when None.
    assert!(
        !fail_line.contains("\"left\":"),
        "bare assert should not emit left field; got: {fail_line}"
    );
    assert!(
        !fail_line.contains("\"right\":"),
        "bare assert should not emit right field; got: {fail_line}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_test_jit_timeout_kills_hanging_test() {
    // A `loop {}` body would normally hang the runner indefinitely.
    // Under JIT mode the per-test timeout (default 30 s; set to 1 s
    // here via `KARAC_TEST_TIMEOUT_SECS`) fires `kill -9` on the
    // subprocess and produces a `test_timeout` JSONL event instead
    // of a hang. Mirrors the interpreter-path's deadline-poll
    // semantics, but enforced externally rather than at every
    // statement boundary.
    let tmp = scratch_project("test-jit-timeout");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"hangs\" { loop {} }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .env("KARAC_TEST_TIMEOUT_SECS", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit on timeout; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let timeout_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_timeout"))
        .unwrap_or_else(|| panic!("expected a test_timeout event in:\n{lines:?}"));
    assert!(timeout_line.contains("\"test\":\"hangs\""));
    assert!(timeout_line.contains("\"timeout_s\":1"));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"failed\":1"));
}

#[test]
fn test_jit_batch_runner_continues_after_failing_tests() {
    // A failing/panicking test under JIT lowers to `exit(1)`, which kills
    // the *persistent* batch runner (`TestBatchRunner`). The runner must
    // re-spawn so every subsequent test still runs — a failure mid-suite
    // must not silently drop the tests after it. This pins the re-spawn
    // contract: interleaved fail / pass / panic / pass must all report.
    // (Under a non-`lljit_prototype` build `KARAC_TEST_JIT` is a no-op and
    // this runs the interpreter, which produces the same outcomes — so the
    // assertion is the user-visible contract either way.)
    let tmp = scratch_project("test-jit-batch-respawn");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"p1\" { assert_eq(add(2, 3), 5) }\n\
         test \"f1\" { assert_eq(add(2, 2), 5) }\n\
         test \"p2\" { assert_eq(add(0, 0), 0) }\n\
         test \"boom\" { unreachable() }\n\
         test \"p3\" { assert_eq(add(1, 1), 2) }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    // Every test reports an outcome — the two failures don't truncate the run.
    for (name, kind) in [
        ("p1", "test_pass"),
        ("f1", "test_fail"),
        ("p2", "test_pass"),
        ("boom", "test_fail"),
        ("p3", "test_pass"),
    ] {
        let line = lines
            .iter()
            .find(|l| l.contains(&format!("\"test\":\"{name}\"")))
            .unwrap_or_else(|| panic!("missing event for test {name:?} in:\n{lines:?}"));
        assert_eq!(
            event_kind(line),
            Some(kind),
            "test {name} wrong outcome: {line}"
        );
    }
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":3"), "summary: {summary}");
    assert!(summary.contains("\"failed\":2"), "summary: {summary}");
}

#[cfg(feature = "llvm")]
#[test]
fn test_jit_skeleton_path_links_real_bodies_not_stubs() {
    // Incremental-typecheck slice: a no-fixture test's per-test `main` module
    // is built from the module's *signature-only skeleton* (helper bodies
    // replaced by `unreachable()`), with the real bodies living declare-only
    // in the persistent shared module. This pins that the declare-only linkage
    // resolves to the REAL bodies: every assert below computes a value that
    // only the real body produces. If a stubbed body were ever emitted into
    // the per-test module instead of linked, the call would hit `unreachable()`
    // and abort (a fault outcome) rather than return the value — so a passing
    // `assert_eq` on a computed result is the regression guard. The module
    // mixes a free fn, a struct method, and an enum/match fn so the skeleton's
    // stub-and-link path is exercised across item kinds, plus an uncalled
    // generic (must emit nothing). The trailing failing test confirms failure
    // surfacing is unaffected by the skeleton path.
    let tmp = scratch_project("test-jit-skeleton-link");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "struct Point { x: i64, y: i64 }\n\
         impl Point {\n\
         \x20   fn new(x: i64, y: i64) -> Point { Point { x: x, y: y } }\n\
         \x20   fn sum(self) -> i64 { self.x + self.y }\n\
         }\n\
         enum Shape { Dot, Box(i64, i64) }\n\
         fn area(s: Shape) -> i64 { match s { Shape.Dot => 0, Shape.Box(w, h) => w * h } }\n\
         fn triple(n: i64) -> i64 { n * 3 }\n\
         fn pick[T](a: T, b: T) -> T { a }\n\
         test \"free fn body\" { assert_eq(triple(7), 21) }\n\
         test \"method body\" { let p = Point.new(3, 4); assert_eq(p.sum(), 7) }\n\
         test \"enum match body\" { assert_eq(area(Shape.Box(3, 4)), 12) }\n\
         test \"this one fails\" { assert_eq(triple(1), 99) }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_JIT", "1")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    for (name, kind) in [
        ("free fn body", "test_pass"),
        ("method body", "test_pass"),
        ("enum match body", "test_pass"),
        ("this one fails", "test_fail"),
    ] {
        let line = lines
            .iter()
            .find(|l| l.contains(&format!("\"test\":\"{name}\"")))
            .unwrap_or_else(|| panic!("missing event for test {name:?} in:\n{lines:?}"));
        assert_eq!(
            event_kind(line),
            Some(kind),
            "test {name} wrong outcome (skeleton stub leaked instead of real body?): {line}"
        );
    }
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":3"), "summary: {summary}");
    assert!(summary.contains("\"failed\":1"), "summary: {summary}");
}

#[test]
fn test_test_filter_narrows_to_substring_match() {
    let tmp = scratch_project("test-filter");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"alpha\" { assert(true); }\ntest \"beta\" { assert(true); }\ntest \"gamma\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["test", "beta"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":1"));
    let pass_lines: Vec<&&str> = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .collect();
    assert_eq!(pass_lines.len(), 1);
    assert!(pass_lines[0].contains("\"test\":\"beta\""));
}

#[test]
fn test_test_filter_no_matches_runs_zero_tests() {
    let tmp = scratch_project("test-filter-zero");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"alpha\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["test", "no_such_test_anywhere"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":0"));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"total\":0"));
    assert!(summary.contains("\"passed\":0"));
    assert!(summary.contains("\"failed\":0"));
}

#[test]
fn test_test_no_test_files_runs_zero_tests() {
    let tmp = scratch_project("test-no-tests");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn helper() -> i64 { 42 }\n",
    );
    // No _test.kara file at all.

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":0"));
}

#[test]
fn test_test_compile_error_exits_nonzero_without_summary() {
    let tmp = scratch_project("test-compile-err");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    // Use of an undefined name in the test file — surfaces as a resolve
    // error and the runner aborts before emitting any test events.
    write(
        &tmp.join("src/main_test.kara"),
        "test \"a\" { undefined_function(); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "stdout:\n{stdout}");
    // Compile-failure path emits diagnostic events from the existing
    // build-pipeline JSONL helpers (with `"type":` keying), but no
    // `run_start` / `summary` test events should fire.
    let test_events = jsonl_lines(&stdout);
    assert!(
        test_events.is_empty(),
        "expected no test events; got: {test_events:?}"
    );
    assert!(stdout.contains("undefined_function"));
}

#[test]
fn test_test_test_only_module_with_no_production_sibling() {
    // A `_test.kara` file with no production sibling — design.md § Three-
    // level visibility says it shares the directory of the module it would
    // test. The walker still classifies it as Test role; the merge produces
    // a Module containing only test items.
    let tmp = scratch_project("test-only-module");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    // No src/standalone.kara — only the test file.
    write(
        &tmp.join("src/standalone_test.kara"),
        "test \"in standalone module\" { assert(true); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":1"));
    let pass = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_pass"))
        .unwrap();
    // The `test` field on the event is the case-name string verbatim
    // — no module qualifier prefix. `--filter` matches against this
    // same string.
    assert!(pass.contains("\"test\":\"in standalone module\""));
}

#[test]
fn test_test_helper_in_test_file_not_run() {
    // Helper functions in a `_test.kara` file — whatever they are
    // named, including `fn test_helper`-style names — are never
    // discovered as tests. Discovery is structural (`Item::TestCase`),
    // not name-based, so a free function cannot accidentally become
    // a test by naming convention.
    let tmp = scratch_project("test-helper-skip");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "fn make_pair() -> (i64, i64) { (1, 2) }\n\
         fn test_helper_named_like_a_test() -> i64 { 42 }\n\
         test \"uses helper\" { let p = make_pair(); assert_eq(p.0, 1); }\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":1"));
    let pass = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_pass"))
        .unwrap();
    assert!(pass.contains("\"test\":\"uses helper\""));
    // Neither the unrelated helper nor the conventionally-named
    // helper appears in any test event — discovery does not see
    // them as cases regardless of their identifiers.
    assert!(!stdout.contains("make_pair"));
    assert!(!stdout.contains("test_helper_named_like_a_test"));
}

#[test]
fn test_test_test_prefixed_function_in_production_code_is_not_a_test() {
    // A `test_` prefix on a production-code function is a name like
    // any other; it has never been a discovery signal (discovery is
    // structural and only walks `_test.kara` companion files). The
    // test keeps existing as a regression guard against any future
    // re-introduction of name-based discovery.
    let tmp = scratch_project("test-prefix-prod");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn test_helper_named_like_a_test() -> i64 { 42 }\n",
    );
    // No `_test.kara` companion — so `test_helper_named_like_a_test`
    // lives in production code only and has no business being a test.

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":0"));
}

#[test]
fn test_test_unknown_flag_rejected() {
    let out = karac_bin()
        .args(["test", "--no-such-flag"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"));
}

#[test]
fn test_test_two_positional_args_rejected() {
    let out = karac_bin()
        .args(["test", "first_filter", "second_filter"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("at most one"));
}

// ── karac run --timeout DURATION (line 861) ──
// Opt-in wall-clock cap on the interpreter. No default — long-running
// services / daemons / REPLs are legitimate `karac run` workloads, so
// a default would silently break real operations. Useful for CI smoke
// tests and exploratory runs where forgetting about a runaway costs
// real laptop battery. On timeout, exits with code 124 matching GNU
// timeout(1).

#[test]
fn test_run_timeout_kills_infinite_loop() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-run-timeout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("loop.kara");
    std::fs::write(
        &path,
        "fn main() { let mut i: i64 = 0; while true { i = i + 1; } }",
    )
    .unwrap();

    let started = std::time::Instant::now();
    let out = karac_bin()
        .args(["run", "--timeout=1s", path.to_str().unwrap()])
        .output()
        .unwrap();
    let wall = started.elapsed();
    let _ = std::fs::remove_dir_all(&tmp);

    // Exit code 124 matches GNU `timeout(1)`.
    assert_eq!(
        out.status.code(),
        Some(124),
        "expected exit 124 (GNU timeout convention); stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Should fire within ~1 s + a small slack. Generous 12 s ceiling
    // so a slow CI box can't flake on a working timeout.
    assert!(
        wall < std::time::Duration::from_secs(12),
        "expected --timeout=1s to kill the runaway in ~1s; took {:?}",
        wall
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("karac: timed out after 1s"),
        "expected GNU-timeout-style stderr message; got: {stderr}"
    );
}

#[test]
fn test_run_no_timeout_default_runs_to_completion() {
    // Without --timeout, a fast program runs to completion with no
    // deadline overhead. Regression guard against accidentally adding
    // a default timeout (which would silently break long-running
    // services).
    let tmp = std::env::temp_dir().join(format!(
        "karac-run-no-timeout-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("hello.kara");
    std::fs::write(&path, "fn main() { println(42); }").unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    assert!(stdout.contains("42"));
}

// `karac run` surfaces interpreter runtime faults: the fault MESSAGE is printed
// (previously dropped — only the `?`-return trace location showed) and the
// process exits nonzero (previously always 0, so scripts couldn't detect
// interpreter-level failures). Covers contract violations and a non-contract
// fault (index out of bounds) to show generality.

fn write_run_temp(tag: &str, src: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-run-rterr-{}-{}-{}",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("prog.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn test_run_contract_violation_prints_message_and_exits_nonzero() {
    let path = write_run_temp(
        "contract",
        "fn checked(x: i64) -> i64 requires x > 100 { x }\nfn main() { println(checked(5)); }\n",
    );
    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a contract violation must exit nonzero; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("runtime error:") && stderr.contains("contract violated"),
        "expected the fault message on stderr; got:\n{stderr}"
    );
}

#[test]
fn test_run_index_oob_prints_message_and_exits_nonzero() {
    let path = write_run_temp(
        "oob",
        "fn main() { let v: Vec[i64] = Vec.new(); println(v[5]); }\n",
    );
    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "OOB must exit nonzero; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("runtime error:"),
        "expected a `runtime error:` line; got:\n{stderr}"
    );
}

#[test]
fn test_run_runtime_error_json_envelope() {
    let path = write_run_temp(
        "json",
        "fn checked(x: i64) -> i64 requires x > 100 { x }\nfn main() { println(checked(5)); }\n",
    );
    let out = karac_bin()
        .args(["run", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "must exit nonzero; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("\"runtime_errors\"") && stdout.contains("contract violated"),
        "expected a runtime_errors JSON object with the message; got:\n{stdout}"
    );
}

#[test]
fn test_run_timeout_rejects_zero_value() {
    // `--timeout=0` is meaningless (would fire immediately) — reject
    // at parse time with a clear diagnostic rather than running a
    // pre-aborted process.
    let out = karac_bin()
        .args(["run", "--timeout=0", "dummy.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--timeout") && stderr.contains("greater than zero"),
        "expected zero-value rejection; got stderr: {stderr}"
    );
}

#[test]
fn test_run_timeout_rejects_unparseable_value() {
    let out = karac_bin()
        .args(["run", "--timeout=fast", "dummy.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--timeout") && stderr.contains("not a valid duration"),
        "expected unparseable-value rejection; got stderr: {stderr}"
    );
}

#[test]
fn test_run_timeout_accepts_bare_integer_as_seconds() {
    // GNU `timeout 60 cmd` compat: bare integer is seconds. Parse
    // success path (the timer doesn't fire because the program is
    // fast).
    let tmp = std::env::temp_dir().join(format!(
        "karac-run-timeout-bareint-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("quick.kara");
    std::fs::write(&path, "fn main() { println(7); }").unwrap();

    let out = karac_bin()
        .args(["run", "--timeout=60", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("7"));
}

// ── Per-test timeout (line 847 sub-step 1) ──
// A runaway loop in a test must be killed within `KARAC_TEST_TIMEOUT_SECS`
// (default 30 s, overridable via env var for fast CI / fixture
// runs). The runner emits a `test_timeout` JSONL event carrying the
// test name, the configured timeout (in seconds), and the elapsed
// wall-clock; the suite continues to the next test, summary reports
// the timeout as a failure. Interpreter polls a per-test deadline
// at every statement boundary (`eval_block_inner`), so a `while true {}`
// surfaces within milliseconds of the deadline.

#[test]
fn test_test_per_test_timeout_kills_runaway_loop() {
    let tmp = scratch_project("test-timeout-runaway");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"hangs\" {\n  \
             let mut i: i64 = 0;\n  \
             while true { i = i + 1; }\n  \
             assert(i > 0);\n\
         }\n",
    );

    let started = std::time::Instant::now();
    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_TIMEOUT_SECS", "1")
        .arg("test")
        .output()
        .unwrap();
    let wall = started.elapsed();
    let _ = std::fs::remove_dir_all(&tmp);

    // Wall-clock should be ~1 s (the timeout) plus a small slack for
    // the per-statement poll cadence — well under our 15s harness
    // timeout. If the watchdog were broken, the binary would either
    // hang past the test harness's own timeout or run the full 30 s
    // default. Generous 12 s ceiling here so a slow CI box doesn't
    // flake on a perfectly-working timeout.
    assert!(
        wall < std::time::Duration::from_secs(12),
        "expected per-test timeout to kill runaway in ~1s; took {:?}",
        wall
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit when a test times out; stdout:\n{stdout}"
    );

    let lines = jsonl_lines(&stdout);
    let timeout_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_timeout"))
        .unwrap_or_else(|| panic!("expected a test_timeout event in:\n{lines:?}"));
    assert!(
        timeout_line.contains("\"test\":\"hangs\""),
        "test name missing from timeout event: {timeout_line}"
    );
    assert!(
        timeout_line.contains("\"timeout_s\":1"),
        "configured timeout missing from event: {timeout_line}"
    );

    let summary = lines.last().unwrap();
    assert_eq!(event_kind(summary), Some("summary"));
    assert!(
        summary.contains("\"failed\":1"),
        "summary should count the timeout as a failure: {summary}"
    );
}

#[test]
fn test_test_normal_completion_under_timeout_passes() {
    // Cheap baseline: a normal-completing test under a generous
    // 5 s timeout reports `test_pass`, not `test_timeout`.
    let tmp = scratch_project("test-timeout-no-trigger");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"quick\" { assert(1 + 1 == 2); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .env("KARAC_TEST_TIMEOUT_SECS", "5")
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(
        lines.iter().any(|l| event_kind(l) == Some("test_pass")),
        "expected test_pass event; got: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| event_kind(l) == Some("test_timeout")),
        "fast test should not emit test_timeout; got: {lines:?}"
    );
}

#[test]
fn test_test_kara_toml_timeout_applies() {
    // Sub-step 2: `[test] timeout_seconds = 1` in kara.toml caps each test
    // at 1 s with NO env var set — proves the manifest value is read and
    // beats the 30 s default.
    let tmp = scratch_project("test-timeout-kara-toml");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[test]\ntimeout_seconds = 1\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"hangs\" {\n  \
             let mut i: i64 = 0;\n  \
             while true { i = i + 1; }\n  \
             assert(i > 0);\n\
         }\n",
    );

    let started = std::time::Instant::now();
    // Deliberately NO KARAC_TEST_TIMEOUT_SECS — the kara.toml value is the
    // only timeout source.
    let out = karac_bin()
        .current_dir(&tmp)
        .env_remove("KARAC_TEST_TIMEOUT_SECS")
        .arg("test")
        .output()
        .unwrap();
    let wall = started.elapsed();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        wall < std::time::Duration::from_secs(12),
        "expected kara.toml timeout to kill runaway in ~1s; took {:?}",
        wall
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit when a test times out; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let timeout_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_timeout"))
        .unwrap_or_else(|| panic!("expected a test_timeout event in:\n{lines:?}"));
    assert!(
        timeout_line.contains("\"timeout_s\":1"),
        "kara.toml timeout should drive the event's timeout_s: {timeout_line}"
    );
}

#[test]
fn test_test_per_test_attribute_timeout_overrides_manifest() {
    // Sub-step 3: a per-test `#[test(timeout_seconds = 1)]` attribute wins
    // over a generous kara.toml `[test] timeout_seconds = 30` — proves the
    // attribute is the highest-precedence layer.
    let tmp = scratch_project("test-timeout-attr");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[test]\ntimeout_seconds = 30\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(timeout_seconds = 1)]\n\
         test \"hangs\" {\n  \
             let mut i: i64 = 0;\n  \
             while true { i = i + 1; }\n  \
             assert(i > 0);\n\
         }\n",
    );

    let started = std::time::Instant::now();
    let out = karac_bin()
        .current_dir(&tmp)
        .env_remove("KARAC_TEST_TIMEOUT_SECS")
        .arg("test")
        .output()
        .unwrap();
    let wall = started.elapsed();
    let _ = std::fs::remove_dir_all(&tmp);

    // If the attribute didn't win, the kara.toml 30 s would let the runaway
    // run past our 12 s ceiling.
    assert!(
        wall < std::time::Duration::from_secs(12),
        "expected per-test attribute (1s) to beat kara.toml (30s); took {:?}",
        wall
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    let timeout_line = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_timeout"))
        .unwrap_or_else(|| panic!("expected a test_timeout event in:\n{lines:?}"));
    assert!(
        timeout_line.contains("\"timeout_s\":1"),
        "per-test attribute timeout should drive the event's timeout_s: {timeout_line}"
    );
}

#[test]
fn test_test_test_outside_project_emits_manifest_error() {
    let tmp = scratch_project("test-no-manifest");
    // No kara.toml at all — manifest discovery should fail.
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("manifest_error"));
}

// ── karac test (test-block syntax — phase-4 slice 2/3) ──────────

#[test]
fn test_test_block_form_all_passing() {
    let tmp = scratch_project("test-block-all-pass");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"add positives\" {\n    assert_eq(add(1, 2), 3);\n}\n\
         test \"add zeros\" {\n    assert_eq(add(0, 0), 0);\n}\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":2"));
    let pass: Vec<&&str> = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .collect();
    assert_eq!(pass.len(), 2, "got: {lines:?}");
    // The `test` field on each event is the case-name string
    // verbatim — no `<root>::__test_...` mangling leaks through.
    assert!(pass
        .iter()
        .any(|l| l.contains("\"test\":\"add positives\"")));
    assert!(pass.iter().any(|l| l.contains("\"test\":\"add zeros\"")));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":2"));
    assert!(summary.contains("\"failed\":0"));
}

#[test]
fn test_test_block_form_failure_surfaces_case_name() {
    let tmp = scratch_project("test-block-failure");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\nfn add(a: i64, b: i64) -> i64 { a + b }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "test \"sum should be five\" {\n    assert_eq(add(2, 2), 5);\n}\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected exit != 0; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail in:\n{lines:?}"));
    assert!(fail.contains("\"test\":\"sum should be five\""));
    assert!(fail.contains("\"left\":\"4\""));
    assert!(fail.contains("\"right\":\"5\""));
}

#[test]
fn test_test_block_form_filter_matches_case_name() {
    let tmp = scratch_project("test-block-filter");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "test \"alpha case\" { assert(true); }\n\
         test \"beta case\" { assert(true); }\n\
         test \"gamma case\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["test", "beta"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":1"));
    let pass: Vec<&&str> = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .collect();
    assert_eq!(pass.len(), 1);
    assert!(pass[0].contains("\"test\":\"beta case\""));
}

#[test]
fn test_test_helpers_alongside_test_cases_in_same_file() {
    // Block-form test cases coexist with free helper functions in
    // the same `_test.kara` file. Discovery is structural — only
    // `Item::TestCase` entries are picked up, so any number of
    // `fn` items can live alongside cases as helpers without
    // accidentally becoming tests themselves.
    let tmp = scratch_project("test-block-helpers");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "fn helper_one() -> i64 { 1 }\n\
         fn helper_two() -> i64 { 2 }\n\
         test \"helpers compose\" {\n    assert_eq(helper_one() + helper_two(), 3);\n}\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":1"));
    let pass: Vec<&&str> = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .collect();
    assert_eq!(pass.len(), 1);
    assert!(pass[0].contains("\"test\":\"helpers compose\""));
}

#[test]
fn test_test_block_form_requires_skips_when_env_var_unset() {
    // Slice 4 lifts `#[test(requires=[...])]` extraction onto
    // `TestCase.attributes`. The runner-side requires-gating path
    // is unchanged — what's new is that block-form cases can now
    // declare requires the same way as the legacy `fn test_*`
    // form, and the resource-probe / skip behavior is identical.
    let tmp = scratch_project("test-block-requires-skip");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_blockreq_skipcase.fake_db])]\n\
         test \"needs db\" { assert(false); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env_remove("KARA_RESOURCE_KARAC_BLOCKREQ_SKIPCASE_FAKE_DB")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let skip = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_skip"))
        .unwrap_or_else(|| panic!("expected test_skip in:\n{lines:?}"));
    assert!(skip.contains("\"reason\":\"unsatisfied_requires\""));
    // The `test` field is the case-name string, not the mangled id.
    assert!(skip.contains("\"test\":\"needs db\""));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"skipped\":1"));
}

#[test]
fn test_test_block_form_with_provider_fixture_is_recognized() {
    // Block-form cases can stack `#[with_provider(R, ctor)]`
    // attributes the same way `fn test_*` does. The fixture
    // pushes a provider frame before the body runs, so a
    // resource-method call inside the body resolves against the
    // fake instead of the ambient default.
    let tmp = scratch_project("test-block-with-provider");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "fn main() {}\n\
         shared struct FakeClock { value: i64 }\n\
         impl FakeClock {\n    fn now(self) -> i64 { self.value }\n}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "#[with_provider(Clock, FakeClock { value: 42 })]\n\
         test \"clock injected\" {\n    assert_eq(Clock.now(), 42);\n}\n",
    );

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let pass = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_pass"))
        .unwrap_or_else(|| panic!("expected test_pass in:\n{lines:?}"));
    assert!(pass.contains("\"test\":\"clock injected\""));
}

// ── karac test (CR-24 follow-up slice 2: requires-gating) ───────

#[test]
fn test_test_requires_skips_when_resource_unavailable() {
    let tmp = scratch_project("test-requires-skip");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_skipcase.fake_db])]\ntest \"needs db\" { assert(false); }\n",
    );

    // Env var KARA_RESOURCE_KARAC_SLICE2_SKIPCASE_FAKE_DB is unset; the
    // assertion in the body is unreachable because the test must be
    // skipped before execution. Exit 0 since skips don't fail by default.
    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env_remove("KARA_RESOURCE_KARAC_SLICE2_SKIPCASE_FAKE_DB")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let skip = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_skip"))
        .unwrap_or_else(|| panic!("expected a test_skip line; got: {lines:?}"));
    assert!(skip.contains("\"reason\":\"unsatisfied_requires\""));
    assert!(skip.contains("\"resources\":[\"karac_slice2_skipcase.fake_db\"]"));
    assert!(skip.contains("\"test\":\"needs db\""));
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"skipped\":1"));
    assert!(summary.contains("\"failed\":0"));
    assert!(summary.contains("\"passed\":0"));
}

#[test]
fn test_test_requires_runs_when_env_var_set() {
    let tmp = scratch_project("test-requires-runs");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_runcase.fake_db])]\ntest \"needs db\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env("KARA_RESOURCE_KARAC_SLICE2_RUNCASE_FAKE_DB", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(
        lines.iter().any(|l| event_kind(l) == Some("test_pass")),
        "expected a test_pass line; got: {lines:?}",
    );
    assert!(
        lines.iter().all(|l| event_kind(l) != Some("test_skip")),
        "expected no test_skip line; got: {lines:?}",
    );
}

#[test]
fn test_test_requires_health_check_overrides_env_var() {
    // Even when the env var IS set, a `[test.resources]` shell command
    // takes precedence — and a failing command means the resource is
    // considered unavailable. Verifies the order-of-precedence rule
    // documented in `docs/design.md § Testing`.
    let tmp = scratch_project("test-requires-healthcheck");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[test.resources]\n\"karac_slice2_healthcase.fake_db\" = \"false\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_healthcase.fake_db])]\ntest \"needs db\" { assert(true); }\n",
    );

    // env var IS set — but the failing health-check should take precedence.
    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env("KARA_RESOURCE_KARAC_SLICE2_HEALTHCASE_FAKE_DB", "1")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(
        lines.iter().any(|l| event_kind(l) == Some("test_skip")),
        "health check exit-1 should win over env var; got: {lines:?}",
    );
}

#[test]
fn test_test_requires_health_check_success_runs_test() {
    let tmp = scratch_project("test-requires-healthok");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[test.resources]\n\"karac_slice2_healthok.fake_db\" = \"true\"\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_healthok.fake_db])]\ntest \"needs db\" { assert(true); }\n",
    );

    // No env var set — only the (passing) health check matters.
    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env_remove("KARA_RESOURCE_KARAC_SLICE2_HEALTHOK_FAKE_DB")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected exit 0; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(
        lines.iter().any(|l| event_kind(l) == Some("test_pass")),
        "passing health check should let the test run; got: {lines:?}",
    );
}

#[test]
fn test_test_requires_partial_satisfied_lists_only_missing() {
    let tmp = scratch_project("test-requires-partial");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    // Two requires; A is set in env, B is not. Skip event must list only B.
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_partial.have_a, karac_slice2_partial.miss_b])]\ntest \"needs both\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .arg("test")
        .env("KARA_RESOURCE_KARAC_SLICE2_PARTIAL_HAVE_A", "1")
        .env_remove("KARA_RESOURCE_KARAC_SLICE2_PARTIAL_MISS_B")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    let skip = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_skip"))
        .unwrap_or_else(|| panic!("expected test_skip; got: {lines:?}"));
    assert!(
        skip.contains("\"resources\":[\"karac_slice2_partial.miss_b\"]"),
        "missing-only list should contain only miss_b; got: {skip}",
    );
    assert!(
        !skip.contains("have_a"),
        "satisfied resource should not appear in missing list; got: {skip}",
    );
}

#[test]
fn test_test_all_promotes_skip_to_failure() {
    let tmp = scratch_project("test-all-flag");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/main_test.kara"),
        "#[test(requires = [karac_slice2_allcase.fake_db])]\ntest \"needs db\" { assert(true); }\n",
    );

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["test", "--all"])
        .env_remove("KARA_RESOURCE_KARAC_SLICE2_ALLCASE_FAKE_DB")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit under --all when a skip occurs; stdout:\n{stdout}",
    );
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail under --all; got: {lines:?}"));
    assert!(fail.contains("\"reason\":\"unsatisfied_requires\""));
    assert!(fail.contains("\"resources\":[\"karac_slice2_allcase.fake_db\"]"));
    assert!(
        lines.iter().all(|l| event_kind(l) != Some("test_skip")),
        "no test_skip events should be emitted under --all; got: {lines:?}",
    );
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"failed\":1"));
    assert!(summary.contains("\"skipped\":0"));
}

#[test]
fn test_test_requires_subcommand_help_documents_all_flag() {
    let out = karac_bin().args(["test", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--all"),
        "test --help should document --all; got:\n{stdout}",
    );
    assert!(
        stdout.contains("KARA_RESOURCE_"),
        "test --help should document the env-var probe; got:\n{stdout}",
    );
}

// ── tracker line 884: dev-deps in test mode, not build mode ─────

#[test]
fn test_build_skips_dev_dependencies_from_resolution() {
    // Build mode must NOT resolve [dev-dependencies]. A dev-dep pointing
    // at a non-existent directory would surface E_PATH_DEP_NOT_FOUND if
    // it participated; the build succeeds, proving the exclusion.
    let tmp = scratch_project("build-skips-dev-deps");
    write(
        &tmp.join("kara.toml"),
        r#"[package]
name = "demo"

[dev-dependencies]
missing-test-helper = { path = "libs/missing-test-helper" }
"#,
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "build mode must ignore [dev-dependencies]; stderr={stderr}",
    );
    assert!(
        !stderr.contains("E_PATH_DEP_NOT_FOUND"),
        "dev-dep path miss must not surface in build mode; got: {stderr}",
    );
}

#[test]
fn test_test_resolves_dev_dependencies() {
    // Test mode must resolve [dev-dependencies]. Declaring a dev-dep with
    // a bad path triggers E_PATH_DEP_NOT_FOUND during test resolution,
    // proving the dev-deps walk is gated on the test mode flag.
    let tmp = scratch_project("test-resolves-dev-deps");
    write(
        &tmp.join("kara.toml"),
        r#"[package]
name = "demo"

[dev-dependencies]
missing-test-helper = { path = "libs/missing-test-helper" }
"#,
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "test mode must fail when a dev-dep path is missing; stdout={stdout}",
    );
    assert!(
        stdout.contains("E_PATH_DEP_NOT_FOUND") || stdout.contains("dep_resolution_error"),
        "expected dev-dep resolution to surface a focused error; got: {stdout}",
    );
}

#[test]
fn test_test_writes_dev_dep_into_lockfile() {
    // Test mode resolution should record the dev-dep into kara.lock so
    // the test pipeline (when it consumes deps) has a deterministic
    // entry. Build mode resolution does not run for solo projects with
    // only dev-deps, so the lockfile is the durable artifact pin.
    let tmp = scratch_project("test-dev-dep-lockfile");
    std::fs::create_dir_all(tmp.join("libs/test-helper/src")).unwrap();
    write(
        &tmp.join("kara.toml"),
        r#"[package]
name = "demo"

[dev-dependencies]
test-helper = { path = "libs/test-helper" }
"#,
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("libs/test-helper/kara.toml"),
        "[package]\nname = \"test-helper\"\n",
    );
    write(
        &tmp.join("libs/test-helper/src/lib.kara"),
        "fn helper() {}\n",
    );

    // Build first to confirm the lockfile is NOT created (no regular deps).
    let out_build = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let lock_after_build = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    assert!(
        out_build.status.success(),
        "build with only dev-deps must succeed",
    );
    assert!(
        !lock_after_build.contains("test-helper"),
        "build mode lockfile must not record dev-deps; got: {lock_after_build}",
    );

    // Test mode should rewrite the lockfile with the dev-dep included.
    let _ = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let lock_after_test = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        lock_after_test.contains("test-helper"),
        "test mode lockfile must include dev-deps; got: {lock_after_test}",
    );
}

// ── karac test (CR-C: `#[with_provider]` fixture) ────────────────

/// Emit a small project with a Clock effect resource, a FakeClock
/// struct, and a single test. The caller supplies the test function
/// body and any attributes so each test scenario can tweak behavior.
fn with_provider_project(tag: &str, test_attrs_and_body: &str) -> std::path::PathBuf {
    let tmp = scratch_project(tag);
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "effect resource Clock;\n\
         struct FakeClock { t: i64 }\n\
         impl FakeClock { fn now(self) -> i64 { self.t } }\n\
         fn main() {}\n",
    );
    write(&tmp.join("src/main_test.kara"), test_attrs_and_body);
    tmp
}

#[test]
fn test_with_provider_fixture_pushes_frame_for_test() {
    let body = "#[with_provider(Clock, FakeClock { t: 42 })]\n\
                test \"reads injected clock\" {\n\
                    assert_eq(Clock.now(), 42);\n\
                }\n";
    let tmp = with_provider_project("test-with-provider-basic", body);
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected pass; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let pass = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_pass"))
        .unwrap_or_else(|| panic!("expected test_pass; got:\n{lines:?}"));
    assert!(pass.contains("\"test\":\"reads injected clock\""));
}

#[test]
fn test_with_provider_constructor_failure_emits_structured_fail() {
    // Constructor calls `boom()` which hits `unreachable()` — a runtime
    // error surfaces before the frame is pushed, and the runner emits
    // `provider_construction_failed` instead of running the test body.
    //
    // Runs under whatever the default execution path is for the build (JIT
    // when compiled `--features lljit_prototype`, interpreter otherwise) —
    // NOT pinned. Under JIT the distinction is recovered by the synth main's
    // per-ctor `PROVIDER_CTOR_MARKER` checkpoints: the failing ctor never
    // prints its marker, so the runner sees fewer markers than fixtures and
    // reports `provider_construction_failed` for the un-constructed resource
    // (with `duration_ms` 0), matching the interpreter path. Both lanes must
    // produce identical fields.
    let body = "fn boom() -> FakeClock { unreachable() }\n\
                #[with_provider(Clock, boom())]\n\
                test \"broken fixture\" {\n\
                    assert_eq(1, 1);\n\
                }\n";
    let tmp = with_provider_project("test-with-provider-ctor-fail", body);
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail; got:\n{lines:?}"));
    assert!(
        fail.contains("\"reason\":\"provider_construction_failed\""),
        "missing reason in fail event: {fail}"
    );
    assert!(
        fail.contains("\"resource\":\"Clock\""),
        "missing resource in fail event: {fail}"
    );
    assert!(
        fail.contains("\"duration_ms\":0"),
        "duration_ms should be 0 when test body never ran: {fail}"
    );
}

#[test]
fn test_with_provider_trait_less_user_resource_fixture_dispatches() {
    // A *trait-less* user resource (`effect resource Foo;`, no provider trait)
    // used as a fixture. Its dispatch is module-local (no canonical method
    // order — derived per module from the override type's inherent impl), so
    // the per-test JIT path must run this module in FULL mode (the persistent-
    // module cache would split the `with_provider` site from the `Foo.val()`
    // call site and drop the override → `Foo.val()` returned 0). Two fixtures
    // exercise the multi-resource path. Both lanes (JIT-default under
    // lljit_prototype, interpreter under plain --features llvm) must pass.
    let tmp = scratch_project("test-with-provider-traitless-user");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "effect resource FooA;\n\
         effect resource FooB;\n\
         struct ProvA { v: i64 }\n\
         impl ProvA { fn val(self) -> i64 { self.v } }\n\
         struct ProvB { v: i64 }\n\
         impl ProvB { fn val(self) -> i64 { self.v } }\n\
         fn make_b() -> ProvB { ProvB { v: 20 } }\n\
         fn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        // First fixture is a struct literal, second is a constructor *call*
        // (exercises the pre-pass return-type inference).
        "#[with_provider(FooA, ProvA { v: 10 })]\n\
         #[with_provider(FooB, make_b())]\n\
         test \"trait-less user resources dispatch\" {\n\
             assert_eq(FooA.val(), 10);\n\
             assert_eq(FooB.val(), 20);\n\
         }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "expected pass; stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    assert!(
        lines.iter().any(|l| event_kind(l) == Some("test_pass")),
        "expected test_pass; got:\n{lines:?}"
    );
}

#[test]
fn test_with_provider_second_constructor_failure_names_that_resource() {
    // Two trait-less user-resource fixtures: the FIRST ctor succeeds, the
    // SECOND faults. The fail event must name the *second* resource (FooB) in
    // `provider_construction_failed` — proving (a) multi-fixture trait-less
    // dispatch codegens at all (full mode + call-ctor type inference), and
    // (b) the marker-count → fixture-index recovery picks the right resource.
    // Both lanes agree (the interpreter stops at the first failing ctor).
    let tmp = scratch_project("test-with-provider-2nd-ctor-fail");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "effect resource FooA;\n\
         effect resource FooB;\n\
         struct ProvA { v: i64 }\n\
         impl ProvA { fn val(self) -> i64 { self.v } }\n\
         struct ProvB { v: i64 }\n\
         impl ProvB { fn val(self) -> i64 { self.v } }\n\
         fn boom_b() -> ProvB { unreachable() }\n\
         fn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "#[with_provider(FooA, ProvA { v: 10 })]\n\
         #[with_provider(FooB, boom_b())]\n\
         test \"second fixture broken\" {\n\
             assert_eq(1, 1);\n\
         }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail; got:\n{lines:?}"));
    assert!(
        fail.contains("\"reason\":\"provider_construction_failed\""),
        "missing reason in fail event: {fail}"
    );
    assert!(
        fail.contains("\"resource\":\"FooB\""),
        "should name the SECOND resource whose ctor faulted: {fail}"
    );
    assert!(
        fail.contains("\"duration_ms\":0"),
        "duration_ms should be 0 when test body never ran: {fail}"
    );
}

#[test]
fn test_with_provider_and_requires_conflict_rejected_at_discovery() {
    // Same resource in both `requires` and `with_provider` — design.md
    // rejects with `requires_and_with_provider_conflict`.
    let body = "#[test(requires = [Clock])]\n\
                #[with_provider(Clock, FakeClock { t: 0 })]\n\
                test \"contradictory fixture\" {\n\
                    assert_eq(1, 1);\n\
                }\n";
    let tmp = with_provider_project("test-with-provider-conflict", body);
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail; got:\n{lines:?}"));
    assert!(
        fail.contains("\"reason\":\"requires_and_with_provider_conflict\""),
        "missing conflict reason: {fail}"
    );
    assert!(
        fail.contains("\"resources\":[\"Clock\"]"),
        "missing conflict resource list: {fail}"
    );
}

#[test]
fn test_with_provider_fail_event_grows_providers_field() {
    // The test body fails; the `test_fail` event should carry a
    // `providers` array listing the active fixtures. Passing tests stay
    // lean and do not carry this field.
    let body = "#[with_provider(Clock, FakeClock { t: 7 })]\n\
                test \"fails with fixture\" {\n\
                    assert_eq(Clock.now(), 999);\n\
                }\n";
    let tmp = with_provider_project("test-with-provider-fail-providers-field", body);
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let fail = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_fail"))
        .unwrap_or_else(|| panic!("expected test_fail; got:\n{lines:?}"));
    assert!(
        fail.contains("\"providers\":[\"Clock\"]"),
        "missing providers field in fail event: {fail}"
    );
}

#[test]
fn test_multiple_with_provider_attributes_all_active_in_body() {
    // Two `#[with_provider]` attributes — both providers visible inside
    // the test body. Source order is outer-to-inner per spec.
    let tmp = scratch_project("test-with-provider-multi");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "effect resource Clock;\n\
         effect resource AuditLog;\n\
         struct FakeClock { t: i64 }\n\
         impl FakeClock { fn now(self) -> i64 { self.t } }\n\
         struct FakeLog { n: i64 }\n\
         impl FakeLog { fn count(self) -> i64 { self.n } }\n\
         fn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "#[with_provider(Clock, FakeClock { t: 42 })]\n\
         #[with_provider(AuditLog, FakeLog { n: 3 })]\n\
         test \"two providers\" {\n\
             assert_eq(Clock.now(), 42);\n\
             assert_eq(AuditLog.count(), 3);\n\
         }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}");
    let lines = jsonl_lines(&stdout);
    let pass = lines
        .iter()
        .find(|l| event_kind(l) == Some("test_pass"))
        .unwrap_or_else(|| panic!("expected test_pass; got:\n{lines:?}"));
    // Pass events stay lean — no `providers` field.
    assert!(
        !pass.contains("\"providers\""),
        "pass event should not carry providers field: {pass}"
    );
}

#[test]
fn test_with_provider_frame_popped_between_tests() {
    // Test A uses `#[with_provider(Clock, ...)]` to install a FakeClock
    // returning a fixed value; test B has no fixture. After A exits, its
    // provider frame must be popped so B's bare `Clock.now()` falls back
    // to the ambient default provider (CR-A slice 3) and does NOT see
    // A's FakeClock value.
    let tmp = scratch_project("test-with-provider-isolation");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "struct FakeClock { t: i64 }\n\
         impl FakeClock { fn now(self) -> i64 { self.t } }\n\
         fn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "#[with_provider(Clock, FakeClock { t: 1 })]\n\
         test \"a has clock\" { assert_eq(Clock.now(), 1); }\n\
         test \"b no fixture\" { assert_ne(Clock.now(), 1); }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    let a = lines
        .iter()
        .find(|l| l.contains("\"test\":\"a has clock\""))
        .unwrap_or_else(|| panic!("expected `a has clock` event; got:\n{lines:?}"));
    assert_eq!(event_kind(a), Some("test_pass"));
    let b = lines
        .iter()
        .find(|l| l.contains("\"test\":\"b no fixture\""))
        .unwrap_or_else(|| panic!("expected `b no fixture` event; got:\n{lines:?}"));
    assert_eq!(event_kind(b), Some("test_pass"));
}

#[test]
fn test_with_provider_overrides_builtin_primitive_clock() {
    // CR-C coverage slot: `#[with_provider(Clock, ...)]` overrides the
    // ambient default provider for the test's duration. Built-in
    // primitives use the same push-on-stack mechanism as user-declared
    // resources (design.md § Testing: "Built-in primitives use the same
    // mechanism"). The second test proves the override is scoped to the
    // attributed test — without the fixture, Clock.now() returns the
    // ambient default (system time, not the fake value 1700000000).
    let tmp = scratch_project("test-with-provider-builtin-override");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "struct FakeClock { t: i64 }\n\
         impl FakeClock { fn now(self) -> i64 { self.t } }\n\
         fn main() {}\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "#[with_provider(Clock, FakeClock { t: 1_700_000_000 })]\n\
         test \"clock override returns fake\" {\n\
             assert_eq(Clock.now(), 1_700_000_000);\n\
         }\n\
         test \"clock without fixture uses ambient default\" {\n\
             assert_ne(Clock.now(), 1_700_000_000);\n\
         }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines = jsonl_lines(&stdout);
    let with_fixture = lines
        .iter()
        .find(|l| l.contains("\"test\":\"clock override returns fake\""))
        .unwrap_or_else(|| panic!("expected override event; got:\n{lines:?}"));
    assert_eq!(event_kind(with_fixture), Some("test_pass"));
    let without_fixture = lines
        .iter()
        .find(|l| l.contains("\"test\":\"clock without fixture uses ambient default\""))
        .unwrap_or_else(|| panic!("expected ambient-default event; got:\n{lines:?}"));
    assert_eq!(event_kind(without_fixture), Some("test_pass"));
}

// ── karac check --profiles ──────────────────────────────────────

#[test]
fn test_check_profiles_subcommand_help_lists_flag() {
    let out = karac_bin().args(["check", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--profiles"),
        "expected --profiles documented in `check --help`, got:\n{stdout}"
    );
}

#[test]
fn test_check_profiles_unknown_profile_rejected() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/clean.kara",
            "--profiles=embedded,does_not_exist",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit on unknown profile name"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown profile"),
        "expected 'unknown profile' diagnostic, got:\n{stderr}"
    );
    assert!(
        stderr.contains("does_not_exist"),
        "expected the offending profile name in error, got:\n{stderr}"
    );
}

#[test]
fn test_check_profiles_empty_list_rejected() {
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara", "--profiles="])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--profiles requires at least one profile"),
        "expected empty-list diagnostic, got:\n{stderr}"
    );
}

#[test]
fn test_check_profiles_all_clean_program_passes() {
    // A profile-neutral program (no `extern` declarations) compiles cleanly
    // under every profile — `--profiles=all` should exit 0 and emit a
    // grouped header per profile.
    let out = karac_bin()
        .args(["check", "tests/snapshots/clean.kara", "--profiles=all"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "expected clean program to pass all profiles, stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("profile: default"));
    assert!(stderr.contains("profile: embedded"));
    assert!(stderr.contains("profile: kernel"));
    assert!(
        stderr.matches("All checks passed").count() == 3,
        "expected one pass message per profile, got:\n{stderr}"
    );
}

#[test]
fn test_check_profiles_per_profile_grouping_on_violation() {
    // The `allocates(Heap)` extern is fine under `default`, rejected under
    // `embedded` and `kernel`. Grouped output must keep the passing and
    // failing profiles separable.
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/profile_extern_heap.kara",
            "--profiles=all",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit when at least one profile fails"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("── profile: default ──"));
    assert!(stderr.contains("── profile: embedded ──"));
    assert!(stderr.contains("── profile: kernel ──"));
    assert!(
        stderr.contains("All checks passed under 'default' profile"),
        "expected default profile to pass; got:\n{stderr}"
    );
    assert!(
        stderr.contains("error(s) under 'embedded' profile"),
        "expected embedded profile to report errors; got:\n{stderr}"
    );
    assert!(
        stderr.contains("error(s) under 'kernel' profile"),
        "expected kernel profile to report errors; got:\n{stderr}"
    );
    // Profile name surfaces in the underlying E0405 message.
    assert!(stderr.contains("'embedded' profile"));
}

#[test]
fn test_check_profiles_subset_only_runs_named_profiles() {
    // `--profiles=embedded,kernel` should not include `default` in the
    // output, even when the file would have passed under it.
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/profile_extern_heap.kara",
            "--profiles=embedded,kernel",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("profile: embedded"));
    assert!(stderr.contains("profile: kernel"));
    assert!(
        !stderr.contains("profile: default"),
        "default profile should not appear when not in --profiles list, got:\n{stderr}"
    );
}

#[test]
fn test_check_profiles_dedupes_repeated_names() {
    // Specifying the same profile twice should run it once, not twice.
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/clean.kara",
            "--profiles=embedded,embedded",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("profile: embedded").count(),
        1,
        "duplicate profile name should be de-duplicated, got:\n{stderr}"
    );
}

#[test]
fn test_check_profiles_json_output() {
    // JSON output must wrap every profile run as a labeled object inside the
    // top-level `profiles` array, with an aggregate `success` field.
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/profile_extern_heap.kara",
            "--profiles=all",
            "--output=json",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.starts_with('{'));
    assert!(
        json.contains("\"profiles\":["),
        "expected top-level profiles array, got:\n{json}"
    );
    assert!(json.contains("\"profile\":\"default\""));
    assert!(json.contains("\"profile\":\"embedded\""));
    assert!(json.contains("\"profile\":\"kernel\""));
    // Aggregate success is false when any profile fails.
    assert!(json.contains("\"success\":false"));
    // E0405 (ProfileViolation) is the underlying effect-checker code.
    assert!(
        json.contains("\"code\":\"E0405\""),
        "expected E0405 ProfileViolation in JSON, got:\n{json}"
    );
}

#[test]
fn test_check_profiles_jsonl_emits_per_profile_events() {
    let out = karac_bin()
        .args([
            "check",
            "tests/snapshots/profile_extern_heap.kara",
            "--profiles=embedded,kernel",
            "--output=jsonl",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"type\":\"profile_start\""));
    assert!(stdout.contains("\"type\":\"profile_complete\""));
    assert!(stdout.contains("\"profile\":\"embedded\""));
    assert!(stdout.contains("\"profile\":\"kernel\""));
    // Each profile's pipeline emits its own build_complete frame.
    let build_completes = stdout.matches("\"type\":\"build_complete\"").count();
    assert_eq!(
        build_completes, 2,
        "expected one build_complete per profile, got {build_completes}"
    );
}

// ── karac query cost-summary ────────────────────────────────────

#[test]
fn test_query_cost_summary_clean_program_zeroes() {
    // A pure program with no RC fallbacks, no `with_provider` calls, and
    // no shared structs returns the all-zeros envelope and an empty
    // `by_function` list.
    let out = karac_bin()
        .args(["query", "cost-summary", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "expected zero exit on clean program: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.starts_with('{'));
    assert!(json.contains("\"scope\":\"tests/snapshots/clean.kara\""));
    assert!(json.contains("\"rc_ops\":{\"count\":0,\"rc\":0,\"arc\":0,\"suppressed\":0}"));
    assert!(json.contains("\"arc_provider_wraps\":0"));
    assert!(json.contains("\"borrow_flag_fields\":0"));
    assert!(json.contains("\"partition_guard_sites\":0"));
    assert!(json.contains("\"auto_clone_insertions\":0"));
    assert!(json.contains("\"by_function\":[]"));
}

#[test]
fn test_query_cost_summary_rc_fallback_attributed_to_function() {
    // The fixture's `process` function consumes `d` in a match arm and
    // re-uses it afterward — a textbook RC fallback. The aggregator must
    // surface one rc_op against `process`, with a derivation entry that
    // names the binding and the trigger.
    let out = karac_bin()
        .args([
            "query",
            "cost-summary",
            "tests/snapshots/cost_summary_rc.kara",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"rc_ops\":{\"count\":1,\"rc\":1,\"arc\":0,\"suppressed\":0}"));
    assert!(json.contains("\"function\":\"process\""));
    assert!(json.contains("\"rc_ops\":1"));
    // Phase-7-codegen.md line 27 — G12 monitoring surface. Without
    // `#[allow(rc_fallback)]`, the row is not flagged suppressed.
    assert!(json.contains("\"rc_ops_suppressed\":false"));
    assert!(
        json.contains("Rc fallback for `d`"),
        "expected derivation reason naming binding `d`, got: {json}"
    );
    assert!(json.contains("direct re-use after consume"));
    // Other categories must remain at zero — the fixture has no shared
    // struct and no `with_provider` calls.
    assert!(json.contains("\"arc_provider_wraps\":0"));
    assert!(json.contains("\"borrow_flag_fields\":0"));
}

#[test]
fn test_query_cost_summary_rc_fallback_suppressed_via_allow_is_surfaced() {
    // Phase-7-codegen.md line 27 — G12 monitoring surface. When the
    // function carries `#[allow(rc_fallback)]`, the user-facing perf
    // note is silenced but the RC entry still flows into cost-summary
    // — and the entry must now be visible as suppressed so PR review
    // tooling can spot AI-agent over-use of `#[allow]`.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-cost-summary-suppressed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("suppressed.kara");
    let src = "struct Data { value: i64 }\n\
               fn consume(d: Data) {}\n\
               fn use_d(d: Data) {}\n\
               #[allow(rc_fallback)]\n\
               fn process(d: Data) {\n\
                   match d.value {\n\
                       0 => consume(d),\n\
                       _ => {}\n\
                   }\n\
                   use_d(d);\n\
               }\n\
               fn main() {\n\
                   process(Data { value: 0 });\n\
               }\n";
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "cost-summary", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "expected zero exit; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    // Totals expose the suppression count alongside the existing rc/arc
    // breakdown — `count` and `suppressed` both equal 1 because every
    // RC entry under the `#[allow]`-bearing function contributes to
    // both. PR-level alerts can compare these numbers to spot the
    // "100% suppression" anti-pattern.
    assert!(
        json.contains("\"rc_ops\":{\"count\":1,\"rc\":1,\"arc\":0,\"suppressed\":1}"),
        "expected suppressed:1 in totals; got {json}",
    );
    // Per-row flag identifies which function carries the allow.
    assert!(
        json.contains("\"function\":\"process\""),
        "expected `process` row; got {json}",
    );
    assert!(
        json.contains("\"rc_ops_suppressed\":true"),
        "expected rc_ops_suppressed:true for process; got {json}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_query_cost_summary_borrow_flags_count_shared_mut_fields() {
    // Two `mut` fields on a `shared struct`, plus one non-mut field, must
    // produce `borrow_flag_fields: 2` (only `mut` fields cost a flag).
    // The category is struct-attributable, so `by_function` stays empty.
    // The same definition is the trigger for the Tier 2
    // `perf[shared-struct-mut-field]` perf note: one entry per offending
    // struct (not per field), with the field names enumerated in the
    // message body so the migration target is visible without re-reading
    // the source.
    let out = karac_bin()
        .args([
            "query",
            "cost-summary",
            "tests/snapshots/cost_summary_shared.kara",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"borrow_flag_fields\":2"));
    assert!(json.contains("\"by_function\":[]"));
    // Tier 2 perf note: exactly one entry, code stable, message names the
    // struct and both `mut` fields, site points at the struct definition.
    assert!(
        json.contains("\"code\":\"perf[shared-struct-mut-field]\""),
        "expected perf note code, got: {json}"
    );
    assert!(
        json.contains("`shared struct Counter`"),
        "expected struct name in perf note message, got: {json}"
    );
    assert!(
        json.contains("`hits`") && json.contains("`misses`"),
        "expected mut field names in perf note message, got: {json}"
    );
    // Exactly one note (one offending struct, one entry — not one per
    // field). Counted by occurrences of the stable code.
    let note_count = json.matches("perf[shared-struct-mut-field]").count();
    assert_eq!(
        note_count, 1,
        "expected exactly one perf note for the offending struct, got {note_count}: {json}"
    );
}

#[test]
fn test_query_cost_summary_no_perf_note_for_shared_without_mut() {
    // Negative-1: a `shared struct` with zero `mut` fields must NOT trigger
    // the Tier 2 perf note. The migration hint is predictive of *future*
    // concurrent-access cost, which depends on a `mut` field being present
    // — without one the borrow-flag and `par struct` migration framing both
    // collapse, so the note has nothing to predict.
    let src = "shared struct ReadOnly {\n    a: i64,\n    b: i64,\n}\nfn main() {}\n";
    let path = std::env::temp_dir().join(format!(
        "karac-perfnote-shared-no-mut-{}-{}.kara",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["query", "cost-summary", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"borrow_flag_fields\":0"));
    assert!(
        json.contains("\"perf_notes\":[]"),
        "expected empty perf_notes for shared struct without mut fields, got: {json}"
    );
}

#[test]
fn test_query_cost_summary_no_perf_note_for_plain_struct_with_mut() {
    // Negative-2: a plain (non-`shared`) struct with `mut` fields must NOT
    // trigger the Tier 2 perf note. The diagnostic is gated on
    // `kind == Shared` because it predicts the cost of a future
    // `shared struct` → `par struct` migration; a plain struct has no such
    // future, and `mut` on a plain-struct field is a parser-level shape that
    // doesn't pay a borrow-flag cost in the first place.
    let src = "struct Plain {\n    mut x: i64,\n    mut y: i64,\n}\nfn main() {}\n";
    let path = std::env::temp_dir().join(format!(
        "karac-perfnote-plain-mut-{}-{}.kara",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["query", "cost-summary", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"borrow_flag_fields\":0"));
    assert!(
        json.contains("\"perf_notes\":[]"),
        "expected empty perf_notes for plain struct with mut fields, got: {json}"
    );
}

#[test]
fn test_query_cost_summary_arc_provider_wraps_per_call_site() {
    // Two `with_provider[Clock](...)` call sites in two different functions
    // must each contribute one `arc_provider_wraps` and a derivation entry.
    let out = karac_bin()
        .args([
            "query",
            "cost-summary",
            "tests/snapshots/cost_summary_provider.kara",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"arc_provider_wraps\":2"));
    assert!(json.contains("\"function\":\"run_one\""));
    assert!(json.contains("\"function\":\"run_two\""));
    assert!(json.contains("with_provider[Clock]"));
}

#[test]
fn test_query_cost_summary_rejects_function_dot_form() {
    // The cost-summary kind takes a bare file — appending `.fn_name` should
    // either be treated as the file path itself (which won't exist) or
    // surface an unhelpful error. Either way the exit must be non-zero.
    let out = karac_bin()
        .args([
            "query",
            "cost-summary",
            "tests/snapshots/clean.kara.process",
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit on missing file"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot read"),
        "expected file-not-found error, got: {stderr}"
    );
}

#[test]
fn test_query_help_lists_cost_summary_kind() {
    let out = karac_bin().args(["query", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cost-summary"),
        "expected cost-summary documented in `query --help`, got:\n{stdout}"
    );
}

#[test]
fn test_query_unknown_kind_lists_cost_summary_in_hint() {
    let out = karac_bin()
        .args(["query", "garbage", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cost-summary"),
        "expected cost-summary listed in unknown-kind hint, got: {stderr}"
    );
}

// ── karac query monomorphization (phase-7-codegen.md line 97) ───

#[test]
fn test_query_monomorphization_clean_program_is_empty_envelope() {
    // Clean fixture has no generic calls — the envelope ships with
    // zero generics and zero instances. Surface lock: external
    // tooling pinning to the schema must see the same `by_generic`,
    // `totals` keys even when empty.
    let out = karac_bin()
        .args(["query", "monomorphization", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "expected zero exit on clean program: stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.starts_with('{'));
    assert!(json.contains("\"scope\":\"tests/snapshots/clean.kara\""));
    assert!(json.contains("\"by_generic\":[]"));
    assert!(json.contains("\"generic_count\":0"));
    assert!(json.contains("\"instance_count\":0"));
}

#[test]
fn test_query_monomorphization_records_each_distinct_type_arg_tuple() {
    // Two callers at different types produce two instances under
    // one generic; the envelope renders both with the expected
    // shape — `types` list, empty `effects` slot, and a string
    // `site` (per design.md schema).
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-monomorphization-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("generic.kara");
    let src = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(7);
    let _ = identity(true);
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["query", "monomorphization", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "expected zero exit; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json = stdout.trim();
    assert!(json.contains("\"generic\":\"identity\""));
    assert!(
        json.contains("\"instance_count\":2"),
        "expected instance_count:2 for two distinct type args; got {json}",
    );
    // Both type tuples surface. `identity[T]` has no `with E`
    // variable, so each instance's effective `effects` set is empty;
    // the `site` is a `<file>:<line>:<col>` string.
    assert!(
        json.contains("\"types\":[\"i64\"]"),
        "expected an i64 instance entry; got {json}",
    );
    assert!(
        json.contains("\"types\":[\"bool\"]"),
        "expected a bool instance entry; got {json}",
    );
    assert!(json.contains("\"effects\":[]"));
    // Site is a string in design.md schema form, not an object. The
    // expected prefix is JSON-escaped (`\` → `\\`) so the assertion
    // also holds on Windows, where the temp path contains backslashes
    // that the JSON renderer escapes (no-op on unix paths).
    assert!(
        json.contains(&format!(
            "\"site\":\"{}:",
            path.to_str().unwrap().replace('\\', "\\\\")
        )),
        "expected site rendered as `<file>:<line>:<col>` string; got {json}",
    );
    // Totals line up — one generic, two instances overall.
    assert!(json.contains("\"generic_count\":1"));
    assert!(json.contains("\"instance_count\":2"));
}

#[test]
fn test_query_monomorphization_help_and_kind_routing() {
    // Help text exposes the new kind so external tooling can
    // discover it; the unknown-kind hint also lists it.
    let help = karac_bin().args(["query", "--help"]).output().unwrap();
    assert!(help.status.success());
    let help_stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        help_stdout.contains("monomorphization"),
        "expected `monomorphization` documented in query help; got:\n{help_stdout}",
    );

    let unknown = karac_bin()
        .args(["query", "garbage", "tests/snapshots/clean.kara"])
        .output()
        .unwrap();
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(
        stderr.contains("monomorphization"),
        "expected `monomorphization` in unknown-kind hint; got: {stderr}",
    );
}

// ── karac build --monomorphization-budget (phase-7-codegen.md line 266) ──

/// One generic (`identity`) instantiated at three distinct primitive
/// types → one generic, three instances. Budget tests dial thresholds
/// around the count of 3. All three literal forms (`i64`, `bool`, `char`)
/// are Copy, so the warn/default variants link cleanly.
#[cfg(feature = "llvm")]
const MONO_BUDGET_FIXTURE: &str = r#"
fn identity[T](x: T) -> T { x }
fn main() {
    let _ = identity(1i64);
    let _ = identity(true);
    let _ = identity('c');
}
"#;

/// Fresh temp `.kara` path keyed by pid + tag + nanos so parallel runs
/// (and the per-test built executable, named after the file stem) don't
/// collide.
#[cfg(feature = "llvm")]
fn mono_budget_scratch(tag: &str, source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-monobudget-{}-{}-{}.kara",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, source).expect("write scratch source");
    path
}

/// `karac build` links an executable named after the source file stem
/// into CWD. Remove it so the warn/default builds don't pollute the tree
/// (mirrors test_build_bare_file_hot_swap_accepted). No-op when the build
/// exited before codegen (the error-threshold variants).
#[cfg(feature = "llvm")]
fn remove_built_exe(src: &std::path::Path) {
    if let Some(stem) = src.file_stem().and_then(|s| s.to_str()) {
        let exe = if cfg!(windows) {
            format!("{stem}.exe")
        } else {
            stem.to_string()
        };
        let _ = std::fs::remove_file(exe);
    }
}

// Parse-layer rejections run without the llvm feature: the malformed
// flag exits during arg parsing, before the (llvm-gated) build body.

#[test]
fn test_monomorphization_budget_rejects_empty_spec() {
    let out = karac_bin()
        .args(["build", "--monomorphization-budget=", "ignored.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("at least one of warn:N or error:M"),
        "got: {stderr}",
    );
}

#[test]
fn test_monomorphization_budget_rejects_unknown_key() {
    let out = karac_bin()
        .args(["build", "--monomorphization-budget=oops:5", "ignored.kara"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown key 'oops'"), "got: {stderr}");
}

#[test]
fn test_monomorphization_budget_rejects_non_numeric_threshold() {
    let out = karac_bin()
        .args([
            "build",
            "--monomorphization-budget=warn:abc",
            "ignored.kara",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("must be a positive integer"),
        "got: {stderr}",
    );
}

#[test]
fn test_monomorphization_budget_rejects_warn_exceeding_error() {
    let out = karac_bin()
        .args([
            "build",
            "--monomorphization-budget=warn:10,error:5",
            "ignored.kara",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warn:10 exceeds error:5"), "got: {stderr}",);
}

#[test]
fn test_monomorphization_budget_listed_in_build_help() {
    let help = karac_bin().args(["build", "--help"]).output().unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        stdout.contains("--monomorphization-budget"),
        "expected the flag in build help; got:\n{stdout}",
    );
}

// Build-behavior tests need the llvm path (cmd_build's body is llvm-gated)
// and, for the warn/default variants, the runtime archive to link.

#[cfg(feature = "llvm")]
#[test]
fn test_monomorphization_budget_error_fails_build() {
    let path = mono_budget_scratch("err", MONO_BUDGET_FIXTURE);
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--monomorphization-budget=error:2",
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    remove_built_exe(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected non-zero exit; stderr={stderr}"
    );
    assert!(
        stderr.contains("error[monomorphization-budget]"),
        "expected the error diagnostic; stderr={stderr}",
    );
    assert!(
        stderr.contains("identity"),
        "expected the offending generic named; stderr={stderr}",
    );
    assert!(
        stderr.contains("limit 2"),
        "expected the breached threshold reported; stderr={stderr}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_monomorphization_budget_warn_emits_note_but_builds() {
    let path = mono_budget_scratch("warn", MONO_BUDGET_FIXTURE);
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--monomorphization-budget=warn:3",
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    remove_built_exe(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning[monomorphization-budget]"),
        "expected the warn note (count 3 >= warn 3); stderr={stderr}",
    );
    assert!(
        out.status.success() && stdout.contains("Built: "),
        "warn-only must still build; status={:?} stdout={stdout} stderr={stderr}",
        out.status,
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_monomorphization_budget_error_supersedes_warn() {
    let path = mono_budget_scratch("super", MONO_BUDGET_FIXTURE);
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--monomorphization-budget=warn:2,error:3",
        ])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    remove_built_exe(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "expected failure; stderr={stderr}");
    assert!(
        stderr.contains("error[monomorphization-budget]") && stderr.contains("limit 3"),
        "expected error at the error threshold; stderr={stderr}",
    );
    // The same generic must not also be reported at warn level.
    assert!(
        !stderr.contains("warning[monomorphization-budget]"),
        "error level supersedes warn for the same generic; stderr={stderr}",
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_monomorphization_budget_disabled_without_flag() {
    let path = mono_budget_scratch("off", MONO_BUDGET_FIXTURE);
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    remove_built_exe(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stdout.contains("monomorphization-budget") && !stderr.contains("monomorphization-budget"),
        "no budget output expected without the flag; stdout={stdout} stderr={stderr}",
    );
    assert!(
        out.status.success() && stdout.contains("Built: "),
        "default build should succeed; stderr={stderr}",
    );
}

// ── karac fix ──────────────────────────────────────────────────

/// Build a temp `.kara` file with the given source. Returns the path so
/// the caller can pass it to `karac fix` and assert on the resulting file
/// contents. Each call uses a fresh path keyed by pid + nanos so parallel
/// test runs don't collide.
fn fix_scratch_file(tag: &str, source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-fix-{}-{}-{}.kara",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, source).expect("write scratch source");
    path
}

#[test]
fn test_fix_applies_did_you_mean_correction() {
    let path = fix_scratch_file(
        "applies",
        "fn helper() -> i64 { 42 }\nfn main() { println(helpr()); }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("applied 1 fix"));
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("println(helper())"),
        "expected `helpr` -> `helper`, got: {rewritten}"
    );
    assert!(
        !rewritten.contains("helpr"),
        "stale identifier still present: {rewritten}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_dry_run_does_not_modify_file() {
    let original = "fn helper() -> i64 { 42 }\nfn main() { println(helpr()); }\n";
    let path = fix_scratch_file("dryrun", original);
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("would apply 1 fix"));
    assert!(stdout.contains("`helpr`"));
    assert!(stdout.contains("`helper`"));
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, original, "dry-run must not write to disk");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_reports_when_no_fixable_diagnostics() {
    // A program with no diagnostics — fix prints the no-op marker and
    // exits 0.
    let path = fix_scratch_file("noop", "fn main() { let x = 42; println(x); }\n");
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no fixable diagnostics"),
        "expected no-op message, got: {stdout}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_multiple_corrections_in_one_file() {
    // Two typo'd identifiers in one file. Both should be fixed in a
    // single invocation; the reverse-offset ordering keeps later edits'
    // offsets valid.
    let path = fix_scratch_file(
        "multi",
        "fn alpha() -> i64 { 1 }\n\
         fn beta() -> i64 { 2 }\n\
         fn main() { println(alphq() + betq()); }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("applied 2 fix"));
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(rewritten.contains("alpha()"));
    assert!(rewritten.contains("beta()"));
    assert!(!rewritten.contains("alphq"));
    assert!(!rewritten.contains("betq"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_e0412_receiver_rewrite() {
    // E0412: resource trait method with a bare `self` receiver but a
    // reads-only declared clause. `karac fix` applies the effect
    // checker's `ref self` rewrite at the trait definition; the file
    // then checks clean (the receiver now seeds reads(Cfg), matching
    // the declaration).
    let path = fix_scratch_file(
        "e0412",
        "pub effect resource Cfg: Config;\n\
         pub trait Config { fn get(self, k: i64) -> i64 with reads(Cfg); }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("applied 1 fix"), "stdout: {stdout}");
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("fn get(ref self, k: i64)"),
        "expected `self` -> `ref self`, got: {rewritten}"
    );
    let recheck = karac_bin()
        .args(["check", path.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let recheck_stdout = String::from_utf8_lossy(&recheck.stdout);
    assert!(
        recheck.status.success() && recheck_stdout.contains("\"diagnostics\":[]"),
        "post-fix check must be clean; got: {recheck_stdout}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_concurrent_plain_struct_migration() {
    // B-2026-07-06-4: `ConcurrentPlainStruct` computes a full multi-edit
    // `fix_diff` migration (insert `par ` keyword + strip `mut ` + wrap
    // the mut field in `Mutex[T]`) and `collect_diagnostics` emits it to
    // JSON as `"fix_diff":[...]`. Before the fix, `cmd_fix` collected only
    // each error's single-edit `.replacement` and applied NOTHING here
    // even though the JSON advertised a fix. `karac fix` must now apply
    // the whole envelope.
    let path = fix_scratch_file(
        "concurrent-plain",
        "struct State { id: i64, mut count: i64 }\n\
         fn use_a(s: State) { }\n\
         fn use_b(s: State) { }\n\
         fn main() {\n\
             let s = State { id: 0, count: 0 };\n\
             par {\n\
                 use_a(s);\n\
                 use_b(s);\n\
             }\n\
         }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // 1 `par ` insert + 1 `mut ` strip + 2 `Mutex[`/`]` wraps = 4 edits.
    assert!(stdout.contains("applied 4 fix"), "stdout: {stdout}");
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct State"),
        "expected `par struct` migration, got: {rewritten}"
    );
    assert!(
        rewritten.contains("count: Mutex[i64]"),
        "expected mut field wrapped in Mutex, got: {rewritten}"
    );
    assert!(
        !rewritten.contains("mut count"),
        "`mut ` should have been stripped from the field, got: {rewritten}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_concurrent_shared_struct_migration() {
    // B-2026-07-06-4, sibling kind: `ConcurrentSharedStruct` renames the
    // `shared` keyword to `par` and wraps every mut field. Same dropped-
    // envelope bug; verify all 7 edits (1 rename + 2 mut strips + 2 mut
    // fields × 2 wraps) land through `karac fix`.
    let path = fix_scratch_file(
        "concurrent-shared",
        "shared struct Counter { val: i64, mut count: i64, mut tag: i64 }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0, count: 0, tag: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("applied 7 fix"), "stdout: {stdout}");
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct Counter"),
        "expected `shared` → `par` rename, got: {rewritten}"
    );
    assert!(
        rewritten.contains("count: Mutex[i64]") && rewritten.contains("tag: Mutex[i64]"),
        "expected both mut fields wrapped in Mutex, got: {rewritten}"
    );
    assert!(
        !rewritten.contains("shared struct"),
        "`shared` keyword should be gone, got: {rewritten}"
    );
    assert!(
        rewritten.contains("val: i64,"),
        "immutable field `val` must stay untouched, got: {rewritten}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_module_binding_const_rename() {
    // B-2026-07-06-3: a module-level `let camelCase` violates the
    // Const-class naming rule; the resolver computes the exact
    // SCREAMING_SNAKE candidate and now carries it as a machine-applicable
    // `.replacement` spanning the name identifier. `karac fix` applies the
    // rename at the declaration site.
    let path = fix_scratch_file("const-rename", "pub let myConfig: i64 = 5;\n");
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("applied 1 fix"), "stdout: {stdout}");
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert_eq!(rewritten, "pub let MY_CONFIG: i64 = 5;\n");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_applies_undefined_label_rename() {
    // B-2026-07-07-3: a misspelled `continue <label>` fuzzy-matches an
    // in-scope loop label; the resolver now anchors the rename on the label
    // token (`label_span`), so `karac fix` corrects it and the program then
    // compiles (the target loop exists).
    let path = fix_scratch_file(
        "label-rename",
        "fn main() {\n    outer: loop {\n        loop {\n            continue otuer;\n        }\n    }\n}\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("applied 1 fix"), "stdout: {stdout}");
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("continue outer;") && !rewritten.contains("otuer"),
        "expected `otuer` → `outer`, got: {rewritten}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_json_e0412_carries_replacement_payload() {
    // The E0412 JSON diagnostic carries the machine-applicable
    // `replacement` payload (same shape as resolver/ownership fixes)
    // so IDE quick-fix and agent consumers can apply it without
    // re-deriving the span.
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join(format!("karac_test_e0412_json_{}.kara", std::process::id()));
    std::fs::write(
        &fixture,
        "pub effect resource Cfg: Config;\n\
         pub trait Config { fn get(self, k: i64) -> i64 with reads(Cfg); }\n",
    )
    .expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"E0412\""),
        "JSON output should carry E0412; got: {stdout}"
    );
    assert!(
        stdout.contains("\"replacement\":{\"offset\":") && stdout.contains("\"text\":\"ref self\""),
        "JSON output should carry the receiver rewrite payload; got: {stdout}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_fix_help_text_lists_dry_run() {
    let out = karac_bin().args(["fix", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac fix"));
    assert!(stdout.contains("--dry-run"));
}

#[test]
fn test_fix_unknown_flag_rejected() {
    let out = karac_bin().args(["fix", "--garbage"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"));
}

#[test]
fn test_fix_missing_file_arg_rejected() {
    let out = karac_bin().arg("fix").output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing file argument"));
}

#[test]
fn test_fix_applies_n0507_mut_ref_to_ref() {
    // Round 12.32: `karac fix` now runs through ownership and applies
    // the closure-prefix rewrite from N0507 (UnusedMutCaptureNote).
    // The source compiles cleanly (the note is a perf note, not an
    // error), so without ownership-side harvesting `karac fix` would
    // print "no fixable diagnostics" — pinning that the new path
    // collects ownership replacements alongside resolver ones.
    let path = fix_scratch_file(
        "n0507-apply",
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || o.x + 1;\n\
             let _ = f();\n\
         }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fix failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("applied 1 fix"),
        "expected one fix applied, got: {stdout}"
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("let f = ref || o.x + 1;"),
        "expected `mut ref` swapped for `ref`, got: {rewritten}"
    );
    assert!(
        !rewritten.contains("mut ref"),
        "stale `mut ref` still present: {rewritten}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_dry_run_previews_n0507_without_writing() {
    // Dry-run companion to the apply test: --dry-run prints the
    // would-be rewrite (`mut ref` → `ref`) but does not modify the
    // file. The source is unchanged on disk after the command exits.
    let original = "struct Owned { x: i64 }\n\
                    fn main() {\n\
                        let o = Owned { x: 1 };\n\
                        let f = mut ref || o.x + 1;\n\
                        let _ = f();\n\
                    }\n";
    let path = fix_scratch_file("n0507-dryrun", original);
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap(), "--dry-run"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("would apply 1 fix"),
        "expected dry-run header, got: {stdout}"
    );
    assert!(
        stdout.contains("`mut ref`"),
        "expected dry-run to mention old text `mut ref`, got: {stdout}"
    );
    assert!(
        stdout.contains("`ref`"),
        "expected dry-run to mention new text `ref`, got: {stdout}"
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(on_disk, original, "dry-run must not write to disk");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_fix_aggregates_resolver_and_ownership_in_one_pass() {
    // A source that triggers BOTH a resolver `did you mean` (typo'd
    // identifier) AND an N0507 perf note (mut ref over a read-only
    // capture). One `karac fix` invocation should apply both.
    // Pinned because the cmd_fix collection step concatenates edits
    // from multiple phases — a regression that forgot one phase
    // would still apply the other and silently leave the source
    // half-rewritten.
    //
    // NOTE: the resolver-typo case (`helpr` vs `helper`) blocks
    // typecheck/ownership downstream because resolve has errors.
    // To exercise both phases in one fix pass we use a non-blocking
    // resolver class — but currently all resolver replacement
    // classes ARE errors that block typecheck. So this test instead
    // pins the negative case: a source with only the N0507 note
    // (resolve clean) gets the ownership fix applied, and the
    // resolver-only multi-fix test above (`test_fix_applies_multiple_corrections_in_one_file`)
    // pins the resolver-only path. The genuine cross-phase case will
    // arrive when a non-error resolver class gains replacement
    // metadata, or when ownership errors (not just notes) gain it.
    let path = fix_scratch_file(
        "n0507-clean-resolve",
        "struct Owned { x: i64 }\n\
         fn helper() -> i64 { 42 }\n\
         fn main() {\n\
             let _ = helper();\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || o.x + 1;\n\
             let _ = f();\n\
         }\n",
    );
    let out = karac_bin()
        .args(["fix", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(rewritten.contains("let f = ref || o.x + 1;"));
    assert!(rewritten.contains("fn helper() -> i64 { 42 }"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_main_help_lists_fix_command() {
    let out = karac_bin().arg("help").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fix <file>"));
}

#[test]
fn test_check_json_includes_replacement_metadata() {
    // The JSON envelope produced by `karac check` should include a
    // `"replacement"` payload alongside the `"hints"` field for
    // diagnostics that carry a machine-applicable edit (here:
    // resolver UndefinedName with a `did you mean` suggestion).
    let path = fix_scratch_file(
        "json-replacement",
        "fn helper() -> i64 { 42 }\nfn main() { println(helpr()); }\n",
    );
    let out = karac_bin()
        .args(["check", path.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "expected non-zero exit on undefined name"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"replacement\":"),
        "expected `replacement` field in JSON, got: {stdout}"
    );
    assert!(
        stdout.contains("\"text\":\"helper\""),
        "expected replacement text `helper`, got: {stdout}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_build_project_json_includes_replacement_for_unknown_module() {
    // Round 12.30: the project-mode JSON envelope must surface the
    // round-12.29 replacement payload for E0223 (UnknownModule).
    // Single-file `karac check` already includes it; the multi-file
    // path through `resolve_errors_json` previously dropped the
    // payload, making it unreachable from IDE consumers using
    // `karac build --output=json` against real projects (where E0223
    // can fire because imports are traversed). This test pins the
    // emission so the asymmetry can't silently regress.
    let tmp = scratch_project("json-replacement-e0223");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import greeet.hello;\nfn main() {}\n",
    );
    write(&tmp.join("src/greet.kara"), "pub fn hello() {}\n");

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["build", "--output=json"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "expected non-zero exit on unknown module"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"E0224\""),
        "expected E0224 (UnknownModule) in JSON output, got: {stdout}"
    );
    assert!(
        stdout.contains("\"replacement\":"),
        "expected `replacement` field in JSON, got: {stdout}"
    );
    assert!(
        stdout.contains("\"text\":\"greet\""),
        "expected replacement text `greet`, got: {stdout}"
    );
}

#[test]
fn test_test_project_jsonl_includes_replacement_for_unknown_item() {
    // Round 12.30 companion to the JSON test: the JSONL envelope
    // emitted by `karac test` (one event per line — `run_start`,
    // `test_pass`, `test_fail`, `resolve_error`, etc.) must also carry
    // the replacement payload when a `resolve_error` event surfaces
    // a fixable diagnostic. The misspelled brace-list item exercises
    // E0225 (UnknownItemInModule) which flows through
    // `resolve_errors_jsonl`. `karac test` always emits JSONL on stdout
    // (no `--output` flag); when compilation fails, the runner exits
    // before the test pipeline starts and dumps the diagnostic events.
    let tmp = scratch_project("jsonl-replacement-e0225");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/main.kara"),
        "import greet.helllo;\nfn main() {}\n",
    );
    write(&tmp.join("src/greet.kara"), "pub fn hello() {}\n");

    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "expected non-zero exit when compile fails before tests run"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"E0225\""),
        "expected E0225 (UnknownItemInModule) in JSONL output, got: {stdout}"
    );
    assert!(
        stdout.contains("\"replacement\":"),
        "expected `replacement` field in JSONL, got: {stdout}"
    );
    assert!(
        stdout.contains("\"text\":\"hello\""),
        "expected replacement text `hello`, got: {stdout}"
    );
}

#[test]
fn test_check_json_includes_replacement_for_n0507_note() {
    // Round 12.31: the ownership-checker N0507 (UnusedMutCaptureNote) now
    // carries machine-applicable `replacement` metadata covering the
    // closure prefix tokens (`mut ref` → `ref`). The single-file
    // `karac check --output=json` path renders ownership notes through
    // the same `extra_json` slot used for resolver replacement payloads,
    // so IDE quick-fix UIs see the same JSON shape across diagnostic
    // phases. First non-resolver class to gain replacement metadata.
    let path = fix_scratch_file(
        "json-replacement-n0507",
        "struct Owned { x: i64 }\n\
         fn main() {\n\
             let o = Owned { x: 1 };\n\
             let f = mut ref || o.x + 1;\n\
             let _ = f();\n\
         }\n",
    );
    let out = karac_bin()
        .args(["check", path.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"code\":\"N0507\""),
        "expected N0507 in JSON output, got: {stdout}"
    );
    assert!(
        stdout.contains("\"replacement\":"),
        "expected `replacement` field on the N0507 note, got: {stdout}"
    );
    assert!(
        stdout.contains("\"text\":\"ref\""),
        "expected replacement text `ref`, got: {stdout}"
    );
    let _ = std::fs::remove_file(&path);
}

// ── karac clean / install / vendor (new subcommands) ────────────────

#[test]
fn test_clean_bare_idempotent_when_no_dist() {
    // `karac clean` against a tempdir with no `dist/` should exit 0
    // and report "already absent" — bare form is idempotent.
    let dir = std::env::temp_dir().join("kara-clean-bare-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = karac_bin().arg("clean").current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "`karac clean` should exit 0 on missing dist/"
    );
    assert!(
        stdout.contains("already absent"),
        "expected `already absent` notice, got: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_clean_bare_removes_existing_dist() {
    // `karac clean` should rm -rf the project-local `dist/` directory
    // and report what was removed.
    let dir = std::env::temp_dir().join("kara-clean-remove-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dist").join("nested")).unwrap();
    std::fs::write(dir.join("dist").join("artifact.txt"), b"old").unwrap();

    let out = karac_bin().arg("clean").current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "`karac clean` should exit 0 (stdout: {stdout})"
    );
    assert!(stdout.contains("removed"));
    assert!(stdout.contains("project dist/"));
    assert!(!dir.join("dist").exists(), "dist/ should have been removed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_clean_global_targets_kara_cache() {
    // `karac clean --global` should resolve `$HOME/.kara/cache/` even
    // when the directory doesn't exist (idempotent) and reference the
    // canonical path in its output.
    let fake_home = std::env::temp_dir().join("kara-clean-global-test");
    let _ = std::fs::remove_dir_all(&fake_home);
    std::fs::create_dir_all(&fake_home).unwrap();
    let out = karac_bin()
        .args(["clean", "--global"])
        .env("HOME", &fake_home)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout: {stdout}");
    // `Path::display()` uses the platform-native separator (`\` on Windows,
    // `/` on Unix); build the expected fragment from `MAIN_SEPARATOR` so the
    // assertion matches both surfaces.
    let expected = format!(".kara{}cache", std::path::MAIN_SEPARATOR);
    assert!(
        stdout.contains(&expected),
        "expected stdout to contain `{expected}`, got: {stdout}"
    );
    assert!(stdout.contains("global cache"));
    let _ = std::fs::remove_dir_all(&fake_home);
}

#[test]
fn test_install_requires_spec() {
    let out = karac_bin().arg("install").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "`karac install` without spec must error"
    );
    assert!(stderr.contains("requires a <bin-spec>"));
}

// Build-pipeline slice (line 874) — path sources now drive the existing
// build pipeline and copy the produced binary into the install root. Git /
// registry sources surface a forward-compat `E_INSTALL_*_UNSUPPORTED`
// diagnostic until the package-fetch slice (line 845) ships.

#[test]
fn test_install_path_spec_missing_directory_emits_focused_diagnostic() {
    // The spec parses cleanly but the filesystem entry doesn't exist —
    // the pipeline-stage error surfaces with its own symbolic code so
    // CI scripts can distinguish "operator typo in spec" from "the
    // referenced directory isn't here".
    let out = karac_bin()
        .args(["install", "path=./does_not_exist_kara_install_target"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "missing directory must error");
    assert!(
        stderr.contains("E_INSTALL_PATH_NOT_FOUND"),
        "expected E_INSTALL_PATH_NOT_FOUND, got: {stderr}"
    );
}

#[test]
fn test_install_git_spec_surfaces_unsupported_error() {
    let out = karac_bin()
        .args(["install", "git=https://github.com/example/my_tool.git"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("E_INSTALL_GIT_UNSUPPORTED"),
        "expected E_INSTALL_GIT_UNSUPPORTED, got: {stderr}"
    );
    assert!(
        stderr.contains("git=https://github.com/example/my_tool.git"),
        "diagnostic must echo the spec, got: {stderr}"
    );
    assert!(
        stderr.contains("line 845"),
        "diagnostic must point at the fetch tracker entry, got: {stderr}"
    );
}

#[test]
fn test_install_registry_unpinned_surfaces_unsupported_error() {
    let out = karac_bin().args(["install", "my_tool"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("E_INSTALL_REGISTRY_UNSUPPORTED"),
        "expected E_INSTALL_REGISTRY_UNSUPPORTED, got: {stderr}"
    );
    assert!(
        stderr.contains("received: my_tool"),
        "diagnostic must echo the spec, got: {stderr}"
    );
}

#[test]
fn test_install_registry_pinned_surfaces_unsupported_error() {
    let out = karac_bin()
        .args(["install", "my_tool@^1.0"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("E_INSTALL_REGISTRY_UNSUPPORTED"),
        "expected E_INSTALL_REGISTRY_UNSUPPORTED, got: {stderr}"
    );
    // VersionReq's canonical Display preserves `^1.0`.
    assert!(
        stderr.contains("received: my_tool@^1.0"),
        "diagnostic must echo the canonical render, got: {stderr}"
    );
}

#[test]
fn test_install_path_empty_value_diagnostic() {
    let out = karac_bin().args(["install", "path="]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("E_INSTALL_MISSING_VALUE"),
        "must surface symbolic code, got: {stderr}"
    );
    assert!(stderr.contains("`path=`"), "got: {stderr}");
}

#[test]
fn test_install_garbage_version_diagnostic() {
    let out = karac_bin()
        .args(["install", "my_tool@not-a-version"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("E_INSTALL_INVALID_VERSION"),
        "got: {stderr}"
    );
    assert!(stderr.contains("`not-a-version`"), "got: {stderr}");
}

#[test]
fn test_install_hyphenated_name_diagnostic_with_suggestion() {
    let out = karac_bin().args(["install", "my-tool"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("E_INSTALL_INVALID_NAME"), "got: {stderr}");
    assert!(stderr.contains("`my_tool`"), "got: {stderr}");
}

#[test]
fn test_install_trailing_at_diagnostic() {
    let out = karac_bin().args(["install", "my_tool@"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("E_INSTALL_EMPTY_VERSION"), "got: {stderr}");
}

fn install_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-install-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_install_path_missing_manifest_surfaces_manifest_error() {
    // The directory exists but has no kara.toml — the install path
    // reaches the manifest loader, which emits its existing
    // `not inside a kara project` diagnostic. install doesn't try to
    // wrap that in an install-specific code; the manifest layer
    // already produces a focused error.
    let tmp = install_tempdir("no-manifest");
    let spec = format!("path={}", tmp.display());
    let out = karac_bin().args(["install", &spec]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "missing manifest must error");
    // Manifest layer emits a recognisable phrase when the project
    // root has no kara.toml; we don't pin the exact code here because
    // it lives in the manifest module.
    assert!(
        stderr.to_lowercase().contains("kara.toml")
            || stderr.to_lowercase().contains("manifest")
            || stderr.to_lowercase().contains("not inside"),
        "expected manifest-layer diagnostic, got: {stderr}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_install_path_source_builds_and_copies_binary_into_install_root() {
    let tmp = install_tempdir("path-build");
    let project = tmp.join("project");
    let install_root = tmp.join("install_root");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("kara.toml"),
        r#"[package]
name = "install_demo"
"#,
    )
    .unwrap();
    std::fs::write(project.join("src/main.kara"), "fn main() {}\n").unwrap();

    let spec = format!("path={}", project.display());
    let out = karac_bin()
        .args(["install", &spec])
        .env("KARAC_INSTALL_ROOT", &install_root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let installed = install_root.join("install_demo");
    let installed_exists = installed.exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "install of a path source must succeed; stdout={stdout} stderr={stderr}"
    );
    assert!(
        installed_exists,
        "the binary must be copied into KARAC_INSTALL_ROOT; expected: {}",
        installed.display()
    );
    assert!(
        stdout.contains("installed `install_demo`"),
        "expected success summary, got stdout={stdout}"
    );
}

#[cfg(feature = "llvm")]
#[test]
fn test_install_path_source_overwrites_existing_binary() {
    // Idempotency pin: a second install over the same install root
    // overwrites the existing binary (no "file exists" failure).
    let tmp = install_tempdir("path-overwrite");
    let project = tmp.join("project");
    let install_root = tmp.join("install_root");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::write(
        project.join("kara.toml"),
        r#"[package]
name = "overwrite_demo"
"#,
    )
    .unwrap();
    std::fs::write(project.join("src/main.kara"), "fn main() {}\n").unwrap();

    let spec = format!("path={}", project.display());
    let first = karac_bin()
        .args(["install", &spec])
        .env("KARAC_INSTALL_ROOT", &install_root)
        .output()
        .unwrap();
    let second = karac_bin()
        .args(["install", &spec])
        .env("KARAC_INSTALL_ROOT", &install_root)
        .output()
        .unwrap();
    let installed = install_root.join("overwrite_demo");
    let installed_exists = installed.exists();
    let first_stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    let second_stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        first.status.success(),
        "first install must succeed; stderr={first_stderr}"
    );
    assert!(
        second.status.success(),
        "second install must succeed (idempotent); stderr={second_stderr}"
    );
    assert!(
        installed_exists,
        "the binary must still be present after the second install"
    );
}

#[test]
fn test_vendor_rejects_extra_args() {
    let out = karac_bin().args(["vendor", "extra"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("takes no positional arguments"));
}

#[test]
fn test_subcommand_help_clean() {
    for flag in ["--help", "-h"] {
        let out = karac_bin().args(["clean", flag]).output().unwrap();
        assert!(out.status.success(), "`karac clean {flag}` should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("karac clean"));
        assert!(stdout.contains("--global"));
    }
}

#[test]
fn test_subcommand_help_install() {
    let out = karac_bin().args(["install", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac install"));
    assert!(stdout.contains("<bin-spec>"));
}

#[test]
fn test_subcommand_help_vendor() {
    let out = karac_bin().args(["vendor", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac vendor"));
    assert!(stdout.contains("--offline"));
}

// ── karac explain --concept=<name> ──────────────────────────────

#[test]
fn test_main_help_lists_explain_command() {
    let out = karac_bin().arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("explain --concept=NAME"),
        "top-level help should advertise the explain subcommand"
    );
    assert!(
        stdout.contains("closures"),
        "top-level help should list the supported explain concepts"
    );
}

#[test]
fn test_subcommand_help_explain() {
    for flag in ["--help", "-h"] {
        let out = karac_bin().args(["explain", flag]).output().unwrap();
        assert!(out.status.success(), "`karac explain {flag}` should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("karac explain"));
        assert!(stdout.contains("--concept=NAME"));
        assert!(stdout.contains("closures"));
        assert!(
            stdout.contains("karac query ownership"),
            "scoped help should point at the per-function inspection surface"
        );
    }
}

#[test]
fn test_explain_concept_closures_renders_page() {
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "explain --concept=closures should exit 0; stderr was: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Section headers from the page.
    assert!(stdout.contains("Closures: parameter modes, capture, and escape"));
    assert!(stdout.contains("Rule 2 first-use inference"));
    assert!(stdout.contains("Explicit prefixes: own | ref | mut ref"));
    assert!(stdout.contains("K2 conflict table"));
}

#[test]
fn test_explain_concept_closures_pins_first_use_classification() {
    // The Rule 2 mapping (read → ref, mutate → mut ref, consume → own)
    // is the load-bearing concept the page exists to teach. Pin all
    // three rows so a future copyedit cannot silently drop one.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("first use is a read"));
    assert!(stdout.contains("first use is a mutate"));
    assert!(stdout.contains("first use is a consume"));
    assert!(stdout.contains("`ref < mut ref < own`"));
}

#[test]
fn test_explain_concept_closures_pins_k2_ref_consume_redirect() {
    // Pin the *exact* diagnostic redirect wording the ownership checker
    // emits for the `ref` + consume K2 violation (see slice 1 of
    // phase-5-diagnostics.md § Closure default capture mode, and
    // src/ownership/expr_check.rs line ~1004). If the diagnostic gets
    // rephrased, this page must move with it or the user reading the
    // page after hitting the error will not see matching guidance.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("drop the `ref` prefix (use `own` or bare) or remove the consume"),
        "page must pin the exact `ref` + consume K2 redirect string the ownership checker emits"
    );
}

#[test]
fn test_explain_concept_closures_pins_k2_mut_ref_consume_redirect() {
    // Symmetric pin for the `mut ref` + consume K2 violation
    // (src/ownership/expr_check.rs line ~1007).
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("drop the `mut ref` prefix and use `own`"),
        "page must pin the exact `mut ref` + consume K2 redirect string"
    );
}

#[test]
fn test_explain_concept_closures_pins_unused_mut_capture_note() {
    // The `mut ref` declared / read-only used perf note is the one
    // non-error K2 row that fires a diagnostic — pin its code name
    // so the page tracks the checker.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("perf[unused-mut-capture]"));
}

#[test]
fn test_explain_concept_closures_cross_references_disjoint_capture() {
    // Slice 2 spec requirement (b): cross-reference the disjoint
    // capture (Rule 2¼) extension so a reader knows the per-name
    // granularity is temporary and where the per-path future lives.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Rule 2¼"));
    assert!(stdout.contains("Disjoint closure capture"));
}

#[test]
fn test_explain_concept_closures_links_to_query_ownership() {
    // Slice 2 spec requirement (c): link to `karac query ownership`
    // as the per-function inspection surface. The page's final
    // section is the user's entry point for inspecting inferred
    // modes against a real source file.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac query ownership <file>.<function>"));
    assert!(stdout.contains("Inspecting inferred capture modes"));
}

#[test]
fn test_explain_concept_closures_describes_outer_scope_rc_routing() {
    // Slice 1 of phase-5-diagnostics.md § Closure default capture mode
    // documents the outer-scope routing case: a bare body that consumes
    // a captured root does NOT produce a use-after-move on a post-closure
    // outer-scope use — the binding promotes via RC fallback trigger 2
    // (RcTrigger::ClosureCaptureWithOuterUse). This is the most
    // common surprise the page exists to demystify.
    let out = karac_bin()
        .args(["explain", "--concept=closures"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("RcTrigger::ClosureCaptureWithOuterUse"));
    assert!(stdout.contains("Outer-scope routing"));
}

#[test]
fn test_explain_requires_concept_flag() {
    let out = karac_bin().arg("explain").output().unwrap();
    assert!(
        !out.status.success(),
        "bare `karac explain` should exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--concept"));
    assert!(stderr.contains("closures"));
}

#[test]
fn test_explain_rejects_unknown_concept_with_supported_set() {
    let out = karac_bin()
        .args(["explain", "--concept=galaxy_brain"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown concept 'galaxy_brain'"),
        "stderr should name the rejected concept; got: {stderr}"
    );
    assert!(
        stderr.contains("Supported:") && stderr.contains("closures"),
        "stderr should list the supported concept set; got: {stderr}"
    );
}

#[test]
fn test_explain_rejects_empty_concept_value() {
    let out = karac_bin()
        .args(["explain", "--concept="])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--concept requires a name"));
}

#[test]
fn test_explain_rejects_duplicate_concept_flags() {
    let out = karac_bin()
        .args(["explain", "--concept=closures", "--concept=closures"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--concept may only be specified once"));
}

#[test]
fn test_explain_rejects_positional_argument() {
    let out = karac_bin().args(["explain", "closures"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--concept=NAME"));
}

#[test]
fn test_explain_rejects_unknown_flag() {
    let out = karac_bin()
        .args(["explain", "--concept=closures", "--banana"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag '--banana'"));
}

// ── karac explain --class=NAME + --format=json (line 619 slice 3) ──

#[test]
fn test_explain_class_text_renders_description() {
    // `karac explain --class=TYPE_MISMATCH` (text mode) renders a
    // catalogue entry with the class name as a header and the prose
    // description below. Pin the class name and a recognisable
    // fragment of the description so a copyedit can't silently drop
    // the header structure.
    let out = karac_bin()
        .args(["explain", "--class=TYPE_MISMATCH"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "explain --class=TYPE_MISMATCH should exit 0; stderr was: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("TYPE_MISMATCH"));
    assert!(
        stdout.contains("expected slot"),
        "description body should appear; got: {}",
        stdout
    );
}

#[test]
fn test_explain_class_json_emits_envelope() {
    // `karac explain --class=INVALID_CAST --format=json` returns a
    // single JSON record. Pin the envelope keys (`kind`, `class`,
    // `description`) and the class value; the description is
    // checked via a fragment to allow future copyedits.
    let out = karac_bin()
        .args(["explain", "--class=INVALID_CAST", "--format=json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"diagnostic_class\""),
        "JSON envelope should carry kind=diagnostic_class; got: {}",
        stdout
    );
    assert!(stdout.contains("\"class\":\"INVALID_CAST\""));
    assert!(stdout.contains("\"description\":"));
    // ptr.const / ptr.mut suggestions appear in the description.
    assert!(stdout.contains("ptr.const"));
}

#[test]
fn test_explain_class_json_escapes_quotes_and_newlines() {
    // The description for TYPE_MISMATCH contains apostrophes and
    // multiple sentences. JSON output must escape any embedded `"` /
    // `\` / control chars; this test verifies the envelope is
    // single-line and parseable shape (one record per call).
    let out = karac_bin()
        .args(["explain", "--class=TYPE_MISMATCH", "--format=json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Single record => single line.
    let line_count = stdout.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        line_count, 1,
        "explain --class --format=json must emit exactly one record; got:\n{}",
        stdout
    );
    // No unescaped raw newlines or quotes in the value bodies (any
    // `"` between the outer object braces must be field delimiters
    // or escape sequences).
    assert!(!stdout.contains("\n\""));
}

#[test]
fn test_explain_concept_json_envelope_carries_body() {
    // `karac explain --concept=closures --format=json` emits a
    // `{ kind, concept, body }` record. The body is the same prose
    // the text mode renders, embedded as a JSON-escaped string.
    let out = karac_bin()
        .args(["explain", "--concept=closures", "--format=json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"kind\":\"concept\""));
    assert!(stdout.contains("\"concept\":\"closures\""));
    assert!(stdout.contains("\"body\":"));
}

#[test]
fn test_explain_rejects_unknown_class_with_supported_set() {
    let out = karac_bin()
        .args(["explain", "--class=NOT_A_REAL_CLASS"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown diagnostic class 'NOT_A_REAL_CLASS'"));
    // The supported-set message should list at least one real class.
    assert!(stderr.contains("TYPE_MISMATCH"));
}

#[test]
fn test_explain_rejects_unknown_format() {
    let out = karac_bin()
        .args(["explain", "--class=TYPE_MISMATCH", "--format=yaml"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown --format value 'yaml'"));
}

#[test]
fn test_explain_rejects_both_concept_and_class() {
    let out = karac_bin()
        .args(["explain", "--concept=closures", "--class=TYPE_MISMATCH"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("mutually exclusive"));
}

// ── Lint level CLI flags (-A/-W/-D/-F + -D warnings, slice 4b polish) ──

/// Build a temp `.kara` file with a function whose `match` body has
/// duplicate arms (fires `unreachable_arm`, default-`Warn`). The
/// `-A`/`-W`/`-D`/`-F` flag tests use this fixture so they exercise
/// the end-to-end CLI → typecheck path against a real diagnostic.
fn write_unreachable_arm_fixture(suffix: &str) -> std::path::PathBuf {
    let src = "enum Color { Red, Green, Blue }\n\
               fn name(c: Color) -> i64 {\n\
                   match c {\n\
                       Red   => 1,\n\
                       Red   => 2,\n\
                       Green => 3,\n\
                       Blue  => 4,\n\
                   }\n\
               }\n\
               fn main() {}\n";
    let path = std::env::temp_dir().join(format!(
        "karac-lintcli-{suffix}-{}-{}.kara",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn test_lint_cli_deny_promotes_unreachable_arm_to_error() {
    let path = write_unreachable_arm_fixture("deny-unreachable");
    let out = karac_bin()
        .args(["check", "-D", "unreachable_arm", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !out.status.success(),
        "`-D unreachable_arm` should promote the warning to an error and exit non-zero",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error") && stderr.contains("unreachable"),
        "expected an unreachable_arm error in stderr; got: {stderr}",
    );
}

#[test]
fn test_lint_cli_allow_suppresses_unreachable_arm() {
    let path = write_unreachable_arm_fixture("allow-unreachable");
    let out = karac_bin()
        .args(["check", "-A", "unreachable_arm", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "`-A unreachable_arm` should suppress the warning and the check should pass",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_lowercase().contains("unreachable"),
        "suppressed lint must not surface in stderr; got: {stderr}",
    );
}

#[test]
fn test_lint_cli_deny_warnings_catch_all_promotes_unreachable() {
    let path = write_unreachable_arm_fixture("deny-warnings");
    let out = karac_bin()
        .args(["check", "-D", "warnings", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !out.status.success(),
        "`-D warnings` should promote every default-Warn lint and exit non-zero",
    );
}

#[test]
fn test_lint_cli_joined_form_deny_equals_name() {
    // `-D=NAME` joined form parses identically to `-D NAME`.
    let path = write_unreachable_arm_fixture("deny-joined");
    let out = karac_bin()
        .args(["check", "-D=unreachable_arm", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !out.status.success(),
        "`-D=unreachable_arm` (joined form) should promote to error",
    );
}

#[test]
fn test_lint_cli_forbid_rejects_inner_allow() {
    let src = "#[allow(unreachable_arm)]\nfn f() -> i64 { 0 }\nfn main() {}\n";
    let path = std::env::temp_dir().join(format!(
        "karac-lintcli-forbid-{}-{}.kara",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["check", "-F", "unreachable_arm", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        !out.status.success(),
        "`-F unreachable_arm` + inner #[allow(unreachable_arm)] should error",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("E_FORBIDDEN_LINT_ALLOW"),
        "expected E_FORBIDDEN_LINT_ALLOW in stderr; got: {stderr}",
    );
}

#[test]
fn test_lint_cli_missing_arg_fails() {
    // Bare `-D` with no following arg should error and exit non-zero.
    let path = write_unreachable_arm_fixture("missing-arg");
    let out = karac_bin().args(["check", "-D"]).output().unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success(), "bare `-D` with no NAME should error",);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("requires a lint name"),
        "expected explanatory error; got: {stderr}",
    );
}

#[test]
fn test_lint_cli_allow_via_build_subcommand() {
    // The flag plumbing covers `build` too — exercise the path even
    // without --features llvm by relying on `cargo test`'s build of
    // the binary (which is built once per cargo invocation). On no-llvm
    // builds, `cmd_build` falls through to `cmd_check`, which still
    // consults the lint overrides — so the assertion is the same.
    let path = write_unreachable_arm_fixture("build-allow");
    // `karac build` derives the output executable name from the
    // source file_stem and writes to CWD on the llvm-feature path.
    // Capture the would-be name so we can clean it up after the
    // test even if the assertion fires first.
    let exe_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let exe_in_cwd = std::path::PathBuf::from(if cfg!(windows) {
        format!("{exe_stem}.exe")
    } else {
        exe_stem.to_string()
    });
    let out = karac_bin()
        .args(["build", "-A", "unreachable_arm", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&exe_in_cwd);
    // Either the build succeeds (llvm feature, link works) or the
    // typecheck pass succeeds and link fails downstream. Either way
    // stderr must not surface the suppressed unreachable_arm
    // warning.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.to_lowercase().contains("unreachable"),
        "suppressed lint must not surface under `karac build -A`; got: {stderr}",
    );
}

// ── karac catalog ───────────────────────────────────────────────

fn catalog_tmp_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "karac-cli-catalog-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[test]
fn test_subcommand_help_catalog() {
    let out = karac_bin().args(["catalog", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac catalog"));
    assert!(stdout.contains("public API surface"));
    assert!(stdout.contains("JSONL"));
}

#[test]
fn test_main_help_lists_catalog_command() {
    let out = karac_bin().arg("help").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("catalog <file>"),
        "top-level help should advertise the catalog subcommand; got: {stdout}",
    );
}

#[test]
fn test_catalog_emits_one_record_per_public_item() {
    let path = catalog_tmp_path("multi");
    let src = r#"pub fn add(x: i64, y: i64) -> i64 { x + y }
pub struct Point { pub x: f64, pub y: f64 }
pub enum Color { Red, Green, Blue }
pub const TAU: f64 = 6.2831853;
fn internal() -> i64 { 0 }
"#;
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["catalog", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "karac catalog should exit 0 on a clean file; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "expected 4 records, got: {stdout}");
    assert!(lines[0].contains("\"kind\":\"fn\""), "got: {}", lines[0]);
    assert!(lines[0].contains("\"name\":\"add\""), "got: {}", lines[0]);
    assert!(
        lines[1].contains("\"kind\":\"struct\""),
        "got: {}",
        lines[1]
    );
    assert!(lines[2].contains("\"kind\":\"enum\""), "got: {}", lines[2]);
    assert!(lines[3].contains("\"kind\":\"const\""), "got: {}", lines[3]);
    // Private item omitted.
    assert!(
        !stdout.contains("\"name\":\"internal\""),
        "non-pub item must not appear in catalog; got: {stdout}",
    );
}

#[test]
fn test_catalog_each_line_is_valid_json_envelope() {
    let path = catalog_tmp_path("jsonl");
    let src = r#"pub fn f(x: ref Vec[i64]) -> i64 with reads(Time) { 0 }
"#;
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["catalog", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    // Structural shape — opens with `{`, closes with `}`, has the
    // anchor fields and ends in a newline.
    assert!(stdout.ends_with('\n'), "JSONL stream must end with \\n");
    assert!(line.starts_with('{') && line.ends_with('}'), "got: {line}");
    assert!(line.contains("\"mode\":\"ref\""), "got: {line}");
    assert!(line.contains("\"ty\":\"Vec[i64]\""), "got: {line}");
    assert!(
        line.contains("\"verb\":\"reads\",\"resources\":[\"Time\"]"),
        "got: {line}",
    );
}

#[test]
fn test_catalog_qualifies_impl_methods_with_target_type() {
    let path = catalog_tmp_path("impl");
    let src = r#"pub struct Point { pub x: f64, pub y: f64 }
impl Point {
    pub fn origin() -> Point { Point { x: 0.0, y: 0.0 } }
    fn private_helper() -> i64 { 0 }
}
"#;
    std::fs::write(&path, src).unwrap();
    let out = karac_bin()
        .args(["catalog", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"kind\":\"impl_method\""), "got: {stdout}",);
    assert!(
        stdout.contains("\"name\":\"Point.origin\""),
        "got: {stdout}",
    );
    assert!(
        !stdout.contains("\"name\":\"Point.private_helper\""),
        "private impl method must be omitted; got: {stdout}",
    );
}

#[test]
fn test_catalog_empty_file_emits_no_output() {
    let path = catalog_tmp_path("empty");
    std::fs::write(&path, "").unwrap();
    let out = karac_bin()
        .args(["catalog", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.is_empty(),
        "empty source must produce zero records; got: {stdout}",
    );
}

#[test]
fn test_catalog_unknown_flag_rejected() {
    let path = catalog_tmp_path("flag");
    std::fs::write(&path, "pub fn f() {}\n").unwrap();
    let out = karac_bin()
        .args(["catalog", "--bogus", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown flag"), "got: {stderr}");
}

#[test]
fn test_catalog_missing_file_arg_rejected() {
    let out = karac_bin().args(["catalog"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("missing file argument"), "got: {stderr}");
}

// ── karac query affected-by ─────────────────────────────────────

fn affected_by_tmp_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "karac-cli-affected-by-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

#[test]
fn test_affected_by_emits_envelope_with_callers_and_callees() {
    let path = affected_by_tmp_path("simple");
    let src =
        "fn leaf() -> i64 { 0 }\nfn middle() -> i64 { leaf() }\nfn top() -> i64 { middle() }\n";
    std::fs::write(&path, src).unwrap();
    let target = format!("{}:middle", path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "affected-by", &target])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "karac query affected-by should exit 0; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(line.starts_with('{') && line.ends_with('}'), "got: {line}");
    assert!(line.contains("\"type\":\"affected_by\""), "got: {line}");
    assert!(line.contains("\"input\":\"middle\""), "got: {line}");
    assert!(line.contains("\"fn\":\"top\""), "got: {line}");
    assert!(line.contains("\"fn\":\"leaf\""), "got: {line}");
}

#[test]
fn test_affected_by_direction_callees_suppresses_callers() {
    let path = affected_by_tmp_path("direction");
    let src =
        "fn leaf() -> i64 { 0 }\nfn middle() -> i64 { leaf() }\nfn top() -> i64 { middle() }\n";
    std::fs::write(&path, src).unwrap();
    let target = format!("{}:top", path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "affected-by", &target, "--direction=callees"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(
        !line.contains("\"callers\""),
        "callers must be omitted; got: {line}"
    );
    assert!(
        !line.contains("\"tests\""),
        "tests must be omitted under callees; got: {line}"
    );
    assert!(line.contains("\"callees\":["), "got: {line}");
    assert!(line.contains("\"fn\":\"leaf\""), "got: {line}");
    assert!(line.contains("\"fn\":\"middle\""), "got: {line}");
}

#[test]
fn test_affected_by_tests_only_filters_to_test_fns() {
    let parent = std::env::temp_dir();
    let unique = format!(
        "karac-cli-affby-tests-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let test_path = parent.join(format!("{unique}_test.kara"));
    let src = "fn helper() -> i64 { 0 }\nfn test_helper_baseline() { let _ = helper(); }\nfn non_test_caller() -> i64 { helper() }\n";
    std::fs::write(&test_path, src).unwrap();
    let target = format!("{}:helper", test_path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "affected-by", &target, "--tests-only"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&test_path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(!line.contains("\"callers\""), "got: {line}");
    assert!(!line.contains("\"callees\""), "got: {line}");
    assert!(line.contains("\"tests\":["), "got: {line}");
    assert!(
        line.contains("\"fn\":\"test_helper_baseline\""),
        "got: {line}"
    );
    assert!(
        !line.contains("\"fn\":\"non_test_caller\""),
        "non-test caller must not appear in --tests-only output; got: {line}",
    );
}

#[test]
fn test_affected_by_file_target_unions_per_seed_reach() {
    let path = affected_by_tmp_path("file_target");
    let src = "fn root_a() -> i64 { 1 }\nfn root_b() -> i64 { 2 }\nfn user_a() -> i64 { root_a() }\nfn user_b() -> i64 { root_b() }\n";
    std::fs::write(&path, src).unwrap();
    let target = path.to_str().unwrap().to_string();
    let out = karac_bin()
        .args(["query", "affected-by", &target])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(line.contains("\"fn\":\"user_a\""), "got: {line}");
    assert!(line.contains("\"fn\":\"user_b\""), "got: {line}");
}

#[test]
fn test_affected_by_file_range_filters_by_line() {
    let path = affected_by_tmp_path("range");
    let src =
        "fn alpha() -> i64 { 1 }\nfn beta() -> i64 { alpha() }\nfn gamma() -> i64 { beta() }\n";
    std::fs::write(&path, src).unwrap();
    let target = format!("{}:2-2", path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "affected-by", &target])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout.trim();
    assert!(line.contains("\"fn\":\"gamma\""), "got: {line}");
    assert!(line.contains("\"fn\":\"alpha\""), "got: {line}");
}

#[test]
fn test_affected_by_unknown_direction_rejected() {
    let path = affected_by_tmp_path("bad_dir");
    std::fs::write(&path, "fn f() {}\n").unwrap();
    let target = format!("{}:f", path.to_str().unwrap());
    let out = karac_bin()
        .args(["query", "affected-by", &target, "--direction=sideways"])
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown --direction"), "got: {stderr}");
}

#[test]
fn test_subcommand_help_query_advertises_affected_by() {
    let out = karac_bin().args(["query", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("affected-by"), "got: {stdout}");
    assert!(stdout.contains("--tests-only"), "got: {stdout}");
    assert!(stdout.contains("--direction"), "got: {stdout}");
}

// ── PubGrub resolver slice 7 — CLI wiring ─────────────────────────

/// Helper: build a unique tempdir for a slice-7 fixture.
fn slice7_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-slice7-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_slice7_registry_dep_warns_but_does_not_fail() {
    // A `[dependencies]` table containing only registry deps should NOT
    // break the build — slice 7 downgrades E_REGISTRY_DEP_UNSUPPORTED
    // to a warning so existing projects with registry-style manifests
    // continue to compile until line 819's fetch surface ships.
    let tmp = slice7_tempdir("registry-warn");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "proj"

[dependencies]
http = "1.2"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    // Slice 4 activates registry fetch only when an explicit proxy is
    // configured. Scrub `KARAC_REGISTRY_PROXY` so a developer's shell env
    // can't flip this project onto the fetch path — with no explicit proxy
    // the registry dep must still warn-and-continue (the pre-fetch contract).
    let out = karac_bin()
        .arg("build")
        .env_remove("KARAC_REGISTRY_PROXY")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        stderr.contains("warning[E_REGISTRY_DEP_UNSUPPORTED]"),
        "expected warning for registry dep; stderr={stderr}",
    );
    // Build doesn't have to succeed (LLVM gating may skip codegen) but
    // the resolver should not have halted it. The error-line invariant
    // is that no `error[E_*]` from the resolver appears.
    assert!(
        !stderr.contains("error[E_REGISTRY_DEP_UNSUPPORTED]"),
        "registry dep should warn, not error; stderr={stderr}",
    );
}

#[test]
fn test_slice7_msrv_too_old_fails_build() {
    // A `[package].kara-version` constraint that excludes the active
    // toolchain version is a hard error — the user can't proceed without
    // upgrading or relaxing the constraint.
    let tmp = slice7_tempdir("msrv-fail");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "proj"
kara-version = ">=999.0.0"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "MSRV-too-old should halt the build; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_TOOLCHAIN_TOO_OLD]"),
        "expected E_TOOLCHAIN_TOO_OLD; stderr={stderr}",
    );
    assert!(
        stderr.contains("999.0.0"),
        "diagnostic should name the declared constraint; stderr={stderr}",
    );
}

#[test]
fn test_slice7_path_dep_cycle_fails_build() {
    // Two path-deps mutually referencing each other — slice 3's cycle
    // detection fires through the FsLoader (the real fs walk, not the
    // in-memory MemLoader the dep_graph unit tests use).
    let tmp = slice7_tempdir("cycle");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("sub/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root"

[dependencies]
sub = { path = "sub" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    // sub points back at the root via `..`
    std::fs::write(
        tmp.join("sub/kara.toml"),
        r#"[package]
name = "sub"

[dependencies]
root = { path = ".." }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("sub/src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "dependency cycle should halt the build; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_DEPENDENCY_CYCLE]"),
        "expected E_DEPENDENCY_CYCLE; stderr={stderr}",
    );
    assert!(
        stderr.contains(" → "),
        "chain should render with arrow separators; stderr={stderr}"
    );
}

#[test]
fn test_slice7_no_deps_no_msrv_skips_resolver() {
    // Regression pin: a project with neither `[dependencies]` nor
    // `kara-version` should not pay for resolver invocation. We verify
    // by asserting no resolver-emit warnings or errors appear in
    // stderr — only the existing build phases produce output.
    let tmp = slice7_tempdir("no-deps");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !stderr.contains("E_REGISTRY_DEP_UNSUPPORTED"),
        "no-dep project should not surface any resolver diagnostics; stderr={stderr}",
    );
    assert!(
        !stderr.contains("E_TOOLCHAIN_TOO_OLD"),
        "no-MSRV project should not surface MSRV diagnostics; stderr={stderr}",
    );
}

// ── karac update slice 1 — bare-form (re-resolve + rewrite lockfile) ─

fn update_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-update-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_update_bare_rewrites_lockfile_for_path_dep_project() {
    let tmp = update_tempdir("bare-path-dep");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .arg("update")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let contents = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac update bare should succeed; stderr={stderr}",
    );
    assert!(
        lockfile_exists,
        "karac update should produce kara.lock at the project root",
    );
    assert!(
        contents.contains("name = \"child\""),
        "lockfile should mention the path-dep child; got: {contents}",
    );
    assert!(
        stderr.contains("re-derived kara.lock"),
        "expected the update summary on stderr; got: {stderr}",
    );
    assert!(
        stderr.contains("(2 locked packages)"),
        "expected '2 locked packages' in the summary; got: {stderr}",
    );
}

#[test]
fn test_update_bare_runs_resolver_even_with_no_deps() {
    // Regression pin: cmd_build_project skips the resolver when no deps
    // are declared; cmd_update must NOT. The user explicitly asked to
    // refresh the lockfile — honoring that is the whole point.
    let tmp = update_tempdir("no-deps");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .arg("update")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac update on no-dep project should succeed; stderr={stderr}",
    );
    assert!(
        lockfile_exists,
        "karac update must produce kara.lock even when manifest declares no deps",
    );
}

#[test]
fn test_update_dep_graph_error_halts_with_diagnostic() {
    // A path-dep cycle halts cmd_update with the same E_DEPENDENCY_CYCLE
    // diagnostic that cmd_build_project uses.
    let tmp = update_tempdir("cycle-halts");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("sub/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root"

[dependencies]
sub = { path = "sub" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("sub/kara.toml"),
        r#"[package]
name = "sub"

[dependencies]
root = { path = ".." }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("sub/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .arg("update")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "karac update on a cycle should halt; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_DEPENDENCY_CYCLE]"),
        "expected E_DEPENDENCY_CYCLE; got: {stderr}",
    );
    assert!(
        !lockfile_exists,
        "kara.lock should not be written when the resolver fails",
    );
}

#[test]
fn test_update_help_works() {
    let out = karac_bin().args(["update", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("karac update"));
    assert!(stdout.contains("kara.lock"));
}

#[test]
fn test_update_rejects_extra_args() {
    let out = karac_bin().args(["update", "foo", "bar"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("takes at most one"),
        "expected too-many-args error; got: {stderr}",
    );
}

// ── karac vendor — real implementation (copies path-deps) ───────────

fn vendor_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-vendor-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_vendor_copies_path_dep_into_vendor_dir() {
    let tmp = vendor_tempdir("path-dep");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "libs/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let vendored_manifest = tmp.join("vendor/child/kara.toml");
    let vendored_src = tmp.join("vendor/child/src/lib.kara");
    let vendored_manifest_contents =
        std::fs::read_to_string(&vendored_manifest).unwrap_or_default();
    let vendored_src_contents = std::fs::read_to_string(&vendored_src).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac vendor should succeed; stderr={stderr}"
    );
    assert!(
        stderr.contains("copied 1 package"),
        "expected vendor summary; got: {stderr}",
    );
    assert!(
        vendored_manifest_contents.contains("name = \"child\""),
        "vendored manifest should match the source; got: {vendored_manifest_contents}",
    );
    assert!(
        vendored_src_contents.contains("fn dummy"),
        "vendored source file should match the source; got: {vendored_src_contents}",
    );
}

#[test]
fn test_vendor_is_idempotent_across_reruns() {
    let tmp = vendor_tempdir("idempotent");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "libs/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let _ = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let first = std::fs::read_to_string(tmp.join("vendor/child/kara.toml")).unwrap();

    // Edit the source manifest, then re-run vendor — the vendored copy
    // must pick up the new content.
    std::fs::write(
        tmp.join("libs/child/kara.toml"),
        r#"[package]
name = "child"
# Trailing comment differentiates the second run.
"#,
    )
    .unwrap();
    let out = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let second = std::fs::read_to_string(tmp.join("vendor/child/kara.toml")).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "second vendor should succeed; stderr={stderr}"
    );
    assert_ne!(
        first, second,
        "the second vendor run must refresh the vendored content",
    );
    assert!(
        second.contains("Trailing comment"),
        "the second vendor must capture the new content; got: {second}",
    );
}

#[test]
fn test_vendor_no_deps_project_succeeds() {
    let tmp = vendor_tempdir("no-deps");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let vendor_exists = tmp.join("vendor").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "no-dep vendor should succeed; stderr={stderr}"
    );
    assert!(
        stderr.contains("copied 0 packages"),
        "expected zero-copy summary; got: {stderr}",
    );
    assert!(
        !vendor_exists,
        "vendor/ should not be created when there are no path-deps to copy",
    );
}

#[test]
fn test_vendor_registry_dep_skipped_with_note() {
    let tmp = vendor_tempdir("reg-skip");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "proj"

[dependencies]
http = "1.2"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    // Registry deps downgrade to a resolver warning; vendor honors that
    // and walks the (empty) remaining resolution.
    assert!(
        out.status.success(),
        "registry-only vendor should not halt; stderr={stderr}",
    );
    assert!(
        stderr.contains("warning[E_REGISTRY_DEP_UNSUPPORTED]"),
        "expected the resolver warning to surface; got: {stderr}",
    );
}

// ── karac build --offline — vendor-only resolver wiring ─────────────

fn offline_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-offline-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_build_offline_no_deps_project_succeeds_without_vendor() {
    // Solo project + --offline: no deps, no vendor needed, no error. Pins
    // that the pre-check is gated on the manifest declaring at least one
    // dep so single-package operators aren't forced to run `karac vendor`.
    let tmp = offline_tempdir("no-deps");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--offline"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "no-dep offline build should succeed; stderr={stderr}"
    );
    assert!(
        !stderr.contains("E_OFFLINE_NO_VENDOR_DIR"),
        "no-dep project must not trip the missing-vendor pre-check; stderr={stderr}",
    );
}

#[test]
fn test_build_offline_missing_vendor_dir_errors() {
    // Manifest declares a path-dep but no vendor/ has been created. The
    // pre-check should surface E_OFFLINE_NO_VENDOR_DIR up front rather
    // than letting per-dep failures cascade.
    let tmp = offline_tempdir("no-vendor");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "libs/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--offline"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "offline build with no vendor/ must fail; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_OFFLINE_NO_VENDOR_DIR]"),
        "expected the missing-vendor-dir diagnostic; got: {stderr}",
    );
    assert!(
        stderr.contains("karac vendor"),
        "diagnostic help should point at `karac vendor`; got: {stderr}",
    );
}

#[test]
fn test_build_offline_consults_vendor_for_path_dep() {
    // The manifest declares `child` at `libs/child`, but vendor/ holds a
    // *different* manifest at vendor/child/. The offline build must pick
    // up the vendored copy, proving the redirect — we verify by deleting
    // the manifest-declared source after vendoring.
    let tmp = offline_tempdir("consults-vendor");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "libs/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    // Vendor + then delete the source — vendor/ is the only available copy.
    let vendor = karac_bin()
        .arg("vendor")
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(
        vendor.status.success(),
        "vendor must succeed before the offline test runs; stderr={}",
        String::from_utf8_lossy(&vendor.stderr)
    );
    std::fs::remove_dir_all(tmp.join("libs")).unwrap();

    let out = karac_bin()
        .args(["build", "--offline"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "offline build should consult vendor/ even when the declared path is gone; stderr={stderr}",
    );
    assert!(
        lockfile_exists,
        "successful offline resolution must persist kara.lock; stderr={stderr}",
    );
}

#[test]
fn test_build_offline_path_dep_missing_from_vendor_errors() {
    // Vendor exists but doesn't contain `child` — the per-dep diagnostic
    // (E_OFFLINE_VENDOR_ENTRY_MISSING) should fire with the dep name.
    let tmp = offline_tempdir("vendor-entry-missing");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor")).unwrap(); // empty vendor
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "libs/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--offline"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "missing vendor entry must fail the offline build; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_OFFLINE_VENDOR_ENTRY_MISSING]"),
        "expected the per-dep offline diagnostic; got: {stderr}",
    );
    assert!(
        stderr.contains("child"),
        "diagnostic should name the missing dep; got: {stderr}",
    );
}

#[test]
fn test_build_offline_registry_dep_is_hard_error() {
    // Outside offline mode, a registry dep downgrades to a warning so the
    // build proceeds with the path-dep half resolved. In offline mode the
    // same diagnostic is fatal — vendor-of-registry-deps lands alongside
    // line 845, so until then registry deps cannot satisfy offline builds.
    let tmp = offline_tempdir("registry-fatal");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "proj"

[dependencies]
http = "1.2"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--offline"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "registry dep in offline mode must halt the build; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_REGISTRY_DEP_UNSUPPORTED]"),
        "expected registry-unsupported promoted to error in offline mode; got: {stderr}",
    );
}

#[test]
fn test_build_offline_suppresses_redundant_no_proxy_note() {
    // --offline implies --no-proxy at the contract level. The redundant
    // no-proxy note must not appear when both flags are set together.
    let tmp = offline_tempdir("no-proxy-suppress");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--offline", "--no-proxy"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "offline + no-proxy on a solo project should succeed; stderr={stderr}",
    );
    assert!(
        !stderr.contains("--no-proxy active"),
        "the no-proxy note should be suppressed under --offline; got: {stderr}",
    );
}

// ── tracker line 898: karac run script-dir manifest discovery ────

fn run_dir_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-rundir-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_run_script_walks_from_script_dir_not_cwd() {
    // The script lives in tmp/proj/foo.kara; karac-toolchain.toml lives
    // in tmp/proj/. Run karac from tmp/ (NOT proj/) — discovery must
    // still find the pin via the script-dir walk.
    let tmp = run_dir_tempdir("walks-from-script");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(proj.join("foo.kara"), "fn main() {}\n").unwrap();
    std::fs::write(proj.join("karac-toolchain.toml"), "version = \">=99.0\"\n").unwrap();

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["run", "proj/foo.kara"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "run should honor pin discovered via script-dir walk; stderr={stderr}",
    );
    assert!(
        stderr.contains("E_TOOLCHAIN_VERSION_MISMATCH"),
        "expected toolchain mismatch surfaced via script-dir walk; got: {stderr}",
    );
}

#[test]
fn test_run_no_manifest_skips_discovery() {
    // Same setup as the previous test, but with --no-manifest, the
    // pin must be ignored and the script runs.
    let tmp = run_dir_tempdir("no-manifest");
    let proj = tmp.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(proj.join("foo.kara"), "fn main() {}\n").unwrap();
    std::fs::write(proj.join("karac-toolchain.toml"), "version = \">=99.0\"\n").unwrap();

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["run", "--no-manifest", "proj/foo.kara"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "--no-manifest must skip pin enforcement; stderr={stderr}",
    );
    assert!(
        !stderr.contains("E_TOOLCHAIN_VERSION_MISMATCH"),
        "pin must not fire under --no-manifest; got: {stderr}",
    );
}

#[test]
fn test_run_manifest_override_loads_explicit_file() {
    // --manifest=<path> loads the supplied manifest. A manifest with
    // an invalid kara-version constraint must surface during parse,
    // proving the override is consulted.
    let tmp = run_dir_tempdir("manifest-override");
    std::fs::write(tmp.join("foo.kara"), "fn main() {}\n").unwrap();
    let bad_manifest = tmp.join("custom.toml");
    // Wrong-type kara-version is a hard parse error in manifest.rs.
    std::fs::write(
        &bad_manifest,
        "[package]\nname = \"demo\"\nkara-version = 42\n",
    )
    .unwrap();

    let out = karac_bin()
        .current_dir(&tmp)
        .args([
            "run",
            "--manifest",
            bad_manifest.to_str().unwrap(),
            "foo.kara",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "manifest-override should surface manifest parse error; stderr={stderr}",
    );
    assert!(
        stderr.contains("kara-version"),
        "expected the manifest parse error to name kara-version; got: {stderr}",
    );
}

#[test]
fn test_run_manifest_and_no_manifest_are_mutually_exclusive() {
    let out = karac_bin()
        .args(["run", "--manifest=x.toml", "--no-manifest", "foo.kara"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("mutually exclusive"),
        "expected mutually-exclusive diagnostic; got: {stderr}",
    );
}

#[test]
fn test_run_script_outside_project_runs_stdlib_only() {
    // Script lives in an isolated tempdir with no ancestor manifest.
    // Run should succeed silently — no manifest-related output.
    let tmp = run_dir_tempdir("no-ancestor-manifest");
    std::fs::write(tmp.join("foo.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .current_dir(&tmp)
        .args(["run", "foo.kara"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "stdlib-only script outside any project should run; stderr={stderr}",
    );
}

#[test]
fn test_run_subcommand_help_documents_manifest_flags() {
    let out = karac_bin().args(["run", "--help"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--manifest=<path>"));
    assert!(stdout.contains("--no-manifest"));
    assert!(stdout.contains("MANIFEST DISCOVERY:"));
}

// ── tracker line 892: karac-toolchain.toml reader ───────────────

fn toolchain_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-toolchain-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_toolchain_pin_matches_active_succeeds() {
    // karac's active version is 0.1.0 (CARGO_PKG_VERSION). A wildcard
    // pin must match — the build succeeds.
    let tmp = toolchain_tempdir("matches");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.join("karac-toolchain.toml"), "version = \"*\"\n").unwrap();
    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "wildcard pin should match active toolchain; stderr={stderr}",
    );
    assert!(
        !stderr.contains("E_TOOLCHAIN_"),
        "no toolchain error expected; got: {stderr}",
    );
}

#[test]
fn test_toolchain_pin_mismatch_halts_build() {
    // Pin requires version 99.x which can't possibly match 0.1.0.
    // Build halts with E_TOOLCHAIN_VERSION_MISMATCH and a karaup hint.
    let tmp = toolchain_tempdir("mismatch");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.join("karac-toolchain.toml"), "version = \">=99.0\"\n").unwrap();
    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "pin should halt build on mismatch; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_TOOLCHAIN_VERSION_MISMATCH]"),
        "expected the version-mismatch diagnostic; got: {stderr}",
    );
    assert!(
        stderr.contains("karaup install"),
        "diagnostic should hint at karaup; got: {stderr}",
    );
}

#[test]
fn test_toolchain_pin_missing_version_field_is_hard_error() {
    let tmp = toolchain_tempdir("missing-version");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("karac-toolchain.toml"),
        "targets = [\"x86_64-apple-darwin\"]\n",
    )
    .unwrap();
    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "missing-version pin should halt; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_TOOLCHAIN_MISSING_VERSION]"),
        "expected missing-version diagnostic; got: {stderr}",
    );
}

#[test]
fn test_toolchain_pin_absent_file_is_noop() {
    // No karac-toolchain.toml — build proceeds without comment.
    let tmp = toolchain_tempdir("absent");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    let out = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "no pin file → build succeeds silently; stderr={stderr}",
    );
    assert!(
        !stderr.contains("toolchain"),
        "no toolchain-related output expected; got: {stderr}",
    );
}

#[test]
fn test_toolchain_pin_in_ancestor_is_honored() {
    // The pin lives one directory up — discovery should walk into it.
    let tmp = toolchain_tempdir("ancestor");
    let proj = tmp.join("subproject");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(proj.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(proj.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(tmp.join("karac-toolchain.toml"), "version = \">=99.0\"\n").unwrap();
    let out = karac_bin()
        .current_dir(&proj)
        .arg("build")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        !out.status.success(),
        "ancestor pin should still gate the build; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_TOOLCHAIN_VERSION_MISMATCH]"),
        "expected ancestor-pin to surface the mismatch; got: {stderr}",
    );
}

// ── karac build --target — [target.X.*] overlay merge ───────────────

fn target_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-target-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_build_target_flag_activates_overlay_dep() {
    // The base manifest declares no deps; `[target.X.dependencies]` adds
    // a path-dep keyed on a specific triple. Building with --target=X
    // must walk the overlay dep; without the flag, the overlay is inert
    // and the build is a solo project.
    let tmp = target_tempdir("overlay-activates");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/mac-only/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[target."x86_64-apple-darwin".dependencies]
mac-only = { path = "libs/mac-only" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/mac-only/kara.toml"),
        r#"[package]
name = "mac-only"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/mac-only/src/lib.kara"), "fn dummy() {}\n").unwrap();

    // With --target=x86_64-apple-darwin, the overlay activates — the
    // lockfile must mention the mac-only entry.
    let out = karac_bin()
        .args(["build", "--target=x86_64-apple-darwin"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lock_with_target = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    assert!(
        out.status.success(),
        "build with --target=mac should succeed; stderr={stderr}",
    );
    assert!(
        lock_with_target.contains("mac-only"),
        "lockfile must include mac-only under active target; got: {lock_with_target}",
    );
    let _ = std::fs::remove_file(tmp.join("kara.lock"));

    // With --target=x86_64-unknown-linux-gnu, the overlay does not
    // activate, the manifest declares no base deps, so no dep
    // resolution runs and no lockfile is produced.
    let out2 = karac_bin()
        .args(["build", "--target=x86_64-unknown-linux-gnu"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr2 = String::from_utf8_lossy(&out2.stderr);
    let lock_after_other = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out2.status.success(),
        "build with --target=linux should succeed (no deps to resolve); stderr={stderr2}",
    );
    assert!(
        !lock_after_other.contains("mac-only"),
        "lockfile must NOT include mac-only under inactive target; got: {lock_after_other}",
    );
}

#[test]
fn test_build_target_space_separated_form_parses() {
    // The `--target <triple>` (space) form must parse identically to
    // `--target=<triple>` (equals). Pins both surface shapes.
    let tmp = target_tempdir("space-separated");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--target", "x86_64-apple-darwin"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "space-separated --target should parse; stderr={stderr}",
    );
}

#[test]
fn test_build_target_empty_value_diagnostic() {
    // Empty triple value (`--target=`) must error up front so a typo
    // can't silently fall back to the host triple.
    let out = karac_bin().args(["build", "--target="]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "empty --target value must fail; stderr={stderr}",
    );
    assert!(
        stderr.contains("--target requires a non-empty target triple"),
        "expected the empty-triple diagnostic; got: {stderr}",
    );
}

#[test]
fn test_build_target_from_manifest_build_section() {
    // With no --target flag, `[build].target` from the manifest selects
    // the active triple. Pins that the manifest fallback is honored.
    let tmp = target_tempdir("build-default");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/mac-only/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[build]
target = "x86_64-apple-darwin"

[target."x86_64-apple-darwin".dependencies]
mac-only = { path = "libs/mac-only" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/mac-only/kara.toml"),
        r#"[package]
name = "mac-only"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("libs/mac-only/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .args(["build"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lock = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "build with [build].target = ... should succeed; stderr={stderr}",
    );
    assert!(
        lock.contains("mac-only"),
        "lockfile must include the manifest-default-target overlay; got: {lock}",
    );
}

#[test]
fn test_build_target_flag_overrides_manifest_default() {
    // --target on the CLI must beat `[build].target` in the manifest.
    // Set them to different triples; only one overlay should activate.
    let tmp = target_tempdir("cli-overrides-manifest");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/mac-only/src")).unwrap();
    std::fs::create_dir_all(tmp.join("libs/linux-only/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[build]
target = "x86_64-apple-darwin"

[target."x86_64-apple-darwin".dependencies]
mac-only = { path = "libs/mac-only" }

[target."x86_64-unknown-linux-gnu".dependencies]
linux-only = { path = "libs/linux-only" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/mac-only/kara.toml"),
        "[package]\nname = \"mac-only\"\n",
    )
    .unwrap();
    std::fs::write(tmp.join("libs/mac-only/src/lib.kara"), "fn dummy() {}\n").unwrap();
    std::fs::write(
        tmp.join("libs/linux-only/kara.toml"),
        "[package]\nname = \"linux-only\"\n",
    )
    .unwrap();
    std::fs::write(tmp.join("libs/linux-only/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let out = karac_bin()
        .args(["build", "--target=x86_64-unknown-linux-gnu"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lock = std::fs::read_to_string(tmp.join("kara.lock")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "build with CLI-override target should succeed; stderr={stderr}",
    );
    assert!(
        lock.contains("linux-only"),
        "lockfile must include CLI-target overlay (linux-only); got: {lock}",
    );
    assert!(
        !lock.contains("mac-only"),
        "lockfile must NOT include the manifest-default-target overlay (mac-only); got: {lock}",
    );
}

// ── karac update slice 2 — surgical <pkg> validation ────────────────

fn make_path_dep_project(slug: &str) -> std::path::PathBuf {
    let tmp = update_tempdir(slug);
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();
    tmp
}

#[test]
fn test_update_pkg_matches_path_dep_emits_note_and_rewrites() {
    let tmp = make_path_dep_project("pkg-path-dep");
    let out = karac_bin()
        .args(["update", "child"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac update child should succeed on a path-dep project; stderr={stderr}",
    );
    assert!(
        stderr.contains("note: `child` is a path-dep"),
        "expected the path-dep informational note; got: {stderr}",
    );
    assert!(
        stderr.contains("re-derived kara.lock"),
        "summary line should still be emitted; got: {stderr}",
    );
    assert!(
        lockfile_exists,
        "kara.lock should be written after a successful surgical update",
    );
}

// ── karac resolve (read-only graph inspection, follow-up (j)) ────────

#[test]
fn test_resolve_lists_path_dep_graph_and_writes_no_lockfile() {
    let tmp = make_path_dep_project("resolve-path");
    let out = karac_bin()
        .arg("resolve")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac resolve should succeed on a path-dep project; stderr={stderr}",
    );
    // Both the root and the path-dep appear in the graph.
    assert!(
        stderr.contains("root-pkg") && stderr.contains("child"),
        "resolve should list both root and the path-dep;\nstderr={stderr}",
    );
    // The path-dep's source is rendered as a path.
    assert!(
        stderr.contains("(path "),
        "resolve should render the dep's source kind;\nstderr={stderr}",
    );
    // The declared_by edge attributes the child to the root package.
    assert!(
        stderr.contains("<- root-pkg"),
        "resolve should show which parent declared the dep;\nstderr={stderr}",
    );
    // Read-only: unlike `karac update`, resolve must not rewrite kara.lock.
    assert!(
        !lockfile_exists,
        "karac resolve must not write kara.lock (it is read-only)",
    );
}

#[test]
fn test_resolve_output_json_shape() {
    let tmp = make_path_dep_project("resolve-json");
    let out = karac_bin()
        .args(["resolve", "--output=json"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);

    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("expected a JSON envelope on stdout;\nstdout={stdout}"));
    let v: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("stdout line is not valid JSON ({e});\nline={line}"));

    assert_eq!(v["status"], "ok", "envelope status;\n{v}");
    assert_eq!(v["command"], "resolve", "envelope command;\n{v}");
    let pkgs = v["packages"]
        .as_array()
        .unwrap_or_else(|| panic!("packages must be an array;\n{v}"));
    // root-pkg + child.
    assert_eq!(pkgs.len(), 2, "expected two resolved packages;\n{v}");
    // The child entry carries name/version/source and a declared_by edge back
    // to the root.
    let child = pkgs
        .iter()
        .find(|p| p["name"] == "child")
        .unwrap_or_else(|| panic!("child package missing from envelope;\n{v}"));
    assert_eq!(child["source"], "path", "child source kind;\n{child}");
    assert!(
        child["version"].is_string(),
        "each package carries a pinned version;\n{child}"
    );
    let edges = child["declared_by"]
        .as_array()
        .unwrap_or_else(|| panic!("declared_by must be an array;\n{child}"));
    assert!(
        edges.iter().any(|e| e["parent"] == "root-pkg"),
        "child must be declared_by root-pkg;\n{child}"
    );
}

#[test]
fn test_resolve_solo_project_lists_only_root() {
    // A project with no dependencies resolves to just its own root package
    // and exits cleanly — no deps, no lockfile.
    let tmp = update_tempdir("resolve-solo");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"solo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .arg("resolve")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac resolve should succeed on a solo project; stderr={stderr}",
    );
    assert!(
        stderr.contains("solo"),
        "resolve should list the root package;\nstderr={stderr}",
    );
    assert!(!lockfile_exists, "karac resolve must not write kara.lock",);
}

#[test]
fn test_resolve_rejects_positional_argument() {
    let tmp = update_tempdir("resolve-badarg");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"solo\"\n").unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin()
        .args(["resolve", "somepkg"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(!out.status.success(), "a positional arg must be rejected");
    assert!(
        stderr.contains("no positional arguments"),
        "expected the positional-argument rejection;\nstderr={stderr}",
    );
}

#[test]
fn test_update_pkg_unknown_errors_with_suggestion() {
    let tmp = make_path_dep_project("pkg-unknown");
    let out = karac_bin()
        .args(["update", "chld"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lockfile_exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "unknown package should halt the command; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_UPDATE_UNKNOWN_PACKAGE]"),
        "expected E_UPDATE_UNKNOWN_PACKAGE; got: {stderr}",
    );
    assert!(
        stderr.contains("did you mean `child`"),
        "expected typo suggestion pointing at `child`; got: {stderr}",
    );
    assert!(
        !lockfile_exists,
        "kara.lock must NOT be written when the surgical target is invalid",
    );
}

#[test]
fn test_update_pkg_root_errors() {
    let tmp = make_path_dep_project("pkg-root");
    let out = karac_bin()
        .args(["update", "root-pkg"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "updating the root package should halt; stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_UPDATE_ROOT_PACKAGE]"),
        "expected E_UPDATE_ROOT_PACKAGE; got: {stderr}",
    );
    assert!(
        stderr.contains("omit the positional"),
        "expected help line suggesting bare-form; got: {stderr}",
    );
}

#[test]
fn test_update_pkg_unknown_json_output() {
    let tmp = make_path_dep_project("pkg-unknown-json");
    let out = karac_bin()
        .args(["update", "nope", "--output=json"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !out.status.success(),
        "unknown package should halt; stdout={stdout}",
    );
    assert!(
        stdout.contains("E_UPDATE_UNKNOWN_PACKAGE"),
        "expected the code in JSON output; got: {stdout}",
    );
    assert!(
        stdout.contains("\"status\":\"error\""),
        "expected status:error in JSON output; got: {stdout}",
    );
}

// ── Lockfile slice 4 — kara.lock CLI integration ────────────────────

fn lockfile_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-lockfile-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

#[test]
fn test_lockfile_written_after_path_dep_resolve() {
    let tmp = lockfile_tempdir("written");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let lockfile_path = tmp.join("kara.lock");
    assert!(
        lockfile_path.exists(),
        "kara.lock should be written after a successful resolve",
    );
    let contents = std::fs::read_to_string(&lockfile_path).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        contents.contains("# This file is auto-generated by karac."),
        "lockfile should carry the header comment; got: {contents}",
    );
    assert!(
        contents.contains("version = 1"),
        "lockfile should declare schema version 1; got: {contents}",
    );
    assert!(
        contents.contains("name = \"root-pkg\""),
        "lockfile should mention the root package; got: {contents}",
    );
    assert!(
        contents.contains("name = \"child\""),
        "lockfile should mention the path-dep child; got: {contents}",
    );
    assert!(
        contents.contains("source = \"root\""),
        "root package should record source = \"root\"; got: {contents}",
    );
    assert!(
        contents.contains("path+"),
        "child package should record a path+ source; got: {contents}",
    );
    assert!(
        contents.contains("blake3:"),
        "content_hash field should be populated for path-deps; got: {contents}",
    );
}

#[test]
fn test_lockfile_byte_stable_across_rebuilds() {
    let tmp = lockfile_tempdir("stable");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let lockfile_path = tmp.join("kara.lock");
    let first = std::fs::read_to_string(&lockfile_path).unwrap();
    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let second = std::fs::read_to_string(&lockfile_path).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(
        first, second,
        "kara.lock should be byte-stable across rebuilds of an unchanged project",
    );
}

#[test]
fn test_lockfile_rewrites_when_path_dep_added() {
    let tmp = lockfile_tempdir("rewrite-add-dep");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"
kara-version = ">=0"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let lockfile_path = tmp.join("kara.lock");
    let initial = std::fs::read_to_string(&lockfile_path).unwrap();
    assert!(
        !initial.contains("name = \"child\""),
        "initial lockfile should not mention child; got: {initial}",
    );

    // Add a path-dep + materialize the target manifest, then rebuild.
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"
kara-version = ">=0"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let updated = std::fs::read_to_string(&lockfile_path).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert_ne!(
        initial, updated,
        "lockfile should be rewritten when manifest deps change",
    );
    assert!(
        updated.contains("name = \"child\""),
        "updated lockfile should mention the newly added child; got: {updated}",
    );
}

#[test]
fn test_lockfile_skipped_for_no_dep_project() {
    let tmp = lockfile_tempdir("skipped");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "solo"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let exists = tmp.join("kara.lock").exists();
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !exists,
        "no-dep, no-MSRV project should not produce a kara.lock",
    );
}

#[test]
fn test_lockfile_rewrites_when_child_manifest_changes() {
    let tmp = lockfile_tempdir("rewrite-child");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::create_dir_all(tmp.join("vendor/child/src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let first = std::fs::read_to_string(tmp.join("kara.lock")).unwrap();

    // Edit the child manifest so its content-hash changes.
    std::fs::write(
        tmp.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
# Trailing comment changes the content-hash.
"#,
    )
    .unwrap();

    let _ = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let second = std::fs::read_to_string(tmp.join("kara.lock")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);

    assert_ne!(
        first, second,
        "kara.lock should be rewritten when the child manifest's content-hash drifts",
    );
}

#[test]
fn test_slice7_workspace_dep_outside_workspace_fails() {
    // `workspace = true` on a dep where the manifest has no
    // [workspace.dependencies] table — fail loudly with the right code.
    let tmp = slice7_tempdir("ws-outside");
    std::fs::write(
        tmp.join("kara.toml"),
        r#"[package]
name = "proj"

[dependencies]
shared = { workspace = true }
"#,
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("src/main.kara"), "fn main() {}\n").unwrap();

    let out = karac_bin().arg("build").current_dir(&tmp).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(!out.status.success(), "should halt; stderr={stderr}");
    assert!(
        stderr.contains("error[E_WORKSPACE_DEP_OUTSIDE_WORKSPACE]"),
        "expected E_WORKSPACE_DEP_OUTSIDE_WORKSPACE; stderr={stderr}",
    );
}

// ── --no-proxy plumbing (slice 2 of phase-5 line 851) ────────────────
//
// The flag is parse-honored on `karac build`, `karac update`, and
// `karac vendor` today. A confirmation `note:` line is emitted when
// `--no-proxy` is set; absent the flag, no proxy-related text appears
// in stderr (the existing registry-dep-unsupported warning carries any
// status). v1.1.x lands the live HTTP fetch — until then the surface
// is the flag + the note so CI scripts can already pin against the
// final name.

fn no_proxy_tempdir(slug: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-no-proxy-{slug}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

fn write_no_proxy_path_dep_project(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("vendor/child/src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        r#"[package]
name = "root-pkg"

[dependencies]
child = { path = "vendor/child" }
"#,
    )
    .unwrap();
    std::fs::write(root.join("src/main.kara"), "fn main() {}\n").unwrap();
    std::fs::write(
        root.join("vendor/child/kara.toml"),
        r#"[package]
name = "child"
"#,
    )
    .unwrap();
    std::fs::write(root.join("vendor/child/src/lib.kara"), "fn dummy() {}\n").unwrap();
}

#[test]
fn test_no_proxy_flag_parses_on_build() {
    let tmp = no_proxy_tempdir("build-parse");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("build")
        .arg("--no-proxy")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        stderr.contains("--no-proxy active"),
        "expected the --no-proxy confirmation note; got: {stderr}",
    );
    assert!(
        stderr.contains("proxy.kara-lang.org"),
        "the note should mention the proxy URL; got: {stderr}",
    );
}

#[test]
fn test_no_proxy_flag_parses_on_update() {
    let tmp = no_proxy_tempdir("update-parse");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("update")
        .arg("--no-proxy")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac update --no-proxy should succeed on a path-dep project; stderr={stderr}",
    );
    assert!(
        stderr.contains("--no-proxy active"),
        "expected the --no-proxy confirmation note on update; got: {stderr}",
    );
}

#[test]
fn test_no_proxy_flag_parses_on_vendor() {
    let tmp = no_proxy_tempdir("vendor-parse");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("vendor")
        .arg("--no-proxy")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        out.status.success(),
        "karac vendor --no-proxy should succeed; stderr={stderr}",
    );
    assert!(
        stderr.contains("--no-proxy active"),
        "expected the --no-proxy confirmation note on vendor; got: {stderr}",
    );
}

#[test]
fn test_no_proxy_absent_does_not_emit_note() {
    // Regression pin: the proxy note must only fire when --no-proxy is
    // explicitly set. The default behavior is silent so existing CI
    // output doesn't churn.
    let tmp = no_proxy_tempdir("absent");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("update")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        !stderr.contains("--no-proxy"),
        "no proxy mention expected when flag absent; got: {stderr}",
    );
}

#[test]
fn test_no_proxy_env_var_overrides_url_in_note() {
    // Pin: when KARAC_REGISTRY_PROXY is set, the note must render the
    // override URL rather than the default. We assert the substring so
    // a future tweak to the surrounding sentence doesn't break.
    let tmp = no_proxy_tempdir("env-override");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("update")
        .arg("--no-proxy")
        .env("KARAC_REGISTRY_PROXY", "https://mirror.example.com")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(
        stderr.contains("https://mirror.example.com"),
        "expected the env-override URL in the note; got: {stderr}",
    );
    assert!(
        !stderr.contains("proxy.kara-lang.org"),
        "default URL should not appear when env var overrides; got: {stderr}",
    );
}

#[test]
fn test_vendor_rejects_unknown_flag() {
    // Slice 2 stiffens parse_vendor_command: the only recognized flag
    // is --no-proxy. An unknown --foo errors with a clear message.
    let tmp = no_proxy_tempdir("unknown-flag");
    write_no_proxy_path_dep_project(&tmp);

    let out = karac_bin()
        .arg("vendor")
        .arg("--made-up")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);

    assert!(!out.status.success(), "should reject unknown flag");
    assert!(
        stderr.contains("unknown flag '--made-up'"),
        "expected unknown-flag error; got: {stderr}",
    );
}

// ── karac cache ───────────────────────────────────────────────────
//
// Line 861 slice 2 — `karac cache info` and `karac cache key` lookups.
// The subcommand is never expected to mutate the cache; these tests
// verify the protocol surface (digest derivation, env-var override,
// stats reporting on populated vs empty caches, JSON envelope).

fn cache_tempdir(slug: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("karac-cache-test-{slug}"));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn test_cache_info_on_empty_cache_reports_zero() {
    // A cache root that doesn't exist yet should report 0 entries / 0
    // bytes and exit 0. This is the cold-machine case.
    let root = cache_tempdir("info-empty");
    let nonexistent = root.join("never-populated");
    let out = karac_bin()
        .args(["cache", "info"])
        .env("KARAC_BUILD_CACHE_ROOT", &nonexistent)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("karac cache info:"));
    assert!(stdout.contains("entries: 0"));
    assert!(stdout.contains("bytes:   0"));
    assert!(stdout.contains(nonexistent.to_str().unwrap()));
}

#[test]
fn test_cache_info_json_envelope() {
    // --output=json must produce the canonical envelope shape so
    // tooling (IDE, CI) can parse it without scraping text.
    let root = cache_tempdir("info-json");
    let out = karac_bin()
        .args(["cache", "info", "--output=json"])
        .env("KARAC_BUILD_CACHE_ROOT", &root)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&root);

    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("\"status\":\"ok\""));
    assert!(stdout.contains("\"command\":\"cache_info\""));
    assert!(stdout.contains("\"entries\":0"));
    assert!(stdout.contains("\"bytes\":0"));
}

#[test]
fn test_cache_key_default_axes_print_digest() {
    // `karac cache key --pkg foo --version 1.0.0` derives the digest
    // for the supplied pair against the active toolchain's defaults
    // (compiler version, host triple, edition `2026`, profile `default`).
    let out = karac_bin()
        .args(["cache", "key", "--pkg=foo", "--version=1.0.0"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("karac cache key:"));
    assert!(stdout.contains("pkg:              foo"));
    assert!(stdout.contains("version:          1.0.0"));
    assert!(stdout.contains("edition:          2026"));
    assert!(stdout.contains("profile:          default"));
    // The digest line must include a 64-hex string. We check for the
    // `digest:` label + the prefix; a full regex would be overkill.
    assert!(stdout.contains("digest:"));
    // Pull out the digest line and verify hex-shape.
    let digest_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("digest:"))
        .expect("expected a digest line");
    let hex = digest_line.trim().trim_start_matches("digest:").trim();
    assert_eq!(hex.len(), 64, "expected 64-hex digest, got `{hex}`");
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex, got `{hex}`"
    );
}

#[test]
fn test_cache_key_is_deterministic_across_invocations() {
    // Same five-tuple → same digest. Pin the host-axis defaults
    // explicitly so the test is hermetic against the machine running
    // it.
    let args = [
        "cache",
        "key",
        "--pkg=demo",
        "--version=1.2.3",
        "--edition=2026",
        "--profile=default",
        "--target-triple=aarch64-apple-darwin",
        "--compiler-version=0.1.0",
    ];
    let extract_digest = |stdout: &str| -> String {
        stdout
            .lines()
            .find(|l| l.trim_start().starts_with("digest:"))
            .expect("digest line")
            .trim()
            .trim_start_matches("digest:")
            .trim()
            .to_string()
    };
    let a = karac_bin().args(args).output().unwrap();
    let b = karac_bin().args(args).output().unwrap();
    let a_out = String::from_utf8_lossy(&a.stdout).to_string();
    let b_out = String::from_utf8_lossy(&b.stdout).to_string();
    let a_d = extract_digest(&a_out);
    let b_d = extract_digest(&b_out);
    assert_eq!(a_d, b_d, "digest must be deterministic");
}

#[test]
fn test_cache_key_axis_change_changes_digest() {
    // Changing any axis (here: edition) must produce a different
    // digest. Pins that the key derivation actually mixes the axis
    // through to the hash.
    let common = [
        "cache",
        "key",
        "--pkg=demo",
        "--version=1.2.3",
        "--profile=default",
        "--target-triple=aarch64-apple-darwin",
        "--compiler-version=0.1.0",
    ];
    let extract = |stdout: &str| -> String {
        stdout
            .lines()
            .find(|l| l.trim_start().starts_with("digest:"))
            .unwrap()
            .trim()
            .trim_start_matches("digest:")
            .trim()
            .to_string()
    };
    let mut a_args = common.to_vec();
    a_args.push("--edition=2026");
    let mut b_args = common.to_vec();
    b_args.push("--edition=2027");
    let a = karac_bin().args(&a_args).output().unwrap();
    let b = karac_bin().args(&b_args).output().unwrap();
    let a_d = extract(&String::from_utf8_lossy(&a.stdout));
    let b_d = extract(&String::from_utf8_lossy(&b.stdout));
    assert_ne!(a_d, b_d, "edition change must shift the digest");
}

#[test]
fn test_cache_key_json_envelope() {
    let out = karac_bin()
        .args([
            "cache",
            "key",
            "--pkg=demo",
            "--version=1.0.0",
            "--output=json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "stdout: {stdout}");
    assert!(stdout.contains("\"status\":\"ok\""));
    assert!(stdout.contains("\"command\":\"cache_key\""));
    assert!(stdout.contains("\"pkg\":\"demo\""));
    assert!(stdout.contains("\"version\":\"1.0.0\""));
    assert!(stdout.contains("\"digest\":\""));
}

#[test]
fn test_cache_key_requires_pkg() {
    let out = karac_bin()
        .args(["cache", "key", "--version=1.0.0"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("requires --pkg"),
        "expected `requires --pkg`, got: {stderr}"
    );
}

#[test]
fn test_cache_key_requires_version() {
    let out = karac_bin()
        .args(["cache", "key", "--pkg=demo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("requires --version"),
        "expected `requires --version`, got: {stderr}"
    );
}

#[test]
fn test_cache_requires_sub_mode() {
    // Bare `karac cache` should list the supported sub-modes rather
    // than fall through silently.
    let out = karac_bin().arg("cache").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("requires a sub-mode"),
        "expected sub-mode error, got: {stderr}"
    );
    assert!(stderr.contains("info"));
    assert!(stderr.contains("key"));
}

#[test]
fn test_cache_rejects_unknown_sub_mode() {
    let out = karac_bin().args(["cache", "purge"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown `karac cache` sub-mode 'purge'"),
        "expected unknown-sub-mode error, got: {stderr}"
    );
}

#[test]
fn test_cache_info_help_flag() {
    // Subcommand-scoped --help must surface the cache help block.
    let out = karac_bin().args(["cache", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success());
    assert!(stdout.contains("karac cache - Inspect the global build-artifact cache"));
    assert!(stdout.contains("SUB-MODES:"));
    assert!(stdout.contains("info"));
    assert!(stdout.contains("key"));
}

// ── E_CONCURRENT_SHARED_STRUCT / E_CONCURRENT_PLAIN_STRUCT JSON ─
//
// Phase-7 line 197 follow-up: the diagnostic JSON envelope carries a
// `fix_diff` array with the per-mut-field `Mutex[T]` wrap edits when
// the struct has any `mut` fields. Cross-checks the cli emitter wires
// the sibling `error_fix_diffs` map through correctly.

#[test]
fn test_json_concurrent_shared_struct_carries_fix_diff_array() {
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_l197_shared_fix_diff.kara");
    std::fs::write(
        &fixture,
        "shared struct Counter { val: i64, mut count: i64 }\n\
         fn use_a(c: Counter) { }\n\
         fn use_b(c: Counter) { }\n\
         fn main() {\n\
             let c = Counter { val: 0, count: 0 };\n\
             par {\n\
                 use_a(c);\n\
                 use_b(c);\n\
             }\n\
         }\n",
    )
    .expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_file(&fixture);
    assert!(!out.status.success());
    assert!(
        stdout.contains("E_CONCURRENT_SHARED_STRUCT"),
        "expected E_CONCURRENT_SHARED_STRUCT code in JSON; got: {stdout}",
    );
    assert!(
        stdout.contains("\"fix_diff\":["),
        "expected fix_diff array in JSON envelope; got: {stdout}",
    );
    assert!(
        stdout.contains("\"text\":\"Mutex[\""),
        "expected `Mutex[` prefix insertion edit; got: {stdout}",
    );
    assert!(
        stdout.contains("\"text\":\"]\""),
        "expected `]` suffix insertion edit; got: {stdout}",
    );
}

#[test]
fn test_json_concurrent_plain_struct_carries_fix_diff_array() {
    let tmp_dir = std::env::temp_dir();
    let fixture = tmp_dir.join("karac_l197_plain_fix_diff.kara");
    std::fs::write(
        &fixture,
        "struct State { id: i64, mut count: i64 }\n\
         fn use_a(s: State) { }\n\
         fn use_b(s: State) { }\n\
         fn main() {\n\
             let s = State { id: 0, count: 0 };\n\
             par {\n\
                 use_a(s);\n\
                 use_b(s);\n\
             }\n\
         }\n",
    )
    .expect("write fixture");
    let out = karac_bin()
        .args(["check", fixture.to_str().unwrap(), "--output=json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_file(&fixture);
    assert!(!out.status.success());
    assert!(
        stdout.contains("E_CONCURRENT_PLAIN_STRUCT"),
        "expected E_CONCURRENT_PLAIN_STRUCT code in JSON; got: {stdout}",
    );
    assert!(
        stdout.contains("\"fix_diff\":["),
        "expected fix_diff array in JSON envelope; got: {stdout}",
    );
}

// ── karac migrate shared-to-par <Type> (phase-7 L215a) ─────────

/// Scratch file helper for migrate tests — keyed by pid + tag + nanos so
/// parallel test runs don't collide. Lives outside any git repo (under
/// the OS temp dir) so the workspace-dirty guard always reports "clean"
/// for these tests.
fn migrate_scratch_file(tag: &str, source: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-migrate-{}-{}-{}.kara",
        std::process::id(),
        tag,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::write(&path, source).expect("write migrate scratch source");
    path
}

#[test]
fn test_migrate_dry_run_prints_diff_for_shared_struct() {
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("dryrun_basic", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "migrate dry-run should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("would apply"),
        "expected dry-run header in stdout; got: {stdout}",
    );
    assert!(
        stdout.contains("`shared` → `par`"),
        "expected keyword rename edit in dry-run; got: {stdout}",
    );
    assert!(
        stdout.contains("→ `Mutex[`"),
        "expected Mutex[ wrap edit in dry-run; got: {stdout}",
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, original,
        "dry-run must not write to disk; got rewritten file: {on_disk}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_apply_rewrites_file() {
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("apply_basic", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            // Pass --force so the test is independent of whether the OS
            // temp dir happens to live inside a git repo (it normally
            // doesn't, but defensive).
            "--force",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        stdout.contains("applied"),
        "expected apply confirmation in stdout; got: {stdout}",
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct Counter"),
        "expected `par struct Counter` post-migrate; got: {rewritten}",
    );
    assert!(
        rewritten.contains("count: Mutex[i64]"),
        "expected `count: Mutex[i64]` post-migrate; got: {rewritten}",
    );
    assert!(
        !rewritten.contains("shared struct"),
        "stale `shared struct` keyword still present: {rewritten}",
    );
    assert!(
        !rewritten.contains("mut count"),
        "stale `mut` keyword on field still present: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_rejects_missing_type() {
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("missing_type", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Nonexistent",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "migrate on missing type should fail",);
    assert!(
        stderr.contains("no struct named `Nonexistent`"),
        "expected missing-type diagnostic; got: {stderr}",
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, original,
        "missing-type failure must not write to disk; got: {on_disk}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_rejects_non_shared_struct() {
    let original = "struct Counter {\n    mut count: i64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("non_shared", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "migrate on plain struct should fail (wrong tool)",
    );
    assert!(
        stderr.contains("not a `shared struct`"),
        "expected wrong-tool diagnostic; got: {stderr}",
    );
    let on_disk = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        on_disk, original,
        "wrong-tool failure must not write to disk; got: {on_disk}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_unknown_kind_rejected() {
    // Subcommand-shape check: only `shared-to-par` is a known kind today.
    let out = karac_bin()
        .args(["migrate", "plain-to-par", "Counter", "/tmp/whatever.kara"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "migrate with unknown kind should fail",
    );
    assert!(
        stderr.contains("unknown migration kind 'plain-to-par'"),
        "expected unknown-kind diagnostic; got: {stderr}",
    );
}

#[test]
fn test_migrate_multiple_mut_fields_all_wrapped() {
    let original = "shared struct State {\n    mut count: i64,\n    name: String,\n    mut total: f64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("multi_field", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "State",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct State"),
        "expected keyword rewrite; got: {rewritten}",
    );
    assert!(
        rewritten.contains("count: Mutex[i64]"),
        "expected count field wrapped; got: {rewritten}",
    );
    assert!(
        rewritten.contains("total: Mutex[f64]"),
        "expected total field wrapped; got: {rewritten}",
    );
    // The non-mut field `name: String` must remain unchanged — only mut
    // fields get wrapped. This is the load-bearing invariant from L201a.
    assert!(
        rewritten.contains("name: String"),
        "non-mut field must NOT be wrapped; got: {rewritten}",
    );
    assert!(
        !rewritten.contains("Mutex[String]"),
        "non-mut field must NOT receive a Mutex wrap; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_help_lists_kind_and_flags() {
    // The per-subcommand help page renders before the arg parser runs,
    // so `karac migrate --help` returns the help text and exits 0.
    let out = karac_bin().args(["migrate", "--help"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "migrate --help should exit 0");
    assert!(
        stdout.contains("karac migrate"),
        "help should self-identify; got: {stdout}",
    );
    assert!(
        stdout.contains("shared-to-par"),
        "help should list the migration kind; got: {stdout}",
    );
    assert!(
        stdout.contains("--apply"),
        "help should list --apply flag; got: {stdout}",
    );
    assert!(
        stdout.contains("--force"),
        "help should list --force flag; got: {stdout}",
    );
    assert!(
        stdout.contains("--no-atomic"),
        "help should document the --no-atomic opt-out; got: {stdout}",
    );
}

// ── L215b1: consumer-site write-rewrite (single-file, type-annotated bindings) ──

#[test]
fn test_migrate_wraps_assign_writes_against_typed_let_binding() {
    // Canonical L215b1 case + L215b2 self-prefix shape: a `let c: Counter`
    // binding with an assign write — the migrate path emits a
    // `lock self.count { ... }` wrap and rewrites the binding root
    // `c` to `self` inside the wrap body (design.md line 8522).
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count = 5;\n}\n";
    let path = migrate_scratch_file("consumer_assign", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct Counter"),
        "type-def rewrite missing; got: {rewritten}",
    );
    assert!(
        rewritten.contains("lock self.count {"),
        "expected L215b2 self-prefix `lock self.count {{` wrap; got: {rewritten}",
    );
    assert!(
        rewritten.contains("self.count = 5"),
        "binding `c` should be rewritten to `self` inside the wrap body; got: {rewritten}",
    );
    assert!(
        !rewritten.contains("c.count = 5"),
        "original `c.count = 5` should have been rewritten to `self.count = 5`; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_wraps_writes_through_ref_parameter() {
    // The common real-world shape: a free function takes the migrating
    // type by ref. Type-match must strip the `ref` modifier so the
    // param is discovered. `mut ref` should also work the same way.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn bump(c: ref Counter) {\n    c.count = c.count + 1;\n}\n\nfn reset(c: mut ref Counter) {\n    c.count = 0;\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("consumer_ref_param", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    // Both function bodies should have their `c.count = ...` writes
    // wrapped with the L215b2 self-prefix shape.
    let lock_wrap_count = rewritten.matches("lock self.count {").count();
    assert_eq!(
        lock_wrap_count, 2,
        "expected two `lock self.count` wraps (one per ref-param fn); got {lock_wrap_count} in: {rewritten}",
    );
    // The value-side `c.count` in `c.count = c.count + 1` should also be
    // binding-rewritten to `self.count` inside the wrap.
    assert!(
        rewritten.contains("self.count = self.count + 1"),
        "value-side `c.count` should be rewritten to `self.count` inside the wrap; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_skips_writes_inside_par_block() {
    // Writes inside `par { ... }` are the diagnostic-emitting territory
    // of `karac fix` (E_CONCURRENT_SHARED_STRUCT). The migrate path
    // must NOT double-wrap them. Only the write outside the par should
    // receive a lock wrap.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count = 1;\n    par {\n        c.count = 2;\n        c.count = 3;\n    }\n}\n";
    let path = migrate_scratch_file("consumer_skip_par", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dry-run should succeed");
    // Three edits per outside-par write under L215b2 (1 binding-rewrite
    // `c` → `self` + 2 wrap insertions: prefix + suffix). Inside-par
    // assigns would contribute six more edits if not filtered.
    // Sum: 4 type-def + 3 consumer = 7 total.
    assert!(
        stdout.contains("would apply 7 migration edit(s)"),
        "expected 7 edits (4 type-def + 3 consumer for the single outside-par write); got: {stdout}",
    );
    let prefix_count = stdout.matches("(insert) → `lock self.count {").count();
    assert_eq!(
        prefix_count, 1,
        "expected one consumer-write wrap (the outside-par assign); got {prefix_count} in: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_wraps_compound_assign() {
    // Compound assignment (`+=`) is structurally a write; the existing
    // walker handles it via the `StmtKind::CompoundAssign` arm.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count += 1;\n}\n";
    let path = migrate_scratch_file("consumer_compound", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("lock self.count {"),
        "compound-assign should be wrapped with L215b2 self-prefix shape; got: {rewritten}",
    );
    assert!(
        rewritten.contains("self.count += 1"),
        "compound-assign body's binding `c` should be rewritten to `self`; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_skips_writes_to_non_mut_fields() {
    // Only `mut` fields get the `Mutex[T]` wrap on the type-def side,
    // so the consumer-rewrite walker must mirror that: writes to the
    // non-mut field `name` stay alone, writes to the mut field `count`
    // get wrapped.
    let original = "shared struct Counter {\n    mut count: i64,\n    name: String,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0, name: \"a\" };\n    c.count = 1;\n    c.name = \"b\";\n}\n";
    let path = migrate_scratch_file("consumer_nonmut", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dry-run should succeed");
    let lock_count = stdout.matches("(insert) → `lock self.count {").count();
    let lock_name = stdout.matches("(insert) → `lock self.name {").count();
    assert_eq!(
        lock_count, 1,
        "expected single `lock self.count` wrap for the mut field; got {lock_count}",
    );
    assert_eq!(
        lock_name, 0,
        "expected zero `lock self.name` wraps for the non-mut field; got {lock_name} in: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_ignores_bindings_of_other_types() {
    // A `let x: i64 = ...` binding inside a function body with a write
    // to `x.something` must NOT be confused for a Counter binding even
    // if the function also has an unrelated Counter binding.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let n: i64 = 0;\n    let c: Counter = Counter { count: 0 };\n    c.count = n;\n}\n";
    let path = migrate_scratch_file("consumer_other_types", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dry-run should succeed");
    // Exactly one wrap for the single Counter write — `n` is an i64
    // binding, so its assignment site shouldn't show up at all.
    let wraps = stdout.matches("(insert) → `lock self.count {").count();
    assert_eq!(
        wraps, 1,
        "expected exactly one wrap for the c.count write; got {wraps} in: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_dry_run_lists_consumer_edits_in_addition_to_typedef() {
    // The dry-run epilogue claims consumer-site wraps are emitted.
    // Pin the actual edit list: it must include both the type-def
    // rewrites (4 for one mut field) AND consumer wraps (2 per assign).
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count = 1;\n}\n";
    let path = migrate_scratch_file("consumer_dryrun_shape", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dry-run should succeed");
    // 4 type-def + 3 consumer (1 binding-rewrite + 2 wrap insertions
    // for the single L215b2 self-prefix write) = 7 total.
    assert!(
        stdout.contains("would apply 7 migration edit(s)"),
        "expected 7 edits (4 type-def + 3 consumer for the single write); got: {stdout}",
    );
    assert!(
        stdout.contains("`shared` → `par`"),
        "type-def keyword rename should still appear; got: {stdout}",
    );
    assert!(
        stdout.contains("(insert) → `lock self.count {"),
        "consumer wrap prefix should appear in dry-run; got: {stdout}",
    );
    assert!(
        stdout.contains("`c` → `self`"),
        "binding-rewrite edit should appear in dry-run; got: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_wraps_read_site() {
    // L215b2: every `<binding>.<mut_field>` rvalue use gets a value-
    // expression wrap. A `let x = c.count;` should rewrite to
    // `let x = lock self.count { self.count };` — a single replacement
    // edit covering the full `c.count` span.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn read(c: ref Counter) -> i64 {\n    let x: i64 = c.count;\n    x\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("consumer_read_site", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("let x: i64 = lock self.count { self.count };"),
        "expected read-site wrap on `c.count` rvalue; got: {rewritten}",
    );
    assert!(
        !rewritten.contains("c.count"),
        "original `c.count` should have been subsumed by the read wrap; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_read_wrap_skips_assign_targets() {
    // L215b2: the read walker must NOT independently wrap reads that
    // are already inside a write-wrapped statement — otherwise the
    // output would have nested locks on the same field. For
    // `c.count = c.count + 1`, both the target and the value RHS get
    // binding-rewritten to `self.count` (one wrap, no nested wraps).
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn bump(c: ref Counter) {\n    c.count = c.count + 1;\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("consumer_read_skip_write_target", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    // Exactly one `lock self.count` opener — the outer write-wrap.
    // A nested read-wrap would produce a second occurrence.
    let lock_count = rewritten.matches("lock self.count {").count();
    assert_eq!(
        lock_count, 1,
        "expected exactly one `lock self.count` wrap (no nested read-wrap); got {lock_count} in: {rewritten}",
    );
    // Both sides of the assign should have their binding rewritten.
    assert!(
        rewritten.contains("self.count = self.count + 1"),
        "expected the value-side `c.count` to be binding-rewritten to `self.count` inside the wrap; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_wraps_read_inside_if_condition() {
    // L215b2: reads inside if-conditions, binary expressions, etc.
    // should also receive the read-wrap.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn check(c: ref Counter) -> i64 {\n    if c.count > 0 {\n        1\n    } else {\n        0\n    }\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("consumer_read_if_cond", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("if lock self.count { self.count } > 0"),
        "expected if-condition read of `c.count` to be wrapped; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_skips_reads_inside_par_block() {
    // L215b2: reads inside `par { ... }` blocks belong to the `karac fix`
    // diagnostic path, not the preemptive migrate. The par-span filter
    // must drop them like it drops par-internal writes (L215b1).
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    let outside: i64 = c.count;\n    par {\n        let inside_a: i64 = c.count;\n        let inside_b: i64 = c.count;\n    }\n}\n";
    let path = migrate_scratch_file("consumer_skip_reads_par", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "dry-run should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Exactly one read-wrap (the outside-par read). The two inside-par
    // reads must be filtered out.
    let read_wraps = stdout
        .matches("`c.count` → `lock self.count { self.count }`")
        .count();
    assert_eq!(
        read_wraps, 1,
        "expected one read-wrap (outside the par block); got {read_wraps} in: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_discovers_inferred_let_binding() {
    // L215b3: the canonical inferred-binding case — `let c = make_counter()`
    // has no annotation, so the parse-only path can't discover the binding.
    // The typecheck-aware path reads `pattern_binding_types[c.span] = "Counter"`
    // and wraps the subsequent write. Without L215b3, this would emit only
    // the 4 type-def edits and silently miss the consumer write.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn make_counter() -> Counter {\n    Counter { count: 0 }\n}\n\nfn main() {\n    let c = make_counter();\n    c.count = 5;\n}\n";
    let path = migrate_scratch_file("inferred_let", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("lock self.count {"),
        "expected the inferred-binding write to be wrapped; got: {rewritten}",
    );
    assert!(
        rewritten.contains("self.count = 5"),
        "binding `c` (inferred-type) should be rewritten to `self` inside the wrap; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_wraps_mutating_method_call_with_typecheck_data() {
    // L215b3 lift: under L215b1/b2 the consumer-rewrite ran with an
    // empty MethodMutClassifier, so mutating method-call writes
    // (`c.items.push(x)`) silently no-op'd. With typecheck data
    // threaded through, `method_callee_types` resolves the call site
    // to `Vec.push` and `stdlib_method_self_borrow_kind` flags it as
    // MutRef — the L207 walker then wraps the call.
    let original = "shared struct Queue {\n    mut items: Vec[i64],\n}\n\nfn main() {\n    let q: Queue = Queue { items: [10, 20, 30] };\n    q.items.push(42);\n}\n";
    let path = migrate_scratch_file("inferred_method_call", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Queue",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("lock self.items {"),
        "expected mutating method-call to be wrapped; got: {rewritten}",
    );
    assert!(
        rewritten.contains("self.items.push(42)"),
        "binding root `q` should be rewritten to `self` for the method receiver; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_typecheck_failure_falls_back_to_annotated_only() {
    // Graceful degradation: when typecheck fails (e.g. unresolved
    // identifier elsewhere in the file), migrate still walks the
    // parse-only annotated-binding path so users on a partially-
    // broken file get a useful starting-point diff rather than
    // nothing. Inferred bindings naturally drop out (no typecheck
    // data to consult), but annotated ones survive.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count = 5;\n    let _ = undefined_function();\n}\n";
    let path = migrate_scratch_file("tc_fail_fallback", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "migrate --apply should succeed even when typecheck fails; stderr={}",
        String::from_utf8_lossy(&out.stderr),
    );
    let rewritten = std::fs::read_to_string(&path).unwrap();
    assert!(
        rewritten.contains("par struct Counter"),
        "type-def rewrite should still run on typecheck-failing source; got: {rewritten}",
    );
    assert!(
        rewritten.contains("lock self.count {"),
        "annotated binding write should still wrap under parse-only fallback; got: {rewritten}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_discovers_inferred_binding_alongside_annotated() {
    // Mixed-discovery shape: one annotated binding (`let c: Counter`)
    // + one inferred binding (`let d = make_counter()`). Both must be
    // wrapped, and the dedup discipline must prevent the annotation
    // overlap on the annotated binding from doubling its edits.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn make_counter() -> Counter {\n    Counter { count: 0 }\n}\n\nfn main() {\n    let c: Counter = Counter { count: 0 };\n    c.count = 1;\n    let d = make_counter();\n    d.count = 2;\n}\n";
    let path = migrate_scratch_file("inferred_mixed", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "dry-run should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Exactly two `lock self.count {` inserts — one per binding.
    let wraps = stdout.matches("lock self.count {").count();
    assert_eq!(
        wraps, 2,
        "expected two lock-wrap inserts (one per binding); got {wraps} in: {stdout}",
    );
    // Both `c` and `d` should get binding-root rewrites to `self`.
    assert!(
        stdout.contains("`c` → `self`"),
        "annotated binding `c` should rewrite to `self`; stdout: {stdout}",
    );
    assert!(
        stdout.contains("`d` → `self`"),
        "inferred binding `d` should rewrite to `self`; stdout: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

// ── L215b4: project-mode (cross-file) walk ──────────────────────

#[test]
fn test_migrate_project_mode_walks_modules() {
    // `shared struct Counter` lives in src/counter.kara; src/main.kara
    // declares an annotated binding of that type and assigns its mut field.
    // Project-mode (no <file> arg) should pick up both: the type-def
    // rewrite in counter.kara AND the consumer write-wrap in main.kara.
    // `--no-atomic` pins the Mutex shape (the Atomic heuristic is on by
    // default and would classify this bare-`=` i64 field as Atomic).
    let tmp = scratch_project("migrate-project-walk");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count = 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--no-atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "project-mode dry-run should succeed; stderr={stderr}; stdout={stdout}",
    );
    // Header announces multi-file scope.
    assert!(
        stdout.contains("across 2 file(s)"),
        "expected `across 2 file(s)` in header; stdout={stdout}",
    );
    // Type-def file: keyword rename + Mutex wrap.
    assert!(
        stdout.contains("counter.kara"),
        "expected counter.kara in output; stdout={stdout}",
    );
    assert!(
        stdout.contains("`shared` → `par`"),
        "expected keyword rename edit; stdout={stdout}",
    );
    assert!(
        stdout.contains("→ `Mutex[`"),
        "expected Mutex wrap edit; stdout={stdout}",
    );
    // Consumer file: lock-wrap insert.
    assert!(
        stdout.contains("main.kara"),
        "expected main.kara in output; stdout={stdout}",
    );
    assert!(
        stdout.contains("lock self.count {"),
        "expected `lock self.count {{` consumer wrap; stdout={stdout}",
    );
}

#[test]
fn test_migrate_project_mode_apply_rewrites_all_files() {
    // `--no-atomic` pins the Mutex apply shape; the default Atomic
    // heuristic would otherwise rewrite this bare-`=` i64 field to
    // `Atomic[i64]` + `.store`/`.load` consumer sites.
    let tmp = scratch_project("migrate-project-apply");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    let counter_path = tmp.join("src/counter.kara");
    let main_path = tmp.join("src/main.kara");
    write(
        &counter_path,
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &main_path,
        "fn bump(c: ref Counter) {\n    c.count = 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            "--no-atomic",
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let counter_after = std::fs::read_to_string(&counter_path).unwrap();
    let main_after = std::fs::read_to_string(&main_path).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "project-mode apply should succeed; stderr={stderr}; stdout={stdout}",
    );
    // Both files report an `applied N` confirmation line.
    let applied = stdout.matches("applied ").count();
    assert_eq!(
        applied, 2,
        "expected 2 `applied N` lines (one per file); stdout={stdout}",
    );
    // Type-def file post-rewrite.
    assert!(
        counter_after.contains("par struct Counter"),
        "counter.kara should have `par struct Counter`; got: {counter_after}",
    );
    assert!(
        counter_after.contains("count: Mutex[i64]"),
        "counter.kara should wrap field type; got: {counter_after}",
    );
    // Consumer file post-rewrite.
    assert!(
        main_after.contains("lock self.count {"),
        "main.kara should wrap the write site; got: {main_after}",
    );
}

#[test]
fn test_migrate_project_mode_errors_when_struct_missing() {
    let tmp = scratch_project("migrate-project-missing");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("no `shared struct Counter`"),
        "expected helpful project-mode error; stderr={stderr}",
    );
}

#[test]
fn test_migrate_project_mode_no_kara_toml_errors() {
    // No kara.toml in tmp dir or its ancestors (under /tmp).
    let tmp = scratch_project("migrate-project-no-manifest");
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("kara.toml"),
        "expected error to mention kara.toml; stderr={stderr}",
    );
}

#[test]
fn test_migrate_project_mode_errors_when_struct_in_multiple_files() {
    let tmp = scratch_project("migrate-project-dup");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/a.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/b.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "expected non-zero exit");
    assert!(
        stderr.contains("multiple") && stderr.contains("Counter"),
        "expected duplicate-struct error; stderr={stderr}",
    );
}

#[test]
fn test_migrate_single_file_mode_unchanged_with_explicit_path() {
    // Single-file invocation still works the same as before L215b4 —
    // passing an explicit <file> short-circuits project-mode.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn main() {}\n";
    let path = migrate_scratch_file("single_file_unchanged", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "single-file dry-run should succeed");
    assert!(
        stdout.contains("would apply"),
        "single-file dry-run shape unchanged; got: {stdout}",
    );
    // Project-mode header must NOT appear in single-file mode.
    assert!(
        !stdout.contains("across "),
        "single-file mode should not emit project-mode header; got: {stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

// ── L215c: Atomic[T] heuristic ──────────────────────────────────

#[test]
fn test_migrate_l215c_atomic_when_only_bare_assigns() {
    // Counter with a single `mut count: i64` field. Consumer only
    // does `c.count = N` (bare =). Both conditions for Atomic met:
    // (a) type is in the lock-free Copy set, (b) every observed
    // write across the workspace is bare =. Expected: type-def emits
    // `Atomic[i64]`, not `Mutex[i64]`.
    let tmp = scratch_project("migrate-l215c-atomic");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count = 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("→ `Atomic[`"),
        "expected Atomic[ wrap; stdout={stdout}",
    );
    assert!(
        !stdout.contains("→ `Mutex[`"),
        "should NOT emit Mutex[ when Atomic-eligible; stdout={stdout}",
    );
    assert!(
        stdout.contains("classified as `Atomic[T]`"),
        "expected note about Atomic-classified fields; stdout={stdout}",
    );
    // L215c-cons — Atomic-classified consumer writes get rewritten to
    // `.store(v, MemoryOrdering.Release)`. Pin the rewrite shape on
    // dry-run output: both the ` = ` → `.store(` replacement and the
    // trailing `, MemoryOrdering.Release)` insertion must appear.
    // No lock-wrap should appear for Atomic-classified sites.
    assert!(
        !stdout.contains("lock self.count {"),
        "Atomic-classified consumer sites should NOT be lock-wrapped; stdout={stdout}",
    );
    assert!(
        stdout.contains("` = ` → `.store(`"),
        "expected ` = ` → `.store(` rewrite for Atomic consumer; stdout={stdout}",
    );
    assert!(
        stdout.contains("(insert) → `, MemoryOrdering.Release)`"),
        "expected Release-ordering suffix insert; stdout={stdout}",
    );
}

#[test]
fn test_migrate_l215c_mutex_when_compound_assign() {
    // Same Counter, but consumer uses `c.count += 1` (compound assign)
    // — disqualifies the field from Atomic. Expected: type-def emits
    // `Mutex[i64]` and the consumer write gets a lock-wrap.
    let tmp = scratch_project("migrate-l215c-compound");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count += 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("→ `Mutex[`"),
        "expected Mutex[ wrap (compound assign disqualifies); stdout={stdout}",
    );
    assert!(
        !stdout.contains("→ `Atomic[`"),
        "should NOT emit Atomic[ when compound assign present; stdout={stdout}",
    );
    assert!(
        stdout.contains("lock self.count {"),
        "consumer compound assign should be lock-wrapped; stdout={stdout}",
    );
}

#[test]
fn test_migrate_l215c_mutex_when_non_eligible_type() {
    // Counter with a `mut items: Vec[i64]` field — type isn't in the
    // Atomic-eligible Copy set. Expected: type-def emits `Mutex[Vec[i64]]`
    // regardless of how the field is written.
    let tmp = scratch_project("migrate-l215c-noneligible-type");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/q.kara"),
        "pub shared struct Q {\n    mut items: Vec[i64],\n}\n",
    );
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Q", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("→ `Mutex[`"),
        "non-eligible type should default to Mutex[; stdout={stdout}",
    );
    assert!(
        !stdout.contains("→ `Atomic[`"),
        "should NOT emit Atomic[ for non-eligible type; stdout={stdout}",
    );
}

#[test]
fn test_migrate_l215c_single_file_always_mutex() {
    // Single-file mode lacks workspace visibility — even when the
    // only observed write is bare =, single-file emits Mutex[T].
    // Pinning this behavior so the heuristic stays project-mode-only.
    let original = "shared struct Counter {\n    mut count: i64,\n}\n\nfn bump(c: ref Counter) { c.count = 1; }\n\nfn main() {}\n";
    let path = migrate_scratch_file("l215c_single_file", original);
    let out = karac_bin()
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "single-file dry-run should succeed");
    assert!(
        stdout.contains("→ `Mutex[`"),
        "single-file mode always emits Mutex[; stdout={stdout}",
    );
    assert!(
        !stdout.contains("→ `Atomic[`"),
        "single-file mode should NEVER emit Atomic[; stdout={stdout}",
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_migrate_l215c_mixed_fields_atomic_and_mutex() {
    // Counter with two Atomic-eligible-typed fields where the writes
    // diverge: `mut count: i64` only sees bare = (→ Atomic[i64]) and
    // `mut total: i64` sees a compound += (→ Mutex[i64]). Two wrap
    // types in one type def, with the consumer-side compound assign
    // getting a lock-wrap.
    let tmp = scratch_project("migrate-l215c-mixed");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n    mut total: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count = 1;\n    c.total += 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    let atomic_wraps = stdout.matches("→ `Atomic[`").count();
    let mutex_wraps = stdout.matches("→ `Mutex[`").count();
    assert_eq!(
        atomic_wraps, 1,
        "expected exactly 1 Atomic[ wrap (for count); stdout={stdout}",
    );
    assert_eq!(
        mutex_wraps, 1,
        "expected exactly 1 Mutex[ wrap (for total); stdout={stdout}",
    );
    // count is Atomic — should NOT be lock-wrapped at consumer site;
    // its bare = should be rewritten to `.store(v, MemoryOrdering.Release)`.
    assert!(
        !stdout.contains("lock self.count {"),
        "Atomic count should not get a consumer wrap; stdout={stdout}",
    );
    assert!(
        stdout.contains("` = ` → `.store(`"),
        "Atomic count's bare = should be rewritten to .store(; stdout={stdout}",
    );
    // total is Mutex — its compound-assign SHOULD be lock-wrapped.
    assert!(
        stdout.contains("lock self.total {"),
        "Mutex total should get a consumer wrap; stdout={stdout}",
    );
}

// ── L215c-cons: Atomic[T] consumer-rewrite (.store / .load) ─────

#[test]
fn test_migrate_l215c_cons_atomic_read_rewrites_to_load() {
    // Counter with a single bare-= Atomic-eligible field. A consumer
    // function `read` does `c.count` (rvalue read in return position).
    // L215c-cons rewrites that read to `c.count.load(MemoryOrdering.Acquire)`.
    let tmp = scratch_project("migrate-l215c-cons-read");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn read(c: ref Counter) -> i64 {\n    c.count\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("(insert) → `.load(MemoryOrdering.Acquire)`"),
        "Atomic read should be rewritten to .load(MemoryOrdering.Acquire); stdout={stdout}",
    );
    assert!(
        !stdout.contains("lock self.count {"),
        "Atomic read should NOT be lock-wrapped; stdout={stdout}",
    );
}

#[test]
fn test_migrate_l215c_cons_atomic_apply_produces_compilable_shape() {
    // End-to-end --apply verification: the on-disk rewrite of an
    // Atomic-classified write produces the canonical Kāra source
    // shape `c.count.store(1, MemoryOrdering.Release);` (semicolon
    // preserved, field name preserved, ordering literal preserved).
    // Pinning the actual rewritten text — not just the edit preview
    // — guards against an offset-math regression that would silently
    // drop or duplicate bytes around the edit boundary.
    let tmp = scratch_project("migrate-l215c-cons-apply");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    let consumer = "fn bump(c: ref Counter) {\n    c.count = 42;\n}\n\nfn read(c: ref Counter) -> i64 {\n    c.count\n}\n\nfn main() {}\n";
    write(&tmp.join("src/main.kara"), consumer);
    let out = karac_bin()
        .current_dir(&tmp)
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            "--atomic",
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "--apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let main_rewritten = std::fs::read_to_string(tmp.join("src/main.kara")).unwrap();
    let counter_rewritten = std::fs::read_to_string(tmp.join("src/counter.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        counter_rewritten.contains("count: Atomic[i64]"),
        "type def should wrap as Atomic[i64]; got: {counter_rewritten}",
    );
    assert!(
        main_rewritten.contains("c.count.store(42, MemoryOrdering.Release);"),
        "write should be rewritten to .store call; got: {main_rewritten}",
    );
    assert!(
        main_rewritten.contains("c.count.load(MemoryOrdering.Acquire)"),
        "read should be rewritten to .load call; got: {main_rewritten}",
    );
    assert!(
        !main_rewritten.contains("c.count = "),
        "bare `c.count = ` should NOT survive the rewrite; got: {main_rewritten}",
    );
}

#[test]
fn test_migrate_l215c_cons_atomic_read_in_complex_expression() {
    // Atomic read embedded inside a binary expression (`c.count + 1`
    // and `c.count > 0`) — the load rewrite must fire on each read
    // site independently, not just on standalone reads.
    let tmp = scratch_project("migrate-l215c-cons-complex");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    let consumer = "fn add_one(c: ref Counter) -> i64 {\n    c.count + 1\n}\n\nfn is_positive(c: ref Counter) -> bool {\n    c.count > 0\n}\n\nfn bump(c: ref Counter) {\n    c.count = 7;\n}\n\nfn main() {}\n";
    write(&tmp.join("src/main.kara"), consumer);
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    // Two distinct read sites — expect two load-insert edits.
    let load_inserts = stdout
        .matches("(insert) → `.load(MemoryOrdering.Acquire)`")
        .count();
    assert_eq!(
        load_inserts, 2,
        "expected 2 .load inserts (one per read site); stdout={stdout}",
    );
}

#[test]
fn test_migrate_l215c_cons_mixed_atomic_and_mutex_rewrites_apply() {
    // Mixed-classification fixture under --apply: count is Atomic
    // (bare = only), total is Mutex (compound +=). Verify the
    // on-disk shape: count writes/reads become .store/.load, total
    // writes become `lock self.total { ... }`. The two field shapes
    // must coexist in one file without offset drift between them.
    let tmp = scratch_project("migrate-l215c-cons-mixed-apply");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n    mut total: i64,\n}\n",
    );
    let consumer = "fn bump(c: ref Counter) {\n    c.count = 1;\n    c.total += 1;\n}\n\nfn snapshot(c: ref Counter) -> i64 {\n    c.count\n}\n\nfn main() {}\n";
    write(&tmp.join("src/main.kara"), consumer);
    let out = karac_bin()
        .current_dir(&tmp)
        .args([
            "migrate",
            "shared-to-par",
            "Counter",
            "--atomic",
            "--apply",
            "--force",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "--apply should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rewritten = std::fs::read_to_string(tmp.join("src/main.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        rewritten.contains("c.count.store(1, MemoryOrdering.Release);"),
        "Atomic count write should be .store-rewritten; got: {rewritten}",
    );
    assert!(
        rewritten.contains("c.count.load(MemoryOrdering.Acquire)"),
        "Atomic count read should be .load-rewritten; got: {rewritten}",
    );
    assert!(
        rewritten.contains("lock self.total {"),
        "Mutex total compound assign should be lock-wrapped; got: {rewritten}",
    );
    assert!(
        !rewritten.contains("lock self.count {"),
        "Atomic count should NEVER be lock-wrapped; got: {rewritten}",
    );
}

#[test]
fn test_migrate_l215c_cons_no_atomic_flag_keeps_mutex_path() {
    // With --no-atomic, the L215a–b4 default (all-Mutex) applies — no
    // Atomic classifier runs, so even an obviously-Atomic-eligible
    // field stays Mutex-wrapped. Pinning that --no-atomic is the gate
    // for the consumer-rewrite path too, not just the type-def half.
    let tmp = scratch_project("migrate-l215c-cons-noflag");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count = 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter", "--no-atomic"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("lock self.count {"),
        "with --no-atomic, default Mutex lock-wrap should apply; stdout={stdout}",
    );
    assert!(
        !stdout.contains(".store("),
        "with --no-atomic, no .store rewrite should fire; stdout={stdout}",
    );
    assert!(
        !stdout.contains(".load("),
        "with --no-atomic, no .load rewrite should fire; stdout={stdout}",
    );
}

// ── L215c default-flip: --atomic is on by default in project-mode ──

#[test]
fn test_migrate_default_atomic_when_only_bare_assigns() {
    // Default-flip: with NO flag at all, project-mode runs the Atomic[T]
    // heuristic. A bare-`=`-only i64 field is wrapped as `Atomic[i64]`
    // and its consumer write is rewritten to `.store(v, ...)` rather than
    // lock-wrapped. Equivalent to passing --atomic explicitly.
    let tmp = scratch_project("migrate-default-atomic");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/counter.kara"),
        "pub shared struct Counter {\n    mut count: i64,\n}\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "fn bump(c: ref Counter) {\n    c.count = 1;\n}\n\nfn main() {}\n",
    );
    let out = karac_bin()
        .current_dir(&tmp)
        .args(["migrate", "shared-to-par", "Counter"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(out.status.success(), "dry-run should succeed");
    assert!(
        stdout.contains("→ `Atomic[`"),
        "default (no flag) should emit Atomic[ wrap; stdout={stdout}",
    );
    assert!(
        !stdout.contains("→ `Mutex[`"),
        "default (no flag) should NOT emit Mutex[ for an Atomic-eligible field; stdout={stdout}",
    );
    assert!(
        !stdout.contains("lock self.count {"),
        "default Atomic classification should NOT lock-wrap; stdout={stdout}",
    );
    assert!(
        stdout.contains("` = ` → `.store(`"),
        "default Atomic should rewrite bare = to .store(; stdout={stdout}",
    );
}

// ── Phase-10: gated stdlib modules (std.web) — single-file mode ─

/// Regression: single-file mode has no ProgramTree, so a gated import
/// used to blind-bind in the resolver and ICE in the interpreter on
/// first use ("variable 'fetch' not found").
///
/// NOTE (target gate): this program calls `paint()` (writes(Display))
/// from `main`, which `karac check`/`build` reject on native — the
/// test runs via `karac run`, where the phase-10 run-leniency decision
/// (2026-06-06) downgrades effect findings (E0411 included) to
/// `warning[effect]` lines and keeps executing, mirroring the
/// typecheck treatment on the lenient script path. `Pipeline::resolve`
/// expands gated imports into the real baked items
/// (`prelude::expand_gated_stdlib_imports`); this pins the full
/// run path: import + effect-clause use + calling the fetch stub body,
/// plus the warn-don't-abort half of the leniency decision.
#[test]
fn std_web_single_file_run_executes_gated_imports() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-std-web-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("webby.kara");
    let src = r#"
import std.web.{Display, Storage};
import std.web.net.fetch;

fn paint() with writes(Display) reads(Storage) {
}

fn main() {
    paint();
    match fetch("https://example.com/") {
        Ok(resp) => println("fetched"),
        Err(e) => println(f"stub: {e.message}"),
    }
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std.web single-file run should succeed; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("stub: std.web.net.fetch: host-call lowering not wired yet"),
        "fetch stub body should execute and surface its message: {stdout}",
    );
    assert!(
        stderr.contains("warning[effect]")
            && stderr.contains("does not provide resource 'Display'"),
        "the E0411 target-gate finding should surface as a warning under run: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── LLJIT Slice 6: `karac run` rejects effect violations (leniency stripped) ─

/// Post-Slice-6, run-leniency is stripped: `karac run` rejects the same
/// static-contract violations `karac check` / `karac build` reject. A hard
/// effect error (E0400 here) now aborts the run with `error[effect]` on
/// stderr instead of downgrading to `warning[effect]` and executing — the
/// run/check *acceptance* asymmetry is gone. (Was
/// `run_effect_violation_warns_and_executes` under the phase-10 leniency
/// decision, 2026-06-06, superseded by the 2026-07-06 LLJIT decision.)
#[test]
fn run_effect_violation_aborts() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-run-effect-abort-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("effabort.kara");
    let src = r#"
effect resource Db;

fn touch() with writes(Db) {
}

pub fn api() {
    touch()
}

fn main() {
    api();
    println("ran anyway");
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "post-Slice-6, an effect violation must abort `karac run`; stdout={stdout} stderr={stderr}",
    );
    assert!(
        !stdout.contains("ran anyway"),
        "program must NOT execute past the rejected effect violation: {stdout}",
    );
    assert!(
        stderr.contains("error[effect]")
            && stderr.contains("public function 'api' performs effects [writes(Db)]"),
        "the E0400 finding should surface as a hard error[effect] on stderr: {stderr}",
    );
    assert!(
        !stderr.contains("warning[effect]"),
        "the effect violation must NOT downgrade to warning[effect]: {stderr}",
    );

    // Symmetry: `karac check` rejects the same program — run now agrees.
    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "`karac check` must also reject the effect violation",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The abort tier for *value-corrupting type errors* (B-2026-06-13-15).
/// A cast the typechecker rejects (`E_INT_AS_CHAR` here) has no defined
/// `as` lowering, so the interpreter would substitute a placeholder and
/// emit silently wrong output at exit 0 — the run-leniency footgun kata
/// #67/#415 surfaced. `karac run` must abort with `error[typecheck]` (not
/// downgrade to a warning), matching `karac check` / `karac build`. Since
/// Slice 6 stripped run-leniency entirely, this abort is no longer special-
/// cased — every type error aborts (see `run_soft_type_error_aborts`) — but
/// the value-corrupting cast remains a load-bearing regression pin.
#[test]
fn run_value_corrupting_cast_aborts() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-run-vcast-abort-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("vcast.kara");
    // `b as char` is rejected (E_INT_AS_CHAR). Before the fix, `karac run`
    // downgraded it to a warning and printed a placeholder for `c`.
    let src = "fn main() {\n    let b: u8 = 65u8;\n    let c = b as char;\n    println(c);\n    println(\"unreachable past the cast\");\n}\n";
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "a value-corrupting cast must abort `karac run`; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stderr.contains("error[typecheck]") && stderr.contains("E_INT_AS_CHAR"),
        "expected a hard error[typecheck]/E_INT_AS_CHAR, not a warning: {stderr}",
    );
    assert!(
        !stderr.contains("warning[typecheck]"),
        "the cast must NOT downgrade to warning[typecheck]: {stderr}",
    );
    // The program must not execute — no placeholder output, nothing past
    // the cast.
    assert!(
        !stdout.contains("unreachable past the cast"),
        "program executed past the rejected cast: {stdout}",
    );

    // Symmetry: `karac check` rejects the same program (it always did —
    // this pins that `run` now agrees rather than silently diverging).
    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "`karac check` must also reject the value-corrupting cast",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Post-Slice-6: run-leniency stripped, so *every* type error is fatal for
/// `karac run` — not just the narrow value-corrupting `is_run_fatal` set. A
/// genuinely soft type error (here a too-many-arguments arity mismatch) now
/// aborts the run with `error[typecheck]` instead of downgrading to
/// `warning[typecheck]` and executing. This is the deliberate reversal of the
/// phase-10 "soft type errors keep their leniency" partition — run now agrees
/// with `check`/`build` on acceptance. (Was
/// `run_noncast_type_error_warns_and_executes`.)
#[test]
fn run_soft_type_error_aborts() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-run-softtype-abort-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("softtype.kara");
    let src = "fn add(a: i32, b: i32) -> i32 { a + b }\nfn main() {\n    let _x = add(1i32, 2i32, 3i32);\n    println(\"ran anyway\");\n}\n";
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "post-Slice-6, a soft type error must abort `karac run`; stdout={stdout} stderr={stderr}",
    );
    assert!(
        !stdout.contains("ran anyway"),
        "program must NOT execute past the rejected type error: {stdout}",
    );
    assert!(
        stderr.contains("error[typecheck]"),
        "the arity mismatch should surface as a hard error[typecheck]: {stderr}",
    );
    assert!(
        !stderr.contains("warning[typecheck]"),
        "the type error must NOT downgrade to warning[typecheck]: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// LLJIT Slice 6b (opt-in): `KARAC_RUN_JIT=1 karac run` routes execution through
/// the LLJIT engine (via the `karac_jit_runner` subprocess) instead of the
/// tree-walk interpreter, so `run` executes the same codegen as `karac build`.
/// This pins the opt-in path end-to-end: the JIT'd program's stdout reaches the
/// user (inherited stdio) and its `main` exit code propagates. Requires the
/// runner beside the karac test binary (cargo builds it under `--features llvm`).
#[cfg(feature = "llvm")]
#[test]
fn run_via_jit_opt_in_executes_and_matches_output() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-run-jit-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("jitrun.kara");
    std::fs::write(
        &path,
        "fn main() {\n    let mut n = 0i64;\n    for i in 1..5 { n = n + i; }\n    println(f\"sum={n}\");\n}\n",
    )
    .unwrap();

    // Point the runner locator at cargo's per-build runner binary.
    let runner = env!("CARGO_BIN_EXE_karac_jit_runner");
    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .env("KARAC_RUN_JIT", "1")
        .env("KARAC_JIT_RUNNER", runner)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "JIT-run should exit 0; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("sum=10"),
        "JIT'd program stdout should reach the user (1+2+3+4=10): stdout={stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The abort tier of the same decision: RAII-across-yield violations
/// break execution-soundness/teardown guarantees (like provider escape),
/// so they abort `karac run` rather than warn. This gate existed in
/// `cmd_run` but was vacuously green before the run-leniency slice —
/// `raii_across_yield_check` keys off `Program.state_struct_layouts` /
/// `yield_points`, which only `Pipeline::effectcheck` populates, and
/// `cmd_run` never invoked it. This pins the gate's liveness.
#[test]
fn run_raii_across_yield_violation_aborts() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-run-raii-abort-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("raiirun.kara");
    let src = r#"
shared struct Hub {
    id: i64,
}

pub fn fetch_data() with sends(Network) receives(Network) {
}

fn driver(h: Hub) {
    fetch_data();
}

fn main() {
    let h = Hub { id: 1 };
    driver(h);
    println("should not print");
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "RAII-across-yield must abort `karac run`; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stderr.contains("error[E_RAII_ACROSS_YIELD]"),
        "abort should carry the RAII diagnostic: {stderr}",
    );
    assert!(
        !stdout.contains("should not print"),
        "the program must not execute past the RAII gate: {stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The other half of the gate: without the import, the resource name
/// must not exist — single-file mode included.
#[test]
fn std_web_single_file_unimported_resource_is_undefined() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-std-web-neg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("nogate.kara");
    std::fs::write(
        &path,
        "fn persist() with writes(Storage) {\n}\n\nfn main() { persist(); }\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "unimported std.web resource must fail to resolve",
    );
    assert!(
        stderr.contains("undefined effect resource 'Storage'"),
        "expected undefined-resource diagnostic, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// std.wasi single-file: the gated-import expansion appends a
/// REDECLARATION of a name scope-0 already provides (prelude resource
/// shadowing is sanctioned — see process.kara on ProcessTable). Pins
/// that the expansion path doesn't turn that into a duplicate
/// definition, and the effect clause works through the import.
#[test]
fn std_wasi_single_file_import_over_prelude_resource() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-std-wasi-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("wasiish.kara");
    let src = r#"
import std.wasi.{FileSystem, Clock};

fn snapshot() with reads(FileSystem) reads(Clock) {
}

fn main() {
    snapshot();
    println("std.wasi ok");
}
"#;
    std::fs::write(&path, src).unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "std.wasi single-file run should succeed; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("std.wasi ok"),
        "program should run: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: `#[target(...)]` absence semantics (single-file) ──

/// Items gated to the current target (or `not(<other>)`) stay active;
/// items gated elsewhere vanish silently unless referenced.
#[test]
fn target_attr_single_file_active_and_filtered() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-target-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("gated.kara");
    std::fs::write(
        &path,
        r#"
#[target(native)]
fn platform_name() -> String { "native" }

#[target(not(gpu))]
fn io_helper() -> i64 { 7 }

#[target(wasm_browser, wasm_wasi)]
fn wasm_only() -> i64 { 1 }

fn main() {
    println(platform_name());
    println(f"{io_helper()}");
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["run", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "native-active gated items must run; stdout={stdout} stderr={stderr}",
    );
    assert_eq!(stdout, "native\n7\n");
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Referencing a filtered item answers with the targeted diagnostic,
/// not a bare undefined-name.
#[test]
fn target_attr_single_file_inactive_reference_diagnostic() {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-target-neg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("gated_ref.kara");
    std::fs::write(
        &path,
        "#[target(wasm_browser)]\nstruct DomNode { id: i64 }\n\n\
         fn main() {\n    let n = DomNode { id: 1 };\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("'DomNode' is not available on target `native`")
            && stderr.contains("#[target(wasm_browser)]"),
        "expected the targeted gating diagnostic, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: effect-driven target gate (check path) ────────────
// `karac run` deliberately skips effect checking (lenient script
// path — typecheck is warnings-only there too); the gate fires on
// `karac check` / `karac build`.

#[test]
fn target_gate_check_rejects_aliased_web_resource() {
    // Alias canonicalization: `import std.web.Display as Screen;`
    // must not evade the Display gate — the renamed clone carries
    // canonical provenance.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-gate-alias-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("alias_gate.kara");
    std::fs::write(
        &path,
        "import std.web.Display as Screen;\n\n\
         fn paint() with writes(Screen) {\n}\n\n\
         fn main() {\n    paint();\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "aliased gate must fail check");
    assert!(
        stderr.contains("target `native` does not provide resource 'Display'")
            && stderr.contains("main → paint"),
        "alias must canonicalize to Display with the chain: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn target_gate_check_provider_bound_web_resource_passes() {
    // SSR pattern end-to-end through the binary: providers-bound
    // Display is legal on native.
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-gate-ssr-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    let path = tmp.join("ssr_gate.kara");
    std::fs::write(
        &path,
        "import std.web.Display;\n\n\
         struct HtmlBuilder { buf: String }\n\n\
         fn render() with writes(Display) {\n}\n\n\
         fn main() {\n\
             providers {\n\
                 Display => HtmlBuilder { buf: \"\" },\n\
             } in {\n\
                 render();\n\
             }\n\
         }\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "provider-bound Display must pass the native gate: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: `karac check` multi-target verification ───────────
//
// `--targets=` (or a discovered manifest's `[build].targets`) runs the
// full pipeline once per v1 target, parameterizing the target-provided
// resource set each time; diagnostics are tagged per target and
// findings identical on every target are deduplicated into an
// "all targets" group. Analysis-only — no llvm/runtime infrastructure
// needed, so none of these tests skip.

/// Fresh temp dir for one multi-target check test.
fn multi_target_dir(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-mtcheck-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

/// Program whose `main` reaches `FileSystem` — in `native` and
/// `wasm_wasi`'s provided sets but NOT `wasm_browser`'s, so it is the
/// minimal target-differential probe.
const MT_FILESYSTEM_PROBE: &str = "pub fn save() with writes(FileSystem) {\n    let _x = 1;\n}\n\n\
     fn main() {\n    save();\n}\n";

#[test]
fn check_targets_flag_tags_per_target_findings() {
    let tmp = multi_target_dir("difftag");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args([
            "check",
            path.to_str().unwrap(),
            "--targets=native,wasm_browser",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "browser gate must fail the matrix");
    assert!(
        stderr.contains("── target: native ──")
            && stderr.contains("All checks passed under target 'native'."),
        "native pass must be reported under its own tag: {stderr}",
    );
    assert!(
        stderr.contains("── target: wasm_browser ──")
            && stderr.contains("target `wasm_browser` does not provide resource 'FileSystem'")
            && stderr.contains("1 error(s) under target 'wasm_browser'."),
        "browser violation must be tagged with its target: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_dedupes_target_agnostic_diagnostics() {
    // A plain typecheck error fires identically on every target — it
    // must be reported once under "all targets", not once per target.
    let tmp = multi_target_dir("dedup");
    let path = tmp.join("shared.kara");
    std::fs::write(&path, "fn main() {\n    let x: i64 = \"oops\";\n}\n").unwrap();

    let out = karac_bin()
        .args([
            "check",
            path.to_str().unwrap(),
            "--targets=native,wasm_wasi",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("── all targets ──"),
        "shared section must be present: {stderr}",
    );
    assert_eq!(
        stderr.matches("expected 'i64', found 'String'").count(),
        1,
        "target-agnostic diagnostic must appear exactly once: {stderr}",
    );
    assert!(
        stderr.contains("1 error(s) under target 'native'.")
            && stderr.contains("1 error(s) under target 'wasm_wasi'."),
        "per-target error counts still include the shared finding: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_json_splits_shared_and_per_target() {
    let tmp = multi_target_dir("json");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args([
            "check",
            path.to_str().unwrap(),
            "--targets=native,wasm_browser",
            "--output=json",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(
        stdout.contains("\"target\":\"native\",\"success\":true")
            && stdout.contains("\"target\":\"wasm_browser\",\"success\":false")
            && stdout.contains("\"code\":\"E0411\"")
            && stdout.contains("\"shared_diagnostics\":[]")
            && stdout.contains("\"success\":false}"),
        "JSON must carry per-target blocks and the (empty) shared group: {stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_jsonl_brackets_each_target() {
    let tmp = multi_target_dir("jsonl");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args([
            "check",
            path.to_str().unwrap(),
            "--targets=native,wasm_browser",
            "--output=jsonl",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    assert!(
        stdout.contains("{\"type\":\"target_start\",\"target\":\"native\"}")
            && stdout
                .contains("\"type\":\"target_complete\",\"target\":\"native\",\"success\":true")
            && stdout.contains("{\"type\":\"target_start\",\"target\":\"wasm_browser\"}")
            && stdout.contains(
                "\"type\":\"target_complete\",\"target\":\"wasm_browser\",\"success\":false"
            ),
        "JSONL must bracket each target's event stream: {stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_single_target_parameterizes_resource_set() {
    // One requested target is not a matrix, but the pass must still run
    // under THAT target's provided-resource set — the probe passes a
    // bare `karac check` (native) and must fail `--targets=wasm_browser`.
    let tmp = multi_target_dir("single");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap(), "--targets=wasm_browser"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "browser-only check must fail");
    assert!(
        stderr.contains("── target: wasm_browser ──")
            && stderr.contains("target `wasm_browser` does not provide resource 'FileSystem'"),
        "single-target run still parameterizes the gate: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_unknown_name_rejected() {
    let tmp = multi_target_dir("badname");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap(), "--targets=native,wasm_wsi"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown target 'wasm_wsi'")
            && stderr.contains("native, wasm_browser, wasm_wasi, gpu"),
        "unknown name must list the closed set: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_targets_mutually_exclusive_with_profiles() {
    let tmp = multi_target_dir("excl");
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args([
            "check",
            path.to_str().unwrap(),
            "--targets=all",
            "--profiles=all",
        ])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("--profiles and --targets are mutually exclusive"),
        "matrix product must be rejected loudly: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_manifest_build_targets_drives_multi_target() {
    // The design trigger: a package declaring `[build].targets` gets the
    // per-target matrix on a bare `karac check`, no flag required.
    // Discovery walks upward from the checked file's own directory.
    let tmp = multi_target_dir("manifest");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"mtprobe\"\n\n[build]\ntargets = [\"native\", \"wasm_browser\"]\n",
    )
    .unwrap();
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    let path = tmp.join("src").join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "manifest matrix must fail on browser"
    );
    assert!(
        stderr.contains("All checks passed under target 'native'.")
            && stderr.contains("target `wasm_browser` does not provide resource 'FileSystem'"),
        "manifest-declared targets must drive the matrix: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn check_manifest_unknown_build_target_is_hard_error() {
    // A typo'd target must abort check, not silently shrink the matrix.
    let tmp = multi_target_dir("badmanifest");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"mtprobe\"\n\n[build]\ntargets = [\"walm\"]\n",
    )
    .unwrap();
    let path = tmp.join("gated.kara");
    std::fs::write(&path, MT_FILESYSTEM_PROBE).unwrap();

    let out = karac_bin()
        .args(["check", path.to_str().unwrap()])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("`[build].targets`: unknown target 'walm'"),
        "manifest typo must be a hard error: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: WASM build path (`--target=wasm_wasi`) ────────────
//
// The build-path E2E needs three pieces of infrastructure beyond the
// karac binary: an llvm-enabled build, the wasm runtime archive
// (`libkarac_runtime_wasm.a` — see CLAUDE.md's three-archive recipe),
// and a wasm linker + node for the WASI host. Each absence skips with
// a stderr note rather than failing, mirroring the codegen E2E
// harness's runtime-archive treatment.

/// Fresh temp dir for one wasm-path test.
fn wasm_test_dir(tag: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(format!(
        "karac-cli-wasm-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    tmp
}

/// Did this `karac build` invocation hit a missing-infrastructure wall
/// rather than a real failure? Returns the skip reason if so.
fn wasm_build_skip_reason(stderr: &str) -> Option<&'static str> {
    if stderr.contains("requires the llvm feature") {
        return Some("karac built without --features llvm");
    }
    if stderr.contains("libkarac_runtime_wasm.a not found") {
        return Some("wasm runtime archive not built (see CLAUDE.md archive recipe)");
    }
    // `--features wasm-threads` infrastructure (phase-10 wasm-threads
    // entry): the FOURTH archive + the threads rustup target.
    if stderr.contains("libkarac_runtime_wasm_threads.a not found") {
        return Some("wasm-threads runtime archive not built (see CLAUDE.md archive recipe)");
    }
    if stderr.contains("no wasm linker found") {
        return Some("no wasm-ld / rust-lld available");
    }
    if stderr.contains("wasm32-wasip1-threads self-contained sysroot not found") {
        return Some("rustup target wasm32-wasip1-threads not installed");
    }
    if stderr.contains("self-contained sysroot not found") {
        return Some("rustup target wasm32-wasip1 not installed");
    }
    // Embedded component bindings shell out to the external wasm-tools
    // binary (phase-10 "embedded-WIT migration"); a missing install is
    // missing infrastructure, not a regression.
    if stderr.contains("wasm-tools not found") {
        return Some("wasm-tools not installed (cargo install wasm-tools)");
    }
    None
}

/// A wasm binary's preamble distinguishes a Component Model component
/// from a core module without external tooling: bytes 4..8 are the
/// version+layer field — `0x0d 0x00 0x01 0x00` for a component
/// (layer one) vs `0x01 0x00 0x00 0x00` for a core module (layer
/// zero). Load-immune shape check for the embedded-component tests.
fn wasm_artifact_kind(path: &std::path::Path) -> &'static str {
    let bytes = std::fs::read(path).expect("wasm artifact must be readable");
    assert!(
        bytes.len() >= 8 && &bytes[0..4] == b"\0asm",
        "not a wasm binary: {}",
        path.display()
    );
    match &bytes[4..8] {
        [0x0d, 0x00, 0x01, 0x00] => "component",
        [0x01, 0x00, 0x00, 0x00] => "core module",
        _ => "unknown wasm layer",
    }
}

/// Path to an installed `wasm-tools`, when one is on PATH — the
/// embedded-component tests use it for WIT round-trip assertions that
/// go beyond the preamble shape check (import naming, world contents).
/// `None` skips those deeper assertions, not the test.
fn wasm_tools_on_path() -> Option<&'static str> {
    let ok = std::process::Command::new("wasm-tools")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        Some("wasm-tools")
    } else {
        None
    }
}

fn wasmtime_on_path() -> Option<&'static str> {
    let ok = std::process::Command::new("wasmtime")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        Some("wasmtime")
    } else {
        None
    }
}

/// `--target=gpu` is a recognized v1 name that is not standalone-
/// buildable (kernels are dispatched from a host program) — it rejects
/// loudly instead of silently emitting a native binary under a
/// cross-target flag. (`wasm_browser` became buildable with the
/// phase-10 "host fn lowering — browser-WASM" slice; see the
/// `wasm_browser_*` tests below.)
#[test]
fn target_flag_gpu_rejected() {
    let tmp = wasm_test_dir("reject");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=gpu"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("gpu") && stderr.contains("not a standalone build target"),
        "expected the gpu rejection, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--enable-hot-swap` is incompatible with wasm targets — a wasm
/// module has no dynamic-symbol-resolution machinery (the wasm half of
/// the phase-7 hot-swap target gating). Project mode rejects before
/// manifest discovery (an empty dir suffices); single-file rejects
/// before any pipeline pass.
#[test]
fn hot_swap_rejected_on_wasm_targets() {
    let tmp = wasm_test_dir("hotswap");
    // Project mode (no file argument) — fires pre-manifest.
    let out = karac_bin()
        .args(["build", "--target=wasm_wasi", "--enable-hot-swap"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("--enable-hot-swap is incompatible with --target=wasm_wasi"),
        "expected the project-mode hot-swap/wasm rejection, got: {stderr}",
    );
    // Single-file — same gate (rides the llvm build path; the non-llvm
    // fallback type-checks instead, so skip the assertion there).
    let path = tmp.join("h.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--enable-hot-swap",
        ])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains("requires the llvm feature") {
        assert!(!out.status.success());
        assert!(
            stderr.contains("--enable-hot-swap is incompatible with --target=wasm_browser"),
            "expected the single-file hot-swap/wasm rejection, got: {stderr}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// E0411 aborts a wasm build: a program whose `main` reaches a host
/// resource `wasm_wasi` does not provide (browser-only `Display`) must
/// fail at the effect gate with the targeted diagnostic — not reach the
/// linker and die on an undefined symbol. Fires before codegen/link, so
/// only the llvm-fallback skip applies.
#[test]
fn wasm_wasi_build_aborts_on_target_gate_violation() {
    let tmp = wasm_test_dir("gate");
    let path = tmp.join("gated.kara");
    std::fs::write(
        &path,
        "import std.web.{Display};\n\n\
         pub fn paint() with writes(Display) {\n    println(\"painting\");\n}\n\n\
         fn main() {\n    paint();\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        // `--bindings=none` keeps this a pure effect-gate test — the
        // component default would first resolve wasm-tools, an
        // unrelated infrastructure dependency.
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_build_aborts_on_target_gate_violation — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success(), "gate violation must abort: {stderr}");
    assert!(
        stderr.contains("E0411")
            && stderr.contains("`wasm_wasi` does not provide resource 'Display'"),
        "expected the E0411 target-gate abort, got: {stderr}",
    );
    assert!(
        !tmp.join("gated.wasm").exists(),
        "no artifact may be produced on a gate violation",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Full build-path E2E: compile a program exercising arithmetic,
/// Vec/String runtime calls, and `#[target]` selection to a `.wasm`
/// module, then run it under node's WASI preview-1 host and assert the
/// output matches the native semantics. Skips gracefully when the wasm
/// archive, a wasm linker, the wasi sysroot, or node are absent.
#[test]
fn wasm_wasi_build_and_run_e2e() {
    let tmp = wasm_test_dir("e2e");
    let path = tmp.join("hello_wasm.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
fn where_am_i() -> String {
    return "wasm";
}

#[target(native)]
fn where_am_i() -> String {
    return "native";
}

fn fib(n: i64) -> i64 {
    if n < 2 { return n; }
    return fib(n - 1) + fib(n - 2);
}

fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(40);
    v.push(2);
    let mut total = 0;
    for x in v {
        total += x;
    }
    println(total);
    println(fib(15));
    println(where_am_i());
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        // `--bindings=none`: node's `node:wasi` host runs core modules,
        // not components — this test exercises the raw-module embedder
        // shape (the wasm_wasi default is now the embedded component).
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        // The dev-tier fallback must resolve the wasm archive — a
        // native KARAC_RUNTIME override in the environment would be
        // honored verbatim and linked into the wasm module.
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_build_and_run_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("hello_wasm.wasm");
    assert!(
        wasm_path.exists(),
        "expected hello_wasm.wasm next to the build cwd",
    );

    // WASI preview-1 host: node's built-in `node:wasi` module.
    let runner = tmp.join("run_wasi.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\n\
         import { WASI } from 'node:wasi';\n\
         import { argv, exit } from 'node:process';\n\
         const wasi = new WASI({ version: 'preview1', args: [], env: {} });\n\
         const wasm = await WebAssembly.compile(await readFile(argv[2]));\n\
         const instance = await WebAssembly.instantiate(wasm, wasi.getImportObject());\n\
         exit(wasi.start(instance));\n",
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm_path)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_wasi_build_and_run_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "wasm module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "42\n610\nwasm\n",
        "wasm output must match native semantics (and pick the \
         #[target(wasm_wasi)] item); stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Regression (B-2026-06-14-15): numeric (int/float) f-string interpolation
/// and `int.to_string()` on the wasm target. These route through `snprintf`,
/// whose `size_t n` param codegen declared as `i64` — correct natively but
/// wrong on wasm32 (size_t = i32), so wasm-ld replaced the call with a
/// trapping `signature_mismatch:snprintf` stub and any numeric f-string
/// aborted with `unreachable`. (Bare `println(<int>)` uses a printf path with
/// no size_t param, so the existing wasm E2E above never caught this.) Fixed
/// by declaring + calling `snprintf` with a pointer-width `size_t`. Asserts the
/// wasm output is byte-identical to the native semantics, incl. a negative int
/// (varargs widening) and a float.
#[test]
fn wasm_numeric_fstring_build_and_run_e2e() {
    let tmp = wasm_test_dir("numfstr");
    let path = tmp.join("numfstr.kara");
    std::fs::write(
        &path,
        r#"
fn main() {
    let x = 42;
    let y = -7;
    let f = 2.5;
    println(f"i {x} n {y} f {f}");
    let n = 100;
    println(n.to_string());
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_numeric_fstring_build_and_run_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("numfstr.wasm");
    assert!(wasm_path.exists(), "expected numfstr.wasm");
    // The trapping stub must NOT be present (its presence is the bug).
    let wasm_bytes = std::fs::read(&wasm_path).unwrap();
    assert!(
        !wasm_bytes
            .windows(b"signature_mismatch:snprintf".len())
            .any(|w| w == b"signature_mismatch:snprintf"),
        "wasm must not contain a trapping signature_mismatch:snprintf stub",
    );

    let runner = tmp.join("run_wasi.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\n\
         import { WASI } from 'node:wasi';\n\
         import { argv, exit } from 'node:process';\n\
         const wasi = new WASI({ version: 'preview1', args: [], env: {} });\n\
         const wasm = await WebAssembly.compile(await readFile(argv[2]));\n\
         const instance = await WebAssembly.instantiate(wasm, wasi.getImportObject());\n\
         exit(wasi.start(instance));\n",
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm_path)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_numeric_fstring_build_and_run_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "numeric f-string wasm module failed under node:wasi (was the snprintf \
         trap): stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "i 42 n -7 f 2.5\n100\n",
        "numeric f-string + to_string must match native semantics; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Full build-path E2E for `String.split` on the wasm target (Weave
/// dogfood follow-up). The split FFI (`karac_runtime_string_split`)
/// allocates the `Vec[String]` from the unified wasi-libc heap
/// (`runtime/src/wasm_alloc.rs`) that codegen's `free` reclaims from, and
/// its size params are i64-width `u64` so the call matches codegen's i64
/// size ABI (the `signature_mismatch:karac_runtime_string_split` trap class
/// — same root as `__karac_malloc64`/B-2026-06-12-1). Exercises a
/// String-separator split with leading/trailing/interior empty pieces and
/// asserts the wasm output is byte-identical to the native semantics. Skips
/// gracefully when the wasm archive, a wasm linker, the wasi sysroot, or
/// node are absent.
#[test]
fn wasm_string_split_build_and_run_e2e() {
    let tmp = wasm_test_dir("split");
    let path = tmp.join("split_wasm.kara");
    std::fs::write(
        &path,
        r#"
fn main() {
    // Interior empty piece (",,") plus a single-char and multi-char piece.
    let s: String = "a,bb,,ccc";
    let parts: Vec[String] = s.split(",");
    println(parts.len());
    for p in parts {
        println(p);
    }
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_string_split_build_and_run_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("split_wasm.wasm");
    assert!(
        wasm_path.exists(),
        "expected split_wasm.wasm next to the build cwd",
    );

    let runner = tmp.join("run_wasi.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\n\
         import { WASI } from 'node:wasi';\n\
         import { argv, exit } from 'node:process';\n\
         const wasi = new WASI({ version: 'preview1', args: [], env: {} });\n\
         const wasm = await WebAssembly.compile(await readFile(argv[2]));\n\
         const instance = await WebAssembly.instantiate(wasm, wasi.getImportObject());\n\
         exit(wasi.start(instance));\n",
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm_path)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_string_split_build_and_run_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "wasm split module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    // 4 pieces: "a", "bb", "" (interior), "ccc" — byte-identical to native.
    assert_eq!(
        node_stdout, "4\na\nbb\n\nccc\n",
        "wasm String.split output must match native semantics; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice A): a `pub fn`
/// positively tagged `#[target(wasm_wasi)]` becomes a wasm module export
/// (`--export=add`), callable from JS alongside `_start`. Built
/// `--bindings=none` (raw core module) and instantiated under
/// `node:wasi` WITHOUT running `_start`, then `instance.exports.add(2,3)`
/// is called directly — proving the symbol is exported and callable.
#[test]
fn wasm_entry_point_export_callable_e2e() {
    let tmp = wasm_test_dir("export-entry");
    let path = tmp.join("exports_demo.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
pub fn add(a: i32, b: i32) -> i32 {
    return a + b;
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_entry_point_export_callable_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("exports_demo.wasm");
    assert!(
        wasm_path.exists(),
        "expected exports_demo.wasm next to the build cwd"
    );

    // Instantiate under node:wasi but do NOT call wasi.start — invoke the
    // discovered export directly. `add` is pure arithmetic, so it needs no
    // WASI runtime state; its presence + correct result proves the export
    // was surfaced by `--export=add`.
    let runner = tmp.join("call_export.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\n\
         import { WASI } from 'node:wasi';\n\
         import { argv } from 'node:process';\n\
         const wasi = new WASI({ version: 'preview1', args: [], env: {} });\n\
         const wasm = await WebAssembly.compile(await readFile(argv[2]));\n\
         const instance = await WebAssembly.instantiate(wasm, wasi.getImportObject());\n\
         if (typeof instance.exports.add !== 'function') {\n\
         \x20 console.error('add not exported'); process.exit(2);\n\
         }\n\
         console.log(instance.exports.add(2, 3));\n",
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm_path)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_entry_point_export_callable_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "export call failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "5\n",
        "exports.add(2,3) must return 5; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice B): a
/// `--bindings browser` build types each scalar `pub fn` export on the
/// handle's `exports` in the generated `.d.ts` (a `KaraExports`
/// interface). Build-only assertion (no node) — reads the emitted
/// `<stem>.d.ts` and checks the per-export signature + the handle wiring.
#[test]
fn wasm_browser_dts_types_scalar_exports() {
    let tmp = wasm_test_dir("dts-exports");
    let path = tmp.join("lib_demo.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_browser)]
pub fn add(a: i32, b: i32) -> i32 {
    return a + b;
}

#[target(wasm_browser)]
pub fn tick(n: i64) {}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_dts_types_scalar_exports — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm browser build failed: {stderr}");
    let dts = std::fs::read_to_string(tmp.join("lib_demo.d.ts")).expect("d.ts must be emitted");
    assert!(
        dts.contains("export interface KaraExports {"),
        "d.ts must declare KaraExports; got:\n{dts}"
    );
    assert!(
        dts.contains("add(a: number, b: number): number;"),
        "d.ts must type the `add` export; got:\n{dts}"
    );
    assert!(
        dts.contains("tick(n: bigint): void;"),
        "d.ts must type `tick` (i64 -> bigint, unit -> void); got:\n{dts}"
    );
    assert!(
        dts.contains("exports: WebAssembly.Exports & KaraExports;"),
        "handle.exports must be typed with KaraExports; got:\n{dts}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice D.4): a
/// `--bindings browser` build marshals RICH exports — structs → JS
/// objects, `Option[T]` → `T | null`, `Result[T,E]` → `{ok}|{err}`,
/// `String` → `string`, `Vec[T]` → `T[]` — through the generated glue
/// (canonical-ABI trampolines + `cabi_realloc`). Imports the emitted ES
/// glue under node, `instantiate()`s the sequential module, and asserts
/// each wrapped export's JS value.
#[test]
fn wasm_browser_rich_exports_marshal_e2e() {
    let tmp = wasm_test_dir("browser-rich");
    let path = tmp.join("richlib.kara");
    std::fs::write(
        &path,
        r#"
#[derive(Copy, Clone)]
pub struct Point { x: f64, y: f64 }

#[target(wasm_browser)]
pub fn mk(x: f64, y: f64) -> Point { return Point { x: x, y: y }; }

#[target(wasm_browser)]
pub fn area(p: Point) -> f64 { return p.x * p.y; }

#[target(wasm_browser)]
pub fn checked_div(a: i32, b: i32) -> Option[i32] {
    if b == 0 { return Option.None; }
    return Option.Some(a / b);
}

#[target(wasm_browser)]
pub fn safe_div(a: i32, b: i32) -> Result[i32, i32] {
    if b == 0 { return Result.Err(0 - 1); }
    return Result.Ok(a / b);
}

#[target(wasm_browser)]
pub fn shout(s: String) -> String { return s + "!"; }

#[target(wasm_browser)]
pub fn squares(n: i32) -> Vec[i32] {
    let mut o: Vec[i32] = Vec.new();
    let mut i = 0;
    while i < n { o.push(i * i); i += 1; }
    return o;
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_rich_exports_marshal_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm browser build failed: {stderr}");

    // The .d.ts types the rich shapes.
    let dts = std::fs::read_to_string(tmp.join("richlib.d.ts")).expect("d.ts emitted");
    assert!(
        dts.contains("mk(x: number, y: number): { x: number; y: number };"),
        "{dts}"
    );
    assert!(
        dts.contains("checked_div(a: number, b: number): number | null;"),
        "{dts}"
    );
    assert!(
        dts.contains("safe_div(a: number, b: number): { ok: number } | { err: number };"),
        "{dts}"
    );
    assert!(dts.contains("shout(s: string): string;"), "{dts}");
    assert!(dts.contains("squares(n: number): number[];"), "{dts}");

    // Run the glue under node and assert the marshalled JS values.
    let runner = tmp.join("run.mjs");
    std::fs::write(
        &runner,
        "import { instantiate } from './richlib.js';\n\
         const e = (await instantiate()).exports;\n\
         const eq = (a, b) => { if (JSON.stringify(a) !== JSON.stringify(b)) {\n\
         \x20 console.error('FAIL', JSON.stringify(a), '!=', JSON.stringify(b)); process.exit(3); } };\n\
         eq(e.mk(2, 3), { x: 2, y: 3 });\n\
         eq(e.area({ x: 4, y: 5 }), 20);\n\
         eq(e.checked_div(10, 2), 5);\n\
         eq(e.checked_div(10, 0), null);\n\
         eq(e.safe_div(10, 2), { ok: 5 });\n\
         eq(e.safe_div(10, 0), { err: -1 });\n\
         eq(e.shout('hi'), 'hi!');\n\
         eq(e.squares(5), [0, 1, 4, 9, 16]);\n\
         console.log('OK');\n",
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_browser_rich_exports_marshal_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("OK"),
        "browser rich marshalling failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice C): a
/// `--bindings component` (wasm_wasi default) build lifts each scalar
/// `pub fn` export into the embedded WIT world. The export name is
/// kebab-cased (`add_two` ⇒ `add-two`); codegen's `wasm-export-name`
/// attribute renames the core export to match so `wasm-tools component
/// new` can find it (a mismatch fails the build outright — so a
/// successful component build already proves the lift). Asserts the
/// artifact is a component and (when wasm-tools is present) the WIT
/// carries the kebab export.
#[test]
fn wasm_wasi_component_exports_scalar_entry_point() {
    let tmp = wasm_test_dir("component-export");
    let path = tmp.join("complib.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
pub fn add_two(a: i32, b: i32) -> i32 {
    return a + b;
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_component_exports_scalar_entry_point — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "component build failed: {stderr}");
    let component = tmp.join("complib.wasm");
    assert!(component.exists(), "missing complib.wasm");
    assert_eq!(
        wasm_artifact_kind(&component),
        "component",
        "the wasm_wasi default must emit a Component Model component",
    );
    if let Some(tool) = wasm_tools_on_path() {
        let wit_dump = std::process::Command::new(tool)
            .args(["component", "wit"])
            .arg(&component)
            .output()
            .unwrap();
        assert!(
            wit_dump.status.success(),
            "wasm-tools component wit must round-trip: {}",
            String::from_utf8_lossy(&wit_dump.stderr)
        );
        let wit = String::from_utf8_lossy(&wit_dump.stdout);
        assert!(
            wit.contains("export add-two: func(a: s32, b: s32) -> s32;"),
            "embedded WIT must export the (kebab-cased) entry point, got:\n{wit}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--bindings component` (the wasm_wasi default) must be byte-for-byte
/// reproducible: two builds of identical source from two distinct
/// processes produce identical component bytes. Regression for
/// B-2026-06-22-3 — the intermediate C-ABI core module was linked to a
/// `karac_<pid>_<stem>.core.wasm` scratch file, and wasm-ld baked that
/// pid-bearing basename into the module-name subsection of the `name`
/// custom section, which `component new` carried verbatim into the final
/// component. Three builds of the same source then differed in exactly
/// those pid digits — same length, different bytes. (`--bindings none`
/// was always deterministic: it links straight to the stable `<stem>.wasm`
/// output, no pid in the basename.) The fix links the core module under a
/// source-derived basename inside a process-unique *directory*, so this
/// test pins reproducibility for the component path too. The exported
/// `add_two` also drives the WIT embed step, so both componentization
/// legs (`component embed` + `component new`) are covered.
#[test]
fn wasm_wasi_component_is_byte_reproducible() {
    let tmp = wasm_test_dir("component-determinism");
    let path = tmp.join("repro.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
pub fn add_two(a: i32, b: i32) -> i32 {
    return a + b;
}

fn main() {
    println(add_two(2, 3));
}
"#,
    )
    .unwrap();

    // Each call is a fresh `karac` process (distinct pid) — the exact
    // condition the old pid-in-basename leak made non-reproducible.
    let build_once = || -> Option<Vec<u8>> {
        let out = karac_bin()
            .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
            .current_dir(&tmp)
            .env_remove("KARAC_RUNTIME")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        if let Some(reason) = wasm_build_skip_reason(&stderr) {
            eprintln!("skip: wasm_wasi_component_is_byte_reproducible — {reason}");
            return None;
        }
        assert!(out.status.success(), "component build failed: {stderr}");
        let component = tmp.join("repro.wasm");
        assert_eq!(
            wasm_artifact_kind(&component),
            "component",
            "the wasm_wasi default must emit a Component Model component",
        );
        Some(std::fs::read(&component).expect("component must be readable"))
    };

    let Some(first) = build_once() else {
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let second = build_once().expect("second build must not suddenly skip");
    // Length parity alone would have passed even with the pid leak — the
    // assertion that bites is byte equality.
    assert_eq!(first.len(), second.len(), "component length must be stable");
    assert_eq!(
        first, second,
        "two builds of identical source must produce byte-identical \
         components (B-2026-06-22-3: the process id must not leak into the \
         embedded core-module name)",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice D): a component
/// export returning a flat record (`fn make_point(x, y) -> Point`) lifts
/// into the WIT world as a `record` + `func(...) -> <record>`, via a
/// codegen canonical-ABI trampoline (the Kāra fn returns the aggregate by
/// value → LLVM `sret`; the trampoline relays it into an aligned return
/// area and returns the pointer the canonical ABI expects). `component
/// new` validates canonical conformance, so a successful build already
/// proves the lowering; when wasmtime is present we additionally invoke
/// `make-point(2, 3)` and assert the lifted `{x: 2, y: 3}`.
#[test]
fn wasm_wasi_component_exports_record_return() {
    let tmp = wasm_test_dir("component-record");
    let path = tmp.join("reclib.kara");
    std::fs::write(
        &path,
        r#"
#[derive(Copy, Clone)]
pub struct Point { x: f64, y: f64 }

#[target(wasm_wasi)]
pub fn make_point(x: f64, y: f64) -> Point {
    return Point { x: x, y: y };
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_component_exports_record_return — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "component build failed: {stderr}");
    let component = tmp.join("reclib.wasm");
    assert_eq!(wasm_artifact_kind(&component), "component");
    if let Some(tool) = wasm_tools_on_path() {
        let wit = String::from_utf8_lossy(
            &std::process::Command::new(tool)
                .args(["component", "wit"])
                .arg(&component)
                .output()
                .unwrap()
                .stdout,
        )
        .into_owned();
        assert!(
            wit.contains("record point") && wit.contains("x: f64") && wit.contains("y: f64"),
            "embedded WIT must declare the record type, got:\n{wit}",
        );
        assert!(
            wit.contains("export make-point: func(x: f64, y: f64) -> point;"),
            "embedded WIT must export the record-returning func, got:\n{wit}",
        );
    }
    if let Some(wt) = wasmtime_on_path() {
        let run = std::process::Command::new(wt)
            .args(["run", "--invoke", "make-point(2, 3)"])
            .arg(&component)
            .output()
            .unwrap();
        let so = String::from_utf8_lossy(&run.stdout);
        let se = String::from_utf8_lossy(&run.stderr);
        assert!(
            run.status.success(),
            "wasmtime invoke failed: stdout={so} stderr={se}",
        );
        assert!(
            so.contains("x: 2") && so.contains("y: 3"),
            "make-point(2,3) must lift to {{x: 2, y: 3}}, got stdout={so} stderr={se}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice D.2): a component
/// export taking a flat-record PARAM (`fn area(p: Point) -> f64`,
/// `fn translate(p: Point, dx, dy) -> Point`) lowers via the canonical
/// trampoline — the record param flattens to its scalar fields, which the
/// trampoline reconstructs into the struct the Kāra fn expects. Validated
/// by `component new` + (when present) wasmtime invoke.
#[test]
fn wasm_wasi_component_exports_record_param() {
    let tmp = wasm_test_dir("component-recparam");
    let path = tmp.join("rplib.kara");
    std::fs::write(
        &path,
        r#"
#[derive(Copy, Clone)]
pub struct Point { x: f64, y: f64 }

#[target(wasm_wasi)]
pub fn area(p: Point) -> f64 {
    return p.x * p.y;
}

#[target(wasm_wasi)]
pub fn translate(p: Point, dx: f64, dy: f64) -> Point {
    return Point { x: p.x + dx, y: p.y + dy };
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_component_exports_record_param — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "component build failed: {stderr}");
    let component = tmp.join("rplib.wasm");
    assert_eq!(wasm_artifact_kind(&component), "component");
    if let Some(tool) = wasm_tools_on_path() {
        let wit = String::from_utf8_lossy(
            &std::process::Command::new(tool)
                .args(["component", "wit"])
                .arg(&component)
                .output()
                .unwrap()
                .stdout,
        )
        .into_owned();
        assert!(
            wit.contains("export area: func(p: point) -> f64;"),
            "record-param export must lift, got:\n{wit}",
        );
        assert!(
            wit.contains("export translate: func(p: point, dx: f64, dy: f64) -> point;"),
            "record-param-and-return export must lift, got:\n{wit}",
        );
    }
    if let Some(wt) = wasmtime_on_path() {
        let run = std::process::Command::new(wt)
            .args(["run", "--invoke", "area({x: 2, y: 3})"])
            .arg(&component)
            .output()
            .unwrap();
        let so = String::from_utf8_lossy(&run.stdout);
        assert!(
            run.status.success() && so.contains('6'),
            "area({{2,3}}) must be 6, got stdout={so} stderr={}",
            String::from_utf8_lossy(&run.stderr),
        );
        let run2 = std::process::Command::new(wt)
            .args(["run", "--invoke", "translate({x: 2, y: 3}, 10, 20)"])
            .arg(&component)
            .output()
            .unwrap();
        let so2 = String::from_utf8_lossy(&run2.stdout);
        assert!(
            run2.status.success() && so2.contains("x: 12") && so2.contains("y: 23"),
            "translate must be {{x:12,y:23}}, got stdout={so2} stderr={}",
            String::from_utf8_lossy(&run2.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice D.3): component
/// exports returning `Option[T]` / `Result[T, E]` over scalar inners lift
/// into the WIT world as idiomatic `option<T>` / `result<T, E>`. The
/// trampoline converts Kāra's `{i64 tag, i64 w0}` enum into the canonical
/// variant return area: discriminant remapped (Kāra `Result` is seeded
/// `Err=0,Ok=1` vs canonical `ok=0,err=1`; `Option` is identity) and the
/// payload's raw low bytes copied (no per-case branch). Validated by
/// wasmtime invoke across both arms of each.
#[test]
fn wasm_wasi_component_exports_option_result() {
    let tmp = wasm_test_dir("component-variant");
    let path = tmp.join("varlib.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
pub fn checked_div(a: i32, b: i32) -> Option[i32] {
    if b == 0 { return Option.None; }
    return Option.Some(a / b);
}

#[target(wasm_wasi)]
pub fn safe_sqrt(x: f64) -> Result[f64, i32] {
    if x < 0.0 { return Result.Err(1); }
    return Result.Ok(x);
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_component_exports_option_result — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "component build failed: {stderr}");
    let component = tmp.join("varlib.wasm");
    assert_eq!(wasm_artifact_kind(&component), "component");
    if let Some(tool) = wasm_tools_on_path() {
        let wit = String::from_utf8_lossy(
            &std::process::Command::new(tool)
                .args(["component", "wit"])
                .arg(&component)
                .output()
                .unwrap()
                .stdout,
        )
        .into_owned();
        assert!(
            wit.contains("export checked-div: func(a: s32, b: s32) -> option<s32>;"),
            "option export must lift, got:\n{wit}",
        );
        assert!(
            wit.contains("export safe-sqrt: func(x: f64) -> result<f64, s32>;"),
            "result export must lift, got:\n{wit}",
        );
    }
    if let Some(wt) = wasmtime_on_path() {
        let invoke = |args: &str| -> String {
            let o = std::process::Command::new(wt)
                .args(["run", "--invoke", args])
                .arg(&component)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "invoke {args} failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert_eq!(invoke("checked-div(10, 2)"), "some(5)");
        assert_eq!(invoke("checked-div(10, 0)"), "none");
        assert_eq!(invoke("safe-sqrt(4)"), "ok(4)");
        assert_eq!(invoke("safe-sqrt(-1)"), "err(1)");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM entry-point discovery" (sub-slice E): component exports
/// over `String` and scalar-element `Vec[T]` lift into the WIT world as
/// `string` / `list<T>`. The trampoline lifts a canonical `(ptr, len)`
/// slice into the Kāra `{ptr, len, cap}` value (the guest owns the
/// host-allocated bytes via `cabi_realloc`) and lowers the returned value
/// back to a `(ptr, len)` return area. Validated by wasmtime invoke.
#[test]
fn wasm_wasi_component_exports_string_and_list() {
    let tmp = wasm_test_dir("component-string-list");
    let path = tmp.join("slib.kara");
    std::fs::write(
        &path,
        r#"
#[target(wasm_wasi)]
pub fn shout(s: String) -> String {
    return s + "!";
}

#[target(wasm_wasi)]
pub fn make_range(n: i32) -> Vec[i32] {
    let mut out: Vec[i32] = Vec.new();
    let mut i = 0;
    while i < n {
        out.push(i);
        i += 1;
    }
    return out;
}

fn main() {}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_component_exports_string_and_list — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "component build failed: {stderr}");
    let component = tmp.join("slib.wasm");
    assert_eq!(wasm_artifact_kind(&component), "component");
    if let Some(tool) = wasm_tools_on_path() {
        let wit = String::from_utf8_lossy(
            &std::process::Command::new(tool)
                .args(["component", "wit"])
                .arg(&component)
                .output()
                .unwrap()
                .stdout,
        )
        .into_owned();
        assert!(
            wit.contains("export shout: func(s: string) -> string;"),
            "string export must lift, got:\n{wit}",
        );
        assert!(
            wit.contains("export make-range: func(n: s32) -> list<s32>;"),
            "list export must lift, got:\n{wit}",
        );
    }
    if let Some(wt) = wasmtime_on_path() {
        let invoke = |args: &str| -> String {
            let o = std::process::Command::new(wt)
                .args(["run", "--invoke", args])
                .arg(&component)
                .output()
                .unwrap();
            assert!(
                o.status.success(),
                "invoke {args} failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        assert_eq!(invoke("shout(\"hi\")"), "\"hi!\"");
        assert_eq!(invoke("make-range(4)"), "[0, 1, 2, 3]");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM concurrency lowering — sequential default", explicit
/// `par {}` leg: the block still lowers through `karac_par_run`
/// (`tests/wasm_codegen.rs` pins the IR shape), and the wasm runtime
/// archive's sequential body (`seq_par_run`) runs the branches in
/// source order on the calling thread. The output is therefore
/// **deterministic** — branch order then the post-join statement — where
/// the native pool's output would be racy. Same node:wasi embedder
/// shape as `wasm_wasi_build_and_run_e2e`.
#[test]
fn wasm_wasi_explicit_par_block_runs_sequentially() {
    let tmp = wasm_test_dir("par-seq");
    let path = tmp.join("par_seq.kara");
    std::fs::write(
        &path,
        r#"
fn main() {
    par {
        println(100);
        println(200);
        println(300);
    }
    println(999);
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_explicit_par_block_runs_sequentially — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("par_seq.wasm");
    assert!(wasm_path.exists(), "expected par_seq.wasm in the build cwd");

    let Some(node_out) = run_wasm_under_node(&tmp, &wasm_path) else {
        eprintln!("skip: wasm_wasi_explicit_par_block_runs_sequentially — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "par module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "100\n200\n300\n999\n",
        "par branches must run in source order, joined before the \
         following statement; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// phase-10 "WASM concurrency lowering — sequential default", `spawn` /
/// `TaskGroup` leg: the wasm archive's `seq_scheduler.rs` provides the
/// `karac_runtime_spawn` / `taskgroup_*` / `task_join` surface (absent
/// before this slice — spawn programs failed at wasm link on the
/// missing net-gated symbols). The program pins all three contract
/// points of the cooperative sequential scheduler:
///
/// 1. **spawn is deferred** — the `0` printed after the five
///    `tg.spawn(...)` calls appears BEFORE the workers' output: spawn
///    enqueues, it does not run inline;
/// 2. **the group drop drives the queue FIFO** — worker output arrives
///    in spawn order `1..=5` (deterministic, where native is racy), and
///    main's exit code pins that the join barrier completed;
/// 3. **`h.join()` transports the result** — the free-spawned task's
///    `40 + 2` crosses back through the handle's result buffer.
#[test]
fn wasm_wasi_spawn_taskgroup_sequential_e2e() {
    let tmp = wasm_test_dir("spawn-seq");
    let path = tmp.join("spawn_seq.kara");
    std::fs::write(
        &path,
        r#"
fn worker(id: i64) {
    println(id);
}

fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn main() {
    let h: TaskHandle[i64] = spawn(|| add(40, 2));
    let r: i64 = h.join();
    println(r);

    let mut tg = TaskGroup.new();
    tg.spawn(|| worker(1));
    tg.spawn(|| worker(2));
    tg.spawn(|| worker(3));
    tg.spawn(|| worker(4));
    tg.spawn(|| worker(5));
    println(0);
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_spawn_taskgroup_sequential_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("spawn_seq.wasm");
    assert!(
        wasm_path.exists(),
        "expected spawn_seq.wasm in the build cwd"
    );

    let Some(node_out) = run_wasm_under_node(&tmp, &wasm_path) else {
        eprintln!("skip: wasm_wasi_spawn_taskgroup_sequential_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "spawn module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "42\n0\n1\n2\n3\n4\n5\n",
        "expected join-transported 42, then the deferred-spawn sentinel 0 \
         BEFORE the workers, then FIFO worker order; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Strip-by-default: `karac build --target=wasm_*` runs `wasm-tools strip`
/// on every emitted `.wasm` artifact. The native link path strips by
/// default, but wasm-ld keeps the `.debug_*` DWARF sections — ~90%+ of an
/// unstripped module (a 482 KiB browser hello is 93% DWARF). Proven by the
/// ~10x size delta against the `KARAC_WASM_KEEP_DEBUG=1` opt-out build.
/// Uses the wasm_wasi component path, so `wasm_build_skip_reason` already
/// covers a missing wasm-tools / runtime archive / linker.
#[test]
fn wasm_build_strips_debug_info_by_default() {
    let tmp = wasm_test_dir("strip-default");
    let path = tmp.join("strip_hello.kara");
    std::fs::write(&path, "fn main() { println(\"hi\"); }\n").unwrap();
    let wasm_path = tmp.join("strip_hello.wasm");

    // Default build — stripped.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .env_remove("KARAC_WASM_KEEP_DEBUG")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_build_strips_debug_info_by_default — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "default wasm build failed: {stderr}");
    let stripped = std::fs::metadata(&wasm_path)
        .expect("expected strip_hello.wasm after default build")
        .len();

    // Opt-out build — DWARF retained.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .env("KARAC_WASM_KEEP_DEBUG", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "keep-debug wasm build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let unstripped = std::fs::metadata(&wasm_path)
        .expect("expected strip_hello.wasm after keep-debug build")
        .len();

    assert!(
        stripped < unstripped,
        "stripped build ({stripped} B) should be smaller than the KEEP_DEBUG \
         build ({unstripped} B)"
    );
    assert!(
        stripped.saturating_mul(3) < unstripped,
        "strip should remove the bulk of the module (DWARF is ~90%+): stripped \
         {stripped} B vs unstripped {unstripped} B — expected >3x reduction"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Two concurrent `karac build` invocations with the SAME source stem must
/// not clobber each other's scratch intermediates. Before the PID-scoped
/// scratch-path fix, `cmd_build` keyed them on the stem alone
/// (`/tmp/karac_<stem>.{o,core.wasm}`), so parallel builds raced — flaky
/// parallel `cargo test` wasm runs, and broken `make -j`. With the fix this
/// passes deterministically (distinct PIDs ⇒ distinct scratch paths); a
/// regression makes it fail probabilistically. Same-stem sources in separate
/// dirs, built simultaneously on two threads.
#[test]
fn concurrent_same_stem_wasm_builds_do_not_collide() {
    let prep = |tag: &str| {
        let dir = wasm_test_dir(tag);
        let src = dir.join("collide.kara"); // identical stem in both dirs
        std::fs::write(&src, "fn main() { println(\"x\"); }\n").unwrap();
        (dir, src)
    };
    let (d1, s1) = prep("collide-a");
    let (d2, s2) = prep("collide-b");

    // Probe build first so a missing archive / wasm-tools / linker skips
    // cleanly rather than failing as a "collision".
    let probe = karac_bin()
        .args(["build", s1.to_str().unwrap(), "--target=wasm_wasi"])
        .current_dir(&d1)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    if let Some(reason) = wasm_build_skip_reason(&String::from_utf8_lossy(&probe.stderr)) {
        eprintln!("skip: concurrent_same_stem_wasm_builds_do_not_collide — {reason}");
        let _ = std::fs::remove_dir_all(&d1);
        let _ = std::fs::remove_dir_all(&d2);
        return;
    }

    let build = |src: std::path::PathBuf, dir: std::path::PathBuf| {
        std::thread::spawn(move || {
            karac_bin()
                .args(["build", src.to_str().unwrap(), "--target=wasm_wasi"])
                .current_dir(&dir)
                .env_remove("KARAC_RUNTIME")
                .output()
                .unwrap()
        })
    };
    let h1 = build(s1, d1.clone());
    let h2 = build(s2, d2.clone());
    let o1 = h1.join().unwrap();
    let o2 = h2.join().unwrap();

    assert!(
        o1.status.success(),
        "concurrent build 1 failed: {}",
        String::from_utf8_lossy(&o1.stderr)
    );
    assert!(
        o2.status.success(),
        "concurrent build 2 failed: {}",
        String::from_utf8_lossy(&o2.stderr)
    );
    assert!(
        d1.join("collide.wasm").exists(),
        "build 1 produced no .wasm"
    );
    assert!(
        d2.join("collide.wasm").exists(),
        "build 2 produced no .wasm"
    );
    let _ = std::fs::remove_dir_all(&d1);
    let _ = std::fs::remove_dir_all(&d2);
}

/// The `Vector[T, N]` program the WASM SIMD-128 tests share (phase-10
/// "WASM SIMD-128 lowering"). The `a` lane inputs flow from function
/// params so the ops survive at `KARAC_OPT_LEVEL=0` (constant inputs
/// would fold at the default -O2 and the instruction-presence
/// assertions would test the optimizer, not the lowering); the `b`
/// vector is deliberately literal-built, riding the lane boundary
/// coercion (`coerce_scalar_to_type` — see
/// `tests/codegen.rs::test_vector_f32_literal_lanes_coerce_to_element_type`)
/// through the wasm pipeline. The `println(f32)` result also pins the
/// varargs float→double promotion wasm32's args-buffer lowering
/// exposed. Expected output: `22` then `12`.
const WASM_SIMD_VECTOR_PROGRAM: &str = r#"
fn lane_sum(p: i64, q: i64) -> i64 {
    let a: Vector[i64, 2] = Vector[i64, 2](p, p);
    let b: Vector[i64, 2] = Vector[i64, 2](q, q);
    let c = a + b;
    c[0] + c[1]
}

fn f32_fma(x: f32) -> f32 {
    let a: Vector[f32, 4] = Vector[f32, 4](x, x, x, x);
    let b: Vector[f32, 4] = Vector[f32, 4](0.5, 0.5, 0.5, 0.5);
    let c = a * b + a;
    c[0] + c[1] + c[2] + c[3]
}

fn main() {
    println(lane_sum(1, 10));
    println(f32_fma(2.0));
}
"#;

/// Run a core wasm module under node's built-in `node:wasi` preview-1
/// host (the same embedder shape as `wasm_wasi_build_and_run_e2e`).
/// `None` when node is not on PATH — callers skip the run leg. Node
/// enables WASM SIMD-128 unconditionally (it is WASM 2.0 baseline), so
/// this also exercises `v128`-carrying modules.
fn run_wasm_under_node(
    tmp: &std::path::Path,
    wasm: &std::path::Path,
) -> Option<std::process::Output> {
    let runner = tmp.join("run_wasi_simd.mjs");
    std::fs::write(
        &runner,
        "import { readFile } from 'node:fs/promises';\n\
         import { WASI } from 'node:wasi';\n\
         import { argv, exit } from 'node:process';\n\
         const wasi = new WASI({ version: 'preview1', args: [], env: {} });\n\
         const wasm = await WebAssembly.compile(await readFile(argv[2]));\n\
         const instance = await WebAssembly.instantiate(wasm, wasi.getImportObject());\n\
         exit(wasi.start(instance));\n",
    )
    .unwrap();
    std::process::Command::new("node")
        .arg(&runner)
        .arg(wasm)
        .current_dir(tmp)
        .output()
        .ok()
}

/// WASM SIMD-128 is the default lowering (phase-10 "WASM SIMD-128
/// lowering"; design.md § Portable SIMD — first-class lowering target):
/// with no feature flags at all, `Vector[i64, 2]` `+` and
/// `Vector[f32, 4]` `*`/`+` select single `v128` instructions
/// (`i64x2.add`, `f32x4.mul`, `f32x4.add`), and the module runs
/// correctly under a WASI host.
#[test]
fn wasm_simd128_default_lowers_vector_ops_to_v128() {
    let tmp = wasm_test_dir("simd_default");
    let path = tmp.join("vecsimd.kara");
    std::fs::write(&path, WASM_SIMD_VECTOR_PROGRAM).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .env("KARAC_OPT_LEVEL", "0")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_simd128_default_lowers_vector_ops_to_v128 — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm build failed: {stderr}");
    let wasm_path = tmp.join("vecsimd.wasm");
    assert!(wasm_path.exists(), "expected vecsimd.wasm in the build cwd");

    // Instruction-level assertion (deterministic, load-immune): the
    // disassembly must carry the single-instruction v128 forms the
    // phase-10 entry names.
    if let Some(tool) = wasm_tools_on_path() {
        let print = std::process::Command::new(tool)
            .arg("print")
            .arg(&wasm_path)
            .output()
            .unwrap();
        assert!(print.status.success(), "wasm-tools print failed");
        let wat = String::from_utf8_lossy(&print.stdout);
        for instr in ["i64x2.add", "f32x4.mul", "f32x4.add"] {
            assert!(
                wat.contains(instr),
                "expected `{instr}` in the disassembly — `+simd128` is \
                 the wasm default and `Vector` ops must select single \
                 v128 instructions",
            );
        }
    } else {
        eprintln!(
            "note: wasm_simd128_default_lowers_vector_ops_to_v128 — \
             wasm-tools not on PATH, instruction assertions skipped"
        );
    }

    let Some(node_out) = run_wasm_under_node(&tmp, &wasm_path) else {
        eprintln!("skip: wasm_simd128_default_lowers_vector_ops_to_v128 — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "v128 module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
    );
    assert_eq!(
        node_stdout, "22\n12\n",
        "vector arithmetic must match native semantics; stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--target-features=-simd128` disables the wasm SIMD-128 default
/// (last-wins over the `+simd128` table default): LLVM scalarizes every
/// vector op and the module stays MVP-clean — the portable-by-guarantee
/// escape for hosts without SIMD-128, which reject any `v128`-carrying
/// module at validation (the feature is module-granular, not
/// per-instruction). Same program, same output, no vector instructions;
/// and `-simd128,+simd128` re-enables (last-wins resolution).
#[test]
fn wasm_simd128_opt_out_scalarizes_module() {
    let tmp = wasm_test_dir("simd_optout");
    let path = tmp.join("vecscalar.kara");
    std::fs::write(&path, WASM_SIMD_VECTOR_PROGRAM).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
            "--target-features=-simd128",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .env("KARAC_OPT_LEVEL", "0")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_simd128_opt_out_scalarizes_module — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "scalar wasm build failed: {stderr}");
    let wasm_path = tmp.join("vecscalar.wasm");

    if let Some(tool) = wasm_tools_on_path() {
        let print = std::process::Command::new(tool)
            .arg("print")
            .arg(&wasm_path)
            .output()
            .unwrap();
        assert!(print.status.success(), "wasm-tools print failed");
        let wat = String::from_utf8_lossy(&print.stdout);
        for fragment in ["v128", "f32x4", "i64x2"] {
            assert!(
                !wat.contains(fragment),
                "`-simd128` must scalarize the whole module (MVP-clean); \
                 found `{fragment}` in the disassembly",
            );
        }
    }

    // Scalar lowering, same semantics.
    if let Some(node_out) = run_wasm_under_node(&tmp, &wasm_path) {
        let node_stdout = String::from_utf8_lossy(&node_out.stdout);
        let node_stderr = String::from_utf8_lossy(&node_out.stderr);
        assert!(
            node_out.status.success(),
            "scalar module failed under node:wasi: stdout={node_stdout} stderr={node_stderr}",
        );
        assert_eq!(node_stdout, "22\n12\n", "stderr={node_stderr}");
    } else {
        eprintln!(
            "note: wasm_simd128_opt_out_scalarizes_module — node not on PATH, run leg skipped"
        );
    }

    // Last-wins: a later `+simd128` in the user list re-enables.
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
            "--target-features=-simd128,+simd128",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .env("KARAC_OPT_LEVEL", "0")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "re-enable build failed: {stderr}");
    if let Some(tool) = wasm_tools_on_path() {
        let print = std::process::Command::new(tool)
            .arg("print")
            .arg(&wasm_path)
            .output()
            .unwrap();
        let wat = String::from_utf8_lossy(&print.stdout);
        assert!(
            wat.contains("i64x2.add"),
            "`-simd128,+simd128` must resolve last-wins to enabled",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `#[require_simd]` is honored on WASM the same way as native
/// (phase-10 "WASM SIMD-128 lowering"): under the `+simd128` default
/// the annotated function's `Vector[f32, 4]` ops classify Native and
/// the build proceeds; under `--target-features=-simd128` the target
/// has no vector unit, every vector op classifies Scalar, and the
/// build rejects with the targeted diagnostic.
#[test]
fn wasm_require_simd_fires_on_simd128_opt_out() {
    let tmp = wasm_test_dir("simd_require");
    let path = tmp.join("reqsimd.kara");
    std::fs::write(
        &path,
        r#"
#[require_simd]
fn f32_scale(x: f32, y: f32) -> f32 {
    let a: Vector[f32, 4] = Vector[f32, 4](x, x, x, x);
    let b: Vector[f32, 4] = Vector[f32, 4](y, y, y, y);
    let c = a * b;
    c[0] + c[1] + c[2] + c[3]
}

fn main() {
    println(f32_scale(2.0, 0.5));
}
"#,
    )
    .unwrap();

    // Opt-out leg: rejected before codegen with the no-vector-unit cause
    // and the actionable hint.
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
            "--target-features=-simd128",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("requires the llvm feature") {
        eprintln!("skip: wasm_require_simd_fires_on_simd128_opt_out — non-llvm karac");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "`#[require_simd]` + `-simd128` must reject the build; stderr={stderr}",
    );
    assert!(
        stderr.contains("E_REQUIRE_SIMD")
            && stderr.contains("wasm SIMD-128 is disabled by `-simd128`"),
        "expected the no-vector-unit require_simd diagnostic, got: {stderr}",
    );
    assert!(
        stderr.contains("drop `-simd128`"),
        "expected the re-enable hint, got: {stderr}",
    );

    // Default leg: `+simd128` is on, the same function classifies
    // Native, the guarantee holds, the build proceeds.
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_require_simd_fires_on_simd128_opt_out (default leg) — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "`#[require_simd]` must pass under the simd128 default: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Server-WASM `host fn` lowering E2E (phase-10): on `wasm_wasi` under
/// `--bindings none` (and browser),
/// `host fn` declarations lower to the same `kara_host` import entries
/// as the browser target — the "C-ABI call + thin shim" shape, where
/// the embedder's hand-rolled import object IS the shim. (Embedded
/// component builds — the wasm_wasi default — rename the imports to
/// canonical-ABI `kara:<pkg>/host` entries instead; see the
/// `bindings_explicit_component_emits_embedded_component` test.) No JS
/// glue is emitted; the WASI host instantiates with
/// `{ ...wasi.getImportObject(), kara_host: {...} }` (the hand-rolled
/// pattern design.md § Host Functions documents). Asserts:
///   - `WebAssembly.Module.imports` lists `kara_host.report` and
///     `kara_host.log_str` (the import-entry assertion lives in this
///     subprocess test — an in-process `set_active_target` would race
///     parallel codegen tests);
///   - i64 crosses as BigInt and the host's answer flows back into
///     guest arithmetic (report(21) → 42 printed by the guest);
///   - a real guest string crosses as `(ptr, len)`: ptr arrives as a
///     JS number (wasm32 pointers are i32-width scalars), i64 len as
///     BigInt, and the host decodes it byte-exactly from instance
///     memory — non-ASCII UTF-8 (`ā`) included;
///   - guest `println` still routes through genuine WASI fd_write
///     alongside the host-fn traffic.
#[test]
fn wasm_wasi_host_fn_e2e() {
    let tmp = wasm_test_dir("we2e-host");
    let path = tmp.join("hosted_wasi.kara");
    std::fs::write(
        &path,
        r#"
effect resource Reporter;

host fn report(x: i64) -> i64 with writes(Reporter);
host fn log_str(ptr: *const u8, len: i64) with writes(Reporter);

fn main() {
    let doubled = report(21);
    println(doubled);
    let msg = c"server-side k\u{101}ra guest";
    log_str(msg.as_ptr(), msg.len());
    println("done");
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        // `--bindings=none`: this test pins the raw-module embedder
        // shape — `kara_host` C-ABI imports hand-rolled by a node
        // `node:wasi` host (which runs core modules, not components;
        // the wasm_wasi default is now the embedded component, whose
        // imports carry canonical-ABI `kara:<pkg>/host` naming
        // instead).
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_host_fn_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm_wasi build failed: {stderr}");
    let wasm_path = tmp.join("hosted_wasi.wasm");
    assert!(wasm_path.exists(), "missing .wasm artifact");
    assert!(
        !tmp.join("hosted_wasi.js").exists(),
        "wasm_wasi must not emit JS glue — the embedder hand-rolls imports",
    );

    let runner = tmp.join("run_host.mjs");
    std::fs::write(
        &runner,
        r#"import { readFile } from "node:fs/promises";
import { WASI } from "node:wasi";
import { argv, exit } from "node:process";

const wasm = await WebAssembly.compile(await readFile(argv[2]));

// Import-entry assertion: both host fns must be genuine kara_host
// imports (both are called from the guest, so both must survive
// wasm-ld import-section GC).
const karaImports = WebAssembly.Module.imports(wasm).filter(
  (i) => i.module === "kara_host",
);
if (!karaImports.some((i) => i.name === "report" && i.kind === "function")) {
  throw new Error(
    "kara_host.report import entry missing: " + JSON.stringify(karaImports),
  );
}
if (!karaImports.some((i) => i.name === "log_str" && i.kind === "function")) {
  throw new Error(
    "kara_host.log_str import entry missing: " + JSON.stringify(karaImports),
  );
}

const wasi = new WASI({ version: "preview1", args: [], env: {} });
let instance;
const kara_host = {
  report(x) {
    if (typeof x !== "bigint") throw new Error("i64 must arrive as BigInt, got " + typeof x);
    return x * 2n;
  },
  log_str(ptr, len) {
    // wasm32 pointers are i32-width scalars → JS number; i64 len → BigInt.
    if (typeof ptr !== "number") throw new Error("ptr must arrive as number, got " + typeof ptr);
    if (typeof len !== "bigint") throw new Error("i64 len must arrive as BigInt, got " + typeof len);
    const bytes = new Uint8Array(instance.exports.memory.buffer, ptr, Number(len));
    console.log("host saw: " + new TextDecoder().decode(bytes));
  },
};
instance = await WebAssembly.instantiate(wasm, {
  ...wasi.getImportObject(),
  kara_host,
});
exit(wasi.start(instance));
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&runner)
        .arg(&wasm_path)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_wasi_host_fn_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "wasm module failed under node:wasi + kara_host: stdout={node_stdout} stderr={node_stderr}",
    );
    // `contains`, not exact-order equality: guest lines arrive via WASI
    // fd_write, the host line via console.log — both synchronous on a
    // pipe, but interleaving is not a contract worth pinning.
    assert!(
        node_stdout.contains("42\n"),
        "host i64 answer must flow back into guest arithmetic: {node_stdout}",
    );
    assert!(
        node_stdout.contains("host saw: server-side kāra guest"),
        "(ptr, len) string must decode byte-exactly on the host: {node_stdout}",
    );
    assert!(
        node_stdout.contains("done\n"),
        "guest println must still route through WASI fd_write: {node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The differential the host-fn lowering must preserve: a plain
/// `extern "C"` declaration gets NO import attributes, so an
/// unresolved one is still a loud undefined-symbol link error on
/// `wasm_wasi` — only `host fn` opts into staying undefined as a
/// `kara_host` import. (Guards against the lowering condition ever
/// widening from the host ABI to all externs.)
#[test]
fn wasm_wasi_extern_c_stays_loud_undefined() {
    let tmp = wasm_test_dir("we2e-extc");
    let path = tmp.join("loud.kara");
    std::fs::write(
        &path,
        r#"
unsafe extern "C" {
    fn karac_test_totally_absent_symbol(x: i64) -> i64;
}

fn main() {
    println(karac_test_totally_absent_symbol(1));
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        // `--bindings=none` keeps this a pure link-diagnostic test —
        // the component default would also resolve wasm-tools, an
        // unrelated infrastructure dependency.
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wasi_extern_c_stays_loud_undefined — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "an unresolved extern \"C\" must fail the wasm link",
    );
    assert!(
        stderr.contains("karac_test_totally_absent_symbol"),
        "link error must name the undefined symbol: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: browser-WASM build path (`--target=wasm_browser`) ─────
//
// Browser builds emit the same wasip1 module flavor as `wasm_wasi`
// plus a `<stem>.js` ES-module glue file (host fn import plumbing
// under the `kara_host` namespace + an inline WASI preview-1
// polyfill) and `<stem>.d.ts` TypeScript declarations. Project mode
// lands the same set as `dist/wasm/<pkg>.{wasm,js,d.ts}` with the
// package name from `kara.toml` (the "WASM browser artifact emission"
// entry). Same infrastructure skips as the wasi tests above.

/// Write a two-module wasm project fixture: the entry calls a host fn
/// declared in a non-entry module, so the merged super-program (not
/// just the entry file) must feed glue/declaration generation.
fn write_wasm_project_fixture(tmp: &std::path::Path, pkg: &str) {
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        format!("[package]\nname = \"{pkg}\"\n"),
    )
    .unwrap();
    std::fs::write(
        tmp.join("src/main.kara"),
        "import metrics.emit_answer;\n\n\
         fn main() {\n    emit_answer();\n    println(\"done\");\n}\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("src/metrics.kara"),
        "effect resource Reporter;\n\n\
         host fn report(value: i64) -> i64 with writes(Reporter);\n\n\
         pub fn emit_answer() with writes(Reporter) {\n    report(42);\n}\n",
    )
    .unwrap();
}

/// Project-mode `--target=wasm_browser` (phase-10 "WASM browser
/// artifact emission"): super-program codegen + wasm-ld land the module
/// in the `dist/wasm/<pkg>.wasm` layout — package name from
/// `kara.toml`, not a source-file stem — plus the `<pkg>.js` glue and
/// `<pkg>.d.ts` TypeScript declarations under the inferred browser
/// bindings. The host fn lives in a non-entry module, pinning that the
/// merged super-program drives glue + declaration generation.
#[test]
fn wasm_browser_project_mode_emits_dist_artifacts() {
    let tmp = wasm_test_dir("bprojart");
    write_wasm_project_fixture(&tmp, "webapp");

    let out = karac_bin()
        .args(["build", "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_project_mode_emits_dist_artifacts — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "project wasm build failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("webapp.wasm")
            && stdout.contains("webapp.js")
            && stdout.contains("webapp.d.ts"),
        "Built line must name all three artifacts, got: {stdout}",
    );
    let dist = tmp.join("dist").join("wasm");
    assert!(dist.join("webapp.wasm").exists(), "missing dist .wasm");
    let glue = std::fs::read_to_string(dist.join("webapp.js")).expect("missing dist .js glue");
    assert!(
        glue.contains("const WASM_FILENAME = \"webapp.wasm\";"),
        "glue must reference the sibling module by package name",
    );
    assert!(
        glue.contains("const DECLARED_IMPORTS = [\"report\"];"),
        "host fn from the non-entry module must reach the glue's import list",
    );
    let dts = std::fs::read_to_string(dist.join("webapp.d.ts")).expect("missing dist .d.ts");
    assert!(
        dts.contains("report(value: bigint, ctx: HostCtx): bigint;"),
        "d.ts must type the host fn per the boundary contract (i64 ⇒ bigint), got:\n{dts}",
    );
    assert!(
        dts.contains("hostImpls: HostImpls") && !dts.contains("hostImpls?: HostImpls"),
        "declared host fns make the hostImpls parameter required",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Project-mode browser E2E: the `dist/wasm/<pkg>.js` glue runs the
/// sibling module under node — the glue resolves `WASM_FILENAME`
/// against `import.meta.url`, so the `dist/wasm/` directory must be
/// self-contained (importable from outside it without configuration).
#[test]
fn wasm_browser_project_mode_run_e2e() {
    let tmp = wasm_test_dir("bproje2e");
    write_wasm_project_fixture(&tmp, "webapp");

    let out = karac_bin()
        .args(["build", "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_project_mode_run_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "project wasm build failed: {stderr}");

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./dist/wasm/webapp.js";
const calls = [];
await run({
  report(x, ctx) {
    if (typeof x !== "bigint") throw new Error("i64 must arrive as BigInt");
    if (!ctx || typeof ctx.readString !== "function") throw new Error("ctx missing");
    calls.push(x);
    return x * 2n;
  },
});
if (calls.length !== 1 || calls[0] !== 42n) {
  throw new Error("bad call sequence: " + calls.map(String).join(","));
}
console.log("PROJ_E2E_OK");
"#,
    )
    .unwrap();

    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_browser_project_mode_run_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "project glue harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("done\n") && node_stdout.contains("PROJ_E2E_OK"),
        "harness assertions failed: stdout={node_stdout} stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Project-mode `--bindings=none` (phase-10 "WASM raw artifact
/// emission"): only `dist/wasm/<pkg>.wasm` is emitted — no glue, no
/// declarations — for users wrapping Kāra WASM with custom host
/// integration.
#[test]
fn wasm_project_bindings_none_emits_raw_module_only() {
    let tmp = wasm_test_dir("bprojraw");
    write_wasm_project_fixture(&tmp, "rawpkg");

    let out = karac_bin()
        .args(["build", "--target=wasm_browser", "--bindings=none"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_project_bindings_none_emits_raw_module_only — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "project raw build failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("rawpkg.wasm")
            && !stdout.contains("rawpkg.js")
            && !stdout.contains("rawpkg.d.ts"),
        "Built line must name only the .wasm, got: {stdout}",
    );
    let dist = tmp.join("dist").join("wasm");
    assert!(dist.join("rawpkg.wasm").exists(), "missing dist .wasm");
    assert!(
        !dist.join("rawpkg.js").exists()
            && !dist.join("rawpkg.d.ts").exists()
            && !dist.join("rawpkg.component.wit").exists(),
        "--bindings=none must emit neither glue, declarations, nor WIT descriptor",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Project-mode `--target=wasm_wasi` with the *inferred* component
/// default emits a single embedded-WIT component at
/// `dist/wasm/<pkg>.wasm` (phase-10 "embedded-WIT migration": the
/// paired `.component.wit` sidecar is gone from this mode; the
/// artifact is self-describing). The host fn lives in a non-entry
/// module, pinning that the merged super-program feeds the embedded
/// world — its import must carry the canonical-ABI
/// `kara:<pkg>/host` naming, not the C-ABI `kara_host` shape.
#[test]
fn wasm_project_wasi_emits_embedded_component() {
    let tmp = wasm_test_dir("bprojwasi");
    write_wasm_project_fixture(&tmp, "wasipkg");

    let out = karac_bin()
        .args(["build", "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_project_wasi_emits_embedded_component — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "project wasi build failed: {stderr}");
    assert!(
        !stderr.contains("deprecated"),
        "inferred component default must stay notice-free, got: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("wasipkg.wasm") && !stdout.contains("wasipkg.component.wit"),
        "Built line must name only the single component artifact, got: {stdout}",
    );
    let dist = tmp.join("dist").join("wasm");
    let component = dist.join("wasipkg.wasm");
    assert!(component.exists(), "missing dist .wasm");
    assert_eq!(
        wasm_artifact_kind(&component),
        "component",
        "the artifact must be a Component Model component, not a core module",
    );
    assert!(
        !dist.join("wasipkg.component.wit").exists()
            && !dist.join("wasipkg.js").exists()
            && !dist.join("wasipkg.d.ts").exists(),
        "embedded component bindings must emit no companion artifacts",
    );
    // Deeper WIT round-trip when wasm-tools is available: the embedded
    // world must import the host interface under canonical-ABI naming
    // with the boundary-typed signature from the non-entry module.
    if let Some(tool) = wasm_tools_on_path() {
        let wit_dump = std::process::Command::new(tool)
            .args(["component", "wit"])
            .arg(&component)
            .output()
            .unwrap();
        assert!(
            wit_dump.status.success(),
            "wasm-tools component wit must round-trip the artifact: {}",
            String::from_utf8_lossy(&wit_dump.stderr)
        );
        let wit = String::from_utf8_lossy(&wit_dump.stdout);
        assert!(
            wit.contains("import kara:wasipkg/host;"),
            "embedded world must import the canonical-ABI host instance, got:\n{wit}",
        );
        assert!(
            wit.contains("report: func(value: s64) -> s64;"),
            "host fn from the non-entry module must reach the embedded WIT, got:\n{wit}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A `[toolchain] wasm-tools` pin that doesn't match the discovered
/// binary is a hard error naming both versions, before any artifact is
/// emitted — the pin exists for reproducible builds, so drift must not
/// silently componentize with whatever is on PATH.
#[test]
fn wasm_project_toolchain_pin_mismatch_fails() {
    let tmp = wasm_test_dir("bprojpin");
    write_wasm_project_fixture(&tmp, "pinpkg");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"pinpkg\"\n\n[toolchain]\nwasm-tools = \"0.0.0-bogus\"\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", "--target=wasm_wasi"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_project_toolchain_pin_mismatch_fails — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "a pin mismatch must fail the build, got success",
    );
    assert!(
        stderr.contains("version mismatch") && stderr.contains("0.0.0-bogus"),
        "error must name the pinned version, got: {stderr}",
    );
    assert!(
        !tmp.join("dist").join("wasm").join("pinpkg.wasm").exists(),
        "no artifact may be emitted on a pin mismatch",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Project-mode wasm gate: a program reaching a host resource the
/// target can't provide (`FileSystem` on `wasm_browser`) fails the
/// build at the effect phase — project builds treat effect errors as
/// fatal — and no `dist/wasm/` artifacts appear.
#[test]
fn wasm_project_gate_violation_aborts() {
    let tmp = wasm_test_dir("bprojgate");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(tmp.join("kara.toml"), "[package]\nname = \"gated\"\n").unwrap();
    std::fs::write(
        tmp.join("src/main.kara"),
        "pub fn save() with writes(FileSystem) {\n    let _x = 1;\n}\n\n\
         fn main() {\n    save();\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("requires the llvm feature") {
        eprintln!("skip: wasm_project_gate_violation_aborts — non-llvm karac");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success(), "gate violation must abort: {stderr}");
    assert!(
        stderr.contains("`wasm_browser` does not provide resource 'FileSystem'"),
        "expected the target-gate diagnostic, got: {stderr}",
    );
    let dist = tmp.join("dist").join("wasm");
    assert!(
        !dist.join("gated.wasm").exists() && !dist.join("gated.js").exists(),
        "no artifact may be produced on a gate violation",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A `wasm_browser` build emits all THREE artifacts — `<stem>.wasm`,
/// the `<stem>.js` glue, and the `<stem>.d.ts` TypeScript declarations
/// — even for a program declaring no host fns (empty
/// `DECLARED_IMPORTS`; the WASI polyfill is what makes a plain wasip1
/// module run in a browser host at all, and the d.ts declares the glue
/// module's own surface with an optional `hostImpls`).
#[test]
fn wasm_browser_build_emits_wasm_and_js() {
    let tmp = wasm_test_dir("bemit");
    let path = tmp.join("plain.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_build_emits_wasm_and_js — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm_browser build failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Built: plain.wasm + plain.js + plain.d.ts"),
        "Built line must name all three artifacts, got: {stdout}",
    );
    assert!(tmp.join("plain.wasm").exists(), "missing .wasm artifact");
    let glue = std::fs::read_to_string(tmp.join("plain.js")).expect("missing .js glue");
    assert!(
        glue.contains("const DECLARED_IMPORTS = [];"),
        "no-host-fn program must bake an empty import list",
    );
    assert!(
        glue.contains("wasi_snapshot_preview1"),
        "glue must carry the WASI polyfill",
    );
    let dts = std::fs::read_to_string(tmp.join("plain.d.ts")).expect("missing .d.ts");
    assert!(
        dts.contains("export function run(") && dts.contains("hostImpls?: HostImpls"),
        "no-host-fn d.ts must declare the glue surface with optional hostImpls, got:\n{dts}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// E0411 differential: `FileSystem` is in `wasm_wasi`'s provided set
/// but NOT in `wasm_browser`'s — a program reaching it from `main`
/// aborts the browser build with the targeted diagnostic and produces
/// no artifacts (neither `.wasm` nor `.js`).
#[test]
fn wasm_browser_build_aborts_on_target_gate_violation() {
    let tmp = wasm_test_dir("bgate");
    let path = tmp.join("gated.kara");
    std::fs::write(
        &path,
        "pub fn save() with writes(FileSystem) {\n    let _x = 1;\n}\n\n\
         fn main() {\n    save();\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_build_aborts_on_target_gate_violation — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success(), "gate violation must abort: {stderr}");
    assert!(
        stderr.contains("E0411")
            && stderr.contains("`wasm_browser` does not provide resource 'FileSystem'"),
        "expected the E0411 target-gate abort, got: {stderr}",
    );
    assert!(
        !tmp.join("gated.wasm").exists() && !tmp.join("gated.js").exists(),
        "no artifact may be produced on a gate violation",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Full browser-path E2E: `host fn` declarations become WASM import
/// entries under `kara_host`, and the emitted JS glue runs the module
/// under node ≥ 18 (same WebAssembly + ES-module surface as a
/// browser; the glue's default loader takes its `file:` branch).
/// Asserts, in one harness run:
///   - i64 params/returns cross as BigInt, values correct BOTH
///     directions across three chained calls (21 → 42 → 84);
///   - every host impl receives the trailing `ctx` argument
///     (`{ memory, readString }`);
///   - `WebAssembly.Module.imports` lists `kara_host.report` — the
///     import-entry assertion lives here (the karac subprocess owns
///     its process-global active target; an in-process IR test would
///     race parallel codegen tests);
///   - a real guest string crosses the boundary: the Kāra side passes
///     `c"..."` through `log_str(msg.as_ptr(), msg.len())` and the host
///     impl decodes it byte-exactly (non-ASCII UTF-8 included) with
///     `ctx.readString` — the pointer arrives as a JS number (wasm32
///     pointers are i32-width scalars), the i64 len as BigInt;
///   - `readString` also decodes a (ptr, len) pair against synthetic
///     memory (standalone-helper check, kept from the pre-`as_ptr` era
///     when no pointer-producer surface existed);
///   - missing host impls reject loudly BEFORE any wasm runs, naming
///     the missing fn;
///   - `println` output routes through the polyfill's fd_write to the
///     console.
#[test]
fn wasm_browser_build_and_run_e2e() {
    let tmp = wasm_test_dir("be2e");
    let path = tmp.join("hosted.kara");
    std::fs::write(
        &path,
        r#"
effect resource Reporter;

host fn report(x: i64) -> i64 with writes(Reporter);
host fn log_str(ptr: *const u8, len: i64) with writes(Reporter);

fn main() {
    let doubled = report(21);
    let tripled = report(doubled);
    report(tripled);
    let msg = c"hello from k\u{101}ra guest";
    log_str(msg.as_ptr(), msg.len());
    println("done");
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_browser_build_and_run_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm_browser build failed: {stderr}");
    assert!(tmp.join("hosted.wasm").exists(), "missing .wasm artifact");
    assert!(tmp.join("hosted.js").exists(), "missing .js glue");

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run, readString } from "./hosted.js";
import { readFile } from "node:fs/promises";

// Missing-impl loudness: instantiation must reject BEFORE any wasm
// runs, naming the absent host fn.
let missingCaught = "";
try {
  await run({ report: (x) => x });
} catch (e) {
  missingCaught = e.message;
}
if (!missingCaught.includes("log_str")) {
  throw new Error("missing-impl error must name log_str, got: " + missingCaught);
}

const calls = [];
const logged = [];
await run({
  report(x, ctx) {
    if (typeof x !== "bigint") throw new Error("i64 must arrive as BigInt");
    if (!ctx || typeof ctx.readString !== "function" || !ctx.memory) {
      throw new Error("trailing ctx argument missing");
    }
    calls.push(x);
    return x * 2n;
  },
  log_str(ptr, len, ctx) {
    // wasm32 pointers are i32-width scalars → JS number; i64 len → BigInt.
    if (typeof ptr !== "number") throw new Error("ptr must arrive as number, got " + typeof ptr);
    if (typeof len !== "bigint") throw new Error("i64 len must arrive as BigInt, got " + typeof len);
    logged.push(ctx.readString(ptr, len));
  },
});
if (calls.length !== 3 || calls[0] !== 21n || calls[1] !== 42n || calls[2] !== 84n) {
  throw new Error("bad call sequence: " + calls.map(String).join(","));
}
// Real guest string through kara_host: c"..." → as_ptr/len → readString.
// Byte-exact, including the non-ASCII "ā" (multi-byte UTF-8 crosses intact).
if (logged.length !== 1 || logged[0] !== "hello from kāra guest") {
  throw new Error("bad log_str round-trip: " + JSON.stringify(logged));
}

// Import-entry assertion: kara_host.report and kara_host.log_str must
// both be genuine WASM imports (log_str is now called from the guest,
// so it must survive wasm-ld import-section GC).
const mod = await WebAssembly.compile(await readFile("./hosted.wasm"));
const karaImports = WebAssembly.Module.imports(mod).filter(
  (i) => i.module === "kara_host",
);
if (!karaImports.some((i) => i.name === "report" && i.kind === "function")) {
  throw new Error(
    "kara_host.report import entry missing: " + JSON.stringify(karaImports),
  );
}
if (!karaImports.some((i) => i.name === "log_str" && i.kind === "function")) {
  throw new Error(
    "kara_host.log_str import entry missing: " + JSON.stringify(karaImports),
  );
}

// readString against synthetic memory (standalone-helper check, kept
// from the pre-as_ptr era; the real-memory path is asserted above).
const synth = { buffer: new TextEncoder().encode("xxhello").buffer };
if (readString(synth, 2, 5) !== "hello") throw new Error("readString broken");

console.log("E2E_OK");
"#,
    )
    .unwrap();

    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_browser_build_and_run_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "glue harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("done\n"),
        "println must route through the polyfill's fd_write: {node_stdout}",
    );
    assert!(
        node_stdout.contains("E2E_OK"),
        "harness assertions failed: stdout={node_stdout} stderr={node_stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The `examples/ssr_counter` SSR example, driven end-to-end on BOTH
/// targets from the one shipped source file — the worked example for
/// design.md § Cross-target Compilation's provider-injection pattern
/// (phase-10-targets.md "SSR provider-injection pattern"). Building the
/// shipped file (not an inline fixture) makes this a bit-rot guard: if
/// the example stops compiling or its output drifts, this test fails.
///
/// Asserts, in one harness run:
///   - the `native` leg builds and the server renders the component to
///     the exact HTML body on stdout (the static heading + the dynamic
///     count/parity, count=42 ⇒ even);
///   - the `wasm_browser` leg builds (.wasm + .js + .d.ts), and the d.ts
///     types the export (`hydrate`) and the DOM host fns
///     (`dom_set_count` / `dom_set_parity`);
///   - the shipped `run_browser.mjs` hydrates under node against a mock
///     DOM — the SHARED component, run through the `DomSink` provider,
///     drives the host fns that mutate the mock (load-immune: the mock
///     ends at count=10/even after a re-render).
#[test]
fn ssr_counter_example_dual_target_e2e() {
    let example_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/ssr_counter");
    let tmp = wasm_test_dir("ssr-counter");
    for f in ["ssr_counter.kara", "run_browser.mjs"] {
        std::fs::copy(example_dir.join(f), tmp.join(f)).unwrap_or_else(|e| panic!("copy {f}: {e}"));
    }
    let src = tmp.join("ssr_counter.kara");

    // ── Server leg (native): render the component to HTML on stdout ──
    let nat = karac_bin()
        .args(["build", src.to_str().unwrap()])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let nat_err = String::from_utf8_lossy(&nat.stderr);
    // Soft-skip like the other native-build E2Es. Two no-binary cases:
    //  - no native runtime archive in this environment (link/codegen failed);
    //  - karac built WITHOUT the llvm feature (plain `cargo test`), which
    //    type-checks and emits NO executable ("requires the llvm feature") yet
    //    exits 0 — so guard on the binary's existence, mirroring the
    //    `requires the llvm feature || !exe.exists()` pattern used elsewhere.
    if (!nat.status.success()
        && (nat_err.contains("link failed") || nat_err.contains("codegen failed")))
        || nat_err.contains("requires the llvm feature")
        || !tmp.join("ssr_counter").exists()
    {
        eprintln!("skip: ssr_counter_example_dual_target_e2e — no native binary (llvm/runtime unavailable)");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(nat.status.success(), "native build failed: {nat_err}");
    let server = std::process::Command::new(tmp.join("ssr_counter"))
        .current_dir(&tmp)
        .output()
        .expect("run server binary");
    let rendered = String::from_utf8_lossy(&server.stdout);
    assert_eq!(
        rendered,
        "<h1>Kāra SSR Counter</h1><output id=\"count\">42</output><span id=\"parity\">even</span>\n",
        "server must render the component to the expected HTML body",
    );

    // ── Client leg (wasm_browser): build + d.ts shape ───────────────
    let web = karac_bin()
        .args(["build", src.to_str().unwrap(), "--target=wasm_browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let web_err = String::from_utf8_lossy(&web.stderr);
    if let Some(reason) = wasm_build_skip_reason(&web_err) {
        eprintln!("skip: ssr_counter_example_dual_target_e2e (client leg) — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(web.status.success(), "wasm_browser build failed: {web_err}");
    for art in ["ssr_counter.wasm", "ssr_counter.js", "ssr_counter.d.ts"] {
        assert!(tmp.join(art).exists(), "missing artifact: {art}");
    }
    let dts = std::fs::read_to_string(tmp.join("ssr_counter.d.ts")).unwrap();
    assert!(
        dts.contains("hydrate(count: bigint): bigint;"),
        "d.ts must type the exported entry point, got:\n{dts}",
    );
    assert!(
        dts.contains("dom_set_count(value: bigint, ctx: HostCtx): void;")
            && dts.contains("dom_set_parity(value: bigint, ctx: HostCtx): void;"),
        "d.ts must type the DOM host fns, got:\n{dts}",
    );

    // ── Client leg under node: hydrate a mock DOM ───────────────────
    let node = std::process::Command::new("node")
        .arg("run_browser.mjs")
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: ssr_counter_example_dual_target_e2e (node leg) — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("HYDRATED {\"count\":10,\"parity\":\"even\"}"),
        "hydration harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: `karac build --bindings` flag ─────────────────────────
//
// WASM output-shape selector (design.md § Target Build Artifacts).
// Default is inferred from the target (wasm_browser → browser,
// wasm_wasi → component — covered implicitly by the emission tests
// above); these tests cover the explicit spellings, the value
// validation, and the non-WASM inertness.

/// An unknown `--bindings` value is a parse-level hard error listing
/// the closed valid set — it must not silently fall back to the
/// target-inferred default. Fires before any file is read, so no
/// wasm infrastructure (or even the named file) is needed.
#[test]
fn bindings_flag_unknown_value_rejected() {
    let out = karac_bin()
        .args(["build", "x.kara", "--bindings=xml"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown --bindings value 'xml'")
            && stderr.contains("browser, component, or none"),
        "expected the valid-set listing, got: {stderr}",
    );
}

/// Space-separated `--bindings` with no following value rejects
/// loudly (mirrors `--target`'s missing-value diagnostic).
#[test]
fn bindings_flag_missing_value_rejected() {
    let out = karac_bin()
        .args(["build", "x.kara", "--bindings"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("--bindings requires a value"),
        "expected the missing-value diagnostic, got: {stderr}",
    );
}

/// `--target=wasm_browser --bindings=none` suppresses the glue: only
/// the raw `.wasm` is emitted, and the `Built:` line names a single
/// artifact ("raw" for browser = no `<stem>.js`, per the phase-10
/// raw-artifact entry).
#[test]
fn bindings_none_suppresses_browser_glue() {
    let tmp = wasm_test_dir("bnone");
    let path = tmp.join("rawmod.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: bindings_none_suppresses_browser_glue — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "build failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Built: rawmod.wasm") && !stdout.contains("rawmod.js"),
        "Built line must name only the .wasm, got: {stdout}",
    );
    assert!(tmp.join("rawmod.wasm").exists(), "missing .wasm artifact");
    assert!(
        !tmp.join("rawmod.js").exists()
            && !tmp.join("rawmod.d.ts").exists()
            && !tmp.join("rawmod.component.wit").exists(),
        "--bindings=none must emit neither glue, declarations, nor WIT descriptor",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--target=wasm_wasi --bindings browser` (space form, the design.md
/// spelling) opts a wasi module into the ES-module glue: both wasm
/// targets lower `host fn` to the same `kara_host` import entries, so
/// the glue (with its inline WASI polyfill) is target-agnostic.
#[test]
fn bindings_browser_on_wasi_emits_glue() {
    let tmp = wasm_test_dir("bwasi");
    let path = tmp.join("wglue.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings",
            "browser",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: bindings_browser_on_wasi_emits_glue — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "build failed: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Built: wglue.wasm + wglue.js + wglue.d.ts"),
        "Built line must name all three artifacts, got: {stdout}",
    );
    let glue = std::fs::read_to_string(tmp.join("wglue.js")).expect("missing .js glue");
    assert!(
        glue.contains("wasi_snapshot_preview1"),
        "glue must carry the WASI polyfill",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Explicit `--bindings=component` emits a single embedded-WIT
/// component (phase-10 "embedded-WIT migration"): `<stem>.wasm` IS the
/// component — no `.component.wit` sidecar, no deprecation notice —
/// with `host fn` imports lowered to the canonical-ABI
/// `kara:<pkg>/host` instance the embedded world declares.
#[test]
fn bindings_explicit_component_emits_embedded_component() {
    let tmp = wasm_test_dir("bcomp");
    let path = tmp.join("compmod.kara");
    std::fs::write(
        &path,
        "effect resource Reporter;\n\n\
         host fn report(value: i64) -> i64 with writes(Reporter);\n\n\
         fn main() {\n    report(42);\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--bindings=component",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: bindings_explicit_component_emits_embedded_component — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "build failed: {stderr}");
    assert!(
        !stderr.contains("deprecated"),
        "the embedded default carries no deprecation notice, got: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Built: compmod.wasm") && !stdout.contains("compmod.component.wit"),
        "Built line must name only the single component artifact, got: {stdout}",
    );
    let component = tmp.join("compmod.wasm");
    assert!(component.exists(), "missing .wasm artifact");
    assert_eq!(
        wasm_artifact_kind(&component),
        "component",
        "the artifact must be a Component Model component, not a core module",
    );
    assert!(
        !tmp.join("compmod.component.wit").exists()
            && !tmp.join("compmod.js").exists()
            && !tmp.join("compmod.d.ts").exists(),
        "embedded component bindings must emit no companion artifacts",
    );
    if let Some(tool) = wasm_tools_on_path() {
        let wit_dump = std::process::Command::new(tool)
            .args(["component", "wit"])
            .arg(&component)
            .output()
            .unwrap();
        assert!(
            wit_dump.status.success(),
            "wasm-tools component wit must round-trip the artifact: {}",
            String::from_utf8_lossy(&wit_dump.stderr)
        );
        let wit = String::from_utf8_lossy(&wit_dump.stdout);
        assert!(
            wit.contains("import kara:compmod/host;"),
            "embedded world must import the canonical-ABI host instance, got:\n{wit}",
        );
        assert!(
            wit.contains("report: func(value: s64) -> s64;"),
            "host fn must map per the WIT boundary contract (i64 ⇒ s64), got:\n{wit}",
        );
        assert!(
            wit.contains("export wasi:cli/run@"),
            "the adapter must synthesize the command entry point, got:\n{wit}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--bindings=component-paired` is GONE — the pre-embedded-WIT paired
/// shape (C-ABI core module + `<stem>.component.wit` descriptor) was
/// removed pre-first-release per the one-release deprecation contract
/// (design.md § Target Build Artifacts; no release ever carried the
/// spelling). The old spelling must now be the parse-level unknown-
/// value hard error listing the closed three-value set — not a silent
/// fallback to the embedded default. Fires before any file is read,
/// so no wasm infrastructure is needed.
#[test]
fn bindings_component_paired_spelling_removed() {
    let out = karac_bin()
        .args(["build", "x.kara", "--bindings=component-paired"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown --bindings value 'component-paired'")
            && stderr.contains("browser, component, or none"),
        "the removed spelling must hit the unknown-value error with the closed set, got: {stderr}",
    );
}

/// `--bindings` on a non-WASM target is accepted-but-inert (the
/// tracker entry's "ignored on a non-WASM target"): the build
/// proceeds and no glue appears. Passes on both the llvm path (real
/// native binary) and the non-llvm check fallback — neither emits a
/// `.js`.
#[test]
fn bindings_ignored_on_non_wasm_target() {
    let tmp = wasm_test_dir("bnative");
    let path = tmp.join("natbind.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();

    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--bindings=browser"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success()
        && (stderr.contains("link failed") || stderr.contains("codegen failed"))
    {
        // Native link can fail in environments without the native runtime
        // archive — an unrelated cause (the release-strip E2E above skips
        // the same way). The flag already cleared arg parsing unrejected,
        // which is half the substance; soft-skip the rest.
        eprintln!("skip: bindings_ignored_on_non_wasm_target — native link unavailable");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "native build failed: {stderr}");
    assert!(
        !tmp.join("natbind.js").exists(),
        "--bindings must be inert on a non-WASM target",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: `--target-cpu` override ───────────────────────────────
//
// CPU baseline override (design.md § CPU Baseline Targeting).
// Precedence: `--target-cpu` flag > `KARAC_TARGET_CPU` env >
// `[release] target-cpu` in kara.toml > the per-target default table.
// Validation is per-active-target against LLVM's CPU registry; an
// unknown name is a hard error carrying the supported listing (LLVM's
// native behavior — warn and silently fall back to generic — is
// exactly what the validation closes). Validation runs before the
// pipeline, so most tests here need no runtime archive: a bogus name
// at any precedence tier fails fast, which also makes tier order
// observable without inspecting codegen output.

/// Skip helper: the `--target-cpu` surface (help listing + validation)
/// rides the llvm build path; the non-llvm fallback accepts the flag
/// inert, so these assertions are vacuous there.
fn target_cpu_skip_reason(stderr: &str) -> Option<&'static str> {
    if stderr.contains("requires the llvm feature") {
        return Some("karac built without --features llvm");
    }
    None
}

/// `--target-cpu=help` prints LLVM's supported-CPU listing for the
/// active target and exits 0 (mirrors `rustc -C target-cpu=help`) —
/// in single-file form, and in the no-file project form even outside
/// any project (the listing needs only the active target, so it's
/// handled before manifest discovery).
#[test]
fn target_cpu_help_lists_cpus() {
    let tmp = wasm_test_dir("cpuhelp");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-cpu=help"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_help_lists_cpus — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "help listing must exit 0: {stderr}");
    assert!(
        stderr.contains("Available CPUs for this target:") && stderr.contains("generic"),
        "expected LLVM's CPU table on stderr, got: {stderr}",
    );

    // No-file form, from a directory with no kara.toml anywhere
    // relevant: still lists and exits 0 (llvm presence established
    // above, so a manifest-discovery error here would be a real bug).
    let out = karac_bin()
        .args(["build", "--target-cpu=help"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stderr.contains("Available CPUs for this target:"),
        "no-file help form must list project-lessly, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `--target=wasm_wasi --target-cpu=help` lists the *wasm32* registry
/// (mvp/generic/bleeding-edge), not the host's — the listing keys on
/// the active target. Needs no wasm archive/linker: only a target
/// machine is constructed.
#[test]
fn target_cpu_help_is_per_target() {
    let tmp = wasm_test_dir("cpuwasm");
    let path = tmp.join("w.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_wasi",
            "--target-cpu=help",
        ])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_help_is_per_target — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "help listing must exit 0: {stderr}");
    assert!(
        stderr.contains("mvp") && !stderr.contains("apple-m1"),
        "expected the wasm32 CPU table, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// An unknown CPU name is a hard error listing the supported set —
/// not LLVM's native warn-and-fall-back-to-generic. Fails fast before
/// the pipeline, so the named file doesn't even need to parse.
#[test]
fn target_cpu_unknown_name_rejected() {
    let tmp = wasm_test_dir("cpubad");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-cpu=not-a-cpu"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_unknown_name_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown CPU 'not-a-cpu'")
            && stderr.contains("Supported CPUs:")
            && stderr.contains("generic"),
        "expected the unknown-CPU diagnostic with the supported listing, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Empty value rejects at the parse layer (both spellings), before any
/// llvm machinery — no skip needed.
#[test]
fn target_cpu_empty_value_rejected() {
    for argv in [
        vec!["build", "x.kara", "--target-cpu="],
        vec!["build", "x.kara", "--target-cpu"],
    ] {
        let out = karac_bin().args(&argv).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(
            stderr.contains("--target-cpu requires a"),
            "expected the missing-value diagnostic for {argv:?}, got: {stderr}",
        );
    }
}

/// `KARAC_TARGET_CPU` is consulted when the flag is absent — a bogus
/// env value fails validation with the env-supplied name.
#[test]
fn target_cpu_env_var_consulted() {
    let tmp = wasm_test_dir("cpuenv");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env("KARAC_TARGET_CPU", "env-bogus-cpu")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_env_var_consulted — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown CPU 'env-bogus-cpu'"),
        "expected validation of the env-supplied name, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The CLI flag wins over the env var: with both set to (distinct)
/// bogus names, the diagnostic names the flag's value — proving the
/// flag tier was consulted first.
#[test]
fn target_cpu_cli_flag_beats_env() {
    let tmp = wasm_test_dir("cpuprec");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target-cpu",
            "cli-bogus-cpu",
        ])
        .env("KARAC_TARGET_CPU", "env-bogus-cpu")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_cli_flag_beats_env — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown CPU 'cli-bogus-cpu'") && !stderr.contains("env-bogus-cpu"),
        "expected the CLI value to win the precedence chain, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `[release] target-cpu` in a discovered manifest is the lowest
/// override tier: consulted when flag and env are absent (walk-up from
/// the built file's directory, the `karac run` discovery rule)…
#[test]
fn target_cpu_manifest_tier_consulted() {
    let tmp = wasm_test_dir("cpumf");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[release]\ntarget-cpu = \"manifest-bogus-cpu\"\n",
    )
    .unwrap();
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env_remove("KARAC_TARGET_CPU")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_manifest_tier_consulted — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown CPU 'manifest-bogus-cpu'"),
        "expected validation of the manifest-supplied name, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// …and the env var beats it: same manifest, env set to a distinct
/// bogus name — the diagnostic names the env value.
#[test]
fn target_cpu_env_beats_manifest() {
    let tmp = wasm_test_dir("cpumfenv");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[release]\ntarget-cpu = \"manifest-bogus-cpu\"\n",
    )
    .unwrap();
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env("KARAC_TARGET_CPU", "env-bogus-cpu")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_env_beats_manifest — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown CPU 'env-bogus-cpu'") && !stderr.contains("manifest-bogus-cpu"),
        "expected the env value to beat the manifest tier, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A valid override (`generic` — present in every LLVM target's
/// registry) passes validation and the build completes end-to-end.
/// Soft-skips when the native link infrastructure is unavailable, the
/// `bindings_ignored_on_non_wasm_target` posture.
#[test]
fn target_cpu_valid_override_builds() {
    let tmp = wasm_test_dir("cpuok");
    let path = tmp.join("cpubuild.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-cpu=generic"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_cpu_valid_override_builds — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    if !out.status.success()
        && (stderr.contains("link failed") || stderr.contains("codegen failed"))
    {
        eprintln!("skip: target_cpu_valid_override_builds — native link unavailable");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "valid override must build: {stderr}");
    assert!(
        !stderr.contains("unknown CPU") && !stderr.contains("is not a recognized processor"),
        "a registry-valid CPU must pass cleanly, got: {stderr}",
    );
    assert!(tmp.join("cpubuild").exists(), "missing built binary");
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Phase-10: `--target-features` override ──────────────────────────
//
// Feature-string sibling of `--target-cpu` (design.md § CPU Baseline
// Targeting > Feature-string override). Own precedence chain (flag >
// KARAC_TARGET_FEATURES > `[release] target-features`), token-shape
// validation (`+`/`-` prefixes) plus registry membership — both fail
// fast before the pipeline, so precedence is observable via distinct
// bogus values per tier without any runtime archive.

/// `--target-features=help` prints the dump (whose `Available features`
/// section is the relevant half) and exits 0.
#[test]
fn target_features_help_lists_features() {
    let tmp = wasm_test_dir("feathelp");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-features=help"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_help_lists_features — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "help listing must exit 0: {stderr}");
    assert!(
        stderr.contains("Available features for this target:"),
        "expected LLVM's feature table on stderr, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// An entry without its `+`/`-` prefix is a hard error naming the fix —
/// a bare name would be silently meaningless to LLVM.
#[test]
fn target_features_missing_prefix_rejected() {
    let tmp = wasm_test_dir("featpfx");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-features=aes"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_missing_prefix_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("missing its '+' or '-' prefix") && stderr.contains("'+aes'"),
        "expected the prefix diagnostic with the suggested spelling, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// An unknown feature name is a hard error carrying the supported
/// listing — not LLVM's native warn-and-ignore.
#[test]
fn target_features_unknown_name_rejected() {
    let tmp = wasm_test_dir("featbad");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target-features=+not-a-feat",
        ])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_unknown_name_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'not-a-feat'") && stderr.contains("Supported features:"),
        "expected the unknown-feature diagnostic with the listing, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Empty value rejects at the parse layer (both spellings) — no llvm
/// machinery involved, so no skip.
#[test]
fn target_features_empty_value_rejected() {
    for argv in [
        vec!["build", "x.kara", "--target-features="],
        vec!["build", "x.kara", "--target-features"],
    ] {
        let out = karac_bin().args(&argv).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success());
        assert!(
            stderr.contains("--target-features requires a"),
            "expected the missing-value diagnostic for {argv:?}, got: {stderr}",
        );
    }
}

/// The chains resolve independently and in order: the CLI flag beats
/// the env var (diagnostic names the flag's bogus value), and the env
/// var is consulted when the flag is absent.
#[test]
fn target_features_cli_beats_env_and_env_consulted() {
    let tmp = wasm_test_dir("featprec");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    // Flag + env, both bogus: the flag's name must win.
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target-features",
            "+cli-bogus-feat",
        ])
        .env("KARAC_TARGET_FEATURES", "+env-bogus-feat")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_cli_beats_env_and_env_consulted — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'cli-bogus-feat'") && !stderr.contains("env-bogus-feat"),
        "expected the CLI value to win, got: {stderr}",
    );
    // Env alone: consulted.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env("KARAC_TARGET_FEATURES", "+env-bogus-feat")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'env-bogus-feat'"),
        "expected validation of the env-supplied list, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `[release] target-features` is the lowest tier (consulted when flag
/// and env are absent), and the env var beats it. Also pins the chains'
/// independence: a CPU supplied via flag does not suppress the
/// manifest's feature list.
#[test]
fn target_features_manifest_tier_and_independence() {
    let tmp = wasm_test_dir("featmf");
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"demo\"\n\n[release]\ntarget-features = \"+manifest-bogus-feat\"\n",
    )
    .unwrap();
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();
    // Manifest tier consulted when the higher tiers are silent.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env_remove("KARAC_TARGET_FEATURES")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_manifest_tier_and_independence — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'manifest-bogus-feat'"),
        "expected validation of the manifest-supplied list, got: {stderr}",
    );
    // Env beats manifest.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap()])
        .env("KARAC_TARGET_FEATURES", "+env-bogus-feat")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'env-bogus-feat'")
            && !stderr.contains("manifest-bogus-feat"),
        "expected the env value to beat the manifest tier, got: {stderr}",
    );
    // Independence: a *CPU* flag does not suppress the manifest's
    // *features* tier — the chains resolve separately, so the bogus
    // manifest features still fail the build.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-cpu=generic"])
        .env_remove("KARAC_TARGET_FEATURES")
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown feature 'manifest-bogus-feat'"),
        "a CPU flag must not suppress the features manifest tier, got: {stderr}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A valid feature list passes validation and the build completes
/// end-to-end. The feature name is read from the target's own `help`
/// listing so the test is portable across host architectures.
#[test]
fn target_features_valid_override_builds() {
    let tmp = wasm_test_dir("featok");
    let path = tmp.join("featbuild.kara");
    std::fs::write(&path, "fn main() {\n    println(\"hello\");\n}\n").unwrap();
    // Pull a real feature name for this host from the help listing.
    let out = karac_bin()
        .args(["build", path.to_str().unwrap(), "--target-features=help"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = target_cpu_skip_reason(&stderr) {
        eprintln!("skip: target_features_valid_override_builds — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    let feature = stderr
        .lines()
        .skip_while(|l| !l.starts_with("Available features"))
        .skip(1)
        .filter_map(|l| l.strip_prefix("  ")?.split_whitespace().next())
        .next()
        .map(str::to_string);
    let Some(feature) = feature else {
        eprintln!("skip: target_features_valid_override_builds — no feature parsed from listing");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            &format!("--target-features=+{feature}"),
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success()
        && (stderr.contains("link failed") || stderr.contains("codegen failed"))
    {
        eprintln!("skip: target_features_valid_override_builds — native link unavailable");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "valid override must build: {stderr}");
    assert!(
        !stderr.contains("unknown feature") && !stderr.contains("is not a recognized feature"),
        "a registry-valid feature must pass cleanly, got: {stderr}",
    );
    assert!(tmp.join("featbuild").exists(), "missing built binary");
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── `--features wasm-threads` (phase-10 "WASM concurrency lowering —
// `--features wasm-threads` opt-in") ───────────────────────────────────
//
// Dual-artifact builds: the sequential module (today's lowering,
// unchanged) plus `<stem>.threads.wasm` — a wasm32-wasip1-threads
// shared-memory module whose spawn/TaskGroup/par run on a Web Worker
// pool (wasi-threads ABI serviced by the glue). Infrastructure needs on
// top of the standard wasm set: the threaded runtime archive
// (`libkarac_runtime_wasm_threads.a`) and the wasm32-wasip1-threads
// rustup target — both skip-reasoned, not failures.

/// `--features` is a closed set: unknown values hard-error naming the
/// set; `help` lists it and exits 0. Parse-level — no llvm needed.
#[test]
fn features_flag_unknown_value_rejected_and_help_lists_set() {
    let out = karac_bin()
        .args(["build", "x.kara", "--features=fibers"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("unknown --features value 'fibers'") && stderr.contains("wasm-threads"),
        "expected the closed-set rejection, got: {stderr}",
    );

    let out = karac_bin()
        .args(["build", "x.kara", "--features=help"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "--features=help must exit 0");
    assert!(
        stdout.contains("wasm-threads") && stdout.contains("COOP/COEP"),
        "help must list the closed set with the deployment note, got: {stdout}",
    );
}

/// Scope gate: wasm-threads is wasm_browser-only (wasi-threads and the
/// component model don't compose; wasm_wasi's default bindings are
/// component), and an explicit `--bindings=component` is rejected even
/// on wasm_browser. Single-file and project mode share the gate.
#[test]
fn wasm_threads_rejected_off_wasm_browser_and_with_component_bindings() {
    let tmp = wasm_test_dir("wtscope");
    let path = tmp.join("p.kara");
    std::fs::write(&path, "fn main() {\n    println(1);\n}\n").unwrap();

    for (args, expect) in [
        (
            vec!["--target=wasm_wasi", "--features=wasm-threads"],
            "--features wasm-threads requires --target=wasm_browser",
        ),
        (
            vec!["--target=native", "--features=wasm-threads"],
            "--features wasm-threads requires --target=wasm_browser",
        ),
        (
            vec![
                "--target=wasm_browser",
                "--bindings=component",
                "--features=wasm-threads",
            ],
            "incompatible with --bindings=component",
        ),
    ] {
        let mut full = vec!["build", path.to_str().unwrap()];
        full.extend(args.iter().copied());
        let out = karac_bin().args(&full).current_dir(&tmp).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The gate rides the llvm build path; the non-llvm fallback
        // accepts-but-inerts (the --bindings posture).
        if stderr.contains("requires the llvm feature") {
            continue;
        }
        assert!(!out.status.success(), "expected rejection for {args:?}");
        assert!(
            stderr.contains(expect),
            "args {args:?}: expected `{expect}`, got: {stderr}",
        );
    }
    // Project mode (no file argument) — same gate, pre-manifest.
    let out = karac_bin()
        .args(["build", "--target=wasm_wasi", "--features=wasm-threads"])
        .current_dir(&tmp)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !stderr.contains("requires the llvm feature") {
        assert!(!out.status.success());
        assert!(
            stderr.contains("--features wasm-threads requires --target=wasm_browser"),
            "expected the project-mode scope rejection, got: {stderr}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `host fn` × wasm-threads: the synchronous worker→main proxy (the v1
/// rejection lifted). The build now SUCCEEDS and the glue carries the
/// proxy machinery — the signature-driven marshalling table, the
/// worker-side `kara_host` stubs (`makeHostProxy`), and the main-thread
/// service loop (`startHostService`). Load-immune: asserts the emitted
/// glue + dual artifact, no execution (the E2E below runs it on node).
#[test]
fn wasm_threads_host_fn_emits_proxy_glue() {
    let tmp = wasm_test_dir("wthostfn");
    let path = tmp.join("hosted.kara");
    std::fs::write(
        &path,
        "effect resource Reporter;\n\n\
         host fn report(value: i64) -> i64 with writes(Reporter);\n\n\
         fn main() with writes(Reporter) {\n    let r = report(42);\n    println(r);\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_host_fn_emits_proxy_glue — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "host fn + wasm-threads must now build: {stderr}"
    );
    assert!(
        tmp.join("hosted.threads.wasm").exists(),
        "dual artifact missing"
    );
    let glue = std::fs::read_to_string(tmp.join("hosted.js")).unwrap();
    assert!(glue.contains("const DECLARED_IMPORTS = [\"report\"];"));
    assert!(glue.contains("{ name: \"report\", params: [\"bigint\"], ret: \"bigint\" }"));
    for needle in [
        "function makeHostProxy(hostCtl)",
        "function startHostService(hostCtl, memory, hostImpls)",
        "imports.kara_host = makeHostProxy(hostCtl);",
    ] {
        assert!(glue.contains(needle), "proxy glue missing: {needle}");
    }
    // d.ts: declared host fns make hostImpls required AND the threaded
    // surface is present — the combination the v1 rejection made
    // unreachable.
    let dts = std::fs::read_to_string(tmp.join("hosted.d.ts")).unwrap();
    assert!(dts.contains("hostImpls: HostImpls"));
    assert!(dts.contains("KaraThreadedHandle"));
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Headline E2E for the worker→main host-fn proxy: a threaded program
/// that calls `host fn`s round-trips each call to a MAIN-THREAD closure
/// synchronously (SAB control block + Atomics), from BOTH the primary
/// worker (`_start`) and a pool worker (a spawned task) — the same
/// shared control block serves every worker. The scalar/string results
/// flow back into the guest. Load-immune threading evidence: the
/// program runs in a worker (every blocking primitive traps on the page
/// main thread), so a host fn returning a usable value at all proves
/// the proxy executed the closure on main and transported the answer
/// back across the SAB.
#[test]
fn wasm_threads_host_fn_proxy_e2e() {
    let tmp = wasm_test_dir("wthoste2e");
    let path = tmp.join("hosted.kara");
    std::fs::write(
        &path,
        r#"
effect resource Reporter;

host fn report(x: i64) -> i64 with writes(Reporter);
host fn log_str(ptr: *const u8, len: i64) with writes(Reporter);

fn task_body(id: i64) -> i64 with writes(Reporter) {
    report(id)
}

fn main() with writes(Reporter) {
    // host fn from the primary worker: the (worker→main→worker) proxy.
    let doubled = report(21);
    println(doubled);
    // string arg: (ptr, len) into the SHARED linear memory the main
    // thread reads directly.
    let msg = c"threaded kara host";
    log_str(msg.as_ptr(), msg.len());
    // host fn from a POOL worker: the spawned task proxies to main on
    // the identical shared control block.
    let h: TaskHandle[i64] = spawn(|| task_body(100));
    let r: i64 = h.join();
    println(r);
    println("guest-done");
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_host_fn_proxy_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm-threads build failed: {stderr}");
    assert!(tmp.join("hosted.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./hosted.js";

// host fn implementations are MAIN-THREAD closures. The proxy must
// invoke them on this thread and feed results back into the guest
// running in the worker. `seen` records that they actually ran here.
const seen = [];
const h = await run({
  report(x, ctx) {
    if (typeof x !== "bigint")
      throw new Error("i64 must arrive as BigInt on main, got " + typeof x);
    seen.push("report:" + x);
    return x * 2n;
  },
  log_str(ptr, len, ctx) {
    if (typeof ptr !== "number")
      throw new Error("ptr must arrive as number, got " + typeof ptr);
    if (typeof len !== "bigint")
      throw new Error("i64 len must arrive as BigInt, got " + typeof len);
    // The string lives in the SHARED memory — readString decodes it on
    // main without any copy across the worker boundary.
    seen.push("log_str:" + ctx.readString(ptr, len));
  },
});
// node has SAB unconditionally → run() takes the threaded pick.
if (h.threaded !== true) throw new Error("expected the threaded module pick");
// report() ran on main from BOTH the primary worker (21) and the pool
// worker spawned by main (100).
if (!seen.includes("report:21"))
  throw new Error("primary-worker host fn did not run on main: " + JSON.stringify(seen));
if (!seen.includes("report:100"))
  throw new Error("pool-worker host fn did not run on main: " + JSON.stringify(seen));
if (!seen.includes("log_str:threaded kara host"))
  throw new Error("log_str (ptr,len) did not decode on main: " + JSON.stringify(seen));
console.log("PROXY_OK");
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_host_fn_proxy_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "threaded host-fn harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("PROXY_OK"),
        "missing PROXY_OK: stdout={node_stdout} stderr={node_stderr}",
    );
    // The host's doubled answers must flow back into the guest and out
    // through its own println — proof the returns crossed the SAB back
    // into the workers: 21 → 42 (primary worker), 100 → 200 (pool
    // worker, transported through the spawned task's join).
    assert!(
        node_stdout.contains("42"),
        "primary-worker host answer must flow back into the guest: {node_stdout}",
    );
    assert!(
        node_stdout.contains("200"),
        "pool-worker host answer must flow back through the spawned task: {node_stdout}",
    );
    assert!(
        node_stdout.contains("guest-done"),
        "guest must run to completion after the host calls: {node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Headline E2E for host-async channel producers (phase-10 thin vertical
/// slice — `std.web.time.after`): `after(Duration.ms(40)).recv()` parks
/// the primary worker in a blocking `recv`, and a MAIN-THREAD `setTimeout`
/// callback feeds the channel ~40ms later through a SECOND "service" wasm
/// instance over the shared memory, waking the worker.
///
/// Load-immune: `run()` resolves only after the primary worker's `_start`
/// returns, which includes the `recv`. A non-blocking `recv` (the
/// sequential-wasm floor) would return immediately and the program would
/// finish in ~0ms; an elapsed ≳ the timer delay is positive evidence the
/// worker actually parked AND was woken cross-instance — the whole spine
/// (sender clone survives `after`'s return, host owns + drops it, the
/// service instance's `channel_send`/`atomic.notify` reaches the parked
/// futex, on a dedicated stack that never clobbers the worker's frames).
#[test]
fn wasm_threads_timer_after_recv_e2e() {
    let tmp = wasm_test_dir("wttimer");
    let path = tmp.join("timer.kara");
    std::fs::write(
        &path,
        "import std.web.time.{after, Duration};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let rx = after(Duration.ms(40));\n    \
             rx.recv();\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_timer_after_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "timer wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("timer.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./timer.js";
const t0 = performance.now();
const h = await run({});
const elapsed = performance.now() - t0;
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
// The guest's own stdout ("before"/"after") comes from the worker thread's
// fd_write — it lands in this process's combined stdout, which the outer
// test inspects. Here we only assert the main-thread timing evidence.
if (elapsed < 30) {
  console.error("FAIL: recv did not block (~40ms expected), elapsed=" + elapsed.toFixed(1) + "ms");
  process.exit(1);
}
console.log("TIMER_OK elapsed=" + elapsed.toFixed(1) + "ms");
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_timer_after_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "timer harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("TIMER_OK"),
        "missing TIMER_OK (recv blocked then woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // The guest ran to completion: recv unblocked AFTER the host fed the
    // channel — `before` strictly precedes `after`.
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Headline E2E for the multi-shot host-async producer (phase-10
/// `std.web.time.animation_frames`): a `loop { frames.recv(); … }` parks the
/// primary worker, and a MAIN-THREAD self-re-arming `requestAnimationFrame`
/// loop (here the node setTimeout(~16ms) fallback) feeds one `()` per frame
/// through the service instance — proving the *multi-shot* spine (the cloned
/// sender survives across frames, the channel is never closed) on top of the
/// same cross-instance wake path the `after` test proves once.
///
/// Load-immune: receiving 3 frames before `_start` returns means the worker
/// parked in `recv` three times and was woken three times. A non-blocking
/// `recv` (the sequential floor) would spin the loop to completion in ~0ms;
/// an elapsed ≳ a couple of frame intervals is positive evidence of the
/// park→feed→wake round-trip repeating, and of the producer re-arming.
#[test]
fn wasm_threads_animation_frames_recv_e2e() {
    let tmp = wasm_test_dir("wtraf");
    let path = tmp.join("raf.kara");
    std::fs::write(
        &path,
        "import std.web.time.{animation_frames};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let frames = animation_frames();\n    \
             let mut n = 0;\n    \
             loop {\n        \
                 frames.recv();\n        \
                 n = n + 1;\n        \
                 if n >= 3 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_animation_frames_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "animation_frames wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("raf.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./raf.js";
const t0 = performance.now();
const h = await run({});
const elapsed = performance.now() - t0;
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
// 3 frames at the ~16ms node fallback cadence ≈ ≥40ms; a non-blocking recv
// would finish in ~0ms. Assert a couple of frame intervals elapsed.
if (elapsed < 25) {
  console.error("FAIL: frame loop did not block per frame, elapsed=" + elapsed.toFixed(1) + "ms");
  process.exit(1);
}
console.log("RAF_OK elapsed=" + elapsed.toFixed(1) + "ms");
// `animation_frames` is MULTI-SHOT: the host rAF loop re-arms forever (correct
// for a browser — the page runs until closed), so node's event loop never
// drains on its own. Force-exit now that the assertions passed, else this
// harness hangs and the test's `node` subprocess never returns.
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_animation_frames_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "raf harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("RAF_OK"),
        "missing RAF_OK (per-frame recv blocked then woke): stdout={node_stdout} stderr={node_stderr}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (frame loop ran to break): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Headline E2E for `std.web.time.every` (phase-10 host-async interval
/// producer — the `after` arg-shape with the `animation_frames` multi-shot
/// lifetime). A `ticks.recv()` parks the primary worker; a MAIN-THREAD
/// `setInterval` feeds one `()` per period through the service instance,
/// re-arming forever and never dropping its cloned sender — so the channel
/// stays open across ticks (vs `after`, which fires once and closes).
///
/// Load-immune, same shape as the `animation_frames` test: receiving 3 ticks
/// at a ~15ms period before `_start` returns means the worker parked in `recv`
/// three times and was woken three times. A non-blocking `recv` (the
/// sequential floor) would spin to completion in ~0ms; an elapsed ≳ a couple
/// of periods is positive evidence of the park→feed→wake round-trip repeating.
#[test]
fn wasm_threads_every_recv_e2e() {
    let tmp = wasm_test_dir("wtevery");
    let path = tmp.join("every.kara");
    std::fs::write(
        &path,
        "import std.web.time.{every, Duration};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let ticks = every(Duration.ms(15));\n    \
             let mut n = 0;\n    \
             loop {\n        \
                 ticks.recv();\n        \
                 n = n + 1;\n        \
                 if n >= 3 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_every_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "every wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("every.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./every.js";
const t0 = performance.now();
const h = await run({});
const elapsed = performance.now() - t0;
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
// 3 ticks at a 15ms period ≈ ≥45ms; a non-blocking recv would finish in ~0ms.
// Assert a couple of periods elapsed.
if (elapsed < 25) {
  console.error("FAIL: interval loop did not block per tick, elapsed=" + elapsed.toFixed(1) + "ms");
  process.exit(1);
}
console.log("EVERY_OK elapsed=" + elapsed.toFixed(1) + "ms");
// `every` is MULTI-SHOT: the host setInterval re-arms forever, so node's event
// loop never drains on its own. Force-exit now that the assertions passed, else
// this harness hangs and the test's `node` subprocess never returns.
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_every_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "every harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("EVERY_OK"),
        "missing EVERY_OK (per-tick recv blocked then woke): stdout={node_stdout} stderr={node_stderr}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (interval loop ran to break): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Headline E2E for the first NON-UNIT host-async producer (phase-10
/// `std.web.events.pointer_moves` — the `Channel[T]`, `T != ()` slice that
/// Plume drives). A `moves.recv()` parks the primary worker; a MAIN-THREAD
/// `pointermove` listener marshals each event's `(clientX, clientY, buttons)`
/// into the service instance's `karac_runtime_event_scratch` buffer and
/// `channel_send`s a 24-byte `PointerEvent` payload — vs the 0-byte `()` a
/// timer/frame producer sends. The guest asserts the *exact* f64 coordinates
/// round-tripped (300.0, 400.0) AND the `buttons` bitmask (1 = primary held →
/// `pressed()`): a unit/zero channel would zero-fill the payload and the guest
/// would print `PTR_FAIL`, so `PTR_OK` is positive evidence the structured
/// payload (including the i64 `buttons` field) crossed host→wasm intact, not
/// just that recv woke.
///
/// Node has no DOM, so the harness injects an `EventTarget` via
/// `opts.pointerTarget` and dispatches synthetic moves on it — the same seam a
/// browser fills with the canvas element.
///
/// The guest consumes the stream the way real code does — a `loop { recv() }`,
/// not a single `recv()` — and breaks on the first VALID `(300, 400, pressed,
/// buttons==1)` sample. That is what makes `PTR_OK` deterministic: the host
/// re-dispatches the same constant event every 12 ms, so the guest only needs
/// ONE clean read, not for the very first parked-recv read to be clean. The
/// earlier single-`recv` form raced ~1-in-5 under sibling-test load — the first
/// parked-recv's out-slot read is acutely stack-layout sensitive — yet billed
/// itself "load-immune". The bounded retry (`tries >= N`) keeps the discriminating
/// power: a unit/zero-floor channel never yields `(300, 400, 1)`, so it spins to
/// the cap and prints `PTR_FAIL` rather than hanging — `PTR_OK` remains positive
/// evidence the structured payload (incl. the i64 `buttons` field) crossed
/// host→wasm intact, not just that recv woke.
#[test]
fn wasm_threads_pointer_moves_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtptr");
    let path = tmp.join("ptr.kara");
    std::fs::write(
        &path,
        "import std.web.events.{pointer_moves, PointerEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let moves = pointer_moves();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let p = moves.recv();\n        \
                 if p.x() == 300.0 and p.y() == 400.0 and p.pressed() and p.buttons() == 1 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"PTR_OK\");\n    \
             } else {\n        \
                 println(\"PTR_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_pointer_moves_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "pointer_moves wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("ptr.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./ptr.js";
// A node EventTarget stands in for the canvas; synthetic pointermove events
// carry fixed coordinates the guest checks exactly.
class PM extends Event {
  constructor(x, y, buttons) { super("pointermove"); this.clientX = x; this.clientY = y; this.buttons = buttons; }
}
const target = new EventTarget();
let dispatched = 0;
// Multi-shot: keep dispatching until the worker parks in recv and the listener
// is registered; the coalescing producer feeds the next event after each drain.
// buttons=1 (primary held) so the guest can also assert the i64 field crossed.
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new PM(300, 400, 1)); }, 12);
// Self-kill if recv never wakes (would otherwise hang the test's node child).
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { pointerTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("PTR_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_pointer_moves_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "pointer harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("PTR_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // The structured payload crossed intact — a zero-filled unit floor would
    // have printed PTR_FAIL.
    assert!(
        node_stdout.contains("PTR_OK") && !node_stdout.contains("PTR_FAIL"),
        "exact (300.0, 400.0) PointerEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for the event-data producer: `pointer_moves`
/// built WITHOUT `--features wasm-threads` is a hard compile error (codegen,
/// pre-link) naming the flag — never a silent never-filling channel. Sibling
/// of `wasm_time_after_sequential_target_rejected`; same no-`llvm` skip.
#[test]
fn wasm_pointer_moves_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtptrgate");
    let path = tmp.join("ptr.kara");
    std::fs::write(
        &path,
        "import std.web.events.{pointer_moves, PointerEvent};\n\n\
         fn main() {\n    \
             let moves = pointer_moves();\n    \
             moves.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_pointer_moves_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm event-data producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `std.web.events.wheel` end-to-end on `wasm_browser --features wasm-threads`:
/// the second non-unit event-data producer. A worker blocks in `wheel().recv()`,
/// the host fires synthetic `wheel` events carrying fixed deltas + cursor
/// position, and the 32-byte `WheelEvent` ({ x, y, delta_x, delta_y }) must
/// round-trip host→wasm intact. Sibling of
/// `wasm_threads_pointer_moves_payload_recv_e2e`; the harness injects an
/// `EventTarget` via `opts.wheelTarget` (the seam a browser fills with the
/// canvas). Like the pointer sibling, the guest drains the stream in a
/// `loop { recv() }` and breaks on the first VALID `(120, 60, 0, -53)` sample,
/// so `WHEEL_OK` is deterministic (the host re-dispatches the constant event
/// every tick) and does not hinge on the fragile first parked-recv read. The
/// bounded retry preserves the discriminating power: a unit/zero-floor channel
/// never yields the four-field payload, so it caps out to `WHEEL_FAIL`.
#[test]
fn wasm_threads_wheel_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtwheel");
    let path = tmp.join("wh.kara");
    std::fs::write(
        &path,
        "import std.web.events.{wheel, WheelEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let wheels = wheel();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let w = wheels.recv();\n        \
                 if w.x() == 120.0 and w.y() == 60.0 and w.delta_x() == 0.0 and w.delta_y() == -53.0 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"WHEEL_OK\");\n    \
             } else {\n        \
                 println(\"WHEEL_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_wheel_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "wheel wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("wh.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./wh.js";
// A node EventTarget stands in for the canvas; synthetic wheel events carry
// fixed deltas + cursor position the guest checks exactly.
class WH extends Event {
  constructor(x, y, dx, dy) { super("wheel"); this.clientX = x; this.clientY = y; this.deltaX = dx; this.deltaY = dy; }
}
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new WH(120, 60, 0, -53)); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { wheelTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("WHEEL_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_wheel_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "wheel harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("WHEEL_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // The four-field payload crossed intact — a zero-filled / wrong-size floor
    // would have printed WHEEL_FAIL.
    assert!(
        node_stdout.contains("WHEEL_OK") && !node_stdout.contains("WHEEL_FAIL"),
        "exact (120,60,0,-53) WheelEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `wheel`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_pointer_moves_sequential_target_rejected`.
#[test]
fn wasm_wheel_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtwheelgate");
    let path = tmp.join("wh.kara");
    std::fs::write(
        &path,
        "import std.web.events.{wheel, WheelEvent};\n\n\
         fn main() {\n    \
             let wheels = wheel();\n    \
             wheels.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_wheel_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm wheel producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `std.web.events.keydown` end-to-end on `wasm_browser --features wasm-threads`:
/// the third non-unit event-data producer (keyboard). A worker blocks in
/// `keydown().recv()`, the host fires synthetic `keydown` events carrying a
/// fixed `keyCode`, and the 8-byte `KeyEvent` ({ key_code }) must round-trip
/// host→wasm intact. Sibling of `wasm_threads_wheel_payload_recv_e2e`; the
/// harness injects an `EventTarget` via `opts.keyTarget` (the seam a browser
/// fills with the window). Like its siblings, the guest drains the stream in a
/// `loop { recv() }` and breaks on the first VALID `key_code == 39` sample, so
/// `KEY_OK` is deterministic (the host re-dispatches the constant event every
/// tick) and does not hinge on the fragile first parked-recv read. The bounded
/// retry preserves the discriminating power: a unit/zero-floor channel never
/// yields `key_code == 39`, so it caps out to `KEY_FAIL`.
#[test]
fn wasm_threads_keydown_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtkey");
    let path = tmp.join("ky.kara");
    std::fs::write(
        &path,
        "import std.web.events.{keydown, KeyEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let keys = keydown();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let k = keys.recv();\n        \
                 if k.key_code() == 39 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"KEY_OK\");\n    \
             } else {\n        \
                 println(\"KEY_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_keydown_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "keydown wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("ky.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./ky.js";
// A node EventTarget stands in for the window; synthetic keydown events carry a
// fixed keyCode (39 = ArrowRight) the guest checks exactly.
class KE extends Event {
  constructor(code) { super("keydown"); this.keyCode = code; }
}
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new KE(39)); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { keyTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("KEY_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_keydown_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "keydown harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("KEY_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // The key_code crossed intact — a zero-filled / wrong-size floor would have
    // printed KEY_FAIL.
    assert!(
        node_stdout.contains("KEY_OK") && !node_stdout.contains("KEY_FAIL"),
        "exact keyCode 39 (ArrowRight) KeyEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `keydown`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_wheel_sequential_target_rejected`.
#[test]
fn wasm_keydown_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtkeygate");
    let path = tmp.join("ky.kara");
    std::fs::write(
        &path,
        "import std.web.events.{keydown, KeyEvent};\n\n\
         fn main() {\n    \
             let keys = keydown();\n    \
             keys.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_keydown_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm keydown producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// `std.web.events.keyup` end-to-end on `wasm_browser --features wasm-threads`:
/// the key-release sibling of `keydown`, carrying the same 8-byte `KeyEvent`.
/// A worker blocks in `keyup().recv()`, the host fires synthetic `keyup` events
/// carrying a fixed `keyCode`, and the payload must round-trip host→wasm intact.
/// Modeled on `wasm_threads_keydown_payload_recv_e2e`. The 8-byte single-i64
/// payload is marshalled atomically (no multi-field tear), but the recv side is
/// still vulnerable to the same flake as the multi-field siblings: under load
/// the first parked-recv's out-slot read is acutely stack-layout sensitive and
/// can come back corrupt — the corruption clobbers the out-slot regardless of
/// payload width. So the guest drains the stream in a `loop { recv() }` and
/// breaks on the first VALID `key_code == 27` sample (the host re-dispatches the
/// constant event every tick), making `KEYUP_OK` deterministic without hinging
/// on the fragile first read. The bounded retry preserves the discriminating
/// power: a unit/zero-floor channel never yields `key_code == 27`, so it caps
/// out to `KEYUP_FAIL`.
#[test]
fn wasm_threads_keyup_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtkeyup");
    let path = tmp.join("ku.kara");
    std::fs::write(
        &path,
        "import std.web.events.{keyup, KeyEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let keys = keyup();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let k = keys.recv();\n        \
                 if k.key_code() == 27 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"KEYUP_OK\");\n    \
             } else {\n        \
                 println(\"KEYUP_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_keyup_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "keyup wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("ku.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./ku.js";
// A node EventTarget stands in for the window; synthetic keyup events carry a
// fixed keyCode (27 = Escape) the guest checks exactly.
class KE extends Event {
  constructor(code) { super("keyup"); this.keyCode = code; }
}
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new KE(27)); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { keyTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("KEYUP_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_keyup_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "keyup harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("KEYUP_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // The key_code crossed intact — a zero-filled / wrong-size floor would have
    // printed KEYUP_FAIL.
    assert!(
        node_stdout.contains("KEYUP_OK") && !node_stdout.contains("KEYUP_FAIL"),
        "exact keyCode 27 (Escape) KeyEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `keyup`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_keydown_sequential_target_rejected`.
#[test]
fn wasm_keyup_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtkeyupgate");
    let path = tmp.join("ku.kara");
    std::fs::write(
        &path,
        "import std.web.events.{keyup, KeyEvent};\n\n\
         fn main() {\n    \
             let keys = keyup();\n    \
             keys.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_keyup_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm keyup producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for `std.web.events.clicks` on `wasm_browser --features
/// wasm-threads`: a blocking `recv()` on the click stream must wake when a host
/// "click" event fires, and the exact element-relative position (offsetX 150,
/// offsetY 75) must round-trip across the service instance as the 16-byte
/// `ClickEvent` payload. Sibling of `wasm_threads_keyup_payload_recv_e2e`; the
/// guest loops until a valid payload is seen (the first parked recv's out-slot
/// read can race under load) rather than trusting the first recv, then prints
/// `CLICKS_OK` — a zero-filled / torn floor would print `CLICKS_FAIL`.
#[test]
fn wasm_threads_clicks_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtclicks");
    let path = tmp.join("ck.kara");
    std::fs::write(
        &path,
        "import std.web.events.{clicks, ClickEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let cs = clicks();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let c = cs.recv();\n        \
                 if c.x() == 150.0 and c.y() == 75.0 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"CLICKS_OK\");\n    \
             } else {\n        \
                 println(\"CLICKS_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_clicks_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "clicks wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("ck.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./ck.js";
// A node EventTarget stands in for the canvas; synthetic click events carry a
// fixed element-relative position the guest checks exactly.
class CE extends Event {
  constructor(x, y) { super("click"); this.offsetX = x; this.offsetY = y; }
}
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new CE(150, 75)); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { clickTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("CLICKS_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_clicks_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "clicks harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("CLICKS_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // Both coordinates crossed intact — a zero-filled / wrong-size floor would
    // have printed CLICKS_FAIL.
    assert!(
        node_stdout.contains("CLICKS_OK") && !node_stdout.contains("CLICKS_FAIL"),
        "exact click position (150, 75) ClickEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `clicks`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_keyup_sequential_target_rejected`.
#[test]
fn wasm_clicks_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtclicksgate");
    let path = tmp.join("ck.kara");
    std::fs::write(
        &path,
        "import std.web.events.{clicks, ClickEvent};\n\n\
         fn main() {\n    \
             let cs = clicks();\n    \
             cs.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_clicks_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm clicks producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for `std.web.events.dblclick` on `wasm_browser --features
/// wasm-threads`: the double-press sibling of `clicks`, carrying the same
/// 16-byte `ClickEvent` payload. A blocking `recv()` on the dblclick stream must
/// wake when a host "dblclick" event fires, and the exact element-relative
/// position (offsetX 220, offsetY 140) must round-trip across the service
/// instance. Sibling of `wasm_threads_clicks_payload_recv_e2e`; the guest loops
/// until a valid payload is seen (the first parked recv's out-slot read can race
/// under load) then prints `DBLCLICK_OK` — a zero-filled / torn floor would print
/// `DBLCLICK_FAIL`.
#[test]
fn wasm_threads_dblclick_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtdblclick");
    let path = tmp.join("dc.kara");
    std::fs::write(
        &path,
        "import std.web.events.{dblclick, ClickEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let ds = dblclick();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let c = ds.recv();\n        \
                 if c.x() == 220.0 and c.y() == 140.0 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"DBLCLICK_OK\");\n    \
             } else {\n        \
                 println(\"DBLCLICK_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_dblclick_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "dblclick wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("dc.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./dc.js";
// A node EventTarget stands in for the canvas; synthetic dblclick events carry a
// fixed element-relative position the guest checks exactly.
class DCE extends Event {
  constructor(x, y) { super("dblclick"); this.offsetX = x; this.offsetY = y; }
}
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new DCE(220, 140)); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { dblclickTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("DBLCLICK_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_dblclick_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "dblclick harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("DBLCLICK_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // Both coordinates crossed intact — a zero-filled / wrong-size floor would
    // have printed DBLCLICK_FAIL.
    assert!(
        node_stdout.contains("DBLCLICK_OK") && !node_stdout.contains("DBLCLICK_FAIL"),
        "exact dblclick position (220, 140) ClickEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `dblclick`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_clicks_sequential_target_rejected`.
#[test]
fn wasm_dblclick_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtdblclickgate");
    let path = tmp.join("dc.kara");
    std::fs::write(
        &path,
        "import std.web.events.{dblclick, ClickEvent};\n\n\
         fn main() {\n    \
             let ds = dblclick();\n    \
             ds.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_dblclick_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm dblclick producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for `std.web.events.resize` on `wasm_browser --features
/// wasm-threads`: a blocking `recv()` on the resize stream must wake when a host
/// "resize" event fires, and the window's *current* dimensions (1024×768 — read
/// off the target, NOT the event object) must round-trip across the service
/// instance as the 16-byte `ResizeEvent` (two `i64`s). Sibling of
/// `wasm_threads_clicks_payload_recv_e2e`; the guest loops until a valid payload
/// is seen (the first parked recv's out-slot read can race under load) then
/// prints `RESIZE_OK` — a zero-filled / torn floor would print `RESIZE_FAIL`.
#[test]
fn wasm_threads_resize_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtresize");
    let path = tmp.join("rs.kara");
    std::fs::write(
        &path,
        "import std.web.events.{resize, ResizeEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let rs = resize();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let r = rs.recv();\n        \
                 if r.width() == 1024 and r.height() == 768 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"RESIZE_OK\");\n    \
             } else {\n        \
                 println(\"RESIZE_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_resize_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "resize wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("rs.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./rs.js";
// A node EventTarget stands in for the window; the resize event carries no
// dimensions, so the guest reads innerWidth/innerHeight off the target — set
// them to a fixed size the guest checks exactly.
const target = new EventTarget();
target.innerWidth = 1024;
target.innerHeight = 768;
let dispatched = 0;
const iv = setInterval(() => { dispatched++; target.dispatchEvent(new Event("resize")); }, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { resizeTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("RESIZE_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_resize_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "resize harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("RESIZE_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // Both dimensions crossed intact — a zero-filled / wrong-size floor would
    // have printed RESIZE_FAIL.
    assert!(
        node_stdout.contains("RESIZE_OK") && !node_stdout.contains("RESIZE_FAIL"),
        "exact window dims (1024, 768) ResizeEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `resize`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_dblclick_sequential_target_rejected`.
#[test]
fn wasm_resize_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtresizegate");
    let path = tmp.join("rs.kara");
    std::fs::write(
        &path,
        "import std.web.events.{resize, ResizeEvent};\n\n\
         fn main() {\n    \
             let rs = resize();\n    \
             rs.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_resize_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm resize producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for `std.web.events.contextmenu` on `wasm_browser --features
/// wasm-threads`: the right-click sibling of `clicks`, carrying the same 16-byte
/// `ClickEvent` payload. A blocking `recv()` must wake on a host "contextmenu"
/// event, the exact element-relative position (offsetX 310, offsetY 95) must
/// round-trip, AND the listener must `preventDefault()` the native menu — the
/// harness dispatches a *cancelable* event and asserts `dispatchEvent` returned
/// false (i.e. the default was prevented). Sibling of
/// `wasm_threads_clicks_payload_recv_e2e`; the guest loops until a valid payload
/// is seen (recv-out-slot race) then prints `CTX_OK` — a zero/torn floor prints
/// `CTX_FAIL`.
#[test]
fn wasm_threads_contextmenu_payload_recv_e2e() {
    let tmp = wasm_test_dir("wtctxmenu");
    let path = tmp.join("cm.kara");
    std::fs::write(
        &path,
        "import std.web.events.{contextmenu, ClickEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let cm = contextmenu();\n    \
             let mut ok = false;\n    \
             let mut tries = 0;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // very first recv (the first parked recv's out-slot read can race\n    \
             // under load); the host re-dispatches the same event every tick.\n    \
             loop {\n        \
                 let c = cm.recv();\n        \
                 if c.x() == 310.0 and c.y() == 95.0 {\n            \
                     ok = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok {\n        \
                 println(\"CTX_OK\");\n    \
             } else {\n        \
                 println(\"CTX_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_contextmenu_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "contextmenu wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("cm.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./cm.js";
// A node EventTarget stands in for the canvas; synthetic contextmenu events are
// CANCELABLE so the listener's preventDefault() is observable: dispatchEvent
// returns false when the default was prevented.
class CME extends Event {
  constructor(x, y) { super("contextmenu", { cancelable: true }); this.offsetX = x; this.offsetY = y; }
}
const target = new EventTarget();
let dispatched = 0, prevented = 0;
const iv = setInterval(() => {
  dispatched++;
  const notCancelled = target.dispatchEvent(new CME(310, 95));
  if (!notCancelled) prevented++;
}, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { contextmenuTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("CTX_HARNESS_OK dispatched=" + dispatched + " prevented=" + prevented);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_contextmenu_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "contextmenu harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("CTX_HARNESS_OK"),
        "harness did not complete (recv woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // Position crossed intact — a zero-filled / wrong-size floor prints CTX_FAIL.
    assert!(
        node_stdout.contains("CTX_OK") && !node_stdout.contains("CTX_FAIL"),
        "exact right-click position (310, 95) ClickEvent payload must round-trip host→wasm: stdout={node_stdout}",
    );
    // The listener must have preventDefault'd the native menu — `prevented` is
    // bumped each time a cancelable contextmenu was cancelled. A listener that
    // forgot preventDefault would leave `prevented=0`.
    assert!(
        !node_stdout.contains("prevented=0"),
        "contextmenu listener must preventDefault the native menu (prevented should be > 0): stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recv blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `contextmenu`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_resize_sequential_target_rejected`.
#[test]
fn wasm_contextmenu_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtctxmenugate");
    let path = tmp.join("cm.kara");
    std::fs::write(
        &path,
        "import std.web.events.{contextmenu, ClickEvent};\n\n\
         fn main() {\n    \
             let cm = contextmenu();\n    \
             cm.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_contextmenu_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm contextmenu producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for the `std.web.events.focus` / `.blur` PAIR on `wasm_browser
/// --features wasm-threads` — the first UNIT-payload `events.*` producers (a
/// 0-byte `()` token per edge, no event-scratch, like `every`/`animation_frames`
/// but driven by a focus/blur listener). The guest drains BOTH streams: a
/// blocking `focus().recv()` parks the worker until a host "focus" fires, then a
/// `blur().recv()` until a "blur" fires — proving the two distinct DOM events
/// wake their own channels without cross-firing on the shared target. There is
/// no payload to validate (unit), so this asserts the ordering `before → FOCUS_OK
/// → BLUR_OK → after`: a never-filling channel would hang (the 8s bail fires).
#[test]
fn wasm_threads_focus_blur_recv_e2e() {
    let tmp = wasm_test_dir("wtfocusblur");
    let path = tmp.join("fb.kara");
    std::fs::write(
        &path,
        "import std.web.events.{focus, blur};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let gained = focus();\n    \
             let lost = blur();\n    \
             gained.recv();\n    \
             println(\"FOCUS_OK\");\n    \
             lost.recv();\n    \
             println(\"BLUR_OK\");\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_focus_blur_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "focus/blur wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("fb.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./fb.js";
// One node EventTarget stands in for the window; dispatch BOTH a "focus" and a
// "blur" each tick so the guest's two sequential recvs each wake on their own
// event. focus/blur are distinct event types, so the listeners never cross-fire.
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => {
  dispatched++;
  target.dispatchEvent(new Event("focus"));
  target.dispatchEvent(new Event("blur"));
}, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { focusTarget: target, blurTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("FB_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_focus_blur_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "focus/blur harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("FB_HARNESS_OK"),
        "harness did not complete (both recvs woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // Both unit streams woke, in order: before → FOCUS_OK → BLUR_OK → after.
    let before = node_stdout.find("before");
    let focus_ok = node_stdout.find("FOCUS_OK");
    let blur_ok = node_stdout.find("BLUR_OK");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, focus_ok, blur_ok, after), (Some(b), Some(f), Some(l), Some(a)) if b < f && f < l && l < a),
        "guest must print before→FOCUS_OK→BLUR_OK→after (both unit recvs blocked then woke in order): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `focus`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the
/// flag — never a silent never-filling channel. Sibling of
/// `wasm_contextmenu_sequential_target_rejected`.
#[test]
fn wasm_focus_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtfocusgate");
    let path = tmp.join("fo.kara");
    std::fs::write(
        &path,
        "import std.web.events.{focus};\n\n\
         fn main() {\n    \
             let gained = focus();\n    \
             gained.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_focus_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm focus producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `blur` (sibling of
/// `wasm_focus_sequential_target_rejected`).
#[test]
fn wasm_blur_sequential_target_rejected() {
    let tmp = wasm_test_dir("wtblurgate");
    let path = tmp.join("bl.kara");
    std::fs::write(
        &path,
        "import std.web.events.{blur};\n\n\
         fn main() {\n    \
             let lost = blur();\n    \
             lost.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_blur_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm blur producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// End-to-end for the `std.web.events` touch family (`touchstart`/`touchmove`/
/// `touchend`) on `wasm_browser --features wasm-threads`: a blocking `recv()`
/// on each of the three streams must wake on the matching synthetic touch and
/// the 16-byte `TouchEvent` (x, y) payload must round-trip host→wasm exactly.
/// The touch trio is the mobile-input analogue of pointer-down/move/up; this
/// proves the full down→drag→up gesture crosses the spine. Uses the now-standard
/// loop-until-valid drain per stream (the first parked recv's out-slot read can
/// race under load, width-independent). Sibling of
/// `wasm_threads_focus_blur_recv_e2e`.
#[test]
fn wasm_threads_touch_payload_recv_e2e() {
    let tmp = wasm_test_dir("wttouch");
    let path = tmp.join("tc.kara");
    std::fs::write(
        &path,
        "import std.web.events.{touchstart, touchmove, touchend, TouchEvent};\n\n\
         fn main() {\n    \
             println(\"before\");\n    \
             let starts = touchstart();\n    \
             let moves = touchmove();\n    \
             let ends = touchend();\n    \
             let mut ok_start = false;\n    \
             let mut ok_move = false;\n    \
             let mut ok_end = false;\n    \
             // Loop until a valid payload is observed rather than trusting the\n    \
             // first recv (the first parked recv's out-slot read can race under\n    \
             // load); the host re-dispatches the same event every tick.\n    \
             let mut tries = 0;\n    \
             loop {\n        \
                 let s = starts.recv();\n        \
                 if s.x() == 10.0 and s.y() == 20.0 {\n            \
                     ok_start = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             tries = 0;\n    \
             loop {\n        \
                 let m = moves.recv();\n        \
                 if m.x() == 30.0 and m.y() == 40.0 {\n            \
                     ok_move = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             tries = 0;\n    \
             loop {\n        \
                 let e = ends.recv();\n        \
                 if e.x() == 50.0 and e.y() == 60.0 {\n            \
                     ok_end = true;\n            \
                     break;\n        \
                 }\n        \
                 tries = tries + 1;\n        \
                 if tries >= 64 {\n            \
                     break;\n        \
                 }\n    \
             }\n    \
             if ok_start and ok_move and ok_end {\n        \
                 println(\"TOUCH_OK\");\n    \
             } else {\n        \
                 println(\"TOUCH_FAIL\");\n    \
             }\n    \
             println(\"after\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_touch_payload_recv_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "touch wasm-threads build failed: {stderr}"
    );
    assert!(tmp.join("tc.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./tc.js";
// One node EventTarget stands in for the canvas; dispatch a synthetic touchstart,
// touchmove and touchend each tick so the guest's three sequential recvs each
// wake on their own stream. Each carries a single primary touch with fixed
// coords; a plain EventTarget has no getBoundingClientRect, so the glue reports
// the raw clientX/Y the guest checks exactly. On a release the live `touches`
// list is empty (the finger lifted) — the position rides changedTouches[0].
class TStart extends Event { constructor(x, y) { super("touchstart"); this.touches = [{ clientX: x, clientY: y }]; this.changedTouches = this.touches; } }
class TMove extends Event { constructor(x, y) { super("touchmove"); this.touches = [{ clientX: x, clientY: y }]; this.changedTouches = this.touches; } }
class TEnd extends Event { constructor(x, y) { super("touchend"); this.touches = []; this.changedTouches = [{ clientX: x, clientY: y }]; } }
const target = new EventTarget();
let dispatched = 0;
const iv = setInterval(() => {
  dispatched++;
  target.dispatchEvent(new TStart(10, 20));
  target.dispatchEvent(new TMove(30, 40));
  target.dispatchEvent(new TEnd(50, 60));
}, 12);
const bail = setTimeout(() => { console.error("FAIL: recv never woke, dispatched=" + dispatched); process.exit(2); }, 8000);
const h = await run({}, { touchTarget: target });
clearInterval(iv);
clearTimeout(bail);
if (h.threaded !== true) { console.error("FAIL: expected threaded pick"); process.exit(1); }
console.log("TOUCH_HARNESS_OK dispatched=" + dispatched);
process.exit(0);
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_touch_payload_recv_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "touch harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    assert!(
        node_stdout.contains("TOUCH_HARNESS_OK"),
        "harness did not complete (all three recvs woke): stdout={node_stdout} stderr={node_stderr}",
    );
    // All three streams woke and every coordinate crossed intact — a zero-filled
    // / wrong-size / wrong-stream floor would have printed TOUCH_FAIL.
    assert!(
        node_stdout.contains("TOUCH_OK") && !node_stdout.contains("TOUCH_FAIL"),
        "exact touchstart (10,20) / touchmove (30,40) / touchend (50,60) TouchEvent payloads must round-trip host→wasm: stdout={node_stdout}",
    );
    let before = node_stdout.find("before");
    let after = node_stdout.find("after");
    assert!(
        matches!((before, after), (Some(b), Some(a)) if b < a),
        "guest must print before→after (recvs blocked then woke): stdout={node_stdout}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `touchstart`: built WITHOUT `--features
/// wasm-threads` it is a hard compile error (codegen, pre-link) naming the flag
/// — never a silent never-filling channel. Sibling of
/// `wasm_clicks_sequential_target_rejected`.
#[test]
fn wasm_touchstart_sequential_target_rejected() {
    let tmp = wasm_test_dir("wttouchstartgate");
    let path = tmp.join("ts.kara");
    std::fs::write(
        &path,
        "import std.web.events.{touchstart, TouchEvent};\n\n\
         fn main() {\n    \
             let starts = touchstart();\n    \
             starts.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_touchstart_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm touchstart producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `touchmove` (sibling of
/// `wasm_touchstart_sequential_target_rejected`).
#[test]
fn wasm_touchmove_sequential_target_rejected() {
    let tmp = wasm_test_dir("wttouchmovegate");
    let path = tmp.join("tm.kara");
    std::fs::write(
        &path,
        "import std.web.events.{touchmove, TouchEvent};\n\n\
         fn main() {\n    \
             let moves = touchmove();\n    \
             moves.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_touchmove_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm touchmove producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `touchend` (sibling of
/// `wasm_touchstart_sequential_target_rejected`).
#[test]
fn wasm_touchend_sequential_target_rejected() {
    let tmp = wasm_test_dir("wttouchendgate");
    let path = tmp.join("te.kara");
    std::fs::write(
        &path,
        "import std.web.events.{touchend, TouchEvent};\n\n\
         fn main() {\n    \
             let ends = touchend();\n    \
             ends.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_touchend_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm touchend producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Full-demo E2E for the Plume flow-field dogfood (`examples/plume/`): builds
/// the shipped `plume.kara` for `wasm_browser --features wasm-threads` and runs
/// it under node, exercising the whole front-end spine together — the blocking
/// `loop { frames.recv(); … }` render loop, `TaskGroup.spawn` row-band
/// parallelism, the `put_pixels` blit, AND the new `pointer_moves`
/// `Channel[PointerEvent]` drained with `try_recv`. Asserts:
///   1. frames actually render (put_pixels called repeatedly — the parallel
///      join + blit round-trip works), and the framebuffer is non-uniform
///      (real flow content, not a blank/constant buffer);
///   2. pointer steering reaches the kernel: the cursor's warm-tint region
///      (R channel boosted near `pu,pv` in `render_rows`) is measurably
///      brighter around the fed pointer position than in a far corner — a
///      deterministic signal independent of the flow noise.
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn plume_example_pointer_steered_flow_e2e() {
    let tmp = wasm_test_dir("plume");
    let path = tmp.join("plume.kara");
    std::fs::write(&path, include_str!("../examples/plume/plume.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: plume_example_pointer_steered_flow_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "plume build failed: {stderr}");
    assert!(tmp.join("plume.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./plume.js";
const W = 520, H = 340;
// Feed the pointer at a fixed canvas position; the warm tint should brighten
// the R channel around it. Synthetic events set clientX/Y (glue falls back to
// them when offsetX/Y are absent).
const PX = 300, PY = 200;
class PM extends Event {
  constructor(x, y) { super("pointermove"); this.clientX = x; this.clientY = y; }
}
const target = new EventTarget();
const iv = setInterval(() => target.dispatchEvent(new PM(PX, PY)), 10);
const bail = setTimeout(() => { console.error("FAIL: too few frames rendered"); process.exit(2); }, 12000);

let frameCount = 0;
function avgR(view, cx, cy) {
  let sum = 0, n = 0;
  for (let dy = -10; dy <= 10; dy++) {
    for (let dx = -10; dx <= 10; dx++) {
      const x = cx + dx, y = cy + dy;
      if (x < 0 || x >= W || y < 0 || y >= H) continue;
      sum += view[(y * W + x) * 4]; n++;
    }
  }
  return sum / n;
}
function put_pixels(ptr, len, w, h, ctx) {
  frameCount++;
  // Assert on a settled frame (pointer events have been picked up by then).
  if (frameCount === 8) {
    const view = new Uint8Array(ctx.memory.buffer, Number(ptr), Number(len));
    // Non-uniformity: the frame has real structure, not one constant value.
    let mn = 255, mx = 0;
    for (let i = 0; i < view.length; i += 4) { const r = view[i]; if (r < mn) mn = r; if (r > mx) mx = r; }
    if (mx - mn < 20) { console.error("FAIL: framebuffer too uniform (mn=" + mn + " mx=" + mx + ")"); process.exit(3); }
    const near = avgR(view, PX, PY);
    const far = avgR(view, 40, 40);
    console.log("PLUME frames=" + frameCount + " mn=" + mn + " mx=" + mx +
                " nearR=" + near.toFixed(1) + " farR=" + far.toFixed(1));
    if (near <= far + 15) { console.error("FAIL: pointer warm-tint not steering (near=" + near.toFixed(1) + " far=" + far.toFixed(1) + ")"); process.exit(4); }
    clearInterval(iv); clearTimeout(bail);
    console.log("PLUME_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { pointerTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: plume_example_pointer_steered_flow_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("PLUME_OK"),
        "plume harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Plume also wires `std.web.events.clicks()` — a click PINS a persistent,
/// counter-rotating vortex at that point, wearing a steady cool glow (additive,
/// not flow-modulated) so a placed swirl stays visible. This is the demo-level
/// consumer of the `clicks` `ClickEvent` producer (the pointer drives a swirl
/// that *follows* the cursor; a click drops one you *leave behind*).
///
/// Reuses Plume's gold-standard signal: a WITHIN-FRAME spatial diff, which is
/// immune to the always-advancing `t` animation noise (you can't compare frames
/// across `t`, but you can compare two regions of the SAME frame). The pinned
/// glow weights the BLUE channel hardest (+200 at the core), so after a click at
/// a point Q far from the cursor and the three fixed vortices, Q's neighbourhood
/// is measurably bluer than a neutral far region. No pointer events are fed, so
/// the only steerable warmth in play is the click's — `PLUME_CLICK_OK` prints
/// only if near-Q blue beats the far reference by a clear margin.
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn plume_example_click_pinned_vortex_e2e() {
    let tmp = wasm_test_dir("plumeclick");
    let path = tmp.join("plume.kara");
    std::fs::write(&path, include_str!("../examples/plume/plume.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: plume_example_click_pinned_vortex_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "plume build failed: {stderr}");
    assert!(tmp.join("plume.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./plume.js";
const W = 520, H = 340;
// Click point Q: clear of the cursor's default centre (0.5,0.5) and the three
// fixed vortices (0.25,0.32)/(0.74,0.66)/(0.50,0.85). 0.85,0.20 normalized.
const QX = Math.round(0.85 * W), QY = Math.round(0.20 * H); // 442, 68
// A neutral far reference, clear of Q, the centre, and the vortices.
const FX = Math.round(0.15 * W), FY = Math.round(0.55 * H); // 78, 187
// The clicks glue reads offsetX/offsetY first (clientX/Y is the fallback).
class Click extends Event {
  constructor(x, y) { super("click", { cancelable: true }); this.offsetX = x; this.offsetY = y; }
}
const target = new EventTarget();
const iv = setInterval(() => target.dispatchEvent(new Click(QX, QY)), 10);
const bail = setTimeout(() => { console.error("FAIL: too few frames rendered"); process.exit(2); }, 12000);

// Average a colour channel over a window — averaging washes out the t-noise so
// the steady additive glow stands out.
function avgC(view, cx, cy, ch) {
  let sum = 0, n = 0;
  for (let dy = -10; dy <= 10; dy++) {
    for (let dx = -10; dx <= 10; dx++) {
      const x = cx + dx, y = cy + dy;
      if (x < 0 || x >= W || y < 0 || y >= H) continue;
      sum += view[(y * W + x) * 4 + ch]; n++;
    }
  }
  return sum / n;
}

let frameCount = 0;
function put_pixels(ptr, len, w, h, ctx) {
  frameCount++;
  // Assert on a settled frame (the click has been picked up by then).
  if (frameCount === 10) {
    const view = new Uint8Array(ctx.memory.buffer, Number(ptr), Number(len));
    let mn = 255, mx = 0;
    for (let i = 0; i < view.length; i += 4) { const b = view[i + 2]; if (b < mn) mn = b; if (b > mx) mx = b; }
    if (mx - mn < 20) { console.error("FAIL: framebuffer too uniform (mn=" + mn + " mx=" + mx + ")"); process.exit(3); }
    const nearB = avgC(view, QX, QY, 2);
    const farB = avgC(view, FX, FY, 2);
    console.log("PLUME_CLICK frames=" + frameCount + " nearB=" + nearB.toFixed(1) + " farB=" + farB.toFixed(1));
    if (nearB <= farB + 20) { console.error("FAIL: pinned-vortex glow not placed (nearB=" + nearB.toFixed(1) + " farB=" + farB.toFixed(1) + ")"); process.exit(4); }
    clearInterval(iv); clearTimeout(bail);
    console.log("PLUME_CLICK_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { clickTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: plume_example_click_pinned_vortex_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("PLUME_CLICK_OK"),
        "plume click harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Fathom (examples/fathom/mandelbrot.kara) wires two newer `std.web.events`
/// `ClickEvent` producers into the live render loop: `dblclick()` dives the
/// view IN toward the clicked point, `contextmenu()` (right-click) backs it
/// OUT. This is the *demo-level* proof they drive a real app end to end — not
/// just the producer-level payload e2e in the host-async tests above.
///
/// The signal is clean because Fathom's render is **deterministic per view**:
/// with no input, every frame is byte-identical (there is no animation phase,
/// unlike Plume's `t`). So a frame whose checksum MOVES off the no-input
/// baseline can only mean a click event crossed host→wasm, was `recv`d, and
/// shifted (cx, cy, scale). The harness:
///   1. captures a baseline frame before dispatching anything (and asserts it
///      has real structure, so rendering works at all),
///   2. fires `dblclick` at a fixed off-centre point for a run of frames — the
///      compounding zoom makes the checksum diverge hugely from baseline,
///   3. switches to `contextmenu` (right-click) for another run — the view
///      backs out, diverging again from the zoomed checksum.
///
/// Each leg asserts the checksum moved; `FATHOM_OK` prints only if both did.
/// One dropped/garbled coordinate read can't mask it — the cumulative zoom
/// across the run dominates, so no loop-until-valid drain is needed here.
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn fathom_example_dblclick_contextmenu_zoom_e2e() {
    let tmp = wasm_test_dir("fathom");
    let path = tmp.join("mandelbrot.kara");
    std::fs::write(&path, include_str!("../examples/fathom/mandelbrot.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: fathom_example_dblclick_contextmenu_zoom_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "fathom build failed: {stderr}");
    assert!(tmp.join("mandelbrot.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./mandelbrot.js";
const W = 640, H = 420;
// An off-centre point so the zoom shifts (cx, cy) too, not just scale — the
// frame change is then unmistakable. CSS px == internal px at 1:1.
const PX = 470, PY = 300;
// The dblclick/contextmenu glue reads offsetX/offsetY first (clientX/Y is the
// fallback); cancelable so contextmenu's preventDefault has something to cancel.
class Click extends Event {
  constructor(type, x, y) { super(type, { cancelable: true }); this.offsetX = x; this.offsetY = y; }
}
const target = new EventTarget();
const bail = setTimeout(() => { console.error("FAIL: too few frames rendered"); process.exit(2); }, 20000);

// Cheap order-sensitive 32-bit checksum over the RGBA bytes. Two genuinely
// different views collide with astronomically small probability.
function sum(view) {
  let s = 0;
  for (let i = 0; i < view.length; i += 4) { s = (s * 31 + view[i] + view[i + 1] * 3 + view[i + 2] * 7) >>> 0; }
  return s;
}

let frame = 0, baseline = 0, zoomed = 0, iv = null;
function put_pixels(ptr, len, w, h, ctx) {
  frame++;
  const view = new Uint8Array(ctx.memory.buffer, Number(ptr), Number(len));
  if (frame === 2) {
    // Untouched view — it must have real structure (the set's classic shape).
    let mn = 255, mx = 0;
    for (let i = 0; i < view.length; i += 4) { const r = view[i]; if (r < mn) mn = r; if (r > mx) mx = r; }
    if (mx - mn < 20) { console.error("FAIL: baseline framebuffer too uniform (mn=" + mn + " mx=" + mx + ")"); process.exit(3); }
    baseline = sum(view);
    // Only NOW start feeding input, so the baseline is genuinely input-free.
    // One event per tick; the loop's per-frame coalescing try_recv tracks it.
    iv = setInterval(() => {
      if (frame < 16) target.dispatchEvent(new Click("dblclick", PX, PY));
      else if (frame < 32) target.dispatchEvent(new Click("contextmenu", PX, PY));
    }, 8);
  }
  if (frame === 16) {
    zoomed = sum(view);
    if (zoomed === baseline) { console.error("FAIL: dblclick did not change the view (sum=" + zoomed + ")"); process.exit(4); }
  }
  if (frame === 32) {
    const backed = sum(view);
    if (backed === zoomed) { console.error("FAIL: contextmenu did not change the view (sum=" + backed + ")"); process.exit(5); }
    if (iv !== null) clearInterval(iv);
    clearTimeout(bail);
    console.log("FATHOM base=" + baseline + " zoomed=" + zoomed + " backed=" + backed);
    console.log("FATHOM_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { dblclickTarget: target, contextmenuTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: fathom_example_dblclick_contextmenu_zoom_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("FATHOM_OK"),
        "fathom harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Fathom wires the newest `std.web.events` family — the touch gesture trio
/// `touchstart()` / `touchmove()` / `touchend()` — into the live render loop as
/// single-finger touch-PAN: `touchstart` records the finger's anchor and the
/// view centre at grab, `touchmove` drags the view so the grabbed complex point
/// tracks the finger (the SAME keep-the-point-fixed transform the pointer drag
/// uses), `touchend` ends the gesture. This is the *demo-level* proof the touch
/// producers drive a real app end to end — the mobile-input counterpart of the
/// `dblclick`/`contextmenu` zoom test above, leaning on the same property.
///
/// The signal is clean because Fathom's render is **deterministic per view**:
/// with no input, every frame is byte-identical (no animation phase). So a
/// frame whose checksum MOVES off the no-input baseline can only mean a touch
/// event crossed host→wasm, was `recv`d, and shifted (cx, cy). Crucially the
/// view only moves in the `touchmove` handler (touchstart/touchend just toggle
/// gesture state), so a moved checksum proves the *pan* fired. The harness:
///   1. captures a baseline frame before dispatching anything (and asserts it
///      has real structure, so rendering works at all),
///   2. opens one gesture (`touchstart` at a fixed anchor) and then ramps a run
///      of `touchmove`s farther out each tick — the anchor-based pan tracks the
///      farthest move that lands, so the view marches monotonically off
///      baseline (robust to a coalesced/dropped intermediate move), then ends
///      the gesture with a `touchend` (exercising its `changedTouches[0]` glue
///      path).
///
/// `FATHOM_TOUCH_OK` prints only if the panned frame moved off baseline; the
/// 20 s bail catches "the pan never crossed". One dropped move can't mask it —
/// the ramped drag's farthest landed sample dominates.
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn fathom_example_touch_pan_e2e() {
    let tmp = wasm_test_dir("fathomtouch");
    let path = tmp.join("mandelbrot.kara");
    std::fs::write(&path, include_str!("../examples/fathom/mandelbrot.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: fathom_example_touch_pan_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "fathom build failed: {stderr}");
    assert!(tmp.join("mandelbrot.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./mandelbrot.js";
// Anchor (finger-down) point. CSS px == internal px at 1:1; a plain node
// EventTarget has no getBoundingClientRect, so the glue reports raw clientX/Y
// and the pan delta is exactly the dispatched delta.
const AX = 160, AY = 120;
// Synthetic touch events: a class extending Event carrying touches /
// changedTouches arrays of { clientX, clientY } (the shape the touch glue
// reads). touchstart/touchmove ride touches[0]; on a release the live touches
// list is empty so touchend rides changedTouches[0].
class TStart extends Event { constructor(x, y) { super("touchstart"); this.touches = [{ clientX: x, clientY: y }]; this.changedTouches = this.touches; } }
class TMove extends Event { constructor(x, y) { super("touchmove", { cancelable: true }); this.touches = [{ clientX: x, clientY: y }]; this.changedTouches = this.touches; } }
class TEnd extends Event { constructor(x, y) { super("touchend"); this.touches = []; this.changedTouches = [{ clientX: x, clientY: y }]; } }
const target = new EventTarget();
const bail = setTimeout(() => { console.error("FAIL: touch pan never crossed"); process.exit(2); }, 20000);

// Cheap order-sensitive 32-bit checksum over the RGBA bytes. Two genuinely
// different views collide with astronomically small probability.
function sum(view) {
  let s = 0;
  for (let i = 0; i < view.length; i += 4) { s = (s * 31 + view[i] + view[i + 1] * 3 + view[i + 2] * 7) >>> 0; }
  return s;
}

let frame = 0, baseline = 0, iv = null, started = false, step = 0;
function put_pixels(ptr, len, w, h, ctx) {
  frame++;
  const view = new Uint8Array(ctx.memory.buffer, Number(ptr), Number(len));
  if (frame === 2) {
    // Untouched view — it must have real structure (the set's classic shape),
    // proving rendering works before any input.
    let mn = 255, mx = 0;
    for (let i = 0; i < view.length; i += 4) { const r = view[i]; if (r < mn) mn = r; if (r > mx) mx = r; }
    if (mx - mn < 20) { console.error("FAIL: baseline framebuffer too uniform (mn=" + mn + " mx=" + mx + ")"); process.exit(3); }
    baseline = sum(view);
    // Only NOW open the gesture, so the baseline is genuinely input-free. One
    // touchstart (it stays pending until drained, so it can't be missed), then
    // a touchmove farther out each tick — the loop's per-frame coalescing
    // try_recv tracks the latest, and the anchor-based pan marches the view off
    // baseline.
    iv = setInterval(() => {
      if (!started) { target.dispatchEvent(new TStart(AX, AY)); started = true; }
      step++;
      target.dispatchEvent(new TMove(AX + 6 * step, AY + 4 * step));
    }, 8);
  }
  if (frame === 16) {
    const panned = sum(view);
    // Close the gesture (exercise the touchend changedTouches[0] glue path) and
    // stop feeding input.
    target.dispatchEvent(new TEnd(AX + 6 * step, AY + 4 * step));
    if (iv !== null) clearInterval(iv);
    clearTimeout(bail);
    if (panned === baseline) { console.error("FAIL: touch pan did not move the view (sum=" + panned + ")"); process.exit(4); }
    console.log("FATHOM base=" + baseline + " panned=" + panned);
    console.log("FATHOM_TOUCH_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { touchTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: fathom_example_touch_pan_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("FATHOM_TOUCH_OK"),
        "fathom touch-pan harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Fathom also wires the UNIT-payload `std.web.events` pair `focus()` / `blur()`
/// to GATE the render: while the tab is unfocused the loop still spins and
/// drains every input channel, but skips the parallel `render_frame` — so a
/// backgrounded tab stops driving the worker pool. This is the demo-level proof
/// the gate works end to end, and it exercises a property the click test can't:
/// the ABSENCE of rendering.
///
/// Two complementary assertions, both leaning on Fathom's per-call `put_pixels`
/// being the one and only "a frame was painted" signal:
///   1. NEGATIVE — after a `blur`, `put_pixels` must not fire AT ALL across a
///      600 ms window, even though `dblclick`s are dispatched the whole time
///      (each would visibly zoom if rendered). If any frame paints while
///      blurred, the gate leaked and we exit non-zero immediately.
///   2. POSITIVE — the moment a `focus` arrives, the loop resumes and the very
///      next `put_pixels` shows a view that MOVED off the pre-blur baseline:
///      proof the loop kept draining input while paused (so the queued zoom
///      accumulated) and that `focus` actually un-gated the render.
///
/// `FATHOM_PAUSE_OK` prints only if the pause held and the resume painted a
/// changed frame; the 20 s bail catches "focus never resumed rendering".
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn fathom_example_focus_blur_pause_e2e() {
    let tmp = wasm_test_dir("fathompause");
    let path = tmp.join("mandelbrot.kara");
    std::fs::write(&path, include_str!("../examples/fathom/mandelbrot.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: fathom_example_focus_blur_pause_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "fathom build failed: {stderr}");
    assert!(tmp.join("mandelbrot.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./mandelbrot.js";
// An off-centre point so a single zoom step shifts (cx, cy) too, not just
// scale — the post-resume frame is then unmistakably different. CSS px ==
// internal px at 1:1.
const PX = 470, PY = 300;
class Click extends Event {
  constructor(type, x, y) { super(type, { cancelable: true }); this.offsetX = x; this.offsetY = y; }
}
// focus/blur are distinct DOM event types with no payload; the listeners read
// only that they fired. One EventTarget stands in for the window.
const target = new EventTarget();
const bail = setTimeout(() => { console.error("FAIL: focus never resumed rendering"); process.exit(2); }, 20000);

function sum(view) {
  let s = 0;
  for (let i = 0; i < view.length; i += 4) { s = (s * 31 + view[i] + view[i + 1] * 3 + view[i + 2] * 7) >>> 0; }
  return s;
}

let frame = 0, baseline = 0, phase = "warmup", dblIv = null;
function put_pixels(ptr, len, w, h, ctx) {
  frame++;
  const view = new Uint8Array(ctx.memory.buffer, Number(ptr), Number(len));
  if (frame === 2) {
    // Untouched view — it must have real structure (the set's classic shape),
    // proving rendering works before we gate it.
    let mn = 255, mx = 0;
    for (let i = 0; i < view.length; i += 4) { const r = view[i]; if (r < mn) mn = r; if (r > mx) mx = r; }
    if (mx - mn < 20) { console.error("FAIL: baseline framebuffer too uniform (mn=" + mn + " mx=" + mx + ")"); process.exit(3); }
    baseline = sum(view);
    // Enter the pause: blur now, then dispatch dblclicks throughout the window.
    // The guest keeps looping and draining them (the view state zooms), but with
    // the render gated NO further put_pixels must fire until we re-focus.
    phase = "paused";
    target.dispatchEvent(new Event("blur"));
    dblIv = setInterval(() => target.dispatchEvent(new Click("dblclick", PX, PY)), 8);
    setTimeout(() => { phase = "resumed"; target.dispatchEvent(new Event("focus")); }, 600);
    return;
  }
  if (phase === "paused") {
    // A frame painted while blurred → the render gate leaked.
    console.error("FAIL: rendered a frame while blurred (frame=" + frame + ")");
    if (dblIv !== null) clearInterval(dblIv);
    clearTimeout(bail);
    process.exit(4);
  }
  if (phase === "resumed") {
    // First frame after focus: the queued zoom that accumulated during the
    // pause must now be visible, so the view has moved off the baseline.
    const s = sum(view);
    if (dblIv !== null) clearInterval(dblIv);
    clearTimeout(bail);
    if (s === baseline) { console.error("FAIL: view unchanged after focus resume (sum=" + s + ")"); process.exit(5); }
    console.log("FATHOM base=" + baseline + " resumed=" + s);
    console.log("FATHOM_PAUSE_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { dblclickTarget: target, focusTarget: target, blurTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: fathom_example_focus_blur_pause_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("FATHOM_PAUSE_OK"),
        "fathom pause harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Fathom wires `std.web.events.resize()` to make the canvas RESPONSIVE: the
/// framebuffer width/height became runtime state, and each `resize` reflows them
/// to the window's current `innerWidth`/`innerHeight`. This is the demo-level
/// proof — and it exercises a dimension the other Fathom tests don't: the
/// framebuffer geometry itself changing mid-run, threaded through the SIMD
/// render path and the `put_pixels` blit.
///
/// The signal is structural and exact: `put_pixels(ptr, len, w, h)` reports the
/// frame's dimensions, so a reflow shows up as a changed `(w, h)` AND a changed
/// `len` (= w*h*4). The harness drives the window dims through three sizes:
///   1. baseline at the default 640x420 (asserts the frame has real structure),
///   2. SHRINK to an ODD innerWidth 321x240 — the guest must floor the width to
///      an even 320 (the SIMD pair loop's invariant), so `put_pixels` reports
///      exactly 320x240 and len 320*240*4,
///   3. GROW to 900x600 — `put_pixels` reports 900x600 and len 900*600*4.
///
/// Each post-reflow frame must also still render real structure at the new size
/// (not a torn/zero buffer). `FATHOM_RESIZE_OK` prints only if all three legs
/// match; the 20 s bail catches "a reflow never took".
///
/// Doubles as a bit-rot guard on the example source (built from the committed
/// file, not an inline copy).
#[test]
fn fathom_example_resize_reflow_e2e() {
    let tmp = wasm_test_dir("fathomresize");
    let path = tmp.join("mandelbrot.kara");
    std::fs::write(&path, include_str!("../examples/fathom/mandelbrot.kara")).unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: fathom_example_resize_reflow_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "fathom build failed: {stderr}");
    assert!(tmp.join("mandelbrot.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./mandelbrot.js";
// One object stands in for the window: `resize` reads innerWidth/innerHeight
// off it (the DOM resize event carries no dimensions), so we mutate these and
// dispatch a bare "resize" to drive a reflow.
const target = new EventTarget();
target.innerWidth = 640;
target.innerHeight = 420;
const bail = setTimeout(() => { console.error("FAIL: a reflow never took"); process.exit(2); }, 20000);

// Real structure at the current size = the frame isn't a torn/zero buffer.
function nonUniform(view) {
  let mn = 255, mx = 0;
  for (let i = 0; i < view.length; i += 4) { const r = view[i]; if (r < mn) mn = r; if (r > mx) mx = r; }
  return mx - mn >= 20;
}

let frame = 0, phase = "warmup", rzIv = null;
function put_pixels(ptr, len, w, h, ctx) {
  frame++;
  w = Number(w); h = Number(h); len = Number(len);
  const view = new Uint8Array(ctx.memory.buffer, Number(ptr), len);
  if (phase === "warmup") {
    if (frame < 2) return; // let the first frame settle
    if (w !== 640 || h !== 420) { console.error("FAIL: default dims " + w + "x" + h); process.exit(3); }
    if (len !== 640 * 420 * 4) { console.error("FAIL: default len " + len); process.exit(3); }
    if (!nonUniform(view)) { console.error("FAIL: default frame too uniform"); process.exit(3); }
    // SHRINK to an ODD width — the guest must floor it to an even 320.
    phase = "shrink";
    target.innerWidth = 321;
    target.innerHeight = 240;
    rzIv = setInterval(() => target.dispatchEvent(new Event("resize")), 10);
    return;
  }
  if (phase === "shrink") {
    if (w === 640) return; // pre-reflow frames still in flight
    if (w !== 320 || h !== 240) { console.error("FAIL: shrink dims " + w + "x" + h + " (want 320x240, odd width must floor even)"); process.exit(4); }
    if (len !== 320 * 240 * 4) { console.error("FAIL: shrink len " + len); process.exit(4); }
    if (!nonUniform(view)) { console.error("FAIL: shrink frame too uniform"); process.exit(4); }
    // GROW past the original size.
    phase = "grow";
    target.innerWidth = 900;
    target.innerHeight = 600;
    return;
  }
  if (phase === "grow") {
    if (w !== 900) return; // wait for the grow to take
    if (h !== 600) { console.error("FAIL: grow dims " + w + "x" + h); process.exit(5); }
    if (len !== 900 * 600 * 4) { console.error("FAIL: grow len " + len); process.exit(5); }
    if (!nonUniform(view)) { console.error("FAIL: grow frame too uniform"); process.exit(5); }
    if (rzIv !== null) clearInterval(rzIv);
    clearTimeout(bail);
    console.log("FATHOM dims 640x420 -> 320x240 -> 900x600");
    console.log("FATHOM_RESIZE_OK");
    process.exit(0);
  }
}
run({ put_pixels }, { resizeTarget: target }).catch((e) => {
  console.error("run failed: " + (e && e.message ? e.message : e));
  process.exit(1);
});
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: fathom_example_resize_reflow_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let so = String::from_utf8_lossy(&node_out.stdout);
    let se = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success() && so.contains("FATHOM_RESIZE_OK"),
        "fathom resize harness failed under node: stdout={so} stderr={se}",
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate: the same host-async producer built WITHOUT
/// `--features wasm-threads` is a hard compile error (codegen, pre-link),
/// pointing at the flag — never a silent zero-value `recv`. Fires during
/// IR emission (no runtime archive / linker / node needed) but DOES need
/// codegen: under a no-`llvm` build `karac build` falls back to a type
/// check, which the gate (a codegen-time check) never reaches and which
/// passes cleanly — so this test skips on that config via
/// `wasm_build_skip_reason` (mirroring `wasm_browser_build_aborts_on_target_gate_violation`).
#[test]
fn wasm_time_after_sequential_target_rejected() {
    let tmp = wasm_test_dir("wttimergate");
    let path = tmp.join("timer.kara");
    std::fs::write(
        &path,
        "import std.web.time.{after, Duration};\n\n\
         fn main() {\n    \
             let rx = after(Duration.ms(40));\n    \
             rx.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // No-`llvm` build: `karac build` warns "requires the llvm feature" and
    // falls back to a type check that never reaches the codegen gate — skip
    // rather than mis-assert a build failure.
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_time_after_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm host-async producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The sequential-target gate for `std.web.time.every` — sibling of
/// `wasm_time_after_sequential_target_rejected`. The interval producer built
/// WITHOUT `--features wasm-threads` is a hard compile error pointing at the
/// flag, never a silent zero-value `recv`. Same no-`llvm` skip rationale.
#[test]
fn wasm_every_sequential_target_rejected() {
    let tmp = wasm_test_dir("wteverygate");
    let path = tmp.join("every.kara");
    std::fs::write(
        &path,
        "import std.web.time.{every, Duration};\n\n\
         fn main() {\n    \
             let rx = every(Duration.ms(15));\n    \
             rx.recv();\n}\n",
    )
    .unwrap();
    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--bindings=none",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_every_sequential_target_rejected — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        !out.status.success(),
        "sequential wasm host-async producer must be rejected, but build succeeded: {stderr}"
    );
    assert!(
        stderr.contains("requires `--features wasm-threads`"),
        "gate must name the flag: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Project-mode artifact set: a wasm-threads build emits BOTH modules
/// plus the glue + declarations, and the glue carries the load-time
/// pick machinery with the manifest's `[wasm]` knobs baked in.
#[test]
fn wasm_threads_project_emits_dual_artifact_set() {
    let tmp = wasm_test_dir("wtproj");
    std::fs::create_dir_all(tmp.join("src")).unwrap();
    std::fs::write(
        tmp.join("kara.toml"),
        "[package]\nname = \"dualapp\"\nedition = \"2026\"\n\n[wasm]\npool-size = 3\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("src/main.kara"),
        "fn main() {\n    println(\"dual\");\n}\n",
    )
    .unwrap();

    let out = karac_bin()
        .args(["build", "--target=wasm_browser", "--features=wasm-threads"])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_project_emits_dual_artifact_set — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(
        out.status.success(),
        "wasm-threads project build failed: {stderr}"
    );
    let dist = tmp.join("dist").join("wasm");
    for artifact in [
        "dualapp.wasm",
        "dualapp.threads.wasm",
        "dualapp.js",
        "dualapp.d.ts",
    ] {
        assert!(
            dist.join(artifact).exists(),
            "missing dist artifact {artifact}"
        );
    }
    // Both modules are core modules (no componentization on this path).
    assert_eq!(
        wasm_artifact_kind(&dist.join("dualapp.wasm")),
        "core module"
    );
    assert_eq!(
        wasm_artifact_kind(&dist.join("dualapp.threads.wasm")),
        "core module"
    );
    let glue = std::fs::read_to_string(dist.join("dualapp.js")).unwrap();
    assert!(glue.contains("const WASM_THREADS_FILENAME = \"dualapp.threads.wasm\";"));
    assert!(
        glue.contains("const THREADS_POOL_SIZE = 3;"),
        "[wasm] pool-size must bake into the glue"
    );
    let dts = std::fs::read_to_string(dist.join("dualapp.d.ts")).unwrap();
    assert!(
        dts.contains("KaraThreadedHandle"),
        "threaded d.ts surface missing"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The headline E2E: spawn/TaskGroup/par run on REAL worker threads
/// under the threaded module on node. Load-immune threading evidence:
/// the threaded scheduler's `task_join` Condvar-blocks with no
/// work-helping, so `h.join()` returning at all proves a pool worker —
/// a real Web Worker thread — executed the task. Also pins the
/// forced-fallback path: `crossOriginIsolated = false` makes the glue
/// console.warn and run the sequential module, same output.
#[test]
fn wasm_threads_spawn_join_runs_on_worker_pool_e2e() {
    let tmp = wasm_test_dir("wte2e");
    let path = tmp.join("threaded.kara");
    std::fs::write(
        &path,
        r#"
fn add(a: i64, b: i64) -> i64 {
    a + b
}

fn worker(id: i64) {
    println(id)
}

fn main() {
    let h: TaskHandle[i64] = spawn(|| add(40, 2));
    let r: i64 = h.join();
    println(r);

    par {
        println("pa");
        println("pb");
    }

    let mut tg = TaskGroup.new();
    tg.spawn(|| worker(1));
    tg.spawn(|| worker(2));
    tg.spawn(|| worker(3));
}
"#,
    )
    .unwrap();

    let out = karac_bin()
        .args([
            "build",
            path.to_str().unwrap(),
            "--target=wasm_browser",
            "--features=wasm-threads",
        ])
        .current_dir(&tmp)
        .env_remove("KARAC_RUNTIME")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if let Some(reason) = wasm_build_skip_reason(&stderr) {
        eprintln!("skip: wasm_threads_spawn_join_runs_on_worker_pool_e2e — {reason}");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    assert!(out.status.success(), "wasm-threads build failed: {stderr}");
    assert!(tmp.join("threaded.threads.wasm").exists());

    let harness = tmp.join("harness.mjs");
    std::fs::write(
        &harness,
        r#"import { run } from "./threaded.js";

// Threaded pick: node has SAB unconditionally, so run() must take the
// worker-pool path and resolve with the threaded handle shape.
const h = await run({});
if (h.threaded !== true) throw new Error("expected the threaded module pick");
console.log("THREADED_OK");

// Forced fallback: an explicit crossOriginIsolated=false simulates a
// deploy without COOP/COEP. The glue must console.warn and run the
// sequential module to the same program output.
globalThis.crossOriginIsolated = false;
const warns = [];
const origWarn = console.warn;
console.warn = (...a) => warns.push(a.join(" "));
const s = await run({});
console.warn = origWarn;
if (s.threaded === true) throw new Error("fallback must use the sequential module");
if (!warns.some((w) => w.includes("falling back to the sequential")))
  throw new Error("missing the fallback console.warn, got: " + JSON.stringify(warns));
console.log("FALLBACK_OK");

// forceSequential opt: same sequential path, no warn.
const f = await run({}, { forceSequential: true });
if (f.threaded === true) throw new Error("forceSequential must skip the pick");
console.log("FORCE_SEQ_OK");
"#,
    )
    .unwrap();
    let node = std::process::Command::new("node")
        .arg(&harness)
        .current_dir(&tmp)
        .output();
    let Ok(node_out) = node else {
        eprintln!("skip: wasm_threads_spawn_join_runs_on_worker_pool_e2e — node not on PATH");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    };
    let node_stdout = String::from_utf8_lossy(&node_out.stdout);
    let node_stderr = String::from_utf8_lossy(&node_out.stderr);
    assert!(
        node_out.status.success(),
        "threaded harness failed under node: stdout={node_stdout} stderr={node_stderr}",
    );
    for marker in ["THREADED_OK", "FALLBACK_OK", "FORCE_SEQ_OK"] {
        assert!(
            node_stdout.contains(marker),
            "missing {marker}: stdout={node_stdout} stderr={node_stderr}",
        );
    }
    // Program output correctness on BOTH paths: the join result, both
    // par branches, and the three group workers each appear at least
    // twice (threaded run + fallback run; the forceSequential run makes
    // three). Cross-thread print ORDER is intentionally unasserted.
    for needle in ["42", "pa", "pb", "1", "2", "3"] {
        let count = node_stdout.matches(needle).count();
        assert!(
            count >= 2,
            "program output `{needle}` seen {count}x (expected from both the \
             threaded and fallback runs): stdout={node_stdout}",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Cross-package module loading (phase-5 line 898) ─────────────
//
// `import <pkg-name>.…` resolves into a resolved path-dependency's module
// tree: the dep's `lib.kara` items are importable as `import pkg.Item`,
// its submodules as `import pkg.sub.Item`, intra-dep imports keep working
// via the tree-build prefix rewrite, and only `pub` items cross the
// package boundary. The build path is strict (diagnostics + halt); the
// `karac run` path merges deps leniently into the interpreter
// super-program.

/// Lay down a two-package fixture: a root `app` (binary) depending on a
/// path-dep `mathx` (library with a hoisted lib.kara item, a submodule,
/// and an intra-dep import of the hoisted item). Returns the app dir.
fn xpkg_fixture(slug: &str) -> std::path::PathBuf {
    let tmp = slice7_tempdir(slug);
    std::fs::create_dir_all(tmp.join("app/src")).unwrap();
    std::fs::create_dir_all(tmp.join("mathx/src")).unwrap();
    std::fs::write(
        tmp.join("app/kara.toml"),
        r#"[package]
name = "app"

[dependencies]
mathx = { path = "../mathx" }
"#,
    )
    .unwrap();
    std::fs::write(tmp.join("mathx/kara.toml"), "[package]\nname = \"mathx\"\n").unwrap();
    std::fs::write(
        tmp.join("mathx/src/lib.kara"),
        "pub fn double(x: i64) -> i64 { x * 2 }\nfn internal_only(x: i64) -> i64 { x }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("mathx/src/geo.kara"),
        "import double;\npub fn area(w: i64, h: i64) -> i64 { double(w * h) / 2 }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("app/src/main.kara"),
        "import mathx.double;\nimport mathx.geo.area;\n\nfn main() {\n    println(double(21));\n    println(area(3, 4));\n}\n",
    )
    .unwrap();
    tmp
}

#[test]
fn test_xpkg_import_typechecks_and_builds() {
    let tmp = xpkg_fixture("xpkg-build");
    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "cross-package build should succeed; stdout={stdout} stderr={stderr}",
    );
    // The summary names the dependency and its module count.
    assert!(
        stdout.contains("deps:    1 package(s), 2 module(s)"),
        "dep summary expected; stdout={stdout}",
    );
    assert!(stdout.contains("mathx"), "dep name listed; stdout={stdout}");

    // Under llvm the produced binary must run with the interpreter-
    // identical output (A/B surface).
    #[cfg(feature = "llvm")]
    {
        let exe = tmp.join("app/app");
        assert!(exe.is_file(), "executable produced at {}", exe.display());
        let run = std::process::Command::new(&exe).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "42\n12\n",
            "compiled output matches interpreter",
        );
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn test_xpkg_run_interpreter_sees_dep_modules() {
    let tmp = xpkg_fixture("xpkg-run");
    let out = karac_bin()
        .arg("run")
        .arg("src/main.kara")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(
        out.status.success(),
        "karac run should succeed; stdout={stdout} stderr={stderr}",
    );
    assert_eq!(stdout, "42\n12\n", "interpreter output");
}

#[test]
fn test_xpkg_nonpub_item_rejected_across_packages() {
    let tmp = xpkg_fixture("xpkg-nonpub");
    std::fs::write(
        tmp.join("app/src/main.kara"),
        "import mathx.internal_only;\nfn main() { println(internal_only(1)); }\n",
    )
    .unwrap();
    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "non-pub import must fail");
    assert!(
        stderr.contains("error[E0222]"),
        "E0222 expected; stderr={stderr}",
    );
    assert!(
        stderr.contains("only `pub` items can be imported across packages"),
        "cross-package message expected; stderr={stderr}",
    );
}

#[test]
fn test_xpkg_local_module_shadows_dep() {
    let tmp = xpkg_fixture("xpkg-shadow");
    // A local `src/mathx.kara` fully shadows the dep of the same name.
    std::fs::write(
        tmp.join("app/src/mathx.kara"),
        "pub fn local_thing() -> i64 { 7 }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("app/src/main.kara"),
        "import mathx.local_thing;\nfn main() { println(local_thing()); }\n",
    )
    .unwrap();
    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "local module wins; stdout={stdout} stderr={stderr}",
    );

    // And the dep's submodule is inaccessible while shadowed.
    std::fs::write(
        tmp.join("app/src/main.kara"),
        "import mathx.geo.area;\nfn main() { println(area(3, 4)); }\n",
    )
    .unwrap();
    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "shadowed dep must be inaccessible");
    assert!(
        stderr.contains("error[E0224]") && stderr.contains("mathx.geo"),
        "E0224 unknown module expected; stderr={stderr}",
    );
}

#[test]
fn test_xpkg_binary_dep_rejected() {
    let tmp = xpkg_fixture("xpkg-bindep");
    // Replace the dep's lib entry with a binary entry.
    std::fs::remove_file(tmp.join("mathx/src/lib.kara")).unwrap();
    std::fs::remove_file(tmp.join("mathx/src/geo.kara")).unwrap();
    std::fs::write(tmp.join("mathx/src/main.kara"), "fn main() {}\n").unwrap();
    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&tmp);
    assert!(!out.status.success(), "binary dep must halt the build");
    assert!(
        stderr.contains("in dependency `mathx`") && stderr.contains("not a library package"),
        "non-library diagnostic expected; stderr={stderr}",
    );
}

#[test]
fn test_xpkg_transitive_path_dep_resolves() {
    let tmp = xpkg_fixture("xpkg-transitive");
    // mathx itself depends on basep; app only declares mathx.
    std::fs::create_dir_all(tmp.join("basep/src")).unwrap();
    std::fs::write(tmp.join("basep/kara.toml"), "[package]\nname = \"basep\"\n").unwrap();
    std::fs::write(
        tmp.join("basep/src/lib.kara"),
        "pub fn base_val() -> i64 { 100 }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("mathx/kara.toml"),
        "[package]\nname = \"mathx\"\n\n[dependencies]\nbasep = { path = \"../basep\" }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("mathx/src/geo.kara"),
        "import double;\nimport basep.base_val;\npub fn area(w: i64, h: i64) -> i64 { double(w * h) / 2 + base_val() }\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("app/src/main.kara"),
        "import mathx.geo.area;\nfn main() { println(area(3, 4)); }\n",
    )
    .unwrap();
    let run = karac_bin()
        .arg("run")
        .arg("src/main.kara")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        run.status.success(),
        "transitive dep run; stdout={stdout} stderr={stderr}",
    );
    assert_eq!(stdout, "112\n", "12 + 100 through two package hops");

    let out = karac_bin()
        .arg("build")
        .current_dir(tmp.join("app"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "transitive dep build; stdout={stdout} stderr={stderr}",
    );
    assert!(
        stdout.contains("deps:    2 package(s)"),
        "both packages in summary; stdout={stdout}",
    );
    #[cfg(feature = "llvm")]
    {
        let exe = tmp.join("app/app");
        let run = std::process::Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&run.stdout), "112\n");
    }
    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Cross-package module loading — `karac test` surface ─────────
//
// Resolver-block follow-up (f): the test runner mirrors the build path's
// dep wiring — `run_dep_resolution` with `include_dev_deps: true`, path-dep
// walks, `build_program_tree_with_deps` — so a root package's tests can
// `import <pkg>.…` from both `[dependencies]` and `[dev-dependencies]`.
// Dep test companions stay excluded: only the root package's tests run.

#[test]
fn test_xpkg_test_runner_imports_path_dep_items() {
    let tmp = xpkg_fixture("xpkg-test-dep");
    write(
        &tmp.join("app/src/main_test.kara"),
        "import mathx.double;\nimport mathx.geo.area;\n\
         test \"dep double\" { assert_eq(double(21), 42); }\n\
         test \"dep area\" { assert_eq(area(3, 4), 12); }\n",
    );
    let out = karac_bin()
        .current_dir(tmp.join("app"))
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "tests importing path-dep items should pass; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    assert!(lines[0].contains("\"total_tests\":2"), "got: {lines:?}");
    let pass_count = lines
        .iter()
        .filter(|l| event_kind(l) == Some("test_pass"))
        .count();
    assert_eq!(pass_count, 2, "expected 2 test_pass events; got: {lines:?}");
}

#[test]
fn test_xpkg_test_runner_resolves_dev_dependency_imports() {
    // A dep declared ONLY under [dev-dependencies] is invisible to `karac
    // build` but must resolve for `karac test` — the test/build split.
    let tmp = slice7_tempdir("xpkg-test-devdep");
    std::fs::create_dir_all(tmp.join("app/src")).unwrap();
    std::fs::create_dir_all(tmp.join("helper/src")).unwrap();
    write(
        &tmp.join("app/kara.toml"),
        "[package]\nname = \"app\"\n\n[dev-dependencies]\nhelper = { path = \"../helper\" }\n",
    );
    write(
        &tmp.join("helper/kara.toml"),
        "[package]\nname = \"helper\"\n",
    );
    write(
        &tmp.join("helper/src/lib.kara"),
        "pub fn fake_val() -> i64 { 99 }\n",
    );
    write(&tmp.join("app/src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("app/src/main_test.kara"),
        "import helper.fake_val;\ntest \"dev dep import\" { assert_eq(fake_val(), 99); }\n",
    );
    let out = karac_bin()
        .current_dir(tmp.join("app"))
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "dev-dep import should pass under `karac test`; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":1"), "got: {lines:?}");
}

#[test]
fn test_xpkg_test_runner_excludes_dep_test_companions() {
    // The dep package ships its own `_test.kara` companion; a consumer
    // never compiles or runs its deps' tests. Only the root's one test
    // must be discovered.
    let tmp = xpkg_fixture("xpkg-test-depcompanion");
    write(
        &tmp.join("mathx/src/lib_test.kara"),
        "test \"dep internal\" { assert_eq(1, 2); }\n",
    );
    write(
        &tmp.join("app/src/main_test.kara"),
        "import mathx.double;\ntest \"root only\" { assert_eq(double(2), 4); }\n",
    );
    let out = karac_bin()
        .current_dir(tmp.join("app"))
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "root suite must pass (a failing dep test running would flunk it); stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    assert!(
        lines[0].contains("\"total_tests\":1"),
        "dep companion tests must not be discovered; got: {lines:?}"
    );
    assert!(
        !stdout.contains("dep internal"),
        "dep test name must not appear; stdout:\n{stdout}"
    );
}

#[test]
fn test_xpkg_test_runner_nonpub_item_rejected() {
    // Cross-package visibility applies to test files exactly as to
    // production code: importing a non-`pub` dep item is a compile
    // failure, surfaced as a resolve_error event with exit non-zero and
    // no run_start.
    let tmp = xpkg_fixture("xpkg-test-nonpub");
    write(
        &tmp.join("app/src/main_test.kara"),
        "import mathx.internal_only;\ntest \"sneaky\" { assert_eq(internal_only(1), 1); }\n",
    );
    let out = karac_bin()
        .current_dir(tmp.join("app"))
        .arg("test")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "non-pub import must fail; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("\"type\":\"resolve_error\"") && stdout.contains("E0222"),
        "E0222 resolve_error expected; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains("run_start"),
        "compile failure must precede any run_start; stdout:\n{stdout}"
    );
}

#[test]
fn test_test_companion_reimport_dedup_keeps_genuine_conflicts() {
    // A companion re-declaring the exact import its production sibling has
    // is deduped at companion-merge (exercised by
    // test_xpkg_test_runner_imports_path_dep_items, whose fixture imports
    // the same dep items in both files). The dedup must NOT swallow a
    // genuine conflict: the same bound name imported from a *different*
    // module stays and errors as a duplicate definition.
    let tmp = scratch_project("test-companion-import-conflict");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(
        &tmp.join("src/util.kara"),
        "pub fn triple(x: i64) -> i64 { x * 3 }\n",
    );
    write(
        &tmp.join("src/other.kara"),
        "pub fn triple(x: i64) -> i64 { x * 30 }\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import util.triple;\nfn main() { let _ = triple(1); }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "import other.triple;\ntest \"conflict\" { assert_eq(triple(4), 12); }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !out.status.success(),
        "same binding from a different module must still conflict; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("\"type\":\"resolve_error\"") && stdout.contains("already defined"),
        "duplicate-definition resolve_error expected; stdout:\n{stdout}"
    );
}

#[test]
fn test_test_cross_module_import_executes() {
    // Same root cause as the dep case, single package: the per-test
    // execution program used to hold only the test module's own items, so
    // a name imported from a sibling module resolved and typechecked but
    // panicked the interpreter at execution ("should be caught by
    // resolver"). The runner now executes against the merged
    // super-program, like `karac run`.
    let tmp = scratch_project("test-cross-module-import");
    write(&tmp.join("kara.toml"), "[package]\nname = \"demo\"\n");
    write(&tmp.join("src/main.kara"), "fn main() {}\n");
    write(
        &tmp.join("src/util.kara"),
        "pub fn triple(x: i64) -> i64 { x * 3 }\n",
    );
    write(
        &tmp.join("src/main_test.kara"),
        "import util.triple;\ntest \"sibling import\" { assert_eq(triple(4), 12); }\n",
    );
    let out = karac_bin().current_dir(&tmp).arg("test").output().unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "sibling-module import should execute; stdout:\n{stdout}"
    );
    let lines = jsonl_lines(&stdout);
    let summary = lines.last().unwrap();
    assert!(summary.contains("\"passed\":1"), "got: {lines:?}");
}

/// Producer-mode library artifact (additive-interop Slice 2 + 3;
/// design.md § Exported C ABI). Builds a Kāra kernel as a `--crate-type
/// staticlib`, then compiles + links a plain C host against ONLY the
/// emitted `.a` + `.h` — no karac toolchain on the C link line — and
/// runs it. Proves the exported `pub extern "C" fn` surface is callable
/// from C, the runtime is bundled into the archive (self-contained), and
/// the header the emitter wrote matches the ABI.
///
/// Soft-skips (returns early) when the no-llvm fallback fires, the
/// runtime archive can't be located, or `cc` is absent — so the test
/// passes vacuously in those environments rather than failing on an
/// unrelated cause (same discipline as the other `#[cfg(feature =
/// "llvm")]` E2E tests in this file).
#[cfg(feature = "llvm")]
#[test]
fn test_build_crate_type_staticlib_links_from_c_e2e() {
    use std::io::Write;
    let src = "#[repr(C)]\n\
               pub struct Stats { sum: f64, count: i64 }\n\
               pub extern \"C\" fn add(a: i32, b: i32) -> i32 { a + b }\n\
               pub extern \"C\" fn stats_mean(s: Stats) -> f64 {\n\
                   if s.count == 0 { return 0.0; }\n\
                   s.sum / (s.count as f64)\n\
               }\n";
    let dir = std::env::temp_dir().join(format!("karac_staticlib_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut f = std::fs::File::create(dir.join("kernel.kara")).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }

    // Build the static library. Soft-skip on the no-llvm fallback or a
    // link failure (missing runtime archive on a fresh checkout).
    let build = karac_bin()
        .current_dir(&dir)
        .args(["build", "kernel.kara", "--crate-type", "staticlib"])
        .output()
        .unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let lib = dir.join("libkernel.a");
    let header = dir.join("libkernel.h");
    if berr.contains("requires the llvm feature")
        || berr.contains("link failed")
        || !lib.exists()
        || !header.exists()
    {
        eprintln!("skip: test_build_crate_type_staticlib_links_from_c_e2e — build/link soft-skip");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    // Rust-host std-collision smoothing: a staticlib build must print the
    // "link the cdylib for a Rust host" note (the .a bundles the runtime's
    // std, which collides with a Rust host's std at static-link time). The
    // note is stderr-only so it never pollutes the `Built:` stdout stream.
    assert!(
        berr.contains("link the cdylib") && berr.contains("Rust host"),
        "staticlib build should print the Rust-host cdylib note; stderr:\n{berr}"
    );
    let bout = String::from_utf8_lossy(&build.stdout);
    assert!(
        !bout.contains("link the cdylib"),
        "the Rust-host note must be stderr-only, not on stdout; stdout:\n{bout}"
    );

    // The emitted header must declare the exported surface + the struct +
    // the runtime lifecycle prototypes.
    let header_text = std::fs::read_to_string(&header).unwrap();
    // The header must also carry the Rust-host caveat so it travels with the
    // artifact for a dev who only reads the `.h`.
    assert!(
        header_text.contains("Rust hosts") && header_text.contains("rust_eh_personality"),
        "header missing Rust-host std-collision caveat:\n{header_text}"
    );
    assert!(
        header_text.contains("int32_t add(int32_t a, int32_t b);"),
        "header missing add proto:\n{header_text}"
    );
    assert!(
        header_text.contains("struct Stats {"),
        "header missing repr(C) struct:\n{header_text}"
    );
    assert!(
        header_text.contains("void karac_runtime_init(void);"),
        "header missing lifecycle proto:\n{header_text}"
    );

    // A plain C host that includes the header and calls in. No karac on
    // the compile/link line — just `cc` + the emitted `.a`/`.h`.
    let host_c = "#include <stdio.h>\n\
                  #include \"libkernel.h\"\n\
                  int main(void) {\n\
                      karac_runtime_init();\n\
                      struct Stats s = { .sum = 30.0, .count = 4 };\n\
                      printf(\"%d %.2f\\n\", add(20, 22), stats_mean(s));\n\
                      karac_runtime_shutdown();\n\
                      return 0;\n\
                  }\n";
    {
        let mut f = std::fs::File::create(dir.join("host.c")).unwrap();
        f.write_all(host_c.as_bytes()).unwrap();
    }

    // `-l:libkernel.a` forces the static archive (a bare `-lkernel` would
    // prefer a `.so` if one existed). If `cc` is unavailable, soft-skip.
    let cc = std::process::Command::new("cc")
        .current_dir(&dir)
        .args([
            "host.c",
            "-L.",
            "-l:libkernel.a",
            "-lpthread",
            "-lm",
            "-ldl",
            "-o",
            "host",
        ])
        .output();
    let cc = match cc {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skip: test_build_crate_type_staticlib_links_from_c_e2e — no `cc`");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    assert!(
        cc.status.success(),
        "C host failed to link against the Kāra staticlib:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );

    let run = common::output_with_hang_watchdog(
        {
            let mut c = std::process::Command::new(dir.join("host"));
            c.current_dir(&dir);
            c
        },
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(run) = run {
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "42 7.50",
            "C host produced wrong output calling the Kāra kernel"
        );
        assert_eq!(run.status.code(), Some(0));
    }
}

/// Producer-mode `#[repr(C)]` all-unit enum across the C ABI (spike
/// `repr-c-tagged-union-enums.md` Slice 1). An all-unit `#[repr(C)]` enum
/// crosses transparently as an `int64_t` (its value is the discriminant),
/// both as a return and a param. The header emits a `typedef int64_t <Name>`
/// plus named constants; a C host treats the enum type as the int64_t alias.
/// This is the authoritative ABI check that a single-field `i64` struct enum
/// value is register-identical to `int64_t` on the host target. Soft-skips
/// on the no-llvm fallback / missing runtime / missing `cc`.
#[cfg(feature = "llvm")]
#[test]
fn test_build_repr_c_enum_roundtrip_from_c_e2e() {
    use std::io::Write;
    let src = "#[repr(C)]\npub enum Status { Ok, NotFound, Denied }\n\
               pub extern \"C\" fn classify(code: i64) -> Status {\n\
               \x20   if code == 0 { return Status.Ok; }\n\
               \x20   if code == 404 { return Status.NotFound; }\n\
               \x20   Status.Denied\n\
               }\n\
               pub extern \"C\" fn escalate(s: Status) -> Status {\n\
               \x20   match s {\n\
               \x20       Status.Ok => Status.Ok,\n\
               \x20       Status.NotFound => Status.Denied,\n\
               \x20       Status.Denied => Status.Denied,\n\
               \x20   }\n\
               }\n";
    let dir = std::env::temp_dir().join(format!("karac_reprc_enum_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut f = std::fs::File::create(dir.join("kernel.kara")).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }

    let build = karac_bin()
        .current_dir(&dir)
        .args(["build", "kernel.kara", "--crate-type", "staticlib"])
        .output()
        .unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let lib = dir.join("libkernel.a");
    let header = dir.join("libkernel.h");
    if berr.contains("requires the llvm feature")
        || berr.contains("link failed")
        || !lib.exists()
        || !header.exists()
    {
        eprintln!("skip: test_build_repr_c_enum_roundtrip_from_c_e2e — build/link soft-skip");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    // Header carries the int64_t typedef + named constants (not an opaque
    // handle, not a bare C `enum`).
    let header_text = std::fs::read_to_string(&header).unwrap();
    assert!(
        header_text.contains("typedef int64_t Status;"),
        "header missing enum typedef:\n{header_text}"
    );
    assert!(
        header_text.contains("Status_NotFound = 1"),
        "header missing named constants:\n{header_text}"
    );
    assert!(
        header_text.contains("Status classify(int64_t code);")
            && header_text.contains("Status escalate(Status s);"),
        "header missing by-value enum prototypes:\n{header_text}"
    );

    let host_c = "#include <stdio.h>\n\
                  #include \"libkernel.h\"\n\
                  int main(void) {\n\
                      karac_runtime_init();\n\
                      Status a = classify(0);\n\
                      Status b = classify(404);\n\
                      Status c = classify(500);\n\
                      Status d = escalate(b);\n\
                      printf(\"%lld %lld %lld %lld %d\\n\",\n\
                             (long long)a, (long long)b, (long long)c, (long long)d,\n\
                             b == Status_NotFound);\n\
                      karac_runtime_shutdown();\n\
                      return 0;\n\
                  }\n";
    {
        let mut f = std::fs::File::create(dir.join("host.c")).unwrap();
        f.write_all(host_c.as_bytes()).unwrap();
    }

    let cc = std::process::Command::new("cc")
        .current_dir(&dir)
        .args([
            "host.c",
            "-L.",
            "-l:libkernel.a",
            "-lpthread",
            "-lm",
            "-ldl",
            "-o",
            "host",
        ])
        .output();
    let cc = match cc {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skip: test_build_repr_c_enum_roundtrip_from_c_e2e — no `cc`");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    assert!(
        cc.status.success(),
        "C host failed to link against the repr(C)-enum Kāra staticlib:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );

    let run = common::output_with_hang_watchdog(
        {
            let mut c = std::process::Command::new(dir.join("host"));
            c.current_dir(&dir);
            c
        },
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(run) = run {
        // Ok=0, NotFound=1, Denied=2, escalate(NotFound)=Denied=2, b==NotFound=1.
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "0 1 2 2 1",
            "repr(C) enum crossed the C ABI with the wrong value(s)"
        );
        assert_eq!(run.status.code(), Some(0));
    }
}

/// Producer-mode auto-boxing + auto-destructor (additive-interop Slice 4
/// Path B). A `pub extern "C" fn -> Vec[i64]` is auto-boxed for the C ABI:
/// the export returns an opaque `KaraVec_int64_t*` (heap box), the header
/// auto-emits the `{data,len,cap}` struct + a `karac_free_<name>`
/// destructor, and a C host reads the data through the struct and frees it
/// via the destructor. Proves the compiler-generated boxing + destructor
/// round-trips with no leak (a leak/UAF would surface as a crash or wrong
/// output here; the ASAN gate is the memory_sanitizer job). Soft-skips on
/// the no-llvm fallback / missing runtime / missing `cc`.
#[cfg(feature = "llvm")]
#[test]
fn test_build_auto_boxed_vec_return_e2e() {
    use std::io::Write;
    let src = "pub extern \"C\" fn make_vec(n: i64) -> Vec[i64] {\n\
               \x20   let mut v: Vec[i64] = Vec.new();\n\
               \x20   let mut i = 0;\n\
               \x20   while i < n { v.push(i * i); i = i + 1; }\n\
               \x20   v\n\
               }\n";
    let dir = std::env::temp_dir().join(format!("karac_pathb_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut f = std::fs::File::create(dir.join("k.kara")).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }
    let build = karac_bin()
        .current_dir(&dir)
        .args(["build", "k.kara", "--crate-type", "staticlib"])
        .output()
        .unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let lib = dir.join("libk.a");
    let header = dir.join("libk.h");
    if berr.contains("requires the llvm feature")
        || berr.contains("link failed")
        || !lib.exists()
        || !header.exists()
    {
        eprintln!("skip: test_build_auto_boxed_vec_return_e2e — build/link soft-skip");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let header_text = std::fs::read_to_string(&header).unwrap();
    assert!(
        header_text.contains("} KaraVec_int64_t;"),
        "boxed struct typedef missing:\n{header_text}"
    );
    assert!(
        header_text.contains("KaraVec_int64_t* make_vec(int64_t n);"),
        "boxed return proto missing:\n{header_text}"
    );
    assert!(
        header_text.contains("void karac_free_make_vec(KaraVec_int64_t* handle);"),
        "destructor proto missing:\n{header_text}"
    );

    let host_c = "#include <stdio.h>\n\
                  #include \"libk.h\"\n\
                  int main(void) {\n\
                      karac_runtime_init();\n\
                      KaraVec_int64_t* v = make_vec(6);\n\
                      long long sum = 0;\n\
                      for (int64_t i = 0; i < v->len; i++) sum += v->data[i];\n\
                      printf(\"%lld %lld\\n\", (long long)v->len, sum);\n\
                      karac_free_make_vec(v);\n\
                      karac_runtime_shutdown();\n\
                      return 0;\n\
                  }\n";
    {
        let mut f = std::fs::File::create(dir.join("host.c")).unwrap();
        f.write_all(host_c.as_bytes()).unwrap();
    }
    let cc = std::process::Command::new("cc")
        .current_dir(&dir)
        .args([
            "host.c",
            "-L.",
            "-l:libk.a",
            "-lpthread",
            "-lm",
            "-ldl",
            "-o",
            "host",
        ])
        .output();
    let cc = match cc {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skip: test_build_auto_boxed_vec_return_e2e — no `cc`");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    assert!(
        cc.status.success(),
        "C host failed to link the auto-boxed Kāra library:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = common::output_with_hang_watchdog(
        {
            let mut c = std::process::Command::new(dir.join("host"));
            c.current_dir(&dir);
            c
        },
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(run) = run {
        // n=6 → squares 0,1,4,9,16,25 → len 6, sum 55.
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "6 55");
        assert_eq!(run.status.code(), Some(0));
    }
}

/// Producer-mode auto-boxing follow-on (Slice 4 Path B): a `Vec[String]`
/// return nests transparently — `KaraVec_KaraString*` with each element a
/// `KaraString {data,len,cap}` — and its auto-destructor recursively frees
/// each element's buffer + the outer buffer. A C host reads the strings and
/// frees via the destructor. Soft-skips like the other producer E2E tests.
#[cfg(feature = "llvm")]
#[test]
fn test_build_auto_boxed_vec_string_return_e2e() {
    use std::io::Write;
    let src = "pub extern \"C\" fn names() -> Vec[String] {\n\
               \x20   let mut v: Vec[String] = Vec.new();\n\
               \x20   v.push(\"alice\"); v.push(\"bob\"); v.push(\"carol\");\n\
               \x20   v\n\
               }\n";
    let dir = std::env::temp_dir().join(format!("karac_pathb_vs_e2e_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    {
        let mut f = std::fs::File::create(dir.join("k.kara")).unwrap();
        f.write_all(src.as_bytes()).unwrap();
    }
    let build = karac_bin()
        .current_dir(&dir)
        .args(["build", "k.kara", "--crate-type", "staticlib"])
        .output()
        .unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let lib = dir.join("libk.a");
    if berr.contains("requires the llvm feature") || berr.contains("link failed") || !lib.exists() {
        eprintln!("skip: test_build_auto_boxed_vec_string_return_e2e — soft-skip");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    let header_text = std::fs::read_to_string(dir.join("libk.h")).unwrap();
    assert!(
        header_text.contains("} KaraVec_KaraString;"),
        "nested typedef missing:\n{header_text}"
    );
    let host_c = "#include <stdio.h>\n\
                  #include \"libk.h\"\n\
                  int main(void) {\n\
                      karac_runtime_init();\n\
                      KaraVec_KaraString* v = names();\n\
                      printf(\"%lld:\", (long long)v->len);\n\
                      for (int64_t i = 0; i < v->len; i++)\n\
                          printf(\"%.*s,\", (int)v->data[i].len, (char*)v->data[i].data);\n\
                      printf(\"\\n\");\n\
                      karac_free_names(v);\n\
                      karac_runtime_shutdown();\n\
                      return 0;\n\
                  }\n";
    {
        let mut f = std::fs::File::create(dir.join("host.c")).unwrap();
        f.write_all(host_c.as_bytes()).unwrap();
    }
    let cc = std::process::Command::new("cc")
        .current_dir(&dir)
        .args([
            "host.c",
            "-L.",
            "-l:libk.a",
            "-lpthread",
            "-lm",
            "-ldl",
            "-o",
            "host",
        ])
        .output();
    let cc = match cc {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skip: test_build_auto_boxed_vec_string_return_e2e — no `cc`");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
    };
    assert!(
        cc.status.success(),
        "C host failed to link:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = common::output_with_hang_watchdog(
        {
            let mut c = std::process::Command::new(dir.join("host"));
            c.current_dir(&dir);
            c
        },
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&dir);
    if let Some(run) = run {
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "3:alice,bob,carol,"
        );
        assert_eq!(run.status.code(), Some(0));
    }
}

/// Project-mode library artifact (additive-interop Slice 2, project `[lib]`
/// table). A multi-module project with `[lib] crate-type = "staticlib"`
/// builds `dist/lib<name>.a` + `dist/lib<name>.h` from `karac build` with
/// no flags; a C host links the archive and calls an export. Proves the
/// manifest `[lib]` table drives a library build across multiple modules.
/// Soft-skips like the single-file producer E2E tests.
#[cfg(feature = "llvm")]
#[test]
fn test_project_lib_table_builds_library_e2e() {
    let tmp = scratch_project("proj-lib");
    write(
        &tmp.join("kara.toml"),
        "[package]\nname = \"mathkit\"\n\n[lib]\ncrate-type = \"staticlib\"\n",
    );
    write(
        &tmp.join("src/main.kara"),
        "import helper.square;\n\
         pub extern \"C\" fn sum_sq(a: i64, b: i64) -> i64 { square(a) + square(b) }\n",
    );
    write(
        &tmp.join("src/helper.kara"),
        "pub fn square(x: i64) -> i64 { x * x }\n",
    );

    let build = karac_bin().current_dir(&tmp).arg("build").output().unwrap();
    let berr = String::from_utf8_lossy(&build.stderr);
    let lib = tmp.join("dist/libmathkit.a");
    let header = tmp.join("dist/libmathkit.h");
    if berr.contains("requires the llvm feature")
        || berr.contains("link failed")
        || !lib.exists()
        || !header.exists()
    {
        eprintln!("skip: test_project_lib_table_builds_library_e2e — build/link soft-skip");
        let _ = std::fs::remove_dir_all(&tmp);
        return;
    }
    let header_text = std::fs::read_to_string(&header).unwrap();
    assert!(
        header_text.contains("int64_t sum_sq(int64_t a, int64_t b);"),
        "export proto missing:\n{header_text}"
    );

    let host_c = "#include <stdio.h>\n\
                  #include \"dist/libmathkit.h\"\n\
                  int main(void){ karac_runtime_init();\n\
                    printf(\"%lld\\n\", (long long)sum_sq(3,4));\n\
                    karac_runtime_shutdown(); return 0; }\n";
    write(&tmp.join("host.c"), host_c);
    let cc = std::process::Command::new("cc")
        .current_dir(&tmp)
        .args([
            "host.c",
            "-Ldist",
            "-l:libmathkit.a",
            "-lpthread",
            "-lm",
            "-ldl",
            "-o",
            "host",
        ])
        .output();
    let cc = match cc {
        Ok(o) => o,
        Err(_) => {
            eprintln!("skip: test_project_lib_table_builds_library_e2e — no `cc`");
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
    };
    assert!(
        cc.status.success(),
        "C host failed to link project library:\n{}",
        String::from_utf8_lossy(&cc.stderr)
    );
    let run = common::output_with_hang_watchdog(
        {
            let mut c = std::process::Command::new(tmp.join("host"));
            c.current_dir(&tmp);
            c
        },
        std::time::Duration::from_secs(15),
    );
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(run) = run {
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "25"); // 9 + 16
        assert_eq!(run.status.code(), Some(0));
    }
}
