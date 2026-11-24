//! The MIR interpreter's `String.bytes()` snapshot belongs to the String
//! it copies (B-2026-11-21-51): asked again unchanged the String gets the
//! same one, a changed String a fresh one, and the snapshot is freed with
//! the String. Every call used to make a snapshot that only a sweep
//! between calls back into Kāra could free, so a program doing its work
//! inside `map` closures (kata 2416's iter arm) grew about 1 GB a second.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn karac(mode: &str, src: &str) -> Output {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("karac-bytes-owner-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg(mode)
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// Bytes read again after the String changed, moved into a `Vec`, cloned,
/// of a literal, and of Strings dropped inside a callback.
const SEMANTICS: &str = r#"fn first_last(b: Slice[u8]) -> i64 {
    (b[0] as i64) * 1000 + (b[b.len() - 1] as i64)
}

fn main() {
    let mut s = "abc".to_string();
    let n1 = s.bytes().len();
    s.push('d');
    let b = s.bytes();
    println(f"{n1} {b.len()} {b[3]}");
    s.push_str("ef");
    println(f"{s.bytes().len()} {first_last(s.bytes())}");
    let mut v: Vec[String] = Vec.new();
    v.push(s);
    v.push("xyz".to_string());
    let c = v[0].clone();
    println(f"{first_last(v[0].bytes())} {first_last(v[1].bytes())} {first_last(c.bytes())}");
    v[0].push('g');
    println(f"{first_last(v[0].bytes())} {first_last(c.bytes())}");
    println(f"{first_last("hello".bytes())} {first_last("hello".bytes())}");
    let total: i64 = (0..3000)
        .map(|i| {
            let t = f"{i % 10}{"q".repeat(400)}";
            first_last(t.bytes())
        })
        .sum();
    println(total);
    let words = ["apple".to_string(), "apply".to_string(), "ape".to_string()].to_vec();
    let common: Vec[i64] = words
        .iter()
        .map(|w| words.iter().map(|o| w.bytes().iter().zip(o.bytes().iter()).take_while(|(a, b)| a == b).count()).sum())
        .collect();
    println(common);
}
"#;

const WANT: &str =
    "3 4 100\n6 97102\n97102 120122 97102\n97103 97102\n104111 104111\n157839000\n[11, 11, 7]\n";

fn runs(mode: &str) {
    let out = karac(mode, SEMANTICS);
    assert!(
        out.status.success(),
        "{mode}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{mode}");
}

#[test]
fn interp_bytes_snapshot_follows_its_string() {
    runs("__mir-run");
}

#[cfg(feature = "llvm")]
#[test]
fn compiled_bytes_snapshot_follows_its_string() {
    runs("__mir-llvm-run");
}

/// 200 words of 600 letters, each pair compared through `bytes()` inside
/// nested `map` closures: 80,000 calls. The interpreter peaked at 1.9 GB
/// before the fix and about 110 MB after (debug build).
const NESTED: &str = r#"fn main() {
    let mut words: Vec[String] = Vec.new();
    let mut x = 17;
    for _ in 0..200 {
        let mut w = String.new();
        for _ in 0..600 {
            x = (x * 1103515245 + 12345) % 2147483648;
            w.push(((97 + x % 3) as u8) as char);
        }
        words.push(w);
    }
    let common: Vec[i64] = words
        .iter()
        .map(|w| words.iter().map(|o| w.bytes().iter().zip(o.bytes().iter()).take_while(|(a, b)| a == b).count()).sum())
        .collect();
    let total: i64 = common.iter().sum();
    println(f"{common.len()} {total}");
    let status = fs.read_to_string("/proc/self/status").unwrap();
    for l in status.lines() {
        if l.starts_with("VmHWM:") {
            println(l);
        }
    }
}
"#;

#[cfg(target_os = "linux")]
#[test]
fn interp_bytes_inside_callbacks_keeps_one_snapshot_per_string() {
    let out = karac("__mir-run", NESTED);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.starts_with("200 139856\n"), "{text}");
    let kib: i64 = text
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .unwrap_or_else(|| panic!("no VmHWM in {text}"))
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .unwrap();
    assert!(kib < 512 * 1024, "peak {kib} KiB");
}
