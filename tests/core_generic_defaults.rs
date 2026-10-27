//! B-2026-10-27-15: a struct's or enum's generic parameters take defaults
//! (design.md § Default generic arguments), and an omitted trailing argument
//! takes its default wherever the type is written: `Pair[i64]` of
//! `struct Pair[A, B = Vec[A]]` is `Pair[i64, Vec[i64]]`. A function's
//! parameters take none.

use std::process::Command;

fn karac(tag: &str, cmd: &str, src: &str) -> (bool, String, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-generic-defaults-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(cmd)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

const TYPES: &str = r#"
struct Pair[A, B = Vec[A]] { a: A, b: B }
enum Either[L, R = String] { Left(L), Right(R) }
"#;

#[test]
fn an_omitted_argument_takes_its_default() {
    let src = format!(
        "{TYPES}
impl[A] Pair[A] {{
    fn count(ref self) -> i64 {{ self.b.len() }}
}}
fn show(e: own Either[i64]) -> String {{
    match e {{
        Either.Left(n) => f\"left {{n}}\",
        Either.Right(s) => f\"right {{s}}\",
    }}
}}
fn main() {{
    let p: Pair[i64] = Pair {{ a: 1, b: vec![2, 3] }};
    println(p.count());
    let q = Pair[i64] {{ a: 5, b: vec![] }};
    println(q.count());
    let e: Either[i64] = Either.Right(String.from(\"x\"));
    println(show(e));
    println(show(Either.Left(4)));
    let full: Pair[i64, String] = Pair {{ a: 1, b: String.from(\"s\") }};
    println(full.b);
}}
"
    );
    let (ok, out, err) = karac("fill", "run", &src);
    assert!(ok, "{err}");
    assert_eq!(out, "2\n0\nright x\nleft 4\ns\n");
}

#[test]
fn a_default_is_the_type_it_names() {
    let src = format!(
        "{TYPES}fn main() {{ let p: Pair[i64] = Pair {{ a: 1, b: String.from(\"s\") }}; println(p.a); }}\n"
    );
    let (ok, _, err) = karac("mismatch", "check", &src);
    assert!(!ok);
    assert!(err.contains("expected 'Vec[i64]', found 'String'"), "{err}");

    let (ok, _, err) = karac(
        "literal-arity",
        "check",
        &format!("{TYPES}fn main() {{ let p = Pair[i64, String, i64] {{ a: 1, b: String.from(\"s\") }}; }}\n"),
    );
    assert!(!ok);
    assert!(
        err.contains("`Pair` takes 2 type arguments, but 3 were supplied here"),
        "{err}"
    );
}

#[test]
fn a_type_parameter_without_a_default_cannot_follow_one_with() {
    let (ok, _, err) = karac(
        "order",
        "check",
        "struct Bad[A = i64, B] { a: A, b: B }\nfn main() { }\n",
    );
    assert!(!ok);
    assert!(
        err.contains("a generic parameter without a default cannot follow one with a default"),
        "{err}"
    );
}
