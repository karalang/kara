//! A table runs the program's own hashing code (collections.md § Hashing,
//! B-2026-10-25-31). A user `BuildHasher` makes its state with `build`, is
//! fed each key's bytes and gives the digest with `finish`; the bytes are
//! the ones collections.md § "What the built-in types feed" lists, and a
//! key type's own `Hash` impl feeds its part. A default or `FxBuildHasher`
//! table runs a key's own `Hash` too, digesting the bytes natively, so a
//! key whose `Hash` and `PartialEq` both ignore case is found under any
//! case, through the table's growth. Each program's expected output is
//! read off the spec, and both backends must print it.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

/// A hasher that prints the bytes each key fed it.
const BYTES: &str = r#"struct Tap { mut h: u64, mut seen: String }
impl Hasher for Tap {
    fn write(mut ref self, bytes: ref Slice[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            self.h = (self.h ^ (bytes[i] as u64)).wrapping_mul(16777619u64);
            self.seen = self.seen + " " + (bytes[i] as i64).to_string();
            i = i + 1;
        }
    }
    fn finish(ref self) -> u64 {
        println(f"finish{self.seen}");
        self.h
    }
}
struct TapBuild { }
impl BuildHasher for TapBuild {
    type Hasher = Tap;
    fn build(ref self) -> Tap { Tap { h: 2166136261u64, seen: "" } }
}
#[derive(PartialEq, Eq, Hash)]
struct P { x: i32, name: String }
#[derive(PartialEq, Eq, Hash)]
enum Shape { Dot, Line(u16) }
struct Tag { id: i64, note: String }
impl Hash for Tag {
    fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_u8(self.id as u8); }
}
impl PartialEq for Tag {
    fn eq(ref self, other: ref Tag) -> bool { self.id == other.id }
}
impl Eq for Tag {}
fn main() {
    let mut m: Map[i64, i64, TapBuild] = Map.new();
    m.insert(258, 10);
    println(f"get {m.get(258).unwrap()}");
    let mut s: Set[String, TapBuild] = Set.new();
    s.insert("ab");
    println(f"has {s.contains("ab")}");
    let mut t: Map[(bool, char), i64, TapBuild] = Map.new();
    t.insert((true, 'A'), 1);
    let mut q: Map[P, i64, TapBuild] = Map.new();
    q.insert(P { x: -2, name: "z" }, 1);
    let mut o: Map[Option[u8], i64, TapBuild] = Map.new();
    o.insert(Some(7u8), 1);
    o.insert(None, 2);
    let mut e: Set[Shape, TapBuild] = Set.new();
    e.insert(Shape.Line(3u16));
    e.insert(Shape.Dot);
    let mut v: Map[Vec[i16], i64, TapBuild] = Map.new();
    v.insert([1i16, -1i16], 1);
    let mut g: Map[(Tag, i8), i64, TapBuild] = Map.new();
    g.insert((Tag { id: 5, note: "x" }, -3i8), 1);
    println(f"{m.len()} {s.len()} {t.len()} {q.len()} {o.len()} {e.len()} {v.len()} {g.len()}");
}
"#;

/// What the spec says `BYTES` prints, on a little-endian target.
const BYTES_OUT: &str = r#"finish 2 1 0 0 0 0 0 0
finish 2 1 0 0 0 0 0 0
get 10
finish 97 98 255
finish 97 98 255
has true
finish 1 65 0 0 0
finish 254 255 255 255 122 255
finish 0 0 0 0 7
finish 1 0 0 0
finish 1 0 0 0 3 0
finish 0 0 0 0
finish 2 0 0 0 0 0 0 0 1 0 255 255
finish 5 253
1 1 1 1 2 2 1 1
"#;

const OWN: &str = r#"struct Name { text: String }
impl Hash for Name {
    fn hash[H: Hasher](ref self, hasher: mut ref H) {
        println(f"hash {self.text}");
        hasher.write(self.text.to_lowercase().bytes());
    }
}
impl PartialEq for Name {
    fn eq(ref self, other: ref Name) -> bool {
        self.text.to_lowercase() == other.text.to_lowercase()
    }
}
impl Eq for Name {}
fn main() {
    let mut m: Map[Name, i64] = Map.new();
    m.insert(Name { text: "Ada" }, 1);
    m.insert(Name { text: "ADA" }, 2);
    println(f"len {m.len()}");
    println(f"get {m.get(Name { text: "ada" }).unwrap()}");
    let mut s: Set[(Name, i64)] = Set.new();
    s.insert((Name { text: "Bo" }, 3));
    println(f"has {s.contains((Name { text: "Bo" }, 3))}");
    let mut f: Map[Name, i64, FxBuildHasher] = Map.new();
    f.insert(Name { text: "Cy" }, 4);
    println(f"fx {f.get(Name { text: "cY" }).unwrap()}");
    let mut big: Map[Name, i64] = Map.new();
    for i in 0..40 {
        big.insert(Name { text: f"k{i}" }, i);
    }
    println(f"big {big.len()} {big.get(Name { text: "K39" }).unwrap()}");
}
"#;

fn run_both(name: &str, src: &str, want: &str) {
    let dir = std::env::temp_dir().join(format!("karac-hasher-{name}-{}", std::process::id()));
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
fn a_user_hasher_is_fed_the_bytes_the_spec_lists() {
    if cfg!(target_endian = "big") {
        return;
    }
    run_both("bytes", BYTES, BYTES_OUT);
}

#[test]
fn a_default_or_fx_table_runs_a_keys_own_hash() {
    let mut want = String::from(
        "hash Ada\nhash ADA\nlen 1\nhash ada\nget 2\nhash Bo\nhash Bo\nhas true\nhash Cy\nhash cY\nfx 4\n",
    );
    for i in 0..40 {
        want.push_str(&format!("hash k{i}\n"));
    }
    want.push_str("hash K39\nbig 40 39\n");
    run_both("own", OWN, &want);
}
