//! The REPL's carry (B-2026-11-19-3): what the interpreter keeps of one
//! cell's run for the next, so a session is one interpreter (one heap, one
//! task pool, one logger) rather than one program per cell that rebuilt its
//! bindings by running the cells that made them again.
//!
//! Each cell is a program of its own, compiled from the session's items and
//! the cell. A run in carry mode ([`carry_on`]) starts from the state the
//! last committed run left ([`carry_commit`]), and its cell's bindings start
//! as the values themselves: `let x: T = __karac_repl_take("x");`
//! ([`REPL_TAKE_FN`]). Values are almost free of the program they were made
//! in; the few parts that name it (a type, a function instance, a `dyn`
//! table, a task's frame) are re-found in the new program by name when the
//! run starts ([`carry_in`]), and a part that cannot be found refuses the
//! carry ([`CARRY_REFUSED`]): the REPL then starts the cell as it did
//! before, from the values' source forms or a replay.

use super::*;
use rustc_hash::{FxHashMap, FxHashSet};

/// The function a carried cell's bindings start from: it returns the value
/// the session holds under its name.
pub const REPL_TAKE_FN: &str = "__karac_repl_take";

/// What a run's error starts with when the carry cannot be given to its
/// program.
pub const CARRY_REFUSED: &str = "repl carry refused";

/// The value text a [`ReplBinding`] has when the binding has no source form
/// but is carried: a cell can start from it only by taking it.
pub const CARRIED: &str = "\u{0}carried";

/// The interpreter state that is values: what a failed cell rolls back.
#[derive(Clone, Default)]
pub(super) struct Data {
    heap: Vec<Option<HeapObj>>,
    free: Vec<u32>,
    key_index: FxHashMap<AllocId, Option<FxHashMap<Vec<u8>, usize>>>,
    table_holes: FxHashMap<AllocId, Holes>,
    unkeyed_tables: FxHashSet<AllocId>,
    deque_front: FxHashMap<AllocId, usize>,
    snapshots: FxHashSet<AllocId>,
    bytes_snapshots: Vec<(Box<[u8]>, AllocId)>,
    forgotten: Vec<Value>,
    arenas: i128,
    channels: Vec<Chan>,
    chan_epoch: i64,
    caps: FxHashMap<AllocId, usize>,
    sorted_tables: FxHashSet<AllocId>,
    table_key_ty: FxHashMap<AllocId, Ty>,
    tasks: Vec<TaskSlot>,
    waiting_tasks: Vec<(usize, Option<Wait>)>,
    log_ids: i128,
    handles: Vec<HandleSlot>,
    groups: Vec<GroupSlot>,
    pools: Vec<PoolEntry>,
    semaphores: Vec<i128>,
    limiters: Vec<Limiter>,
    interners: Vec<Interned>,
    log_sink: Option<(Value, Ty, Value)>,
    tracing_sink: Option<(Value, Ty)>,
    log_min: i128,
    tracing_min: i128,
    next_frame_id: u64,
    rand: u64,
    walk_perm: (u64, u64, u64),
}

/// The interpreter state that is the process's: files, sockets, processes.
/// It is not rolled back (what a failed cell wrote stays written).
#[derive(Default)]
pub(super) struct Io {
    files: Vec<Option<std::fs::File>>,
    fs_extents: Vec<FsExtent>,
    fake_files: Vec<Option<(usize, i128)>>,
    net_extents: Vec<NetExtent>,
    fake_sockets: Vec<Option<(usize, i128)>>,
    bufs: Vec<Option<Buf>>,
    lines_ended: Vec<i128>,
    stdin_lines_ended: bool,
    buf_windows: FxHashMap<i128, AllocId>,
    procs: Procs,
    sockets: Sockets,
    regexes: std::collections::HashMap<String, Option<regex::Regex>>,
    crypto: CryptoStates,
    http_headers: FxHashMap<AllocId, Vec<(String, String)>>,
    http_bodies: FxHashMap<AllocId, Vec<u8>>,
    http_error_responses: FxHashMap<AllocId, HttpAnswer>,
    http_requests: FxHashMap<usize, crate::http1::Request>,
    http_progress: FxHashMap<usize, crate::http1::Progress>,
    built_requests: Vec<Option<(String, crate::http1::Request)>>,
    free_requests: Vec<usize>,
    pinned_texts: FxHashMap<String, Box<[u8]>>,
}

