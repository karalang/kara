//! Kata rows fixed together: a closure's `-> R` moved to its binding when
//! the body's tail is a literal-filled local (kata 2359) or arithmetic over
//! a `sum` (kata 2409), a fill of fills returned whole (kata 2373), a
//! `.map(..).sum()` reading a literal fill (kata 2391), and a `ref` to a
//! `Copy` tuple assigned to an open tuple slot (kata 2419), and a tuple of
//! `collect`s opened element by element (kata 2353).

use std::path::Path;
use std::process::{Command, Stdio};

fn karac(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg("cell.kara")
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let fixed = std::fs::read_to_string(dir.join("cell.kara")).unwrap_or_default();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        fixed,
    )
}

fn mend(tag: &str, src: &str) -> (String, String) {
    let dir = std::env::temp_dir().join(format!("karac-kata-2359-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let mut fixed = String::new();
    for _ in 0..8 {
        let (_, _, now) = karac(&dir, &["fix"]);
        if now == fixed {
            break;
        }
        fixed = now;
    }
    let (ok, out, _) = karac(&dir, &["check"]);
    assert!(ok, "{tag}: {out}\n{fixed}");
    let (ok, out, _) = karac(&dir, &["__mir-run"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "{tag}: {out}\n{fixed}");
    (fixed, out)
}

const WALK: &str = r#"// LeetCode #2359: Find Closest Node to Given Two Nodes -- LeetCode's Rust
// template, carried over by `karac fix`: each start's path walked into a
// distance table, then the nodes both reach scanned.

struct Solution;

impl Solution {
    pub fn closest_meeting_node(edges: Vec<i32>, node1: i32, node2: i32) -> i32 {
        let n = edges.len();
        let walk = |start: i32| -> Vec<i32> {
            let mut dist = vec![-1; n];
            let mut node = start;
            let mut d = 0;
            while node != -1 && dist[node as usize] == -1 {
                dist[node as usize] = d;
                node = edges[node as usize];
                d += 1;
            }
            dist
        };
        let (d1, d2) = (walk(node1), walk(node2));
        let mut best = -1;
        let mut best_dist = i32::MAX;
        for v in 0..n {
            if d1[v] >= 0 && d2[v] >= 0 {
                let far = d1[v].max(d2[v]);
                if far < best_dist {
                    best_dist = far;
                    best = v as i32;
                }
            }
        }
        best
    }
}

fn closest_meeting_node(edges: Vec[i64], node1: i64, node2: i64) -> i64 {
    let edges: Vec[i32] = edges.iter().map(|x| x as i32).collect();
    Solution::closest_meeting_node(edges, node1 as i32, node2 as i32)
}

// ---- test harness ---------------------------------------------------------

fn next_random(seed: mut ref i64) -> i64 {
    *seed = (*seed * 1103515245 + 12345) % 2147483648;
    *seed / 65536
}

fn main() {
    // LeetCode's two examples; one node; two nodes pointing at each other;
    // a chain of 2000 from both ends; a cycle of 2000 started half way
    // round; two paths of 1000 into a shared tail
    println(f"{closest_meeting_node([2, 2, 3, -1], 0, 1)}");
    println(f"{closest_meeting_node([1, 2, -1], 0, 2)}");
    println(f"{closest_meeting_node([-1], 0, 0)}");
    println(f"{closest_meeting_node([1, 0], 0, 1)}");
    let chain: Vec[i64] = (0..2000).map(|i| if i < 1999 { i + 1 } else { -1 }).collect();
    println(f"{closest_meeting_node(chain, 0, 1999)} {closest_meeting_node(chain, 1999, 0)}");
    let cycle: Vec[i64] = (0..2000).map(|i| (i + 1) % 2000).collect();
    println(f"{closest_meeting_node(cycle, 0, 1000)} {closest_meeting_node(cycle, 3, 1500)}");
    let merge: Vec[i64] = (0..3000).map(|i| if i == 999 or i == 1999 { 2000 } else if i == 2999 { -1 } else { i + 1 }).collect();
    println(f"{closest_meeting_node(merge, 0, 1000)} {closest_meeting_node(merge, 500, 1000)}");
    // 3000 random graphs of 1 to 30 nodes, each edge -1 with a chance of 1
    // in 1 to 1 in 8, and two random starts
    let mut seed = 2359;
    let mut check = 0;
    let mut found = 0;
    for _ in 0..3000 {
        let n = 1 + next_random(mut seed) % 30;
        let odds = 1 + next_random(mut seed) % 8;
        let mut edges: Vec[i64] = Vec.new();
        for _ in 0..n {
            edges.push(if next_random(mut seed) % odds == 0 and odds > 1 { -1 } else { next_random(mut seed) % n });
        }
        let a = next_random(mut seed) % n;
        let b = next_random(mut seed) % n;
        let m = closest_meeting_node(edges, a, b);
        check = (check * 31 + m + 2) % 1000000007;
        if m >= 0 {
            found += 1;
        }
    }
    println(f"3000 graphs: checksum {check}, met in {found}");
}
"#;

#[test]
fn kata_2359_closure_return_moved_for_literal_local_tail() {
    let (fixed, _) = mend("walk", WALK);
    assert!(
        fixed.contains("let walk: Fn(i32) -> Vec[i32] = |start: i32| {"),
        "{fixed}"
    );
}

const DAYS: &str = r#"// LeetCode #2409: Count Days Spent Together -- LeetCode's Rust template,
// as Rust writes it, carried by karac fix: a closure over the month table
// reading each date's fields by `parse::<..>` on slices, the days before a
// month by `sum::<i32>()`.

impl Solution {
    pub fn count_days_together(arrive_alice: String, leave_alice: String, arrive_bob: String, leave_bob: String) -> i32 {
        let days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let f = |s: &String| -> i32 {
            let m: usize = s[..2].parse().unwrap();
            let d: i32 = s[3..].parse().unwrap();
            days[..m - 1].iter().sum::<i32>() + d
        };
        let a = f(&arrive_alice).max(f(&arrive_bob));
        let b = f(&leave_alice).min(f(&leave_bob));
        (b - a + 1).max(0)
    }
}

struct Solution;

fn count_days_together(arrive_alice: String, leave_alice: String, arrive_bob: String, leave_bob: String) -> i64 {
    Solution.count_days_together(arrive_alice, leave_alice, arrive_bob, leave_bob) as i64
}

// ---- test harness ---------------------------------------------------------

fn next_random(seed: mut ref i64) -> i64 {
    *seed = (*seed * 1103515245 + 12345) % 2147483648;
    *seed / 65536
}

// Day `n` of the year, from 0, as "MM-DD".
fn date(n: i64) -> String {
    let lengths = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 0;
    let mut day = n;
    while day >= lengths[month] {
        day -= lengths[month];
        month += 1;
    }
    f"{month + 1:02}-{day + 1:02}"
}

// A stay from one random day to a later or the same one.
fn stay(seed: mut ref i64) -> (String, String) {
    let a = next_random(mut seed) % 365;
    let b = next_random(mut seed) % 365;
    (date(a.min(b)), date(a.max(b)))
}

fn main() {
    // LeetCode's two examples; the whole year twice; one shared day; Bob
    // leaving the day Alice arrives
    println(f"{count_days_together("08-15", "08-18", "08-16", "08-19")}");
    println(f"{count_days_together("10-01", "10-31", "11-01", "12-31")}");
    println(f"{count_days_together("01-01", "12-31", "01-01", "12-31")}");
    println(f"{count_days_together("02-28", "02-28", "02-28", "02-28")}");
    println(f"{count_days_together("03-01", "06-30", "01-15", "03-01")}");
    // Alice the whole year, Bob each month in turn
    let mut months: Vec[String] = Vec.new();
    for m in 0..12 {
        let first = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365];
        months.push(f"{count_days_together("01-01", "12-31", date(first[m]), date(first[m + 1] - 1))}");
    }
    println(months.join(" "));
    // 3000 random pairs of stays; a checksum of the answers and their total
    let mut seed = 2409;
    let mut check = 0;
    let mut total = 0;
    for _ in 0..3000 {
        let (arrive_alice, leave_alice) = stay(mut seed);
        let (arrive_bob, leave_bob) = stay(mut seed);
        let a = count_days_together(arrive_alice, leave_alice, arrive_bob, leave_bob);
        total += a;
        check = (check * 31 + a) % 1000000007;
    }
    println(f"3000 random: checksum {check}, total {total}");
}
"#;

#[test]
fn kata_2409_closure_return_moved_for_sum_arithmetic_tail() {
    let (fixed, _) = mend("days", DAYS);
    assert!(
        fixed.contains("let f: Fn(ref String) -> i32 = |s: ref String| {"),
        "{fixed}"
    );
}

const GRID: &str = r#"fn largest(grid: Vec[Vec[i32]]) -> Vec[Vec[i32]] {
    let n = grid.len();
    let mut res = Vec.filled(n - 2, Vec.filled(n - 2, 0));
    for i in 0..n - 2 {
        for j in 0..n - 2 {
            for a in i..i + 3 {
                for b in j..j + 3 {
                    res[i][j] = res[i][j].max(grid[a][b]);
                }
            }
        }
    }
    res
}

fn main() {
    let g: Vec[Vec[i32]] = [[1i32, 2, 3].to_vec(), [4i32, 5, 6].to_vec(), [7i32, 8, 9].to_vec()].to_vec();
    println(f"{largest(g)[0][0]}");
}
"#;

#[test]
fn kata_2373_fill_of_fills_returned_whole_gets_the_suffix() {
    let (fixed, out) = mend("grid", GRID);
    assert!(
        fixed.contains("Vec.filled(n - 2, Vec.filled(n - 2, 0i32))"),
        "{fixed}"
    );
    assert_eq!(out.trim(), "9");
}

const TRAVEL: &str = r#"fn total(travel: Vec[i32], last: Vec[i64], ans: i32) -> i32 {
    let n = travel.len() + 1;
    let mut pre = Vec.filled(n, 0);
    for i in 1..n {
        pre[i] = pre[i - 1] + travel[i - 1];
    }
    ans + last.iter().map(|j| pre[j]).sum()
}

