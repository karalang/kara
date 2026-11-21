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

#[test]
fn go_params_fields_and_result_lists_are_mended_b_2026_11_20_77() {
    mended(
        "struct P { x i64; y i64 }\nfn divmod(a i64, b i64) (i64, i64) { return a / b, a % b; }\nfn main() { let p = P { x: 1, y: 2 }; let (q, r) = divmod(7, 2); println(p.x + p.y + q + r); }\n",
        "fn divmod(a: i64, b: i64) -> (i64, i64) { return (a / b, a % b); }",
        "7\n",
    );
}

#[test]
fn a_match_arm_return_keeps_its_comma_b_2026_11_20_77() {
    mended(
        "fn f(o: Option[i64]) -> i64 { match o { Some(x) => return x, None => return 0, } }\nfn main() { println(f(Some(4))); }\n",
        "Some(x) => return x, None => return 0,",
        "4\n",
    );
}

#[test]
fn python_hash_comments_and_lambda_are_mended_b_2026_11_20_73() {
    mended(
        "fn main() {\n    # a comment\n    let f = lambda x: x + 1;\n    let g = lambda a, b: a * b;\n    println(f(2) + g(2, 3));\n}\n",
        "let g = |a, b| a * b;",
        "9\n",
    );
}

#[test]
fn go_c_style_for_is_mended_to_a_range_b_2026_11_20_78() {
    mended(
        "fn main() { let mut s = 0; for i := 0; i < 5; i++ { s += i; } for let mut j = 1; j <= 4; j += 1 { s += j; } println(s); }\n",
        "for i in 0..5 { s += i; } for j in 1..=4 {",
        "20\n",
    );
}

#[test]
fn a_c_style_for_whose_body_writes_its_counter_or_bound_is_declined_b_2026_11_20_78() {
    for src in [
        "fn main() { let mut s = 0; for i := 0; i < 10; i++ { if i == 2 { i += 3; } s += 1; } println(s); }\n",
        "fn main() { let mut v = [1]; for i := 0; i < v.len(); i++ { if v.len() < 3 { v.push(i); } } println(v.len()); }\n",
    ] {
        let m = mend(src);
        assert!(!m.clean, "{}", m.text);
        assert!(m.first.contains("three-part"), "{}", m.first);
        assert!(m.text.contains("for i := 0;"), "{}", m.text);
    }
}

#[test]
fn go_range_loops_are_mended_b_2026_11_20_78() {
    mended(
        "fn main() { let v = [5, 6]; let mut s = 0; for i, x := range v { s += i * x; } for _, x := range v { s += x; } for i := range v { s += i; } for x in range v { s += x; } println(s); }\n",
        "for (i, x) in v.iter().enumerate()",
        "29\n",
    );
}

#[test]
fn go_switch_is_mended_to_match_b_2026_11_20_78() {
    mended(
        "fn main() { let x = 3; switch x { case 1, 3: println(\"odd\"); case 2: println(\"two\"); } let s = \"b\"; switch s { case \"a\": println(1); default: println(2); } }\n",
        "match x { 1 | 3 => {",
        "odd\n2\n",
    );
    // A case that is not a literal has no `match` pattern spelling.
    let m = mend("fn main() { let y = 2; let x = 2; switch x { case y: println(\"y\"); } }\n");
    assert!(
        !m.clean && m.first.contains("`switch` is Go's"),
        "{}",
        m.first
    );
}

#[test]
fn python_slices_and_comprehensions_are_mended_b_2026_11_20_75() {
    mended(
        "fn main() { let v = [1, 2, 3, 4]; let s = \"kara.txt\"; let w = [x * x for x in v if x % 2 == 0]; println(v[1:3].len() + w[1]); println(s[:-4]); }\n",
        "s[..s.len() - 4]",
        "18\nkara\n",
    );
}

#[test]
fn python_builtins_are_mended_to_methods_b_2026_11_20_76() {
    mended(
        "fn main() { let v = [1, 2, 3]; let a = [1, 2]; let b = [3, 4]; let mut s = sum(v) + len(v); for (x, y) in zip(a, b) { s += x * y; } for (i, x) in enumerate(v) { s += i * x; } println(f\"{s} {str(7).len()}\"); }\n",
        "v.iter().sum()",
        "28 1\n",
    );
}

#[test]
fn python_string_methods_are_renamed_b_2026_11_20_76() {
    mended(
        "fn main() { let s = \"  Hi \"; println(s.strip().upper()); if \"kara\".endswith(\"ra\") { let v = [\"a\", \"b\"]; println(\", \".join(v)); } }\n",
        "s.trim().to_uppercase()",
        "HI\na, b\n",
    );
}

#[test]
fn python_class_and_type_names_are_mended_b_2026_11_20_74() {
    mended(
        "class Counter { n: int }\nfn total(v: list[int]) -> int { let mut s = 0; for x in v { s += x; } s }\nfn main() { let c = Counter { n: 2 }; println(c.n + total([1, 2])); }\n",
        "struct Counter { n: i64 }",
        "5\n",
    );
}

#[test]
fn python_raise_try_and_with_are_told_without_an_edit_b_2026_11_20_74() {
    for (src, word) in [
        ("fn main() { raise \"bad\"; println(1); }\n", "`raise`"),
        (
            "fn main() { try { println(1); } except { println(2); } }\n",
            "`try`/`except`",
        ),
        (
            "fn main() { with open(\"p\") as f { println(1); } }\n",
            "`with`",
        ),
    ] {
        let m = mend(src);
        assert!(!m.clean, "{}", m.text);
        assert!(m.first.contains(word), "{}", m.first);
        assert_eq!(m.text, src);
    }
}

#[test]
fn soft_words_with_and_raise_stay_names_b_2026_11_20_74() {
    for src in [
        "fn f(with: i64) -> i64 { with * 2 }\nfn main() { println(f(2)); }\n",
        "fn main() { let raise = 3; let n = raise + 1; println(n); }\n",
        "fn main() { let mut with = 1; with += 1; println(with); }\n",
    ] {
        let m = mend(src);
        assert!(m.clean, "{src}\n{}", m.first);
        assert_eq!(m.text, src);
    }
}

#[test]
fn a_par_branch_return_keeps_its_comma_b_2026_11_20_77() {
    let src = "fn main() {\n    let (a, b) = par { return 5, 3 };\n    println(a + b);\n}\n";
    let m = mend(src);
    assert_eq!(m.text, src);
    assert!(!m.first.contains("several values"), "{}", m.first);
}
