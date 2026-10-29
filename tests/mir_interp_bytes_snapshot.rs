//! The MIR interpreter hands `s.bytes()` a read-only snapshot of the text,
//! and reuses the snapshot of an unchanged string instead of copying it on
//! every call (B-2026-10-29-38). A string changed between calls gets a fresh
//! one, and several strings asked in turn each read their own bytes.

#[test]
fn bytes_snapshots_are_reused_only_for_unchanged_text() {
    let src = "fn byte_at(s: Str, i: i64) -> i64 {\n\
               \x20   let bytes = s.bytes();\n\
               \x20   bytes[i] as i64\n}\n\n\
               fn main() {\n\
               \x20   let mut s = String.new();\n\
               \x20   for _ in 0..100000 { s.push('7'); }\n\
               \x20   let mut total = 0;\n\
               \x20   for k in 0..3000 { total += byte_at(s, k % 100000); }\n\
               \x20   s.push('8');\n\
               \x20   total += byte_at(s, 100000);\n\
               \x20   let names = [\"ab\", \"cd\", \"ef\", \"gh\", \"ij\", \"kl\"].to_vec();\n\
               \x20   let mut sum = 0;\n\
               \x20   for round in 0..3 {\n\
               \x20       for n in names.iter() { sum += byte_at(n, round % 2); }\n\
               \x20   }\n\
               \x20   let before = s.bytes().len();\n\
               \x20   s.push('9');\n\
               \x20   println(f\"{total} {sum} {before} {s.bytes().len()} {byte_at(s, 100001)}\");\n}\n";
    let run = karac::mir::lower::run_source(src).unwrap_or_else(|e| panic!("build: {e}"));
    // 3000 sevens (55), then the '8' (56); the names' bytes summed over
    // three rounds; the length before and after one more push.
    let names: i64 = (0..3)
        .map(|r| {
            ["ab", "cd", "ef", "gh", "ij", "kl"]
                .iter()
                .map(|n| i64::from(n.as_bytes()[r % 2]))
                .sum::<i64>()
        })
        .sum();
    assert_eq!(
        run.output,
        format!("{} {names} 100001 100002 57\n", 3000 * 55 + 56)
    );
}
