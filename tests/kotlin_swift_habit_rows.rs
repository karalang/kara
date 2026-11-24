//! The Kotlin/Swift habit sweep of the Mend loop, B-2026-11-24-47: habits
//! a newcomer from Kotlin or Swift writes on day one, checked and mended
//! with `karac fix`, then run. A habit whose Kāra form means something
//! else is told with no edit, and those tests say so.

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
        "karac-kotlin-swift-habit-{}-{}",
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

/// Told, with no edit: the check fails as written and `karac fix` leaves
/// the text alone.
fn told(src: &str, note: &str) {
    let m = mend(src);
    assert!(!m.clean, "mended, but should be told only:\n{}", m.text);
    assert!(m.first.contains(note), "{}", m.first);
    assert_eq!(m.text, src, "an edit was applied");
}

#[test]
fn kotlin_val_and_fun_are_let_and_fn_b_2026_11_24_47() {
    mended(
        "fun add(a: Int, b: Int): Int { return a + b }\nfun main() { val x = add(1, 2); println(x) }\n",
        "let x = add(1, 2)",
        "3\n",
    );
}

#[test]
fn kotlin_expression_body_is_a_block_b_2026_11_24_47() {
    mended(
        "fun sq(x: Int): Int = x * x\nfn main() { println(sq(3)); }\n",
        "{ x * x }",
        "9\n",
    );
}

#[test]
fn kotlin_and_swift_type_names_are_karas_b_2026_11_24_47() {
    mended(
        "fn main() { let a: Long = 1; let b: Double = 1.5; let c: Boolean = true; let d: Bool = false; println(f\"{a} {b} {c} {d}\"); }\n",
        "let a: i64",
        "1 1.5 true false\n",
    );
}

#[test]
fn swift_interpolation_is_an_f_string_b_2026_11_24_47() {
    mended(
        "struct P { x: i64 }\nfn main() { let n = 3; let p = P { x: 4 }; println(\"n is \\(n), next \\(n + 1), x \\(p.x)\"); }\n",
        "f\"n is {n}, next {n + 1}, x {p.x}\"",
        "n is 3, next 4, x 4\n",
    );
}

#[test]
fn kotlin_elvis_is_coalesce_b_2026_11_24_47() {
    mended(
        "fn f() -> Option[i64] { None }\nfn main() { let x = f() ?: 7; println(x); }\n",
        "f() ?? 7",
        "7\n",
    );
}

#[test]
fn kotlin_and_swift_force_unwrap_is_unwrap_b_2026_11_24_47() {
    mended(
        "fn f() -> Option[i64] { Some(3) }\nfn main() { let a = f()!!; let b = f()!; println(a + b); }\n",
        "f().unwrap()",
        "6\n",
    );
}

#[test]
fn nullable_type_is_option_b_2026_11_24_47() {
    mended(
        "fn f(x: Int?) -> i64 { x ?? 0 }\nfn main() { println(f(None) + f(Some(2))); }\n",
        "x: Option[i64]",
        "2\n",
    );
}

#[test]
fn swift_if_let_binds_the_value_b_2026_11_24_47() {
    mended(
        "fn f() -> Option[i64] { Some(3) }\nfn main() { let y = f(); if let y { println(y + 1); } }\n",
        "if let Some(y) = y",
        "4\n",
    );
}

#[test]
fn swift_guard_let_is_let_else_b_2026_11_24_47() {
    mended(
        "fn f() -> Option[i64] { Some(3) }\nfn g() { guard let x = f() else { return }; println(x); }\nfn main() { g(); }\n",
        "let Some(x) = f() else { return };",
        "3\n",
    );
}

#[test]
fn swift_guard_condition_is_if_not_b_2026_11_24_47() {
    mended(
        "fn check(x: i64) { guard x > 0 else { println(\"neg\"); return }; println(\"pos\"); }\nfn main() { check(1); check(-1); }\n",
        "if not (x > 0)",
        "pos\nneg\n",
    );
}

#[test]
fn kotlin_when_with_a_subject_is_match_b_2026_11_24_47() {
    mended(
        "fn main() { let x = 3; let s = when (x) { 1, 3 -> \"odd\"; 2 -> \"two\"; else -> \"other\" }; println(s); }\n",
        "match x { 1 | 3 => \"odd\", 2 => \"two\", _ => \"other\" }",
        "odd\n",
    );
}

#[test]
fn kotlin_when_over_lines_is_match_b_2026_11_24_47() {
    mended(
        "fn main() {\n    let x = 2;\n    when (x) {\n        1 -> println(\"one\")\n        2 -> println(\"two\")\n        else -> println(\"many\")\n    }\n}\n",
        "match x",
        "two\n",
    );
}

