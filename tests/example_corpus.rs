//! Every `.kara` file under `examples/` still compiles — or is pinned as
//! known-broken with a reason.
//!
//! # Why this exists
//!
//! `examples/` is the corpus a reader learns the language from, and until now
//! most of it was verified by nothing. `tests/tangle_corpus.rs` covers five
//! files, `tests/example_packages.rs` covers two packages, and a handful of
//! examples appear in `tests/codegen.rs` — everything else could stop
//! compiling and no test would notice. The 2026-07-29 audit that produced
//! B-2026-07-29-3 found exactly that: `elevator_project` and `game_of_life`
//! had rotted so far they no longer PARSED, and nothing was red.
//!
//! This gate is deliberately shallow — it asks only "does the compiler still
//! accept this?", not "does it produce the right answer". Depth is the job of
//! the oracle-bearing suites above. Breadth is the job here, because breadth
//! is what was missing: rot is silent, and a file nothing ever compiles is a
//! file that will eventually stop compiling.
//!
//! # Three shapes, three invocations
//!
//! Examples come in three shapes and each needs a different command. Getting
//! this wrong is not cosmetic — see the `KNOWN_BROKEN` note on the ownership
//! false positive below.
//!
//! 1. **Entry package** — `kara.toml` plus `src/main.kara` or `src/lib.kara`.
//!    Verified with a package-level `karac build` from the package root, which
//!    type-checks the whole module graph. (Without `--features llvm` it stops
//!    after type-checking and emits no binary, which is exactly what we want:
//!    the gate stays cheap and needs no runtime archives.)
//!
//! 2. **File-targeted directory** — either no `kara.toml` at all, or a
//!    `kara.toml` whose `src/` holds several INDEPENDENT programs and so has
//!    no entry file. `examples/tangle` is the established precedent (see
//!    `tests/tangle_corpus.rs`, which documents "the package has no
//!    `src/main.kara`, so they are [run individually]"); `examples/cartographer`
//!    is the same shape. Each `.kara` file is checked on its own.
//!
//! 3. **Excluded** — `examples/mend`, whose files are deliberately-broken Mend
//!    fixtures. Compiling clean would defeat their purpose.
//!
//! # Why members of an entry package are never checked individually
//!
//! `karac check src/main.kara` on a file that belongs to an entry package
//! reports a FALSE ownership error (B-2026-07-29-16): a `mut` binding
//! initialized from a free function and then passed to an IMPORTED `ref`
//! parameter is reported as moved, though the parameter only borrows. The
//! package-level path accepts the same program and it runs correctly. So this
//! gate checks entry packages only as packages — checking their members
//! file-by-file would report failures that are artifacts of the invocation
//! rather than defects in the example.
//!
//! # Guarding against a vacuous green
//!
//! `the_gate_is_not_vacuous` guards the walk itself: it asserts the corpus
//! scan still finds files, that every excluded directory still exists and
//! still holds `.kara` files, and that every pin still names a real example.
//! A gate over a directory walk fails OPEN — an empty walk reports green —
//! so that guard is what makes the green meaningful.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

fn examples_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
}

/// Directories whose contents are intentionally not compilable.
const EXCLUDED_DIRS: &[&str] = &[
    // Mend fixtures are broken ON PURPOSE — they are the input to the
    // machine-fix harness (`examples/mend/harness/mend_batch.py`). A Mend
    // fixture that compiles is a broken fixture.
    "mend",
];

/// Entry packages (`kara.toml` + an entry file) that do not currently
/// type-check, each with the first error the package-level build reports.
/// Pinned rather than dropped: a fix turns this test RED and forces promotion.
const KNOWN_BROKEN_PACKAGES: &[(&str, &str)] = &[(
    "db_pipeline",
    // History, because this pin has been retargeted repeatedly as each layer
    // was peeled off. B-2026-07-29-19 fixed the call-site `ref` parse error
    // that had been masking every later phase, exposing 18 latent errors. The
    // example rot among those is now gone: the abandoned
    // `Display::fmt`/`Formatter`/`write` in query.kara, a `Value` enum nested
    // one level too deep, two missing imports, and the `Eq`/`Clone` derives
    // `Value` needed. Three compiler bugs found underneath it have since been
    // fixed too — B-2026-07-29-25 (imported type alias not expanded, which
    // alone accounted for 8 errors), -26, and -27.
    //
    // Every remaining error is a compiler-side gap, none of them filed as a
    // bug yet because each is arguably a design question rather than a defect.
    "1 error, and not example rot: E_NOT_CROSS_TASK at main.kara:43 — the \
         `InMemoryDb` provider cannot cross the `par` boundary the seed loop \
         puts it across. Three earlier groups are gone: a map literal typing \
         as `HashMap`, so unwritable in a typed position (B-2026-07-30-14); \
         `Map` lookup rejecting a borrowed key (B-2026-07-30-17); and \
         `Ok(Vec.new())` not receiving the expected payload type \
         (B-2026-07-31-2, which this pin had misdiagnosed as a `?`-chain \
         inference gap — neither the `?` nor the match was required).",
)];

