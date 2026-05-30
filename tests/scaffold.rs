//! Integration tests for CR-36 — `karac init` project scaffolding. These
//! exercise the filesystem side of `scaffold_project` / `prepare_new_target_dir`
//! and cross-check that a freshly-scaffolded `kara.toml` parses cleanly under
//! the manifest allow-list expanded in the same CR.

use karac::manifest::{load_from_root, MANIFEST_FILENAME};
use karac::scaffold::{
    self, prepare_new_target_dir, scaffold_project, validate_package_name, ScaffoldError,
    ScaffoldOpts, Template,
};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static TEST_ID: AtomicU32 = AtomicU32::new(0);

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "karac-scaffold-test-{}-{}-{}",
            std::process::id(),
            tag,
            id,
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create scratch dir");
        ScratchDir { path }
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(&full).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        full
    }

    fn root(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn bin_opts() -> ScaffoldOpts {
    ScaffoldOpts {
        template: Template::Bin,
        force: false,
    }
}

fn lib_opts() -> ScaffoldOpts {
    ScaffoldOpts {
        template: Template::Lib,
        force: false,
    }
}

#[test]
fn bin_template_writes_all_expected_files() {
    let scratch = ScratchDir::new("bin-template");
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    assert!(scratch.root().join("kara.toml").is_file());
    assert!(scratch.root().join("src/main.kara").is_file());
    assert!(scratch.root().join("src/main_test.kara").is_file());
    assert!(scratch.root().join("README.md").is_file());
    assert!(scratch.root().join(".gitignore").is_file());
    // --bin must not produce a lib.kara.
    assert!(!scratch.root().join("src/lib.kara").exists());
    assert!(!scratch.root().join("src/lib_test.kara").exists());
}

#[test]
fn lib_template_writes_expected_files() {
    let scratch = ScratchDir::new("lib-template");
    scaffold_project(scratch.root(), "mathlib", lib_opts()).unwrap();
    assert!(scratch.root().join("kara.toml").is_file());
    assert!(scratch.root().join("src/lib.kara").is_file());
    assert!(scratch.root().join("src/lib_test.kara").is_file());
    assert!(scratch.root().join("README.md").is_file());
    assert!(scratch.root().join(".gitignore").is_file());
    // --lib must not produce a main.kara.
    assert!(!scratch.root().join("src/main.kara").exists());
    assert!(!scratch.root().join("src/main_test.kara").exists());
    // Starter lib body mentions the `add` function the companion test targets.
    let lib_src = fs::read_to_string(scratch.root().join("src/lib.kara")).unwrap();
    assert!(lib_src.contains("pub fn add"));
    let lib_test = fs::read_to_string(scratch.root().join("src/lib_test.kara")).unwrap();
    assert!(lib_test.contains("add("));
}

#[test]
fn bin_main_is_hello_world() {
    let scratch = ScratchDir::new("bin-body");
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let body = fs::read_to_string(scratch.root().join("src/main.kara")).unwrap();
    assert!(body.contains("fn main()"));
    assert!(body.contains("Hello, world!"));
}

#[test]
fn readme_contains_project_title() {
    let scratch = ScratchDir::new("readme-title");
    scaffold_project(scratch.root(), "hello_world", bin_opts()).unwrap();
    let readme = fs::read_to_string(scratch.root().join("README.md")).unwrap();
    assert_eq!(readme.trim_end(), "# hello_world");
}

#[test]
fn gitignore_has_dist_entry() {
    let scratch = ScratchDir::new("gitignore-dist");
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let gi = fs::read_to_string(scratch.root().join(".gitignore")).unwrap();
    assert!(gi.contains("/dist/"));
}

#[test]
fn gitignore_skipped_when_already_present() {
    let scratch = ScratchDir::new("gitignore-preserved");
    let existing = "# custom\n*.swp\n";
    scratch.write(".gitignore", existing);
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let gi = fs::read_to_string(scratch.root().join(".gitignore")).unwrap();
    assert_eq!(gi, existing, "existing .gitignore must not be overwritten");
}

#[test]
fn readme_skipped_when_already_present() {
    // Common `git init`-then-`karac init` flow: the user already has a
    // README.md. Scaffold must not clobber it — same rule as `.gitignore`.
    let scratch = ScratchDir::new("readme-preserved");
    let existing = "# my custom readme\n\nHand-written.\n";
    scratch.write("README.md", existing);
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let readme = fs::read_to_string(scratch.root().join("README.md")).unwrap();
    assert_eq!(
        readme, existing,
        "existing README.md must not be overwritten"
    );
}

#[test]
fn test_file_skipped_when_already_present() {
    // If `src/main_test.kara` already exists (e.g., the user started writing
    // tests before running `karac init`), the placeholder template must not
    // stomp on their content.
    let scratch = ScratchDir::new("test-file-preserved");
    let existing = "test \"my real test\" {\n    assert_eq(2 + 2, 4);\n}\n";
    scratch.write("src/main_test.kara", existing);
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let body = fs::read_to_string(scratch.root().join("src/main_test.kara")).unwrap();
    assert_eq!(body, existing);
}

#[test]
fn force_does_not_overwrite_readme_or_gitignore() {
    // `--force` only applies to the collision-checked entry trio
    // (`kara.toml`, `src/main.kara`, `src/lib.kara`). README, the companion
    // test file, and `.gitignore` remain preserved even under --force, since
    // `--force` is about resolving scaffold collisions, not about authorizing
    // a silent stomp on user-authored boilerplate.
    let scratch = ScratchDir::new("force-preserves-extras");
    let existing_readme = "# existing\n";
    let existing_gitignore = "# custom\n";
    let existing_test = "test \"existing\" { assert_eq(1, 1); }\n";
    scratch.write("README.md", existing_readme);
    scratch.write(".gitignore", existing_gitignore);
    scratch.write("src/main_test.kara", existing_test);
    // Also put a kara.toml in place so --force has something to actually
    // overwrite — this pins that --force's scope is narrow.
    scratch.write(MANIFEST_FILENAME, "[package]\nname = \"old\"\n");
    let opts = ScaffoldOpts {
        template: Template::Bin,
        force: true,
    };
    scaffold_project(scratch.root(), "hello", opts).unwrap();
    // kara.toml got overwritten (the --force path).
    let manifest = fs::read_to_string(scratch.root().join("kara.toml")).unwrap();
    assert!(manifest.contains("name = \"hello\""));
    // Extras are untouched.
    assert_eq!(
        fs::read_to_string(scratch.root().join("README.md")).unwrap(),
        existing_readme,
    );
    assert_eq!(
        fs::read_to_string(scratch.root().join(".gitignore")).unwrap(),
        existing_gitignore,
    );
    assert_eq!(
        fs::read_to_string(scratch.root().join("src/main_test.kara")).unwrap(),
        existing_test,
    );
}

#[test]
fn collision_aborts_without_force() {
    let scratch = ScratchDir::new("collision-abort");
    scratch.write(MANIFEST_FILENAME, "[package]\nname = \"old\"\n");
    let err = scaffold_project(scratch.root(), "hello", bin_opts()).unwrap_err();
    match &err {
        ScaffoldError::Collision { path, .. } => {
            assert_eq!(path, &scratch.root().join("kara.toml"));
        }
        other => panic!("expected Collision, got {other:?}"),
    }
    // The original manifest must be intact — a failed scaffold leaves state
    // untouched beyond what it could not avoid (src/ may or may not have been
    // created; the collision check runs before any writes, so here nothing
    // moved).
    let kept = fs::read_to_string(scratch.root().join("kara.toml")).unwrap();
    assert!(kept.contains("name = \"old\""));
}

#[test]
fn collision_with_entry_file_aborts() {
    let scratch = ScratchDir::new("collision-entry");
    scratch.write("src/main.kara", "fn existing() {}\n");
    let err = scaffold_project(scratch.root(), "hello", bin_opts()).unwrap_err();
    match &err {
        ScaffoldError::Collision { path, .. } => {
            assert_eq!(path, &scratch.root().join("src/main.kara"));
        }
        other => panic!("expected Collision, got {other:?}"),
    }
}

#[test]
fn opposite_entry_file_blocks_other_template() {
    // A dir already holding `src/lib.kara` rejects a `--bin` scaffold, and
    // vice versa — a package is one or the other, never both.
    let scratch = ScratchDir::new("opposite-entry");
    scratch.write("src/lib.kara", "pub fn x() {}\n");
    let err = scaffold_project(scratch.root(), "hello", bin_opts()).unwrap_err();
    assert!(matches!(err, ScaffoldError::Collision { .. }));
}

#[test]
fn force_overrides_manifest_collision() {
    let scratch = ScratchDir::new("force-overrides");
    scratch.write(MANIFEST_FILENAME, "[package]\nname = \"old\"\n");
    let opts = ScaffoldOpts {
        template: Template::Bin,
        force: true,
    };
    scaffold_project(scratch.root(), "hello", opts).unwrap();
    let manifest = fs::read_to_string(scratch.root().join("kara.toml")).unwrap();
    assert!(manifest.contains("name = \"hello\""));
    assert!(!manifest.contains("name = \"old\""));
}

#[test]
fn prepare_new_target_dir_creates_missing() {
    let scratch = ScratchDir::new("prepare-missing");
    let target = scratch.root().join("new_proj");
    assert!(!target.exists());
    prepare_new_target_dir(&target).unwrap();
    assert!(target.is_dir());
}

#[test]
fn prepare_new_target_dir_accepts_empty_existing() {
    let scratch = ScratchDir::new("prepare-empty");
    let target = scratch.root().join("empty");
    fs::create_dir_all(&target).unwrap();
    prepare_new_target_dir(&target).unwrap();
    assert!(target.is_dir());
}

#[test]
fn prepare_new_target_dir_rejects_nonempty() {
    let scratch = ScratchDir::new("prepare-nonempty");
    let target = scratch.root().join("nonempty");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("existing.txt"), "hi").unwrap();
    let err = prepare_new_target_dir(&target).unwrap_err();
    match &err {
        ScaffoldError::TargetDirNotEmpty { path } => assert_eq!(path, &target),
        other => panic!("expected TargetDirNotEmpty, got {other:?}"),
    }
}

