//! B-2026-11-20-109: `v.windows(k)` and `v.chunks(k)` on a `Vec` or a
//! `Slice`, walked where they are written (`.into_iter()` straight after
//! the call, or a `for` over it), are the library's `SliceRuns`, which
//! cuts each run as it is asked for. The collected `Vec[Slice[T]]` they
//! return when stored is not built. Kata 1752's
//! `nums.windows(2).into_iter().filter(|w| w[0] > w[1]).count()` ran 1.96G
//! instructions against Rust's 0.86G; it is 0.80G, the bounds checks on
//! `w[0]` and `w[1]` folding once each run's length is plainly `k`.

fn built(src: &str, name: &str) -> String {
    let l = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::pretty::pretty_body(&l.program.bodies[name], &l.tys)
}

const SHAPES: &str = r#"
fn drops(v: ref Vec[i64]) -> i64 {
    v.windows(2).into_iter().filter(|w| w[0] > w[1]).count()
}
fn kept(v: ref Vec[i64]) -> i64 {
    let w = v.windows(2);
    w.len()
}
fn looped(v: ref Vec[i64]) -> i64 {
    let mut n = 0;
    for c in v.chunks(2) { n += c.len(); }
    n
}
fn sliced(s: Slice[i64]) -> i64 {
    let mut n = 0;
    for w in s.windows(3).into_iter() { n += w[2]; }
    n
}
fn main() {
    let v: Vec[i64] = [3, 1, 2, 5];
    println(f"{drops(v)} {kept(v)} {looped(v)} {sliced(v.as_slice())}");
}
"#;

#[test]
fn lazy_runs_walked_windows_build_no_vec() {
    let drops = built(SHAPES, "drops");
    assert!(drops.contains("vec_runs[i64]("), "{drops}");
    assert!(!drops.contains(".windows("), "{drops}");
    let looped = built(SHAPES, "looped");
    assert!(looped.contains("vec_runs[i64]("), "{looped}");
    assert!(looped.contains("SliceRuns[i64].next("), "{looped}");
    assert!(!looped.contains(".chunks("), "{looped}");
    let sliced = built(SHAPES, "sliced");
    assert!(sliced.contains("slice_runs[i64]("), "{sliced}");
    // A stored `windows(k)` is the documented `Vec[Slice[T]]`.
    let kept = built(SHAPES, "kept");
    assert!(kept.contains("Vec[i64].windows("), "{kept}");
    let r = karac::mir::lower::run_source(SHAPES).expect("run");
    assert_eq!(r.output, "1 3 4 7\n");
}

/// Every walked form prints the runs the collected `Vec` holds, in the same
/// order (`stored` is the collected form, beside `lazy`).
const WALKS: &str = r#"fn show(xs: Vec[Slice[i64]]) -> String {
    let mut out = String.new();
    for s in xs.iter() {
        out.push_str(f"{s.to_vec()}");
    }
    out
}

