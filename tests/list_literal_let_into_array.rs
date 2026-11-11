//! Rust's `let local = [127u8, 0, 0, 1];` is an array there and a `Vec` in
//! Kāra (design.md § Collection literals), so passing it to an
//! `Array[u8, 4]` parameter is refused. The refusal now says why at each
//! use, and when the binding only ever holds its literal and the length
//! matches, `karac fix` declares it `local: Array[u8, 4]` (B-2026-11-10-47,
//! from porting socks5_proxy).

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn temp(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-list-let-array-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    path
}

const SOCKS: &str = "fn f(a: Array[u8, 4]) -> i64 {\n    a.len()\n}\n\nfn main() {\n    \
                     let local = [127u8, 0, 0, 1];\n    println(f(local));\n    \
                     println(f(local));\n}\n";

#[test]
fn a_list_literal_let_used_as_an_array_is_declared_one() {
    let path = temp("socks", SOCKS);
    let out = karac(&["check"], &path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!out.status.success(), "{stderr}");
    assert_eq!(
        stderr
            .matches("`local` (bound at 6:9) is a `Vec`, because a list literal")
            .count(),
        2,
        "{stderr}"
    );
    assert!(
        stderr.contains("declare it `local: Array[u8, 4]`"),
        "{stderr}"
    );
    let fixed = karac(&["fix"], &path);
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.contains("let local: Array[u8, 4] = [127u8, 0, 0, 1];"),
        "{text}"
    );
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "4\n4\n",
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_list_literal_let_of_another_length_or_reassigned_gets_no_fix() {
    let src = "fn f(a: Array[u8, 4]) -> i64 {\n    a.len()\n}\n\nfn g(a: own Array[i64, 3]) -> i64 {\n    \
               a[0]\n}\n\nfn main() {\n    let mut local = [127u8, 0, 0, 1];\n    \
               local = [1u8, 2, 3, 4];\n    println(f(local));\n    let three = [1, 2, 3];\n    \
               println(g(three));\n    let wrong = [1u8, 2, 3];\n    println(f(wrong));\n}\n";
    let path = temp("edges", src);
    let out = karac(&["check"], &path);
    let _ = std::fs::remove_file(&path);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // Reassigned: explained, no annotation offered.
    assert!(
        stderr.contains("`local` (bound at 10:13) is a `Vec`"),
        "{stderr}"
    );
    assert!(!stderr.contains("declare it `local:"), "{stderr}");
    // A list of open literals takes the parameter's element type.
    assert!(
        stderr.contains("declare it `three: Array[i64, 3]`"),
        "{stderr}"
    );
    // Three elements are no `Array[u8, 4]`.
    assert!(!stderr.contains("`wrong`"), "{stderr}");
}