/// A run's state with the bindings its cell ended with, and how the types
/// and `dyn` tables in it were written in the program that made it.
#[derive(Clone)]
struct Carried {
    data: Data,
    bindings: Vec<(String, Value)>,
    ty_keys: FxHashMap<Ty, String>,
    vt_keys: FxHashMap<u32, (String, String)>,
    /// Each waiting task's frame, field by field, as the program that
    /// suspended it laid it out: a program that builds the task's body
    /// another way (another cell made the program cancel, say) cannot
    /// resume it.
    frames: FxHashMap<usize, Vec<String>>,
}

/// The fields of the frame type `t`, written out.
fn frame_layout(tys: &TyInterner, t: Ty) -> Vec<String> {
    let mut out = vec![tys.display(t)];
    let mut i = 0;
    while let Some(f) = tys.field_ty(t, None, i) {
        out.push(tys.display(f));
        i += 1;
    }
    out
}

#[derive(Default)]
struct State {
    /// This thread's runs collect a carry.
    on: bool,
    /// The next run starts from the committed carry.
    take: bool,
    committed: Option<Carried>,
    /// The last run's, until the REPL accepts its cell.
    pending: Option<Carried>,
    /// Whether every binding the last run recorded is in `pending`.
    complete: bool,
    io: Option<Io>,
}

thread_local! {
    static STATE: std::cell::RefCell<State> = std::cell::RefCell::new(State::default());
}

/// Turns carry mode on for this thread's runs (the REPL's worker), or off
/// with everything it held.
pub fn carry_on(on: bool) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        *s = State::default();
        s.on = on;
    });
}

/// Whether the next run starts from the committed carry (its program takes
/// its bindings with [`REPL_TAKE_FN`]) or from a fresh heap.
pub fn carry_take(take: bool) {
    STATE.with(|s| s.borrow_mut().take = take);
}

/// Accepts the last run: its state is what the next run starts from, with
/// its bindings named `names` (in the order the run recorded them; `None`
/// for one the session does not keep). Returns whether the run carried
/// every binding it recorded; when it did not, nothing is committed and the
/// committed carry is dropped.
pub fn carry_commit(names: &[Option<String>]) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let pending = s.pending.take();
        match pending {
            Some(mut c) if s.complete && c.bindings.len() == names.len() => {
                let mut kept = Vec::new();
                for ((_, v), n) in c.bindings.drain(..).zip(names) {
                    if let Some(n) = n {
                        kept.push((n.clone(), v));
                    }
                }
                c.bindings = kept;
                s.committed = Some(c);
                true
            }
            _ => {
                s.committed = None;
                false
            }
        }
    })
}

/// Drops the bindings `names` from the committed carry (a redefined type's
/// values, which no later cell can name). Their values are left in the
/// heap, unreached.
pub fn carry_forget(names: &[String]) {
    STATE.with(|s| {
        if let Some(c) = s.borrow_mut().committed.as_mut() {
            c.bindings.retain(|(n, _)| !names.contains(n));
        }
    })
}

/// The names of the cell functions (`__karac_cell_N`) whose bodies the
/// committed carry still calls: a closure or a task one made. A later
/// program must define them for the carry to be given to it.
pub fn carry_bodies_used() -> FxHashSet<String> {
    STATE.with(|s| {
        let s = s.borrow();
        let mut used = FxHashSet::default();
        if let Some(c) = &s.committed {
            let mut note = |name: &str| {
                used.insert(name.to_string());
            };
            for t in c.data.tasks.iter().filter(|t| !t.resume.is_empty()) {
                note(&t.resume);
            }
            for_each_value(&c.data, &c.bindings, &mut |v| {
                match v {
                    Value::Erased { body, .. } => note(body),
                    Value::Fn(inst) => note(&inst.name),
                    _ => {}
                }
                Ok(())
            })
            .ok();
        }
        used
    })
}

/// Runs `f` on every value the carry holds, and every value inside one.
fn for_each_value(
    d: &Data,
    bindings: &[(String, Value)],
    f: &mut dyn FnMut(&Value) -> Result<(), String>,
) -> Result<(), String> {
    fn walk(v: &Value, f: &mut dyn FnMut(&Value) -> Result<(), String>) -> Result<(), String> {
        f(v)?;
        match v {
            Value::Agg(xs) | Value::Variant(_, xs) => xs.iter().try_for_each(|x| walk(x, f)),
            _ => Ok(()),
        }
    }
    for obj in d.heap.iter().flatten() {
        walk(&obj.value, f)?;
    }
    for t in &d.tasks {
        for (a, b) in &t.log_held {
            walk(a, f)?;
            walk(b, f)?;
        }
    }
    for (_, v) in bindings {
        walk(v, f)?;
    }
    for v in &d.forgotten {
        walk(v, f)?;
    }
    for c in &d.channels {
        c.queue.iter().try_for_each(|v| walk(v, f))?;
    }
    for h in &d.handles {
        if let Some(v) = &h.result {
            walk(v, f)?;
        }
    }
    for p in &d.pools {
        walk(&p.create, f)?;
        if let Some(v) = &p.check {
            walk(v, f)?;
        }
        p.slots.iter().try_for_each(|v| walk(v, f))?;
    }
    if let Some((a, _, b)) = &d.log_sink {
        walk(a, f)?;
        walk(b, f)?;
    }
    if let Some((a, _)) = &d.tracing_sink {
        walk(a, f)?;
    }
    Ok(())
}

