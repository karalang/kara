//! §12 trait-ceiling shapes the design-effects probe filed (B-2026-11-06-29).
//!
//! - A trait method's clause is a ceiling, and `allocates(Heap)` and `panics`
//!   never have to be declared under it: §12's own `ReadOnlyView.save`, whose
//!   body panics, is accepted, and so is an impl that allocates.
//! - A call through a bound whose trait method writes a clause has the
//!   clause's effects whatever the instance, so a `pub` generic fn is checked
//!   at its definition, called or not.

use std::process::{Command, Output, Stdio};

fn check(tag: &str, src: &str) -> Output {
    let path = std::env::temp_dir().join(format!(
        "karac-design-effects-{tag}-{}.kara",
        std::process::id()
    ));
    std::fs::write(&path, src).expect("write");
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .expect("karac");
    let _ = std::fs::remove_file(&path);
    out
}

fn clean(tag: &str, src: &str) {
    let out = check(tag, src);
    assert!(
        out.status.success(),
        "{tag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn refused(tag: &str, src: &str, err: &str) {
    let out = check(tag, src);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{tag} was accepted");
    assert!(stderr.contains(err), "{tag}: {stderr}");
}

#[test]
fn design_effects_ceiling_admits_default_permitted_effects() {
    let src = "effect resource Data;\ntrait Storage { fn save(self, k: i64) with writes(Data); }\nstruct Ro {}\nimpl Storage for Ro { fn save(self, k: i64) with writes(Data) { panic(\"read-only view\"); } }\nstruct Grow {}\nimpl Storage for Grow { fn save(self, k: i64) with writes(Data) { let v: Vec[i64] = [k, k]; let _ = v.len(); } }\nfn main() {}\n";
    clean("ceiling-defaults", src);
    // A real widening is still refused, naming the ceiling.
    let src = "effect resource Data;\ntrait Storage { fn save(self, k: i64) with writes(Data); }\nstruct Loud {}\nimpl Storage for Loud { fn save(self, k: i64) with writes(Data) { println(k); } }\nfn main() {}\n";
    refused(
        "ceiling-widened",
        src,
        "impl method 'Loud.save' performs writes(Stdout) but trait 'Storage.save' ceiling is [writes(Data)]",
    );
}

#[test]
fn design_effects_pub_generic_fn_is_charged_its_bound_ceiling() {
    let src = "effect resource Data;\ntrait Storage { fn load(self, k: i64) -> i64 with reads(Data); }\npub fn use_it[S: Storage](s: S) -> i64 { s.load(1) }\nfn main() {}\n";
    refused(
        "bound-uncalled",
        src,
        "public function 'use_it' performs effects [reads(Data)] but has no effect declaration",
    );
    // Through a private generic helper too.
    let src = "effect resource Data;\ntrait Storage { fn load(self, k: i64) -> i64 with reads(Data); }\nfn helper[S: Storage](s: S) -> i64 { s.load(2) }\npub fn outer[S: Storage](s: S) -> i64 { helper(s) }\nfn main() {}\n";
    refused(
        "bound-helper",
        src,
        "public function 'outer' performs effects [reads(Data)] but has no effect declaration",
    );
    let src = "effect resource Data;\ntrait Storage { fn load(self, k: i64) -> i64 with reads(Data); }\npub fn use_it[S: Storage](s: S) -> i64 with reads(Data) { s.load(1) }\nfn main() {}\n";
    clean("bound-declared", src);
}
