//! What a built-in operation runs on the values it holds (B-2026-11-23-15).
//!
//! `xs.contains(x)` on a `Vec[Big]` calls `Big`'s `eq`, `m.insert(k, v)`
//! hashes its key, `println(x)` renders through `Display`, and a derived
//! impl calls the same trait on each field. The library is ordinary Kāra
//! code and a call through a bound has the effects of the instance's impl
//! (design.md § Generics), so each of these has the effects of the impls it
//! reaches. This module answers, for a type and one of the traits the
//! library calls on its elements, which written impl methods that is; the
//! tables at the end say which built-in method calls which trait, taken
//! from the bounds docs/library writes on each method.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::typechecker::{Type, TypeCheckResult, VariantTypeInfo};

/// A trait the library calls on the elements, keys or values of a built-in
/// type, each with the one method the call runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ElemTrait {
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Display,
    Debug,
    Clone,
    Default,
}

const TRAITS: usize = 8;

impl ElemTrait {
    pub const ALL: [ElemTrait; TRAITS] = [
        ElemTrait::Eq,
        ElemTrait::PartialOrd,
        ElemTrait::Ord,
        ElemTrait::Hash,
        ElemTrait::Display,
        ElemTrait::Debug,
        ElemTrait::Clone,
        ElemTrait::Default,
    ];

    fn trait_name(self) -> &'static str {
        match self {
            ElemTrait::Eq => "PartialEq",
            ElemTrait::PartialOrd => "PartialOrd",
            ElemTrait::Ord => "Ord",
            ElemTrait::Hash => "Hash",
            ElemTrait::Display => "Display",
            ElemTrait::Debug => "Debug",
            ElemTrait::Clone => "Clone",
            ElemTrait::Default => "Default",
        }
    }

    /// The trait's method (design.md § Standard traits).
    pub fn method(self) -> &'static str {
        match self {
            ElemTrait::Eq => "eq",
            ElemTrait::PartialOrd => "partial_cmp",
            ElemTrait::Ord => "cmp",
            ElemTrait::Hash => "hash",
            ElemTrait::Display => "to_string",
            ElemTrait::Debug => "fmt_debug",
            ElemTrait::Clone => "clone",
            ElemTrait::Default => "default",
        }
    }

    pub fn of_method(method: &str) -> Option<ElemTrait> {
        ElemTrait::ALL.into_iter().find(|t| t.method() == method)
    }

    /// The element trait a bound on `name` runs (`Eq` adds no method to
    /// `PartialEq`'s).
    pub fn of_trait(name: &str) -> Option<ElemTrait> {
        match name {
            "Eq" => Some(ElemTrait::Eq),
            _ => ElemTrait::ALL.into_iter().find(|t| t.trait_name() == name),
        }
    }
}

/// The written impl methods one trait reaches in a type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImplReach {
    /// `Type.method` of each written impl.
    pub keys: Vec<String>,
    /// Type parameters in the type: the instance decides.
    pub params: Vec<String>,
    /// A `dyn` or opaque type: any impl of the trait.
    pub open: bool,
}

impl ImplReach {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.params.is_empty() && !self.open
    }

    fn merge(&mut self, other: &ImplReach) {
        self.keys.extend(other.keys.iter().cloned());
        self.params.extend(other.params.iter().cloned());
        self.open |= other.open;
    }

    fn tidy(&mut self) {
        self.keys.sort();
        self.keys.dedup();
        self.params.sort();
        self.params.dedup();
    }
}

/// The built-in type a value has, as far as the method tables care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Family {
    /// `Vec`, `VecDeque`, `Slice`, `Array`.
    Seq,
    /// `Map`, `Set`: keyed by `Hash` and `Eq`.
    Hashed,
    /// `SortedMap`, `SortedSet`, `PriorityQueue`: keyed by `Ord`.
    Sorted,
    #[default]
    Other,
}

