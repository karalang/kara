//! B-2026-11-19-96: an escaping closure moves what it captures in (§9.3), so
//! calling it and getting back a view of its capture borrows the closure
//! itself (§5.4). Called through a field, a `Vec` element, a tuple, an
//! `Option` or a call's result, the view got no origins and outlived the
//! closure: heap-use-after-free under ASan. Each is now refused at check, and
//! the same calls used while the holder lives still run.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str], src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-escaping-call-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let p = f.to_str().unwrap().to_string();
    let mut all: Vec<&str> = args.to_vec();
    all.push(&p);
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(&all)
        .output()
        .expect("karac");
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

/// The probe's frame: `r` outlives the block that holds the closure.
fn frame(decls: &str, body: &str) -> String {
    format!(
        "{decls}fn main() {{ let e = [0]; let mut r: Slice[i64] = e.as_slice(); {{ let v = [1, 2, 3, 4, 5, 6, 7, 8]; {body} }} let w = [9, 9, 9, 9, 9, 9, 9, 9]; println(r[0]); println(w[0]); }}\n"
    )
}

#[test]
fn a_view_from_a_held_closure_cannot_outlive_its_holder() {
    for (decls, body, holder) in [
        (
            "struct H { f: Fn() -> Slice[i64] }\n",
            "let h = H { f: || v.as_slice() }; r = (h.f)();",
            "`h`",
        ),
        (
            "",
            "let g = || v.as_slice(); let mut fs: Vec[Fn() -> Slice[i64]] = Vec.new(); fs.push(g); r = (fs[0])();",
            "`fs`",
        ),
        ("", "let t = (|| v.as_slice(), 1); r = (t.0)();", "`t`"),
        (
            "",
            "let o: Option[Fn() -> Slice[i64]] = Some(|| v.as_slice()); r = (o.unwrap())();",
            "temporary",
        ),
        (
            "fn mk(v: own Vec[i64]) -> Fn() -> Slice[i64] { || v.as_slice() }\n",
            "let f = mk(v); r = f();",
            "`f`",
        ),
    ] {
        let src = frame(decls, body);
        let (ok, text) = karac(&["check"], &src);
        assert!(!ok, "{body}: {text}");
        assert!(text.contains("E0516"), "{body}: {text}");
        assert!(text.contains(holder), "{body}: {text}");
    }
}

#[test]
fn a_view_from_a_held_closure_is_used_while_the_holder_lives() {
    let src = "struct H { f: Fn() -> Slice[i64] }
fn mk(v: own Vec[i64]) -> Fn() -> Slice[i64] { || v.as_slice() }
fn main() {
    let v = [1, 2, 3];
    let h = H { f: || v.as_slice() };
    let r = (h.f)();
    println(r[0]);
    let f = mk([4, 5]);
    let s = f();
    println(s[1]);
    let t = (|| 7, 1);
    println((t.0)());
}
";
    let (ok, text) = karac(&["check"], src);
    assert!(ok, "{text}");
    let (ok, text) = karac(&["__mir-run"], src);
    assert!(ok, "{text}");
    assert_eq!(text, "1\n5\n7\n");
}
