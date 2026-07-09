//! PubGrub-backed dependency version solving (phase-5 resolver follow-up
//! (a)). The dependency resolver's version-selection engine.
//!
//! **Why PubGrub.** The v1.1 resolver ([`crate::dep_resolver`]) is a
//! topological walk that pins each package to a single pre-fetched candidate
//! and validates parent constraints against it — correct while every package
//! has exactly one candidate, but it cannot *choose* a version or backtrack
//! when constraints conflict across a diamond. Now that registry + git fetch
//! ship (line 819), the candidate set widens beyond one-per-package, so the
//! resolver needs a real version solver. PubGrub gives that plus derivation-
//! tree conflict explanations.
//!
//! **This slice (slice 1) is the atomic primitive**, mirroring how the
//! registry- and git-fetch epics started (a self-contained, fully-tested
//! building block before any production wiring):
//!
//! - [`version_req_to_range`] — the load-bearing bridge from `semver`'s
//!   [`semver::VersionReq`] (Cargo caret / tilde / wildcard / comparator
//!   semantics, including the `^0.x` / `^0.0.x` zero-cases) to PubGrub's
//!   [`Range`]`<`[`semver::Version`]`>`. Cross-checked against `semver`'s own
//!   `VersionReq::matches` as an oracle over a version grid.
//! - [`solve`] — runs PubGrub over an in-memory candidate registry and
//!   returns the selected `{package → version}` map (or a rendered conflict).
//!   Proves the engine resolves + **backtracks** over the converted ranges.
//!
//! **Not in this slice (deferred to slice 2):** wiring [`solve`] into
//! [`crate::dep_resolver::resolve`] behind a `DependencyProvider` backed by
//! the live `DepGraph` / fetch cache, and widening the graph to enumerate the
//! full published candidate set per package. The `dep_resolver` public
//! input/output types stay stable across that swap.
//!
//! **Prerelease boundary (v1):** the conversion targets *release* version
//! semantics — the domain registry v1 packages live in (the reference server
//! serves release versions; [`crate::registry_proxy`] selection is release-
//! oriented). Cargo's prerelease-exclusion rule (a bare `>=1.2.3` does not
//! match `1.5.0-alpha`) is not representable as a single `Range` interval and
//! is a documented follow-on; the oracle test therefore ranges over release
//! versions.

use pubgrub::{
    resolve, DefaultStringReporter, Dependencies, DependencyProvider, OfflineDependencyProvider,
    PackageResolutionStatistics, PubGrubError, Reporter,
};
use semver::{Comparator, Op, Version, VersionReq};
use std::collections::BTreeMap;
use std::convert::Infallible;

/// PubGrub's version-set type instantiated for `semver::Version`. Re-exported
/// under the shorter `Range` alias by the `pubgrub` crate.
type Range = pubgrub::Range<Version>;

/// Convert a `semver::VersionReq` into the equivalent PubGrub [`Range`] over
/// `semver::Version`.
///
/// A `VersionReq` is a conjunction of comparators; the resulting range is the
/// intersection of each comparator's range. An empty comparator list is `*`
/// (the whole line). See the module docs for the prerelease boundary.
pub fn version_req_to_range(req: &VersionReq) -> Range {
    if req.comparators.is_empty() {
        return Range::full();
    }
    req.comparators.iter().fold(Range::full(), |acc, c| {
        acc.intersection(&comparator_to_range(c))
    })
}

/// A `semver::Version` with the given `major.minor.patch` and empty
/// prerelease / build metadata — the exclusive/inclusive interval endpoints.
fn v(major: u64, minor: u64, patch: u64) -> Version {
    Version::new(major, minor, patch)
}

/// The exact version a fully-specified comparator names, carrying its
/// prerelease tag (so `~1.2.3-alpha`'s lower bound is `1.2.3-alpha`, not
/// `1.2.3`).
fn exact(c: &Comparator) -> Version {
    let mut ver = Version::new(c.major, c.minor.unwrap_or(0), c.patch.unwrap_or(0));
    ver.pre = c.pre.clone();
    ver
}

