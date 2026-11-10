//! B-2026-11-10-33 — a bare handle parameter handed to `BufReader.new` /
//! `BufWriter.new` (`fn serve(conn: TcpStream) { BufReader.new(conn) }`, a
//! Rust port's owned parameter) gives away a borrow. It got E0297 "not a
//! `ref TcpStream`" with no fix, where every other `own` parameter gets the
//! D5 refusal and the fix that writes `own`; before E0297 it passed check
//! and the buffer closed the caller's handle. Any other borrowed handle is
//! refused by name.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn scratch(tag: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-buf-borrowed-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_bare_handle_parameter_buffered_gets_the_own_fix() {
    let src = "import std.io.{BufReader, BufWriter};\nfn first(conn: TcpStream) -> String {\n    let mut r = BufReader.new(conn);\n    let mut s = String.new();\n    let _ = r.read_line(mut s);\n    s\n}\nfn say(f: File) {\n    let mut w = BufWriter.with_capacity(f, 16);\n    let _ = w.write_all(\"hi\".bytes());\n}\nfn main() {}\n";
    let path = scratch("param", src);
    let p = path.to_str().unwrap();
    let (ok, out) = karac(&["check", p]);
    assert!(!ok, "{out}");
    assert_eq!(
        out.matches("cannot give away parameter").count(),
        2,
        "{out}"
    );
    assert!(!out.contains("E0297") && !out.contains("wraps a"), "{out}");
    karac(&["fix", p]);
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("fn first(conn: own TcpStream)"), "{after}");
    assert!(after.contains("fn say(f: own File)"), "{after}");
    let (ok, out) = karac(&["check", p]);
    assert!(ok, "the fixed program checks:\n{out}");
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn another_borrowed_handle_buffered_is_refused_by_name() {
    let src = "import std.io.{BufReader};\nfn main() {\n    let files = [File.open(\"o.txt\").unwrap()].to_vec();\n    for f in files.iter() {\n        let r = BufReader.new(f);\n        drop(r);\n    }\n}\n";
    let path = scratch("iter", src);
    let (ok, out) = karac(&["check", path.to_str().unwrap()]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("`BufReader` takes the `File` it wraps") && out.contains("is borrowed"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}
