//! B-2026-11-24-22 (design.md §9 Associated types): there is no qualified
//! spelling such as `T.Add.Output`. It was accepted in a signature and then
//! refused at the call site by an internal lowering error (E0516,
//! `Type(UnknownName("Output"))`); now it is refused where it is written,
//! naming the forms that are.

use std::path::PathBuf;
use std::process::Command;

fn file(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-qassoc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    f
}

fn karac(args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Checks clean and prints `want` on the interpreter, and compiled with the
/// backend.
fn runs(tag: &str, src: &str, want: &str) {
    let f = file(tag, src);
    let f = f.to_str().unwrap();
    let (ok, out, err) = karac(&["check", f]);
    assert!(ok, "check refused:\n{out}{err}");
    let (ok, out, err) = karac(&["__mir-run", f]);
    assert!(ok, "interp failed:\n{out}{err}");
    assert_eq!(out, want, "interp\n{err}");
    #[cfg(feature = "llvm")]
    {
        let (ok, out, err) = karac(&["__mir-llvm-run", f]);
        assert!(ok, "mir-llvm failed:\n{out}{err}");
        assert_eq!(out, want, "mir-llvm\n{err}");
    }
}

#[test]
fn a_trait_qualified_projection_is_refused_where_it_is_written() {
    for (tag, src, at) in [
        (
            "ret",
            "fn f[T: Add + Mul + Copy](a: T) -> T.Add.Output { a + a }\nfn main() { println(f(2)); }\n",
            ":1:36:",
        ),
        (
            "param",
            "fn h[T: Add + Copy](a: T, b: T.Add.Output) -> i64 { 1 }\nfn main() { println(1); }\n",
            ":1:30:",
        ),
    ] {
        let f = file(tag, src);
        let (ok, out, err) = karac(&["check", f.to_str().unwrap()]);
        let all = format!("{out}{err}");
        assert!(!ok, "{tag}: accepted:\n{all}");
        assert!(
            all.contains(&format!("{at} `T.Add.Output` is not a type: there is no qualified spelling"))
                && all.contains("`T: Add[Output = A]` with `A` among the generic parameters"),
            "{tag}:\n{all}"
        );
        assert!(!all.contains("E0516"), "{tag}: a lowering error leaked:\n{all}");
        assert_eq!(all.matches("error[").count(), 1, "{tag}: one error:\n{all}");
    }
}

#[test]
fn the_forms_the_qualified_projection_refusal_names_run() {
    runs(
        "forms",
        "fn f[T: Add[Output = A] + Mul + Copy, A](a: T) -> A { a + a }\n\
         fn g[T: Add + Copy](a: T) -> T.Output { a + a }\n\
         fn main() { println(f(2)); println(g(3)); }\n",
        "4\n6\n",
    );
}