fn main() {
    loops();
    let v: Vec[i64] = [1, 2, 3, 4, 5];
    let lazy: Vec[Slice[i64]] = v.windows(2).into_iter().collect();
    let stored = v.windows(2);
    let eager: Vec[Slice[i64]] = stored.into_iter().collect();
    println(f"{show(lazy)} {show(eager)}");
    let c: Vec[Slice[i64]] = v.chunks(2).into_iter().collect();
    println(show(c));
    let r: Vec[Slice[i64]] = v.chunks(2).into_iter().rev().collect();
    println(show(r));
    let wr: Vec[Slice[i64]] = v.windows(3).into_iter().rev().collect();
    println(show(wr));
    println(f"{v.windows(2).into_iter().len()} {v.chunks(3).into_iter().len()} {v.windows(9).into_iter().len()} {v.chunks(9).into_iter().len()}");
    let s = v.as_slice();
    let sums: Vec[i64] = s.windows(2).into_iter().map(|w| w[0] + w[1]).collect();
    println(f"{sums}");
    for (i, w) in v.windows(4).into_iter().enumerate() {
        println(f"{i} {w.to_vec()}");
    }
    let e: Vec[i64] = Vec.new();
    println(f"{e.windows(1).into_iter().count()} {e.chunks(1).into_iter().count()}");
    let names: Vec[String] = ["a".to_string(), "b".to_string(), "c".to_string()];
    let joined: Vec[String] = names.windows(2).into_iter().map(|w| f"{w[0]}{w[1]}").collect();
    println(f"{joined}");
    let mut it = v.chunks(2).into_iter();
    let _ = it.next();
    println(f"{it.next().unwrap().to_vec()} {it.next_back().unwrap().to_vec()} {it.len()}");
    let skipped: Vec[Slice[i64]] = v.windows(2).into_iter().skip(2).collect();
    println(show(skipped));
    let zipped: Vec[i64] = v.windows(2).into_iter().zip(v.chunks(1).into_iter()).map(|(a, b)| a[1] * b[0]).collect();
    println(f"{zipped}");
}

fn total(xs: Slice[i64]) -> i64 {
    let mut t = 0;
    for c in xs.chunks(2) {
        for x in c.iter() {
            t += *x;
        }
    }
    t
}

fn loops() {
    let mut v: Vec[i64] = [1, 2, 3, 4, 5, 6, 7];
    for w in v.windows(3) {
        if w[0] == 3 {
            continue;
        }
        if w[0] == 5 {
            break;
        }
        println(f"w {w.to_vec()}");
    }
    for c in v.chunks(3) {
        println(f"c {c.to_vec()} {c.len()}");
    }
    println(f"{total(v.as_slice())}");
    let names: Vec[String] = ["x".to_string(), "y".to_string(), "z".to_string()];
    'outer: for w in names.windows(2) {
        for c in names.chunks(2) {
            if c.len() == 1 {
                continue 'outer;
            }
            println(f"{w[0]}{w[1]} {c[0]}");
        }
    }
    for w in v.as_slice().windows(10) {
        println(f"never {w.len()}");
    }
    v.push(8);
    let mut n = 0;
    for (i, w) in v.windows(2).into_iter().enumerate() {
        n += i * w[1];
    }
    println(f"{n} {v.len()}");
}
"#;

const WALKS_OUT: &str = r#"w [1, 2, 3]
w [2, 3, 4]
w [4, 5, 6]
c [1, 2, 3] 3
c [4, 5, 6] 3
c [7] 1
28
xy x
yz x
133 8
[1, 2][2, 3][3, 4][4, 5] [1, 2][2, 3][3, 4][4, 5]
[1, 2][3, 4][5]
[5][3, 4][1, 2]
[3, 4, 5][2, 3, 4][1, 2, 3]
4 2 0 1
[3, 5, 7, 9]
0 [1, 2, 3, 4]
1 [2, 3, 4, 5]
0 0
[ab, bc]
[3, 4] [5] 0
[3, 4][4, 5]
[2, 6, 12, 20]
"#;

#[test]
fn lazy_runs_walks_print_the_collected_runs() {
    let r = karac::mir::lower::run_source(WALKS).expect("run");
    assert_eq!(r.output, WALKS_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn lazy_runs_walks_print_the_collected_runs_when_built() {
    let dir = std::env::temp_dir().join(format!("karac-lazy-runs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::write(dir.join("m.kara"), WALKS).expect("write source");
    let built = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .args(["build", "m.kara", "-o", "m"])
        .output()
        .expect("karac");
    let berr = String::from_utf8_lossy(&built.stderr).into_owned();
    if !built.status.success() && berr.contains("runtime archive") {
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!("skipped: no runtime archive");
        return;
    }
    assert!(built.status.success(), "{berr}");
    let out = std::process::Command::new(dir.join("m"))
        .output()
        .expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&out.stdout), WALKS_OUT);
}
