//! A method or associated function of an inherent impl takes design.md §4's
//! three levels of visibility as a free function does (B-2026-11-25-54):
//! `pub` everywhere, the default in its package, `private` in its
//! directory. A call from where it is not visible is `E0111`, in every
//! spelling: a method call, `Type.f()`, `Type.m(x)` and `Type.m` as a value.
//! A trait's methods are as visible as the trait.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "karac-method-visibility-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Scratch(dir)
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `karac check` in `dir`: whether it passed, and what it printed.
fn check(dir: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .current_dir(dir)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

const REC: &str = "pub struct Rec { pub v: i64 }\n\
impl Rec {\n\
    pub fn new(v: i64) -> Rec { Rec { v: v } }\n\
    fn shown(ref self) -> i64 { self.v }\n\
    private fn hidden(ref self) -> i64 { self.v }\n\
    private fn make() -> Rec { Rec { v: 3 } }\n\
}\n";

/// A package whose `src/d/x.kara` declares `Rec`, and whose main calls
/// `body`.
fn package(name: &str, body: &str) -> Scratch {
    let d = Scratch::new(name);
    d.write("kara.toml", &format!("[package]\nname = \"{name}\"\n"));
    d.write("src/d/x.kara", REC);
    d.write(
        "src/main.kara",
        &format!("import d.x.Rec;\nfn main() {{ {body} }}\n"),
    );
    d
}

#[test]
fn private_method_from_another_directory_is_e0111_in_every_spelling() {
    for (i, body) in [
        "let r = Rec.new(1); println(r.hidden());",
        "let r = Rec.new(1); println(Rec.hidden(r));",
        "let r = Rec.new(1); let f = Rec.hidden; println(f(r));",
        "let r = Rec.make(); println(r.v);",
    ]
    .iter()
    .enumerate()
    {
        let d = package(&format!("private{i}"), body);
        let (ok, text) = check(d.path());
        let what = if body.contains("make") {
            "`Rec.make`"
        } else {
            "`Rec.hidden`"
        };
        assert!(
            !ok && text.contains("error[E0111]")
                && text.contains(&format!(
                    "{what} in module `d.x` is `private` — visible only to files in the same \
                     directory"
                )),
            "{body}:\n{text}"
        );
    }
}

#[test]
fn default_and_pub_methods_are_visible_in_their_package() {
    let d = package(
        "default_in_package",
        "let r = Rec.new(1); println(r.shown()); println(Rec.shown(r));",
    );
    let (ok, text) = check(d.path());
    assert!(ok, "{text}");
}

#[test]
fn private_method_from_its_own_directory_is_visible() {
    let d = Scratch::new("same_dir");
    d.write("kara.toml", "[package]\nname = \"same_dir\"\n");
    d.write("src/d/x.kara", REC);
    d.write(
        "src/d/y.kara",
        "import d.x.Rec;\npub fn peek(r: Rec) -> i64 { r.hidden() + Rec.make().v }\n",
    );
    d.write(
        "src/main.kara",
        "import d.x.Rec;\nimport d.y.peek;\nfn main() { println(peek(Rec.new(1))); }\n",
    );
    let (ok, text) = check(d.path());
    assert!(ok, "{text}");
}

/// A path dependency `geo-kit` whose `Circle` has a `pub`, a default and a
/// `private` method, and a trait impl.
fn dependent(name: &str, body: &str) -> Scratch {
    let d = Scratch::new(name);
    d.write("lib/kara.toml", "[package]\nname = \"geo-kit\"\n");
    d.write(
        "lib/src/lib.kara",
        "pub import shapes.circle.{Circle, Area};\n",
    );
    d.write(
        "lib/src/shapes/circle.kara",
        "pub trait Area { fn area(ref self) -> i64; }\n\
         pub struct Circle { pub r: i64 }\n\
         impl Circle {\n\
             pub fn new(r: i64) -> Circle { Circle { r: r } }\n\
             fn inner(ref self) -> i64 { self.r }\n\
             private fn very(ref self) -> i64 { 9 }\n\
         }\n\
         impl Area for Circle { fn area(ref self) -> i64 { 3 * self.inner() * self.r } }\n",
    );
    d.write(
        "bin/kara.toml",
        &format!(
            "[package]\nname = \"{name}\"\n\n[dependencies]\ngeo-kit = {{ path = \"../lib\" }}\n"
        ),
    );
    d.write(
        "bin/src/main.kara",
        &format!("import geo_kit.{{Circle, Area}};\nfn main() {{ {body} }}\n"),
    );
    d
}

#[test]
fn default_method_of_a_dependency_is_e0111() {
    let d = dependent("dep_default", "println(Circle.new(2).inner());");
    let (ok, text) = check(&d.path().join("bin"));
    assert!(
        !ok && text.contains("error[E0111]")
            && text.contains(
                "`Circle.inner` in package `geo_kit` is not `pub` — only `pub` items can be used \
                 across packages"
            ),
        "{text}"
    );
}

#[test]
fn private_method_of_a_dependency_is_e0111() {
    let d = dependent("dep_private", "println(Circle.new(2).very());");
    let (ok, text) = check(&d.path().join("bin"));
    assert!(
        !ok && text.contains("error[E0111]") && text.contains("`Circle.very` in module"),
        "{text}"
    );
}

#[test]
fn pub_and_trait_methods_of_a_dependency_are_visible() {
    let d = dependent(
        "dep_ok",
        "let c = Circle.new(2); println(c.r); println(c.area());",
    );
    let (ok, text) = check(&d.path().join("bin"));
    assert!(ok, "{text}");
}
