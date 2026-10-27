//! A hashed table compares keys through their `PartialEq` even when only a
//! part of the key has a hand-written one (collections.md § Hashing,
//! B-2026-10-27-26): a tuple, a derived struct, an `Option` and a `Vec` over
//! a case-insensitive `Name` find their keys under any case, as `==` on
//! the same values does, on both backends. The table that grows past its
//! first buckets keeps one entry per case-folded name.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

const SRC: &str = r#"struct Name { text: String }
impl Hash for Name {
    fn hash[H: Hasher](ref self, hasher: mut ref H) {
        hasher.write(self.text.to_lowercase().bytes());
    }
}
impl PartialEq for Name {
    fn eq(ref self, other: ref Name) -> bool {
        self.text.to_lowercase() == other.text.to_lowercase()
    }
}
impl Eq for Name {}
#[derive(PartialEq, Eq, Hash)]
struct Rec { name: Name, n: i64 }
fn main() {
    let mut s: Set[(Name, i64)] = Set.new();
    s.insert((Name { text: "Bo" }, 3));
    s.insert((Name { text: "BO" }, 3));
    println(f"tuple {s.len()} {s.contains((Name { text: "bO" }, 3))} {s.contains((Name { text: "bO" }, 4))}");
    let mut m: Map[Rec, i64] = Map.new();
    m.insert(Rec { name: Name { text: "Ada" }, n: 1 }, 10);
    m.insert(Rec { name: Name { text: "ADA" }, n: 1 }, 11);
    println(f"struct {m.len()} {m.get(Rec { name: Name { text: "ada" }, n: 1 }).unwrap()}");
    let mut o: Set[Option[Name]] = Set.new();
    o.insert(Some(Name { text: "Cy" }));
    o.insert(None);
    println(f"option {o.len()} {o.contains(Some(Name { text: "CY" }))} {o.contains(None)}");
    let mut v: Map[Vec[Name], i64, FxBuildHasher] = Map.new();
    v.insert([Name { text: "a" }, Name { text: "b" }], 1);
    println(f"vec {v.contains_key([Name { text: "A" }, Name { text: "B" }])} {v.contains_key([Name { text: "A" }])}");
    let mut big: Set[(Name, i64)] = Set.new();
    for i in 0..50 {
        big.insert((Name { text: f"K{i}" }, i));
    }
    for i in 0..50 {
        big.insert((Name { text: f"k{i}" }, i));
    }
    println(f"big {big.len()} {big.contains((Name { text: "k49" }, 49))}");
}
"#;

fn run_both(name: &str, src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!("karac-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let mut ran = 0;
    for cmd in ["__mir-run", "__mir-llvm-run"] {
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .arg(cmd)
            .arg(&file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        if cmd == "__mir-llvm-run" && stderr.contains("undefined reference") {
            if common::link_or_skip(Err(stderr)).is_none() {
                continue;
            }
            unreachable!("link_or_skip passes only a successful link");
        }
        assert!(out.status.success(), "{cmd}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{cmd}");
        ran += 1;
    }
    assert!(ran >= 1, "neither backend ran");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_table_key_whose_part_has_its_own_eq_is_found_by_it() {
    run_both(
        "key-part-eq",
        SRC,
        "tuple 1 true false\nstruct 1 11\noption 2 true true\nvec true false\nbig 50 true\n",
    );
}
