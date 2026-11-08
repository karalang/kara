//! B-2026-11-08-75: a `select` used as a function's tail value drew false
//! `must_use` warnings (W0278) on statements its desugar made, and a library
//! body's discard reached the user's output at a line their file does not
//! have. A select's value is used where it is; a discarded one still warns.

use std::process::Command;

fn karac(tag: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-select-tail-must-use-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const TAIL: &str = r#"import std.time.{Duration};
fn get(rx: Receiver[i64]) -> Result[i64, String] {
    select {
        v = rx.recv() => match v { Some(v) => Ok(v), None => Err("closed".to_string()) },
        _ = after(Duration.ms(5)) => Err("timeout".to_string()),
    }
}
fn main() {
    let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);
    let _ = tx.send(3);
    println(get(rx).unwrap());
}
"#;

#[test]
fn a_select_tail_is_used_not_discarded() {
    for cmd in ["check", "run"] {
        let (ok, out, err) = karac(cmd, cmd, TAIL);
        assert!(ok, "{cmd}: {err}");
        assert!(!err.contains("must_use"), "{cmd}: {err}");
        assert!(!out.contains("must_use"), "{cmd}: {out}");
        if cmd == "run" {
            assert_eq!(out, "3\n");
        }
    }
}

#[test]
fn a_library_bodys_discard_is_not_the_programs() {
    // `recv_timeout` is a library function whose body is a `select`.
    let src = "import std.time.{Duration};\n\
               fn main() {\n\
               \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);\n\
               \x20   let _ = tx.send(4);\n\
               \x20   println(rx.recv_timeout(Duration.ms(5)).unwrap());\n\
               }\n";
    let (ok, out, err) = karac("library", "check", src);
    assert!(ok, "{err}");
    assert!(!err.contains("warning"), "{err}");
    assert!(!out.contains("warning"), "{out}");
}

#[test]
fn a_discarded_select_result_still_warns() {
    let src = "import std.time.{Duration};\n\
               fn main() {\n\
               \x20   let (tx, rx): (Sender[i64], Receiver[i64]) = channel(1);\n\
               \x20   let _ = tx.send(5);\n\
               \x20   select {\n\
               \x20       v = rx.recv() => match v { Some(v) => Ok(v), None => Err(0) },\n\
               \x20       _ = after(Duration.ms(5)) => Err(1),\n\
               \x20   };\n\
               }\n";
    let (_, out, err) = karac("discarded", "check", src);
    let all = format!("{out}{err}");
    assert_eq!(all.matches("warning[must_use]").count(), 1, "{all}");
    assert!(all.contains(":5:5:"), "{all}");
}
