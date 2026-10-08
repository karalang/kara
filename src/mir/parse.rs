//! Reads the textual form (`docs/spikes/mir-types.md` §7) back into MIR.
//!
//! A module is a list of type declarations followed by function bodies.
//! Bodies are exactly what [`pretty_body`](super::pretty_body) prints; the
//! declarations are this file's addition, since a body alone names its
//! types but does not define them:
//!
//! ```text
//! struct R: Drop { id: i64 }
//! struct Pair: Copy(i64, i64)
//! enum E { Two(R, R), One(R), Named { a: i64 }, Empty }
//! ```
//!
//! `: Drop` marks a type with a user `Drop` body, `: Copy` a derived `Copy`
//! type. A tuple-like struct or variant names its fields `0`, `1`, ....
//! Declarations may refer to each other in any order. Parsing then printing
//! a module with [`pretty_module`] gives back the same text, modulo
//! comments and blank lines.
//!
//! Every type and function gets a [`DefId`] in order of appearance: types
//! first, then bodies, then any callee no body defines (an intrinsic such
//! as `println`), the first time it is named.

use std::collections::HashMap;

use crate::ids::{DefId, NodeId};

use super::place_ty::place_ty;
use super::pretty::pretty_body;
use super::syntax::*;
use super::ty::{
    AdtDef, AdtId, FloatTy, FnKind, IntTy, IntrinsicTy, Ty, TyInterner, TyKind, VariantDef,
};

/// A parsed module: its types and its bodies, in source order.
#[derive(Debug)]
pub struct MirModule {
    pub tys: TyInterner,
    /// The declared ADTs, in declaration order.
    pub adts: Vec<AdtId>,
    pub bodies: Vec<Body>,
    /// The name of each [`DefId`], indexed by its number.
    pub def_names: Vec<String>,
}

impl MirModule {
    pub fn body(&self, name: &str) -> Option<&Body> {
        self.bodies.iter().find(|b| b.instance.name == name)
    }

    pub fn adt_named(&self, name: &str) -> Option<AdtId> {
        self.adts
            .iter()
            .copied()
            .find(|&a| self.tys.adt(a).name == name)
    }
}

/// Parses a module; an error names the line it was found on.
pub fn parse_module(src: &str) -> Result<MirModule, String> {
    let lines: Vec<Line> = src
        .lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let (code, comment) = split_comment(raw);
            let code = code.trim();
            (!code.is_empty()).then(|| Line {
                no: i + 1,
                code: code.to_string(),
                comment: comment.map(|c| c.trim().to_string()),
            })
        })
        .collect();
    let mut p = Parser::default();
    p.declare_names(&lines)?;
    let mut i = 0;
    while i < lines.len() {
        let line = &lines[i];
        if line.code.starts_with("fn ") {
            i = p.body(&lines, i)?;
        } else if line.code.starts_with("struct ") || line.code.starts_with("enum ") {
            // A declaration runs until its braces balance.
            let mut text = String::new();
            let start = line.no;
            loop {
                let l = lines
                    .get(i)
                    .ok_or_else(|| format!("line {start}: unterminated declaration"))?;
                text.push_str(&l.code);
                text.push(' ');
                i += 1;
                if brace_depth(&text) == 0 {
                    break;
                }
            }
            p.adt_decl(&text)
                .map_err(|e| format!("line {start}: {e}"))?;
        } else {
            return Err(format!(
                "line {}: expected `fn`, `struct` or `enum`, found `{}`",
                line.no, line.code
            ));
        }
    }
    if p.adts.len() != p.adt_decls.len() {
        // `declare_names` found a declaration the main pass never parsed.
        return Err("internal: declaration count mismatch".into());
    }
    Ok(MirModule {
        tys: p.tys,
        adts: p.adts,
        bodies: p.bodies,
        def_names: p.def_names,
    })
}

/// Prints every declaration, then every body, in module order.
pub fn pretty_module(m: &MirModule) -> String {
    let mut out = String::new();
    for &a in &m.adts {
        out.push_str(&pretty_adt(&m.tys, a));
        out.push('\n');
    }
    for b in &m.bodies {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&pretty_body(b, &m.tys));
    }
    out
}

/// The declaration line of one ADT.
pub fn pretty_adt(tys: &TyInterner, id: AdtId) -> String {
    let adt = tys.adt(id);
    let mut traits = Vec::new();
    if adt.is_copy {
        traits.push("Copy");
    }
    if adt.has_drop_impl {
        traits.push("Drop");
    }
    let bounds = if traits.is_empty() {
        String::new()
    } else {
        format!(": {}", traits.join(", "))
    };
    if adt.is_enum {
        let vs: Vec<String> = adt
            .variants
            .iter()
            .map(|v| format!("{}{}", v.name, fields_text(tys, &v.fields, true)))
            .collect();
        format!("enum {}{bounds} {{ {} }}", adt.name, vs.join(", "))
    } else {
        let fields = adt.variants.first().map(|v| &v.fields[..]).unwrap_or(&[]);
        let f = fields_text(tys, fields, false);
        let f = if f.is_empty() { " {}".to_string() } else { f };
        format!("struct {}{bounds}{f}", adt.name)
    }
}

fn fields_text(tys: &TyInterner, fields: &[(String, Ty)], unit_ok: bool) -> String {
    if fields.is_empty() {
        return if unit_ok { String::new() } else { " {}".into() };
    }
    let tuple_like = fields
        .iter()
        .enumerate()
        .all(|(i, (n, _))| *n == i.to_string());
    if tuple_like {
        let ts: Vec<String> = fields.iter().map(|(_, t)| tys.display(*t)).collect();
        format!("({})", ts.join(", "))
    } else {
        let fs: Vec<String> = fields
            .iter()
            .map(|(n, t)| format!("{n}: {}", tys.display(*t)))
            .collect();
        format!(" {{ {} }}", fs.join(", "))
    }
}

struct Line {
    no: usize,
    code: String,
    comment: Option<String>,
}

/// Splits `// ...` off a line, ignoring `//` inside string and char literals.
fn split_comment(raw: &str) -> (&str, Option<&str>) {
    let b = raw.as_bytes();
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < b.len() {
        match quote {
            Some(q) => {
                if b[i] == b'\\' {
                    i += 1;
                } else if b[i] == q {
                    quote = None;
                }
            }
            None => {
                // A `'` opens a char literal only where one can start, so
                // `'` never confuses the scan (MIR has no lifetimes).
                if b[i] == b'"' || b[i] == b'\'' {
                    quote = Some(b[i]);
                } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
                    return (&raw[..i], Some(&raw[i + 2..]));
                }
            }
        }
        i += 1;
    }
    (raw, None)
}