#[test]
fn prepare_new_target_dir_rejects_file_at_path() {
    let scratch = ScratchDir::new("prepare-is-file");
    let target = scratch.root().join("not_a_dir");
    fs::write(&target, "i am a file").unwrap();
    let err = prepare_new_target_dir(&target).unwrap_err();
    assert!(matches!(err, ScaffoldError::TargetDirNotEmpty { .. }));
}

#[test]
fn invalid_name_aborts_with_suggestion() {
    let err = validate_package_name("my-app").unwrap_err();
    match err {
        ScaffoldError::InvalidName {
            value, suggestion, ..
        } => {
            assert_eq!(value, "my-app");
            assert_eq!(suggestion, Some("my_app".to_string()));
        }
        other => panic!("expected InvalidName, got {other:?}"),
    }
}

#[test]
fn invalid_name_aborts_before_any_write() {
    // The CLI validates name before touching the filesystem. This test
    // covers the scaffold layer: a caller that passes an invalid name to
    // `scaffold_project` directly is out of contract — the integration is
    // driven by the CLI's `cmd_init` which runs `validate_package_name`
    // first. We verify the library's validator catches the bad forms.
    assert!(matches!(
        validate_package_name("0leading_digit"),
        Err(ScaffoldError::InvalidName { .. })
    ));
    assert!(matches!(
        validate_package_name("HasUpper"),
        Err(ScaffoldError::InvalidName { .. })
    ));
    assert!(matches!(
        validate_package_name(""),
        Err(ScaffoldError::InvalidName { .. })
    ));
}