/// [`for_each_value`], able to change each value.
fn for_each_value_mut(
    d: &mut Data,
    bindings: &mut [(String, Value)],
    f: &mut dyn FnMut(&mut Value) -> Result<(), String>,
) -> Result<(), String> {
    fn walk(
        v: &mut Value,
        f: &mut dyn FnMut(&mut Value) -> Result<(), String>,
    ) -> Result<(), String> {
        f(v)?;
        match v {
            Value::Agg(xs) | Value::Variant(_, xs) => xs.iter_mut().try_for_each(|x| walk(x, f)),
            _ => Ok(()),
        }
    }
    for obj in d.heap.iter_mut().flatten() {
        walk(&mut obj.value, f)?;
    }
    for t in &mut d.tasks {
        for (a, b) in &mut t.log_held {
            walk(a, f)?;
            walk(b, f)?;
        }
    }
    for (_, v) in bindings.iter_mut() {
        walk(v, f)?;
    }
    for v in &mut d.forgotten {
        walk(v, f)?;
    }
    for c in &mut d.channels {
        c.queue.iter_mut().try_for_each(|v| walk(v, f))?;
    }
    for h in &mut d.handles {
        if let Some(v) = &mut h.result {
            walk(v, f)?;
        }
    }
    for p in &mut d.pools {
        walk(&mut p.create, f)?;
        if let Some(v) = &mut p.check {
            walk(v, f)?;
        }
        p.slots.iter_mut().try_for_each(|v| walk(v, f))?;
    }
    if let Some((a, _, b)) = &mut d.log_sink {
        walk(a, f)?;
        walk(b, f)?;
    }
    if let Some((a, _)) = &mut d.tracing_sink {
        walk(a, f)?;
    }
    Ok(())
}

/// The types of `d` that are found again by how they are written (the
/// others, a closure's environment and a task's frame, are found by the
/// body that takes them).
fn written_tys(d: &mut Data) -> Vec<&mut Ty> {
    let mut out: Vec<&mut Ty> = Vec::new();
    out.extend(d.table_key_ty.values_mut());
    out.extend(d.handles.iter_mut().map(|h| &mut h.held));
    out.extend(d.channels.iter_mut().map(|c| &mut c.elem));
    out.extend(d.pools.iter_mut().map(|p| &mut p.elem));
    if let Some((_, t, _)) = &mut d.log_sink {
        out.push(t);
    }
    if let Some((_, t)) = &mut d.tracing_sink {
        out.push(t);
    }
    out
}

/// The type `body` takes its environment or frame as (its first
/// parameter, read through a reference).
fn env_of(program: &Program, tys: &TyInterner, body: &str) -> Result<Ty, String> {
    let b = program
        .bodies
        .get(body)
        .ok_or_else(|| format!("no body `{body}` in this cell's program"))?;
    let t = b
        .locals
        .get(1)
        .ok_or_else(|| format!("`{body}` takes no environment"))?
        .ty;
    Ok(match tys.kind(t) {
        TyKind::Ref(t) | TyKind::MutRef(t) => t,
        _ => t,
    })
}

