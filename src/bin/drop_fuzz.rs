//! `drop_fuzz` — the drop-soundness fuzzer (ownership-model-mechanization spike, Slice 1).
//!
//! Hunts the memory-safety bug *class* the katas find one-at-a-time today
//! (double-free / use-after-free / leak in codegen's drop insertion), but
//! exhaustively and automatically. It generates small, well-typed Kāra
//! programs over the **heap core** — owned heap values (String, Vec, Box,
//! structs/enums with heap fields), moves (by-value pass, return, store into an
//! aggregate), borrows (`.iter()`, index reads), projections (field / index /
//! destructure), and the stressful containers (for-loops over collections,
//! `Option` / enum payloads, `par {}` captures) — compiles each with the exact
//! AOT path `karac build` ships, links it under AddressSanitizer +
//! LeakSanitizer, runs it, and lets the **sanitizer be the judge**. No model
//! required: ASan catches double-free / UAF at runtime, LSan catches leaks.
//!
//! This is Slice 1 of the spike: *measure first, build nothing in the
//! compiler.* It touches no compiler code — it only drives the existing
//! `karac` library (`parse → resolve → typecheck → lower → ownershipcheck →
//! concurrency_analyze → compile_to_object → link_executable_with_sanitizer`),
//! the same pipeline `tests/memory_sanitizer.rs` trusts. See
//! `docs/spikes/ownership-model-mechanization.md`.
//!
//! ## Three surfaces
//!
//! Every generated program is run three ways:
//!   - **seq**     — `concurrency = None`  → auto-par codegen dormant
//!   - **autopar** — `concurrency = Some(analysis)` → inferred parallel groups
//!     lowered (the default-`karac build` posture)
//!   - **interp**  — the tree-walk interpreter, in-process via
//!     `Interpreter::captured_output`. Compiles nothing and links no sanitizer,
//!     so it contributes only a drop log — but `run-vs-build` is a 56-entry
//!     ledger class that no pair of AOT surfaces can see by construction.
//!
//! Some drop bugs diverge only under auto-par ([[auto-par-is-third-ab-surface]]),
//! so a finding on *any* surface is a finding, and a *disagreement* between two
//! of them is a finding in its own right.
//!
//! ## Two oracles
//!
//! **The sanitizer**, for anything that touches memory: ASan catches
//! double-free / UAF, LSan catches leaks. No model required.
//!
//! **The drop log**, for user `impl Drop` bodies — which the sanitizers cannot
//! see at all. A body that runs twice, or never, but touches no freed pointer
//! is invisible to both: the memory is fine, only the side effect is wrong.
//! That is not a corner case. It is what most of the ledger's recent drop bugs
//! were, and with no `impl Drop` anywhere in the corpus this fuzzer could
//! report zero findings while they kept landing. `new_tracked` prints
//! `N<tag>`, the Drop body prints `D<tag>`, and the invariant is per-tag
//! balance. See `DropLog` for why balance rather than an expected schedule.
//!
//! ## Gotchas honored (do not re-discover — see the spike's Gotchas section)
//!   - **≥36-byte payloads.** LSan misses *reachable* short-String leaks
//!     ([[lsan-reachability-short-string-leaks]]); every generated String
//!     payload is ≥40 bytes.
//!   - **Escape or DCE hides the leak.** Every heap value produced is *read*
//!     (its length / an element / a field) into a running `acc` accumulator
//!     that is `println`'d each loop iteration — so a leaked or use-after-freed
//!     value is observable ([[struct-drop-depth-invariant-and-option-blocker]]).
//!   - **Loop the body.** Double-frees surface on the *second* free and leaks
//!     accumulate, so `main`'s body runs inside a `while` loop.
//!   - **Valid-program discipline.** A program is only *run* if it parses,
//!     typechecks, and passes the ownership checker cleanly — so a sanitizer
//!     finding implicates the *lowering*, never buggy generated source.
//!
//! ## Usage
//!
//! ```text
//! cargo run --features llvm --bin drop_fuzz -- [options]
//!   --count N        number of programs to generate (default 200)
//!   --seed S         base seed (default 1); program k uses seed S+k
//!   --out DIR        write shrunk repros + report here (default target/drop-fuzz)
//!   --no-shrink      skip the shrinker (faster; repros stay full-size)
//!   --keep-going     do not stop after the first finding of each signature
//!   --verbose        print per-program progress
//! ```
//!
//! Prefer the one-command wrapper `scripts/drop-fuzz.sh`, which builds the
//! runtime archives first (a hard prerequisite for the ASan link).

#[cfg(not(feature = "llvm"))]
fn main() {
    eprintln!(
        "drop_fuzz requires the `llvm` feature (it drives the AOT codegen path).\n\
         Build/run with: cargo run --features llvm --bin drop_fuzz -- ..."
    );
    std::process::exit(2);
}

#[cfg(feature = "llvm")]
fn main() {
    llvm_main::run();
}

#[cfg(feature = "llvm")]
mod llvm_main {
    use karac::drop_differential::DiffOutcome;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{Duration, Instant};

