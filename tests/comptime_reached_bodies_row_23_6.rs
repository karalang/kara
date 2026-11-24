//! B-2026-11-23-6: `karac check` of a three-line file with one
//! `#[derive(Serialize)]` spent 216 of its 432 ms evaluating the derive,
//! and most of that type-checking all ~800 library bodies set aside for the
//! evaluation to run the 33 its root reaches. The evaluation now gets back
//! only the bodies its roots can reach by name, a library type's methods
//! once that type is named, and puts back any other body the builder
//! lowers for another round. `KARAC_COMPTIME_ALL_BODIES=1` puts back every
//! body, as before, and is the control arm here: the answers must agree.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const DERIVE: &str = r#"import std.json.{Json};
#[derive(Serialize)]
struct Tag { name: String, weight: i64 }
fn main() {
    println(Json.serialize(Tag { name: "a", weight: 3 }));
}
"#;

/// A comptime body that reaches the library through a `for` loop and a
/// range's `collect`, names it never writes as the methods it calls.
const LOOP: &str = r#"comptime fn total(n: i64) -> i64 {
    let v: Vec[i64] = (1..=n).collect();
    let mut s = 0;
    for x in v {
        s += x;
    }
    s
}
fn main() { println(comptime { total(4) }); }
"#;

fn cell(src: &str) -> PathBuf {
    static CALL: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "karac-reached-bodies-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("cell.kara");
    std::fs::write(&file, src).unwrap();
    file
}

/// What `karac <cmd>` printed, its success, and the first evaluation's
/// restored and set-aside body counts.
fn karac(cmd: &str, file: &PathBuf, all: bool) -> (String, bool, Option<(usize, usize)>) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_karac"));
    c.arg(cmd).arg(file).env("KARAC_MIR_TIME", "1");
    if all {
        c.env("KARAC_COMPTIME_ALL_BODIES", "1");
    } else {
        c.env_remove("KARAC_COMPTIME_ALL_BODIES");
    }
    let out = c.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let counts = stderr.lines().find_map(|l| {
        let rest = l.strip_prefix("[mir-time] evaluation restored ")?;
        let mut w = rest.split_whitespace();
        let kept = w.next()?.parse().ok()?;
        let all = w.nth(1)?.parse().ok()?;
        Some((kept, all))
    });
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
        counts,
    )
}

#[test]
fn a_derive_evaluation_restores_only_the_bodies_it_reaches() {
    let file = cell(DERIVE);
    let (_, ok, counts) = karac("check", &file, false);
    assert!(ok, "the check passes");
    let (kept, all) = counts.expect("the evaluation reports its counts");
    assert!(all > 400, "the library's bodies were set aside: {all}");
    assert!(
        kept * 4 < all,
        "the derive restores a fraction of the library: {kept} of {all}"
    );
    let (_, ok, counts) = karac("check", &file, true);
    assert!(ok, "the control check passes");
    let (kept, all) = counts.unwrap();
    assert_eq!(kept, all, "the control restores every body");
}

#[test]
fn a_derive_runs_the_same_with_fewer_bodies() {
    let file = cell(DERIVE);
    let (out, ok, _) = karac("run", &file, false);
    let (control, control_ok, _) = karac("run", &file, true);
    assert!(ok && control_ok);
    assert_eq!(out, "{\"name\":\"a\",\"weight\":3}\n");
    assert_eq!(out, control);
}

#[test]
fn a_comptime_loop_reaches_the_library_without_naming_it() {
    let file = cell(LOOP);
    let (out, ok, _) = karac("run", &file, false);
    let (control, control_ok, _) = karac("run", &file, true);
    assert!(ok && control_ok);
    assert_eq!(out, "10\n");
    assert_eq!(out, control);
}
