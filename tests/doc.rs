//! Integration tests for `karac doc` — the doc-comment renderer.
//!
//! Two layers:
//! - Unit-style: drive `karac::doc::build_docs` directly against a
//!   minimal in-memory project tree.
//! - End-to-end: run the `karac doc` binary in a scratch project root
//!   and assert against the files it leaves under `dist/doc/`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static TEST_ID: AtomicU32 = AtomicU32::new(0);

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "karac-doc-test-{}-{}-{}",
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

fn karac_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_karac"))
}

// ── End-to-end: karac doc in a scratch project ────────────────────────────

#[test]
fn doc_emits_html_for_documented_function() {
    let scratch = ScratchDir::new("fn-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"docproj\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Doubles its argument.\n\
         /// Cheap.\n\
         pub fn double(n: i64) -> i64 { n * 2 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "karac doc failed: stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("rendered"),
        "summary line missing: {stdout}"
    );

    let doc_root = scratch.root().join("dist").join("doc");
    let item_page = doc_root.join("double.html");
    assert!(item_page.exists(), "expected {item_page:?} to exist");
    let item_html = fs::read_to_string(&item_page).unwrap();
    assert!(item_html.contains("Doubles its argument."));
    assert!(item_html.contains("Cheap."));
    assert!(item_html.contains("fn double(n: i64)"));

    // The synthetic main isn't documented, so no main.html is emitted.
    assert!(!doc_root.join("main.html").exists());

    // Index lists the documented item.
    let index_html = fs::read_to_string(doc_root.join("index.html")).unwrap();
    assert!(index_html.contains("double"));
    assert!(index_html.contains("(crate root)"));
}

#[test]
fn doc_skips_items_without_doc_comment() {
    let scratch = ScratchDir::new("no-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"silent\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "pub fn undocumented(n: i64) -> i64 { n }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let doc_root = scratch.root().join("dist").join("doc");
    assert!(!doc_root.join("undocumented.html").exists());
    // The index still gets emitted even when empty — useful for a stable
    // entry-point URL on a deployed doc site.
    assert!(doc_root.join("index.html").exists());
}

#[test]
fn doc_renders_struct_and_enum() {
    let scratch = ScratchDir::new("struct-enum");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"shapes\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A 2D point.\n\
         pub struct Point { x: i64, y: i64 }\n\
         /// A direction.\n\
         pub enum Direction { Up, Down }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let doc_root = scratch.root().join("dist").join("doc");
    let point_html = fs::read_to_string(doc_root.join("Point.html")).unwrap();
    assert!(point_html.contains("A 2D point."));
    assert!(point_html.contains("struct Point"));

    let dir_html = fs::read_to_string(doc_root.join("Direction.html")).unwrap();
    assert!(dir_html.contains("A direction."));
    assert!(dir_html.contains("enum Direction"));
}

#[test]
fn doc_emits_per_module_subdirectory() {
    let scratch = ScratchDir::new("per-module");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"layered\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\npub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let doc_root = scratch.root().join("dist").join("doc");
    // Items in module `util` land under `dist/doc/util/`.
    let add_page = doc_root.join("util").join("add.html");
    assert!(add_page.exists(), "expected {add_page:?} to exist");
    let html = fs::read_to_string(&add_page).unwrap();
    assert!(html.contains("Adds two numbers."));
    assert!(html.contains("fn add(a: i64, b: i64)"));

    let index = fs::read_to_string(doc_root.join("index.html")).unwrap();
    assert!(index.contains("util"));
}

#[test]
fn doc_renders_markdown_in_doc_comment() {
    let scratch = ScratchDir::new("markdown");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"md\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// **Bold** and `code` inside the doc.\n\
         pub fn marked() -> i64 { 1 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/marked.html")).unwrap();
    assert!(html.contains("<strong>Bold</strong>"));
    assert!(html.contains("<code>code</code>"));
}

