//! Kata 1209's two Rust arms, stacks of runs (B-2026-11-18-45).
//!
//! `match stack.last_mut() { Some(top) if top.0 == c => { top.1 += 1; if
//! top.1 == k { stack.pop(); } } _ => stack.push((c, 1)) }` names the
//! vector in the arm, after the element's last use, as Rust's borrow
//! checker allows; and `if let Some((c, n)) = st.last_mut()` binds the
//! element's fields. Both rewrite through an index. A `usize` count given
//! to `String.repeat` fixes the program's written `usize`s to `i64`.

use std::process::{Command, Output, Stdio};

fn karac(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .args(args)
        .arg("m.kara")
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

/// `src` after `karac fix` until nothing is left to fix, its `check`
/// stderr, and its output on the interpreter.
fn fixed(tag: &str, src: &str) -> (String, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-kata-1209-last-mut-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("m.kara"), src).unwrap();
    for _ in 0..4 {
        karac(&dir, &["fix"]);
    }
    let text = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    let check = karac(&dir, &["check"]);
    let run = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    (
        text,
        String::from_utf8_lossy(&check.stderr).into_owned(),
        String::from_utf8_lossy(&run.stdout).into_owned(),
    )
}

fn clean(check: &str) -> bool {
    check.contains("All checks passed") || check.is_empty()
}

#[test]
fn a_last_mut_arm_that_pops_after_the_elements_last_use_is_rewritten() {
    let src = "fn squash(s: String, k: i64) -> String {
    let k = k as usize;
    let mut stack: Vec[(char, usize)] = Vec.new();
    for c in s.chars() {
        match stack.last_mut() {
            Some(top) if top.0 == c => {
                top.1 += 1;
                if top.1 == k {
                    stack.pop();
                }
            }
            _ => stack.push((c, 1)),
        }
    }
    let mut out = String.new();
    for (c, n) in stack.iter() {
        out.push_str(c.to_string().repeat(n));
    }
    out
}

fn main() {
    println(squash(\"deeedbbcccbdaa\", 3));
    println(squash(\"pbbcggttciiippooaais\", 2));
}
";
    let (text, check, out) = fixed("pop", src);
    assert!(
        text.contains(
            "        if stack.len() > 0 and stack[stack.len() - 1].0 == c {
            stack[stack.len() - 1].1 += 1;
            if stack[stack.len() - 1].1 == k {
                stack.pop();
            }
        } else { stack.push((c, 1)); }"
        ) && !text.contains("let k = k")
            && text.contains("Vec[(char, i64)]"),
        "{text}"
    );
    assert!(clean(&check), "{check}");
    assert_eq!(out, "aa\nps\n");
}

#[test]
fn a_last_mut_if_let_over_a_pair_binds_the_fields_through_the_index() {
    let src = "fn squash(s: String, k: i32) -> String {
    let mut st: Vec[(u8, i32)] = Vec.new();
    for b in s.bytes().iter() {
        if let Some((c, n)) = st.last_mut() {
            if *c == b {
                *n += 1;
                if *n == k {
                    st.pop();
                }
                continue;
            }
        }
        st.push((b, 1));
    }
    let mut out = String.new();
    for (c, n) in st.iter() {
        for _ in 0..n {
            out.push(c as char);
        }
    }
    out
}

fn main() {
    println(squash(\"deeedbbcccbdaa\", 3));
    println(squash(\"yfttttfbbbbnnnnffbgffffgbbbbgssssgthyyyy\", 4));
}
";
    let (text, check, out) = fixed("pair", src);
    assert!(
        text.contains(
            "        if st.len() > 0 {
            if st[st.len() - 1].0 == b {
                st[st.len() - 1].1 += 1;
                if st[st.len() - 1].1 == k {
                    st.pop();
                }
                continue;
            }
        }"
        ),
        "{text}"
    );
    assert!(clean(&check), "{check}");
    assert_eq!(out, "aa\nybth\n");
}

#[test]
fn a_last_mut_arm_that_uses_the_element_after_naming_the_vector_keeps_the_message() {
    // Rust refuses both: the element is used after the vector is, once
    // in order and once by a loop's next round.
    for (tag, arm) in [
        ("after", "Some(x) => { v.push(0); *x += 1; }"),
        ("loop", "Some(x) => { for _ in 0..2 { *x += 1; v.pop(); } }"),
    ] {
        let src = format!(
            "fn main() {{
    let mut v: Vec[i64] = [1, 2];
    match v.last_mut() {{
        {arm}
        _ => {{}}
    }}
    println(f\"{{v}}\");
}}
"
        );
        let (text, check, _) = fixed(tag, &src);
        assert!(text.contains("v.last_mut()"), "{tag}: {text}");
        assert!(
            check.contains("after checking the vector is not empty"),
            "{tag}: {check}"
        );
    }
}
