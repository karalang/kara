//! An `impl` that leaves out a trait's required method is E0203, and only
//! that: the trait's default body calling the method, and the program's own
//! calls of it, are not E0236 `no method` errors of their own
//! (B-2026-11-25-62).

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac check --output=json` on `src`: each error's code.
fn error_codes(src: &str) -> Vec<String> {
    let dir = std::env::temp_dir().join(format!(
        "karac-missing-trait-method-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "--output=json"])
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout);
    let json: serde_json::Value = serde_json::from_str(&text).expect(&text);
    json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .map(|d| d["code"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn a_missing_required_method_is_one_error() {
    let codes = error_codes(
        "trait Describable {\n\
             fn name(self) -> String;\n\
             fn description(self) -> String { f\"A thing called {self.name()}\" }\n\
         }\n\
         struct Rock {}\n\
         impl Describable for Rock {}\n\
         fn main() { let r = Rock {}; println(r.description()); println(r.name()); }\n",
    );
    assert_eq!(codes, vec!["E0203"]);
}

#[test]
fn a_generic_impl_missing_a_required_method_is_one_error() {
    let codes = error_codes(
        "trait Sized2 {\n\
             fn size(self) -> i64;\n\
             fn twice(self) -> i64 { self.size() * 2 }\n\
         }\n\
         struct Bag[T] { items: Vec[T] }\n\
         impl[T] Sized2 for Bag[T] {}\n\
         fn main() { let b = Bag { items: vec![1, 2] }; println(b.twice() + b.size()); }\n",
    );
    assert_eq!(codes, vec!["E0203"]);
}

#[test]
fn a_method_no_trait_declares_is_still_e0236() {
    let codes = error_codes(
        "trait Describable {\n\
             fn name(self) -> String;\n\
         }\n\
         struct Rock {}\n\
         impl Describable for Rock {}\n\
         fn main() { let r = Rock {}; println(r.nmae()); }\n",
    );
    assert_eq!(codes, vec!["E0203", "E0236"]);
}