#[test]
fn doc_resolves_cross_reference_to_known_item() {
    // `[Holder]` in one item's prose should rewrite to a link pointing
    // at `Holder.html` because the project defines a `pub struct Holder`.
    let scratch = ScratchDir::new("xref-known");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"xref\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Holds a number.\n\
         pub struct Holder { n: i64 }\n\
         /// Reads from a [Holder].\n\
         pub fn use_holder(h: Holder) -> i64 { h.n }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/use_holder.html")).unwrap();
    assert!(
        html.contains("href=\"Holder.html\""),
        "expected resolved link to Holder.html; got:\n{html}"
    );
    assert!(html.contains(">Holder</a>"));
}

#[test]
fn doc_unresolved_cross_reference_renders_as_plain_text() {
    // `[Missing]` (no such item exists in the project) should render
    // verbatim with the brackets — not as an `<a>` element.
    let scratch = ScratchDir::new("xref-missing");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"missing\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Talks about a [Missing] type that nobody defined.\n\
         pub fn lonely() -> i64 { 0 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/lonely.html")).unwrap();
    assert!(
        !html.contains("href=\"Missing"),
        "should NOT have produced a link to Missing; got:\n{html}"
    );
    assert!(html.contains("[Missing]"));
}

#[test]
fn doc_renders_struct_fields_section() {
    // The struct page should include a "Fields" section listing each
    // field by name and rendered type.
    let scratch = ScratchDir::new("struct-fields");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"shapes2\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A 2D point with named coordinates.\n\
         pub struct Point { x: i64, y: i64 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Point.html")).unwrap();
    assert!(html.contains("<h2>Fields</h2>"), "got:\n{html}");
    assert!(html.contains("<dt>x</dt>"));
    assert!(html.contains("<dt>y</dt>"));
    // Type renders as the path segment.
    assert!(html.matches("i64").count() >= 2);
}

#[test]
fn doc_renders_enum_variants_section() {
    // The enum page should include a "Variants" section listing every
    // variant — covering unit, tuple, and struct payload shapes.
    let scratch = ScratchDir::new("enum-variants");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"shapes3\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A grab-bag enum exercising every payload shape.\n\
         pub enum Shape {\n\
             Empty,\n\
             Pair(i64, i64),\n\
             Named { kind: i64 },\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Shape.html")).unwrap();
    assert!(html.contains("<h2>Variants</h2>"), "got:\n{html}");
    assert!(html.contains("Empty"));
    assert!(
        html.contains("Pair(i64, i64)"),
        "expected tuple variant rendering; got:\n{html}"
    );
    assert!(
        html.contains("Named { kind: i64 }"),
        "expected struct variant rendering; got:\n{html}"
    );
}

