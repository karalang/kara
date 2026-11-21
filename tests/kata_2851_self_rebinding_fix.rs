//! `let n = n as i64;` (katas 2851 and 2852, Rust's way to make a `usize`
//! length an `i64`), carried by `karac fix`. A length is already an `i64`
//! in Kāra (`v.len()` is one), so the fix deletes the cast as redundant, and it used to leave
//! `let n = n;` behind, a statement that does nothing; the tuple form left
//! `let (nr, nc) = (nr, nc);`. The `self_rebinding` lint now names such a
//! `let` and its fix deletes the line. A `let x = x;` that freezes a
//! `mut` binding is the same program only with the freeze, so it is left
//! alone.

fn fix_round(src: &str) -> Option<String> {
    let mut edits: Vec<karac::SourceFix> = karac::check_source(src)
        .into_iter()
        .flat_map(|d| d.fixes)
        .collect();
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|e| std::cmp::Reverse(e.offset));
    let mut out = src.to_string();
    for e in edits {
        out.replace_range(e.offset..e.offset + e.length, &e.replacement);
    }
    Some(out)
}

fn fixed(src: &str) -> String {
    let mut now = src.to_string();
    for _ in 0..5 {
        match fix_round(&now) {
            Some(next) => now = next,
            None => break,
        }
    }
    now
}

fn said(src: &str, lint: &str) -> Vec<String> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.message.contains(lint))
        .map(|d| d.message)
        .collect()
}

#[test]
fn kata_2851_a_cast_of_a_length_to_itself_goes_with_its_let() {
    let src = "fn spread(v: Vec[i64], grid: Vec[Vec[i64]]) -> i64 {\n\
               \x20   let n = v.len();\n\
               \x20   let n = n as i64;\n\
               \x20   let (rows, cols) = (grid.len(), grid[0].len());\n\
               \x20   let (rows, cols) = (rows as i64, cols as i64);\n\
               \x20   n * rows + cols\n}\n\n\
               fn main() {\n\
               \x20   let row = [1, 2, 3, 4, 5];\n\
               \x20   println(f\"{spread([1, 2, 3], [row, row, row, row])}\");\n}\n";
    let out = fixed(src);
    assert!(!out.contains("let n = n"), "the self-binding stays:\n{out}");
    assert!(
        !out.contains("(rows, cols) = (rows"),
        "the tuple self-binding stays:\n{out}"
    );
    assert!(
        out.contains("    let n = v.len();\n    let (rows, cols) = (grid.len(), grid[0].len());\n    n * rows + cols\n}"),
        "the lines go whole, indentation and newline:\n{out}"
    );
    let clean: Vec<_> = karac::check_source(&out)
        .into_iter()
        .filter(|d| d.severity == "error" || !d.fixes.is_empty())
        .map(|d| d.message)
        .collect();
    assert!(clean.is_empty(), "{clean:?}\n{out}");
    let run = karac::mir::lower::run_source(&out).unwrap_or_else(|e| panic!("build: {e}"));
    assert_eq!(run.output, "17\n");
}

#[test]
fn kata_2851_a_let_that_freezes_a_mut_binding_is_left_alone() {
    let src = "fn main() {\n\
               \x20   let mut total = 1;\n\
               \x20   total += 2;\n\
               \x20   let total = total;\n\
               \x20   let k = 4;\n\
               \x20   let k = k;\n\
               \x20   println(f\"{total} {k}\");\n}\n";
    let warned = said(src, "binds `");
    assert_eq!(warned.len(), 1, "only `let k = k;`: {warned:?}");
    assert!(warned[0].contains("binds `k` to itself"), "{warned:?}");
    let out = fixed(src);
    assert!(
        out.contains("let total = total;"),
        "the freeze goes:\n{out}"
    );
    assert!(!out.contains("let k = k;"), "{out}");
}
