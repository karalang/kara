//! Compile-time evaluation on the MIR interpreter (B-2026-10-22-20).
//!
//! `comptime` blocks, `#[derive(X)]` backed by a `comptime fn derive_x` and
//! `#[proto_schema]` constants run here, as ordinary MIR, rather than on the
//! legacy tree-walk interpreter. `crate::comptime` builds one root function
//! per evaluation, lowers the program with those roots, and runs each one
//! inside [`with_state`], which supplies what the natives below read:
//!
//! * a comptime `Type` is carried as its name, an `Item` or `Expr` as its
//!   source text (`crate::ty` lowers all three to `String`);
//! * `T.name()`, `T.fields()`, ... (deferred.md § Comptime — Reflection
//!   API) read the type checker's tables, copied into a [`Reflection`];
//! * `ast.item(s)` and `ast.expr(s)` return their text, which the caller
//!   parses once the evaluation is done;
//! * `compiler.error(msg)` records a [`Diag`] at its call site and goes on.
//!
//! The root's return value is copied out of the interpreter's heap as a
//! [`CtValue`] before the heap is gone ([`take_result`]).

use std::cell::RefCell;

use super::*;

/// The reflection methods of a `Type` (deferred.md § Comptime — Reflection
/// API), as `Point.fields()` names them in call position.
pub const REFLECTION_METHODS: &[&str] = &[
    "name",
    "is_struct",
    "is_enum",
    "is_union",
    "is_generic",
    "derives",
    "fields",
    "variants",
    "attributes",
    "generic_params",
    "element_type",
    "key_type",
    "value_type",
];

/// One field of a reflected struct or union.
#[derive(Debug, Clone, Default)]
pub struct ReflField {
    pub name: String,
    /// The field's type, as `type_display` renders it.
    pub ty: String,
    pub is_pub: bool,
    pub attrs: Vec<String>,
}

/// One variant of a reflected enum.
#[derive(Debug, Clone, Default)]
pub struct ReflVariant {
    pub name: String,
    pub field_count: i64,
    /// A single-field tuple variant's payload type, else empty.
    pub payload: String,
    pub field_names: Vec<String>,
    pub field_types: Vec<String>,
    pub attrs: Vec<String>,
}

/// A struct, enum or union as the reflection methods see it.
#[derive(Debug, Clone, Default)]
pub struct ReflType {
    pub is_struct: bool,
    pub is_enum: bool,
    pub is_union: bool,
    pub generic_params: Vec<String>,
    pub derives: Vec<String>,
    /// The type's own attributes, rendered.
    pub attrs: Vec<String>,
    pub fields: Vec<ReflField>,
    pub variants: Vec<ReflVariant>,
}

/// The program's types, by name, for the reflection methods.
#[derive(Debug, Clone, Default)]
pub struct Reflection {
    pub types: rustc_hash::FxHashMap<String, ReflType>,
}

/// A `compiler.error(msg)`: its message and call site.
#[derive(Debug, Clone)]
pub struct Diag {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub offset: usize,
    pub length: usize,
}

/// A value copied out of the interpreter, with its shape.
#[derive(Debug, Clone, PartialEq)]
pub enum CtValue {
    Bool(bool),
    Int(i128),
    Char(char),
    Float(f64),
    Unit,
    Str(String),
    /// A `Vec`'s elements.
    Vec(Vec<CtValue>),
    /// An array's elements.
    Array(Vec<CtValue>),
    Tuple(Vec<CtValue>),
    /// A struct: its name and fields, by name, in order.
    Struct(String, Vec<(String, CtValue)>),
    /// An enum value: the enum's name, the variant's, and its fields.
    Variant(String, String, Vec<CtValue>),
}