/// Convert one comparator to a range, following the `semver`/Cargo semantics
/// for partial versions (missing minor/patch widen the interval) and caret's
/// zero-cases.
fn comparator_to_range(c: &Comparator) -> Range {
    let maj = c.major;
    match c.op {
        // `=1.2.3` exact; `=1.2` → [1.2.0, 1.3.0); `=1` → [1.0.0, 2.0.0)
        Op::Exact => match (c.minor, c.patch) {
            (Some(_), Some(_)) => Range::singleton(exact(c)),
            (Some(mi), None) => Range::between(v(maj, mi, 0), v(maj, mi + 1, 0)),
            (None, _) => Range::between(v(maj, 0, 0), v(maj + 1, 0, 0)),
        },
        // `>1.2.3`; `>1.2` → >=1.3.0; `>1` → >=2.0.0
        Op::Greater => match (c.minor, c.patch) {
            (Some(_), Some(_)) => Range::strictly_higher_than(exact(c)),
            (Some(mi), None) => Range::higher_than(v(maj, mi + 1, 0)),
            (None, _) => Range::higher_than(v(maj + 1, 0, 0)),
        },
        // `>=1.2.3`; `>=1.2` → >=1.2.0; `>=1` → >=1.0.0
        Op::GreaterEq => match (c.minor, c.patch) {
            (Some(_), Some(_)) => Range::higher_than(exact(c)),
            (Some(mi), None) => Range::higher_than(v(maj, mi, 0)),
            (None, _) => Range::higher_than(v(maj, 0, 0)),
        },
        // `<1.2.3`; `<1.2` → <1.2.0; `<1` → <1.0.0
        Op::Less => match (c.minor, c.patch) {
            (Some(_), Some(_)) => Range::strictly_lower_than(exact(c)),
            (Some(mi), None) => Range::strictly_lower_than(v(maj, mi, 0)),
            (None, _) => Range::strictly_lower_than(v(maj, 0, 0)),
        },
        // `<=1.2.3` inclusive; `<=1.2` → <1.3.0; `<=1` → <2.0.0
        Op::LessEq => match (c.minor, c.patch) {
            (Some(_), Some(_)) => Range::lower_than(exact(c)),
            (Some(mi), None) => Range::strictly_lower_than(v(maj, mi + 1, 0)),
            (None, _) => Range::strictly_lower_than(v(maj + 1, 0, 0)),
        },
        // `~1.2.3` → [1.2.3, 1.3.0); `~1.2` → [1.2.0, 1.3.0); `~1` → [1.0.0, 2.0.0)
        Op::Tilde => match (c.minor, c.patch) {
            (Some(mi), Some(_)) => Range::between(exact(c), v(maj, mi + 1, 0)),
            (Some(mi), None) => Range::between(v(maj, mi, 0), v(maj, mi + 1, 0)),
            (None, _) => Range::between(v(maj, 0, 0), v(maj + 1, 0, 0)),
        },
        Op::Caret => caret_to_range(c),
        // `1.2.*` → [1.2.0, 1.3.0); `1.*` → [1.0.0, 2.0.0)
        Op::Wildcard => match c.minor {
            Some(mi) => Range::between(v(maj, mi, 0), v(maj, mi + 1, 0)),
            None => Range::between(v(maj, 0, 0), v(maj + 1, 0, 0)),
        },
        // `semver::Op` is `#[non_exhaustive]`; an unrecognized future op is
        // treated permissively as `*` rather than silently excluding — a
        // conservative default that never rejects a legitimately-declared dep.
        _ => Range::full(),
    }
}

/// Caret semantics — the default for a bare `1.2.3` requirement. The upper
/// bound is the next version that changes the left-most non-zero component, so
/// zero-prefixed versions get progressively tighter bounds
/// (`^0.2.3` → <0.3.0, `^0.0.3` → <0.0.4).
fn caret_to_range(c: &Comparator) -> Range {
    let maj = c.major;
    match (c.minor, c.patch) {
        (Some(mi), Some(pa)) => {
            let hi = if maj > 0 {
                v(maj + 1, 0, 0)
            } else if mi > 0 {
                v(0, mi + 1, 0)
            } else {
                v(0, 0, pa + 1)
            };
            Range::between(exact(c), hi)
        }
        (Some(mi), None) => {
            // `^1.2` → <2.0.0; `^0.2` → <0.3.0; `^0.0` → <0.1.0
            let hi = if maj > 0 {
                v(maj + 1, 0, 0)
            } else {
                v(0, mi + 1, 0)
            };
            Range::between(v(maj, mi, 0), hi)
        }
        // `^1` → [1.0.0, 2.0.0); `^0` → [0.0.0, 1.0.0)
        (None, _) => Range::between(v(maj, 0, 0), v(maj + 1, 0, 0)),
    }
}