#[test]
fn doc_renders_per_field_doc_comments() {
    // `///` directly above a struct field should appear on the rendered
    // page next to that field. Tests both top-level fields and the
    // markdown rendering pass (so cross-references still work inside a
    // per-field doc).
    let scratch = ScratchDir::new("field-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"perfield\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A 2D point.\n\
         pub struct Point {\n\
           /// Horizontal coordinate.\n\
           x: i64,\n\
           /// Vertical coordinate, **measured from the top**.\n\
           y: i64,\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Point.html")).unwrap();
    // The MVP signatures are still there.
    assert!(html.contains("<dt>x</dt>"));
    assert!(html.contains("<dt>y</dt>"));
    // Per-field prose is rendered.
    assert!(
        html.contains("Horizontal coordinate."),
        "expected x's doc comment; got:\n{html}"
    );
    assert!(
        html.contains("Vertical coordinate,"),
        "expected y's doc comment; got:\n{html}"
    );
    // Markdown formatting inside the per-field comment renders too.
    assert!(
        html.contains("<strong>measured from the top</strong>"),
        "expected markdown emphasis to render in field doc; got:\n{html}"
    );
}

#[test]
fn doc_renders_per_variant_doc_comments() {
    // `///` directly above an enum variant should appear on the rendered
    // page next to that variant.
    let scratch = ScratchDir::new("variant-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"pervariant\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A traffic-light state.\n\
         pub enum Light {\n\
           /// Stop.\n\
           Red,\n\
           /// Slow down — `transitioning`.\n\
           Yellow,\n\
           /// Go.\n\
           Green,\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Light.html")).unwrap();
    // Per-variant prose is rendered.
    assert!(html.contains("Stop."), "expected Red's doc; got:\n{html}");
    assert!(
        html.contains("Slow down"),
        "expected Yellow's doc; got:\n{html}"
    );
    assert!(html.contains("Go."), "expected Green's doc; got:\n{html}");
    // Markdown inline code inside the per-variant comment renders too.
    assert!(
        html.contains("<code>transitioning</code>"),
        "expected markdown code span in variant doc; got:\n{html}"
    );
}

#[test]
fn doc_renders_per_param_doc_comments() {
    // `///` directly above a function parameter should render in a
    // dedicated "Parameters" section on the function's doc page.
    let scratch = ScratchDir::new("param-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"perparam\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Sums three numbers.\n\
         pub fn sum3(\n\
           /// The base term, **required**.\n\
           a: i64,\n\
           b: i64,\n\
           /// The third term — `optional` in callers.\n\
           c: i64,\n\
         ) -> i64 { a + b + c }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/sum3.html")).unwrap();
    // Parameters section is present with the documented params.
    assert!(
        html.contains("<h2>Parameters</h2>"),
        "expected Parameters section; got:\n{html}"
    );
    assert!(
        html.contains("<dl class=\"params\">"),
        "expected params dl; got:\n{html}"
    );
    // Each documented param appears with its name and type in the dt.
    assert!(html.contains("<dt>a: i64</dt>"));
    assert!(html.contains("<dt>b: i64</dt>"));
    assert!(html.contains("<dt>c: i64</dt>"));
    // Per-param prose is rendered, including markdown formatting.
    assert!(
        html.contains("The base term,"),
        "expected a's doc; got:\n{html}"
    );
    assert!(
        html.contains("<strong>required</strong>"),
        "expected markdown emphasis in param doc; got:\n{html}"
    );
    assert!(
        html.contains("<code>optional</code>"),
        "expected markdown code span in param doc; got:\n{html}"
    );
    // Undocumented param `b` still appears (full param list renders when
    // any param has a doc), with an empty <dd>.
    assert!(html.contains("<dt>b: i64</dt>\n<dd></dd>"));
}

#[test]
fn doc_renders_module_level_doc_comments() {
    // `//!` at the top of the crate root and a sub-module render as
    // module-level prose on the index page. Crate-root doc renders
    // above the items table; sub-module doc renders inside a row that
    // precedes that module's item rows.
    let scratch = ScratchDir::new("module-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"moddocs\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "//! Crate-level **summary**.\n\
         //! Continues onto a second line.\n\
         /// A documented item.\n\
         pub fn root_item() -> i64 { 1 }\n\
         fn main() {}\n",
    );
    scratch.write(
        "src/util.kara",
        "//! Sub-module covering helper utilities.\n\
         /// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let index = fs::read_to_string(scratch.root().join("dist/doc/index.html")).unwrap();
    // Crate-root `//!` renders above the table.
    let pre_table = index.split("<table>").next().unwrap_or("");
    assert!(
        pre_table.contains("class=\"module-doc crate-root-doc\""),
        "expected crate-root-doc block above the table; got:\n{index}"
    );
    assert!(
        pre_table.contains("Crate-level"),
        "expected crate-root prose above the table; got:\n{index}"
    );
    assert!(
        pre_table.contains("<strong>summary</strong>"),
        "expected markdown emphasis to render in module doc; got:\n{index}"
    );
    // Sub-module `//!` renders inside the table preceding the module's items.
    assert!(
        index.contains("Sub-module covering helper utilities."),
        "expected sub-module prose; got:\n{index}"
    );
    assert!(
        index.contains("<td colspan=\"3\"><div class=\"module-doc\">"),
        "expected module-doc row preceding sub-module items; got:\n{index}"
    );
}

#[test]
fn doc_renders_struct_variant_per_field_when_field_has_doc() {
    // Enum struct-variant with `///` on at least one field renders a
    // nested per-field `<dl>` inside the `Variant { ... }` shell.
    let scratch = ScratchDir::new("struct-variant-field-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"svfd\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A request envelope.\n\
         pub enum Msg {\n\
           /// A read of `key`.\n\
           Read {\n\
             /// The key to look up.\n\
             key: String,\n\
             /// **Maximum** entries to return.\n\
             limit: i64,\n\
           },\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Msg.html")).unwrap();
    // Variant shell preserved.
    assert!(
        html.contains("Read {"),
        "expected Read {{ ... }} shell; got:\n{html}"
    );
    // Nested per-field dl is emitted.
    assert!(
        html.contains("<dl class=\"variant-fields\">"),
        "expected variant-fields dl; got:\n{html}"
    );
    assert!(html.contains("<dt>key: String</dt>"));
    assert!(html.contains("<dt>limit: i64</dt>"));
    // Per-field prose with markdown rendering.
    assert!(
        html.contains("The key to look up."),
        "expected key's doc; got:\n{html}"
    );
    assert!(
        html.contains("<strong>Maximum</strong>"),
        "expected markdown emphasis in field doc; got:\n{html}"
    );
    // Variant-level doc still renders.
    assert!(
        html.contains("A read of"),
        "expected variant doc; got:\n{html}"
    );
}

#[test]
fn doc_keeps_inline_struct_variant_when_no_field_has_doc() {
    // Regression guard: when no field carries `///`, the struct variant
    // renders inline as `Variant { f: T, g: U }` — no empty <dl>.
    let scratch = ScratchDir::new("struct-variant-no-field-docs");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"svnfd\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A 2D point variant.\n\
         pub enum Shape {\n\
           /// A point.\n\
           Point { x: i64, y: i64 },\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/Shape.html")).unwrap();
    // Inline shape preserved.
    assert!(
        html.contains("Point { x: i64, y: i64 }"),
        "expected inline variant signature; got:\n{html}"
    );
    // No nested per-field dl when no field has a doc.
    assert!(
        !html.contains("variant-fields"),
        "expected no variant-fields dl; got:\n{html}"
    );
}

#[test]
fn doc_omits_params_section_when_no_param_has_doc() {
    // Regression guard: a documented function whose params carry no
    // `///` doc comments must not emit an empty Parameters section.
    let scratch = ScratchDir::new("param-docs-empty");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"perparamnone\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Doubles its argument.\n\
         pub fn double(n: i64) -> i64 { n * 2 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/double.html")).unwrap();
    assert!(
        !html.contains("<h2>Parameters</h2>"),
        "expected no Parameters section when no param has a doc; got:\n{html}"
    );
    assert!(
        !html.contains("class=\"params\""),
        "expected no params dl when no param has a doc; got:\n{html}"
    );
}

#[test]
fn doc_emits_search_index_for_documented_items() {
    // Multi-item project across two modules produces a JSON array
    // with one entry per documented item: name/kind/href/summary.
    let scratch = ScratchDir::new("search-index");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"searchable\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Doubles its argument.\n\
         /// More detail on a second line.\n\
         pub fn double(n: i64) -> i64 { n * 2 }\n\
         /// A point.\n\
         pub struct Point { x: i64, y: i64 }\n\
         fn main() {}\n",
    );
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\npub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let json_path = scratch.root().join("dist/doc/search-index.json");
    assert!(
        json_path.exists(),
        "expected {json_path:?} to exist alongside index.html"
    );
    let json = fs::read_to_string(&json_path).unwrap();

    // Top-level shape: a JSON array.
    assert!(json.starts_with('['), "expected JSON array; got: {json}");
    assert!(json.ends_with(']'));

    // Every item appears with the right shape. Spot-check by substring
    // — a real JSON parser would be over-engineering for the schema
    // pin (the schema is tiny and stable).
    assert!(
        json.contains("\"name\":\"double\""),
        "expected double entry; got: {json}"
    );
    assert!(json.contains("\"kind\":\"fn\""));
    assert!(json.contains("\"href\":\"double.html\""));
    assert!(
        json.contains("\"summary\":\"Doubles its argument.\""),
        "expected first-line summary; got: {json}"
    );

    assert!(
        json.contains("\"name\":\"Point\""),
        "expected Point entry; got: {json}"
    );
    assert!(json.contains("\"kind\":\"struct\""));

    assert!(
        json.contains("\"name\":\"add\""),
        "expected add entry; got: {json}"
    );
    // Cross-module item lands under util/.
    assert!(json.contains("\"href\":\"util/add.html\""));

    // CLI summary is unaffected by the JSON file (it only counts HTML pages).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("rendered"));
}

#[test]
fn doc_emits_empty_search_index_when_no_items_documented() {
    // Project with no doc comments still produces a stable, empty JSON
    // array — the file always exists at the published URL.
    let scratch = ScratchDir::new("empty-search");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"undocumented\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "pub fn undocumented() -> i64 { 0 }\nfn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let json_path = scratch.root().join("dist/doc/search-index.json");
    assert!(json_path.exists());
    let json = fs::read_to_string(&json_path).unwrap();
    assert_eq!(json, "[]");
}

#[test]
fn doc_signature_renders_no_with_clause_for_pure_pub_fn() {
    // A `pub fn` with no effect annotation and a pure body should
    // render its signature without any `with` clause.
    let scratch = ScratchDir::new("pure-fn");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"pure\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Doubles its argument.\n\
         pub fn double(n: i64) -> i64 { n * 2 }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/double.html")).unwrap();
    assert!(html.contains("fn double(n: i64)"));
    assert!(
        !html.contains(" with "),
        "pure fn should not render a with-clause; got:\n{html}"
    );
}

#[test]
fn doc_signature_renders_declared_effects_on_pub_fn() {
    // A `pub fn` with a declared effect annotation should render the
    // corresponding `with <effects>` clause in its signature.
    let scratch = ScratchDir::new("declared-effects");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"effectful\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "effect resource UserDB;\n\
         /// Saves a record.\n\
         pub fn save(n: i64) writes(UserDB) { }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/save.html")).unwrap();
    // The `with writes(UserDB)` clause is rendered alongside the signature.
    // HTML escapes `(` and `)` as themselves, so substring matches work.
    assert!(
        html.contains("with writes(UserDB)"),
        "expected declared-effect clause; got:\n{html}"
    );
}

#[test]
fn doc_signature_renders_polymorphic_with_underscore() {
    // A `pub fn` annotated `with _` (effect-polymorphic) should render
    // ` with _` in its signature.
    let scratch = ScratchDir::new("polymorphic-effects");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"polymorphic\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// Applies an arbitrary effectful closure.\n\
         pub fn apply(f: Fn() -> () with _) -> () with _ { f() }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/apply.html")).unwrap();
    assert!(
        html.contains("with _"),
        "expected polymorphic with-clause; got:\n{html}"
    );
}

