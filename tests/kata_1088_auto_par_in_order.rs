//! A build that runs nothing as tasks is the in-order program
//! (B-2026-11-10-1). With auto-par on, a build lowers the program once
//! recording its candidates, and that recording lowering puts a block
//! between every statement of a block to delimit them. When the verdicts
//! parallelized nothing, the build kept that lowering, so a plain
//! `karac build` shipped different code from `KARAC_AUTO_PAR=0`: kata
//! 1088's bench, where every loop is refused, ran 15% slower from it. Now
//! the program is lowered in order, and the two builds agree body for body.

/// Kata 1088's ★ arm and a `main` like its bench's: every candidate is
/// refused (a recursive walk, a loop carrying `seed`, a fold into `check`).
const NOTHING_PARALLEL: &str = r#"
fn rotated(d: i64) -> i64 {
    match d {
        0 => 0,
        1 => 1,
        6 => 9,
        8 => 8,
        9 => 6,
        _ => -1,
    }
}

fn count_from(num: i64, rot: i64, unit: i64, n: i64) -> i64 {
    let mut total = 0;
    for d in [0, 1, 6, 8, 9] {
        let next = num * 10 + d;
        if next > n {
            break;
        }
        if next == 0 {
            continue;
        }
        let r = rotated(d) * unit + rot;
        if next != r {
            total += 1;
        }
        total += count_from(next, r, unit * 10, n);
    }
    total
}

fn next_random(seed: mut ref i64) -> i64 {
    *seed = (*seed * 1103515245 + 12345) % 2147483648;
    *seed / 65536
}

fn main() {
    let mut seed = 1088;
    let mut check = 0;
    for _ in 0..20 {
        let n = next_random(mut seed) % 100000 + 1;
        check = (check * 31 + count_from(0, 0, 1, n)) % 1000000007;
    }
    println(f"{check}");
}
"#;

fn bodies(lowered: &karac::mir::lower::Lowered) -> Vec<(String, String)> {
    lowered
        .program
        .bodies
        .iter()
        .map(|(name, body)| {
            (
                name.clone(),
                karac::mir::pretty::pretty_body(body, &lowered.tys),
            )
        })
        .collect()
}

#[test]
fn a_build_with_nothing_parallel_is_the_in_order_program() {
    if !karac::par_cost::auto_par_enabled() {
        // `KARAC_AUTO_PAR=0` in the environment makes both builds the
        // in-order one, which proves nothing; the default leg runs with it on.
        eprintln!("skipped: KARAC_AUTO_PAR=0");
        return;
    }
    let plain = karac::mir::lower::build_source(NOTHING_PARALLEL).expect("MIR");
    assert!(
        plain.auto_par_tasks.is_empty()
            && plain.auto_par_groups.is_empty()
            && plain.auto_par_reductions.is_empty(),
        "the program was meant to parallelize nothing: tasks {:?}, groups {:?}, reductions {:?}",
        plain.auto_par_tasks,
        plain.auto_par_groups,
        plain.auto_par_reductions
    );
    let in_order = karac::mir::lower::build_sequential_source(NOTHING_PARALLEL).expect("MIR");
    let (a, b) = (bodies(&plain), bodies(&in_order));
    assert_eq!(
        a.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        b.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        "the two builds have different bodies"
    );
    for ((name, x), (_, y)) in a.iter().zip(&b) {
        assert_eq!(x, y, "`{name}` differs from its KARAC_AUTO_PAR=0 lowering");
    }
}
