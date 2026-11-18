//! B-2026-11-18-57: text passed where `std.crypto` / `std.encoding` take
//! `Slice[u8]` (`sha256("abc")`) was E0200 with no fix; crypto.md names
//! `.bytes()`, which is now the machine-applicable edit.
//!
//! B-2026-11-18-56: a `Secret[T]` used where a trait it withholds is needed
//! (a log field, `dbg`, `assert_eq`, a `Set` element) was told to derive the
//! trait on `Secret`, which secret.md forbids. Each refusal now says the
//! trait is withheld on purpose and names the real path (`.expose()`,
//! `.ct_eq`, a non-secret digest), and `a == b` / `a != b` on
//! `Secret[String]` is fixed to `a.ct_eq(b)` / `not a.ct_eq(b)`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

const BYTES: &str = "import std.crypto.sha2.{sha256};\nimport std.crypto.hmac.{hmac_sha256};\nimport std.encoding.{hex_encode, base64_encode};\nstruct U { name: String }\nfn main() {\n    let s = \"abc\".to_string();\n    let u = U { name: \"n\".to_string() };\n    println(hex_encode(sha256(\"abc\")));\n    println(hex_encode(sha256(s)));\n    println(hex_encode(hmac_sha256(\"key\", u.name)));\n    println(base64_encode(s.as_str()));\n    println(hex_encode(sha256(s.to_uppercase())));\n}\n";
const BYTES_OUT: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\nba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n15d3e3bd1b61ac4338bbaea5d17c9839b972bea1940825f05acd005df0ad439a\nYWJj\nb5d4045c3f466fa91fe2cc6abe79232a1a57cdf104f7a26e716e0a1e2789df78\n";
const CT_EQ: &str = "import std.secret.{Secret};\nfn main() {\n    let a: Secret[String] = Secret.new(\"x\".to_string());\n    let b: Secret[String] = Secret.new(\"x\".to_string());\n    let c: Secret[String] = Secret.new(\"y\".to_string());\n    if a == b { println(\"same\"); }\n    println(a != c);\n}\n";

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

fn scratch(src: &str) -> (std::path::PathBuf, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-secret-bytes-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap().to_string();
    (dir, p)
}

/// Mend: `check --output=json`'s codes, then `karac fix`, then the fixed
/// program's check and its output on `lane`.
fn mend(src: &str, lane: &str) -> (Vec<String>, String, String) {
    let (dir, p) = scratch(src);
    let json = String::from_utf8_lossy(&karac(&["check", "--output=json", &p]).stdout).into_owned();
    let codes = json
        .split("\"code\":\"")
        .skip(1)
        .map(|s| s.split('"').next().unwrap().to_string())
        .collect();
    karac(&["fix", &p]);
    let fixed = std::fs::read_to_string(&p).unwrap();
    let check = karac(&["check", &p]);
    assert!(
        check.status.success(),
        "{fixed}\n{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let run = karac(&[lane, &p]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    (
        codes,
        fixed,
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

fn check_text(src: &str) -> String {
    let (dir, p) = scratch(src);
    let out = karac(&["check", &p]);
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout)
}

#[test]
fn text_passed_as_bytes_is_fixed_with_bytes() {
    let (codes, fixed, out) = mend(BYTES, "__mir-run");
    assert_eq!(
        codes.iter().filter(|c| *c == "E0200").count(),
        6,
        "{codes:?}"
    );
    for line in [
        "sha256(\"abc\".bytes())",
        "sha256(s.bytes())",
        "hmac_sha256(\"key\".bytes(), u.name.bytes())",
        "sha256(s.to_uppercase().bytes())",
    ] {
        assert!(fixed.contains(line), "{line}\n{fixed}");
    }
    assert_eq!(out, BYTES_OUT);
}

#[test]
fn secret_equality_is_fixed_to_ct_eq() {
    let (codes, fixed, out) = mend(CT_EQ, "__mir-run");
    assert_eq!(
        codes.iter().filter(|c| *c == "E0208").count(),
        2,
        "{codes:?}"
    );
    assert!(fixed.contains("if a.ct_eq(b) {"), "{fixed}");
    assert!(fixed.contains("println(not a.ct_eq(c));"), "{fixed}");
    assert_eq!(out, "same\ntrue\n");
}

#[test]
fn secret_refusals_never_advise_a_derive() {
    let head = "import std.secret.{Secret};\nimport std.log.{info};\nfn main() {\n    let a: Secret[String] = Secret.new(\"hunter2\".to_string());\n    let b: Secret[String] = Secret.new(\"x\".to_string());\n";
    for body in [
        "    info(\"login\", tok: a);\n}\n",
        "    dbg(a);\n}\n",
        "    assert_eq(a, b);\n}\n",
        "    let mut s: Set[Secret[String]] = Set.new();\n    s.insert(a);\n}\n",
    ] {
        let out = check_text(&format!("{head}{body}"));
        assert!(out.contains("withholds"), "{body}\n{out}");
        assert!(out.contains("docs/library/secret.md"), "{body}\n{out}");
        assert!(!out.contains("#[derive"), "{body}\n{out}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn the_fixed_secret_and_bytes_programs_run_compiled() {
    assert_eq!(mend(BYTES, "__mir-llvm-run").2, BYTES_OUT);
    assert_eq!(mend(CT_EQ, "__mir-llvm-run").2, "same\ntrue\n");
}
