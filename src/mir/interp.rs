//! The MIR interpreter: the reference implementation and the oracle the
//! LLVM backend is checked against (`docs/spikes/mir-types.md` §6).
//!
//! Every local holds a [`Value`] whose parts can be individually
//! uninitialized; a `Move` marks its source uninitialized and reading an
//! uninitialized place is an error. `shared` boxes live in a heap with
//! reference counts, so a use after free, a double release and a leak are
//! all reported. Each run records an ownership event trace.
//!
//! The phase of each body decides how `Drop` behaves. Before drop
//! elaboration a `Drop` of a place that is not initialized does nothing,
//! which is the dynamic meaning of the builder's scope-end drops. From
//! `DropsElaborated` on, the same `Drop` is an error, because elaboration
//! must have removed or guarded it. Running one program both ways is the
//! check on drop elaboration.

use std::collections::BTreeMap;
use std::fmt;

use super::coroutine;
use super::parse::MirModule;
use super::place_ty::place_ty;
use super::pretty;
use super::syntax::*;
use super::ty::{AdtId, IntTy, IntrinsicTy, Ty, TyInterner, TyKind};
use super::validate::validate;

/// The functions of a program, by instance name, and the `Drop` body of
/// each ADT that has one.
#[derive(Debug, Default, Clone)]
pub struct Program {
    pub bodies: BTreeMap<String, Body>,
    /// The instance name of each ADT's `Drop` body, which takes the value
    /// as its single `mut ref` parameter.
    pub drop_impls: BTreeMap<AdtId, String>,
    /// The `Drop` body of each instance of a generic ADT, by its type.
    pub drop_by_ty: BTreeMap<Ty, String>,
    /// ADTs whose derived `Display` is not the default shape.
    pub display_styles: BTreeMap<AdtId, DisplayStyle>,
    /// The program's statics, indexed by `ConstKind::Static`.
    pub statics: Vec<StaticDef>,
}

/// How a derived `Display` departs from the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayStyle {
    /// `#[derive(Display(snake_case))]`: variant names in snake_case.
    SnakeCase,
    /// The library's `Secret[T]`: shown as `<redacted>` at any depth.
    Redacted,
    /// A `distinct type`: shown as its base value.
    Transparent,
}

impl Program {
    pub fn add(&mut self, body: Body) {
        self.bodies.insert(body.instance.name.clone(), body);
    }

    /// Every body of a parsed module; a body named `T.drop` is the `Drop`
    /// body of the ADT `T`.
    pub fn from_module(m: &MirModule) -> Program {
        let mut p = Program::default();
        for b in &m.bodies {
            if let Some(adt) = b
                .instance
                .name
                .strip_suffix(".drop")
                .and_then(|t| m.adt_named(t))
            {
                p.drop_impls.insert(adt, b.instance.name.clone());
            }
            p.add(b.clone());
        }
        p.statics = m.statics.clone();
        p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllocId(pub u32);

/// The empty weak handle: a `weak T` made from `None`. It upgrades to
/// `None` and owns no weak count.
const EMPTY_WEAK: AllocId = AllocId(u32::MAX);

impl fmt::Display for AllocId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Root {
    /// A local of the frame at `frame` on the stack; `id` detects a frame
    /// that has returned and been replaced.
    Local {
        frame: usize,
        id: u64,
        local: Local,
    },
    Heap(AllocId),
    /// The static at this index in the program's statics.
    Static(u32),
}

/// Where a value lives: a root and the field or element indices below it.
#[derive(Debug, Clone, PartialEq)]
pub struct Addr {
    root: Root,
    path: Vec<u64>,
}

impl Addr {
    fn child(&self, i: u64) -> Addr {
        let mut a = self.clone();
        a.path.push(i);
        a
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Uninit,
    Bool(bool),
    Int(i128),
    Char(char),
    Float(f64),
    Unit,
    Str(String),
    /// A tuple, struct, array or closure: its parts in order.
    Agg(Vec<Value>),
    /// An enum value: the variant index and its fields.
    Variant(u32, Vec<Value>),
    Shared(AllocId),
    /// A `weak` handle to the `shared` value in this slot.
    Weak(AllocId),
    /// A library collection (`String`, `Vec[T]`): the sole owner of its
    /// heap allocation, which holds a `Str` or the elements as an `Agg`.
    Box(AllocId),
    Ref(Addr),
    /// A `Slice[T]` view: `len` elements of the sequence at `base` (a
    /// `Vec`'s allocation or an array place), starting at `lo`.
    Slice {
        base: Addr,
        lo: u64,
        len: u64,
    },
    Fn(InstanceId),
    /// An erased function value (`Fn`, `MutFn`, `OnceFn`): the body it
    /// calls and, for a closure, the heap slot holding its environment,
    /// whose type is `env_ty`. It owns the environment.
    Erased {
        body: String,
        env: Option<(AllocId, Ty)>,
    },
}

impl Value {
    fn fully_init(&self) -> bool {
        match self {
            Value::Uninit => false,
            Value::Agg(fs) | Value::Variant(_, fs) => fs.iter().all(Value::fully_init),
            _ => true,
        }
    }

    fn any_init(&self) -> bool {
        match self {
            Value::Uninit => false,
            // A value with no parts (a fieldless variant, an empty tuple)
            // is initialized; one whose every part was moved out is not.
            Value::Agg(fs) | Value::Variant(_, fs) => {
                fs.is_empty() || fs.iter().any(Value::any_init)
            }
            _ => true,
        }
    }
}

/// One line of the ownership trace (`docs/spikes/mir-types.md` §6).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Enter(String),
    Exit(String),
    Alloc(AllocId, String),
    Free(AllocId),
    Move(String),
    Init(String),
    Drop(String, String),
    DropBody(String),
    Retain(AllocId, u32),
    Release(AllocId, u32),
    Flag(Local, bool),
    Abort(AbortReason),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Event::Enter(n) => write!(f, "enter {n}"),
            Event::Exit(n) => write!(f, "exit {n}"),
            Event::Alloc(a, t) => write!(f, "alloc {a} {t}"),
            Event::Free(a) => write!(f, "free {a}"),
            Event::Move(p) => write!(f, "move {p}"),
            Event::Init(p) => write!(f, "init {p}"),
            Event::Drop(p, t) => write!(f, "drop {p} {t}"),
            Event::DropBody(t) => write!(f, "drop_body {t}"),
            Event::Retain(a, c) => write!(f, "retain {a} {c}"),
            Event::Release(a, c) => write!(f, "release {a} {c}"),
            Event::Flag(l, b) => write!(f, "flag {l} {b}"),
            Event::Abort(r) => write!(f, "abort {r:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Returned(Value),
    /// An `Abort` terminator ran: the process exits with 101 and runs no
    /// drops (core semantics §9).
    Aborted(AbortReason),
    /// `main` returned an `Err`: its error went to stderr and the process
    /// exits with this code (1).
    Exited(i32),
    /// The program did something the semantics forbid, or MIR the
    /// interpreter cannot run: a bug in the program or in an earlier pass.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub output: String,
    /// What the program wrote to stderr (`eprintln`, `main`'s `Err`).
    pub stderr: String,
    pub events: Vec<Event>,
    pub outcome: Outcome,
    /// How many times the executor resumed a task root that was pending
    /// (`KARAC_MIR_COROUTINES=1`); 0 otherwise.
    pub resumes: u64,
}

impl RunResult {
    pub fn exit_code(&self) -> Option<i32> {
        match self.outcome {
            Outcome::Returned(_) => Some(0),
            Outcome::Aborted(_) => Some(101),
            Outcome::Exited(code) => Some(code),
            Outcome::Error(_) => None,
        }
    }

    pub fn trace(&self) -> String {
        let mut s = String::new();
        for e in &self.events {
            s.push_str(&e.to_string());
            s.push('\n');
        }
        s
    }
}

const MAX_DEPTH: usize = 200_000;
/// The default step budget, which only stops a runaway loop: real
/// programs in the corpus run tens of millions of steps, and the corpus
/// runner's timeout bounds wall time. `KARAC_MIR_MAX_STEPS` overrides it.
const MAX_STEPS: u64 = 4_000_000_000;

/// The program's file name as `karac __mir-run` was given it, for
/// `dbg`'s `[file:line]` prefix; `<unknown>` when nothing set it.
static SOURCE_NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Names the program's file for `dbg` output. The first name set wins.
pub fn set_source_name(name: &str) {
    let _ = SOURCE_NAME.set(name.to_string());
}

/// Runs `entry` with `args`, validating every body first, and records
/// the [`Event`] trace.
pub fn run(program: &Program, tys: &TyInterner, entry: &str, args: Vec<Value>) -> RunResult {
    run_with(program, tys, entry, args, true, false, tasks_flag())
}

/// [`run`], without the trace: [`RunResult::events`] stays empty.
pub fn run_untraced(
    program: &Program,
    tys: &TyInterner,
    entry: &str,
    args: Vec<Value>,
) -> RunResult {
    run_with(program, tys, entry, args, false, false, tasks_flag())
}

/// [`run_untraced`], writing the program's stdout and stderr to the
/// process's as it goes, so what printed survives an abort;
/// [`RunResult::output`] and [`RunResult::stderr`] stay empty.
pub fn run_streaming(
    program: &Program,
    tys: &TyInterner,
    entry: &str,
    args: Vec<Value>,
) -> RunResult {
    run_with(program, tys, entry, args, false, true, tasks_flag())
}

/// [`run`], with coroutines run as state machines under the executor
/// whatever `KARAC_MIR_COROUTINES` says.
pub fn run_coroutines(
    program: &Program,
    tys: &TyInterner,
    entry: &str,
    args: Vec<Value>,
) -> RunResult {
    run_with(program, tys, entry, args, true, false, Tasks::Run)
}

/// [`run_coroutines`], dropping the root's frame through its `drop_frame`
/// once it has been pending `after` times, as a cancellation would.
pub fn run_coroutines_cancelled(
    program: &Program,
    tys: &TyInterner,
    entry: &str,
    args: Vec<Value>,
    after: u64,
) -> RunResult {
    run_with(
        program,
        tys,
        entry,
        args,
        true,
        false,
        Tasks::CancelAfter(after),
    )
}

/// How coroutines run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tasks {
    /// Their suspending calls complete synchronously.
    Sync,
    /// As state machines, the root resumed by the executor until `Ready`.
    Run,
    /// As `Run`, cancelling the root after it has been pending this often.
    CancelAfter(u64),
}

/// `KARAC_MIR_COROUTINES=1` runs coroutines under the executor.
fn tasks_flag() -> Tasks {
    if std::env::var("KARAC_MIR_COROUTINES").is_ok_and(|v| v == "1") {
        Tasks::Run
    } else {
        Tasks::Sync
    }
}

fn run_with(
    program: &Program,
    tys: &TyInterner,
    entry: &str,
    args: Vec<Value>,
    trace: bool,
    stream: bool,
    tasks: Tasks,
) -> RunResult {
    let coroutines = tasks != Tasks::Sync;
    let transformed;
    let program = if coroutines {
        let bodies: Vec<Body> = program.bodies.values().cloned().collect();
        match coroutine::transform(&bodies, tys) {
            Ok((bodies, _)) => {
                let mut p = program.clone();
                p.bodies = bodies
                    .into_iter()
                    .map(|b| (b.instance.name.clone(), b))
                    .collect();
                transformed = p;
                &transformed
            }
            Err(e) => {
                return RunResult {
                    output: String::new(),
                    stderr: String::new(),
                    events: Vec::new(),
                    outcome: Outcome::Error(e),
                    resumes: 0,
                }
            }
        }
    } else {
        program
    };
    let mut problems = Vec::new();
    for (name, body) in &program.bodies {
        for e in validate(body, tys) {
            problems.push(format!("{name}: {e}"));
        }
    }
    let mut it = Interp {
        program,
        tys,
        frames: Vec::new(),
        next_frame_id: 0,
        heap: Vec::new(),
        free: Vec::new(),
        key_index: Default::default(),
        events: Vec::new(),
        trace,
        stream,
        needs_drop: Default::default(),
        output: String::new(),
        stderr: String::new(),
        steps: 0,
        max_steps: std::env::var("KARAC_MIR_MAX_STEPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(MAX_STEPS),
        snapshots: Default::default(),
        arenas: 0,
        channels: Vec::new(),
        files: Vec::new(),
        statics: Vec::new(),
        caps: Default::default(),
        rand: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64)
            | 1,
        sorted_tables: Default::default(),
        flags: Vec::new(),
        resumes: 0,
        parked_flags: Vec::new(),
        tracing_min: 0,
        debug_render: false,
        tracing_sink: None,
        active_spans: Vec::new(),
        interners: Vec::new(),
        http_builders: Vec::new(),
        http_headers: Vec::new(),
        cancel_after: match tasks {
            Tasks::CancelAfter(n) => Some(n),
            _ => None,
        },
    };
    let task = coroutines && program.bodies.contains_key(&coroutine::resume_name(entry));
    let outcome = if !problems.is_empty() {
        Outcome::Error(format!("invalid MIR:\n{}", problems.join("\n")))
    } else {
        let ret = it.init_statics().and_then(|()| {
            if task {
                it.run_task(entry, args)
            } else {
                it.call(entry, args)
            }
        });
        match ret.and_then(|v| it.main_result(entry, v)) {
            Ok(done) => match it.leaks() {
                Some(leaks) => Outcome::Error(leaks),
                None => done,
            },
            Err(Stop::Abort(r)) => Outcome::Aborted(r),
            Err(Stop::Exit(code)) => Outcome::Exited(code),
            Err(Stop::Error(e)) => Outcome::Error(e),
        }
    };
    RunResult {
        output: it.output,
        stderr: it.stderr,
        events: it.events,
        outcome,
        resumes: it.resumes,
    }
}

/// The unary float function `method` of `x`, at `f64` when `wide`, else
/// at `f32`; `None` for a name that is not one.
fn float_unary(method: &str, x: f64, wide: bool) -> Option<f64> {
    use crate::float_math as fm;
    macro_rules! at {
        ($m:ident) => {
            if wide {
                f64::$m(x)
            } else {
                f32::$m(x as f32) as f64
            }
        };
    }
    macro_rules! libm {
        ($w:path, $n:path) => {
            if wide {
                $w(x)
            } else {
                $n(x as f32) as f64
            }
        };
    }
    Some(match method {
        "sqrt" => at!(sqrt),
        "sin" => at!(sin),
        "cos" => at!(cos),
        "tan" => at!(tan),
        "exp" => at!(exp),
        "ln" => at!(ln),
        "log2" => at!(log2),
        "log10" => at!(log10),
        "floor" => at!(floor),
        "ceil" => at!(ceil),
        "round" => at!(round),
        "trunc" => at!(trunc),
        "asin" => at!(asin),
        "acos" => at!(acos),
        "atan" => at!(atan),
        "sinh" => at!(sinh),
        "cosh" => at!(cosh),
        "tanh" => at!(tanh),
        "exp2" => at!(exp2),
        "exp_m1" => at!(exp_m1),
        "ln_1p" => at!(ln_1p),
        "asinh" => libm!(fm::asinh_f64, fm::asinh_f32),
        "acosh" => libm!(fm::acosh_f64, fm::acosh_f32),
        "atanh" => libm!(fm::atanh_f64, fm::atanh_f32),
        "cbrt" => libm!(fm::cbrt_f64, fm::cbrt_f32),
        "recip" => at!(recip),
        "to_degrees" => at!(to_degrees),
        "to_radians" => at!(to_radians),
        "fract" => at!(fract),
        "signum" => at!(signum),
        _ => return None,
    })
}

/// A float result rounded to its type: the interpreter computes in `f64`,
/// and every operation on a narrower float rounds, as the hardware does.
fn narrow_float(v: Value, kind: TyKind) -> Value {
    match (v, kind) {
        (Value::Float(f), TyKind::Float(ft)) => Value::Float(ft.round(f)),
        (v, _) => v,
    }
}

enum Stop {
    Abort(AbortReason),
    /// `process.exit(code)`: the process ends now with `code`, running no
    /// drops.
    Exit(i32),
    Error(String),
}

impl From<String> for Stop {
    fn from(s: String) -> Stop {
        Stop::Error(s)
    }
}

type R<T> = Result<T, Stop>;

fn err<T>(msg: impl Into<String>) -> R<T> {
    Err(Stop::Error(msg.into()))
}

/// The borrow flags a frame holds: by loan site, each flagged field's
/// address and how it is held.
type Held = Vec<(u32, Vec<(Addr, BorrowKind)>)>;

struct Frame {
    id: u64,
    locals: Vec<Value>,
    /// The §6.2 borrow flags this frame holds, by loan site: each flag's
    /// field address and how it is held.
    held: Held,
}

/// A §6.2 borrow flag: how many readers hold it, or whether a writer does.
#[derive(Default)]
struct Flag {
    readers: u32,
    writer: bool,
}

struct HeapObj {
    count: u32,
    /// The `weak` handles to a `shared` value (core semantics §6.5). At
    /// strong count zero the value is dropped; the slot is freed once
    /// this is zero too.
    weak: u32,
    value: Value,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Read,
    Write,
    /// Like `Read`, but an uninitialized prefix yields `None`.
    Probe,
}

struct Interp<'a> {
    program: &'a Program,
    tys: &'a TyInterner,
    frames: Vec<Frame>,
    next_frame_id: u64,
    heap: Vec<Option<HeapObj>>,
    /// Freed slots of `heap`, reused by [`Interp::alloc`].
    free: Vec<u32>,
    /// Per `Map`/`Set`: each key's hashable form ([`Interp::key_repr`]) to
    /// its entry's position, built on the first lookup and kept up to date
    /// by the table's own inserts and removes. `None` for a table holding a
    /// key with no hashable form, which is searched entry by entry.
    key_index: rustc_hash::FxHashMap<AllocId, Option<rustc_hash::FxHashMap<String, usize>>>,
    events: Vec<Event>,
    /// Whether `events` is kept. A run outside the tests skips it: one
    /// event per statement is most of the time and memory a long program
    /// spends.
    trace: bool,
    /// Write output to the process's stdout and stderr as it happens.
    stream: bool,
    /// `needs_drop` per type, asked on every write: the type context
    /// walks the type each time.
    needs_drop: std::cell::RefCell<rustc_hash::FxHashMap<Ty, bool>>,
    output: String,
    stderr: String,
    steps: u64,
    max_steps: u64,
    /// Read-only copies a library method hands out a view of (the bytes
    /// of a `String`): live until exit, and not leaks.
    snapshots: rustc_hash::FxHashSet<AllocId>,
    /// The last `Arena` id handed out.
    arenas: i128,
    /// The queues behind `Channel` ends and `BoundedChannel`s, by id - 1.
    channels: Vec<Chan>,
    /// The open files behind `File` values, by id - 1; `None` once closed.
    files: Vec<Option<std::fs::File>>,
    /// The values of the program's statics, in declaration order: each
    /// initializer runs before `main`, and its value lives until exit.
    statics: Vec<Value>,
    /// The capacity `reserve` promised each `Vec` allocation, beyond its length.
    caps: rustc_hash::FxHashMap<AllocId, usize>,
    /// `RandomSource`'s xorshift state.
    rand: u64,
    /// The tables a `Vacant` entry was made from that keep key order.
    sorted_tables: rustc_hash::FxHashSet<AllocId>,
    /// The borrow flags currently held, by field address.
    flags: Vec<(Addr, Flag)>,
    /// Resumes of a pending task root.
    resumes: u64,
    /// Drop the root's frame once it has been pending this often.
    cancel_after: Option<u64>,
    /// The borrow flags each suspended coroutine frame holds, by its address.
    parked_flags: Vec<(Addr, Held)>,
    /// `std.tracing`'s minimum level, by rank (trace 0 .. error 4).
    tracing_min: i128,
    /// Set while `dbg` renders its value: the `Debug` form, which quotes
    /// strings and characters and ignores a user `Display`.
    debug_render: bool,
    /// The exporter `Log.set_exporter` registered, and its type; `None`
    /// sends events to the default `StdoutExporter`.
    tracing_sink: Option<(Value, Ty)>,
    /// The ids of the spans `with_span` made active, innermost last.
    active_spans: Vec<i128>,
    /// The tables behind `Interner` values, by handle - 1: each interned
    /// string, its symbol's position, and the place `resolve` lends it
    /// from once asked.
    interners: Vec<Interned>,
    /// The requests `Client.request` started, by `RequestBuilder` handle - 1.
    http_builders: Vec<HttpBuilder>,
    /// Each `Response`'s headers, by the id its value carries after its
    /// fields.
    http_headers: Vec<Vec<(String, String)>>,
}

/// A request `RequestBuilder` assembles: method, url, headers in order,
/// body and timeout in milliseconds (0 for none).
#[derive(Default, Clone)]
struct HttpBuilder {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
    timeout_ms: i128,
}

/// One `Interner`'s strings.
#[derive(Default)]
struct Interned {
    strings: Vec<String>,
    ids: rustc_hash::FxHashMap<String, usize>,
    lent: Vec<Option<AllocId>>,
}

impl<'a> Interp<'a> {
    fn call(&mut self, name: &str, args: Vec<Value>) -> R<Value> {
        let Some(body) = self.program.bodies.get(name) else {
            let unit = self.tys.unit();
            return self.native(name, args, &[], unit);
        };
        if args.len() != body.arg_count {
            return err(format!(
                "{name} takes {} arguments, got {}",
                body.arg_count,
                args.len()
            ));
        }
        if self.frames.len() >= MAX_DEPTH {
            return err("stack overflow");
        }
        // A coroutine's borrow flags outlive the `resume` that suspended
        // holding them: they are parked with its frame until the next
        // resume, or until `drop_frame` gives them back.
        let frame_at = match args.first() {
            Some(Value::Ref(a)) if coroutine::is_resume(name) || coroutine::is_drop_frame(name) => {
                Some(a.clone())
            }
            _ => None,
        };
        let parked = frame_at.as_ref().and_then(|a| {
            let i = self.parked_flags.iter().position(|(at, _)| at == a)?;
            Some(self.parked_flags.swap_remove(i).1)
        });
        let mut locals = vec![Value::Uninit; body.locals.len()];
        for (i, a) in args.into_iter().enumerate() {
            locals[i + 1] = a;
        }
        self.next_frame_id += 1;
        let mut held = Vec::new();
        if coroutine::is_drop_frame(name) {
            for (_, flags) in parked.into_iter().flatten() {
                self.release_flags(flags);
            }
        } else {
            held = parked.unwrap_or_default();
        }
        self.frames.push(Frame {
            id: self.next_frame_id,
            locals,
            held,
        });
        if self.trace {
            self.events.push(Event::Enter(name.to_string()));
        }
        // A deep recursion outgrows any fixed thread stack: the body runs
        // on a fresh segment once this one runs low.
        let result = stacker::maybe_grow(256 * 1024, 16 * 1024 * 1024, || self.run_body(body));
        let frame = self.frames.pop().expect("frame");
        match (&result, frame_at) {
            (Ok(Value::Variant(1, _)), Some(at)) if coroutine::is_resume(name) => {
                if !frame.held.is_empty() {
                    self.parked_flags.push((at, frame.held));
                }
            }
            _ => {
                for (_, flags) in frame.held {
                    self.release_flags(flags);
                }
            }
        }
        if result.is_ok() && self.trace {
            self.events.push(Event::Exit(name.to_string()));
        }
        result
    }