fn brace_depth(s: &str) -> i64 {
    s.chars().fold(0, |d, c| match c {
        '{' => d + 1,
        '}' => d - 1,
        _ => d,
    })
}

#[derive(Default)]
struct Parser {
    tys: TyInterner,
    adts: Vec<AdtId>,
    /// Name -> the id the ADT will get, known before any is parsed.
    adt_decls: HashMap<String, AdtId>,
    fns: HashMap<String, DefId>,
    def_names: Vec<String>,
    bodies: Vec<Body>,
}

impl Parser {
    fn new_def(&mut self, name: &str) -> DefId {
        let d = DefId(self.def_names.len() as u32);
        self.def_names.push(name.to_string());
        d
    }

    /// Assigns every declared ADT its [`AdtId`] and every body its
    /// [`DefId`], so declarations and calls can refer forward.
    fn declare_names(&mut self, lines: &[Line]) -> Result<(), String> {
        let mut adt_names = Vec::new();
        let mut fn_names = Vec::new();
        for l in lines {
            for kw in ["struct ", "enum "] {
                if let Some(rest) = l.code.strip_prefix(kw) {
                    let mut c = Cur::new(rest);
                    let name = c.type_name().map_err(|e| format!("line {}: {e}", l.no))?;
                    adt_names.push((l.no, name));
                }
            }
            if let Some(rest) = l.code.strip_prefix("fn ") {
                let mut c = Cur::new(rest);
                let name = c.fn_name().map_err(|e| format!("line {}: {e}", l.no))?;
                fn_names.push((l.no, name));
            }
        }
        for (i, (no, name)) in adt_names.iter().enumerate() {
            if self
                .adt_decls
                .insert(name.clone(), AdtId(i as u32))
                .is_some()
            {
                return Err(format!("line {no}: `{name}` is declared twice"));
            }
            self.new_def(name);
        }
        for (no, name) in fn_names {
            if self.fns.contains_key(&name) {
                return Err(format!("line {no}: `fn {name}` is defined twice"));
            }
            let d = self.new_def(&name);
            self.fns.insert(name, d);
        }
        Ok(())
    }

    fn adt_decl(&mut self, text: &str) -> Result<(), String> {
        let mut c = Cur::new(text);
        let is_enum = if c.eat_kw("struct") {
            false
        } else {
            c.expect_kw("enum")?;
            true
        };
        let name = c.type_name()?;
        let id = self.adt_decls[&name];
        if id.0 as usize != self.adts.len() {
            return Err("internal: declarations parsed out of order".into());
        }
        let (mut is_copy, mut has_drop_impl) = (false, false);
        if c.eat(":") {
            loop {
                match c.ident()? {
                    "Copy" => is_copy = true,
                    "Drop" => has_drop_impl = true,
                    other => return Err(format!("unknown bound `{other}`")),
                }
                if !c.eat(",") {
                    break;
                }
            }
        }
        let variants = if is_enum {
            c.expect("{")?;
            let mut vs = Vec::new();
            while !c.eat("}") {
                let vname = c.ident()?.to_string();
                let fields = self.fields(&mut c, true)?;
                vs.push(VariantDef {
                    name: vname,
                    fields,
                });
                if !c.eat(",") {
                    c.expect("}")?;
                    break;
                }
            }
            vs
        } else {
            let fields = self.fields(&mut c, false)?;
            vec![VariantDef {
                name: name.clone(),
                fields,
            }]
        };
        c.done()?;
        let def = DefId(id.0);
        let got = self.tys.add_adt(AdtDef {
            def,
            name,
            is_enum,
            variants,
            has_drop_impl,
            is_copy,
        });
        debug_assert_eq!(got, id);
        self.adts.push(got);
        Ok(())
    }

    /// `(T, U)`, `{ a: T, b: U }`, or nothing (only where `unit_ok`).
    fn fields(&mut self, c: &mut Cur, unit_ok: bool) -> Result<Vec<(String, Ty)>, String> {
        let mut out = Vec::new();
        if c.eat("(") {
            while !c.eat(")") {
                let t = self.ty(c)?;
                out.push((out.len().to_string(), t));
                if !c.eat(",") {
                    c.expect(")")?;
                    break;
                }
            }
        } else if c.eat("{") {
            while !c.eat("}") {
                let n = c.ident()?.to_string();
                c.expect(":")?;
                let t = self.ty(c)?;
                out.push((n, t));
                if !c.eat(",") {
                    c.expect("}")?;
                    break;
                }
            }
        } else if !unit_ok {
            return Err(format!("expected `(` or `{{` at `{}`", c.rest()));
        }
        Ok(out)
    }

    fn adt(&self, name: &str) -> Result<AdtId, String> {
        self.adt_decls
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown type `{name}`"))
    }