/// Whose impls a built-in method runs: every part of the receiver's type,
/// or its key alone (`Map[K, V]`'s `K`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Whole,
    Key,
}

/// One type's reach, per trait.
#[derive(Debug, Clone, Default)]
pub struct TypeImpls {
    pub family: Family,
    whole: [ImplReach; TRAITS],
    key: [ImplReach; TRAITS],
    /// The methods of a program hasher a `Map` or `Set` of this type
    /// drives.
    pub hasher: Vec<String>,
}

impl TypeImpls {
    pub fn reach(&self, tr: ElemTrait, scope: Scope) -> &ImplReach {
        match scope {
            Scope::Whole => &self.whole[tr as usize],
            Scope::Key => &self.key[tr as usize],
        }
    }

    fn is_empty(&self) -> bool {
        self.hasher.is_empty()
            && self.whole.iter().all(ImplReach::is_empty)
            && self.key.iter().all(ImplReach::is_empty)
    }
}

/// Per program type and trait, what the trait's impl for it reaches: its own
/// written method, or for a derived impl what the fields reach.
#[derive(Debug, Default)]
pub struct ElemImplIndex {
    by_name: FxHashMap<String, [ImplReach; TRAITS]>,
    /// Program types with a written impl of the trait: their arguments are
    /// not looked into.
    written: FxHashSet<(String, ElemTrait)>,
    /// Every trait impl method of each program type that implements
    /// `BuildHasher` or `Hasher`.
    hasher_methods: FxHashMap<String, Vec<String>>,
    hashers: Vec<String>,
    /// The types the program wrote a trait impl for, of any trait.
    program_targets: FxHashSet<String>,
}

