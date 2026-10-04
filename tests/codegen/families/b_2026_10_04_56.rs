//! B-2026-10-04-56 -- a mono `Map[K, V]` operation takes its key's hash at the
//! call site, so the counting idiom hashes once; maps under any other hasher
//! keep hashing through their stored `hash_fn`.

use super::*;

/// Compile `src` to unoptimized IR, the way `attrs_ir.rs` does.
fn ir_of(src: &str) -> String {
    let mut parsed = karac::parse(src);
    let resolved = karac::resolve(&parsed.program);
    let typed = karac::typecheck(&parsed.program, &resolved);
    karac::lower(&mut parsed.program, &typed);
    let ownership = karac::ownershipcheck(&parsed.program, &typed);
    karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None).expect("codegen")
}

/// The body of `define internal i64 @<name>(...)`, up to its closing brace.
fn body_of(ir: &str, name: &str) -> String {
    let head = format!("@{name}(");
    let start = ir
        .lines()
        .position(|l| l.starts_with("define") && l.contains(&head))
        .unwrap_or_else(|| panic!("no definition of {name} in the IR"));
    ir.lines()
        .skip(start)
        .take_while(|l| *l != "}")
        .collect::<Vec<_>>()
        .join("\n")
}

/// The attribute group a `define ... #N {` line names, resolved to its text.
fn fn_attrs(ir: &str, name: &str) -> String {
    let head = format!("@{name}(");
    let line = ir
        .lines()
        .find(|l| l.starts_with("define") && l.contains(&head))
        .unwrap();
    let Some(group) = line.rsplit(" #").next().and_then(|t| t.split(' ').next()) else {
        return String::new();
    };
    let decl = format!("attributes #{group} = ");
    ir.lines()
        .find(|l| l.starts_with(&decl))
        .unwrap_or("")
        .to_string()
}

const COUNTING: &str = r#"
fn main() {
    let text = "abracadabra alakazam";
    let mut counts: Map[char, i64] = Map.new();
    for c in text.chars() {
        counts.insert(c, counts.get(c).unwrap_or(0) + 1);
    }
    let mut lines: Vec[String] = Vec.new();
    for (c, n) in counts {
        lines.push(f"{c}={n}");
    }
    lines.sort();
    for l in lines {
        println(l);
    }
    let mut sq: Map[i64, i64] = Map.new();
    let mut i = 0;
    while i < 3000 {
        let k = (i * 7919) % 1000;
        sq.insert(k, sq.get(k).unwrap_or(0) + i);
        i += 1;
    }
    let mut total = 0;
    let mut j = 0;
    while j < 1000 {
        total = (total * 31 + sq.get(j).unwrap_or(-1)) % 1000000007;
        j += 1;
    }
    println(f"{sq.len()} {total}");
}
"#;

/// The counting idiom over a `char` key and an `i64` key gives the right
/// counts once both of its calls share one hash. Checked against the
/// interpreter's answer, written out.
#[test]
fn e2e_counting_idiom_with_a_call_site_hash_counts_right() {
    let Some(out) = run_program(COUNTING) else {
        return;
    };
    assert_eq!(
        out,
        " =1\na=9\nb=2\nc=1\nd=1\nk=1\nl=1\nm=1\nr=2\nz=1\n1000 \
         913593203\n"
    );
}

/// The default-hasher-only program gets the call-site hash: the per-key hash
/// fn calls the runtime's SipHash on the key value and is marked
/// `memory(none)`, which is what lets LLVM share it between a `get` and the
/// `insert` after it.
#[test]
fn a_default_hasher_program_hashes_at_the_call_site() {
    let ir = ir_of(COUNTING);
    let i32_body = body_of(&ir, "karac_map_i32_hash");
    assert!(
        i32_body.contains("@karac_hash_int(") && !i32_body.contains("hash.fn"),
        "char key should hash its value at the call site:\n{i32_body}"
    );
    let i64_body = body_of(&ir, "karac_map_i64_hash");
    assert!(
        i64_body.contains("@karac_hash_word(") && !i64_body.contains("hash.fn"),
        "i64 key should hash its value at the call site:\n{i64_body}"
    );
    let attrs = fn_attrs(&ir, "karac_map_i64_hash");
    assert!(
        attrs.contains("memory(none)"),
        "the call-site hash must be pure to be shared: {attrs}"
    );
}

