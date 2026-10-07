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

use super::parse::MirModule;
use super::pretty;
use super::syntax::*;
use super::ty::{AdtId, IntTy, IntrinsicTy, Ty, TyInterner, TyKind};
use super::validate::validate;

/// The functions of a program, by instance name, and the `Drop` body of
/// each ADT that has one.
#[derive(Debug, Default)]
pub struct Program {
    pub bodies: BTreeMap<String, Body>,
    /// The instance name of each ADT's `Drop` body, which takes the value
    /// as its single `mut ref` parameter.
    pub drop_impls: BTreeMap<AdtId, String>,
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
        p
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AllocId(pub u32);

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
    /// A library collection (`String`, `Vec[T]`): the sole owner of its
    /// heap allocation, which holds a `Str` or the elements as an `Agg`.
    Box(AllocId),
    Ref(Addr),
    Fn(InstanceId),
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
            Value::Agg(fs) | Value::Variant(_, fs) => fs.iter().any(Value::any_init),
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
    /// The program did something the semantics forbid, or MIR the
    /// interpreter cannot run: a bug in the program or in an earlier pass.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub output: String,
    pub events: Vec<Event>,
    pub outcome: Outcome,
}

impl RunResult {
    pub fn exit_code(&self) -> Option<i32> {
        match self.outcome {
            Outcome::Returned(_) => Some(0),
            Outcome::Aborted(_) => Some(101),
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

const MAX_DEPTH: usize = 1000;
const MAX_STEPS: u64 = 10_000_000;

/// Runs `entry` with `args`, validating every body first.
pub fn run(program: &Program, tys: &TyInterner, entry: &str, args: Vec<Value>) -> RunResult {
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
        events: Vec::new(),
        output: String::new(),
        steps: 0,
    };
    let outcome = if !problems.is_empty() {
        Outcome::Error(format!("invalid MIR:\n{}", problems.join("\n")))
    } else {
        match it.call(entry, args) {
            Ok(v) => match it.leaks() {
                Some(leaks) => Outcome::Error(leaks),
                None => Outcome::Returned(v),
            },
            Err(Stop::Abort(r)) => Outcome::Aborted(r),
            Err(Stop::Error(e)) => Outcome::Error(e),
        }
    };
    RunResult {
        output: it.output,
        events: it.events,
        outcome,
    }
}

enum Stop {
    Abort(AbortReason),
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

struct Frame {
    id: u64,
    locals: Vec<Value>,
}

struct HeapObj {
    count: u32,
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
    events: Vec<Event>,
    output: String,
    steps: u64,
}

impl<'a> Interp<'a> {
    fn call(&mut self, name: &str, args: Vec<Value>) -> R<Value> {
        let Some(body) = self.program.bodies.get(name) else {
            return self.native(name, args);
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
        let mut locals = vec![Value::Uninit; body.locals.len()];
        for (i, a) in args.into_iter().enumerate() {
            locals[i + 1] = a;
        }
        self.next_frame_id += 1;
        self.frames.push(Frame {
            id: self.next_frame_id,
            locals,
        });
        self.events.push(Event::Enter(name.to_string()));
        let result = self.run_body(body);
        self.frames.pop();
        if result.is_ok() {
            self.events.push(Event::Exit(name.to_string()));
        }
        result
    }

    /// Library functions the interpreter implements directly. A method of
    /// a library type is named after the type instance, `Vec[R].len`.
    fn native(&mut self, name: &str, args: Vec<Value>) -> R<Value> {
        let (ty_name, method) = split_method(name);
        let base = ty_name.split('[').next().unwrap_or(ty_name);
        match (base, method) {
            ("println" | "print", "") => {
                // The arguments print one after another (an f-string's
                // parts); a reference prints what it points to.
                let mut text = String::new();
                for v in &args {
                    text.push_str(&self.display(v)?);
                }
                self.output.push_str(&text);
                if name == "println" {
                    self.output.push('\n');
                }
                Ok(Value::Unit)
            }
            ("String", "from") => {
                let [Value::Str(s)] = args.as_slice() else {
                    return err("String.from takes a string constant");
                };
                Ok(self.alloc_box(ty_name, Value::Str(s.clone())))
            }
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
                    self.events.push(Event::Abort(AbortReason::BoundsCheck));
                    return Err(Stop::Abort(AbortReason::BoundsCheck));
                }
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![*i as u64],
                }))
            }
            _ => err(format!("call of unknown function {name}")),
        }
    }

    fn alloc_box(&mut self, ty_name: &str, value: Value) -> Value {
        let a = AllocId(self.heap.len() as u32);
        self.heap.push(Some(HeapObj { count: 1, value }));
        self.events.push(Event::Alloc(a, ty_name.to_string()));
        Value::Box(a)
    }

    /// The allocation a library value, or a reference to one, owns.
    fn box_behind(&mut self, v: &Value) -> R<AllocId> {
        match v {
            Value::Box(a) => Ok(*a),
            Value::Ref(addr) => match self.slot(addr)? {
                Value::Box(a) => Ok(a),
                other => err(format!("expected a library value, found {other:?}")),
            },
            other => err(format!("expected a library value, found {other:?}")),
        }
    }

    /// How `print` shows a value.
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
        if self.steps > MAX_STEPS {
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
            if self.tys.needs_drop(ty) && v.any_init() {
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
            {
                match self.slot(&addr)? {
                    Value::Shared(a) => {
                        addr = Addr {
                            root: Root::Heap(a),
                            path: Vec::new(),
                        };
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
                        TyKind::Ref(t) | TyKind::MutRef(t) => t,
                        _ => return err("deref of a non-reference type"),
                    };
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
                let needs_drop = self.tys.needs_drop(ty);
                let slot = self.slot_mut(&addr)?;
                if needs_drop && slot.any_init() {
                    let what = pretty::place(body, self.tys, p);
                    return err(format!(
                        "assignment overwrites {what}, which still owns a value"
                    ));
                }
                *slot = v.clone();
                if p.projection.is_empty() && body.local(p.local).kind == LocalKind::DropFlag {
                    self.events
                        .push(Event::Flag(p.local, matches!(v, Value::Bool(true))));
                } else {
                    self.events.push(Event::Init(self.place_str(body, p)));
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
                let needs_drop = self.tys.needs_drop(ty);
                let slot = &mut self.frames.last_mut().expect("frame").locals[l.index()];
                if needs_drop && slot.any_init() {
                    return err(format!("StorageDead({l}) while it still owns a value"));
                }
                *slot = Value::Uninit;
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
            StatementKind::Nop => Ok(()),
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
                    self.events.push(Event::Move(self.place_str(body, p)));
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
            ConstKind::Float(bits) => Value::Float(f64::from_bits(*bits)),
            ConstKind::Str(s) => Value::Str(s.clone()),
            ConstKind::Unit | ConstKind::ZeroSized => Value::Unit,
            ConstKind::FnDef(inst) => Value::Fn(inst.clone()),
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
                self.events.push(Event::Retain(a, c));
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
                match (op, x, self.tys.kind(ty)) {
                    (UnOp::Not, Value::Bool(b), _) => Ok(Value::Bool(!b)),
                    (UnOp::Not, Value::Int(i), TyKind::Int(it)) => Ok(Value::Int(wrap(!i, it))),
                    (UnOp::Neg, Value::Int(i), TyKind::Int(it)) => {
                        let r = -i;
                        if wrap(r, it) != r {
                            return err("negation overflowed");
                        }
                        Ok(Value::Int(r))
                    }
                    (UnOp::Neg, Value::Float(f), _) => Ok(Value::Float(-f)),
                    (op, x, _) => err(format!("cannot apply {op:?} to {x:?}")),
                }
            }
            Rvalue::Cast(kind, o, to) => {
                let (x, _) = self.operand(body, o)?;
                let to_kind = self.tys.kind(*to).clone();
                match (kind, x, to_kind) {
                    (CastKind::IntToInt, Value::Int(i), TyKind::Int(it)) => {
                        Ok(Value::Int(wrap(i, it)))
                    }
                    (CastKind::IntToFloat, Value::Int(i), _) => Ok(Value::Float(i as f64)),
                    (CastKind::FloatToInt, Value::Float(f), TyKind::Int(it)) => {
                        let (lo, hi) = range(it);
                        Ok(Value::Int(if f.is_nan() {
                            0
                        } else {
                            (f as i128).clamp(lo, hi)
                        }))
                    }
                    (CastKind::FloatToFloat, Value::Float(f), TyKind::Float(ft)) => {
                        Ok(Value::Float(match ft {
                            super::ty::FloatTy::F32 => f as f32 as f64,
                            super::ty::FloatTy::F64 => f,
                        }))
                    }
                    (k, x, _) => err(format!("cannot cast {x:?} with {k:?}")),
                }
            }
            Rvalue::Discriminant(p) => {
                let (addr, _) = self.resolve_read(body, p)?;
                match self.slot(&addr)? {
                    Value::Variant(v, _) => Ok(Value::Int(v as i128)),
                    _ => err("discriminant of a value that is not an initialized enum"),
                }
            }
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
                        let a = AllocId(self.heap.len() as u32);
                        self.heap.push(Some(HeapObj { count: 1, value }));
                        self.events.push(Event::Alloc(a, self.tys.display(*ty)));
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
        match (x, y) {
            (Value::Int(a), Value::Int(b)) => {
                if op.is_comparison() {
                    return Ok((cmp(a.cmp(&b)), false));
                }
                let TyKind::Int(it) = self.tys.kind(ty) else {
                    return err("integer operands of a non-integer type");
                };
                let exact = match op {
                    Add => a + b,
                    Sub => a - b,
                    Mul => a.checked_mul(b).unwrap_or(i128::MAX),
                    Div | Rem if b == 0 => {
                        return err("division by zero; the builder must guard it")
                    }
                    Div => a / b,
                    Rem => a % b,
                    BitAnd => a & b,
                    BitOr => a | b,
                    BitXor => a ^ b,
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
                Ok((Value::Int(wrapped), wrapped != exact))
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
                    Add => Value::Float(a + b),
                    Sub => Value::Float(a - b),
                    Mul => Value::Float(a * b),
                    Div => Value::Float(a / b),
                    Rem => Value::Float(a % b),
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
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
            } => {
                let (f, _) = self.operand(body, func)?;
                let Value::Fn(inst) = f else {
                    return err("call of a value that is not a function item");
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.operand(body, a)?.0);
                }
                let ret = self.call(&inst.name, vals)?;
                let Some(target) = target else {
                    return err(format!(
                        "{} returned, but the call site says it never does",
                        inst.name
                    ));
                };
                let (addr, ty) = self
                    .resolve(body, destination, Mode::Write)?
                    .expect("write mode never probes");
                let needs_drop = self.tys.needs_drop(ty);
                let slot = self.slot_mut(&addr)?;
                if needs_drop && slot.any_init() {
                    return err("call result overwrites a place that still owns a value");
                }
                *slot = ret;
                self.events
                    .push(Event::Init(self.place_str(body, destination)));
                Ok(Some(*target))
            }
            TerminatorKind::Drop { place, target } => {
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
                self.events.push(Event::Drop(what, self.tys.display(ty)));
                self.drop_at(&addr, ty)?;
                *self.slot_mut(&addr)? = Value::Uninit;
                Ok(Some(*target))
            }
            TerminatorKind::Return => Ok(None),
            TerminatorKind::Abort { reason } => {
                self.events.push(Event::Abort(reason.clone()));
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
            TyKind::Intrinsic(k) => {
                let Value::Box(id) = v else {
                    return err(format!("a {} place holds no box", self.tys.display(ty)));
                };
                match k {
                    IntrinsicTy::String => {}
                    IntrinsicTy::Vec(e) => {
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
                    IntrinsicTy::Map(..) | IntrinsicTy::Set(_) => {
                        return err(format!(
                            "drop of {} is not implemented in the interpreter yet",
                            self.tys.display(ty)
                        ))
                    }
                }
                self.live(id)?;
                self.heap[id.0 as usize] = None;
                self.events.push(Event::Free(id));
                Ok(())
            }
            _ => Ok(()),
        }
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
            let Some(f) = self.program.drop_impls.get(&a) else {
                return err(format!("no Drop body registered for {}", adt.name));
            };
            self.events.push(Event::DropBody(adt.name.clone()));
            self.call(f, vec![Value::Ref(addr.clone())])?;
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

    fn release(&mut self, id: AllocId, ty: Ty, a: AdtId) -> R<()> {
        let obj = self.live(id)?;
        obj.count -= 1;
        let c = obj.count;
        self.events.push(Event::Release(id, c));
        if c == 0 {
            let root = Addr {
                root: Root::Heap(id),
                path: Vec::new(),
            };
            self.drop_adt(&root, ty, a)?;
            self.heap[id.0 as usize] = None;
            self.events.push(Event::Free(id));
        }
        Ok(())
    }

    fn leaks(&self) -> Option<String> {
        let live: Vec<String> = self
            .heap
            .iter()
            .enumerate()
            .filter_map(|(i, o)| o.as_ref().map(|o| format!("a{i} (count {})", o.count)))
            .collect();
        if live.is_empty() {
            None
        } else {
            Some(format!("leaked at exit: {}", live.join(", ")))
        }
    }
}

/// `Vec[R].len` -> (`Vec[R]`, `len`); a plain `println` -> (`println`, ``).
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
        Stop::Error(e) => Stop::Error(format!("in {} at {at}: {e}", body.instance.name)),
        abort => abort,
    }
}

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
            },
        );
        if end_drop {
            b.terminate(
                bb2,
                TerminatorKind::Drop {
                    place: r.into(),
                    target: bb3,
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
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Drop {
                place: t.into(),
                target: bb2,
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
            },
        );
        b.terminate(
            bb1,
            TerminatorKind::Drop {
                place: x.into(),
                target: bb2,
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
}
