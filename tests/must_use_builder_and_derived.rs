//! B-2026-11-19-19, a remainder of B-2026-11-04-71: design.md § `#[must_use]`
//! mandate categories 4 and 5. A discarded builder step (`Command.arg`, now
//! marked in the library's process.kara, and the compiler-provided
//! `RequestBuilder.header` and `OpenOptions` steps) and a discarded value
//! derived from its receiver (`to_lowercase`, `replace`, `repeat`, `bytes`,
//! `Vec.to_vec`, a `len` query) warn under `karac check`. A mutation and a
//! bound result stay silent.

use std::process::{Command, Stdio};

fn check(tag: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!(
        "karac-must-use-builder-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn discarded_builder_steps_and_derived_values_warn() {
    let out = check(
        "warn",
        "fn main() {
    let c = Command.new(\"true\");
    c.arg(\"x\");
    let cl = Client.new();
    let r = cl.request(\"GET\".to_string(), \"http://x.invalid\".to_string());
    r.header(\"a\".to_string(), \"b\".to_string());
    let s = \" A \".to_string();
    s.to_lowercase();
    s.to_uppercase();
    s.replace(\"a\", \"b\");
    s.repeat(2);
    s.bytes();
    let v = [1, 2];
    v.to_vec();
    v.len();
}
",
    );
    for key in [
        "Command.arg",
        "RequestBuilder.header",
        "String.to_lowercase",
        "String.to_uppercase",
        "String.replace",
        "String.repeat",
        "String.bytes",
        "Vec.to_vec",
        "Vec.len",
    ] {
        assert!(
            out.contains(&format!("discarded return value of `{key}`")),
            "{key}\n{out}"
        );
    }
    assert!(out.contains("loses every option set on it"), "{out}");
    assert!(out.contains("leaves it as it was"), "{out}");
}

#[test]
fn mutations_and_bound_results_stay_silent() {
    let out = check(
        "silent",
        "fn main() {
    let mut v = [1, 2];
    v.push(3);
    let n = v.len();
    let mut s = \"a\".to_string();
    s.push_str(\"b\");
    let l = s.to_lowercase();
    let _c = Command.new(\"true\").arg(\"x\");
    let _ = s.repeat(2);
    println(f\"{n} {l} {v.len()}\");
}
",
    );
    assert!(!out.contains("must_use"), "{out}");
}
