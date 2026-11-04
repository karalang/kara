//! Rust spellings `karac fix` takes to a program that checks in one pass:
//! generic parameters in angle brackets on a declaration (B-2026-11-04-6),
//! `&[T]` / `&mut [T]` as Kāra's `Slice[T]` / `mut Slice[T]`, and
//! `Box<dyn Fn(A) -> R>` as the function type itself (B-2026-11-04-7).
//! Also the deprecated `vec![..]` list literal, which `karac check` warns on
//! and `karac fix` rewrites (design.md § Collection literals, 2026-11-04).

use std::process::{Command, Stdio};

fn repaired(src: &str) -> (String, Vec<String>) {
    let result = karac::parse(src);
    let messages: Vec<String> = result.errors.iter().map(|e| e.message.clone()).collect();
    let mut edits: Vec<_> = result.fix_edits.values().cloned().collect();
    edits.extend(result.fix_diffs.values().flatten().cloned());
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    (out, messages)
}

#[test]
fn rust_angle_generic_params_on_declarations_are_bracketed() {
    let src = "struct Bx<T> { v: T }\n\
               enum E<T> { A(T) }\n\
               trait Get<T> { fn get(self) -> T; }\n\
               impl<T: Copy> Bx<T> { fn get(self) -> T { self.v } }\n\
               fn id<T>(x: own T) -> T { x }\n\
               fn main() {\n    let b = Bx { v: 4 };\n    let e = E.A(1);\n\
               \x20   match e { E.A(n) => println(n + id(2)) }\n    println(b.get());\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(
        messages,
        [
            "`Bx<..>` is Rust's spelling: Kāra writes generic parameters in brackets, `Bx[..]`",
            "`E<..>` is Rust's spelling: Kāra writes generic parameters in brackets, `E[..]`",
            "`Get<..>` is Rust's spelling: Kāra writes generic parameters in brackets, `Get[..]`",
            "`impl<..>` is Rust's spelling: Kāra writes generic parameters in brackets, `impl[..]`",
            "`Bx<..>` is Rust's spelling: Kāra writes generic arguments in brackets, `Bx[..]`",
            "`id<..>` is Rust's spelling: Kāra writes generic parameters in brackets, `id[..]`",
        ],
    );
    assert!(out.contains("struct Bx[T] {"), "{out}");
    assert!(out.contains("impl[T: Copy] Bx[T] {"), "{out}");
    assert!(out.contains("fn id[T](x: own T)"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "3\n4\n");
}

#[test]
fn rust_borrowed_slice_types_are_slice_in_one_edit() {
    let src = "fn first(s: &[i64]) -> i64 { s[0] }\n\
               fn zero(s: &mut [i64]) { s[0] = 0; }\n\
               fn main() {\n    let mut v = [9, 8].to_vec();\n    println(first(v[..]));\n\
               \x20   zero(mut v[..]);\n    println(first(v[..]));\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(
        messages,
        [
            "`&[T]` is Rust's slice -- in Kāra a borrowed view of a run of elements is `Slice[T]`",
            "`&mut [T]` is Rust's mutable slice -- in Kāra it is `mut Slice[T]`",
        ],
    );
    assert!(out.contains("fn first(s: Slice[i64])"), "{out}");
    assert!(out.contains("fn zero(s: mut Slice[i64])"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "9\n0\n");
}

#[test]
fn rust_boxed_closure_type_is_the_function_type() {
    let src = "fn mk(k: i64) -> Box<dyn Fn(i64) -> i64> { |x| x + k }\n\
               fn main() {\n    let f = mk(2);\n    println(f(3));\n}\n";
    let (out, messages) = repaired(src);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(
        messages[0].starts_with("`Box<dyn Fn(..)>` is Rust's boxed closure"),
        "{messages:?}"
    );
    assert!(out.contains("fn mk(k: i64) -> Fn(i64) -> i64 {"), "{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "5\n");
}

fn karac(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn karac_fix_rewrites_the_deprecated_vec_macro() {
    let src = "fn main() {
    let mut v = vec![1, 2];
    v.push(3);
    let n = 2;
    let z = vec![7; n];
    let e: Vec[i64] = vec![];
    println(f\"{v} {z} {e}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-vec-macro-{}.kara", std::process::id()));
    let file = path.to_str().unwrap();
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check", file]);
    let err = String::from_utf8_lossy(&check.stderr).to_string();
    assert!(check.status.success(), "a warning, not an error: {err}");
    assert_eq!(
        err.matches("warning[deprecated]").count(),
        3,
        "one warning per literal: {err}"
    );
    assert!(err.contains("use of deprecated `vec![..]`"), "{err}");
    let fix = karac(&["fix", file]);
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let out = std::fs::read_to_string(&path).unwrap();
    let recheck = karac(&["check", file]);
    let run = karac(&["__mir-run", file]);
    let _ = std::fs::remove_file(&path);
    for want in [
        "let mut v = [1, 2];",
        "let z = Vec.filled(n, 7);",
        "let e: Vec[i64] = Vec.new();",
    ] {
        assert!(out.contains(want), "missing {want:?} in:\n{out}");
    }
    let recheck_err = String::from_utf8_lossy(&recheck.stderr);
    assert!(
        recheck.status.success() && !recheck_err.contains("deprecated"),
        "{recheck_err}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "[1, 2, 3] [7, 7] []\n",
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}
