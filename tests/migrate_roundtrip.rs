//! `karac migrate shared-to-par` round trips (B-2026-11-16-285): every
//! migration either leaves code that checks and runs as before, or is refused
//! with every file put back. Plus `karac check <file>` on a module importing a
//! `shared struct` (B-2026-11-17-114), which the multi-file migration re-check
//! runs through.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("karac-migrate-rt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    dir
}

fn karac(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_karac"))
        .current_dir(dir)
        .env("KARAC_HASH_SEED", "1")
        .args(args)
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Migrate `file` (one file, `--apply --force`), then check and run the
/// result, which must print `expected` as the original did.
fn round_trip_file(name: &str, ty: &str, source: &str, expected: &str) -> String {
    let dir = scratch(name);
    std::fs::write(dir.join("m.kara"), source).unwrap();
    let before = karac(&dir, &["run", "m.kara"]);
    assert_eq!(
        stdout(&before),
        expected,
        "original run: {}",
        stderr(&before)
    );
    let mig = karac(
        &dir,
        &[
            "migrate",
            "shared-to-par",
            ty,
            "m.kara",
            "--apply",
            "--force",
        ],
    );
    let migrated = std::fs::read_to_string(dir.join("m.kara")).unwrap();
    assert!(
        mig.status.success(),
        "migrate refused: {}\n{}",
        stderr(&mig),
        migrated
    );
    let check = karac(&dir, &["check", "m.kara"]);
    assert!(
        check.status.success(),
        "migrated code does not check:\n{migrated}\n{}{}",
        stdout(&check),
        stderr(&check)
    );
    let after = karac(&dir, &["run", "m.kara"]);
    assert_eq!(
        stdout(&after),
        expected,
        "migrated run:\n{migrated}\n{}",
        stderr(&after)
    );
    let _ = std::fs::remove_dir_all(&dir);
    migrated
}

#[test]
fn migrate_round_trip_wraps_writes_reads_and_literals() {
    let out = round_trip_file(
        "basic",
        "Counter",
        "shared struct Counter { mut n: i64, mut log: Vec[i64] }\n\
         fn bump(c: Counter) { c.n = c.n + 1; c.log.push(c.n); }\n\
         fn main() {\n\
         \x20   let c = Counter { n: 0, log: Vec.new() };\n\
         \x20   bump(c);\n\
         \x20   bump(c);\n\
         \x20   println(c.n);\n\
         \x20   println(c.log.len());\n\
         }\n",
        "2\n2\n",
    );
    assert!(out.contains("sync struct Counter { n: Mutex[i64], log: Mutex[Vec[i64]] }"));
    // The wrap names `Mutex`, which the migrated program then imports.
    assert!(out.starts_with("import std.sync.{Mutex};\n"), "{out}");
    assert!(out.contains("Counter { n: Mutex.new(0), log: Mutex.new(Vec.new()) }"));
}

#[test]
fn migrate_round_trip_methods_on_self_index_and_shorthand() {
    let out = round_trip_file(
        "self",
        "Counter",
        "shared struct Counter { mut n: i64, mut log: Vec[i64], name: String }\n\
         impl Counter {\n\
         \x20   fn bump(self) { self.n += 1; self.log.push(self.n); }\n\
         \x20   fn total(self) -> i64 { self.n + self.log.len() }\n\
         }\n\
         fn first(c: Counter) -> i64 { c.log[0] }\n\
         fn main() {\n\
         \x20   let n = 5;\n\
         \x20   let c = Counter { n, log: Vec.new(), name: \"a\" };\n\
         \x20   c.bump();\n\
         \x20   c.n += 2;\n\
         \x20   if c.n > 3 { println(\"big\") }\n\
         \x20   println(c.total());\n\
         \x20   println(first(c));\n\
         \x20   println(c.name);\n\
         }\n",
        "big\n9\n6\na\n",
    );
    assert!(out.contains("let n_guard = self.n.lock();"), "{out}");
    assert!(out.contains("Counter { n: Mutex.new(n),"), "{out}");
}

#[test]
fn migrate_reads_a_held_field_through_its_guard() {
    // A second `lock()` of the mutex the call already holds would deadlock.
    let out = round_trip_file(
        "nested",
        "Bag",
        "shared struct Bag { mut items: Vec[i64] }\n\
         fn main() {\n\
         \x20   let b = Bag { items: Vec.new() };\n\
         \x20   b.items.push(1);\n\
         \x20   println(b.items.contains(b.items.len()));\n\
         \x20   println(b.items[b.items.len() - 1]);\n\
         }\n",
        "true\n1\n",
    );
    assert!(
        out.contains("items_guard.value.contains(items_guard.value.len())"),
        "{out}"
    );
}

#[test]
fn migrate_round_trip_across_modules_with_atomics() {
    let dir = scratch("project");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(
        dir.join("src/counter.kara"),
        "pub shared struct Counter {\n    pub mut count: i64,\n    pub mut hits: i64,\n}\n\
         pub fn make() -> Counter { Counter { count: 0, hits: 0 } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import counter.{Counter, make};\n\
         fn set(c: Counter, v: i64) { c.count = v; }\n\
         fn hit(c: Counter) { c.hits += 1; }\n\
         fn main() {\n\
         \x20   let c = make();\n\
         \x20   set(c, 7);\n\
         \x20   hit(c);\n\
         \x20   hit(c);\n\
         \x20   println(c.count + c.hits);\n\
         }\n",
    )
    .unwrap();
    assert_eq!(stdout(&karac(&dir, &["run"])), "9\n");
    let mig = karac(
        &dir,
        &[
            "migrate",
            "shared-to-par",
            "Counter",
            "--atomic",
            "--apply",
            "--force",
        ],
    );
    let main = std::fs::read_to_string(dir.join("src/main.kara")).unwrap();
    assert!(mig.status.success(), "{}\n{main}", stderr(&mig));
    // `c` is typed only through the imported `make`; the atomic takes no
    // ordering argument (that form is deprecated).
    assert!(main.contains("c.count.store(v);"), "{main}");
    assert!(main.contains("println(c.count.load() + "), "{main}");
    let check = karac(&dir, &["check"]);
    assert!(
        check.status.success(),
        "{}{}",
        stdout(&check),
        stderr(&check)
    );
    assert_eq!(stdout(&karac(&dir, &["run"])), "9\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn migrate_refuses_and_restores_output_that_does_not_check() {
    // Iterating a guarded field is not rewritten; the result would move the
    // Vec out of the guard, so `--apply` must put the file back.
    let dir = scratch("refuse");
    let source = "shared struct Flag { mut items: Vec[String] }\n\
                  fn main() {\n\
                  \x20   let f = Flag { items: Vec.new() };\n\
                  \x20   f.items.push(\"x\");\n\
                  \x20   for s in f.items { println(s) }\n\
                  }\n";
    std::fs::write(dir.join("m.kara"), source).unwrap();
    let dry = karac(&dir, &["migrate", "shared-to-par", "Flag", "m.kara"]);
    assert!(
        stderr(&dry).contains("would not check; `--apply` will refuse it"),
        "{}",
        stderr(&dry)
    );
    let mig = karac(
        &dir,
        &[
            "migrate",
            "shared-to-par",
            "Flag",
            "m.kara",
            "--apply",
            "--force",
        ],
    );
    assert!(!mig.status.success());
    assert!(
        stderr(&mig).contains("the migrated code does not check, so every file was restored"),
        "{}",
        stderr(&mig)
    );
    assert_eq!(std::fs::read_to_string(dir.join("m.kara")).unwrap(), source);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_file_accepts_an_imported_shared_struct_in_a_signature() {
    // `show(c: Counter)` was lowered before the import registered `Counter`
    // as shared, so it named a plain nominal: "expected 'Counter', found
    // 'Counter'" from `karac check <file>` and the editor, never `karac check`.
    let dir = scratch("shared-import");
    std::fs::write(dir.join("kara.toml"), "[package]\nname = \"demo\"\n").unwrap();
    std::fs::write(
        dir.join("src/counter.kara"),
        "pub shared struct Counter { pub n: i64 }\n\
         pub fn make() -> Counter { Counter { n: 0 } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.kara"),
        "import counter.{Counter, make};\n\
         fn show(c: Counter) { println(c.n); }\n\
         fn main() {\n\
         \x20   let c: Counter = make();\n\
         \x20   show(Counter { n: 1 });\n\
         \x20   show(c);\n\
         }\n",
    )
    .unwrap();
    for args in [
        &["check", "src/main.kara"][..],
        &["check", "src/counter.kara"][..],
    ] {
        let out = karac(&dir, args);
        assert!(
            out.status.success(),
            "{args:?}: {}{}",
            stdout(&out),
            stderr(&out)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