#[test]
fn doc_writes_per_module_index_for_nested_module() {
    // A non-crate-root module with documented items gets its own
    // `<module>/index.html` listing only that module's items.
    let scratch = ScratchDir::new("per-module-index");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"pmi\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n\
         /// Subtracts.\n\
         pub fn sub(a: i64, b: i64) -> i64 { a - b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let module_index = scratch.root().join("dist/doc/util/index.html");
    assert!(
        module_index.exists(),
        "expected per-module index at {module_index:?}"
    );
    let html = fs::read_to_string(&module_index).unwrap();
    assert!(html.contains("module <code>util</code>"));
    assert!(html.contains("add.html"));
    assert!(html.contains("sub.html"));
    // Back-link to crate root.
    assert!(html.contains("href=\"../index.html\""));
}

#[test]
fn doc_per_module_index_includes_module_doc() {
    // A `//!` on a sub-module renders inside its per-module index.
    let scratch = ScratchDir::new("per-module-index-doc");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"pmid\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    scratch.write(
        "src/util.kara",
        "//! Helper utilities.\n\
         /// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/util/index.html")).unwrap();
    assert!(
        html.contains("Helper utilities."),
        "expected module doc on per-module index; got:\n{html}"
    );
    assert!(html.contains("class=\"module-doc\""));
}

