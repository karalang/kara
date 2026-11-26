//! An empty list literal `[]` whose element type nothing fixes is refused
//! by name wherever it stands, as `let mut w = [];` is: as a method's
//! receiver (`[].len()`, `[].iter().map(..).collect()`) it typed as
//! `Vec[Error]` with nothing reported, and the user saw only MIR's E0516 "a
//! type that failed to check is not lowered yet" (B-2026-11-26-75).

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-empty-list-receiver-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn refused_by_name(name: &str, src: &str, at: &str) {
    let (ok, text) = check(name, src);
    assert!(!ok, "{name}: accepted:\n{text}");
    assert!(
        text.contains("cannot infer the element type of the empty list literal `[]`"),
        "{name}: {text}"
    );
    assert!(text.contains(at), "{name}: not at {at}: {text}");
    assert!(!text.contains("not lowered yet"), "{name}: {text}");
}

#[test]
fn empty_list_receiver_of_len_is_refused_by_name() {
    refused_by_name(
        "len",
        "fn main() { let n = [].len(); println(n); }\n",
        "main.kara:1:21",
    );
}

#[test]
fn empty_list_receiver_of_an_iterator_chain_is_refused_by_name() {
    refused_by_name(
        "chain",
        "fn main() { let v: Vec[i64] = [].iter().map(|x| *x).collect(); println(v.len()); }\n",
        "main.kara:1:31",
    );
}

#[test]
fn empty_list_an_annotation_or_a_later_use_types_still_checks() {
    for (name, src) in [
        (
            "annotated",
            "fn main() { let w: Vec[i64] = []; println(w.len()); }\n",
        ),
        (
            "argument",
            "fn total(v: Vec[i64]) -> i64 { v.len() as i64 }\nfn main() { println(total([])); }\n",
        ),
        (
            "returned",
            "fn none() -> Vec[String] { [] }\nfn main() { println(none().len()); }\n",
        ),
    ] {
        let (ok, text) = check(name, src);
        assert!(ok, "{name}: {text}");
    }
}
