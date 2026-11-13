//! `for x in it` over an owned parameter `it: own I` with `I:
//! Iterator[Item = T]` yields `T` (design.md § `Iterator` and `Iterable`).
//! The item was typed as `I` itself, so `x.abs()` was "no method 'abs' on
//! type parameter 'I'" and `let y: i64 = x` was "expected 'i64', found 'I'",
//! though this is the form B-2026-11-04-17's message points a borrowed
//! iterator to (B-2026-11-13-18). A `T` that only the bound's equation names
//! (`fn show[T: Display, I: Iterator[Item = T]]`) is solved at the call
//! from the iterator's item, where it was refused as `I.Item = T` not
//! satisfied.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn runs_to(tag: &str, src: &str, want: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-owned-iter-item-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} {lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn owned_iterator_param_item_takes_methods_and_annotations() {
    runs_to(
        "concrete",
        "fn total[I: Iterator[Item = i64]](it: own I) -> i64 {\n    \
         let mut t = 0;\n    for x in it { t += x.abs(); }\n    t\n}\n\
         fn firsts[I: Iterator[Item = i64]](it: own I) -> i64 {\n    \
         let mut t = 0;\n    for x in it { let y: i64 = x; t += y; }\n    t\n}\n\
         fn joined[I: Iterator[Item = Str]](it: own I) -> String {\n    \
         let mut v: Vec[String] = Vec.new();\n    for x in it { v.push(x.to_string()); }\n    v.join(\"|\")\n}\n\
         fn main() {\n    println(f\"{total([1, -2, 3].into_iter())}\");\n    \
         println(f\"{firsts([1, 2].into_iter())}\");\n    \
         println(joined(\"a,b,c\".split(\",\")));\n}\n",
        "6\n3\na|b|c\n",
    );
}

#[test]
fn owned_iterator_param_item_through_a_generic_t_and_a_where_clause() {
    runs_to(
        "generic",
        "fn show[T: Display, I: Iterator[Item = T]](it: own I) -> String {\n    \
         let mut s = String.new();\n    for x in it { s.push_str(x.to_string().as_str()); }\n    s\n}\n\
         fn total[I](it: own I) -> i64 where I: Iterator[Item = i64] {\n    \
         let mut t = 0;\n    for x in it { t += x.abs(); }\n    t\n}\n\
         fn names[T, I](it: own I) -> String where T: Display, I: Iterator[Item = T] {\n    \
         let mut s = \"\".to_string();\n    for x in it { s.push_str(x.to_string()); }\n    s\n}\n\
         fn cnt[T, I: Iterator[Item = T]](it: own I) -> i64 {\n    \
         let mut n = 0;\n    for _x in it { n += 1; }\n    n\n}\n\
         fn main() {\n    println(show([1, 2].into_iter()));\n    \
         let v: Vec[i64] = [1, -2];\n    println(total(v.into_iter()));\n    \
         let w: Vec[Str] = [\"a\", \"b\"];\n    println(names(w.into_iter()));\n    \
         let s: Vec[String] = [\"x\".to_string()];\n    println(cnt(s.into_iter()));\n}\n",
        "12\n3\nab\n1\n",
    );
}