/// Single files that do not currently check, with the first reported error.
const KNOWN_BROKEN_FILES: &[(&str, &str)] = &[
    (
        "word_count.kara",
        "resolve: undefined name 'read_file' — the spec spells this \
         `fs.read_to_string` (design.md § I/O surface); the example predates \
         the rename.",
    ),
    (
        "fathom/mandelbrot.kara",
        "effect: target `native` does not provide resource 'Timer'.",
    ),
    (
        "plume/plume.kara",
        "effect: target `native` does not provide resource 'Timer' (same \
         cause as fathom/mandelbrot.kara).",
    ),
    (
        "leetcode/course_schedule.kara",
        "typecheck: no method 'get_mut' on type 'Map'.",
    ),
    (
        "leetcode/lru_cache.kara",
        "typecheck: implicit i64 -> u64 coercion would narrow or change sign.",
    ),
    (
        "leetcode/merge_sorted_lists.kara",
        "typecheck: cannot infer type parameter 'T' without an annotation.",
    ),
    (
        "tests/user_db_test.kara",
        "parse: 'mut' is a reserved keyword and cannot be used as an \
         identifier — predates the keyword reservation.",
    ),
];

fn is_excluded(rel: &Path) -> bool {
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|first| EXCLUDED_DIRS.contains(&first))
}

/// An `examples/` subdirectory that is an entry package: it has a `kara.toml`
/// AND an entry file. A `kara.toml` without an entry file is the
/// several-independent-programs shape (tangle, cartographer), checked per file.
fn is_entry_package(dir: &Path) -> bool {
    dir.join("kara.toml").is_file()
        && (dir.join("src/main.kara").is_file() || dir.join("src/lib.kara").is_file())
}

fn entry_packages() -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(examples_root()) else {
        return out;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() && is_entry_package(&path) {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.push(name.to_string());
            }
        }
    }
    out.sort();
    out
}

/// Every `.kara` file under `examples/`, relative to `examples/`.
fn all_kara_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|x| x == "kara") {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    let root = examples_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

/// Files checked individually: every `.kara` that is neither excluded nor a
/// member of an entry package.
fn file_targeted() -> Vec<PathBuf> {
    let pkgs = entry_packages();
    all_kara_files()
        .into_iter()
        .filter(|rel| !is_excluded(rel))
        .filter(|rel| {
            let first = rel
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or_default();
            !pkgs.iter().any(|p| p == first)
        })
        .collect()
}

/// `karac check <file>`, run from the file's own directory root.
fn check_file(rel: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(rel)
        .current_dir(examples_root())
        .output()
        .expect("spawn karac check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Package-level `karac build`. Without `--features llvm` this type-checks the
/// whole module graph and stops before codegen, which is all this gate needs.
/// Used only by `entry_packages_type_check`, which is `llvm`-gated for exactly
/// that reason — so under `llvm` this helper is genuinely dead.
#[cfg_attr(feature = "llvm", allow(dead_code))]
fn build_package(name: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("build")
        .current_dir(examples_root().join(name))
        .output()
        .expect("spawn karac build");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), combined)
}

/// Deliberately NOT run under `llvm`. `karac build` stops after type-checking
/// only when the compiler has no codegen backend; with `--features llvm` the
/// same command runs codegen and links, so it answers a different question and
/// this gate would start reporting codegen gaps as example rot. That is not
/// hypothetical: `elevator_project` type-checks clean but fails codegen on the
/// returned-borrow limitation cited by B-2026-06-07-5, and it would show up
/// here as "no longer type-checks", which is false and points at the wrong
/// thing.
///
/// Codegen coverage of these packages is `tests/example_packages.rs`'s job —
/// it runs each package's own suite on both backends and pins the codegen leg
/// per package. This gate stays what it says it is: a type-check gate.
#[cfg(not(feature = "llvm"))]
#[test]
fn entry_packages_type_check() {
    let broken: BTreeSet<&str> = KNOWN_BROKEN_PACKAGES.iter().map(|(n, _)| *n).collect();
    for name in entry_packages() {
        let (ok, output) = build_package(&name);
        if broken.contains(name.as_str()) {
            let reason = KNOWN_BROKEN_PACKAGES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, r)| *r)
                .unwrap_or_default();
            assert!(
                !ok,
                "examples/{name} is pinned KNOWN_BROKEN_PACKAGES but now BUILDS. \
                 Remove its entry and close the tracked bug.\nPinned reason: {reason}",
            );
        } else {
            assert!(
                ok,
                "examples/{name} no longer type-checks. Fix the example, or add it \
                 to KNOWN_BROKEN_PACKAGES with the reason.\n{output}",
            );
        }
    }
}

