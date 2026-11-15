//! The MIR interpreter's decoded steps (`src/mir/interp_fast.rs`) take
//! over what an iterator chain's `next` and a small call are made of
//! (B-2026-11-14-62): a move of a value that can own something (an
//! adaptor's inner iterator taken out of its `Option` and put back), a
//! move out of an enum's payload, a store through a reference into a
//! field that owns, a `StorageDead` of a local already moved out, a
//! reference to a field of a local tuple, a `bool as i64`, a tuple of
//! scalars passed or wrapped by value, and the frame's return. Each falls
//! back to the general step when the value is not what it reads. These pin
//! that the answers are the general steps' (each is checked by hand: the
//! pair count is the sum over masks of k(6 - k), 6 * 5 * 2^4).

use std::process::Command;

const SRC: &str = r#"
struct Holder { name: String, tags: Vec[String], hits: i64 }

fn bump(h: mut ref Holder, tag: own String) {
    h.hits += 1;
    h.tags.push(tag);
}

fn rename(h: mut ref Holder, name: own String) {
    h.name = name;
}

fn pairs_kept(n: i64, mask: i64) -> i64 {
    (0..n).flat_map(|i| (0..n).map(|j| (i, j))).filter(|(i, j)| (mask >> i) & 1 == 1 and (mask >> j) & 1 == 0).count()
}

fn first_pair(v: Vec[(i64, i64)]) -> Option[(i64, i64)] {
    let mut best: Option[(i64, i64)] = None;
    for p in v.iter() {
        match best {
            Some(b) => if p.0 + p.1 > b.0 + b.1 { best = Some(p); },
            None => best = Some(p),
        }
    }
    best
}

fn swap_parts(t: (i64, i64)) -> (i64, i64) {
    let mut u = t;
    let a = mut ref u.0;
    *a += 10;
    let b = ref u.1;
    (*b, u.0)
}

fn flags(v: Vec[bool]) -> i64 {
    let mut n = 0;
    for b in v.iter() { n += b as i64; }
    n
}

fn take_and_put(slot: own Option[Vec[String]], extra: own String) -> Option[Vec[String]] {
    let mut slot = slot;
    match slot.take() {
        Some(v) => {
            let mut v = v;
            v.push(extra);
            slot = Some(v);
        }
        None => slot = Some([extra].to_vec()),
    }
    slot
}

fn main() {
    let mut h = Holder { name: "a".to_string(), tags: Vec.new(), hits: 0 };
    bump(mut h, "x".to_string());
    bump(mut h, "y".to_string());
    rename(mut h, "b".to_string());
    println(f"{h.name} {h.tags} {h.hits}");
    let mut total = 0;
    for mask in 0..64 { total += pairs_kept(6, mask); }
    println(f"pairs {total}");
    println(f"{first_pair([(1, 2), (5, 0), (2, 2)].to_vec())} {first_pair(Vec.new())}");
    println(f"{swap_parts((1, 2))}");
    println(f"flags {flags([true, false, true, true].to_vec())}");
    let mut s: Option[Vec[String]] = None;
    for w in ["p", "q", "r"] { s = take_and_put(s, w.to_string()); }
    println(f"{s}");
    let words: Vec[String] = (0..5).map(|i| f"w{i}").collect();
    let lens: i64 = words.iter().flat_map(|w| w.chars()).filter(|c| c != 'w').count();
    println(f"chars {lens}");
}
"#;

#[test]
fn decoded_owning_moves_give_the_general_answers() {
    let dir = std::env::temp_dir().join(format!("karac-decoded-moves-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, SRC).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("__mir-run")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "b [x, y] 2\npairs 480\nSome((5, 0)) None\n(2, 11)\nflags 3\nSome([p, q, r])\nchars 5\n"
    );
}
