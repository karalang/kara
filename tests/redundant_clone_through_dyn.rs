//! `redundant_clone` (B-2026-11-14-3) ran only where free functions and
//! concrete methods check their arguments, so a clone into a bare
//! parameter of a trait method called through `dyn` or a bound on a type
//! parameter stayed after `karac fix`: kata 1274's `sea.has_ships(
//! top_right.clone(), bottom_left.clone())` (B-2026-11-20-87). Both paths
//! borrow a bare parameter as a concrete method does, so both are flagged
//! and fixed; an `own` parameter is not.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The check's diagnostics, the text `karac fix` leaves, and what it
/// prints under the MIR interpreter.
fn mend(name: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-clone-dyn-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    let first = karac(&dir, &["check", "cell.kara"]).2;
    karac(&dir, &["fix", "cell.kara"]);
    let text = std::fs::read_to_string(&file).unwrap();
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (first, text, out)
}

const SRC: &str = "#[derive(Clone)]\nstruct P {\n    x: i64,\n    y: i64,\n}\n\ntrait Sea {\n    fn has_ships(self, a: P, b: P) -> bool;\n}\n\nstruct Grid {\n    n: i64,\n}\n\nimpl Sea for Grid {\n    fn has_ships(self, a: P, b: P) -> bool {\n        a.x + b.y < self.n\n    }\n}\n\nfn through_dyn(sea: dyn Sea, top: P, bottom: P) -> bool {\n    sea.has_ships(top.clone(), bottom.clone())\n}\n\nfn through_bound[S: Sea](sea: S, top: P, bottom: P) -> bool {\n    sea.has_ships(top.clone(), bottom.clone())\n}\n\nfn main() {\n    let g = Grid { n: 10 };\n    println(through_dyn(g, P { x: 1, y: 2 }, P { x: 3, y: 4 }));\n    println(through_bound(Grid { n: 3 }, P { x: 1, y: 2 }, P { x: 3, y: 4 }));\n}\n";

const OWN: &str = "#[derive(Clone)]\nstruct P {\n    x: i64,\n    y: i64,\n}\n\ntrait Sea {\n    fn has_ships(self, a: P, b: P) -> bool;\n    fn keep(self, a: own P) -> i64;\n}\n\nstruct Grid {\n    n: i64,\n}\n\nimpl Sea for Grid {\n    fn has_ships(self, a: P, b: P) -> bool {\n        a.x + b.y < self.n\n    }\n\n    fn keep(self, a: own P) -> i64 {\n        a.x\n    }\n}\n\nfn through_dyn(sea: dyn Sea, top: P, bottom: P) -> bool {\n    sea.has_ships(top.clone(), bottom.clone())\n}\n\nfn through_bound[S: Sea](sea: S, top: P, bottom: P) -> bool {\n    sea.has_ships(top.clone(), bottom.clone())\n}\n\nfn keeps(sea: dyn Sea, top: P) -> i64 {\n    sea.keep(top.clone())\n}\n\nfn main() {\n    let g = Grid { n: 10 };\n    println(through_dyn(g, P { x: 1, y: 2 }, P { x: 3, y: 4 }));\n    println(keeps(Grid { n: 1 }, P { x: 5, y: 6 }));\n    println(through_bound(Grid { n: 3 }, P { x: 1, y: 2 }, P { x: 3, y: 4 }));\n}\n";

#[test]
fn a_clone_into_a_trait_method_through_dyn_or_a_bound_is_redundant() {
    let (first, text, out) = mend("both", SRC);
    assert_eq!(
        first.matches("warning[redundant_clone]").count(),
        4,
        "{first}"
    );
    assert_eq!(
        text.matches("sea.has_ships(top, bottom)").count(),
        2,
        "{text}"
    );
    assert_eq!(out, "true\nfalse\n");
}

#[test]
fn a_clone_into_an_own_trait_method_parameter_stays() {
    let (first, text, out) = mend("own", OWN);
    assert_eq!(
        first.matches("warning[redundant_clone]").count(),
        4,
        "{first}"
    );
    assert!(text.contains("sea.keep(top.clone())"), "{text}");
    assert_eq!(out, "true\n5\nfalse\n");
}
