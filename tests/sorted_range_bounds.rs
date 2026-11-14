//! A sorted set's or map's `range` / `range_inclusive` finds each bound by
//! one search of the tree (B-2026-11-14-16). The bounds were found by a
//! linear walk over the elements; these pin what the search returns:
//! half-open and closed bounds, reversed and empty ranges, both ends of a
//! range iterator, string keys, and a key with a user `Ord` that orders
//! descending, on both lanes.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

fn on_both_lanes(tag: &str, src: &str, want: &str) {
    let path = std::env::temp_dir().join(format!(
        "karac-sorted-range-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    for lane in [&["run"][..], &["run", "--interp"][..]] {
        let out = karac(lane, &path);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{lane:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn sorted_range_bounds_on_builtin_keys() {
    on_both_lanes(
        "builtin",
        r#"fn main() {
    let mut s: SortedSet[i64] = SortedSet.new();
    for x in [5, 1, 9, 3, 7] {
        s.insert(x);
    }
    let a: Vec[i64] = s.range(3, 8).map(|x| *x).collect();
    println(f"{a}");
    let b: Vec[i64] = s.range_inclusive(3, 9).map(|x| *x).collect();
    println(f"{b}");
    println(f"{*s.range(0, 6).next_back().unwrap()} {*s.range(6, 100).next().unwrap()}");
    println(f"{s.range(8, 3).count()} {s.range(10, 20).count()} {s.range_inclusive(9, 9).count()}");
    let mut m: SortedMap[String, i64] = SortedMap.new();
    for (k, v) in [("b", 2), ("d", 4), ("a", 1), ("c", 3)] {
        m.insert(k.to_string(), v);
    }
    for (k, v) in m.range("b".to_string(), "d".to_string()) {
        println(f"{k}={v}");
    }
    for (k, v) in m.range_inclusive("b".to_string(), "d".to_string()) {
        println(f"{k}:{v}");
    }
}
"#,
        "[3, 5, 7]\n[3, 5, 7, 9]\n5 7\n0 0 1\nb=2\nc=3\nb:2\nc:3\nd:4\n",
    );
}

#[test]
fn sorted_range_bounds_on_a_user_ord_key() {
    on_both_lanes(
        "user-ord",
        r#"struct Gap {
    len: i64,
    at: i64,
}

impl PartialEq for Gap {
    fn eq(ref self, other: ref Gap) -> bool {
        self.len == other.len and self.at == other.at
    }
}

impl Eq for Gap {}

impl PartialOrd for Gap {
    fn partial_cmp(ref self, other: ref Gap) -> Option[Ordering] {
        Some(self.cmp(other))
    }
}

impl Ord for Gap {
    fn cmp(ref self, other: ref Gap) -> Ordering {
        if self.len != other.len {
            other.len.cmp(self.len)
        } else {
            self.at.cmp(other.at)
        }
    }
}

fn main() {
    let mut s: SortedSet[Gap] = SortedSet.new();
    for (l, a) in [(3, 0), (5, 2), (3, 9), (1, 4), (5, 0)] {
        s.insert(Gap { len: l, at: a });
    }
    for g in s.range(Gap { len: 5, at: 1 }, Gap { len: 3, at: 9 }) {
        println(f"{g.len}@{g.at}");
    }
    println(s.range_inclusive(Gap { len: 5, at: 1 }, Gap { len: 3, at: 9 }).count());
    let mut m: SortedMap[i64, String] = SortedMap.new();
    for k in [40, 10, 30, 20] {
        m.insert(k, f"v{k}");
    }
    for (k, v) in m.range(15, 35) {
        println(f"{k}:{v}");
    }
    println(f"{m.range(41, 50).count()} {m.range(30, 30).count()} {m.range_inclusive(30, 30).count()}");
}
"#,
        "5@2\n3@0\n3\n20:v20\n30:v30\n0 0 1\n",
    );
}
