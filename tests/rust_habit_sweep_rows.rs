//! Rows from the Rust-habit sweep of the Mend loop, B-2026-11-20-14..22:
//! Rust a newcomer writes on day one, checked and mended with `karac fix`,
//! then run. Each test names its row.

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
        "karac-rust-habit-{}-{}",
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

fn mend_check_only(src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-rust-habit-c-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
    )
}

#[test]
fn deref_write_through_a_for_item_is_refused_b_2026_11_20_14() {
    for src in [
        "fn main() { let mut v = [1, 2]; for x in mut ref v { *x += 1; } println(f\"{v[0]}\"); }\n",
        "fn main() { let mut v = Vec.new(); v.push(1); for x in mut ref v { *x = 9; } println(f\"{v[0]}\"); }\n",
        "fn main() { let mut v = [[1], [2]]; for x in mut ref v { (*x).push(3); } println(f\"{v[0].len()}\"); }\n",
    ] {
        let (ok, text) = mend_check_only(src);
        assert!(!ok, "{src}: {text}");
        assert!(text.contains("E0517"), "{text}");
    }
    let (ok, text) = mend_check_only(
        "fn main() { let mut v = [1, 2]; for x in v.iter_mut() { *x += 1; } println(f\"{v[0]}\"); }\n",
    );
    assert!(ok, "{text}");
}

#[test]
fn rust_for_over_mut_borrow_is_mended_to_iter_mut_b_2026_11_20_14() {
    let m = mend(
        "fn main() { let mut v = vec![1, 2]; for x in &mut v { *x += 1; } \
         println!(\"{} {}\", v[0], v[1]); }\n",
    );
    assert!(m.text.contains("v.iter_mut()"), "{}", m.text);
    assert!(m.clean, "{}", m.text);
    assert_eq!(m.out, "2 3\n");
}

#[test]
fn rust_static_item_is_mended_to_const_b_2026_11_20_15() {
    let m = mend("static NAME: &str = \"kara\";\nfn main() { println!(\"{}\", NAME); }\n");
    assert!(m.text.starts_with("const NAME"), "{}", m.text);
    assert_eq!(m.out, "kara\n");
    let (ok, text) = mend_check_only("static mut N: i64 = 0;\nfn main() {}\n");
    assert!(!ok);
    assert!(text.contains("no mutable globals"), "{text}");
}

#[test]
fn refcell_of_a_scalar_gets_no_copying_fix_b_2026_11_20_16() {
    let m = mend(
        "use std::rc::Rc;\nuse std::cell::RefCell;\nfn main() { let c = Rc::new(RefCell::new(1)); \
         let d = Rc::clone(&c); *d.borrow_mut() += 4; println!(\"{}\", c.borrow()); }\n",
    );
    assert!(!m.clean, "a copy would print 1: {}", m.text);
    assert!(m.text.contains("RefCell"), "{}", m.text);
    // A struct the file declares still takes the rewrite.
    let m = mend(
        "use std::rc::Rc;\nuse std::cell::RefCell;\nstruct TreeNode { val: i64, left: \
         Option<Rc<RefCell<TreeNode>>> }\nfn main() { let a = Rc::new(RefCell::new(TreeNode { \
         val: 1, left: None })); let b = Rc::new(RefCell::new(TreeNode { val: 2, left: \
         Some(Rc::clone(&a)) })); a.borrow_mut().val = 7; println!(\"{}\", \
         b.borrow().left.as_ref().unwrap().borrow().val); }\n",
    );
    assert!(m.text.contains("shared struct TreeNode"), "{}", m.text);
    assert_eq!(m.out, "7\n");
}

#[test]
fn std_path_in_a_format_argument_drops_its_modules_b_2026_11_20_17() {
    let m = mend("fn main() { println!(\"{}\", std::cmp::max(3, 9)); }\n");
    assert!(m.text.contains("{max(3, 9)}"), "{}", m.text);
    assert_eq!(m.out, "9\n");
}

#[test]
fn debug_hole_is_named_in_the_print_macro_message_b_2026_11_20_18() {
    let m = mend("fn main() { let v = vec![1, 2]; println!(\"{:?}\", v); }\n");
    assert!(m.first.contains("Debug form"), "{}", m.first);
}

#[test]
fn struct_literal_and_sum_turbofish_go_into_the_hole_b_2026_11_20_19() {
    let m = mend(
        "struct P { x: i64 }\nfn main() { let v = vec![1, 2, 3]; println!(\"{} {}\", \
         v.iter().sum::<i64>(), P { x: 3 }.x); }\n",
    );
    assert!(m.clean, "{}", m.text);
    assert_eq!(m.out, "6 3\n");
}

#[test]
fn method_on_a_unit_struct_name_is_called_on_its_value_b_2026_11_20_20() {
    let m = mend(
        "struct A;\nimpl A { fn hi(&self) -> i64 { 1 } }\nfn main() { println!(\"{}\", A.hi()); }\n",
    );
    assert!(m.text.contains("A {}.hi()"), "{}", m.text);
    assert_eq!(m.out, "1\n");
}

#[test]
fn impl_iterator_with_item_binding_is_bracketed_b_2026_11_20_21() {
    let m = mend(
        "fn sum_all(it: impl Iterator<Item = i64>) -> i64 { it.sum() }\nfn main() { \
         println!(\"{}\", sum_all(vec![1, 2].into_iter())); }\n",
    );
    assert!(m.text.contains("Iterator[Item = i64]"), "{}", m.text);
    assert_eq!(m.out, "3\n");
}

#[test]
fn inline_mod_is_unwrapped_and_its_paths_lose_the_module_b_2026_11_20_22() {
    let m = mend(
        "mod geo {\n    pub struct P { pub x: i64 }\n    pub fn origin() -> P { P { x: 0 } }\n}\n\
         fn main() { let p = geo::origin(); let q = geo::P { x: 2 }; println!(\"{} {}\", p.x, \
         q.x); println!(\"{}\", geo::origin().x); }\n",
    );
    assert!(!m.text.contains("mod geo"), "{}", m.text);
    assert_eq!(m.out, "0 2\n0\n");
}
