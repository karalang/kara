//! `karac fix` on three typo families probe wk49e found it left or made
//! worse (B-2026-11-26-16), each pinned fix-then-check:
//!
//! - A misspelled item in a std import (`import std.time.{Duratoin};`):
//!   E0113 suggested nothing, and the use's E0100 offered the misspelling,
//!   so the fix rewrote a correct `Duration` into `Duratoin`. E0113 now
//!   names the closest item and renames it, and a use of the name the fixed
//!   import binds is not reported or corrected beside it.
//! - A misspelled std module (`std.tiem`, `import std.randmo;`): E0112
//!   suggested the module with no edit, and the fix rewrote `random.` uses
//!   to `rand.`. The import's misspelled segment is now renamed.
//! - A method typo whose first argument opens with `(` (`v.psuh((1))`)
//!   lost its edit.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static CALL: AtomicUsize = AtomicUsize::new(0);

/// `karac fix` then `karac check` on `src`: the fixed text, and whether it
/// checks clean.
fn fix_then_check(src: &str) -> (String, bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-fix-typos-{}-{}",
        std::process::id(),
        CALL.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let f = dir.join("main.kara");
    std::fs::write(&f, src).unwrap();
    let _ = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("fix")
        .arg(&f)
        .output()
        .unwrap();
    let fixed = std::fs::read_to_string(&f).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&f)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        fixed,
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn fixes_to(src: &str, want: &str) {
    let (fixed, ok, out) = fix_then_check(src);
    assert!(fixed.contains(want), "{src}\nfixed to:\n{fixed}");
    assert!(ok, "{fixed}\n{out}");
}

#[test]
fn a_misspelled_std_import_item_is_renamed_not_copied_into_uses() {
    fixes_to(
        "import std.time.{Duratoin};\nfn main() { let d = Duration.ms(5); println(d.as_ms()); }\n",
        "import std.time.{Duration};\nfn main() { let d = Duration.ms(5);",
    );
    fixes_to(
        "import std.time.{Durtion, Instant};\n\
         fn main() { let d = Duration.ms(5); let t = Instant.now(); println(d.as_ms()); }\n",
        "import std.time.{Duration, Instant};",
    );
}

#[test]
fn a_misspelled_std_module_is_renamed_in_the_import() {
    fixes_to(
        "import std.tiem.{Duration};\nfn main() { println(Duration.ms(5).as_ms()); }\n",
        "import std.time.{Duration};",
    );
    fixes_to(
        "import std.randmo;\nfn main() { println(random.next_u64() > 0); }\n",
        "import std.random;\nfn main() { println(random.next_u64() > 0); }",
    );
}

#[test]
fn a_method_typo_before_a_parenthesized_argument_is_renamed() {
    fixes_to(
        "fn main() { let mut v: Vec[i64] = Vec.new(); v.psuh((1)); v.psuh(((2))); \
         v.psuh((1 + 2) * 3); println(v.len()); }\n",
        "v.push((1)); v.push(((2))); v.push((1 + 2) * 3);",
    );
}
