//! `v.iter().chunks(2).collect()` into a `Vec[Vec[i64]]`: E0236 drops the
//! `.iter()` hop, and the `collect` that then gathers slices is refused with
//! E0200, whose fix copies each slice into a `Vec` first,
//! `.map(|c| c.to_vec())` (B-2026-11-18-70). The fixed program checks and
//! runs; `windows` and `String` items take the same path.

use karac::{check_source, run_playground};

/// `src` after applying the first fixable error's edits, round by round, the
/// way `karac fix` reaches a fixpoint, until no error carries a fix.
fn mend(src: &str) -> String {
    let mut out = src.to_string();
    for _ in 0..6 {
        let ds = check_source(&out);
        let Some(d) = ds
            .iter()
            .find(|d| d.severity == "error" && !d.fixes.is_empty())
        else {
            break;
        };
        let mut fixes = d.fixes.clone();
        fixes.sort_by_key(|f| std::cmp::Reverse(f.offset));
        for f in fixes {
            out.replace_range(f.offset..f.offset + f.length, &f.replacement);
        }
    }
    out
}

#[test]
fn collected_chunks_are_copied_into_vecs() {
    let src = "fn main() {\n    let v: Vec[i64] = [1, 2, 3, 4, 5];\n    let cs: Vec[Vec[i64]] = v.iter().chunks(2).collect();\n    println(f\"{cs.len()} {cs[0][1]} {cs[2][0]}\");\n}\n";
    let out = mend(src);
    assert!(out.contains(".map(|c| c.to_vec()).collect()"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "3 2 5\n");
}

#[test]
fn collected_string_windows_are_copied_into_vecs() {
    let src = "fn main() {\n    let w: Vec[String] = [\"a\".to_string(), \"b\".to_string(), \"c\".to_string()];\n    let ws: Vec[Vec[String]] = w.iter().windows(2).collect();\n    println(f\"{ws.len()} {ws[0][1]} {ws[1][0]}\");\n}\n";
    let out = mend(src);
    assert!(out.contains(".map(|c| c.to_vec()).collect()"), "{out}");
    let after = run_playground(&out);
    assert!(after.ok, "{out}\n{:#?}", after.diagnostics);
    assert_eq!(after.stdout.concat(), "2 b b\n");
}

#[test]
fn a_slice_collect_into_vec_of_vecs_names_the_copy() {
    let src = "fn main() {\n    let a: Vec[i64] = [1, 2, 3];\n    let cs: Vec[Vec[i64]] = a.chunks(2).into_iter().collect();\n    println(cs.len());\n}\n";
    let d = check_source(src)
        .into_iter()
        .find(|d| d.code.as_deref() == Some("E0200"))
        .expect("E0200");
    assert!(d.message.contains("`.map(|c| c.to_vec())`"), "{d:#?}");
    assert_eq!(d.fixes.len(), 1, "{d:#?}");
    assert_eq!(d.fixes[0].replacement, ".map(|c| c.to_vec())");
}
