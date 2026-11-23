//! A local or a place stored into a slot that holds a reference is lent,
//! not moved (core-semantics §5.3).
//!
//! `karac check` knew that only for a local at `push`, a Vec `insert` and a
//! deque's pushes. Every other store read the local as moved (E0500 at its
//! next use) and an element as moved out of its collection (E0515, or
//! E_INDEX_MOVE_NON_COPY for `v[i] = words[1]`), with a fix that appends
//! `.clone()`: a temporary, which §5.5 then refuses. The MIR builder lends
//! in every one of these forms, so the programs ran once the check let them
//! through (B-2026-11-22-49).

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Every call gets a directory of its own. Two tests here pass the same
/// names, and the name and the process id alone gave both one directory,
/// where one test's cleanup deleted the other's source (B-2026-11-23-13).
static CALL: AtomicUsize = AtomicUsize::new(0);

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-lent-ref-slots-{name}-{cmd}-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

const HEAD: &str = "struct User { name: String, tags: Vec[String] }\n\
                    struct Holder { r: ref String }\n\
                    fn pair_len(p: (ref String, ref String)) -> i64 { p.0.len() + p.1.len() }\n\
                    fn main() {\n\
                    let mut words: Vec[String] = [\"alpha\".to_string(), \"beta\".to_string()];\n\
                    let mut user = User { name: \"u\".to_string(), tags: [\"t\".to_string()] };\n\
                    let a = \"a\".to_string();\n\
                    let mut r: Vec[ref String] = Vec.new();\n";

/// `karac check` accepts `HEAD` + `body`, and the interpreter and, built
/// with the backend, the compiled program and its ASan build print `want`.
fn lends(name: &str, body: &str, want: &str) {
    let src = format!("{HEAD}{body}}}\n");
    let check = karac(name, &src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}\n{src}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, &src, cmd);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        // Only a failed link may skip: a refusal, a runtime error or a
        // sanitizer report is the program's own.
        if *cmd != "__mir-run" {
            let linked = common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            });
            if linked.is_none() {
                continue;
            }
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}\n{src}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

