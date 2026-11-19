//! B-2026-11-19-60: a unit struct is built and matched with empty braces
//! (design.md § Unit struct), so its bare name is refused with the braced
//! form as a machine fix. With `std.time.TimedOut` imported, `Err(TimedOut)`
//! was read as the enum variant `RecvTimeoutError.TimedOut` (E0521, E0279)
//! and as a pattern was "not an enum with a `TimedOut` variant" (E0273),
//! neither saying `TimedOut {}`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Runs `karac <args> main.kara` on `src`; returns stderr and the file after.
fn karac(src: &str, args: &[&str]) -> (String, String) {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-unitbare-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("main.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("main.kara")
        .current_dir(&dir)
        .output()
        .unwrap();
    let after = std::fs::read_to_string(dir.join("main.kara")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (String::from_utf8_lossy(&out.stderr).into_owned(), after)
}

fn error_codes(err: &str) -> Vec<String> {
    err.lines()
        .filter_map(|l| l.strip_prefix("error["))
        .filter_map(|l| l.split_once(']').map(|(c, _)| c.to_string()))
        .collect()
}

const TIMED_OUT: &str = "import std.time.{timeout, TimedOut, Duration};

fn main() {
    let r = timeout(Duration.ms(500), || 42);
    match r {
        Ok(v) => println(v),
        Err(TimedOut) => println(\"late\"),
    }
    let e: Result[i64, TimedOut] = Err(TimedOut);
    println(e.is_err());
}
";

#[test]
fn unit_struct_bare_imported_timed_out_names_the_struct_in_both_positions() {
    let (err, _) = karac(TIMED_OUT, &["check"]);
    assert_eq!(error_codes(&err), ["E0206", "E0206"], "{err}");
    assert!(
        err.contains("`TimedOut` here is the unit struct `TimedOut`, which is matched with empty braces: write `TimedOut {}`"),
        "{err}"
    );
    assert!(
        err.contains("`TimedOut` here is the unit struct `TimedOut`, which is built with empty braces: write `TimedOut {}`"),
        "{err}"
    );
    for gone in [
        "RecvTimeoutError",
        "ambiguous variant",
        "enum-variant pattern",
    ] {
        assert!(!err.contains(gone), "{gone} in {err}");
    }
}

#[test]
fn unit_struct_bare_fix_writes_the_braces_and_the_program_checks() {
    let (_, fixed) = karac(TIMED_OUT, &["fix"]);
    assert!(
        fixed.contains("Err(TimedOut {}) => println(\"late\")"),
        "{fixed}"
    );
    assert!(fixed.contains("= Err(TimedOut {});"), "{fixed}");
    let (err, _) = karac(&fixed, &["check"]);
    assert!(error_codes(&err).is_empty(), "{err}");
}

#[test]
fn unit_struct_bare_user_struct_is_one_error_per_use() {
    let src = "struct Unit {}

fn main() {
    let u = Unit;
    let r: Result[i64, Unit] = Err(Unit);
    match r {
        Ok(v) => println(v),
        Err(Unit) => println(\"unit\"),
    }
}
";
    let (err, _) = karac(src, &["check"]);
    assert_eq!(error_codes(&err), ["E0206", "E0206", "E0206"], "{err}");
}

#[test]
fn unit_struct_bare_name_a_local_or_variant_still_wins() {
    // A local of the name is a value; an enum that declares the name as a
    // variant still matches it as its own variant.
    let src = "struct Closed {}

enum Door {
    Open,
    Closed,
}

fn main() {
    let d = Door.Closed;
    match d {
        Door.Open => println(\"open\"),
        Door.Closed => println(\"closed\"),
    }
    let c = Closed {};
    match c {
        Closed {} => println(\"unit\"),
    }
}
";
    let (err, _) = karac(src, &["check"]);
    assert!(error_codes(&err).is_empty(), "{err}");
}