    fn ty(&mut self, c: &mut Cur) -> Result<Ty, String> {
        if c.eat("(") {
            let mut ts = Vec::new();
            while !c.eat(")") {
                ts.push(self.ty(c)?);
                if !c.eat(",") {
                    c.expect(")")?;
                    break;
                }
            }
            return Ok(if ts.is_empty() {
                self.tys.unit()
            } else {
                self.tys.intern(TyKind::Tuple(ts))
            });
        }
        if c.eat("!") {
            return Ok(self.tys.intern(TyKind::Never));
        }
        if c.eat_kw("ref") {
            let t = self.ty(c)?;
            return Ok(self.tys.intern(TyKind::Ref(t)));
        }
        if c.eat_kw("mut") {
            c.expect_kw("ref")?;
            let t = self.ty(c)?;
            return Ok(self.tys.intern(TyKind::MutRef(t)));
        }
        if c.eat_kw("shared") {
            let a = self.adt(&c.type_name()?)?;
            return Ok(self.tys.intern(TyKind::Shared(a)));
        }
        if c.eat_kw("weak") {
            let a = self.adt(&c.type_name()?)?;
            let s = self.tys.intern(TyKind::Shared(a));
            return Ok(self.tys.intern(TyKind::Weak(s)));
        }
        let name = c.ident()?;
        let fn_kind = match name {
            "Fn" => Some(FnKind::Fn),
            "MutFn" => Some(FnKind::MutFn),
            "OnceFn" => Some(FnKind::OnceFn),
            _ => None,
        };
        if let Some(kind) = fn_kind {
            c.expect("(")?;
            let mut params = Vec::new();
            while !c.eat(")") {
                params.push(self.ty(c)?);
                if !c.eat(",") {
                    c.expect(")")?;
                    break;
                }
            }
            c.expect("->")?;
            let ret = self.ty(c)?;
            return Ok(self.tys.intern(TyKind::Fn { params, ret, kind }));
        }
        if c.eat("#") {
            let n = c.number()? as u32;
            return match name {
                "fn" => Ok(self.tys.intern(TyKind::FnDef(DefId(n)))),
                "closure" => self.closure_ty(c, n),
                _ => Err(format!("unknown type `{name}#{n}`")),
            };
        }
        let kind = match name {
            "bool" => TyKind::Bool,
            "char" => TyKind::Char,
            "str" => TyKind::Str,
            "f32" => TyKind::Float(FloatTy::F32),
            "f64" => TyKind::Float(FloatTy::F64),
            "String" => TyKind::Intrinsic(IntrinsicTy::String),
            "Array" | "Slice" | "Vec" | "Set" | "Map" | "VecDeque" | "SortedSet" | "SortedMap" => {
                c.expect("[")?;
                let a = self.ty(c)?;
                let kind = match name {
                    "Array" => {
                        c.expect(",")?;
                        TyKind::Array(a, c.number()?)
                    }
                    "Slice" => TyKind::Slice(a),
                    "Vec" => TyKind::Intrinsic(IntrinsicTy::Vec(a)),
                    "Set" => TyKind::Intrinsic(IntrinsicTy::Set(a)),
                    "VecDeque" => TyKind::Intrinsic(IntrinsicTy::VecDeque(a)),
                    "SortedSet" => TyKind::Intrinsic(IntrinsicTy::SortedSet(a)),
                    _ => {
                        c.expect(",")?;
                        let b = self.ty(c)?;
                        if name == "SortedMap" {
                            TyKind::Intrinsic(IntrinsicTy::SortedMap(a, b))
                        } else {
                            TyKind::Intrinsic(IntrinsicTy::Map(a, b))
                        }
                    }
                };
                c.expect("]")?;
                kind
            }
            other => match int_ty(other) {
                Some(i) => TyKind::Int(i),
                None => {
                    let full = format!("{other}{}", c.bracket_suffix()?);
                    TyKind::Adt(self.adt(&full)?)
                }
            },
        };
        Ok(self.tys.intern(kind))
    }

    /// Parses the body whose header is `lines[start]`; returns the index of
    /// the line after its closing brace.
    fn body(&mut self, lines: &[Line], start: usize) -> Result<usize, String> {
        let at = |l: &Line, e: String| format!("line {}: {e}", l.no);
        let head = &lines[start];
        let mut c = Cur::new(&head.code);
        c.expect_kw("fn").map_err(|e| at(head, e))?;
        let name = c.fn_name().map_err(|e| at(head, e))?;
        let mut locals = vec![LocalDecl {
            ty: self.tys.unit(), // replaced by the return type below
            mutability: Mutability::Mut,
            kind: LocalKind::ReturnPlace,
            source_info: SourceInfo::dummy(),
        }];
        (|| -> Result<(), String> {
            c.expect("(")?;
            while !c.eat(")") {
                let l = c.local()?;
                if l.index() != locals.len() {
                    return Err(format!("expected parameter _{}, found {l}", locals.len()));
                }
                c.expect(":")?;
                let ty = self.ty(&mut c)?;
                locals.push(LocalDecl {
                    ty,
                    mutability: Mutability::Not,
                    kind: LocalKind::Arg {
                        name: l.to_string(),
                        node: NodeId::DUMMY,
                    },
                    source_info: SourceInfo::dummy(),
                });
                if !c.eat(",") {
                    c.expect(")")?;
                    break;
                }
            }
            c.expect("->")?;
            locals[0].ty = self.ty(&mut c)?;
            c.expect("{")?;
            c.done()
        })()
        .map_err(|e| at(head, e))?;
        let arg_count = locals.len() - 1;

        // Local declarations. `_0` is declared again here, with its
        // mutability, exactly as the printer writes it.
        let mut i = start + 1;
        while let Some(line) = lines.get(i) {
            if !line.code.starts_with("let ") {
                break;
            }
            self.local_decl(line, &mut locals, arg_count)
                .map_err(|e| at(line, e))?;
            i += 1;
        }

        let mut body = Body {
            instance: InstanceId {
                def: self.fns[&name],
                args: Vec::new(),
                name: name.clone(),
            },
            locals,
            arg_count,
            blocks: Vec::new(),
            scopes: vec![SourceScopeData {
                parent: None,
                span: SourceInfo::dummy().span,
            }],
            phase: MirPhase::Built,
            span: SourceInfo::dummy().span,
        };

        // Blocks, until the body's closing brace.
        loop {
            let line = lines
                .get(i)
                .ok_or_else(|| at(head, format!("`fn {name}` is not closed")))?;
            if line.code == "}" {
                i += 1;
                break;
            }
            let mut c = Cur::new(&line.code);
            let bb = c
                .block()
                .and_then(|bb| {
                    c.expect(":")?;
                    c.expect("{")?;
                    c.done()?;
                    Ok(bb)
                })
                .map_err(|e| at(line, e))?;
            if bb.index() != body.blocks.len() {
                return Err(at(
                    line,
                    format!("expected bb{}, found {bb}", body.blocks.len()),
                ));
            }
            i += 1;
            let first = i;
            while lines.get(i).is_some_and(|l| l.code != "}") {
                i += 1;
            }
            if i == first || i >= lines.len() {
                return Err(at(line, format!("{bb} needs a terminator and a `}}`")));
            }
            let mut statements = Vec::new();
            for l in &lines[first..i - 1] {
                let kind = self.statement(&body, &l.code).map_err(|e| at(l, e))?;
                statements.push(Statement {
                    kind,
                    source_info: SourceInfo::dummy(),
                });
            }
            let last = &lines[i - 1];
            let kind = self
                .terminator(&body, &last.code)
                .map_err(|e| at(last, e))?;
            body.blocks.push(BasicBlockData {
                statements,
                terminator: Terminator {
                    kind,
                    source_info: SourceInfo::dummy(),
                },
            });
            i += 1; // the block's `}`
        }
        if body.blocks.is_empty() {
            return Err(at(head, format!("`fn {name}` has no blocks")));
        }
        self.bodies.push(body);
        Ok(i)
    }

