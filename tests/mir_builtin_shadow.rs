//! A library body's call of a builtin is the builtin's, whatever function of
//! that name the program declares (B-2026-10-27-29). The library is appended
//! to the program's text, so `Option.expect`'s `panic(msg)` read the
//! program's `fn panic` and returned its value instead of panicking. The
//! program's own calls still reach its own function (B-2026-10-23-10).

use std::path::PathBuf;
use std::process::{Command, Output};

fn write(tag: &str, src: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("karac-builtin-shadow-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Both v2 executors: the default, and `--interp`.
const LANES: [&[&str]; 2] = [&["run"], &["run", "--interp"]];

const OWN_PANIC: &str = "fn panic(s: Str) -> i64 {\n    7\n}\n";

#[test]
fn library_expect_panics_past_a_program_panic() {
    let path = write(
        "expect",
        &format!(
            "{OWN_PANIC}fn main() {{\n    println(panic(\"x\"));\n    \
             println(Some(3).expect(\"no\"));\n    let o: Option[i64] = None;\n    \
             println(o.expect(\"boom\"));\n    println(\"after\");\n}}\n"
        ),
    );
    for lane in LANES {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{lane:?}: {err}");
        assert_eq!(text(&out.stdout), "7\n3\n", "{lane:?}: {err}");
        assert!(err.contains("runtime error: boom"), "{lane:?}: {err}");
    }
}

#[test]
fn library_log_writes_past_a_program_eprintln() {
    let path = write(
        "log",
        "import std.log.{info, init, text_backend, Stream};\n\
         fn eprintln(n: i64) -> i64 {\n    n + 1\n}\n\
         fn main() {\n    init(text_backend(Stream.Stderr));\n    info(\"hi\", k: 1);\n    \
         println(5);\n}\n",
    );
    for lane in LANES {
        let out = karac(lane, &path);
        let err = text(&out.stderr);
        assert!(out.status.success(), "{lane:?}: {err}");
        assert_eq!(text(&out.stdout), "5\n", "{lane:?}");
        assert!(err.contains("INFO  hi k=1"), "{lane:?}: {err}");
    }
}
