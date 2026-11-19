//! B-2026-11-19-63: `#[derive(Serialize)]` (or `Deserialize`) over a type
//! holding a `Secret[T]` passed `karac check`, then every MIR lane refused
//! the derived code with "no method 'serialize' on type 'Secret'". A secret
//! is never serialized implicitly (design.md § Secret Type, secret.md), so
//! the derive is refused at the declaration, naming the field and the two
//! ways to put it on the wire on purpose.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn check(src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-derive-secret-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("main.kara");
    std::fs::write(&p, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", p.to_str().unwrap()])
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn derive_serialize_over_a_secret_field_is_refused_at_the_declaration() {
    let (ok, out) = check(
        "import std.secret.{Secret};\n#[derive(Serialize)]\nstruct User {\n    name: String,\n    token: Secret[String],\n}\nfn main() {\n    let u = User { name: \"a\".to_string(), token: Secret.new(\"t\".to_string()) };\n    println(u.name);\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("struct 'User' derives Serialize"), "{out}");
    assert!(
        out.contains("field 'token' has type 'Secret[String]'"),
        "{out}"
    );
    assert!(out.contains("never serialized by a derive"), "{out}");
    assert!(out.contains("impl Serialize"), "{out}");
    assert!(!out.contains("no method 'serialize'"), "{out}");
}

#[test]
fn derive_deserialize_over_a_nested_secret_is_refused() {
    let (ok, out) = check(
        "import std.secret.{Secret};\n#[derive(Deserialize)]\nstruct Keys {\n    all: Vec[Option[Secret[String]]],\n}\nfn main() {\n    let k = Keys { all: [] };\n    println(k.all.len());\n}\n",
    );
    assert!(!ok, "{out}");
    assert!(out.contains("struct 'Keys' derives Deserialize"), "{out}");
    assert!(out.contains("never deserialized by a derive"), "{out}");
}

#[test]
fn a_user_type_named_secret_still_derives() {
    let (ok, out) = check(
        "#[derive(Serialize)]\nstruct Secret {\n    v: i64,\n}\n#[derive(Serialize)]\nstruct Holder {\n    s: Secret,\n}\nfn main() {\n    let h = Holder { s: Secret { v: 1 } };\n    println(h.s.v);\n}\n",
    );
    assert!(ok, "{out}");
}
