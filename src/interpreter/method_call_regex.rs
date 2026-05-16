//! Regex-method dispatch — the bodies of the `is_match`/`find`/
//! `find_all`/`replace_all` arms lifted out of `eval_method_call`.
//! The receiver shape is `Value::Struct { name: "Regex", fields: { pattern, regex_ptr } }`
//! where `regex_ptr` is an opaque integer encoding an `Arc<RustRegex>`.

use std::collections::HashMap;

use regex::Regex as RustRegex;

use crate::ast::*;
use crate::token::Span;

use super::value::{EnumData, Value};

impl<'a> super::Interpreter<'a> {
    pub(super) fn try_eval_regex_method(
        &mut self,
        method: &str,
        obj: Value,
        args: &[CallArg],
        _span: &Span,
    ) -> Option<Value> {
        match method {
            "is_match" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Regex" {
                        if let Some(Value::String(ref pattern)) = fields.get("pattern") {
                            if let Ok(rx) = RustRegex::new(pattern) {
                                let haystack = args
                                    .first()
                                    .map(|a| match self.eval_expr_inner(&a.value) {
                                        Value::String(s) => s,
                                        _ => String::new(),
                                    })
                                    .unwrap_or_default();
                                return Some(Value::Bool(rx.is_match(&haystack)));
                            }
                        }
                    }
                }
            }
            "find" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Regex" {
                        if let Some(Value::String(ref pattern)) = fields.get("pattern") {
                            if let Ok(rx) = RustRegex::new(pattern) {
                                let haystack = args
                                    .first()
                                    .map(|a| match self.eval_expr_inner(&a.value) {
                                        Value::String(s) => s,
                                        _ => String::new(),
                                    })
                                    .unwrap_or_default();
                                return Some(match rx.find(&haystack) {
                                    Some(m) => {
                                        let mut mf = HashMap::new();
                                        mf.insert(
                                            "text".to_string(),
                                            Value::String(m.as_str().to_string()),
                                        );
                                        mf.insert(
                                            "start".to_string(),
                                            Value::Int(m.start() as i64),
                                        );
                                        mf.insert("end".to_string(), Value::Int(m.end() as i64));
                                        Value::EnumVariant {
                                            enum_name: "Option".to_string(),
                                            variant: "Some".to_string(),
                                            data: EnumData::Tuple(vec![Value::Struct {
                                                name: "Match".to_string(),
                                                fields: mf,
                                            }]),
                                        }
                                    }
                                    None => Value::EnumVariant {
                                        enum_name: "Option".to_string(),
                                        variant: "None".to_string(),
                                        data: EnumData::Unit,
                                    },
                                });
                            }
                        }
                    }
                }
            }
            "find_all" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Regex" {
                        if let Some(Value::String(ref pattern)) = fields.get("pattern") {
                            if let Ok(rx) = RustRegex::new(pattern) {
                                let haystack = args
                                    .first()
                                    .map(|a| match self.eval_expr_inner(&a.value) {
                                        Value::String(s) => s,
                                        _ => String::new(),
                                    })
                                    .unwrap_or_default();
                                let matches: Vec<Value> = rx
                                    .find_iter(&haystack)
                                    .map(|m| {
                                        let mut mf = HashMap::new();
                                        mf.insert(
                                            "text".to_string(),
                                            Value::String(m.as_str().to_string()),
                                        );
                                        mf.insert(
                                            "start".to_string(),
                                            Value::Int(m.start() as i64),
                                        );
                                        mf.insert("end".to_string(), Value::Int(m.end() as i64));
                                        Value::Struct {
                                            name: "Match".to_string(),
                                            fields: mf,
                                        }
                                    })
                                    .collect();
                                return Some(Value::array_of(matches));
                            }
                        }
                    }
                }
            }
            "replace_all" => {
                if let Value::Struct {
                    ref name,
                    ref fields,
                } = obj
                {
                    if name == "Regex" {
                        if let Some(Value::String(ref pattern)) = fields.get("pattern") {
                            if let Ok(rx) = RustRegex::new(pattern) {
                                let mut arg_iter = args.iter();
                                let haystack = arg_iter
                                    .next()
                                    .map(|a| match self.eval_expr_inner(&a.value) {
                                        Value::String(s) => s,
                                        _ => String::new(),
                                    })
                                    .unwrap_or_default();
                                let replacement = arg_iter
                                    .next()
                                    .map(|a| match self.eval_expr_inner(&a.value) {
                                        Value::String(s) => s,
                                        _ => String::new(),
                                    })
                                    .unwrap_or_default();
                                let result = rx.replace_all(&haystack, replacement.as_str());
                                return Some(Value::String(result.into_owned()));
                            }
                        }
                    }
                }
            }
            _ => return None,
        }
        None
    }
}
