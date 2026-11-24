//! `Vec.set_len` is an `unsafe fn` (docs/library/collections.md): it
//! forgets the elements past the new length without dropping them, which
//! `VecIntoIter`'s drop relies on after reading its elements out. A call
//! outside `unsafe { }` is refused, a shrink drops nothing, and the
//! interpreter refuses a growth, which has no initialized slots to show.

use std::process::Command;

fn karac(src: &str, cmd: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-vec-set-len-{cmd}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args([cmd, "a.kara"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    (out.status.success(), text)
}

#[test]
fn set_len_outside_unsafe_is_refused() {
    let (ok, out) = karac(
        "fn main() {\n    let mut v = [1, 2, 3];\n    v.set_len(1);\n    println(v.len());\n}\n",
        "check",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains(
            "call to `unsafe fn Vec.set_len` must be wrapped in an `unsafe { ... }` block"
        ),
        "{out}"
    );
}

#[test]
fn set_len_shrinks_without_dropping() {
    let src = "struct Loud { n: i64 }\n\
impl Drop for Loud {\n\
    fn drop(mut ref self) { print(f\"d{self.n} \"); }\n\
}\n\
fn main() {\n\
    let mut v = [Loud { n: 1 }, Loud { n: 2 }, Loud { n: 3 }];\n\
    let p = v.as_mut_ptr();\n\
    let last = unsafe { p.offset(2).read() };\n\
    unsafe { v.set_len(2); }\n\
    println(f\"len {v.len()}\");\n\
    drop(last);\n\
    println(\"\");\n\
}\n";
    let (ok, out) = karac(src, "__mir-run");
    assert!(ok, "{out}");
    assert_eq!(out, "len 2\nd3 \nd1 d2 ");
}

#[test]
fn set_len_past_the_length_is_refused_by_the_interpreter() {
    let (ok, out) = karac(
        "fn main() {\n    let mut v = [1, 2, 3];\n    unsafe { v.set_len(5); }\n    println(v.len());\n}\n",
        "__mir-run",
    );
    assert!(!ok, "{out}");
    assert!(
        out.contains("Vec.set_len(5) on a vector of length 3: the length may only shrink"),
        "{out}"
    );
}

/// The interpreter made a borrow of a borrow the borrow in every native's
/// result, `as_mut_ptr`'s too, so a raw pointer into a `Vec[ref String]`
/// pointed into its first element's referent and `offset` walked that
/// referent's neighbours: `into_iter` over a `SortedSet[ref String]` read
/// `[x, y, x]` for `[x, y, zz]` (B-2026-11-20-109).
#[test]
fn a_raw_pointer_into_a_vec_of_refs_walks_its_slots() {
    let src = "fn main() {\n\
    let names: Vec[String] = [\"x\", \"y\", \"x\", \"zz\"];\n\
    let mut v: Vec[ref String] = Vec.new();\n\
    v.push(names[3]);\n\
    v.push(names[0]);\n\
    let p = v.as_mut_ptr();\n\
    // Safety: both slots are in bounds and are read, not moved.\n\
    let (a, b) = unsafe { (p.read(), p.offset(1).read()) };\n\
    println(f\"{a} {b}\");\n\
    let mut s: SortedSet[ref String] = SortedSet.new();\n\
    for n in names.iter() { s.insert(n); }\n\
    let w: Vec[ref String] = s.into_iter().collect();\n\
    println(f\"{w}\");\n\
}\n";
    let (ok, out) = karac(src, "__mir-run");
    assert!(ok, "{out}");
    assert_eq!(out, "zz x\n[x, y, zz]\n");
}
