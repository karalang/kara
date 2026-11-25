//! A hand-written ordering or equality impl satisfies the matching bound
//! (`PartialOrd`, `Ord`, `PartialEq`, `Eq`, `Hash`) on a free generic fn
//! (`min`, `max`, `clamp`), a user generic fn and an `impl[T: ..]` block,
//! and calls route to its body (B-2026-11-26-15).
//!
//! `impl PartialOrd for Iv` alone was refused by every `T: PartialOrd`
//! bound with "that verdict is about the derive, not your impl": the bound
//! asked the derive's question, while the operators already desugar through
//! `partial_cmp`. Each comparator here REVERSES its field's order, so a
//! declaration-order comparator would print the opposite answer.

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-hand-ordering-bounds-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    (dir, f)
}

/// `karac check` on `src`: whether it passed, and its output.
fn check(src: &str) -> (bool, String) {
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// The program's stdout under `cmd`, after `karac check` accepted it;
/// `None` when a compiled backend cannot link.
fn run(src: &str, cmd: &str) -> Option<String> {
    let (ok, out) = check(src);
    assert!(ok, "check:\n{out}\n{src}");
    let (dir, f) = file(src);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr).to_string();
    if cmd != "__mir-run" {
        common::link_or_skip(if out.status.success() {
            Ok(())
        } else {
            Err(err.clone())
        })?;
    }
    assert!(out.status.success(), "{cmd}: {err}\n{src}");
    Some(String::from_utf8(out.stdout).unwrap())
}

fn runs_everywhere(src: &str, want: &str) {
    let mut lanes = vec!["__mir-run"];
    if cfg!(feature = "llvm") {
        lanes.extend(["__mir-llvm-run", "__mir-llvm-asan"]);
    }
    for cmd in lanes {
        if let Some(out) = run(src, cmd) {
            assert_eq!(out, want, "{cmd}\n{src}");
        }
    }
}

const IV: &str = "struct Iv { lo: i64 }\n\
     impl PartialEq for Iv { fn eq(ref self, other: ref Iv) -> bool { self.lo == other.lo } }\n\
     impl PartialOrd for Iv {\n\
     fn partial_cmp(ref self, other: ref Iv) -> Option[Ordering] { other.lo.partial_cmp(self.lo) }\n\
     }\n";

#[test]
fn a_hand_written_partial_ord_meets_free_and_user_bounds() {
    runs_everywhere(
        &format!(
            "{IV}fn big[T: PartialOrd](a: own T, b: own T) -> T {{ if a > b {{ a }} else {{ b }} }}\n\
             fn main() {{\n\
             println(max(Iv {{ lo: 1 }}, Iv {{ lo: 5 }}).lo);\n\
             println(min(Iv {{ lo: 1 }}, Iv {{ lo: 5 }}).lo);\n\
             println(big(Iv {{ lo: 1 }}, Iv {{ lo: 5 }}).lo);\n\
             }}\n"
        ),
        "1\n5\n1\n",
    );
}

#[test]
fn a_hand_written_partial_ord_meets_an_impl_block_bound() {
    runs_everywhere(
        &format!(
            "{IV}struct Hold[T] {{ a: T, b: T }}\n\
             impl[T: PartialOrd] Hold[T] {{ fn a_on_top(ref self) -> bool {{ self.a > self.b }} }}\n\
             fn best[T: PartialOrd](xs: ref Vec[T]) -> i64 {{\n\
             let mut i = 0; let mut j = 1;\n\
             while j < xs.len() {{ if xs[j] > xs[i] {{ i = j; }} j += 1; }}\n\
             i as i64\n\
             }}\n\
             fn main() {{\n\
             println(Hold {{ a: Iv {{ lo: 1 }}, b: Iv {{ lo: 5 }} }}.a_on_top());\n\
             println(best(vec![Iv {{ lo: 4 }}, Iv {{ lo: 2 }}, Iv {{ lo: 7 }}]));\n\
             }}\n"
        ),
        "true\n1\n",
    );
}

