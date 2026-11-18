//! B-2026-11-18-65: `karac fix` held back every `lossless_cast`,
//! `redundant_cast` and `redundant_clone` fix in a program with a mutual
//! recursion group. Those fixes wait for a pass with no error, and the
//! group's L0002 note, which sits among the effect errors, counted as one.
//! The fix-idempotence sweep found 199 corpus programs that checked clean
//! and kept such a warning after `karac fix`.

use std::process::{Command, Output, Stdio};

fn karac(args: &[&str], path: &std::path::Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .expect("karac")
}

#[test]
fn a_mutual_recursion_note_does_not_hold_back_the_cast_fixes() {
    let src = "struct Cursor {
    bytes: Vec[u8],
    at: i64,
}

impl Cursor {
    fn peek(self) -> u8 {
        if self.at < self.bytes.len() { self.bytes[self.at] } else { 0 }
    }

    fn sum(mut ref self) -> i64 {
        let mut value = self.factor();
        while self.peek() == b'+' {
            self.at += 1;
            value = value + self.factor();
        }
        value
    }

    fn factor(mut ref self) -> i64 {
        if self.peek() == b'(' {
            self.at += 1;
            let value = self.sum();
            self.at += 1;
            return value;
        }
        let mut value = 0;
        while self.peek() >= b'0' and self.peek() <= b'9' {
            value = value * 10 + (self.peek() - b'0') as i64;
            self.at += 1;
        }
        value as i64
    }
}

fn main() {
    let mut c = Cursor { bytes: \"1+(20+3)\".bytes().to_vec(), at: 0 };
    println(c.sum());
}
";
    let path = std::env::temp_dir().join(format!(
        "karac-fix-recursion-note-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).unwrap();
    let check = karac(&["check"], &path);
    let err = String::from_utf8_lossy(&check.stderr).into_owned();
    assert!(err.contains("warning[lossless_cast]"), "{err}");
    assert!(err.contains("mutual recursion group"), "{err}");
    let fix = karac(&["fix"], &path);
    assert!(fix.status.success(), "{fix:?}");
    let fixed = std::fs::read_to_string(&path).unwrap();
    assert!(
        fixed.contains("value = value * 10 + (self.peek() - b'0');"),
        "{fixed}"
    );
    assert!(fixed.contains("        value\n    }\n}"), "{fixed}");
    let again = karac(&["fix"], &path);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), fixed, "{again:?}");
    let after = karac(&["check"], &path);
    let after_err = String::from_utf8_lossy(&after.stderr).into_owned();
    assert!(!after_err.contains("_cast]"), "{after_err}");
    let run = karac(&["__mir-run"], &path);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "24\n", "{run:?}");
    let _ = std::fs::remove_file(&path);
}