struct State {
    refl: Reflection,
    diags: Vec<Diag>,
    result: Option<Result<CtValue, String>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` with `refl` available to the natives, and returns what it
/// returned with the `compiler.error`s raised meanwhile.
pub(crate) fn active_now() -> bool {
    active()
}

pub fn with_state<T>(refl: Reflection, f: impl FnOnce() -> T) -> (T, Vec<Diag>) {
    STATE.with(|s| {
        *s.borrow_mut() = Some(State {
            refl,
            diags: Vec::new(),
            result: None,
        })
    });
    let out = f();
    let diags = STATE.with(|s| s.borrow_mut().take().map(|st| st.diags).unwrap_or_default());
    (out, diags)
}

/// The last root's return value, copied out of the heap, once.
pub fn take_result() -> Option<Result<CtValue, String>> {
    STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.result.take()))
}

fn active() -> bool {
    STATE.with(|s| s.borrow().is_some())
}

fn reflected(name: &str) -> Option<ReflType> {
    STATE.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.refl.types.get(name).cloned())
    })
}

/// The `n`th top-level generic argument of a rendered type:
/// `Map[String, Vec[i64]]` at 1 is `Vec[i64]`.
fn nth_type_arg(name: &str, n: usize) -> Option<String> {
    let lb = name.find('[')?;
    let rb = name.rfind(']')?;
    if rb <= lb + 1 {
        return None;
    }
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut cur = String::new();
    for c in name[lb + 1..rb].chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => {
                parts.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    parts.push(cur.trim().to_string());
    parts.get(n).cloned()
}

impl Interp<'_> {
    /// A comptime native: `Type.<method>`, `ast.item`, `ast.expr` or
    /// `compiler.error`.
    pub(super) fn comptime_native(
        &mut self,
        base: &str,
        method: &str,
        args: Vec<Value>,
        ret: Ty,
    ) -> R<Value> {
        let mut text = Vec::with_capacity(args.len());
        for a in &args {
            let v = self.through_refs(a)?;
            text.push(match v {
                Value::Int(_) => String::new(),
                _ => self.string_at(&v)?,
            });
        }
        let arg = |i: usize| text.get(i).cloned().unwrap_or_default();
        match (base, method) {
            ("ast", "item" | "expr") => Ok(self.alloc_box("String", Value::Str(arg(0)))),
            ("compiler", "error") => {
                let at = |i: usize| match args.get(i) {
                    Some(Value::Int(n)) => *n as usize,
                    _ => 0,
                };
                let d = Diag {
                    message: arg(0),
                    line: at(1),
                    column: at(2),
                    offset: at(3),
                    length: at(4),
                };
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.diags.push(d);
                    }
                });
                Ok(Value::Unit)
            }
            ("Type", _) => self.reflect(&arg(0), method, &arg(1), ret),
            _ => err(format!("call of unknown function {base}.{method}")),
        }
    }

    fn reflect(&mut self, name: &str, method: &str, operand: &str, ret: Ty) -> R<Value> {
        if !active() {
            return err(format!(
                "`Type.{method}` runs only at compile time, inside a `comptime` evaluation"
            ));
        }
        let t = reflected(name).unwrap_or_default();
        let string = |s: &mut Self, v: &str| s.alloc_box("String", Value::Str(v.to_string()));
        match method {
            "name" | "clone" => Ok(string(self, name)),
            "element_type" | "key_type" => {
                let peeled = nth_type_arg(name, 0).unwrap_or_else(|| name.to_string());
                Ok(string(self, &peeled))
            }
            "value_type" => {
                let peeled = nth_type_arg(name, 1).unwrap_or_else(|| name.to_string());
                Ok(string(self, &peeled))
            }
            "is_struct" => Ok(Value::Bool(t.is_struct)),
            "is_enum" => Ok(Value::Bool(t.is_enum)),
            "is_union" => Ok(Value::Bool(t.is_union)),
            "is_generic" => Ok(Value::Bool(!t.generic_params.is_empty())),
            "derives" => Ok(Value::Bool(t.derives.iter().any(|d| d == operand))),
            "generic_params" => Ok(self.string_vec(&t.generic_params)),
            "attributes" => Ok(self.string_vec(&t.attrs)),
            "fields" => {
                let names = self.record_fields(ret)?;
                let mut out = Vec::new();
                for f in &t.fields {
                    let mut parts = Vec::new();
                    for n in &names {
                        parts.push(match n.as_str() {
                            "name" => string(self, &f.name),
                            "ty" => string(self, &f.ty),
                            "is_pub" => Value::Bool(f.is_pub),
                            "attrs" => self.string_vec(&f.attrs),
                            other => return err(format!("Field has no part {other}")),
                        });
                    }
                    out.push(Value::Agg(parts));
                }
                Ok(self.alloc_box("Vec", Value::Agg(out)))
            }
            "variants" => {
                let names = self.record_fields(ret)?;
                let mut out = Vec::new();
                for v in &t.variants {
                    let mut parts = Vec::new();
                    for n in &names {
                        parts.push(match n.as_str() {
                            "name" => string(self, &v.name),
                            "field_count" => Value::Int(v.field_count as i128),
                            "payload" | "payload_ty" => string(self, &v.payload),
                            "field_names" => self.string_vec(&v.field_names),
                            "field_types" => self.string_vec(&v.field_types),
                            "attrs" => self.string_vec(&v.attrs),
                            other => return err(format!("Variant has no part {other}")),
                        });
                    }
                    out.push(Value::Agg(parts));
                }
                Ok(self.alloc_box("Vec", Value::Agg(out)))
            }
            other => err(format!(
                "unknown comptime reflection method `{other}` on type `{name}`"
            )),
        }
    }

    fn string_vec(&mut self, items: &[String]) -> Value {
        let boxed = items
            .iter()
            .map(|s| self.alloc_box("String", Value::Str(s.clone())))
            .collect();
        self.alloc_box("Vec", Value::Agg(boxed))
    }

    /// The field names, in layout order, of the record a `Vec[Field]` or
    /// `Vec[Variant]` result holds.
    fn record_fields(&self, ret: Ty) -> R<Vec<String>> {
        let elem = match self.tys.kind(ret) {
            TyKind::Intrinsic(IntrinsicTy::Vec(e)) => e,
            _ => return err(format!("a reflection list of {}", self.tys.display(ret))),
        };
        let TyKind::Adt(a) = self.tys.kind(elem) else {
            return err(format!("a reflection record of {}", self.tys.display(elem)));
        };
        let adt = self.tys.adt(a);
        Ok(adt.variants[0]
            .fields
            .iter()
            .map(|(n, _)| n.clone())
            .collect())
    }

    /// Keeps the comptime root's result, when a comptime evaluation runs.
    pub(super) fn comptime_capture(&mut self, entry: &str, v: &Value) {
        if !active() {
            return;
        }
        let Some(ty) = self.program.bodies.get(entry).map(|b| b.locals[0].ty) else {
            return;
        };
        let got = self.ct_value(v, ty).map_err(|e| match e {
            Stop::Error(m) => m,
            _ => "the result could not be read".to_string(),
        });
        STATE.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                st.result = Some(got);
            }
        });
    }

    fn ct_value(&mut self, v: &Value, ty: Ty) -> R<CtValue> {
        let v = self.through_refs(v)?;
        let ty = match self.tys.kind(ty) {
            TyKind::Ref(t) | TyKind::MutRef(t) => t,
            _ => ty,
        };
        // An `Item` or `Expr` is its text whatever type the program gives
        // the name: a program that declares its own `struct Item` has the
        // name resolve to that struct in a derive's `-> Vec[Item]`.
        if let (TyKind::Adt(a), Value::Box(id)) = (self.tys.kind(ty), &v) {
            if matches!(self.tys.adt(a).name.as_str(), "Item" | "Expr") {
                if let Value::Str(s) = &self.live(*id)?.value {
                    return Ok(CtValue::Str(s.to_string()));
                }
            }
        }
        Ok(match (self.tys.kind(ty), &v) {
            (TyKind::Bool, Value::Bool(b)) => CtValue::Bool(*b),
            (TyKind::Int(_), Value::Int(n)) => CtValue::Int(*n),
            (TyKind::Char, Value::Char(c)) => CtValue::Char(*c),
            (TyKind::Float(_), Value::Float(f)) => CtValue::Float(*f),
            (TyKind::Unit, _) => CtValue::Unit,
            (TyKind::Str | TyKind::Intrinsic(IntrinsicTy::String), _) => {
                CtValue::Str(self.string_at(&v)?)
            }
            (TyKind::Intrinsic(IntrinsicTy::Vec(e)), _) => {
                let id = self.box_behind(&v)?;
                let items = match &self.live(id)?.value {
                    Value::Agg(items) => items.clone(),
                    other => return err(format!("a Vec holding {other:?}")),
                };
                let mut out = Vec::with_capacity(items.len());
                for it in &items {
                    out.push(self.ct_value(it, e)?);
                }
                CtValue::Vec(out)
            }
            (TyKind::Array(e, _), Value::Agg(items)) => {
                let items = items.clone();
                let mut out = Vec::with_capacity(items.len());
                for it in &items {
                    out.push(self.ct_value(it, e)?);
                }
                CtValue::Array(out)
            }
            (TyKind::Tuple(ts), Value::Agg(items)) => {
                let (ts, items) = (ts.clone(), items.clone());
                let mut out = Vec::with_capacity(items.len());
                for (t, it) in ts.iter().zip(&items) {
                    out.push(self.ct_value(it, *t)?);
                }
                CtValue::Tuple(out)
            }
            (TyKind::Adt(a), Value::Variant(k, fs)) => {
                let adt = self.tys.adt(a);
                let var = &adt.variants[*k as usize];
                let fs = fs.clone();
                let mut out = Vec::with_capacity(fs.len());
                for (i, f) in fs.iter().enumerate() {
                    let ft = self
                        .tys
                        .field_ty(ty, Some(*k), i as u32)
                        .ok_or_else(|| Stop::Error(format!("{} has no field {i}", var.name)))?;
                    out.push(self.ct_value(f, ft)?);
                }
                CtValue::Variant(adt.name.clone(), var.name.clone(), out)
            }
            (TyKind::Adt(a), Value::Agg(fs)) => {
                let adt = self.tys.adt(a);
                let fs = fs.clone();
                let mut out = Vec::with_capacity(fs.len());
                for (i, f) in fs.iter().enumerate() {
                    let ft = self
                        .tys
                        .field_ty(ty, None, i as u32)
                        .ok_or_else(|| Stop::Error(format!("{} has no field {i}", adt.name)))?;
                    let name = adt.variants[0].fields[i].0.clone();
                    out.push((name, self.ct_value(f, ft)?));
                }
                CtValue::Struct(adt.name.clone(), out)
            }
            (_, other) => {
                return err(format!(
                    "a `comptime` result of type {} ({other:?}) cannot be read out",
                    self.tys.display(ty)
                ))
            }
        })
    }
}