    fn local_decl(
        &mut self,
        line: &Line,
        locals: &mut Vec<LocalDecl>,
        arg_count: usize,
    ) -> Result<(), String> {
        let mut c = Cur::new(&line.code);
        c.expect_kw("let")?;
        let mutability = if c.eat_kw("mut") {
            Mutability::Mut
        } else {
            Mutability::Not
        };
        let l = c.local()?;
        c.expect(":")?;
        let ty = self.ty(&mut c)?;
        c.expect(";")?;
        c.done()?;
        if l.index() == 0 {
            if ty != locals[0].ty {
                return Err("_0 must have the return type".into());
            }
            locals[0].mutability = mutability;
            return Ok(());
        }
        let expected = locals.len().max(arg_count + 1);
        if l.index() != expected {
            return Err(format!("expected _{expected}, found {l}"));
        }
        let kind = match line.comment.as_deref() {
            Some("drop flag") => LocalKind::DropFlag,
            Some(name) if !name.is_empty() => LocalKind::User {
                name: name.to_string(),
                node: NodeId::DUMMY,
            },
            _ => LocalKind::Temp,
        };
        locals.push(LocalDecl {
            ty,
            mutability,
            kind,
            source_info: SourceInfo::dummy(),
        });
        Ok(())
    }

    fn statement(&mut self, body: &Body, code: &str) -> Result<StatementKind, String> {
        let mut c = Cur::new(code);
        let kind = if c.eat_kw("nop") {
            StatementKind::Nop
        } else if c.eat("StorageLive(") {
            let l = c.local()?;
            c.expect(")")?;
            StatementKind::StorageLive(l)
        } else if c.eat("StorageDead(") {
            let l = c.local()?;
            c.expect(")")?;
            StatementKind::StorageDead(l)
        } else if c.eat("discriminant(") {
            let p = self.place(body, &mut c)?;
            c.expect(")")?;
            c.expect("=")?;
            let v = variant_of(body, &self.tys, &p, c.ident()?)?;
            StatementKind::SetDiscriminant(p, v)
        } else if c.eat("flag_acquire(") {
            c.expect("&")?;
            let kind = if c.eat_kw("mut") {
                BorrowKind::Mut
            } else {
                BorrowKind::Shared
            };
            let place = self.place(body, &mut c)?;
            c.expect(",")?;
            c.expect("L")?;
            let loan = c.number()? as u32;
            c.expect(")")?;
            StatementKind::BorrowFlag(FlagOp::Acquire { place, kind, loan })
        } else if c.eat("flag_release(") {
            c.expect("L")?;
            let loan = c.number()? as u32;
            c.expect(")")?;
            StatementKind::BorrowFlag(FlagOp::Release { loan })
        } else if c.eat("flag_check(") {
            c.expect("&")?;
            let kind = if c.eat_kw("mut") {
                BorrowKind::Mut
            } else {
                BorrowKind::Shared
            };
            let place = self.place(body, &mut c)?;
            c.expect(")")?;
            StatementKind::BorrowFlag(FlagOp::Check { place, kind })
        } else {
            let p = self.place(body, &mut c)?;
            c.expect("=")?;
            let dest = place_ty(body, &self.tys, &p)?.ty;
            let rv = self.rvalue(body, &mut c, dest)?;
            StatementKind::Assign(p, rv)
        };
        c.expect(";")?;
        c.done()?;
        Ok(kind)
    }

    fn terminator(&mut self, body: &Body, code: &str) -> Result<TerminatorKind, String> {
        let mut c = Cur::new(code);
        let kind = if c.eat_kw("return") {
            TerminatorKind::Return
        } else if c.eat_kw("unreachable") {
            TerminatorKind::Unreachable
        } else if c.eat_kw("goto") {
            c.expect("->")?;
            TerminatorKind::Goto { target: c.block()? }
        } else if c.eat("abort(") {
            let reason = match c.ident()? {
                "Panic" => AbortReason::Panic,
                "Overflow" => AbortReason::Overflow,
                "DivByZero" => AbortReason::DivByZero,
                "BoundsCheck" => AbortReason::BoundsCheck,
                "UnreachableArm" => AbortReason::UnreachableArm,
                other => return Err(format!("unknown abort reason `{other}`")),
            };
            c.expect(")")?;
            TerminatorKind::Abort { reason }
        } else if c.eat("drop(") {
            let place = self.place(body, &mut c)?;
            c.expect(")")?;
            c.expect("->")?;
            // The text form names no unwind action: v1 has only `Abort`.
            TerminatorKind::Drop {
                place,
                target: c.block()?,
                unwind: UnwindAction::Abort,
            }
        } else if c.eat("switchInt(") {
            let discr = self.operand(body, &mut c)?;
            c.expect(")")?;
            c.expect("->")?;
            c.expect("[")?;
            let mut values = Vec::new();
            let otherwise = loop {
                if c.eat_kw("otherwise") {
                    c.expect(":")?;
                    let b = c.block()?;
                    c.expect("]")?;
                    break b;
                }
                let v = c.number_u128()?;
                c.expect(":")?;
                values.push((v, c.block()?));
                c.expect(",")?;
            };
            TerminatorKind::SwitchInt {
                discr,
                targets: SwitchTargets { values, otherwise },
            }
        } else {
            let destination = self.place(body, &mut c)?;
            c.expect("=")?;
            let func = if c.at_operand() {
                self.operand(body, &mut c)?
            } else {
                let name = c.fn_name()?;
                self.fn_const(&name)
            };
            c.expect("(")?;
            let args = self.operands_until(body, &mut c, ")")?;
            c.expect("->")?;
            let target = if c.eat("!") { None } else { Some(c.block()?) };
            TerminatorKind::Call {
                func,
                args,
                destination,
                target,
                unwind: UnwindAction::Abort,
            }
        };
        c.expect(";")?;
        c.done()?;
        Ok(kind)
    }

    fn fn_const(&mut self, name: &str) -> Operand {
        let def = match self.fns.get(name) {
            Some(&d) => d,
            None => {
                let d = self.new_def(name);
                self.fns.insert(name.to_string(), d);
                d
            }
        };
        Operand::Const(Const {
            ty: self.tys.intern(TyKind::FnDef(def)),
            kind: ConstKind::FnDef(InstanceId {
                def,
                args: Vec::new(),
                name: name.to_string(),
            }),
        })
    }

