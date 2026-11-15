//! B-2026-11-15-7: `VecDeque.truncate` drops the removed elements from index `len`
//! to the back, in that order (docs/library/collections.md), as `clear`,
//! `retain`, a scope-end drop and `Vec.truncate` do. It popped them off
//! the back, so `[1, 2, 3].truncate(0)` dropped 3, 2, 1. Each program runs
//! on the interpreter, compiled and under ASan.

use std::process::{Command, Stdio};

fn run(sub: &str, tag: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!("karac-deque-trunc-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("cell.kara"), src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args([sub, "cell.kara"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn lanes(tag: &str, src: &str, want: &str) {
    let (ok, out, err) = run("__mir-run", tag, src);
    assert!(ok, "{tag} interpreted: {err}");
    assert_eq!(out, want, "{tag} interpreted: {err}");
    if !cfg!(feature = "llvm") {
        return;
    }
    for sub in ["__mir-llvm-run", "__mir-llvm-asan"] {
        let (ok, out, err) = run(sub, tag, src);
        if err.contains("Linker failed")
            && std::env::var_os("KARAC_REQUIRE_RUNTIME_ARCHIVE").is_none()
        {
            eprintln!("skipping {sub}: no runtime archive");
            continue;
        }
        assert!(ok, "{tag} {sub}: {err}");
        assert_eq!(out, want, "{tag} {sub}: {err}");
    }
}

const PRELUDE: &str = r#"struct T { n: i64 }
impl Drop for T { fn drop(mut ref self) { println(f"drop {self.n}"); } }
fn mk() -> VecDeque[T] {
  let mut d: VecDeque[T] = VecDeque.new();
  d.push_back(T { n: 2 });
  d.push_back(T { n: 3 });
  d.push_back(T { n: 4 });
  d.push_front(T { n: 1 });
  d
}
"#;

#[test]
fn deque_truncate_drops_front_to_back() {
    let src = format!(
        r#"{PRELUDE}fn main() {{
  let mut a = mk(); println("truncate0"); a.truncate(0); println(f"len {{a.len()}}");
  let mut b = mk(); println("truncate1"); b.truncate(1);
  println(f"kept {{b.front().unwrap().n}} len {{b.len()}}");
  let mut c = mk(); println("truncate-1"); c.truncate(-1); println(f"len {{c.len()}}");
  let mut d = mk(); println("truncate9"); d.truncate(9); println(f"len {{d.len()}}");
  println("end");
}}
"#
    );
    lanes(
        "order",
        &src,
        "truncate0\ndrop 1\ndrop 2\ndrop 3\ndrop 4\nlen 0\n\
         truncate1\ndrop 2\ndrop 3\ndrop 4\nkept 1 len 1\n\
         truncate-1\ndrop 1\ndrop 2\ndrop 3\ndrop 4\nlen 0\n\
         truncate9\nlen 4\nend\n\
         drop 1\ndrop 2\ndrop 3\ndrop 4\ndrop 1\n",
    );
}

/// The kept front still drops whole at scope end, after the deque wrapped.
#[test]
fn deque_truncate_after_wrapping_keeps_the_front() {
    let src = format!(
        r#"{PRELUDE}fn main() {{
  let mut d = mk();
  let _ = d.pop_front();
  d.push_back(T {{ n: 5 }});
  d.push_back(T {{ n: 6 }});
  println("truncate2");
  d.truncate(2);
  for t in d.iter() {{ println(f"kept {{t.n}}"); }}
  println("end");
}}
"#
    );
    lanes(
        "wrapped",
        &src,
        "drop 1\ntruncate2\ndrop 4\ndrop 5\ndrop 6\nkept 2\nkept 3\nend\ndrop 2\ndrop 3\n",
    );
}