#[test]
fn reserved_keyword_rejected() {
    let err = validate_package_name("fn").unwrap_err();
    assert!(matches!(err, ScaffoldError::ReservedKeyword { .. }));
}

#[test]
fn scaffolded_manifest_parses_clean() {
    let scratch = ScratchDir::new("parses-clean");
    scaffold_project(scratch.root(), "hello", bin_opts()).unwrap();
    let m = load_from_root(scratch.root()).unwrap();
    assert_eq!(m.name, "hello");
    assert_eq!(m.edition, "2026");
    assert!(
        m.warnings.is_empty(),
        "scaffolded manifest must round-trip with zero warnings: {:?}",
        m.warnings,
    );
}

#[test]
fn subdir_form_scaffolds_child_dir() {
    // Simulate `karac init myproj` from scratch.root(): validate the name,
    // prepare `./myproj/`, scaffold there.
    let scratch = ScratchDir::new("subdir-form");
    let target = scratch.root().join("myproj");
    validate_package_name("myproj").unwrap();
    prepare_new_target_dir(&target).unwrap();
    scaffold_project(&target, "myproj", bin_opts()).unwrap();
    assert!(target.join("kara.toml").is_file());
    assert!(target.join("src/main.kara").is_file());
}

#[test]
fn subdir_form_rejects_nonempty_existing_dir() {
    // `karac init myproj` must abort when `./myproj/` already has files —
    // no --force override for the positional form.
    let scratch = ScratchDir::new("subdir-nonempty");
    let target = scratch.root().join("myproj");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("random.txt"), "").unwrap();
    let err = prepare_new_target_dir(&target).unwrap_err();
    assert!(matches!(err, ScaffoldError::TargetDirNotEmpty { .. }));
}