/// One available version of a package plus the dependency requirements that
/// version imposes (`(dependency-name, constraint)` pairs).
#[derive(Debug, Clone)]
pub struct CandidateVersion {
    pub version: Version,
    pub deps: Vec<(String, VersionReq)>,
}

/// Every published candidate version of a single package.
#[derive(Debug, Clone, Default)]
pub struct PackageCandidates {
    pub versions: Vec<CandidateVersion>,
}

/// Outcome of a failed [`solve`].
#[derive(Debug)]
pub enum SolveError {
    /// No assignment satisfies every constraint. Carries PubGrub's rendered
    /// derivation-tree explanation (the human-readable "because A needs X and
    /// B needs Y…" chain).
    NoSolution(String),
    /// An internal solver error unrelated to the constraint set (should not
    /// occur with the in-memory provider, whose fetch is infallible).
    Internal(String),
}

/// Solve a dependency graph rooted at `root` (at `root_version`, requiring
/// `root_deps`) against an in-memory `registry` of candidate versions, using
/// PubGrub. Returns the selected `{package → version}` map on success.
///
/// This is the slice-1 in-memory demonstration of the engine (see module
/// docs); slice 2 replaces the eager registry with a lazy `DependencyProvider`
/// backed by the live `DepGraph` / fetch cache, keeping this success/error
/// mapping.
pub fn solve(
    root: &str,
    root_version: &Version,
    root_deps: &[(String, VersionReq)],
    registry: &BTreeMap<String, PackageCandidates>,
) -> Result<BTreeMap<String, Version>, SolveError> {
    solve_with_pins(root, root_version, root_deps, registry, &BTreeMap::new())
}

/// [`solve`] with **lockfile version pins** (resolver follow-up (d) / (h),
/// lockfile-pin-over-catalog). `pins` maps a package name to the exact version
/// recorded in `kara.lock`. The solver *prefers* the pinned version whenever it
/// still satisfies the constraints in play, and only moves off it when the pin
/// is incompatible (a sibling capped it out, or the manifest tightened past it)
/// — matching Cargo's "use the lock unless it's incompatible" rule, so
/// backtracking still finds a compatible version rather than failing. Passing
/// an empty map is byte-identical to [`solve`] (the wrapper's `choose_version`
/// then always defers to the inner provider's highest-in-range default).
pub fn solve_with_pins(
    root: &str,
    root_version: &Version,
    root_deps: &[(String, VersionReq)],
    registry: &BTreeMap<String, PackageCandidates>,
    pins: &BTreeMap<String, Version>,
) -> Result<BTreeMap<String, Version>, SolveError> {
    let mut dp = OfflineDependencyProvider::<String, Range>::new();

    let to_range_deps = |deps: &[(String, VersionReq)]| -> Vec<(String, Range)> {
        deps.iter()
            .map(|(name, req)| (name.clone(), version_req_to_range(req)))
            .collect()
    };

    dp.add_dependencies(
        root.to_string(),
        root_version.clone(),
        to_range_deps(root_deps),
    );
    for (name, cands) in registry {
        for cand in &cands.versions {
            dp.add_dependencies(
                name.clone(),
                cand.version.clone(),
                to_range_deps(&cand.deps),
            );
        }
    }

    let provider = PinPreferringProvider {
        inner: dp,
        pins: pins.clone(),
    };
    match resolve(&provider, root.to_string(), root_version.clone()) {
        Ok(selected) => Ok(selected.into_iter().collect()),
        Err(PubGrubError::NoSolution(mut tree)) => {
            tree.collapse_no_versions();
            Err(SolveError::NoSolution(DefaultStringReporter::report(&tree)))
        }
        Err(other) => Err(SolveError::Internal(format!("{other:?}"))),
    }
}

