//! Extern-function effect registration + FFI linter hints + profile
//! enforcement.
//!
//! Houses three related functions:
//!
//! - `register_extern_function_effects` — per-`extern` declaration
//!   handler: seeds `declared_effects` from the ABI's trust-not-verify
//!   `with` clause (every effect without one), and runs the FFI linter.
//! - `check_ffi_linter_hints` — advisory `FfiLintHint` diagnostics
//!   for extern symbols whose names suggest commonly-omitted effects
//!   (`blocks`, `allocates(Heap)`).
//! - `profile_forbids` — compile-profile forbidden-effect lookup
//!   (e.g., the `no_alloc` profile rejects `allocates(Heap)`).
//!
//! Lives in a sibling `impl<'a> super::EffectChecker<'a>` block.

use crate::ast::*;
use crate::token::Span;

use super::{
    verb_name, DeclaredEffects, Effect, EffectError, EffectErrorKind, EffectOrigin, EffectSet,
};

impl<'a> super::EffectChecker<'a> {
    /// Per-`ExternFunction` effect-set registration. Used at both the
    /// (now-dead) top-level `Item::ExternFunction` arm (with
    /// `block_attrs = &[]`) and the per-item arm inside an
    /// `Item::ExternBlock` (with `block_attrs = &b.attributes`).
    /// Block-level attributes are NOT pre-merged into per-item
    /// `attributes`; they're passed through here.
    pub(crate) fn register_extern_function_effects(
        &mut self,
        e: &ExternFunction,
        _block_attrs: &[Attribute],
    ) {
        // design.md §15: an `extern` function's effects are exactly its
        // `with` clause; with no clause it has every effect, and `pure`
        // none. The ABI's defaults (`blocks`, and `panics` for C-unwind)
        // were added to an explicit clause, which the `noblock` attribute
        // existed to take back; both are gone (B-2026-11-16-261).
        let builtin_span = Span {
            line: 0,
            column: 0,
            offset: 0,
            length: 0,
        };
        let blocks = |set: &mut EffectSet, verb: EffectVerbKind| {
            set.add(
                Effect {
                    verb,
                    resource: String::new(),
                },
                EffectOrigin::Direct(builtin_span),
            )
        };
        // What a declaration with no clause is read as stating for the
        // profile check and the linter's hints: what its ABI always does,
        // so the hints name the rest.
        let mut abi_defaults = EffectSet::new();
        if !e.is_pure {
            match e.abi.as_str() {
                "C" => blocks(&mut abi_defaults, EffectVerbKind::Blocks),
                "C-unwind" => {
                    blocks(&mut abi_defaults, EffectVerbKind::Blocks);
                    blocks(&mut abi_defaults, EffectVerbKind::Panics);
                }
                _ => {}
            }
        }
        let programmer_decl = self.parse_signature_effects(&e.effects, &e.params, None);
        let final_decl = match &programmer_decl {
            // No `with` clause and not `pure`: the foreign body is
            // invisible, so it has every effect (design.md § 15;
            // core-semantics.md § 12, B-2026-11-14-33).
            DeclaredEffects::None if !e.is_pure => DeclaredEffects::Explicit(
                super::every_effect_set(EffectOrigin::Direct(builtin_span)),
            ),
            // A clause, or `pure`: exactly what it says.
            _ => programmer_decl.clone(),
        };

        let stated_decl = if e.effects.is_none() && !e.is_pure && !abi_defaults.is_empty() {
            DeclaredEffects::Explicit(abi_defaults)
        } else if e.effects.is_none() && !e.is_pure {
            DeclaredEffects::None
        } else {
            final_decl.clone()
        };

        // Profile-compatibility check: reject effects forbidden by the
        // active compile profile at the extern declaration site, among those
        // it states (with no clause, the ABI defaults). The rest of an
        // every-effect set is refused at each call the profile governs.
        if let DeclaredEffects::Explicit(ref set) = stated_decl {
            for te in &set.effects {
                if let Some(forbidden_reason) = self.profile_forbids(&te.effect, &e.name, &e.abi) {
                    self.errors.push(EffectError {
                        message: forbidden_reason,
                        span: e.span,
                        kind: EffectErrorKind::ProfileViolation,
                        subtype_trace: None,
                        replacement: None,
                        undeclared: None,
                    });
                }
            }
        }

        // Advisory linter hints for commonly-omitted effects. A `pure`
        // declaration is the author saying there are none.
        if !e.is_pure {
            self.check_ffi_linter_hints(&e.name, &e.span, &stated_decl);
        }

        let sym = self.interner.intern(&e.name);
        self.declared_effects.insert(sym, final_decl);
        self.function_visibility.insert(sym, true);
        self.function_spans.insert(sym, e.span);
        // Seed inferred_effects from the declared set so callers accumulate
        // its leaf effects.
        if let Some(DeclaredEffects::Explicit(ref set)) = self.declared_effects.get(&sym) {
            self.inferred_effects.insert(sym, set.clone());
        } else {
            self.inferred_effects.insert(sym, EffectSet::new());
        }
    }

