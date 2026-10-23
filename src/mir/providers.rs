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
        fx.body.blocks[bb.index()].terminator.kind = TerminatorKind::Call {
            func: fn_def(self.tys, sig.def, sig.args.clone(), new),
            args: ops,
            destination,
            target: Some(next),
            unwind: UnwindAction::Abort,
        };
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
}