/// A [`DependencyProvider`] that wraps an [`OfflineDependencyProvider`] and
/// **prefers a pinned version** for each package, when one is supplied and the
/// pin still lies within the package's currently-allowed range — the
/// lockfile-pin-over-catalog rule. The pin is a *preference*, not a hard
/// constraint: when the locked version is excluded by the constraints in play,
/// `choose_version` defers to the inner provider (highest-in-range) so PubGrub
/// backtracks to a compatible version rather than failing. Only
/// `choose_version` is overridden; priority, dependency lookup, and
/// cancellation delegate to the inner provider unchanged, so an empty pin set
/// reproduces `OfflineDependencyProvider`'s behavior exactly.
struct PinPreferringProvider {
    inner: OfflineDependencyProvider<String, Range>,
    pins: BTreeMap<String, Version>,
}

impl DependencyProvider for PinPreferringProvider {
    type P = String;
    type V = Version;
    type VS = Range;
    type M = String;
    type Priority = <OfflineDependencyProvider<String, Range> as DependencyProvider>::Priority;
    type Err = Infallible;

    fn prioritize(
        &self,
        package: &String,
        range: &Range,
        stats: &PackageResolutionStatistics,
    ) -> Self::Priority {
        self.inner.prioritize(package, range, stats)
    }

    fn choose_version(
        &self,
        package: &String,
        range: &Range,
    ) -> Result<Option<Version>, Infallible> {
        // Prefer the locked version while it is still in the allowed range;
        // otherwise defer to the inner provider so the solver can move off an
        // incompatible pin (PubGrub narrows `range` to exclude a version that
        // led to a conflict, so the pin stops being offered on backtrack).
        if let Some(pinned) = self.pins.get(package) {
            if range.contains(pinned) {
                return Ok(Some(pinned.clone()));
            }
        }
        self.inner.choose_version(package, range)
    }

