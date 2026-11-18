//! Generic distinct types (B-2026-11-18-43): `distinct type Ids[T] = Vec[T]`
//! was accepted at its declaration, but `Ids([1])` was E0200 "expected
//! 'Ids[i64]', found 'Ids'". The constructor is now generic as a tuple
//! variant's is, `.raw()` and the constructor pattern give the substituted
//! base, `#[derive(Default)]` builds `Ids(Vec.new())`, and a derived `Display`
//! shows the argument's value on both lanes.

use std::process::Command;

const BUILD: &str = "#[derive(PartialEq)]\ndistinct type Ids[T] = Vec[T];\nfn count(ids: Ids[i64]) -> i64 { ids.raw().len() }\nfn main() {\n    let a: Ids[i64] = Ids([1, 2]);\n    let b = Ids([1, 2]);\n    println(a == b);\n    println(count(a));\n    let Ids(v) = b;\n    println(v[1]);\n    let c: Ids[String] = Ids([\"x\".to_string()]);\n    match c { Ids(w) => println(w[0]) }\n}\n";
const BUILD_OUT: &str = "true\n2\n2\nx\n";

const DERIVES: &str = "#[derive(Default, PartialEq, Eq, Display, Clone)]\ndistinct type Ids[T] = Vec[T];\n#[derive(Default, PartialEq, Copy, Clone, Display)]\ndistinct type Wrap[T] = T;\nimpl[T] Ids[T] {\n    fn n(self) -> i64 { self.raw().len() }\n}\nfn first[T: Clone](x: Ids[T]) -> T { x.raw()[0].clone() }\nfn main() {\n    let a: Ids[i64] = Ids.default();\n    println(a.n());\n    let b = Ids([3, 4]);\n    println(first(b));\n    let b = Ids([3, 4]);\n    println(b.n());\n    let w: Wrap[i64] = Wrap.default();\n    let v = Wrap(7);\n    println(f\"{w} {v}\");\n    println(w == Wrap(0));\n    let s: Ids[String] = Ids([\"a\".to_string(), \"b\".to_string()]);\n    println(first(s));\n}\n";
const DERIVES_OUT: &str = "0\n3\n2\n0 7\ntrue\na\n";

const IN_GENERIC_FN: &str = "distinct type Ids[T] = Vec[T];\nfn mk[T]() -> Ids[T] { Ids(Vec.new()) }\nfn main() {\n    let a: Ids[i64] = Ids(Vec.new());\n    println(a.raw().len());\n    let b: Ids[i64] = mk();\n    println(b.raw().len());\n}\n";
const IN_GENERIC_FN_OUT: &str = "0\n0\n";

const MAP_KEY: &str = "#[derive(PartialEq, Eq, Hash, Clone, Display)]\ndistinct type Key[T] = T;\nfn main() {\n    let mut m: Map[Key[String], i64] = Map.new();\n    m[Key(\"a\".to_string())] = 1;\n    m[Key(\"b\".to_string())] = 2;\n    println(m[Key(\"a\".to_string())]);\n    let ks: Vec[Key[String]] = [Key(\"x\".to_string()), Key(\"y\".to_string())];\n    for k in ks { println(f\"{k}\"); }\n}\n";
const MAP_KEY_OUT: &str = "1\nx\ny\n";

fn run(tag: &str, lane: &str, src: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "karac-distinct-generic-{tag}-{lane}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(lane)
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        check.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert!(
        out.status.success(),
        "{tag} {lane}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_generic_distinct_type_is_built_and_unwrapped() {
    assert_eq!(run("build", "__mir-run", BUILD), BUILD_OUT);
}

#[test]
fn a_generic_distinct_type_derives_default_display_and_eq() {
    assert_eq!(run("derives", "__mir-run", DERIVES), DERIVES_OUT);
}

#[test]
fn a_generic_distinct_constructor_in_a_generic_fn_takes_its_parameter() {
    assert_eq!(
        run("generic-fn", "__mir-run", IN_GENERIC_FN),
        IN_GENERIC_FN_OUT
    );
}

#[test]
fn a_generic_distinct_type_is_a_map_key() {
    assert_eq!(run("map-key", "__mir-run", MAP_KEY), MAP_KEY_OUT);
}

#[cfg(feature = "llvm")]
#[test]
fn generic_distinct_types_agree_compiled() {
    assert_eq!(run("build", "__mir-llvm-run", BUILD), BUILD_OUT);
    assert_eq!(run("derives", "__mir-llvm-run", DERIVES), DERIVES_OUT);
    assert_eq!(
        run("generic-fn", "__mir-llvm-run", IN_GENERIC_FN),
        IN_GENERIC_FN_OUT
    );
    assert_eq!(run("map-key", "__mir-llvm-run", MAP_KEY), MAP_KEY_OUT);
}
