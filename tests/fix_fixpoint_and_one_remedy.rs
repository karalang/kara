//! `karac fix` runs until a pass applies nothing (B-2026-11-15-131), keeps one
//! remedy where an ownership error and a type error answer the same move
//! (B-2026-11-15-132), and offers `.clone()` for a borrowed branch tail of a
//! match passed as an argument (B-2026-11-15-133). With them, the two
//! probe rows on derived `Debug` under `Display(snake_case)` and a receiver's
//! name shadowed inside its own argument (B-2026-11-16-1, B-2026-11-16-2).

use std::process::{Command, Output};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .output()
        .unwrap()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn scratch(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-fixpoint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

fn checks_and_prints(path: &std::path::Path, want: &str) {
    let check = karac(&["check"], path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
}

#[test]
fn one_fix_invocation_reaches_the_fixpoint() {
    // `s = s + i` becomes `s += i`, and only then is the counted `while` a
    // `for` over a range: two passes, one invocation. Both style lints are
    // off unless asked for, in `fix` as in `check` (B-2026-11-20-68).
    let path = scratch(
        "fixpoint.kara",
        "fn main() {
    let mut s = 0;
    let mut i = 0;
    while i < 3 {
        s = s + i;
        i = i + 1;
    }
    println(s);
}
",
    );
    let fix = karac(
        &[
            "fix",
            "-W",
            "manual_compound_assign",
            "-W",
            "counter_while_loop",
        ],
        &path,
    );
    let out = text(&fix);
    assert!(fix.status.success(), "{out}");
    assert_eq!(out.matches("applied").count(), 1, "{out}");
    let again = karac(
        &[
            "fix",
            "-W",
            "manual_compound_assign",
            "-W",
            "counter_while_loop",
        ],
        &path,
    );
    assert!(text(&again).contains("no fixable"), "{}", text(&again));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(src.contains("for i in 0..3"), "{src}");
    checks_and_prints(&path, "3\n");
}

#[test]
fn a_dry_run_is_still_one_pass() {
    let path = scratch(
        "fixpoint_dry.kara",
        "fn main() {
    let mut s = 0;
    let mut i = 0;
    while i < 3 {
        s = s + i;
        i = i + 1;
    }
    println(s);
}
",
    );
    let fix = karac(
        &[
            "fix",
            "--dry-run",
            "-W",
            "manual_compound_assign",
            "-W",
            "counter_while_loop",
        ],
        &path,
    );
    assert!(
        text(&fix).contains("would apply 2 fix(es)"),
        "{}",
        text(&fix)
    );
}

#[test]
fn a_move_out_through_a_shared_struct_gets_clone_alone() {
    let src = "shared struct Inner { word: String }
struct Holder { inner: Inner }
fn main() {
    let mut hs: Vec[Holder] = Vec.new();
    hs.push(Holder { inner: Inner { word: \"c\".to_string() } });
    let hop = hs[0].inner.word;
    println(hop);
}
";
    let path = scratch("shared_projection.kara", src);
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("let hop = hs[0].inner.word.clone();"),
        "{fixed}"
    );
    checks_and_prints(&path, "c\n");

    // With a plain struct the place can be borrowed, and the fix still does.
    let path = scratch(
        "plain_projection.kara",
        &src.replace("shared struct", "struct"),
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("let hop = ref hs[0].inner.word;"), "{fixed}");
    checks_and_prints(&path, "c\n");
}

#[test]
fn a_borrowed_tail_of_a_match_argument_gets_the_clone_fix() {
    let path = scratch(
        "match_arg_tail.kara",
        "enum Ve { A(String), B }
impl Drop for Ve { fn drop(mut ref self) { println(\"d\") } }
fn mk() -> Ve { Ve.A(\"p\".to_string()) }
fn sink(s: String) -> i64 { s.len() }
fn main() {
    println(sink(match mk() { Ve.A(ref s) => { s } Ve.B => { \"none\" } }));
}
",
    );
    let check = karac(&["check"], &path);
    let out = text(&check);
    assert!(out.contains("borrow_projection_copy"), "{out}");
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(fixed.contains("Ve.A(ref s) => { s.clone() }"), "{fixed}");
    checks_and_prints(&path, "d\n1\n");
}

#[test]
fn derived_debug_names_the_variant_under_snake_case_display() {
    let path = scratch(
        "debug_snake.kara",
        "#[derive(Debug, Display(snake_case))]
enum Dir { Up, BigDown }
#[derive(Debug)]
enum Plain { BigDown }
fn main() {
    println(f\"{Dir.Up} {Dir.BigDown}\");
    println(Dir.Up.fmt_debug());
    println(Dir.BigDown.fmt_debug());
    println(Plain.BigDown.fmt_debug());
}
",
    );
    let want = "up big_down\nUp\nBigDown\nBigDown\n";
    checks_and_prints(&path, want);
    if cfg!(feature = "llvm") {
        let run = karac(&["__mir-llvm-run"], &path);
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{}", text(&run));
    }
}

#[test]
fn a_receiver_name_shadowed_inside_its_argument_is_accepted() {
    let path = scratch(
        "shadow_receiver.kara",
        "fn main() {
    let mut m: Vec[Vec[i64]] = Vec.new();
    m.push({ let mut m: Vec[i64] = Vec.new(); m.push(1); m });
    m.push({ let m: Vec[i64] = [2, 3]; m });
    println(f\"{m}\");
}
",
    );
    checks_and_prints(&path, "[[1], [2, 3]]\n");
}

#[test]
fn the_outer_receiver_used_inside_its_argument_is_still_refused() {
    let path = scratch(
        "shadow_receiver_use.kara",
        "fn main() {
    let mut m: Vec[Vec[i64]] = Vec.new();
    m.push({ let k = m; Vec.new() });
    println(f\"{m}\");
}
",
    );
    let check = karac(&["check"], &path);
    assert!(!check.status.success(), "{}", text(&check));
}