#[test]
fn file_targeted_examples_check() {
    let broken: BTreeSet<&str> = KNOWN_BROKEN_FILES.iter().map(|(f, _)| *f).collect();
    for rel in file_targeted() {
        let key = rel.to_string_lossy().replace('\\', "/");
        let (ok, output) = check_file(&rel);
        if broken.contains(key.as_str()) {
            let reason = KNOWN_BROKEN_FILES
                .iter()
                .find(|(f, _)| *f == key)
                .map(|(_, r)| *r)
                .unwrap_or_default();
            assert!(
                !ok,
                "examples/{key} is pinned KNOWN_BROKEN_FILES but now CHECKS clean. \
                 Remove its entry and close the tracked bug.\nPinned reason: {reason}",
            );
        } else {
            assert!(
                ok,
                "examples/{key} no longer checks. Fix the example, or add it to \
                 KNOWN_BROKEN_FILES with the reason.\n{output}",
            );
        }
    }
}

/// Guards this gate against passing VACUOUSLY — the exact failure mode that
/// let the corpus rot in the first place, and the one a coverage gate is most
/// prone to: if the directory walk silently returned nothing, both tests above
/// would iterate an empty list and report green while checking nothing.
///
/// Note there is deliberately no "every file is classified" assertion here:
/// `file_targeted()` is *defined* as everything minus the excluded dirs minus
/// entry-package members, so such an assert could never fail. It would read as
/// a coverage guarantee while proving nothing. These assertions are the ones
/// that can actually go red.
#[test]
fn the_gate_is_not_vacuous() {
    let pkgs = entry_packages();
    let targeted: BTreeSet<PathBuf> = file_targeted().into_iter().collect();
    let all = all_kara_files();

    // Floors, not exact counts: the corpus should grow without editing this.
    // They only catch a walk that collapses toward zero.
    assert!(
        all.len() >= 40,
        "found only {} .kara files under examples/ — the walk is broken, or the \
         corpus shrank drastically",
        all.len(),
    );
    assert!(
        pkgs.len() >= 8,
        "found only {} entry packages, expected at least 8",
        pkgs.len(),
    );
    assert!(
        targeted.len() >= 20,
        "found only {} file-targeted examples, expected at least 20",
        targeted.len(),
    );

    // An exclusion that names a directory which no longer exists (or holds no
    // `.kara` files) is a silent hole: it would keep excusing nothing while
    // looking deliberate.
    for dir in EXCLUDED_DIRS {
        let path = examples_root().join(dir);
        assert!(
            path.is_dir(),
            "EXCLUDED_DIRS names '{dir}', which is not a directory under examples/",
        );
        assert!(
            all.iter().any(|rel| is_excluded(rel)
                && rel
                    .components()
                    .next()
                    .is_some_and(|c| c.as_os_str() == *dir)),
            "EXCLUDED_DIRS names '{dir}', which contains no .kara files — drop it",
        );
    }

    // A pin that no longer names a real example stops meaning anything.
    for (name, _) in KNOWN_BROKEN_PACKAGES {
        assert!(
            pkgs.iter().any(|p| p == name),
            "KNOWN_BROKEN_PACKAGES names '{name}', which is not an entry package",
        );
    }
    for (file, _) in KNOWN_BROKEN_FILES {
        assert!(
            targeted.iter().any(|r| r.to_string_lossy() == *file),
            "KNOWN_BROKEN_FILES names '{file}', which is not a file-targeted example",
        );
    }
}
