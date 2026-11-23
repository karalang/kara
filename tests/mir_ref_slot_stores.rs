//! Stores into a slot that holds a reference, beyond `push`.
//!
//! `v[i] = r` on a `Vec[ref T]`, `VecDeque[ref T]` or `mut Slice[ref T]`
//! and `ptr.mut(xs[i])` were refused as not lowered, so a
//! `PriorityQueue[ref i32]` did not build: the library heap's sift takes
//! the address of each slot (B-2026-11-20-40). No type names a reference to
//! a reference, so the assignment is the native `__replace` and the pointer
//! comes from the native `index_slot`.
//!
//! An owned value stored into such a slot is lent (core-semantics §5.3),
//! and one that dies while the collection still holds the reference is
//! refused (§5.5). `m[k] = temp`, `m.entry(k).or_insert(temp)`,
//! `o.replace(temp)` and `v.extend([temp])` checked clean and read freed
//! memory: the first two moved the value into the reference's slot, the
//! third lent its receiver shared, and the last consumed the source whose
//! items the collection referred to (B-2026-11-22-2's remainders).
//!
//! The item of `for x in nums.iter()` over `Copy` elements is the reference
//! into `nums` wherever such a slot wants one (ruled 2026-11-23,
//! B-2026-11-22-52): `q.push(x)` lent the loop's own copy of the element,
//! which the queue outlives, and was E0516, while `let start = x;
//! q.push(start)` lent `nums`; `best = x` into a `ref` local stopped the
//! build with invalid MIR.

mod common;

use std::process::Command;

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-slot-stores-{name}-{cmd}-{}",
        std::process::id()
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

