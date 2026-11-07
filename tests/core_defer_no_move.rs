//! design.md §10: a `defer` or `errdefer` block never captures by move
//! (B-2026-11-06-28). A block that moves a binding of the enclosing scope is
//! refused with E0522 at the move; reading, borrowing, copying a `Copy`
//! value, cloning, and moving a binding the block declares itself are not
//! moves of a captured binding and still compile and run.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-defer-no-move-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

/// The `(line, column)` of every E0522 `karac check --output=json` reports.
fn e0522(tag: &str, src: &str) -> Vec<(u64, u64)> {
    let out = karac(tag, &["check", "--output=json"], src);
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{tag}: {e}"));
    json["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|d| d["code"] == "E0522")
        .map(|d| (d["line"].as_u64().unwrap(), d["column"].as_u64().unwrap()))
        .collect()
}

const N: &str = "struct N { s: String }
impl N {
    fn close(own self) { println(f\"close {self.s}\"); }
    fn peek(self) { println(self.s); }
}
fn eat(n: own N) { }
";

#[test]
fn defer_moving_an_outer_binding_is_e0522() {
    let src = format!(
        "{N}fn f() {{
    let a = N {{ s: \"a\" }};
    defer {{ eat(a); }}
    println(1);
}}
fn main() {{ f(); }}
"
    );
    assert_eq!(e0522("whole", &src), vec![(9, 17)]);
}

#[test]
fn defer_moving_into_a_let_a_field_or_an_own_self_method_is_e0522() {
    let src = format!(
        "{N}struct P {{ n: N, k: i64 }}
fn f() {{
    let a = N {{ s: \"a\" }};
    let p = P {{ n: N {{ s: \"p\" }}, k: 3 }};
    let c = N {{ s: \"c\" }};
    defer {{ let b = a; b.peek(); }}
    defer {{ eat(p.n); }}
    defer c.close();
}}
fn main() {{ f(); }}
"
    );
    assert_eq!(e0522("shapes", &src), vec![(12, 21), (13, 17), (14, 11)]);
}

#[test]
fn errdefer_moving_an_outer_binding_is_e0522() {
    let src = format!(
        "{N}fn r() -> Result[i64, String] {{
    let a = N {{ s: \"a\" }};
    errdefer a.close();
    Err(\"x\")
}}
fn main() {{ println(r().is_err()); }}
"
    );
    assert_eq!(e0522("errdefer", &src), vec![(9, 14)]);
}

/// Reads, `ref self` calls, `Copy` values, clones and the block's own
/// bindings are not moves of a captured binding: they check and run.
#[test]
fn defer_that_borrows_copies_or_moves_its_own_binding_still_runs() {
    let src = format!(
        "#[derive(Clone)]
{N}fn f() {{
    let a = N {{ s: \"a\" }};
    let k = 4;
    let v: Vec[i64] = [1, 2];
    defer {{ a.peek(); println(f\"{{v.len()}}\"); }}
    defer {{ let z = k; println(z); }}
    defer {{ eat(a.clone()); let mine = N {{ s: \"m\" }}; mine.close(); }}
    println(0);
}}
fn main() {{ f(); }}
"
    );
    assert_eq!(e0522("allowed", &src), vec![]);
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac("allowed-run", &[b], &src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "0\nclose m\n4\na\n2\n",
            "{b}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
