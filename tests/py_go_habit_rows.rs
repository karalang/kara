//! Rows from the Python- and Go-habit sweep of the Mend loop,
//! B-2026-11-20-70..78: habits a newcomer from Python or Go writes on day
//! one, checked and mended with `karac fix`, then run. Each test names its row.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Mended {
    first: String,
    text: String,
    clean: bool,
    out: String,
}

/// `karac check` on `src`, `karac fix` rounds until the check is clean or
/// the text stops changing, then `__mir-run` when it is clean.
fn mend(src: &str) -> Mended {
    let dir = std::env::temp_dir().join(format!(
        "karac-py-go-habit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let karac = env!("CARGO_BIN_EXE_karac");
    let check = |f: &std::path::Path| {
        let o = Command::new(karac).arg("check").arg(f).output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
        )
    };
    let (mut clean, first) = check(&f);
    for _ in 0..8 {
        if clean {
            break;
        }
        let before = std::fs::read_to_string(&f).unwrap();
        Command::new(karac).arg("fix").arg(&f).output().unwrap();
        clean = check(&f).0;
        if std::fs::read_to_string(&f).unwrap() == before {
            break;
        }
    }
    let text = std::fs::read_to_string(&f).unwrap();
    let out = if clean {
        let o = Command::new(karac)
            .arg("__mir-run")
            .arg(&f)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout).to_string()
    } else {
        String::new()
    };
    let _ = std::fs::remove_dir_all(&dir);
    Mended {
        first,
        text,
        clean,
        out,
    }
}

fn mended(src: &str, want_text: &str, want_out: &str) {
    let m = mend(src);
    assert!(m.clean, "not mended: {}\n{}", m.first, m.text);
    assert!(m.text.contains(want_text), "{}", m.text);
    assert_eq!(m.out, want_out, "{}", m.text);
}

#[test]
fn python_power_operator_is_mended_to_pow_b_2026_11_20_70() {
    let m =
        mend("fn main() { let x = 3; println(x ** 4); println(2 * x ** 2); println(2 ** 10); }\n");
    assert!(m.first.contains("power operator"), "{}", m.first);
    assert!(
        m.text.contains("x.pow(4)")
            && m.text.contains("2 * x.pow(2)")
            && m.text.contains("(2).pow(10)"),
        "{}",
        m.text
    );
    assert_eq!(m.out, "81\n18\n1024\n");
}

#[test]
fn a_python_power_with_a_computed_exponent_has_no_edit_b_2026_11_20_70() {
    let m = mend("fn main() { let x = 3; println(x ** (1 + 1)); }\n");
    assert!(!m.clean, "{}", m.text);
    assert!(m.first.contains("power operator"), "{}", m.first);
}

#[test]
fn a_constant_negative_index_is_refused_with_len_minus_b_2026_11_20_71() {
    mended(
        "struct S { v: Vec[i64] }\nfn main() { let mut s = S { v: [1, 2, 3] }; s.v[-2] = 9; let w = [4, 5]; println(s.v[1] + w[-1]); }\n",
        "s.v[s.v.len() - 2]",
        "14\n",
    );
}

#[test]
fn a_computed_negative_index_still_checks_b_2026_11_20_71() {
    let m = mend("fn main() { let v = [1, 2, 3]; let i = 0 - 1; if i >= 0 { println(v[i]); } else { println(0); } }\n");
    assert!(
        m.first.is_empty() || !m.first.contains("never negative"),
        "{}",
        m.first
    );
    assert_eq!(m.out, "0\n");
}

#[test]
fn python_one_element_append_is_mended_to_push_b_2026_11_20_72() {
    mended(
        "fn main() { let mut v: Vec[String] = []; let s = \"x\"; v.append(s); v.append(\"y\"); let mut n: Vec[i64] = []; let x = 7; n.append(x); println(v.len() + n[0]); }\n",
        "v.push(s); v.push(\"y\")",
        "9\n",
    );
    // A list argument still takes `extend`.
    mended(
        "fn main() { let mut a = [1]; let b = [2, 3]; a.append(b); println(a.len()); }\n",
        "a.extend(b)",
        "3\n",
    );
}

#[test]
fn python_elif_is_and_pass_are_mended_b_2026_11_20_73() {
    mended(
        "fn noop() { pass }\nfn main() { let r: Option[i64] = None; let x = 5; noop();\n  if x < 3 { println(\"a\"); } elif r is None { println(\"b\"); } else { println(\"c\"); }\n  if r is not None { println(\"d\"); }\n}\n",
        "} else if r == None {",
        "b\n",
    );
}

#[test]
fn pass_as_a_variable_is_left_alone_b_2026_11_20_73() {
    let m =
        mend("fn main() { let pass = 3; let n = if pass > 2 { pass } else { 0 }; println(n); }\n");
    assert!(
        m.first.contains("passed") || !m.first.contains("empty statement"),
        "{}",
        m.first
    );
    assert_eq!(m.out, "3\n");
}

#[test]
fn go_short_declarations_and_var_are_mended_b_2026_11_20_77() {
    mended(
        "fn divmod(a: i64, b: i64) -> (i64, i64) { (a / b, a % b) }\nfn main() { x := 4; q, r := divmod(7, 2); var y i64 = 3; var z: i64; z = 1; println(x + q + r + y + z); }\n",
        "let (q, r) = divmod(7, 2)",
        "12\n",
    );
}

#[test]
fn go_loops_and_increments_are_mended_b_2026_11_20_78() {
    mended(
        "fn main() { let mut i = 0; for i < 3 { i++; } for { i--; if i == 1 { break; } } println(i); }\n",
        "while i < 3 { i += 1; } loop { i -= 1;",
        "1\n",
    );
}

#[test]
fn pass_named_in_an_fstring_hole_is_left_alone_b_2026_11_20_73() {
    let m = mend("fn main() { let (user, pass) = (\"a\", \"b\"); println(f\"{user}:{pass}\"); }\n");
    assert!(!m.first.contains("empty statement"), "{}", m.first);
    assert_eq!(m.out, "a:b\n");
}
