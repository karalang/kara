//! B-2026-10-19-43: a view may not be stored in a field of a `shared` value
//! (core semantics §5.7), and an instance whose body stores one there is an
//! error (§5.8). The check is by type, so it holds where the view arrives
//! as a parameter: through a `ref` handle, pushed into a field's
//! collection, and in a generic instance (`put[ref String]`). A `shared`
//! value of a view type that stores nothing, and an owned copy stored in
//! one, still run.

const PUSHED_INLINE: &str = r#"
shared struct Slot { mut items: Vec[ref String] }
fn main() {
    let name = "a".to_string();
    let s: Slot = Slot { items: Vec.new() };
    let r = ref name;
    s.items.push(r);
    println(f"{s.items.len()}");
}
"#;

const PUSHED_THROUGH_A_PARAMETER: &str = r#"
shared struct Slot { mut items: Vec[ref String] }
fn put(s: Slot, x: ref String) { s.items.push(x); }
fn main() {
    let name = "a".to_string();
    let s: Slot = Slot { items: Vec.new() };
    let r = ref name;
    put(s, r);
    println(f"{s.items.len()}");
}
"#;

const PUSHED_BY_A_GENERIC_INSTANCE: &str = r#"
shared struct Slot[T] { mut items: Vec[T] }
fn put[T](s: Slot[T], x: own T) { s.items.push(x); }
fn main() {
    let name = "a".to_string();
    let s: Slot[ref String] = Slot { items: Vec.new() };
    let r = ref name;
    put(s, r);
    println(f"{s.items.len()}");
}
"#;

const ASSIGNED_BY_A_GENERIC_INSTANCE: &str = r#"
shared struct Slot[T] { mut item: Option[T] }
fn put[T](s: Slot[T], x: own T) { s.item = Some(x); }
fn main() {
    let name = "a".to_string();
    let s: Slot[ref String] = Slot { item: None };
    let r = ref name;
    put(s, r);
    println(f"{s.item.is_some()}");
}
"#;

const OK: &str = r#"
shared struct Slot[T] { mut items: Vec[T] }
fn put[T](s: Slot[T], x: own T) { s.items.push(x); }
fn main() {
    let name = "a".to_string();
    let empty: Slot[ref String] = Slot { items: Vec.new() };
    let s: Slot[String] = Slot { items: Vec.new() };
    put(s, name.clone());
    println(f"{empty.items.len()} {s.items.len()} {name}");
}
"#;

fn refused(src: &str) -> String {
    karac::mir::lower::build_source(src)
        .err()
        .expect("the borrow checker refuses a view stored in a shared field")
}

#[test]
fn a_view_stored_in_a_shared_field_by_type_is_refused() {
    for (src, at) in [
        (PUSHED_INLINE, "borrow check main: 7:"),
        (PUSHED_THROUGH_A_PARAMETER, "borrow check put: 3:"),
    ] {
        let err = refused(src);
        assert!(err.contains("[E0509]"), "{err}");
        assert!(
            err.contains(
                "a view (`ref String`) is stored in `s.items`, a field of a `shared` value"
            ),
            "{err}"
        );
        assert!(err.starts_with(at), "{err}");
    }
}

#[test]
fn a_generic_instance_storing_a_view_in_a_shared_field_is_refused() {
    for src in [PUSHED_BY_A_GENERIC_INSTANCE, ASSIGNED_BY_A_GENERIC_INSTANCE] {
        let err = refused(src);
        assert!(err.contains("[E0509]"), "{err}");
        assert!(
            err.contains("the instance put[ref String] stores a view"),
            "{err}"
        );
    }
}

#[test]
fn a_shared_value_storing_no_view_still_runs() {
    let out = karac::mir::lower::run_source(OK).expect("MIR interpreter");
    assert_eq!(out.output, "0 1 a\n");
}
