//! `format!` whose argument is an `if`, `match` or block expression
//! (B-2026-11-10-17): the f-string rewrite takes it into the hole, a block
//! in parentheses so its brace is not read as an escaped `{{`. An argument
//! with a `:` outside a literal, or a literal holding a brace or a colon,
//! stays unrewritten.

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
    let dir = std::env::temp_dir().join(format!("karac-format-block-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, src).unwrap();
    path
}

#[test]
fn a_format_call_with_if_match_and_block_arguments_is_rewritten_to_an_f_string() {
    let path = scratch(
        "exprs.kara",
        "enum Dir { Up, Down }
fn main() {
    let n = 3;
    let up = true;
    let d = Dir.Down;
    let s = format!(\"{}={}\", n, if up { \"up\" } else { \"down\" });
    let t = format!(\"{} {}\", match d { Dir.Up => \"u\", Dir.Down => \"d\" }, { let k = n * 2; k + 1 });
    let w = format!(\"{:>5}|\", if up { n } else { 0 });
    println(s);
    println(t);
    println(w);
}
",
    );
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{}", text(&fix));
    let src = std::fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("let s = f\"{n}={if up { \"up\" } else { \"down\" }}\";"),
        "{src}"
    );
    assert!(src.contains("{({ let k = n * 2; k + 1 })}"), "{src}");
    assert!(
        src.contains("let w = f\"{if up { n } else { 0 }:>5}|\";"),
        "{src}"
    );
    let check = karac(&["check"], &path);
    assert!(check.status.success(), "{}", text(&check));
    let run = karac(&["__mir-run"], &path);
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "3=up\nd 7\n    3|\n",
        "{}",
        text(&run)
    );
}

#[test]
fn a_format_argument_a_hole_cannot_hold_keeps_the_call_unrewritten() {
    let src = "fn main() {
    let x = true;
    let a = format!(\"{}\", if x { \"a:b\" } else { \"c\" });
    let c = format!(\"{}\", if x { \"{\" } else { \"c\" });
    println(a);
    println(c);
}
";
    let path = scratch("refused.kara", src);
    let _ = karac(&["fix"], &path);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), src);
}
