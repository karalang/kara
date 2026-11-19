//! An escaping closure captures by move (core semantics §9.3), per place
//! (§9.1): one that reads only a `Copy` field (`hs.push(|| v.id)`) copies
//! that field, so the binding stays usable and drops at its own scope's
//! end, whether or not its type has a `Drop` body (review ruling on
//! B-2026-11-19-97). Check and MIR agree on it. Taking a non-`Copy` field
//! out of a `Drop` type takes the whole binding, since such a type gives up
//! no part; out of a type without one, just that field.

use std::process::{Command, Output, Stdio};

fn karac(tag: &str, args: &[&str], src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-escaping-copy-field-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
}

fn runs_as(tag: &str, src: &str, want: &str) {
    let check = karac(tag, &["check"], src);
    assert!(check.status.success(), "{tag}: check: {}", text(&check));
    let backends: &[&str] = if cfg!(feature = "llvm") {
        &["__mir-run", "__mir-llvm-run"]
    } else {
        &["__mir-run"]
    };
    for b in backends {
        let out = karac(tag, &[b], src);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            want,
            "{tag} on {b}: {}",
            text(&out)
        );
        assert!(out.status.success(), "{tag} on {b}: {:?}", out.status);
    }
}

fn refused(tag: &str, src: &str, needle: &str) {
    let check = karac(tag, &["check"], src);
    assert!(
        !check.status.success() && text(&check).contains(needle),
        "{tag}: want {needle:?}: {}",
        text(&check)
    );
}

const R: &str = "struct R { id: i64, s: String }
impl Drop for R { fn drop(mut ref self) { println(f\"d{self.id}\"); } }
struct S { id: i64, s: String }
fn eat(x: own R) { }
";

#[test]
fn escaping_closure_copies_a_copy_field_of_a_drop_binding() {
    let src = format!(
        "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = R {{ id: 1, s: \"a\" }}; \
         hs.push(|| v.id); println(\"pushed\"); println(v.id); println(hs[0]()); println(\"end\"); }}\n"
    );
    runs_as("vec", &src, "pushed\n1\n1\nend\nd1\n");
}

#[test]
fn escaping_closure_copy_capture_leaves_the_binding_free_to_move() {
    let src = format!(
        "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = R {{ id: 1, s: \"a\" }}; \
         hs.push(|| v.id); eat(v); println(\"ate\"); println(hs[0]()); println(\"end\"); }}\n"
    );
    runs_as("eat-after", &src, "d1\nate\n1\nend\n");
}

#[test]
fn closure_stored_in_a_field_copies_a_copy_field() {
    let src = format!(
        "{R}struct H {{ f: Fn() -> i64 }}\n\
         fn main() {{ let v = R {{ id: 1, s: \"a\" }}; let h = H {{ f: || v.id }}; println(v.id); \
         println((h.f)()); println(\"end\"); }}\n"
    );
    runs_as("field", &src, "1\n1\nend\nd1\n");
}

#[test]
fn escaping_closure_copy_capture_agrees_on_a_type_without_drop() {
    let src = format!(
        "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = S {{ id: 1, s: \"x\" }}; \
         hs.push(|| v.id); println(v.s); println(v.id); println(hs[0]()); }}\n"
    );
    runs_as("no-drop", &src, "x\n1\n1\n");
    let field = format!(
        "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = S {{ id: 1, s: \"a\" }}; \
         hs.push(|| v.s.len() as i64); println(v.id); println(hs[0]()); }}\n"
    );
    runs_as("no-drop-field", &field, "1\n1\n");
}

#[test]
fn escaping_closure_capture_still_refuses_a_real_move() {
    refused(
        "moved-before",
        &format!(
            "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = R {{ id: 1, s: \"a\" }}; \
             eat(v); hs.push(|| v.id); println(hs[0]()); }}\n"
        ),
        "moved",
    );
    // A non-`Copy` field out of a `Drop` type moves the whole binding.
    refused(
        "drop-non-copy-field",
        &format!(
            "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = R {{ id: 1, s: \"a\" }}; \
             hs.push(|| v.s.len() as i64); println(v.id); println(hs[0]()); }}\n"
        ),
        "a moved `R` cannot be used again",
    );
    // The field a type without `Drop` gave up is gone.
    refused(
        "no-drop-moved-field",
        &format!(
            "{R}fn main() {{ let mut hs: Vec[Fn() -> i64] = Vec.new(); let v = S {{ id: 1, s: \"a\" }}; \
             hs.push(|| v.s.len() as i64); println(v.s); println(hs[0]()); }}\n"
        ),
        "moved into an escaping closure",
    );
}
