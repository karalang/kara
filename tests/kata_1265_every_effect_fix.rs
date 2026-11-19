//! B-2026-11-20-84, from kata 1265: a public function calling through a
//! `dyn` value whose type writes no clause has every effect, and an effect
//! on `*` cannot be written in a `with` clause. E0400's machine fix wrote
//! one anyway (`with allocates(*) blocks reads(*) ...`), which does not
//! parse, so `karac fix` refused its own edit and asked to have it
//! reported. No clause is offered now; the message names the callee.

use std::process::Command;

const SRC: &str = "trait Next {
    fn get_next(self) -> Option[dyn Next];
    fn show(self);
}

struct Walk {}

impl Walk {
    pub fn walk(head: own dyn Next) {
        let mut cur = Some(head);
        while let Some(node) = cur {
            node.show();
            cur = node.get_next();
        }
    }
}

fn main() {}
";

#[test]
fn every_effect_through_a_dyn_value_gets_no_clause_edit() {
    let dir = std::env::temp_dir().join(format!("karac-every-effect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("walk.kara");
    std::fs::write(&file, SRC).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", "walk.kara"])
        .current_dir(&dir)
        .output()
        .expect("karac");
    let said = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    assert!(said.contains("E0400"), "{said}");
    assert!(said.contains("has every effect"), "{said}");
    let fix = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", "--dry-run", "walk.kara"])
        .current_dir(&dir)
        .output()
        .expect("karac");
    let fixed =
        String::from_utf8_lossy(&fix.stdout).into_owned() + &String::from_utf8_lossy(&fix.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!fixed.contains("(*)"), "{fixed}");
    assert!(!fixed.contains("please report it"), "{fixed}");
}
