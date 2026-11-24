//! A trait impl for a tuple over type parameters (`impl[A: Show, B: Show]
//! Show for (A, B)`), design.md § Coherence. It was refused as not
//! supported; corpus/design/ok_generic_trait_impls_for_tuples pins what it
//! runs. These pin what it refuses, and how the refusals name a tuple.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("karac-tuple-generic-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const SHOW: &str = "trait Show { fn show(self) -> String; }
impl Show for i64 { fn show(self) -> String { f\"{self}\" } }
impl Show for String { fn show(self) -> String { self.clone() } }
";

/// A parameter written twice stands for one type: `(A, A)` is not an
/// impl for `(i64, String)`.
#[test]
fn tuple_generic_impl_repeated_param_is_one_type() {
    let src = format!(
        "{SHOW}impl[A: Show] Show for (A, A) {{ fn show(self) -> String {{ self.0.show() }} }}
fn main() {{ println((1, 2).show()); println((1, \"a\".to_string()).show()); }}
"
    );
    let (ok, out) = check("repeated", &src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("no method 'show' on type '(i64, String)'"),
        "{out}"
    );
    assert_eq!(out.matches("error[").count(), 1, "{out}");
}

/// A part that misses the bound leaves the tuple without the impl, and
/// the list of what implements the trait names the generic tuple as
/// written.
#[test]
fn tuple_generic_impl_bound_must_hold_for_each_part() {
    let src = format!(
        "{SHOW}impl[A: Show, B: Show] Show for (A, B) {{ fn show(self) -> String {{ self.0.show() }} }}
fn render[T: Show](x: T) -> String {{ x.show() }}
fn main() {{ println(render((1, 2.5))); }}
"
    );
    let (ok, out) = check("bound", &src);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`(i64, f64)` does not implement `Show`"),
        "{out}"
    );
    assert!(out.contains("implemented by: (A, B), String, i64"), "{out}");
}

/// Two impls whose targets unify overlap, and the refusal names the
/// tuples; impls for different arities do not.
#[test]
fn tuple_generic_impl_overlap_names_the_tuples() {
    let src = format!(
        "{SHOW}impl Show for (i64, i64) {{ fn show(self) -> String {{ \"c\" }} }}
impl[A: Show, B: Show] Show for (A, B) {{ fn show(self) -> String {{ \"g\" }} }}
fn main() {{ }}
"
    );
    let (ok, out) = check("overlap", &src);
    assert!(!ok, "{out}");
    assert!(out.contains("overlapping impls are not allowed"), "{out}");
    assert!(out.contains("`impl Show for (A, B)`"), "{out}");
    assert!(!out.contains("Tuple"), "{out}");
    let src = format!(
        "{SHOW}impl[A: Show, B: Show] Show for (A, B) {{ fn show(self) -> String {{ \"two\" }} }}
impl[A: Show, B: Show] Show for (A, B) {{ fn show(self) -> String {{ \"again\" }} }}
fn main() {{ }}
"
    );
    let (ok, out) = check("duplicate", &src);
    assert!(!ok, "{out}");
    assert!(out.contains("`(A, B)` already implements `Show`"), "{out}");
    let src = format!(
        "{SHOW}impl[A: Show, B: Show] Show for (A, B) {{ fn show(self) -> String {{ \"two\" }} }}
impl[A: Show, B: Show, C: Show] Show for (A, B, C) {{ fn show(self) -> String {{ \"three\" }} }}
fn main() {{ println((1, 2).show()); println((1, 2, 3).show()); }}
"
    );
    let (ok, out) = check("arities", &src);
    assert!(ok, "{out}");
}
