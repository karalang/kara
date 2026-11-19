//! Rows of 2026-11-19 from the design.md §9 and §11 probes:
//! - B-2026-11-19-4: `.into()` from a `Str`, or a literal, to `String`
//!   (design.md §9 Conversion traits lists `impl From[Str] for String`),
//!   and `.into()` taking its target from a library method's parameter
//!   (`v.push("mn".into())` on a `Vec[String]`), as a user method's does.
//! - B-2026-11-19-5: `v[a..b] = other` takes a `Vec`, an array or a list
//!   literal on the right, read as a slice and not moved (design.md §9
//!   Index and IndexMut).
//! - B-2026-11-19-10: a list literal under a `SortedSet[T]` builds one, as
//!   under a `Set[T]`.
//! - B-2026-11-19-20: `#[allow(deprecated)]` on an item covers its own
//!   header (an impl's self type, a function's or method's signature).

use karac::check_source;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn errors(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn modes() -> Vec<&'static str> {
    let mut m = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        m.push("__mir-llvm-run");
    }
    m
}

/// `src`'s stdout under each backend, `None` for a compiled lane with no
/// runtime archive.
fn outputs(src: &str) -> Vec<(&'static str, Option<String>)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    modes()
        .into_iter()
        .map(|mode| {
            let n = N.fetch_add(1, Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("karac-rows1119-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("main.kara"), src).unwrap();
            let out = Command::new(env!("CARGO_BIN_EXE_karac"))
                .args([mode, "main.kara"])
                .current_dir(&dir)
                .output()
                .unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            let err = String::from_utf8_lossy(&out.stderr).into_owned();
            if mode != "__mir-run" && err.contains("runtime archive") {
                return (mode, None);
            }
            assert!(out.status.success(), "{mode}: {err}\n{src}");
            (
                mode,
                Some(String::from_utf8_lossy(&out.stdout).into_owned()),
            )
        })
        .collect()
}

fn assert_prints(src: &str, want: &str) {
    assert!(errors(src).is_empty(), "{src}\n{:#?}", errors(src));
    for (mode, out) in outputs(src) {
        if let Some(out) = out {
            assert_eq!(out, want, "{mode}\n{src}");
        }
    }
}

#[test]
fn str_and_literal_into_string() {
    assert_prints(
        "struct Name {\n    v: String,\n}\nfn show(s: Str) -> String {\n    let t: String = s.into();\n    t\n}\nfn tail(s: Str) -> String {\n    s.into()\n}\nfn main() {\n    let a: String = \"cd\".into();\n    println(a);\n    println(show(\"ef\"));\n    println(tail(\"ij\"));\n    let n = Name { v: \"kl\".into() };\n    println(n.v);\n    let r: Result[String, String] = Err(\"op\".into());\n    println(r.unwrap_err());\n}\n",
        "cd\nef\nij\nkl\nop\n",
    );
}
#[test]
fn into_takes_a_library_method_parameter_s_type() {
    assert_prints(
        "struct Meters {\n    v: i64,\n}\nimpl From[i64] for Meters {\n    fn from(x: i64) -> Meters {\n        Meters { v: x }\n    }\n}\nfn main() {\n    let mut v: Vec[String] = Vec.new();\n    v.push(\"mn\".into());\n    let s: Str = \"op\";\n    v.push(s.into());\n    v.insert(0, \"z\".into());\n    println(f\"{v[0]} {v[1]} {v[2]}\");\n    let mut ms: Vec[Meters] = Vec.new();\n    ms.push(5.into());\n    let mut d: VecDeque[Meters] = VecDeque.new();\n    d.push_back(3.into());\n    d.push_front(4.into());\n    println(f\"{ms[0].v} {d[0].v} {d[1].v}\");\n    let mut m: Map[String, String] = Map.new();\n    m.insert(\"a\".into(), \"b\".into());\n    let mut set: SortedSet[String] = SortedSet.new();\n    set.insert(\"x\".into());\n    println(f\"{m[\"a\"]} {set.contains(\"x\")}\");\n}\n",
        "z mn op\n5 4 3\nb true\n",
    );
}
#[test]
fn range_assign_takes_a_vec_an_array_or_a_list() {
    assert_prints(
        "fn main() {\n    let mut v: Vec[u8] = [1, 2, 3, 4];\n    v[1..3] = [200, 201];\n    println(v);\n    let x: Vec[u8] = [9, 8];\n    v[2..] = x;\n    println(v);\n    println(x.len());\n    let mut a = [0; 4];\n    a[..2] = [5, 6];\n    let src = [7, 7];\n    a[2..4] = src;\n    println(a);\n    let r = ref x;\n    v[..2] = r;\n    println(v);\n}\n",
        "[1, 200, 201, 4]\n[1, 200, 9, 8]\n2\n[5, 6, 7, 7]\n[9, 8, 9, 8]\n",
    );
}
#[test]
fn a_list_literal_builds_a_sorted_set() {
    assert_prints(
        "fn total(s: SortedSet[i64]) -> i64 {\n    let mut t = 0;\n    for x in s {\n        t = t + x;\n    }\n    t\n}\nfn main() {\n    let s: SortedSet[i64] = [3, 1, 2, 3];\n    println(s.len());\n    for x in s {\n        println(x);\n    }\n    let e: SortedSet[String] = [];\n    println(e.len());\n    let mut w: SortedSet[String] = [\"pear\".to_string(), \"apple\".to_string()];\n    w.insert(\"fig\".to_string());\n    for x in w {\n        println(x);\n    }\n    println(total([9, 4, 4]));\n}\n",
        "3\n1\n2\n3\n0\napple\nfig\npear\n13\n",
    );
}
#[test]
fn range_assign_refuses_another_element_type() {
    let errs = errors(
        "fn main() {\n    let mut v = [1, 2, 3, 4];\n    let w: Vec[i32] = [5, 6];\n    v[0..2] = w;\n}\n",
    );
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(errs[0].contains("Vec[i32]"), "{errs:#?}");
}

fn deprecated_warnings(src: &str) -> Vec<String> {
    check_source(src)
        .into_iter()
        .filter(|d| d.message.contains("use of deprecated item"))
        .map(|d| format!("{}:{} {}", d.line, d.column, d.severity))
        .collect()
}

#[test]
fn allow_deprecated_covers_the_item_s_own_header() {
    let allowed = "#[deprecated]\nstruct Old {\n    v: i64,\n}\n#[allow(deprecated)]\nimpl Old {\n    fn get(self) -> i64 {\n        self.v\n    }\n}\n#[allow(deprecated)]\nfn mk() -> Old {\n    Old { v: 1 }\n}\n#[allow(deprecated)]\nfn take(o: Old) -> i64 {\n    o.v\n}\nstruct New {\n    v: i64,\n}\nimpl New {\n    #[allow(deprecated)]\n    fn old(self) -> Old {\n        Old { v: self.v }\n    }\n}\n#[allow(deprecated)]\nfn main() {\n    println(take(mk()));\n    println(New { v: 3 }.old().get());\n}\n";
    assert_eq!(deprecated_warnings(allowed), Vec::<String>::new());
    // Without the allow, each header still warns.
    let bare = "#[deprecated]\nstruct Old {\n    v: i64,\n}\nimpl Old {\n    fn get(self) -> i64 {\n        self.v\n    }\n}\nfn mk() -> Old {\n    Old { v: 1 }\n}\nfn main() {\n    println(mk().get());\n}\n";
    assert_eq!(
        deprecated_warnings(bare),
        vec!["5:6 warning", "10:12 warning", "11:5 warning"],
    );
}
