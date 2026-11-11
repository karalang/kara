//! Kata 2111's fold arm wrote `places.clone().count()` on a `step_by`
//! range. No library iterator is `Clone`, yet the message offered
//! "did you mean 'cloned'?", whose rename maps the items and leaves the
//! call as wrong as before. The message now says what a second walk takes
//! and offers no neighbour.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn kata_2111_step_by_clone_names_a_second_walk() {
    let stderr = check(
        "k2111-clone",
        "fn main() {
    let places = (0..10).step_by(3);
    let n = places.clone().count();
    println(f\"{n}\");
}
",
    );
    assert!(
        stderr.contains("the library's iterators are not `Clone`"),
        "{stderr}"
    );
    assert!(!stderr.contains("did you mean 'cloned'"), "{stderr}");
}

#[test]
fn kata_2111_vec_iter_clone_names_a_second_walk() {
    let stderr = check(
        "k2111-clone-iter",
        "fn main() {
    let v = [1, 2, 3].to_vec();
    let it = v.iter().map(|x| x * 2);
    let n = it.clone().count();
    println(f\"{n}\");
}
",
    );
    assert!(
        stderr.contains("the library's iterators are not `Clone`"),
        "{stderr}"
    );
}
