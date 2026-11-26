//! B-2026-11-26-10: false `par` conflicts (core semantics §11.2). A branch
//! that writes `t.k` and reads `t.v.len()` or the whole `t` took `t` by one
//! `mut` borrow, read as a write of all of it; a `Copy` tuple's items
//! were copied in whole; and a `let mut` closure called in one branch was
//! refused E0517. Each is accepted now, and each real conflict beside it
//! is still E0418.

use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "struct In { x: i64, y: i64 }\nstruct Top { a: In, k: i64, v: Vec[i64] }\nfn peek(p: Top) -> i64 { p.k + p.a.x }\nfn mk() -> Top { Top { a: In { x: 1, y: 2 }, k: 6, v: [1, 2] } }\n";

/// Accepted: name, `main`, expected output.
const ACCEPTED: &[(&str, &str, &str)] = &[
    (
        "whole_binding_read_beside_a_field_write",
        "fn main() { let mut t = mk(); let (a, b) = par { { t.a.x += 1; peek(t) }, t.a.y }; println(f\"{a} {b} {t.a.x}\"); }\n",
        "8 2 2\n",
    ),
    (
        "method_read_beside_a_field_write",
        "fn main() { let mut t = mk(); let (a, b) = par { { t.k = 0; t.v.len() }, t.v.len() }; println(f\"{a} {b} {t.k}\"); }\n",
        "2 2 0\n",
    ),
    (
        "tuple_items_written_and_read_apart",
        "fn main() { let mut p = (1, 2); let (a, b) = par { { p.0 = 5; 0 }, p.1 }; println(f\"{a} {b} {p.0}\"); }\n",
        "0 2 5\n",
    ),
    (
        "mut_closure_called_in_one_branch",
        "fn main() { let mut n = 0; let mut f = || { n += 1; }; let (a, b) = par { { f(); 0 }, 3 }; println(f\"{a} {b} {n}\"); }\n",
        "0 3 1\n",
    ),
];

/// Refused E0418: name, `main`, a piece of the conflict it names.
const REFUSED: &[(&str, &str, &str)] = &[
    (
        "whole_read_and_the_written_field",
        "fn main() { let mut t = mk(); let (a, b) = par { { t.a.x += 1; peek(t) }, t.a.x }; println(f\"{a} {b}\"); }\n",
        "mutable borrow of t.0.0",
    ),
    (
        "field_write_and_a_whole_read",
        "fn main() { let mut t = mk(); let (a, b) = par { { t.a.x = 1; 0 }, peek(t) }; println(f\"{a} {b}\"); }\n",
        "read of t",
    ),
    (
        "tuple_item_written_and_read",
        "fn main() { let mut p = (1, 2); let (a, b) = par { { p.0 = 5; 0 }, p.0 }; println(f\"{a} {b}\"); }\n",
        "read of p.0",
    ),
    (
        "push_and_len_of_one_field",
        "fn main() { let mut t = mk(); let (a, b) = par { { t.v.push(1); 0 }, t.v.len() }; println(f\"{a} {b}\"); }\n",
        "mutable borrow of t.2",
    ),
    (
        "mut_closure_called_in_two_branches",
        "fn main() { let mut n = 0; let mut f = || { n += 1; }; let (a, b) = par { { f(); 0 }, { f(); 0 } }; println(f\"{a} {b} {n}\"); }\n",
        "mutable borrow of f",
    ),
    (
        "whole_mut_argument_beside_a_field_read",
        "fn bump(p: mut ref Top) { p.k += 1; }\nfn main() { let mut t = mk(); let (a, b) = par { { t.a.x += 1; bump(mut t); 0 }, t.k }; println(f\"{a} {b}\"); }\n",
        "mutable borrow of t",
    ),
];

fn program(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "karac-par-false-conflict-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, format!("{HEADER}{body}")).unwrap();
    file
}

fn karac(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .output()
        .expect("karac");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn par_branches_apart_by_field_tuple_item_or_closure_run_on_the_interpreter() {
    for (name, body, want) in ACCEPTED {
        let file = program(&format!("interp-{name}"), body);
        let f = file.to_str().unwrap();
        let (ok, out) = karac(&["check", f]);
        assert!(ok, "{name}: {out}");
        let (ok, out) = karac(&["__mir-run", f]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, *want, "{name}");
    }
}

#[test]
fn par_branches_that_share_a_written_place_are_still_refused() {
    for (name, body, conflict) in REFUSED {
        let file = program(&format!("refused-{name}"), body);
        let (ok, out) = karac(&["check", file.to_str().unwrap()]);
        assert!(!ok, "{name}: {out}");
        assert!(out.contains("E0418"), "{name}: {out}");
        assert!(out.contains(conflict), "{name}: {out}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn par_branches_apart_by_field_tuple_item_or_closure_compiled() {
    for (name, body, want) in ACCEPTED {
        let file = program(&format!("run-{name}"), body);
        let (ok, out) = karac(&["run", file.to_str().unwrap()]);
        assert!(ok, "{name}: {out}");
        assert_eq!(out, *want, "{name}");
    }
}