#[test]
fn doc_per_module_index_omitted_for_module_without_docs_or_items() {
    // A sub-module with neither documented items nor a `//!` block
    // gets no per-module index.
    let scratch = ScratchDir::new("per-module-index-empty");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"pmie\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    // Only an undocumented item — module index should not be emitted.
    scratch.write(
        "src/util.kara",
        "pub fn undocumented(n: i64) -> i64 { n }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let module_index = scratch.root().join("dist/doc/util/index.html");
    assert!(
        !module_index.exists(),
        "did not expect per-module index at {module_index:?}"
    );
}

#[test]
fn doc_item_page_nav_links_to_module_index() {
    // The "back to module index" nav on an item page is a relative link
    // to `index.html` (resolves to the module's own index for nested
    // modules, or the crate-root index for items at the root).
    let scratch = ScratchDir::new("item-nav");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"itemnav\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let item_html = fs::read_to_string(scratch.root().join("dist/doc/util/add.html")).unwrap();
    assert!(
        item_html.contains("href=\"index.html\""),
        "expected nav to link to ./index.html; got:\n{item_html}"
    );
}

#[test]
fn doc_sidebar_appears_on_every_page() {
    // Item page, per-module index, and global crate-root index all
    // render the same global sidebar (`<aside class="sidebar">`).
    let scratch = ScratchDir::new("sidebar-everywhere");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"sb\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A documented item.\n\
         pub fn root_fn() -> i64 { 1 }\n\
         fn main() {}\n",
    );
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc_root = scratch.root().join("dist/doc");
    for path in [
        doc_root.join("index.html"),
        doc_root.join("util/index.html"),
        doc_root.join("util/add.html"),
        doc_root.join("root_fn.html"),
    ] {
        let html = fs::read_to_string(&path).unwrap();
        assert!(
            html.contains("<aside class=\"sidebar\">"),
            "expected sidebar on {path:?}; got:\n{html}"
        );
    }
}

