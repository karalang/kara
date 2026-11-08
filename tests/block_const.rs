//! A block may declare a constant (design.md § Constants, B-2026-11-08-60):
//! `const MOD: i64 = 1_000_000_007;` as the first line of a function body,
//! the way a LeetCode Rust solution writes it. Its scope is the rest of the
//! block; it shadows like a `let`; its initializer names constants only.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `karac check`'s success and diagnostics, `karac fmt`'s text, and what the
/// MIR interpreter prints.
fn try_program(tag: &str, src: &str) -> (bool, String, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-block-const-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let (clean, _, diags) = karac(&dir, &["check", "cell.kara"]);
    let (_, formatted, _) = karac(&dir, &["fmt", "cell.kara"]);
    let (_, out, _) = karac(&dir, &["__mir-run", "cell.kara"]);
    let _ = std::fs::remove_dir_all(&dir);
    (clean, diags, formatted, out)
}

#[test]
fn block_const_scopes_shadows_and_reaches_closures() {
    let src = r#"const MOD: i64 = 5;

fn twice() -> i64 {
    MOD * 2
}

fn main() {
    println(f"before {MOD}");
    const MOD: i64 = 1000000007;
    const BIG: i64 = MOD * 2 + 1;
    println(f"inside {MOD} {BIG}");
    {
        println(f"nested before {MOD}");
        const MOD: i64 = MOD + 1;
        println(f"nested after {MOD}");
        const SEVEN: i64 = 7;
        match 7 {
            SEVEN => println(f"matched {SEVEN}"),
            _ => println("no match"),
        }
    }
    let f = |x: i64| x % MOD;
    println(f"after {MOD}, closure {f(1000000010)}, other fn {twice()}");
}
"#;
    let (clean, diags, formatted, out) = try_program("scope", src);
    assert!(clean, "{diags}");
    assert_eq!(
        out,
        "before 5\ninside 1000000007 2000000015\nnested before 1000000007\n\
         nested after 1000000008\nmatched 7\nafter 1000000007, closure 3, other fn 10\n"
    );
    assert_eq!(formatted, src, "fmt must write a block's `const` back");
}

#[test]
fn block_const_in_a_method_is_the_leetcode_shape() {
    let src = r#"struct Solution {}

impl Solution {
    pub fn num_of_subarrays(arr: Vec[i32]) -> i32 {
        const MOD: i64 = 1_000_000_007;
        let mut odd = 0;
        let mut even = 1;
        let mut sum = 0;
        let mut ans: i64 = 0;
        for x in arr {
            sum += x;
            if sum % 2 == 1 {
                ans += even;
                odd += 1;
            } else {
                ans += odd;
                even += 1;
            }
        }
        (ans % MOD) as i32
    }
}

fn main() {
    println(f"{Solution.num_of_subarrays([1, 3, 5])}");
}
"#;
    let (clean, diags, _, out) = try_program("method", src);
    assert!(clean, "{diags}");
    assert_eq!(out, "4\n");
}

#[test]
fn block_const_initializer_naming_a_local_is_refused() {
    let src = "fn main() {\n    let x = 3;\n    const LIMIT: i64 = x * 2;\n    println(f\"{LIMIT}\");\n}\n";
    let (clean, diags, _, _) = try_program("local", src);
    assert!(!clean);
    assert!(
        diags.contains("the constant `LIMIT` cannot use `x`")
            && diags.contains("no local binding")
            && !diags.contains("r#const"),
        "{diags}"
    );
}

#[test]
fn block_const_diagnostic_names_it_as_written() {
    let src = "fn main() {\n    const NAME: String = \"x\";\n    println(f\"{NAME}\");\n}\n";
    let (clean, diags, _, _) = try_program("heap", src);
    assert!(!clean);
    assert!(
        diags.contains("constant 'NAME' is declared with type 'String'")
            && !diags.contains("__BLOCK"),
        "{diags}"
    );
}

#[test]
fn block_const_is_not_visible_outside_its_block() {
    let src = "fn main() {\n    {\n        const LIMIT: i64 = 3;\n        println(f\"{LIMIT}\");\n    }\n    println(f\"{LIMIT}\");\n}\n";
    let (clean, diags, _, _) = try_program("outside", src);
    assert!(!clean);
    assert!(
        diags.contains("LIMIT") && !diags.contains("__BLOCK"),
        "{diags}"
    );
}
