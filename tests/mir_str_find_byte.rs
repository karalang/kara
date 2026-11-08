//! `find` with a one-byte `Str` needle searches by byte, as a char needle
//! does, rather than building a two-way searcher on every call (kata 591's
//! `find(">")`). The offsets are the same: none, the first of several, one
//! after multi-byte chars, at either end, in a slice, and a longer needle
//! beside it.

#[cfg(feature = "llvm")]
mod common;

const SRC: &str = r#"
fn show(o: Option[i64]) -> String {
    match o {
        Some(i) => f"{i}",
        None => "-".to_string(),
    }
}
fn main() {
    let s = "é<a>b>c";
    let t = ">x>";
    let none = "abc";
    print(f"{show(s.find(">"))} {show(t.find(">"))} {show(none.find(">"))} ");
    print(f"{show(s[4..].find(">"))} {show(s.find("é"))} {show(s.find("b>"))} ");
    println(f"{show("".find(">"))} {show(s.find("c"))}");
}
"#;

const EXPECTED: &str = "4 0 - 0 0 5 - 7\n";

#[test]
fn mir_str_find_one_byte_needle() {
    let r = karac::mir::lower::run_source(SRC).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(r.output, EXPECTED, "stderr: {}", r.stderr);
}

#[cfg(feature = "llvm")]
#[test]
fn mir_llvm_str_find_one_byte_needle() {
    use std::process::Command;
    let lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    karac::mir::lower::compiled_check(&lowered).expect("checks");
    let ir = karac::backend::mir::compile_mir_ir(&lowered.program, &lowered.tys).expect("IR");
    let dir = std::env::temp_dir().join(format!("karac-str-find-byte-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 2).expect("object");
    let linked = common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ));
    if linked.is_some() {
        let out = Command::new(&exe).output().expect("run");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), EXPECTED);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
