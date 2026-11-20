//! B-2026-11-20-32: the `let mut` rule took a baked library method's
//! receiver from its signature's first parameter, which is the first one
//! after `self`. `BufReader.read_line(ref self, buf: mut ref String)` read as
//! a write, so a plain `let br` was refused (E0510), while
//! `consume(mut ref self, n: i64)` and `fill_buf(mut ref self)` read as reads,
//! so they were accepted on a plain `let`. The receiver now comes from the
//! library's source.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <args> main.kara` on `src` next to a three-line `data.txt`;
/// returns (exit code, stdout, stderr).
fn karac(src: &str, args: &[&str]) -> (Option<i32>, String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-librecv-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    std::fs::write(dir.join("data.txt"), "one\ntwo\nthree\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const READ_LINE: &str = "fn main() {
    let br = BufReader.new(File.open(\"data.txt\").unwrap());
    let mut s = String.new();
    let _ = br.read_line(mut s);
    let mut rest = String.new();
    let _ = br.read_to_string(mut rest);
    print(s);
    print(rest);
}
";

#[test]
fn read_line_on_a_plain_let_bufreader_is_accepted() {
    let (code, out, err) = karac(READ_LINE, &["check"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(!err.contains("warning"), "{err}");
    let (code, out, err) = karac(READ_LINE, &["run"]);
    assert_eq!(
        (code, out.as_str()),
        (Some(0), "one\ntwo\nthree\n"),
        "{err}"
    );
}

#[test]
fn read_line_on_a_mut_bufreader_leaves_the_mut_unused() {
    let src = READ_LINE.replace("let br", "let mut br");
    let (code, _, err) = karac(&src, &["check"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        err.contains("warning[unused_mut]") && err.contains("`br`"),
        "{err}"
    );
}

const CONSUME: &str = "fn main() {
    let br = BufReader.with_capacity(File.open(\"data.txt\").unwrap(), 4);
    let n = match br.fill_buf() { Ok(b) => b.len(), Err(_) => 0 };
    br.consume(n);
    let mut s = String.new();
    let _ = br.read_line(mut s);
    print(s);
}
";

#[test]
fn fill_buf_and_consume_on_a_plain_let_are_refused() {
    let (code, _, err) = karac(CONSUME, &["check"]);
    assert_ne!(code, Some(0), "{err}");
    assert!(
        err.contains("error[E0510]")
            && err.contains("cannot call `fill_buf` on `br`")
            && err.contains("cannot call `consume` on `br`"),
        "{err}"
    );
}

#[test]
fn fill_buf_and_consume_on_a_mut_bufreader_run() {
    let src = CONSUME.replace("let br", "let mut br");
    let (code, out, err) = karac(&src, &["run"]);
    assert_eq!((code, out.as_str()), (Some(0), "two\n"), "{err}");
}