/// Every type `program`'s bodies name, by how it is written.
fn written_index(program: &Program, tys: &TyInterner) -> FxHashMap<String, Ty> {
    fn add(tys: &TyInterner, t: Ty, seen: &mut FxHashSet<Ty>, out: &mut FxHashMap<String, Ty>) {
        if !seen.insert(t) {
            return;
        }
        out.entry(tys.display(t)).or_insert(t);
        let mut parts: Vec<Ty> = Vec::new();
        match tys.kind(t) {
            TyKind::Tuple(ts) => parts.extend(ts),
            TyKind::Array(e, _) | TyKind::Slice(e) => parts.push(e),
            TyKind::Weak(e) | TyKind::Ref(e) | TyKind::MutRef(e) => parts.push(e),
            TyKind::RawPtr { pointee, .. } => parts.push(pointee),
            TyKind::Intrinsic(i) => match i {
                IntrinsicTy::String => {}
                IntrinsicTy::Vec(e)
                | IntrinsicTy::Set(e)
                | IntrinsicTy::VecDeque(e)
                | IntrinsicTy::SortedSet(e) => parts.push(e),
                IntrinsicTy::Map(k, v) | IntrinsicTy::SortedMap(k, v) => parts.extend([k, v]),
            },
            TyKind::Fn { params, ret, .. } => {
                parts.extend(params);
                parts.push(ret);
            }
            TyKind::Closure(_, caps) => parts.extend(caps),
            TyKind::Dyn { args, .. } => parts.extend(args),
            TyKind::Adt(_) | TyKind::Shared(_) => {
                if let Some((adt, args)) = tys.tcx().adt_of(t) {
                    parts.extend(args);
                    for (k, var) in adt.variants.iter().enumerate() {
                        let v = adt.is_enum.then_some(k as u32);
                        for i in 0..var.fields.len() {
                            parts.extend(tys.field_ty(t, v, i as u32));
                        }
                    }
                }
            }
            _ => {}
        }
        for p in parts {
            add(tys, p, seen, out);
        }
    }
    let mut seen = FxHashSet::default();
    let mut out = FxHashMap::default();
    for b in program.bodies.values() {
        for l in &b.locals {
            add(tys, l.ty, &mut seen, &mut out);
        }
    }
    for vt in &program.vtables {
        add(tys, vt.dyn_ty, &mut seen, &mut out);
        add(tys, vt.concrete, &mut seen, &mut out);
    }
    out
}

/// `c` as the program `it` runs writes it: each type, function, `dyn`
/// table and frame found again by name.
fn translate(it: &Interp<'_>, c: &mut Carried) -> Result<(), String> {
    let program = it.program;
    let tys = it.tys;
    let mut index: Option<FxHashMap<String, Ty>> = None;
    let ty_keys = std::mem::take(&mut c.ty_keys);
    for t in written_tys(&mut c.data) {
        let key = ty_keys
            .get(t)
            .ok_or_else(|| "a type the last cell did not write down".to_string())?;
        let index = index.get_or_insert_with(|| written_index(program, tys));
        // A type no body of the program names belongs to a part of the
        // state the program cannot reach (a handle whose task ended and
        // whose result went, a channel nothing holds an end of): it stands
        // as `()`, which nothing will ask.
        *t = *index
            .get(key)
            .or_else(|| index.get("()"))
            .ok_or_else(|| format!("no type `{key}` in this cell's program"))?;
    }
    for (k, task) in c.data.tasks.iter_mut().enumerate() {
        if task.resume.is_empty() {
            continue;
        }
        let b = program
            .bodies
            .get(&task.resume)
            .ok_or_else(|| format!("no body `{}` for a running task", task.resume))?;
        let TyKind::MutRef(frame) = tys.kind(b.locals[1].ty) else {
            return Err(format!("`{}` does not take its frame", task.resume));
        };
        if c.frames.get(&k) != Some(&frame_layout(tys, frame)) {
            return Err(format!(
                "`{}` lays its frame out anew in this cell's program",
                task.resume
            ));
        }
        task.frame_ty = frame;
    }
    let vt_keys = std::mem::take(&mut c.vt_keys);
    let mut vtables: FxHashMap<u32, u32> = FxHashMap::default();
    for (old, (name, concrete)) in &vt_keys {
        let at = program
            .vtables
            .iter()
            .position(|vt| &vt.name == name && &tys.display(vt.concrete) == concrete)
            .ok_or_else(|| format!("no `{name}` table for `{concrete}` in this cell's program"))?;
        vtables.insert(*old, at as u32);
    }
    let Carried { data, bindings, .. } = c;
    for_each_value_mut(data, bindings, &mut |v| {
        match v {
            Value::Erased {
                body,
                env: Some((_, t)),
            } => *t = env_of(program, tys, body)?,
            Value::Erased { body, env: None } if !program.bodies.contains_key(body.as_str()) => {
                return Err(format!("no function `{body}` in this cell's program"));
            }
            Value::Fn(inst) => {
                let b = program
                    .bodies
                    .get(&inst.name)
                    .ok_or_else(|| format!("no function `{}` in this cell's program", inst.name))?;
                *inst = b.instance.clone();
            }
            Value::Dyn { vtable, .. } | Value::DynView { vtable, .. } => {
                *vtable = *vtables
                    .get(vtable)
                    .ok_or_else(|| "a `dyn` table the last cell did not write down".to_string())?;
            }
            _ => {}
        }
        Ok(())
    })
}

