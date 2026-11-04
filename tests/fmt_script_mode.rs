//! `karac fmt` keeps a script-mode file a script (design.md § Script mode;
//! review-thread ruling 2026-11-04): its top-level statements stay at the
//! top level, in file order among the items, and the implicit
//! `fn main() -> Result[(), AnyError]` and its `Ok(())` are never written.
//! It wrote them, the statements moved after every item (B-2026-11-04-36).

const SCRIPT: &str = "// a script that propagates with `?`
fn h(n: i64) -> Result[i64, AnyError] { Ok(n * 2) }
let x = h(1)?;   // the first
println(x);

// an item between statements
struct P { a: i64 }
let p = P { a: h(x)? };
if p.a > 1 { println(\"big\"); }
println(p.a);
";

const FORMATTED: &str = "// a script that propagates with `?`
fn h(n: i64) -> Result[i64, AnyError] {
    Ok(n * 2)
}

let x = h(1)?; // the first
println(x);

// an item between statements
struct P {
    a: i64,
}

let p = P {
    a: h(x)?,
};
if p.a > 1 {
    println(\"big\");
}
println(p.a);
";

#[test]
fn a_script_formats_as_a_script() {
    let once = karac::format_source(SCRIPT).expect("the script parses");
    assert_eq!(once, FORMATTED);
    assert!(
        !once.contains("fn main") && !once.contains("Ok(())"),
        "{once}"
    );
    assert_eq!(
        karac::format_source(&once).as_deref(),
        Some(FORMATTED),
        "a fixed point"
    );
}

#[test]
fn a_formatted_script_runs_the_same() {
    let dir = std::env::temp_dir().join(format!("karac-fmt-script-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |name: &str, text: &str| {
        let f = dir.join(name);
        std::fs::write(&f, text).unwrap();
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["run", "--interp"])
            .arg(&f)
            .output()
            .unwrap();
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    };
    let before = run("before.kara", SCRIPT);
    let after = run("after.kara", FORMATTED);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(before, (Some(0), "2\nbig\n4\n".to_string()));
    assert_eq!(after, before);
}