    // ───────────────────────── PRNG (splitmix64) ─────────────────────────
    // No `rand` dependency in the tree; a deterministic splitmix64 keeps each
    // program reproducible from its seed alone (the whole point of a shrinkable
    // corpus — a finding is `--seed <s> --count 1`).
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            // Avoid the all-zero state's degenerate first outputs.
            Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next_u64() % n as u64) as usize
            }
        }
        /// True with probability `num/den`.
        fn chance(&mut self, num: u64, den: u64) -> bool {
            self.next_u64() % den < num
        }
    }

    // ───────────────────────── heap-core types ───────────────────────────
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Ty {
        Str,        // String
        VecStr,     // Vec[String]
        Pair,       // (i64, String)
        VecPair,    // Vec[(i64, String)]
        Payload,    // struct Payload { tag: i64, name: String, items: Vec[String] }
        OptStr,     // Option[String]
        VecVecStr,  // Vec[Vec[String]]
        VecPayload, // Vec[Payload]
        MapStr,     // Map[String, i64]
        SetStr,     // Set[String]
        // ── user-`impl Drop` shapes (the drop-log oracle's subjects) ──
        // Everything above drops *implicitly*: codegen frees the heap and the
        // sanitizer judges. These four carry a user `impl Drop` body whose
        // execution is observable in stdout, which is a strictly harder
        // contract — a body that runs twice but only prints is invisible to
        // ASan and LSan alike. See `DropLog`.
        Tracked,    // struct Tracked { tag: i64, name: String } + impl Drop
        VecTracked, // Vec[Tracked]
        OptTracked, // Option[Tracked]
        CrateT,     // struct Crate { lid: i64, item: Tracked } — Drop one level down
    }

    // A live binding in the generated `main` body.
    #[derive(Clone)]
    struct Var {
        name: String,
        ty: Ty,
        /// Declared `let mut` — required for a `mut ref` call-site marker
        /// (`grow(mut v)`) to typecheck.
        mutable: bool,
    }

    // ───────────────────────── the generator ─────────────────────────────
    struct Gen {
        rng: Rng,
        scope: Vec<Var>, // live (unmoved) bindings
        body: Vec<String>,
        counter: usize,
        // A monotonic "payload id" so every String literal is distinct across
        // the run (distinct enough that a UAF reads a recognizably-wrong value,
        // and long enough — ≥40 bytes — to defeat short-String LSan blindness).
        payload_id: usize,
        used_par: bool,
        /// Distinct `Tracked.tag` per construction *site*. The oracle counts
        /// `N<tag>` / `D<tag>` tokens per tag rather than per value, so a site
        /// inside the round loop legitimately contributes many of each — what
        /// must hold is that they balance.
        tracked_tag: i64,
    }

    impl Gen {
        fn new(seed: u64) -> Self {
            Gen {
                rng: Rng::new(seed),
                scope: Vec::new(),
                body: Vec::new(),
                counter: 0,
                payload_id: 0,
                used_par: false,
                tracked_tag: 0,
            }
        }

        /// A fresh `Tracked` construction-site tag.
        fn fresh_tag(&mut self) -> i64 {
            self.tracked_tag += 1;
            self.tracked_tag
        }

        fn fresh(&mut self, prefix: &str) -> String {
            self.counter += 1;
            format!("{prefix}{}", self.counter)
        }

        /// A ≥40-byte distinct String literal expression (`"...".to_string()`).
        fn str_literal(&mut self) -> String {
            self.payload_id += 1;
            let id = self.payload_id;
            let pad = "payload_bytes_kept_long_enough_for_lsan_"; // 41 chars
            format!("\"df{id}_{pad}{id}\".to_string()")
        }

        /// A small non-negative i64 literal expression.
        fn i64_literal(&mut self) -> String {
            format!("{}i64", self.rng.below(90) + 1)
        }

        fn emit(&mut self, line: String) {
            self.body.push(line);
        }

        fn add_var(&mut self, name: String, ty: Ty) {
            self.scope.push(Var {
                name,
                ty,
                mutable: false,
            });
        }

        /// Add a binding that was declared `let mut` (eligible for a `mut ref`
        /// call-site marker).
        fn add_var_mut(&mut self, name: String, ty: Ty) {
            self.scope.push(Var {
                name,
                ty,
                mutable: true,
            });
        }

        /// A live binding of the given type that was declared `let mut`.
        fn live_mut_of(&mut self, ty: Ty) -> Option<Var> {
            let idxs: Vec<usize> = self
                .scope
                .iter()
                .enumerate()
                .filter(|(_, v)| v.ty == ty && v.mutable)
                .map(|(i, _)| i)
                .collect();
            if idxs.is_empty() {
                return None;
            }
            let pick = idxs[self.rng.below(idxs.len())];
            Some(self.scope[pick].clone())
        }

        /// Remove a live binding by name (it was moved out).
        fn kill(&mut self, name: &str) {
            self.scope.retain(|v| v.name != name);
        }

        /// A live binding of the given type, if any (does not remove it).
        fn live_of(&mut self, ty: Ty) -> Option<Var> {
            let idxs: Vec<usize> = self
                .scope
                .iter()
                .enumerate()
                .filter(|(_, v)| v.ty == ty)
                .map(|(i, _)| i)
                .collect();
            if idxs.is_empty() {
                return None;
            }
            let pick = idxs[self.rng.below(idxs.len())];
            Some(self.scope[pick].clone())
        }

        /// Consume (move out) a live binding of `ty`, returning its name.
        fn take(&mut self, ty: Ty) -> Option<String> {
            let v = self.live_of(ty)?;
            self.kill(&v.name);
            Some(v.name)
        }

        // ── producers: create a fresh binding of a given type ─────────────

        fn make_str(&mut self) {
            let n = self.fresh("s");
            let lit = self.str_literal();
            self.emit(format!("        let {n}: String = {lit};"));
            self.add_var(n, Ty::Str);
        }

        fn make_vecstr(&mut self) {
            let n = self.fresh("v");
            // Either a literal or a `Vec.new()`+push sequence (both heap; the
            // push path is where several element-drop bugs lived).
            if self.rng.chance(1, 2) {
                let a = self.str_literal();
                let b = self.str_literal();
                let c = self.str_literal();
                self.emit(format!(
                    "        let {n}: Vec[String] = Vec[{a}, {b}, {c}];"
                ));
                self.add_var(n, Ty::VecStr);
            } else {
                self.emit(format!("        let mut {n}: Vec[String] = Vec.new();"));
                let count = 2 + self.rng.below(2);
                for _ in 0..count {
                    let lit = self.str_literal();
                    self.emit(format!("        {n}.push({lit});"));
                }
                self.add_var_mut(n, Ty::VecStr);
            }
        }

        fn make_pair(&mut self) {
            // Prefer moving a live String into the pair (aliasing stress); else
            // a fresh literal.
            let n = self.fresh("p");
            let k = self.i64_literal();
            let s = self
                .take(Ty::Str)
                .unwrap_or_else(|| self.str_literal_binding());
            self.emit(format!("        let {n}: (i64, String) = ({k}, {s});"));
            self.add_var(n, Ty::Pair);
        }

        /// Bind a fresh String to a name and return that name (helper for when
        /// a place-expression — not a literal — is required as a move source).
        fn str_literal_binding(&mut self) -> String {
            let n = self.fresh("s");
            let lit = self.str_literal();
            self.emit(format!("        let {n}: String = {lit};"));
            n
        }

        fn make_payload(&mut self) {
            let n = self.fresh("pl");
            let tag = self.i64_literal();
            let name = self.take(Ty::Str).unwrap_or_else(|| self.str_literal());
            let items = self
                .take(Ty::VecStr)
                .unwrap_or_else(|| self.vecstr_literal());
            self.emit(format!(
                "        let {n}: Payload = Payload {{ tag: {tag}, name: {name}, items: {items} }};"
            ));
            self.add_var(n, Ty::Payload);
        }

        fn vecstr_literal(&mut self) -> String {
            let a = self.str_literal();
            let b = self.str_literal();
            format!("Vec[{a}, {b}]")
        }

        /// A `let mut` Vec[String] with two elements, then index-stores fresh
        /// Strings over both (`v[0i64] = ...`). The reassignment must drop the
        /// overwritten element exactly once — the index-store-of-heap-elem
        /// double-free / leak class.
        fn make_indexed_vecstr(&mut self) {
            let n = self.fresh("iv");
            self.emit(format!("        let mut {n}: Vec[String] = Vec.new();"));
            let a = self.str_literal();
            let b = self.str_literal();
            self.emit(format!("        {n}.push({a});"));
            self.emit(format!("        {n}.push({b});"));
            let c = self.str_literal();
            let d = self.str_literal();
            self.emit(format!("        {n}[0i64] = {c};"));
            self.emit(format!("        {n}[1i64] = {d};"));
            self.add_var_mut(n, Ty::VecStr);
        }

        /// A nested Vec[Vec[String]] — either fresh inner literals or a live
        /// Vec[String] moved into the outer vec (aliasing across nesting).
        fn make_nested_vec(&mut self) {
            let n = self.fresh("vv");
            self.emit(format!(
                "        let mut {n}: Vec[Vec[String]] = Vec.new();"
            ));
            let count = 2 + self.rng.below(2);
            for _ in 0..count {
                let inner = self
                    .take(Ty::VecStr)
                    .unwrap_or_else(|| self.vecstr_literal());
                self.emit(format!("        {n}.push({inner});"));
            }
            self.add_var(n, Ty::VecVecStr);
        }

        /// A recursive boxed `shared enum Tree` with a heap String at the leaf,
        /// summed via a recursive fn (the boxed-enum / move-out-of-payload
        /// class). Consumed immediately (owned into `tree_len`).
        fn make_tree(&mut self) {
            let n = self.fresh("tr");
            let s = self.take(Ty::Str).unwrap_or_else(|| self.str_literal());
            let depth = 1 + self.rng.below(3);
            let mut expr = format!("Leaf({s})");
            for _ in 0..depth {
                expr = format!("Node({expr})");
            }
            self.emit(format!("        let {n}: Tree = {expr};"));
            self.emit(format!("        acc = acc + tree_len({n});"));
        }

        /// A Vec[Payload] — a Vec of heap-bearing structs, exercising the
        /// aggregate-in-collection drop (each element's String + inner Vec must
        /// be freed, once, when the outer Vec drops).
        fn make_payload_vec(&mut self) {
            let n = self.fresh("pv");
            self.emit(format!("        let mut {n}: Vec[Payload] = Vec.new();"));
            let count = 2 + self.rng.below(2);
            for _ in 0..count {
                let tag = self.i64_literal();
                let name = self.str_literal();
                let items = self.vecstr_literal();
                self.emit(format!(
                    "        {n}.push(Payload {{ tag: {tag}, name: {name}, items: {items} }});"
                ));
            }
            self.add_var(n, Ty::VecPayload);
        }

        /// A Map[String, i64] with owned String keys inserted — the key-
        /// adoption class (the map must take ownership of each key exactly once;
        /// the "Map/Set key no-adopt leak" bug lived here).
        fn make_map(&mut self) {
            let n = self.fresh("m");
            self.emit(format!(
                "        let mut {n}: Map[String, i64] = Map.new();"
            ));
            let count = 2 + self.rng.below(2);
            for _ in 0..count {
                let key = self.take(Ty::Str).unwrap_or_else(|| self.str_literal());
                let val = self.i64_literal();
                self.emit(format!("        {n}.insert({key}, {val});"));
            }
            self.add_var_mut(n, Ty::MapStr);
        }

        /// A Set[String] with owned String elements — the sibling key-adoption
        /// class (`Set[T]` lowers to `Map[T, ()]`).
        fn make_set(&mut self) {
            let n = self.fresh("st");
            self.emit(format!("        let mut {n}: Set[String] = Set.new();"));
            let count = 2 + self.rng.below(2);
            for _ in 0..count {
                let key = self.take(Ty::Str).unwrap_or_else(|| self.str_literal());
                self.emit(format!("        {n}.insert({key});"));
            }
            self.add_var_mut(n, Ty::SetStr);
        }

        fn make_optstr(&mut self) {
            let n = self.fresh("o");
            if self.rng.chance(3, 4) {
                let s = self.take(Ty::Str).unwrap_or_else(|| self.str_literal());
                self.emit(format!("        let {n}: Option[String] = Some({s});"));
            } else {
                self.emit(format!("        let {n}: Option[String] = None;"));
            }
            self.add_var(n, Ty::OptStr);
        }

        // ── producers: values carrying a user `impl Drop` body ────────────
        //
        // Always `let mut`: the displacement transforms below need to reassign
        // them, and that is the shape most of the recent ledger bugs live in.

        fn make_tracked(&mut self) {
            let n = self.fresh("t");
            let tag = self.fresh_tag();
            let lit = self.str_literal();
            self.emit(format!(
                "        let mut {n}: Tracked = new_tracked({tag}i64, {lit});"
            ));
            self.add_var_mut(n, Ty::Tracked);
        }

        fn make_vec_tracked(&mut self) {
            let n = self.fresh("vt");
            self.emit(format!("        let mut {n}: Vec[Tracked] = Vec.new();"));
            let pushes = 1 + self.rng.below(3);
            for _ in 0..pushes {
                let tag = self.fresh_tag();
                let lit = self.str_literal();
                self.emit(format!("        {n}.push(new_tracked({tag}i64, {lit}));"));
            }
            self.add_var_mut(n, Ty::VecTracked);
        }

        fn make_opt_tracked(&mut self) {
            let n = self.fresh("ot");
            if self.rng.chance(3, 4) {
                let tag = self.fresh_tag();
                let lit = self.str_literal();
                self.emit(format!(
                    "        let mut {n}: Option[Tracked] = Some(new_tracked({tag}i64, {lit}));"
                ));
            } else {
                self.emit(format!("        let mut {n}: Option[Tracked] = None;"));
            }
            self.add_var_mut(n, Ty::OptTracked);
        }

        fn make_crate(&mut self) {
            let n = self.fresh("cr");
            let tag = self.fresh_tag();
            let lit = self.str_literal();
            self.emit(format!(
                "        let mut {n}: Crate = new_crate({tag}i64, {lit});"
            ));
            self.add_var_mut(n, Ty::CrateT);
        }

        // ── transforms: consume live bindings, exercise drop-prone shapes ──

        /// Move a live String into a fresh Vec[String] via push, then read an
        /// element back into `acc` (the move-into-aggregate shape).
        fn move_str_into_vec(&mut self) -> bool {
            let Some(s) = self.take(Ty::Str) else {
                return false;
            };
            let n = self.fresh("v");
            self.emit(format!("        let mut {n}: Vec[String] = Vec.new();"));
            self.emit(format!("        {n}.push({s});"));
            self.add_var_mut(n, Ty::VecStr);
            true
        }

        /// Conditionally move a live String into a Vec — `if round % 2 == 0 {
        /// v.push(s); }`. The `round`-based guard is false on ~half the loop
        /// iterations, so `s` is moved on some and left owned on others: codegen
        /// must free `s` at scope exit exactly on the not-moved path (a leak if
        /// it wrongly elides, a double-free if it wrongly frees the moved path).
        /// This is the conditional-move drop shape the straight-line corpus
        /// lacked — the runtime (ASan/LSan) counterpart to the oracle's
        /// branch-state merge. `s` is consumed (not read after), so the program
        /// stays ownership-valid.
        fn conditional_move_str_into_vec(&mut self) -> bool {
            let Some(s) = self.take(Ty::Str) else {
                return false;
            };
            let v = match self.live_mut_of(Ty::VecStr) {
                Some(v) => v.name,
                None => {
                    let n = self.fresh("v");
                    self.emit(format!("        let mut {n}: Vec[String] = Vec.new();"));
                    self.add_var_mut(n.clone(), Ty::VecStr);
                    n
                }
            };
            self.emit(format!(
                "        if round % 2i64 == 0i64 {{ {v}.push({s}); }}"
            ));
            true
        }

        /// For-loop over a live Vec[String] (owned iteration — consumes it),
        /// folding element lengths into `acc`. The element `e` aliases the
        /// source buffer in codegen; the loop's element drop + the source's
        /// scope-exit free is the classic double-free surface.
        fn for_owned_vecstr(&mut self) -> bool {
            let Some(v) = self.take(Ty::VecStr) else {
                return false;
            };
            let e = self.fresh("e");
            self.emit(format!(
                "        for {e} in {v} {{ acc = acc + {e}.len(); }}"
            ));
            true
        }

        /// For-loop borrow over a live Vec[String] (source stays live).
        fn for_borrow_vecstr(&mut self) -> bool {
            let Some(v) = self.live_of(Ty::VecStr) else {
                return false;
            };
            let vn = v.name;
            let e = self.fresh("e");
            self.emit(format!(
                "        for {e} in {vn}.iter() {{ acc = acc + {e}.len(); }}"
            ));
            true
        }

        /// Move a live String into a pair, push the pair into a fresh
        /// Vec[(i64,String)], read an element field back (the for-loop-element-
        /// escape / tuple-heap-component shape).
        fn pair_into_vec(&mut self) -> bool {
            let Some(s) = self.take(Ty::Str) else {
                return false;
            };
            let k = self.i64_literal();
            let n = self.fresh("vp");
            self.emit(format!(
                "        let mut {n}: Vec[(i64, String)] = Vec.new();"
            ));
            self.emit(format!("        {n}.push(({k}, {s}));"));
            self.add_var(n, Ty::VecPair);
            true
        }

        /// Fully destructure a live Payload (splits the aggregate's obligation
        /// across its fields), reading each field into `acc`.
        fn destructure_payload(&mut self) -> bool {
            let Some(pl) = self.take(Ty::Payload) else {
                return false;
            };
            let tag = self.fresh("t");
            let name = self.fresh("nm");
            let items = self.fresh("it");
            self.emit(format!(
                "        let Payload {{ tag: {tag}, name: {name}, items: {items} }} = {pl};"
            ));
            self.emit(format!("        acc = acc + {tag} + {name}.len();"));
            // `items` is a live Vec[String] now — hand it to the sink pool.
            self.add_var(items, Ty::VecStr);
            true
        }

        /// Match a live Option[String], reading the payload on `Some`.
        fn match_optstr(&mut self) -> bool {
            let Some(o) = self.take(Ty::OptStr) else {
                return false;
            };
            let x = self.fresh("x");
            self.emit(format!(
                "        match {o} {{ Some({x}) => {{ acc = acc + {x}.len(); }}, None => {{}} }}"
            ));
            true
        }

        /// Pass a live String by value to a helper (owned move across a call
        /// boundary), folding the returned length into `acc`.
        fn pass_owned_str(&mut self) -> bool {
            let Some(s) = self.take(Ty::Str) else {
                return false;
            };
            self.emit(format!("        acc = acc + take_str({s});"));
            true
        }

        /// Pass a live Vec[String] by value to a helper that returns a fresh
        /// Vec[String] (owned in, owned out — return-move).
        fn roundtrip_vecstr(&mut self) -> bool {
            let Some(v) = self.take(Ty::VecStr) else {
                return false;
            };
            let n = self.fresh("v");
            self.emit(format!("        let {n}: Vec[String] = echo_vec({v});"));
            self.add_var(n, Ty::VecStr);
            true
        }

        /// A `par {}` block capturing two live Strings by shared use across the
        /// group (cross-task shared-heap capture — a bug class of its own). Uses
        /// the corpus's Arc-promotion shape via a `shared struct`.
        fn par_capture(&mut self) -> bool {
            if self.used_par {
                return false; // one par block per program keeps shrinking sane
            }
            // Needs two live Strings to wrap.
            let a = match self.take(Ty::Str) {
                Some(a) => a,
                None => return false,
            };
            let b = match self.take(Ty::Str) {
                Some(b) => b,
                None => {
                    // put `a` back conceptually — just re-bind via sink
                    self.emit(format!("        acc = acc + {a}.len();"));
                    return true;
                }
            };
            self.used_par = true;
            self.emit(format!("        let __ha: Holder = Holder {{ s: {a} }};"));
            self.emit(format!("        let __hb: Holder = Holder {{ s: {b} }};"));
            self.emit("        par {".to_string());
            self.emit("            acc = acc + hold_len(__ha);".to_string());
            self.emit("            acc = acc + hold_len(__hb);".to_string());
            self.emit("        }".to_string());
            true
        }

        /// Forward a live String to a `ref String` param (a borrow — source
        /// stays live and is read again after). Exercises borrow-forwarding /
        /// caller-retains-param: the callee must NOT free the borrowed source.
        fn ref_peek_str(&mut self) -> bool {
            let Some(v) = self.live_of(Ty::Str) else {
                return false;
            };
            let s = v.name;
            self.emit(format!("        acc = acc + peek({s});"));
            true
        }

        /// Mutate a live `let mut` Vec[String] in place through a `mut ref`
        /// param (`grow(mut v)` — the call-site `mut` marker is required). The
        /// vec stays live; the pushed element must be freed once at its drop.
        fn mut_grow_vec(&mut self) -> bool {
            let Some(v) = self.live_mut_of(Ty::VecStr) else {
                return false;
            };
            let vn = v.name;
            self.emit(format!("        grow(mut {vn});"));
            true
        }

        /// Capture a live Vec[String] into 2-3 `spawn`ed tasks that each read
        /// it, joining their sums into `acc` — the cross-task shared-heap
        /// capture class (the Vec is referenced by multiple tasks, so codegen
        /// auto-promotes it to a shared reference; the single free must happen
        /// after all joins). One spawn cluster per program keeps shrinking sane.
        fn spawn_capture_vecstr(&mut self) -> bool {
            if self.used_par {
                return false;
            }
            let Some(v) = self.take(Ty::VecStr) else {
                return false;
            };
            self.used_par = true;
            let pool = self.fresh("pool");
            let hs = self.fresh("hs");
            self.emit(format!(
                "        let mut {pool}: TaskGroup = TaskGroup.new();"
            ));
            self.emit(format!(
                "        let mut {hs}: Vec[TaskHandle[i64]] = Vec.new();"
            ));
            let tasks = 2 + self.rng.below(2);
            for t in 0..tasks {
                self.emit(format!(
                    "        {hs}.push({pool}.spawn(|| band({v}, {t}i64)));"
                ));
            }
            let h = self.fresh("h");
            self.emit(format!(
                "        for {h} in {hs} {{ acc = acc + {h}.join(); }}"
            ));
            true
        }

        // ── transforms: displacement of a user-`impl Drop` value ──────────
        //
        // The shapes every recent drop bug in the ledger actually took. What
        // they share is that a live binding's value is *displaced* — overwritten,
        // reassigned, or moved through a variant — rather than simply going out
        // of scope at the end of a block. The straight-line corpus above never
        // produces that, which is why it ran clean while the ledger filled up.

        /// `t = new_tracked(..)` over a live `Tracked`. The displaced value must
        /// run its Drop body exactly once and free its field heap exactly once.
        /// B-2026-07-31-37 ran the body twice here; B-2026-07-31-39 never freed
        /// the old field heap.
        fn reassign_tracked(&mut self) -> bool {
            let Some(v) = self.live_mut_of(Ty::Tracked) else {
                return false;
            };
            let tag = self.fresh_tag();
            let lit = self.str_literal();
            self.emit(format!(
                "        {} = new_tracked({tag}i64, {lit});",
                v.name
            ));
            true
        }

        /// Reassign a `Tracked` that lives one level down, inside a struct
        /// field. Same displacement, but codegen reaches it through a
        /// projection rather than a local slot.
        fn reassign_crate_field(&mut self) -> bool {
            let Some(v) = self.live_mut_of(Ty::CrateT) else {
                return false;
            };
            let tag = self.fresh_tag();
            let lit = self.str_literal();
            self.emit(format!(
                "        {}.item = new_tracked({tag}i64, {lit});",
                v.name
            ));
            true
        }

        /// Displace an `Option[Tracked]` — either to `None` or to a fresh
        /// `Some`. B-2026-07-31-38's shape: a binding moved into a variant
        /// constructor and then reassigned never dropped at all.
        fn reassign_opt_tracked(&mut self) -> bool {
            let Some(v) = self.live_mut_of(Ty::OptTracked) else {
                return false;
            };
            if self.rng.chance(1, 2) {
                self.emit(format!("        {} = None;", v.name));
            } else {
                let tag = self.fresh_tag();
                let lit = self.str_literal();
                self.emit(format!(
                    "        {} = Some(new_tracked({tag}i64, {lit}));",
                    v.name
                ));
            }
            true
        }

        /// Move a live `Tracked` into a fresh `Option`, then displace it. The
        /// two-step version of the above: the value is inside a variant payload
        /// when it gets overwritten.
        fn tracked_into_opt_then_displace(&mut self) -> bool {
            let Some(t) = self.take(Ty::Tracked) else {
                return false;
            };
            let n = self.fresh("ot");
            self.emit(format!("        let mut {n}: Option[Tracked] = Some({t});"));
            if self.rng.chance(1, 2) {
                self.emit(format!("        {n} = None;"));
                self.emit(format!("        acc = acc + {n}.is_none().to_i64();"));
            } else {
                self.add_var_mut(n, Ty::OptTracked);
            }
            true
        }

        /// Consume an `Option[Tracked]` through a match arm binding — the
        /// B-2026-07-30-11 match-arm leg, where the moved payload's Drop body
        /// ran for a value the arm had already taken ownership of.
        fn match_opt_tracked(&mut self) -> bool {
            let Some(o) = self.take(Ty::OptTracked) else {
                return false;
            };
            let x = self.fresh("mt");
            self.emit(format!(
                "        match {o} {{ Some({x}) => {{ acc = acc + tracked_len({x}); }}, None => {{}} }}"
            ));
            true
        }

        /// Move a live `Tracked` into a `Vec[Tracked]`. Element drop inside a
        /// container is a distinct codegen path from a local slot's drop.
        fn tracked_into_vec(&mut self) -> bool {
            let Some(t) = self.take(Ty::Tracked) else {
                return false;
            };
            let n = self.fresh("vt");
            self.emit(format!("        let mut {n}: Vec[Tracked] = Vec.new();"));
            self.emit(format!("        {n}.push({t});"));
            self.add_var_mut(n, Ty::VecTracked);
            true
        }

        /// Consume a `Vec[Tracked]` by owned for-loop: every element's Drop body
        /// must run exactly once as the loop takes it.
        fn for_owned_vec_tracked(&mut self) -> bool {
            let Some(v) = self.take(Ty::VecTracked) else {
                return false;
            };
            let e = self.fresh("et");
            self.emit(format!(
                "        for {e} in {v} {{ acc = acc + tracked_len({e}); }}"
            ));
            true
        }

        /// Borrow a live `Tracked` through a `ref` param. The callee must not
        /// drop it; the binding stays live and drops later, exactly once.
        fn ref_peek_tracked(&mut self) -> bool {
            let Some(v) = self.live_of(Ty::Tracked) else {
                return false;
            };
            self.emit(format!("        acc = acc + tracked_peek({});", v.name));
            true
        }

        /// Move two `Tracked` values into a `par {}` block. B-2026-07-31-41
        /// found user-Drop *timing* diverging between the default build and the
        /// auto-par lanes; B-2026-07-31-40 found a whole value leaking when it
        /// was introduced inside a parallel group.
        fn par_capture_tracked(&mut self) -> bool {
            if self.used_par {
                return false;
            }
            let Some(a) = self.take(Ty::Tracked) else {
                return false;
            };
            let Some(b) = self.take(Ty::Tracked) else {
                // Only one available — still consume it so the tag balances.
                self.emit(format!("        acc = acc + tracked_len({a});"));
                return true;
            };
            self.used_par = true;
            self.emit("        par {".to_string());
            self.emit(format!("            acc = acc + tracked_len({a});"));
            self.emit(format!("            acc = acc + tracked_len({b});"));
            self.emit("        }".to_string());
            true
        }

        // ── sinks: drain every remaining live binding into `acc` ──────────

        fn sink_all(&mut self) {
            // Snapshot then clear — each read consumes the binding.
            let scope = std::mem::take(&mut self.scope);
            for v in scope {
                let line = match v.ty {
                    Ty::Str => format!("        acc = acc + {}.len();", v.name),
                    Ty::VecStr => {
                        let e = self.fresh("e");
                        format!(
                            "        for {e} in {}.iter() {{ acc = acc + {e}.len(); }}",
                            v.name
                        )
                    }
                    Ty::Pair => format!("        acc = acc + {}.0 + {}.1.len();", v.name, v.name),
                    Ty::VecPair => {
                        let e = self.fresh("e");
                        format!(
                            "        for {e} in {}.iter() {{ acc = acc + {e}.0 + {e}.1.len(); }}",
                            v.name
                        )
                    }
                    Ty::Payload => {
                        format!(
                            "        acc = acc + {}.tag + {}.name.len();",
                            v.name, v.name
                        )
                    }
                    Ty::OptStr => {
                        let x = self.fresh("x");
                        format!(
                            "        match {} {{ Some({x}) => {{ acc = acc + {x}.len(); }}, None => {{}} }}",
                            v.name
                        )
                    }
                    Ty::VecVecStr => {
                        let inner = self.fresh("iv");
                        let e = self.fresh("e");
                        format!(
                            "        for {inner} in {}.iter() {{ for {e} in {inner}.iter() {{ acc = acc + {e}.len(); }} }}",
                            v.name
                        )
                    }
                    Ty::VecPayload => {
                        let p = self.fresh("p");
                        format!(
                            "        for {p} in {}.iter() {{ acc = acc + {p}.tag + {p}.name.len(); }}",
                            v.name
                        )
                    }
                    Ty::MapStr => format!("        acc = acc + {}.len();", v.name),
                    Ty::SetStr => format!("        acc = acc + {}.len();", v.name),
                    // Owned sinks for the Drop-carrying types: moving into
                    // `tracked_len` makes the callee the drop site, which is a
                    // different codegen path from an end-of-scope drop and is
                    // where several of the ledger's double-run bodies lived.
                    Ty::Tracked => format!("        acc = acc + tracked_len({});", v.name),
                    Ty::VecTracked => {
                        let e = self.fresh("et");
                        format!(
                            "        for {e} in {} {{ acc = acc + tracked_len({e}); }}",
                            v.name
                        )
                    }
                    Ty::OptTracked => {
                        let x = self.fresh("xt");
                        format!(
                            "        match {} {{ Some({x}) => {{ acc = acc + tracked_len({x}); }}, None => {{}} }}",
                            v.name
                        )
                    }
                    // Borrowed: the Crate itself drops at end of scope, which is
                    // what must cascade into its `item` field exactly once.
                    Ty::CrateT => format!(
                        "        acc = acc + {}.lid + tracked_peek({}.item);",
                        v.name, v.name
                    ),
                };
                self.emit(line);
            }
        }

        /// Build a full program from the seed and return its source text.
        fn build_program(&mut self) -> String {
            // A handful of producers up front so transforms have material.
            let seeds = 3 + self.rng.below(4);
            for _ in 0..seeds {
                self.produce_one();
            }

            let steps = 6 + self.rng.below(10);
            for _ in 0..steps {
                self.step_one();
            }

            self.sink_all();

            let body = self.body.join("\n");
            let preamble = PREAMBLE;
            format!("{preamble}\nfn main() {{\n    let mut acc: i64 = 0i64;\n    let mut round: i64 = 0i64;\n    while round < 40i64 {{\n{body}\n        round = round + 1i64;\n    }}\n    println(acc);\n}}\n")
        }

        fn produce_one(&mut self) {
            // The four Drop-carrying producers get roughly a third of the
            // weight. They are the only ones the drop-log oracle can judge, and
            // the displacement transforms all need one live to apply at all —
            // too thin a share and most programs never reach the new shapes.
            match self.rng.below(15) {
                0 => self.make_str(),
                1 => self.make_vecstr(),
                2 => self.make_pair(),
                3 => self.make_payload(),
                4 => self.make_optstr(),
                5 => self.make_indexed_vecstr(),
                6 => self.make_nested_vec(),
                7 => self.make_payload_vec(),
                8 => self.make_map(),
                9 => self.make_set(),
                10 => self.make_tree(),
                11 => self.make_tracked(),
                12 => self.make_vec_tracked(),
                13 => self.make_opt_tracked(),
                _ => self.make_crate(),
            }
        }

        fn step_one(&mut self) {
            // Try transforms in a random order until one applies; if none does
            // (nothing live of the needed type), produce fresh material.
            let mut order: [u8; 23] = [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
            ];
            // Fisher-Yates on the fixed array.
            for i in (1..order.len()).rev() {
                let j = self.rng.below(i + 1);
                order.swap(i, j);
            }
            for &choice in &order {
                let applied = match choice {
                    0 => self.move_str_into_vec(),
                    1 => self.for_owned_vecstr(),
                    2 => self.for_borrow_vecstr(),
                    3 => self.pair_into_vec(),
                    4 => self.destructure_payload(),
                    5 => self.match_optstr(),
                    6 => self.pass_owned_str(),
                    7 => self.roundtrip_vecstr(),
                    8 => self.par_capture(),
                    9 => self.spawn_capture_vecstr(),
                    10 => self.ref_peek_str(),
                    11 => self.mut_grow_vec(),
                    12 => self.conditional_move_str_into_vec(),
                    // ── displacement of a user-`impl Drop` value ──
                    13 => self.reassign_tracked(),
                    14 => self.reassign_crate_field(),
                    15 => self.reassign_opt_tracked(),
                    16 => self.tracked_into_opt_then_displace(),
                    17 => self.match_opt_tracked(),
                    18 => self.tracked_into_vec(),
                    19 => self.for_owned_vec_tracked(),
                    20 => self.ref_peek_tracked(),
                    21 => self.par_capture_tracked(),
                    _ => false, // slot 22: fall through to a producer
                };
                if applied {
                    return;
                }
            }
            self.produce_one();
        }
    }

    /// Fixed type / helper-fn preamble shared by every generated program.
    const PREAMBLE: &str = r#"struct Payload { tag: i64, name: String, items: Vec[String] }

