//! Lookups on a `Map`, `SortedMap`, `Set` or `SortedSet` whose keys are
//! shared references (B-2026-11-23-3).
//!
//! The natives read the key of `m.get(k)` as the key type, a `ref i64`, and
//! the `i64` itself went in its place: every keyed lookup checked clean,
//! answered right under the interpreter, and segfaulted compiled, reading
//! the key's value as an address. The library's own `get_mut` read its
//! reference key through as a number at `self[key]`. Lent for the call, the
//! key is not kept by the collection either, so a key that dies before the
//! map is not a borrow conflict.

mod common;

use std::process::Command;

fn karac(name: &str, src: &str, cmd: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-ref-key-lookups-{name}-{cmd}-{}",
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

#[test]
fn ref_key_lookups_on_maps() {
    for map in ["Map", "SortedMap"] {
        runs_everywhere(
            &format!("ref_key_{map}"),
            &format!(
                "fn main() {{\n\
                 let a: i64 = 1;\n\
                 let b: i64 = 2;\n\
                 let k: i64 = 2;\n\
                 let mut m: {map}[ref i64, i64] = {map}.new();\n\
                 m.insert(a, 5);\n\
                 m.insert(b, 6);\n\
                 println(f\"{{m[1]}} {{m[k]}} {{m[b]}} {{m[k * 1]}} {{m.get_or(1, 0)}} {{m.get_or(7, 0)}}\");\n\
                 println(f\"{{m.get(1).is_some()}} {{m.get(k).unwrap()}} {{m.get(a).unwrap()}} {{m.get(9).is_none()}}\");\n\
                 println(f\"{{m.contains_key(1)}} {{m.contains_key(k)}} {{m.contains_key(3)}}\");\n\
                 *m.get_mut(k).unwrap() += 10;\n\
                 *m.get_mut(1).unwrap() += 20;\n\
                 println(f\"{{m[1]}} {{m[2]}} {{m.remove(k).unwrap()}} {{m.remove(2).is_none()}} {{m.len()}}\");\n\
                 }}\n"
            ),
            "5 6 6 6 5 0\ntrue 6 5 true\ntrue true false\n25 16 16 true 1\n",
        );
    }
}

#[test]
fn ref_key_lookups_on_sets() {
    for set in ["Set", "SortedSet"] {
        runs_everywhere(
            &format!("ref_key_{set}"),
            &format!(
                "fn main() {{\n\
                 let nums: Vec[i64] = [1, 2, 3];\n\
                 let mut s: {set}[ref i64] = {set}.new();\n\
                 for x in nums.iter() {{\n\
                 s.insert(x);\n\
                 }}\n\
                 let k: i64 = 2;\n\
                 println(f\"{{s.contains(1)}} {{s.contains(k)}} {{s.contains(nums[2])}} {{s.contains(4)}}\");\n\
                 let mut gone = 0;\n\
                 for i in 0..5 {{\n\
                 let key: i64 = i;\n\
                 if s.remove(key) {{\n\
                 gone += 1;\n\
                 }}\n\
                 }}\n\
                 println(f\"{{gone}} {{s.len()}}\");\n\
                 }}\n"
            ),
            "true true true false\n3 0\n",
        );
    }
}

#[test]
fn ref_key_lookups_with_a_key_that_dies_first() {
    // The key is lent for the call only: a loop's or a block's key
    // that dies while the map lives on is not refused.
    runs_everywhere(
        "ref_key_dies_first",
        "fn main() {\n\
         let nums: Vec[i64] = [1, 2, 3, 2];\n\
         let mut m: Map[ref i64, i64] = Map.new();\n\
         for x in nums.iter() {\n\
         *m.entry(x).or_insert(0) += 1;\n\
         }\n\
         for i in 0..4 {\n\
         let key: i64 = i;\n\
         if m.contains_key(key) {\n\
         *m.get_mut(key).unwrap() += 10;\n\
         }\n\
         match m.get_mut(key) {\n\
         Some(v) => { *v += i; }\n\
         None => {}\n\
         }\n\
         }\n\
         let words: Vec[String] = [\"a\".to_string(), \"b\".to_string()];\n\
         let mut w: Map[ref String, i64] = Map.new();\n\
         w.insert(words[0], 1);\n\
         w.insert(words[1], 2);\n\
         {\n\
         let k = \"a\".to_string();\n\
         let v = w.get_mut(k);\n\
         *v.unwrap() += 5;\n\
         let r = \"b\".to_string();\n\
         let _ = w.remove(r);\n\
         }\n\
         println(f\"{m[1]} {m[2]} {m[3]} {w[words[0]]} {w.len()}\");\n\
         }\n",
        "12 14 14 6 1\n",
    );
}

#[test]
fn ref_key_lookups_with_text_keys() {
    runs_everywhere(
        "ref_key_text",
        "fn main() {\n\
         let a = \"a\".to_string();\n\
         let b = \"b\".to_string();\n\
         let mut count: Map[ref String, i64] = Map.new();\n\
         count.insert(a, 5);\n\
         count.insert(b, 6);\n\
         let k = \"b\".to_string();\n\
         println(f\"{count.get(k).unwrap()} {count.contains_key(\"a\")} {count[k]} {count[\"a\"]} {count.get(\"zz\").is_none()}\");\n\
         let mut s: Set[ref String] = Set.new();\n\
         s.insert(a);\n\
         println(f\"{s.contains(\"a\")} {s.contains(k)} {s.contains(b.clone())}\");\n\
         }\n",
        "6 true 6 5 true\ntrue false false\n",
    );
}

#[test]
fn ref_key_lookup_with_a_narrower_key() {
    runs_everywhere(
        "ref_key_i32",
        "fn main() {\n\
         let nums: Vec[i32] = [1, 2];\n\
         let mut m: SortedMap[ref i32, i64] = SortedMap.new();\n\
         for x in nums.iter() {\n\
         m.insert(x, 1);\n\
         }\n\
         println(f\"{m.contains_key(1)} {m[2]} {m.get(3).is_none()}\");\n\
         }\n",
        "true 1 true\n",
    );
}

#[test]
fn ref_key_insert_of_a_key_that_dies_first_is_still_refused() {
    // Only lookups lend for the call: `insert` keeps the reference.
    let src = "fn main() {\n\
               let mut m: Map[ref String, i64] = Map.new();\n\
               {\n\
               let t = \"t\".to_string();\n\
               m.insert(t, 1);\n\
               }\n\
               println(f\"{m.len()}\");\n\
               }\n";
    for cmd in ["check", "__mir-run"] {
        let out = karac("ref_key_insert_dies", src, cmd);
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!out.status.success(), "{cmd}: accepted\n{src}");
        assert!(shown.contains("E0516"), "{cmd}: {shown}");
    }
}