    fn place(&mut self, body: &Body, c: &mut Cur) -> Result<Place, String> {
        let mut p = if c.eat("(") {
            if c.eat("*") {
                let inner = self.place(body, c)?;
                c.expect(")")?;
                inner.project(ProjElem::Deref)
            } else {
                let inner = self.place(body, c)?;
                c.expect_kw("as")?;
                let v = variant_of(body, &self.tys, &inner, c.ident()?)?;
                c.expect(")")?;
                inner.project(ProjElem::Downcast(v))
            }
        } else {
            Place::local(c.local()?)
        };
        loop {
            c.ws();
            if c.peek() == Some('.') && c.peek_at(1).is_some_and(|ch| ch.is_ascii_digit()) {
                c.eat(".");
                let f = c.number()? as u32;
                let pt = place_ty(body, &self.tys, &p)?;
                let fty = self
                    .tys
                    .field_ty(pt.ty, pt.variant, f)
                    .ok_or_else(|| format!("{} has no field {f}", self.tys.display(pt.ty)))?;
                p = p.field(f, fty);
            } else if c.eat("[") {
                let e = if c.peek() == Some('_') {
                    ProjElem::Index(c.local()?)
                } else {
                    ProjElem::ConstIndex(c.number()?)
                };
                c.expect("]")?;
                p = p.project(e);
            } else {
                return Ok(p);
            }
        }
    }

    fn operand(&mut self, body: &Body, c: &mut Cur) -> Result<Operand, String> {
        if c.eat_kw("copy") {
            Ok(Operand::Copy(self.place(body, c)?))
        } else if c.eat_kw("move") {
            Ok(Operand::Move(self.place(body, c)?))
        } else if c.eat_kw("const") {
            self.constant(c)
        } else {
            Err(format!("expected an operand at `{}`", c.rest()))
        }
    }

    fn operands_until(
        &mut self,
        body: &Body,
        c: &mut Cur,
        close: &str,
    ) -> Result<Vec<Operand>, String> {
        let mut out = Vec::new();
        while !c.eat(close) {
            out.push(self.operand(body, c)?);
            if !c.eat(",") {
                c.expect(close)?;
                break;
            }
        }
        Ok(out)
    }

    fn constant(&mut self, c: &mut Cur) -> Result<Operand, String> {
        c.ws();
        let konst = |ty, kind| Ok(Operand::Const(Const { ty, kind }));
        if c.eat_kw("true") || c.eat_kw("false") {
            let v = c.last_kw == "true";
            let ty = self.tys.bool();
            return konst(ty, ConstKind::Scalar(v as u128));
        }
        if c.eat("()") {
            let ty = self.tys.unit();
            return konst(ty, ConstKind::Unit);
        }
        if c.eat("<ZST") {
            let ty = self.ty(c)?;
            c.expect(">")?;
            return konst(ty, ConstKind::ZeroSized);
        }
        match c.peek() {
            Some('"') => {
                let s = c.quoted('"')?;
                let ty = self.tys.intern(TyKind::Str);
                konst(ty, ConstKind::Str(s))
            }
            Some('\'') => {
                let s = c.quoted('\'')?;
                let mut chars = s.chars();
                let (Some(ch), None) = (chars.next(), chars.next()) else {
                    return Err(format!("`'{s}'` is not one char"));
                };
                let ty = self.tys.intern(TyKind::Char);
                konst(ty, ConstKind::Scalar(ch as u128))
            }
            Some(ch)
                if ch == '-'
                    || ch.is_ascii_digit()
                    || c.rest().starts_with("inf_")
                    || c.rest().starts_with("NaN_") =>
            {
                // `5_i64`, `-5_i64`, `1.0_f64`, `inf_f64`, `NaN_f32`.
                let tok = c.take_while(|ch| ch.is_ascii_alphanumeric() || "_.+-".contains(ch));
                let (v, suffix) = tok
                    .rsplit_once('_')
                    .ok_or_else(|| format!("constant `{tok}` has no type suffix"))?;
                let mut sc = Cur::new(suffix);
                let ty = self.ty(&mut sc)?;
                sc.done()?;
                let kind = match self.tys.kind(ty) {
                    TyKind::Float(_) => ConstKind::Float(
                        v.parse::<f64>()
                            .map_err(|_| format!("bad float `{v}`"))?
                            .to_bits(),
                    ),
                    TyKind::Int(i) if i.signed() => ConstKind::Scalar(
                        v.parse::<i128>()
                            .map_err(|_| format!("bad integer `{v}`"))?
                            as u128,
                    ),
                    _ => ConstKind::Scalar(
                        v.parse::<u128>()
                            .map_err(|_| format!("bad integer `{v}`"))?,
                    ),
                };
                konst(ty, kind)
            }
            _ => {
                let name = c.fn_name()?;
                Ok(self.fn_const(&name))
            }
        }
    }

    /// An rvalue assigned to a place of type `dest`.
    /// The rest of a closure type after `closure#<n>`: its captures'
    /// types in capture order, as `(T1, T2)`. `<n>` is the [`DefId`] of
    /// the closure's body, numbered as for `fn#<n>`.
    fn closure_ty(&mut self, c: &mut Cur, n: u32) -> Result<Ty, String> {
        c.expect("(")?;
        let mut caps = Vec::new();
        if !c.eat(")") {
            loop {
                caps.push(self.ty(c)?);
                if c.eat(")") {
                    break;
                }
                c.expect(",")?;
            }
        }
        Ok(self.tys.intern(TyKind::Closure(DefId(n), caps)))
    }

