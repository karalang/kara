//! B-2026-10-31-16 step one: a check that keeps the library's bodies
//! (`KARAC_CHECK_LIBRARY_BODIES=1`, what a check shared with the build
//! needs) resolves and type-checks them, then sets them aside for the passes
//! after, whose findings in the library it drops. Its report is the default
//! check's, and the bodies come back whole.

use std::process::Command;

fn check(src: &str, tag: &str, keep_bodies: bool) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-lib-bodies-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_karac"));
    cmd.arg("check").arg(&path).current_dir(&dir);
    if keep_bodies {
        cmd.env("KARAC_CHECK_LIBRARY_BODIES", "1");
    } else {
        cmd.env_remove("KARAC_CHECK_LIBRARY_BODIES");
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// Effects a library body performs beyond its signature, spawned tasks,
/// a borrow error and a mutual-recursion note: each report reads the same
/// whether the bodies were kept or not.
#[test]
fn a_check_keeping_library_bodies_reports_what_the_default_check_does() {
    let programs = [
        (
            "clean",
            "fn fib(n: i64) -> i64 { if n < 2 { n } else { fib(n - 1) + fib(n - 2) } }
fn main() {
    let mut v: Vec[i64] = Vec.new();
    for i in 0..10 { v.push(fib(i)); }
    let m: Map[String, i64] = Map.new();
    let (tx, rx) = channel[i64](4);
    tx.send(v.len());
    println(f\"{v[9]} {m.len()} {rx.recv().unwrap()}\");
}
",
        ),
        (
            "borrow",
            "fn main() {
    let mut v: Vec[i64] = Vec.new();
    v.push(1);
    let r = ref v[0];
    v.push(2);
    println(r);
}
",
        ),
        (
            "recursion",
            "fn even(n: i64) -> bool { if n == 0 { true } else { odd(n - 1) } }
fn odd(n: i64) -> bool { if n == 0 { false } else { even(n - 1) } }
fn main() { println(even(10)); }
",
        ),
    ];
    for (tag, src) in programs {
        let default = check(src, tag, false);
        let kept = check(src, tag, true);
        assert_eq!(default, kept, "{tag}: the reports differ");
    }
}

/// The bodies set aside come back as they were.
#[test]
fn library_bodies_set_aside_come_back_whole() {
    let (text, user_len) = karac::mir::lower::library_source("fn main() { println(1); }\n");
    let parsed = karac::parse(&text);
    assert!(parsed.errors.is_empty());
    let mut program = parsed.program;
    let before = format!("{:?}", program.items);
    let end = karac::mir::lower::library_sources_end(user_len);
    let aside = karac::mir::lower::take_library_bodies(&mut program, user_len..end);
    assert_ne!(
        format!("{:?}", program.items),
        before,
        "nothing was set aside"
    );
    karac::mir::lower::restore_library_bodies(&mut program, aside);
    assert_eq!(format!("{:?}", program.items), before);
}
