//! The TypeScript/JavaScript and Java/C# habit sweep of the Mend loop,
//! B-2026-11-24-8: habits a newcomer from those languages writes on day one,
//! checked and mended with `karac fix`, then run. A habit whose Kāra form
//! means something else is told with no edit, and those tests say so.

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
        "karac-ts-java-habit-{}-{}",
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
fn js_strict_equality_is_kara_equality_b_2026_11_24_8() {
    mended(
        "fn main() { let x = 3; if x === 3 { println(\"eq\"); } if x !== 4 { println(\"ne\"); } }\n",
        "x == 3",
        "eq\nne\n",
    );
}

#[test]
fn js_template_literal_and_csharp_interpolation_are_f_strings_b_2026_11_24_8() {
    mended(
        "fn main() { let n = 3; println(`n is ${n}`); println($\"n={n}\"); println(`plain`); }\n",
        "f\"n is {n}\"",
        "n is 3\nn=3\nplain\n",
    );
}

#[test]
fn foreign_print_calls_are_println_b_2026_11_24_8() {
    mended(
        "fn main() { console.log(\"a\"); System.out.println(\"b\"); Console.WriteLine(\"c\"); System.out.print(\"d\"); Console.Write(\"e\\n\"); }\n",
        "println(\"b\")",
        "a\nb\nc\nde\n",
    );
}

#[test]
fn a_program_of_its_own_console_is_not_rewritten_b_2026_11_24_8() {
    let m = mend(
        "struct Log { n: i64 }\nimpl Log { fn log(self, s: String) { println(f\"{self.n}: {s}\"); } }\nfn main() { let console = Log { n: 1 }; console.log(\"x\"); }\n",
    );
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "1: x\n");
}

#[test]
fn math_functions_are_number_methods_b_2026_11_24_8() {
    mended(
        "fn main() { println(Math.max(3, 7)); println(Math.min(3, 7)); println(Math.abs(-3)); }\n",
        "3.max(7)",
        "7\n3\n3\n",
    );
}

#[test]
fn new_before_a_literal_or_a_collection_goes_b_2026_11_24_8() {
    mended(
        "struct P { x: i64 }\nfn main() { let p = new P { x: 1 }; let mut v: Vec[i64] = new ArrayList<>(); v.push(p.x); let z = new int[3]; println(v.len() + z.len()); }\n",
        "Vec.filled(3, 0_i32)",
        "4\n",
    );
}

#[test]
fn this_in_a_method_is_self_b_2026_11_24_8() {
    mended(
        "struct P { x: i64 }\nimpl P { fn get(self) -> i64 { this.x } }\nfn main() { println(P { x: 2 }.get()); }\n",
        "self.x",
        "2\n",
    );
}

#[test]
fn arrow_functions_are_closures_b_2026_11_24_8() {
    mended(
        "fn main() { let f = (x) => x + 1; let g = (a, b) => { a * b }; let v = [1, 2]; let w: Vec[i64] = v.iter().map(x => x * 2).collect(); println(f(2) + g(3, 4) + w[1]); }\n",
        "|a, b| { a * b }",
        "19\n",
    );
}

#[test]
fn a_match_guard_and_a_tuple_arm_are_not_arrow_functions_b_2026_11_24_8() {
    let m = mend(
        "fn main() { let t = (1, true); let b = true; match t { (a, c) if b => println(a), _ => println(0) } let r = match b { x => x }; println(r); }\n",
    );
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "1\ntrue\n");
}

#[test]
fn the_conditional_operator_is_an_if_b_2026_11_24_8() {
    mended(
        "fn main() { let x = 5; let s = x > 3 ? \"big\" : \"small\"; println(s); }\n",
        "if x > 3 { \"big\" } else { \"small\" }",
        "big\n",
    );
}

#[test]
fn propagation_is_not_a_conditional_b_2026_11_24_8() {
    let m = mend(
        "fn f() -> Option[i64] { Some(2) }\nfn g() -> Option[i64] { let a = f()?; let b = [f()?, 1]; Some(a + b[0]) }\nfn main() { println(g() ?? 0); }\n",
    );
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "4\n");
}

#[test]
fn parenthesized_for_loops_are_kara_loops_b_2026_11_24_8() {
    mended(
        "fn main() { let v = [1, 2, 3]; let mut s = 0; for (let i = 0; i < 4; i++) { s += i; } for (int i = 0; i < 2; i++) { s += i; } for (const x of v) { s += x; } for (int x : v) { s += x; } foreach (var x in v) { s += x; } for x of v { s += x; } println(s); }\n",
        "for i in 0..4",
        "31\n",
    );
}

#[test]
fn js_for_in_over_keys_is_told_b_2026_11_24_8() {
    told(
        "fn main() { let mut m: Map[String, i64] = Map.new(); m[\"a\"] = 1; for (const k in m) { println(k); } }\n",
        "loop over keys",
    );
}

#[test]
fn c_switch_with_break_is_a_match_b_2026_11_24_8() {
    mended(
        "fn main() { let x = 2; switch (x) { case 1: case 3: println(\"odd\"); break; case 2: println(\"two\"); break; default: println(\"other\"); } }\n",
        "1 | 3 =>",
        "two\n",
    );
}

#[test]
fn a_c_switch_that_falls_through_is_declined_b_2026_11_24_8() {
    told(
        "fn main() { let x = 1; switch (x) { case 1: println(\"one\"); case 2: println(\"two\"); break; } }\n",
        "never fall through",
    );
}

