//! A `shared` handle may not enter a task through a parameter or a type
//! parameter (core semantics §11.3, design.md § CrossTask): an `own`
//! parameter captured by a task of a group the function was handed (`g:
//! TaskGroup` reads as a `ref TaskGroup`), a generic `T` with no `CrossTask`
//! bound captured by a task or a `par` branch, and a closure over such a `T`
//! returned as a `+ CrossTask` function value (B-2026-11-19-86).

use karac::check_source;

const HEAD: &str = "shared struct C { n: i64 }\nstruct Wrap { c: C }\n";

fn refused(body: &str) {
    let errs: Vec<(Option<String>, String)> = check_source(&format!("{HEAD}{body}"))
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| (d.code, d.message))
        .collect();
    assert!(
        errs.iter().any(
            |(c, m)| c.as_deref() == Some("E_NOT_CROSS_TASK") || m.contains("E_NOT_CROSS_TASK")
        ),
        "{body}\n{errs:#?}"
    );
}

fn accepted(body: &str) {
    let errs: Vec<String> = check_source(&format!("{HEAD}{body}"))
        .into_iter()
        .filter(|d| d.severity == "error")
        .map(|d| d.message)
        .collect();
    assert!(errs.is_empty(), "{body}\n{errs:#?}");
}

#[test]
fn own_parameter_holding_a_handle_into_a_handed_group() {
    for (ty, ex) in [
        ("C", "c"),
        ("Option[C]", "Some(c)"),
        ("Vec[C]", "[c]"),
        ("(C, i64)", "(c, 1)"),
        ("Wrap", "Wrap { c: c }"),
    ] {
        refused(&format!(
            "fn f(g: TaskGroup, x: own {ty}) -> i64 {{ g.spawn(|| {{ let y = x; 1 }}).join() }}\n\
             fn main() {{ let c = C {{ n: 0 }}; let g = TaskGroup.new(); println(f(g, {ex})); }}\n"
        ));
    }
}

#[test]
fn spawned_handle_parameter_returned_as_a_task_handle() {
    refused(
        "fn sp(g: TaskGroup, x: own C) -> TaskHandle[i64] { g.spawn(|| { let y = x; 1 }) }\n\
         fn main() { let c = C { n: 0 }; let g = TaskGroup.new(); let h = sp(g, c); println(h.join()); }\n",
    );
}

#[test]
fn unbounded_type_parameter_into_a_task_or_a_branch() {
    refused(
        "fn f[T](g: TaskGroup, x: own T) -> i64 { g.spawn(|| { let y = x; 1 }).join() }\n\
         fn main() { let c = C { n: 0 }; let g = TaskGroup.new(); println(f(g, c)); }\n",
    );
    refused(
        "fn f[T](x: own T) -> i64 { let (a, b) = par { { let y = x; 1 }, 0 }; a + b }\n\
         fn main() { let c = C { n: 0 }; println(f(Wrap { c: c })); }\n",
    );
    refused(
        "fn f[T](x: T) -> i64 { let v = par for i in 0..1 { { let y = x; 1 } }; v[0] }\n\
         fn main() { let c = C { n: 0 }; println(f(c)); }\n",
    );
}

#[test]
fn closure_over_an_unbounded_parameter_returned_as_cross_task() {
    refused(
        "fn hold[T](x: own T) -> Fn() -> i64 + CrossTask { || { let r = ref x; 1 } }\n\
         fn main() { let c = C { n: 1 }; let f = hold(c); let g = TaskGroup.new(); println(g.spawn(|| f()).join()); }\n",
    );
}

#[test]
fn owned_values_and_bounded_parameters_still_cross() {
    accepted(
        "import std.sync.{TaskGroup};\nfn f(g: TaskGroup, x: own String) -> i64 { g.spawn(|| { let y = x; 1 }).join() }\n\
         fn main() { let g = TaskGroup.new(); println(f(g, \"ab\".to_string())); }\n",
    );
    accepted(
        "import std.sync.{TaskGroup};\nfn f[T: CrossTask](g: TaskGroup, x: own T) -> i64 { g.spawn(|| { let y = x; 1 }).join() }\n\
         fn main() { let g = TaskGroup.new(); println(f(g, \"ab\".to_string())); }\n",
    );
    accepted(
        "import std.sync.{TaskGroup};\nfn hold[T: CrossTask](x: own T) -> Fn() -> i64 + CrossTask { || { let r = ref x; 1 } }\n\
         fn main() { let f = hold(\"s\".to_string()); let g = TaskGroup.new(); println(g.spawn(|| f()).join()); }\n",
    );
    accepted(
        "fn f(x: own String) -> i64 { let (a, b) = par { { let y = x; 1 }, 0 }; a + b }\n\
         fn main() { println(f(\"ab\".to_string())); }\n",
    );
}
