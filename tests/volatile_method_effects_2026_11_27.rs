//! `p.read_volatile()` and `p.write_volatile(v)` on a raw pointer carry the
//! effects of the `volatile_read` / `volatile_write` intrinsics they are the
//! method form of: `reads(Hardware)` and `writes(Hardware)` (review ruling
//! 2026-11-26), as `VolatileCell`'s read and write declare. They carried
//! none, so a `pub fn` doing one declared nothing and `query effects`
//! listed nothing (B-2026-11-26-98). A program type's own method named
//! `read_volatile` is not a pointer's and keeps its own effects.

use std::process::Command;

fn karac(args: &[&str], name: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!(
        "karac-volatile-effects-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(args)
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const UNDECLARED: &str = "pub fn rd(p: *const i32) -> i32 {\n    // Safety: test\n    unsafe { p.read_volatile() }\n}\npub fn wr(p: *mut i32, v: i32) {\n    // Safety: test\n    unsafe { p.write_volatile(v) }\n}\nfn main() {}\n";

#[test]
fn volatile_method_effects_must_be_declared_on_a_pub_fn() {
    let (ok, t) = karac(&["check"], "undeclared", UNDECLARED);
    assert!(!ok, "accepted:\n{t}");
    assert!(
        t.contains("public function 'rd' performs effects [reads(Hardware)]"),
        "{t}"
    );
    assert!(
        t.contains("public function 'wr' performs effects [writes(Hardware)]"),
        "{t}"
    );
}

#[test]
fn volatile_method_effects_declared_check_and_are_queried() {
    let src = "pub fn rd(p: *const i32) -> i32 with reads(Hardware) {\n    // Safety: test\n    unsafe { p.read_volatile() }\n}\nfn wr(p: *mut i32, v: i32) {\n    // Safety: test\n    unsafe { p.write_volatile(v) }\n}\npub fn poke(p: *mut i32) with writes(Hardware) { wr(p, 1); }\nfn main() {}\n";
    let (ok, t) = karac(&["check"], "declared", src);
    assert!(ok, "{t}");
    let (ok, q) = karac(&["query", "effects"], "declared_query", UNDECLARED);
    assert!(ok, "{q}");
    assert!(
        q.contains(r#""function":"rd","line":1,"is_test":false,"inferred_effects":[{"verb":"reads","resource":"Hardware"}]"#),
        "{q}"
    );
    assert!(
        q.contains(r#""function":"wr","line":5,"is_test":false,"inferred_effects":[{"verb":"writes","resource":"Hardware"}]"#),
        "{q}"
    );
}

#[test]
fn volatile_method_effects_a_program_method_of_that_name_keeps_its_own() {
    let src = "pub struct Reg { v: i32 }\nimpl Reg {\n    pub fn read_volatile(ref self) -> i32 { self.v }\n}\npub fn get(r: ref Reg) -> i32 { r.read_volatile() }\nfn main() { let r = Reg { v: 3 }; println(get(r)); }\n";
    let (ok, t) = karac(&["check"], "own_method", src);
    assert!(ok, "{t}");
}