    fn check_ffi_linter_hints(&mut self, symbol: &str, span: &Span, decl: &DeclaredEffects) {
        // Normalize: take the last segment after any `.` separator, strip a
        // leading `_` that some platforms prepend (e.g. macOS `_malloc`).
        let base = symbol.rsplit('.').next().unwrap_or(symbol);
        let base = base.strip_prefix('_').unwrap_or(base);

        let declared_set: Option<&EffectSet> = match decl {
            DeclaredEffects::Explicit(s) => Some(s),
            DeclaredEffects::PolymorphicWithFixed(s) => Some(s),
            _ => None,
        };

        let has_verb = |verb: EffectVerbKind, resource: &str| -> bool {
            declared_set.is_some_and(|s| {
                s.effects.iter().any(|te| {
                    te.effect.verb == verb
                        && (resource.is_empty() || te.effect.resource == resource)
                })
            })
        };

        // Known-blocking symbols — suggest `blocks`.
        const KNOWN_BLOCKING: &[&str] = &[
            "sleep",
            "usleep",
            "nanosleep",
            "read",
            "write",
            "recv",
            "recvfrom",
            "recvmsg",
            "send",
            "sendto",
            "sendmsg",
            "accept",
            "accept4",
            "connect",
            "poll",
            "select",
            "pselect",
            "epoll_wait",
            "kevent",
            "waitpid",
            "wait",
            "wait4",
            "flock",
            "lockf",
            "pthread_mutex_lock",
            "pthread_cond_wait",
            "pthread_join",
            "open",
            "fopen",
            "openat",
            "creat",
            "close",
            "fsync",
            "fdatasync",
            "gethostbyname",
            "getaddrinfo",
        ];

        if KNOWN_BLOCKING.contains(&base) && !has_verb(EffectVerbKind::Blocks, "") {
            self.errors.push(EffectError {
                message: format!(
                    "FFI lint: '{}' is commonly blocking; consider adding `blocks` to its \
                     effect list",
                    symbol
                ),
                span: *span,
                kind: EffectErrorKind::FfiLintHint,
                subtype_trace: None,
                replacement: None,
                undeclared: None,
            });
        }

        // Known-allocating symbols — suggest `allocates(Heap)`.
        const KNOWN_ALLOCATING: &[&str] = &[
            "malloc",
            "calloc",
            "realloc",
            "reallocarray",
            "strdup",
            "strndup",
            "asprintf",
            "vasprintf",
            "posix_memalign",
            "memalign",
            "aligned_alloc",
            "getaddrinfo",
        ];

        if KNOWN_ALLOCATING.contains(&base) && !has_verb(EffectVerbKind::Allocates, "Heap") {
            // Effect resources are user-defined names — `Heap` is a
            // convention, not a builtin. If the program hasn't declared it,
            // following this hint verbatim would hit resolve's "undefined
            // effect resource 'Heap'" (B-2026-08-02-4), so name the missing
            // declaration in the same breath.
            let heap_declared = self
                .program
                .items
                .iter()
                .any(|i| matches!(i, Item::EffectResource(r) if r.name == "Heap"));
            let decl_note = if heap_declared {
                ""
            } else {
                " (and declare `effect resource Heap;` — effect resources are user-defined names)"
            };
            self.errors.push(EffectError {
                message: format!(
                    "FFI lint: '{}' is commonly allocating; consider adding `allocates(Heap)` \
                     to its effect list{}",
                    symbol, decl_note
                ),
                span: *span,
                kind: EffectErrorKind::FfiLintHint,
                subtype_trace: None,
                replacement: None,
                undeclared: None,
            });
        }
    }

    fn profile_forbids(&self, effect: &Effect, fn_name: &str, abi: &str) -> Option<String> {
        // Single source of truth for the per-profile forbidden-effect table:
        // `CompileProfile::forbids` (manifest.rs). `profile_compat.rs` queries
        // the same method for `#[profile(...)]`-attribute gating.
        if !self.profile().forbids_effect(effect) {
            return None;
        }
        let effect_str = if effect.resource.is_empty() {
            verb_name(&effect.verb)
        } else {
            effect.to_string()
        };
        Some(format!(
            "extern \"{}\" fn {} declares effect `{}`, which is forbidden by the '{}' profile",
            abi,
            fn_name,
            effect_str,
            self.profile().as_str(),
        ))
    }
}