    fn rvalue(&mut self, body: &Body, c: &mut Cur, dest: Ty) -> Result<Rvalue, String> {
        c.ws();
        if c.eat("&") {
            let kind = if c.eat_kw("mut") {
                BorrowKind::Mut
            } else {
                BorrowKind::Shared
            };
            return Ok(Rvalue::Ref(kind, self.place(body, c)?));
        }
        if c.at_operand() {
            let o = self.operand(body, c)?;
            if c.eat_kw("as") {
                let t = self.ty(c)?;
                c.expect("(")?;
                let kind = match c.ident()? {
                    "IntToInt" => CastKind::IntToInt,
                    "IntToFloat" => CastKind::IntToFloat,
                    "FloatToInt" => CastKind::FloatToInt,
                    "FloatToFloat" => CastKind::FloatToFloat,
                    "IntToChar" => CastKind::IntToChar,
                    "CharToInt" => CastKind::CharToInt,
                    "BoolToInt" => CastKind::BoolToInt,
                    "Erase" => CastKind::Erase,
                    "Downgrade" => CastKind::Downgrade,
                    "Upgrade" => CastKind::Upgrade,
                    other => return Err(format!("unknown cast kind `{other}`")),
                };
                c.expect(")")?;
                return Ok(Rvalue::Cast(kind, o, t));
            }
            return Ok(Rvalue::Use(o));
        }
        if c.eat("(") {
            return Ok(Rvalue::Aggregate(
                AggregateKind::Tuple,
                self.operands_until(body, c, ")")?,
            ));
        }
        if c.eat("[") {
            let elem = match self.tys.kind(dest) {
                TyKind::Array(e, _) => e,
                _ => {
                    return Err(format!(
                        "array aggregate assigned to {}",
                        self.tys.display(dest)
                    ))
                }
            };
            return Ok(Rvalue::Aggregate(
                AggregateKind::Array(elem),
                self.operands_until(body, c, "]")?,
            ));
        }
        if c.eat("closure#") {
            let n = c.number()? as u32;
            let ty = self.closure_ty(c, n)?;
            c.expect("[")?;
            return Ok(Rvalue::Aggregate(
                AggregateKind::Closure { ty },
                self.operands_until(body, c, "]")?,
            ));
        }
        let shared = c.eat_kw("shared");
        let head = c.type_name()?;
        if !shared && c.eat("(") {
            let one = |p: &mut Self, c: &mut Cur| -> Result<Place, String> {
                let pl = p.place(body, c)?;
                c.expect(")")?;
                Ok(pl)
            };
            return Ok(match head.as_str() {
                "retain" => Rvalue::Retain(one(self, c)?),
                "discriminant" => Rvalue::Discriminant(one(self, c)?),
                "Len" => Rvalue::Len(one(self, c)?),
                "Not" | "Neg" => {
                    let o = self.operand(body, c)?;
                    c.expect(")")?;
                    let op = if head == "Not" { UnOp::Not } else { UnOp::Neg };
                    Rvalue::UnaryOp(op, o)
                }
                _ => {
                    let (checked, name) = match head.strip_prefix("Checked") {
                        Some(n) => (true, n),
                        None => (false, head.as_str()),
                    };
                    let op = bin_op(name).ok_or_else(|| format!("unknown rvalue `{head}(...)`"))?;
                    let a = self.operand(body, c)?;
                    c.expect(",")?;
                    let b = self.operand(body, c)?;
                    c.expect(")")?;
                    if checked {
                        Rvalue::CheckedBinaryOp(op, a, b)
                    } else {
                        Rvalue::BinaryOp(op, a, b)
                    }
                }
            });
        }
        // `[shared ]Name[.Variant] { ops }`.
        let a = self.adt(&head)?;
        let variant = if c.peek() == Some('.') {
            c.eat(".");
            let vname = c.ident()?;
            let adt = self.tys.adt(a);
            if !adt.is_enum {
                return Err(format!("`{head}` is a struct, not an enum"));
            }
            VariantIdx(
                adt.variants
                    .iter()
                    .position(|v| v.name == vname)
                    .ok_or_else(|| format!("`{head}` has no variant `{vname}`"))?
                    as u32,
            )
        } else {
            if self.tys.adt(a).is_enum {
                return Err(format!("`{head}` is an enum: name the variant"));
            }
            VariantIdx(0)
        };
        c.expect("{")?;
        let ops = self.operands_until(body, c, "}")?;
        let kind = if shared {
            AggregateKind::Shared {
                ty: self.tys.intern(TyKind::Shared(a)),
                variant,
            }
        } else {
            AggregateKind::Adt {
                ty: self.tys.intern(TyKind::Adt(a)),
                variant,
            }
        };
        Ok(Rvalue::Aggregate(kind, ops))
    }
}

fn variant_of(body: &Body, tys: &TyInterner, p: &Place, name: &str) -> Result<VariantIdx, String> {
    let pt = place_ty(body, tys, p)?;
    let (TyKind::Adt(a) | TyKind::Shared(a)) = tys.kind(pt.ty) else {
        return Err(format!("{} is not an enum", tys.display(pt.ty)));
    };
    tys.adt(a)
        .variants
        .iter()
        .position(|v| v.name == name)
        .map(|i| VariantIdx(i as u32))
        .ok_or_else(|| format!("{} has no variant `{name}`", tys.display(pt.ty)))
}

fn int_ty(s: &str) -> Option<IntTy> {
    Some(match s {
        "i8" => IntTy::I8,
        "i16" => IntTy::I16,
        "i32" => IntTy::I32,
        "i64" => IntTy::I64,
        "u8" => IntTy::U8,
        "u16" => IntTy::U16,
        "u32" => IntTy::U32,
        "u64" => IntTy::U64,
        "usize" => IntTy::Usize,
        _ => return None,
    })
}

fn bin_op(s: &str) -> Option<BinOp> {
    Some(match s {
        "Add" => BinOp::Add,
        "Sub" => BinOp::Sub,
        "Mul" => BinOp::Mul,
        "Div" => BinOp::Div,
        "Rem" => BinOp::Rem,
        "BitAnd" => BinOp::BitAnd,
        "BitOr" => BinOp::BitOr,
        "BitXor" => BinOp::BitXor,
        "Shl" => BinOp::Shl,
        "Shr" => BinOp::Shr,
        "Eq" => BinOp::Eq,
        "Ne" => BinOp::Ne,
        "Lt" => BinOp::Lt,
        "Le" => BinOp::Le,
        "Gt" => BinOp::Gt,
        "Ge" => BinOp::Ge,
        _ => return None,
    })
}

/// A cursor over one line (or one joined declaration).
struct Cur<'a> {
    s: &'a str,
    pos: usize,
    last_kw: &'static str,
}

impl<'a> Cur<'a> {
    fn new(s: &'a str) -> Self {
        Cur {
            s,
            pos: 0,
            last_kw: "",
        }
    }

    fn rest(&self) -> &'a str {
        &self.s[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.rest().chars().nth(n)
    }

    fn ws(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start().len();
    }