#[test]
fn doc_sidebar_lists_modules_and_items() {
    // Every documented item shows up in the sidebar; the module
    // appears as a `<details>` with its items as `<li>` rows.
    let scratch = ScratchDir::new("sidebar-content");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"sbc\"\nedition = \"2026\"\n",
    );
    scratch.write("src/main.kara", "fn main() {}\n");
    scratch.write(
        "src/util.kara",
        "/// Adds.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n\
         /// Subtracts.\n\
         pub fn sub(a: i64, b: i64) -> i64 { a - b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/index.html")).unwrap();
    let aside_start = html.find("<aside class=\"sidebar\">").unwrap();
    let aside_end = html[aside_start..].find("</aside>").unwrap() + aside_start;
    let sidebar = &html[aside_start..aside_end];
    assert!(
        sidebar.contains("<details"),
        "expected <details> for module"
    );
    assert!(sidebar.contains(">util<"), "expected module title 'util'");
    assert!(sidebar.contains(">add<"), "expected item 'add'");
    assert!(sidebar.contains(">sub<"), "expected item 'sub'");
}

#[test]
fn doc_sidebar_paths_resolve_from_deep_page() {
    // From a page at `<doc>/util/add.html`, sidebar links to a
    // crate-root item must walk up via `../Foo.html`.
    let scratch = ScratchDir::new("sidebar-deep");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"sbd\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A struct at the crate root.\n\
         pub struct Foo { n: i64 }\n\
         fn main() {}\n",
    );
    scratch.write(
        "src/util.kara",
        "/// Adds two numbers.\n\
         pub fn add(a: i64, b: i64) -> i64 { a + b }\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/util/add.html")).unwrap();
    let aside_start = html.find("<aside class=\"sidebar\">").unwrap();
    let aside_end = html[aside_start..].find("</aside>").unwrap() + aside_start;
    let sidebar = &html[aside_start..aside_end];
    // The crate-root item is reached via `../Foo.html`; the crate root
    // index via `../index.html`.
    assert!(
        sidebar.contains("href=\"../Foo.html\""),
        "expected `../Foo.html` from deep page; got:\n{sidebar}"
    );
    assert!(
        sidebar.contains("href=\"../index.html\""),
        "expected `../index.html` from deep page; got:\n{sidebar}"
    );
}

