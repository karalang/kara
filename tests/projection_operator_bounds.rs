//! B-2026-11-13-48 (design.md §9, Operator traits): a where clause that
//! bounds a projection by an operator trait (`where I.Item: Add[Output =
//! I.Item]`, §9's own `total`) gives that projection the operator, as a
//! bound on a parameter does. The operator lookup read only parameter
//! bounds, so every lane refused it with "type 'I.Item' does not implement
//! trait Add". A bare `Add` names no `Output`, which the fix adds, exactly
//! as for `T: Add` (B-2026-10-27-14).

use std::path::PathBuf;
use std::process::Command;

fn karac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

fn fixture(tag: &str, src: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "karac-proj-op-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.kara");
    std::fs::write(&path, src).unwrap();
    (dir, path)
}

fn runs(tag: &str, src: &str, want: &str) {
    let (dir, path) = fixture(tag, src);
    let out = karac().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: check: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{tag}: run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{tag}");
    let _ = std::fs::remove_dir_all(&dir);
}

const MAIN: &str =
    "fn main() { let v: Vec[i64] = [2, 3, 4].to_vec(); println(total(v.into_iter(), ZERO)); }\n";

fn total(bound: &str, op: &str, zero: &str) -> String {
    format!(
        "fn total[I: Iterator](it: own I, zero: own I.Item) -> I.Item where I.Item: {bound} {{\n    \
         let mut acc = zero;\n    let mut it = it;\n    \
         while let Some(x) = it.next() {{ {op} }}\n    acc\n}}\n{}",
        MAIN.replace("ZERO", zero)
    )
}

#[test]
fn an_operator_bound_on_a_projection_gives_it_the_operator() {
    runs(
        "add",
        &total("Add[Output = I.Item]", "acc = acc + x;", "0"),
        "9\n",
    );
    runs(
        "mul",
        &total("Mul[Output = I.Item]", "acc = acc * x;", "1"),
        "24\n",
    );
    runs(
        "compound",
        &total(
            "Add[Output = I.Item] + Sub[Output = I.Item]",
            "acc += x; acc = acc - x + x;",
            "0",
        ),
        "9\n",
    );
}

#[test]
fn a_bare_projection_operator_bound_is_fixed_to_name_its_output() {
    let (dir, path) = fixture("bare", &total("Add", "acc = acc + x;", "0"));
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("write `Add[Output = I.Item]`"), "{err}");
    karac().arg("fix").arg(&path).output().unwrap();
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("where I.Item: Add[Output = I.Item] {"),
        "{fixed}"
    );
    let out = karac().arg("__mir-run").arg(&path).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "9\n", "{fixed}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_projection_operator_takes_the_right_hand_type_its_bound_names() {
    let (dir, path) = fixture("rhs", &total("Add[Output = I.Item]", "acc = acc + 1;", "0"));
    let out = karac().arg("check").arg(&path).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "check must refuse: {err}");
    assert!(err.contains("expected 'I.Item', found 'i64'"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