    fn eat(&mut self, tok: &str) -> bool {
        self.ws();
        if self.rest().starts_with(tok) {
            self.pos += tok.len();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, tok: &str) -> Result<(), String> {
        if self.eat(tok) {
            Ok(())
        } else {
            Err(format!("expected `{tok}` at `{}`", self.rest()))
        }
    }

    /// Eats `kw` only as a whole word.
    fn eat_kw(&mut self, kw: &'static str) -> bool {
        self.ws();
        let r = self.rest();
        let boundary = r[kw.len().min(r.len())..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        if r.starts_with(kw) && boundary {
            self.pos += kw.len();
            self.last_kw = kw;
            true
        } else {
            false
        }
    }

    fn expect_kw(&mut self, kw: &'static str) -> Result<(), String> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(format!("expected `{kw}` at `{}`", self.rest()))
        }
    }

    fn done(&mut self) -> Result<(), String> {
        self.ws();
        if self.rest().is_empty() {
            Ok(())
        } else {
            Err(format!("unexpected `{}`", self.rest()))
        }
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> &'a str {
        let r = self.rest();
        let n = r.find(|c| !f(c)).unwrap_or(r.len());
        self.pos += n;
        &r[..n]
    }

    fn ident(&mut self) -> Result<&'a str, String> {
        self.ws();
        let start = self.rest();
        if !start
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        {
            return Err(format!("expected a name at `{start}`"));
        }
        Ok(self.take_while(|c| c.is_alphanumeric() || c == '_'))
    }

    /// A type name: `R`, or a monomorphic instance such as `Option[R]` or
    /// `Result[i64, String]`, whose brackets follow the name directly.
    fn type_name(&mut self) -> Result<String, String> {
        let n = self.ident()?;
        Ok(format!("{n}{}", self.bracket_suffix()?))
    }

