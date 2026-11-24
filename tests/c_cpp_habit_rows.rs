//! The C/C++ habit sweep of the Mend loop, B-2026-11-24-31: habits a
//! newcomer from C or C++ writes on day one, checked and mended with
//! `karac fix`, then run. A habit whose Kāra form means something else is
//! told with no edit, and those tests say so.

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
        "karac-c-cpp-habit-{}-{}",
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
fn c_include_and_using_namespace_go_b_2026_11_24_31() {
    mended(
        "#include <stdio.h>\n#include \"x.h\"\nusing namespace std;\nfn main() { println(\"hi\"); }\n",
        "fn main()",
        "hi\n",
    );
}

#[test]
fn c_int_main_returning_zero_is_kara_main_b_2026_11_24_31() {
    mended(
        "int main() { println(\"hi\"); return 0; }\n",
        "fn main() { println(\"hi\");  }",
        "hi\n",
    );
}

#[test]
fn c_main_arguments_it_never_reads_go_b_2026_11_24_31() {
    mended(
        "int main(int argc, char** argv) { println(\"hi\"); return 0; }\n",
        "fn main()",
        "hi\n",
    );
}

#[test]
fn c_main_returning_another_status_is_told_b_2026_11_24_31() {
    told("int main() { println(\"hi\"); return 1; }\n", "ExitCode");
}

#[test]
fn c_printf_conversions_are_f_string_holes_b_2026_11_24_31() {
    mended(
        "fn main() { let n = 42; let s = \"ab\"; let x = 3.14159; printf(\"[%5d][%-4s][%03d]\\n\", n, s, 7); printf(\"%.2f %d%%\\n\", x, 50); printf(\"%x\", 255); printf(\"\\n\"); }\n",
        "println(f\"[{n:>5}][{s:<4}][{7:03}]\")",
        "[   42][ab  ][007]\n3.14 50%\nff\n",
    );
}

#[test]
fn c_printf_f_prints_six_places_b_2026_11_24_31() {
    mended(
        "fn main() { let x = 1.5; printf(\"%f\\n\", x); }\n",
        "{x:.6}",
        "1.500000\n",
    );
}

#[test]
fn c_printf_e_and_c_of_a_number_are_told_b_2026_11_24_31() {
    told(
        "fn main() { let x = 1500.0; let c = 65; printf(\"%e %c\\n\", x, c); }\n",
        "conversion whose Kāra text differs",
    );
}

#[test]
fn c_puts_is_println_b_2026_11_24_31() {
    mended("fn main() { puts(\"hi\"); }\n", "println(\"hi\")", "hi\n");
}

#[test]
fn cpp_cout_chain_is_println_b_2026_11_24_31() {
    mended(
        "fn main() { let v = [1, 2]; std::cout << \"n=\" << v.len() * 2 << std::endl; let x = 1; cout << x << ' ' << x + 1 << '\\n'; std::cout << \"a\" << \"b\"; std::cout << std::endl; }\n",
        "println(f\"n={v.len() * 2}\")",
        "n=4\n1 2\nab\n",
    );
}

#[test]
fn c_arrow_member_access_is_a_dot_b_2026_11_24_31() {
    mended(
        "struct P { x: i64 }\nimpl P { fn get(self) -> i64 { self.x } }\nstruct Q { p: P }\nfn main() { let q = Q { p: P { x: 4 } }; let p = P { x: 3 }; println(p->x + p->get() + q.p->x); }\n",
        "p.x + p.get()",
        "10\n",
    );
}

#[test]
fn c_pointer_and_const_ref_parameters_are_lent_b_2026_11_24_31() {
    mended(
        "struct P { x: i64 }\nfn show(p: P*) { println(p->x); }\nfn greet(const std::string& name) { printf(\"hi %s\\n\", name); }\nfn main() { let p = P { x: 3 }; show(p); greet(\"bo\"); }\n",
        "name: String",
        "3\nhi bo\n",
    );
}

#[test]
fn cpp_vector_locals_are_kara_vecs_b_2026_11_24_31() {
    mended(
        "fn main() { std::vector<int> v; v.push_back(1); std::vector<int> w = {1, 2, 3}; std::vector<int> f(3, 7); vector<int> z(2); println(v.len() + w.len()); println(f); println(z); }\n",
        "Vec.filled(3, 7)",
        "4\n[7, 7, 7]\n[0, 0]\n",
    );
}

