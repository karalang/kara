//! `karac doc` signatures keep generic arguments and type parameters; a
//! struct page lists its public methods and the traits it implements, and a
//! trait page lists its methods (B-2026-11-08-34).

use std::path::{Path, PathBuf};
use std::process::Command;

fn build(tag: &str, lib: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("karac-doc-sig-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("kara.toml"),
        "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\nedition = \"2026\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.kara"), lib).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("doc")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "karac doc failed: {}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    root
}

fn page(root: &Path, rel: &str) -> String {
    let path = root.join("dist/doc").join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| {
        let mut listing = String::new();
        for entry in walk(&root.join("dist/doc")) {
            listing.push_str(&format!("{}\n", entry.display()));
        }
        panic!("{}: {e}\n{listing}", path.display())
    })
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

const LIB: &str = "\
/// A counter.
pub trait Counter {
    /// The current count.
    fn get(self) -> i64;
    /// One more than the count.
    fn next(self) -> i64 { self.get() + 1 }
}

/// A stack of items.
pub struct Stack[T] {
    items: Vec[T],
}

impl[T] Stack[T] {
    /// An empty stack.
    pub fn new() -> Stack[T] { Stack { items: Vec.new() } }
    /// Pushes one item.
    pub fn push(mut ref self, item: own T) { self.items.push(item); }
    fn secret(self) -> i64 { 0 }
}

impl[T] Counter for Stack[T] {
    fn get(self) -> i64 { self.items.len() as i64 }
}

/// The first `n` items' keys.
pub fn keys[K: Clone](m: Map[K, i64], n: i64) -> Vec[K] with panics {
    let mut out: Vec[K] = Vec.new();
    for (k, _) in m {
        if (out.len() as i64) < n { out.push(k.clone()); }
    }
    out
}
";

#[test]
fn doc_signatures_keep_type_parameters_and_generic_arguments() {
    let root = build("generics", LIB);
    let keys = page(&root, "keys.html");
    let stack = page(&root, "Stack.html");
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        keys.contains("fn keys[K: Clone](m: Map[K, i64], n: i64) -&gt; Vec[K] with panics"),
        "{keys}"
    );
    assert!(stack.contains("struct Stack[T]"), "{stack}");
    assert!(stack.contains("items"), "{stack}");
    assert!(stack.contains("Vec[T]"), "{stack}");
}

#[test]
fn doc_struct_page_lists_its_methods_and_trait_impls() {
    let root = build("methods", LIB);
    let stack = page(&root, "Stack.html");
    let _ = std::fs::remove_dir_all(&root);
    assert!(stack.contains("<h2>Methods</h2>"), "{stack}");
    assert!(stack.contains("fn new() -&gt; Stack[T]"), "{stack}");
    assert!(
        stack.contains("fn push(mut ref self, item: own T)"),
        "{stack}"
    );
    assert!(stack.contains("Pushes one item."), "{stack}");
    assert!(
        !stack.contains("secret"),
        "a non-pub method is listed: {stack}"
    );
    assert!(stack.contains("<h2>Trait implementations</h2>"), "{stack}");
    assert!(stack.contains("impl[T] Counter for Stack[T]"), "{stack}");
}

#[test]
fn doc_trait_page_lists_its_required_and_provided_methods() {
    let root = build("trait", LIB);
    let counter = page(&root, "Counter.html");
    let _ = std::fs::remove_dir_all(&root);
    assert!(counter.contains("<h2>Required methods</h2>"), "{counter}");
    assert!(counter.contains("fn get(self) -&gt; i64"), "{counter}");
    assert!(counter.contains("The current count."), "{counter}");
    assert!(counter.contains("<h2>Provided methods</h2>"), "{counter}");
    assert!(counter.contains("fn next(self) -&gt; i64"), "{counter}");
}
