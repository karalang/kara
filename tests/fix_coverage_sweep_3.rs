//! The third round of the corpus fix-coverage sweep (review thread,
//! 2026-11-16): a part of a bare parameter given away after the move-out fix
//! borrowed it, and a closure that hands back a bare parameter. Each test
//! runs `karac fix` and checks the program it leaves, then runs it.

use std::process::Command;

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac")
}

/// Writes `src`, runs `karac fix`, and returns the fixed text, the output of
/// `karac check` on it and the stdout of `karac run`.
fn fixed(tag: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-fixcov3-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap();
    let fix = karac(&["fix", p]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let after = std::fs::read_to_string(&f).unwrap();
    let check = karac(&["check", p]);
    let text = String::from_utf8_lossy(&check.stdout).into_owned()
        + &String::from_utf8_lossy(&check.stderr);
    let run = karac(&["run", p]);
    let out = String::from_utf8_lossy(&run.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&dir);
    (after, text, out)
}

const JOB: &str = "struct Job { id: i64, name: String }
struct Outcome { job: Job, w: i64 }
struct Q { dead: Vec[Job] }
impl Q {
    fn settle(mut ref self, o: Outcome) {
        LET
        println(f\"{job.id} {o.w}\");
        self.dead.push(job);
    }
}
fn main() {
    let mut q = Q { dead: Vec.new() };
    q.settle(Outcome { job: Job { id: 7, name: \"a\".to_string() }, w: 2 });
    println(f\"{q.dead.len()} {q.dead[0].name}\");
}
";

/// B-2026-11-16-206: `let job = ref o.job;` (the fix a move out of `o.job`
/// takes) and then `push(job)`: the give-away takes `o` `own` and drops the
/// `ref`, where it used to stop at "expected 'Job', found 'ref Job'".
#[test]
fn a_borrowed_part_given_away_takes_its_parameter_own() {
    let (after, check, out) = fixed("refpart", &JOB.replace("LET", "let job = ref o.job;"));
    assert!(
        after.contains("fn settle(mut ref self, o: own Outcome)"),
        "{after}"
    );
    assert!(after.contains("let job = o.job;"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "7 2\n1 a\n");
}

/// B-2026-11-16-206: the same program as written, a move out of `o.job`,
/// reaches the same fixed program over two passes.
#[test]
fn a_moved_part_given_away_reaches_the_same_fix() {
    let (after, check, out) = fixed("movepart", &JOB.replace("LET", "let job = o.job;"));
    assert!(
        after.contains("fn settle(mut ref self, o: own Outcome)"),
        "{after}"
    );
    assert!(after.contains("let job = o.job;"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "7 2\n1 a\n");
}

/// B-2026-11-16-207: a closure that returns a bare parameter, called where
/// an owned value is wanted, takes the parameter `own`.
#[test]
fn a_closure_returning_a_bare_parameter_takes_it_own() {
    let (after, check, out) = fixed(
        "closure",
        "struct W { s: String }
enum E { A(String), B }
fn f(w: W) -> W { let g = || w; g() }
fn fe(e: E) -> E { let g = || { println(\"x\"); e }; g() }
fn main() {
    let w = f(W { s: \"a\".to_string() });
    let e = fe(E.A(\"b\".to_string()));
    match e { E.A(s) => println(f\"{w.s} {s}\"), E.B => println(\"b\") }
}
",
    );
    assert!(after.contains("fn f(w: own W) -> W"), "{after}");
    assert!(after.contains("fn fe(e: own E) -> E"), "{after}");
    assert!(check.contains("All checks passed"), "{check}");
    assert_eq!(out, "x\na b\n");
}
