//! B-2026-11-17-152: an element reached only through `mut ref`s
//! (`self.slots[i]` in a `mut ref self` method) is a `mut ref` scrutinee,
//! as a field is (core semantics §4.6), so a plain binding of it holds a
//! `mut ref` and an arm that reads `self` while it is live is E0516
//! (§5.6). When the arm only reads the binding, `karac fix` writes `ref`
//! before it: the binding is then shared, the scrutinee is reborrowed
//! shared, and the read of `self` is allowed. A binding the arm writes
//! through gets no fix.

use std::process::{Command, Stdio};

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    path.to_str().unwrap().to_string()
}

const READS: &str = "struct V { n: i64 }
struct P { slots: Vec[Option[V]], one: Option[V], k: i64 }
impl P {
    fn look(ref self, v: ref V) -> i64 { v.n + self.k }
    fn slot(mut ref self, i: i64) -> i64 {
        match self.slots[i] { Some(v) => self.look(v), None => -1 }
    }
    fn one(mut ref self) -> i64 {
        if let Some(w) = self.one { self.look(w) } else { -1 }
    }
}
fn main() {
    let mut p = P { slots: [Some(V { n: 1 }), None], one: Some(V { n: 2 }), k: 5 };
    println(f\"{p.slot(0)} {p.slot(1)} {p.one()}\");
}
";

#[test]
fn mut_ref_element_and_field_binding_gets_ref_fix() {
    let file = write("b152-ref-fix", READS);
    let check = karac(&["check", &file]);
    let stderr = String::from_utf8_lossy(&check.stderr).to_string();
    assert_eq!(stderr.matches("error[E0516]").count(), 2, "{stderr}");
    let fix = karac(&["fix", &file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&file).unwrap();
    assert!(
        fixed.contains("match self.slots[i] { Some(ref v) => self.look(v), None => -1 }"),
        "{fixed}"
    );
    assert!(
        fixed.contains("if let Some(ref w) = self.one { self.look(w) } else { -1 }"),
        "{fixed}"
    );
    let run = karac(&["run", &file]);
    let _ = std::fs::remove_file(&file);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "6 -1 7\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn mut_ref_element_binding_written_through_gets_no_ref_fix() {
    let src = "struct V { n: i64 }
struct P { slots: Vec[Option[V]] }
impl P {
    fn go(mut ref self, i: i64) -> i64 {
        match self.slots[i] { Some(v) => { let k = self.slots.len(); v.n += k; v.n }, None => 0 }
    }
}
fn main() {
    let mut p = P { slots: [Some(V { n: 1 })] };
    println(f\"{p.go(0)}\");
}
";
    let file = write("b152-no-ref-fix", src);
    let check = karac(&["check", "--output=json", &file]);
    let _ = std::fs::remove_file(&file);
    let out = String::from_utf8_lossy(&check.stdout).to_string();
    assert!(out.contains("\"code\":\"E0516\""), "{out}");
    assert!(!out.contains("\"text\":\"ref \""), "{out}");
}