impl ElemImplIndex {
    pub fn new(typed: &TypeCheckResult) -> Self {
        let mut index = ElemImplIndex::default();
        for (target, method) in &typed.program_trait_impl_methods {
            index.program_targets.insert(target.clone());
            if let Some(tr) = ElemTrait::of_method(method) {
                index.written.insert((target.clone(), tr));
            }
            let hashes = ["BuildHasher", "Hasher"]
                .iter()
                .any(|t| typed.trait_impls.contains(&(t.to_string(), target.clone())));
            if hashes {
                index
                    .hasher_methods
                    .entry(target.clone())
                    .or_default()
                    .push(format!("{target}.{method}"));
            }
        }
        index.hashers = typed
            .trait_impls
            .iter()
            .filter(|(t, _)| t == "Hasher")
            .map(|(_, target)| target.clone())
            .collect();
        index.hashers.sort();
        if index.written.is_empty() && index.hasher_methods.is_empty() {
            return index;
        }
        // The fields a derived impl walks, per program type, with the type's
        // own parameters (an instance's arguments are read where it is used).
        struct Decl<'t> {
            derived: &'t std::collections::HashSet<String>,
            params: &'t [String],
            fields: Vec<&'t Type>,
        }
        let no_traits = std::collections::HashSet::new();
        let distinct_derives: FxHashMap<&str, std::collections::HashSet<String>> = typed
            .distinct_type_traits
            .iter()
            .map(|(name, traits)| (name.as_str(), traits.iter().cloned().collect()))
            .collect();
        let mut decls: FxHashMap<&str, Decl> = FxHashMap::default();
        for (name, info) in &typed.struct_info {
            decls.insert(
                name.as_str(),
                Decl {
                    derived: &info.derived_traits,
                    params: &info.generic_params,
                    fields: info.fields.iter().map(|(_, t, _)| t).collect(),
                },
            );
        }
        for (name, info) in &typed.enum_info {
            let mut fields = Vec::new();
            for (_, v) in &info.variants {
                match v {
                    VariantTypeInfo::Unit => {}
                    VariantTypeInfo::Tuple(ts) => fields.extend(ts.iter()),
                    VariantTypeInfo::Struct(fs) => fields.extend(fs.iter().map(|(_, t)| t)),
                }
            }
            decls.insert(
                name.as_str(),
                Decl {
                    derived: &info.derived_traits,
                    params: &info.generic_params,
                    fields,
                },
            );
        }
        for (name, base) in &typed.distinct_bases {
            decls.insert(
                name.as_str(),
                Decl {
                    derived: distinct_derives.get(name.as_str()).unwrap_or(&no_traits),
                    params: typed
                        .distinct_params
                        .get(name)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    fields: vec![base],
                },
            );
        }
        for &name in decls.keys() {
            let mut row: [ImplReach; TRAITS] = Default::default();
            for tr in ElemTrait::ALL {
                if index.written.contains(&(name.to_string(), tr)) {
                    row[tr as usize]
                        .keys
                        .push(format!("{name}.{}", tr.method()));
                }
            }
            index.by_name.insert(name.to_string(), row);
        }
        // A derived impl reaches what its fields reach, to a fixed point
        // (a type may hold itself).
        loop {
            let mut changed = false;
            for (&name, decl) in &decls {
                for tr in ElemTrait::ALL {
                    if index.written.contains(&(name.to_string(), tr)) {
                        continue;
                    }
                    let walks = if decl.derived.contains(tr.trait_name()) {
                        Some(tr)
                    } else if tr == ElemTrait::PartialOrd
                        && (decl.derived.contains("Ord")
                            || index.written.contains(&(name.to_string(), ElemTrait::Ord)))
                    {
                        // `<` on a type with `Ord` alone runs its `cmp`.
                        None
                    } else {
                        continue;
                    };
                    let mut reach = ImplReach::default();
                    match walks {
                        Some(tr) => {
                            for field in &decl.fields {
                                index.collect(field, tr, &mut reach);
                            }
                        }
                        None => reach.merge(&index.by_name[name][ElemTrait::Ord as usize]),
                    }
                    reach.params.retain(|p| !decl.params.contains(p));
                    reach.tidy();
                    let slot = index.by_name.get_mut(name).map(|row| &mut row[tr as usize]);
                    if let Some(slot) = slot {
                        if *slot != reach {
                            *slot = reach;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        index
            .by_name
            .retain(|_, row| row.iter().any(|r| !r.is_empty()));
        index
    }

    /// No program type has a written impl of any of the traits: nothing a
    /// built-in runs has effects.
    pub fn is_empty(&self) -> bool {
        self.written.is_empty() && self.hasher_methods.is_empty()
    }

    /// The program wrote a trait impl: a library method bounded on the
    /// trait may run it.
    pub fn has_program_impls(&self) -> bool {
        !self.program_targets.is_empty()
    }

    fn collect(&self, ty: &Type, tr: ElemTrait, out: &mut ImplReach) {
        match ty {
            Type::Named { name, args } => {
                if let Some(row) = self.by_name.get(name) {
                    out.merge(&row[tr as usize]);
                }
                if !self.written.contains(&(name.clone(), tr)) {
                    for a in args {
                        self.collect(a, tr, out);
                    }
                }
            }
            Type::Shared(name) => {
                if let Some(row) = self.by_name.get(name) {
                    out.merge(&row[tr as usize]);
                }
            }
            Type::Tuple(ts) => {
                for t in ts {
                    self.collect(t, tr, out);
                }
            }
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => self.collect(element, tr, out),
            Type::Ref(inner)
            | Type::MutRef(inner)
            | Type::Rc(inner)
            | Type::Arc(inner)
            | Type::Weak(inner)
            | Type::Pointer { inner, .. } => self.collect(inner, tr, out),
            Type::Refinement { base, .. } => self.collect(base, tr, out),
            // A function value stands for what it returns (`sort_by_key`'s
            // key).
            Type::Function { return_type, .. } | Type::OnceFunction { return_type, .. } => {
                self.collect(return_type, tr, out)
            }
            Type::TypeParam(p) => out.params.push(p.clone()),
            Type::AssocProjection { param, .. } => out.params.push(param.clone()),
            Type::Dyn { .. } => out.open = true,
            Type::Existential {
                trait_name,
                trait_args,
                assoc_bindings,
                ..
            } => {
                for t in trait_args {
                    self.collect(t, tr, out);
                }
                for (_, t) in assoc_bindings {
                    self.collect(t, tr, out);
                }
                // An opaque iterator is its items; any other opaque type
                // hides which impl it is.
                if !trait_name.ends_with("Iterator") {
                    out.open = true;
                }
            }
            _ => {}
        }
    }

    /// Whether any trait could reach anything in `ty`.
    pub fn touches(&self, ty: &Type) -> bool {
        match ty {
            Type::Named { name, args } => {
                self.by_name.contains_key(name)
                    || self.hasher_methods.contains_key(name)
                    || self.program_targets.contains(name)
                    || args.iter().any(|a| self.touches(a))
            }
            Type::Shared(name) => {
                self.by_name.contains_key(name) || self.program_targets.contains(name)
            }
            Type::Tuple(ts) => ts.iter().any(|t| self.touches(t)),
            Type::Array { element, .. }
            | Type::Vector { element, .. }
            | Type::Slice { element, .. } => self.touches(element),
            Type::Ref(inner)
            | Type::MutRef(inner)
            | Type::Rc(inner)
            | Type::Arc(inner)
            | Type::Weak(inner)
            | Type::Pointer { inner, .. } => self.touches(inner),
            Type::Refinement { base, .. } => self.touches(base),
            Type::Function { return_type, .. } | Type::OnceFunction { return_type, .. } => {
                self.touches(return_type)
            }
            Type::TypeParam(_)
            | Type::AssocProjection { .. }
            | Type::Dyn { .. }
            | Type::Existential { .. } => true,
            _ => false,
        }
    }

    /// What each trait reaches in `ty`, or `None` when none reaches
    /// anything.
    pub fn of(&self, ty: &Type) -> Option<TypeImpls> {
        if self.is_empty() || !self.touches(ty) {
            return None;
        }
        let mut seen = ty;
        while let Type::Ref(inner) | Type::MutRef(inner) | Type::Refinement { base: inner, .. } =
            seen
        {
            seen = inner;
        }
        let mut impls = TypeImpls::default();
        let mut key_ty = None;
        match seen {
            Type::Named { name, args } => match name.as_str() {
                "Vec" | "VecDeque" => impls.family = Family::Seq,
                "Map" | "Set" => {
                    impls.family = Family::Hashed;
                    key_ty = args.first();
                    let hasher_at = if name == "Map" { 2 } else { 1 };
                    if let Some(Type::Named { name: h, .. }) = args.get(hasher_at) {
                        if self.hasher_methods.contains_key(h) {
                            // The table builds a hasher with `h` and feeds
                            // it; which `Hasher` that is, `h`'s impl says.
                            let mut keys: Vec<String> = self.hasher_methods[h].clone();
                            for hasher in &self.hashers {
                                keys.extend(
                                    self.hasher_methods
                                        .get(hasher)
                                        .into_iter()
                                        .flatten()
                                        .cloned(),
                                );
                            }
                            keys.sort();
                            keys.dedup();
                            impls.hasher = keys;
                        }
                    }
                }
                "SortedMap" | "SortedSet" | "PriorityQueue" => {
                    impls.family = Family::Sorted;
                    key_ty = args.first();
                }
                _ => {}
            },
            Type::Slice { .. } | Type::Array { .. } => impls.family = Family::Seq,
            _ => {}
        }
        for tr in ElemTrait::ALL {
            let mut whole = ImplReach::default();
            self.collect(ty, tr, &mut whole);
            whole.tidy();
            impls.key[tr as usize] = match key_ty {
                Some(k) => {
                    let mut key = ImplReach::default();
                    self.collect(k, tr, &mut key);
                    key.tidy();
                    key
                }
                None => whole.clone(),
            };
            impls.whole[tr as usize] = whole;
        }
        (!impls.is_empty()).then_some(impls)
    }
}

/// Whether a standard trait (design.md § Standard traits), or one of its
/// supertraits, declares `method`. None of them writes a clause, so a call
/// through such a bound has the effects of the instance's impl.
pub fn standard_trait_declares(trait_name: &str, method: &str) -> bool {
    let declares: &[&str] = match trait_name {
        "PartialEq" | "Eq" => &["eq"],
        "PartialOrd" => &["partial_cmp", "eq"],
        "Ord" => &["cmp", "partial_cmp", "eq"],
        "Hash" => &["hash"],
        "Display" => &["to_string"],
        "Debug" => &["fmt_debug"],
        "Clone" | "Copy" => &["clone"],
        "Default" => &["default"],
        _ => &[],
    };
    declares.contains(&method)
}

/// Whether the typechecker solved every part of `ty`.
pub fn is_solved(ty: &Type) -> bool {
    match ty {
        Type::TypeVar(_) | Type::Error => false,
        Type::Named { args, .. } => args.iter().all(is_solved),
        Type::Tuple(ts) => ts.iter().all(is_solved),
        Type::Array { element, .. }
        | Type::Vector { element, .. }
        | Type::Slice { element, .. } => is_solved(element),
        Type::Ref(inner)
        | Type::MutRef(inner)
        | Type::Rc(inner)
        | Type::Arc(inner)
        | Type::Weak(inner)
        | Type::Pointer { inner, .. } => is_solved(inner),
        Type::Refinement { base, .. } => is_solved(base),
        Type::Function {
            params,
            return_type,
            ..
        }
        | Type::OnceFunction {
            params,
            return_type,
            ..
        } => params.iter().all(is_solved) && is_solved(return_type),
        _ => true,
    }
}

/// The traits a method of a built-in type runs on the receiver's elements,
/// from the bound docs/library writes on it (`fn contains(self, val: T) ->
/// bool where T: PartialEq`, collections.md), and for the keyed types from
/// the bound on the type (`Map[K, V]` where `K: Hash + Eq`): every method
/// of a keyed type that takes, stores or looks up a key runs it.
pub fn method_traits(family: Family, method: &str) -> &'static [(ElemTrait, Scope)] {
    use ElemTrait as T;
    use Scope::{Key, Whole};
    // Whatever the receiver: the trait methods themselves, on a built-in
    // type or through a derived impl, and the methods the docs bound on the
    // element whatever holds it.
    match method {
        "eq" | "ne" => return &[(T::Eq, Whole)],
        "partial_cmp" | "lt" | "le" | "gt" | "ge" => return &[(T::PartialOrd, Whole)],
        "cmp" => return &[(T::Ord, Whole)],
        "hash" => return &[(T::Hash, Whole)],
        "to_string" => return &[(T::Display, Whole)],
        "fmt_debug" => return &[(T::Debug, Whole)],
        "clone" | "try_clone" | "cloned" | "cycle" => return &[(T::Clone, Whole)],
        "unwrap_or_default" => return &[(T::Default, Whole)],
        // `Map.default()` is an empty table, not its values' defaults.
        "default" if family == Family::Other => return &[(T::Default, Whole)],
        _ => {}
    }
    match family {
        Family::Seq => match method {
            "contains" | "starts_with" | "ends_with" | "dedup" => &[(T::Eq, Whole)],
            "binary_search" | "is_sorted" | "sort" => &[(T::Ord, Whole)],
            "to_vec" | "fill" | "clone_from_slice" | "resize" | "extend_from_slice" => {
                &[(T::Clone, Whole)]
            }
            "join" => &[(T::Display, Whole)],
            _ => &[],
        },
        // Every method: the type is declared with the bound
        // (`impl[K: Hash + Eq, V] Map[K, V]`), so each may run it.
        Family::Hashed => &[(T::Hash, Key), (T::Eq, Key)],
        Family::Sorted => &[(T::Ord, Key)],
        // An iterator's `min` / `max` (iterators.md); a `SortedSet`'s are
        // its ends, read with no comparison.
        Family::Other => match method {
            "min" | "max" => &[(T::Ord, Whole)],
            // A range's bounds are compared (`impl[T: PartialOrd + Copy]
            // Range[T]`).
            "contains" | "is_empty" => &[(T::PartialOrd, Whole)],
            _ => &[],
        },
    }
}

/// The methods that build a keyed collection from their receiver: charged
/// the key traits of the collection they return.
pub const BUILDS: &[&str] = &["collect", "try_collect", "into", "try_into", "from_iter"];

/// The methods that order by a key their first argument computes
/// (`fn sort_by_key[K: Ord]`): charged `K`'s `cmp`.
pub const BY_KEY: &[&str] = &["sort_by_key", "min_by_key", "max_by_key"];

/// The built-in functions that run a trait on each argument: the print
/// family renders through `Display` (design.md § Display), `assert_eq`
/// compares and, when it fails, renders through `Debug`.
pub const FUNCTIONS: &[(&str, &[ElemTrait])] = &[
    ("println", &[ElemTrait::Display]),
    ("print", &[ElemTrait::Display]),
    ("eprintln", &[ElemTrait::Display]),
    ("eprint", &[ElemTrait::Display]),
    ("assert_eq", &[ElemTrait::Eq, ElemTrait::Debug]),
    ("assert_ne", &[ElemTrait::Eq, ElemTrait::Debug]),
    ("dbg", &[ElemTrait::Debug]),
    ("repeat", &[ElemTrait::Clone]),
];

/// The associated functions of a built-in type that run a trait on an
/// argument (`fn filled(n: i64, val: T) -> Vec[T] where T: Clone`).
pub const ASSOCIATED: &[(&str, &str, ElemTrait)] = &[("Vec", "filled", ElemTrait::Clone)];

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILIES: [Family; 4] = [Family::Seq, Family::Hashed, Family::Sorted, Family::Other];

    fn charged(method: &str, tr: ElemTrait) -> bool {
        FAMILIES
            .iter()
            .any(|f| method_traits(*f, method).iter().any(|(t, _)| *t == tr))
            || (tr == ElemTrait::Ord && BY_KEY.contains(&method))
            || FUNCTIONS
                .iter()
                .any(|(f, traits)| *f == method && traits.contains(&tr))
            || ASSOCIATED.iter().any(|(_, f, t)| *f == method && *t == tr)
    }

    /// `(method, signature)` of each row of a method table in a library
    /// page, with the `## ` section it is under.
    fn rows(file: &str) -> Vec<(String, String, String)> {
        let path = format!("{}/docs/library/{file}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut section = String::new();
        let mut out = Vec::new();
        for line in text.lines() {
            if let Some(title) = line.strip_prefix("## ") {
                section = title.to_string();
            }
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            if cells.len() >= 4 && cells[2].starts_with("`fn ") {
                out.push((
                    section.clone(),
                    cells[1].trim_matches('`').to_string(),
                    cells[2].trim_matches('`').to_string(),
                ));
            }
        }
        out
    }

    /// The list is docs/library's: every method a library page bounds on an
    /// element trait (`where T: PartialEq`, `[K: Ord]`) is charged that
    /// trait. A method documented with a new bound fails here until the
    /// table has it.
    #[test]
    fn every_method_the_library_docs_bound_on_an_element_trait_is_charged_it() {
        let mut seen = 0;
        for file in ["collections.md", "core-types.md", "iterators.md"] {
            for (section, method, sig) in rows(file) {
                for (bound, tr) in [
                    (": PartialEq", ElemTrait::Eq),
                    (": Ord", ElemTrait::Ord),
                    (": PartialOrd", ElemTrait::PartialOrd),
                    (": Hash", ElemTrait::Hash),
                    (": Display", ElemTrait::Display),
                    (": Debug", ElemTrait::Debug),
                    (": Clone", ElemTrait::Clone),
                    (": Default", ElemTrait::Default),
                ] {
                    if sig.contains(bound) {
                        seen += 1;
                        assert!(
                            charged(&method, tr),
                            "{file} § {section}: `{method}` is bounded `{bound}` (`{sig}`) and \
                             no table charges it {tr:?}"
                        );
                    }
                }
            }
        }
        assert!(seen >= 20, "the pages' bounded rows were not found: {seen}");
    }

    /// A table entry for a method the library writes in Kāra is never
    /// used: the signature rule charges such a method by its bounds
    /// (B-2026-11-23-17). So the table is the natives', and where it names
    /// a method the library also writes, the signature must bound the same
    /// trait, or the two would disagree about what a call runs. A keyed
    /// type's entry covers every method, so each one the library writes,
    /// in an inherent impl or a trait's, is bounded on its key unless it is
    /// one that reads no key.
    #[test]
    fn a_table_entry_the_library_writes_in_kara_is_bounded_the_same_in_its_signature() {
        let dir = format!("{}/runtime/stdlib/source", env!("CARGO_MANIFEST_DIR"));
        let families = [
            (Family::Seq, &["Vec", "VecDeque", "Slice", "Array"][..]),
            (Family::Hashed, &["Map", "Set"][..]),
            (
                Family::Sorted,
                &["SortedMap", "SortedSet", "PriorityQueue"][..],
            ),
        ];
        // Methods of a keyed type that compare and hash no key: they walk,
        // count or take an end of what is already in order.
        const READS_NO_KEY: &[&str] = &[
            "iter",
            "keys",
            "values",
            "values_mut",
            "into_keys",
            "into_values",
            "into_iter",
            "first",
            "last",
            "pop_first",
            "pop_last",
            "default",
            "len",
            "is_empty",
            "serialize",
        ];
        let mut written = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap();
            // Each inherent impl block's header and the methods inside it.
            let mut header: Option<(String, String)> = None;
            let mut depth = 0i32;
            let mut native = false;
            for line in text.lines() {
                let t = line.trim();
                if depth == 0 && t.starts_with("impl") && t.ends_with('{') {
                    let head = t.trim_end_matches('{').trim();
                    let ty = head
                        .split_whitespace()
                        .last()
                        .unwrap_or_default()
                        .split('[')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    header = Some((ty, head.to_string()));
                }
                if t.starts_with("#[compiler_builtin]") {
                    native = true;
                }
                if depth == 1 && t.starts_with("fn ") {
                    if let Some((ty, head)) = &header {
                        let name = t[3..].split(['(', '[']).next().unwrap_or_default();
                        for (family, types) in families {
                            if !types.contains(&ty.as_str()) || native {
                                continue;
                            }
                            if matches!(family, Family::Hashed | Family::Sorted)
                                && READS_NO_KEY.contains(&name)
                            {
                                continue;
                            }
                            for (tr, _) in method_traits(family, name) {
                                written += 1;
                                let bound = match tr {
                                    ElemTrait::Eq => ["PartialEq", "Eq"],
                                    ElemTrait::PartialOrd => ["PartialOrd", "Ord"],
                                    ElemTrait::Ord => ["Ord", "Ord"],
                                    other => [other.trait_name(), other.trait_name()],
                                };
                                assert!(
                                    bound.iter().any(|b| head.contains(b) || t.contains(b)),
                                    "{}: `{ty}.{name}` is written in Kāra under `{head}` \
                                     and the table charges it {tr:?}, which its signature \
                                     does not bound",
                                    path.display()
                                );
                            }
                        }
                    }
                    native = false;
                }
                depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                if depth == 0 {
                    header = None;
                }
            }
        }
        assert!(
            written >= 3,
            "no Kāra-written table method found: {written}"
        );
    }
}
