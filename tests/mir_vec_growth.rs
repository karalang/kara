//! A push grows a full `Vec` or `String` through one cold, never-inlined
//! function per element size (`grow1.<size>`), so the push itself stays
//! small enough for LLVM to inline the function holding it: kata 225's
//! queue rotation kept its `mut ref` struct's fields in memory through the
//! loop when `stack_push` missed inlining by 25 (B-2026-11-02-2). Every
//! growth path still runs: the first block, doubling, and a borrowed
//! literal's copy-out.
#![cfg(feature = "llvm")]

mod common;

use std::process::Command;

fn ir(src: &str) -> String {
    let lowered = karac::mir::lower::build_source(src).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR")
}

/// The compiled program's exit code and standard output.
fn compiled(src: &str, tag: &str) -> Option<(i32, String)> {
    let ir = ir(src);
    let dir = std::env::temp_dir().join(format!("karac-mir-growth-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
    ))
}

/// The body of the function `name` in `ir`.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = ir
        .match_indices(&format!("@{name}("))
        .map(|(i, _)| i)
        .find(|&i| {
            ir[..i]
                .rsplit('\n')
                .next()
                .is_some_and(|l| l.starts_with("define"))
        })
        .unwrap_or_else(|| panic!("no definition of {name}"));
    let end = ir[at..].find("\n}\n").map_or(ir.len(), |e| at + e);
    &ir[at..end]
}

const QUEUE: &str = r#"struct Queue {
    data: Vec[i64],
    mut head: i64,
}

fn enqueue(q: mut ref Queue, x: i64) {
    q.data.push(x);
}

fn dequeue(q: mut ref Queue) -> i64 {
    let v = q.data[q.head];
    q.head = q.head + 1;
    return v;
}

fn stack_push(q: mut ref Queue, x: i64) {
    enqueue(mut q, x);
    let mut rotations = q.data.len() - q.head - 1;
    while rotations > 0 {
        let front = dequeue(mut q);
        enqueue(mut q, front);
        rotations = rotations - 1;
    }
}

fn main() {
    let mut q = Queue { data: Vec.new(), head: 0 };
    let mut i = 1;
    while i <= 40 {
        stack_push(mut q, i * 3);
        i = i + 1;
    }
    let mut sum = 0;
    let mut k = 0;
    while k < 5 {
        let v = dequeue(mut q);
        println(v);
        sum = sum + v;
        k = k + 1;
    }
    println(f"{sum} {q.data.len()}");
    let mut pairs: Vec[(i64, i64)] = Vec.new();
    let mut j = 0;
    while j < 33 {
        pairs.push((j, j * j));
        j = j + 1;
    }
    let (a, b) = pairs[32];
    println(f"{pairs.len()} {a} {b}");
    let mut s = String.from("ab");
    s.push('c');
    let mut n = 0;
    while n < 30 {
        s.push('z');
        n = n + 1;
    }
    println(f"{s.len()} {s}");
}
"#;

#[test]
fn mir_vec_push_grows_out_of_line_on_every_path() {
    let Some((code, out)) = compiled(QUEUE, "queue") else {
        return;
    };
    assert_eq!(code, 0, "{out}");
    let zs = "z".repeat(30);
    assert_eq!(
        out,
        format!("120\n117\n114\n111\n108\n570 820\n33 32 1024\n33 abc{zs}\n")
    );
}

#[test]
fn mir_vec_push_calls_one_cold_grow_per_element_size() {
    let ir = ir(QUEUE);
    let enqueue = body(&ir, "kara.enqueue");
    assert!(enqueue.contains("@grow1.8("), "{enqueue}");
    assert!(!enqueue.contains("karac_realloc_or_panic"), "{enqueue}");
    let grow = body(&ir, "grow1.8");
    assert!(grow.contains("karac_realloc_or_panic"), "{grow}");
    let line = ir
        .lines()
        .find(|l| l.starts_with("define") && l.contains("@grow1.8("))
        .unwrap();
    let attrs = line.rsplit('#').next().unwrap().trim_end_matches(" {");
    let group = ir
        .lines()
        .find(|l| l.starts_with(&format!("attributes #{attrs} ")))
        .unwrap_or_else(|| panic!("no attribute group in {line}"));
    assert!(
        group.contains("noinline") && group.contains("cold"),
        "{group}"
    );
    assert!(ir.contains("@grow1.16("), "a 16-byte tuple has its own");
}
