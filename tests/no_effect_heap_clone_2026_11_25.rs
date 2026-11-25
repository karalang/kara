//! `.clone()` of a value that owns a heap buffer is charged
//! `allocates(Heap)` (B-2026-11-25-51): a `String`, a `Vec`, an
//! `Option[Vec[T]]`, a tuple holding a `String`, a derived-`Clone` struct
//! or enum holding a `Vec`, and `Option[ref T]`'s `.cloned()` of one.
//! `#[no_effect(allocates(Heap))]` accepted all of them while it refused a
//! `Map`, `Set` or `SortedMap` clone. A clone of a `Copy` value, of a
//! derived-`Clone` struct of numbers, or of a `shared` handle stays free.

use std::process::Command;

fn check(name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-no-effect-clone-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

/// `quiet` under `#[no_effect(allocates(Heap))]`, taking `param` and
/// returning `body`, with `prelude` above it.
fn program(prelude: &str, param: &str, body: &str, call: &str) -> String {
    format!(
        "{prelude}\n#[no_effect(allocates(Heap))]\nfn quiet(v: {param}) -> i64 {{ {body} }}\nfn main() {{ println(quiet({call})); }}\n"
    )
}

fn refused(name: &str, src: &str) {
    let (ok, text) = check(name, src);
    assert!(!ok, "{name}: accepted\n{src}");
    assert!(
        text.contains("E_NO_EFFECT_VIOLATED") && text.contains("allocates(Heap)"),
        "{name}: {text}"
    );
}

fn accepted(name: &str, src: &str) {
    let (ok, text) = check(name, src);
    assert!(ok, "{name}: {text}\n{src}");
}

#[test]
fn no_effect_heap_refuses_a_clone_of_an_owning_value() {
    let cases = [
        (
            "vec",
            "",
            "ref Vec[i64]",
            "let t = v.clone(); t.len() as i64",
            "[1, 2]",
        ),
        (
            "vec_of_string",
            "",
            "ref Vec[String]",
            "let t = v.clone(); t.len() as i64",
            "[\"a\".to_string()]",
        ),
        (
            "string",
            "",
            "ref String",
            "let t = v.clone(); t.len() as i64",
            "\"ab\".to_string()",
        ),
        (
            "option_vec",
            "",
            "ref Option[Vec[i64]]",
            "let t = v.clone(); if t.is_some() { 1 } else { 0 }",
            "Some([1])",
        ),
        (
            "tuple_string",
            "",
            "ref (i64, String)",
            "let t = v.clone(); t.0",
            "(1, \"a\".to_string())",
        ),
        (
            "derived_struct",
            "#[derive(Clone)]\nstruct Hold { w: Vec[i64] }",
            "ref Hold",
            "let t = v.clone(); t.w.len() as i64",
            "Hold { w: [1] }",
        ),
        (
            "derived_enum",
            "#[derive(Clone)]\nenum Bag { Empty, Full(String) }",
            "ref Bag",
            "let t = v.clone(); match t { Bag.Empty => 0, Bag.Full(s) => s.len() as i64 }",
            "Bag.Full(\"a\".to_string())",
        ),
        (
            "generic_struct",
            "#[derive(Clone)]\nstruct Wrap[T] { x: T }",
            "ref Wrap[String]",
            "let t = v.clone(); t.x.len() as i64",
            "Wrap { x: \"a\".to_string() }",
        ),
        (
            "map_control",
            "",
            "ref Map[i64, i64]",
            "let t = v.clone(); t.len() as i64",
            "Map.new()",
        ),
    ];
    for (name, prelude, param, body, call) in cases {
        refused(name, &program(prelude, param, body, call));
    }
}

#[test]
fn no_effect_heap_refuses_option_cloned_of_a_string() {
    refused(
        "option_cloned",
        "#[no_effect(allocates(Heap))]\n\
         fn quiet(v: ref Vec[String]) -> i64 {\n\
         let o = v.first().cloned();\n\
         match o { Some(s) => s.len() as i64, None => 0 }\n\
         }\n\
         fn main() { println(quiet([\"a\".to_string()])); }\n",
    );
}

#[test]
fn no_effect_heap_accepts_a_clone_that_copies_no_buffer() {
    let cases = [
        ("int", "", "ref i64", "let t = v.clone(); t", "3"),
        (
            "tuple_of_numbers",
            "",
            "ref (i64, bool)",
            "let t = v.clone(); t.0",
            "(1, true)",
        ),
        (
            "option_int",
            "",
            "ref Option[i64]",
            "let t = v.clone(); t.unwrap_or(0)",
            "Some(1)",
        ),
        (
            "derived_numbers",
            "#[derive(Clone)]\nstruct P { x: i64, y: i64 }",
            "ref P",
            "let t = v.clone(); t.x + t.y",
            "P { x: 1, y: 2 }",
        ),
        (
            "shared_handle",
            "shared struct Node { v: i64 }",
            "ref Node",
            "let t = v.clone(); t.v",
            "Node { v: 1 }",
        ),
    ];
    for (name, prelude, param, body, call) in cases {
        accepted(name, &program(prelude, param, body, call));
    }
}