/// `karac check` accepts `src`, and the interpreter and, built with the
/// backend, the compiled program and its ASan build print `want`.
fn runs_everywhere(name: &str, src: &str, want: &str) {
    let check = karac(name, src, "check");
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let cmds: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run", "__mir-llvm-asan"]
    } else {
        &["__mir-run"]
    };
    for cmd in cmds {
        let out = karac(name, src, cmd);
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
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

/// `karac check` refuses `src` with a message holding each of `needles`,
/// and the MIR pipeline does not run it either.
fn refused(name: &str, src: &str, needles: &[&str]) {
    let check = karac(name, src, "check");
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

/// As [`refused`], by the MIR borrow check too, which every backend runs.
fn refused_by_borrowck(name: &str, src: &str, needles: &[&str]) {
    refused(name, src, needles);
    let run = karac(name, src, "__mir-run");
    let err = String::from_utf8_lossy(&run.stderr).to_string();
    assert!(!run.status.success(), "{name}: ran\n{src}");
    assert!(err.contains("E0516"), "{name}: {err}");
}

#[test]
fn an_index_assignment_replaces_a_reference_element() {
    runs_everywhere(
        "index_assign",
        "fn swap2(xs: mut Slice[ref String]) {\n\
         let t = xs[0];\n\
         xs[0] = xs[1];\n\
         xs[1] = t;\n\
         }\n\
         fn main() {\n\
         let a = \"a\".to_string();\n\
         let b = \"b\".to_string();\n\
         let mut v: Vec[ref String] = Vec.new();\n\
         v.push(a);\n\
         v.push(a);\n\
         v[1] = b;\n\
         let t = v[0];\n\
         v[0] = v[1];\n\
         v[1] = t;\n\
         println(f\"{v[0]} {v[1]}\");\n\
         let mut d: VecDeque[ref String] = VecDeque.new();\n\
         d.push_back(a);\n\
         let c = \"c\".to_string();\n\
         d[0] = c;\n\
         println(f\"{d[0]}\");\n\
         swap2(mut v);\n\
         println(f\"{v[0]} {v[1]}\");\n\
         }\n",
        "b a\nc\na b\n",
    );
}

#[test]
fn a_priority_queue_of_references_sifts_its_slots() {
    runs_everywhere(
        "heap_of_refs",
        "fn main() {\n\
         let nums: Vec[i32] = [5, 1, 4, 9, 2];\n\
         let mut q: PriorityQueue[ref i32] = PriorityQueue.new();\n\
         for x in nums.iter() {\n\
         let start = x;\n\
         q.push(start);\n\
         }\n\
         let top = q.pop().unwrap();\n\
         println(f\"{top} {q.len()}\");\n\
         while let Some(v) = q.pop() {\n\
         println(f\"{v}\");\n\
         }\n\
         }\n",
        "1 4\n2\n4\n5\n9\n",
    );
}

#[test]
fn a_for_item_is_the_reference_where_a_slot_holds_one() {
    runs_everywhere(
        "for_item_stores",
        "struct H { r: ref i32 }\n\
         fn keep(v: mut ref Vec[ref i32], x: ref i32) { v.push(x); }\n\
         fn main() {\n\
         let nums: Vec[i32] = [5, 1, 4, 9, 2];\n\
         let mut q: PriorityQueue[ref i32] = PriorityQueue.new();\n\
         let mut v: Vec[ref i32] = Vec.new();\n\
         let mut d: VecDeque[ref i32] = VecDeque.new();\n\
         let mut s: Set[ref i32] = Set.new();\n\
         let mut m: Map[ref i32, i64] = Map.new();\n\
         let mut at: Map[i64, ref i32] = Map.new();\n\
         let mut hs: Vec[H] = Vec.new();\n\
         let mut ps: Vec[(ref i32, i64)] = Vec.new();\n\
         let mut last: Option[ref i32] = None;\n\
         let mut o: Option[ref i32] = None;\n\
         for x in nums.iter() {\n\
         q.push(x);\n\
         v.push(x);\n\
         d.push_front(x);\n\
         s.insert(x);\n\
         m.insert(x, 1);\n\
         *m.entry(x).or_insert(0) += 1;\n\
         hs.push(H { r: x });\n\
         ps.push((x, 1));\n\
         last = Some(x);\n\
         let _ = o.replace(x);\n\
         keep(mut v, x);\n\
         }\n\
         for (i, x) in nums.iter().enumerate() {\n\
         at.insert(i, x);\n\
         at[i + 10] = x;\n\
         at.entry(i + 20).or_insert(x);\n\
         }\n\
         let mut best: ref i32 = ref nums[0];\n\
         let mut h = H { r: ref nums[0] };\n\
         let mut t: (ref i32, i64) = (ref nums[0], 0);\n\
         for x in nums.iter() {\n\
         if x < best { best = x; }\n\
         if x > h.r { h.r = x; }\n\
         if x < t.0 { t.0 = x; }\n\
         }\n\
         println(f\"{q.pop().unwrap()} {v[0]} {v[9]} {d[0]} {s.len()} {m.len()}\");\n\
         println(f\"{at[1]} {at[14]} {at[22]} {hs[3].r} {ps[2].0}\");\n\
         println(f\"{last.unwrap()} {o.unwrap()} {best} {h.r} {t.0} {nums.len()}\");\n\
         }\n",
        "1 5 2 2 5 5\n1 2 4 9 4\n2 2 1 9 1 5\n",
    );
}

#[test]
fn a_for_item_reference_keeps_its_collection_borrowed() {
    for (name, src, needle) in [
        (
            "for_item_lender_pushed",
            "fn main() {\n\
             let mut nums: Vec[i64] = [1, 1, 2];\n\
             let mut count: Map[ref i64, i64] = Map.new();\n\
             for x in nums.iter() {\n\
             *count.entry(x).or_insert(0) += 1;\n\
             }\n\
             nums.push(3);\n\
             println(f\"{count.len()}\");\n\
             }\n",
            "cannot mutably borrow `nums` while `count` still holds a borrow of it",
        ),
        (
            "for_item_lender_dropped",
            "fn main() {\n\
             let mut q: PriorityQueue[ref i32] = PriorityQueue.new();\n\
             {\n\
             let nums: Vec[i32] = [5, 1, 4];\n\
             for x in nums.iter() {\n\
             q.push(x);\n\
             }\n\
             }\n\
             println(f\"{q.len()}\");\n\
             }\n",
            "cannot drop `nums` while `q` still holds a borrow of it",
        ),
        (
            "for_item_lender_written",
            "fn main() {\n\
             let mut nums: Vec[i32] = [5, 1, 4];\n\
             let mut q: Vec[ref i32] = Vec.new();\n\
             for x in nums.iter() {\n\
             q.push(x);\n\
             }\n\
             nums[0] = 9;\n\
             println(f\"{q[0]}\");\n\
             }\n",
            "cannot mutably borrow `nums` while `q` still holds a borrow of it",
        ),
        (
            "for_item_lender_moved",
            "fn main() {\n\
             let nums: Vec[i32] = [5, 1, 4];\n\
             let mut best: Option[ref i32] = None;\n\
             for x in nums.iter() {\n\
             best = Some(x);\n\
             }\n\
             let moved = nums;\n\
             println(f\"{best.unwrap()} {moved.len()}\");\n\
             }\n",
            "cannot move `nums` while `best` still holds a borrow of it",
        ),
    ] {
        refused_by_borrowck(name, src, &["E0516", needle]);
    }
}

#[test]
fn a_raw_pointer_to_a_reference_element_is_to_its_slot() {
    runs_everywhere(
        "slot_pointer",
        "fn rotate[T: Ord](xs: mut Slice[T]) {\n\
         let p: *mut T = ptr.mut(xs[1]);\n\
         let base: *mut T = ptr.mut(xs[0]);\n\
         // Safety: both indices were checked above; each element is read\n\
         // out once and written back once.\n\
         let x = unsafe { p.read() };\n\
         if x > xs[0] {\n\
         // Safety: as above.\n\
         unsafe {\n\
         base.offset(1).write(base.offset(0).read());\n\
         base.offset(0).write(x);\n\
         }\n\
         }\n\
         }\n\
         fn main() {\n\
         let a: i32 = 1;\n\
         let b: i32 = 2;\n\
         let mut v: Vec[ref i32] = Vec.new();\n\
         v.push(a);\n\
         v.push(b);\n\
         rotate(mut v);\n\
         println(f\"{v[0]} {v[1]}\");\n\
         }\n",
        "2 1\n",
    );
}

#[test]
fn extend_takes_the_references_a_source_lends() {
    runs_everywhere(
        "extend_lent",
        "fn main() {\n\
         let words: Vec[String] = [\"alpha\".to_string(), \"beta\".to_string()];\n\
         let mut refs: Vec[ref String] = Vec.new();\n\
         refs.extend(words.iter());\n\
         refs.extend(words[0..1]);\n\
         let mut d: VecDeque[ref String] = VecDeque.new();\n\
         d.extend(words.iter());\n\
         println(f\"{refs[0]} {refs[2]} {refs.len()} {d[1]}\");\n\
         }\n",
        "alpha alpha 3 beta\n",
    );
}

#[test]
fn an_index_assignment_of_a_value_that_dies_first_is_refused() {
    refused_by_borrowck(
        "index_assign_dies",
        "fn main() {\n\
         let a = \"a\".to_string();\n\
         let mut v: Vec[ref String] = Vec.new();\n\
         v.push(a);\n\
         {\n\
         let t = \"t\".to_string();\n\
         v[0] = t;\n\
         }\n\
         println(f\"{v[0]}\");\n\
         }\n",
        &[
            "E0516",
            "cannot drop `t` while `v` still holds a borrow of it",
        ],
    );
}

#[test]
fn a_temporary_stored_over_a_reference_is_refused() {
    let temp = "cannot drop the temporary value";
    let head = "fn main() {\n\
                let words: Vec[String] = [\"alpha\".to_string(), \"beta\".to_string()];\n";
    for (name, body) in [
        (
            "vec_index",
            "let mut v: Vec[ref String] = Vec.new();\n\
             v.push(words[1]);\n\
             v[0] = words[0].clone();\n\
             println(f\"{v[0]}\");\n",
        ),
        (
            "map_index",
            "let mut m: Map[i64, ref String] = Map.new();\n\
             m[1] = words[0].clone();\n\
             println(f\"{m[1]}\");\n",
        ),
        (
            "or_insert",
            "let mut m: Map[i64, ref String] = Map.new();\n\
             m.entry(1).or_insert(words[0].clone());\n\
             println(f\"{m[1]}\");\n",
        ),
        (
            "option_replace",
            "let mut o: Option[ref String] = None;\n\
             o.replace(words[0].clone());\n\
             println(f\"{o.unwrap()}\");\n",
        ),
    ] {
        let src = format!("{head}{body}}}\n");
        refused_by_borrowck(name, &src, &["E0516", temp]);
    }
}

#[test]
fn extend_by_a_source_that_owns_its_items_is_refused() {
    let needle = "a reference to a value outlives the value (core-semantics.md §5.5)";
    let head = "fn main() {\n\
                let words: Vec[String] = [\"alpha\".to_string(), \"beta\".to_string()];\n";
    for (name, body) in [
        (
            "vec_temp",
            "let mut refs: Vec[ref String] = Vec.new();\n\
             refs.extend([words[0].clone()]);\n\
             println(f\"{refs[0]}\");\n",
        ),
        (
            "vec_place",
            "let mut refs: Vec[ref String] = Vec.new();\n\
             refs.extend(words);\n\
             println(f\"{refs[0]}\");\n",
        ),
        (
            "deque_temp",
            "let mut d: VecDeque[ref String] = VecDeque.new();\n\
             d.extend([words[0].clone()]);\n\
             println(f\"{d[0]} {words.len()}\");\n",
        ),
        (
            "literals",
            "let mut n: Vec[ref i64] = Vec.new();\n\
             n.extend([1, 2]);\n\
             println(f\"{n[0]} {words.len()}\");\n",
        ),
    ] {
        refused(name, &format!("{head}{body}}}\n"), &[needle]);
    }
    // The help names the place that can lend its items.
    refused(
        "vec_place_help",
        &format!(
            "{head}let mut refs: Vec[ref String] = Vec.new();\n\
             refs.extend(words);\n\
             println(f\"{{refs[0]}}\");\n}}\n"
        ),
        &["`words.iter()`"],
    );
}

#[test]
fn an_option_is_not_taken_or_replaced_while_its_value_is_borrowed() {
    for (name, write) in [
        ("take", "o.take()"),
        ("replace", "o.replace(\"b\".to_string())"),
    ] {
        refused_by_borrowck(
            name,
            &format!(
                "fn main() {{\n\
                 let mut o: Option[String] = Some(\"a\".to_string());\n\
                 let r = o.as_ref().unwrap();\n\
                 let old = {write};\n\
                 println(f\"{{r}} {{old.is_some()}}\");\n\
                 }}\n"
            ),
            &[
                "E0516",
                "cannot mutably borrow `o` while `r` still holds a borrow",
            ],
        );
    }
}
