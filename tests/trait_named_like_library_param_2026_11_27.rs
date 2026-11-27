//! A program trait named like a library type parameter (B-2026-11-27-20).
//! The library's `C.from_iter(..)` under `[C: FromIterator[T]]` was read as a
//! call on the program's `trait C`, and every program that declared one
//! failed before it ran: "trait `C` has no method `from_iter`". A type
//! parameter in scope names the type (design.md §9 Associated functions),
//! as a local binding already did.

use std::process::Command;

fn run(name: &str, src: &str) -> (i32, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac_trait_like_param_{}_{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", path.to_str().unwrap()])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn a_program_trait_named_c_does_not_capture_the_library_collect_param() {
    let (code, out) = run(
        "lib",
        "trait C {\n    fn f(self) -> i64;\n}\nfn main() {\n    let v: Vec[i64] = [1, 2].iter().map(|x| x * 2).collect();\n    println(f\"{v}\");\n}\n",
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("[2, 4]"), "{out}");
}

#[test]
fn a_program_type_param_named_like_its_own_trait_names_the_type() {
    let (code, out) = run(
        "own",
        "trait C {\n    fn f(self) -> i64;\n}\nstruct Z {\n    n: i64,\n}\nimpl Default for Z {\n    fn default() -> Z {\n        Z { n: 4 }\n    }\n}\nfn mk[C: Default]() -> C {\n    C.default()\n}\nfn main() {\n    let z: Z = mk();\n    println(f\"{z.n}\");\n}\n",
    );
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("4\n"), "{out}");
}