#[test]
fn a_generic_or_enum_target_with_a_hand_written_partial_ord() {
    runs_everywhere(
        "enum E { A(i64), B }\n\
         impl PartialEq for E { fn eq(ref self, other: ref E) -> bool { true } }\n\
         impl PartialOrd for E { fn partial_cmp(ref self, other: ref E) -> Option[Ordering] { None } }\n\
         struct Pr[T] { x: T }\n\
         impl[T: PartialOrd] PartialEq for Pr[T] { fn eq(ref self, other: ref Pr[T]) -> bool { false } }\n\
         impl[T: PartialOrd] PartialOrd for Pr[T] {\n\
         fn partial_cmp(ref self, other: ref Pr[T]) -> Option[Ordering] { other.x.partial_cmp(self.x) }\n\
         }\n\
         fn lt[T: PartialOrd](a: ref T, b: ref T) -> bool { a < b }\n\
         fn main() {\n\
         println(lt(E.A(1), E.B));\n\
         println(lt(Pr { x: 1 }, Pr { x: 2 }));\n\
         println(max(Pr { x: 1 }, Pr { x: 2 }).x);\n\
         }\n",
        "false\nfalse\n1\n",
    );
}

#[test]
fn hand_written_ord_eq_and_hash_meet_their_bounds() {
    runs_everywhere(
        "struct K { v: i64 }\n\
         impl PartialEq for K { fn eq(ref self, other: ref K) -> bool { self.v % 10 == other.v % 10 } }\n\
         impl Eq for K {}\n\
         impl PartialOrd for K { fn partial_cmp(ref self, other: ref K) -> Option[Ordering] { Some(self.cmp(other)) } }\n\
         impl Ord for K { fn cmp(ref self, other: ref K) -> Ordering { other.v.cmp(self.v) } }\n\
         impl Hash for K { fn hash[H: Hasher](ref self, hasher: mut ref H) { hasher.write_i64(self.v % 10) } }\n\
         fn g[T: Ord](a: own T, b: own T) -> T { if a < b { a } else { b } }\n\
         fn same[T: PartialEq](a: ref T, b: ref T) -> bool { a == b }\n\
         fn same_eq[T: Eq](a: ref T, b: ref T) -> bool { a == b }\n\
         fn count[T: Hash + Eq](xs: own Vec[T]) -> i64 {\n\
         let mut s: Set[T] = Set.new(); for x in xs.into_iter() { s.insert(x); } s.len() as i64\n\
         }\n\
         fn main() {\n\
         println(g(K { v: 1 }, K { v: 9 }).v);\n\
         println(min(K { v: 1 }, K { v: 9 }).v);\n\
         println(clamp(K { v: 5 }, K { v: 9 }, K { v: 1 }).v);\n\
         println(same(K { v: 3 }, K { v: 13 }));\n\
         println(same_eq(K { v: 3 }, K { v: 14 }));\n\
         println(count(vec![K { v: 3 }, K { v: 13 }, K { v: 4 }]));\n\
         }\n",
        "9\n9\n5\ntrue\nfalse\n2\n",
    );
}

#[test]
fn a_type_with_no_ordering_impl_is_still_refused() {
    for src in [
        "struct N { v: i64 }\n\
         impl PartialEq for N { fn eq(ref self, other: ref N) -> bool { true } }\n\
         fn main() { let m = max(N { v: 1 }, N { v: 2 }); }\n",
        // `partial_cmp` in an inherent impl is no ordering impl.
        "struct N { v: i64 }\n\
         impl N { fn partial_cmp(ref self, other: ref N) -> Option[Ordering] { None } }\n\
         fn big[T: PartialOrd](a: own T, b: own T) -> T { a }\n\
         fn main() { let m = big(N { v: 1 }, N { v: 2 }); }\n",
    ] {
        let (ok, out) = check(src);
        assert!(!ok, "{src}\n{out}");
        assert!(
            out.contains("trait bound `T: PartialOrd` is not satisfied"),
            "{src}\n{out}"
        );
    }
    // A hand-written `Ord` still has to meet an `Ord` bound itself:
    // `PartialOrd` alone does not.
    let (ok, out) = check(&format!(
        "{IV}fn g[T: Ord](a: own T, b: own T) -> T {{ a }}\n\
         fn main() {{ let m = g(Iv {{ lo: 1 }}, Iv {{ lo: 2 }}); }}\n"
    ));
    assert!(!ok && out.contains("`T: Ord` is not satisfied"), "{out}");
}
