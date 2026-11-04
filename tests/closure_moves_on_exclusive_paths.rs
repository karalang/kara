//! Two false ownership errors found porting the mini_ledger app.
//!
//! B-2026-11-04-34: a closure (or a task) made in each arm of an `if`,
//! each moving the same value in, was reported as "moved into a closure,
//! used again here", though no run makes both. A value declared inside a
//! loop body is a new value on each turn, so the loop's back edge does not
//! join the two arms either. The same shapes with the value declared
//! outside the loop, a use after the `if`, or two tasks in a row still
//! move twice.
//!
//! B-2026-11-04-35: `match p.c { C.At(s) => .. }` with `p` from `.iter()`
//! was taken as consuming `p.c`, and so conflicted with a closure that
//! reads `p.c`; a field of a borrowed place is matched in place.

use std::process::{Command, Stdio};

fn check(name: &str, src: &str) -> (bool, String) {
    let path = std::env::temp_dir().join(format!("karac-{name}-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["check", path.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn tasks_made_in_each_arm_of_an_if_in_a_loop() {
    let src = r#"fn work(s: String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let names = vec!["a.journal".to_string(), "bb.csv".to_string()];
    let group = TaskGroup.new();
    let mut handles = Vec.new();
    for (i, name) in names.iter().enumerate() {
        let text = f"text of {name}";
        if name.ends_with(".journal") {
            handles.push(group.spawn(|| work(text, i)));
        } else {
            handles.push(group.spawn(|| work(text, i * 10)));
        }
    }
    for h in handles.into_iter() {
        println(h.join());
    }
}
"#;
    let (ok, err) = check("tasks_made_in_each_arm_of_an_if_in_a_loop", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "17\n24\n");
}

#[test]
fn tasks_made_in_each_arm_of_an_if() {
    let src = r#"fn work(s: String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let group = TaskGroup.new();
    let mut handles = Vec.new();
    let text = "text".to_string();
    let i = 3;
    if i > 2 {
        handles.push(group.spawn(|| work(text, i)));
    } else {
        handles.push(group.spawn(|| work(text, i * 10)));
    }
    for h in handles.into_iter() {
        println(h.join());
    }
}
"#;
    let (ok, err) = check("tasks_made_in_each_arm_of_an_if", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "7\n");
}

#[test]
fn closures_that_move_in_each_arm_of_an_if_in_a_loop() {
    let src = r#"fn work(s: own String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let mut out = Vec.new();
    for i in 0..3 {
        let text = f"t{i}";
        if i > 1 {
            let c = || work(text, i);
            out.push(c());
        } else {
            let c = || work(text, i * 10);
            out.push(c());
        }
    }
    println(out.len());
}
"#;
    let (ok, err) = check("closures_that_move_in_each_arm_of_an_if_in_a_loop", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "3\n");
}

#[test]
fn a_match_on_a_field_a_closure_reads_through_a_borrow() {
    let src = r#"enum C {
    N,
    At(String),
}

struct P {
    c: C,
}

fn f(ps: Vec[P]) {
    for p in ps.iter() {
        let fits = |x: i64| match p.c {
            C.At(s) => s.len() == x,
            _ => true,
        };
        let w = match p.c {
            C.At(s) => s.clone(),
            _ => "".to_string(),
        };
        println(f"{fits(1)} {w}");
    }
}

fn main() {
    f(vec![P { c: C.At("x".to_string()) }, P { c: C.N }]);
}
"#;
    let (ok, err) = check("a_match_on_a_field_a_closure_reads_through_a_borrow", src);
    assert!(ok, "{err}");
    let out = karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "true x\ntrue \n");
}

#[test]
fn a_value_declared_outside_the_loop_still_moves_once() {
    let src = r#"fn work(s: own String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let mut out = Vec.new();
    let text = "t".to_string();
    for i in 0..3 {
        if i > 1 {
            let c = || work(text, i);
            out.push(c());
        } else {
            let c = || work(text, i * 10);
            out.push(c());
        }
    }
    println(out.len());
}
"#;
    let (ok, err) = check("a_value_declared_outside_the_loop_still_moves_once", src);
    assert!(!ok, "the second move must still be refused");
    assert!(
        err.contains("value 'text' was moved into a closure"),
        "{err}"
    );
}

#[test]
fn a_use_after_the_if_still_sees_the_move() {
    let src = r#"fn work(s: own String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let text = "t".to_string();
    let i = 3;
    let mut r = 0;
    if i > 1 {
        let c = || work(text, i);
        r = c();
    }
    println(f"{r} {text}");
}
"#;
    let (ok, err) = check("a_use_after_the_if_still_sees_the_move", src);
    assert!(!ok, "the second move must still be refused");
    assert!(
        err.contains("value 'text' was moved into a closure"),
        "{err}"
    );
}

#[test]
fn two_tasks_in_a_row_still_move_twice() {
    let src = r#"fn work(s: own String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let group = TaskGroup.new();
    let text = "t".to_string();
    let a = group.spawn(|| work(text, 1));
    let b = group.spawn(|| work(text, 2));
    println(a.join() + b.join());
}
"#;
    let (ok, err) = check("two_tasks_in_a_row_still_move_twice", src);
    assert!(!ok, "the second move must still be refused");
    assert!(
        err.contains("value 'text' was moved into a closure"),
        "{err}"
    );
}

#[test]
fn tasks_in_each_arm_inside_a_loop_still_move_twice() {
    let src = r#"fn work(s: own String, n: i64) -> i64 {
    s.len() + n
}

fn main() {
    let group = TaskGroup.new();
    let mut hs = Vec.new();
    let text = "t".to_string();
    for i in 0..2 {
        if i == 0 {
            hs.push(group.spawn(|| work(text, 1)));
        } else {
            hs.push(group.spawn(|| work(text, 2)));
        }
    }
    println(hs.len());
}
"#;
    let (ok, err) = check("tasks_in_each_arm_inside_a_loop_still_move_twice", src);
    assert!(!ok, "the second move must still be refused");
    assert!(
        err.contains("value 'text' was moved into a closure"),
        "{err}"
    );
}
