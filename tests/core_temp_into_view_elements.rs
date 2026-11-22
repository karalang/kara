//! A temporary stored into a collection of views is refused: the collection
//! keeps the argument as one of its elements, the temporary is dropped at the
//! `;`, and there is no temporary lifetime extension (core-semantics.md §5.5).
//! `refs.push(words[0].clone())` into a `Vec[ref String]` used to check clean
//! and read freed memory (B-2026-11-22-2). A view of a place that outlives the
//! collection, an owning collection and a literal stay accepted.

use std::process::Command;

fn karac(sub: &str, name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-tempviewelem-{}-{sub}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(&dir)
        .arg(sub)
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const PUSH_CLONE: &str = r#"fn main() {
    let words: Vec[String] = ["alpha".to_string(), "beta".to_string()];
    let mut refs: Vec[ref String] = Vec.new();
    refs.push(words[0].clone());
    println(f"{refs[0]}");
}
"#;

const TEMP_MSG: &str = "a reference to a temporary outlives its statement (core-semantics.md §5.5)";

#[test]
fn temp_view_elem_push_clone_is_refused() {
    let (ok, err) = karac("check", "push", PUSH_CLONE);
    assert!(
        !ok,
        "check accepted a temporary pushed into Vec[ref String]"
    );
    assert!(err.contains(TEMP_MSG), "{err}");
    assert!(
        err.contains("the `Vec[ref String]` keeps this argument as one of its elements"),
        "{err}"
    );
    assert!(
        err.contains("store a view of `words[0]` instead (`let w = ref words[0];` before the statement, then `push` `w`)"),
        "{err}"
    );
    assert!(err.contains("(a `String` element)"), "{err}");
}

#[test]
fn temp_view_elem_other_slots_are_refused() {
    for (name, decl, stmt) in [
        (
            "vec_insert",
            "let mut c: Vec[ref String] = Vec.new();",
            "c.insert(0, words[1].to_uppercase());",
        ),
        (
            "vec_resize",
            "let mut c: Vec[ref String] = Vec.new();",
            "c.resize(2, words[1].clone());",
        ),
        (
            "vec_fill",
            "let mut c: Vec[ref String] = Vec.new();",
            "c.fill(words[1].clone());",
        ),
        (
            "str_of_temp",
            "let mut c: Vec[Str] = Vec.new();",
            "c.push(words[1].to_uppercase().as_str());",
        ),
        (
            "map_value",
            "let mut c: Map[i64, ref String] = Map.new();",
            "c.insert(1, words[0].clone());",
        ),
        (
            "map_key",
            "let mut c: Map[ref String, i64] = Map.new();",
            "c.insert(words[0].clone(), 1);",
        ),
        (
            "set",
            "let mut c: Set[ref String] = Set.new();",
            "c.insert(words[0].clone());",
        ),
        (
            "deque_front",
            "let mut c: VecDeque[ref String] = VecDeque.new();",
            "c.push_front(words[0].clone());",
        ),
    ] {
        let src = format!(
            "fn main() {{
    let words: Vec[String] = [\"alpha\".to_string(), \"beta\".to_string()];
    {decl}
    {stmt}
    println(f\"{{c.len()}}\");
}}
"
        );
        let (ok, err) = karac("check", name, &src);
        assert!(!ok, "{name}: check accepted a temporary stored as a view");
        assert!(err.contains(TEMP_MSG), "{name}: {err}");
    }
}

#[test]
fn temp_view_elem_views_and_owners_are_accepted() {
    for (name, src, want) in [
        (
            "view_of_place",
            r#"fn main() {
    let words: Vec[String] = ["alpha".to_string(), "beta".to_string()];
    let mut refs: Vec[ref String] = Vec.new();
    let w = ref words[0];
    refs.push(w);
    for s in words.iter() { refs.push(s); }
    println(f"{refs[0]} {refs.len()}");
}
"#,
            "alpha 3\n",
        ),
        (
            "owning_vec",
            r#"fn main() {
    let words: Vec[String] = ["alpha".to_string(), "beta".to_string()];
    let mut own: Vec[String] = Vec.new();
    own.push(words[0].clone());
    println(f"{own[0]}");
}
"#,
            "alpha\n",
        ),
        (
            "str_literal_and_place",
            r#"fn main() {
    let words: Vec[String] = ["alpha".to_string(), "beta".to_string()];
    let mut strs: Vec[Str] = Vec.new();
    strs.push("lit");
    strs.push(words[0].as_str());
    println(f"{strs[0]} {strs[1]}");
}
"#,
            "lit alpha\n",
        ),
    ] {
        let (ok, err) = karac("check", name, src);
        assert!(ok, "{name}: {err}");
        let got = karac::mir::lower::run_source(src)
            .expect("MIR interpreter")
            .output;
        assert_eq!(got, want, "{name}");
    }
}

#[cfg(feature = "llvm")]
#[test]
fn temp_view_elem_run_and_build_refuse_too() {
    for sub in ["run", "build"] {
        let (ok, err) = karac(sub, "push", PUSH_CLONE);
        assert!(
            !ok,
            "{sub} accepted a temporary pushed into Vec[ref String]"
        );
        assert!(err.contains(TEMP_MSG), "{sub}: {err}");
    }
}
