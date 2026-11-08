//! B-2026-11-08-59 — other languages' queue names (`Deque`, `ArrayDeque`,
//! `deque`, `Queue`, and Rust's `LinkedList` used as one) are Kāra's
//! `VecDeque`: `karac check` names it, as it names `Map` for `HashMap`, and
//! `karac fix` writes it. Edit distance does not reach it from them.

use std::process::Command;

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .unwrap();
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
fn a_queue_named_by_another_language_is_vecdeque() {
    let dir = std::env::temp_dir().join(format!("karac-queue-names-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["Deque", "ArrayDeque", "deque", "Queue", "LinkedList"] {
        let path = dir.join(format!("{name}.kara"));
        std::fs::write(
            &path,
            format!(
                "fn main() {{\n    let mut q: {name}[i64] = {name}.new();\n    q.push_back(3);\n    println(f\"{{q.len()}}\");\n}}\n"
            ),
        )
        .unwrap();
        let p = path.to_str().unwrap();
        let (ok, out) = karac(&["check", p]);
        assert!(
            !ok && out.contains(&format!(
                "undefined type '{name}', did you mean 'VecDeque'?"
            )) && out.contains(&format!(
                "undefined name '{name}', did you mean 'VecDeque'?"
            )),
            "{name}: {out}"
        );
        karac(&["fix", p]);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.contains("let mut q: VecDeque[i64] = VecDeque.new();"),
            "{name}: {after}"
        );
        let (ok, out) = karac(&["run", p]);
        assert!(ok && out.ends_with("1\n"), "{name}: {out}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_program_s_own_queue_is_not_renamed() {
    let dir = std::env::temp_dir().join(format!("karac-queue-own-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(
        &path,
        "struct Queue { items: Vec[i64] }\nfn main() {\n    let q = Queue { items: [1, 2] };\n    println(f\"{q.items.len()}\");\n}\n",
    )
    .unwrap();
    let (ok, out) = karac(&["run", path.to_str().unwrap()]);
    assert!(
        ok && out.ends_with("2\n") && !out.contains("VecDeque"),
        "{out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
