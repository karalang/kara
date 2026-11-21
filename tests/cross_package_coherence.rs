//! B-2026-11-17-36 — design.md § Coherence and the orphan rule, between the
//! packages of one build: a library implements a trait for a type only if it
//! defines the trait, the type, or a type in the trait's arguments; writes a
//! blanket impl only of its own trait; and every package, the application
//! included, writes inherent impls only for its own types.
//! B-2026-11-17-39 — a `pub fn`'s bounds are part of its signature, so a
//! bound on a package-private trait is E0221 (review ruling).

use std::path::{Path, PathBuf};
use std::process::Command;

/// `app` depends on `mid`, which depends on `lib`; each `src` file is given.
fn packages(tag: &str, lib: &str, mid: &str, main: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-xpkg-coherence-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let manifest = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nauthors = []\nedition = \"2026\"\n\n[dependencies]\n{deps}"
        )
    };
    for (name, deps, file, text) in [
        ("lib", "", "lib.kara", lib),
        ("mid", "lib = { path = \"../lib\" }\n", "lib.kara", mid),
        (
            "app",
            "mid = { path = \"../mid\" }\nlib = { path = \"../lib\" }\n",
            "main.kara",
            main,
        ),
    ] {
        std::fs::create_dir_all(dir.join(name).join("src")).unwrap();
        std::fs::write(dir.join(name).join("kara.toml"), manifest(name, deps)).unwrap();
        std::fs::write(dir.join(name).join("src").join(file), text).unwrap();
    }
    dir
}

fn check(pkg: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(pkg)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const LIB: &str = "pub trait Tw { fn tw(self) -> i64; }\npub struct S { pub n: i64 }\n";
const MAIN: &str = "fn main() { println(1); }\n";

/// Refused from the app's build and from the library's own directory.
fn refused(tag: &str, mid: &str, want: &str) {
    let dir = packages(tag, LIB, mid, MAIN);
    for pkg in ["app", "mid"] {
        let (ok, out) = check(&dir.join(pkg));
        assert!(!ok, "{tag}: checked in {pkg}, expected a refusal:\n{out}");
        assert!(
            out.contains("E0246") && out.contains(want),
            "{tag}: checked in {pkg}, expected E0246 `{want}`:\n{out}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_library_implements_another_packages_trait_for_a_std_type() {
    refused(
        "orphan",
        "import lib.Tw;\nimpl Tw for String { fn tw(self) -> i64 { self.len() } }\n",
        "orphan impl",
    );
}

#[test]
fn a_library_writes_a_blanket_impl_of_a_foreign_trait() {
    refused(
        "blanket-dep",
        "import lib.Tw;\npub trait Mine { fn m(self) -> i64; }\nimpl[T: Mine] Tw for T { fn tw(self) -> i64 { 1 } }\n",
        "blanket impl",
    );
    refused(
        "blanket-std",
        "pub trait Mine { fn m(self) -> i64; }\nimpl[T: Mine] Display for T { fn to_string(self) -> String { \"X\" } }\n",
        "blanket impl",
    );
}

#[test]
fn a_library_writes_an_inherent_impl_on_another_packages_type() {
    refused(
        "inherent-lib",
        "import lib.S;\nimpl S { pub fn dbl(self) -> i64 { self.n * 2 } }\n",
        "inherent impl",
    );
}

#[test]
fn the_application_writes_an_inherent_impl_on_a_dependencys_type() {
    let dir = packages(
        "inherent-app",
        LIB,
        "pub fn z() -> i64 { 0 }\n",
        "import lib.S;\nimpl S { fn dbl(self) -> i64 { self.n * 2 } }\nfn main() { println(S { n: 2 }.dbl()); }\n",
    );
    let (ok, out) = check(&dir.join("app"));
    assert!(
        !ok && out.contains("E0246") && out.contains("inherent impl"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn what_the_orphan_rule_allows_still_checks() {
    // A library's own trait for a std type, a foreign trait for its own
    // type, a local type in the trait's arguments, its own blanket impl;
    // and the application's foreign trait for a foreign type (the exemption).
    let dir = packages(
        "allowed",
        "pub trait Tw { fn tw(self) -> i64; }\npub struct S { pub n: i64 }\npub trait Mul2[R] { fn mul2(self, r: R) -> i64; }\n",
        "import lib.{Tw, S, Mul2};\npub trait Own { fn own(self) -> i64; }\nimpl Own for String { fn own(self) -> i64 { 1 } }\npub struct Col { pub v: i64 }\nimpl Tw for Col { fn tw(self) -> i64 { self.v } }\nimpl Mul2[Col] for i64 { fn mul2(self, r: Col) -> i64 { self * r.v } }\npub trait Loud { fn loud(self) -> i64; }\nimpl[T: Own] Loud for T { fn loud(self) -> i64 { 2 } }\npub fn z() -> i64 { 0 }\n",
        "import lib.{Tw, S};\nimpl Tw for i64 { fn tw(self) -> i64 { self } }\nfn main() { println(5.tw()); }\n",
    );
    for pkg in ["app", "mid"] {
        let (ok, out) = check(&dir.join(pkg));
        assert!(ok, "checked in {pkg}:\n{out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pub_fn_bounded_on_a_private_trait_leaks_it() {
    let dir = packages(
        "bound",
        "trait Hid { fn x(self) -> i64; }\npub fn f[T: Hid](t: T) -> i64 { t.x() }\npub trait Shown { fn y(self) -> i64; }\npub fn g[T](t: T) -> i64 where T: Shown { t.y() }\n",
        "pub fn z() -> i64 { 0 }\n",
        MAIN,
    );
    let (ok, out) = check(&dir.join("lib"));
    assert!(!ok, "{out}");
    assert!(
        out.contains("E0221") && out.contains("trait 'Hid', which is not `pub`"),
        "{out}"
    );
    assert!(
        !out.contains("'Shown'"),
        "a pub trait's bound is no leak:\n{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
