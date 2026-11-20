//! Review-thread rulings of 2026-11-20 on the io conformance pass
//! (conformance/io-2026-11-20), pinned on the interpreter and, with
//! `--features llvm`, the compiled backend:
//! - B-2026-11-20-51: `fs.copy` of a file onto itself, by the same path or
//!   through a link, is `InvalidInput` and leaves the file intact (it was
//!   emptied, as Rust's `std::fs::copy` empties it).
//! - B-2026-11-20-52: `std.path.parent` ends in no `.` component:
//!   `parent("a/./b")` is `"a"`, not `"a/."`.
//! - B-2026-11-20-53: `eprint` is in the prelude beside `eprintln`.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const PROG: &str = r#"import std.path.{parent};
fn main() {
    let _ = fs.write("f.txt", "keep me");
    match fs.copy("f.txt", "f.txt") { Ok(n) => println(f"same path: ok {n}"), Err(e) => println(f"same path: err[{e}]") }
    let _ = fs.symlink("f.txt", "l.txt");
    match fs.copy("f.txt", "l.txt") { Ok(n) => println(f"through link: ok {n}"), Err(e) => println(f"through link: err[{e}]") }
    match fs.copy("l.txt", "./f.txt") { Ok(n) => println(f"link to file: ok {n}"), Err(e) => println(f"link to file: err[{e}]") }
    match fs.read_to_string("f.txt") { Ok(s) => println(f"intact: {s}"), Err(e) => println(f"err[{e}]") }
    match fs.copy("f.txt", "g.txt") { Ok(n) => println(f"other: ok {n}"), Err(e) => println(f"err[{e}]") }
    match fs.copy("f.txt", "g.txt") { Ok(n) => println(f"over existing: ok {n}"), Err(e) => println(f"err[{e}]") }
    eprint("to stderr ");
    eprint(42);
    eprintln("");
    for p in ["/usr//lib/./x", "../x/./y", "a/./b", "/./x", "./x", "a/././b/", "a/b/c", "/a", "a"] {
        match parent(p) { Some(s) => println(f"{p} -> [{s}]"), None => println(f"{p} -> None") }
    }
}
"#;

const OUT: &str = "same path: err[invalid input]
through link: err[invalid input]
link to file: err[invalid input]
intact: keep me
other: ok 7
over existing: ok 7
/usr//lib/./x -> [/usr//lib]
../x/./y -> [../x]
a/./b -> [a]
/./x -> [/]
./x -> [.]
a/././b/ -> [a]
a/b/c -> [a/b]
/a -> [/]
a -> []
";

#[cfg(unix)]
#[test]
fn copy_onto_itself_eprint_and_parent_on_every_backend() {
    static N: AtomicUsize = AtomicUsize::new(0);
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("karac-iorul-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.kara"), PROG).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(b)
            .arg("main.kara")
            .current_dir(&dir)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        let _ = std::fs::remove_dir_all(&dir);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success()
            && stderr.contains("archive")
            && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1")
        {
            eprintln!("skipped {b}: {stderr}");
            continue;
        }
        assert_eq!(String::from_utf8_lossy(&out.stdout), OUT, "{b}: {stderr}");
        assert_eq!(stderr, "to stderr 42\n", "{b}");
    }
}

#[test]
fn eprint_checks_clean() {
    let dir = std::env::temp_dir().join(format!("karac-iorul-chk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("main.kara"),
        "fn main() {\n    eprint(\"a\");\n    eprint(1);\n}\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "main.kara"])
        .current_dir(&dir)
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0), "{all}");
    assert!(!all.contains("warning"), "{all}");
}