fn main() {
    println(f"{total([2i32, 4, 3].to_vec(), [3, 1].to_vec(), 5)}");
}
"#;

#[test]
fn kata_2391_mapped_sum_reads_the_literal_fill() {
    let (fixed, out) = mend("travel", TRAVEL);
    assert!(fixed.contains("Vec.filled(n, 0i32)"), "{fixed}");
    assert_eq!(out.trim(), "16");
}

const RUNS: &str = r#"fn main() {
    let runs: Vec[(i64, i64)] = [(1, 2), (3, 1), (3, 4)].to_vec();
    let mut best = (0, 0);
    for r in runs.iter() {
        if r.0 > best.0 or (r.0 == best.0 and r.1 > best.1) {
            best = r;
        }
    }
    println(f"{best.0} {best.1}");
}
"#;

#[test]
fn kata_2419_ref_copy_tuple_assigned_to_open_tuple_slot() {
    let (fixed, out) = mend("runs", RUNS);
    assert!(fixed.contains("best = r;"), "{fixed}");
    assert_eq!(out.trim(), "3 4");
}

const TWO_COLLECTS: &str = r#"fn take(pair: (Vec[i64], Vec[String])) -> i64 {
    pair.0.len() + pair.1.len()
}

fn main() {
    let xs: Vec[i64] = [1, 2, 3].to_vec();
    let pair = (xs.iter().map(|x| x * 2).collect(), xs.iter().map(|x| f"{x}").collect());
    let one = (xs.iter().map(|x| x + 1).collect(), 5);
    let v: Vec[i64] = one.0;
    println(f"{take(pair)} {v.len()} {one.1}");
}
"#;

#[test]
fn kata_2353_tuple_of_collects_opens_each_element() {
    let (_, out) = mend("collects", TWO_COLLECTS);
    assert_eq!(out.trim(), "6 3 5");
}