    /// `[...]` directly after a name, with nested brackets, or nothing.
    fn bracket_suffix(&mut self) -> Result<&'a str, String> {
        let r = self.rest();
        if !r.starts_with('[') {
            return Ok("");
        }
        let mut depth = 0;
        for (i, ch) in r.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos += i + 1;
                        return Ok(&r[..=i]);
                    }
                }
                _ => {}
            }
        }
        Err(format!("unclosed `[` at `{r}`"))
    }

    fn number(&mut self) -> Result<u64, String> {
        self.ws();
        let t = self.take_while(|c| c.is_ascii_digit());
        t.parse()
            .map_err(|_| format!("expected a number at `{}`", self.rest()))
    }

    fn number_u128(&mut self) -> Result<u128, String> {
        self.ws();
        let t = self.take_while(|c| c.is_ascii_digit());
        t.parse()
            .map_err(|_| format!("expected a number at `{}`", self.rest()))
    }

    fn local(&mut self) -> Result<Local, String> {
        self.expect("_")?;
        Ok(Local(self.number()? as u32))
    }

    fn block(&mut self) -> Result<BasicBlock, String> {
        self.expect("bb")?;
        Ok(BasicBlock(self.number()? as u32))
    }

    fn at_operand(&mut self) -> bool {
        self.ws();
        let save = self.pos;
        let yes = self.eat_kw("copy") || self.eat_kw("move") || self.eat_kw("const");
        self.pos = save;
        yes
    }

    /// A function name: `eat`, `R.drop`, `Vec[R].push`. Brackets nest and
    /// may hold spaces; outside them the name ends at `(`, `,`, `)` or
    /// whitespace.
    fn fn_name(&mut self) -> Result<String, String> {
        self.ws();
        let r = self.rest();
        let mut depth = 0i32;
        let mut end = r.len();
        for (i, ch) in r.char_indices() {
            match ch {
                '[' => depth += 1,
                ']' => depth -= 1,
                '(' | ',' | ')' | ';' if depth == 0 => {
                    end = i;
                    break;
                }
                c if c.is_whitespace() && depth == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        if end == 0 || depth != 0 {
            return Err(format!("expected a function name at `{r}`"));
        }
        self.pos += end;
        Ok(r[..end].to_string())
    }

    /// A Rust-`Debug`-escaped string or char literal, without its quotes.
    fn quoted(&mut self, q: char) -> Result<String, String> {
        self.expect(&q.to_string())?;
        let mut out = String::new();
        let mut chars = self.rest().char_indices();
        while let Some((i, ch)) = chars.next() {
            if ch == q {
                self.pos += i + 1;
                return Ok(out);
            }
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            let esc = chars.next().map(|(_, e)| e);
            out.push(match esc {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('0') => '\0',
                Some('\\') => '\\',
                Some('\'') => '\'',
                Some('"') => '"',
                Some('u') => {
                    let mut hex = String::new();
                    if chars.next().map(|(_, c)| c) != Some('{') {
                        return Err("bad `\\u` escape".into());
                    }
                    for (_, h) in chars.by_ref() {
                        if h == '}' {
                            break;
                        }
                        hex.push(h);
                    }
                    u32::from_str_radix(&hex, 16)
                        .ok()
                        .and_then(char::from_u32)
                        .ok_or_else(|| format!("bad `\\u{{{hex}}}` escape"))?
                }
                other => return Err(format!("unknown escape `\\{}`", other.unwrap_or(' '))),
            });
        }
        Err("unterminated literal".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::validate;

    /// Parsing the printed form and printing again gives the same text.
    fn round_trip(src: &str) -> MirModule {
        let m = parse_module(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
        let printed = pretty_module(&m);
        let again = parse_module(&printed).unwrap_or_else(|e| panic!("{e}\n{printed}"));
        assert_eq!(printed, pretty_module(&again));
        for b in &m.bodies {
            assert_eq!(validate(b, &m.tys), Vec::<String>::new(), "{printed}");
        }
        m
    }

    #[test]
    fn mir_text_round_trips_the_spec_example() {
        let src = "\
struct R: Drop { id: i64 }

fn eat(_1: R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"mid\") -> bb1;
    }
    bb1: {
        drop(_1) -> bb2;       // by-value parameter, owned by the callee (D6)
    }
    bb2: {
        return;
    }
}
";
        let m = round_trip(src);
        assert_eq!(
            pretty_module(&m),
            src.replace("       // by-value parameter, owned by the callee (D6)", "")
        );
        let eat = m.body("eat").unwrap();
        assert_eq!(eat.arg_count, 1);
        assert_eq!(m.def_names, ["R", "eat", "println"]);
    }

    #[test]
    fn mir_text_round_trips_every_construct() {
        let src = r#"
enum E: Drop { A(R, i64), B, C { x: u8, y: shared Node } }
struct Node { next: Vec[shared Node], label: String }
struct R: Drop { id: i64 }
struct P: Copy(i64, bool)

fn f(_1: E, _2: ref R, _3: mut ref P, _4: Array[i64, 3]) -> (i64, char) {
    let mut _0: (i64, char);
    let mut _5: i64; // x
    let _6: bool; // drop flag
    let _7: usize;
    let _8: (i64, bool);
    let _9: shared Node;
    let _10: Map[String, Set[u8]];
    let _11: f64;
    let _12: fn#3;
    let _13: E;
    let _14: Array[i64, 3];
    let _15: ref R;
    let _16: Slice[i64];
    let _17: !;
    bb0: {
        StorageLive(_5);
        _5 = Add(copy (_1 as A).1, const -5_i64);
        _5 = copy (*_2).0;
        _5 = copy _4[_7];
        _5 = copy _4[2];
        _8 = CheckedMul(copy _5, const 9223372036854775807_i64);
        _6 = Not(copy _8.1);
        _6 = Eq(copy (*_3).0, const 0_i64);
        _7 = copy _5 as usize (IntToInt);
        _11 = const -1.5e-7_f64;
        _11 = const inf_f64;
        _5 = Neg(copy _5);
        _15 = &(*_2);
        _9 = retain(_9);
        _7 = discriminant(_1);
        _7 = Len(_16);
        _14 = [copy _5, const 1_i64, copy _4[0]];
        _0 = (copy _5, const '\'');
        _13 = E.C { const 255_u8, move _9 };
        _13 = E.B {  };
        _9 = shared Node { move _10, const "a \"b\" // c\n" };
        _12 = const g;
        discriminant(_13) = B;
        StorageDead(_5);
        nop;
        switchInt(copy _6) -> [0: bb1, 7: bb2, otherwise: bb3];
    }
    bb1: {
        _6 = g(move _1, move _3, const <ZST ()>, const true) -> bb2;
    }
    bb2: {
        _17 = copy _12(copy _5) -> !;
    }
    bb3: {
        drop((_1 as C).1) -> bb4;
    }
    bb4: {
        abort(Overflow);
    }
}

fn g(_1: E, _2: mut ref P, _3: (), _4: bool) -> bool {
    let mut _0: bool;
    bb0: {
        unreachable;
    }
}
"#;
        // Not every line here is well-typed MIR; the round trip is about the
        // text. Strip the validator by parsing only.
        let m = parse_module(src).unwrap_or_else(|e| panic!("{e}"));
        let printed = pretty_module(&m);
        let again = parse_module(&printed).unwrap_or_else(|e| panic!("{e}\n{printed}"));
        assert_eq!(printed, pretty_module(&again));
        for needle in [
            "enum E: Drop { A(R, i64), B, C { x: u8, y: shared Node } }",
            "struct P: Copy(i64, bool)",
            "let mut _5: i64; // x",
            "let _6: bool; // drop flag",
            "_5 = Add(copy (_1 as A).1, const -5_i64);",
            "_11 = const -1.5e-7_f64;",
            "_0 = (copy _5, const '\\'');",
            "_13 = E.C { const 255_u8, move _9 };",
            "_9 = shared Node { move _10, const \"a \\\"b\\\" // c\\n\" };",
            "discriminant(_13) = B;",
            "switchInt(copy _6) -> [0: bb1, 7: bb2, otherwise: bb3];",
            "_6 = g(move _1, move _3, const <ZST ()>, const true) -> bb2;",
            "_17 = copy _12(copy _5) -> !;",
            "drop((_1 as C).1) -> bb4;",
            "abort(Overflow);",
        ] {
            assert!(printed.contains(needle), "missing `{needle}` in\n{printed}");
        }
        // `g` is defined later in the module but called earlier: one DefId.
        let f = m.body("f").unwrap();
        let g = m.body("g").unwrap();
        let TerminatorKind::Call { func, .. } = &f.blocks[1].terminator.kind else {
            panic!()
        };
        let Operand::Const(Const {
            kind: ConstKind::FnDef(inst),
            ..
        }) = func
        else {
            panic!()
        };
        assert_eq!(inst.def, g.instance.def);
    }

    #[test]
    fn mir_text_parses_a_lowered_pin() {
        // core pin `drop_callee_owns`, by hand.
        let m = round_trip(
            "\
struct R: Drop { id: i64 }

fn R.drop(_1: mut ref R) -> () {
    let mut _0: ();
    let _2: ();
    bb0: {
        _2 = println(const \"d\", copy (*_1).0) -> bb1;
    }
    bb1: {
        return;
    }
}

fn take(_1: R) -> () {
    let mut _0: ();
    let _2: R; // x
    let _3: ();
    bb0: {
        _2 = R { const 9_i64 };
        _3 = println(const \"in\", copy _1.0) -> bb1;
    }
    bb1: {
        drop(_2) -> bb2;
    }
    bb2: {
        drop(_1) -> bb3;
    }
    bb3: {
        return;
    }
}
",
        );
        assert!(m.tys.adt(m.adt_named("R").unwrap()).has_drop_impl);
        assert!(m.body("R.drop").is_some());
    }

    #[test]
    fn mir_text_names_monomorphic_instances() {
        let m = round_trip(
            "\
struct R: Drop { id: i64 }
enum Option[R] { None, Some(R) }
enum Result[i64, String] { Ok(i64), Err(String) }

fn f(_1: R) -> Option[R] {
    let mut _0: Option[R];
    let _2: Result[i64, String];
    bb0: {
        _0 = Option[R].Some { move _1 };
        _2 = Result[i64, String].Ok { const 1_i64 };
        drop(_2) -> bb1;
    }
    bb1: {
        return;
    }
}
",
        );
        assert!(m.adt_named("Result[i64, String]").is_some());
    }

    #[test]
    fn mir_text_reports_errors_with_line_numbers() {
        let cases = [
            ("fn f() -> () {\n    let mut _0: ();\n    bb0: {\n        return;\n    }\n", "line 1: `fn f` is not closed"),
            ("struct R { a: Q }\n", "line 1: unknown type `Q`"),
            ("fn f() -> () {\n    let mut _0: ();\n    bb1: {\n        return;\n    }\n}\n", "line 3: expected bb0, found bb1"),
            ("enum E { A }\nfn f(_1: E) -> () {\n    let mut _0: ();\n    bb0: {\n        discriminant(_1) = Z;\n        return;\n    }\n}\n", "line 5: E has no variant `Z`"),
            ("fn f() -> () {\n    let mut _0: ();\n    let _2: i64;\n    bb0: {\n        return;\n    }\n}\n", "line 3: expected _1, found _2"),
            ("fn f(_1: i64) -> () {\n    let mut _0: ();\n    bb0: {\n        _1 = copy _1.0;\n        return;\n    }\n}\n", "line 4: i64 has no field 0"),
        ];
        for (src, want) in cases {
            match parse_module(src) {
                Ok(_) => panic!("parsed:\n{src}"),
                Err(e) => assert!(e.contains(want), "`{e}` lacks `{want}`"),
            }
        }
    }
}
