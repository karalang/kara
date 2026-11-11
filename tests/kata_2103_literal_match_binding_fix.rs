//! Kata 2103's Rust template binds `let bit = match pair[0] { b'R' => 1,
//! b'G' => 2, _ => 4 };` and then writes `rods[i] |= bit` into a
//! `[0u8; 10]`. Such a `let` took the `i64` default at once, so the store
//! was refused as narrowing. Since B-2026-11-10-49 a join of literals
//! stays open until a use fixes it (design.md §8), so `side` below takes
//! the `u8` its use gives it. Whether `|=` fixes `bit` too (the §8
//! ruling of 2026-11-10) or the refusal's `bit: u8` fix does, the Mend
//! loop ends in a program that runs.

#[test]
fn kata_2103_match_of_literals_binding_takes_its_use() {
    let src = "fn main() {
    let mut rods = [0u8; 3];
    for pair in \"R0G0B0R1\".bytes().chunks(2) {
        let bit = match pair[0] {
            b'R' => 1,
            b'G' => 2,
            _ => 4,
        };
        rods[(pair[1] - b'0') as i64] |= bit;
    }
    let side = if rods[0] == 7 { 1 } else { 0 };
    let total: u8 = side;
    println(f\"{rods[0]} {rods[1]} {total}\");
}
";
    let path = std::env::temp_dir().join(format!("karac-k2103-join-{}.kara", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let fix = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["fix", path.to_str().unwrap()])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("karac");
    assert!(
        fix.status.success(),
        "{}",
        String::from_utf8_lossy(&fix.stderr)
    );
    let fixed = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(fixed.contains("let side = if"), "{fixed}");
    let out = karac::mir::lower::run_source(&fixed)
        .expect("MIR interpreter")
        .output;
    assert_eq!(out, "7 1 1\n");
}