/// An `FxBuildHasher` map with `i64` keys files them under Fx, so every `i64`
/// map in the program keeps the stored pointer, the default one included.
/// The `char` key width has no such map and keeps the call-site hash.
#[test]
fn an_fx_map_keeps_its_key_width_on_the_stored_hash() {
    let src = r#"
fn main() {
    let mut fx: Map[i64, i64, FxBuildHasher] = Map.new();
    fx.insert(1, fx.get(1).unwrap_or(0) + 1);
    let mut d: Map[i64, i64] = Map.new();
    d.insert(2, d.get(2).unwrap_or(0) + 1);
    let mut c: Map[char, i64] = Map.new();
    c.insert('x', c.get('x').unwrap_or(0) + 1);
    println(f"{fx.len()} {d.len()} {c.len()}");
}
"#;
    let ir = ir_of(src);
    let i64_body = body_of(&ir, "karac_map_i64_hash");
    assert!(
        i64_body.contains("hash.fn") && !i64_body.contains("@karac_hash_word("),
        "an Fx i64 map must keep i64 keys on the stored hash fn:\n{i64_body}"
    );
    assert!(!fn_attrs(&ir, "karac_map_i64_hash").contains("memory(none)"));
    let i32_body = body_of(&ir, "karac_map_i32_hash");
    assert!(
        i32_body.contains("@karac_hash_int("),
        "the char key width has no Fx map and keeps the call-site hash:\n{i32_body}"
    );
}

/// B-2026-08-22-27's program, now with the counting idiom: an Fx-hashed
/// `Map[i64, i64]` grown well past several resizes, beside a default-hashed
/// one, finds every key it holds with the right count. A map whose keys
/// were filed under one hash and probed under another loses a contiguous
/// tail of them, so `missing` is the column that would show it.
#[test]
fn e2e_fx_and_default_i64_maps_side_by_side_find_every_key() {
    let Some(out) = run_program(
        r#"
fn main() {
    let mut fx: Map[i64, i64, FxBuildHasher] = Map.new();
    let mut d: Map[i64, i64] = Map.new();
    let mut i = 0;
    while i < 60000 {
        let k = i % 40000;
        fx.insert(k, fx.get(k).unwrap_or(0) + 1);
        d.insert(k, d.get(k).unwrap_or(0) + 1);
        i += 1;
    }
    let mut missing = 0;
    let mut twos = 0;
    let mut j = 0;
    while j < 40000 {
        match fx.get(j) {
            Some(n) => { if n == 2 { twos += 1; } }
            None => { missing += 1; }
        }
        match d.get(j) {
            Some(n) => { if n == 2 { twos += 1; } }
            None => { missing += 1; }
        }
        j += 1;
    }
    println(f"{fx.len()} {d.len()} missing={missing} twos={twos}");
}
"#,
    ) else {
        return;
    };
    assert_eq!(out, "40000 40000 missing=0 twos=40000\n");
}

/// A user hasher's `i64` map keeps the stored pointer too, and still counts
/// right through the idiom.
#[test]
fn e2e_user_hashed_i64_map_counts_through_its_own_hash() {
    let src = format!(
        "{USER_HASHERS}\
fn main() {{
    let mut m: Map[i64, i64, FnvBuild] = Map.new();
    let mut i = 0;
    while i < 5000 {{
        let k = i % 700;
        m.insert(k, m.get(k).unwrap_or(0) + 1);
        i += 1;
    }}
    let mut missing = 0;
    let mut sum = 0;
    let mut j = 0;
    while j < 700 {{
        match m.get(j) {{
            Some(n) => {{ sum += n; }}
            None => {{ missing += 1; }}
        }}
        j += 1;
    }}
    println(f\"{{m.len()}} missing={{missing}} sum={{sum}}\");
}}
"
    );
    let ir = ir_of(&src);
    assert!(body_of(&ir, "karac_map_i64_hash").contains("hash.fn"));
    let Some(out) = run_program(&src) else {
        return;
    };
    assert_eq!(out, "700 missing=0 sum=5000\n");
}
