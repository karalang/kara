//! B-2026-11-19-93: moving a `T` field out of a generic struct with a `Drop`
//! body (`fn take[T](b: own Bx[T]) -> T { b.v }`) passed `karac check` and
//! was refused only when an instance was elaborated, so a library author
//! with no call never saw it. The generic body is now refused at check, as
//! the concrete one is (core-semantics §3.7); a `T: Copy` field is a copy and
//! stays legal.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn karac(args: &[&str], src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-generic-drop-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let mut all: Vec<&str> = args.to_vec();
    let p = f.to_str().unwrap().to_string();
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

const BX: &str = "struct Bx[T] { v: T }\nimpl[T] Drop for Bx[T] { fn drop(mut ref self) { println(\"bx\"); } }\n";

#[test]
fn generic_drop_struct_partial_move_is_refused_at_check() {
    // With no call at all: the body alone is wrong.
    for body in [
        "fn take[T](b: own Bx[T]) -> T { b.v }\nfn main() {}\n",
        "fn take[T](b: own Bx[T]) -> T { let v = b.v; v }\nfn main() {}\n",
    ] {
        let (ok, text) = karac(&["check"], &format!("{BX}{body}"));
        assert!(!ok, "{body}: {text}");
        assert!(text.contains("partial_move_of_drop_struct"), "{text}");
        assert!(text.contains("`Bx`"), "{text}");
    }
}

#[test]
fn generic_drop_struct_copy_field_is_read() {
    let src = format!(
        "{BX}fn take[T: Copy](b: own Bx[T]) -> T {{ b.v }}\nfn main() {{ println(take(Bx {{ v: 3 }})); }}\n"
    );
    let (ok, text) = karac(&["check"], &src);
    assert!(ok, "{text}");
    let (ok, text) = karac(&["__mir-run"], &src);
    assert!(ok, "{text}");
    assert_eq!(text, "bx\n3\n");
}
