//! B-2026-11-07-19: `BufReader.read_line` / `read_to_string` (and a child's
//! `read_to_string`) append into the `String` they are given. A `String`
//! built from a literal still has the literal's static bytes (`cap == 0`),
//! and the runtime's append computed its free room as `cap - len`, which
//! wrapped: compiled code wrote the line into the literal's static data and
//! crashed. The interpreter was always right; both backends must print the
//! appended text.

use std::process::{Command, Stdio};

#[test]
fn reading_into_a_literal_initialised_string_appends() {
    let dir = std::env::temp_dir();
    let data = dir.join(format!("karac-io-literal-{}.txt", std::process::id()));
    std::fs::write(&data, "abc\ndef").expect("write data");
    let src = format!(
        "import std.io.{{File, BufReader}};
fn main() {{
    let r = BufReader.new(File.open(\"{p}\").unwrap());
    let mut s: String = \">\";
    println(r.read_line(mut s).unwrap());
    println(s);
    let mut t: String = \"<\";
    println(r.read_to_string(mut t).unwrap());
    println(t);
    let all = BufReader.new(File.open(\"{p}\").unwrap());
    let mut u: String = \"[\";
    println(all.read_to_string(mut u).unwrap());
    println(f\"{{u}}]\");
}}
",
        // Forward slashes: a Windows temp path's backslashes would read as
        // escapes inside the Kāra string literal.
        p = data.display().to_string().replace('\\', "/")
    );
    let prog = dir.join(format!("karac-io-literal-{}.kara", std::process::id()));
    std::fs::write(&prog, &src).expect("write program");
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(b)
            .arg(&prog)
            .stdin(Stdio::null())
            .output()
            .expect("karac");
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !out.status.success()
            && stderr.contains("archive")
            && std::env::var("KARAC_REQUIRE_RUNTIME_ARCHIVE").as_deref() != Ok("1")
        {
            // A checkout without the archives skips the compiled lane.
            eprintln!("skipped {b}: {stderr}");
            continue;
        }
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "4\n>abc\n\n3\n<def\n7\n[abc\ndef]\n",
            "{b}: {stderr}"
        );
    }
    let _ = std::fs::remove_file(&prog);
    let _ = std::fs::remove_file(&data);
}
