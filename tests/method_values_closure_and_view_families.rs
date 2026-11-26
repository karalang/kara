//! A built-in method that takes a closure, hands out a mutable view or
//! consumes its receiver is a function value too (design.md § Function
//! values: "Unbound methods are function values"; B-2026-11-26-71):
//! `Vec.sort_by`, `VecDeque.retain`, `Vec.into_iter`, `VecDeque.extend`,
//! `Map.get_or`, `String.sorted` and the rest have a generated wrapper
//! (src/method_value_gen.rs). A closure handed to one of them reaches the
//! native through the wrapper's function-typed parameter, which the MIR
//! interpreter could not call ("cannot call Erased").

mod common;

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

const PRELUDE: &str = "";

fn file(src: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-method-values-b71-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, format!("{PRELUDE}{src}")).unwrap();
    (dir, f)
}

/// `karac check` on the prelude and `src`: whether it passed, and its output.
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

#[test]
fn closure_taking_methods_are_values() {
    runs_everywhere(
        "fn main() { let mut v = [3, 1, 2]; let sb = Vec.sort_by; sb(mut v, |a, b| b.cmp(a)); \
         println(f\"{v[0]}{v[1]}{v[2]}\"); let sk = Vec.sort_by_key; sk(mut v, |x| x); \
         println(f\"{v[0]}{v[1]}{v[2]}\"); }\n",
        "321\n123\n",
    );
    runs_everywhere(
        "fn main() { let mut d: VecDeque[i64] = VecDeque.new(); let ex = VecDeque.extend; \
         ex(mut d, [5, 6, 7]); let rt = VecDeque.retain; rt(mut d, |x| x % 2 == 1); \
         let ts = VecDeque.to_string; println(ts(d)); }\n",
        "[5, 7]\n",
    );
    runs_everywhere(
        "fn main() { let sb = String.sorted_by; println(sb(String.from(\"bca\"), |a, b| b.cmp(a))); \
         let so = Str.sorted; println(so(\"cab\")); }\n",
        "cba\nabc\n",
    );
}

#[test]
fn a_closure_through_a_forwarding_function_value_reaches_the_native() {
    runs_everywhere(
        "fn f(x: mut ref Vec[i64], cmp: own MutFn(i64, i64) -> Ordering) { x.sort_by(cmp) }\n\
         fn main() { let mut v = [3, 1, 2]; let h = f; let k = 10; h(mut v, |a, b| (b + k).cmp(a + k)); \
         println(f\"{v[0]}{v[1]}{v[2]}\"); }\n",
        "321\n",
    );
}

#[test]
fn consuming_and_view_methods_are_values() {
    runs_everywhere(
        "fn main() { let v = [1, 2, 3]; let it = Vec.into_iter; let t: i64 = it(v).sum(); println(t); \
         let mut w = [1, 2, 3, 4]; let sm = Vec.split_at_mut; let (a, b) = sm(mut w, 2); a[0] = 9; b[0] = 8; \
         println(f\"{w[0]}{w[2]}\"); let im = Vec.iter_mut; for x in im(mut w) { *x += 1; } println(w[3]); }\n",
        "6\n98\n5\n",
    );
    runs_everywhere(
        "fn main() { let w = [String.from(\"x\"), String.from(\"y\")]; \
         let mut u = [String.from(\"p\"), String.from(\"q\")]; let cf = Vec.clone_from_slice; \
         cf(mut u, w); println(f\"{u[0]}{u[1]} {w[0]}\"); }\n",
        "xy x\n",
    );
}

#[test]
fn map_methods_are_values() {
    runs_everywhere(
        "fn main() { let mut m: Map[String, i64] = Map.new(); m.insert(String.from(\"a\"), 1); \
         let go = Map.get_or; let k = String.from(\"b\"); println(go(m, k, 9)); println(k); \
         let en = Map.entry; en(mut m, String.from(\"a\")).or_insert(5); println(m[\"a\"]); \
         let mut s: SortedMap[i64, i64] = SortedMap.new(); s.insert(1, 2); \
         let sg = SortedMap.get_or; println(sg(s, 3, 7)); }\n",
        "9\nb\n1\n7\n",
    );
}
