//! B-2026-11-26-12 (library/collections.md, `SortedSet[T]`): `clear`, as
//! every other collection has, drops the elements in ascending order, as
//! `SortedMap.clear` drops its entries in key order, and leaves the set
//! empty and reusable. The conformance cell
//! `conformance/library/collections/sorted_set_clear_drops_in_order` runs the
//! same program on the compiled lanes.

#[test]
fn sorted_set_clear_drops_in_ascending_order() {
    let src = "#[derive(PartialEq, Eq, PartialOrd, Ord)]\n\
        struct Tag { n: i64 }\n\
        impl Drop for Tag { fn drop(mut ref self) { println(\"drop \" + self.n.to_string()); } }\n\
        fn main() {\n\
            let mut s: SortedSet[Tag] = SortedSet.new();\n\
            s.insert(Tag { n: 3 }); s.insert(Tag { n: 1 }); s.insert(Tag { n: 2 });\n\
            s.clear();\n\
            println(s.len());\n\
            s.insert(Tag { n: 9 });\n\
            println(\"end\");\n\
        }\n";
    let dir = std::env::temp_dir().join(format!("karac-sorted-set-clear-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("m.kara");
    std::fs::write(&file, src).expect("write source");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_karac"))
        .args(["run", "--interp"])
        .arg(&file)
        .output()
        .expect("run karac");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout, "drop 1\ndrop 2\ndrop 3\n0\nend\ndrop 9\n");
}
