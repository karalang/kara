//! B-2026-11-27-7 (the remainder of B-2026-11-25-48): a collection's `.clone()` and
//! `to_vec` clone each element, so a `Vec[T]`, `Option[T]` or slice of an
//! enclosing type parameter `T` needs `T: Clone`. The check let any type
//! parameter through, so a generic function cloned a `Vec[T]` field by field
//! whatever `T` was: a `Vec[Mutex[i64]]` became a second, independent set of
//! mutexes. Each is refused now, with a machine fix that declares the bound
//! on the parameter (`T` -> `T: Clone`, `T: Ord` -> `T: Ord + Clone`, on the
//! function's generics or the impl's), and the forms that do clone stay
//! accepted.

use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, src: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-generic-clone-bound-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.kara"), src).unwrap();
        Scratch(dir)
    }

    fn karac(&self, cmd: &str) -> (bool, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .current_dir(&self.0)
            .arg(cmd)
            .arg("main.kara")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    }

    fn source(&self) -> String {
        std::fs::read_to_string(self.0.join("main.kara")).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MAIN: &str =
    "fn main() { println(f(vec_of(1)).len()); }\nfn vec_of(x: i64) -> Vec[i64] { [x] }\n";

#[test]
fn generic_collection_clone_without_bound_is_refused() {
    for (name, item, want) in [
        (
            "vec_clone",
            "fn f[T](v: Vec[T]) -> Vec[T] { v.clone() }",
            "`.clone()` on 'Vec[T]' clones each element, and the type parameter `T` has no `Clone` bound: declare it `T: Clone`",
        ),
        (
            "vec_to_vec",
            "fn f[T: Ord](v: Vec[T]) -> Vec[T] { v.to_vec() }",
            "`.to_vec()` on 'Vec[T]' clones each element, and the type parameter `T` has no `Clone` bound",
        ),
        (
            "option_clone",
            "fn f[T](v: own Vec[T]) -> Vec[T] { let o: Option[Vec[T]] = Some(v); o.clone().unwrap() }",
            "`.clone()` on 'Option[Vec[T]]' clones each element, and the type parameter `T` has no `Clone` bound",
        ),
        (
            "impl_param",
            "struct Bag[T] { xs: Vec[T] }\nimpl[T] Bag[T] { fn snap(ref self) -> Vec[T] { self.xs.clone() } }\nfn f(v: Vec[i64]) -> Vec[i64] { Bag { xs: v }.snap() }",
            "`.clone()` on 'Vec[T]' clones each element, and the type parameter `T` has no `Clone` bound",
        ),
    ] {
        let s = Scratch::new(name, &format!("{MAIN}{item}\n"));
        let (ok, out) = s.karac("check");
        assert!(!ok, "{name}: check accepted an unbounded clone:\n{out}");
        assert!(out.contains(want), "{name}: want `{want}`, got:\n{out}");
    }
}

#[test]
fn generic_collection_clone_fix_declares_the_bound() {
    for (name, item, fixed) in [
        (
            "bare",
            "fn f[T](v: Vec[T]) -> Vec[T] { v.clone() }",
            "fn f[T: Clone](v: Vec[T]) -> Vec[T] { v.clone() }",
        ),
        (
            "bounded",
            "fn f[T: Ord](v: Vec[T]) -> Vec[T] { v.to_vec() }",
            "fn f[T: Ord + Clone](v: Vec[T]) -> Vec[T] { v.to_vec() }",
        ),
        (
            "twice_one_edit",
            "fn f[T](v: Vec[T]) -> Vec[T] { let w = v.clone(); w.to_vec() }",
            "fn f[T: Clone](v: Vec[T]) -> Vec[T] { let w = v.clone(); w.to_vec() }",
        ),
        (
            "impl_param",
            "struct Bag[T] { xs: Vec[T] }\nimpl[T] Bag[T] { fn snap(ref self) -> Vec[T] { self.xs.clone() } }\nfn f(v: Vec[i64]) -> Vec[i64] { Bag { xs: v }.snap() }",
            "impl[T: Clone] Bag[T] { fn snap(ref self) -> Vec[T] { self.xs.clone() } }",
        ),
    ] {
        let s = Scratch::new(&format!("fix-{name}"), &format!("{MAIN}{item}\n"));
        let (_, out) = s.karac("fix");
        let src = s.source();
        assert!(src.contains(fixed), "{name}: fix gave:\n{src}\n{out}");
        let (ok, out) = s.karac("check");
        assert!(ok, "{name}: the fixed program still fails check:\n{out}");
        let (ok, out) = s.karac("__mir-run");
        assert!(ok && out.contains('1'), "{name}: the fixed program does not run:\n{out}");
    }
}

#[test]
fn generic_collection_clone_with_bound_stays_accepted() {
    for (name, item) in [
        ("clone_bound", "fn f[T: Clone](v: Vec[T]) -> Vec[T] { v.clone() }"),
        ("copy_bound", "fn f[T: Copy](v: Vec[T]) -> Vec[T] { v.to_vec() }"),
        (
            "where_clause",
            "fn f[T](v: Vec[T]) -> Vec[T] where T: Clone { v.clone() }",
        ),
        (
            "views",
            "fn f(v: Vec[i64]) -> Vec[i64] { let r: Vec[ref i64] = v.iter().collect(); println(g(r)); v.clone() }\nfn g[T](v: Vec[ref T]) -> i64 { v.clone().len() }",
        ),
        (
            "concrete",
            "fn f(v: Vec[i64]) -> Vec[i64] { let o: Option[i64] = None; let w = [o].to_vec(); v.clone() }",
        ),
    ] {
        let s = Scratch::new(&format!("ok-{name}"), &format!("{MAIN}{item}\n"));
        let (ok, out) = s.karac("check");
        assert!(ok, "{name}: check refused a clone that clones:\n{out}");
    }
}