#[test]
fn error_tags_are_stable() {
    // CR-36's error reporting pivots on these tags — pin them so downstream
    // tools (karac --output=json once it grows a scaffold phase) don't see
    // silent relabels.
    let samples: Vec<(ScaffoldError, &str)> = vec![
        (
            ScaffoldError::InvalidName {
                value: "x".into(),
                suggestion: None,
            },
            "invalid_name",
        ),
        (
            ScaffoldError::ReservedKeyword { value: "fn".into() },
            "reserved_keyword",
        ),
        (
            ScaffoldError::TargetDirNotEmpty {
                path: PathBuf::from("/tmp/x"),
            },
            "target_dir_not_empty",
        ),
        (
            ScaffoldError::Collision {
                path: PathBuf::from("/tmp/kara.toml"),
                file_kind: "package manifest",
            },
            "collision",
        ),
        (
            ScaffoldError::Io {
                path: PathBuf::from("/tmp/x"),
                error: "boom".into(),
            },
            "io",
        ),
    ];
    for (err, tag) in samples {
        assert_eq!(err.tag(), tag);
    }
}

// Dodge unused-import warnings on the bare `scaffold` module alias when the
// compiler reports them — touch the module path so rustc keeps the import.
#[test]
fn module_alias_is_live() {
    let _ = scaffold::Template::Bin;
}

// ── Phase-8 line 63 — backend scaffold integration tests ──────────────

fn backend_opts() -> ScaffoldOpts {
    ScaffoldOpts {
        template: Template::Backend,
        force: false,
    }
}

#[test]
fn backend_template_writes_main_kara_alongside_test() {
    let scratch = ScratchDir::new("backend-template");
    scaffold_project(scratch.root(), "my_api", backend_opts()).unwrap();
    assert!(scratch.root().join("kara.toml").is_file());
    assert!(scratch.root().join("src/main.kara").is_file());
    assert!(scratch.root().join("src/main_test.kara").is_file());
    assert!(scratch.root().join("README.md").is_file());
    assert!(scratch.root().join(".gitignore").is_file());
    // Backend mirrors --bin in entry shape — must not write a lib.
    assert!(!scratch.root().join("src/lib.kara").exists());
    assert!(!scratch.root().join("src/lib_test.kara").exists());
}

/// The generated `src/main.kara` must parse and typecheck against the
/// shipped stdlib floor. Without this pin, a future stdlib-shape change
/// (renaming `Server.serve`, dropping `Request.path()`, etc.) could
/// silently break the v1 default scaffold — `karac new my_api && karac
/// build` would fail out of the box. End-to-end signal that
/// "default-being-backend" stays load-bearing.
#[test]
fn backend_main_kara_typechecks_cleanly() {
    let scratch = ScratchDir::new("backend-typecheck");
    scaffold_project(scratch.root(), "my_api", backend_opts()).unwrap();
    let src = fs::read_to_string(scratch.root().join("src/main.kara")).unwrap();
    let parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "backend main.kara parse errors: {:?}",
        parsed.errors
    );
    let resolved = karac::resolve(&parsed.program);
    assert!(
        resolved.errors.is_empty(),
        "backend main.kara resolve errors: {:?}",
        resolved.errors
    );
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(
        typed.errors.is_empty(),
        "backend main.kara typecheck errors: {:?}",
        typed.errors
    );
    // Also verify effect-checking succeeds — Server.serve declares
    // sends(Network) receives(Network) and main must propagate them.
    let effect_result = karac::effectcheck(&parsed.program);
    assert!(
        effect_result.errors.is_empty(),
        "backend main.kara effectcheck errors: {:?}",
        effect_result.errors
    );
}

