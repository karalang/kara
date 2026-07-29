//! Oracle for the multi-module packages under `examples/` that ship their own
//! Kāra `test "..."` suites (B-2026-07-29-3, -4, -7).
//!
//! # Why this exists
//!
//! `examples/elevator_project` and `examples/game_of_life` are complete
//! multi-module packages, each carrying `*_test.kara` companions full of
//! `assert_eq` cases — a real oracle, written by the example authors, that
//! nothing ever ran. Neither directory was referenced anywhere in
//! `.github/workflows`, `tests/`, `scripts/`, or `src/`, and on 2026-07-29 an
//! audit found both had rotted so far they no longer PARSED: they wrote `ref`
//! at call sites (banned by design.md Feature 4 Part 1½), the abandoned
//! Rust-style `Display::fmt(&self, Formatter)`, a `layout` block bound to a
//! type rather than a binding site, and a `\x1b` escape the lexer never
//! supported. `karac test` on either was additionally blocked by a false
//! `E0223` module cycle (B-2026-07-29-3) and by imported trait bounds
//! resolving no methods (B-2026-07-29-9).
//!
//! Running the example's OWN tests is the oracle here — unlike
//! `tests/tangle_corpus.rs`, whose oracle is prose in a README. That makes
//! this gate self-maintaining: an example that grows a new `test` block grows
//! its coverage here for free.
//!
//! # Both backends, explicitly
//!
//! `karac test` runs its cases through the **LLJIT executor by default** and
//! only uses the tree-walk interpreter under `--interp` (or on a build without
//! the `llvm` feature, where the interpreter is the sole executor). So the
//! backend a bare `karac test` exercises depends on how `karac` was built —
//! which means a single-leg gate would silently cover different things in
//! different CI tiers. Each package is therefore pinned per backend, and the
//! codegen leg only runs when this test binary itself has `llvm` (otherwise it
//! would merely re-run the interpreter and a `KnownBroken` pin would invert).
//!
//! # Known-broken legs are pinned, not skipped
//!
//! `elevator_project` passes all 14 cases under the interpreter and fails all
//! 14 under codegen — `e.pending().is_empty()` in `elevator_test.kara` puts a
//! `Vec` method on a call-result receiver, which `compile_method_call` has no
//! arm for (B-2026-07-29-12). Rather than drop the package, rewrite the example
//! to dodge the gap, or gate the whole file on the interpreter, that leg is
//! `Expect::KnownBroken`: fixing the codegen gap turns this test RED and forces
//! the pin to be promoted, instead of the fix landing with the gate still
//! ignoring it.

use std::path::PathBuf;
use std::process::Command;

/// What we currently expect of a package's own suite, per backend.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Every case must pass.
    AllPass,
    /// Known-broken. Asserted to have at least one FAILING case, so a fix
    /// trips this test and forces promotion to `AllPass`.
    KnownBroken,
}

/// A package under `examples/` whose own `test "..."` suite is the oracle.
struct Package {
    /// Directory name under `examples/`.
    dir: &'static str,
    /// Exact number of `test "..."` cases expected to run. Pinned rather than
    /// asserted `> 0` so that a test block silently disappearing — the failure
    /// mode that let these rot in the first place — turns this red.
    expected_tests: usize,
    interp: Expect,
    /// Read only by the `llvm`-gated codegen leg; without that feature there is
    /// no LLJIT executor to run it against, so the field is genuinely dead.
    #[cfg_attr(not(feature = "llvm"), allow(dead_code))]
    codegen: Expect,
    /// Why a leg is pinned, if one is.
    note: &'static str,
}

const PACKAGES: &[Package] = &[
    Package {
        dir: "elevator_project",
        expected_tests: 14,
        interp: Expect::AllPass,
        codegen: Expect::KnownBroken,
        note: "B-2026-07-29-12: `e.pending().is_empty()` — a Vec method on a \
               call-result receiver — has no arm in `compile_method_call`",
    },
    Package {
        dir: "game_of_life",
        expected_tests: 10,
        interp: Expect::AllPass,
        codegen: Expect::AllPass,
        note: "",
    },
];

fn package_dir(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(dir)
}

