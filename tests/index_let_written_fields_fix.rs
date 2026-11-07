//! A `let t = v[i];` over a non-`Copy` element whose binding's fields are
//! written afterwards (`t.status = 2;`) was steered toward a copy: `karac
//! fix` derived `Clone` and wrote `let mut t`, so the writes landed in the
//! copy and the element never saw them (B-2026-11-07-15, found writing the
//! mini_rq app). On a collection the function may write, the repair is the
//! mutable borrow `let t = mut ref v[i];`. Where the collection is used
//! while the binding still is, the borrow would conflict, so no fix is
//! offered; on a collection the function may not write, the copy stands.

use std::process::{Command, Output, Stdio};

const JOBS: &str = r#"struct Job { name: String, status: i64, count: i64, tags: Vec[String] }

fn mk() -> Vec[Job] {
    let mut jobs: Vec[Job] = Vec.new();
    jobs.push(Job { name: "a", status: 1, count: 0, tags: Vec.new() });
    jobs.push(Job { name: "b", status: 1, count: 0, tags: Vec.new() });
    jobs
}
"#;

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn write(tag: &str, src: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "karac-index-let-written-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, format!("{JOBS}\n{src}")).expect("write");
    path
}

/// `karac check` refuses `src`; `karac fix` repairs it so that it checks
/// clean, contains `want` and prints `out` under `__mir-run`.
fn fixed_to(tag: &str, src: &str, want: &str, out: &str) {
    let path = write(tag, src);
    let before = karac(&["check"], &path);
    assert!(!before.status.success(), "{tag}: check accepted the move");
    let fix = karac(&["fix"], &path);
    assert!(
        fix.status.success(),
        "{tag}: fix: {}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let text = std::fs::read_to_string(&path).expect("read");
    let after = karac(&["check"], &path);
    let run = karac(&["__mir-run"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(
        after.status.success(),
        "{tag}: still refused after the fix:\n{}\n{text}",
        String::from_utf8_lossy(&after.stdout)
    );
    assert!(text.contains(want), "{tag}: {text}");
    assert!(!text.contains("derive(Clone)"), "{tag}: {text}");
    assert_eq!(String::from_utf8_lossy(&run.stdout), out, "{tag}");
}

/// The JSON diagnostics `karac check` gives `src`.
fn check_json(tag: &str, src: &str) -> String {
    let path = write(tag, src);
    let out = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    assert!(!out.status.success(), "{tag}: check accepted the move");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_field_written_through_an_element_of_a_mut_ref_param_is_borrowed_mutably() {
    fixed_to(
        "param",
        "fn bump(jobs: mut ref Vec[Job], j: i64) {\n    let job = jobs[j];\n    job.status = 2;\n}\n\nfn main() {\n    let mut jobs = mk();\n    bump(mut jobs, 0);\n    println(jobs[0].status);\n}\n",
        "let job = mut ref jobs[j];",
        "2\n",
    );
}

#[test]
fn a_compound_field_write_is_a_write_too() {
    fixed_to(
        "compound",
        "fn bump(jobs: mut ref Vec[Job], j: i64) {\n    let job = jobs[j];\n    job.count += 1;\n    println(f\"{job.name} {job.count}\");\n}\n\nfn main() {\n    let mut jobs = mk();\n    bump(mut jobs, 1);\n    println(jobs[1].count);\n}\n",
        "let job = mut ref jobs[j];",
        "b 1\n1\n",
    );
}

#[test]
fn a_mut_ref_self_method_using_self_in_another_branch_borrows_mutably() {
    fixed_to(
        "self-branches",
        "struct Rq { jobs: Vec[Job], clock: i64 }\n\nimpl Rq {\n    fn put(mut ref self, j: i64) {\n        self.jobs[j].count += 10;\n    }\n\n    fn settle(mut ref self, j: i64) {\n        let job = self.jobs[j];\n        job.status = 3;\n        job.count = self.clock + job.count;\n        if job.status == 3 {\n            self.put(j);\n        } else {\n            println(f\"else {job.name}\");\n        }\n    }\n}\n\nfn main() {\n    let mut rq = Rq { jobs: mk(), clock: 5 };\n    rq.settle(0);\n    println(f\"{rq.jobs[0].status} {rq.jobs[0].count}\");\n}\n",
        "let job = mut ref self.jobs[j];",
        "3 15\n",
    );
}

#[test]
fn a_mutating_method_on_a_field_of_the_binding_borrows_mutably() {
    fixed_to(
        "method",
        "fn main() {\n    let mut jobs = mk();\n    let job = jobs[0];\n    job.tags.push(\"t\");\n    println(job.tags.len());\n    println(jobs[0].tags.len());\n}\n",
        "let job = mut ref jobs[0];",
        "1\n1\n",
    );
}

#[test]
fn a_field_of_an_element_pushed_to_is_borrowed_mutably() {
    fixed_to(
        "field",
        "fn main() {\n    let mut jobs = mk();\n    let tags = jobs[0].tags;\n    tags.push(\"t\");\n    println(jobs[0].tags.len());\n}\n",
        "let tags = mut ref jobs[0].tags;",
        "1\n",
    );
}

#[test]
fn a_collection_used_while_the_binding_is_held_gets_no_fix() {
    let json = check_json(
        "conflict",
        "fn bump(jobs: mut ref Vec[Job], j: i64) {\n    let job = jobs[j];\n    println(jobs.len());\n    job.status = 2;\n}\n\nfn main() {\n    let mut jobs = mk();\n    bump(mut jobs, 0);\n}\n",
    );
    assert!(json.contains("borrow it with `mut ref` instead"), "{json}");
    assert!(!json.contains("derive(Clone)"), "{json}");
    assert!(!json.contains("\"text\":\" mut\""), "{json}");
}

#[test]
fn a_collection_the_function_may_not_write_keeps_the_copy() {
    let json = check_json(
        "readonly-root",
        "fn show(jobs: Vec[Job], j: i64) {\n    let job = jobs[j];\n    job.status = 2;\n    println(job.status);\n}\n\nfn main() {\n    show(mk(), 0);\n}\n",
    );
    assert!(json.contains("derive(Clone)"), "{json}");
    assert!(!json.contains("mut ref "), "{json}");
}

#[test]
fn a_copy_element_written_through_keeps_the_let_mut_refusal() {
    let path = std::env::temp_dir().join(format!(
        "karac-index-let-written-copy-{}.kara",
        std::process::id()
    ));
    std::fs::write(
        &path,
        "#[derive(Copy, Clone)]\nstruct P { x: i64, y: i64 }\n\nfn main() {\n    let mut pts: Vec[P] = Vec.new();\n    pts.push(P { x: 1, y: 2 });\n    let p = pts[0];\n    p.x = 5;\n    println(f\"{p.x} {pts[0].x}\");\n}\n",
    )
    .expect("write");
    let out = karac(&["check", "--output=json"], &path);
    let _ = std::fs::remove_file(&path);
    let json = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "{json}");
    assert!(json.contains("declared without `mut`"), "{json}");
    assert!(!json.contains("mut ref "), "{json}");
}