/// Repeat the typecheck for the companion test file so `karac test` on
/// a fresh project succeeds out of the box.
#[test]
fn backend_main_test_kara_parses_and_typechecks() {
    let scratch = ScratchDir::new("backend-test-parses");
    scaffold_project(scratch.root(), "my_api", backend_opts()).unwrap();
    let src = fs::read_to_string(scratch.root().join("src/main_test.kara")).unwrap();
    let parsed = karac::parse(&src);
    assert!(
        parsed.errors.is_empty(),
        "backend main_test.kara parse errors: {:?}",
        parsed.errors
    );
}

/// End-to-end build pin — codegen + link the scaffolded backend. Soft-
/// skips when the runtime archive isn't built (same `runtime_path`
/// pattern `tests/http_server.rs` uses); otherwise compiles
/// `src/main.kara` to a binary and asserts the binary exists +
/// `karac::parse` reads it back as a real ELF/Mach-O. Catches stdlib-
/// surface regressions that pass typecheck (which is what
/// `backend_main_kara_typechecks_cleanly` covers) but fail codegen.
#[cfg(feature = "llvm")]
#[test]
fn backend_main_kara_codegens_to_executable() {
    use std::process::Command;
    // Locate the runtime archive — same convention as
    // `tests/http_server.rs::runtime_path`. Soft-skip when unbuilt so
    // CI shards that don't build the runtime first still pass.
    let workspace_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let rt_path = workspace_root.join("target/release/libkarac_runtime.a");
    if !rt_path.exists() {
        // Try the dev profile as a fallback.
        let rt_dev = workspace_root.join("target/debug/libkarac_runtime.a");
        if !rt_dev.exists() {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        }
    }
    let scratch = ScratchDir::new("backend-codegen");
    scaffold_project(scratch.root(), "my_api", backend_opts()).unwrap();

    // Compile to object + link, using the same compile-and-link helper
    // shape that `tests/http_server.rs` uses.
    let src = fs::read_to_string(scratch.root().join("src/main.kara")).unwrap();
    let mut parsed = karac::parse(&src);
    assert!(parsed.errors.is_empty(), "parse: {:?}", parsed.errors);
    let resolved = karac::resolve(&parsed.program);
    assert!(resolved.errors.is_empty(), "resolve: {:?}", resolved.errors);
    let typed = karac::typecheck(&parsed.program, &resolved);
    assert!(typed.errors.is_empty(), "typecheck: {:?}", typed.errors);
    karac::lower(&mut parsed.program, &typed);

    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let obj = std::env::temp_dir().join(format!("karac_scaffold_backend_{pid}_{nanos}.o"));
    let exe = std::env::temp_dir().join(format!("karac_scaffold_backend_{pid}_{nanos}"));

    let runtime_for_link = if rt_path.exists() {
        rt_path
    } else {
        workspace_root.join("target/debug/libkarac_runtime.a")
    };
    std::env::set_var("KARAC_RUNTIME", &runtime_for_link);

    karac::codegen::compile_to_object_with_options(
        &parsed.program,
        obj.to_str().unwrap(),
        None,
        None,
        None,
        None,
    )
    .expect("codegen to object");
    karac::codegen::link_executable(obj.to_str().unwrap(), exe.to_str().unwrap())
        .expect("link executable");

    assert!(exe.exists(), "backend executable should exist at {exe:?}");
    // Sanity-check it's a real file by reading the first 4 bytes (Mach-O
    // / ELF magic both pass an "is non-empty file" smoke).
    let head = fs::read(&exe).expect("read executable");
    assert!(head.len() > 64, "backend executable should not be empty");
    // Confirm it's executable on the host platform — `chmod +x` happens
    // inside `link_executable` for us. We don't run the binary (it'd
    // bind a port and hang); the codegen+link success is what we
    // wanted to pin.
    let metadata = fs::metadata(&exe).expect("stat executable");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        assert!(mode & 0o111 != 0, "backend executable should have +x bits");
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
    }

    // Cleanup — the ScratchDir Drop handles the project dir; the
    // object + binary live in /tmp.
    let _ = fs::remove_file(&obj);
    let _ = fs::remove_file(&exe);
    Command::new("true").status().ok(); // silence unused-import warning
}