/// Gives the run `it` is about to start the carry, when its thread
/// collects one: the process state, and the committed values when the
/// program takes them. Refuses, leaving everything as it was, when a part
/// of the values cannot be found in `it`'s program.
pub(super) fn carry_in(it: &mut Interp<'_>) -> Result<(), String> {
    let (on, take) = STATE.with(|s| {
        let s = s.borrow();
        (s.on, s.take)
    });
    if !on {
        return Ok(());
    }
    let mut start = None;
    if take {
        let committed = STATE.with(|s| s.borrow().committed.clone());
        let Some(mut c) = committed else {
            return Err("the session holds no values".to_string());
        };
        translate(it, &mut c)?;
        start = Some(c);
    }
    it.repl_carry = Some(Vec::new());
    if let Some(io) = STATE.with(|s| s.borrow_mut().io.take()) {
        install_io(it, io);
    }
    if let Some(c) = start {
        install_data(it, c.data);
        it.repl_take = c.bindings;
        // `main` runs as a task after the session's earlier ones.
        it.root_task = it.tasks.len();
        it.cur_task = it.root_task;
    }
    Ok(())
}

/// Takes the carry back from the finished run `it`: the process state
/// always, and, when the run `ok`, its values with the bindings it
/// recorded, as the pending carry the REPL commits if it accepts the cell.
pub(super) fn carry_out(it: &mut Interp<'_>, ok: bool) {
    let Some(recorded) = it.repl_carry.take() else {
        return;
    };
    let io = extract_io(it);
    let complete = recorded.iter().all(Option::is_some);
    let pending = ok.then(|| {
        let mut data = extract_data(it);
        // A task the run did not leave waiting has ended: its slot stays,
        // as handles and groups name it by place, but there is nothing of
        // it to run again.
        let waiting: FxHashSet<usize> = data.waiting_tasks.iter().map(|(t, _)| *t).collect();
        let mut frames = FxHashMap::default();
        for (k, t) in data.tasks.iter_mut().enumerate() {
            if waiting.contains(&k) {
                frames.insert(k, frame_layout(it.tys, t.frame_ty));
            } else {
                t.resume.clear();
            }
        }
        let bindings: Vec<(String, Value)> = recorded
            .into_iter()
            .map(|b| b.unwrap_or_else(|| (String::new(), Value::Uninit)))
            .collect();
        let mut ty_keys = FxHashMap::default();
        for t in written_tys(&mut data) {
            ty_keys.entry(*t).or_insert_with(|| it.tys.display(*t));
        }
        let mut vt_keys = FxHashMap::default();
        let vtables = &it.program.vtables;
        let tys = it.tys;
        for_each_value(&data, &bindings, &mut |v| {
            if let Value::Dyn { vtable, .. } | Value::DynView { vtable, .. } = v {
                if let Some(vt) = vtables.get(*vtable as usize) {
                    vt_keys
                        .entry(*vtable)
                        .or_insert_with(|| (vt.name.clone(), tys.display(vt.concrete)));
                }
            }
            Ok(())
        })
        .ok();
        Carried {
            data,
            bindings,
            ty_keys,
            vt_keys,
            frames,
        }
    });
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.io = Some(io);
        s.complete = complete;
        s.pending = pending;
    });
}

impl<'a> Interp<'a> {
    /// A call of [`REPL_TAKE_FN`]: the value the session holds for `name`.
    pub(super) fn repl_take_value(&mut self, name: &str) -> R<Value> {
        match self.repl_take.iter().position(|(n, _)| n == name) {
            Some(at) => Ok(self.repl_take.remove(at).1),
            None => err(format!("the session holds no value for `{name}`")),
        }
    }

