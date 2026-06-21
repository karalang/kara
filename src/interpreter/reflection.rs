//! Comptime `Type` reflection (substrate 2).
//!
//! Implements the reflection API the comptime evaluator dispatches on a
//! [`Value::TypeVal`] pseudovalue — `name()`, `is_struct()` / `is_enum()` /
//! `is_union()` / `is_generic()`, `fields()`, and `variants()`. The data
//! comes from the typecheck result's `struct_info` / `enum_info` /
//! `union_info` tables, exposed to comptime code as ordinary Kāra values
//! (`String`, `Bool`, and the built-in `Field` / `Variant` record structs).
//!
//! Spec: deferred.md § Comptime — "Types as first-class values" + the
//! "Reflection API" table. `size_of()` / `align_of()` / `methods()` /
//! `attributes()` / `generic_args()` are a later slice — they need the
//! layout pass / impl-table threading; this slice ships the structural core.

use std::collections::HashMap;

use crate::typechecker::{type_display, VariantTypeInfo};

use super::value::Value;
use super::Interpreter;
use crate::ast::CallArg;
use crate::token::Span;

impl Interpreter<'_> {
    /// The reflection method names recognized on a `Type` pseudovalue. Kept
    /// in sync with the typechecker's `is_reflection_method`.
    pub(crate) fn is_reflection_method_name(method: &str) -> bool {
        matches!(
            method,
            "name"
                | "is_struct"
                | "is_enum"
                | "is_union"
                | "is_generic"
                | "fields"
                | "variants"
                | "derives"
        )
    }

    /// True if `name` is a struct / enum / union known to the typechecker —
    /// i.e. a name usable as a `Type` pseudovalue in comptime value position.
    pub(crate) fn is_known_type_name(&self, name: &str) -> bool {
        self.typecheck_result.struct_info.contains_key(name)
            || self.typecheck_result.enum_info.contains_key(name)
            || self.typecheck_result.union_info.contains_key(name)
    }

    /// Dispatch a reflection method on a `Type` pseudovalue named `type_name`.
    /// The typechecker has already validated the method name + arity for a
    /// `Type` receiver, so unknown methods here are a defensive fallback.
    pub(crate) fn eval_type_reflection(
        &mut self,
        type_name: &str,
        method: &str,
        args: &[CallArg],
        span: &Span,
    ) -> Value {
        // Evaluate every argument (for side effects, and so `derives` can read
        // its trait-name operand). All methods but `derives` are nullary.
        let arg_vals: Vec<Value> = args
            .iter()
            .map(|a| self.eval_expr_inner(&a.value))
            .collect();

        let tc = self.typecheck_result;
        match method {
            "name" => Value::String(type_name.to_string()),
            "is_struct" => Value::Bool(tc.struct_info.contains_key(type_name)),
            "is_enum" => Value::Bool(tc.enum_info.contains_key(type_name)),
            "is_union" => Value::Bool(tc.union_info.contains_key(type_name)),
            // `T.derives("Trait")` — true when the struct/enum was declared with
            // `#[derive(Trait)]` (including comptime-backed derives such as
            // `Message`). Lets a derive validate that a nested field type is
            // itself a derived message before emitting code that calls its
            // generated methods.
            "derives" => {
                let trait_name = match arg_vals.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => {
                        return self.record_runtime_error(
                            "reflection method `derives` expects a `String` trait name".to_string(),
                            span,
                        )
                    }
                };
                let has = tc
                    .struct_info
                    .get(type_name)
                    .map(|s| s.derived_traits.contains(&trait_name))
                    .or_else(|| {
                        tc.enum_info
                            .get(type_name)
                            .map(|e| e.derived_traits.contains(&trait_name))
                    })
                    .unwrap_or(false);
                Value::Bool(has)
            }
            "is_generic" => {
                let generic = tc
                    .struct_info
                    .get(type_name)
                    .map(|s| !s.generic_params.is_empty())
                    .or_else(|| {
                        tc.enum_info
                            .get(type_name)
                            .map(|e| !e.generic_params.is_empty())
                    })
                    .unwrap_or(false);
                Value::Bool(generic)
            }
            "fields" => Value::array_of(self.reflect_fields(type_name)),
            "variants" => Value::array_of(self.reflect_variants(type_name)),
            other => self.record_runtime_error(
                format!(
                    "unknown comptime reflection method `{other}` on type `{type_name}`; \
                     this slice supports name / is_struct / is_enum / is_union / \
                     is_generic / fields / variants / derives"
                ),
                span,
            ),
        }
    }

    /// Build the `Vec[Field]` for a struct or union — one `Field { name, ty,
    /// is_pub }` record per declared field. Empty for an enum (its payloads
    /// live on `variants()`), matching `T.fields()`'s per-struct semantics.
    fn reflect_fields(&self, type_name: &str) -> Vec<Value> {
        let tc = self.typecheck_result;
        let raw: &[(String, crate::typechecker::Type, bool)] =
            if let Some(s) = tc.struct_info.get(type_name) {
                &s.fields
            } else if let Some(u) = tc.union_info.get(type_name) {
                &u.fields
            } else {
                return Vec::new();
            };
        raw.iter()
            .map(|(fname, fty, is_pub)| {
                let mut fields: HashMap<String, Value> = HashMap::new();
                fields.insert("name".to_string(), Value::String(fname.clone()));
                fields.insert("ty".to_string(), Value::TypeVal(type_display(fty)));
                fields.insert("is_pub".to_string(), Value::Bool(*is_pub));
                Value::Struct {
                    name: "Field".to_string(),
                    fields,
                }
            })
            .collect()
    }

    /// Build the `Vec[Variant]` for an enum — one `Variant { name,
    /// field_count }` record per variant. Empty for a non-enum.
    fn reflect_variants(&self, type_name: &str) -> Vec<Value> {
        let tc = self.typecheck_result;
        let Some(e) = tc.enum_info.get(type_name) else {
            return Vec::new();
        };
        e.variants
            .iter()
            .map(|(vname, vinfo)| {
                let field_count = match vinfo {
                    VariantTypeInfo::Unit => 0,
                    VariantTypeInfo::Tuple(tys) => tys.len(),
                    VariantTypeInfo::Struct(fs) => fs.len(),
                };
                let mut fields: HashMap<String, Value> = HashMap::new();
                fields.insert("name".to_string(), Value::String(vname.clone()));
                fields.insert("field_count".to_string(), Value::Int(field_count as i64));
                Value::Struct {
                    name: "Variant".to_string(),
                    fields,
                }
            })
            .collect()
    }
}
