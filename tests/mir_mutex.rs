//! `Mutex[T].acquire` and `Mutex[T].release` on both MIR backends
//! (B-2026-10-19-23): the acquire is a suspending native whose `__ready` is
//! "no task holds the mutex", so a task that holds it across a pause keeps
//! the others out.
//!
//! A `lock` block lowers to an `acquire` and a `MutexGuard` whose drop
//! releases the mutex. The second test lowers a program whose `bump` is one
//! `lock` block and adds a `release` before its return, so the guard's own
//! release finds the mutex free.
#![cfg(feature = "llvm")]

mod common;

use karac::mir::syntax::{
    BasicBlock, Const, ConstKind, Operand, Place, TerminatorKind, UnwindAction,
};
use std::process::Command;

const SRC: &str = r#"import std.time.{sleep, Duration};
fn bump(m: Mutex[i64]) {
    lock m x {
        let v = x;
        sleep(Duration.ms(2));
        x = v + 1;
    }
}
fn main() {
    let m = Mutex.new(0);
    {
        let g = TaskGroup.new();
        for _ in 0..4 {
            g.spawn(|| bump(m));
        }
    }
    lock m x { println(x); }
}
"#;

/// `SRC` lowered, with `releases` more calls of `release` before `bump`'s
/// return.
fn lowered(releases: usize) -> karac::mir::lower::Lowered {
    let mut lowered = karac::mir::lower::build_source(SRC).expect("MIR");
    let body = lowered
        .program
        .bodies
        .values_mut()
        .find(|b| b.instance.name == "bump")
        .expect("bump");
    let mut recv = None;
    for b in &mut body.blocks {
        if let TerminatorKind::Call {
            func:
                Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }),
            args,
            ..
        } = &mut b.terminator.kind
        {
            if let Some(ty) = inst.name.strip_suffix(".acquire").map(str::to_string) {
                let r = match &args[0] {
                    Operand::Move(p) | Operand::Copy(p) => p.clone(),
                    other => panic!("acquire of {other:?}"),
                };
                let mut release = inst.clone();
                release.name = format!("{ty}.release");
                recv = Some((release, r));
            }
        }
    }
    let (inst, r) = recv.expect("bump's acquire");
    let release = Operand::Const(Const {
        ty: lowered.tys.unit(),
        kind: ConstKind::FnDef(inst),
    });
    let returns: Vec<usize> = (0..body.blocks.len())
        .filter(|&i| matches!(body.blocks[i].terminator.kind, TerminatorKind::Return))
        .collect();
    for i in returns {
        let mut at = i;
        for _ in 0..releases {
            let next = body.blocks.len();
            let mut ret = body.blocks[at].clone();
            ret.statements.clear();
            body.blocks.push(ret);
            body.blocks[at].terminator.kind = TerminatorKind::Call {
                func: release.clone(),
                args: vec![Operand::Copy(r.clone())],
                destination: Place::local(karac::mir::Local(0)),
                target: Some(BasicBlock(next as u32)),
                unwind: UnwindAction::Abort,
            };
            at = next;
        }
    }
    lowered
}

/// The interpreter's run of `lowered(releases)`: its output and error.
fn interp(releases: usize) -> (String, Option<String>) {
    let l = lowered(releases);
    let r = karac::mir::interp::run_untraced(&l.program, &l.tys, "main", vec![]);
    let error = match r.outcome {
        karac::mir::interp::Outcome::Error(e) => Some(e),
        _ => None,
    };
    (r.output, error)
}

/// The compiled run: its stdout, stderr and whether it succeeded, or
/// `None` when the runtime archive does not link.
fn compiled(releases: usize) -> Option<(String, String, bool)> {
    let l = lowered(releases);
    let ir = karac::backend::mir::compile_mir_ir(&l.program, &l.tys).expect("IR");
    let dir =
        std::env::temp_dir().join(format!("karac-mir-mutex-{releases}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, exe) = (dir.join("main.o"), dir.join("main"));
    karac::backend::mir_aot::compile_object(&ir, &obj, 0).expect("object");
    common::link_or_skip(karac::backend::driver::link_executable(
        &obj.to_string_lossy(),
        &exe.to_string_lossy(),
    ))?;
    let out = Command::new(&exe).output().expect("run");
    let _ = std::fs::remove_dir_all(&dir);
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    ))
}

#[test]
fn mir_mutex_excludes_across_a_pause() {
    let (out, error) = interp(0);
    assert_eq!(error, None);
    assert_eq!(out, "4\n");
    if let Some((out, err, ok)) = compiled(0) {
        assert!(ok, "{err}");
        assert_eq!(out, "4\n");
    }
}

#[test]
fn mir_mutex_release_of_a_free_mutex_stops_the_run() {
    let (_, error) = interp(1);
    let error = error.expect("a second release fails");
    assert!(
        error.contains("unlock of a mutex that is not held"),
        "{error}"
    );
    if let Some((_, err, ok)) = compiled(1) {
        assert!(!ok);
        assert!(err.contains("unlock of a mutex that is not held"), "{err}");
    }
}