    /// `v`, of type `ty`, as the session carries it, with its type as
    /// source, or `None` for one it cannot: a reference or slice, which
    /// points into the run's frames. A closure or function item is carried
    /// erased, as the `Fn` type its signature makes.
    pub(super) fn repl_carried(&mut self, v: Value, ty: Ty) -> R<Option<(Value, String)>> {
        if self.views_a_frame(&v) {
            return Ok(None);
        }
        match self.tys.kind(ty) {
            TyKind::Closure(def, _) => {
                let program: &'a Program = self.program;
                let Some(body) = program.bodies.values().find(|b| b.instance.def == def) else {
                    return Ok(None);
                };
                let Some(sig) = self.signature_source(body, 2) else {
                    return Ok(None);
                };
                let kind = match self.tys.kind(body.locals[1].ty) {
                    TyKind::Ref(_) => "Fn",
                    TyKind::MutRef(_) => "MutFn",
                    _ => "OnceFn",
                };
                let erased = self.erase(v, ty)?;
                Ok(Some((erased, format!("{kind}{sig}"))))
            }
            TyKind::FnDef(_) => {
                let Value::Fn(inst) = &v else {
                    return Ok(None);
                };
                let program: &'a Program = self.program;
                let Some(body) = program.bodies.get(&inst.name) else {
                    return Ok(None);
                };
                let Some(sig) = self.signature_source(body, 1) else {
                    return Ok(None);
                };
                let erased = self.erase(v, ty)?;
                Ok(Some((erased, format!("Fn{sig}"))))
            }
            // A closure held as a function value whose body takes its
            // parameters in other modes than the value's type says (a
            // borrowing `|v: Vec[i64]|` in an `Fn(own Vec[i64])`) is called
            // through what its own cell's code knew: replayed, not carried.
            TyKind::Fn { .. } => {
                let Some(t) = self.carry_ty(ty) else {
                    return Ok(None);
                };
                if let Value::Erased { body, env } = &v {
                    let program: &'a Program = self.program;
                    let first = if env.is_some() { 2 } else { 1 };
                    let sig = program
                        .bodies
                        .get(body)
                        .and_then(|b| self.signature_source(b, first));
                    let params = |s: &str| s.find('(').map(|i| s[i..].to_string());
                    if sig.as_deref().and_then(params) != params(&t) {
                        return Ok(None);
                    }
                }
                Ok(Some((v, t)))
            }
            _ => Ok(self.carry_ty(ty).map(|t| (v, t))),
        }
    }

    /// Whether `v`, or a collection or box it owns, holds a reference,
    /// slice or `dyn` view rooted in a frame's local: a closure that
    /// borrowed a local of its cell, say. The frame is gone once the run
    /// ends, so such a value is replayed rather than carried.
    fn views_a_frame(&self, v: &Value) -> bool {
        fn walk(heap: &[Option<HeapObj>], v: &Value, seen: &mut FxHashSet<u32>) -> bool {
            let local = |a: &Addr| matches!(a.root, Root::Local { .. });
            match v {
                Value::Ref(a) => local(a),
                Value::Slice { base, .. } => local(base),
                Value::DynView { at, .. } => local(at),
                Value::Agg(xs) | Value::Variant(_, xs) => xs.iter().any(|x| walk(heap, x, seen)),
                Value::Box(id)
                | Value::Boxed(id)
                | Value::Shared(id)
                | Value::Dyn { data: id, .. } => {
                    seen.insert(id.0)
                        && heap
                            .get(id.0 as usize)
                            .and_then(Option::as_ref)
                            .is_some_and(|o| walk(heap, &o.value, seen))
                }
                Value::Erased {
                    env: Some((id, _)), ..
                } => {
                    seen.insert(id.0)
                        && heap
                            .get(id.0 as usize)
                            .and_then(Option::as_ref)
                            .is_some_and(|o| walk(heap, &o.value, seen))
                }
                _ => false,
            }
        }
        walk(&self.heap, v, &mut FxHashSet::default())
    }

    /// `(A, B) -> R` for `body`'s parameters from local `first` on.
    fn signature_source(&self, body: &Body, first: usize) -> Option<String> {
        let last = body.arg_count;
        let params: Option<Vec<String>> = (first..=last)
            .map(|i| {
                let t = body.locals.get(i)?.ty;
                match self.tys.kind(t) {
                    TyKind::Ref(t) => self.carry_ty(t),
                    TyKind::MutRef(t) => Some(format!("mut ref {}", self.carry_ty(t)?)),
                    _ if self.tys.tcx().is_copy(t) => self.carry_ty(t),
                    _ => Some(format!("own {}", self.carry_ty(t)?)),
                }
            })
            .collect();
        let ret = self.carry_ty(body.locals[0].ty)?;
        Some(format!("({}) -> {ret}", params?.join(", ")))
    }