    /// Library functions the interpreter implements directly. A method of
    /// a library type is named after the type instance, `Vec[R].len`.
    /// A library function with no body: `arg_tys` and `ret` are the
    /// argument and result types at the call site, which say what a
    /// generic method's element type is and how its `Option` is laid out.
    fn native(&mut self, name: &str, args: Vec<Value>, arg_tys: &[Ty], ret: Ty) -> R<Value> {
        let (ty_name, method) = split_method(name);
        let base = ty_name.split('[').next().unwrap_or(ty_name);
        match (base, method) {
            // `std.mem`: moves through borrowed places, which MIR itself
            // does not spell (`take` is `replace` with the default).
            // `cpu.supports("avx2")`: the host's CPU features, as the
            // other backends probe them.
            ("Cpu", "supports") => {
                let [n] = args.as_slice() else {
                    return err(format!("{name} takes a feature name"));
                };
                let n = self.string_at(n)?;
                Ok(Value::Bool(crate::interpreter::host_cpu_supports(&n)))
            }
            // The guard of `critical_section.acquire()`: interrupts were
            // never masked, so there is nothing to restore.
            ("CriticalSectionGuard", "drop") => Ok(Value::Unit),
            ("process", "exit") => match args.as_slice() {
                [Value::Int(code)] => Err(Stop::Exit(*code as i32)),
                _ => err(format!("{name} takes an exit code")),
            },
            ("mem", "swap") => {
                let [Value::Ref(a), Value::Ref(b)] = args.as_slice() else {
                    return err(format!("{name} takes two references"));
                };
                let va = self.slot(a)?;
                let vb = std::mem::replace(self.slot_mut(b)?, va);
                *self.slot_mut(a)? = vb;
                Ok(Value::Unit)
            }
            ("mem", "replace") => {
                let [Value::Ref(a), v] = args.as_slice() else {
                    return err(format!("{name} takes a reference and a value"));
                };
                let v = v.clone();
                Ok(std::mem::replace(self.slot_mut(a)?, v))
            }
            ("CStr" | "CString", _) | ("String", "to_cstring") => {
                self.c_string_method(name, base, method, args, ret)
            }
            ("String", "slice") => {
                // `slice(start, end) -> StringSlice`, held as a `String` of
                // its own: the bytes `[start, end)`, saturating as legacy's
                // (a start out of range is empty, `end` clamps).
                let [recv, Value::Int(a), Value::Int(b)] = args.as_slice() else {
                    return err(format!("{name} takes a receiver and two indices"));
                };
                let text = self.string_at(recv)?;
                let len = text.len() as i128;
                let piece = if *a < 0 || *a > len {
                    String::new()
                } else {
                    let end = (*b).clamp(*a, len);
                    String::from_utf8_lossy(&text.as_bytes()[*a as usize..end as usize])
                        .into_owned()
                };
                Ok(self.alloc_box("String", Value::Str(piece)))
            }
            ("F64" | "F32" | "F16" | "Bf16", "from") if args.len() == 1 => {
                // The total-order wrapper around its one float field.
                Ok(Value::Agg(args))
            }
            ("f64" | "f32", "total_cmp") if args.len() == 2 => {
                // The total order the `F64` / `F32` wrappers compare by.
                let mut x = [0f64; 2];
                for (i, a) in args.iter().enumerate() {
                    let v = match a {
                        Value::Ref(at) => self.slot(at)?,
                        v => v.clone(),
                    };
                    let Value::Float(f) = v else {
                        return err(format!("{name}: argument {i} is not a float"));
                    };
                    x[i] = f;
                }
                // Every NaN orders as the positive quiet one, last: its sign
                // is the producer's (x86 division gives a negative NaN), not
                // the program's, as the legacy backends canonicalize it.
                let canon = |f: f64| if f.is_nan() { f64::NAN } else { f };
                let o = if base == "f32" {
                    (canon(x[0]) as f32).total_cmp(&(canon(x[1]) as f32))
                } else {
                    canon(x[0]).total_cmp(&canon(x[1]))
                };
                let want = match o {
                    std::cmp::Ordering::Less => "Less",
                    std::cmp::Ordering::Equal => "Equal",
                    std::cmp::Ordering::Greater => "Greater",
                };
                self.variant_named(ret, None, want, Vec::new())
            }
            ("Env", "var") => {
                let [n] = args.as_slice() else {
                    return err(format!("{name} takes a name"));
                };
                let n = self.string_at(n)?;
                match std::env::var(n) {
                    Ok(v) => {
                        let v = self.alloc_box("String", Value::Str(v));
                        self.variant_named(ret, None, "Ok", vec![v])
                    }
                    Err(_) => {
                        let e = self.variant_named(ret, Some("Err"), "NotPresent", Vec::new())?;
                        self.variant_named(ret, None, "Err", vec![e])
                    }
                }
            }
            ("Env", "set") => {
                let [n, v] = args.as_slice() else {
                    return err(format!("{name} takes a name and a value"));
                };
                let (n, v) = (self.string_at(n)?, self.string_at(v)?);
                std::env::set_var(n, v);
                // Owned `String`s given by value drop here.
                for (a, t) in args.iter().zip(arg_tys) {
                    if matches!(a, Value::Box(_)) {
                        self.drop_value(a.clone(), *t)?;
                    }
                }
                Ok(Value::Unit)
            }
            ("Stdout" | "Stderr", "print" | "println" | "flush") => {
                if let [s] = args.as_slice() {
                    let mut text = self.string_at(s)?;
                    if method == "println" {
                        text.push('\n');
                    }
                    self.write_out(base == "Stderr", &text);
                }
                Ok(Value::Unit)
            }
            ("Stdin", "read_line" | "read_to_string") => {
                use std::io::Read;
                let mut buf = String::new();
                let r = if method == "read_line" {
                    std::io::stdin().read_line(&mut buf).map(|_| ())
                } else {
                    std::io::stdin().read_to_string(&mut buf).map(|_| ())
                };
                match r {
                    Ok(()) => {
                        let s = self.alloc_box("String", Value::Str(buf));
                        self.variant_named(ret, None, "Ok", vec![s])
                    }
                    Err(e) => self.io_err(ret, e),
                }
            }
            ("Clock", "now") => Ok(Value::Int(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i128::from(d.as_secs())),
            )),
            ("RandomSource", "next_u64") => {
                // Xorshift64 from a clock seed, as legacy's.
                let mut x = self.rand;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.rand = x;
                Ok(Value::Int(i128::from(x)))
            }
            ("Env", "args") => {
                // argv[0] only: the MIR interpreter passes no arguments,
                // and legacy counts the program name.
                let prog = self.alloc_box("String", Value::Str("main".into()));
                Ok(self.alloc_box(ty_name, Value::Agg(vec![prog])))
            }
            ("Option", "take" | "replace") if matches!(args.first(), Some(Value::Ref(_))) => {
                // Moves the value out of the borrowed `Option` and leaves
                // `None`, or `Some(value)` for `replace`.
                let Some(Value::Ref(at)) = args.first() else {
                    unreachable!("checked above")
                };
                let at = at.clone();
                let fill = match args.get(1) {
                    Some(v) => self.variant_named(ret, None, "Some", vec![v.clone()])?,
                    None => self.variant_named(ret, None, "None", Vec::new())?,
                };
                let slot = self.slot_mut(&at)?;
                Ok(std::mem::replace(slot, fill))
            }
            ("__yield_now", "") => Ok(Value::Unit),
            ("fence" | "compiler_fence", "") => {
                // One thread runs the program; a fence orders nothing. A
                // `Relaxed` one is refused, as legacy refuses it.
                if base == "fence" && matches!(args.first(), Some(Value::Variant(0, _))) {
                    return err("`fence` takes an ordering stronger than `Relaxed`");
                }
                Ok(Value::Unit)
            }
            ("LazyLock", "new") => match args.into_iter().next() {
                // The initializer, until the first `get` runs it.
                Some(f) => Ok(Value::Agg(vec![f])),
                None => err(format!("{name} takes an initializer")),
            },
            ("LazyLock", "get") => {
                let Some(recv) = args.first() else {
                    return err(format!("{name} needs its receiver"));
                };
                let at = self.cell_struct(recv, name)?;
                let filled = match self.slot(&at)? {
                    Value::Agg(fs) => fs.len() > 1,
                    other => return err(format!("{name} of {other:?}")),
                };
                if !filled {
                    let v = self.call_erased(Value::Ref(at.child(0)), Vec::new())?;
                    if let Value::Agg(fs) = self.slot_mut(&at)? {
                        fs.push(v);
                    }
                }
                Ok(Value::Ref(at.child(1)))
            }
            ("dbg", "") => {
                // `[file:line] text = value` on stderr.
                let [line, text, value] = args.as_slice() else {
                    return err("dbg takes a line, a text and a value");
                };
                let line = match line {
                    Value::Int(l) => *l,
                    _ => 0,
                };
                let text = self.string_at(text)?;
                self.debug_render = true;
                let shown = match arg_tys.get(2) {
                    Some(&t) => self.display_typed(value, t),
                    None => self.display(value),
                };
                self.debug_render = false;
                let shown = shown?;
                let file = SOURCE_NAME.get().map_or("<unknown>", String::as_str);
                self.write_out(true, &format!("[{file}:{line}] {text} = {shown}\n"));
                Ok(Value::Unit)
            }
            ("Client" | "RequestBuilder" | "Response" | "HttpError", _) => {
                self.http_method(name, base, method, args, ret)
            }
            ("Vector", _) => self.vector_method(method, &args, arg_tys, ret),
            ("Secret", "expose" | "expose_mut" | "ct_eq") => {
                // The value is the struct's one field, lent; `ct_eq`
                // compares two such values (the interpreter has no timing
                // to keep constant).
                let Some(recv) = args.first() else {
                    return err(format!("{name} needs its receiver"));
                };
                let at = self.cell_struct(recv, name)?.child(0);
                if method != "ct_eq" {
                    return Ok(Value::Ref(at));
                }
                let Some(other) = args.get(1) else {
                    return err(format!("{name} takes another secret"));
                };
                let other = self.cell_struct(other, name)?.child(0);
                let a = self.key_form(&Value::Ref(at))?;
                let b = self.key_form(&Value::Ref(other))?;
                Ok(Value::Bool(a == b))
            }
            ("Interner", "new") => {
                self.interners.push(Interned::default());
                Ok(Value::Agg(vec![Value::Int(self.interners.len() as i128)]))
            }
            ("Interner", "intern" | "resolve" | "len") => self.interner_method(method, &args),
            ("tracing_active_span", "") => {
                Ok(Value::Int(self.active_spans.last().copied().unwrap_or(0)))
            }
            ("tracing_level_enabled", "") => match args.as_slice() {
                [Value::Int(rank)] => Ok(Value::Bool(*rank >= self.tracing_min)),
                _ => err(format!("{name} takes a rank")),
            },
            ("tracing_set_min_level", "") => {
                if let [Value::Int(rank)] = args.as_slice() {
                    self.tracing_min = *rank;
                }
                Ok(Value::Unit)
            }
            ("tracing_reset", "") => {
                self.tracing_min = 0;
                if let Some((v, t)) = self.tracing_sink.take() {
                    self.drop_value(v, t)?;
                }
                Ok(Value::Unit)
            }
            ("Log", "set_exporter") => {
                let (Some(v), Some(&t)) = (args.into_iter().next(), arg_tys.first()) else {
                    return err(format!("{name} takes an exporter"));
                };
                if let Some((old, ot)) = self.tracing_sink.replace((v, t)) {
                    self.drop_value(old, ot)?;
                }
                Ok(Value::Unit)
            }
            ("tracing_emit_event", "") => {
                let (Some(event), Some(&t)) = (args.into_iter().next(), arg_tys.first()) else {
                    return err(format!("{name} takes an event"));
                };
                self.emit_event(event, t)?;
                Ok(Value::Unit)
            }
            ("with_span", "") => {
                // The body runs with the span's id active; `LogEvent`'s
                // constructors stamp it.
                let mut it = args.into_iter();
                let (Some(span), Some(body)) = (it.next(), it.next()) else {
                    return err(format!("{name} takes a span and a body"));
                };
                let span = match span {
                    Value::Ref(at) => self.slot(&at)?,
                    v => v,
                };
                let id = match &span {
                    Value::Agg(fs) => match fs.get(1) {
                        Some(Value::Int(id)) => *id,
                        _ => 0,
                    },
                    _ => 0,
                };
                self.active_spans.push(id);
                let r = self.call_erased(body, Vec::new());
                self.active_spans.pop();
                r
            }
            ("format", "") => {
                // An f-string used as a value: print's convention, into a
                // new String.
                let text = self.show(&args, arg_tys)?;
                Ok(self.alloc_box("String", Value::Str(text)))
            }
            (_, "to_string") => {
                if args.len() != 1 {
                    return err(format!("{name} takes one argument"));
                }
                let text = self.show(&args, arg_tys)?;
                Ok(self.alloc_box("String", Value::Str(text)))
            }
            ("println" | "print" | "eprintln" | "eprint", "") => {
                // The arguments print one after another (an f-string's
                // parts); a reference prints what it points to.
                let mut text = self.show(&args, arg_tys)?;
                if name.ends_with("ln") {
                    text.push('\n');
                }
                self.write_out(name.starts_with('e'), &text);
                Ok(Value::Unit)
            }
            ("sleep_ms", "") => {
                // `std.time::sleep_ms`, bodyless: a real pause, as legacy's
                // sequential interpreter does; a negative count is a no-op.
                if let [Value::Int(ms)] = args.as_slice() {
                    if *ms > 0 {
                        #[cfg(not(target_arch = "wasm32"))]
                        std::thread::sleep(std::time::Duration::from_millis(*ms as u64));
                    }
                }
                Ok(Value::Unit)
            }
            ("String", "from") => {
                let [Value::Str(s)] = args.as_slice() else {
                    return err("String.from takes a string constant");
                };
                Ok(self.alloc_box(ty_name, Value::Str(s.clone())))
            }
            ("Atomic", _) => self.atomic_method(name, method, args, ret),
            // A `Mutex` holds its value as an `Atomic` does; a `lock` block
            // reaches it through `get_mut` (one task runs at a time here).
            ("Mutex", "new" | "get_mut" | "into_inner") => {
                self.atomic_method(name, method, args, ret)
            }
            // Only the library entry's methods: a user `struct Entry` keeps
            // its own (derived `clone`, ...) through the arms below.
            ("Entry", "and_modify" | "or_insert" | "or_insert_with") => {
                self.entry_method(name, method, args, arg_tys, ret)
            }
            ("OnceLock" | "OnceCell", _) => self.once_method(name, method, args, arg_tys, ret),
            ("Arena", _) => self.arena_method(name, method, args, arg_tys),
            ("Channel" | "Sender" | "Receiver" | "BoundedChannel", _) => {
                self.channel_method(name, base, method, args, arg_tys, ret)
            }
            ("File", _) => self.file_method(name, method, args, arg_tys, ret),
            ("TaskGroup" | "TaskHandle", _) => self.task_method(name, base, method, args, arg_tys),
            ("Vec", "from_array") => {
                let [Value::Agg(elems)] = args.as_slice() else {
                    return err(format!("{name} takes an array"));
                };
                if !elems.iter().all(Value::fully_init) {
                    return err(format!("{name} of a partly uninitialized array"));
                }
                Ok(self.alloc_box(ty_name, Value::Agg(elems.clone())))
            }
            ("Vec", "len") => {
                let [v] = args.as_slice() else {
                    return err(format!("{name} takes one argument"));
                };
                let id = self.box_behind(v)?;
                match &self.live(id)?.value {
                    Value::Agg(fs) => Ok(Value::Int(fs.len() as i128)),
                    other => err(format!("{name} of {other:?}")),
                }
            }
            ("Vec", "index" | "index_mut") => {
                let [v, Value::Int(i)] = args.as_slice() else {
                    return err(format!("{name} takes a reference and an index"));
                };
                let id = self.box_behind(v)?;
                let len = match &self.live(id)?.value {
                    Value::Agg(fs) => fs.len() as i128,
                    other => return err(format!("{name} of {other:?}")),
                };
                if *i < 0 || *i >= len {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![*i as u64],
                }))
            }
            ("Slice" | "Vec", "to_vec") => {
                // A new Vec of clones of the receiver's elements.
                let [view] = args.as_slice() else {
                    return err(format!("{name} takes a receiver"));
                };
                let e = match self.tys.kind(ret) {
                    TyKind::Intrinsic(IntrinsicTy::Vec(e)) => e,
                    _ => return err(format!("{name} into {}", self.tys.display(ret))),
                };
                let (base, lo, len) = self.view_of(view)?;
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let x = self.slot(&base.child(lo + i))?;
                    out.push(self.clone_value(&x, e)?);
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            ("Vec", "reserve" | "reserve_exact" | "shrink_to_fit" | "capacity") => {
                // The capacity a `reserve` asked for, so `capacity()` keeps
                // its promise; a non-positive request changes nothing.
                let Some(v) = args.first() else {
                    return err(format!("{name} needs a receiver"));
                };
                let id = self.box_behind(v)?;
                let len = self.vec_elems(id)?.len();
                let cap = self.caps.get(&id).copied().unwrap_or(0).max(len);
                match (method, &args[1..]) {
                    ("capacity", []) => return Ok(Value::Int(cap as i128)),
                    ("shrink_to_fit", []) => {
                        self.caps.remove(&id);
                    }
                    (_, [Value::Int(n)]) if *n > 0 => {
                        self.caps.insert(id, cap.max(len + *n as usize));
                    }
                    _ => {}
                }
                Ok(Value::Unit)
            }
            ("Vec" | "Slice", "binary_search") => {
                // Rust's binary search over the elements' key forms, as
                // legacy runs it: the index of an equal element, if found.
                let [view, x] = args.as_slice() else {
                    return err(format!("{name} takes a receiver and a value"));
                };
                let (base, lo, len) = self.view_of(view)?;
                let mut x = x.clone();
                while let Value::Ref(a) = &x {
                    x = self.slot(a)?;
                }
                let want = self.key_form(&x)?;
                let mut keys = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let e = self.slot(&base.child(lo + i))?;
                    keys.push(self.key_form(&e)?);
                }
                let found = keys
                    .binary_search_by(|k| key_order(k, &want))
                    .ok()
                    .map(|i| Value::Int(i as i128));
                self.option(ret, found)
            }
            ("String", "reserve" | "reserve_exact") => Ok(Value::Unit),
            ("String", "with_capacity") => Ok(self.alloc_box("String", Value::Str(String::new()))),
            ("Vec", "from_fn") => {
                let (Some(Value::Int(n)), Some(f), Some(&fty)) =
                    (args.first(), args.get(1), arg_tys.get(1))
                else {
                    return err(format!("{name} takes a length and a closure"));
                };
                let mut out = Vec::with_capacity((*n).max(0) as usize);
                if let Value::Erased { .. } = f {
                    // A `Fn(i64) -> T` value: called through a reference to
                    // it, then dropped.
                    let slot = self.alloc(HeapObj {
                        count: 1,
                        weak: 0,
                        value: f.clone(),
                    });
                    let at = Value::Ref(Addr {
                        root: Root::Heap(slot),
                        path: Vec::new(),
                    });
                    for i in 0..*n {
                        out.push(self.call_erased(at.clone(), vec![Value::Int(i)])?);
                    }
                    let f = std::mem::replace(&mut self.live(slot)?.value, Value::Uninit);
                    self.free_slot(slot);
                    self.drop_value(f, fty)?;
                } else {
                    let mut callee = self.hold_callee(f.clone(), fty, name)?;
                    for i in 0..*n {
                        out.push(self.call_callee(&mut callee, vec![Value::Int(i)])?);
                    }
                    self.release_callee(callee, fty)?;
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            ("Vec", "split_off") => {
                let [v, Value::Int(at)] = args.as_slice() else {
                    return err(format!("{name} takes a receiver and an index"));
                };
                let id = self.box_behind(v)?;
                let at = self.bounds(id, *at, true)?;
                let tail = self.vec_elems(id)?.split_off(at);
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(tail)))
            }
            ("Vec", "sorted" | "sorted_by" | "sorted_by_key") => {
                // A sorted copy: an owned receiver sorts in place and comes
                // back; a borrowed one is cloned first.
                let Some(recv) = args.first() else {
                    return err(format!("{name} needs a receiver"));
                };
                let sorted = match recv {
                    Value::Box(_) => recv.clone(),
                    _ => {
                        let id = self.box_behind(recv)?;
                        self.clone_value(&Value::Box(id), ret)?
                    }
                };
                let mut rest = vec![sorted.clone()];
                rest.extend(args[1..].iter().cloned());
                let unit = self.tys.unit();
                let base_name = name.replacen(".sorted", ".sort", 1);
                self.native(&base_name, rest, arg_tys, unit)?;
                Ok(sorted)
            }
            ("Map" | "SortedMap" | "Set" | "SortedSet", "reserve" | "shrink_to_fit") => {
                Ok(Value::Unit)
            }
            ("Slice", m) if m.starts_with("sort") && VEC_MORE_METHODS.contains(&m) => {
                // Sorted through a scratch `Vec` of the view's elements,
                // written back in their new order.
                let Some(view) = args.first() else {
                    return err(format!("{name} needs a receiver"));
                };
                let (base, lo, len) = self.view_of(view)?;
                let mut elems = Vec::with_capacity(len as usize);
                for i in 0..len {
                    elems.push(self.slot(&base.child(lo + i))?);
                }
                let scratch = self.alloc_box("Vec", Value::Agg(elems));
                let mut rest = vec![scratch.clone()];
                rest.extend(args[1..].iter().cloned());
                let unit = self.tys.unit();
                let vec_name = name.replacen("Slice", "Vec", 1);
                self.native(&vec_name, rest, arg_tys, unit)?;
                let Value::Box(sid) = scratch else {
                    unreachable!()
                };
                let sorted = std::mem::take(self.vec_elems(sid)?);
                self.free_slot(sid);
                for (i, v) in sorted.into_iter().enumerate() {
                    *self.slot_mut(&base.child(lo + i as u64))? = v;
                }
                Ok(Value::Unit)
            }
            ("Vec" | "VecDeque" | "String" | "Map" | "SortedMap" | "Set" | "SortedSet", m)
                if m.starts_with("try_") && m != "try_into" && m != "try_from" =>
            {
                // The fallible companions: the interpreter's allocator never
                // fails, so each is its base method, in `Ok`.
                let ok_ty = self.payload_ty(ret, "Ok")?;
                let base_name = name.replacen(".try_", ".", 1);
                let v = self.native(&base_name, args, arg_tys, ok_ty)?;
                self.variant_named(ret, None, "Ok", vec![v])
            }
            ("Slice" | "Array", "fill") => {
                // Each element drops and becomes a clone of the value, which
                // drops once every slot holds its own.
                let [view, v] = args.as_slice() else {
                    return err(format!("{name} takes a value"));
                };
                let e = self.vec_elem_ty(arg_tys, name)?;
                let (base, lo, len) = self.view_of(view)?;
                for i in 0..len {
                    let at = base.child(lo + i);
                    self.drop_at(&at, e)?;
                    let c = self.clone_value(v, e)?;
                    *self.slot_mut(&at)? = c;
                }
                if !matches!(v, Value::Ref(_)) {
                    self.drop_value(v.clone(), e)?;
                }
                Ok(Value::Unit)
            }
            (_, "as_slice_mut") => self.view_method(name, "as_mut_slice", args, ret),
            ("Vec", "get_unchecked") => self.view_method(name, method, args, ret),
            (_, "as_slice" | "as_mut_slice" | "slice" | "slice_mut") | ("Slice" | "Array", _) => {
                self.view_method(name, method, args, ret)
            }
            ("format_spec", "") => {
                // One hole with a spec (`{x:.3}`, `{n:04}`, `{s:>10}`): the
                // value, then the raw spec text after the colon.
                let (Some(Value::Str(spec)), [v]) = (args.last(), &args[..args.len() - 1]) else {
                    return err(format!("{name} takes a value and a spec"));
                };
                let fs = crate::format_spec::FormatSpec::parse(spec)
                    .map_err(|e| Stop::Error(format!("{name}: {e}")))?;
                let mut t = arg_tys.first().copied().unwrap_or(ret);
                let mut v = v.clone();
                while let (TyKind::Ref(inner) | TyKind::MutRef(inner), Value::Ref(at)) =
                    (self.tys.kind(t), &v)
                {
                    v = self.slot(at)?;
                    t = inner;
                }
                let text = match (self.tys.kind(t), &v) {
                    (TyKind::Int(IntTy::U64 | IntTy::Usize), Value::Int(i)) => {
                        fs.apply_uint(*i as u64)
                    }
                    (TyKind::Int(IntTy::I128), Value::Int(i)) => fs.apply_int128(*i),
                    (TyKind::Int(IntTy::U128), Value::Int(i)) => fs.apply_uint128(*i as u128),
                    (_, Value::Int(i)) => fs.apply_int(*i as i64),
                    (_, Value::Float(f)) => fs.apply_float(*f),
                    _ => {
                        let shown = self.display_typed(&v, t)?;
                        fs.apply_str(&shown)
                    }
                };
                Ok(self.alloc_box("String", Value::Str(text)))
            }
            ("String", "from_utf8") => {
                // The bytes by value (a `Vec[u8]`, freed here) or a view.
                let [v] = args.as_slice() else {
                    return err(format!("{name} takes one argument"));
                };
                let held: Vec<Value> = match v {
                    Value::Box(id) => {
                        let fs = std::mem::take(self.vec_elems(*id)?);
                        self.free_slot(*id);
                        if self.trace {
                            self.events.push(Event::Free(*id));
                        }
                        fs
                    }
                    view => {
                        let (base, lo, len) = self.view_of(view)?;
                        let mut fs = Vec::with_capacity(len as usize);
                        for i in 0..len {
                            fs.push(self.slot(&base.child(lo + i))?);
                        }
                        fs
                    }
                };
                let mut bytes = Vec::with_capacity(held.len());
                for b in held {
                    let Value::Int(b) = b else {
                        return err(format!("{name} of a non-byte {b:?}"));
                    };
                    bytes.push(b as u8);
                }
                match String::from_utf8(bytes) {
                    Ok(text) => {
                        let s = self.alloc_box("String", Value::Str(text));
                        self.variant_named(ret, None, "Ok", vec![s])
                    }
                    Err(e) => {
                        // Legacy's reading of `Utf8Error::error_len`: none
                        // means the input ended inside a sequence.
                        let kind = if e.utf8_error().error_len().is_none() {
                            "IncompleteSequence"
                        } else {
                            "InvalidByte"
                        };
                        let payload = self.variant_named(ret, Some("Err"), kind, Vec::new())?;
                        self.variant_named(ret, None, "Err", vec![payload])
                    }
                }
            }
            ("String", "sorted_by") => {
                // The chars, in the comparator's order, as a new String; the
                // comparator sees two chars by reference.
                let (Some(recv), Some(f), Some(&fty)) = (args.first(), args.get(1), arg_tys.get(1))
                else {
                    return err(format!("{name} takes a comparator"));
                };
                let text = self.string_at(recv)?;
                let chars: Vec<Value> = text.chars().map(Value::Char).collect();
                let n = chars.len();
                let scratch = self.alloc(HeapObj {
                    count: 1,
                    weak: 0,
                    value: Value::Agg(chars),
                });
                let at = |i: usize| Addr {
                    root: Root::Heap(scratch),
                    path: vec![i as u64],
                };
                let mut callee = self.hold_callee(f.clone(), fty, name)?;
                let ord_ty = callee.body.return_ty();
                let order = self.merge_order(n, &mut |me, a, b| {
                    let o =
                        me.call_callee(&mut callee, vec![Value::Ref(at(a)), Value::Ref(at(b))])?;
                    me.ordering(&o, ord_ty)
                });
                self.release_callee(callee, fty)?;
                let order = order?;
                let chars = std::mem::take(self.vec_elems(scratch)?);
                self.free_slot(scratch);
                let out: String = order
                    .into_iter()
                    .filter_map(|i| match chars[i] {
                        Value::Char(c) => Some(c),
                        _ => None,
                    })
                    .collect();
                Ok(self.alloc_box("String", Value::Str(out)))
            }
            ("String", _) if STRING_TEXT_METHODS.contains(&method) => {
                self.string_text_method(name, method, args, ret)
            }
            ("char", "try_from") => match args.as_slice() {
                // `Err` holds the codepoint that is not a Unicode scalar.
                [Value::Int(n)] => match u32::try_from(*n).ok().and_then(char::from_u32) {
                    Some(c) => self.variant_named(ret, None, "Ok", vec![Value::Char(c)]),
                    None => self.variant_named(ret, None, "Err", vec![Value::Int(*n)]),
                },
                _ => err(format!("{name} takes an integer")),
            },
            ("char", "len_utf8") => match args.as_slice() {
                [Value::Char(c)] => Ok(Value::Int(c.len_utf8() as i128)),
                _ => err(format!("{name} takes a char")),
            },
            ("char", _) if CHAR_METHODS.contains(&method) => {
                self.char_method(name, method, &args, ret)
            }
            (
                t,
                "is_ascii_digit"
                | "is_ascii_alphabetic"
                | "is_ascii_alphanumeric"
                | "is_ascii_whitespace"
                | "is_ascii_hexdigit",
            ) if int_width(t).is_some() => {
                // A byte's ASCII class.
                let [Value::Int(b)] = args.as_slice() else {
                    return err(format!("{name} takes an integer"));
                };
                let c = u8::try_from(*b).ok().map(char::from);
                self.char_method(
                    name,
                    method,
                    &c.map(Value::Char).into_iter().collect::<Vec<_>>(),
                    ret,
                )
                .or(Ok(Value::Bool(false)))
            }
            (
                t,
                "trailing_zeros" | "leading_zeros" | "count_zeros" | "count_ones" | "abs_diff"
                | "checked_add" | "checked_sub" | "checked_mul" | "saturating_add"
                | "saturating_sub" | "saturating_mul" | "clamp" | "overflowing_add"
                | "overflowing_sub" | "overflowing_mul" | "to_ne_bytes" | "to_le_bytes"
                | "to_be_bytes",
            ) if int_width(t).is_some() => self.int_method(name, t, method, &args, ret),
            (t, "try_from" | "from") if int_width(t).is_some() || t == "char" => {
                self.convert_from(name, t, method, &args, ret)
            }
            (_, m)
                if matches!(args.first(), Some(Value::Float(_)))
                    && crate::numeric_conv::parse_float_to_int(m).is_some() =>
            {
                let Some(Value::Float(f)) = args.first() else {
                    unreachable!()
                };
                let (family, _, bits, signed) =
                    crate::numeric_conv::parse_float_to_int(m).expect("checked by the guard");
                use crate::numeric_conv::ConvOutcome;
                match crate::numeric_conv::convert_float_to_int(*f, family, bits, signed) {
                    ConvOutcome::Value(n) if method.starts_with("checked_to_") => {
                        self.option(ret, Some(Value::Int(n)))
                    }
                    ConvOutcome::Value(n) => Ok(Value::Int(n)),
                    ConvOutcome::None => self.option(ret, None),
                    ConvOutcome::Panic => {
                        if self.trace {
                            self.events.push(Event::Abort(AbortReason::Panic));
                        }
                        Err(Stop::Abort(AbortReason::Panic))
                    }
                }
            }
            ("Vec", "from_slice") => {
                // A new Vec of clones of the slice's elements.
                let [view] = args.as_slice() else {
                    return err(format!("{name} takes a slice"));
                };
                let e = match self.tys.kind(ret) {
                    TyKind::Intrinsic(IntrinsicTy::Vec(e)) => e,
                    _ => return err(format!("{name} into {}", self.tys.display(ret))),
                };
                let (base, lo, len) = self.view_of(view)?;
                let mut out = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let x = self.slot(&base.child(lo + i))?;
                    out.push(self.clone_value(&x, e)?);
                }
                // A Vec passed by value is consumed: its elements were
                // cloned, so it drops here.
                if let (Value::Box(_), Some(&t)) = (view, arg_tys.first()) {
                    if matches!(self.tys.kind(t), TyKind::Intrinsic(IntrinsicTy::Vec(_))) {
                        self.drop_value(view.clone(), t)?;
                    }
                }
                Ok(self.alloc_box(ty_name, Value::Agg(out)))
            }
            ("Vec", _) if VEC_MORE_METHODS.contains(&method) => {
                self.vec_more_method(ty_name, method, args, arg_tys)
            }
            ("Vec" | "String", _) => self.collection_method(ty_name, method, args, arg_tys, ret),
            ("FileSystem", "write" | "read_to_string" | "read_lines") => {
                self.fs_method(method, args, ret)
            }
            (t, "parse" | "from_str_radix") if t == "f64" || int_width(t).is_some() => {
                self.parse_number(t, method, &args, ret)
            }
            ("VecDeque", _) => self.deque_method(ty_name, method, args, arg_tys, ret),
            ("Map" | "Set" | "SortedMap" | "SortedSet", "clone") => {
                let [v] = args.as_slice() else {
                    return err(format!("{name} takes one argument"));
                };
                let t = self.deref_ty(arg_tys.first().copied(), name)?;
                let v = match v {
                    Value::Ref(a) => self.slot(a)?,
                    v => v.clone(),
                };
                self.clone_value(&v, t)
            }
            ("Map" | "Set" | "SortedMap" | "SortedSet", _) => {
                self.table_method(ty_name, method, args, arg_tys, ret)
            }
            (
                _,
                "max" | "min" | "abs" | "pow" | "wrapping_add" | "wrapping_sub" | "wrapping_mul"
                | "count_ones" | "signum" | "sqrt" | "floor" | "ceil" | "round" | "rem_euclid"
                | "div_euclid" | "is_power_of_two" | "clamp",
            ) if args
                .first()
                .is_some_and(|a| matches!(a, Value::Int(_) | Value::Float(_))) =>
            {
                // An integer literal beside a float receiver is that float,
                // as legacy reads it (`f.max(2)`).
                let args = match args.first() {
                    Some(Value::Float(_)) => args
                        .into_iter()
                        .map(|a| match a {
                            Value::Int(n) => Value::Float(n as f64),
                            a => a,
                        })
                        .collect(),
                    _ => args,
                };
                self.scalar_method(name, method, &args, ret)
            }
            (t, "to_f64" | "to_f32") if int_width(t).is_some() => match args.as_slice() {
                [Value::Int(n)] => Ok(narrow_float(Value::Float(*n as f64), self.tys.kind(ret))),
                _ => err(format!("{name} takes an integer")),
            },
            // IEEE-754 reinterpretation, as legacy's: `to_bits` is the f64
            // pattern whatever the float's width, `to_bits32` the pattern of
            // the value rounded to f32.
            (_, "to_bits" | "to_bits32" | "bits_as_f64" | "bits_as_f32") => {
                match (method, args.as_slice()) {
                    ("to_bits", [Value::Float(f)]) => Ok(Value::Int(i128::from(f.to_bits()))),
                    ("to_bits32", [Value::Float(f)]) => {
                        Ok(Value::Int(i128::from((*f as f32).to_bits())))
                    }
                    ("bits_as_f64", [Value::Int(b)]) => Ok(Value::Float(f64::from_bits(*b as u64))),
                    ("bits_as_f32", [Value::Int(b)]) => {
                        Ok(Value::Float(f64::from(f32::from_bits(*b as u32))))
                    }
                    _ => err(format!("{name}: wrong arguments")),
                }
            }
            (_, m)
                if matches!(args.first(), Some(Value::Float(_)))
                    && (float_unary(m, 0.0, true).is_some()
                        || matches!(m, "atan2" | "hypot" | "copysign")) =>
            {
                self.scalar_method(name, method, &args, ret)
            }
            (_, "clone") if args.len() == 1 => {
                // No user `Clone` body was found (it would have been
                // called instead): the structural clone a derive gives.
                let t = self.deref_ty(arg_tys.first().copied(), name)?;
                let v = match &args[0] {
                    Value::Ref(a) => self.slot(a)?,
                    v => v.clone(),
                };
                self.clone_value(&v, t)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// The core `Vec[T]` and `String` methods (`design.md` § Collection
    /// Core Methods) beyond the constructors above.
    fn collection_method(
        &mut self,
        ty_name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        let name = format!("{ty_name}.{method}");
        let is_vec = ty_name.starts_with("Vec");
        match (is_vec, method, args.as_slice()) {
            (true, "with_capacity" | "filled", [Value::Int(n), ..]) if *n < 0 => {
                // A negative length panics, as in legacy.
                if self.trace {
                    self.events.push(Event::Abort(AbortReason::Panic));
                }
                Err(Stop::Abort(AbortReason::Panic))
            }
            (true, "new" | "with_capacity", _) => {
                let v = self.alloc_box(ty_name, Value::Agg(vec![]));
                if let (Value::Box(id), [Value::Int(n), ..]) = (&v, args.as_slice()) {
                    self.caps.insert(*id, *n as usize);
                }
                Ok(v)
            }
            (false, "new", []) => Ok(self.alloc_box(ty_name, Value::Str(String::new()))),
            (true, "push", [v, val]) => {
                let id = self.box_behind(v)?;
                self.vec_elems(id)?.push(val.clone());
                Ok(Value::Unit)
            }
            (true, "pop", [v]) => {
                let id = self.box_behind(v)?;
                let last = self.vec_elems(id)?.pop();
                self.option(ret, last)
            }
            (true, "insert", [v, Value::Int(i), val]) => {
                let id = self.box_behind(v)?;
                let i = self.bounds(id, *i, true)?;
                self.vec_elems(id)?.insert(i, val.clone());
                Ok(Value::Unit)
            }
            (true, "remove", [v, Value::Int(i)]) => {
                let id = self.box_behind(v)?;
                let i = self.bounds(id, *i, false)?;
                Ok(self.vec_elems(id)?.remove(i))
            }
            (true, "is_empty", [v]) => {
                let id = self.box_behind(v)?;
                Ok(Value::Bool(self.vec_elems(id)?.is_empty()))
            }
            (true, "filled", [Value::Int(n), val]) => {
                let e = match self.tys.kind(ret) {
                    TyKind::Intrinsic(IntrinsicTy::Vec(e)) => e,
                    _ => return err(format!("{name} into {}", self.tys.display(ret))),
                };
                // `val` moves into the first slot and is cloned into the rest.
                let n = *n as usize;
                let mut elems = Vec::with_capacity(n);
                for i in 0..n {
                    if i + 1 == n {
                        elems.push(val.clone());
                    } else {
                        elems.push(self.clone_value(val, e)?);
                    }
                }
                if n == 0 {
                    self.drop_value(val.clone(), e)?;
                }
                Ok(self.alloc_box(ty_name, Value::Agg(elems)))
            }
            (true, "clone", [v]) => {
                let id = self.box_behind(v)?;
                let e = self.vec_elem_ty(arg_tys, &name)?;
                let elems = self.vec_elems(id)?.clone();
                let mut out = Vec::with_capacity(elems.len());
                for x in &elems {
                    out.push(self.clone_value(x, e)?);
                }
                Ok(self.alloc_box(ty_name, Value::Agg(out)))
            }
            (true, "contains", [v, needle]) => {
                let id = self.box_behind(v)?;
                let e = self.vec_elem_ty(arg_tys, &name)?;
                let user_eq = self.user_eq(e);
                let want = self.key_form(needle)?;
                let elems = self.vec_elems(id)?.clone();
                for x in &elems {
                    let equal = match &user_eq {
                        Some(eq) => self.call_eq(eq, x, needle)?,
                        None => self.key_form(x)? == want,
                    };
                    if equal {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            (true, "truncate", [v, Value::Int(n)]) => {
                let id = self.box_behind(v)?;
                let e = self.vec_elem_ty(arg_tys, &name)?;
                let len = self.vec_elems(id)?.len();
                let keep = (*n).clamp(0, len as i128) as usize;
                // The tail drops first to last, as a dying Vec does.
                for i in keep..len {
                    let at = Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    };
                    self.drop_at(&at, e)?;
                }
                self.vec_elems(id)?.truncate(keep);
                Ok(Value::Unit)
            }
            (true, "first", [v]) => {
                let id = self.box_behind(v)?;
                let n = self.vec_elems(id)?.len();
                let elem = (n > 0).then(|| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![0],
                    })
                });
                self.option_of_place(ret, elem)
            }
            (true, "last", [v]) => {
                let id = self.box_behind(v)?;
                let n = self.vec_elems(id)?.len();
                let elem = (n > 0).then(|| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![n as u64 - 1],
                    })
                });
                self.option_of_place(ret, elem)
            }
            (true, "get" | "last", [v, Value::Int(i)]) => {
                let id = self.box_behind(v)?;
                let n = self.vec_elems(id)?.len() as i128;
                let i = if method == "last" { n - 1 - i } else { *i };
                let elem = (0..n).contains(&i).then(|| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    })
                });
                self.option_of_place(ret, elem)
            }
            (true, "swap", [v, Value::Int(i), Value::Int(j)]) => {
                let id = self.box_behind(v)?;
                let (i, j) = (self.bounds(id, *i, false)?, self.bounds(id, *j, false)?);
                self.vec_elems(id)?.swap(i, j);
                Ok(Value::Unit)
            }
            (true, "reverse", [v]) => {
                let id = self.box_behind(v)?;
                self.vec_elems(id)?.reverse();
                Ok(Value::Unit)
            }
            (true, "clear", [v]) => {
                let id = self.box_behind(v)?;
                let e = self.vec_elem_ty(arg_tys, &name)?;
                let n = self.vec_elems(id)?.len();
                // First to last, as when the whole Vec drops.
                for i in 0..n {
                    let at = Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    };
                    self.drop_at(&at, e)?;
                }
                self.vec_elems(id)?.clear();
                Ok(Value::Unit)
            }
            (true, "index_set", [v, Value::Int(i), val]) => {
                let id = self.box_behind(v)?;
                let e = self.vec_elem_ty(arg_tys, &name)?;
                let i = self.bounds(id, *i, false)?;
                let at = Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64],
                };
                // The new value is evaluated already; the old one drops
                // before the store (core-semantics D4).
                self.drop_at(&at, e)?;
                *self.slot_mut(&at)? = val.clone();
                Ok(Value::Unit)
            }
            (false, "len", [s]) => Ok(Value::Int(self.string_at(s)?.len() as i128)),
            (false, "is_empty", [s]) => Ok(Value::Bool(self.string_at(s)?.is_empty())),
            (false, "clone", [s]) => {
                let text = self.string_at(s)?;
                Ok(self.alloc_box(ty_name, Value::Str(text)))
            }
            (false, "add", [a, b]) => {
                let text = self.string_at(a)? + &self.string_at(b)?;
                Ok(self.alloc_box(ty_name, Value::Str(text)))
            }
            (false, "eq", [a, b]) => Ok(Value::Bool(self.string_at(a)? == self.string_at(b)?)),
            (false, "push_str" | "push" | "clear", [s, rest @ ..]) => {
                let tail = match rest {
                    [Value::Char(c)] => c.to_string(),
                    [other] => self.string_at(other)?,
                    _ => String::new(),
                };
                let id = self.box_behind(s)?;
                match &mut self.live(id)?.value {
                    Value::Str(text) if method == "clear" => text.clear(),
                    Value::Str(text) => text.push_str(&tail),
                    other => return err(format!("{name} of {other:?}")),
                }
                Ok(Value::Unit)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `Vec` methods that order, combine or filter the elements, some
    /// through a closure argument (`design.md` § Collection Core Methods).
    fn vec_more_method(
        &mut self,
        ty_name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
    ) -> R<Value> {
        let name = format!("{ty_name}.{method}");
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        let id = self.box_behind(recv)?;
        let e = self.vec_elem_ty(arg_tys, &name)?;
        let elem = |i: usize| Addr {
            root: Root::Heap(id),
            path: vec![i as u64],
        };
        let closure_ty = || {
            arg_tys
                .get(1)
                .copied()
                .ok_or_else(|| Stop::Error(format!("{name} needs its closure's type")))
        };
        match (method, &args[1..]) {
            ("sort" | "sort_unstable", []) => {
                let elems = self.vec_elems(id)?.clone();
                let mut keys = Vec::with_capacity(elems.len());
                for x in &elems {
                    keys.push(self.key_form(x)?);
                }
                let order =
                    self.merge_order(elems.len(), &mut |_, a, b| Ok(cmp_key(&keys[a], &keys[b])))?;
                self.permute(id, &order)?;
                Ok(Value::Unit)
            }
            ("is_sorted", []) => {
                let elems = self.vec_elems(id)?.clone();
                let mut prev: Option<Value> = None;
                for x in &elems {
                    let k = self.key_form(x)?;
                    if prev.as_ref().is_some_and(|p| cmp_key(p, &k).is_gt()) {
                        return Ok(Value::Bool(false));
                    }
                    prev = Some(k);
                }
                Ok(Value::Bool(true))
            }
            ("sort_by" | "sort_unstable_by", [f]) => {
                // The comparator sees two elements in place, by reference.
                let fty = closure_ty()?;
                let n = self.vec_elems(id)?.len();
                let mut f = self.hold_callee(f.clone(), fty, &name)?;
                let ord_ty = f.body.return_ty();
                let order = self.merge_order(n, &mut |me, a, b| {
                    let args = vec![Value::Ref(elem(a)), Value::Ref(elem(b))];
                    let o = me.call_callee(&mut f, args)?;
                    me.ordering(&o, ord_ty)
                });
                let order = order?;
                self.release_callee(f, fty)?;
                self.permute(id, &order)?;
                Ok(Value::Unit)
            }
            ("sort_by_key" | "sort_unstable_by_key" | "sort_by_cached_key", [f]) => {
                // One key per element, in order; the keys drop after the sort.
                let fty = closure_ty()?;
                let n = self.vec_elems(id)?.len();
                let mut f = self.hold_callee(f.clone(), fty, &name)?;
                let key_ty = f.body.return_ty();
                let mut owned = Vec::with_capacity(n);
                let mut keys = Vec::with_capacity(n);
                for i in 0..n {
                    let k = self.call_callee(&mut f, vec![Value::Ref(elem(i))])?;
                    keys.push(self.key_form(&k)?);
                    owned.push(k);
                }
                self.release_callee(f, fty)?;
                let order = self.merge_order(n, &mut |_, a, b| Ok(cmp_key(&keys[a], &keys[b])))?;
                self.permute(id, &order)?;
                for k in owned {
                    self.drop_value(k, key_ty)?;
                }
                Ok(Value::Unit)
            }
            ("retain", [f]) => {
                // Each element the predicate rejects drops right away, in
                // order, as Rust's `retain` does.
                let fty = closure_ty()?;
                let n = self.vec_elems(id)?.len();
                let mut f = self.hold_callee(f.clone(), fty, &name)?;
                let mut keep = Vec::with_capacity(n);
                for i in 0..n {
                    let k = self.call_callee(&mut f, vec![Value::Ref(elem(i))])?;
                    let Value::Bool(k) = k else {
                        return err(format!("{name}: the predicate returned {k:?}"));
                    };
                    if !k {
                        self.drop_at(&elem(i), e)?;
                    }
                    keep.push(k);
                }
                self.release_callee(f, fty)?;
                let elems = std::mem::take(self.vec_elems(id)?);
                *self.vec_elems(id)? = elems
                    .into_iter()
                    .zip(keep)
                    .filter_map(|(x, k)| k.then_some(x))
                    .collect();
                Ok(Value::Unit)
            }
            ("dedup", []) => {
                // A run of equal elements keeps its first; the rest drop in
                // order.
                let elems = std::mem::take(self.vec_elems(id)?);
                let mut kept: Vec<Value> = Vec::with_capacity(elems.len());
                let mut last: Option<Value> = None;
                for x in elems {
                    let k = self.key_form(&x)?;
                    if last.as_ref() == Some(&k) {
                        self.drop_value(x, e)?;
                    } else {
                        kept.push(x);
                        last = Some(k);
                    }
                }
                *self.vec_elems(id)? = kept;
                Ok(Value::Unit)
            }
            ("join", [sep]) => {
                let sep = self.string_at(sep)?;
                let elems = self.vec_elems(id)?.clone();
                let mut parts = Vec::with_capacity(elems.len());
                for x in &elems {
                    parts.push(self.string_at(x)?);
                }
                Ok(self.alloc_box("String", Value::Str(parts.join(&sep))))
            }
            ("extend_from_slice", [other]) => {
                let (base, lo, len) = self.view_of(other)?;
                let mut copies = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let x = self.slot(&base.child(lo + i))?;
                    copies.push(self.clone_value(&x, e)?);
                }
                self.vec_elems(id)?.extend(copies);
                Ok(Value::Unit)
            }
            ("extend" | "append", [other]) => {
                // The other `Vec` is drained. Passed by value (`extend`
                // always, and `append`, whose parameter the typechecker
                // makes owned) it is consumed and freed; an `append` through
                // a borrow leaves it empty.
                let from = self.box_behind(other)?;
                let moved = std::mem::take(self.vec_elems(from)?);
                self.vec_elems(id)?.extend(moved);
                if method == "extend" && !matches!(other, Value::Box(_)) {
                    return err(format!("{name} takes the other Vec by value"));
                }
                if matches!(other, Value::Box(_)) {
                    self.free_slot(from);
                    if self.trace {
                        self.events.push(Event::Free(from));
                    }
                }
                Ok(Value::Unit)
            }
            ("swap_remove", [Value::Int(i)]) => {
                let i = self.bounds(id, *i, false)?;
                Ok(self.vec_elems(id)?.swap_remove(i))
            }
            ("resize" | "fill", rest) => {
                // The value is cloned into every new (or, for `fill`,
                // every) slot but the last, which takes it; an unused value
                // drops. Overwritten and cut-off elements drop in order.
                let (want, val) = match (method, rest) {
                    ("resize", [Value::Int(n), val]) => ((*n).max(0) as usize, val),
                    ("fill", [val]) => (self.vec_elems(id)?.len(), val),
                    _ => return err(format!("{name}: wrong arguments")),
                };
                let len = self.vec_elems(id)?.len();
                let from = if method == "fill" { 0 } else { len.min(want) };
                for i in from..len {
                    self.drop_at(&elem(i), e)?;
                }
                self.vec_elems(id)?.truncate(from);
                if from == want {
                    self.drop_value(val.clone(), e)?;
                }
                for i in from..want {
                    let v = if i + 1 == want {
                        val.clone()
                    } else {
                        self.clone_value(val, e)?
                    };
                    self.vec_elems(id)?.push(v);
                }
                Ok(Value::Unit)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// A stable merge sort of `0..n` under `cmp`, which may call into the
    /// program; the result lists the old index of each new position.
    fn merge_order(
        &mut self,
        n: usize,
        cmp: &mut dyn FnMut(&mut Self, usize, usize) -> R<std::cmp::Ordering>,
    ) -> R<Vec<usize>> {
        let mut order: Vec<usize> = (0..n).collect();
        let mut width = 1;
        while width < n {
            let mut out = Vec::with_capacity(n);
            for lo in (0..n).step_by(2 * width) {
                let mid = (lo + width).min(n);
                let hi = (lo + 2 * width).min(n);
                let (mut i, mut j) = (lo, mid);
                while i < mid && j < hi {
                    // The right element goes first only when strictly less.
                    if cmp(self, order[j], order[i])?.is_lt() {
                        out.push(order[j]);
                        j += 1;
                    } else {
                        out.push(order[i]);
                        i += 1;
                    }
                }
                out.extend_from_slice(&order[i..mid]);
                out.extend_from_slice(&order[j..hi]);
            }
            order = out;
            width *= 2;
        }
        Ok(order)
    }

    /// Reorders the elements of `id`: new position `k` takes old `order[k]`.
    fn permute(&mut self, id: AllocId, order: &[usize]) -> R<()> {
        let elems = std::mem::take(self.vec_elems(id)?);
        let mut slots: Vec<Option<Value>> = elems.into_iter().map(Some).collect();
        let sorted = order
            .iter()
            .map(|&i| slots[i].take().expect("a permutation"))
            .collect();
        *self.vec_elems(id)? = sorted;
        Ok(())
    }

    /// The `std::cmp::Ordering` an `Ordering` value of type `ty` names.
    fn ordering(&self, v: &Value, ty: Ty) -> R<std::cmp::Ordering> {
        let (Value::Variant(k, _), TyKind::Adt(a)) = (v, self.tys.kind(ty)) else {
            return err(format!("expected an Ordering, found {v:?}"));
        };
        match self
            .tys
            .adt(a)
            .variants
            .get(*k as usize)
            .map(|v| v.name.as_str())
        {
            Some("Less") => Ok(std::cmp::Ordering::Less),
            Some("Equal") => Ok(std::cmp::Ordering::Equal),
            Some("Greater") => Ok(std::cmp::Ordering::Greater),
            _ => err(format!("expected an Ordering, found {v:?}")),
        }
    }

    /// Takes a closure (or function item) a library method will call. A
    /// closure's environment moves into a scratch heap slot, so each call
    /// can pass it the way its body takes it.
    fn hold_callee(&mut self, f: Value, fty: Ty, name: &str) -> R<Callee<'a>> {
        let program: &'a Program = self.program;
        match (f, self.tys.kind(fty)) {
            (Value::Fn(inst), _) => {
                let body = program
                    .bodies
                    .get(&inst.name)
                    .ok_or_else(|| Stop::Error(format!("{name}: no body for {}", inst.name)))?;
                Ok(Callee {
                    body,
                    env: None,
                    by_value: false,
                    taken: false,
                })
            }
            (v, TyKind::Closure(def, _)) => {
                let body = program
                    .bodies
                    .values()
                    .find(|b| b.instance.def == def)
                    .ok_or_else(|| Stop::Error(format!("{name}: no body for closure {def:?}")))?;
                let env_ty = body
                    .args()
                    .next()
                    .map(|l| body.local(l).ty)
                    .ok_or_else(|| Stop::Error(format!("{name}: a closure body with no env")))?;
                let by_value = !matches!(self.tys.kind(env_ty), TyKind::Ref(_) | TyKind::MutRef(_));
                let slot = self.alloc(HeapObj {
                    count: 1,
                    weak: 0,
                    value: v,
                });
                Ok(Callee {
                    body,
                    env: Some(slot),
                    by_value,
                    taken: false,
                })
            }
            (v, _) => err(format!("{name}: cannot call {v:?}")),
        }
    }

    fn call_callee(&mut self, f: &mut Callee<'a>, args: Vec<Value>) -> R<Value> {
        let mut all = Vec::with_capacity(args.len() + 1);
        if let Some(slot) = f.env {
            if f.by_value {
                if f.taken {
                    return err(format!(
                        "{} takes its environment by value and was already called",
                        f.body.instance.name
                    ));
                }
                f.taken = true;
                all.push(std::mem::replace(
                    &mut self.live(slot)?.value,
                    Value::Uninit,
                ));
            } else {
                all.push(Value::Ref(Addr {
                    root: Root::Heap(slot),
                    path: Vec::new(),
                }));
            }
        }
        // A closure that names a `Copy` parameter's type (`|a: i64, b: i64|`
        // for `sort_by`'s `Fn(ref T, ref T)`) takes the value, not the
        // reference the library hands it.
        let skip = all.len();
        for (i, a) in args.into_iter().enumerate() {
            let pt = f.body.locals.get(skip + i + 1).map(|l| l.ty);
            let a = match (a, pt) {
                (Value::Ref(at), Some(t))
                    if !matches!(self.tys.kind(t), TyKind::Ref(_) | TyKind::MutRef(_))
                        && !self.needs_drop(t) =>
                {
                    self.slot(&at)?
                }
                (a, _) => a,
            };
            all.push(a);
        }
        self.call(&f.body.instance.name, all)
    }

    /// Writes a call's result to its destination and goes on to `target`.
    fn finish_call(
        &mut self,
        body: &Body,
        destination: &Place,
        target: &Option<BasicBlock>,
        ret: Value,
        what: &str,
    ) -> R<Option<BasicBlock>> {
        let Some(target) = target else {
            return err(format!(
                "{what} returned, but the call site says it never does"
            ));
        };
        let (addr, ty) = self
            .resolve(body, destination, Mode::Write)?
            .expect("write mode never probes");
        let old = self.slot(&addr)?;
        if self.owns_drop(&old, ty) {
            return err("call result overwrites a place that still owns a value");
        }
        *self.slot_mut(&addr)? = ret;
        if self.trace {
            self.events
                .push(Event::Init(self.place_str(body, destination)));
        }
        Ok(Some(*target))
    }

    /// The `Erase` cast: a function item, a closure (whose environment
    /// moves to a heap slot the erased value owns), or an erased value
    /// widened to another kind, which is the same value.
    fn erase(&mut self, x: Value, from: Ty) -> R<Value> {
        match (x, self.tys.kind(from).clone()) {
            (Value::Fn(inst), _) => Ok(Value::Erased {
                body: inst.name,
                env: None,
            }),
            (v @ Value::Erased { .. }, _) => Ok(v),
            (v, TyKind::Closure(def, _)) => {
                let program: &'a Program = self.program;
                let body = program
                    .bodies
                    .values()
                    .find(|b| b.instance.def == def)
                    .ok_or_else(|| Stop::Error(format!("Erase: no body for closure {def:?}")))?;
                let slot = self.alloc(HeapObj {
                    count: 1,
                    weak: 0,
                    value: v,
                });
                Ok(Value::Erased {
                    body: body.instance.name.clone(),
                    env: Some((slot, from)),
                })
            }
            (v, _) => err(format!(
                "cannot erase {v:?} of type {}",
                self.tys.display(from)
            )),
        }
    }

    /// A call through an erased value: by `ref` or `mut ref` (the value
    /// stays), or by value (an `OnceFn`, consumed by the call). The
    /// closure body takes its environment the way its own first parameter
    /// says; an environment it only borrowed is dropped once a by-value
    /// call returns.
    fn call_erased(&mut self, f: Value, args: Vec<Value>) -> R<Value> {
        let (f, by_value) = match f {
            Value::Ref(a) => (self.slot(&a)?, false),
            v => (v, true),
        };
        let Value::Erased { body: name, env } = f else {
            return err(format!("call through {f:?}, which is not a function value"));
        };
        let Some((slot, env_ty)) = env else {
            return self.call(&name, args);
        };
        let program: &'a Program = self.program;
        let body = program
            .bodies
            .get(&name)
            .ok_or_else(|| Stop::Error(format!("no body for {name}")))?;
        let takes_env = body
            .args()
            .next()
            .map(|l| {
                !matches!(
                    self.tys.kind(body.local(l).ty),
                    TyKind::Ref(_) | TyKind::MutRef(_)
                )
            })
            .ok_or_else(|| Stop::Error(format!("{name}: a closure body with no env")))?;
        let at = Addr {
            root: Root::Heap(slot),
            path: Vec::new(),
        };
        let mut all = Vec::with_capacity(args.len() + 1);
        if takes_env {
            if !by_value {
                return err(format!(
                    "{name} takes its environment by value and is called through a reference"
                ));
            }
            all.push(std::mem::replace(
                &mut self.live(slot)?.value,
                Value::Uninit,
            ));
            self.free_slot(slot);
        } else {
            all.push(Value::Ref(at.clone()));
        }
        all.extend(args);
        let ret = self.call(&name, all)?;
        if by_value && !takes_env {
            self.drop_at(&at, env_ty)?;
            self.free_slot(slot);
        }
        Ok(ret)
    }

    /// Drops what is left of a held closure's environment.
    fn release_callee(&mut self, f: Callee<'a>, fty: Ty) -> R<()> {
        if let Some(slot) = f.env {
            if !f.taken {
                let at = Addr {
                    root: Root::Heap(slot),
                    path: Vec::new(),
                };
                self.drop_at(&at, fty)?;
            }
            self.free_slot(slot);
        }
        Ok(())
    }

    /// Slice views over a `Vec`, an array or another slice: `as_slice`,
    /// `as_mut_slice`, `slice(lo, hi)` and `slice_mut(lo, hi)`, plus
    /// `len`, `index` and `index_mut` on a slice (or an array). A view
    /// borrows its elements; nothing is copied or dropped.
    /// `Atomic[T]` (design.md § Atomics). The value is the struct with its
    /// one cell, `Agg([v])`; every operation after `new` takes the atomic
    /// by shared reference (core §6.3) and writes the cell through it. The
    /// interpreter runs one task at a time, so each operation is atomic by
    /// construction and the `MemoryOrdering` arguments are not read.
    fn atomic_method(&mut self, name: &str, method: &str, args: Vec<Value>, ret: Ty) -> R<Value> {
        if method == "new" {
            let [v] = args.as_slice() else {
                return err(format!("{name} takes one value"));
            };
            return Ok(Value::Agg(vec![v.clone()]));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs its receiver"));
        };
        let cell = self.atomic_cell(recv, name)?;
        let old = self.slot(&cell)?;
        let operand = |i: usize| -> R<Value> {
            args.get(i)
                .cloned()
                .ok_or_else(|| Stop::Error(format!("{name}: missing argument {i}")))
        };
        let new = match method {
            "load" => return Ok(old),
            "get_mut" => return Ok(Value::Ref(cell)),
            "into_inner" => return Ok(old),
            "store" => {
                *self.slot_mut(&cell)? = operand(1)?;
                return Ok(Value::Unit);
            }
            "swap" => operand(1)?,
            "compare_exchange" | "compare_exchange_weak" => {
                let (expected, new) = (operand(1)?, operand(2)?);
                if old == expected {
                    *self.slot_mut(&cell)? = new;
                    return self.variant_named(ret, None, "Ok", vec![old]);
                }
                return self.variant_named(ret, None, "Err", vec![old]);
            }
            _ => {
                let rhs = operand(1)?;
                match (&old, &rhs) {
                    (Value::Int(a), Value::Int(b)) => {
                        let TyKind::Int(it) = self.tys.kind(ret) else {
                            return err(format!("{name} returns {}", self.tys.display(ret)));
                        };
                        let (a, b) = (*a, *b);
                        Value::Int(wrap(
                            match method {
                                "fetch_add" => a.wrapping_add(b),
                                "fetch_sub" => a.wrapping_sub(b),
                                "fetch_and" => a & b,
                                "fetch_or" => a | b,
                                "fetch_xor" => a ^ b,
                                "fetch_nand" => !(a & b),
                                "fetch_max" => a.max(b),
                                "fetch_min" => a.min(b),
                                _ => return err(format!("call of unknown function {name}")),
                            },
                            it,
                        ))
                    }
                    (Value::Bool(a), Value::Bool(b)) => Value::Bool(match method {
                        "fetch_and" => a & b,
                        "fetch_or" => a | b,
                        "fetch_xor" => a ^ b,
                        "fetch_nand" => !(a & b),
                        _ => return err(format!("call of unknown function {name}")),
                    }),
                    _ => return err(format!("{name} of {old:?} and {rhs:?}")),
                }
            }
        };
        *self.slot_mut(&cell)? = new;
        Ok(old)
    }

    /// `OnceLock[T]` and `OnceCell[T]`: the struct holds its value, once
    /// set, as its one part (`Agg([])` empty, `Agg([v])` set).
    fn once_method(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        if method == "new" {
            return Ok(Value::Agg(Vec::new()));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs its receiver"));
        };
        let at = self.cell_struct(recv, name)?;
        let set = match self.slot(&at)? {
            Value::Agg(fs) => !fs.is_empty(),
            other => return err(format!("{name} of {other:?}")),
        };
        match (method, &args[1..]) {
            ("is_set", []) => Ok(Value::Bool(set)),
            ("get", []) => {
                let v = set.then(|| Value::Ref(at.child(0)));
                self.option(ret, v)
            }
            ("set", [v]) if !set => {
                *self.slot_mut(&at)? = Value::Agg(vec![v.clone()]);
                self.variant_named(ret, None, "Ok", vec![Value::Unit])
            }
            ("set", [v]) => {
                // The rejected value goes back in `AlreadySetError`.
                let rejected = Value::Agg(vec![v.clone()]);
                self.variant_named(ret, None, "Err", vec![rejected])
            }
            ("get_or_init", [f]) => {
                if !set {
                    let fty = arg_tys
                        .get(1)
                        .copied()
                        .ok_or_else(|| Stop::Error(format!("{name} needs its closure's type")))?;
                    let mut callee = self.hold_callee(f.clone(), fty, name)?;
                    let v = self.call_callee(&mut callee, Vec::new());
                    self.release_callee(callee, fty)?;
                    *self.slot_mut(&at)? = Value::Agg(vec![v?]);
                }
                Ok(Value::Ref(at.child(0)))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `Arena[T]`: the struct holds the arena's id, then its items in push
    /// order (`Agg([id, item0, item1, ...])`). A handle is the arena's id
    /// and the item's index; a checkpoint the id and the length.
    fn arena_method(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
    ) -> R<Value> {
        if method == "new" {
            self.arenas += 1;
            return Ok(Value::Agg(vec![Value::Int(self.arenas)]));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs its receiver"));
        };
        let at = self.cell_struct(recv, name)?;
        let (id, len) = match self.slot(&at)? {
            Value::Agg(fs) => match fs.first() {
                Some(Value::Int(id)) => (*id, fs.len() as i128 - 1),
                _ => return err(format!("{name} of an arena with no id")),
            },
            other => return err(format!("{name} of {other:?}")),
        };
        // A handle or checkpoint argument, by value or by reference: its
        // arena's id and its index or mark.
        let pair = |me: &mut Self, v: &Value| -> R<(i128, i128)> {
            let mut v = v.clone();
            while let Value::Ref(a) = &v {
                v = me.slot(a)?;
            }
            match v {
                Value::Agg(fs) => match fs.as_slice() {
                    [Value::Int(a), Value::Int(b)] => Ok((*a, *b)),
                    _ => err(format!("{name}: a malformed handle")),
                },
                other => err(format!("{name}: a handle, found {other:?}")),
            }
        };
        match (method, &args[1..]) {
            ("len", []) => Ok(Value::Int(len)),
            ("push", [v]) => {
                let Value::Agg(fs) = self.slot_mut(&at)? else {
                    unreachable!("checked above");
                };
                fs.push(v.clone());
                Ok(Value::Agg(vec![Value::Int(id), Value::Int(len)]))
            }
            ("get", [r]) => {
                let (owner, index) = pair(self, r)?;
                if owner != id || index < 0 || index >= len {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                Ok(Value::Ref(at.child(index as u64 + 1)))
            }
            ("high_water_mark", []) => Ok(Value::Agg(vec![Value::Int(id), Value::Int(len)])),
            ("rewind_to", [cp]) => {
                // A checkpoint from another arena is ignored; the items past
                // the mark drop, last pushed first.
                let (owner, mark) = pair(self, cp)?;
                if owner == id && (0..len).contains(&mark) {
                    let arena = self.deref_ty(arg_tys.first().copied(), name)?;
                    let held = self
                        .tys
                        .tcx()
                        .adt_of(arena)
                        .and_then(|(_, a)| a.first().copied())
                        .ok_or_else(|| Stop::Error(format!("{name} needs its item type")))?;
                    for i in (mark..len).rev() {
                        self.drop_at(&at.child(i as u64 + 1), held)?;
                    }
                    let Value::Agg(fs) = self.slot_mut(&at)? else {
                        unreachable!("checked above");
                    };
                    fs.truncate(mark as usize + 1);
                }
                Ok(Value::Unit)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `Entry[K, V]`'s methods, on the `Occupied` / `Vacant` value
    /// `Map.entry` makes; each takes the entry by value.
    fn entry_method(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        let Some(Value::Variant(k, fields)) = args.first() else {
            return err(format!("{name} needs an entry"));
        };
        let entry_ty = *arg_tys
            .first()
            .ok_or_else(|| Stop::Error(format!("{name} needs the entry's type")))?;
        let (adt, _) = self
            .tys
            .tcx()
            .adt_of(entry_ty)
            .ok_or_else(|| Stop::Error(format!("{name} of a non-entry")))?;
        let occupied = adt.variants.get(*k as usize).map(|v| v.name.as_str()) == Some("Occupied");
        let call = |me: &mut Self, f: &Value, fty: Ty, args: Vec<Value>| -> R<Value> {
            let mut callee = me.hold_callee(f.clone(), fty, name)?;
            let v = me.call_callee(&mut callee, args);
            me.release_callee(callee, fty)?;
            v
        };
        match (method, &args[1..], fields.as_slice()) {
            ("and_modify", [f], _) => {
                let fty = arg_tys.get(1).copied().unwrap_or(ret);
                if let (true, [at]) = (occupied, fields.as_slice()) {
                    call(self, f, fty, vec![at.clone()])?;
                } else {
                    // An unused closure still drops.
                    self.drop_value(f.clone(), fty)?;
                }
                Ok(args[0].clone())
            }
            ("or_insert" | "or_insert_with", rest, [at]) if occupied => {
                if let (Some(v), Some(&t)) = (rest.first(), arg_tys.get(1)) {
                    self.drop_value(v.clone(), t)?;
                }
                Ok(at.clone())
            }
            ("or_insert" | "or_insert_with", rest, [key, map]) => {
                let v = match (method, rest) {
                    ("or_insert", [v]) => v.clone(),
                    ("or_insert_with", [f]) => {
                        let fty = arg_tys.get(1).copied().unwrap_or(ret);
                        call(self, f, fty, Vec::new())?
                    }
                    _ => return err(format!("{name}: wrong arguments")),
                };
                let id = self.box_behind(map)?;
                let sorted = self.sorted_tables.contains(&id);
                let at = self.insert_at(id, key, true, sorted)?;
                self.table_insert(id, at, Value::Agg(vec![key.clone(), v]), key)?;
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![at as u64, 1],
                }))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `Channel.new()` / `Channel.bounded(n)` and their `Sender` /
    /// `Receiver` ends, and `BoundedChannel[T]`: a queue in `channels`,
    /// which each end (and a `BoundedChannel`) names by its id. Single
    /// threaded, as the legacy interpreter is: a `send` that would block
    /// panics and a `recv` from an empty channel aborts, since no other task
    /// could ever make progress.
    fn channel_method(
        &mut self,
        name: &str,
        base: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        let panic = |me: &mut Self| -> R<Value> {
            if me.trace {
                me.events.push(Event::Abort(AbortReason::Panic));
            }
            Err(Stop::Abort(AbortReason::Panic))
        };
        match (base, method) {
            ("Channel", "new" | "bounded") => {
                let cap = match args.as_slice() {
                    [] => 0,
                    [Value::Int(n)] => (*n).max(0) as usize,
                    _ => return err(format!("{name}: wrong arguments")),
                };
                let elem = self
                    .tys
                    .field_ty(ret, None, 0)
                    .and_then(|end| self.tys.tcx().adt_of(end))
                    .and_then(|(_, args)| args.first().copied())
                    .ok_or_else(|| Stop::Error(format!("{name} needs its element type")))?;
                let id = self.new_chan(elem, cap, 1, 1);
                let end = || Value::Agg(vec![Value::Int(id)]);
                Ok(Value::Agg(vec![end(), end()]))
            }
            ("BoundedChannel", "new") => {
                let [Value::Int(n), on_full] = args.as_slice() else {
                    return err(format!("{name}: wrong arguments"));
                };
                let elem = self
                    .tys
                    .tcx()
                    .adt_of(ret)
                    .and_then(|(_, args)| args.first().copied())
                    .ok_or_else(|| Stop::Error(format!("{name} needs its element type")))?;
                if let Some(t) = arg_tys.get(1) {
                    self.drop_value(on_full.clone(), *t)?;
                }
                // Capacity 0 holds nothing: every send is `Full`.
                let id = self.new_chan(elem, (*n).max(0) as usize, 1, 1);
                self.chan_mut(id, name)?.bounded = true;
                Ok(Value::Agg(vec![Value::Int(id)]))
            }
            (_, "drop") => {
                // `BoundedChannel.drop`, registered by the builder for the
                // `#[compiler_builtin]` Drop impl.
                let id = self.chan_id(args.first(), name)?;
                self.end_dropped(id, true)?;
                self.end_dropped(id, false)?;
                Ok(Value::Unit)
            }
            (_, "clone") => {
                let id = self.chan_id(args.first(), name)?;
                let c = self.chan_mut(id, name)?;
                if base == "Sender" {
                    c.senders += 1;
                } else {
                    c.receivers += 1;
                }
                Ok(Value::Agg(vec![Value::Int(id)]))
            }
            (_, "send" | "try_send") => {
                let [recv, v] = args.as_slice() else {
                    return err(format!("{name} takes a value"));
                };
                let id = self.chan_id(Some(recv), name)?;
                let c = self.chan_mut(id, name)?;
                let full = (c.bounded || c.cap > 0) && c.queue.len() >= c.cap;
                let closed = !c.bounded && c.receivers == 0;
                let elem = c.elem;
                if !closed && !full {
                    c.queue.push_back(v.clone());
                }
                match (base, method, closed, full) {
                    (_, "try_send", true, _) | (_, "try_send", _, true) => {
                        // `Closed` before `Full`: a retry can never succeed.
                        let want = if closed { "Closed" } else { "Full" };
                        let e = self.variant_named(ret, Some("Err"), want, vec![v.clone()])?;
                        self.variant_named(ret, None, "Err", vec![e])
                    }
                    (_, "try_send", ..) => self.variant_named(ret, None, "Ok", vec![Value::Unit]),
                    ("BoundedChannel", ..) if full => {
                        self.drop_value(v.clone(), elem)?;
                        let e = self.variant_named(ret, Some("Err"), "Full", Vec::new())?;
                        self.variant_named(ret, None, "Err", vec![e])
                    }
                    ("BoundedChannel", ..) => {
                        self.variant_named(ret, None, "Ok", vec![Value::Unit])
                    }
                    (.., true, _) | (.., true) => {
                        self.drop_value(v.clone(), elem)?;
                        panic(self)
                    }
                    _ => Ok(Value::Unit),
                }
            }
            (_, "recv" | "recv_blocking" | "try_recv") => {
                let id = self.chan_id(args.first(), name)?;
                let v = self.chan_mut(id, name)?.queue.pop_front();
                if base == "BoundedChannel" || method == "try_recv" {
                    return self.option(ret, v);
                }
                match v {
                    Some(v) => Ok(v),
                    None => panic(self),
                }
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    fn new_chan(&mut self, elem: Ty, cap: usize, senders: usize, receivers: usize) -> i128 {
        self.channels.push(Chan {
            queue: std::collections::VecDeque::new(),
            elem,
            cap,
            bounded: false,
            senders,
            receivers,
        });
        self.channels.len() as i128
    }

    fn chan_mut(&mut self, id: i128, name: &str) -> R<&mut Chan> {
        usize::try_from(id - 1)
            .ok()
            .and_then(|i| self.channels.get_mut(i))
            .ok_or_else(|| Stop::Error(format!("{name} of a channel never made")))
    }

    /// The id a channel end (or a reference to one) holds.
    fn chan_id(&mut self, v: Option<&Value>, name: &str) -> R<i128> {
        let v = v.ok_or_else(|| Stop::Error(format!("{name} needs its receiver")))?;
        let held = match v {
            Value::Ref(_) => {
                let at = self.cell_struct(v, name)?;
                self.slot(&at)?.clone()
            }
            other => other.clone(),
        };
        match held {
            Value::Agg(fs) => match fs.as_slice() {
                [Value::Int(id)] => Ok(*id),
                _ => err(format!("{name} of a channel end holding {fs:?}")),
            },
            other => err(format!("{name} of {other:?}")),
        }
    }

    /// One end of channel `id` is gone; with both gone, what is still
    /// queued drops.
    fn end_dropped(&mut self, id: i128, sender: bool) -> R<()> {
        let c = self.chan_mut(id, "drop")?;
        let count = if sender {
            &mut c.senders
        } else {
            &mut c.receivers
        };
        *count = count.saturating_sub(1);
        if c.senders > 0 || c.receivers > 0 {
            return Ok(());
        }
        let elem = c.elem;
        let queued = std::mem::take(&mut c.queue);
        for v in queued {
            self.drop_value(v, elem)?;
        }
        Ok(())
    }

    /// `File`: an id into `files`, closed when the `File` drops. Bytes go
    /// through `Slice[u8]` views one element at a time, as legacy's do.
    fn file_method(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        use std::io::{Read, Seek, Write};
        if let ("open" | "create" | "append", [path]) = (method, args.as_slice()) {
            let path = self.string_at(path)?;
            let mut opts = std::fs::OpenOptions::new();
            match method {
                "open" => opts.read(true),
                "create" => opts.write(true).create(true).truncate(true),
                _ => opts.append(true).create(true),
            };
            return match opts.open(path) {
                Ok(f) => {
                    self.files.push(Some(f));
                    let file = Value::Agg(vec![Value::Int(self.files.len() as i128)]);
                    self.variant_named(ret, None, "Ok", vec![file])
                }
                Err(e) => self.io_err(ret, e),
            };
        }
        let id = self.chan_id(args.first(), name)?;
        let slot = usize::try_from(id - 1).ok();
        let Some(Some(mut f)) = slot.and_then(|i| self.files.get_mut(i)).map(Option::take) else {
            return err(format!("{name} of a closed file"));
        };
        let res = match (method, &args[1..]) {
            ("read", [buf]) => {
                let (base, lo, len) = self.view_of(buf)?;
                let mut bytes = vec![0u8; len as usize];
                f.read(&mut bytes).map(|n| {
                    for (i, b) in bytes[..n].iter().enumerate() {
                        let at = base.child(lo + i as u64);
                        if let Ok(v) = self.slot_mut(&at) {
                            *v = Value::Int(i128::from(*b));
                        }
                    }
                    Value::Int(n as i128)
                })
            }
            ("write", [buf]) => {
                let (base, lo, len) = self.view_of(buf)?;
                let mut bytes = Vec::with_capacity(len as usize);
                for i in 0..len {
                    match self.slot(&base.child(lo + i))? {
                        Value::Int(b) => bytes.push(b as u8),
                        other => return err(format!("{name} of a byte holding {other:?}")),
                    }
                }
                f.write(&bytes).map(|n| Value::Int(n as i128))
            }
            ("flush", []) => f.flush().map(|()| Value::Unit),
            ("sync_all", []) => f.sync_all().map(|()| Value::Unit),
            ("sync_data", []) => f.sync_data().map(|()| Value::Unit),
            ("seek", [whence, Value::Int(off)]) => {
                // `SeekFrom` comes by value or behind a reference.
                let k = match whence {
                    Value::Variant(k, _) => *k,
                    Value::Ref(at) => match self.slot(at)? {
                        Value::Variant(k, _) => k,
                        other => return err(format!("{name} of {other:?}")),
                    },
                    other => return err(format!("{name} of {other:?}")),
                };
                let whence_ty = arg_tys.get(1).map(|&t| match self.tys.kind(t) {
                    TyKind::Ref(t) | TyKind::MutRef(t) => t,
                    _ => t,
                });
                let whence = whence_ty
                    .and_then(|t| self.tys.tcx().adt_of(t))
                    .and_then(|(adt, _)| adt.variants.get(k as usize).map(|v| v.name.clone()));
                let off = *off as i64;
                let pos = match whence.as_deref() {
                    Some("Start") if off < 0 => Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "invalid seek to a negative position",
                    )),
                    Some("Start") => Ok(std::io::SeekFrom::Start(off as u64)),
                    Some("Current") => Ok(std::io::SeekFrom::Current(off)),
                    Some("End") => Ok(std::io::SeekFrom::End(off)),
                    _ => return err(format!("{name} of an unknown SeekFrom")),
                };
                pos.and_then(|p| f.seek(p))
                    .map(|p| Value::Int(i128::from(p)))
            }
            ("drop", []) => {
                // `f` is closed by going out of scope here.
                return Ok(Value::Unit);
            }
            _ => return err(format!("call of unknown function {name}")),
        };
        if let Some(i) = slot {
            self.files[i] = Some(f);
        }
        match res {
            Ok(v) => self.variant_named(ret, None, "Ok", vec![v]),
            Err(e) => self.io_err(ret, e),
        }
    }

    /// `TaskGroup.spawn`, run eagerly at the spawn as the legacy
    /// interpreter does: a `TaskHandle[T]` holds the child's result where
    /// its id would be, and `join` hands it back.
    fn task_method(
        &mut self,
        name: &str,
        base: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
    ) -> R<Value> {
        match (base, method) {
            ("TaskGroup", "new") => Ok(Value::Agg(vec![Value::Int(0)])),
            ("TaskGroup", "cancel" | "drop") => Ok(Value::Unit),
            ("TaskGroup", "spawn") => {
                let (Some(f), Some(&fty)) = (args.get(1), arg_tys.get(1)) else {
                    return err(format!("{name} takes a closure"));
                };
                let mut callee = self.hold_callee(f.clone(), fty, name)?;
                let v = self.call_callee(&mut callee, Vec::new());
                self.release_callee(callee, fty)?;
                Ok(Value::Agg(vec![v?]))
            }
            ("TaskHandle", "join") => match args.as_slice() {
                [Value::Agg(fs)] if fs.len() == 1 => Ok(fs[0].clone()),
                _ => err(format!("{name} of a handle holding {args:?}")),
            },
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// The struct a (possibly doubly) referenced library cell points at.
    fn cell_struct(&mut self, v: &Value, name: &str) -> R<Addr> {
        let mut v = v.clone();
        loop {
            let Value::Ref(addr) = v else {
                return err(format!(
                    "{name} needs a reference to its receiver, found {v:?}"
                ));
            };
            match self.slot(&addr)? {
                inner @ Value::Ref(_) => v = inner,
                _ => return Ok(addr),
            }
        }
    }

    /// The cell inside the atomic a (possibly doubly) referenced value
    /// points at.
    fn atomic_cell(&mut self, v: &Value, name: &str) -> R<Addr> {
        let mut v = v.clone();
        loop {
            let Value::Ref(addr) = v else {
                return err(format!(
                    "{name} needs a reference to the atomic, found {v:?}"
                ));
            };
            match self.slot(&addr)? {
                inner @ Value::Ref(_) => v = inner,
                Value::Agg(_) => return Ok(addr.child(0)),
                other => return err(format!("{name} of {other:?}")),
            }
        }
    }

    /// `CStr` and `CString` (design.md § C-String Literals): a value is
    /// its bytes, without the trailing NUL, one `u8` field each.
    fn c_string_method(
        &mut self,
        name: &str,
        base: &str,
        method: &str,
        args: Vec<Value>,
        ret: Ty,
    ) -> R<Value> {
        if (base, method) == ("CStr", "from_bytes") {
            return Ok(Value::Agg(args));
        }
        if (base, method) == ("CStr", "from_ptr") {
            // The bytes from the pointed-at element of a byte sequence up
            // to its first NUL (or its end).
            let [Value::Ref(p)] = args.as_slice() else {
                return err(format!("{name} takes a pointer"));
            };
            let mut seq = p.clone();
            let Some(start) = seq.path.pop() else {
                return err(format!("{name} of a pointer to no sequence"));
            };
            let Value::Agg(cells) = self.slot(&seq)? else {
                return err(format!("{name} of a pointer into no sequence"));
            };
            let mut bytes = Vec::new();
            for c in cells.iter().skip(start as usize) {
                match c {
                    Value::Int(0) => break,
                    Value::Int(b) => bytes.push(Value::Int(*b)),
                    other => return err(format!("{name}: byte {other:?}")),
                }
            }
            // A `ref CStr` view of them, held until exit like any other
            // read-only snapshot a library method hands out.
            let id = self.alloc(HeapObj {
                count: 1,
                weak: 0,
                value: Value::Agg(bytes),
            });
            self.snapshots.insert(id);
            return Ok(Value::Ref(Addr {
                root: Root::Heap(id),
                path: Vec::new(),
            }));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        if base == "String" {
            // `to_cstring`: the text's bytes, unless C would cut it short
            // at an interior NUL.
            let text = self.string_at(recv)?;
            if text.as_bytes().contains(&0) {
                let e = self.variant_named(ret, Some("Err"), "InteriorNul", Vec::new())?;
                return self.variant_named(ret, None, "Err", vec![e]);
            }
            let bytes = text.bytes().map(|b| Value::Int(i128::from(b))).collect();
            return self.variant_named(ret, None, "Ok", vec![Value::Agg(bytes)]);
        }
        // The receiver is borrowed: find the place holding the bytes.
        let mut v = recv.clone();
        let mut at = None;
        while let Value::Ref(a) = v {
            v = self.slot(&a)?;
            at = Some(a);
        }
        let Value::Agg(cells) = v else {
            return err(format!("{name} of {v:?}"));
        };
        let mut bytes = Vec::with_capacity(cells.len());
        for c in &cells {
            match c {
                Value::Int(b) => bytes.push(*b as u8),
                other => return err(format!("{name}: byte {other:?}")),
            }
        }
        match method {
            "len" => Ok(Value::Int(bytes.len() as i128)),
            "is_empty" => Ok(Value::Bool(bytes.is_empty())),
            // The address of the first byte.
            "as_ptr" => match at {
                Some(at) => Ok(Value::Ref(at.child(0))),
                None => err(format!("{name} of an unborrowed value")),
            },
            "as_bytes" => {
                let Some(at) = at else {
                    return err(format!("{name} of an unborrowed value"));
                };
                Ok(Value::Slice {
                    base: at,
                    lo: 0,
                    len: bytes.len() as u64,
                })
            }
            // UTF-8-validated into a `String` (a `StringSlice` is held as
            // one): a cut-off final sequence is `IncompleteSequence`, any
            // other bad byte `InvalidByte`, as `String.from_utf8`.
            "to_string" | "to_string_slice" if base == "CStr" => match String::from_utf8(bytes) {
                Ok(text) => {
                    let s = self.alloc_box("String", Value::Str(text));
                    self.variant_named(ret, None, "Ok", vec![s])
                }
                Err(e) => {
                    let which = match e.utf8_error().error_len() {
                        None => "IncompleteSequence",
                        Some(_) => "InvalidByte",
                    };
                    let e = self.variant_named(ret, Some("Err"), which, Vec::new())?;
                    self.variant_named(ret, None, "Err", vec![e])
                }
            },
            _ => err(format!("call of unknown function {name}")),
        }
    }

    fn view_method(&mut self, name: &str, method: &str, args: Vec<Value>, ret: Ty) -> R<Value> {
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        let (base, lo, len) = self.view_of(recv)?;
        match (method, &args[1..]) {
            ("as_slice" | "as_mut_slice", []) => Ok(Value::Slice { base, lo, len }),
            ("slice" | "slice_mut", [Value::Int(a), Value::Int(b)]) => {
                if *a < 0 || a > b || *b > len as i128 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                Ok(Value::Slice {
                    base,
                    lo: lo + *a as u64,
                    len: (*b - *a) as u64,
                })
            }
            ("len", []) => Ok(Value::Int(len as i128)),
            ("is_empty", []) => Ok(Value::Bool(len == 0)),
            ("first" | "last" | "get", rest) => {
                let i = match (method, rest) {
                    ("first", []) => 0,
                    ("last", []) => len as i128 - 1,
                    // `last(k)` counts from the end: `last(0)` is `last()`.
                    ("last", [Value::Int(k)]) => len as i128 - 1 - k,
                    ("get", [Value::Int(i)]) => *i,
                    _ => return err(format!("{name}: wrong arguments")),
                };
                let at = (0..len as i128)
                    .contains(&i)
                    .then(|| Value::Ref(base.child(lo + i as u64)));
                self.option_of_place(ret, at)
            }
            ("split_at" | "split_at_mut", [Value::Int(m)]) => {
                if *m < 0 || *m > len as i128 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                let m = *m as u64;
                Ok(Value::Agg(vec![
                    Value::Slice {
                        base: base.clone(),
                        lo,
                        len: m,
                    },
                    Value::Slice {
                        base,
                        lo: lo + m,
                        len: len - m,
                    },
                ]))
            }
            ("chunks" | "windows", [Value::Int(n)]) => {
                // Views into the receiver: back to back, the last one
                // shorter, or every run of `n` in a row.
                if *n <= 0 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                }
                let n = *n as u64;
                let mut views = Vec::new();
                let mut at = 0;
                while at < len && (method == "chunks" || at + n <= len) {
                    views.push(Value::Slice {
                        base: base.clone(),
                        lo: lo + at,
                        len: n.min(len - at),
                    });
                    at += if method == "chunks" { n } else { 1 };
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(views)))
            }
            ("contains", [needle]) => {
                let mut needle = needle.clone();
                while let Value::Ref(a) = &needle {
                    needle = self.slot(a)?;
                }
                let want = self.key_form(&needle)?;
                for i in 0..len {
                    let x = self.slot(&base.child(lo + i))?;
                    if self.key_form(&x)? == want {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            ("reverse", []) => {
                let mut elems = Vec::with_capacity(len as usize);
                for i in 0..len {
                    elems.push(self.slot(&base.child(lo + i))?);
                }
                for (i, v) in elems.into_iter().rev().enumerate() {
                    *self.slot_mut(&base.child(lo + i as u64))? = v;
                }
                Ok(Value::Unit)
            }
            ("swap", [Value::Int(i), Value::Int(j)]) => {
                if !(0..len as i128).contains(i) || !(0..len as i128).contains(j) {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                let (a, b) = (base.child(lo + *i as u64), base.child(lo + *j as u64));
                let (x, y) = (self.slot(&a)?, self.slot(&b)?);
                *self.slot_mut(&a)? = y;
                *self.slot_mut(&b)? = x;
                Ok(Value::Unit)
            }
            ("is_sorted", []) => {
                let mut prev: Option<Value> = None;
                for i in 0..len {
                    let x = self.slot(&base.child(lo + i))?;
                    let k = self.key_form(&x)?;
                    if prev.as_ref().is_some_and(|p| cmp_key(p, &k).is_gt()) {
                        return Ok(Value::Bool(false));
                    }
                    prev = Some(k);
                }
                Ok(Value::Bool(true))
            }
            ("get_unchecked", [Value::Int(i)]) if !matches!(self.tys.kind(ret), TyKind::Ref(_)) => {
                // Typed `T`: a copy of the element.
                if *i < 0 || *i >= len as i128 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                let x = self.slot(&base.child(lo + *i as u64))?;
                self.clone_value(&x, ret)
            }
            ("index" | "index_mut" | "get_unchecked", [Value::Int(i)]) => {
                if *i < 0 || *i >= len as i128 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    }
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                Ok(Value::Ref(base.child(lo + *i as u64)))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// The sequence a view method's receiver reaches: a `Vec` (owned or
    /// behind a reference), an array place behind a reference, or a slice.
    fn view_of(&mut self, v: &Value) -> R<(Addr, u64, u64)> {
        let held = match v {
            Value::Ref(addr) => (Some(addr.clone()), self.slot(addr)?),
            other => (None, other.clone()),
        };
        match held {
            (_, Value::Box(id)) => {
                let n = self.vec_elems(id)?.len() as u64;
                let base = Addr {
                    root: Root::Heap(id),
                    path: Vec::new(),
                };
                Ok((base, 0, n))
            }
            (_, Value::Slice { base, lo, len }) => Ok((base, lo, len)),
            (Some(addr), Value::Agg(fs)) => Ok((addr, 0, fs.len() as u64)),
            (_, other) => err(format!("expected a sequence, found {other:?}")),
        }
    }

    /// The core `Map[K, V]` and `Set[T]` methods. A table is a box of
    /// entries in insertion order (`(key, value)` pairs for a `Map`);
    /// lookup compares keys by value, through boxes and references.
    fn table_method(
        &mut self,
        ty_name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        let name = format!("{ty_name}.{method}");
        let is_map = ty_name.starts_with("Map") || ty_name.starts_with("SortedMap");
        // A sorted table keeps its entries in key order, so iteration and
        // `entry_at` walk keys in ascending order.
        let sorted = ty_name.starts_with("Sorted");
        if method == "new" {
            return Ok(self.alloc_box(ty_name, Value::Agg(vec![])));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        let id = self.box_behind(recv)?;
        let (key_ty, val_ty) = self.table_tys(arg_tys, &name)?;
        match (method, &args[1..]) {
            ("len", []) => Ok(Value::Int(self.vec_elems(id)?.len() as i128)),
            ("is_empty", []) => Ok(Value::Bool(self.vec_elems(id)?.is_empty())),
            ("get" | "contains_key" | "contains", [key]) => {
                let found = self.find_key(id, key, key_ty, is_map)?;
                if method != "get" {
                    return Ok(Value::Bool(found.is_some()));
                }
                let at = found.map(|i| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64, 1],
                    })
                });
                self.option_of_place(ret, at)
            }
            ("index" | "index_mut", [key]) if is_map => {
                // `m[k]`: a missing key panics (design.md § Collection
                // Core Methods).
                let Some(i) = self.find_key(id, key, key_ty, true)? else {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                };
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64, 1],
                }))
            }
            ("insert", [key, rest @ ..]) => {
                let found = self.find_key(id, key, key_ty, is_map)?;
                match (is_map, found, rest) {
                    // An existing key is kept and the new one dropped;
                    // the old value is handed back.
                    (true, Some(i), [val]) => {
                        self.drop_value(key.clone(), key_ty)?;
                        let Value::Agg(entry) = &mut self.vec_elems(id)?[i] else {
                            return err(format!("{name}: a malformed entry"));
                        };
                        let old = std::mem::replace(&mut entry[1], val.clone());
                        // A call whose result the program discards (a
                        // `match` arm in statement position) is typed `()`.
                        if ret == self.tys.unit() {
                            self.drop_value(old, val_ty)?;
                            return Ok(Value::Unit);
                        }
                        self.option(ret, Some(old))
                    }
                    (true, None, [val]) => {
                        let entry = Value::Agg(vec![key.clone(), val.clone()]);
                        let at = self.insert_at(id, key, is_map, sorted)?;
                        self.table_insert(id, at, entry, key)?;
                        if ret == self.tys.unit() {
                            return Ok(Value::Unit);
                        }
                        self.option(ret, None)
                    }
                    (false, Some(_), []) => {
                        self.drop_value(key.clone(), key_ty)?;
                        Ok(Value::Bool(false))
                    }
                    (false, None, []) => {
                        let at = self.insert_at(id, key, is_map, sorted)?;
                        self.table_insert(id, at, key.clone(), key)?;
                        Ok(Value::Bool(true))
                    }
                    _ => err(format!("{name}: wrong arguments")),
                }
            }
            ("remove", [key]) => {
                let found = self.find_key(id, key, key_ty, is_map)?;
                let Some(i) = found else {
                    return if is_map {
                        self.option(ret, None)
                    } else {
                        Ok(Value::Bool(false))
                    };
                };
                let removed = self.table_remove(id, i)?;
                if !is_map {
                    self.drop_value(removed, key_ty)?;
                    return Ok(Value::Bool(true));
                }
                let Value::Agg(mut entry) = removed else {
                    return err(format!("{name}: a malformed entry"));
                };
                let val = entry.pop().expect("an entry is a pair");
                let k = entry.pop().expect("an entry is a pair");
                self.drop_value(k, key_ty)?;
                self.option(ret, Some(val))
            }
            ("entry_at", [Value::Int(i)]) => {
                // The `i`th entry in iteration order, for a borrowing `for`:
                // a `ref (K, V)` into a Map, a `ref T` into a Set.
                let i = self.bounds(id, *i, false)?;
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64],
                }))
            }
            ("get_or", [key, default]) if is_map => {
                // A copy of the stored value, or the default (cloned when
                // it came by reference).
                let found = self.find_key(id, key, key_ty, true)?;
                let owned_default = !matches!(default, Value::Ref(_));
                match found {
                    Some(i) => {
                        let v = self.slot(&Addr {
                            root: Root::Heap(id),
                            path: vec![i as u64, 1],
                        })?;
                        if owned_default {
                            self.drop_value(default.clone(), val_ty)?;
                        }
                        self.clone_value(&v, val_ty)
                    }
                    None if owned_default => Ok(default.clone()),
                    None => {
                        let Value::Ref(a) = default else {
                            unreachable!("checked above")
                        };
                        let v = self.slot(a)?;
                        self.clone_value(&v, val_ty)
                    }
                }
            }
            ("keys" | "values", []) if is_map => {
                // A `Vec` of the keys (or values) in iteration order:
                // references when the result holds references, else copies.
                let e = match self.tys.kind(ret) {
                    TyKind::Intrinsic(IntrinsicTy::Vec(e)) => e,
                    _ => return err(format!("{name} into {}", self.tys.display(ret))),
                };
                let part = u64::from(method == "values");
                let part_ty = if method == "values" { val_ty } else { key_ty };
                let by_ref = matches!(self.tys.kind(e), TyKind::Ref(_));
                let n = self.vec_elems(id)?.len();
                let mut out = Vec::with_capacity(n);
                for i in 0..n as u64 {
                    let at = Addr {
                        root: Root::Heap(id),
                        path: vec![i, part],
                    };
                    if by_ref {
                        out.push(Value::Ref(at));
                    } else {
                        let v = self.slot(&at)?;
                        out.push(self.clone_value(&v, part_ty)?);
                    }
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            ("entry", [key]) if is_map => {
                // `Occupied` lends the stored value; `Vacant` keeps the key
                // and the map until `or_insert` (design.md § Entry[K, V]).
                let found = self.find_key(id, key, key_ty, true)?;
                if let Some(i) = found {
                    self.drop_value(key.clone(), key_ty)?;
                    let at = Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64, 1],
                    });
                    return self.variant_named(ret, None, "Occupied", vec![at]);
                }
                if sorted {
                    self.sorted_tables.insert(id);
                } else {
                    self.sorted_tables.remove(&id);
                }
                self.variant_named(ret, None, "Vacant", vec![key.clone(), recv.clone()])
            }
            ("entry_or_insert" | "entry_or_insert_with", [key, val]) if is_map => {
                // `m.entry(k).or_insert(v)`, fused: a reference to the
                // stored value, inserting `v` (or `f()`) when `k` is new.
                // A found key drops the new key and the unused value.
                let found = self.find_key(id, key, key_ty, true)?;
                let i = match found {
                    Some(i) => {
                        self.drop_value(key.clone(), key_ty)?;
                        let unused_ty = *arg_tys.get(2).unwrap_or(&val_ty);
                        self.drop_value(val.clone(), unused_ty)?;
                        i
                    }
                    None => {
                        let at = self.insert_at(id, key, true, sorted)?;
                        let v = if method == "entry_or_insert_with" {
                            let fty = *arg_tys.get(2).ok_or_else(|| {
                                Stop::Error(format!("{name} needs its closure's type"))
                            })?;
                            let mut f = self.hold_callee(val.clone(), fty, &name)?;
                            let v = self.call_callee(&mut f, Vec::new())?;
                            self.release_callee(f, fty)?;
                            v
                        } else {
                            val.clone()
                        };
                        self.table_insert(id, at, Value::Agg(vec![key.clone(), v]), key)?;
                        at
                    }
                };
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64, 1],
                }))
            }
            ("min" | "max" | "first" | "last", []) if !is_map => {
                // The least / greatest element, in place.
                let n = self.vec_elems(id)?.len();
                let mut keys = Vec::with_capacity(n);
                for i in 0..n {
                    let k = self.entry_key(id, i, false)?;
                    keys.push(self.key_form(&k)?);
                }
                let pick = if matches!(method, "min" | "first") {
                    (0..n).min_by(|&a, &b| key_order(&keys[a], &keys[b]))
                } else {
                    (0..n).max_by(|&a, &b| key_order(&keys[a], &keys[b]))
                };
                let at = pick.map(|i| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    })
                });
                self.option_of_place(ret, at)
            }
            (
                "entries" | "range" | "floor" | "ceiling" | "min" | "max" | "first" | "last",
                rest,
            ) if is_map => {
                // Clones of entries as `(K, V)` tuples: all of them, the
                // keys in `lo..=hi`, the greatest key at most / least key
                // at least `k`, or the first / last.
                let n = self.vec_elems(id)?.len();
                let mut keys = Vec::with_capacity(n);
                for i in 0..n {
                    let k = self.entry_key(id, i, true)?;
                    keys.push(self.key_form(&k)?);
                }
                let bound = |me: &mut Self, i: usize| -> R<Value> {
                    let mut v = rest[i].clone();
                    while let Value::Ref(a) = &v {
                        v = me.slot(a)?;
                    }
                    me.key_form(&v)
                };
                use std::cmp::Ordering::*;
                let picked: Vec<usize> = match method {
                    "entries" => (0..n).collect(),
                    "range" => {
                        let (lo, hi) = (bound(self, 0)?, bound(self, 1)?);
                        (0..n)
                            .filter(|&i| {
                                key_order(&keys[i], &lo) != Less
                                    && key_order(&keys[i], &hi) != Greater
                            })
                            .collect()
                    }
                    "floor" => {
                        let k = bound(self, 0)?;
                        (0..n)
                            .filter(|&i| key_order(&keys[i], &k) != Greater)
                            .max_by(|&a, &b| key_order(&keys[a], &keys[b]))
                            .into_iter()
                            .collect()
                    }
                    "ceiling" => {
                        let k = bound(self, 0)?;
                        (0..n)
                            .filter(|&i| key_order(&keys[i], &k) != Less)
                            .min_by(|&a, &b| key_order(&keys[a], &keys[b]))
                            .into_iter()
                            .collect()
                    }
                    "min" | "first" => (0..n)
                        .min_by(|&a, &b| key_order(&keys[a], &keys[b]))
                        .into_iter()
                        .collect(),
                    _ => (0..n)
                        .max_by(|&a, &b| key_order(&keys[a], &keys[b]))
                        .into_iter()
                        .collect(),
                };
                let mut out = Vec::with_capacity(picked.len());
                for i in picked {
                    let e = Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    };
                    let (k, v) = (self.slot(&e.child(0))?, self.slot(&e.child(1))?);
                    out.push(Value::Agg(vec![
                        self.clone_value(&k, key_ty)?,
                        self.clone_value(&v, val_ty)?,
                    ]));
                }
                if matches!(method, "entries" | "range") {
                    let vname = self.tys.display(ret);
                    return Ok(self.alloc_box(&vname, Value::Agg(out)));
                }
                self.option(ret, out.pop())
            }
            ("union" | "intersection" | "difference" | "symmetric_difference", [other])
                if !is_map =>
            {
                // A new set of clones, the receiver's elements first.
                let other = self.box_behind(other)?;
                let mine = self.vec_elems(id)?.clone();
                let theirs = self.vec_elems(other)?.clone();
                let mut out = Vec::new();
                for x in &mine {
                    let in_other = self.find_key(other, x, key_ty, false)?.is_some();
                    let keep = match method {
                        "union" => true,
                        "intersection" => in_other,
                        _ => !in_other,
                    };
                    if keep {
                        out.push(self.clone_value(x, key_ty)?);
                    }
                }
                if matches!(method, "union" | "symmetric_difference") {
                    for x in &theirs {
                        if self.find_key(id, x, key_ty, false)?.is_none() {
                            out.push(self.clone_value(x, key_ty)?);
                        }
                    }
                }
                if sorted {
                    let mut keyed = Vec::with_capacity(out.len());
                    for x in out {
                        keyed.push((self.key_form(&x)?, x));
                    }
                    keyed.sort_by(|a, b| key_order(&a.0, &b.0));
                    out = keyed.into_iter().map(|(_, x)| x).collect();
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            ("clear", []) => {
                let n = self.vec_elems(id)?.len();
                for i in 0..n as u64 {
                    let at = Addr {
                        root: Root::Heap(id),
                        path: vec![i],
                    };
                    if is_map {
                        self.drop_at(&at.child(0), key_ty)?;
                        self.drop_at(&at.child(1), val_ty)?;
                    } else {
                        self.drop_at(&at, key_ty)?;
                    }
                }
                self.vec_elems(id)?.clear();
                self.key_index.remove(&id);
                Ok(Value::Unit)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// The key and value types of a `Map` receiver, or the element type
    /// (twice) of a `Set` one.
    /// Where a new key goes: the end of an insertion-ordered table, or
    /// before the first larger key of a sorted one.
    fn insert_at(&mut self, id: AllocId, key: &Value, is_map: bool, sorted: bool) -> R<usize> {
        let n = self.vec_elems(id)?.len();
        if !sorted {
            return Ok(n);
        }
        // The first entry whose key is larger, by binary search: the
        // entries are in key order.
        let want = self.key_form(key)?;
        let (mut lo, mut hi) = (0, n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let k = self.entry_key(id, mid, is_map)?;
            if key_order(&self.key_form(&k)?, &want) == std::cmp::Ordering::Greater {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        Ok(lo)
    }

    /// `VecDeque[T]`: a `Vec`'s allocation, used from both ends.
    fn deque_method(
        &mut self,
        ty_name: &str,
        method: &str,
        args: Vec<Value>,
        arg_tys: &[Ty],
        ret: Ty,
    ) -> R<Value> {
        let name = format!("{ty_name}.{method}");
        if matches!(method, "new" | "with_capacity") {
            if let [Value::Int(n), ..] = args.as_slice() {
                if *n < 0 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                }
            }
            return Ok(self.alloc_box(ty_name, Value::Agg(vec![])));
        }
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        let id = self.box_behind(recv)?;
        match (method, &args[1..]) {
            ("len", []) => Ok(Value::Int(self.vec_elems(id)?.len() as i128)),
            ("is_empty", []) => Ok(Value::Bool(self.vec_elems(id)?.is_empty())),
            ("push_back", [v]) => {
                self.vec_elems(id)?.push(v.clone());
                Ok(Value::Unit)
            }
            ("push_front", [v]) => {
                self.vec_elems(id)?.insert(0, v.clone());
                Ok(Value::Unit)
            }
            ("pop_back", []) => {
                let v = self.vec_elems(id)?.pop();
                self.option(ret, v)
            }
            ("pop_front", []) => {
                let elems = self.vec_elems(id)?;
                let v = (!elems.is_empty()).then(|| elems.remove(0));
                self.option(ret, v)
            }
            ("front" | "back", []) => {
                let n = self.vec_elems(id)?.len();
                let i = if method == "front" {
                    0
                } else {
                    n.wrapping_sub(1)
                };
                let at = (n > 0).then(|| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![i as u64],
                    })
                });
                self.option_of_place(ret, at)
            }
            ("get", [Value::Int(i)]) => {
                let n = self.vec_elems(id)?.len() as i128;
                let at = (0..n).contains(i).then(|| {
                    Value::Ref(Addr {
                        root: Root::Heap(id),
                        path: vec![*i as u64],
                    })
                });
                self.option_of_place(ret, at)
            }
            ("entry_at" | "index" | "index_mut", [Value::Int(i)]) => {
                let i = self.bounds(id, *i, false)?;
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64],
                }))
            }
            ("clone", []) => {
                let t = self.deref_ty(arg_tys.first().copied(), &name)?;
                let v = Value::Box(id);
                self.clone_value(&v, t)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `FileSystem.write(path, text)` and `FileSystem.read_to_string(path)`
    /// on the real file system, as the legacy interpreter does; an I/O
    /// failure is an `Err(IoError)` with legacy's variant for its kind.
    fn fs_method(&mut self, method: &str, args: Vec<Value>, ret: Ty) -> R<Value> {
        let name = format!("FileSystem.{method}");
        let res = match (method, args.as_slice()) {
            ("write", [path, text]) => {
                let (path, text) = (self.string_at(path)?, self.string_at(text)?);
                std::fs::write(path, text).map(|()| None)
            }
            ("read_to_string" | "read_lines", [path]) => {
                let path = self.string_at(path)?;
                std::fs::read_to_string(path).map(Some)
            }
            _ => return err(format!("{name}: wrong arguments")),
        };
        match res {
            Ok(None) => self.variant_named(ret, None, "Ok", vec![Value::Unit]),
            Ok(Some(text)) if method == "read_lines" => {
                let lines = text
                    .lines()
                    .map(|l| self.alloc_box("String", Value::Str(l.to_string())))
                    .collect();
                let v = self.alloc_box("Vec[String]", Value::Agg(lines));
                self.variant_named(ret, None, "Ok", vec![v])
            }
            Ok(Some(text)) => {
                let s = self.alloc_box("String", Value::Str(text));
                self.variant_named(ret, None, "Ok", vec![s])
            }
            Err(e) => self.io_err(ret, e),
        }
    }

    /// `Err(IoError)` in the `Result` type `ret`, with legacy's variant for
    /// the error's kind.
    fn io_err(&mut self, ret: Ty, e: std::io::Error) -> R<Value> {
        use std::io::ErrorKind as K;
        let (kind, msg) = match e.kind() {
            K::NotFound => ("NotFound", None),
            K::PermissionDenied => ("PermissionDenied", None),
            K::AlreadyExists => ("AlreadyExists", None),
            K::UnexpectedEof => ("UnexpectedEof", None),
            K::InvalidData => ("InvalidUtf8", None),
            K::Interrupted => ("Interrupted", None),
            _ => ("Other", Some(e.to_string())),
        };
        let fields = match msg {
            Some(m) => vec![self.alloc_box("String", Value::Str(m))],
            None => Vec::new(),
        };
        let payload = self.variant_named(ret, Some("Err"), kind, fields)?;
        self.variant_named(ret, None, "Err", vec![payload])
    }

    /// `i64.parse(s)`, `f64.parse(s)`, `u8.from_str_radix(s, 16)`: `Some`
    /// of the number when the trimmed text is one that fits the type.
    fn parse_number(&mut self, ty: &str, method: &str, args: &[Value], ret: Ty) -> R<Value> {
        let name = format!("{ty}.{method}");
        let (text, radix) = match (method, args) {
            ("parse", [s]) => (self.string_at(s)?, 10),
            ("from_str_radix", [s, Value::Int(r)]) => (self.string_at(s)?, *r),
            _ => return err(format!("{name}: wrong arguments")),
        };
        let text = text.trim();
        let v = if ty == "f64" {
            text.parse::<f64>().ok().map(Value::Float)
        } else {
            let (bits, signed) = int_width(ty).expect("checked by the caller");
            let n = if !(2..=36).contains(&radix) || (!signed && text.starts_with('-')) {
                None
            } else if bits == 128 && !signed {
                // A `u128` is held as its bits.
                u128::from_str_radix(text, radix as u32)
                    .ok()
                    .map(|n| n as i128)
            } else {
                i128::from_str_radix(text, radix as u32).ok().filter(|n| {
                    if bits == 128 {
                        true
                    } else if signed {
                        let half = 1i128 << (bits - 1);
                        (-half..half).contains(n)
                    } else {
                        *n >= 0 && *n < (1i128 << bits)
                    }
                })
            };
            n.map(Value::Int)
        };
        self.option(ret, v)
    }

    fn table_tys(&self, arg_tys: &[Ty], name: &str) -> R<(Ty, Ty)> {
        let mut t = *arg_tys
            .first()
            .ok_or_else(|| Stop::Error(format!("{name} needs its receiver's type")))?;
        while let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(t) {
            t = inner;
        }
        match self.tys.kind(t) {
            TyKind::Intrinsic(IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v)) => Ok((k, v)),
            TyKind::Intrinsic(IntrinsicTy::Set(e) | IntrinsicTy::SortedSet(e)) => Ok((e, e)),
            _ => err(format!("{name} on {}", self.tys.display(t))),
        }
    }

    /// The index of the entry whose key equals `key`, if any.
    fn find_key(&mut self, id: AllocId, key: &Value, key_ty: Ty, is_map: bool) -> R<Option<usize>> {
        if self.total_float_key(key_ty) {
            // `F64` / `F32` keys are equal by total order: their bits.
            let want = total_form(self.key_form(key)?);
            let n = self.vec_elems(id)?.len();
            for i in 0..n {
                let k = self.entry_key(id, i, is_map)?;
                if total_form(self.key_form(&k)?) == want {
                    return Ok(Some(i));
                }
            }
            return Ok(None);
        }
        let user_eq = self.user_eq(key_ty);
        if user_eq.is_none() {
            if let Some(r) = self.key_repr(key)? {
                self.build_key_index(id, is_map)?;
                if let Some(Some(ix)) = self.key_index.get(&id) {
                    return Ok(ix.get(&r).copied());
                }
            }
        }
        let want = match user_eq {
            Some(_) => Value::Unit,
            None => self.key_form(key)?,
        };
        let n = self.vec_elems(id)?.len();
        for i in 0..n {
            let k = self.entry_key(id, i, is_map)?;
            let equal = match &user_eq {
                Some(eq) => self.call_eq(eq, &k, key)?,
                None => self.key_form(&k)? == want,
            };
            if equal {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// Whether `ty` is a library total-order float wrapper (`F64`, `F32`).
    fn total_float_key(&self, ty: Ty) -> bool {
        let mut ty = ty;
        while let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(ty) {
            ty = inner;
        }
        matches!(self.tys.kind(ty), TyKind::Adt(_))
            && matches!(self.tys.adt_name(ty).as_str(), "F64" | "F32")
    }

    /// The key of table `id`'s entry `i`.
    fn entry_key(&mut self, id: AllocId, i: usize, is_map: bool) -> R<Value> {
        match (is_map, &self.vec_elems(id)?[i]) {
            (true, Value::Agg(pair)) => Ok(pair[0].clone()),
            (false, k) => Ok(k.clone()),
            _ => err("a malformed map entry"),
        }
    }

    /// A string that two keys share exactly when they are equal, for the
    /// table index; `None` for a key holding a float, whose equality is not
    /// its representation's (`-0.0 == 0.0`, `NaN != NaN`).
    fn key_repr(&mut self, k: &Value) -> R<Option<String>> {
        fn has_float(v: &Value) -> bool {
            match v {
                Value::Float(_) => true,
                Value::Agg(fs) | Value::Variant(_, fs) => fs.iter().any(has_float),
                _ => false,
            }
        }
        let f = self.key_form(k)?;
        Ok((!has_float(&f)).then(|| format!("{f:?}")))
    }

    /// Builds table `id`'s key index unless it has one.
    fn build_key_index(&mut self, id: AllocId, is_map: bool) -> R<()> {
        if self.key_index.contains_key(&id) {
            return Ok(());
        }
        let n = self.vec_elems(id)?.len();
        let mut ix = rustc_hash::FxHashMap::default();
        for i in 0..n {
            let k = self.entry_key(id, i, is_map)?;
            match self.key_repr(&k)? {
                Some(r) => {
                    ix.insert(r, i);
                }
                None => {
                    self.key_index.insert(id, None);
                    return Ok(());
                }
            }
        }
        self.key_index.insert(id, Some(ix));
        Ok(())
    }

    /// Inserts `entry`, whose key is `key`, at position `at` of table `id`.
    fn table_insert(&mut self, id: AllocId, at: usize, entry: Value, key: &Value) -> R<()> {
        let r = match self.key_index.get(&id) {
            Some(Some(_)) => self.key_repr(key)?,
            _ => None,
        };
        let len = self.vec_elems(id)?.len();
        self.vec_elems(id)?.insert(at, entry);
        if let Some(slot) = self.key_index.get_mut(&id) {
            match (slot.as_mut(), r) {
                (Some(ix), Some(r)) => {
                    if at < len {
                        for v in ix.values_mut() {
                            if *v >= at {
                                *v += 1;
                            }
                        }
                    }
                    ix.insert(r, at);
                }
                _ => *slot = None,
            }
        }
        Ok(())
    }

    /// Removes and returns entry `i` of table `id`.
    fn table_remove(&mut self, id: AllocId, i: usize) -> R<Value> {
        let removed = self.vec_elems(id)?.remove(i);
        if let Some(Some(ix)) = self.key_index.get_mut(&id) {
            ix.retain(|_, v| *v != i);
            for v in ix.values_mut() {
                if *v > i {
                    *v -= 1;
                }
            }
        }
        Ok(removed)
    }

    /// The lowered body of `ty`'s user `PartialEq.eq`, if it has one.
    /// Lowering queues it for the keys of every table it calls into and
    /// the elements of every `Vec` it searches.
    fn user_eq(&self, ty: Ty) -> Option<String> {
        let mut ty = ty;
        while let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(ty) {
            ty = inner;
        }
        if !matches!(self.tys.kind(ty), TyKind::Adt(_) | TyKind::Shared(_)) {
            return None;
        }
        let name = format!("{}.eq", self.tys.adt_name(ty));
        self.program.bodies.contains_key(&name).then_some(name)
    }

    /// `a == b` by the user `eq` body `name`, which takes both by
    /// reference.
    fn call_eq(&mut self, name: &str, a: &Value, b: &Value) -> R<bool> {
        let mut scratch = Vec::new();
        let mut refs = Vec::new();
        for v in [a, b] {
            if let Value::Ref(_) = v {
                refs.push(v.clone());
                continue;
            }
            let slot = self
                .alloc(HeapObj {
                    count: 1,
                    weak: 0,
                    value: v.clone(),
                })
                .0 as usize;
            scratch.push(slot);
            refs.push(Value::Ref(Addr {
                root: Root::Heap(AllocId(slot as u32)),
                path: Vec::new(),
            }));
        }
        let r = self.call(name, refs);
        // The copies share the values' allocations, so they go undropped.
        for slot in scratch {
            self.free_slot(AllocId(slot as u32));
        }
        match r? {
            Value::Bool(b) => Ok(b),
            other => err(format!("{name} returned {other:?}")),
        }
    }

    /// A key's value with references and boxes replaced by what they
    /// hold, so two keys compare equal when their contents do.
    fn key_form(&mut self, v: &Value) -> R<Value> {
        Ok(match v {
            Value::Ref(addr) => {
                let inner = self.slot(addr)?;
                self.key_form(&inner)?
            }
            Value::Box(id) | Value::Shared(id) => {
                // A `shared` key compares by what it holds, as legacy's does.
                let inner = self.live(*id)?.value.clone();
                self.key_form(&inner)?
            }
            Value::Agg(fs) => {
                Value::Agg(fs.iter().map(|f| self.key_form(f)).collect::<R<Vec<_>>>()?)
            }
            Value::Variant(k, fs) => Value::Variant(
                *k,
                fs.iter().map(|f| self.key_form(f)).collect::<R<Vec<_>>>()?,
            ),
            other => other.clone(),
        })
    }

    /// A copy of `v`, a value of type `ty`, for `Clone` on the library
    /// types: scalars and `Copy` aggregates copy, a `String` or `Vec`
    /// gets a new allocation with its contents cloned. A user type that
    /// is not `Copy` needs its own `Clone` body, which is a call in MIR.
    fn clone_value(&mut self, v: &Value, ty: Ty) -> R<Value> {
        if self.tys.is_copy(ty) {
            return Ok(v.clone());
        }
        match (self.tys.kind(ty), v) {
            (TyKind::Intrinsic(IntrinsicTy::String), _) => {
                let text = self.string_at(v)?;
                Ok(self.alloc_box("String", Value::Str(text)))
            }
            (TyKind::Intrinsic(IntrinsicTy::Vec(e) | IntrinsicTy::VecDeque(e)), _) => {
                let id = self.box_behind(v)?;
                let elems = self.vec_elems(id)?.clone();
                let mut out = Vec::with_capacity(elems.len());
                for x in &elems {
                    out.push(self.clone_value(x, e)?);
                }
                let name = self.tys.display(ty);
                Ok(self.alloc_box(&name, Value::Agg(out)))
            }
            (TyKind::Tuple(ts), Value::Agg(fs)) => {
                let mut out = Vec::with_capacity(fs.len());
                for (f, t) in fs.iter().zip(ts.iter()) {
                    out.push(self.clone_value(f, *t)?);
                }
                Ok(Value::Agg(out))
            }
            (TyKind::Array(e, _), Value::Agg(fs)) => {
                let mut out = Vec::with_capacity(fs.len());
                for f in fs {
                    out.push(self.clone_value(f, e)?);
                }
                Ok(Value::Agg(out))
            }
            // A new handle to the same object (core semantics §6.1).
            (TyKind::Shared(_), Value::Shared(id)) => {
                let obj = self.live(*id)?;
                obj.count += 1;
                let c = obj.count;
                if self.trace {
                    self.events.push(Event::Retain(*id, c));
                }
                Ok(Value::Shared(*id))
            }
            // A new weak handle to the same object.
            (TyKind::Weak(_), Value::Weak(id)) => {
                if *id != EMPTY_WEAK {
                    self.live(*id)?.weak += 1;
                }
                Ok(Value::Weak(*id))
            }
            (TyKind::Intrinsic(IntrinsicTy::Map(k, val) | IntrinsicTy::SortedMap(k, val)), _) => {
                let id = self.box_behind(v)?;
                let entries = self.vec_elems(id)?.clone();
                let mut out = Vec::with_capacity(entries.len());
                for e in &entries {
                    let Value::Agg(pair) = e else {
                        return err("a malformed map entry");
                    };
                    let ck = self.clone_value(&pair[0], k)?;
                    let cv = self.clone_value(&pair[1], val)?;
                    out.push(Value::Agg(vec![ck, cv]));
                }
                let name = self.tys.display(ty);
                Ok(self.alloc_box(&name, Value::Agg(out)))
            }
            (TyKind::Intrinsic(IntrinsicTy::Set(e) | IntrinsicTy::SortedSet(e)), _) => {
                let id = self.box_behind(v)?;
                let elems = self.vec_elems(id)?.clone();
                let mut out = Vec::with_capacity(elems.len());
                for x in &elems {
                    out.push(self.clone_value(x, e)?);
                }
                let name = self.tys.display(ty);
                Ok(self.alloc_box(&name, Value::Agg(out)))
            }
            // A struct or enum with no user `Clone` body: field by field,
            // as `#[derive(Clone)]` does, whether or not it has a `Drop`
            // body (design.md §9). The checker allows `.clone()` only on a
            // type that derives or implements `Clone`.
            (TyKind::Adt(_), Value::Agg(_) | Value::Variant(..)) => {
                let (variant, fs) = match v {
                    Value::Variant(k, fs) => (Some(*k), fs.clone()),
                    Value::Agg(fs) => (None, fs.clone()),
                    _ => unreachable!(),
                };
                let mut out = Vec::with_capacity(fs.len());
                for (i, f) in fs.iter().enumerate() {
                    let Some(t) = self.tys.field_ty(ty, variant, i as u32) else {
                        return err(format!("{} has no field {i}", self.tys.display(ty)));
                    };
                    out.push(self.clone_value(f, t)?);
                }
                Ok(match variant {
                    Some(k) => Value::Variant(k, out),
                    None => Value::Agg(out),
                })
            }
            _ => err(format!(
                "clone of {} needs its Clone body",
                self.tys.display(ty)
            )),
        }
    }

    /// Drops a value that has no place of its own (a duplicate key), by
    /// parking it in a scratch heap slot for the length of the drop.
    fn drop_value(&mut self, v: Value, ty: Ty) -> R<()> {
        let scratch = self
            .alloc(HeapObj {
                count: 1,
                weak: 0,
                value: v,
            })
            .0 as usize;
        let at = Addr {
            root: Root::Heap(AllocId(scratch as u32)),
            path: Vec::new(),
        };
        self.drop_at(&at, ty)?;
        self.free_slot(AllocId(scratch as u32));
        Ok(())
    }

    /// The pointee type of a method's receiver type (`ref T` -> `T`).
    fn deref_ty(&self, t: Option<Ty>, name: &str) -> R<Ty> {
        let mut t = t.ok_or_else(|| Stop::Error(format!("{name} needs its receiver's type")))?;
        while let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(t) {
            t = inner;
        }
        Ok(t)
    }

    /// `char` classification and case mapping.
    fn char_method(&mut self, name: &str, method: &str, args: &[Value], ret: Ty) -> R<Value> {
        let Some(Value::Char(c)) = args.first() else {
            return err(format!("{name} takes a char"));
        };
        let c = *c;
        let b = |v: bool| Ok(Value::Bool(v));
        match method {
            "is_alphabetic" => b(c.is_alphabetic()),
            "is_numeric" => b(c.is_numeric()),
            "is_alphanumeric" => b(c.is_alphanumeric()),
            "is_uppercase" => b(c.is_uppercase()),
            "is_lowercase" => b(c.is_lowercase()),
            "is_whitespace" => b(c.is_whitespace()),
            "is_ascii" => b(c.is_ascii()),
            "is_ascii_digit" => b(c.is_ascii_digit()),
            "is_ascii_alphabetic" => b(c.is_ascii_alphabetic()),
            "is_ascii_alphanumeric" => b(c.is_ascii_alphanumeric()),
            "is_ascii_uppercase" => b(c.is_ascii_uppercase()),
            "is_ascii_lowercase" => b(c.is_ascii_lowercase()),
            "is_ascii_punctuation" => b(c.is_ascii_punctuation()),
            "is_ascii_whitespace" => b(c.is_ascii_whitespace()),
            // A mapping to several chars leaves the char as it is, as
            // legacy and the runtime's `single_scalar` do.
            "to_lowercase" => Ok(Value::Char(single_scalar(c.to_lowercase()).unwrap_or(c))),
            "to_uppercase" => Ok(Value::Char(single_scalar(c.to_uppercase()).unwrap_or(c))),
            "is_ascii_hexdigit" => b(c.is_ascii_hexdigit()),
            "is_digit" => match args.get(1) {
                Some(Value::Int(r)) if (2..=36).contains(r) => b(c.is_digit(*r as u32)),
                _ => err(format!("{name} takes a radix from 2 to 36")),
            },
            "to_ascii_lowercase" => Ok(Value::Char(c.to_ascii_lowercase())),
            "to_ascii_uppercase" => Ok(Value::Char(c.to_ascii_uppercase())),
            "to_digit" => {
                let radix = match args.get(1) {
                    Some(Value::Int(r)) if (2..=36).contains(r) => *r as u32,
                    _ => return err(format!("{name} takes a radix from 2 to 36")),
                };
                let d = c.to_digit(radix).map(|d| Value::Int(d as i128));
                self.option(ret, d)
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// Integer methods that depend on the receiver's width (`t`).
    /// `i8.try_from(x)` (`Err` names the target, as legacy's does),
    /// `i64.from(x)` (a widening the checker allowed), `char.try_from(n)`
    /// (`Err` carries the code point).
    fn convert_from(
        &mut self,
        name: &str,
        t: &str,
        method: &str,
        args: &[Value],
        ret: Ty,
    ) -> R<Value> {
        let n = match args {
            [Value::Int(n)] => *n,
            [Value::Bool(b)] => i128::from(*b),
            [Value::Char(c)] => i128::from(u32::from(*c)),
            _ => return err(format!("{name} takes a number")),
        };
        match (t, method) {
            ("char", _) => match u32::try_from(n).ok().and_then(char::from_u32) {
                Some(c) => self.variant_named(ret, None, "Ok", vec![Value::Char(c)]),
                None => self.variant_named(ret, None, "Err", vec![Value::Int(n)]),
            },
            (_, "from") => Ok(Value::Int(n)),
            _ => {
                if crate::numeric_conv::fits_in_target(n, t) {
                    self.variant_named(ret, None, "Ok", vec![Value::Int(n)])
                } else {
                    let msg = self.alloc_box("String", Value::Str(format!("out of range for {t}")));
                    self.variant_named(ret, None, "Err", vec![msg])
                }
            }
        }
    }

    fn int_method(
        &mut self,
        name: &str,
        t: &str,
        method: &str,
        args: &[Value],
        ret: Ty,
    ) -> R<Value> {
        let Some((bits, signed)) = int_width(t) else {
            return err(format!("{name} on {t}"));
        };
        let (lo, hi) = if signed && bits == 128 {
            (i128::MIN, i128::MAX)
        } else if signed {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else if bits == 128 {
            (0, i128::MAX)
        } else {
            (0, (1i128 << bits) - 1)
        };
        let ints: Vec<i128> = args
            .iter()
            .map(|a| match a {
                Value::Int(n) => Ok(*n),
                other => err(format!("{name} of {other:?}")),
            })
            .collect::<R<_>>()?;
        let raw = |v: i128| -> u128 {
            let shift = 128 - bits;
            (v as u128) << shift >> shift
        };
        match (method, ints.as_slice()) {
            ("trailing_zeros", [a]) => Ok(Value::Int(raw(*a).trailing_zeros().min(bits) as i128)),
            ("leading_zeros", [a]) => {
                Ok(Value::Int((raw(*a).leading_zeros() - (128 - bits)) as i128))
            }
            ("count_zeros", [a]) => Ok(Value::Int((bits - raw(*a).count_ones()) as i128)),
            ("count_ones", [a]) => Ok(Value::Int(raw(*a).count_ones() as i128)),
            ("abs_diff", [a, b]) => Ok(Value::Int((a - b).abs())),
            // `lo` when below it, else `hi` when above it, as legacy: a
            // reversed range is not an error.
            ("clamp", [a, l, h]) => Ok(Value::Int(if a < l {
                *l
            } else if a > h {
                *h
            } else {
                *a
            })),
            ("overflowing_add" | "overflowing_sub" | "overflowing_mul", [a, b]) => {
                let (r, of) = match method {
                    "overflowing_add" => a.overflowing_add(*b),
                    "overflowing_sub" => a.overflowing_sub(*b),
                    _ => a.overflowing_mul(*b),
                };
                let m = raw(r);
                let wrapped = if signed && bits < 128 && (m >> (bits - 1)) & 1 == 1 {
                    m as i128 - (1i128 << bits)
                } else {
                    m as i128
                };
                let of = of || !(lo..=hi).contains(&r);
                Ok(Value::Agg(vec![Value::Int(wrapped), Value::Bool(of)]))
            }
            ("to_ne_bytes" | "to_le_bytes" | "to_be_bytes", [a]) => {
                let n = (bits / 8) as usize;
                let le = raw(*a).to_le_bytes();
                let mut bytes: Vec<Value> =
                    le[..n].iter().map(|b| Value::Int(i128::from(*b))).collect();
                if method == "to_be_bytes" {
                    bytes.reverse();
                }
                Ok(Value::Agg(bytes))
            }
            ("checked_add" | "checked_sub" | "checked_mul", [a, b]) => {
                let r = match method {
                    "checked_add" => a.checked_add(*b),
                    "checked_sub" => a.checked_sub(*b),
                    _ => a.checked_mul(*b),
                };
                let r = r.filter(|r| (lo..=hi).contains(r)).map(Value::Int);
                self.option(ret, r)
            }
            ("saturating_add" | "saturating_sub" | "saturating_mul", [a, b]) => {
                let r = match method {
                    "saturating_add" => a.saturating_add(*b),
                    "saturating_sub" => a.saturating_sub(*b),
                    _ => a.saturating_mul(*b),
                };
                Ok(Value::Int(r.clamp(lo, hi)))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// `String` methods that read text and build new values
    /// (`design.md` § Collection Core Methods, `String`).
    fn string_text_method(
        &mut self,
        name: &str,
        method: &str,
        args: Vec<Value>,
        ret: Ty,
    ) -> R<Value> {
        let mut texts = Vec::with_capacity(args.len());
        for a in &args {
            texts.push(match a {
                Value::Int(_) => None,
                Value::Char(c) => Some(c.to_string()),
                a => Some(self.string_at(a)?),
            });
        }
        let text = |i: usize| -> R<&str> {
            texts
                .get(i)
                .and_then(|t| t.as_deref())
                .ok_or_else(|| Stop::Error(format!("{name}: argument {i} is not a string")))
        };
        let int = |i: usize| -> R<i128> {
            match args.get(i) {
                Some(Value::Int(n)) => Ok(*n),
                _ => err(format!("{name}: argument {i} is not an integer")),
            }
        };
        let new_string = |me: &mut Self, t: String| Ok(me.alloc_box("String", Value::Str(t)));
        let s = text(0)?;
        match method {
            "lt" => Ok(Value::Bool(s < text(1)?)),
            "cmp" => {
                let want = match s.cmp(text(1)?) {
                    std::cmp::Ordering::Less => "Less",
                    std::cmp::Ordering::Equal => "Equal",
                    std::cmp::Ordering::Greater => "Greater",
                };
                self.variant_named(ret, None, want, Vec::new())
            }
            "find" => {
                // The byte offset of the first occurrence of a String or
                // char needle (legacy's `str::find`).
                let at = s.find(text(1)?).map(|b| Value::Int(b as i128));
                self.option(ret, at)
            }
            "char_at" => {
                // The i-th char, `None` past the end or below zero.
                let i = int(1)?;
                let c = usize::try_from(i)
                    .ok()
                    .and_then(|i| s.chars().nth(i))
                    .map(Value::Char);
                self.option(ret, c)
            }
            "contains" => Ok(Value::Bool(s.contains(text(1)?))),
            "starts_with" => Ok(Value::Bool(s.starts_with(text(1)?))),
            "ends_with" => Ok(Value::Bool(s.ends_with(text(1)?))),
            "trim" => {
                let t = s.trim().to_string();
                new_string(self, t)
            }
            "to_uppercase" => {
                let t = s.to_uppercase();
                new_string(self, t)
            }
            "to_lowercase" => {
                let t = s.to_lowercase();
                new_string(self, t)
            }
            "replace" => {
                let t = s.replace(text(1)?, text(2)?);
                new_string(self, t)
            }
            "repeat" => {
                let t = s.repeat(int(1)?.max(0) as usize);
                new_string(self, t)
            }
            "substring" => {
                // Byte offsets, saturating to an empty String when out of
                // range or inverted; a cut inside a codepoint panics
                // (legacy's rule, B-2026-08-14-19).
                let len = s.len() as i128;
                let lo = int(1)?;
                // An end past the text is the text's end.
                let hi = if args.len() > 2 {
                    int(2)?.min(len)
                } else {
                    len
                };
                if lo < 0 || lo >= hi {
                    return new_string(self, String::new());
                }
                let (lo, hi) = (lo as usize, hi as usize);
                if !s.is_char_boundary(lo) || !s.is_char_boundary(hi) {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                }
                let t = s[lo..hi].to_string();
                new_string(self, t)
            }
            "char_at_byte" => {
                // The char starting at byte `i`, for the builder's `chars()`
                // cursor; a non-boundary or out-of-range byte panics.
                let i = int(1)?;
                let c = (0..s.len() as i128)
                    .contains(&i)
                    .then(|| s.get(i as usize..).and_then(|t| t.chars().next()))
                    .flatten();
                let Some(c) = c else {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                };
                Ok(Value::Char(c))
            }
            "index_range" => {
                // `s[a..b]`: a new String; out of range, inverted or off a
                // char boundary panics, unlike `substring`.
                let (lo, hi) = (int(1)?, int(2)?);
                let ok = 0 <= lo
                    && lo <= hi
                    && hi <= s.len() as i128
                    && s.is_char_boundary(lo as usize)
                    && s.is_char_boundary(hi as usize);
                if !ok {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Panic));
                    }
                    return Err(Stop::Abort(AbortReason::Panic));
                }
                let t = s[lo as usize..hi as usize].to_string();
                new_string(self, t)
            }
            "split" | "split_whitespace" | "lines" => {
                let parts: Vec<String> = match method {
                    "split" => s.split(text(1)?).map(str::to_string).collect(),
                    "split_whitespace" => s.split_whitespace().map(str::to_string).collect(),
                    _ => s.lines().map(str::to_string).collect(),
                };
                let mut out = Vec::with_capacity(parts.len());
                for p in parts {
                    out.push(self.alloc_box("String", Value::Str(p)));
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            "sorted" => {
                // The chars in order.
                let mut cs: Vec<char> = s.chars().collect();
                cs.sort_unstable();
                new_string(self, cs.into_iter().collect())
            }
            "trim_start" => {
                let t = s.trim_start().to_string();
                new_string(self, t)
            }
            "trim_end" => {
                let t = s.trim_end().to_string();
                new_string(self, t)
            }
            "strip_prefix" | "strip_suffix" => {
                let rest = if method == "strip_prefix" {
                    s.strip_prefix(text(1)?)
                } else {
                    s.strip_suffix(text(1)?)
                };
                let rest = rest.map(str::to_string);
                let v = match rest {
                    Some(t) => Some(new_string(self, t)?),
                    None => None,
                };
                self.option(ret, v)
            }
            "replacen" => {
                let t = s.replacen(text(1)?, text(2)?, int(3)?.max(0) as usize);
                new_string(self, t)
            }
            "char_count" => Ok(Value::Int(s.chars().count() as i128)),
            "bytes" => {
                // A read-only `Slice[u8]` over a snapshot of the bytes.
                let bytes: Vec<Value> = s.bytes().map(|b| Value::Int(b as i128)).collect();
                let n = bytes.len() as u64;
                let Value::Box(id) = self.alloc_box("bytes", Value::Agg(bytes)) else {
                    unreachable!("alloc_box makes a box")
                };
                self.snapshots.insert(id);
                Ok(Value::Slice {
                    base: Addr {
                        root: Root::Heap(id),
                        path: Vec::new(),
                    },
                    lo: 0,
                    len: n,
                })
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// Methods on a scalar receiver (by value): `max`, `min`, `abs`,
    /// `pow`, the wrapping operations, and a few float functions.
    fn scalar_method(&mut self, name: &str, method: &str, args: &[Value], ret: Ty) -> R<Value> {
        let v = self.scalar_method_wide(name, method, args, ret)?;
        Ok(narrow_float(v, self.tys.kind(ret)))
    }

    fn scalar_method_wide(
        &mut self,
        name: &str,
        method: &str,
        args: &[Value],
        ret: Ty,
    ) -> R<Value> {
        let it = match self.tys.kind(ret) {
            TyKind::Int(it) => Some(it),
            _ => None,
        };
        let fit = |me: &mut Self, v: i128| -> R<Value> {
            match it {
                Some(it) if wrap(v, it) != v => {
                    if me.trace {
                        me.events.push(Event::Abort(AbortReason::Overflow));
                    }
                    Err(Stop::Abort(AbortReason::Overflow))
                }
                _ => Ok(Value::Int(v)),
            }
        };
        match (method, args) {
            ("max", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(*a.max(b))),
            ("min", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int(*a.min(b))),
            ("max", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.max(*b))),
            ("min", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.min(*b))),
            ("clamp", [Value::Int(a), Value::Int(l), Value::Int(h)]) => Ok(Value::Int(if a < l {
                *l
            } else if a > h {
                *h
            } else {
                *a
            })),
            ("clamp", [Value::Float(a), Value::Float(l), Value::Float(h)]) => {
                Ok(Value::Float(if a < l {
                    *l
                } else if a > h {
                    *h
                } else {
                    *a
                }))
            }
            ("abs", [Value::Int(a)]) => fit(self, a.abs()),
            ("abs", [Value::Float(a)]) => Ok(Value::Float(a.abs())),
            ("signum", [Value::Int(a)]) => Ok(Value::Int(a.signum())),
            ("pow", [Value::Int(a), Value::Int(b)]) => {
                let Ok(e) = u32::try_from(*b) else {
                    return err(format!("{name}: exponent {b} out of range"));
                };
                match a.checked_pow(e) {
                    Some(v) => fit(self, v),
                    None => fit(self, i128::MAX),
                }
            }
            ("wrapping_add" | "wrapping_sub" | "wrapping_mul", [Value::Int(a), Value::Int(b)]) => {
                let Some(it) = it else {
                    return err(format!("{name} into a non-integer"));
                };
                let r = match method {
                    "wrapping_add" => a.wrapping_add(*b),
                    "wrapping_sub" => a.wrapping_sub(*b),
                    _ => a.wrapping_mul(*b),
                };
                Ok(Value::Int(wrap(r, it)))
            }
            ("is_power_of_two", [Value::Int(a)]) => Ok(Value::Bool(*a > 0 && (*a & (*a - 1)) == 0)),
            ("count_ones", [Value::Int(a)]) => {
                let bits = match self.tys.kind(ret) {
                    TyKind::Int(_) => (*a as u128 & u64::MAX as u128).count_ones(),
                    _ => (*a as u128).count_ones(),
                };
                Ok(Value::Int(bits as i128))
            }
            ("rem_euclid" | "div_euclid", [Value::Int(a), Value::Int(b)]) => {
                if *b == 0 {
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::DivByZero));
                    }
                    return Err(Stop::Abort(AbortReason::DivByZero));
                }
                if method == "rem_euclid" {
                    Ok(Value::Int(a.rem_euclid(*b)))
                } else {
                    fit(self, a.div_euclid(*b))
                }
            }
            ("rem_euclid", [Value::Float(a), Value::Float(b)]) => {
                Ok(Value::Float(a.rem_euclid(*b)))
            }
            ("div_euclid", [Value::Float(a), Value::Float(b)]) => {
                Ok(Value::Float(a.div_euclid(*b)))
            }
            (_, [Value::Float(a)]) if float_unary(method, *a, true).is_some() => {
                // Computed at the receiver's width, as legacy and codegen do:
                // an `f32` (or narrower) goes through the `f32` function.
                let narrow = !matches!(self.tys.kind(ret), TyKind::Float(super::ty::FloatTy::F64));
                Ok(Value::Float(float_unary(method, *a, !narrow).unwrap()))
            }
            (_, [Value::Float(a), Value::Float(b)])
                if matches!(method, "pow" | "atan2" | "hypot" | "copysign") =>
            {
                let wide = matches!(self.tys.kind(ret), TyKind::Float(super::ty::FloatTy::F64));
                let (x, y) = (*a, *b);
                Ok(Value::Float(if wide {
                    match method {
                        "pow" => x.powf(y),
                        "atan2" => x.atan2(y),
                        "hypot" => x.hypot(y),
                        _ => x.copysign(y),
                    }
                } else {
                    let (x, y) = (x as f32, y as f32);
                    (match method {
                        "pow" => x.powf(y),
                        "atan2" => x.atan2(y),
                        "hypot" => x.hypot(y),
                        _ => x.copysign(y),
                    }) as f64
                }))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// The elements of the `Vec` allocation `id`.
    fn vec_elems(&mut self, id: AllocId) -> R<&mut Vec<Value>> {
        match &mut self.live(id)?.value {
            Value::Agg(fs) => Ok(fs),
            other => err(format!("a Vec allocation holds {other:?}")),
        }
    }

    /// `T` for a method whose receiver is a reference to a `Vec[T]`.
    fn vec_elem_ty(&self, arg_tys: &[Ty], name: &str) -> R<Ty> {
        let mut t = *arg_tys
            .first()
            .ok_or_else(|| Stop::Error(format!("{name} needs its receiver's type")))?;
        while let TyKind::Ref(inner) | TyKind::MutRef(inner) = self.tys.kind(t) {
            t = inner;
        }
        match self.tys.kind(t) {
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) | TyKind::Slice(e) | TyKind::Array(e, _) => {
                Ok(e)
            }
            _ => err(format!("{name} on {}", self.tys.display(t))),
        }
    }

    /// Checks `i` against the length of `id` (one past the end too when
    /// `may_append`); out of bounds aborts, as the index operator does.
    fn bounds(&mut self, id: AllocId, i: i128, may_append: bool) -> R<usize> {
        let n = self.vec_elems(id)?.len() as i128;
        let limit = if may_append { n + 1 } else { n };
        if (0..limit).contains(&i) {
            Ok(i as usize)
        } else {
            if self.trace {
                self.events.push(Event::Abort(AbortReason::BoundsCheck));
            }
            Err(Stop::Abort(AbortReason::BoundsCheck))
        }
    }

    /// The text of a `String` (owned or behind a reference) or a constant.
    fn string_at(&mut self, v: &Value) -> R<String> {
        if let Value::Str(s) = v {
            return Ok(s.clone());
        }
        let id = self.box_behind(v)?;
        match &self.live(id)?.value {
            Value::Str(s) => Ok(s.clone()),
            other => err(format!("expected a String, found {other:?}")),
        }
    }

    /// `Some` of the element a lookup found, or `None`: a reference to it
    /// when `ret`'s payload is a reference, else a copy (a lookup typed
    /// `Option[V]` hands back its own value).
    fn option_of_place(&mut self, ret: Ty, found: Option<Value>) -> R<Value> {
        let Some(Value::Ref(at)) = found else {
            return self.option(ret, found);
        };
        let payload = match self.tys.kind(ret) {
            TyKind::Adt(a) => {
                let some = self
                    .tys
                    .adt(a)
                    .variants
                    .iter()
                    .position(|v| v.name == "Some");
                some.and_then(|k| self.tys.field_ty(ret, Some(k as u32), 0))
            }
            _ => None,
        };
        match payload.map(|t| (t, self.tys.kind(t))) {
            Some((_, TyKind::Ref(_) | TyKind::MutRef(_))) | None => {
                self.option(ret, Some(Value::Ref(at)))
            }
            // A `weak` element read as the `shared` type it points at
            // (`Map[K, weak N].get`): upgraded, so a dead target reads as
            // `None` (core semantics §6.5).
            Some((_, TyKind::Shared(_))) if matches!(self.slot(&at)?, Value::Weak(_)) => {
                let Value::Weak(id) = self.slot(&at)? else {
                    unreachable!()
                };
                if id == EMPTY_WEAK {
                    return self.option(ret, None);
                }
                let obj = self.live(id)?;
                let alive = obj.count > 0;
                if alive {
                    obj.count += 1;
                }
                self.option(ret, alive.then_some(Value::Shared(id)))
            }
            Some((t, _)) => {
                let v = self.slot(&at)?;
                let copy = self.clone_value(&v, t)?;
                self.option(ret, Some(copy))
            }
        }
    }

    /// The variant `want` of the enum `ty`, holding `fields`. With
    /// `inside`, the enum is instead the type of field 0 of `ty`'s variant
    /// `inside` (the `E` of a `Result[T, E]`'s `Err`).
    fn variant_named(
        &self,
        ty: Ty,
        inside: Option<&str>,
        want: &str,
        fields: Vec<Value>,
    ) -> R<Value> {
        let mut ty = ty;
        if let Some(outer) = inside {
            let TyKind::Adt(a) = self.tys.kind(ty) else {
                return err(format!("expected an enum, found {}", self.tys.display(ty)));
            };
            let k = self
                .tys
                .adt(a)
                .variants
                .iter()
                .position(|v| v.name == outer);
            ty = k
                .and_then(|k| self.tys.field_ty(ty, Some(k as u32), 0))
                .ok_or_else(|| {
                    Stop::Error(format!("{} has no {outer} payload", self.tys.display(ty)))
                })?;
        }
        let TyKind::Adt(a) = self.tys.kind(ty) else {
            return err(format!("expected an enum, found {}", self.tys.display(ty)));
        };
        let adt = self.tys.adt(a);
        let Some(idx) = adt.variants.iter().position(|var| var.name == want) else {
            return err(format!("{} has no variant {want}", adt.name));
        };
        Ok(Value::Variant(idx as u32, fields))
    }

    /// The type of the one payload of variant `want` of enum `ty`.
    fn payload_ty(&self, ty: Ty, want: &str) -> R<Ty> {
        let TyKind::Adt(a) = self.tys.kind(ty) else {
            return err(format!("expected an enum, found {}", self.tys.display(ty)));
        };
        let k = self.tys.adt(a).variants.iter().position(|v| v.name == want);
        k.and_then(|k| self.tys.field_ty(ty, Some(k as u32), 0))
            .ok_or_else(|| Stop::Error(format!("{} has no {want} payload", self.tys.display(ty))))
    }

    /// `Some(v)` or `None` in the `Option` type `ret`, by variant name.
    fn option(&self, ret: Ty, v: Option<Value>) -> R<Value> {
        let TyKind::Adt(a) = self.tys.kind(ret) else {
            return err(format!(
                "expected an Option, found {}",
                self.tys.display(ret)
            ));
        };
        let adt = self.tys.adt(a);
        let want = if v.is_some() { "Some" } else { "None" };
        let Some(idx) = adt.variants.iter().position(|var| var.name == want) else {
            return err(format!("{} has no variant {want}", adt.name));
        };
        Ok(Value::Variant(idx as u32, v.into_iter().collect()))
    }

    fn alloc_box(&mut self, ty_name: &str, value: Value) -> Value {
        let a = self.alloc(HeapObj {
            count: 1,
            weak: 0,
            value,
        });
        if self.trace {
            self.events.push(Event::Alloc(a, ty_name.to_string()));
        }
        Value::Box(a)
    }

    /// The allocation a library value, or a reference to one, owns.
    fn box_behind(&mut self, v: &Value) -> R<AllocId> {
        match v {
            Value::Box(a) => Ok(*a),
            // A reference to a reference (`ref ref Vec`, a `ref` binding
            // of a `ref` element) reaches the same box.
            Value::Ref(addr) => {
                let inner = self.slot(addr)?;
                match inner {
                    Value::Box(a) => Ok(a),
                    Value::Ref(_) => self.box_behind(&inner),
                    other => err(format!("expected a library value, found {other:?}")),
                }
            }
            other => err(format!("expected a library value, found {other:?}")),
        }
    }

    /// The arguments of a print or an f-string, shown one after another,
    /// by type when the call site's types are known.
    fn show(&mut self, args: &[Value], tys: &[Ty]) -> R<String> {
        let mut text = String::new();
        for (i, v) in args.iter().enumerate() {
            let shown = match tys.get(i) {
                Some(&t) if tys.len() == args.len() => self.display_typed(v, t)?,
                _ => self.display(v)?,
            };
            text.push_str(&shown);
        }
        Ok(text)
    }

    /// `v` shown as legacy prints it: sequences as `[a, b]` with their
    /// elements shown the same way, tuples as `(a, b)`, an enum value as
    /// its variant's name with its fields in parentheses. A struct needs
    /// its `Display` body, which is a call in MIR.
    fn display_typed(&mut self, v: &Value, ty: Ty) -> R<String> {
        let list = |me: &mut Self, items: &[Value], tys: &[Ty]| -> R<Vec<String>> {
            items
                .iter()
                .zip(tys)
                .map(|(x, &t)| me.display_typed(x, t))
                .collect()
        };
        Ok(match (self.tys.kind(ty), v) {
            (TyKind::Int(IntTy::U128), Value::Int(i)) => (*i as u128).to_string(),
            (TyKind::Ref(t) | TyKind::MutRef(t), Value::Ref(addr)) => {
                let inner = self.slot(addr)?;
                if !inner.fully_init() {
                    return err("print through a reference to an uninitialized value");
                }
                self.display_typed(&inner, t)?
            }
            (TyKind::Array(e, _), Value::Agg(fs)) => {
                let tys = vec![e; fs.len()];
                let shown = list(self, fs, &tys)?.join(", ");
                // A lane vector names itself, as both legacy backends print it.
                match self.tys.is_vector(ty) {
                    true => format!("Vector({shown})"),
                    false => format!("[{shown}]"),
                }
            }
            (TyKind::Intrinsic(IntrinsicTy::Vec(e) | IntrinsicTy::VecDeque(e)), Value::Box(id)) => {
                let fs = self.vec_elems(*id)?.clone();
                let tys = vec![e; fs.len()];
                format!("[{}]", list(self, &fs, &tys)?.join(", "))
            }
            (TyKind::Tuple(ts), Value::Agg(fs)) => {
                format!("({})", list(self, fs, &ts)?.join(", "))
            }
            (TyKind::Slice(e), Value::Slice { base, lo, len }) => {
                let mut fs = Vec::with_capacity(*len as usize);
                for i in *lo..*lo + *len {
                    fs.push(self.slot(&base.child(i))?);
                }
                let tys = vec![e; fs.len()];
                format!("[{}]", list(self, &fs, &tys)?.join(", "))
            }
            (TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String), _) if self.debug_render => {
                format!("{:?}", self.display(v)?)
            }
            (TyKind::Char, _) if self.debug_render => {
                let shown = self.display(v)?;
                match shown.chars().next() {
                    Some(c) if shown.chars().count() == 1 => format!("{c:?}"),
                    _ => format!("{shown:?}"),
                }
            }
            (
                TyKind::Intrinsic(IntrinsicTy::Map(kt, vt) | IntrinsicTy::SortedMap(kt, vt)),
                Value::Box(id),
            ) => {
                // `{k: v, ...}` in iteration order, with no type name (the
                // 2026-10-08 display decision).
                let entries = self.vec_elems(*id)?.clone();
                let mut parts = Vec::with_capacity(entries.len());
                for e in &entries {
                    let Value::Agg(pair) = e else {
                        return err("a malformed map entry");
                    };
                    let k = self.display_typed(&pair[0], kt)?;
                    let v = self.display_typed(&pair[1], vt)?;
                    parts.push(format!("{k}: {v}"));
                }
                format!("{{{}}}", parts.join(", "))
            }
            (
                TyKind::Intrinsic(IntrinsicTy::Set(e) | IntrinsicTy::SortedSet(e)),
                Value::Box(id),
            ) => {
                let fs = self.vec_elems(*id)?.clone();
                let tys = vec![e; fs.len()];
                format!("{{{}}}", list(self, &fs, &tys)?.join(", "))
            }
            (TyKind::Adt(a) | TyKind::Shared(a), _)
                if self.program.display_styles.get(&a) == Some(&DisplayStyle::Redacted) =>
            {
                "<redacted>".to_string()
            }
            (TyKind::Adt(_) | TyKind::Shared(_), _)
                if !self.debug_render && self.user_display(ty).is_some() =>
            {
                // A user `Display` wins at every depth (legacy B-2026-08-26-29).
                let name = self.user_display(ty).unwrap();
                self.call_display(&name, v)?
            }
            (TyKind::Adt(a), Value::Agg(fs))
                if self.program.display_styles.get(&a) == Some(&DisplayStyle::Transparent) =>
            {
                let ft = self.tys.adt(a).variants[0].fields[0].1;
                self.display_typed(&fs[0], ft)?
            }
            (TyKind::Shared(_), Value::Shared(id)) => {
                // A derived Display shows the value behind the handle.
                let inner = self.live(*id)?.value.clone();
                let TyKind::Shared(a) = self.tys.kind(ty) else {
                    unreachable!("matched above")
                };
                let adt = self.tys.adt(a);
                self.display_adt(&adt, ty, &inner)?
            }
            (TyKind::Adt(a), Value::Agg(_)) => {
                let adt = self.tys.adt(a);
                self.display_adt(&adt, ty, v)?
            }
            (TyKind::Adt(a), Value::Variant(k, fs))
                if !self.tys.adt(a).is_enum || {
                    let adt = self.tys.adt(a);
                    adt.variants.get(*k as usize).is_some_and(|var| {
                        var.fields.iter().any(|(n, _)| n.parse::<u32>().is_err())
                    })
                } =>
            {
                let adt = self.tys.adt(a);
                self.display_adt(&adt, ty, &Value::Variant(*k, fs.clone()))?
            }
            (TyKind::Adt(a), Value::Variant(k, fs)) => {
                let adt = self.tys.adt(a);
                let Some(var) = adt.variants.get(*k as usize) else {
                    return err(format!("{} has no variant {k}", adt.name));
                };
                let vname = if self.program.display_styles.get(&a) == Some(&DisplayStyle::SnakeCase)
                {
                    crate::interpreter::pascal_to_snake(&var.name)
                } else {
                    var.name.clone()
                };
                if fs.is_empty() {
                    vname
                } else {
                    let mut tys = Vec::with_capacity(fs.len());
                    for i in 0..fs.len() {
                        let Some(t) = self.tys.field_ty(ty, Some(*k), i as u32) else {
                            return err(format!("{}.{vname} has no field {i}", adt.name));
                        };
                        tys.push(t);
                    }
                    format!("{vname}({})", list(self, fs, &tys)?.join(", "))
                }
            }
            _ => self.display(v)?,
        })
    }

    /// The lowered body of `ty`'s user `Display`, if it has one. Lowering
    /// queues it for every type a printed value contains.
    fn user_display(&self, ty: Ty) -> Option<String> {
        let name = format!("{}.to_string", self.tys.adt_name(ty));
        self.program.bodies.contains_key(&name).then_some(name)
    }

    /// `v` shown by its type's user `to_string` body, which takes
    /// it by reference: a scratch slot holds a shallow copy for the call.
    fn call_display(&mut self, name: &str, v: &Value) -> R<String> {
        let scratch = self
            .alloc(HeapObj {
                count: 1,
                weak: 0,
                value: v.clone(),
            })
            .0 as usize;
        let at = Addr {
            root: Root::Heap(AllocId(scratch as u32)),
            path: Vec::new(),
        };
        let shown = self.call(name, vec![Value::Ref(at)]);
        // The copy shares `v`'s allocations, so it goes without a drop.
        self.free_slot(AllocId(scratch as u32));
        let shown = shown?;
        let text = self.string_at(&shown)?;
        let st = self.program.bodies[name].locals[0].ty;
        self.drop_value(shown, st)?;
        Ok(text)
    }

    /// A struct, or an enum variant with named fields, as a derived
    /// `Display` shows it: `Name { f: v, g: w }`, `Name(a, b)` for
    /// positional fields, `Name` alone with none. `ty` is the ADT type,
    /// possibly `shared`.
    fn display_adt(&mut self, adt: &crate::mir::ty::AdtDef, ty: Ty, v: &Value) -> R<String> {
        let (k, fs, name) = match v {
            Value::Agg(fs) if !adt.is_enum => (None, fs.clone(), adt.name.clone()),
            Value::Variant(k, fs) => {
                let Some(var) = adt.variants.get(*k as usize) else {
                    return err(format!("{} has no variant {k}", adt.name));
                };
                (Some(*k), fs.clone(), var.name.clone())
            }
            other => return err(format!("print cannot show {other:?} as {}", adt.name)),
        };
        if fs.is_empty() {
            return Ok(name);
        }
        if k.is_none() && matches!(name.as_str(), "F64" | "F32") && fs.len() == 1 {
            // The total-order float wrappers show as the float they hold.
            if let Some(t) = self.tys.field_ty(ty, None, 0) {
                return self.display_typed(&fs[0], t);
            }
        }
        let var = &adt.variants[k.unwrap_or(0) as usize];
        let mut parts = Vec::with_capacity(fs.len());
        for (i, f) in fs.iter().enumerate() {
            let Some(t) = self.tys.field_ty(ty, k, i as u32) else {
                return err(format!("{name} has no field {i}"));
            };
            let shown = self.display_typed(f, t)?;
            match var.fields.get(i).map(|(n, _)| n.as_str()) {
                Some(n) if n.parse::<u32>().is_err() => parts.push(format!("{n}: {shown}")),
                _ => parts.push(shown),
            }
        }
        let named = var.fields.iter().any(|(n, _)| n.parse::<u32>().is_err());
        Ok(if named {
            format!("{name} {{ {} }}", parts.join(", "))
        } else {
            format!("{name}({})", parts.join(", "))
        })
    }

    /// How `print` shows a value when its type is not known.
    fn display(&mut self, v: &Value) -> R<String> {
        Ok(match v {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Char(c) => c.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Unit => "()".to_string(),
            Value::Ref(addr) => {
                let inner = self.slot(addr)?;
                if !inner.fully_init() {
                    return err("print through a reference to an uninitialized value");
                }
                self.display(&inner)?
            }
            Value::Box(a) => match &self.live(*a)?.value {
                Value::Str(s) => s.clone(),
                other => return err(format!("print cannot show {other:?}")),
            },
            other => return err(format!("print cannot show {other:?}")),
        })
    }

    fn run_body(&mut self, body: &'a Body) -> R<Value> {
        let strict = body.phase >= MirPhase::DropsElaborated;
        let mut bb = BasicBlock(0);
        loop {
            let block = body.block(bb);
            for (i, st) in block.statements.iter().enumerate() {
                self.tick()?;
                self.statement(body, &st.kind)
                    .map_err(|s| locate(s, body, format!("{bb}[{i}]")))?;
            }
            self.tick()?;
            let next = self
                .terminator(body, &block.terminator.kind, strict)
                .map_err(|s| locate(s, body, format!("{bb}[term]")))?;
            match next {
                Some(b) => bb = b,
                None => {
                    return self
                        .finish_frame(body)
                        .map_err(|s| locate(s, body, format!("{bb}[term]")));
                }
            }
        }
    }

    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.max_steps {
            return err("step limit exceeded");
        }
        Ok(())
    }

    /// At `Return`: every local except `_0` must have been dropped or
    /// moved, and `_0` must be initialized.
    fn finish_frame(&mut self, body: &Body) -> R<Value> {
        let frame = self.frames.last().expect("a frame is running");
        for (i, v) in frame.locals.iter().enumerate().skip(1) {
            let ty = body.locals[i].ty;
            if self.owns_drop(v, ty) {
                return err(format!(
                    "_{i}: {} still owns a value at return; a drop is missing",
                    self.tys.display(ty)
                ));
            }
        }
        let ret = frame.locals[0].clone();
        if !ret.fully_init() {
            return err("return with _0 not initialized");
        }
        Ok(ret)
    }

    fn place_str(&self, body: &Body, p: &Place) -> String {
        pretty::place(body, self.tys, p)
    }

    // ---- memory ----

    fn root_mut(&mut self, root: &Root) -> R<&mut Value> {
        match *root {
            Root::Local { frame, id, local } => match self.frames.get_mut(frame) {
                Some(f) if f.id == id => Ok(&mut f.locals[local.index()]),
                _ => err("dangling reference to a local of a function that returned"),
            },
            Root::Heap(a) => match self.heap.get_mut(a.0 as usize) {
                Some(Some(obj)) => Ok(&mut obj.value),
                _ => err(format!("use of {a} after it was freed")),
            },
            Root::Static(i) => match self.statics.get_mut(i as usize) {
                Some(v) => Ok(v),
                None => err(format!("use of static#{i} before its initializer ran")),
            },
        }
    }

    fn slot_mut(&mut self, addr: &Addr) -> R<&mut Value> {
        let mut v = self.root_mut(&addr.root)?;
        for &i in &addr.path {
            v = match v {
                Value::Agg(fs) | Value::Variant(_, fs) => {
                    let n = fs.len();
                    match fs.get_mut(i as usize) {
                        Some(x) => x,
                        None => return err(format!("index {i} out of bounds for length {n}")),
                    }
                }
                Value::Uninit => return err("projection into an uninitialized value"),
                other => return err(format!("projection into non-aggregate {other:?}")),
            };
        }
        Ok(v)
    }

    fn slot(&mut self, addr: &Addr) -> R<Value> {
        Ok(self.slot_mut(addr)?.clone())
    }

    fn n_fields(&self, ty: Ty, variant: Option<u32>) -> usize {
        match self.tys.kind(ty) {
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => ts.len(),
            TyKind::Array(_, n) => n as usize,
            TyKind::Adt(a) | TyKind::Shared(a) => self
                .tys
                .adt(a)
                .variants
                .get(variant.unwrap_or(0) as usize)
                .map_or(0, |v| v.fields.len()),
            _ => 0,
        }
    }

    /// The address and type of `place` in the current frame.
    fn resolve(&mut self, body: &Body, place: &Place, mode: Mode) -> R<Option<(Addr, Ty)>> {
        let fi = self.frames.len() - 1;
        let mut addr = Addr {
            root: Root::Local {
                frame: fi,
                id: self.frames[fi].id,
                local: place.local,
            },
            path: Vec::new(),
        };
        let mut ty = body.local(place.local).ty;
        let mut variant: Option<u32> = None;
        // A downcast of a `shared enum` follows the handle and keeps the
        // type, so the field after it must not follow it again.
        let mut behind_handle = false;
        let tys = self.tys;
        let uninit = |what: &str| -> R<Option<(Addr, Ty)>> {
            if mode == Mode::Probe {
                Ok(None)
            } else {
                err(format!(
                    "{what} of {} goes through an uninitialized value",
                    pretty::place(body, tys, place)
                ))
            }
        };
        for elem in &place.projection {
            if matches!(elem, ProjElem::Field(..) | ProjElem::Downcast(_))
                && matches!(self.tys.kind(ty), TyKind::Shared(_))
                && !behind_handle
            {
                match self.slot(&addr)? {
                    Value::Shared(a) => {
                        addr = Addr {
                            root: Root::Heap(a),
                            path: Vec::new(),
                        };
                        behind_handle = true;
                    }
                    Value::Uninit => return uninit("projection"),
                    other => return err(format!("expected a shared handle, found {other:?}")),
                }
            }
            match *elem {
                ProjElem::Field(f, fty) => {
                    let n = self.n_fields(ty, variant);
                    let slot = self.slot_mut(&addr)?;
                    if *slot == Value::Uninit {
                        if mode != Mode::Write || variant.is_some() {
                            return uninit("field access");
                        }
                        *slot = Value::Agg(vec![Value::Uninit; n]);
                    }
                    addr.path.push(f.0 as u64);
                    ty = fty;
                    variant = None;
                    behind_handle = false;
                }
                ProjElem::Downcast(v) => {
                    let n = self.n_fields(ty, Some(v.0));
                    let slot = self.slot_mut(&addr)?;
                    match slot {
                        Value::Variant(cur, _) if *cur == v.0 => {}
                        Value::Variant(cur, _) => {
                            let cur = *cur;
                            return err(format!(
                                "downcast to variant {} of a value holding variant {cur}",
                                v.0
                            ));
                        }
                        Value::Uninit if mode == Mode::Write => {
                            *slot = Value::Variant(v.0, vec![Value::Uninit; n]);
                        }
                        Value::Uninit => return uninit("downcast"),
                        other => return err(format!("downcast of non-enum value {other:?}")),
                    }
                    variant = Some(v.0);
                }
                ProjElem::Deref => {
                    match self.slot(&addr)? {
                        Value::Ref(a) => addr = a,
                        Value::Uninit => return uninit("deref"),
                        other => return err(format!("deref of non-reference {other:?}")),
                    }
                    ty = match self.tys.kind(ty) {
                        TyKind::Ref(t) | TyKind::MutRef(t) | TyKind::RawPtr { pointee: t, .. } => t,
                        _ => return err("deref of a non-reference type"),
                    };
                    behind_handle = false;
                }
                ProjElem::Index(_) | ProjElem::ConstIndex(_) => {
                    let i = match *elem {
                        ProjElem::Index(l) => match self.frames[fi].locals[l.index()] {
                            Value::Int(i) => i as u64,
                            _ => {
                                return err(format!(
                                    "index local {l} is not an initialized integer"
                                ))
                            }
                        },
                        ProjElem::ConstIndex(i) => i,
                        _ => unreachable!(),
                    };
                    addr.path.push(i);
                    ty = match self.tys.kind(ty) {
                        TyKind::Array(e, _) | TyKind::Slice(e) => e,
                        _ => return err("index into a non-array type"),
                    };
                    behind_handle = false;
                    // Bounds are checked when the slot is reached.
                    self.slot_mut(&addr)?;
                }
            }
        }
        Ok(Some((addr, ty)))
    }

    fn resolve_read(&mut self, body: &Body, place: &Place) -> R<(Addr, Ty)> {
        Ok(self
            .resolve(body, place, Mode::Read)?
            .expect("read mode never probes"))
    }

    // ---- statements and rvalues ----

    fn statement(&mut self, body: &Body, s: &StatementKind) -> R<()> {
        match s {
            StatementKind::Assign(p, rv) => {
                let v = self.rvalue(body, rv)?;
                let (addr, ty) = self
                    .resolve(body, p, Mode::Write)?
                    .expect("write mode never probes");
                let old = self.slot(&addr)?;
                if self.owns_drop(&old, ty) {
                    let what = pretty::place(body, self.tys, p);
                    return err(format!(
                        "assignment overwrites {what}, which still owns a value"
                    ));
                }
                *self.slot_mut(&addr)? = v.clone();
                if p.projection.is_empty() && body.local(p.local).kind == LocalKind::DropFlag {
                    if self.trace {
                        self.events
                            .push(Event::Flag(p.local, matches!(v, Value::Bool(true))));
                    }
                } else {
                    if self.trace {
                        self.events.push(Event::Init(self.place_str(body, p)));
                    }
                }
                Ok(())
            }
            StatementKind::StorageLive(l) => {
                let slot = &mut self.frames.last_mut().expect("frame").locals[l.index()];
                *slot = Value::Uninit;
                Ok(())
            }
            StatementKind::StorageDead(l) => {
                let ty = body.local(*l).ty;
                let slot = &self.frames.last().expect("frame").locals[l.index()];
                if self.owns_drop(slot, ty) {
                    return err(format!("StorageDead({l}) while it still owns a value"));
                }
                self.frames.last_mut().expect("frame").locals[l.index()] = Value::Uninit;
                Ok(())
            }
            StatementKind::SetDiscriminant(p, v) => {
                let (addr, ty) = self
                    .resolve(body, p, Mode::Write)?
                    .expect("write mode never probes");
                let n = self.n_fields(ty, Some(v.0));
                let slot = self.slot_mut(&addr)?;
                match slot {
                    Value::Variant(cur, _) if *cur == v.0 => {}
                    Value::Uninit => *slot = Value::Variant(v.0, vec![Value::Uninit; n]),
                    _ => return err("set_discriminant over a different live variant"),
                }
                Ok(())
            }
            StatementKind::BorrowFlag(op) => self.borrow_flag(body, op),
            StatementKind::Nop => Ok(()),
        }
    }

    /// §6.2: a conflicting access through another handle panics.
    fn borrow_flag(&mut self, body: &Body, op: &FlagOp) -> R<()> {
        match op {
            FlagOp::Acquire { place, kind, loan } => {
                let (addr, _) = self.resolve_read(body, place)?;
                // A loan made again while it is held, as in a loop, gives
                // back what it held first.
                let again = self
                    .frames
                    .last_mut()
                    .expect("frame")
                    .held
                    .iter_mut()
                    .find(|(l, _)| l == loan)
                    .and_then(|(_, h)| h.iter().position(|(a, _)| *a == addr).map(|j| h.remove(j)));
                if let Some(old) = again {
                    self.release_flags(vec![old]);
                }
                self.flag_conflict(&addr, *kind)?;
                let flag = match self.flags.iter().position(|(a, _)| *a == addr) {
                    Some(i) => &mut self.flags[i].1,
                    None => {
                        self.flags.push((addr.clone(), Flag::default()));
                        &mut self.flags.last_mut().expect("pushed").1
                    }
                };
                match kind {
                    BorrowKind::Shared => flag.readers += 1,
                    BorrowKind::Mut => flag.writer = true,
                }
                let frame = self.frames.last_mut().expect("frame");
                match frame.held.iter_mut().find(|(l, _)| l == loan) {
                    Some((_, v)) => v.push((addr, *kind)),
                    None => frame.held.push((*loan, vec![(addr, *kind)])),
                }
                Ok(())
            }
            FlagOp::Release { loan } => {
                let frame = self.frames.last_mut().expect("frame");
                if let Some(i) = frame.held.iter().position(|(l, _)| l == loan) {
                    let (_, flags) = frame.held.remove(i);
                    self.release_flags(flags);
                }
                Ok(())
            }
            FlagOp::Check { place, kind } => {
                // A drop of a field already moved or never set touches no
                // flag.
                let Some((addr, _)) = self.resolve(body, place, Mode::Probe)? else {
                    return Ok(());
                };
                self.flag_conflict(&addr, *kind)
            }
        }
    }

    /// Panics when the flag at `addr` is held in a way `kind` conflicts with.
    fn flag_conflict(&mut self, addr: &Addr, kind: BorrowKind) -> R<()> {
        let Some((_, f)) = self.flags.iter().find(|(a, _)| a == addr) else {
            return Ok(());
        };
        let conflict = match kind {
            BorrowKind::Shared => f.writer,
            BorrowKind::Mut => f.writer || f.readers > 0,
        };
        if conflict {
            if self.trace {
                self.events.push(Event::Abort(AbortReason::Panic));
            }
            return Err(Stop::Abort(AbortReason::Panic));
        }
        Ok(())
    }

    fn release_flags(&mut self, flags: Vec<(Addr, BorrowKind)>) {
        for (addr, kind) in flags {
            if let Some(i) = self.flags.iter().position(|(a, _)| *a == addr) {
                let f = &mut self.flags[i].1;
                match kind {
                    BorrowKind::Shared => f.readers = f.readers.saturating_sub(1),
                    BorrowKind::Mut => f.writer = false,
                }
                if f.readers == 0 && !f.writer {
                    self.flags.remove(i);
                }
            }
        }
    }

    fn operand(&mut self, body: &Body, o: &Operand) -> R<(Value, Ty)> {
        match o {
            Operand::Copy(p) | Operand::Move(p) => {
                let (addr, ty) = self.resolve_read(body, p)?;
                let v = self.slot(&addr)?;
                if !v.fully_init() {
                    return err(format!("read of uninitialized {}", self.place_str(body, p)));
                }
                if let Operand::Move(_) = o {
                    *self.slot_mut(&addr)? = Value::Uninit;
                    if self.trace {
                        self.events.push(Event::Move(self.place_str(body, p)));
                    }
                }
                Ok((v, ty))
            }
            Operand::Const(c) => Ok((self.constant(c)?, c.ty)),
        }
    }

    fn constant(&self, c: &Const) -> R<Value> {
        Ok(match &c.kind {
            ConstKind::Scalar(v) => match self.tys.kind(c.ty) {
                TyKind::Bool => Value::Bool(*v != 0),
                TyKind::Char => match char::from_u32(*v as u32) {
                    Some(ch) => Value::Char(ch),
                    None => return err(format!("invalid char constant {v}")),
                },
                TyKind::Int(it) => Value::Int(from_bits(*v, it)),
                _ => return err("scalar constant of a non-scalar type"),
            },
            ConstKind::Float(bits) => {
                narrow_float(Value::Float(f64::from_bits(*bits)), self.tys.kind(c.ty))
            }
            ConstKind::Str(s) => Value::Str(s.clone()),
            ConstKind::Unit | ConstKind::ZeroSized => Value::Unit,
            ConstKind::FnDef(inst) => Value::Fn(inst.clone()),
            ConstKind::Static(i) => Value::Ref(Addr {
                root: Root::Static(*i),
                path: Vec::new(),
            }),
        })
    }

    fn rvalue(&mut self, body: &Body, rv: &Rvalue) -> R<Value> {
        match rv {
            Rvalue::Use(o) => Ok(self.operand(body, o)?.0),
            Rvalue::Ref(_, p) => Ok(Value::Ref(self.resolve_read(body, p)?.0)),
            Rvalue::Retain(p) => {
                let (addr, _) = self.resolve_read(body, p)?;
                let Value::Shared(a) = self.slot(&addr)? else {
                    return err("retain of a place that holds no shared handle");
                };
                let obj = self.live(a)?;
                obj.count += 1;
                let c = obj.count;
                if self.trace {
                    self.events.push(Event::Retain(a, c));
                }
                Ok(Value::Shared(a))
            }
            Rvalue::BinaryOp(op, a, b) => {
                let (x, ty) = self.operand(body, a)?;
                let (y, _) = self.operand(body, b)?;
                let (v, overflow) = self.binop(*op, x, y, ty)?;
                if overflow {
                    return err(format!(
                        "unchecked {} overflowed; the builder must use CheckedBinaryOp",
                        op.name()
                    ));
                }
                Ok(v)
            }
            Rvalue::CheckedBinaryOp(op, a, b) => {
                let (x, ty) = self.operand(body, a)?;
                let (y, _) = self.operand(body, b)?;
                let (v, overflow) = self.binop(*op, x, y, ty)?;
                Ok(Value::Agg(vec![v, Value::Bool(overflow)]))
            }
            Rvalue::UnaryOp(op, a) => {
                let (x, ty) = self.operand(body, a)?;
                self.unop(*op, x, ty)
            }
            Rvalue::Cast(CastKind::Erase, o, _) => {
                let (x, from) = self.operand(body, o)?;
                self.erase(x, from)
            }
            // A raw pointer is the address the reference held.
            Rvalue::Cast(CastKind::RefToPtr | CastKind::PtrToPtr, o, _) => {
                Ok(self.operand(body, o)?.0)
            }
            Rvalue::Cast(CastKind::Downgrade, o, _) => {
                // A new weak handle to the value a `ref shared T`,
                // `ref weak T` or `ref Option[shared T]` reaches; `None`
                // gives the empty weak handle.
                let (x, _) = self.operand(body, o)?;
                let id = match self.handle_behind(&x)? {
                    Some(id) => id,
                    None => return Ok(Value::Weak(EMPTY_WEAK)),
                };
                if id != EMPTY_WEAK {
                    self.live(id)?.weak += 1;
                }
                Ok(Value::Weak(id))
            }
            Rvalue::Cast(CastKind::Upgrade, o, to) => {
                // `Some` of a new strong handle while the value lives.
                let (x, _) = self.operand(body, o)?;
                let id = self.handle_behind(&x)?.unwrap_or(EMPTY_WEAK);
                if id == EMPTY_WEAK {
                    return self.option(*to, None);
                }
                let obj = self.live(id)?;
                let alive = obj.count > 0;
                if alive {
                    obj.count += 1;
                }
                self.option(*to, alive.then_some(Value::Shared(id)))
            }
            Rvalue::Cast(kind, o, to) => {
                let (x, from) = self.operand(body, o)?;
                let from_u128 = self.tys.kind(from) == TyKind::Int(IntTy::U128);
                let to_kind = self.tys.kind(*to).clone();
                match (kind, x, to_kind) {
                    (CastKind::IntToInt, Value::Int(i), TyKind::Int(it)) => {
                        Ok(Value::Int(wrap(i, it)))
                    }
                    (CastKind::IntToFloat, Value::Int(i), TyKind::Float(ft)) => {
                        let f = if from_u128 {
                            i as u128 as f64
                        } else {
                            i as f64
                        };
                        Ok(Value::Float(ft.round(f)))
                    }
                    (CastKind::FloatToInt, Value::Float(f), TyKind::Int(IntTy::U128)) => {
                        // Saturating, as the compiled backends' `fptoui.sat`.
                        Ok(Value::Int(if f.is_nan() { 0 } else { f as u128 as i128 }))
                    }
                    (CastKind::FloatToInt, Value::Float(f), TyKind::Int(it)) => {
                        let (lo, hi) = range(it);
                        Ok(Value::Int(if f.is_nan() {
                            0
                        } else {
                            (f as i128).clamp(lo, hi)
                        }))
                    }
                    (CastKind::FloatToFloat, Value::Float(f), TyKind::Float(ft)) => {
                        Ok(Value::Float(ft.round(f)))
                    }
                    (CastKind::IntToChar, Value::Int(i), _) => match u8::try_from(i) {
                        Ok(b) => Ok(Value::Char(b as char)),
                        Err(_) => err(format!("{i} is not a u8")),
                    },
                    (CastKind::CharToInt, Value::Char(c), TyKind::Int(it)) => {
                        Ok(Value::Int(wrap(c as i128, it)))
                    }
                    (CastKind::BoolToInt, Value::Bool(b), _) => Ok(Value::Int(b as i128)),
                    (k, x, _) => err(format!("cannot cast {x:?} with {k:?}")),
                }
            }
            Rvalue::Discriminant(p) => {
                let (addr, _) = self.resolve_read(body, p)?;
                let v = match self.slot(&addr)? {
                    // A `shared enum`: the variant lives in the counted box.
                    Value::Shared(id) => self.live(id)?.value.clone(),
                    v => v,
                };
                match v {
                    Value::Variant(v, _) => Ok(Value::Int(v as i128)),
                    _ => err("discriminant of a value that is not an initialized enum"),
                }
            }
            Rvalue::NullaryOp(op, t) => match super::layout::layout(self.tys, *t) {
                Some(l) => Ok(Value::Int(i128::from(match op {
                    NullOp::SizeOf => l.size,
                    NullOp::AlignOf => l.align,
                }))),
                None => err("layout of a type that has none"),
            },
            Rvalue::Len(p) => {
                let (addr, _) = self.resolve_read(body, p)?;
                match self.slot(&addr)? {
                    Value::Agg(fs) => Ok(Value::Int(fs.len() as i128)),
                    _ => err("Len of a non-array value"),
                }
            }
            Rvalue::Aggregate(kind, ops) => {
                let mut vals = Vec::with_capacity(ops.len());
                for o in ops {
                    vals.push(self.operand(body, o)?.0);
                }
                Ok(match kind {
                    AggregateKind::Tuple
                    | AggregateKind::Array(_)
                    | AggregateKind::Closure { .. } => Value::Agg(vals),
                    AggregateKind::Adt { ty, variant } => self.adt_value(*ty, *variant, vals),
                    AggregateKind::Shared { ty, variant } => {
                        let value = self.adt_value(*ty, *variant, vals);
                        let a = self.alloc(HeapObj {
                            count: 1,
                            weak: 0,
                            value,
                        });
                        if self.trace {
                            self.events.push(Event::Alloc(a, self.tys.display(*ty)));
                        }
                        Value::Shared(a)
                    }
                })
            }
        }
    }

    fn adt_value(&self, ty: Ty, variant: VariantIdx, vals: Vec<Value>) -> Value {
        match self.tys.kind(ty) {
            TyKind::Adt(a) | TyKind::Shared(a) if self.tys.adt(a).is_enum => {
                Value::Variant(variant.0, vals)
            }
            _ => Value::Agg(vals),
        }
    }

    /// The result of `op` and whether it overflowed `ty`.
    /// A unary operator; a lane vector's works lane by lane.
    fn unop(&mut self, op: UnOp, x: Value, ty: Ty) -> R<Value> {
        if let (Value::Agg(fs), TyKind::Array(et, _)) = (&x, self.tys.kind(ty)) {
            if self.tys.is_vector(ty) {
                let mut lanes = Vec::with_capacity(fs.len());
                for l in fs.clone() {
                    lanes.push(self.unop(op, l, et)?);
                }
                return Ok(Value::Agg(lanes));
            }
        }
        match (op, x, self.tys.kind(ty)) {
            (UnOp::Not, Value::Bool(b), _) => Ok(Value::Bool(!b)),
            (UnOp::Not, Value::Int(i), TyKind::Int(it)) => Ok(Value::Int(wrap(!i, it))),
            (UnOp::Neg, Value::Int(i), TyKind::Int(it)) => {
                let (r, of) = i.overflowing_neg();
                if of || wrap(r, it) != r {
                    // `-MIN` overflows, and C8 traps it.
                    if self.trace {
                        self.events.push(Event::Abort(AbortReason::Overflow));
                    }
                    return Err(Stop::Abort(AbortReason::Overflow));
                }
                Ok(Value::Int(r))
            }
            (UnOp::Neg, Value::Float(f), _) => Ok(Value::Float(-f)),
            (op, x, _) => err(format!("cannot apply {op:?} to {x:?}")),
        }
    }

    fn binop(&self, op: BinOp, x: Value, y: Value, ty: Ty) -> R<(Value, bool)> {
        use BinOp::*;
        let cmp = |o: std::cmp::Ordering| -> Value {
            Value::Bool(match op {
                Eq => o.is_eq(),
                Ne => o.is_ne(),
                Lt => o.is_lt(),
                Le => o.is_le(),
                Gt => o.is_gt(),
                Ge => o.is_ge(),
                _ => unreachable!(),
            })
        };
        if let (Value::Agg(a), Value::Agg(b), TyKind::Array(et, _)) = (&x, &y, self.tys.kind(ty)) {
            if self.tys.is_vector(ty) {
                // A lane vector's operators work lane by lane; a comparison
                // gives a lane mask.
                let mut lanes = Vec::with_capacity(a.len());
                let mut overflow = false;
                for (p, q) in a.iter().zip(b) {
                    let (v, of) = self.binop(op, p.clone(), q.clone(), et)?;
                    overflow |= of;
                    lanes.push(v);
                }
                return Ok((Value::Agg(lanes), overflow));
            }
        }
        match (x, y) {
            (Value::Int(a), Value::Int(b)) if self.tys.kind(ty) == TyKind::Int(IntTy::U128) => {
                Ok(u128_binop(op, a as u128, b as u128, cmp)?)
            }
            (Value::Int(a), Value::Int(b)) => {
                if op.is_comparison() {
                    return Ok((cmp(a.cmp(&b)), false));
                }
                let TyKind::Int(it) = self.tys.kind(ty) else {
                    return err("integer operands of a non-integer type");
                };
                // In 128 bits, with the 128-bit overflow kept: a narrower
                // type's result then wraps to the right value, and an
                // `i128` one is flagged.
                let (exact, of) = match op {
                    Add => a.overflowing_add(b),
                    Sub => a.overflowing_sub(b),
                    Mul => a.overflowing_mul(b),
                    Div | Rem if b == 0 => {
                        return err("division by zero; the builder must guard it")
                    }
                    Div => a.overflowing_div(b),
                    // `MIN % -1` is 0 mathematically, but it overflows on
                    // the machine exactly as `MIN / -1` does, and C8 traps
                    // both (Rust's `overflowing_rem`).
                    Rem if it.signed() && b == -1 && a == wrap(1 << (it.bits() - 1), it) => {
                        return Ok((Value::Int(0), true));
                    }
                    Rem => a.overflowing_rem(b),
                    BitAnd => (a & b, false),
                    BitOr => (a | b, false),
                    BitXor => (a ^ b, false),
                    Shl | Shr => {
                        if b < 0 || b >= it.bits() as i128 {
                            return Ok((Value::Int(0), true));
                        }
                        let r = if op == Shl { wrap(a << b, it) } else { a >> b };
                        return Ok((Value::Int(r), false));
                    }
                    _ => unreachable!(),
                };
                let wrapped = wrap(exact, it);
                Ok((Value::Int(wrapped), of || wrapped != exact))
            }
            (Value::Bool(a), Value::Bool(b)) => Ok((
                match op {
                    BitAnd => Value::Bool(a & b),
                    BitOr => Value::Bool(a | b),
                    BitXor => Value::Bool(a ^ b),
                    _ if op.is_comparison() => cmp(a.cmp(&b)),
                    _ => return err(format!("{} on bool", op.name())),
                },
                false,
            )),
            (Value::Char(a), Value::Char(b)) if op.is_comparison() => Ok((cmp(a.cmp(&b)), false)),
            (Value::Float(a), Value::Float(b)) => Ok((
                match op {
                    Add | Sub | Mul | Div | Rem => {
                        let r = match op {
                            Add => a + b,
                            Sub => a - b,
                            Mul => a * b,
                            Div => a / b,
                            _ => a % b,
                        };
                        narrow_float(Value::Float(r), self.tys.kind(ty))
                    }
                    _ if op.is_comparison() => match a.partial_cmp(&b) {
                        Some(o) => cmp(o),
                        None => Value::Bool(op == Ne),
                    },
                    _ => return err(format!("{} on float", op.name())),
                },
                false,
            )),
            (x, y) => err(format!("{} on {x:?} and {y:?}", op.name())),
        }
    }

    // ---- terminators and drops ----

    /// Runs a terminator; `None` means the body returned.
    fn terminator(
        &mut self,
        body: &Body,
        t: &TerminatorKind,
        strict: bool,
    ) -> R<Option<BasicBlock>> {
        match t {
            TerminatorKind::Goto { target } => Ok(Some(*target)),
            TerminatorKind::SwitchInt { discr, targets } => {
                let (v, ty) = self.operand(body, discr)?;
                let bits = match (v, self.tys.kind(ty)) {
                    (Value::Bool(b), _) => b as u128,
                    (Value::Char(c), _) => c as u128,
                    (Value::Int(i), TyKind::Int(it)) => to_bits(i, it),
                    (v, _) => return err(format!("switchInt on {v:?}")),
                };
                let next = targets
                    .values
                    .iter()
                    .find(|(v, _)| *v == bits)
                    .map_or(targets.otherwise, |(_, b)| *b);
                Ok(Some(next))
            }
            // A panic aborts the run whatever the unwind action; v1 has no
            // other.
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
                unwind: UnwindAction::Abort,
            } => {
                let (f, _) = self.operand(body, func)?;
                let mut vals = Vec::with_capacity(args.len());
                let mut arg_tys = Vec::with_capacity(args.len());
                for a in args {
                    let (v, t) = self.operand(body, a)?;
                    vals.push(v);
                    arg_tys.push(t);
                }
                let ret_ty = place_ty(body, self.tys, destination)
                    .map_err(|e| Stop::Error(format!("call destination: {e}")))?
                    .ty;
                let inst = match f {
                    Value::Fn(inst) => inst,
                    erased @ (Value::Erased { .. } | Value::Ref(_)) => {
                        let ret = self.call_erased(erased, vals)?;
                        return self.finish_call(body, destination, target, ret, "an erased call");
                    }
                    _ => return err("call of a value that is not a function item"),
                };
                let ret = if self.program.bodies.contains_key(&inst.name) {
                    self.call(&inst.name, vals)?
                } else {
                    self.native(&inst.name, vals, &arg_tys, ret_ty)?
                };
                self.finish_call(body, destination, target, ret, &inst.name)
            }
            TerminatorKind::Drop {
                place,
                target,
                unwind: UnwindAction::Abort,
            } => {
                let Some((addr, ty)) = self.resolve(body, place, Mode::Probe)? else {
                    if strict {
                        return err(format!(
                            "drop of {}, which is not initialized",
                            self.place_str(body, place)
                        ));
                    }
                    return Ok(Some(*target));
                };
                let v = self.slot(&addr)?;
                let what = self.place_str(body, place);
                if !v.any_init() {
                    if strict {
                        return err(format!("drop of {what}, which is not initialized"));
                    }
                    return Ok(Some(*target));
                }
                if strict && !v.fully_init() {
                    return err(format!(
                        "drop of {what}, which is partly moved; elaboration must drop its parts"
                    ));
                }
                if self.trace {
                    self.events.push(Event::Drop(what, self.tys.display(ty)));
                }
                self.drop_at(&addr, ty)?;
                *self.slot_mut(&addr)? = Value::Uninit;
                Ok(Some(*target))
            }
            TerminatorKind::Return => Ok(None),
            TerminatorKind::Abort { reason } => {
                if self.trace {
                    self.events.push(Event::Abort(reason.clone()));
                }
                Err(Stop::Abort(reason.clone()))
            }
            TerminatorKind::Unreachable => err("reached an unreachable terminator"),
        }
    }

    /// Drops the initialized parts of the value at `addr`, in the order
    /// of core semantics §6: the user `Drop` body first, then struct,
    /// tuple and enum parts last to first, array elements first to last.
    fn drop_at(&mut self, addr: &Addr, ty: Ty) -> R<()> {
        let v = self.slot(addr)?;
        if !v.any_init() {
            return Ok(());
        }
        match self.tys.kind(ty).clone() {
            TyKind::Adt(a) => self.drop_adt(addr, ty, a),
            TyKind::Shared(a) => {
                let Value::Shared(id) = v else {
                    return err("a shared-typed place holds no handle");
                };
                self.release(id, ty, a)
            }
            TyKind::Weak(_) => {
                let Value::Weak(id) = v else {
                    return err("a weak-typed place holds no handle");
                };
                self.release_weak(id)
            }
            TyKind::Tuple(ts) | TyKind::Closure(_, ts) => {
                for i in (0..ts.len()).rev() {
                    self.drop_at(&addr.child(i as u64), ts[i])?;
                }
                Ok(())
            }
            TyKind::Array(e, n) => {
                for i in 0..n {
                    self.drop_at(&addr.child(i), e)?;
                }
                Ok(())
            }
            // An erased function value owns its closure's environment.
            TyKind::Fn { .. } => match v {
                Value::Erased { env: None, .. } => Ok(()),
                Value::Erased {
                    env: Some((slot, env_ty)),
                    ..
                } => {
                    let at = Addr {
                        root: Root::Heap(slot),
                        path: Vec::new(),
                    };
                    self.drop_at(&at, env_ty)?;
                    self.free_slot(slot);
                    Ok(())
                }
                other => err(format!("a function-value place holds {other:?}")),
            },
            TyKind::Intrinsic(k) => {
                let Value::Box(id) = v else {
                    return err(format!("a {} place holds no box", self.tys.display(ty)));
                };
                match k {
                    IntrinsicTy::String => {}
                    IntrinsicTy::Vec(e) | IntrinsicTy::VecDeque(e) => {
                        let n = match &self.live(id)?.value {
                            Value::Agg(fs) => fs.len(),
                            other => return err(format!("a Vec allocation holds {other:?}")),
                        };
                        let root = Addr {
                            root: Root::Heap(id),
                            path: Vec::new(),
                        };
                        for i in 0..n {
                            self.drop_at(&root.child(i as u64), e)?;
                        }
                    }
                    // Entries in insertion order, the interpreter's
                    // (unspecified) iteration order; within an entry the
                    // key drops before the value (core-semantics §7).
                    IntrinsicTy::Map(k, val) | IntrinsicTy::SortedMap(k, val) => {
                        for i in 0..self.vec_elems(id)?.len() as u64 {
                            let entry = Addr {
                                root: Root::Heap(id),
                                path: vec![i],
                            };
                            self.drop_at(&entry.child(0), k)?;
                            self.drop_at(&entry.child(1), val)?;
                        }
                    }
                    IntrinsicTy::Set(e) | IntrinsicTy::SortedSet(e) => {
                        for i in 0..self.vec_elems(id)?.len() as u64 {
                            let at = Addr {
                                root: Root::Heap(id),
                                path: vec![i],
                            };
                            self.drop_at(&at, e)?;
                        }
                    }
                }
                self.live(id)?;
                self.free_slot(id);
                if self.trace {
                    self.events.push(Event::Free(id));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Whether `v`, a value of type `ty`, holds anything its drop would
    /// act on: a user `Drop` body, a heap allocation or a counted handle.
    /// A fieldless variant, or one whose payload was moved out, owns
    /// nothing, so leaving it in place at a return or overwriting it is
    /// not a missing drop.
    fn needs_drop(&self, ty: Ty) -> bool {
        if let Some(&d) = self.needs_drop.borrow().get(&ty) {
            return d;
        }
        let d = self.tys.needs_drop(ty);
        self.needs_drop.borrow_mut().insert(ty, d);
        d
    }

    fn owns_drop(&self, v: &Value, ty: Ty) -> bool {
        if !self.needs_drop(ty) {
            return false;
        }
        let (variant, parts) = match v {
            Value::Uninit | Value::Slice { .. } | Value::Ref(_) => return false,
            Value::Agg(fs) => (None, fs),
            Value::Variant(k, fs) => (Some(*k), fs),
            _ => return true,
        };
        let kind = self.tys.kind(ty);
        if let TyKind::Adt(a) = kind {
            if self.tys.adt(a).has_drop_impl && v.any_init() {
                return true;
            }
        }
        parts.iter().enumerate().any(|(i, part)| {
            let pty = match kind {
                TyKind::Array(e, _) => Some(e),
                _ => self.tys.field_ty(ty, variant, i as u32),
            };
            pty.is_some_and(|t| self.owns_drop(part, t))
        })
    }

    /// `ty` is the ADT or shared type; its fields are read through it.
    fn drop_adt(&mut self, addr: &Addr, ty: Ty, a: AdtId) -> R<()> {
        let adt = self.tys.adt(a);
        if adt.has_drop_impl {
            if !self.slot(addr)?.fully_init() {
                return err(format!(
                    "{} has a Drop body but the value is partly moved",
                    adt.name
                ));
            }
            let Some(f) = self
                .program
                .drop_impls
                .get(&a)
                .or_else(|| self.program.drop_by_ty.get(&ty))
            else {
                return err(format!("no Drop body registered for {}", adt.name));
            };
            if self.trace {
                self.events.push(Event::DropBody(adt.name.clone()));
            }
            let f = f.clone();
            match (self.tys.kind(ty), &addr.root) {
                // `fn T.drop(mut ref self)` on a `shared` type takes a
                // reference to a handle, as every field read through a
                // handle does. The object stays live, at count 0, until
                // the body returns.
                (TyKind::Shared(_), Root::Heap(id)) if addr.path.is_empty() => {
                    let scratch = self
                        .alloc(HeapObj {
                            count: 0,
                            weak: 0,
                            value: Value::Shared(*id),
                        })
                        .0 as usize;
                    let handle = Addr {
                        root: Root::Heap(AllocId(scratch as u32)),
                        path: Vec::new(),
                    };
                    let r = self.call(&f, vec![Value::Ref(handle)]);
                    self.free_slot(AllocId(scratch as u32));
                    r?;
                }
                _ => {
                    self.call(&f, vec![Value::Ref(addr.clone())])?;
                }
            }
        }
        // The library cells keep their contents where the type's fields
        // would be (`atomic_method`, `once_method`, `arena_method`).
        let first = match adt.name.as_str() {
            "Atomic" => return Ok(()),
            "Sender" | "Receiver" | "File" => {
                if let Value::Agg(fs) = self.slot(addr)? {
                    if let [Value::Int(id)] = fs.as_slice() {
                        let id = *id;
                        if adt.name == "File" {
                            if let Some(f) = usize::try_from(id - 1)
                                .ok()
                                .and_then(|i| self.files.get_mut(i))
                            {
                                *f = None;
                            }
                            return Ok(());
                        }
                        return self.end_dropped(id, adt.name == "Sender");
                    }
                }
                return Ok(());
            }
            "OnceLock" | "OnceCell" | "TaskHandle" | "Mutex" => Some(0),
            "Arena" => Some(1),
            _ => None,
        };
        if let Some(first) = first {
            let held = self
                .tys
                .tcx()
                .adt_of(ty)
                .and_then(|(_, args)| args.first().copied())
                .ok_or_else(|| Stop::Error(format!("{} has no type argument", adt.name)))?;
            let n = match self.slot(addr)? {
                Value::Agg(fs) => fs.len(),
                other => return err(format!("{} holds {other:?}", adt.name)),
            };
            for i in (first..n).rev() {
                self.drop_at(&addr.child(i as u64), held)?;
            }
            return Ok(());
        }
        let (variant, n) = match self.slot(addr)? {
            Value::Variant(k, fs) => (Some(k), fs.len()),
            Value::Agg(fs) => (None, fs.len()),
            Value::Uninit => return Ok(()),
            other => return err(format!("ADT place holds {other:?}")),
        };
        for i in (0..n).rev() {
            let Some(fty) = self.tys.field_ty(ty, variant, i as u32) else {
                return err("ADT value has more fields than its type");
            };
            self.drop_at(&addr.child(i as u64), fty)?;
        }
        Ok(())
    }

    fn live(&mut self, a: AllocId) -> R<&mut HeapObj> {
        match self.heap.get_mut(a.0 as usize) {
            Some(Some(obj)) => Ok(obj),
            _ => err(format!("use of {a} after it was freed")),
        }
    }

    /// A heap slot for `obj`: a freed one when there is one, so memory
    /// follows the live allocations rather than all of them.
    fn alloc(&mut self, obj: HeapObj) -> AllocId {
        match self.free.pop() {
            Some(i) => {
                self.heap[i as usize] = Some(obj);
                AllocId(i)
            }
            None => {
                self.heap.push(Some(obj));
                AllocId(self.heap.len() as u32 - 1)
            }
        }
    }

    /// Frees slot `id` for reuse. A slot with weak handles is never freed
    /// here: `release` keeps it, dead, until the last weak handle goes, so
    /// an `Upgrade` cannot reach whatever reuses it.
    fn free_slot(&mut self, id: AllocId) {
        self.key_index.remove(&id);
        self.caps.remove(&id);
        if self.heap[id.0 as usize].take().is_some() {
            self.free.push(id.0);
        }
    }

    fn release(&mut self, id: AllocId, ty: Ty, a: AdtId) -> R<()> {
        debug_assert!(id != EMPTY_WEAK);
        let obj = self.live(id)?;
        obj.count -= 1;
        let c = obj.count;
        if self.trace {
            self.events.push(Event::Release(id, c));
        }
        if c == 0 {
            let root = Addr {
                root: Root::Heap(id),
                path: Vec::new(),
            };
            // The body may hold the last weak handle to its own box (a
            // node whose `weak` field points at itself): keep the box for
            // the length of the drop, so releasing that handle cannot free
            // it under us.
            self.live(id)?.weak += 1;
            self.drop_adt(&root, ty, a)?;
            let obj = self.live(id)?;
            obj.weak -= 1;
            if obj.weak > 0 {
                // Weak handles keep the slot, dead, for `Upgrade` to see.
                obj.value = Value::Uninit;
                return Ok(());
            }
            self.free_slot(id);
            if self.trace {
                self.events.push(Event::Free(id));
            }
        }
        Ok(())
    }

    /// Drops one weak handle to `id`, freeing the slot when it was the
    /// last handle of any kind.
    fn release_weak(&mut self, id: AllocId) -> R<()> {
        if id == EMPTY_WEAK {
            return Ok(());
        }
        let obj = self.live(id)?;
        obj.weak -= 1;
        if obj.weak == 0 && obj.count == 0 {
            self.free_slot(id);
            if self.trace {
                self.events.push(Event::Free(id));
            }
        }
        Ok(())
    }

    /// The handle behind a `ref shared T`, `ref weak T` or
    /// `ref Option[shared T]` operand; `None` for an `Option` that is
    /// `None`.
    fn handle_behind(&mut self, v: &Value) -> R<Option<AllocId>> {
        let held = match v {
            Value::Ref(addr) => self.slot(addr)?,
            other => other.clone(),
        };
        match held {
            Value::Shared(id) | Value::Weak(id) => Ok(Some(id)),
            Value::Variant(_, fs) if fs.is_empty() => Ok(None),
            Value::Variant(_, fs) => match fs.as_slice() {
                [Value::Shared(id)] => Ok(Some(*id)),
                _ => err(format!(
                    "expected an Option of a shared handle, found {fs:?}"
                )),
            },
            other => err(format!("expected a shared or weak handle, found {other:?}")),
        }
    }

    /// What `main` returning `v` means: an `Err` goes to stderr as
    /// `Error: {e}` and exits 1, as legacy and the compiled binary do.
    /// A returned `Result` is dropped here, since nothing else owns it.
    fn main_result(&mut self, entry: &str, v: Value) -> R<Outcome> {
        let ret = match self.program.bodies.get(entry) {
            Some(b) if entry == "main" => b.locals[0].ty,
            None if entry == "main" => {
                // A coroutine `main`: the `T` of its resume's `Poll[T]`.
                match self.program.bodies.get(&coroutine::resume_name(entry)) {
                    Some(b) => match self.tys.field_ty(b.locals[0].ty, Some(0), 0) {
                        Some(t) => t,
                        None => return Ok(Outcome::Returned(v)),
                    },
                    None => return Ok(Outcome::Returned(v)),
                }
            }
            _ => return Ok(Outcome::Returned(v)),
        };
        let TyKind::Adt(a) = self.tys.kind(ret) else {
            return Ok(Outcome::Returned(v));
        };
        let adt = self.tys.adt(a);
        // `fn main() -> ExitCode`: the process exits with its code.
        if adt.name == "ExitCode" {
            if let Value::Agg(fs) = &v {
                if let [Value::Int(n)] = fs.as_slice() {
                    return Ok(Outcome::Exited(*n as i32));
                }
            }
        }
        if adt.name != "Result" {
            return Ok(Outcome::Returned(v));
        }
        let mut done = Outcome::Returned(Value::Unit);
        if let Value::Variant(k, fs) = &v {
            if adt
                .variants
                .get(*k as usize)
                .is_some_and(|var| var.name == "Err")
            {
                let Some(et) = self.tys.field_ty(ret, Some(*k), 0) else {
                    return err("Result.Err has no payload");
                };
                let text = self.display_typed(&fs[0], et)?;
                self.write_out(true, &format!("Error: {text}\n"));
                done = Outcome::Exited(1);
            }
        }
        self.drop_value(v, ret)?;
        Ok(done)
    }

    /// `text` onto the program's stdout, or its stderr when `err`.
    fn write_out(&mut self, err: bool, text: &str) {
        use std::io::Write;
        match (self.stream, err) {
            (true, false) => {
                let _ = std::io::stdout().write_all(text.as_bytes());
            }
            (true, true) => {
                let _ = std::io::stdout().flush();
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
            (false, false) => self.output.push_str(text),
            (false, true) => self.stderr.push_str(text),
        }
    }

    /// A coroutine frame of type `ty`, before its first resume: state 0,
    /// every local uninitialized, and each callee frame shaped the same way
    /// so the resume body can start it field by field.
    fn fresh_frame(&self, ty: Ty) -> Value {
        match self.tys.kind(ty) {
            TyKind::Adt(a) if coroutine::is_frame(&self.tys.adt(a).name) => {
                let adt = self.tys.adt(a);
                let fields = &adt.variants[0].fields;
                Value::Agg(
                    fields
                        .iter()
                        .enumerate()
                        .map(|(i, (_, t))| {
                            if i == 0 {
                                Value::Int(0)
                            } else {
                                self.fresh_frame(*t)
                            }
                        })
                        .collect(),
                )
            }
            _ => Value::Uninit,
        }
    }

    /// Whether `v`, of type `ty`, still owns something a drop would give
    /// back: a heap handle, a closure environment, or a value whose type has
    /// a `Drop` body. A fieldless variant, or an enum whose payload was
    /// moved, owns nothing, though it reads as initialized; elaboration
    /// leaves such a shell without a drop.
    fn owns(&self, v: &Value, ty: Ty) -> bool {
        match v {
            Value::Uninit => false,
            Value::Box(_) | Value::Shared(_) | Value::Weak(_) => true,
            Value::Erased { env, .. } => env.is_some(),
            Value::Agg(fs) | Value::Variant(_, fs) => {
                if self.tys.has_drop_impl(ty) && v.any_init() {
                    return true;
                }
                let variant = match v {
                    Value::Variant(k, _) => Some(*k),
                    _ => None,
                };
                fs.iter().enumerate().any(|(i, f)| {
                    match self.tys.field_ty(ty, variant, i as u32) {
                        Some(t) => self.owns(f, t),
                        // An array element: no field type to ask.
                        None => self.owns(f, self.tys.unit()),
                    }
                })
            }
            _ => false,
        }
    }

    /// The field of frame `v` (of type `ty`) that still holds a value of
    /// its body needing a drop, if any: a frame that returned holds none.
    fn frame_holds(&self, v: &Value, ty: Ty) -> Option<String> {
        let owned = || self.owns(v, ty).then(String::new);
        let TyKind::Adt(a) = self.tys.kind(ty) else {
            return owned();
        };
        let adt = self.tys.adt(a);
        if !coroutine::is_frame(&adt.name) {
            return owned();
        }
        let Value::Agg(fs) = v else {
            return owned();
        };
        fs.iter()
            .zip(&adt.variants[0].fields)
            .skip(1)
            .find_map(|(f, (name, t))| {
                let inner = self.frame_holds(f, *t)?;
                Some(if inner.is_empty() {
                    format!("{}.{name}", adt.name)
                } else {
                    inner
                })
            })
    }

    /// Runs coroutine `entry` as a task root (`KARAC_MIR_COROUTINES=1`): the
    /// executor owns its frame and resumes it until it is `Ready`.
    fn run_task(&mut self, entry: &str, args: Vec<Value>) -> R<Value> {
        let resume = coroutine::resume_name(entry);
        let Some(body) = self.program.bodies.get(&resume) else {
            return err(format!("{entry} is not a coroutine"));
        };
        let TyKind::MutRef(frame_ty) = self.tys.kind(body.locals[1].ty) else {
            return err(format!("{resume} does not take its frame"));
        };
        let mut frame = self.fresh_frame(frame_ty);
        if let Value::Agg(fs) = &mut frame {
            // The arguments are the frame's first locals after the state.
            for (i, a) in args.into_iter().enumerate() {
                fs[i + 1] = a;
            }
        }
        let slot = self.alloc(HeapObj {
            count: 1,
            weak: 0,
            value: frame,
        });
        let at = Addr {
            root: Root::Heap(slot),
            path: Vec::new(),
        };
        loop {
            match self.call(&resume, vec![Value::Ref(at.clone())])? {
                Value::Variant(0, mut fs) if fs.len() == 1 => {
                    let frame = self.live(slot)?.value.clone();
                    if let Some(field) = self.frame_holds(&frame, frame_ty) {
                        return err(format!(
                            "{entry} returned with its frame still holding a value in {field}"
                        ));
                    }
                    self.free_slot(slot);
                    return Ok(fs.remove(0));
                }
                Value::Variant(1, _) => {
                    self.resumes += 1;
                    if self.cancel_after == Some(self.resumes) {
                        let drop = coroutine::drop_frame_name(entry);
                        self.call(&drop, vec![Value::Ref(at.clone())])?;
                        let frame = self.live(slot)?.value.clone();
                        if let Some(field) = self.frame_holds(&frame, frame_ty) {
                            return err(format!("{drop} left a value in {field}"));
                        }
                        self.free_slot(slot);
                        return Ok(Value::Unit);
                    }
                }
                other => return err(format!("{resume} returned {other:?}, not a Poll")),
            }
        }
    }

    /// Runs each static's initializer, in declaration order, before `main`.
    fn init_statics(&mut self) -> R<()> {
        let program = self.program;
        for def in &program.statics {
            let v = self.call(&def.init, Vec::new()).map_err(|e| match e {
                Stop::Error(e) => Stop::Error(format!("initializing static {}: {e}", def.name)),
                abort => abort,
            })?;
            self.statics.push(v);
        }
        Ok(())
    }

    /// `std.http`'s client, through `ureq` as legacy's. A `Response` is its
    /// status and body, then the id of its headers; a `RequestBuilder` the
    /// handle of the request it assembles.
    fn http_method(
        &mut self,
        name: &str,
        base: &str,
        method: &str,
        args: Vec<Value>,
        ret: Ty,
    ) -> R<Value> {
        let recv = match args.first() {
            Some(Value::Ref(at)) => self.slot(at)?,
            Some(v) => v.clone(),
            None => Value::Unit,
        };
        let field = |i: usize| match &recv {
            Value::Agg(fs) => fs.get(i).cloned(),
            _ => None,
        };
        match (base, method) {
            ("Client", "new") => Ok(Value::Agg(Vec::new())),
            ("Client", "get" | "post") => {
                let url = self.string_at(&args[1])?;
                let body = match args.get(2) {
                    Some(b) => self.string_at(b)?,
                    None => String::new(),
                };
                let req = HttpBuilder {
                    method: if method == "get" { "GET" } else { "POST" }.into(),
                    url,
                    body,
                    ..HttpBuilder::default()
                };
                self.http_send(req, method == "post", ret)
            }
            ("Client", "request") => {
                let method = self.string_at(&args[1])?;
                let url = self.string_at(&args[2])?;
                self.http_builders.push(HttpBuilder {
                    method,
                    url,
                    ..HttpBuilder::default()
                });
                Ok(Value::Agg(vec![Value::Int(
                    self.http_builders.len() as i128
                )]))
            }
            ("RequestBuilder", _) => {
                let Some(Value::Int(h)) = field(0) else {
                    return err(format!("{name} of {recv:?}"));
                };
                let i = (h as usize).wrapping_sub(1);
                if i >= self.http_builders.len() {
                    return err(format!("{name} on an unknown request"));
                }
                match method {
                    "header" => {
                        let k = self.string_at(&args[1])?;
                        let v = self.string_at(&args[2])?;
                        self.http_builders[i].headers.push((k, v));
                    }
                    "body" => self.http_builders[i].body = self.string_at(&args[1])?,
                    "timeout" => {
                        if let Some(Value::Int(ms)) = args.get(1) {
                            self.http_builders[i].timeout_ms = *ms;
                        }
                    }
                    "send" => {
                        let req = self.http_builders[i].clone();
                        let with_body = !req.body.is_empty();
                        return self.http_send(req, with_body, ret);
                    }
                    _ => return err(format!("call of unknown function {name}")),
                }
                Ok(recv)
            }
            ("Response", "status") => {
                field(0).ok_or_else(|| Stop::Error(format!("{name} of {recv:?}")))
            }
            ("Response" | "HttpError", "body" | "message") => {
                let text = match field(if base == "Response" { 1 } else { 0 }) {
                    Some(v) => self.string_at(&v)?,
                    None => return err(format!("{name} of {recv:?}")),
                };
                Ok(self.alloc_box("String", Value::Str(text)))
            }
            ("Response", "bytes") => {
                let text = match field(1) {
                    Some(v) => self.string_at(&v)?,
                    None => return err(format!("{name} of {recv:?}")),
                };
                let bytes = text.bytes().map(|b| Value::Int(i128::from(b))).collect();
                Ok(self.alloc_box("Vec", Value::Agg(bytes)))
            }
            ("Response", "header" | "headers") => {
                let headers = match field(2) {
                    Some(Value::Int(id)) => self
                        .http_headers
                        .get(id as usize)
                        .cloned()
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                if method == "header" {
                    let want = self.string_at(&args[1])?.to_ascii_lowercase();
                    let found = headers
                        .into_iter()
                        .find(|(k, _)| k.to_ascii_lowercase() == want)
                        .map(|(_, v)| self.alloc_box("String", Value::Str(v)));
                    return self.option(ret, found);
                }
                let pairs = headers
                    .into_iter()
                    .map(|(k, v)| {
                        let k = self.alloc_box("String", Value::Str(k));
                        let v = self.alloc_box("String", Value::Str(v));
                        Value::Agg(vec![k, v])
                    })
                    .collect();
                Ok(self.alloc_box("Vec", Value::Agg(pairs)))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    /// Sends `req`: `Ok` of a `Response`, or `Err` of an `HttpError`
    /// carrying `ureq`'s message (a non-2xx status is one, as legacy's).
    #[cfg(not(target_arch = "wasm32"))]
    fn http_send(&mut self, req: HttpBuilder, with_body: bool, ret: Ty) -> R<Value> {
        let mut r = ureq::request(&req.method, &req.url);
        for (k, v) in &req.headers {
            r = r.set(k, v);
        }
        if req.timeout_ms > 0 {
            r = r.timeout(std::time::Duration::from_millis(req.timeout_ms as u64));
        }
        let result = if with_body {
            r.send_string(&req.body)
        } else {
            r.call()
        };
        match result {
            Ok(resp) => {
                let status = i128::from(resp.status());
                let headers: Vec<(String, String)> = resp
                    .headers_names()
                    .into_iter()
                    .filter_map(|n| resp.header(&n).map(|v| (n, v.to_string())))
                    .collect();
                let body = resp.into_string().unwrap_or_default();
                self.http_headers.push(headers);
                let id = self.http_headers.len() as i128 - 1;
                let body = self.alloc_box("String", Value::Str(body));
                let response = Value::Agg(vec![Value::Int(status), body, Value::Int(id)]);
                self.variant_named(ret, None, "Ok", vec![response])
            }
            Err(ureq::Error::Transport(t)) if t.kind() == ureq::ErrorKind::ConnectionFailed => {
                // The runtime's wording, which the compiled program prints.
                use std::error::Error;
                let cause = match t.source() {
                    Some(io) => io.to_string(),
                    None => t.to_string(),
                };
                self.http_error(format!("{}: connect failed: {cause}", req.url), ret)
            }
            Err(e) => self.http_error(e.to_string(), ret),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn http_send(&mut self, _req: HttpBuilder, _with_body: bool, ret: Ty) -> R<Value> {
        self.http_error(
            "the HTTP client is not available in the browser".into(),
            ret,
        )
    }

    fn http_error(&mut self, message: String, ret: Ty) -> R<Value> {
        let message = self.alloc_box("String", Value::Str(message));
        self.variant_named(ret, None, "Err", vec![Value::Agg(vec![message])])
    }

    /// `Interner.intern`, `.resolve` and `.len`. A symbol is its string's
    /// position; `resolve` lends the string from a place that lives until
    /// exit, as the interner's strings do.
    /// A panic raised by a native: the run aborts with exit 101.
    fn native_panic<T>(&mut self) -> R<T> {
        if self.trace {
            self.events.push(Event::Abort(AbortReason::Panic));
        }
        Err(Stop::Abort(AbortReason::Panic))
    }

    /// A lane vector's lanes, from the vector or a reference to it.
    fn lanes(&mut self, v: &Value) -> R<Vec<Value>> {
        match v {
            Value::Agg(fs) => Ok(fs.clone()),
            Value::Ref(at) => {
                let inner = self.slot(at)?;
                self.lanes(&inner)
            }
            other => err(format!("a lane vector expected, found {other:?}")),
        }
    }

    /// The element addresses of a slice, an array or a reference to
    /// either: where `gather` reads and `scatter` writes.
    fn element_addrs(&mut self, v: &Value) -> R<Vec<Addr>> {
        match v {
            Value::Slice { base, lo, len } => {
                Ok((*lo..*lo + *len).map(|i| base.child(i)).collect())
            }
            Value::Ref(at) => match self.slot(at)? {
                Value::Agg(fs) => Ok((0..fs.len() as u64).map(|i| at.child(i)).collect()),
                inner => self.element_addrs(&inner),
            },
            other => err(format!("a slice expected, found {other:?}")),
        }
    }

    /// `Vector[T, N]`'s methods and constructors (design.md § Portable
    /// SIMD), lane by lane with the scalar operators, as legacy runs them.
    fn vector_method(&mut self, method: &str, args: &[Value], arg_tys: &[Ty], ret: Ty) -> R<Value> {
        let strip = |me: &Self, mut t: Ty| {
            while let TyKind::Ref(i) | TyKind::MutRef(i) = me.tys.kind(t) {
                t = i;
            }
            t
        };
        // The element type of a vector type.
        let elem_of = |me: &Self, t: Ty| match me.tys.kind(strip(me, t)) {
            TyKind::Array(e, n) => Some((e, n as usize)),
            _ => None,
        };
        let int_index = |v: &Value| match v {
            Value::Int(i) => Some(*i),
            _ => None,
        };
        let fold = |me: &mut Self, op: BinOp, lanes: Vec<Value>, et: Ty| -> R<Value> {
            let mut it = lanes.into_iter();
            let Some(mut acc) = it.next() else {
                return err("a vector with no lanes");
            };
            for x in it {
                let (v, of) = me.binop(op, acc, x, et)?;
                if of {
                    return Err(Stop::Abort(AbortReason::Overflow));
                }
                acc = v;
            }
            Ok(acc)
        };
        match method {
            // Constructors: the receiver is the type.
            "splat" | "from_array" | "from_slice" | "load_masked" | "gather" | "cast_from" => {
                let Some((et, n)) = elem_of(self, ret) else {
                    return err(format!("Vector.{method} returns no vector"));
                };
                let lanes = match method {
                    "splat" => vec![args[0].clone(); n],
                    "from_array" => self.lanes(&args[0])?,
                    "from_slice" => {
                        let addrs = self.element_addrs(&args[0])?;
                        if addrs.len() != n {
                            return self.native_panic();
                        }
                        let mut out = Vec::with_capacity(n);
                        for a in &addrs {
                            out.push(self.slot(a)?);
                        }
                        out
                    }
                    "load_masked" => {
                        let addrs = self.element_addrs(&args[0])?;
                        let mask = self.lanes(&args[1])?;
                        let zero = match self.tys.kind(et) {
                            TyKind::Float(_) => Value::Float(0.0),
                            _ => Value::Int(0),
                        };
                        let mut out = Vec::with_capacity(n);
                        for i in 0..n {
                            if !matches!(mask.get(i), Some(Value::Bool(true))) {
                                out.push(zero.clone());
                                continue;
                            }
                            let Some(a) = addrs.get(i) else {
                                return self.native_panic();
                            };
                            out.push(self.slot(a)?);
                        }
                        out
                    }
                    "gather" => {
                        let addrs = self.element_addrs(&args[0])?;
                        let idx = self.lanes(&args[1])?;
                        let mut out = Vec::with_capacity(idx.len());
                        for i in &idx {
                            let a = int_index(i)
                                .and_then(|i| usize::try_from(i).ok())
                                .and_then(|i| addrs.get(i));
                            let Some(a) = a else {
                                return self.native_panic();
                            };
                            out.push(self.slot(a)?);
                        }
                        out
                    }
                    _ => {
                        // `cast_from`: each lane converted to the target
                        // element type.
                        let src = self.lanes(&args[0])?;
                        let kind = self.tys.kind(et);
                        src.into_iter()
                            .map(|l| match (l, &kind) {
                                (Value::Int(i), TyKind::Float(ft)) => {
                                    Value::Float(ft.round(i as f64))
                                }
                                (Value::Float(f), TyKind::Float(ft)) => Value::Float(ft.round(f)),
                                (Value::Float(f), TyKind::Int(it)) => {
                                    Value::Int(wrap(f as i64 as i128, *it))
                                }
                                (Value::Int(i), TyKind::Int(it)) => Value::Int(wrap(i, *it)),
                                (l, _) => l,
                            })
                            .collect()
                    }
                };
                Ok(Value::Agg(lanes))
            }
            _ => {
                let Some((et, n)) = arg_tys.first().and_then(|&t| elem_of(self, t)) else {
                    return err(format!("Vector.{method} on no vector"));
                };
                let lanes = self.lanes(&args[0])?;
                let float = |x: &Value| match x {
                    Value::Float(f) => Some(*f),
                    _ => None,
                };
                let et_kind = self.tys.kind(et);
                match method {
                    "sqrt" | "exp" | "ln" | "tanh" | "sigmoid" | "floor" | "ceil" | "round"
                    | "trunc" => Ok(Value::Agg(
                        lanes
                            .into_iter()
                            .map(|l| match float(&l) {
                                Some(x) => {
                                    let r = match method {
                                        "sqrt" => x.sqrt(),
                                        "exp" => x.exp(),
                                        "ln" => x.ln(),
                                        "tanh" => x.tanh(),
                                        "floor" => x.floor(),
                                        "ceil" => x.ceil(),
                                        "round" => x.round(),
                                        "trunc" => x.trunc(),
                                        _ => 1.0 / (1.0 + (-x).exp()),
                                    };
                                    narrow_float(Value::Float(r), et_kind.clone())
                                }
                                None => l,
                            })
                            .collect(),
                    )),
                    "to_bits" => Ok(Value::Agg(
                        lanes
                            .into_iter()
                            .map(|l| match (float(&l), &et_kind) {
                                (Some(x), TyKind::Float(crate::mir::ty::FloatTy::F32)) => {
                                    Value::Int((x as f32).to_bits() as i128)
                                }
                                (Some(x), _) => Value::Int(x.to_bits() as i128),
                                (None, _) => l,
                            })
                            .collect(),
                    )),
                    "bits_as_f32" | "bits_as_f64" => Ok(Value::Agg(
                        lanes
                            .into_iter()
                            .map(|l| match l {
                                Value::Int(b) if method == "bits_as_f32" => {
                                    Value::Float(f32::from_bits(b as u32) as f64)
                                }
                                Value::Int(b) => Value::Float(f64::from_bits(b as u64)),
                                l => l,
                            })
                            .collect(),
                    )),
                    "reduce_sum" | "reduce_product" | "reduce_and" | "reduce_or" | "reduce_xor" => {
                        let op = match method {
                            "reduce_sum" => BinOp::Add,
                            "reduce_product" => BinOp::Mul,
                            "reduce_and" => BinOp::BitAnd,
                            "reduce_or" => BinOp::BitOr,
                            _ => BinOp::BitXor,
                        };
                        fold(self, op, lanes, et)
                    }
                    "reduce_min" | "reduce_max" => {
                        let want = match method {
                            "reduce_min" => BinOp::Lt,
                            _ => BinOp::Gt,
                        };
                        let mut it = lanes.into_iter();
                        let Some(mut acc) = it.next() else {
                            return err("a vector with no lanes");
                        };
                        for x in it {
                            let (keep, _) = self.binop(want, acc.clone(), x.clone(), et)?;
                            if !matches!(keep, Value::Bool(true)) {
                                acc = x;
                            }
                        }
                        Ok(acc)
                    }
                    "dot" => {
                        let rhs = self.lanes(&args[1])?;
                        let mut prods = Vec::with_capacity(n);
                        for (x, y) in lanes.into_iter().zip(rhs) {
                            let (v, of) = self.binop(BinOp::Mul, x, y, et)?;
                            if of {
                                return Err(Stop::Abort(AbortReason::Overflow));
                            }
                            prods.push(v);
                        }
                        fold(self, BinOp::Add, prods, et)
                    }
                    "cross" => {
                        let b = self.lanes(&args[1])?;
                        let a = lanes;
                        if a.len() != 3 || b.len() != 3 {
                            return err("cross is defined only for 3-lane vectors");
                        }
                        let mut out = Vec::with_capacity(3);
                        for (p, q, r, t) in [(1, 2, 2, 1), (2, 0, 0, 2), (0, 1, 1, 0)] {
                            let pq = fold(self, BinOp::Mul, vec![a[p].clone(), b[q].clone()], et)?;
                            let rt = fold(self, BinOp::Mul, vec![a[r].clone(), b[t].clone()], et)?;
                            out.push(fold(self, BinOp::Sub, vec![pq, rt], et)?);
                        }
                        Ok(Value::Agg(out))
                    }
                    "select" => {
                        let a = self.lanes(&args[1])?;
                        let b = self.lanes(&args[2])?;
                        Ok(Value::Agg(
                            lanes
                                .into_iter()
                                .zip(a.into_iter().zip(b))
                                .map(|(m, (x, y))| match m {
                                    Value::Bool(true) => x,
                                    _ => y,
                                })
                                .collect(),
                        ))
                    }
                    "reverse" => {
                        let mut out = lanes;
                        out.reverse();
                        Ok(Value::Agg(out))
                    }
                    "rotate_lanes_left" | "rotate_lanes_right" => {
                        let Some(k) = args.get(1).and_then(int_index) else {
                            return err(format!("{method} takes a lane count"));
                        };
                        if n == 0 {
                            return Ok(Value::Agg(lanes));
                        }
                        let k = k.rem_euclid(n as i128) as usize;
                        Ok(Value::Agg(
                            (0..n)
                                .map(|i| match method {
                                    "rotate_lanes_left" => lanes[(i + k) % n].clone(),
                                    _ => lanes[(i + n - k) % n].clone(),
                                })
                                .collect(),
                        ))
                    }
                    "replace" => {
                        let i = args.get(1).and_then(int_index);
                        let mut out = lanes;
                        match i
                            .and_then(|i| usize::try_from(i).ok())
                            .filter(|&i| i < out.len())
                        {
                            Some(i) => {
                                out[i] = args[2].clone();
                                Ok(Value::Agg(out))
                            }
                            None => self.native_panic(),
                        }
                    }
                    "shuffle" => {
                        let idx = self.lanes(&args[1])?;
                        let mut out = Vec::with_capacity(idx.len());
                        for i in &idx {
                            match int_index(i)
                                .and_then(|i| usize::try_from(i).ok())
                                .and_then(|i| lanes.get(i))
                            {
                                Some(l) => out.push(l.clone()),
                                None => return self.native_panic(),
                            }
                        }
                        Ok(Value::Agg(out))
                    }
                    "store_masked" | "scatter" => {
                        let addrs = self.element_addrs(&args[1])?;
                        let sel = self.lanes(&args[2])?;
                        for (i, l) in lanes.into_iter().enumerate() {
                            let at = match method {
                                "store_masked" => {
                                    if !matches!(sel.get(i), Some(Value::Bool(true))) {
                                        continue;
                                    }
                                    addrs.get(i)
                                }
                                _ => sel
                                    .get(i)
                                    .and_then(int_index)
                                    .and_then(|i| usize::try_from(i).ok())
                                    .and_then(|i| addrs.get(i)),
                            };
                            let Some(at) = at else {
                                return self.native_panic();
                            };
                            *self.slot_mut(at)? = l;
                        }
                        Ok(Value::Unit)
                    }
                    _ => err(format!("call of unknown function Vector.{method}")),
                }
            }
        }
    }

    fn interner_method(&mut self, method: &str, args: &[Value]) -> R<Value> {
        let recv = match args.first() {
            Some(Value::Ref(at)) => self.slot(at)?,
            Some(v) => v.clone(),
            None => return err(format!("Interner.{method} takes a receiver")),
        };
        let Value::Agg(fs) = &recv else {
            return err(format!("an Interner is {recv:?}"));
        };
        let Some(Value::Int(h)) = fs.first() else {
            return err(format!("an Interner is {recv:?}"));
        };
        let h = (*h as usize).wrapping_sub(1);
        if h >= self.interners.len() {
            return err(format!("Interner.{method} on an unknown handle"));
        }
        match method {
            "intern" => {
                let text = self.string_at(&args[1])?;
                let t = &mut self.interners[h];
                let id = match t.ids.get(&text) {
                    Some(&id) => id,
                    None => {
                        t.strings.push(text.clone());
                        t.lent.push(None);
                        t.ids.insert(text, t.strings.len() - 1);
                        t.strings.len() - 1
                    }
                };
                Ok(Value::Agg(vec![Value::Int(id as i128)]))
            }
            "resolve" => {
                let sym = match &args[1] {
                    Value::Ref(at) => self.slot(at)?,
                    v => v.clone(),
                };
                let id = match sym {
                    Value::Agg(fs) => match fs.first() {
                        Some(Value::Int(i)) => *i,
                        _ => -1,
                    },
                    Value::Int(i) => i,
                    _ => -1,
                };
                // A foreign symbol reads as the empty string, as legacy's.
                let i = usize::try_from(id)
                    .ok()
                    .filter(|&i| i < self.interners[h].strings.len());
                let holder = match i.and_then(|i| self.interners[h].lent[i]) {
                    Some(a) => a,
                    None => {
                        let text =
                            i.map_or(String::new(), |i| self.interners[h].strings[i].clone());
                        let Value::Box(b) = self.alloc_box("String", Value::Str(text)) else {
                            unreachable!()
                        };
                        let a = self.alloc(HeapObj {
                            count: 1,
                            weak: 0,
                            value: Value::Box(b),
                        });
                        self.snapshots.insert(b);
                        self.snapshots.insert(a);
                        if let Some(i) = i {
                            self.interners[h].lent[i] = Some(a);
                        }
                        a
                    }
                };
                Ok(Value::Ref(Addr {
                    root: Root::Heap(holder),
                    path: Vec::new(),
                }))
            }
            _ => Ok(Value::Int(self.interners[h].strings.len() as i128)),
        }
    }

    /// `tracing_emit_event`: the registered exporter's `export_event`
    /// receives the event, or the default `StdoutExporter` prints it as its
    /// body does: `[level] message`, each ` key=value`, then a non-zero
    /// ` span_id=`.
    fn emit_event(&mut self, event: Value, ety: Ty) -> R<()> {
        let event_ty = match self.tys.kind(ety) {
            TyKind::Ref(t) | TyKind::MutRef(t) => t,
            _ => ety,
        };
        let by_ref = matches!(event, Value::Ref(_));
        let owned = match &event {
            Value::Ref(at) => self.slot(at)?,
            v => v.clone(),
        };
        if let Some((sink, sink_ty)) = self.tracing_sink.clone() {
            let name = format!("{}.export_event", self.tys.adt_name(sink_ty));
            if let Some(body) = self.program.bodies.get(&name) {
                let takes_ref = matches!(
                    self.tys.kind(body.locals[2].ty),
                    TyKind::Ref(_) | TyKind::MutRef(_)
                );
                let recv = self.scratch_ref(sink);
                let mut scratch = vec![recv.1];
                let arg = if takes_ref {
                    let (r, id) = self.scratch_ref(owned);
                    scratch.push(id);
                    r
                } else if by_ref {
                    self.clone_value(&owned, event_ty)?
                } else {
                    owned
                };
                let out = self.call(&name, vec![recv.0, arg]);
                for id in scratch {
                    self.free_slot(id);
                }
                out?;
                if takes_ref && !by_ref {
                    self.drop_value(event, event_ty)?;
                }
                return Ok(());
            }
        }
        let Value::Agg(fs) = &owned else {
            return err(format!("a LogEvent is {owned:?}"));
        };
        let mut line = format!("[{}] {}", self.string_at(&fs[0])?, self.string_at(&fs[1])?);
        let fields = match &fs[2] {
            Value::Box(id) => self.vec_elems(*id)?.clone(),
            _ => Vec::new(),
        };
        for f in &fields {
            if let Value::Agg(kv) = f {
                let (k, v) = (self.string_at(&kv[0])?, self.string_at(&kv[1])?);
                line.push_str(&format!(" {k}={v}"));
            }
        }
        if let Some(Value::Int(id)) = fs.get(3).filter(|v| !matches!(v, Value::Int(0))) {
            line.push_str(&format!(" span_id={id}"));
        }
        line.push('\n');
        self.write_out(false, &line);
        if !by_ref {
            self.drop_value(event, event_ty)?;
        }
        Ok(())
    }

    /// A reference to a shallow copy of `v` in a scratch slot, which the
    /// caller frees without dropping.
    fn scratch_ref(&mut self, v: Value) -> (Value, AllocId) {
        let id = self.alloc(HeapObj {
            count: 1,
            weak: 0,
            value: v,
        });
        (
            Value::Ref(Addr {
                root: Root::Heap(id),
                path: Vec::new(),
            }),
            id,
        )
    }

    /// The heap a static's value reaches: live until exit, like the static.
    fn held_by_statics(&self) -> rustc_hash::FxHashSet<AllocId> {
        let mut seen = rustc_hash::FxHashSet::default();
        let mut todo: Vec<&Value> = self
            .statics
            .iter()
            .chain(self.tracing_sink.iter().map(|(v, _)| v))
            .collect();
        while let Some(v) = todo.pop() {
            match v {
                Value::Agg(fs) | Value::Variant(_, fs) => todo.extend(fs),
                Value::Box(a) | Value::Shared(a) | Value::Weak(a) => {
                    if seen.insert(*a) {
                        if let Some(Some(obj)) = self.heap.get(a.0 as usize) {
                            todo.push(&obj.value);
                        }
                    }
                }
                Value::Erased {
                    env: Some((a, _)), ..
                } => {
                    if seen.insert(*a) {
                        if let Some(Some(obj)) = self.heap.get(a.0 as usize) {
                            todo.push(&obj.value);
                        }
                    }
                }
                _ => {}
            }
        }
        seen
    }

    /// What is still allocated at exit and is a leak. A cycle of strong
    /// handles is never freed (core semantics §6.5), so an allocation whose
    /// every count is a handle held inside other leftover allocations is the
    /// cycle's, not a leak. An allocation counted more often than the
    /// leftovers hold it (a drop that lost a handle), or held by nothing
    /// (a value that was never dropped), is one, with whatever it reaches.
    fn leaks(&self) -> Option<String> {
        let held = self.held_by_statics();
        let live: Vec<AllocId> = self
            .heap
            .iter()
            .enumerate()
            .map(|(i, _)| AllocId(i as u32))
            .filter(|a| !self.snapshots.contains(a) && !held.contains(a))
            .filter(|a| self.heap[a.0 as usize].is_some())
            .collect();
        if live.is_empty() {
            return None;
        }
        // The handles each leftover allocation holds, strong and weak.
        let holds = |a: AllocId| -> Vec<(AllocId, bool)> {
            let mut out = Vec::new();
            let mut todo: Vec<&Value> = vec![&self.heap[a.0 as usize].as_ref().unwrap().value];
            while let Some(v) = todo.pop() {
                match v {
                    Value::Agg(fs) | Value::Variant(_, fs) => todo.extend(fs),
                    Value::Box(h) | Value::Shared(h) => out.push((*h, true)),
                    Value::Weak(h) if *h != EMPTY_WEAK => out.push((*h, false)),
                    Value::Erased {
                        env: Some((h, _)), ..
                    } => out.push((*h, true)),
                    _ => {}
                }
            }
            out
        };
        let mut strong: rustc_hash::FxHashMap<AllocId, u32> = Default::default();
        let mut weak: rustc_hash::FxHashMap<AllocId, u32> = Default::default();
        for &a in &live {
            for (h, is_strong) in holds(a) {
                *if is_strong { &mut strong } else { &mut weak }
                    .entry(h)
                    .or_default() += 1;
            }
        }
        let mut leaked: Vec<AllocId> = live
            .iter()
            .copied()
            .filter(|a| {
                let o = self.heap[a.0 as usize].as_ref().unwrap();
                o.count > strong.get(a).copied().unwrap_or(0)
                    || o.weak > weak.get(a).copied().unwrap_or(0)
            })
            .collect();
        let mut i = 0;
        while i < leaked.len() {
            for (h, _) in holds(leaked[i]) {
                if live.contains(&h) && !leaked.contains(&h) {
                    leaked.push(h);
                }
            }
            i += 1;
        }
        if leaked.is_empty() {
            return None;
        }
        leaked.sort();
        let text: Vec<String> = leaked
            .iter()
            .map(|a| {
                let o = self.heap[a.0 as usize].as_ref().unwrap();
                format!("{a} (count {})", o.count)
            })
            .collect();
        Some(format!("leaked at exit: {}", text.join(", ")))
    }
}

/// `String` methods [`Interp::string_text_method`] implements.
const STRING_TEXT_METHODS: &[&str] = &[
    "find",
    "cmp",
    "char_at",
    "contains",
    "starts_with",
    "ends_with",
    "trim",
    "to_uppercase",
    "to_lowercase",
    "replace",
    "repeat",
    "substring",
    "split",
    "bytes",
    "index_range",
    "char_at_byte",
    "lt",
    "sorted",
    "trim_start",
    "trim_end",
    "strip_prefix",
    "strip_suffix",
    "split_whitespace",
    "lines",
    "replacen",
    "char_count",
];

/// `char` methods [`Interp::char_method`] implements.
const CHAR_METHODS: &[&str] = &[
    "is_alphabetic",
    "is_numeric",
    "is_alphanumeric",
    "is_uppercase",
    "is_lowercase",
    "is_whitespace",
    "is_ascii",
    "is_ascii_digit",
    "is_ascii_alphabetic",
    "is_ascii_alphanumeric",
    "is_ascii_uppercase",
    "is_ascii_lowercase",
    "is_ascii_punctuation",
    "is_ascii_whitespace",
    "to_lowercase",
    "to_uppercase",
    "to_ascii_lowercase",
    "to_ascii_uppercase",
    "to_digit",
    "is_ascii_hexdigit",
    "is_digit",
];

/// The one char of a case mapping, or `None` when it maps to several.
fn single_scalar(mut it: impl Iterator<Item = char>) -> Option<char> {
    let first = it.next()?;
    it.next().is_none().then_some(first)
}

/// `Vec` methods [`Interp::vec_more_method`] implements.
const VEC_MORE_METHODS: &[&str] = &[
    "sort",
    "sort_unstable",
    "is_sorted",
    "sort_by",
    "sort_unstable_by",
    "sort_by_key",
    "sort_unstable_by_key",
    "sort_by_cached_key",
    "retain",
    "dedup",
    "join",
    "extend_from_slice",
    "extend",
    "append",
    "swap_remove",
    "resize",
    "fill",
];

/// A closure or function item held by a library method that calls it.
struct Callee<'a> {
    body: &'a Body,
    /// The heap slot holding a closure's environment.
    env: Option<AllocId>,
    /// The body takes its environment by value: it can be called once.
    by_value: bool,
    taken: bool,
}

/// The natural order of two key forms ([`Interp::key_form`]): numbers,
/// text and chars by value, `false < true`, aggregates field by field,
/// variants by declaration order and then payload.
fn cmp_key(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering::Equal;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Agg(xs), Value::Agg(ys)) => cmp_keys(xs, ys),
        (Value::Variant(i, xs), Value::Variant(j, ys)) => i.cmp(j).then_with(|| cmp_keys(xs, ys)),
        _ => Equal,
    }
}

fn cmp_keys(xs: &[Value], ys: &[Value]) -> std::cmp::Ordering {
    for (x, y) in xs.iter().zip(ys) {
        let o = cmp_key(x, y);
        if o.is_ne() {
            return o;
        }
    }
    xs.len().cmp(&ys.len())
}

/// `Vec[R].len` -> (`Vec[R]`, `len`); a plain `println` -> (`println`, ``).
/// A channel's queue: its element type, its capacity (0 is unbounded for a
/// `Channel`), and how many of each end are live.
struct Chan {
    queue: std::collections::VecDeque<Value>,
    elem: Ty,
    cap: usize,
    /// A `BoundedChannel`: capacity 0 holds nothing, and no end closes it.
    bounded: bool,
    senders: usize,
    receivers: usize,
}

fn split_method(name: &str) -> (&str, &str) {
    let mut depth = 0;
    let mut dot = None;
    for (i, c) in name.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            '.' if depth == 0 => dot = Some(i),
            _ => {}
        }
    }
    match dot {
        Some(i) => (&name[..i], &name[i + 1..]),
        None => (name, ""),
    }
}

fn locate(s: Stop, body: &Body, at: String) -> Stop {
    match s {
        // A deep recursion's chain stops growing once it is long: the
        // innermost frames say where it failed.
        Stop::Error(e) if e.len() > 2000 => match e.starts_with("... ") {
            true => Stop::Error(e),
            false => Stop::Error(format!("... {e}")),
        },
        Stop::Error(e) => Stop::Error(format!("in {} at {at}: {e}", body.instance.name)),
        abort => abort,
    }
}

/// A `u128` operation. Its values are held as their bits in an `i128`, so
/// they are compared, divided and shifted as `u128` here.
fn u128_binop(
    op: BinOp,
    a: u128,
    b: u128,
    cmp: impl Fn(std::cmp::Ordering) -> Value,
) -> R<(Value, bool)> {
    use BinOp::*;
    if op.is_comparison() {
        return Ok((cmp(a.cmp(&b)), false));
    }
    let (r, of) = match op {
        Add => a.overflowing_add(b),
        Sub => a.overflowing_sub(b),
        Mul => a.overflowing_mul(b),
        Div | Rem if b == 0 => return err("division by zero; the builder must guard it"),
        Div => (a / b, false),
        Rem => (a % b, false),
        BitAnd => (a & b, false),
        BitOr => (a | b, false),
        BitXor => (a ^ b, false),
        Shl | Shr if b >= 128 => return Ok((Value::Int(0), true)),
        Shl => (a << b, false),
        Shr => (a >> b, false),
        _ => unreachable!(),
    };
    Ok((Value::Int(r as i128), of))
}

/// The range of `it`, for a type narrower than `u128` (whose values the
/// interpreter holds as their bits in an `i128`).
fn range(it: IntTy) -> (i128, i128) {
    let bits = it.bits();
    if it.signed() {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// `v` reduced modulo 2^bits into the range of `it`.
fn wrap(v: i128, it: IntTy) -> i128 {
    let bits = it.bits();
    let shift = 128 - bits;
    if it.signed() {
        (v << shift) >> shift
    } else {
        ((v as u128) << shift >> shift) as i128
    }
}

fn to_bits(v: i128, it: IntTy) -> u128 {
    let shift = 128 - it.bits();
    (v as u128) << shift >> shift
}

fn from_bits(v: u128, it: IntTy) -> i128 {
    wrap(v as i128, it)
}

/// The width and signedness of an integer type named `name`.
fn int_width(name: &str) -> Option<(u32, bool)> {
    Some(match name {
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" | "isize" => (64, true),
        "i128" => (128, true),
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" | "usize" => (64, false),
        "u128" => (128, false),
        _ => return None,
    })
}

/// The order of two keys in `key_form`: numbers, text and chars by value,
/// tuples and structs field by field, enums by variant then payload.
fn key_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Agg(xs), Value::Agg(ys)) => xs
            .iter()
            .zip(ys)
            .map(|(x, y)| key_order(x, y))
            .find(|o| o.is_ne())
            .unwrap_or(xs.len().cmp(&ys.len())),
        (Value::Variant(i, xs), Value::Variant(j, ys)) => i.cmp(j).then_with(|| {
            xs.iter()
                .zip(ys)
                .map(|(x, y)| key_order(x, y))
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        }),
        _ => Ordering::Equal,
    }
}

/// `v` with each float replaced by its bits, which are equal exactly when
/// the floats are under the total order (every NaN as one).
fn total_form(v: Value) -> Value {
    match v {
        Value::Float(f) if f.is_nan() => Value::Int(f64::NAN.to_bits() as i128),
        Value::Float(f) => Value::Int(f.to_bits() as i128),
        Value::Agg(fs) => Value::Agg(fs.into_iter().map(total_form).collect()),
        Value::Variant(i, fs) => Value::Variant(i, fs.into_iter().map(total_form).collect()),
        v => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::DefId;
    use crate::mir::build::BodyBuilder;
    use crate::mir::ty::{AdtDef, VariantDef};

    fn inst(name: &str) -> InstanceId {
        InstanceId {
            def: DefId(0),
            args: Vec::new(),
            name: name.to_string(),
        }
    }

    fn fn_const(tys: &mut TyInterner, name: &str) -> Operand {
        let unit = tys.unit();
        Operand::Const(Const {
            ty: unit,
            kind: ConstKind::FnDef(inst(name)),
        })
    }

    fn int_const(ty: Ty, v: i64) -> Operand {
        Operand::Const(Const {
            ty,
            kind: ConstKind::Scalar(v as u128),
        })
    }

    fn unit_const(tys: &mut TyInterner) -> Rvalue {
        let unit = tys.unit();
        Rvalue::Use(Operand::Const(Const {
            ty: unit,
            kind: ConstKind::Unit,
        }))
    }

    struct World {
        tys: TyInterner,
        prog: Program,
        i64t: Ty,
        unit: Ty,
        boolt: Ty,
        /// `struct R { id: i64 }` whose Drop body prints `drop <id>`.
        rt: Ty,
    }

    fn world() -> World {
        let mut tys = TyInterner::new();
        let i64t = tys.int(IntTy::I64);
        let unit = tys.unit();
        let boolt = tys.bool();
        let r = tys.add_adt(AdtDef {
            def: DefId(1),
            name: "R".into(),
            is_enum: false,
            variants: vec![VariantDef {
                name: "R".into(),
                fields: vec![("id".into(), i64t)],
            }],
            has_drop_impl: true,
            is_copy: false,
        });
        let rt = tys.intern(TyKind::Adt(r));
        let rref = tys.intern(TyKind::MutRef(rt));
        let mut prog = Program::default();

        // fn drop_R(self: mut ref R) { print("drop "); println(self.id) }
        let mut b = BodyBuilder::new(inst("drop_R"), unit);
        let s = b.arg("self", rref);
        let t = b.temp(unit);
        let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
        let strt = tys.intern(TyKind::Str);
        let print = fn_const(&mut tys, "print");
        let println = fn_const(&mut tys, "println");
        b.terminate(
            bb0,
            TerminatorKind::Call {
                func: print,
                args: vec![Operand::Const(Const {
                    ty: strt,
                    kind: ConstKind::Str("drop ".into()),
                })],
                destination: t.into(),
                target: Some(bb1),
                unwind: UnwindAction::Abort,
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Call {
                func: println,
                args: vec![Operand::Copy(
                    Place::from(s).project(ProjElem::Deref).field(0, i64t),
                )],
                destination: t.into(),
                target: Some(bb2),
                unwind: UnwindAction::Abort,
            },
        );
        let u = unit_const(&mut tys);
        b.assign(bb2, Local::RETURN_PLACE, u);
        b.terminate(bb2, TerminatorKind::Return);
        prog.add(b.finish().unwrap());
        prog.drop_impls.insert(r, "drop_R".into());

        // fn consume(r: R) { drop(r) }
        let mut b = BodyBuilder::new(inst("consume"), unit);
        let a = b.arg("r", rt);
        let (bb0, bb1) = (b.new_block(), b.new_block());
        b.terminate(
            bb0,
            TerminatorKind::Drop {
                place: a.into(),
                target: bb1,
                unwind: UnwindAction::Abort,
            },
        );
        let u = unit_const(&mut tys);
        b.assign(bb1, Local::RETURN_PLACE, u);
        b.terminate(bb1, TerminatorKind::Return);
        prog.add(b.finish().unwrap());

        World {
            tys,
            prog,
            i64t,
            unit,
            boolt,
            rt,
        }
    }

    fn make_r(w: &World, b: &mut BodyBuilder, bb: BasicBlock, dest: Local, id: i64) {
        b.assign(
            bb,
            dest,
            Rvalue::Aggregate(
                AggregateKind::Adt {
                    ty: w.rt,
                    variant: VariantIdx(0),
                },
                vec![int_const(w.i64t, id)],
            ),
        );
    }

    /// `fn main(c: bool) { let r = R { id: 7 }; if c { consume(r) } }`,
    /// with the builder's unconditional scope-end `drop(r)` when
    /// `end_drop` is set and no drop at all otherwise.
    fn conditional_move(w: &mut World, phase: MirPhase, end_drop: bool) {
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let c = b.arg("c", w.boolt);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let t = b.temp(w.unit);
        let (bb0, bb1, bb2, bb3) = (b.new_block(), b.new_block(), b.new_block(), b.new_block());
        make_r(w, &mut b, bb0, r, 7);
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(c.into()),
                targets: SwitchTargets::if_else(bb1, bb2),
            },
        );
        let consume = fn_const(&mut w.tys, "consume");
        b.terminate(
            bb1,
            TerminatorKind::Call {
                func: consume,
                args: vec![Operand::Move(r.into())],
                destination: t.into(),
                target: Some(bb2),
                unwind: UnwindAction::Abort,
            },
        );
        if end_drop {
            b.terminate(
                bb2,
                TerminatorKind::Drop {
                    place: r.into(),
                    target: bb3,
                    unwind: UnwindAction::Abort,
                },
            );
        } else {
            b.terminate(bb2, TerminatorKind::Goto { target: bb3 });
        }
        let u = unit_const(&mut w.tys);
        b.assign(bb3, Local::RETURN_PLACE, u);
        b.terminate(bb3, TerminatorKind::Return);
        let mut body = b.finish().unwrap();
        body.phase = phase;
        w.prog.add(body);
    }

    #[test]
    fn mir_interp_built_drop_is_dynamic() {
        for c in [true, false] {
            let mut w = world();
            conditional_move(&mut w, MirPhase::Built, true);
            let res = run(&w.prog, &w.tys, "main", vec![Value::Bool(c)]);
            assert_eq!(res.outcome, Outcome::Returned(Value::Unit), "c={c}");
            assert_eq!(res.output, "drop 7\n", "c={c}");
            assert_eq!(res.exit_code(), Some(0));
        }
    }

    #[test]
    fn mir_interp_trace_of_a_move_into_a_callee() {
        let mut w = world();
        conditional_move(&mut w, MirPhase::Built, true);
        let res = run(&w.prog, &w.tys, "main", vec![Value::Bool(true)]);
        let expected = "\
enter main
init _2
move _2
enter consume
drop _1 R
drop_body R
enter drop_R
init _2
init _2
init _0
exit drop_R
init _0
exit consume
init _3
init _0
exit main
";
        assert_eq!(res.trace(), expected);
    }

    #[test]
    fn mir_interp_elaborated_drop_of_moved_place_is_an_error() {
        let mut w = world();
        conditional_move(&mut w, MirPhase::DropsElaborated, true);
        let res = run(&w.prog, &w.tys, "main", vec![Value::Bool(true)]);
        let Outcome::Error(e) = res.outcome else {
            panic!("expected an error, got {:?}", res.outcome)
        };
        assert!(e.contains("drop of _2, which is not initialized"), "{e}");
    }

    #[test]
    fn mir_interp_missing_drop_is_reported_at_return() {
        let mut w = world();
        conditional_move(&mut w, MirPhase::DropsElaborated, false);
        let res = run(&w.prog, &w.tys, "main", vec![Value::Bool(false)]);
        let Outcome::Error(e) = res.outcome else {
            panic!("expected an error, got {:?}", res.outcome)
        };
        assert!(e.contains("_2: R still owns a value at return"), "{e}");
        // On the path that moves it, nothing is missing.
        let res = run(&w.prog, &w.tys, "main", vec![Value::Bool(true)]);
        assert_eq!(res.outcome, Outcome::Returned(Value::Unit));
    }

    /// Tuple parts drop last to first; array elements first to last
    /// (core semantics §6).
    #[test]
    fn mir_interp_aggregate_drop_order() {
        let mut w = world();
        let tup = w.tys.intern(TyKind::Tuple(vec![w.rt, w.rt]));
        let arr = w.tys.intern(TyKind::Array(w.rt, 2));
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let (r1, r2) = (b.temp(w.rt), b.temp(w.rt));
        let (r3, r4) = (b.temp(w.rt), b.temp(w.rt));
        let t = b.user_local("t", tup, Mutability::Not);
        let a = b.user_local("a", arr, Mutability::Not);
        let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
        make_r(&w, &mut b, bb0, r1, 1);
        make_r(&w, &mut b, bb0, r2, 2);
        make_r(&w, &mut b, bb0, r3, 3);
        make_r(&w, &mut b, bb0, r4, 4);
        b.assign(
            bb0,
            t,
            Rvalue::Aggregate(
                AggregateKind::Tuple,
                vec![Operand::Move(r1.into()), Operand::Move(r2.into())],
            ),
        );
        b.assign(
            bb0,
            a,
            Rvalue::Aggregate(
                AggregateKind::Array(w.rt),
                vec![Operand::Move(r3.into()), Operand::Move(r4.into())],
            ),
        );
        // Reverse declaration order: `a` first, then `t`.
        b.terminate(
            bb0,
            TerminatorKind::Drop {
                place: a.into(),
                target: bb1,
                unwind: UnwindAction::Abort,
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Drop {
                place: t.into(),
                target: bb2,
                unwind: UnwindAction::Abort,
            },
        );
        let u = unit_const(&mut w.tys);
        b.assign(bb2, Local::RETURN_PLACE, u);
        b.terminate(bb2, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        let res = run(&w.prog, &w.tys, "main", vec![]);
        assert_eq!(res.outcome, Outcome::Returned(Value::Unit));
        assert_eq!(res.output, "drop 3\ndrop 4\ndrop 2\ndrop 1\n");
    }

    fn shared_node(w: &mut World) -> Ty {
        let n = w.tys.add_adt(AdtDef {
            def: DefId(5),
            name: "Node".into(),
            is_enum: false,
            variants: vec![VariantDef {
                name: "Node".into(),
                fields: vec![("r".into(), w.rt)],
            }],
            has_drop_impl: false,
            is_copy: false,
        });
        w.tys.intern(TyKind::Shared(n))
    }

    #[test]
    fn mir_interp_shared_counts_and_frees() {
        let mut w = world();
        let st = shared_node(&mut w);
        let mut b = BodyBuilder::new(inst("main"), w.unit);
        let r = b.temp(w.rt);
        let x = b.user_local("x", st, Mutability::Not);
        let y = b.user_local("y", st, Mutability::Not);
        let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
        make_r(&w, &mut b, bb0, r, 9);
        b.assign(
            bb0,
            x,
            Rvalue::Aggregate(
                AggregateKind::Shared {
                    ty: st,
                    variant: VariantIdx(0),
                },
                vec![Operand::Move(r.into())],
            ),
        );
        b.assign(bb0, y, Rvalue::Retain(x.into()));
        b.terminate(
            bb0,
            TerminatorKind::Drop {
                place: y.into(),
                target: bb1,
                unwind: UnwindAction::Abort,
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Drop {
                place: x.into(),
                target: bb2,
                unwind: UnwindAction::Abort,
            },
        );
        let u = unit_const(&mut w.tys);
        b.assign(bb2, Local::RETURN_PLACE, u);
        b.terminate(bb2, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        let res = run(&w.prog, &w.tys, "main", vec![]);
        assert_eq!(res.outcome, Outcome::Returned(Value::Unit));
        assert_eq!(res.output, "drop 9\n");
        let trace = res.trace();
        let rc: Vec<&str> = trace
            .lines()
            .filter(|l| {
                ["alloc", "free", "retain", "release"]
                    .iter()
                    .any(|k| l.starts_with(k))
            })
            .collect();
        assert_eq!(
            rc,
            [
                "alloc a0 shared Node",
                "retain a0 2",
                "release a0 1",
                "release a0 0",
                "free a0"
            ]
        );
    }

    #[test]
    fn mir_interp_checked_overflow_aborts_with_101() {
        let mut w = world();
        let i8t = w.tys.int(IntTy::I8);
        let pair = w.tys.intern(TyKind::Tuple(vec![i8t, w.boolt]));
        let mut b = BodyBuilder::new(inst("main"), i8t);
        let p = b.temp(pair);
        let (bb0, bb1, bb2) = (b.new_block(), b.new_block(), b.new_block());
        b.assign(
            bb0,
            p,
            Rvalue::CheckedBinaryOp(BinOp::Add, int_const(i8t, 127), int_const(i8t, 1)),
        );
        b.terminate(
            bb0,
            TerminatorKind::SwitchInt {
                discr: Operand::Copy(Place::from(p).field(1, w.boolt)),
                targets: SwitchTargets::if_else(bb1, bb2),
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Abort {
                reason: AbortReason::Overflow,
            },
        );
        b.assign(
            bb2,
            Local::RETURN_PLACE,
            Rvalue::Use(Operand::Copy(Place::from(p).field(0, i8t))),
        );
        b.terminate(bb2, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        let res = run(&w.prog, &w.tys, "main", vec![]);
        assert_eq!(res.outcome, Outcome::Aborted(AbortReason::Overflow));
        assert_eq!(res.exit_code(), Some(101));
        assert_eq!(
            res.events.last(),
            Some(&Event::Abort(AbortReason::Overflow))
        );
    }

    /// C8: `MIN % -1` overflows like `MIN / -1` and the checked op flags
    /// it; `Not` on an unsigned int stays within its width.
    #[test]
    fn mir_interp_signed_rem_overflow_and_unsigned_not() {
        let src = "
fn main() -> () {
    let mut _0: ();
    let _1: u8;
    let _2: ();
    let _3: (i64, bool);
    bb0: {
        _1 = Not(const 5_u8);
        _2 = println(copy _1) -> bb1;
    }
    bb1: {
        _3 = CheckedRem(const -9223372036854775808_i64, const -1_i64);
        switchInt(copy _3.1) -> [0: bb2, otherwise: bb3];
    }
    bb2: {
        _2 = println(const \"no trap\") -> bb4;
    }
    bb3: {
        abort(Overflow);
    }
    bb4: {
        _0 = const ();
        return;
    }
}
";
        let m = crate::mir::parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let r = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(r.output, "250\n");
        assert_eq!(r.exit_code(), Some(101), "{:?}", r.outcome);
    }

    /// C8: `-MIN` traps like the checked ops, and a library method reaches
    /// its box through a reference to a reference.
    #[test]
    fn mir_interp_neg_overflow_traps_and_ref_chains_reach_the_box() {
        let src = "
fn main() -> () {
    let mut _0: ();
    let _1: Vec[i64];
    let _2: ref Vec[i64];
    let _3: ref ref Vec[i64];
    let _4: i64;
    let _5: ();
    let _6: i8;
    bb0: {
        _1 = Vec[i64].new() -> bb1;
    }
    bb1: {
        _2 = &_1;
        _3 = &_2;
        _4 = Vec[i64].len(move _3) -> bb2;
    }
    bb2: {
        _5 = println(copy _4) -> bb3;
    }
    bb3: {
        _6 = Neg(const -128_i8);
        _5 = println(copy _6) -> bb4;
    }
    bb4: {
        drop(_1) -> bb5;
    }
    bb5: {
        _0 = const ();
        return;
    }
}
";
        let m = crate::mir::parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let r = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(r.output, "0\n");
        assert_eq!(r.exit_code(), Some(101), "{:?}", r.outcome);
    }

    /// A field read through a downcast of a `shared enum` follows the
    /// handle once: the downcast reaches the heap and the field stays there.
    #[test]
    fn mir_interp_reads_a_shared_enum_payload_through_one_handle_hop() {
        let src = "
enum V { Num(i64), Nil }

fn main() -> () {
    let mut _0: ();
    let _1: shared V;
    let _2: i64;
    let _3: ();
    bb0: {
        _1 = shared V.Num { const 42_i64 };
        _2 = copy (_1 as Num).0;
        _3 = println(copy _2) -> bb1;
    }
    bb1: {
        drop(_1) -> bb2;
    }
    bb2: {
        _0 = const ();
        return;
    }
}
";
        let m = crate::mir::parse_module(src).unwrap();
        let prog = Program::from_module(&m);
        let r = run(&prog, &m.tys, "main", vec![]);
        assert_eq!(r.output, "42\n", "{:?}", r.outcome);
        assert_eq!(r.exit_code(), Some(0), "{:?}", r.outcome);
    }

    #[test]
    fn mir_interp_read_after_move_is_an_error() {
        let mut w = world();
        let mut b = BodyBuilder::new(inst("main"), w.i64t);
        let r = b.user_local("r", w.rt, Mutability::Not);
        let s = b.user_local("s", w.rt, Mutability::Not);
        let bb0 = b.new_block();
        make_r(&w, &mut b, bb0, r, 1);
        b.assign(bb0, s, Rvalue::Use(Operand::Move(r.into())));
        b.assign(
            bb0,
            Local::RETURN_PLACE,
            Rvalue::Use(Operand::Copy(Place::from(r).field(0, w.i64t))),
        );
        b.terminate(bb0, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        let res = run(&w.prog, &w.tys, "main", vec![]);
        let Outcome::Error(e) = res.outcome else {
            panic!("expected an error, got {:?}", res.outcome)
        };
        assert!(e.contains("in main at bb0[2]"), "{e}");
        assert!(e.contains("uninitialized"), "{e}");
    }

    #[test]
    fn mir_interp_dangling_reference_is_an_error() {
        let mut w = world();
        let iref = w.tys.intern(TyKind::Ref(w.i64t));
        // fn escape() -> ref i64 { let x = 5; &x }
        let mut b = BodyBuilder::new(inst("escape"), iref);
        let x = b.user_local("x", w.i64t, Mutability::Not);
        let bb0 = b.new_block();
        b.assign(bb0, x, Rvalue::Use(int_const(w.i64t, 5)));
        b.assign(
            bb0,
            Local::RETURN_PLACE,
            Rvalue::Ref(BorrowKind::Shared, x.into()),
        );
        b.terminate(bb0, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        // fn main() -> i64 { *escape() }
        let mut b = BodyBuilder::new(inst("main"), w.i64t);
        let p = b.temp(iref);
        let (bb0, bb1) = (b.new_block(), b.new_block());
        let escape = fn_const(&mut w.tys, "escape");
        b.terminate(
            bb0,
            TerminatorKind::Call {
                func: escape,
                args: vec![],
                destination: p.into(),
                target: Some(bb1),
                unwind: UnwindAction::Abort,
            },
        );
        b.assign(
            bb1,
            Local::RETURN_PLACE,
            Rvalue::Use(Operand::Copy(Place::from(p).project(ProjElem::Deref))),
        );
        b.terminate(bb1, TerminatorKind::Return);
        w.prog.add(b.finish().unwrap());
        let res = run(&w.prog, &w.tys, "main", vec![]);
        let Outcome::Error(e) = res.outcome else {
            panic!("expected an error, got {:?}", res.outcome)
        };
        assert!(e.contains("dangling reference"), "{e}");
    }

    /// The core pins (`corpus/core/`), hand-lowered to elaborated MIR in
    /// `tests/mir/core/<pin>.mir`, print exactly the pin's expected output
    /// and exit with its code. Each pin's `expected.out` is the same oracle
    /// the drop reference model is checked against.
    #[test]
    fn mir_interp_runs_the_hand_lowered_core_pins() {
        run_core_pins("tests/mir/core", MirPhase::DropsElaborated);
    }

    /// The same pins as the builder emits them, before drop elaboration:
    /// no drop flags, and a scope-end `drop` of every owning local even
    /// where it was moved, which the `Built` phase runs dynamically. These
    /// are drop elaboration's input.
    #[test]
    fn mir_interp_runs_the_built_core_pins() {
        run_core_pins("tests/mir/core-built", MirPhase::Built);
    }

    /// The §6 trace of one pin, in full: a conditional move with its drop
    /// flag, where `f(true)` moves `a` into `take` (whose drop runs the
    /// user body) and `f(false)` drops it at `f`'s end.
    #[test]
    fn mir_interp_trace_of_the_conditional_move_pin() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let src =
            std::fs::read_to_string(root.join("tests/mir/core/drop_conditional_move.mir")).unwrap();
        let mut m = crate::mir::parse_module(&src).unwrap();
        for b in &mut m.bodies {
            b.phase = MirPhase::DropsElaborated;
        }
        let r = run(&Program::from_module(&m), &m.tys, "main", vec![]);
        assert_eq!(r.output, "took1\nd1\nend\nend\nd1\n");
        let body_drop = "drop_body R\nenter R.drop\ninit _2\ninit _0\nexit R.drop\n";
        let want = format!(
            "enter main\nenter f\ninit _2\nflag _3 true\nflag _3 false\nmove _2\n\
             enter take\ninit _2\ndrop _1 R\n{body_drop}init _0\nexit take\n\
             init _4\ninit _4\ninit _0\nexit f\ninit _1\n\
             enter f\ninit _2\nflag _3 true\ninit _4\ndrop _2 R\n{body_drop}\
             init _0\nexit f\ninit _1\ninit _0\nexit main\n"
        );
        assert_eq!(r.trace(), want);
    }

    /// Closure pins (core-semantics §9.4), each in elaborated and Built
    /// form: captures drop with the closure, last captured first, and a
    /// once-callable body that moves one capture out drops the rest at
    /// its end. Expected output is hand-derived in `tests/mir/closures`.
    #[test]
    fn mir_interp_runs_the_closure_pins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let want = |pin: &str| {
            let out = root.join("tests/mir/closures").join(format!("{pin}.out"));
            (std::fs::read_to_string(out).unwrap(), 0)
        };
        let elaborated = run_pin_files("tests/mir/closures", MirPhase::DropsElaborated, &want);
        let built = run_pin_files("tests/mir/closures-built", MirPhase::Built, &want);
        assert_eq!((elaborated, built), (2, 2));
    }

    /// The core `Vec` and `String` methods, with element drops on
    /// `index_set`, `pop` (through the caller) and `clear`.
    #[test]
    fn mir_interp_runs_the_library_method_pins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let want = |pin: &str| {
            let out = root.join("tests/mir/lib").join(format!("{pin}.out"));
            (std::fs::read_to_string(out).unwrap(), 0)
        };
        let ran = run_pin_files("tests/mir/lib", MirPhase::DropsElaborated, &want);
        assert_eq!(ran, 11);
    }

    /// A strict drop of a fieldless variant, a fieldless variant left in
    /// place at a return, and the discriminant of a `shared enum`.
    #[test]
    fn mir_interp_runs_the_enum_pins() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let want = |pin: &str| {
            let out = root.join("tests/mir/interp").join(format!("{pin}.out"));
            (std::fs::read_to_string(out).unwrap(), 0)
        };
        let ran = run_pin_files("tests/mir/interp", MirPhase::DropsElaborated, &want);
        assert_eq!(ran, 2);
    }

    #[test]
    fn mir_text_round_trips_closures() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for dir in ["tests/mir/closures", "tests/mir/closures-built"] {
            for e in std::fs::read_dir(root.join(dir)).unwrap() {
                let path = e.unwrap().path();
                if path.extension().is_none_or(|e| e != "mir") {
                    continue;
                }
                let m = crate::mir::parse_module(&std::fs::read_to_string(&path).unwrap())
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                let once = crate::mir::pretty_module(&m);
                assert!(once.contains("closure#"), "{}", path.display());
                let again = crate::mir::pretty_module(&crate::mir::parse_module(&once).unwrap());
                assert_eq!(once, again, "{}", path.display());
            }
        }
    }

    fn run_core_pins(dir: &str, phase: MirPhase) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let ran = run_pin_files(dir, phase, &|pin| {
            let pin_dir = root.join("corpus/core").join(pin);
            let expected = std::fs::read_to_string(pin_dir.join("expected.out")).unwrap();
            let meta = std::fs::read_to_string(pin_dir.join("meta.toml")).unwrap();
            let exit: i32 = meta
                .lines()
                .find_map(|l| l.strip_prefix("exit = "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            (expected, exit)
        });
        assert_eq!(ran, 23, "every runnable core pin has hand-written MIR");
    }

    /// Runs every `.mir` file in `dir` as `phase` and compares it with
    /// `want(pin)`, the expected output and exit code; returns how many ran.
    fn run_pin_files(dir: &str, phase: MirPhase, want: &dyn Fn(&str) -> (String, i32)) -> usize {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut ran = 0;
        let mut bad = Vec::new();
        let mut files: Vec<_> = std::fs::read_dir(root.join(dir))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "mir"))
            .collect();
        files.sort();
        for path in files {
            let pin = path.file_stem().unwrap().to_str().unwrap().to_string();
            let (expected, exit) = want(&pin);
            let src = std::fs::read_to_string(&path).unwrap();
            let mut m = crate::mir::parse_module(&src).unwrap_or_else(|e| panic!("{pin}: {e}"));
            for b in &mut m.bodies {
                b.phase = phase;
            }
            let prog = Program::from_module(&m);
            let r = run(&prog, &m.tys, "main", vec![]);
            ran += 1;
            if r.output != expected || r.exit_code() != Some(exit) {
                bad.push(format!(
                    "{pin}: exit {:?} (want {exit}), outcome {:?}\n--- got\n{}--- want\n{expected}",
                    r.exit_code(),
                    r.outcome,
                    r.output
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        ran
    }

    /// A cycle of strong handles is never freed (core semantics §6.5), so
    /// it is not a leak at exit. (A handle a body forgot to drop is caught
    /// at its return, before the exit check.)
    #[test]
    fn mir_interp_exit_leak_check_spares_strong_cycles() {
        let cycle = "
shared struct Node {
    val: i64,
    mut next: Option[Node],
}

fn main() {
    let a = Node { val: 1, next: None };
    let b = Node { val: 2, next: Some(a) };
    a.next = Some(b);
    println(f\"{a.val}\");
}
";
        let r = crate::mir::lower::run_source(cycle).unwrap();
        assert_eq!(
            (r.output.as_str(), r.exit_code()),
            ("1\n", Some(0)),
            "{:?}",
            r.outcome
        );
    }

    /// A `Result` of `Copy` parts is `Copy`, and one of shared handles is
    /// a handle aggregate (core semantics §1.1, §6.1; Gowtham 2026-10-09):
    /// using either as a value copies it rather than moving.
    #[test]
    fn mir_interp_result_is_copy_and_a_handle_aggregate() {
        let src = "
shared struct Node { val: i64 }

fn cls(r: own Result[Option[i64], i64]) -> i64 {
    match r {
        Result.Ok(Option.Some(x)) => x,
        Result.Ok(_) => -1,
        Result.Err(e) => e,
    }
}

fn val(r: own Result[Node, i64]) -> i64 {
    match r {
        Result.Ok(n) => n.val,
        Result.Err(e) => e,
    }
}

fn main() {
    let v: Vec[Result[Option[i64], i64]] = [Result.Ok(Option.Some(4)), Result.Err(7)];
    let a = cls(v[0]) + cls(v[1]) + cls(v[0]);
    let h: Result[Node, i64] = Result.Ok(Node { val: 30 });
    let b = val(h) + val(h);
    println(f\"{a} {b}\");
}
";
        let r = crate::mir::lower::run_source(src).unwrap();
        assert_eq!(
            (r.output.as_str(), r.exit_code()),
            ("15 60\n", Some(0)),
            "{:?}",
            r.outcome
        );
    }
}