#[test]
fn doc_sidebar_omits_undocumented_module() {
    // A module whose only items are undocumented (and no `//!`) is
    // not represented in the sidebar — keeps the nav scoped to what's
    // actually browsable.
    let scratch = ScratchDir::new("sidebar-omit-empty");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"sbo\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// A documented item.\n\
         pub fn doc_root() -> i64 { 1 }\n\
         fn main() {}\n",
    );
    // util has nothing documented and no //! block.
    scratch.write("src/util.kara", "pub fn undocumented() -> i64 { 1 }\n");

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/index.html")).unwrap();
    let aside_start = html.find("<aside class=\"sidebar\">").unwrap();
    let aside_end = html[aside_start..].find("</aside>").unwrap() + aside_start;
    let sidebar = &html[aside_start..aside_end];
    assert!(
        !sidebar.contains(">util<"),
        "module 'util' should not appear in sidebar; got:\n{sidebar}"
    );
}

#[test]
fn doc_signature_propagates_inferred_effects_across_modules() {
    // A `pub fn` with no `with` clause that calls a `pub fn` in
    // another module via `import` should have the callee's effects
    // surface in the rendered signature. Pre-fix, `karac doc` ran
    // effectcheck per-module: the call into another module was
    // effect-empty in the local function table, so the inferred set
    // for the caller was empty on the doc page. The fix runs
    // effectcheck on a merged Program containing every module's items
    // so the bare-name lookup resolves cross-module callees.
    let scratch = ScratchDir::new("xmod-effects");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"xmod\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "import util.helper;\n\
         import util.UserDB;\n\
         /// Forwards to the writing helper in another module.\n\
         pub fn process() { helper() }\n\
         fn main() {}\n",
    );
    scratch.write(
        "src/util.kara",
        "effect resource UserDB;\n\
         /// Writes to the user database.\n\
         pub fn helper() writes(UserDB) {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let html = fs::read_to_string(scratch.root().join("dist/doc/process.html")).unwrap();
    assert!(
        html.contains("with writes(UserDB)"),
        "expected `with writes(UserDB)` propagated from cross-module callee; got:\n{html}"
    );
}

#[test]
fn doc_outside_project_emits_manifest_error() {
    let scratch = ScratchDir::new("no-manifest");
    // No kara.toml — `karac doc` should fail with the manifest error.

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("kara.toml") || stderr.contains("project"),
        "expected a manifest-related diagnostic; got: {stderr}"
    );
}

// ── Extern block child surfacing ─────────────────────────────────────────
// `unsafe extern "ABI" { ... }` blocks expand into per-child documentables
// so foreign-import declarations surface their `///` prose exactly like the
// pre-block standalone form did. Block-level `///` prose (the `# Safety`
// carrier the `undocumented_unsafe` lint reads) is inlined at the top of
// each child's rendered page — same carrier shared by lint and renderer,
// no separate block-level page (slice 5b, FFI hardening epic).

#[test]
fn doc_emits_html_for_extern_block_child_function() {
    let scratch = ScratchDir::new("extern-block-child");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"ffi\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "effect resource FileSystem;\n\
         unsafe extern \"C\" {\n\
             /// Closes the file descriptor.\n\
             pub fn close(fd: i32) -> i32 writes(FileSystem);\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "karac doc failed: stderr={stderr}");

    let doc_root = scratch.root().join("dist").join("doc");
    let item_page = doc_root.join("close.html");
    assert!(
        item_page.exists(),
        "expected per-child page at {item_page:?}"
    );
    let html = fs::read_to_string(&item_page).unwrap();
    assert!(html.contains("Closes the file descriptor."));
    assert!(html.contains("extern &quot;C&quot; fn close"));
    // The kind tag uses the existing ExternFn label.
    assert!(html.contains("extern fn"));

    // Index lists the extern child like any other documented item.
    let index_html = fs::read_to_string(doc_root.join("index.html")).unwrap();
    assert!(index_html.contains("close"));
}

