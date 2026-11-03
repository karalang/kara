//! `ref place` builds a view (design.md § `ref place` borrows a place), and
//! the spec's own example builds an `Option[ref T]` with it:
//! `let first = Some(ref self.items[0]);`. The parser rejected every `ref`
//! in an argument list as a call-site mode marker and offered a fix that
//! deleted it, turning the borrow into a move (B-2026-11-03-19). A
//! constructor (a callee named in Type case: `Some`, `Ok`, an enum variant)
//! has no parameter modes, so there the borrow stands. A call to a function
//! still refuses `ref` and `mut ref`.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-ctorref-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const VIEWS: &str = "struct Bag {
    items: Vec[String],
}

impl Bag {
    fn first(self) -> Option[ref String] {
        let first = Some(ref self.items[0]);
        first
    }
}

enum Pick {
    One(ref String),
    Nothing,
}

fn longest(among: Option[ref Vec[String]]) -> i64 {
    match among {
        Some(v) => v.len(),
        None => 0,
    }
}

fn main() {
    let b = Bag { items: vec![\"ab\".to_string(), \"c\".to_string()] };
    println(b.first() ?? \"none\".to_string());
    let g = vec![\"x\".to_string(), \"y\".to_string()];
    println(f\"{longest(Some(ref g))} {g.len()}\");
    let p = Pick.One(ref g[1]);
    match p {
        Pick.One(s) => println(s),
        Pick.Nothing => println(\"-\"),
    }
}
";

#[test]
fn ref_place_in_a_constructor_argument_builds_a_view() {
    let (ok, err) = check("views", VIEWS);
    assert!(ok, "{err}");
    assert_eq!(
        karac::mir::lower::run_source(VIEWS)
            .expect("MIR interpreter")
            .output,
        "ab\n2 2\ny\n"
    );
}

#[test]
fn ref_in_a_function_call_is_still_refused() {
    for (name, call) in [("ref", "show(ref s)"), ("mut_ref", "grow(mut ref s)")] {
        let src = format!(
            "fn show(s: String) {{ println(s); }}
fn grow(s: mut ref String) {{ s.push_str(\"!\"); }}
fn main() {{
    let mut s = \"a\".to_string();
    {call};
    println(s);
}}
"
        );
        let (ok, err) = check(name, &src);
        assert!(!ok, "{name}: `{call}` must be refused");
        assert!(err.contains("not written at call sites"), "{name}: {err}");
    }
}
