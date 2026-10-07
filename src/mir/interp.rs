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
use super::place_ty::place_ty;
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
    /// A `Slice[T]` view: `len` elements of the sequence at `base` (a
    /// `Vec`'s allocation or an array place), starting at `lo`.
    Slice {
        base: Addr,
        lo: u64,
        len: u64,
    },
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
/// The default step budget, which only stops a runaway loop: real
/// programs in the corpus run tens of millions of steps, and the corpus
/// runner's timeout bounds wall time. `KARAC_MIR_MAX_STEPS` overrides it.
const MAX_STEPS: u64 = 4_000_000_000;

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
        max_steps: std::env::var("KARAC_MIR_MAX_STEPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(MAX_STEPS),
        snapshots: Vec::new(),
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
    max_steps: u64,
    /// Read-only copies a library method hands out a view of (the bytes
    /// of a `String`): live until exit, and not leaks.
    snapshots: Vec<AllocId>,
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
    /// A library function with no body: `arg_tys` and `ret` are the
    /// argument and result types at the call site, which say what a
    /// generic method's element type is and how its `Option` is laid out.
    fn native(&mut self, name: &str, args: Vec<Value>, arg_tys: &[Ty], ret: Ty) -> R<Value> {
        let (ty_name, method) = split_method(name);
        let base = ty_name.split('[').next().unwrap_or(ty_name);
        match (base, method) {
            ("Env", "args") => {
                // argv[0] only: the MIR interpreter passes no arguments,
                // and legacy counts the program name.
                let prog = self.alloc_box("String", Value::Str("main".into()));
                Ok(self.alloc_box(ty_name, Value::Agg(vec![prog])))
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
            ("println" | "print", "") => {
                // The arguments print one after another (an f-string's
                // parts); a reference prints what it points to.
                let text = self.show(&args, arg_tys)?;
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
            (_, "as_slice" | "as_mut_slice" | "slice" | "slice_mut") | ("Slice" | "Array", _) => {
                self.view_method(name, method, args)
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
                        self.heap[id.0 as usize] = None;
                        self.events.push(Event::Free(*id));
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
            ("String", _) if STRING_TEXT_METHODS.contains(&method) => {
                self.string_text_method(name, method, args, ret)
            }
            ("char", "len_utf8") => match args.as_slice() {
                [Value::Char(c)] => Ok(Value::Int(c.len_utf8() as i128)),
                _ => err(format!("{name} takes a char")),
            },
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
                Ok(self.alloc_box(ty_name, Value::Agg(out)))
            }
            ("Vec", _) if VEC_MORE_METHODS.contains(&method) => {
                self.vec_more_method(ty_name, method, args, arg_tys)
            }
            ("Vec" | "String", _) => self.collection_method(ty_name, method, args, arg_tys, ret),
            ("FileSystem", "write" | "read_to_string") => self.fs_method(method, args, ret),
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
                | "count_ones" | "signum" | "sqrt" | "floor" | "ceil" | "round",
            ) if args
                .first()
                .is_some_and(|a| matches!(a, Value::Int(_) | Value::Float(_))) =>
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
            (true, "new" | "with_capacity", _) => Ok(self.alloc_box(ty_name, Value::Agg(vec![]))),
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
                let n = (*n).max(0) as usize;
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
                let want = self.key_form(needle)?;
                let elems = self.vec_elems(id)?.clone();
                for x in &elems {
                    if self.key_form(x)? == want {
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
                // `extend` consumes the other `Vec`, which is freed;
                // `append` empties one it borrows.
                let from = self.box_behind(other)?;
                let moved = std::mem::take(self.vec_elems(from)?);
                self.vec_elems(id)?.extend(moved);
                if method == "extend" {
                    if !matches!(other, Value::Box(_)) {
                        return err(format!("{name} takes the other Vec by value"));
                    }
                    self.heap[from.0 as usize] = None;
                    self.events.push(Event::Free(from));
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
                let slot = AllocId(self.heap.len() as u32);
                self.heap.push(Some(HeapObj { count: 1, value: v }));
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
        all.extend(args);
        self.call(&f.body.instance.name, all)
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
            self.heap[slot.0 as usize] = None;
        }
        Ok(())
    }

    /// Slice views over a `Vec`, an array or another slice: `as_slice`,
    /// `as_mut_slice`, `slice(lo, hi)` and `slice_mut(lo, hi)`, plus
    /// `len`, `index` and `index_mut` on a slice (or an array). A view
    /// borrows its elements; nothing is copied or dropped.
    fn view_method(&mut self, name: &str, method: &str, args: Vec<Value>) -> R<Value> {
        let Some(recv) = args.first() else {
            return err(format!("{name} needs a receiver"));
        };
        let (base, lo, len) = self.view_of(recv)?;
        match (method, &args[1..]) {
            ("as_slice" | "as_mut_slice", []) => Ok(Value::Slice { base, lo, len }),
            ("slice" | "slice_mut", [Value::Int(a), Value::Int(b)]) => {
                if *a < 0 || a > b || *b > len as i128 {
                    self.events.push(Event::Abort(AbortReason::BoundsCheck));
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
            ("index" | "index_mut", [Value::Int(i)]) => {
                if *i < 0 || *i >= len as i128 {
                    self.events.push(Event::Abort(AbortReason::BoundsCheck));
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
                let found = self.find_key(id, key, is_map)?;
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
                let Some(i) = self.find_key(id, key, true)? else {
                    self.events.push(Event::Abort(AbortReason::Panic));
                    return Err(Stop::Abort(AbortReason::Panic));
                };
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64, 1],
                }))
            }
            ("insert", [key, rest @ ..]) => {
                let found = self.find_key(id, key, is_map)?;
                match (is_map, found, rest) {
                    // An existing key is kept and the new one dropped;
                    // the old value is handed back.
                    (true, Some(i), [val]) => {
                        self.drop_value(key.clone(), key_ty)?;
                        let Value::Agg(entry) = &mut self.vec_elems(id)?[i] else {
                            return err(format!("{name}: a malformed entry"));
                        };
                        let old = std::mem::replace(&mut entry[1], val.clone());
                        self.option(ret, Some(old))
                    }
                    (true, None, [val]) => {
                        let entry = Value::Agg(vec![key.clone(), val.clone()]);
                        let at = self.insert_at(id, key, is_map, sorted)?;
                        self.vec_elems(id)?.insert(at, entry);
                        self.option(ret, None)
                    }
                    (false, Some(_), []) => {
                        self.drop_value(key.clone(), key_ty)?;
                        Ok(Value::Bool(false))
                    }
                    (false, None, []) => {
                        let at = self.insert_at(id, key, is_map, sorted)?;
                        self.vec_elems(id)?.insert(at, key.clone());
                        Ok(Value::Bool(true))
                    }
                    _ => err(format!("{name}: wrong arguments")),
                }
            }
            ("remove", [key]) => {
                let found = self.find_key(id, key, is_map)?;
                let Some(i) = found else {
                    return if is_map {
                        self.option(ret, None)
                    } else {
                        Ok(Value::Bool(false))
                    };
                };
                let removed = self.vec_elems(id)?.remove(i);
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
                let found = self.find_key(id, key, true)?;
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
            ("entry_or_insert" | "entry_or_insert_with", [key, val]) if is_map => {
                // `m.entry(k).or_insert(v)`, fused: a reference to the
                // stored value, inserting `v` (or `f()`) when `k` is new.
                // A found key drops the new key and the unused value.
                let found = self.find_key(id, key, true)?;
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
                        self.vec_elems(id)?
                            .insert(at, Value::Agg(vec![key.clone(), v]));
                        at
                    }
                };
                Ok(Value::Ref(Addr {
                    root: Root::Heap(id),
                    path: vec![i as u64, 1],
                }))
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
        let entries = self.vec_elems(id)?.clone();
        if !sorted {
            return Ok(entries.len());
        }
        let want = self.key_form(key)?;
        for (i, e) in entries.iter().enumerate() {
            let k = match (is_map, e) {
                (true, Value::Agg(pair)) => &pair[0],
                (false, k) => k,
                _ => return err("a malformed map entry"),
            };
            if key_order(&self.key_form(k)?, &want) == std::cmp::Ordering::Greater {
                return Ok(i);
            }
        }
        Ok(entries.len())
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
            ("read_to_string", [path]) => {
                let path = self.string_at(path)?;
                std::fs::read_to_string(path).map(Some)
            }
            _ => return err(format!("{name}: wrong arguments")),
        };
        match res {
            Ok(None) => self.variant_named(ret, None, "Ok", vec![Value::Unit]),
            Ok(Some(text)) => {
                let s = self.alloc_box("String", Value::Str(text));
                self.variant_named(ret, None, "Ok", vec![s])
            }
            Err(e) => {
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
        }
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
            let n = if (2..=36).contains(&radix) && (signed || !text.starts_with('-')) {
                i128::from_str_radix(text, radix as u32).ok()
            } else {
                None
            };
            n.filter(|n| {
                if signed {
                    let half = 1i128 << (bits - 1);
                    (-half..half).contains(n)
                } else {
                    *n >= 0 && (bits == 128 || *n < (1i128 << bits))
                }
            })
            .map(Value::Int)
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
    fn find_key(&mut self, id: AllocId, key: &Value, is_map: bool) -> R<Option<usize>> {
        let want = self.key_form(key)?;
        let entries = self.vec_elems(id)?.clone();
        for (i, e) in entries.iter().enumerate() {
            let k = match (is_map, e) {
                (true, Value::Agg(pair)) => &pair[0],
                (false, k) => k,
                _ => return err("a malformed map entry"),
            };
            if self.key_form(k)? == want {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    /// A key's value with references and boxes replaced by what they
    /// hold, so two keys compare equal when their contents do.
    fn key_form(&mut self, v: &Value) -> R<Value> {
        Ok(match v {
            Value::Ref(addr) => {
                let inner = self.slot(addr)?;
                self.key_form(&inner)?
            }
            Value::Box(id) => {
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
                self.events.push(Event::Retain(*id, c));
                Ok(Value::Shared(*id))
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
            // as `#[derive(Clone)]` does. A type with a user `Drop` body
            // owns something a field copy cannot duplicate, so it needs
            // its own `Clone`.
            (TyKind::Adt(a), Value::Agg(_) | Value::Variant(..))
                if !self.tys.adt(a).has_drop_impl =>
            {
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
        let scratch = self.heap.len();
        self.heap.push(Some(HeapObj { count: 1, value: v }));
        let at = Addr {
            root: Root::Heap(AllocId(scratch as u32)),
            path: Vec::new(),
        };
        self.drop_at(&at, ty)?;
        self.heap[scratch] = None;
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
                let hi = if args.len() > 2 { int(2)? } else { len };
                if lo < 0 || hi > len || lo >= hi {
                    return new_string(self, String::new());
                }
                let (lo, hi) = (lo as usize, hi as usize);
                if !s.is_char_boundary(lo) || !s.is_char_boundary(hi) {
                    self.events.push(Event::Abort(AbortReason::Panic));
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
                    self.events.push(Event::Abort(AbortReason::Panic));
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
                    self.events.push(Event::Abort(AbortReason::Panic));
                    return Err(Stop::Abort(AbortReason::Panic));
                }
                let t = s[lo as usize..hi as usize].to_string();
                new_string(self, t)
            }
            "split" => {
                let parts: Vec<String> = s.split(text(1)?).map(str::to_string).collect();
                let mut out = Vec::with_capacity(parts.len());
                for p in parts {
                    out.push(self.alloc_box("String", Value::Str(p)));
                }
                let vname = self.tys.display(ret);
                Ok(self.alloc_box(&vname, Value::Agg(out)))
            }
            "bytes" => {
                // A read-only `Slice[u8]` over a snapshot of the bytes.
                let bytes: Vec<Value> = s.bytes().map(|b| Value::Int(b as i128)).collect();
                let n = bytes.len() as u64;
                let Value::Box(id) = self.alloc_box("bytes", Value::Agg(bytes)) else {
                    unreachable!("alloc_box makes a box")
                };
                self.snapshots.push(id);
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
        let it = match self.tys.kind(ret) {
            TyKind::Int(it) => Some(it),
            _ => None,
        };
        let fit = |me: &mut Self, v: i128| -> R<Value> {
            match it {
                Some(it) if wrap(v, it) != v => {
                    me.events.push(Event::Abort(AbortReason::Overflow));
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
            ("count_ones", [Value::Int(a)]) => {
                let bits = match self.tys.kind(ret) {
                    TyKind::Int(_) => (*a as u128 & u64::MAX as u128).count_ones(),
                    _ => (*a as u128).count_ones(),
                };
                Ok(Value::Int(bits as i128))
            }
            ("sqrt", [Value::Float(a)]) => Ok(Value::Float(a.sqrt())),
            ("floor", [Value::Float(a)]) => Ok(Value::Float(a.floor())),
            ("ceil", [Value::Float(a)]) => Ok(Value::Float(a.ceil())),
            ("round", [Value::Float(a)]) => Ok(Value::Float(a.round())),
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
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => Ok(e),
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
            self.events.push(Event::Abort(AbortReason::BoundsCheck));
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
        let a = AllocId(self.heap.len() as u32);
        self.heap.push(Some(HeapObj { count: 1, value }));
        self.events.push(Event::Alloc(a, ty_name.to_string()));
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
            (TyKind::Ref(t) | TyKind::MutRef(t), Value::Ref(addr)) => {
                let inner = self.slot(addr)?;
                if !inner.fully_init() {
                    return err("print through a reference to an uninitialized value");
                }
                self.display_typed(&inner, t)?
            }
            (TyKind::Array(e, _), Value::Agg(fs)) => {
                let tys = vec![e; fs.len()];
                format!("[{}]", list(self, fs, &tys)?.join(", "))
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
            (
                TyKind::Intrinsic(IntrinsicTy::Map(kt, vt) | IntrinsicTy::SortedMap(kt, vt)),
                Value::Box(id),
            ) => {
                // `{k: v, ...}` in iteration order.
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
                let vname = var.name.clone();
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
                        TyKind::Ref(t) | TyKind::MutRef(t) => t,
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
                            // `-MIN` overflows, and C8 traps it.
                            self.events.push(Event::Abort(AbortReason::Overflow));
                            return Err(Stop::Abort(AbortReason::Overflow));
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
                    // `MIN % -1` is 0 mathematically, but it overflows on
                    // the machine exactly as `MIN / -1` does, and C8 traps
                    // both (Rust's `overflowing_rem`).
                    Rem if it.signed() && b == -1 && a == wrap(1 << (it.bits() - 1), it) => {
                        return Ok((Value::Int(0), true));
                    }
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
                let Value::Fn(inst) = f else {
                    return err("call of a value that is not a function item");
                };
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
                let ret = if self.program.bodies.contains_key(&inst.name) {
                    self.call(&inst.name, vals)?
                } else {
                    self.native(&inst.name, vals, &arg_tys, ret_ty)?
                };
                let Some(target) = target else {
                    return err(format!(
                        "{} returned, but the call site says it never does",
                        inst.name
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
                self.events
                    .push(Event::Init(self.place_str(body, destination)));
                Ok(Some(*target))
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
                self.heap[id.0 as usize] = None;
                self.events.push(Event::Free(id));
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
    fn owns_drop(&self, v: &Value, ty: Ty) -> bool {
        if !self.tys.needs_drop(ty) {
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
            let Some(f) = self.program.drop_impls.get(&a) else {
                return err(format!("no Drop body registered for {}", adt.name));
            };
            self.events.push(Event::DropBody(adt.name.clone()));
            let f = f.clone();
            match (self.tys.kind(ty), &addr.root) {
                // `fn T.drop(mut ref self)` on a `shared` type takes a
                // reference to a handle, as every field read through a
                // handle does. The object stays live, at count 0, until
                // the body returns.
                (TyKind::Shared(_), Root::Heap(id)) if addr.path.is_empty() => {
                    let scratch = self.heap.len();
                    self.heap.push(Some(HeapObj {
                        count: 0,
                        value: Value::Shared(*id),
                    }));
                    let handle = Addr {
                        root: Root::Heap(AllocId(scratch as u32)),
                        path: Vec::new(),
                    };
                    let r = self.call(&f, vec![Value::Ref(handle)]);
                    self.heap[scratch] = None;
                    r?;
                }
                _ => {
                    self.call(&f, vec![Value::Ref(addr.clone())])?;
                }
            }
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
            .filter(|(i, _)| !self.snapshots.contains(&AllocId(*i as u32)))
            .filter_map(|(i, o)| o.as_ref().map(|o| format!("a{i} (count {})", o.count)))
            .collect();
        if live.is_empty() {
            None
        } else {
            Some(format!("leaked at exit: {}", live.join(", ")))
        }
    }
}

/// `String` methods [`Interp::string_text_method`] implements.
const STRING_TEXT_METHODS: &[&str] = &[
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
];

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
        assert_eq!(ran, 9);
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
}
