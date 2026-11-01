//! Core semantics §14.5 with §9.5: a task spawned into a group made outside
//! a `with_provider` call outlives the call, since the group borrows its
//! tasks' origins until it drops, and that is after the provider is gone.
//! `karac check` passed these and the task then read the dropped provider
//! (a use-after-free under ASan, B-2026-11-01-27). A group made inside the
//! call joins its tasks before the provider drops, and stays accepted.

use std::process::Command;

fn check(tag: &str, src: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("karac-outer-group-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.kara");
    std::fs::write(&file, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_karac"))
        .arg("check")
        .arg(&file)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

const CLK: &str = "effect resource Clk: ClkP;
trait ClkP { fn now(ref self) -> i64; }
struct F { t: Vec[i64] }
impl ClkP for F { fn now(ref self) -> i64 { self.t[0] } }
";

fn refused(tag: &str, body: &str, resource: &str) {
    let (ok, out) = check(tag, body);
    assert!(!ok, "{tag}: check accepted it:\n{out}");
    assert!(
        out.contains(&format!("provider-rooted resource `{resource}`"))
            && out.contains("spawned on a task group made outside the `with_provider`"),
        "{tag}: not the §14.5 refusal:\n{out}"
    );
}

fn accepted(tag: &str, body: &str) {
    let (ok, out) = check(tag, body);
    assert!(ok, "{tag}: check refused it:\n{out}");
}

#[test]
fn outer_group_spawn_under_with_provider_is_refused() {
    refused(
        "let",
        &format!(
            "{CLK}fn main() {{ let tg = TaskGroup.new(); \
             let h = with_provider[Clk](F {{ t: vec![1] }}, || tg.spawn(|| Clk.now())); \
             println(h.join()); }}"
        ),
        "Clk",
    );
}

#[test]
fn outer_group_as_parameter_is_refused() {
    refused(
        "param",
        &format!(
            "{CLK}fn under(tg: TaskGroup) -> TaskHandle[i64] {{ \
             with_provider[Clk](F {{ t: vec![1] }}, || tg.spawn(|| Clk.now())) }}
fn main() {{ let tg = TaskGroup.new(); let h = under(tg); println(h.join()); }}"
        ),
        "Clk",
    );
}

#[test]
fn outer_isolating_group_is_refused() {
    refused(
        "isolating",
        &format!(
            "{CLK}fn main() {{ let tg = TaskGroup.isolating(); \
             let h = with_provider[Clk](F {{ t: vec![1] }}, || tg.spawn(|| Clk.now())); \
             println(h.join().unwrap()); }}"
        ),
        "Clk",
    );
}

#[test]
fn outer_group_task_reaching_the_provider_through_a_function_is_refused() {
    refused(
        "named-fn",
        &format!(
            "{CLK}fn rd() -> i64 {{ Clk.now() }}
fn main() {{ let tg = TaskGroup.new(); \
             let h = with_provider[Clk](F {{ t: vec![1] }}, || tg.spawn(|| rd())); \
             println(h.join()); }}"
        ),
        "Clk",
    );
}

#[test]
fn group_from_an_outer_with_provider_spawned_under_an_inner_one_is_refused() {
    refused(
        "nested",
        &format!(
            "{CLK}fn main() {{ let r = with_provider[Clk](F {{ t: vec![1] }}, || {{ \
             let tg = TaskGroup.new(); \
             let h = with_provider[Clk](F {{ t: vec![2] }}, || tg.spawn(|| Clk.now())); \
             h.join() }}); println(r); }}"
        ),
        "Clk",
    );
}

#[test]
fn outer_group_under_a_stdout_override_is_refused() {
    refused(
        "stdout",
        "struct Cap { n: i64 }
impl StdoutProvider for Cap {
    fn write(mut ref self, s: Str) { self.n = self.n + 1; }
    fn flush(mut ref self) {}
}
fn main() { let tg = TaskGroup.new(); \
         let h = with_provider[Stdout](Cap { n: 0 }, || tg.spawn(|| { println(\"x\"); 0 })); \
         println(h.join()); }",
        "Stdout",
    );
}

#[test]
fn group_made_inside_the_with_provider_is_accepted() {
    accepted(
        "inside",
        &format!(
            "{CLK}fn main() {{ let r = with_provider[Clk](F {{ t: vec![1] }}, || {{ \
             let tg = TaskGroup.new(); let h = tg.spawn(|| Clk.now()); h.join() }}); \
             println(r); }}"
        ),
    );
    accepted(
        "inside-nested-block",
        &format!(
            "{CLK}fn main() {{ let r = with_provider[Clk](F {{ t: vec![1] }}, || {{ \
             if true {{ let g = TaskGroup.new(); g.spawn(|| Clk.now()).join() }} else {{ 0 }} }}); \
             println(r); }}"
        ),
    );
}

#[test]
fn outer_group_task_not_using_the_resource_is_accepted() {
    accepted(
        "unused",
        &format!(
            "{CLK}fn main() {{ let tg = TaskGroup.new(); \
             let h = with_provider[Clk](F {{ t: vec![1] }}, || tg.spawn(|| 7)); \
             println(h.join()); }}"
        ),
    );
}