shared struct Holder { s: String }

shared enum Tree { Leaf(String), Node(Tree) }

fn take_str(s: String) -> i64 { return s.len(); }

fn echo_vec(v: Vec[String]) -> Vec[String] { return v; }

fn hold_len(h: Holder) -> i64 { return h.s.len(); }

fn peek(s: ref String) -> i64 { return s.len(); }

fn grow(v: mut ref Vec[String]) {
    v.push("grow_appended_payload_kept_long_for_lsan_xx".to_string());
}

fn band(data: Vec[String], lo: i64) -> i64 {
    let mut acc: i64 = 0i64;
    for e in data.iter() { acc = acc + e.len() + lo; }
    return acc;
}

fn tree_len(t: Tree) -> i64 {
    match t {
        Leaf(s) => s.len(),
        Node(inner) => tree_len(inner),
    }
}

struct Tracked { tag: i64, name: String }

impl Drop for Tracked {
    fn drop(mut ref self) { println(f"D{self.tag}"); }
}

fn new_tracked(tag: i64, name: String) -> Tracked {
    println(f"N{tag}");
    return Tracked { tag: tag, name: name };
}

fn tracked_len(t: Tracked) -> i64 { return t.name.len(); }

fn tracked_peek(t: ref Tracked) -> i64 { return t.name.len(); }