#[test]
fn java_arrow_switch_is_a_match_b_2026_11_24_8() {
    mended(
        "fn main() { let x = 2; let s = switch (x) { case 1 -> \"one\"; case 2 -> \"two\"; default -> \"other\"; }; println(s); }\n",
        "2 => { \"two\" }",
        "two\n",
    );
}

#[test]
fn java_headers_parameters_and_locals_are_kara_b_2026_11_24_8() {
    mended(
        "int add(int a, int b) { return a + b; }\nvoid hello() { println(\"h\"); }\nstatic fn twice(a: i64) -> i64 { a * 2 }\npublic static void main(String[] args) { final int x = 3; int[] v = {1, 2, 3}; String s = \"a\"; hello(); println(add(x, v[0])); println(twice(2)); println(s); }\n",
        "fn add(a: i32, b: i32) -> i32",
        "h\n4\n4\na\n",
    );
}

#[test]
fn ts_function_with_result_annotation_and_export_b_2026_11_24_8() {
    mended(
        "export function add(a: i64, b: i64): i64 { return a + b; };\nfn main() { println(add(1, 2)); }\n",
        "pub fn add(a: i64, b: i64) -> i64",
        "3\n",
    );
}

#[test]
fn interface_implements_and_override_are_kara_traits_b_2026_11_24_8() {
    mended(
        "interface Shape { fn area(self) -> i64; }\nstruct Sq { s: i64 }\nimpl Sq implements Shape { @Override fn area(self) -> i64 { self.s * self.s } }\nfn main() { println(Sq { s: 3 }.area()); }\n",
        "impl Shape for Sq",
        "9\n",
    );
}

#[test]
fn imports_of_java_and_csharp_go_b_2026_11_24_8() {
    mended(
        "using System;\nimport java.util.*;\nfn main() { println(\"u\"); }\n",
        "fn main",
        "u\n",
    );
}

#[test]
fn foreign_method_names_with_a_twin_are_renamed_b_2026_11_24_8() {
    mended(
        "fn main() { let s = \"ab\"; let mut v: Vec[i64] = []; v.add(1); let mut m: Map[String, i64] = Map.new(); m.put(\"a\", 1); if v.includes(1) and s.equals(\"ab\") and m.containsKey(\"a\") { println(s.toUpperCase()); } println(v.length + s.length + v.size()); }\n",
        "s.to_uppercase()",
        "AB\n4\n",
    );
}

#[test]
fn char_at_is_told_not_renamed_b_2026_11_24_8() {
    told(
        "fn main() { let s = \"abc\"; println(s.charAt(1)); }\n",
        "Option[char]",
    );
}

#[test]
fn index_of_throw_and_try_catch_are_told_b_2026_11_24_8() {
    told(
        "fn main() { let v = [5, 6]; println(v.indexOf(6)); }\n",
        "-1 when the value is absent",
    );
    told(
        "fn f(n: i64) -> Result[i64, String] { if n < 0 { throw new Error(\"neg\"); } Ok(n) }\nfn main() { println(f(1).unwrap()); }\n",
        "`throw` is Java's and JavaScript's",
    );
    told(
        "fn f() -> Result[i64, String] { Err(\"bad\") }\nfn main() { try { let n = f()?; println(n); } catch (e) { println(\"caught\"); } finally { println(\"done\"); } }\n",
        "`try`/`catch` is Java's",
    );
}

#[test]
fn optional_chaining_and_or_default_are_told_b_2026_11_24_8() {
    told(
        "struct P { x: i64 }\nfn main() { let p: Option[P] = Some(P { x: 3 }); println(p?.x ?? 0); }\n",
        "optional chaining",
    );
    let m = mend("fn main() { let x: Option[i64] = None; println(x || 5); }\n");
    assert!(!m.clean, "{}", m.text);
    assert!(m.text.contains("x or 5"), "{}", m.text);
}

#[test]
fn ts_number_and_js_object_literal_are_told_b_2026_11_24_8() {
    told(
        "fn add(a: number, b: number) -> number { a + b }\nfn main() { println(add(1, 2)); }\n",
        "64-bit float",
    );
    told(
        "fn main() { let o = { a: 1, b: 2 }; println(o); }\n",
        "object literal",
    );
}

#[test]
fn a_class_with_methods_is_told_b_2026_11_24_8() {
    told(
        "class Main { fn main() { println(\"hi\"); } }\n",
        "a function needs no class around it",
    );
}

#[test]
fn kara_names_new_switch_and_const_are_untouched_b_2026_11_24_8() {
    let m = mend(
        "struct P { x: i64 }\nimpl P { fn new(x: i64) -> P { P { x } } }\nconst LIMIT: i64 = 2;\nfn main() { let switch = P.new(LIMIT); let n = switch.x; const K: i64 = 3; println(n + K); }\n",
    );
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "5\n");
}

#[test]
fn a_guard_before_an_arm_arrow_is_not_an_arrow_function_b_2026_11_24_8() {
    let m = mend(
        "fn main() { let first = false; let c: Option[char] = Some(','); match c { Some(',') if not first => println(\"comma\"), Some(_) => println(\"other\"), None => println(\"none\") } }\n",
    );
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "comma\n");
}

#[test]
fn a_programs_own_struct_keeps_its_size_field_b_2026_11_24_8() {
    let m = mend("struct Map { size: i64 }\nfn main() { println(Map { size: 1 }.size); }\n");
    assert!(m.clean, "{}", m.first);
    assert_eq!(m.out, "1\n");
}
