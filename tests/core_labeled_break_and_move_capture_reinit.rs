//! Two move-check gaps from the probe's drop fuzzer (probe-core/dropx).
//!
//! - B-2026-11-15-94: the code after a labeled block is reached from each
//!   `break 'b` as well as from the block's end, so a binding moved before a
//!   break is moved there (core-semantics §3.4). `karac check` passed
//!   `eat(d1); if k == 5 { break 'b 50; } d1 = D { n: 2 }; .. eat(d1)`, and
//!   the MIR move check refused it at run. The move analysis's control-flow
//!   graph skipped labeled blocks entirely.
//! - B-2026-11-15-95: a closure that consumes a capture moves it in when it
//!   is made (§9.3), so reassigning the binding afterwards gives it a new
//!   value (§3.4). The check refused `let c = || { eat(a); }; a = D { n: 2 };
//!   eat(a); c();`, and named the two lines in reverse order.

use std::process::Command;

const HEAD: &str = "struct D { n: i64 }
impl Drop for D { fn drop(mut ref self) { println(f\"d{self.n}\"); } }
fn eat(x: own D) { println(f\"eat{x.n}\"); }
";

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-dx-{}-{name}", std::process::id()));
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
        String::from_utf8_lossy(&out.stderr).into_owned() + &String::from_utf8_lossy(&out.stdout),
    )
}

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

#[test]
fn dx2_a_move_before_a_labeled_break_reaches_the_code_after_the_block() {
    let src = format!(
        "{HEAD}fn run(k: i64) {{
    let mut d1 = D {{ n: 1 }};
    let v = 'b: {{
        eat(d1);
        if k == 5 {{
            break 'b 50;
        }}
        d1 = D {{ n: 2 }};
        45
    }};
    println(f\"{{v}}\");
    eat(d1);
}}
fn main() {{ run(0); run(5); }}
"
    );
    let (ok, err) = check("dx2-value", &src);
    assert!(!ok, "accepted a use after a move carried by `break 'b`");
    assert!(err.contains("'d1'") && err.contains("15:9"), "{err}");

    let src = format!(
        "{HEAD}fn run(k: i64) {{
    let d1 = D {{ n: 1 }};
    'b: {{
        if k == 5 {{
            eat(d1);
            break 'b;
        }}
        println(\"x\");
    }};
    eat(d1);
}}
fn main() {{ run(0); run(5); }}
"
    );
    let (ok, err) = check("dx2-stmt", &src);
    assert!(!ok, "accepted a use after a move carried by `break 'b`");
    assert!(err.contains("'d1'"), "{err}");
}

#[test]
fn dx2_a_labeled_block_whose_breaks_reassign_first_is_accepted() {
    let src = format!(
        "{HEAD}fn run(k: i64) {{
    let mut d1 = D {{ n: 1 }};
    let v = 'b: {{
        eat(d1);
        if k == 5 {{
            d1 = D {{ n: 3 }};
            break 'b 50;
        }}
        d1 = D {{ n: 2 }};
        45
    }};
    println(f\"{{v}}\");
    eat(d1);
    let w = 'c: {{
        let s = D {{ n: 7 }};
        if k == 5 {{ break 'c s; }}
        s
    }};
    eat(w);
}}
fn main() {{ run(0); run(5); }}
"
    );
    let (ok, err) = check("dx2-ok", &src);
    assert!(ok, "{err}");
    assert_eq!(
        run(&src),
        "eat1\nd1\n45\neat2\nd2\neat7\nd7\neat1\nd1\n50\neat3\nd3\neat7\nd7\n"
    );
}

#[test]
fn dx3_reassigning_after_a_move_capture_gives_a_new_value() {
    let src = format!(
        "{HEAD}fn main() {{
    let mut a = D {{ n: 1 }};
    let c = || {{ eat(a); }};
    a = D {{ n: 2 }};
    eat(a);
    c();
}}
"
    );
    let (ok, err) = check("dx3-ok", &src);
    assert!(ok, "{err}");
    assert_eq!(run(&src), "eat2\nd2\neat1\nd1\n");
}

#[test]
fn dx3_a_use_after_a_move_capture_is_still_refused_in_source_order() {
    for (name, body) in [
        (
            "no-reassign",
            "let c = || { eat(a); };\n    eat(a);\n    c();",
        ),
        (
            "reassign-after-use",
            "let c = || { eat(a); };\n    eat(a);\n    a = D { n: 2 };\n    c();",
        ),
        (
            "moved-first",
            "eat(a);\n    let c = || { eat(a); };\n    c();",
        ),
    ] {
        let src = format!("{HEAD}fn main() {{\n    let mut a = D {{ n: 1 }};\n    {body}\n}}\n");
        let (ok, err) = check(name, &src);
        assert!(!ok, "{name}: accepted");
        assert!(err.contains("'a'"), "{name}: {err}");
        // The move is named first, at the earlier line.
        assert!(
            err.contains("moved at line 6:") || err.contains("(moved at line 6:"),
            "{name}: {err}"
        );
    }
}
