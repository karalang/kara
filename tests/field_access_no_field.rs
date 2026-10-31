//! A field access the receiver's type cannot answer is a `karac check` error
//! naming that type (B-2026-10-31-18). An enum has no fields, so `e.message` on
//! an `IoError` is refused at check; it used to check clean and fail only in
//! the MIR builder ("a type that failed to check is not lowered yet").

fn errors(src: &str) -> Vec<String> {
    karac::check_source(src)
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect()
}

fn one_error(src: &str) -> String {
    let e = errors(src);
    assert_eq!(e.len(), 1, "{e:?}");
    e[0].clone()
}

#[test]
fn field_access_on_an_enum_is_a_check_error() {
    let user = "enum E {\n    A,\n    B(String),\n}\n\nfn main() {\n    let e = E.A;\n    println(f\"{e.message}\");\n}\n";
    assert_eq!(
        one_error(user),
        "no field 'message' on enum 'E': an enum has no fields; `match` on its variants to reach a payload"
    );
    let io = "fn main() {\n    match fs.read_to_string(\"nope\") {\n        Ok(_) => {},\n        Err(e) => println(f\"{e.message}\"),\n    }\n}\n";
    assert!(one_error(io).starts_with("no field 'message' on enum 'IoError'"));
    let opt = "fn main() {\n    let o = Some(1);\n    let _ = o.value;\n}\n";
    assert!(one_error(opt).starts_with("no field 'value' on enum 'Option'"));
}

#[test]
fn field_access_error_names_the_type_and_only_nameable_fields() {
    // A tuple or closure is named by its type, never "this type".
    let tuple = "fn main() {\n    let t = (1, 2);\n    let _ = t.foo;\n}\n";
    assert_eq!(one_error(tuple), "no field 'foo' on type '(i64, i64)'");
    // A type with no fields the program can name gets no empty list, and a
    // compiler-internal `__` field is never offered.
    let vec = "fn main() {\n    let v = [1, 2];\n    let _ = v.foo;\n}\n";
    assert_eq!(
        one_error(vec),
        "no field 'foo' on type 'Vec', which has no fields"
    );
    let req = "fn main() {\n    let r = Request.new(\"GET\", \"/\");\n    let _ = r.foo;\n}\n";
    assert_eq!(
        one_error(req),
        "no field 'foo' on type 'Request', which has no fields"
    );
    // A struct still lists its fields.
    let st = "struct P { x: i64 }\n\nfn main() {\n    let p = P { x: 1 };\n    println(f\"{p.y}\");\n}\n";
    assert_eq!(
        one_error(st),
        "no field 'y' on struct 'P', available fields: x"
    );
}
