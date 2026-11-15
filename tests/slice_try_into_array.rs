//! `s.try_into_array()` (B-2026-11-15-77, library/collections.md §
//! `Slice[T]`): a `Slice[T]`, `Vec[T]` or `Array[T, N]` copied into an
//! `Array[T, N]`, `None` when the length is not `N`. `N` comes from the
//! expected type only: a parameter, an annotated `let` or a return type.

use std::process::{Command, Output};

fn karac(name: &str, args: &[&str], src: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("karac-tia-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

const SRC: &str = "fn word(b: Slice[u8]) -> u32 {
    u32.from_le_bytes(b[..4].try_into_array().unwrap())
}
fn pair(v: Vec[i64]) -> Option[Array[i64, 2]] {
    v.try_into_array()
}
fn main() {
    let v: Vec[u8] = [1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 1];
    let a: Array[u8, 4] = v[..4].try_into_array().unwrap();
    println(f\"{a[0]} {a[3]}\");
    println(u32.from_le_bytes(v[4..8].try_into_array().unwrap()));
    println(u64.from_le_bytes(v[4..12].try_into_array().unwrap()));
    let short: Option[Array[u8, 4]] = v[..3].try_into_array();
    println(short.is_none());
    println(word(v.as_slice()));
    let arr: Array[u8, 3] = [7, 8, 9];
    let back: Array[u8, 3] = arr.try_into_array().unwrap();
    println(back[2]);
    match pair([5, 6]) {
        Some(p) => println(f\"{p[0]} {p[1]}\"),
        None => println(\"none\"),
    }
    println(pair([5, 6, 7]).is_none());
}
";

// From the bytes by hand: [2,0,0,0] is 2, and [2,0,0,0,0,0,0,1] is
// 2 + 2^56 = 72057594037927938.
const WANT: &str = "1 0\n2\n72057594037927938\ntrue\n1\n9\n5 6\ntrue\n";

#[test]
fn try_into_array_copies_or_gives_none_on_every_lane() {
    let check = karac("check", &["check"], SRC);
    assert!(check.status.success(), "{}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(b, &[b], SRC);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            WANT,
            "{b}: {}",
            text(&out)
        );
    }
}

#[test]
fn try_into_array_needs_an_expected_length_and_copy_elements() {
    let out = karac(
        "no-length",
        &["check"],
        "fn main() {
    let v: Vec[u8] = [1, 2];
    let a = v.try_into_array();
    println(a.is_some());
}
",
    );
    let t = text(&out);
    assert!(!out.status.success());
    assert!(
        t.contains("cannot tell the length of the array `try_into_array()` makes")
            && t.contains("let a: Array[u8, 4] = s.try_into_array().unwrap();"),
        "{t}"
    );
    let out = karac(
        "not-copy",
        &["check"],
        "fn main() {
    let s: Vec[String] = [\"a\".to_string()];
    let b: Option[Array[String, 1]] = s.try_into_array();
    println(b.is_some());
}
",
    );
    let t = text(&out);
    assert!(!out.status.success());
    assert!(t.contains("must be `Copy`, and `String` is not"), "{t}");
}

#[test]
fn karac_fix_writes_try_into_array_for_rust_s_try_into() {
    let dir = std::env::temp_dir().join(format!("karac-tia-fix-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "fn main() {
    let v: Vec[u8] = [1, 0, 0, 0, 2, 0, 0, 0];
    let x = u32.from_le_bytes(v[4..8].try_into().unwrap());
    let a: Array[u8, 4] = v[..4].try_into().unwrap();
    println(f\"{x} {a[0]}\");
}
",
    )
    .unwrap();
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&path)
        .output()
        .unwrap();
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        src.contains("v[4..8].try_into_array().unwrap()")
            && src.contains("v[..4].try_into_array().unwrap()"),
        "{src}"
    );
    assert_eq!(
        karac::mir::lower::run_source(&src)
            .expect("MIR interpreter")
            .output,
        "2 1\n"
    );
}