    /// `ty` as source, for any type a binding can be carried with.
    fn carry_ty(&self, ty: Ty) -> Option<String> {
        let list =
            |ts: &[Ty]| -> Option<Vec<String>> { ts.iter().map(|&t| self.carry_ty(t)).collect() };
        Some(match self.tys.kind(ty) {
            TyKind::Bool => "bool".into(),
            TyKind::Char => "char".into(),
            TyKind::Int(i) => i.name().into(),
            TyKind::Float(f) => f.name().into(),
            TyKind::Unit => "()".into(),
            TyKind::Str => "Str".into(),
            TyKind::Tuple(ts) if ts.len() == 1 => format!("({},)", list(&ts)?[0]),
            TyKind::Tuple(ts) => format!("({})", list(&ts)?.join(", ")),
            TyKind::Array(e, n) if !self.tys.is_vector(ty) => {
                format!("Array[{}, {n}]", self.carry_ty(e)?)
            }
            TyKind::Intrinsic(i) => match i {
                IntrinsicTy::String => "String".into(),
                IntrinsicTy::Vec(e) => format!("Vec[{}]", self.carry_ty(e)?),
                IntrinsicTy::VecDeque(e) => format!("VecDeque[{}]", self.carry_ty(e)?),
                IntrinsicTy::Set(e) => format!("Set[{}]", self.carry_ty(e)?),
                IntrinsicTy::SortedSet(e) => format!("SortedSet[{}]", self.carry_ty(e)?),
                IntrinsicTy::Map(k, v) => {
                    format!("Map[{}, {}]", self.carry_ty(k)?, self.carry_ty(v)?)
                }
                IntrinsicTy::SortedMap(k, v) => {
                    format!("SortedMap[{}, {}]", self.carry_ty(k)?, self.carry_ty(v)?)
                }
            },
            TyKind::Adt(_) | TyKind::Shared(_) => {
                let (adt, args) = self.tys.tcx().adt_of(ty)?;
                match args.is_empty() {
                    true => adt.name.clone(),
                    false => format!("{}[{}]", adt.name, list(&args)?.join(", ")),
                }
            }
            TyKind::Weak(s) => format!("weak {}", self.carry_ty(s)?),
            TyKind::Fn { params, ret, kind } => {
                let ps: Option<Vec<String>> = params
                    .iter()
                    .map(|&t| match self.tys.kind(t) {
                        TyKind::Ref(t) => self.carry_ty(t),
                        TyKind::MutRef(t) => Some(format!("mut ref {}", self.carry_ty(t)?)),
                        _ if self.tys.tcx().is_copy(t) => self.carry_ty(t),
                        _ => Some(format!("own {}", self.carry_ty(t)?)),
                    })
                    .collect();
                format!(
                    "{}({}) -> {}",
                    kind.name(),
                    ps?.join(", "),
                    self.carry_ty(ret)?
                )
            }
            _ => return None,
        })
    }
}

fn install_data(it: &mut Interp<'_>, d: Data) {
    let Data {
        heap,
        free,
        key_index,
        table_holes,
        unkeyed_tables,
        deque_front,
        snapshots,
        bytes_snapshots,
        forgotten,
        arenas,
        channels,
        chan_epoch,
        caps,
        sorted_tables,
        table_key_ty,
        tasks,
        waiting_tasks,
        log_ids,
        handles,
        groups,
        pools,
        semaphores,
        limiters,
        interners,
        log_sink,
        tracing_sink,
        log_min,
        tracing_min,
        next_frame_id,
        rand,
        walk_perm,
    } = d;
    it.heap = heap;
    it.free = free;
    it.key_index = key_index;
    it.table_holes = table_holes;
    it.unkeyed_tables = unkeyed_tables;
    it.deque_front = deque_front;
    it.snapshots = snapshots;
    it.bytes_snapshots = bytes_snapshots;
    it.forgotten = forgotten;
    it.arenas = arenas;
    it.channels = channels;
    it.chan_epoch = chan_epoch;
    it.caps = caps;
    it.sorted_tables = sorted_tables;
    it.table_key_ty = table_key_ty;
    it.tasks = tasks;
    it.waiting_tasks = waiting_tasks;
    it.log_ids = log_ids;
    it.handles = handles;
    it.groups = groups;
    it.pools = pools;
    it.semaphores = semaphores;
    it.limiters = limiters;
    it.interners = interners;
    it.log_sink = log_sink;
    it.tracing_sink = tracing_sink;
    it.log_min = log_min;
    it.tracing_min = tracing_min;
    it.next_frame_id = next_frame_id;
    it.rand = rand;
    it.walk_perm = walk_perm;
}

