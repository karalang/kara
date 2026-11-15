//! Kata 2188: a generic free function called inside the closure of a
//! method call read its own type parameters as the method's receiver's.
//! `successors(Some(f), |lap| Some(lap * r))` inside `tires.iter().map(|t|
//! ..)` typed `lap` as the iterator's `ref Vec[i64]`, since the library's
//! `successors[T, ..]` and the receiver's `T` share the name: "type 'ref
//! Vec[i64]' does not implement trait Mul". Renaming the free function's
//! parameter made it pass, which was the tell.

use std::path::Path;
use std::process::{Command, Stdio};

fn run(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2188-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", "cell.kara"])
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

#[test]
fn kata_2188_user_generic_fn_in_map_closure_keeps_its_own_t() {
    let (ok, out) = run(
        "user",
        "fn apply[T, F: Fn(ref T) -> T](x: own T, f: own F) -> T {\n    f(x)\n}\nfn main() {\n    let t: Vec[Vec[i64]] = [[2, 3].to_vec(), [5, 1].to_vec()].to_vec();\n    let b: Vec[i64] = t.iter().map(|v| { let f = v[0]; apply(f, |y| y * 2) }).collect();\n    println(f\"{b}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "[4, 10]", "{out}");
}

#[test]
fn kata_2188_successors_in_map_closure_keeps_its_own_t() {
    let (ok, out) = run(
        "successors",
        "fn main() {\n    let tires: Vec[Vec[i64]] = [[2, 3].to_vec(), [1, 2].to_vec()].to_vec();\n    let runs: Vec[Vec[i64]] = tires\n        .iter()\n        .map(|t| {\n            let f = t[0];\n            let r = t[1];\n            successors(Some(f), |lap| Some(lap * r)).take(3).collect()\n        })\n        .collect();\n    println(f\"{runs}\");\n}",
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "[[2, 6, 18], [1, 2, 4]]", "{out}");
}

#[test]
fn kata_2188_free_fn_in_closure_still_refuses_a_wrong_body() {
    let (ok, out) = run(
        "wrong",
        "fn apply[T, F: Fn(ref T) -> T](x: own T, f: own F) -> T {\n    f(x)\n}\nfn main() {\n    let t: Vec[Vec[i64]] = [[2, 3].to_vec()].to_vec();\n    let b: Vec[i64] = t.iter().map(|v| { let f = v[0]; apply(f, |y| \"no\") }).collect();\n    println(f\"{b}\");\n}",
    );
    assert!(!ok, "{out}");
}
