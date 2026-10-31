//! Contracts on v2. A struct `invariant` belongs to the verification track,
//! not 0.9, so `karac check` refuses it by name (B-2026-10-31-23), and an
//! `ensures(result)` clause reads the returned value without moving it out
//! of the return place (B-2026-10-31-29). examples/weave, which needed both,
//! prints its README's output.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn program(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-contracts-v2-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    path
}

fn karac(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .env("KARAC_AUTO_PAR", "0")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn all(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The MIR interpreter, and under `llvm` `karac run`, print `expected`.
fn prints(path: &Path, expected: &str) {
    let mut cmds = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        cmds.push("run");
    }
    for cmd in cmds {
        let out = karac(&[cmd], path);
        assert!(out.status.success(), "{cmd}: {}", all(&out));
        assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "{cmd}");
    }
}

#[test]
fn check_refuses_a_struct_invariant_as_a_later_track() {
    for (tag, clause) in [("plain", "invariant"), ("impl", "impl invariant")] {
        let path = program(
            tag,
            &format!(
                "struct Counter {{\n    n: i64,\n\n    {clause} self.n >= 0\n}}\n\n\
                 fn main() {{\n    let c = Counter {{ n: 1 }};\n    println(c.n);\n}}\n"
            ),
        );
        let out = karac(&["check", "--output=json"], &path);
        let text = all(&out);
        assert!(!out.status.success(), "{text}");
        assert!(text.contains("\"E_DEFERRED_TRACK\""), "{text}");
        assert!(
            text.contains(&format!("`{clause}` on `Counter` is not available")),
            "{text}"
        );
        assert!(text.contains("verification track"), "{text}");
    }
}

#[test]
fn an_ensures_clause_matching_a_returned_value_leaves_it_returned() {
    let path = program(
        "ensures-match",
        "struct Row {\n    name: String,\n    n: i64,\n}\n\n\
         fn parse(raw: String) -> Result[Row, String]\n    \
         ensures(result) match result {\n        Ok(row) => row.n > 0,\n        Err(_) => true,\n    }\n{\n    \
         if raw.is_empty() {\n        return Err(\"empty\".to_string());\n    }\n    \
         Ok(Row { name: raw.clone(), n: 1 })\n}\n\n\
         fn main() {\n    match parse(\"a\".to_string()) {\n        \
         Ok(r) => println(f\"{r.name} {r.n}\"),\n        Err(e) => println(e),\n    }\n    \
         match parse(\"\".to_string()) {\n        Ok(r) => println(f\"{r.name} {r.n}\"),\n        \
         Err(e) => println(e),\n    }\n}\n",
    );
    let out = karac(&["check"], &path);
    assert!(out.status.success(), "{}", all(&out));
    prints(&path, "a 1\nempty\n");
}

#[test]
fn weave_prints_its_readme_output() {
    let main = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/weave/src/main.kara");
    prints(
        &main,
        "  ok    alice@example.com  usd=13.5  x3\n\
         \x20 ok    bob@example.com  usd=4.32  x10\n\
         \x20 ok    carol@example.com  usd=10.789200000000001  x1\n\
         \x20 skip  email is empty\n\
         \x20 skip  email 'bad-no-at' has no '@'\n\
         \x20 skip  price -2.0 is not > 0\n\
         summary: 3 rows, usd_total=94.48920000000001\n",
    );
}