    fn get_dependencies(
        &self,
        package: &String,
        version: &Version,
    ) -> Result<Dependencies<String, Range, String>, Infallible> {
        self.inner.get_dependencies(package, version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(s: &str) -> VersionReq {
        VersionReq::parse(s).unwrap()
    }

    fn ver(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    // --- version_req_to_range: oracle cross-check ------------------------

    /// The converted range must agree with `semver`'s own `VersionReq::matches`
    /// on every release version in a grid — the strongest correctness check,
    /// since it needs no hand-specified expected ranges. Release versions only
    /// (see the module's prerelease boundary).
    #[test]
    fn range_matches_semver_oracle_over_release_grid() {
        let reqs = [
            "*",
            "=1.2.3",
            "=1.2",
            "=1",
            ">1.2.3",
            ">=1.2.3",
            "<1.2.3",
            "<=1.2.3",
            ">1.2",
            ">=1.2",
            "<1.2",
            "<=1.2",
            ">1",
            ">=1",
            "<1",
            "<=1",
            "~1.2.3",
            "~1.2",
            "~1",
            "^1.2.3",
            "^1.2",
            "^1",
            "^0.2.3",
            "^0.2",
            "^0.0.3",
            "^0.0",
            "^0",
            "1.2.*",
            "1.*",
            ">=1.2.0, <1.5.0",
            "^1.2.3, <1.4.0",
        ];
        // A grid spanning the boundaries the reqs care about.
        let mut versions = Vec::new();
        for maj in 0..=2 {
            for min in 0..=4 {
                for pat in 0..=4 {
                    versions.push(v(maj, min, pat));
                }
            }
        }

        for r in reqs {
            let parsed = req(r);
            let range = version_req_to_range(&parsed);
            for ver in &versions {
                assert_eq!(
                    range.contains(ver),
                    parsed.matches(ver),
                    "mismatch for req `{r}` at version `{ver}`: range says {}, semver says {}",
                    range.contains(ver),
                    parsed.matches(ver),
                );
            }
        }
    }

    // --- version_req_to_range: targeted zero-case pins -------------------

    #[test]
    fn caret_zero_minor_tightens_to_minor() {
        // ^0.2.3 → [0.2.3, 0.3.0)
        let r = version_req_to_range(&req("^0.2.3"));
        assert!(r.contains(&ver("0.2.3")));
        assert!(r.contains(&ver("0.2.9")));
        assert!(!r.contains(&ver("0.3.0")));
        assert!(!r.contains(&ver("0.2.2")));
    }

    #[test]
    fn caret_zero_zero_tightens_to_patch() {
        // ^0.0.3 → [0.0.3, 0.0.4)
        let r = version_req_to_range(&req("^0.0.3"));
        assert!(r.contains(&ver("0.0.3")));
        assert!(!r.contains(&ver("0.0.4")));
        assert!(!r.contains(&ver("0.0.2")));
    }

    #[test]
    fn caret_nonzero_widens_to_major() {
        // ^1.2.3 → [1.2.3, 2.0.0)
        let r = version_req_to_range(&req("^1.2.3"));
        assert!(r.contains(&ver("1.2.3")));
        assert!(r.contains(&ver("1.9.9")));
        assert!(!r.contains(&ver("2.0.0")));
        assert!(!r.contains(&ver("1.2.2")));
    }

    #[test]
    fn wildcard_and_star() {
        assert!(version_req_to_range(&req("*")).contains(&ver("99.0.0")));
        let r = version_req_to_range(&req("1.2.*"));
        assert!(r.contains(&ver("1.2.0")));
        assert!(r.contains(&ver("1.2.5")));
        assert!(!r.contains(&ver("1.3.0")));
    }

    // --- solve: resolution + backtracking --------------------------------

    fn cand(version: &str, deps: &[(&str, &str)]) -> CandidateVersion {
        CandidateVersion {
            version: ver(version),
            deps: deps.iter().map(|(n, r)| (n.to_string(), req(r))).collect(),
        }
    }

    fn registry(entries: &[(&str, Vec<CandidateVersion>)]) -> BTreeMap<String, PackageCandidates> {
        entries
            .iter()
            .map(|(name, versions)| {
                (
                    name.to_string(),
                    PackageCandidates {
                        versions: versions.clone(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn solve_picks_highest_matching_version() {
        // root → a ^1.0; a has 1.0.0 and 1.2.0 → highest match is 1.2.0.
        let reg = registry(&[(
            "a",
            vec![cand("1.0.0", &[]), cand("1.2.0", &[]), cand("2.0.0", &[])],
        )]);
        let sol = solve(
            "root",
            &ver("0.0.0"),
            &[("a".to_string(), req("^1.0"))],
            &reg,
        )
        .expect("should resolve");
        assert_eq!(sol.get("a"), Some(&ver("1.2.0")));
    }

    #[test]
    fn solve_backtracks_over_a_diamond() {
        // The heart of why PubGrub earns its keep — a greedy highest-first
        // pick fails, a solver must backtrack:
        //   root → a *, b *
        //   a 2.0.0 → c =2.0.0     (a 1.0.0 → c =1.0.0)
        //   b 1.0.0 → c =1.0.0
        //   c: 1.0.0, 2.0.0
        // Greedily picking a=2.0.0 forces c=2.0.0, which b=1.0.0 rejects.
        // The only solution is a=1.0.0, b=1.0.0, c=1.0.0.
        let reg = registry(&[
            (
                "a",
                vec![
                    cand("1.0.0", &[("c", "=1.0.0")]),
                    cand("2.0.0", &[("c", "=2.0.0")]),
                ],
            ),
            ("b", vec![cand("1.0.0", &[("c", "=1.0.0")])]),
            ("c", vec![cand("1.0.0", &[]), cand("2.0.0", &[])]),
        ]);
        let sol = solve(
            "root",
            &ver("0.0.0"),
            &[("a".to_string(), req("*")), ("b".to_string(), req("*"))],
            &reg,
        )
        .expect("should backtrack to a compatible assignment");
        assert_eq!(
            sol.get("a"),
            Some(&ver("1.0.0")),
            "must backtrack off a=2.0.0"
        );
        assert_eq!(sol.get("b"), Some(&ver("1.0.0")));
        assert_eq!(sol.get("c"), Some(&ver("1.0.0")));
    }

    #[test]
    fn solve_with_pins_prefers_locked_version() {
        // a has 1.0.0 and 1.9.0; root wants a ^1. A fresh solve picks the
        // highest (1.9.0); a lock pinning a=1.0.0 makes the solver honor the
        // recorded version instead — lockfile-pin-over-catalog.
        let reg = registry(&[("a", vec![cand("1.0.0", &[]), cand("1.9.0", &[])])]);
        let root_deps = [("a".to_string(), req("^1"))];

        let fresh = solve("root", &ver("0.0.0"), &root_deps, &reg).expect("resolve");
        assert_eq!(
            fresh.get("a"),
            Some(&ver("1.9.0")),
            "fresh solve picks the catalog highest"
        );

        let pins = BTreeMap::from([("a".to_string(), ver("1.0.0"))]);
        let pinned =
            solve_with_pins("root", &ver("0.0.0"), &root_deps, &reg, &pins).expect("resolve");
        assert_eq!(
            pinned.get("a"),
            Some(&ver("1.0.0")),
            "the lock pin is honored over the catalog highest"
        );
    }

    #[test]
    fn solve_with_pins_falls_back_when_pin_incompatible() {
        // The manifest now requires a ^2, but the lock pins a=1.0.0
        // (incompatible). The pin is a *preference*, not a hard constraint —
        // the solver moves off it to the only compatible version (2.0.0)
        // rather than failing.
        let reg = registry(&[("a", vec![cand("1.0.0", &[]), cand("2.0.0", &[])])]);
        let pins = BTreeMap::from([("a".to_string(), ver("1.0.0"))]);
        let sol = solve_with_pins(
            "root",
            &ver("0.0.0"),
            &[("a".to_string(), req("^2"))],
            &reg,
            &pins,
        )
        .expect("must move off the incompatible pin, not fail");
        assert_eq!(sol.get("a"), Some(&ver("2.0.0")));
    }

    #[test]
    fn solve_with_pins_still_backtracks_when_pin_forces_a_conflict() {
        // The diamond from `solve_backtracks_over_a_diamond`, but pinning
        // a=2.0.0 — the version that forces c=2.0.0, which b rejects. Because
        // the pin is only a preference, the solver tries a=2.0.0, hits the
        // conflict, and backtracks to the compatible a=1.0.0.
        let reg = registry(&[
            (
                "a",
                vec![
                    cand("1.0.0", &[("c", "=1.0.0")]),
                    cand("2.0.0", &[("c", "=2.0.0")]),
                ],
            ),
            ("b", vec![cand("1.0.0", &[("c", "=1.0.0")])]),
            ("c", vec![cand("1.0.0", &[]), cand("2.0.0", &[])]),
        ]);
        let pins = BTreeMap::from([("a".to_string(), ver("2.0.0"))]);
        let sol = solve_with_pins(
            "root",
            &ver("0.0.0"),
            &[("a".to_string(), req("*")), ("b".to_string(), req("*"))],
            &reg,
            &pins,
        )
        .expect("a pin must not break backtracking");
        assert_eq!(
            sol.get("a"),
            Some(&ver("1.0.0")),
            "backtracks off the pinned-but-conflicting a=2.0.0"
        );
        assert_eq!(sol.get("c"), Some(&ver("1.0.0")));
    }

    #[test]
    fn solve_with_pins_empty_matches_solve() {
        // An empty pin set must reproduce `solve` exactly (the wrapper always
        // defers to the inner highest-in-range provider).
        let reg = registry(&[("a", vec![cand("1.0.0", &[]), cand("1.2.0", &[])])]);
        let root_deps = [("a".to_string(), req("^1.0"))];
        let via_solve = solve("root", &ver("0.0.0"), &root_deps, &reg).unwrap();
        let via_pins =
            solve_with_pins("root", &ver("0.0.0"), &root_deps, &reg, &BTreeMap::new()).unwrap();
        assert_eq!(via_solve, via_pins, "empty pins must be identical to solve");
    }

    #[test]
    fn solve_reports_no_solution_for_contradictory_constraints() {
        // root wants both a ^1 and a ^2 — no version of a satisfies both.
        let reg = registry(&[("a", vec![cand("1.0.0", &[]), cand("2.0.0", &[])])]);
        let err = solve(
            "root",
            &ver("0.0.0"),
            &[("a".to_string(), req("^1")), ("a".to_string(), req("^2"))],
            &reg,
        )
        .expect_err("contradictory constraints must not resolve");
        match err {
            SolveError::NoSolution(report) => {
                assert!(!report.is_empty(), "conflict report should be non-empty");
            }
            other => panic!("expected NoSolution, got {other:?}"),
        }
    }

    #[test]
    fn solve_reports_no_solution_for_missing_dependency() {
        // root needs `ghost`, which the registry doesn't carry.
        let reg = registry(&[("a", vec![cand("1.0.0", &[])])]);
        let err = solve(
            "root",
            &ver("0.0.0"),
            &[("ghost".to_string(), req("*"))],
            &reg,
        )
        .expect_err("a missing dependency cannot resolve");
        assert!(matches!(err, SolveError::NoSolution(_)));
    }
}