#[test]
fn kotlin_when_without_a_subject_is_an_if_chain_b_2026_11_24_47() {
    mended(
        "fn main() {\n    let x = 5;\n    when {\n        x < 3 -> println(\"small\")\n        else -> println(\"big\")\n    }\n}\n",
        "if x < 3",
        "big\n",
    );
}

#[test]
fn kotlin_when_with_an_in_arm_is_told_b_2026_11_24_47() {
    told(
        "fn main() { let x = 3; when (x) { in 1..5 -> println(\"small\"); else -> println(\"big\") } }\n",
        "`in` or `is` arm",
    );
}

#[test]
fn kotlin_list_builders_are_lists_b_2026_11_24_47() {
    mended(
        "fn main() { let v = listOf(1, 2, 3); let mut w: Vec[i64] = mutableListOf(); w.push(4); println(v.len() + w.len()); }\n",
        "[1, 2, 3]",
        "4\n",
    );
}

#[test]
fn kotlin_map_of_and_set_of_are_told_b_2026_11_24_47() {
    told(
        "fn main() { let m = mapOf(\"a\" to 1); println(m.len()); }\n",
        "keeps the order",
    );
}

#[test]
fn kotlin_trailing_lambdas_are_closures_b_2026_11_24_47() {
    mended(
        "fn main() { let v = [1, 2, 3]; v.forEach { println(it) } let w: Vec[i64] = v.map { it * 2 }; println(w); let u: Vec[i64] = v.filter { x -> x > 1 }; println(u); }\n",
        ".iter().for_each(|it| println(it))",
        "1\n2\n3\n[2, 4, 6]\n[2, 3]\n",
    );
}

#[test]
fn kotlin_lambda_value_is_a_closure_b_2026_11_24_47() {
    mended(
        "fn main() { let f = { x: Int -> x * 2 }; let add = { a: Int, b: Int -> a + b }; println(f(3) + add(1, 2)); }\n",
        "|x: i64| x * 2",
        "9\n",
    );
}

#[test]
fn kotlin_for_ranges_include_their_end_b_2026_11_24_47() {
    mended(
        "fn main() { for (i in 1..3) { println(i); } let v = [5, 6]; for (i in 0 until v.len()) { println(v[i]); } for (x in v) { println(x); } }\n",
        "for i in 1..=3",
        "1\n2\n3\n5\n6\n5\n6\n",
    );
}

#[test]
fn kotlin_down_to_is_told_b_2026_11_24_47() {
    told(
        "fn main() { for (i in 3 downTo 1) { println(i); } }\n",
        "downTo",
    );
}

#[test]
fn swift_ranges_are_karas_b_2026_11_24_47() {
    mended(
        "fn main() { for i in 1...3 { println(i); } for i in 0..<2 { println(i); } }\n",
        "1..=3",
        "1\n2\n3\n0\n1\n",
    );
}

#[test]
fn swift_unlabeled_parameters_and_dictionary_type_b_2026_11_24_47() {
    mended(
        "func add(_ a: Int, _ b: Int) -> Int { return a + b }\nfn main() { let mut m: [String: Int] = Map.new(); m[\"a\"] = add(1, 2); println(m[\"a\"]); }\n",
        "fn add(a: i64, b: i64)",
        "3\n",
    );
}

#[test]
fn kotlin_data_class_and_swift_fields_are_structs_b_2026_11_24_47() {
    mended(
        "data class P(val x: Int, val y: Int)\nstruct Q { let a: Int; var b: Int }\nfn main() { let p = P { x: 1, y: 2 }; let q = Q { a: 3, b: 4 }; println(p.x + p.y + q.a + q.b); }\n",
        "struct P { x: i64, y: i64 }",
        "10\n",
    );
}

#[test]
fn kotlin_and_swift_properties_and_names_b_2026_11_24_47() {
    mended(
        "fn main() { let v = [1, 2]; let e: Vec[i64] = Vec.new(); println(v.count); println(e.isEmpty); println(\"ab\".uppercase()); println(\"CD\".lowercased()); }\n",
        ".is_empty()",
        "2\ntrue\nAB\ncd\n",
    );
}

#[test]
fn braceless_if_is_a_block_b_2026_11_24_47() {
    mended(
        "fn main() { let x = 3; if (x > 2) println(\"big\"); let s = if (x > 5) \"a\" else \"b\"; println(s); }\n",
        "if x > 2 { println(\"big\"); }",
        "big\nb\n",
    );
}