/// `karac check` refuses `HEAD` + `body` with a message holding each of
/// `needles`.
fn refused(name: &str, body: &str, needles: &[&str]) {
    let src = format!("{HEAD}{body}}}\n");
    let check = karac(name, &src, "check");
    let shown = format!(
        "{}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(!check.status.success(), "{name}: check passed\n{src}");
    for n in needles {
        assert!(shown.contains(n), "{name}: no `{n}` in:\n{shown}");
    }
}

#[test]
fn lent_slot_a_local_stays_usable_after_every_store() {
    for (name, body, want) in [
        (
            "vec",
            "r.push(a);\n\
             r.insert(0, a);\n\
             r.fill(a);\n\
             r.resize(3, a);\n\
             r[0] = a;\n\
             println(f\"{r[0]} {r.len()} {a}\");\n",
            "a 3 a\n",
        ),
        (
            "deque",
            "let mut d: VecDeque[ref String] = VecDeque.new();\n\
             d.push_back(a);\n\
             d.push_front(a);\n\
             d[1] = a;\n\
             println(f\"{d[0]} {d.len()} {a}\");\n",
            "a 2 a\n",
        ),
        (
            "map",
            "let mut m: Map[i64, ref String] = Map.new();\n\
             m.insert(1, a);\n\
             m[2] = a;\n\
             m.entry(3).or_insert(a);\n\
             println(f\"{m[1]} {m[2]} {m[3]} {a}\");\n",
            "a a a a\n",
        ),
        (
            "set",
            "let mut s: Set[ref String] = Set.new();\n\
             s.insert(a);\n\
             let mut o: Option[ref String] = None;\n\
             o.replace(a);\n\
             println(f\"{s.len()} {o.unwrap()} {a}\");\n",
            "1 a a\n",
        ),
    ] {
        lends(name, body, want);
    }
}

#[test]
fn lent_slot_an_element_is_borrowed_where_it_is() {
    for (name, body, want) in [
        (
            "vec",
            "r.push(words[0]);\n\
             r.insert(0, words[1]);\n\
             r.fill(words[0]);\n\
             r.resize(3, words[1]);\n\
             r[0] = words[1];\n\
             println(f\"{r[0]} {r[1]} {r[2]} {words.len()}\");\n",
            "beta alpha beta 2\n",
        ),
        (
            "deque",
            "let mut d: VecDeque[ref String] = VecDeque.new();\n\
             d.push_back(words[0]);\n\
             d.push_front(words[1]);\n\
             println(f\"{d[0]} {d[1]} {words.len()}\");\n",
            "beta alpha 2\n",
        ),
        (
            "map",
            "let mut m: Map[i64, ref String] = Map.new();\n\
             m.insert(1, words[0]);\n\
             m[2] = words[1];\n\
             m.entry(3).or_insert(words[0]);\n\
             println(f\"{m[1]} {m[2]} {m[3]} {words.len()}\");\n",
            "alpha beta alpha 2\n",
        ),
        (
            "set",
            "let mut s: Set[ref String] = Set.new();\n\
             s.insert(words[0]);\n\
             let mut o: Option[ref String] = None;\n\
             o.replace(words[1]);\n\
             println(f\"{s.len()} {o.unwrap()} {words.len()}\");\n",
            "1 beta 2\n",
        ),
        (
            "loop",
            "for i in 0..words.len() {\n\
             r.push(words[i]);\n\
             }\n\
             println(f\"{r[0]} {r[1]} {words.len()}\");\n",
            "alpha beta 2\n",
        ),
        (
            "numbers",
            "let nums: Vec[i64] = [4, 5];\n\
             let mut n: Vec[ref i64] = Vec.new();\n\
             n.push(nums[1]);\n\
             n.fill(nums[0]);\n\
             println(f\"{n[0]} {nums.len()}\");\n",
            "4 2\n",
        ),
    ] {
        lends(name, body, want);
    }
}

#[test]
fn lent_slot_a_field_or_a_tuple_part_is_borrowed_where_it_is() {
    lends(
        "parts",
        "let t = (\"x\".to_string(), 1);\n\
         let grid: Vec[Vec[String]] = [[\"g\".to_string()]];\n\
         r.push(user.name);\n\
         r.push(user.tags[0]);\n\
         r.push(t.0);\n\
         r.insert(0, grid[0][0]);\n\
         println(f\"{r[0]} {r[1]} {r[2]} {r[3]} {user.name} {user.tags.len()} {t.0} {grid.len()}\");\n",
        "g u t x u 1 x 1\n",
    );
}

#[test]
fn lent_slot_a_literal_lends_what_its_reference_slots_hold() {
    for (name, body, want) in [
        (
            "struct_field",
            "let h = Holder { r: a };\n\
             let k = Holder { r: words[1] };\n\
             println(f\"{h.r} {k.r} {a} {words.len()}\");\n",
            "a beta a 2\n",
        ),
        (
            "list",
            "let v: Vec[ref String] = [a, words[1]];\n\
             println(f\"{v[0]} {v[1]} {a} {words.len()}\");\n",
            "a beta a 2\n",
        ),
        (
            "tuple",
            "let t: (ref String, i64) = (a, 1);\n\
             let u: (ref String, i64) = (words[0], 2);\n\
             println(f\"{t.0} {u.0} {a} {words.len()}\");\n",
            "a alpha a 2\n",
        ),
        (
            "argument",
            "let n = pair_len((a, words[0]));\n\
             println(f\"{n} {a} {words.len()}\");\n",
            "6 a 2\n",
        ),
        (
            "some",
            "let mut o: Option[ref String] = Some(a);\n\
             println(f\"{o.unwrap()}\");\n\
             o = Some(words[1]);\n\
             println(f\"{o.unwrap()} {a} {words.len()}\");\n",
            "a\nbeta a 2\n",
        ),
    ] {
        lends(name, body, want);
    }
}

#[test]
fn lent_slot_the_lender_is_not_written_moved_or_dropped_first() {
    for (name, body, needle) in [
        (
            "write",
            "r.push(words[0]);\n\
             words.push(\"gamma\".to_string());\n\
             println(f\"{r[0]}\");\n",
            "cannot mutably borrow `words` while `r` still holds a borrow of it",
        ),
        (
            "element_write",
            "r.push(words[0]);\n\
             r.fill(words[1]);\n\
             words[1] = \"z\".to_string();\n\
             println(f\"{r[0]}\");\n",
            "cannot mutably borrow `words` while `r` still holds a borrow of it",
        ),
        (
            "field_write",
            "r.push(user.name);\n\
             user.name = \"v\".to_string();\n\
             println(f\"{r[0]}\");\n",
            "while `r` still holds a borrow of it",
        ),
        (
            "move",
            "r.resize(2, words[0]);\n\
             let moved = words;\n\
             println(f\"{r[0]} {moved.len()}\");\n",
            "cannot move `words` while `r` still holds a borrow of it",
        ),
        (
            "local_move",
            "r.push(a);\n\
             let b = a;\n\
             println(f\"{r[0]} {b}\");\n",
            "cannot move `a` while `r` still holds a borrow of it",
        ),
        (
            "scope",
            "r.push(a);\n\
             {\n\
             let inner: Vec[String] = [\"i\".to_string()];\n\
             r[0] = inner[0];\n\
             }\n\
             println(f\"{r.len()}\");\n",
            "cannot drop `inner` while `r` still holds a borrow of it",
        ),
        (
            "set_scope",
            "let mut s: Set[ref String] = Set.new();\n\
             {\n\
             let inner: Vec[String] = [\"i\".to_string()];\n\
             s.insert(inner[0]);\n\
             }\n\
             println(f\"{s.len()}\");\n",
            "cannot drop `inner` while `s` still holds a borrow of it",
        ),
        (
            "entry_scope",
            "let mut m: Map[i64, ref String] = Map.new();\n\
             {\n\
             let inner: Vec[String] = [\"i\".to_string()];\n\
             m.entry(1).or_insert(inner[0]);\n\
             }\n\
             println(f\"{m.len()}\");\n",
            "cannot drop `inner` while `m` still holds a borrow of it",
        ),
    ] {
        refused(name, body, &["E0516", needle]);
    }
}

#[test]
fn lent_slot_a_slot_that_owns_its_value_still_takes_it() {
    refused(
        "owned_element",
        "let mut o: Vec[String] = Vec.new();\n\
         o.push(words[0]);\n\
         println(f\"{o[0]}\");\n",
        &[
            "E0515",
            "cannot move a non-`Copy` value out of a collection element",
        ],
    );
    refused(
        "owned_field",
        "let mut o: Vec[String] = Vec.new();\n\
         o.push(user.name);\n\
         println(f\"{o[0]} {user.name}\");\n",
        &["E0500", "value 'user.name' moved here"],
    );
    refused(
        "owned_let",
        "let x: String = words[0];\n\
         println(f\"{x}\");\n",
        &["E_INDEX_MOVE_NON_COPY"],
    );
    refused(
        "owned_index_assign",
        "let mut o: Vec[String] = [\"o\".to_string()];\n\
         o[0] = words[0];\n\
         println(f\"{o[0]}\");\n",
        &["E_INDEX_MOVE_NON_COPY"],
    );
    refused(
        "owned_local",
        "let mut o: Vec[String] = Vec.new();\n\
         o.push(a);\n\
         println(f\"{o[0]} {a}\");\n",
        &["E0500", "value 'a' moved here"],
    );
}
