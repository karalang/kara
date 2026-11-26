//! B-2026-11-26-80: `karac catalog` reports a parameter's DECLARED mode as
//! design.md §7 reads it, the same as `karac explain` and `karac query
//! ownership`: a bare `x: T` borrows, so it is `ref`; only a written `own T`
//! is `own`. Before the fix every bare parameter catalogued as `own`, the
//! meaning a bare `T` had before D5.

use std::process::Command;

fn catalog(name: &str, src: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("karac_catalog_modes_{}_{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("m.kara");
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["catalog", path.to_str().unwrap()])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn catalog_reads_a_bare_parameter_as_borrowed_and_own_t_as_owned() {
    let out = catalog(
        "modes",
        "pub fn f(a: String, b: own String, c: mut ref Vec[i64], d: i64) -> i64 {\n    0\n}\nfn main() {}\n",
    );
    let line = out.trim();
    assert!(
        line.contains(
            "\"params\":[{\"name\":\"a\",\"mode\":\"ref\",\"ty\":\"String\"},\
             {\"name\":\"b\",\"mode\":\"own\",\"ty\":\"String\"},\
             {\"name\":\"c\",\"mode\":\"mut ref\",\"ty\":\"Vec[i64]\"},\
             {\"name\":\"d\",\"mode\":\"ref\",\"ty\":\"i64\"}]"
        ),
        "got: {line}"
    );
}

#[test]
fn catalog_mode_of_a_pub_method_parameter_follows_the_same_rule() {
    let out = catalog(
        "method",
        "pub struct P {\n    x: i64,\n}\nimpl P {\n    pub fn put(mut ref self, v: i64, w: own String) {\n        self.x = v;\n    }\n}\nfn main() {}\n",
    );
    let method = out
        .lines()
        .find(|l| l.contains("\"kind\":\"impl_method\""))
        .unwrap_or_else(|| panic!("no impl_method record: {out}"));
    assert!(
        method.contains("{\"name\":\"v\",\"mode\":\"ref\",\"ty\":\"i64\"}")
            && method.contains("{\"name\":\"w\",\"mode\":\"own\",\"ty\":\"String\"}"),
        "got: {method}"
    );
}
