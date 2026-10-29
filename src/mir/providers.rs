//! Provider specialisation (core semantics §14.3, §14.4; B-2026-10-20-9).
//!
//! The builder emits a call on a resource, `R.m(args)`, as a call of a
//! body-less `R.m`, and `with_provider[R](p, f)` as a call of a body-less
//! `with_provider[R]` ([`super::effects::ResourceDecl`]). Effect inference
//! then decides which resources each function reaches. This pass makes that
//! real. Each function that reaches a bound resource is copied once for each
//! provider type it runs under, takes a hidden `ref P` or `mut ref P`
//! parameter for it after its own, and passes it on to its callees. A call on
//! the resource calls the provider type's method with the hidden parameter as
//! its receiver. `with_provider` keeps `p` in a local, calls its closure's
//! copy for `P` with a borrow of it, then drops the closure and `p`.
//!
//! A closure that reaches a bound resource and is made into a function value
//! (`apply(|| tick())`, an `Erase` cast) is called through that value, where
//! no hidden argument can follow it, so it carries its providers instead: the
//! value holds a copy of the closure with a borrow of each provider after its
//! captures, and the copy of its body reads them from there (B-2026-10-23-9).
//! It cannot outlive the providers: `karac check` refuses a closure that
//! uses a bound resource escaping the `with_provider` (§14.5).
//!
//! A resource with a default provider (§14.7) that no `with_provider`
//! overrides on the way takes no parameter: a call on it builds the
//! (field-less) default where it is made, as the builder does for a program
//! that never binds the resource.

use std::collections::BTreeMap;

use super::effects::{Access, ResourceDecl};
use super::interp::Program;
use super::place_ty::place_ty;
use super::syntax::*;
use super::ty::{Ty, TyInterner};
use crate::ids::{DefId, NodeId};
use crate::ty::TyKind as SK;

/// What a resource is bound to where a function runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Binding {
    /// The resource's default provider, of this type, built where it is
    /// used.
    Default(Ty),
    /// A provider of this type, reached through a hidden parameter.
    Bound(Ty),
}

type Env = BTreeMap<String, Binding>;
type Needs = BTreeMap<String, Access>;

/// What the pass needs to know about a body before any is rewritten.
struct Sig {
    def: DefId,
    args: Vec<Ty>,
    /// The type of the first parameter (a closure's environment, a
    /// method's receiver).
    first: Option<Ty>,
    /// The closure type of a closure body's environment, if it is one.
    closure: Option<Ty>,
    /// The parameter types and the return type.
    params: Vec<Ty>,
    ret: Ty,
}

/// A copy of a closure's body that reads its providers from its
/// environment: the closure type that carries them, and per resource the
/// field holding its borrow and whether it is `mut`.
#[derive(Clone)]
struct Carried {
    ty: Ty,
    def: DefId,
    fields: Vec<(String, u32, bool)>,
}

/// The ambient native a default provider's method is (core semantics
/// §14.7, §14.10): `OsEnv.var` is `Env.var` and `OsStdout.write` is
/// `Stdout.print`, called without the receiver, which carries nothing.
/// `None` for a type that is no library default.
pub fn os_default(base: &str, method: &str) -> Option<(&'static str, String)> {
    let resource = match base {
        "OsEnv" => "Env",
        "OsClock" => "Clock",
        "OsRandom" => "RandomSource",
        "OsStdout" => "Stdout",
        "OsStderr" => "Stderr",
        "OsStdin" => "Stdin",
        "OsFileSystem" => "FileSystem",
        _ => return None,
    };
    let method = match (resource, method) {
        ("Stdout" | "Stderr", "write") => "print",
        _ => method,
    };
    Some((resource, method.to_string()))
}

/// The fused native that prints for `r.m` outside an override:
/// `Stdout.println` is `println`, which takes the format parts and builds
/// no `String` (core semantics §14.10). `None` for any other call.
pub fn fused_print(r: &str, m: &str) -> Option<&'static str> {
    Some(match (r, m) {
        ("Stdout", "print") => "print",
        ("Stdout", "println") => "println",
        ("Stderr", "print") => "eprint",
        ("Stderr", "println") => "eprintln",
        _ => return None,
    })
}

/// The `File` methods that, in a program binding `FileSystem`, are calls
/// on the resource (core semantics §14.11): `f.read(buf)` is
/// `FileSystem.file_read(f, buf)`, so the code that makes it takes the
/// provider as any call on `FileSystem` does, and a task or `par` branch
/// that reads a file is checked like one that calls the provider.
pub const FILE_OPS: [&str; 7] = [
    "read",
    "write",
    "flush",
    "sync_all",
    "sync_data",
    "seek",
    "set_len",
];

/// The calls on `FileSystem` that stand for a `File` native: the native,
/// which takes the same arguments in and out of an override. `open_file`
/// (path, flags) is `File.open_flags`; under an override it opens through
/// the provider instead (`Spec::open_via`).
pub fn file_native(r: &str, m: &str) -> Option<String> {
    if r != "FileSystem" {
        return None;
    }
    if m == "open_file" {
        return Some("File.open_flags".to_string());
    }
    let op = m.strip_prefix("file_")?;
    FILE_OPS.contains(&op).then(|| format!("File.{op}"))
}

/// The provider method a call `r.m` reaches: `m` itself, or for the calls
/// that stand for something else, the method that serves them (`print`
/// writes, `open_file` opens, `file_read` reads through `read_handle`).
pub fn via_method<'m>(r: &str, m: &'m str) -> &'m str {
    match (r, m) {
        (_, _) if fused_print(r, m).is_some() => "write",
        ("FileSystem", "open_file") => "open",
        ("FileSystem", "file_read") => "read_handle",
        ("FileSystem", "file_write") => "write_handle",
        _ => m,
    }
}

/// `OpenOptions`' bits for `File.open`, `File.create` and `File.append`
/// (read 1, write 2, append 4, truncate 8, create 16; io.kara).
fn open_flags(m: &str) -> Option<u128> {
    Some(match m {
        "open" => 1,
        "create" => 2 | 8 | 16,
        "append" => 4 | 16,
        _ => return None,
    })
}