#[test]
fn doc_emits_per_child_pages_for_multi_item_extern_block() {
    let scratch = ScratchDir::new("extern-block-multi");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"ffi_multi\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "unsafe extern \"C\" {\n\
             /// Returns the current pid.\n\
             pub fn getpid() -> i32;\n\
             pub fn undocumented_helper() -> i32;\n\
             /// Returns the parent pid.\n\
             pub fn getppid() -> i32;\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let doc_root = scratch.root().join("dist").join("doc");
    assert!(doc_root.join("getpid.html").exists());
    assert!(doc_root.join("getppid.html").exists());
    // Undocumented sibling is skipped — same rule as top-level items.
    assert!(!doc_root.join("undocumented_helper.html").exists());

    let getpid_html = fs::read_to_string(doc_root.join("getpid.html")).unwrap();
    assert!(getpid_html.contains("Returns the current pid."));
}

#[test]
fn doc_resolves_cross_reference_to_extern_block_child() {
    // A `[close]` reference in another item's prose should rewrite to a
    // link pointing at `close.html` because the project's `unsafe extern`
    // block defines `close` and the link table walks the block's children.
    let scratch = ScratchDir::new("extern-xref");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"ffi_xref\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "unsafe extern \"C\" {\n\
             /// Closes the descriptor.\n\
             pub fn close(fd: i32) -> i32;\n\
         }\n\
         /// Calls into [close] under the hood.\n\
         pub fn shutdown(fd: i32) -> i32 { close(fd) }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/shutdown.html")).unwrap();
    assert!(
        html.contains("href=\"close.html\""),
        "expected resolved link to close.html; got:\n{html}"
    );
    assert!(html.contains(">close</a>"));
}

#[test]
fn doc_inlines_block_level_doc_on_each_child_page() {
    // Block-level `///` prose (carrying `# Safety`) shows up on every
    // documented child page, prepended to the child's own prose. This
    // is slice 5b of the unsafe-extern hardening epic: same carrier the
    // `undocumented_unsafe` lint reads (`ExternBlock.doc_comment`), now
    // surfaced through the renderer too.
    let scratch = ScratchDir::new("extern-block-doc-inline");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"ffi_doc\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// libc string utilities.\n\
         ///\n\
         /// # Safety\n\
         ///\n\
         /// Callers must pass valid NUL-terminated pointers.\n\
         unsafe extern \"C\" {\n\
             /// Returns string length.\n\
             pub fn strlen(s: i64) -> i64;\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let html = fs::read_to_string(scratch.root().join("dist/doc/strlen.html")).unwrap();
    assert!(
        html.contains("Callers must pass valid NUL-terminated pointers."),
        "expected block-level Safety prose inlined on strlen page; got:\n{html}"
    );
    assert!(
        html.contains("Returns string length."),
        "expected child's own prose still rendered; got:\n{html}"
    );
    // Block-level `# Safety` header survives rendering (the `#` becomes
    // an `<h1>` or `<h2>` depending on the markdown renderer). Just look
    // for the Safety text adjacent to a heading tag.
    assert!(
        html.contains("Safety"),
        "expected `Safety` heading from block-level prose; got:\n{html}"
    );
}

#[test]
fn doc_block_level_doc_alone_still_generates_child_page() {
    // A block has prose but the child function has none. The child
    // page is generated anyway because the block's safety contract is
    // documentation that applies to every import in the block — every
    // child deserves a landing page where the contract is reachable.
    let scratch = ScratchDir::new("extern-block-doc-alone");
    scratch.write(
        "kara.toml",
        "[package]\nname = \"ffi_doc_alone\"\nedition = \"2026\"\n",
    );
    scratch.write(
        "src/main.kara",
        "/// # Safety\n\
         ///\n\
         /// All imports require initialised state.\n\
         unsafe extern \"C\" {\n\
             pub fn raw_op(x: i32) -> i32;\n\
         }\n\
         fn main() {}\n",
    );

    let out = karac_bin()
        .arg("doc")
        .current_dir(scratch.root())
        .output()
        .unwrap();
    assert!(out.status.success());

    let page = scratch.root().join("dist/doc/raw_op.html");
    assert!(
        page.exists(),
        "expected raw_op.html even though only the block has prose"
    );
    let html = fs::read_to_string(&page).unwrap();
    assert!(
        html.contains("All imports require initialised state."),
        "expected inherited block prose on raw_op page; got:\n{html}"
    );
}
