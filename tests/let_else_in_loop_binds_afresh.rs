//! A `let .. else` inside a loop binds afresh on every iteration, as a plain
//! `let` does, so moving what it bound (or a field of it) is a move of this
//! iteration's value, not of the previous one's. The use classifier's CFG
//! recorded a definition for a plain `let` only, so `karac check` refused
//! `let Some(done) = rx.recv() else { .. }; out[done.slot] = Some(done.outcome);`
//! in a loop as "moved inside a loop and not assigned again before the next
//! iteration", while every backend ran it (B-2026-11-07-14). A binding made
//! by a `let .. else` outside the loop and moved inside it is still refused.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-letelse-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

const PRELUDE: &str = "struct Done { slot: i64, outcome: Result[String, String] }

fn next(k: i64) -> Option[Done] {
    if k < 3 { Some(Done { slot: k, outcome: Ok(f\"r{k}\") }) } else { None }
}
";

#[test]
fn moving_a_field_of_a_let_else_binding_in_a_loop_checks() {
    let src = format!(
        "{PRELUDE}
fn main() {{
    let mut outcomes: Vec[Option[Result[String, String]]] = Vec.filled(3, None);
    for k in 0..3 {{
        let Some(done) = next(k) else {{
            return;
        }};
        outcomes[done.slot] = Some(done.outcome);
    }}
    println(outcomes.len());
}}
"
    );
    let (ok, out) = check("field", &src);
    assert!(ok, "{out}");
}

#[test]
fn moving_a_whole_let_else_binding_in_a_loop_checks() {
    let src = format!(
        "{PRELUDE}
fn main() {{
    let mut kept: Vec[Done] = Vec.new();
    let mut k = 0;
    loop {{
        let Some(done) = next(k) else {{
            break;
        }};
        kept.push(done);
        k += 1;
    }}
    println(kept.len());
}}
"
    );
    let (ok, out) = check("whole", &src);
    assert!(ok, "{out}");
}

#[test]
fn a_let_else_binding_made_outside_the_loop_is_still_refused() {
    let src = format!(
        "{PRELUDE}
fn main() {{
    let Some(done) = next(0) else {{
        return;
    }};
    let mut out: Vec[Result[String, String]] = Vec.new();
    for _ in 0..2 {{
        out.push(done.outcome);
    }}
    println(out.len());
}}
"
    );
    let (ok, out) = check("outside", &src);
    assert!(!ok, "a move of an outer binding in a loop must be refused");
    assert!(out.contains("moved inside a loop"), "{out}");
}