/// In a program that binds `FileSystem`, its `File` natives become calls
/// on the resource (core semantics §14.11): an open is
/// `FileSystem.open_file(path, flags)`, and an operation on an open file
/// `FileSystem.file_<op>`. Runs before effect inference, which then sees
/// them as the resource's, so `specialise` passes the provider to the code
/// that makes them. A program with no binding of `FileSystem` is left as it
/// is.
pub fn route_files(program: &mut Program, tys: &TyInterner) {
    if !program.resources.contains_key("FileSystem") {
        return;
    }
    let i64t = tys.int(super::ty::IntTy::I64);
    for body in program.bodies.values_mut() {
        for bb in &mut body.blocks {
            let TerminatorKind::Call { func, args, .. } = &mut bb.terminator.kind else {
                continue;
            };
            let Operand::Const(Const {
                kind: ConstKind::FnDef(inst),
                ..
            }) = func
            else {
                continue;
            };
            let Some((base, m)) = inst.name.split_once('.') else {
                continue;
            };
            let new = match (base, m, args.as_mut_slice()) {
                ("File", _, [path]) if open_flags(m).is_some() => {
                    let flags = Operand::Const(Const {
                        ty: i64t,
                        kind: ConstKind::Scalar(open_flags(m).unwrap_or(0)),
                    });
                    *args = vec![path.clone(), flags];
                    "FileSystem.open_file".to_string()
                }
                // `OpenOptions { flags }`, behind the `ref self`.
                ("OpenOptions", "open", [Operand::Move(o) | Operand::Copy(o), path]) => {
                    let flags = Operand::Copy(o.project(ProjElem::Deref).field(0, i64t));
                    *args = vec![path.clone(), flags];
                    "FileSystem.open_file".to_string()
                }
                ("File", op, _) if FILE_OPS.contains(&op) => format!("FileSystem.file_{op}"),
                _ => continue,
            };
            *func = fn_def(tys, DefId(u32::MAX), Vec::new(), new);
        }
    }
}

/// Closure types made here take their ids from this one down, far from the
/// ids the resolver hands out.
const CARRIED_DEFS: u32 = u32::MAX - 1;

/// Rewrites `program`, built with resource placeholders, so that every
/// provider is passed explicitly. `needs` is effect inference's
/// [`super::effects::EffectReport::providers`] for the program as built.
pub fn specialise(
    program: &mut Program,
    tys: &TyInterner,
    needs: &BTreeMap<String, Needs>,
) -> Result<(), String> {
    if program.resources.is_empty() {
        return Ok(());
    }
    let mut sigs = BTreeMap::new();
    for (name, b) in &program.bodies {
        let first = b.args().next().map(|l| b.local(l).ty);
        let closure = first.and_then(|t| {
            let t = tys.pointee(t).unwrap_or(t);
            matches!(tys.tcx().kind(t), SK::Closure { .. }).then_some(t)
        });
        sigs.insert(
            name.clone(),
            Sig {
                def: b.instance.def,
                args: b.instance.args.clone(),
                first,
                closure,
                params: b.args().map(|l| b.local(l).ty).collect(),
                ret: b.locals[0].ty,
            },
        );
    }
    let resources = program.resources.clone();
    let mut s = Spec {
        tys,
        resources: &resources,
        needs,
        sigs: &sigs,
        queued: BTreeMap::new(),
        queue: Vec::new(),
        carried: BTreeMap::new(),
    };
    // A body that reaches only resources with a default runs against the
    // defaults under its own name; it is also the template of its copies.
    let templates: BTreeMap<String, Body> = program
        .bodies
        .iter()
        .filter(|(n, _)| needs.get(*n).is_some_and(|n| !n.is_empty()))
        .map(|(n, b)| (n.clone(), b.clone()))
        .collect();
    let mut removed = Vec::new();
    let names: Vec<String> = program.bodies.keys().cloned().collect();
    for name in names {
        let mut env = Env::new();
        let mut rooted = true;
        for r in needs.get(&name).into_iter().flat_map(|n| n.keys()) {
            match resources.get(r).and_then(|d| d.default) {
                Some(t) => {
                    env.insert(r.clone(), Binding::Default(t));
                }
                None => rooted = false,
            }
        }
        if !rooted {
            removed.push(name);
            continue;
        }
        s.queued.insert(name.clone(), ());
        let body = program.bodies.get_mut(&name).unwrap();
        s.rewrite(&name, body, &env)?;
    }
    while let Some((template, env, name)) = s.queue.pop() {
        let mut body = templates[&template].clone();
        body.instance.name = name.clone();
        s.rewrite(&template, &mut body, &env)?;
        program.bodies.insert(name, body);
    }
    for name in removed {
        if !s.queued.contains_key(&name) {
            program.bodies.remove(&name);
        }
    }
    Ok(())
}

struct Spec<'p> {
    tys: &'p TyInterner,
    resources: &'p BTreeMap<String, ResourceDecl>,
    needs: &'p BTreeMap<String, Needs>,
    sigs: &'p BTreeMap<String, Sig>,
    /// The name of every body made or kept.
    queued: BTreeMap<String, ()>,
    /// Copies still to make: template, environment, name.
    queue: Vec<(String, Env, String)>,
    /// The closure body copies that carry their providers, by name.
    carried: BTreeMap<String, Carried>,
}

/// One body being rewritten under one environment.
struct Fx<'b> {
    body: &'b mut Body,
    /// What each resource is bound to in the block being rewritten.
    env: Env,
    /// Where the provider of each bound resource is, in the block being
    /// rewritten: behind a hidden parameter, or in a `providers` block's
    /// local.
    src: BTreeMap<String, Place>,
}