struct Crate { lid: i64, item: Tracked }

fn new_crate(tag: i64, name: String) -> Crate {
    return Crate { lid: tag, item: new_tracked(tag, name) };
}"#;

    // ───────────────────────── compile + run under ASan ──────────────────

    /// The two build surfaces the spike targets.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Surface {
        Seq,
        AutoPar,
        /// The tree-walk interpreter — `karac run --interp`, and the oracle
        /// `karac build` is supposed to agree with. It compiles nothing and
        /// links no sanitizer, so it can only ever contribute a drop log; its
        /// value is entirely in the cross-surface comparison. `run-vs-build`
        /// is a 56-entry ledger class, and two of the drop bugs this widening
        /// targets (B-2026-07-31-38, -41) are in it — a divergence no
        /// AOT-only pair of surfaces can see by construction.
        Interp,
    }

    /// Every surface each generated program is run on.
    const SURFACES: [Surface; 3] = [Surface::Seq, Surface::AutoPar, Surface::Interp];

    impl Surface {
        fn tag(self) -> &'static str {
            match self {
                Surface::Seq => "seq",
                Surface::AutoPar => "autopar",
                Surface::Interp => "interp",
            }
        }
    }

    // ───────────────────────── the drop-log oracle ───────────────────────
    //
    // ASan and LSan judge *memory*. They are silent on a user `impl Drop` body
    // that runs the wrong number of times but touches no freed pointer — a body
    // that only prints is invisible to both. That is not a hypothetical: it is
    // most of what the ledger's recent drop bugs actually were (B-2026-07-31-37
    // ran a body twice, B-2026-07-31-38 never ran it at all), and it is why the
    // straight-line corpus could measure zero findings while those kept landing.
    //
    // The protocol is deliberately the weakest thing that still catches them.
    // `new_tracked(tag, …)` prints `N<tag>` before constructing; the `impl Drop`
    // body prints `D<tag>`. The invariant is per-tag **balance**: over the whole
    // run, every construction is matched by exactly one drop.
    //
    // Balance, rather than an expected schedule, is the point. It needs no model
    // of the generated control flow — the round loop, conditional moves, early
    // returns and par lanes all reuse a construction site an unpredictable
    // number of times, and any count-based expectation would have to re-derive
    // that. Balance holds regardless: N != D is a bug no matter how the program
    // got there. The cost is that it cannot see a *reordering* that keeps counts
    // intact, which is what the cross-surface comparison covers instead.
    #[derive(Clone, Default, PartialEq, Eq)]
    struct DropLog {
        /// tag → (times constructed, times the Drop body ran)
        counts: BTreeMap<i64, (u64, u64)>,
    }

    impl DropLog {
        fn parse(stdout: &str) -> DropLog {
            let mut counts: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
            for line in stdout.lines() {
                let line = line.trim();
                let mut chars = line.chars();
                let kind = chars.next();
                if !matches!(kind, Some('N') | Some('D')) {
                    continue;
                }
                // The rest must be exactly a tag. `parse` rejects anything else,
                // which is what discards a torn par-lane write rather than
                // letting it count as a token.
                let Ok(tag) = chars.as_str().parse::<i64>() else {
                    continue;
                };
                let e = counts.entry(tag).or_insert((0, 0));
                if kind == Some('N') {
                    e.0 += 1;
                } else {
                    e.1 += 1;
                }
            }
            DropLog { counts }
        }

        fn constructions(&self) -> u64 {
            self.counts.values().map(|(n, _)| *n).sum()
        }

        /// Tags whose construct / drop counts disagree, worst first.
        fn imbalances(&self) -> Vec<(i64, u64, u64)> {
            let mut out: Vec<(i64, u64, u64)> = self
                .counts
                .iter()
                .filter(|(_, (n, d))| n != d)
                .map(|(t, (n, d))| (*t, *n, *d))
                .collect();
            out.sort_by_key(|(t, n, d)| (-((*n as i64 - *d as i64).abs()), *t));
            out
        }

        /// A stable signature fragment: which *direction* each bad tag went.
        /// Buckets "body ran twice" apart from "body never ran" without
        /// splitting the corpus on the tag number or the exact count.
        fn imbalance_kind(&self) -> &'static str {
            let bad = self.imbalances();
            let over = bad.iter().any(|(_, n, d)| d > n);
            let under = bad.iter().any(|(_, n, d)| n > d);
            match (over, under) {
                (true, true) => "drop-imbalance-both",
                (true, false) => "drop-ran-too-often",
                (false, true) => "drop-never-ran",
                (false, false) => "drop-balanced",
            }
        }

        fn detail(&self) -> String {
            let bad = self.imbalances();
            let mut s = String::from("user `impl Drop` body count != construction count\n");
            for (tag, n, d) in bad.iter().take(6) {
                s.push_str(&format!("  tag {tag}: constructed {n}x, dropped {d}x\n"));
            }
            s
        }
    }

    /// Outcome of compiling+running one program on one surface.
    enum Outcome {
        /// Program did not parse / typecheck / ownership-check cleanly, or
        /// codegen/link failed — uninteresting, discarded (not a finding).
        Invalid(&'static str),
        /// Compiled, linked, ran, and exited cleanly under ASan+LSan, with a
        /// balanced drop log. Carries the log so the driver can compare it
        /// across surfaces.
        Clean { log: DropLog },
        /// Sanitizer (or a crash) flagged a memory error. `signature` is the
        /// bucket key; `detail` is a short excerpt of the sanitizer report.
        Finding { signature: String, detail: String },
    }

    struct Runner {
        workdir: PathBuf,
        counter: std::cell::Cell<u64>,
    }

    impl Runner {
        fn new(workdir: PathBuf) -> Self {
            std::fs::create_dir_all(&workdir).ok();
            Runner {
                workdir,
                counter: std::cell::Cell::new(0),
            }
        }

        fn run(&self, src: &str, surface: Surface) -> Outcome {
            use karac::codegen::{compile_to_object, link_executable_with_sanitizer};

            let mut parsed = karac::parse(src);
            if !parsed.errors.is_empty() {
                return Outcome::Invalid("parse");
            }
            let resolved = karac::resolve(&parsed.program);
            let typed = karac::typecheck(&parsed.program, &resolved);
            if !typed.errors.is_empty() {
                return Outcome::Invalid("typecheck");
            }
            karac::lower(&mut parsed.program, &typed);
            let ownership = karac::ownershipcheck(&parsed.program, &typed);
            if !ownership.errors.is_empty() {
                // `karac check` would reject it — a buggy *program*, not a
                // codegen drop bug. Discard so findings stay attributable.
                return Outcome::Invalid("ownership");
            }

            // The interpreter forks off here: no object, no link, no sanitizer.
            // `captured_output` is the in-process equivalent of piping stdout.
            //
            // `catch_unwind` because a tree-walk gap is a `panic!`, not a
            // `RuntimeError`, and one generated program hitting an unsupported
            // construct must not take the whole batch down. Anything that
            // panics or errors is Invalid, not a finding: the interpreter has
            // its own feature gaps, and reporting those as run-vs-build
            // divergence would bury the real ones.
            if surface == Surface::Interp {
                let program = &parsed.program;
                let typed_ref = &typed;
                let captured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut interp = karac::interpreter::Interpreter::new(program, typed_ref);
                    interp.captured_output = Some(Vec::new());
                    interp.run();
                    (
                        interp.captured_output.take().unwrap_or_default(),
                        interp.runtime_errors.is_empty(),
                    )
                }));
                return match captured {
                    Err(_) => Outcome::Invalid("interp-panic"),
                    Ok((_, false)) => Outcome::Invalid("interp-runtime-error"),
                    Ok((lines, true)) => {
                        let log = DropLog::parse(&lines.join("\n"));
                        if log.imbalances().is_empty() {
                            Outcome::Clean { log }
                        } else {
                            Outcome::Finding {
                                signature: format!("interp:{}", log.imbalance_kind()),
                                detail: log.detail(),
                            }
                        }
                    }
                };
            }

            let concurrency = match surface {
                Surface::AutoPar => {
                    let effects = karac::effectcheck(&parsed.program);
                    Some(karac::concurrency_analyze(&parsed.program, &effects))
                }
                Surface::Seq | Surface::Interp => None,
            };

            let id = self.counter.get();
            self.counter.set(id + 1);
            let obj = self
                .workdir
                .join(format!("df_{}_{}.o", std::process::id(), id));
            let exe = self
                .workdir
                .join(format!("df_{}_{}", std::process::id(), id));

            let obj_s = obj.to_string_lossy().to_string();
            let exe_s = exe.to_string_lossy().to_string();

            if compile_to_object(
                &parsed.program,
                &obj_s,
                Some(&ownership),
                concurrency.as_ref(),
            )
            .is_err()
            {
                return Outcome::Invalid("codegen");
            }
            if !obj.exists() {
                return Outcome::Invalid("codegen-noobj");
            }
            if link_executable_with_sanitizer(&obj_s, &exe_s, &["-fsanitize=address"]).is_err() {
                let _ = std::fs::remove_file(&obj);
                return Outcome::Invalid("link");
            }

            // Steady-state leak detection on (Linux LSan). abort_on_error=0 so
            // ASan returns exitcode=23 rather than SIGABRT — a distinguishable
            // signal. detect_leaks=1 catches the leak arm.
            let output = run_with_watchdog(
                &exe_s,
                &[(
                    "ASAN_OPTIONS",
                    "detect_leaks=1:abort_on_error=0:exitcode=23:log_threads=0",
                )],
                Duration::from_secs(20),
            );

            let _ = std::fs::remove_file(&obj);
            let _ = std::fs::remove_file(&exe);

            match output {
                None => Outcome::Invalid("hang"),
                Some((code, stderr, stdout)) => classify(code, &stderr, &stdout, surface),
            }
        }
    }

    /// Classify a run result into Clean / Finding. ASan's `exitcode=23`, or a
    /// double-free SIGABRT/SIGTRAP (134/133), or a SEGV (139) is a finding.
    ///
    /// A clean exit is no longer automatically `Clean`: the drop log still has
    /// to balance. The sanitizer arm takes precedence when both fire — an ASan
    /// report names the offending allocation, which is strictly more actionable
    /// than "this tag dropped twice".
    fn classify(code: Option<i32>, stderr: &str, stdout: &str, surface: Surface) -> Outcome {
        // Extract the canonical ASan error kind if present.
        let kind = asan_error_kind(stderr);
        match code {
            Some(0) => {
                let log = DropLog::parse(stdout);
                if log.imbalances().is_empty() {
                    Outcome::Clean { log }
                } else {
                    Outcome::Finding {
                        signature: format!("{}:{}", surface.tag(), log.imbalance_kind()),
                        detail: log.detail(),
                    }
                }
            }
            None if kind.is_some() => finding(kind.unwrap(), stderr, surface),
            // Killed / exited-by-signal with no ASan report. Not a finding, but
            // the drop log is untrustworthy (the process died mid-run), so hand
            // back an empty one rather than a partial count the cross-surface
            // comparison would read as a divergence.
            None => Outcome::Clean {
                log: DropLog::default(),
            },
            Some(23) if kind.is_some() => finding(kind.unwrap(), stderr, surface),
            Some(23) => finding("asan-error", stderr, surface),
            Some(134) => finding(kind.unwrap_or("abort-sigabrt"), stderr, surface),
            Some(133) => finding(kind.unwrap_or("trap-sigtrap"), stderr, surface),
            Some(139) => finding(kind.unwrap_or("segv"), stderr, surface),
            Some(other) => {
                if let Some(k) = kind {
                    finding(k, stderr, surface)
                } else {
                    // Non-zero exit with no sanitizer report: a plain runtime
                    // panic (bounds check, etc.) from generated arithmetic —
                    // not a drop bug. Bucket loosely but keep it visible.
                    finding(&format!("exit-{other}"), stderr, surface)
                }
            }
        }
    }

    fn asan_error_kind(stderr: &str) -> Option<&'static str> {
        // Order matters — the more specific double-free string appears within a
        // generic "attempting free" report on some libc paths.
        const PATS: &[(&str, &str)] = &[
            ("attempting double-free", "double-free"),
            ("double-free", "double-free"),
            ("heap-use-after-free", "heap-use-after-free"),
            (
                "attempting free on address which was not malloc",
                "bad-free",
            ),
            ("detected memory leaks", "memory-leak"),
            ("heap-buffer-overflow", "heap-buffer-overflow"),
            ("stack-buffer-overflow", "stack-buffer-overflow"),
            ("SEGV on unknown address", "segv"),
        ];
        for (needle, kind) in PATS {
            if stderr.contains(needle) {
                return Some(kind);
            }
        }
        None
    }

    fn finding(kind: &str, stderr: &str, surface: Surface) -> Outcome {
        // Signature = surface + error kind. Keeps the corpus bucketed by class
        // and surface without over-splitting on addresses/line numbers.
        let signature = format!("{}:{}", surface.tag(), kind);
        // A short, address-scrubbed excerpt for the report.
        let detail = stderr
            .lines()
            .filter(|l| {
                l.contains("ERROR")
                    || l.contains("SUMMARY")
                    || l.contains("freed by")
                    || l.contains("allocated by")
                    || l.contains("leak")
            })
            .take(6)
            .collect::<Vec<_>>()
            .join("\n");
        Outcome::Finding { signature, detail }
    }

    /// Human-readable per-tag delta between two surfaces' drop logs.
    fn droplog_diff(a: &DropLog, b: &DropLog) -> String {
        let mut tags: Vec<i64> = a.counts.keys().chain(b.counts.keys()).copied().collect();
        tags.sort_unstable();
        tags.dedup();
        let mut s = String::new();
        for tag in tags {
            let x = a.counts.get(&tag).copied().unwrap_or((0, 0));
            let y = b.counts.get(&tag).copied().unwrap_or((0, 0));
            if x != y {
                s.push_str(&format!(
                    "  tag {tag}: {}x new/{}x drop vs {}x new/{}x drop\n",
                    x.0, x.1, y.0, y.1
                ));
            }
        }
        s
    }

    /// Bucket a finding by signature, log it, and (first of its signature, or
    /// always under `--keep-going`) shrink and retain it.
    #[allow(clippy::too_many_arguments)]
    fn record_finding(
        cfg: &Config,
        runner: &Runner,
        src: &str,
        seed: u64,
        signature: String,
        detail: String,
        surface: Surface,
        seen_sigs: &mut BTreeMap<String, u64>,
        findings: &mut Vec<Finding>,
    ) {
        let count = seen_sigs.entry(signature.clone()).or_insert(0);
        *count += 1;
        let first_of_sig = *count == 1;
        eprintln!(
            "  [FINDING] seed={seed} sig={signature}\n{}",
            indent(&detail, "      ")
        );

        // A pinned known-open signature keeps exactly one unshrunk exemplar,
        // even under `--keep-going`. Two reasons, both about the nightly's time
        // budget rather than tidiness: B-2026-07-30-11 alone fires on ~62% of
        // executions, and the shrinker re-compiles and re-runs the program once
        // per candidate line — so shrinking every hit would mean hundreds of
        // delta-debug passes per run, blowing the 90-minute CI cap to re-derive
        // a minimal repro the ledger entry already records. One exemplar is
        // enough to confirm the pin still describes something real.
        if known_open_for(&signature).is_some() {
            if first_of_sig {
                findings.push(Finding {
                    seed,
                    signature,
                    detail,
                    src: src.to_string(),
                });
            }
            return;
        }

        if first_of_sig || cfg.keep_going {
            let final_src = if cfg.shrink {
                shrink(runner, src, &signature, surface)
            } else {
                src.to_string()
            };
            findings.push(Finding {
                seed,
                signature,
                detail,
                src: final_src,
            });
        }
    }

    /// Run `exe` with a wall-clock watchdog; return `(exit_code, stderr, stdout)`
    /// or `None` if it had to be killed (hang). exit_code is `None` on signal.
    ///
    /// stdout used to be `Stdio::null()` — the sanitizer was the only judge, and
    /// it speaks on stderr. It is piped now because the `N<tag>` / `D<tag>` drop
    /// tokens the generated programs print are the drop-log oracle's entire
    /// input; see `DropLog`.
    fn run_with_watchdog(
        exe: &str,
        envs: &[(&str, &str)],
        timeout: Duration,
    ) -> Option<(Option<i32>, String, String)> {
        use std::sync::mpsc;
        let mut cmd = Command::new(exe);
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        // Bound the auto-par pool so a fuzz batch does not oversubscribe.
        cmd.env("KARAC_PAR_WORKERS", "2");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().ok()?;
        let pid = child.id();
        // Both pipes must be drained on their own threads to avoid a full-pipe
        // stall — and stdout now carries hundreds of token lines per program,
        // so it fills far faster than stderr ever did.
        let mut stderr_pipe = child.stderr.take();
        let mut stdout_pipe = child.stdout.take();
        let (tx, rx) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if rx.recv_timeout(timeout).is_err() {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                true
            } else {
                false
            }
        });
        let stderr_handle = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(ref mut p) = stderr_pipe {
                let _ = p.read_to_string(&mut buf);
            }
            buf
        });
        let stdout_handle = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            if let Some(ref mut p) = stdout_pipe {
                let _ = p.read_to_end(&mut buf);
            }
            // Lossy: a par-lane interleave can split a UTF-8 sequence across
            // two writes. Token lines are pure ASCII, so a mangled byte can
            // only corrupt a line the parser then ignores.
            String::from_utf8_lossy(&buf).into_owned()
        });
        let status = child.wait().ok();
        let _ = tx.send(());
        let killed = watchdog.join().unwrap_or(false);
        let stderr = stderr_handle.join().unwrap_or_default();
        let stdout = stdout_handle.join().unwrap_or_default();
        if killed {
            return None;
        }
        let status = status?;
        #[cfg(unix)]
        let code = {
            use std::os::unix::process::ExitStatusExt;
            status.code().or_else(|| status.signal().map(|s| 128 + s))
        };
        #[cfg(not(unix))]
        let code = status.code();
        Some((code, stderr, stdout))
    }

    // ───────────────────────── the shrinker ──────────────────────────────

    /// Line-based delta debug: repeatedly try deleting body lines and keep any
    /// deletion that preserves the *same signature* on the *same surface*.
    /// The generated body is one statement per line, so this reduces a failing
    /// program to a minimal, kata-sized repro. Helper fns / preamble are left
    /// intact (they are shared scaffolding, cheap to keep).
    fn shrink(runner: &Runner, src: &str, signature: &str, surface: Surface) -> String {
        let mut current = src.to_string();
        let mut changed = true;
        let mut rounds = 0;
        while changed && rounds < 200 {
            changed = false;
            rounds += 1;
            let lines: Vec<&str> = current.lines().collect();
            for i in 0..lines.len() {
                let l = lines[i].trim_start();
                // Only try to drop interior body statements — never the `fn
                // main`, the `while`, the closing braces, the `println(acc)`,
                // or preamble decls, or the result stops compiling trivially.
                if !is_droppable(l) {
                    continue;
                }
                let candidate: String = lines
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, s)| *s)
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Outcome::Finding { signature: sig, .. } = runner.run(&candidate, surface) {
                    if sig == signature {
                        current = candidate;
                        changed = true;
                        break; // re-scan from the top with the smaller program
                    }
                }
            }
        }
        current
    }

    fn is_droppable(trimmed: &str) -> bool {
        // Body statements the shrinker may try to delete. Deliberately
        // conservative: skips control-flow headers and structural lines.
        if trimmed.is_empty() {
            return false;
        }
        let structural = trimmed.starts_with("fn ")
            || trimmed.starts_with("struct ")
            || trimmed.starts_with("shared ")
            || trimmed.starts_with("while ")
            || trimmed.starts_with("par {")
            || trimmed == "}"
            || trimmed.starts_with("println(acc)")
            || trimmed.starts_with("round = round")
            || trimmed.starts_with("let mut acc")
            || trimmed.starts_with("let mut round")
            || trimmed.starts_with("return ");
        !structural
    }

    // ───────────────────────── driver / reporting ────────────────────────

    struct Config {
        count: u64,
        seed: u64,
        out: PathBuf,
        shrink: bool,
        keep_going: bool,
        verbose: bool,
        /// Run ONLY the ownership oracle (Slice 3 model self-check), skipping
        /// the ASan/LSan compile+run entirely. Fast model coverage over many
        /// programs — no runtime archives or `cc` needed.
        oracle_only: bool,
        /// Run the oracle↔codegen **differential** (Slice 4 down-payment):
        /// compare the oracle's drop schedule against the drops codegen
        /// actually emits (via the `codegen::drop_obs` recorder + in-process
        /// `compile_to_ir`). Flags a *missing drop* — the oracle schedules a
        /// drop codegen never emitted a cleanup action for → a real leak,
        /// localized to `(function, place)`. Needs LLVM (the bin is
        /// `--features llvm`) but NOT the runtime archives or `cc` — no linking
        /// or execution happens. Mutually exclusive with the ASan run.
        differential: bool,
    }

    fn parse_args() -> Config {
        let mut cfg = Config {
            count: 200,
            seed: 1,
            out: PathBuf::from("target/drop-fuzz"),
            shrink: true,
            keep_going: false,
            verbose: false,
            oracle_only: false,
            differential: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--count" => {
                    cfg.count = args
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(cfg.count)
                }
                "--seed" => cfg.seed = args.next().and_then(|s| s.parse().ok()).unwrap_or(cfg.seed),
                "--out" => {
                    if let Some(s) = args.next() {
                        cfg.out = PathBuf::from(s);
                    }
                }
                "--dump" => {
                    // Debug aid: print the program for `--seed S` and exit.
                    let src = Gen::new(cfg.seed).build_program();
                    println!("{src}");
                    std::process::exit(0);
                }
                "--explain" => {
                    // Debug aid: per-function oracle-vs-codegen drop sets for
                    // one seed (default `--seed`). Prints the local drop
                    // schedule, codegen's emitted set, and any missing drops.
                    let s = args.next().and_then(|x| x.parse().ok()).unwrap_or(cfg.seed);
                    let src = Gen::new(s).build_program();
                    differential_explain(s, &src);
                    std::process::exit(0);
                }
                "--no-shrink" => cfg.shrink = false,
                "--keep-going" => cfg.keep_going = true,
                "--oracle-only" => cfg.oracle_only = true,
                "--differential" => cfg.differential = true,
                "--verbose" => cfg.verbose = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    eprintln!("drop_fuzz: unknown arg {other:?} (try --help)");
                    std::process::exit(2);
                }
            }
        }
        cfg
    }

    fn print_help() {
        println!(
            "drop_fuzz — the drop-soundness fuzzer (ownership-model-mechanization spike, Slice 1)\n\n\
             USAGE: cargo run --features llvm --bin drop_fuzz -- [options]\n\n\
             OPTIONS:\n\
             \x20 --count N      programs to generate (default 200)\n\
             \x20 --seed S       base seed; program k uses seed S+k (default 1)\n\
             \x20 --out DIR      write shrunk repros + report.md here (default target/drop-fuzz)\n\
             \x20 --no-shrink    skip the shrinker\n\
             \x20 --keep-going   keep saving repros past the first of each signature\n\
             \x20 --oracle-only  run ONLY the ownership oracle (Slice 3 model self-check);\n\
             \x20                no compile/ASan — fast model coverage, no toolchain needed\n\
             \x20 --differential run the oracle↔codegen differential (Slice 4 down-payment):\n\
             \x20                oracle drop schedule vs codegen's emitted drops. Flags a\n\
             \x20                missing drop (leak) localized to (function, place). Needs\n\
             \x20                LLVM but no runtime archives / cc — no linking or execution\n\
             \x20 --verbose      per-program progress\n"
        );
    }

    struct Finding {
        seed: u64,
        signature: String,
        detail: String,
        src: String,
    }

    pub fn run() {
        let cfg = parse_args();
        if cfg.differential {
            run_differential(&cfg);
            return;
        }
        std::fs::create_dir_all(&cfg.out).ok();
        // Transient object/exe scratch lives in the system temp dir, NOT under
        // `--out` — so a committed corpus directory stays clean (repros + report
        // only). Each `.o`/exe is removed right after its run regardless.
        let work = std::env::temp_dir().join(format!("drop_fuzz_work_{}", std::process::id()));
        let runner = Runner::new(work);

        let start = Instant::now();
        let mut valid = 0u64; // programs that compiled+ran on ≥1 surface
        let mut runs = 0u64; // total (program, surface) executions that were valid
        let mut findings: Vec<Finding> = Vec::new();
        let mut seen_sigs: BTreeMap<String, u64> = BTreeMap::new();
        // Non-vacuity counter for the drop-log oracle: how many user-`impl Drop`
        // values the corpus actually constructed across all clean runs. If this
        // comes back at or near zero the grammar never reached the Drop-carrying
        // shapes, and a clean report says nothing about them — the exact failure
        // mode that let the pre-widening corpus report 0 findings.
        let mut drop_tokens = 0u64;
        // Slice 3 — ownership-oracle model self-check across the corpus.
        let mut oracle_programs = 0u64;
        let mut oracle_drops = 0u64;
        let mut oracle_violations = 0u64;

        eprintln!(
            "drop_fuzz: generating {} programs (seed base {}), out={}{}",
            cfg.count,
            cfg.seed,
            cfg.out.display(),
            if cfg.oracle_only {
                " [oracle-only]"
            } else {
                ""
            }
        );

        for k in 0..cfg.count {
            let seed = cfg.seed.wrapping_add(k);
            let src = Gen::new(seed).build_program();
            let mut any_valid = false;

            // ── Ownership oracle (Slice 3): run the executable judgment on
            //    every generated program. The generator only emits ownership-
            //    clean programs, so the model MUST report zero invariant
            //    violations — a violation means the model and the generator
            //    disagree (an oracle bug or a checker gap), surfaced loudly.
            {
                let parsed = karac::parse(&src);
                if parsed.errors.is_empty() {
                    let res = karac::ownership_oracle::analyze(&parsed.program);
                    oracle_programs += 1;
                    oracle_drops += res.drop_count() as u64;
                    for v in res.violations() {
                        oracle_violations += 1;
                        eprintln!(
                            "  [ORACLE] seed={seed} {:?} on `{}`: {} (line {})",
                            v.kind, v.place, v.message, v.span.line
                        );
                    }
                }
            }

            if cfg.oracle_only {
                if cfg.verbose && (k + 1) % 50 == 0 {
                    eprintln!(
                        "  .. {}/{} analyzed, {oracle_violations} oracle violation(s)",
                        k + 1,
                        cfg.count
                    );
                }
                continue;
            }

            // Clean logs, kept for the cross-surface comparison below.
            let mut clean_logs: Vec<(Surface, DropLog)> = Vec::new();

            for surface in SURFACES {
                match runner.run(&src, surface) {
                    Outcome::Invalid(_reason) => {}
                    Outcome::Clean { log } => {
                        any_valid = true;
                        runs += 1;
                        drop_tokens += log.constructions();
                        clean_logs.push((surface, log));
                    }
                    Outcome::Finding { signature, detail } => {
                        any_valid = true;
                        runs += 1;
                        record_finding(
                            &cfg,
                            &runner,
                            &src,
                            seed,
                            signature,
                            detail,
                            surface,
                            &mut seen_sigs,
                            &mut findings,
                        );
                    }
                }
            }

            // ── cross-surface drop-log comparison ──
            //
            // Each surface having a self-consistent log does not make them
            // agree with each other. A value dropped once under the sequential
            // build and once inside an auto-par lane balances on both sides
            // while the *programs* disagree — B-2026-07-31-41's shape, and the
            // reason the per-surface balance check is not the whole oracle.
            //
            // Only counts are compared, never order: par lanes may legitimately
            // interleave. A divergence is confirmed by re-running the diverging
            // surface, because a torn write from two lanes printing at once
            // would otherwise read as a real disagreement.
            if clean_logs.len() > 1 {
                let (base_surface, base) = &clean_logs[0];
                for (other_surface, other) in &clean_logs[1..] {
                    if other == base {
                        continue;
                    }
                    let Outcome::Clean { log: confirm } = runner.run(&src, *other_surface) else {
                        continue; // the re-run did not reproduce cleanly — drop it
                    };
                    if &confirm == base {
                        continue; // first read was a torn write, not a divergence
                    }
                    let detail = format!(
                        "user `impl Drop` counts differ between build surfaces\n{}",
                        droplog_diff(base, &confirm)
                    );
                    record_finding(
                        &cfg,
                        &runner,
                        &src,
                        seed,
                        format!(
                            "{}-vs-{}:droplog-divergence",
                            base_surface.tag(),
                            other_surface.tag()
                        ),
                        detail,
                        *other_surface,
                        &mut seen_sigs,
                        &mut findings,
                    );
                }
            }
            if any_valid {
                valid += 1;
            }
            if cfg.verbose && (k + 1) % 20 == 0 {
                eprintln!(
                    "  .. {}/{} generated, {valid} valid, {} finding(s)",
                    k + 1,
                    cfg.count,
                    findings.len()
                );
            }
        }

        let elapsed = start.elapsed();
        let oracle = OracleStats {
            programs: oracle_programs,
            drops: oracle_drops,
            violations: oracle_violations,
        };
        write_report(
            &cfg, &findings, &seen_sigs, valid, runs, cfg.count, elapsed, &oracle,
        );
        print_summary(
            &findings, &seen_sigs, valid, runs, cfg.count, elapsed, &cfg.out, &oracle,
        );
        // Non-vacuity line for the drop-log oracle. A clean report with zero
        // constructions means the corpus never built a Drop-carrying value, so
        // it cleared nothing about that class — say so rather than let the
        // green summary imply coverage the run did not have.
        if drop_tokens == 0 {
            eprintln!(
                "  drop-log oracle: NO user-`impl Drop` values were constructed — \
                 this run says nothing about the user-drop class"
            );
        } else {
            eprintln!(
                "  drop-log oracle: {drop_tokens} construction(s) observed on runs \
                 that balanced (imbalanced runs are listed above)"
            );
        }

        // Known-open pins: report which fired, and flag any that did not so a
        // fixed bug forces its stale pin out instead of leaving the gate
        // disarmed for that signature kind indefinitely.
        for (kind, ledger_id) in KNOWN_OPEN {
            let hits: u64 = seen_sigs
                .iter()
                .filter(|(s, _)| s.rsplit(':').next().unwrap_or(s) == *kind)
                .map(|(_, n)| *n)
                .sum();
            if hits > 0 {
                eprintln!(
                    "  known-open: {kind} fired {hits}x — explained by {ledger_id}, not gating"
                );
            } else if cfg.count >= 20 {
                eprintln!(
                    "  known-open: {kind} did NOT fire in {} programs — if {ledger_id} is fixed, \
                     drop it from KNOWN_OPEN so the gate covers this kind again",
                    cfg.count
                );
            }
        }

        // Exit non-zero if any *memory-safety* signature fired OR the model
        // self-check found an invariant violation, so the harness can gate.
        // Plain `exit-N` runtime panics do not gate, and neither do the
        // KNOWN_OPEN pins (reported above, tracked in the ledger).
        let mem_findings = seen_sigs
            .keys()
            .filter(|s| is_memory_signature(s) && known_open_for(s).is_none())
            .count();
        if mem_findings > 0 || oracle_violations > 0 {
            std::process::exit(1);
        }
    }

    // ───────────────── oracle↔codegen differential (Slice 4) ───────────────

    /// One place where codegen's emitted drop set diverges from the oracle's
    /// schedule. Currently only the **missing-drop** direction (leak risk): the
    /// oracle schedules a drop for `place` in `function`, but codegen emitted no
    /// cleanup action for it. The extra-drop (double-free) direction is not
    /// checked here — codegen frequently neutralizes a moved-out value's drop by
    /// a runtime null/cap guard while *keeping* the cleanup action, so at
    /// emit-time a guarded no-op is indistinguishable from a real free; the
    /// ASan/LSan run (the default mode) remains the double-free authority.
    struct DiffFinding {
        seed: u64,
        function: String,
        place: String,
        src: String,
    }

    /// `--explain S` triage: print, per function, the oracle's local drop
    /// schedule, codegen's emitted drop set, and the missing drops — the
    /// per-seed detail behind a `--differential` divergence.
    fn differential_explain(seed: u64, src: &str) {
        use std::collections::{BTreeMap, BTreeSet};

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            eprintln!("seed {seed}: parse error");
            return;
        }
        let resolved = karac::resolve(&parsed.program);
        let typed = karac::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            eprintln!("seed {seed}: typecheck error");
            return;
        }
        let oracle = karac::ownership_oracle::analyze(&parsed.program);
        let params = karac::drop_differential::param_names_by_function(&parsed.program);
        karac::lower(&mut parsed.program, &typed);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        if !ownership.errors.is_empty() {
            eprintln!("seed {seed}: ownership error");
            return;
        }
        karac::codegen::drop_obs::begin();
        let _ = karac::codegen::compile_to_ir(&parsed.program, Some(&ownership), None);
        let recs = karac::codegen::drop_obs::take();
        let mut cg: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for r in &recs {
            cg.entry(r.function.clone())
                .or_default()
                .insert(r.place.clone());
        }
        let empty = std::collections::HashSet::new();
        println!("seed {seed}: per-function drop sets (locals only)\n");
        for f in &oracle.functions {
            let fn_params = params.get(&f.function).unwrap_or(&empty);
            let scheduled: BTreeSet<&str> = f
                .drops
                .iter()
                .map(|d| d.place.as_str())
                .filter(|p| !fn_params.contains(*p))
                .collect();
            let emitted = cg.get(&f.function).cloned().unwrap_or_default();
            let missing: Vec<&str> = scheduled
                .iter()
                .copied()
                .filter(|p| !emitted.contains(*p))
                .collect();
            if scheduled.is_empty() && emitted.is_empty() {
                continue;
            }
            println!("  fn {}", f.function);
            println!("    oracle local drops : {scheduled:?}");
            println!("    codegen emitted    : {emitted:?}");
            println!("    MISSING            : {missing:?}");
        }
    }

    /// `--differential` mode entry: run [`differential_check`] over the corpus,
    /// report, and exit nonzero on any divergence (so it can gate).
    fn run_differential(cfg: &Config) {
        std::fs::create_dir_all(&cfg.out).ok();
        let start = Instant::now();
        let mut programs = 0u64;
        let mut drops_checked = 0u64;
        let mut skipped_capture = 0u64;
        let mut findings: Vec<DiffFinding> = Vec::new();

        eprintln!(
            "drop_fuzz: differential over {} programs (seed base {}) — oracle drop schedule vs \
             codegen emitted drops [seq surface]",
            cfg.count, cfg.seed
        );

        for k in 0..cfg.count {
            let seed = cfg.seed.wrapping_add(k);
            let src = Gen::new(seed).build_program();
            match karac::drop_differential::differential_check(&src) {
                // §7 open edge: the oracle keeps a spawn/par-captured heap value
                // Owned (conservative Read-role closure walk) and schedules a
                // drop codegen elides (the task frees it). Documented
                // model-conservatism, not a leak — out of the heap-core gate.
                // Counted, never silently dropped.
                DiffOutcome::CaptureEdge => skipped_capture += 1,
                DiffOutcome::Invalid => {}
                DiffOutcome::Checked {
                    drops_checked: dc,
                    divergences,
                } => {
                    programs += 1;
                    drops_checked += dc as u64;
                    for d in divergences {
                        eprintln!(
                            "  [DIFF] seed={seed} fn={} place=`{}` — oracle schedules a drop \
                             codegen emitted no cleanup for (leak risk)",
                            d.function, d.place
                        );
                        findings.push(DiffFinding {
                            seed,
                            function: d.function,
                            place: d.place,
                            src: src.clone(),
                        });
                    }
                }
            }
            if cfg.verbose && (k + 1) % 50 == 0 {
                eprintln!(
                    "  .. {}/{} checked, {} divergence(s)",
                    k + 1,
                    cfg.count,
                    findings.len()
                );
            }
        }

        let elapsed = start.elapsed();
        write_differential_report(
            cfg,
            programs,
            drops_checked,
            skipped_capture,
            &findings,
            elapsed,
        );

        eprintln!("\n── differential summary ──");
        eprintln!("  programs checked : {programs}");
        eprintln!("  drops checked    : {drops_checked}");
        eprintln!("  skipped (§7 capture edge) : {skipped_capture}");
        eprintln!("  divergences      : {}", findings.len());
        eprintln!("  elapsed          : {:.1}s", elapsed.as_secs_f64());
        if findings.is_empty() {
            eprintln!(
                "  ✓ on every function, codegen's emitted drop set covers the oracle's schedule."
            );
        } else {
            eprintln!("  report: {}/differential.md", cfg.out.display());
            std::process::exit(1);
        }
    }

    fn write_differential_report(
        cfg: &Config,
        programs: u64,
        drops_checked: u64,
        skipped_capture: u64,
        findings: &[DiffFinding],
        elapsed: Duration,
    ) {
        let mut md = String::new();
        md.push_str("# drop_fuzz — oracle↔codegen differential\n\n");
        md.push_str(&format!(
            "The ownership oracle's per-function drop schedule (`ownership_oracle::analyze`) \
             checked against the drops codegen actually emits (recorded via \
             `codegen::drop_obs`, seq surface). A divergence is a **missing drop**: the oracle \
             schedules a drop codegen emitted no cleanup action for → a leak, localized to \
             `(function, place)`.\n\n\
             - programs checked: **{programs}**\n\
             - scheduled drops checked: **{drops_checked}**\n\
             - skipped (§7 closure/par capture edge): **{skipped_capture}**\n\
             - divergences: **{}**\n\
             - base seed: `{}`\n\
             - elapsed: {:.1}s\n\n",
            findings.len(),
            cfg.seed,
            elapsed.as_secs_f64()
        ));
        if findings.is_empty() {
            md.push_str(
                "_No divergences: codegen's emitted drop set covers the oracle's schedule on \
                 every function of every checked program._\n",
            );
        } else {
            md.push_str("## Divergences\n\n| seed | function | place |\n|---|---|---|\n");
            for f in findings {
                md.push_str(&format!(
                    "| {} | `{}` | `{}` |\n",
                    f.seed, f.function, f.place
                ));
            }
            md.push_str("\n## Repros\n\n");
            for f in findings {
                md.push_str(&format!(
                    "### seed {} — `{}` / `{}`\n\n```rust\n{}\n```\n\n",
                    f.seed, f.function, f.place, f.src
                ));
            }
        }
        let _ = std::fs::write(cfg.out.join("differential.md"), md);
    }

    /// Slice-3 ownership-oracle self-check stats across the corpus.
    struct OracleStats {
        programs: u64,
        drops: u64,
        violations: u64,
    }

    /// Signature kinds a currently-**open** ledger bug already explains.
    ///
    /// The drop-log oracle went live while B-2026-07-30-11 was open, and that
    /// bug fires on roughly 60% of generated programs — every displaced or
    /// overwritten user-`impl Drop` value, from four independent directions
    /// (`o = None`, `o = Some(..)`, `c.field = ..`, and a match arm binding
    /// that receives a moved payload). Gating on it would make the nightly
    /// permanently red and teach everyone to ignore it.
    ///
    /// Pinning rather than narrowing the grammar is deliberate: dropping those
    /// shapes would route around a known bug and quietly un-cover the class the
    /// moment it is fixed. Pinned findings are still reported and still have
    /// their repros saved — they just do not set the exit code.
    ///
    /// The cost is real and worth stating: while a kind is pinned, a *new* bug
    /// producing the same signature will not redden CI either. It will appear
    /// in the report and the corpus, but someone has to read it. That is the
    /// price of keeping the shapes in the corpus while the known bug is open,
    /// and it ends when the pin does.
    ///
    /// Two-sided, like `tests/extern_keep_list.rs`'s: `run()` reports a pin
    /// that stopped firing, so a fixed bug forces its own pin out instead of
    /// silently disarming the gate for that kind forever.
    const KNOWN_OPEN: &[(&str, &str)] = &[("drop-never-ran", "B-2026-07-30-11")];

    /// The ledger id explaining `sig`, if it is a pinned known-open kind.
    fn known_open_for(sig: &str) -> Option<&'static str> {
        let kind = sig.rsplit(':').next().unwrap_or(sig);
        KNOWN_OPEN
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, id)| *id)
    }

    fn is_memory_signature(sig: &str) -> bool {
        let kind = sig.rsplit(':').next().unwrap_or(sig);
        matches!(
            kind,
            "double-free"
                | "heap-use-after-free"
                | "bad-free"
                | "memory-leak"
                | "heap-buffer-overflow"
                | "stack-buffer-overflow"
                | "segv"
                | "abort-sigabrt"
                | "trap-sigtrap"
                | "asan-error"
                // Drop-log oracle. These gate for the same reason the ASan
                // signatures do — a user `impl Drop` body that runs twice or
                // not at all is a miscompile, and the fact that no sanitizer
                // can see it is the argument for gating on it, not against.
                | "drop-ran-too-often"
                | "drop-never-ran"
                | "drop-imbalance-both"
                | "droplog-divergence"
        )
    }

    fn indent(s: &str, pad: &str) -> String {
        s.lines()
            .map(|l| format!("{pad}{l}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[allow(clippy::too_many_arguments)]
    fn write_report(
        cfg: &Config,
        findings: &[Finding],
        seen_sigs: &BTreeMap<String, u64>,
        valid: u64,
        runs: u64,
        total: u64,
        elapsed: Duration,
        oracle: &OracleStats,
    ) {
        let mut md = String::new();
        md.push_str("# drop_fuzz report\n\n");
        md.push_str(&format!(
            "- programs generated: **{total}**\n\
             - programs valid (compiled+ran on ≥1 surface): **{valid}**\n\
             - valid (program,surface) executions: **{runs}**\n\
             - base seed: `{}`\n\
             - elapsed: {:.1}s\n\n",
            cfg.seed,
            elapsed.as_secs_f64()
        ));

        md.push_str(&format!(
            "## Ownership-oracle self-check (Slice 3)\n\n\
             The executable judgment ran on **{}** generated programs, scheduling \
             **{}** drops, with **{}** invariant violation(s). The generator emits \
             only ownership-clean programs, so a nonzero violation count means the \
             model and the generator disagree (an oracle bug or a checker gap).\n\n",
            oracle.programs, oracle.drops, oracle.violations
        ));

        let mem: u64 = seen_sigs
            .iter()
            .filter(|(s, _)| is_memory_signature(s))
            .map(|(_, n)| *n)
            .sum();
        let rate = if runs > 0 {
            mem as f64 / runs as f64 * 100.0
        } else {
            0.0
        };
        md.push_str(&format!(
            "## Measured drop-bug rate\n\n\
             **{mem}** memory-safety findings over **{runs}** valid executions = **{rate:.2}%**.\n\n"
        ));

        md.push_str("## Signatures (bucketed corpus)\n\n");
        if seen_sigs.is_empty() {
            md.push_str("_No findings on this run._\n\n");
        } else {
            md.push_str("| signature | count | memory-safety |\n|---|---|---|\n");
            for (sig, n) in seen_sigs {
                md.push_str(&format!(
                    "| `{sig}` | {n} | {} |\n",
                    if is_memory_signature(sig) {
                        "yes"
                    } else {
                        "no"
                    }
                ));
            }
            md.push('\n');
        }

        if !findings.is_empty() {
            md.push_str("## Minimal repros\n\n");
            for (i, f) in findings.iter().enumerate() {
                md.push_str(&format!(
                    "### repro {} — `{}` (seed {})\n\n",
                    i + 1,
                    f.signature,
                    f.seed
                ));
                md.push_str("<details><summary>sanitizer excerpt</summary>\n\n```\n");
                md.push_str(&f.detail);
                md.push_str("\n```\n\n</details>\n\n```rust\n");
                md.push_str(&f.src);
                md.push_str("\n```\n\n");
                // Also drop each repro as a standalone .kara for the kata pipe.
                let fname = format!("repro_{:02}_{}.kara", i + 1, sanitize(&f.signature));
                let _ = std::fs::write(cfg.out.join(&fname), &f.src);
            }
        }

        let _ = std::fs::write(cfg.out.join("report.md"), &md);
    }

    fn sanitize(s: &str) -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn print_summary(
        findings: &[Finding],
        seen_sigs: &BTreeMap<String, u64>,
        valid: u64,
        runs: u64,
        total: u64,
        elapsed: Duration,
        out: &Path,
        oracle: &OracleStats,
    ) {
        let mem: u64 = seen_sigs
            .iter()
            .filter(|(s, _)| is_memory_signature(s))
            .map(|(_, n)| *n)
            .sum();
        eprintln!("\n════════ drop_fuzz summary ════════");
        eprintln!("generated:  {total}");
        eprintln!("valid:      {valid} programs ({runs} valid executions)");
        eprintln!("elapsed:    {:.1}s", elapsed.as_secs_f64());
        eprintln!(
            "oracle:     {} programs, {} scheduled drops, {} invariant violation(s){}",
            oracle.programs,
            oracle.drops,
            oracle.violations,
            if oracle.violations == 0 {
                " ✓"
            } else {
                " ⚠"
            }
        );
        eprintln!("signatures:");
        if seen_sigs.is_empty() {
            eprintln!("  (none — all clean)");
        }
        for (sig, n) in seen_sigs {
            let mark = if is_memory_signature(sig) {
                "⚠ "
            } else {
                "  "
            };
            eprintln!("  {mark}{sig}: {n}");
        }
        let rate = if runs > 0 {
            mem as f64 / runs as f64 * 100.0
        } else {
            0.0
        };
        eprintln!("memory-safety findings: {mem} ({rate:.2}% of valid executions)");
        eprintln!("repros saved: {} -> {}", findings.len(), out.display());
        eprintln!("report: {}", out.join("report.md").display());
    }

    // ───────────────────────── drop-log oracle tests ─────────────────────
    //
    // The oracle's *under*-count arm has a live subject (B-2026-07-30-11 is
    // open, and the corpus hits it constantly). Its *over*-count arm does not:
    // the bug that ran a Drop body twice, B-2026-07-31-37, is fixed, so no
    // generated program currently exercises that direction. Without these tests
    // "drop-ran-too-often" would be code that has never once been observed to
    // work — and the next double-run bug would be the first thing to test it.
    #[cfg(test)]
    mod tests {
        use super::DropLog;

        #[test]
        fn balanced_log_reports_nothing() {
            let log = DropLog::parse("N1\nN2\nD2\nD1\n42\n");
            assert!(log.imbalances().is_empty());
            assert_eq!(log.constructions(), 2);
        }

        #[test]
        fn a_body_that_never_ran_is_an_under_count() {
            let log = DropLog::parse("N1\nN1\nD1\n");
            assert_eq!(log.imbalances(), vec![(1, 2, 1)]);
            assert_eq!(log.imbalance_kind(), "drop-never-ran");
        }

        #[test]
        fn a_body_that_ran_twice_is_an_over_count() {
            let log = DropLog::parse("N7\nD7\nD7\n");
            assert_eq!(log.imbalances(), vec![(7, 1, 2)]);
            assert_eq!(log.imbalance_kind(), "drop-ran-too-often");
        }

        #[test]
        fn both_directions_at_once_get_their_own_bucket() {
            let log = DropLog::parse("N1\nN1\nD1\nN2\nD2\nD2\n");
            assert_eq!(log.imbalance_kind(), "drop-imbalance-both");
        }

        /// Non-token output — the program's own `println(acc)`, and a line torn
        /// by two par lanes writing at once — must not be counted. A torn write
        /// read as a token would show up as a phantom imbalance, which is the
        /// one way this oracle could manufacture a bug that is not there.
        #[test]
        fn non_token_and_torn_lines_are_ignored() {
            let log = DropLog::parse("N3\n123456\nND3\nN\nDx9\nN3garbage\nD3\n");
            assert!(
                log.imbalances().is_empty(),
                "unexpected imbalance: {:?}",
                log.imbalances()
            );
            assert_eq!(log.constructions(), 1);
        }

        /// Negative-count directions must not cancel out across tags: one tag
        /// over and another under is still two findings, not zero.
        #[test]
        fn imbalances_do_not_cancel_across_tags() {
            let log = DropLog::parse("N1\nN1\nD1\nN2\nD2\nD2\n");
            assert_eq!(log.imbalances().len(), 2);
        }
    }
}