fn extract_data(it: &mut Interp<'_>) -> Data {
    use std::mem::take;
    Data {
        heap: take(&mut it.heap),
        free: take(&mut it.free),
        key_index: take(&mut it.key_index),
        table_holes: take(&mut it.table_holes),
        unkeyed_tables: take(&mut it.unkeyed_tables),
        deque_front: take(&mut it.deque_front),
        snapshots: take(&mut it.snapshots),
        bytes_snapshots: take(&mut it.bytes_snapshots),
        forgotten: take(&mut it.forgotten),
        arenas: it.arenas,
        channels: take(&mut it.channels),
        chan_epoch: it.chan_epoch,
        caps: take(&mut it.caps),
        sorted_tables: take(&mut it.sorted_tables),
        table_key_ty: take(&mut it.table_key_ty),
        tasks: take(&mut it.tasks),
        waiting_tasks: take(&mut it.waiting_tasks),
        log_ids: it.log_ids,
        handles: take(&mut it.handles),
        groups: take(&mut it.groups),
        pools: take(&mut it.pools),
        semaphores: take(&mut it.semaphores),
        limiters: take(&mut it.limiters),
        interners: take(&mut it.interners),
        log_sink: it.log_sink.take(),
        tracing_sink: it.tracing_sink.take(),
        log_min: it.log_min,
        tracing_min: it.tracing_min,
        next_frame_id: it.next_frame_id,
        rand: it.rand,
        walk_perm: it.walk_perm,
    }
}

fn install_io(it: &mut Interp<'_>, io: Io) {
    let Io {
        files,
        fs_extents,
        fake_files,
        net_extents,
        fake_sockets,
        bufs,
        lines_ended,
        stdin_lines_ended,
        buf_windows,
        procs,
        sockets,
        regexes,
        crypto,
        http_headers,
        http_bodies,
        http_error_responses,
        http_requests,
        http_progress,
        built_requests,
        free_requests,
        pinned_texts,
    } = io;
    it.files = files;
    it.fs_extents = fs_extents;
    it.fake_files = fake_files;
    it.net_extents = net_extents;
    it.fake_sockets = fake_sockets;
    it.bufs = bufs;
    it.lines_ended = lines_ended;
    it.stdin_lines_ended = stdin_lines_ended;
    it.buf_windows = buf_windows;
    it.procs = procs;
    it.sockets = sockets;
    it.regexes = regexes;
    it.crypto = crypto;
    it.http_headers = http_headers;
    it.http_bodies = http_bodies;
    it.http_error_responses = http_error_responses;
    it.http_requests = http_requests;
    it.http_progress = http_progress;
    it.built_requests = built_requests;
    it.free_requests = free_requests;
    it.pinned_texts = pinned_texts;
}

fn extract_io(it: &mut Interp<'_>) -> Io {
    use std::mem::take;
    Io {
        files: take(&mut it.files),
        fs_extents: take(&mut it.fs_extents),
        fake_files: take(&mut it.fake_files),
        net_extents: take(&mut it.net_extents),
        fake_sockets: take(&mut it.fake_sockets),
        bufs: take(&mut it.bufs),
        lines_ended: take(&mut it.lines_ended),
        stdin_lines_ended: it.stdin_lines_ended,
        buf_windows: take(&mut it.buf_windows),
        procs: take(&mut it.procs),
        sockets: take(&mut it.sockets),
        regexes: take(&mut it.regexes),
        crypto: take(&mut it.crypto),
        http_headers: take(&mut it.http_headers),
        http_bodies: take(&mut it.http_bodies),
        http_error_responses: take(&mut it.http_error_responses),
        http_requests: take(&mut it.http_requests),
        http_progress: take(&mut it.http_progress),
        built_requests: take(&mut it.built_requests),
        free_requests: take(&mut it.free_requests),
        pinned_texts: take(&mut it.pinned_texts),
    }
}

/// A task as the committed carry keeps it: an HTTP fetch it has in flight
/// is the run's, and a rolled-back session starts the task without it.
impl Clone for TaskSlot {
    fn clone(&self) -> Self {
        TaskSlot {
            frame: self.frame,
            frame_ty: self.frame_ty,
            resume: self.resume.clone(),
            group: self.group,
            handle: self.handle,
            deadline: self.deadline,
            slept: self.slept,
            pool_wait: self.pool_wait,
            pool_verdict: self.pool_verdict,
            http: None,
            spans: self.spans.clone(),
            cancelled: self.cancelled,
            torn: self.torn,
            unwind: self.unwind,
            panicked: self.panicked.clone(),
            log_id: self.log_id,
            log_held: self.log_held.clone(),
            log_hold: self.log_hold,
            log_up: self.log_up,
            log_seq: self.log_seq,
            log_done: self.log_done,
        }
    }
}
