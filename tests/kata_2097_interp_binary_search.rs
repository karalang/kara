//! Kata 2097's renamed-ids arm finds each pair's value with
//! `values.binary_search(v)` over up to 200001 sorted values. The MIR
//! interpreter's `binary_search` read every element and ranked them all on
//! each call, so a search was linear and the arm's 100000-pair chain did not
//! finish in 15 minutes under `__mir-run` (0.6 s compiled). It now reads
//! only the probed elements, with std's probe order, as the compiled
//! `karac_mir_bsearch` does, so a run of equal elements answers the same
//! index on both.

fn run(src: &str) -> String {
    karac::mir::lower::run_source(src)
        .expect("MIR interpreter")
        .output
}

/// The index each backend gives is std's: among equal elements, the one
/// std's search lands on. A user `Ord` is honoured.
#[test]
fn kata_2097_binary_search_answers_as_std() {
    let src = "struct Rev { k: i64 }
impl PartialEq for Rev { fn eq(ref self, other: ref Rev) -> bool { self.k == other.k } }
impl Eq for Rev {}
impl PartialOrd for Rev { fn partial_cmp(ref self, other: ref Rev) -> Option[Ordering] { Some(self.cmp(other)) } }
impl Ord for Rev { fn cmp(ref self, other: ref Rev) -> Ordering { other.k.cmp(self.k) } }

fn main() {
    let v = [1, 2, 2, 2, 3, 5, 5, 5, 5, 8].to_vec();
    for x in 0..10 {
        print(f\"{v.binary_search(x)} \");
    }
    println(\"\");
    let r = [Rev { k: 9 }, Rev { k: 7 }, Rev { k: 7 }, Rev { k: 4 }, Rev { k: 1 }].to_vec();
    for k in [9, 8, 7, 4, 1, 0] {
        print(f\"{r.binary_search(Rev { k })} \");
    }
    println(\"\");
    let words = [\"ant\".to_string(), \"bee\".to_string(), \"cat\".to_string()].to_vec();
    println(f\"{words.binary_search(\"bee\".to_string())} {words.binary_search(\"dog\".to_string())}\");
    let empty: Vec[i64] = Vec.new();
    println(f\"{empty.binary_search(1)} {v[2..8].binary_search(5)}\");
}
";
    let v = [1i64, 2, 2, 2, 3, 5, 5, 5, 5, 8];
    let show = |r: Result<usize, usize>| r.map_or("None".to_string(), |i| format!("Some({i})"));
    let mut want = String::new();
    for x in 0..10 {
        want += &format!("{} ", show(v.binary_search(&x)));
    }
    want += "\n";
    let r = [9i64, 7, 7, 4, 1];
    for k in [9i64, 8, 7, 4, 1, 0] {
        want += &format!("{} ", show(r.binary_search_by(|e| k.cmp(e))));
    }
    want += "\nSome(1) None\n";
    want += &format!("None {}\n", show(v[2..8].binary_search(&5)));
    assert_eq!(run(src), want);
}

/// 30000 searches over 30000 values: linear searches would read 9 * 10^8
/// elements.
#[test]
fn kata_2097_binary_search_is_logarithmic() {
    let src = "fn main() {
    let n = 30000;
    let values: Vec[i64] = (0..n).map(|i| i * 3).collect();
    let mut s = 0;
    for i in 0..n {
        s += values.binary_search(i * 3).unwrap();
    }
    println(f\"{s}\");
}
";
    let start = std::time::Instant::now();
    assert_eq!(run(src), "449985000\n");
    assert!(start.elapsed().as_secs() < 60, "{:?}", start.elapsed());
}
