//! A trait impl on a const-generic `Array[T, N]` resolves: `impl[T: Tally,
//! const N: i64] Tally for Array[T, N]` and then `a.tally()` on an
//! `Array[i64, 3]` runs, with `T` read off the element and `N` off the
//! length. It was refused E0236 "no method 'tally' on type 'Array'" at
//! check for a generic element, and for a concrete one (`Array[i64, N]`)
//! check passed and the MIR died "call of unknown function Array[i64,
//! 3].tally" (B-2026-11-26-68). A method whose signature names `N`
//! (`-> Array[T, N]`) infers it from the receiver, on an array and on a
//! struct's const parameter (`Buf[T, N]`) alike.

mod common;

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "karac-array-impl-{name}-{}-{}",
        args[0].trim_start_matches('_'),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// `karac check` accepts `src`, and the interpreter (and, with the backend,
/// the compiled program) prints `want`.
fn accepted_and_prints(name: &str, src: &str, want: &str) {
    let check = karac(&["check"], name, src);
    assert!(
        check.status.success(),
        "{name}: check: {}{}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for cmd in backends {
        let out = karac(&[cmd], name, src);
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        if *cmd != "__mir-run"
            && common::link_or_skip(if out.status.success() {
                Ok(())
            } else {
                Err(err.clone())
            })
            .is_none()
        {
            continue;
        }
        assert!(out.status.success(), "{name}: {cmd}: {err}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{name}: {cmd}\n{src}"
        );
    }
}

#[test]
fn array_impl_generic_element_and_length() {
    accepted_and_prints(
        "tally",
        "trait Tally { fn tally(ref self) -> i64; }\nimpl Tally for i64 { fn tally(ref self) -> i64 { 1 } }\nimpl[T: Tally, const N: i64] Tally for Array[T, N] {\n    fn tally(ref self) -> i64 {\n        let mut c = 0;\n        for x in self.iter() { c += x.tally(); }\n        c + N\n    }\n}\nfn main() { let a: Array[i64, 3] = [1, 2, 3]; println(a.tally()); }\n",
        "6\n",
    );
}

#[test]
fn array_impl_nested_array_receiver() {
    accepted_and_prints(
        "nested",
        "trait Tally { fn tally(ref self) -> i64; }\nimpl Tally for i64 { fn tally(ref self) -> i64 { 1 } }\nimpl[T: Tally, const N: i64] Tally for Array[T, N] {\n    fn tally(ref self) -> i64 {\n        let mut c = 0;\n        for x in self.iter() { c += x.tally(); }\n        c + N\n    }\n}\nfn main() { let b: Array[Array[i64, 2], 2] = [[1, 2], [3, 4]]; println(b.tally()); }\n",
        "10\n",
    );
}

#[test]
fn array_impl_through_a_generic_bound() {
    accepted_and_prints(
        "bound",
        "trait Tally { fn tally(ref self) -> i64; }\nimpl Tally for i64 { fn tally(ref self) -> i64 { 1 } }\nimpl[T: Tally, const N: i64] Tally for Array[T, N] {\n    fn tally(ref self) -> i64 {\n        let mut c = 0;\n        for x in self.iter() { c += x.tally(); }\n        c + N\n    }\n}\nfn count[T: Tally](x: ref T) -> i64 { x.tally() }\nfn main() { let a: Array[i64, 4] = [1, 2, 3, 4]; println(count(a)); }\n",
        "8\n",
    );
}

#[test]
fn array_impl_length_only_parameter() {
    accepted_and_prints(
        "lenonly",
        "trait Size { fn size(ref self) -> i64; }\nimpl[const N: i64] Size for Array[i64, N] { fn size(ref self) -> i64 { N * 10 } }\nimpl[const N: i64] Size for Array[bool, N] { fn size(ref self) -> i64 { N } }\nfn total[T: Size](x: T) -> i64 { x.size() }\nfn main() {\n    let a: Array[i64, 3] = [1, 2, 3];\n    let b: Array[bool, 2] = [true, false];\n    println(a.size());\n    println(b.size());\n    println(total(a));\n    println(total(b));\n}\n",
        "30\n2\n30\n2\n",
    );
}

#[test]
fn array_impl_element_only_parameter() {
    accepted_and_prints(
        "elemonly",
        "trait Tally { fn tally(ref self) -> i64; }\nimpl[T, const N: i64] Tally for Array[T, N] { fn tally(ref self) -> i64 { N } }\nfn main() { let a: Array[String, 2] = [\"x\", \"y\"]; println(a.tally()); }\n",
        "2\n",
    );
}

#[test]
fn array_impl_method_returns_the_array_type() {
    accepted_and_prints(
        "twice",
        "trait Twice { fn twice(ref self) -> Self; }\nimpl Twice for i64 { fn twice(ref self) -> i64 { self * 2 } }\nimpl[T: Twice + Copy, const N: i64] Twice for Array[T, N] {\n    fn twice(ref self) -> Array[T, N] {\n        let mut out = self.clone();\n        for i in 0..N { out[i] = self[i].twice(); }\n        out\n    }\n}\nfn main() { let a: Array[i64, 3] = [1, 2, 3]; let b = a.twice(); println(f\"{b[0]} {b[1]} {b[2]}\"); }\n",
        "2 4 6\n",
    );
}

#[test]
fn array_impl_method_with_its_own_generic() {
    accepted_and_prints(
        "own",
        "trait Pick { fn pick[U](ref self, f: Fn(i64) -> U) -> U; }\nimpl[T, const N: i64] Pick for Array[T, N] {\n    fn pick[U](ref self, f: Fn(i64) -> U) -> U { f(N) }\n}\nfn main() { let a: Array[i64, 3] = [1, 2, 3]; println(a.pick(|n| f\"n={n}\")); }\n",
        "n=3\n",
    );
}

#[test]
fn struct_const_param_impl_method_returns_self_type() {
    accepted_and_prints(
        "buf",
        "struct Buf[T, const N: i64] { a: Array[T, N] }\ntrait Twice { fn twice(ref self) -> Self; }\nimpl[T: Copy, const N: i64] Twice for Buf[T, N] {\n    fn twice(ref self) -> Buf[T, N] { Buf { a: self.a.clone() } }\n}\nfn main() { let b: Buf[i64, 3] = Buf { a: [1, 2, 3] }; let c = b.twice(); println(c.a[2]); }\n",
        "3\n",
    );
}

#[test]
fn array_impl_bound_unmet_by_the_element_is_refused() {
    let src = "trait Tally { fn tally(ref self) -> i64; }\nimpl Tally for i64 { fn tally(ref self) -> i64 { 1 } }\nimpl[T: Tally, const N: i64] Tally for Array[T, N] {\n    fn tally(ref self) -> i64 {\n        let mut c = 0;\n        for x in self.iter() { c += x.tally(); }\n        c + N\n    }\n}\nfn main() { let s: Array[String, 2] = [\"a\", \"b\"]; println(s.tally()); }\n";
    let out = karac(&["check"], "unmet", src);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "unmet: check accepted:\n{text}");
    assert!(
        text.contains("`String` does not implement `Tally`"),
        "unmet: {text}"
    );
}