/// One `{"type":"summary",...}` line from `karac test`'s JSON output.
struct Summary {
    total: usize,
    passed: usize,
    failed: usize,
}

/// Pull an integer field out of the summary line without taking a JSON
/// dependency — the runner's schema is documented in design.md § Testing and
/// is stable, and the rest of `tests/` parses its output the same way.
fn field(line: &str, key: &str) -> Option<usize> {
    let needle = format!("\"{key}\":");
    let rest = &line[line.find(&needle)? + needle.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn run_package_tests(dir: &str, interp: bool) -> (Summary, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("test");
    if interp {
        cmd.arg("--interp");
    }
    let out = cmd
        .current_dir(package_dir(dir))
        .output()
        .expect("spawn karac test");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let combined = format!("{stdout}{stderr}");
    let backend = if interp { "--interp" } else { "default" };

    let summary = stdout
        .lines()
        .find(|l| l.contains("\"type\":\"summary\""))
        .unwrap_or_else(|| {
            panic!("`karac test {backend}` in examples/{dir} emitted no summary line.\n{combined}")
        });

    (
        Summary {
            total: field(summary, "total").expect("total in summary"),
            passed: field(summary, "passed").expect("passed in summary"),
            failed: field(summary, "failed").expect("failed in summary"),
        },
        combined,
    )
}

fn check_leg(pkg: &Package, interp: bool, expect: Expect) {
    let backend = if interp { "interp" } else { "codegen" };
    let (summary, output) = run_package_tests(pkg.dir, interp);

    // The case COUNT is backend-independent: discovery happens before
    // execution, so a vanished test block is caught on either leg.
    assert_eq!(
        summary.total, pkg.expected_tests,
        "examples/{} [{backend}] ran {} tests, expected {} — if a test was \
         intentionally added or removed, update `expected_tests`\n{output}",
        pkg.dir, summary.total, pkg.expected_tests,
    );

    match expect {
        Expect::AllPass => {
            assert_eq!(
                summary.failed, 0,
                "examples/{} [{backend}] has failing tests\n{output}",
                pkg.dir,
            );
            assert_eq!(
                summary.passed, pkg.expected_tests,
                "examples/{} [{backend}] passed {} of {}\n{output}",
                pkg.dir, summary.passed, pkg.expected_tests,
            );
        }
        Expect::KnownBroken => {
            assert!(
                summary.failed > 0,
                "examples/{} [{backend}] now PASSES but is pinned KnownBroken \
                 ({}). Promote its `{backend}` leg to `Expect::AllPass` and \
                 close the tracked bug.\n{output}",
                pkg.dir,
                pkg.note,
            );
        }
    }
}

#[test]
fn example_packages_pass_their_own_test_suites_under_interp() {
    for pkg in PACKAGES {
        check_leg(pkg, true, pkg.interp);
    }
}

/// Only meaningful with `llvm`: without it `karac test` has no LLJIT executor
/// and the default backend IS the interpreter, so this leg would duplicate the
/// one above and invert every `KnownBroken` pin.
#[cfg(feature = "llvm")]
#[test]
fn example_packages_pass_their_own_test_suites_under_codegen() {
    for pkg in PACKAGES {
        check_leg(pkg, false, pkg.codegen);
    }
}

/// The packages must also stay *runnable*, not merely test-clean: `karac test`
/// only executes test functions, so a broken `main.kara` would slip past the
/// suites above. Both entry points are pure compute with deterministic output.
#[test]
fn example_packages_entry_points_run() {
    // (package, entry file, a substring its output must contain)
    let cases = [
        ("elevator_project", "src/main.kara", "=== SCAN ==="),
        ("game_of_life", "src/main.kara", "population: 5"),
    ];
    for (dir, entry, expected) in cases {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg("run")
            .arg("--interp")
            .arg(entry)
            .current_dir(package_dir(dir))
            .output()
            .expect("spawn karac run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "examples/{dir}/{entry} exited {:?}\n{}{}",
            out.status.code(),
            stdout,
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            stdout.contains(expected),
            "examples/{dir}/{entry} output missing {expected:?}\n{stdout}",
        );
    }
}
