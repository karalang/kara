//! Kata 2208 (minimum operations to halve an array's sum), three gaps found
//! writing the heap arms. B-2026-11-15-78: `T.cmp` under `T: Ord` at `F64`, as
//! `Reverse[F64]` in a `PriorityQueue` does, was refused by the MIR builder
//! as not lowered. B-2026-11-15-79: a field of a generic struct bound by a `let`
//! struct pattern took the declared `T`, so `let Reverse { value: top } =
//! heap.pop().unwrap()` over `i64` read `top` as moved after one use.
//! B-2026-11-15-80: `karac fix` renamed `let h: BinaryHeap<i64> = it.collect();`
//! to a `PriorityQueue`, which pops the smallest first.

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(tag: &str, src: &str, args: &[&str]) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2208-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), format!("{src}\n")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(Path::new(&dir))
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

#[test]
fn kata_2208_reverse_f64_heap_pops_largest() {
    let (ok, out, _) = karac(
        "f64",
        "import std.cmp.Reverse;\nfn main() {\n    let mut q: PriorityQueue[Reverse[F64]] = PriorityQueue.new();\n    for x in [2.5, 9.0, 4.0] {\n        q.push(Reverse { value: F64.from(x) });\n    }\n    let Reverse { value: top } = q.pop().unwrap();\n    println(f\"{top.value} {Reverse { value: F64.from(1.0) } < Reverse { value: F64.from(2.0) }}\");\n}",
        &["__mir-run"],
    );
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "9.0 false", "{out}");
}

#[test]
fn kata_2208_generic_struct_pattern_binds_copy_field() {
    let src = "import std.cmp.Reverse;\nstruct W[T] {\n    value: T,\n}\nfn main() {\n    let w: W[i64] = W { value: 6 };\n    let W { value: top } = w;\n    let Reverse { value: r } = Reverse { value: 10 };\n    println(f\"{top / 2} {top / 3} {r + r}\");\n}";
    let (ok, out, _) = karac("copy", src, &["check"]);
    assert!(ok, "{out}");
    let (ok, out, _) = karac("copy-run", src, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "3 2 20", "{out}");
}

#[test]
fn kata_2208_generic_struct_pattern_still_moves_a_string() {
    let (ok, out, _) = karac(
        "move",
        "struct W[T] {\n    value: T,\n}\nfn eat(s: own String) -> i64 {\n    s.len()\n}\nfn main() {\n    let w: W[String] = W { value: \"ab\".to_string() };\n    let W { value: s } = w;\n    let a = eat(s);\n    let b = eat(s);\n    println(f\"{a} {b}\");\n}",
        &["check"],
    );
    assert!(!ok, "{out}");
    assert!(out.contains("moved"), "{out}");
}

#[test]
fn kata_2208_collected_binary_heap_stays_largest_first() {
    let src = "fn main() {\n    let nums = [3, 9, 1];\n    let mut heap: BinaryHeap<i64> = nums.iter().map(|x| x * 2).collect();\n    println(f\"{heap.pop().unwrap()}\");\n}";
    // `<i64>` takes Kāra's brackets in the first round, and the heap its
    // name and its order in the second.
    let (_, _, first) = karac("heap", src, &["fix"]);
    let (_, out, fixed) = karac("heap2", &first, &["fix"]);
    assert!(
        fixed.contains(
            "let mut heap: PriorityQueue[i64] = PriorityQueue.max_first_from(nums.iter().map(|x| x * 2).collect());"
        ),
        "{out}\n{fixed}"
    );
    let (ok, out, _) = karac("heap-run", &fixed, &["__mir-run"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim(), "18", "{out}");
}

#[test]
fn kata_2208_collected_reverse_heap_takes_the_plain_rename() {
    let (_, _, fixed) = karac(
        "rev",
        "use std::cmp::Reverse;\nfn main() {\n    let nums = [3, 9, 1];\n    let mut heap: BinaryHeap<Reverse<i64>> = nums.iter().map(|x| Reverse(x)).collect();\n    println(f\"{heap.len()}\");\n}",
        &["fix"],
    );
    let (_, out, fixed) = karac("rev2", &fixed, &["fix"]);
    assert!(!fixed.contains("max_first"), "{out}\n{fixed}");
}