#[test]
fn cpp_std_map_is_the_ordered_map_b_2026_11_24_31() {
    mended(
        "fn main() { std::map<std::string, int> m; m[\"b\"] = 2; m[\"a\"] = 1; for (auto kv : m) { println(kv); } }\n",
        "SortedMap[String, i32]",
        "(a, 1)\n(b, 2)\n",
    );
}

#[test]
fn cpp_auto_const_and_unsigned_locals_are_let_b_2026_11_24_31() {
    mended(
        "fn main() { auto x = 3; const int y = 4; unsigned int z = 5; long long w = 6; std::string s = \"ab\"; int u; u = 1; println(f\"{x} {y} {z} {w} {s} {u}\"); }\n",
        "let z: u32 = 5",
        "3 4 5 6 ab 1\n",
    );
}

#[test]
fn c_prefix_increment_steps_the_counter_b_2026_11_24_31() {
    mended(
        "fn main() { let mut i = 0; ++i; ++i; --i; println(i); for (size_t k = 0; k < 2; ++k) { println(k); } }\n",
        "i += 1",
        "1\n0\n1\n",
    );
}

#[test]
fn c_endless_for_is_loop_b_2026_11_24_31() {
    mended(
        "fn main() { let mut i = 0; for (;;) { i += 1; if i == 3 { break; } } println(i); }\n",
        "loop {",
        "3\n",
    );
}

#[test]
fn cpp_range_for_over_auto_is_for_in_b_2026_11_24_31() {
    mended(
        "fn main() { let v = [1, 2]; for (auto x : v) { println(x); } for (const auto& x : v) { println(x); } }\n",
        "for x in v",
        "1\n2\n1\n2\n",
    );
}

#[test]
fn cpp_range_for_writing_through_a_reference_is_told_b_2026_11_24_31() {
    told(
        "fn main() { let mut v = [1, 2]; for (auto& x : v) { x *= 2; } println(v); }\n",
        "iter_mut",
    );
}

#[test]
fn cpp_std_sort_and_reverse_over_begin_end_are_methods_b_2026_11_24_31() {
    mended(
        "fn main() { let mut v = [3, 1, 2]; std::sort(v.begin(), v.end()); println(v); std::reverse(v.begin(), v.end()); println(v); }\n",
        "v.sort()",
        "[1, 2, 3]\n[3, 2, 1]\n",
    );
}

#[test]
fn cpp_sort_with_a_comparator_is_told_b_2026_11_24_31() {
    told(
        "fn main() { let mut v = [1, 3, 2]; std::sort(v.begin(), v.end(), desc); println(v); }\n",
        "sort_by",
    );
}

#[test]
fn cpp_to_string_is_told_b_2026_11_24_31() {
    told("fn main() { println(std::to_string(42)); }\n", "six places");
}

#[test]
fn cpp_empty_is_is_empty_and_back_is_told_b_2026_11_24_31() {
    mended(
        "fn main() { let v: Vec[i64] = Vec.new(); println(v.empty()); }\n",
        "v.is_empty()",
        "true\n",
    );
}

#[test]
fn cpp_back_is_told_b_2026_11_24_31() {
    told(
        "fn main() { let v = [1, 2]; println(v.back()); }\n",
        "`v.last()` is an `Option`",
    );
}

#[test]
fn c_struct_fields_and_brace_initializers_are_kara_b_2026_11_24_31() {
    mended(
        "struct P { int x; int y; };\nfn main() { P p = {3, 4}; printf(\"%d,%d\\n\", p.x, p.y); }\n",
        "let p = P { x: 3, y: 4 }",
        "3,4\n",
    );
}

#[test]
fn c_designated_initializer_names_the_fields_b_2026_11_24_31() {
    mended(
        "struct P { x: i64, y: i64 }\nfn main() { let p = P { .x = 1, .y = 2 }; println(p.x + p.y); }\n",
        "P { x: 1, y: 2 }",
        "3\n",
    );
}

#[test]
fn c_reference_variable_and_default_struct_are_told_b_2026_11_24_31() {
    told(
        "fn main() { let mut a = 1; int& r = a; r = 5; println(a); }\n",
        "alias of another place",
    );
}

#[test]
fn c_nested_conditional_is_a_nested_if_b_2026_11_24_31() {
    mended(
        "fn main() { let x = 0; let s = x > 0 ? \"pos\" : x < 0 ? \"neg\" : \"zero\"; println(s); }\n",
        "else { if x < 0",
        "zero\n",
    );
}
