//! The interpreter builds, walks and drops an owned (non-`shared`)
//! recursive list in linear time (B-2026-11-09-46). Each node cost the
//! depth of the list already built: a move checked the whole value, a drop
//! copied what was left of it and found each node by walking from the
//! head. 20000 nodes took 56 s; LeetCode's lists run to 100000.

use std::process::Command;
use std::time::{Duration, Instant};

fn interp(name: &str, src: &str) -> (String, Duration) {
    let path = std::env::temp_dir().join(format!(
        "karac-interp-owned-list-{name}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["__mir-run", path.to_str().unwrap()])
        .output()
        .expect("karac");
    let took = start.elapsed();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "{name}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    (String::from_utf8(out.stdout).unwrap(), took)
}

/// 50000 nodes built by prepending, then dropped whole, and again walked
/// by moving each node's `next` out. Quadratic, this was most of an hour
/// on a debug build; linear, it is seconds.
#[test]
fn interp_builds_and_drops_an_owned_list_in_linear_time() {
    let build = "struct N { v: i64, next: Option[N] }

fn main() {
    let mut head: Option[N] = None;
    for i in 0..50000 {
        head = Some(N { v: i, next: head });
    }
    match head {
        Some(ref n) => println(f\"head {n.v}\"),
        None => println(\"empty\"),
    }
}
";
    let (out, took) = interp("build", build);
    assert_eq!(out, "head 49999\n");
    assert!(
        took < Duration::from_secs(120),
        "build and drop took {took:?}"
    );

    let walk = "struct N { v: i64, next: Option[N] }

fn main() {
    let mut head: Option[N] = None;
    for i in 0..50000 {
        head = Some(N { v: i, next: head });
    }
    let mut count = 0;
    let mut sum = 0;
    let mut cur = head;
    while let Some(node) = cur {
        count += 1;
        sum += node.v;
        cur = node.next;
    }
    println(f\"{count} {sum}\");
}
";
    let (out, took) = interp("walk", walk);
    assert_eq!(out, "50000 1249975000\n");
    assert!(
        took < Duration::from_secs(120),
        "build and walk took {took:?}"
    );
}

/// A list deeper than the interpreter drops in place still runs each
/// node's `Drop` body before its fields, outermost first (core semantics
/// §6): the nodes past the sixteenth level drop from a slot of their own.
#[test]
fn interp_drops_a_deep_owned_list_outermost_first() {
    let src = "struct N { v: i64, next: Option[N] }

impl Drop for N {
    fn drop(mut ref self) {
        print(f\"{self.v} \");
    }
}

fn main() {
    let mut head: Option[N] = None;
    for i in 0..40 {
        head = Some(N { v: i, next: head });
    }
    println(\"built\");
}
";
    let (out, _) = interp("drop_order", src);
    let want: Vec<String> = (0..40).rev().map(|i| i.to_string()).collect();
    assert_eq!(out, format!("built\n{} ", want.join(" ")));
}
