//! Every `karac new` template is already in `karac fmt`'s canonical form,
//! so a fresh project passes `karac fmt --check` (B-2026-10-30-30 gave fmt
//! its package and check modes; the backend template then failed it).

use karac::scaffold::{scaffold_project, ScaffoldOpts, Template};
use std::process::Command;

#[test]
fn every_new_project_template_passes_fmt_check() {
    for (name, template) in [
        ("bin", Template::Bin),
        ("cli", Template::Cli),
        ("lib", Template::Lib),
        ("backend", Template::Backend),
    ] {
        let dir =
            std::env::temp_dir().join(format!("karac-scaffold-fmt-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        scaffold_project(
            &dir,
            "demo",
            ScaffoldOpts {
                template,
                force: false,
            },
        )
        .unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_karac"))
            .args(["fmt", "--check"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "the {name} template is not fmt-clean; would change:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