impl Spec<'_> {
    /// The name of `template`'s copy under `env` (restricted to what it
    /// needs), queued if it is new.
    fn request(&mut self, template: &str, env: &Env) -> Result<String, String> {
        let (env, name) = self.copy_name(template, env)?;
        if name != template && self.queued.insert(name.clone(), ()).is_none() {
            self.queue.push((template.to_string(), env, name.clone()));
        }
        Ok(name)
    }

    /// `env` restricted to what `template` needs, and the name of
    /// `template`'s copy under it (its own name when nothing is bound).
    fn copy_name(&self, template: &str, env: &Env) -> Result<(Env, String), String> {
        let env: Env = match self.needs.get(template) {
            Some(n) => n
                .keys()
                .map(|r| {
                    env.get(r).map(|b| (r.clone(), *b)).ok_or_else(|| {
                        format!("providers: `{template}` needs `{r}` and nothing supplies it")
                    })
                })
                .collect::<Result<_, _>>()?,
            None => Env::new(),
        };
        let bound: Vec<String> = env
            .iter()
            .filter_map(|(r, b)| match b {
                Binding::Bound(t) => Some(format!("{r}={}", self.tys.display(*t))),
                Binding::Default(_) => None,
            })
            .collect();
        if bound.is_empty() {
            return Ok((env, template.to_string()));
        }
        let name = format!("{template}{{{}}}", bound.join(", "));
        Ok((env, name))
    }

    /// Rewrites `body`, a copy of `template`, under `env`.
    fn rewrite(&mut self, template: &str, body: &mut Body, env: &Env) -> Result<(), String> {
        let needs = self.needs.get(template).cloned().unwrap_or_default();
        let mut fx = Fx {
            body,
            env: env.clone(),
            src: BTreeMap::new(),
        };
        if let Some(c) = self.carried.get(&fx.body.instance.name).cloned() {
            // Its providers are in its environment, after the captures.
            fx.body.instance.def = c.def;
            let env_l = Local(1);
            let t = fx.body.local(env_l).ty;
            let (ty, by_ref) = match self.tys.tcx().kind(t) {
                SK::Ref(_) => (self.tys.tcx().reference(c.ty, false), true),
                SK::MutRef(_) => (self.tys.tcx().reference(c.ty, true), true),
                _ => (c.ty, false),
            };
            fx.body.locals[env_l.index()].ty = ty;
            let mut at = Place::local(env_l);
            if by_ref {
                at = at.project(ProjElem::Deref);
            }
            for (r, k, mutable) in &c.fields {
                let Some(Binding::Bound(pt)) = env.get(r) else {
                    continue;
                };
                let rt = self.tys.tcx().reference(*pt, *mutable);
                fx.src
                    .insert(r.clone(), at.field(*k, rt).project(ProjElem::Deref));
            }
        } else {
            // The hidden parameters, after the body's own.
            let mut params = Vec::new();
            for (r, b) in env {
                if let Binding::Bound(t) = b {
                    let mutable = needs.get(r) == Some(&Access::Writes);
                    params.push((r.clone(), self.tys.tcx().reference(*t, mutable)));
                }
            }
            if !params.is_empty() {
                fx.add_params(&params);
            }
        }
        let src = fx.src.clone();
        let provided = std::mem::take(&mut fx.body.provided);
        for i in 0..fx.body.blocks.len() {
            let bb = BasicBlock(i as u32);
            // A `providers` block binds its resources to its locals, the
            // innermost last (core semantics §14.8).
            fx.env = env.clone();
            fx.src = src.clone();
            for p in provided.iter().rev().filter(|p| p.blocks.contains(&bb)) {
                let t = fx.body.local(p.local).ty;
                fx.env.insert(p.resource.clone(), Binding::Bound(t));
                fx.src.insert(p.resource.clone(), Place::local(p.local));
            }
            self.carry_erased(&mut fx, bb)?;
            self.check_values(&fx, bb)?;
            let TerminatorKind::Call {
                func:
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                ..
            } = &fx.body.blocks[i].terminator.kind
            else {
                continue;
            };
            let callee = inst.name.clone();
            if callee == "FileSystem.enter" && self.resources.contains_key("FileSystem") {
                // A `providers` block's: its handle methods run with the
                // resources outside the block's own `FileSystem` binding.
                let mut outer = env.clone();
                let mut own = provided
                    .iter()
                    .rev()
                    .filter(|p| p.blocks.contains(&bb))
                    .collect::<Vec<_>>();
                if let Some(i) = own.iter().rposition(|p| p.resource == "FileSystem") {
                    own.remove(i);
                }
                for p in own {
                    let t = fx.body.local(p.local).ty;
                    outer.insert(p.resource.clone(), Binding::Bound(t));
                }
                self.region_extent(&mut fx, bb, &outer)?;
                continue;
            }
            if callee == "FileSystem.exit" {
                continue;
            }
            if let Some(r) = callee
                .strip_prefix("with_provider[")
                .and_then(|r| r.strip_suffix(']'))
                .filter(|r| self.resources.contains_key(*r))
            {
                self.with_provider(&mut fx, bb, r)?;
            } else if let Some((r, m)) = callee
                .split_once('.')
                .filter(|(r, _)| self.resources.contains_key(*r))
                .filter(|_| !self.sigs.contains_key(&callee))
            {
                self.resource_call(&mut fx, bb, r, m)?;
            } else if self.needs.get(&callee).is_some_and(|n| !n.is_empty()) {
                self.plain_call(&mut fx, bb, &callee)?;
            }
        }
        Ok(())
    }

    /// A call of `callee`, which reaches resources: its copy for this
    /// environment, with the hidden arguments it takes.
    fn plain_call(&mut self, fx: &mut Fx, bb: BasicBlock, callee: &str) -> Result<(), String> {
        let new = self.request(callee, &fx.env)?;
        let extra = self.hidden_args(fx, bb, callee, &BTreeMap::new())?;
        let sig = &self.sigs[callee];
        let (def, args) = (sig.def, sig.args.clone());
        let TerminatorKind::Call {
            func, args: ops, ..
        } = &mut fx.body.blocks[bb.index()].terminator.kind
        else {
            unreachable!()
        };
        *func = fn_def(self.tys, def, args, new);
        ops.extend(extra);
        Ok(())
    }

    /// The hidden arguments of a call of `callee` from `fx`: a reborrow of
    /// each of `fx`'s hidden parameters the callee takes, or of the local
    /// `local` holds for a resource this call supplies.
    fn hidden_args(
        &self,
        fx: &mut Fx,
        bb: BasicBlock,
        callee: &str,
        local: &BTreeMap<String, Local>,
    ) -> Result<Vec<Operand>, String> {
        let mut out = Vec::new();
        for (r, a) in self.needs.get(callee).cloned().unwrap_or_default() {
            let mutable = a == Access::Writes;
            if let Some(&l) = local.get(&r) {
                out.push(fx.borrow(bb, Place::local(l), mutable, self.tys));
                continue;
            }
            match fx.env.get(&r) {
                Some(Binding::Bound(_)) => {
                    let p = fx.src[&r].clone();
                    out.push(fx.borrow(bb, p, mutable, self.tys));
                }
                Some(Binding::Default(_)) => {}
                None => {
                    return Err(format!(
                        "providers: in `{}`, `{callee}` needs `{r}` and nothing supplies it",
                        fx.body.instance.name
                    ))
                }
            }
        }
        Ok(out)
    }

    /// `R.m(args)`: the bound provider's method, with the provider as its
    /// receiver.
    fn resource_call(
        &mut self,
        fx: &mut Fx,
        bb: BasicBlock,
        r: &str,
        m: &str,
    ) -> Result<(), String> {
        let decl = &self.resources[r];
        let binding = *fx.env.get(r).ok_or_else(|| {
            format!(
                "providers: in `{}`, a call on `{r}` with no provider",
                fx.body.instance.name
            )
        })?;
        if let Some(native) = file_native(r, m) {
            if let (Binding::Bound(_), "open_file") = (binding, m) {
                return self.open_via(fx, bb, r);
            }
            // Out of an override and in one alike, an operation on an open
            // `File` is the native, which finds the provider a file opened
            // under one belongs to (core semantics §14.11).
            let TerminatorKind::Call { func, .. } = &mut fx.body.blocks[bb.index()].terminator.kind
            else {
                unreachable!()
            };
            *func = fn_def(self.tys, DefId(u32::MAX), Vec::new(), native);
            return Ok(());
        }
        if let Some(native) = fused_print(r, m) {
            return match binding {
                Binding::Default(_) => {
                    let TerminatorKind::Call { func, .. } =
                        &mut fx.body.blocks[bb.index()].terminator.kind
                    else {
                        unreachable!()
                    };
                    *func = fn_def(self.tys, DefId(u32::MAX), Vec::new(), native.to_string());
                    Ok(())
                }
                Binding::Bound(_) => self.print_to(fx, bb, r, m == "println"),
            };
        }
        let (Binding::Bound(t) | Binding::Default(t)) = binding;
        let method = decl
            .impls
            .get(&t)
            .and_then(|ms| ms.get(m))
            .cloned()
            .ok_or_else(|| {
                format!(
                    "providers: `{}` has no method `{m}` for `{r}`",
                    self.tys.display(t)
                )
            })?;
        let mutable = match self.sigs.get(&method).and_then(|s| s.first) {
            Some(first) => match self.tys.tcx().kind(first) {
                SK::MutRef(_) => true,
                SK::Ref(_) => false,
                _ => {
                    return Err(format!(
                        "providers: `{method}` takes its provider by value, which a call \
                         through `{r}` cannot give it (core semantics §14.2)"
                    ))
                }
            },
            None => decl.methods.get(m).copied().unwrap_or(false),
        };
        let recv = match binding {
            Binding::Bound(_) => {
                let p = fx.src[r].clone();
                fx.borrow(bb, p, mutable, self.tys)
            }
            Binding::Default(t) => {
                let v = fx.temp(t);
                fx.push(
                    bb,
                    StatementKind::Assign(
                        Place::local(v),
                        Rvalue::Aggregate(
                            AggregateKind::Adt {
                                ty: t,
                                variant: VariantIdx(0),
                            },
                            Vec::new(),
                        ),
                    ),
                );
                fx.borrow(bb, Place::local(v), mutable, self.tys)
            }
        };
        let (name, def, args, extra) = if self.sigs.contains_key(&method) {
            let new = self.request(&method, &fx.env)?;
            let extra = self.hidden_args(fx, bb, &method, &BTreeMap::new())?;
            let sig = &self.sigs[&method];
            (new, sig.def, sig.args.clone(), extra)
        } else {
            (method, DefId(u32::MAX), Vec::new(), Vec::new())
        };
        let TerminatorKind::Call {
            func, args: ops, ..
        } = &mut fx.body.blocks[bb.index()].terminator.kind
        else {
            unreachable!()
        };
        *func = fn_def(self.tys, def, args, name);
        ops.insert(0, recv);
        ops.extend(extra);
        Ok(())
    }

    /// `print` / `println` on a stream bound to a provider (core semantics
    /// §14.10): the parts formatted into a `String` (with the newline for
    /// `println`), the provider's `write` called with a view of it, and
    /// the `String` dropped.
    fn print_to(&mut self, fx: &mut Fx, bb: BasicBlock, r: &str, line: bool) -> Result<(), String> {
        let TerminatorKind::Call {
            args: mut parts,
            destination,
            target,
            ..
        } = fx.body.blocks[bb.index()].terminator.kind.clone()
        else {
            unreachable!()
        };
        let here = fx.body.instance.name.clone();
        let after = target
            .ok_or_else(|| format!("providers: in `{here}`, a `{r}` print that does not return"))?;
        let info = fx.body.blocks[bb.index()].terminator.source_info;
        let tcx = self.tys.tcx();
        let (string, view) = (tcx.intern(SK::Str), tcx.intern(SK::StaticStr));
        if line {
            parts.push(Operand::Const(Const {
                ty: view,
                kind: ConstKind::Str("\n".to_string()),
            }));
        }
        let text = fx.temp(string);
        let viewed = fx.temp(view);
        // The write, its view of the text, then the text's drop.
        let drop_text = fx.block(
            TerminatorKind::Drop {
                place: Place::local(text),
                target: after,
                unwind: UnwindAction::Abort,
            },
            info,
        );
        let write = fx.block(
            TerminatorKind::Call {
                func: fn_def(self.tys, DefId(u32::MAX), Vec::new(), format!("{r}.write")),
                args: vec![Operand::Move(Place::local(viewed))],
                destination,
                target: Some(drop_text),
                unwind: UnwindAction::Abort,
            },
            info,
        );
        let as_view = fx.block(TerminatorKind::Goto { target: write }, info);
        let text_ref = fx.borrow(as_view, Place::local(text), false, self.tys);
        fx.body.blocks[as_view.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(
                self.tys,
                DefId(u32::MAX),
                Vec::new(),
                "String.as_str".to_string(),
            ),
            args: vec![text_ref],
            destination: Place::local(viewed),
            target: Some(write),
            unwind: UnwindAction::Abort,
        };
        fx.body.blocks[bb.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(self.tys, DefId(u32::MAX), Vec::new(), "format".to_string()),
            args: parts,
            destination: Place::local(text),
            target: Some(as_view),
            unwind: UnwindAction::Abort,
        };
        // The write is a call on the resource like any other.
        self.resource_call(fx, write, r, "write")
    }

    /// `FileSystem.open_file(path, flags)` under an override (core
    /// semantics §14.11): the provider's `open` with the options, then
    /// `File.adopt`, which makes the `File` of the id it returned, tied to
    /// this provider.
    fn open_via(&mut self, fx: &mut Fx, bb: BasicBlock, r: &str) -> Result<(), String> {
        let TerminatorKind::Call {
            args,
            destination,
            target,
            ..
        } = fx.body.blocks[bb.index()].terminator.kind.clone()
        else {
            unreachable!()
        };
        let here = fx.body.instance.name.clone();
        let [path, flags] = args.as_slice() else {
            return Err(format!(
                "providers: in `{here}`, an open without a path and flags"
            ));
        };
        let (Some(Binding::Bound(t)) | Some(Binding::Default(t))) = fx.env.get(r).copied() else {
            unreachable!()
        };
        let open = self.resources[r]
            .impls
            .get(&t)
            .and_then(|ms| ms.get("open"))
            .cloned()
            .ok_or_else(|| format!("providers: `{}` has no `open`", self.tys.display(t)))?;
        let sig = self
            .sigs
            .get(&open)
            .ok_or_else(|| format!("providers: no body for `{open}`"))?;
        let (opts_ref, res) = match sig.params.as_slice() {
            [_, _, o] => (*o, sig.ret),
            _ => {
                return Err(format!(
                    "providers: `{open}` is not `open(self, path, opts)`"
                ))
            }
        };
        let opts_ty = self
            .tys
            .pointee(opts_ref)
            .ok_or_else(|| format!("providers: `{open}` takes its options by value"))?;
        let info = fx.body.blocks[bb.index()].terminator.source_info;
        let after = target
            .ok_or_else(|| format!("providers: in `{here}`, an open that does not return"))?;
        let opts = fx.temp(opts_ty);
        fx.push(
            bb,
            StatementKind::Assign(
                Place::local(opts),
                Rvalue::Aggregate(
                    AggregateKind::Adt {
                        ty: opts_ty,
                        variant: VariantIdx(0),
                    },
                    vec![flags.clone()],
                ),
            ),
        );
        let opts_arg = fx.borrow(bb, Place::local(opts), false, self.tys);
        let id = fx.temp(res);
        let adopt = fx.block(TerminatorKind::Goto { target: after }, info);
        let provider = fx.borrow(adopt, fx.src[r].clone(), true, self.tys);
        fx.body.blocks[adopt.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(
                self.tys,
                DefId(u32::MAX),
                Vec::new(),
                "File.adopt".to_string(),
            ),
            args: vec![provider, Operand::Move(Place::local(id))],
            destination,
            target: Some(after),
            unwind: UnwindAction::Abort,
        };
        fx.body.blocks[bb.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(self.tys, DefId(u32::MAX), Vec::new(), format!("{r}.open")),
            args: vec![path.clone(), opts_arg],
            destination: Place::local(id),
            target: Some(adopt),
            unwind: UnwindAction::Abort,
        };
        // The open is a call on the resource like any other.
        self.resource_call(fx, bb, r, "open")
    }

    /// `with_provider[R](p, f)`: `p` kept in a local while `f`'s copy for
    /// `p`'s type runs with a borrow of it, then the closure and `p`
    /// dropped (core semantics §14.4).
    fn with_provider(&mut self, fx: &mut Fx, bb: BasicBlock, r: &str) -> Result<(), String> {
        let here = fx.body.instance.name.clone();
        let TerminatorKind::Call {
            args,
            destination,
            target,
            ..
        } = fx.body.blocks[bb.index()].terminator.kind.clone()
        else {
            unreachable!()
        };
        let [p, f] = args.as_slice() else {
            return Err(format!(
                "providers: in `{here}`, `with_provider[{r}]` takes a provider and a closure"
            ));
        };
        let pt = fx.operand_ty(p, self.tys)?;
        let ft = fx.operand_ty(f, self.tys)?;
        // The body `f` runs: a closure's, or a function item's.
        let (callee, env_arg) = match (self.tys.tcx().kind(ft), f) {
            (SK::Closure { .. }, Operand::Move(fp) | Operand::Copy(fp)) => {
                let callee = self
                    .sigs
                    .iter()
                    .find(|(_, s)| s.closure == Some(ft))
                    .map(|(n, _)| n.clone())
                    .ok_or_else(|| format!("providers: in `{here}`, no body for the closure"))?;
                (callee, Some(fp.clone()))
            }
            (
                _,
                Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }),
            ) if self.sigs.contains_key(&inst.name) => (inst.name.clone(), None),
            _ => {
                return Err(format!(
                    "providers: in `{here}`, a `with_provider[{r}]` whose second argument is \
                     not a closure or a function is not lowered yet"
                ))
            }
        };
        let pv = fx.temp(pt);
        fx.push(
            bb,
            StatementKind::Assign(Place::local(pv), Rvalue::Use(p.clone())),
        );
        let entered = fx.body.blocks[bb.index()].statements.len();
        let mut env = fx.env.clone();
        env.insert(r.to_string(), Binding::Bound(pt));
        let new = self.request(&callee, &env)?;
        let mut ops = Vec::new();
        // The closure's environment: by value, or a borrow of it that is
        // dropped after the call.
        let mut drop_f = None;
        if let Some(fp) = env_arg {
            let first = self.sigs[&callee].first;
            match first.map(|t| self.tys.tcx().kind(t)) {
                Some(SK::Ref(_)) => {
                    ops.push(fx.borrow(bb, fp.clone(), false, self.tys));
                    drop_f = Some(fp);
                }
                Some(SK::MutRef(_)) => {
                    ops.push(fx.borrow(bb, fp.clone(), true, self.tys));
                    drop_f = Some(fp);
                }
                _ => ops.push(Operand::Move(fp)),
            }
        }
        let local = BTreeMap::from([(r.to_string(), pv)]);
        let saved = std::mem::replace(&mut fx.env, env);
        let extra = self.hidden_args(fx, bb, &callee, &local);
        fx.env = saved;
        ops.extend(extra?);
        let info = fx.body.blocks[bb.index()].terminator.source_info;
        let after = target.ok_or_else(|| {
            format!("providers: in `{here}`, a `with_provider` call that does not return")
        })?;
        let drop_p = fx.block(
            TerminatorKind::Drop {
                place: Place::local(pv),
                target: after,
                unwind: UnwindAction::Abort,
            },
            info,
        );
        let next = match drop_f {
            Some(place) => fx.block(
                TerminatorKind::Drop {
                    place,
                    target: drop_p,
                    unwind: UnwindAction::Abort,
                },
                info,
            ),
            None => drop_p,
        };
        let sig = &self.sigs[&callee];
        let call = TerminatorKind::Call {
            func: fn_def(self.tys, sig.def, sig.args.clone(), new),
            args: ops,
            destination,
            target: Some(next),
            unwind: UnwindAction::Abort,
        };
        if r == "FileSystem" {
            return self.file_extent(fx, bb, entered, pv, pt, call);
        }
        fx.body.blocks[bb.index()].terminator.kind = call;
        Ok(())
    }

    /// The call `call` of a `with_provider[FileSystem]` closure, made at
    /// the end of `bb`, inside the provider's extent (core semantics
    /// §14.11): `FileSystem.enter` registers the provider `pv` and its
    /// methods for an open file's reads, writes and close before the
    /// statements from `entered` on, which borrow it for the call, and
    /// `FileSystem.exit` ends it after, so that a file of the provider
    /// used later fails with `ProviderUnavailable` and drops quietly.
    fn file_extent(
        &mut self,
        fx: &mut Fx,
        bb: BasicBlock,
        entered: usize,
        pv: Local,
        pt: Ty,
        call: TerminatorKind,
    ) -> Result<(), String> {
        let env = fx.env.clone();
        let handlers = self.file_handlers(fx, &env, pt, "the `with_provider[FileSystem]` call")?;
        let info = fx.body.blocks[bb.index()].terminator.source_info;
        let TerminatorKind::Call {
            target: Some(next), ..
        } = call
        else {
            unreachable!()
        };
        let i64t = self.tys.int(super::ty::IntTy::I64);
        let extent = fx.temp(i64t);
        let unit = fx.temp(self.tys.unit());
        let exit = fx.block(
            TerminatorKind::Call {
                func: fn_def(
                    self.tys,
                    DefId(u32::MAX),
                    Vec::new(),
                    "FileSystem.exit".to_string(),
                ),
                args: vec![Operand::Move(Place::local(extent))],
                destination: Place::local(unit),
                target: Some(next),
                unwind: UnwindAction::Abort,
            },
            info,
        );
        let TerminatorKind::Call {
            func,
            args,
            destination,
            unwind,
            ..
        } = call
        else {
            unreachable!()
        };
        let body = fx.block(
            TerminatorKind::Call {
                func,
                args,
                destination,
                target: Some(exit),
                unwind,
            },
            info,
        );
        let moved = fx.body.blocks[bb.index()].statements.split_off(entered);
        fx.body.blocks[body.index()].statements = moved;
        let mut args = vec![fx.borrow(bb, Place::local(pv), true, self.tys)];
        args.extend(handlers);
        fx.body.blocks[bb.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(
                self.tys,
                DefId(u32::MAX),
                Vec::new(),
                "FileSystem.enter".to_string(),
            ),
            args,
            destination: Place::local(extent),
            target: Some(body),
            unwind: UnwindAction::Abort,
        };
        Ok(())
    }

    /// The handle methods of the `FileSystem` provider type `pt` that a
    /// `FileSystem.enter` registers, specialised for `env`, the resources
    /// around `site`. The interpreter calls the first three; the compiled
    /// runtime, which cannot build an `IoError`, calls `close` and the
    /// last two.
    fn file_handlers(
        &mut self,
        fx: &Fx,
        env: &Env,
        pt: Ty,
        site: &str,
    ) -> Result<Vec<Operand>, String> {
        let here = fx.body.instance.name.clone();
        let mut handlers = Vec::new();
        for m in ["read_handle", "write_handle", "close", "__read", "__write"] {
            let method = self.resources["FileSystem"]
                .impls
                .get(&pt)
                .and_then(|ms| ms.get(m))
                .cloned()
                .ok_or_else(|| format!("providers: `{}` has no `{m}`", self.tys.display(pt)))?;
            // The library calls it with no hidden arguments, so it may use
            // only resources that are at their defaults here.
            for r in self.needs.get(&method).into_iter().flat_map(|n| n.keys()) {
                if let Some(Binding::Bound(_)) = env.get(r) {
                    return Err(format!(
                        "providers: in `{here}`, `{method}` uses `{r}`, which is overridden \
                         where {site} is; a file's `{m}` runs with every other resource at \
                         its default (core semantics §14.11)"
                    ));
                }
            }
            let name = self.request(&method, env)?;
            let sig = &self.sigs[&method];
            handlers.push(fn_def(self.tys, sig.def, sig.args.clone(), name));
        }
        Ok(handlers)
    }

    /// The `FileSystem.enter(&mut p)` a `providers { FileSystem => p }`
    /// block starts with (core semantics §14.11): `p`'s handle methods,
    /// specialised for `outer`, added after the provider.
    fn region_extent(&mut self, fx: &mut Fx, bb: BasicBlock, outer: &Env) -> Result<(), String> {
        let TerminatorKind::Call { args, .. } = &fx.body.blocks[bb.index()].terminator.kind else {
            unreachable!()
        };
        let [p] = args.as_slice() else {
            return Ok(());
        };
        let rt = fx.operand_ty(p, self.tys)?;
        let pt = match self.tys.tcx().kind(rt) {
            SK::MutRef(t) => t,
            _ => return Err("providers: a `FileSystem.enter` without its provider".to_string()),
        };
        let handlers = self.file_handlers(fx, outer, pt, "the `providers` block")?;
        let TerminatorKind::Call { args, .. } = &mut fx.body.blocks[bb.index()].terminator.kind
        else {
            unreachable!()
        };
        args.extend(handlers);
        Ok(())
    }

    /// The bound resources the body of the closure type `t` reaches, each
    /// with whether it writes, and that body's name; `None` for another
    /// type or a closure that reaches none.
    fn carrying(&self, fx: &Fx, t: Ty) -> Option<(String, Vec<(String, bool)>)> {
        let (template, _) = self.sigs.iter().find(|(_, s)| s.closure == Some(t))?;
        let bound: Vec<(String, bool)> = self
            .needs
            .get(template)?
            .iter()
            .filter(|(r, _)| matches!(fx.env.get(*r), Some(Binding::Bound(_))))
            .map(|(r, a)| (r.clone(), *a == Access::Writes))
            .collect();
        (!bound.is_empty()).then(|| (template.clone(), bound))
    }

    /// The closure type, and the name of its body's copy, that carries
    /// `bound`'s providers for `template` (a closure body whose
    /// environment is `from`) under `env`; queued if new.
    fn carry_request(
        &mut self,
        template: &str,
        from: Ty,
        env: &Env,
        bound: &[(String, bool)],
    ) -> Result<(Ty, String), String> {
        let (env, copy) = self.copy_name(template, env)?;
        let name = format!("{copy}.carried");
        if let Some(c) = self.carried.get(&name) {
            return Ok((c.ty, name));
        }
        let SK::Closure { captures, .. } = self.tys.tcx().kind(from) else {
            return Err(format!("providers: `{template}` is not a closure body"));
        };
        // A body that borrows its environment shared cannot write through
        // a borrow kept there.
        let shared_env = self.sigs[template]
            .first
            .is_some_and(|t| matches!(self.tys.tcx().kind(t), SK::Ref(_)));
        if let Some((r, _)) = bound.iter().find(|(_, m)| *m).filter(|_| shared_env) {
            return Err(format!(
                "a closure that writes `{r}`'s provider, made into a `Fn` value, is not \
                 lowered yet (`{template}`)"
            ));
        }
        let mut caps = self.tys.tcx().list(captures);
        let mut fields = Vec::new();
        for (r, mutable) in bound {
            let Some(Binding::Bound(pt)) = env.get(r).copied() else {
                continue;
            };
            fields.push((r.clone(), caps.len() as u32, *mutable));
            caps.push(self.tys.tcx().reference(pt, *mutable));
        }
        let def = DefId(CARRIED_DEFS - self.carried.len() as u32);
        let ty = self.tys.intern(super::ty::TyKind::Closure(def, caps));
        self.carried
            .insert(name.clone(), Carried { ty, def, fields });
        self.queued.insert(name.clone(), ());
        self.queue.push((template.to_string(), env, name.clone()));
        Ok((ty, name))
    }

    /// Each closure of `bb` made into a function value that reaches a bound
    /// resource: made instead as its copy carrying the providers, a
    /// borrow of each after its captures.
    fn carry_erased(&mut self, fx: &mut Fx, bb: BasicBlock) -> Result<(), String> {
        let stmts = std::mem::take(&mut fx.body.blocks[bb.index()].statements);
        let mut out = Vec::with_capacity(stmts.len());
        for st in stmts {
            let StatementKind::Assign(dst, Rvalue::Cast(CastKind::Erase, op, to)) = &st.kind else {
                out.push(st);
                continue;
            };
            let (Operand::Move(src) | Operand::Copy(src)) = op else {
                out.push(st);
                continue;
            };
            let from = place_ty(fx.body, self.tys, src)?.ty;
            let Some((template, bound)) = self.carrying(fx, from) else {
                out.push(st);
                continue;
            };
            let env = fx.env.clone();
            let (ty, _) = self.carry_request(&template, from, &env, &bound)?;
            let SK::Closure { captures, .. } = self.tys.tcx().kind(from) else {
                unreachable!()
            };
            let copy = matches!(op, Operand::Copy(_));
            let mut ops: Vec<Operand> = self
                .tys
                .tcx()
                .list(captures)
                .into_iter()
                .enumerate()
                .map(|(i, t)| {
                    let p = src.field(i as u32, t);
                    if copy {
                        Operand::Copy(p)
                    } else {
                        Operand::Move(p)
                    }
                })
                .collect();
            let info = st.source_info;
            for (r, mutable) in &bound {
                let p = fx.src[r].clone();
                let pt = place_ty(fx.body, self.tys, &p)?.ty;
                let l = fx.temp(self.tys.tcx().reference(pt, *mutable));
                let kind = if *mutable {
                    BorrowKind::Mut
                } else {
                    BorrowKind::Shared
                };
                out.push(Statement {
                    kind: StatementKind::Assign(Place::local(l), Rvalue::Ref(kind, p)),
                    source_info: info,
                });
                ops.push(Operand::Move(Place::local(l)));
            }
            let v = fx.temp(ty);
            out.push(Statement {
                kind: StatementKind::Assign(
                    Place::local(v),
                    Rvalue::Aggregate(AggregateKind::Closure { ty }, ops),
                ),
                source_info: info,
            });
            out.push(Statement {
                kind: StatementKind::Assign(
                    dst.clone(),
                    Rvalue::Cast(CastKind::Erase, Operand::Move(Place::local(v)), *to),
                ),
                source_info: info,
            });
        }
        fx.body.blocks[bb.index()].statements = out;
        Ok(())
    }

    /// A function item made as a value that reaches a bound resource would
    /// have to carry its provider, which only a closure does so far
    /// ([`Self::carry_erased`]). A closure made as a value is called
    /// directly, or carries its providers once erased; one that would
    /// escape the providers is refused by `karac check` (§14.5).
    fn check_values(&self, fx: &Fx, bb: BasicBlock) -> Result<(), String> {
        let bound = |name: &str| {
            self.needs.get(name).and_then(|n| {
                n.keys()
                    .find(|r| !matches!(fx.env.get(*r), Some(Binding::Default(_))))
                    .cloned()
            })
        };
        let err = |r: String| {
            Err(format!(
                "in `{}`: a function used as a value that uses `{r}`'s provider is not \
                 lowered yet; call it from a closure (`|| f()`) instead",
                fx.body.instance.name
            ))
        };
        for st in &fx.body.blocks[bb.index()].statements {
            let StatementKind::Assign(_, rv) = &st.kind else {
                continue;
            };
            match rv {
                Rvalue::Use(Operand::Const(Const {
                    kind: ConstKind::FnDef(inst),
                    ..
                }))
                | Rvalue::Cast(
                    _,
                    Operand::Const(Const {
                        kind: ConstKind::FnDef(inst),
                        ..
                    }),
                    _,
                ) => {
                    if let Some(r) = bound(&inst.name) {
                        return err(r);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

fn fn_def(tys: &TyInterner, def: DefId, args: Vec<Ty>, name: String) -> Operand {
    Operand::Const(Const {
        ty: tys.unit(),
        kind: ConstKind::FnDef(InstanceId { def, args, name }),
    })
}

impl Fx<'_> {
    /// Adds `params` after the body's own parameters, moving its other
    /// locals up.
    fn add_params(&mut self, params: &[(String, Ty)]) {
        let n = self.body.arg_count as u32;
        let k = params.len() as u32;
        let shift = |l: Local| if l.0 > n { Local(l.0 + k) } else { l };
        renumber(self.body, &shift);
        let info = self.body.locals[0].source_info;
        for (i, (r, t)) in params.iter().enumerate() {
            let l = Local(n + 1 + i as u32);
            self.body.locals.insert(
                l.index(),
                LocalDecl {
                    ty: *t,
                    mutability: Mutability::Not,
                    kind: LocalKind::Arg {
                        name: format!("provider of {r}"),
                        node: NodeId::DUMMY,
                    },
                    source_info: info,
                },
            );
            self.src
                .insert(r.clone(), Place::local(l).project(ProjElem::Deref));
        }
        self.body.arg_count += params.len();
    }

    fn temp(&mut self, ty: Ty) -> Local {
        let info = self.body.locals[0].source_info;
        self.body.locals.push(LocalDecl {
            ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Temp,
            source_info: info,
        });
        Local(self.body.locals.len() as u32 - 1)
    }

    fn push(&mut self, bb: BasicBlock, kind: StatementKind) {
        let source_info = self.body.blocks[bb.index()].terminator.source_info;
        self.body.blocks[bb.index()]
            .statements
            .push(Statement { kind, source_info });
    }

    fn block(&mut self, kind: TerminatorKind, source_info: SourceInfo) -> BasicBlock {
        self.body.blocks.push(BasicBlockData {
            statements: Vec::new(),
            terminator: Terminator { kind, source_info },
        });
        BasicBlock(self.body.blocks.len() as u32 - 1)
    }

    /// A borrow of `p` in a new temporary, made at the end of `bb`.
    fn borrow(&mut self, bb: BasicBlock, p: Place, mutable: bool, tys: &TyInterner) -> Operand {
        let t = place_ty(self.body, tys, &p)
            .map(|t| t.ty)
            .unwrap_or_else(|_| tys.unit());
        let r = self.temp(tys.tcx().reference(t, mutable));
        let kind = if mutable {
            BorrowKind::Mut
        } else {
            BorrowKind::Shared
        };
        self.push(
            bb,
            StatementKind::Assign(Place::local(r), Rvalue::Ref(kind, p)),
        );
        Operand::Move(Place::local(r))
    }

    fn operand_ty(&self, o: &Operand, tys: &TyInterner) -> Result<Ty, String> {
        match o {
            Operand::Copy(p) | Operand::Move(p) => place_ty(self.body, tys, p).map(|t| t.ty),
            Operand::Const(c) => Ok(c.ty),
        }
    }
}

/// Renames every local of `body` by `f`.
fn renumber(body: &mut Body, f: &impl Fn(Local) -> Local) {
    let place = |p: &mut Place| {
        p.local = f(p.local);
        for e in &mut p.projection {
            if let ProjElem::Index(i) = e {
                *i = f(*i);
            }
        }
    };
    let operand = |o: &mut Operand| {
        if let Operand::Copy(p) | Operand::Move(p) = o {
            place(p);
        }
    };
    for bb in &mut body.blocks {
        for st in &mut bb.statements {
            match &mut st.kind {
                StatementKind::Assign(p, rv) => {
                    place(p);
                    match rv {
                        Rvalue::Use(o) | Rvalue::UnaryOp(_, o) | Rvalue::Cast(_, o, _) => {
                            operand(o)
                        }
                        Rvalue::BinaryOp(_, a, b) | Rvalue::CheckedBinaryOp(_, a, b) => {
                            operand(a);
                            operand(b);
                        }
                        Rvalue::Aggregate(_, ops) => ops.iter_mut().for_each(operand),
                        Rvalue::Ref(_, p)
                        | Rvalue::Retain(p)
                        | Rvalue::Discriminant(p)
                        | Rvalue::Len(p) => place(p),
                        Rvalue::NullaryOp(..) => {}
                    }
                }
                StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => *l = f(*l),
                StatementKind::SetDiscriminant(p, _) | StatementKind::FreeBox(p) => place(p),
                StatementKind::BorrowFlag(op) => match op {
                    FlagOp::Acquire { place: p, .. } | FlagOp::Check { place: p, .. } => place(p),
                    FlagOp::Release { .. } => {}
                },
                StatementKind::Nop => {}
            }
        }
        match &mut bb.terminator.kind {
            TerminatorKind::SwitchInt { discr, .. } => operand(discr),
            TerminatorKind::Call {
                func,
                args,
                destination,
                ..
            } => {
                operand(func);
                args.iter_mut().for_each(operand);
                place(destination);
            }
            TerminatorKind::Drop { place: p, .. } => place(p),
            TerminatorKind::Goto { .. }
            | TerminatorKind::Return
            | TerminatorKind::Abort { .. }
            | TerminatorKind::Unreachable => {}
        }
    }
    for (p, l) in &mut body.drop_flags {
        place(p);
        *l = f(*l);
    }
    for p in &mut body.provided {
        p.local = f(p.local);
    }
    for (l, _) in &mut body.fn_value_effects {
        *l = f(*l);
    }
}
