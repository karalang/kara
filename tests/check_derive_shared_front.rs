//! B-2026-11-21-91: `karac check` of a program with a comptime-backed
//! derive (`Serialize`) ran the derive's expansion twice, once in check's own
//! pipeline and again in the MIR checks' front end, which re-parsed,
//! re-checked and re-expanded the program from its text. The MIR checks now
//! lower check's expanded, type-checked program, so the second front end
//! (its `comptime` phase) is gone, and they still refuse what they refused.
//! `KARAC_CHECK_SHARED_FRONT=0` keeps the old path, which is the control arm
//! here: it must still show the second expansion, or this test measures
//! nothing.

use std::path::PathBuf;
use std::process::{Command, Stdio};

const PROGRAM: &str = r#"#[derive(Serialize)]
struct Tag { name: String }

shared struct Node {
    value: i64,
}

struct Cache {
    index: Map[String, Node],
    hits: i64,
}

impl Cache {
    fn touch(mut ref self, node: Node) {
        self.hits += node.value;
    }

    fn get(mut ref self, key: String) -> Option[i64] {
        let found = self.index.get(key);
        match found {
            Some(node) => {
                self.touch(node);
                Some(node.value)
            }
            None => None,
        }
    }
}

fn main() {
    let mut c = Cache { index: Map.new(), hits: 0 };
    c.index.insert("a".to_string(), Node { value: 5 });
    println(f"{c.get("a".to_string()).unwrap_or(0)} {c.hits}");
}
"#;

fn cell(tag: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-derive-shared-front-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    dir
}

fn check(dir: &PathBuf, shared: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "cell.kara"])
        .current_dir(dir)
        .env("KARAC_CHECK_SHARED_FRONT", shared)
        .env("KARAC_MIR_TIME", "1")
        .env_remove("KARAC_CHECK_MIR")
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
    )
}

const REFUSAL: &str = "cannot mutably borrow `self` while `node` still holds a borrow of it";

#[test]
fn derive_program_mir_checks_reuse_checks_expansion() {
    let dir = cell("refused", PROGRAM);
    let (ok, shared) = check(&dir, "1");
    assert!(!ok && shared.contains(REFUSAL), "{shared}");
    assert_eq!(shared.matches(REFUSAL).count(), 1, "{shared}");
    assert!(shared.contains("check ownership+mir"), "{shared}");
    assert!(
        !shared.contains("] comptime "),
        "a second expansion ran: {shared}"
    );
    // The control arm: the old path expands the derive again.
    let (ok, text) = check(&dir, "0");
    assert!(!ok && text.contains(REFUSAL), "{text}");
    assert!(text.contains("] comptime "), "control arm is void: {text}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn derive_program_with_copying_form_passes_check() {
    let dir = cell(
        "accepted",
        &PROGRAM.replace("self.index.get(key);", "self.index.get(key).cloned();"),
    );
    let (ok, out) = check(&dir, "1");
    assert!(ok, "{out}");
    assert!(!out.contains("] comptime "), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}
