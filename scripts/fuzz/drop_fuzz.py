#!/usr/bin/env python3
"""The drop-soundness fuzzer on v2 (B-2026-11-02-22).

Generates small, well-typed Kāra programs over the heap core (owned
Strings, Vecs, Maps, Sets, Arrays, tuples, Options, Results, user and
generic enums, `shared` types) and the moves, borrows, projections,
displacements and parallel captures that drop elaboration has to get
right, then runs each one three ways and judges it with two oracles:

  surfaces  interp   `karac __mir-run` (the MIR interpreter, in order)
            seq      `karac __mir-llvm-asan` with `KARAC_AUTO_PAR=0`
            autopar  `karac __mir-llvm-asan` with `KARAC_AUTO_PAR=1`
                     on four workers

  oracles   ASan + LSan on the two compiled surfaces: a double free, a
            use after free or a leak.
            The drop log on every surface: `new_tracked` prints `N<tag>`
            and `Tracked`'s `Drop` body prints `D<tag>`, and every tag
            must balance. A body that runs twice, or never, while the
            heap is freed correctly is invisible to the sanitizers; this
            is the only instrument that sees it.

A compiled surface whose stdout differs from the interpreter's is a
finding too (the run-vs-build class): exactly, or, for a program with a
`par` block or spawned tasks, as a multiset of lines.

The program is only judged once `karac check` accepts it, so a finding
implicates the compiler and never the generated source. A rejection is
counted per first error code and reported: a high rejection rate means
the generator has drifted from the language, and a run whose programs
are all rejected exits 2 (void), never 0.

Usage:
  python3 scripts/fuzz/drop_fuzz.py [--count N] [--seed S] [--out DIR]
                                    [--karac PATH] [--no-shrink]
                                    [--canary] [--verbose]

Exit status: 0 clean, 1 at least one finding, 2 void (nothing judged,
or the canary went unreported). Needs the runtime archives that
`karac __mir-llvm-asan` links (CLAUDE.md, "Runtime archives") and a `cc`
with `-fsanitize=address`. Leaks are judged on Linux only (no LSan on
macOS).
"""

from __future__ import annotations

import argparse
import collections
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
MASK = (1 << 64) - 1


class Rng:
    """splitmix64, so a program is reproducible from its seed alone."""

    def __init__(self, seed: int):
        self.s = (seed ^ 0x9E3779B97F4A7C15) & MASK

    def next(self) -> int:
        self.s = (self.s + 0x9E3779B97F4A7C15) & MASK
        z = self.s
        z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
        z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
        return z ^ (z >> 31)

    def below(self, n: int) -> int:
        return 0 if n == 0 else self.next() % n

    def chance(self, num: int, den: int) -> bool:
        return self.next() % den < num


# The heap-core types a live binding can have. The `*Tracked` shapes and
# the user/generic enums carry a user `impl Drop` somewhere inside, so the
# drop log judges them; the rest drop implicitly and the sanitizers judge.
STR, VECSTR, PAIR, VECPAIR, PAYLOAD, OPTSTR, VECVECSTR, VECPAYLOAD = range(8)
MAPSTR, SETSTR, TRACKED, VECTRACKED, OPTTRACKED, CRATE = range(8, 14)
TUPTRACKED, VECTUPTRACKED, OPTTUPVEC, MAPTRACKED, RESTRACKED = range(14, 19)
ARRSTR, ARRTRACKED, ARRARRTRACKED, OPTARRTRACKED = range(19, 23)
USERENUM, SLOTSTR, SLOTTRACKED, WRAPTRACKED, USERENUMARR = range(23, 28)

PAD = "payload_bytes_kept_long_enough_for_lsan_"  # >= 36 bytes defeats LSan's short-String blindness


class Gen:
    def __init__(self, seed: int):
        self.rng = Rng(seed)
        self.scope: list[tuple[str, int, bool]] = []  # (name, ty, declared mut)
        self.body: list[str] = []
        self.counter = 0
        self.payload_id = 0
        self.used_par = False
        self.tag = 0

    # ── helpers ──────────────────────────────────────────────────────────
    def fresh_tag(self) -> int:
        self.tag += 1
        return self.tag

    def fresh(self, prefix: str) -> str:
        self.counter += 1
        return f"{prefix}{self.counter}"

    def lit(self) -> str:
        self.payload_id += 1
        i = self.payload_id
        return f'"df{i}_{PAD}{i}".to_string()'

    def i64(self) -> str:
        return f"{self.rng.below(90) + 1}"

    def emit(self, line: str):
        self.body.append("        " + line)

    def add(self, name: str, ty: int, mut: bool = False):
        self.scope.append((name, ty, mut))

    def kill(self, name: str):
        self.scope = [v for v in self.scope if v[0] != name]

    def live(self, ty: int, mut: bool = False):
        c = [v for v in self.scope if v[1] == ty and (v[2] or not mut)]
        return c[self.rng.below(len(c))][0] if c else None

    def take(self, ty: int):
        n = self.live(ty)
        if n is not None:
            self.kill(n)
        return n

    def tracked(self) -> str:
        return f"new_tracked({self.fresh_tag()}, {self.lit()})"

    def str_binding(self) -> str:
        n = self.fresh("s")
        self.emit(f"let {n}: String = {self.lit()};")
        return n

    def vecstr_lit(self) -> str:
        return f"Vec[{self.lit()}, {self.lit()}]"

    # ── producers ────────────────────────────────────────────────────────
    def make_str(self):
        n = self.fresh("s")
        self.emit(f"let {n}: String = {self.lit()};")
        self.add(n, STR)

    def make_vecstr(self):
        n = self.fresh("v")
        if self.rng.chance(1, 2):
            self.emit(f"let {n}: Vec[String] = Vec[{self.lit()}, {self.lit()}, {self.lit()}];")
            self.add(n, VECSTR)
        else:
            self.emit(f"let mut {n}: Vec[String] = Vec.new();")
            for _ in range(2 + self.rng.below(2)):
                self.emit(f"{n}.push({self.lit()});")
            self.add(n, VECSTR, True)

    def make_pair(self):
        n = self.fresh("p")
        k = self.i64()
        s = self.take(STR) or self.str_binding()
        self.emit(f"let {n}: (i64, String) = ({k}, {s});")
        self.add(n, PAIR)

    def make_payload(self):
        n = self.fresh("pl")
        tag = self.i64()
        name = self.take(STR) or self.lit()
        items = self.take(VECSTR) or self.vecstr_lit()
        self.emit(f"let {n}: Payload = Payload {{ tag: {tag}, name: {name}, items: {items} }};")
        self.add(n, PAYLOAD)

    def make_indexed_vecstr(self):
        # Index stores over heap elements: each displaced element drops once.
        n = self.fresh("iv")
        self.emit(f"let mut {n}: Vec[String] = Vec.new();")
        self.emit(f"{n}.push({self.lit()});")
        self.emit(f"{n}.push({self.lit()});")
        self.emit(f"{n}[0] = {self.lit()};")
        self.emit(f"{n}[1] = {self.lit()};")
        self.add(n, VECSTR, True)

    def make_nested_vec(self):
        n = self.fresh("vv")
        self.emit(f"let mut {n}: Vec[Vec[String]] = Vec.new();")
        for _ in range(2 + self.rng.below(2)):
            inner = self.take(VECSTR) or self.vecstr_lit()
            self.emit(f"{n}.push({inner});")
        self.add(n, VECVECSTR)

    def make_tree(self):
        # A recursive `shared enum`, consumed at once.
        n = self.fresh("tr")
        s = self.take(STR) or self.lit()
        expr = f"Tree.Leaf({s})"
        for _ in range(1 + self.rng.below(3)):
            expr = f"Tree.Node({expr})"
        self.emit(f"let {n}: Tree = {expr};")
        self.emit(f"acc = acc + tree_len({n});")

    def make_payload_vec(self):
        n = self.fresh("pv")
        self.emit(f"let mut {n}: Vec[Payload] = Vec.new();")
        for _ in range(2 + self.rng.below(2)):
            self.emit(
                f"{n}.push(Payload {{ tag: {self.i64()}, name: {self.lit()}, items: {self.vecstr_lit()} }});"
            )
        self.add(n, VECPAYLOAD)

    def make_map(self):
        # Key adoption: the map owns each key exactly once.
        n = self.fresh("m")
        self.emit(f"let mut {n}: Map[String, i64] = Map.new();")
        for _ in range(2 + self.rng.below(2)):
            key = self.take(STR) or self.lit()
            self.emit(f"{n}.insert({key}, {self.i64()});")
        self.add(n, MAPSTR, True)

    def make_set(self):
        n = self.fresh("st")
        self.emit(f"let mut {n}: Set[String] = Set.new();")
        for _ in range(2 + self.rng.below(2)):
            key = self.take(STR) or self.lit()
            self.emit(f"{n}.insert({key});")
        self.add(n, SETSTR, True)

    def make_optstr(self):
        n = self.fresh("o")
        if self.rng.chance(3, 4):
            s = self.take(STR) or self.lit()
            self.emit(f"let {n}: Option[String] = Some({s});")
        else:
            self.emit(f"let {n}: Option[String] = None;")
        self.add(n, OPTSTR)

    # Drop-carrying producers are always `let mut`: the displacement
    # transforms reassign them, the shape most drop bugs have taken.
    def make_tracked(self):
        n = self.fresh("t")
        self.emit(f"let mut {n}: Tracked = {self.tracked()};")
        self.add(n, TRACKED, True)

    def make_vec_tracked(self):
        n = self.fresh("vt")
        self.emit(f"let mut {n}: Vec[Tracked] = Vec.new();")
        for _ in range(1 + self.rng.below(3)):
            self.emit(f"{n}.push({self.tracked()});")
        self.add(n, VECTRACKED, True)

    def make_opt_tracked(self):
        n = self.fresh("ot")
        if self.rng.chance(3, 4):
            self.emit(f"let mut {n}: Option[Tracked] = Some({self.tracked()});")
        else:
            self.emit(f"let mut {n}: Option[Tracked] = None;")
        self.add(n, OPTTRACKED, True)

    def make_crate(self):
        n = self.fresh("cr")
        self.emit(f"let mut {n}: Crate = new_crate({self.fresh_tag()}, {self.lit()});")
        self.add(n, CRATE, True)

    def make_tup_tracked(self):
        n = self.fresh("tt")
        self.emit(f"let mut {n}: (i64, Tracked) = ({self.i64()}, {self.tracked()});")
        self.add(n, TUPTRACKED, True)

    def make_vec_tup_tracked(self):
        n = self.fresh("vtt")
        self.emit(f"let mut {n}: Vec[(i64, Tracked)] = Vec.new();")
        for _ in range(1 + self.rng.below(3)):
            self.emit(f"{n}.push(({self.i64()}, {self.tracked()}));")
        self.add(n, VECTUPTRACKED, True)

    def make_opt_tup_vec(self):
        n = self.fresh("otv")
        v, i = self.vecstr_lit(), self.i64()
        if self.rng.chance(4, 5):
            self.emit(f"let mut {n}: Option[(Vec[String], i64)] = Some(({v}, {i}));")
        else:
            self.emit(f"let mut {n}: Option[(Vec[String], i64)] = None;")
        self.add(n, OPTTUPVEC, True)

    def make_map_tracked(self):
        n = self.fresh("mt")
        self.emit(f"let mut {n}: Map[String, Tracked] = Map.new();")
        for _ in range(1 + self.rng.below(3)):
            self.emit(f"{n}.insert({self.lit()}, {self.tracked()});")
        self.add(n, MAPTRACKED, True)

    def make_res_tracked(self):
        n = self.fresh("rt")
        if self.rng.chance(4, 5):
            self.emit(f"let mut {n}: Result[Tracked, i64] = Ok({self.tracked()});")
        else:
            self.emit(f"let mut {n}: Result[Tracked, i64] = Err({self.i64()});")
        self.add(n, RESTRACKED, True)

    def make_arr_str(self):
        n = self.fresh("as")
        self.emit(f"let mut {n}: Array[String, 3] = [{self.lit()}, {self.lit()}, {self.lit()}];")
        self.add(n, ARRSTR, True)

    def make_arr_tracked(self):
        n = self.fresh("at")
        self.emit(f"let mut {n}: Array[Tracked, 2] = [{self.tracked()}, {self.tracked()}];")
        self.add(n, ARRTRACKED, True)

    def make_arr_arr_tracked(self):
        n = self.fresh("aat")
        a = f"[{self.tracked()}, {self.tracked()}]"
        b = f"[{self.tracked()}, {self.tracked()}]"
        self.emit(f"let mut {n}: Array[Array[Tracked, 2], 2] = [{a}, {b}];")
        self.add(n, ARRARRTRACKED, True)

    def make_opt_arr_tracked(self):
        n = self.fresh("oat")
        if self.rng.chance(4, 5):
            self.emit(
                f"let mut {n}: Option[Array[Tracked, 2]] = Some([{self.tracked()}, {self.tracked()}]);"
            )
        else:
            self.emit(f"let mut {n}: Option[Array[Tracked, 2]] = None;")
        self.add(n, OPTARRTRACKED, True)

    def make_user_enum(self):
        # One type with all three arm shapes: a multi-payload variant, a
        # scalar payload, and a unit variant.
        n = self.fresh("pc")
        r = self.rng.below(5)
        if r == 0:
            self.emit(f"let mut {n}: Parcel = Parcel.Note({self.i64()});")
        elif r == 1:
            self.emit(f"let mut {n}: Parcel = Parcel.Nothing;")
        else:
            self.emit(f"let mut {n}: Parcel = Parcel.Two({self.tracked()}, {self.lit()});")
        self.add(n, USERENUM, True)

    def make_user_enum_arr(self):
        # A payload wider than the enum's inline slot: built inline (both
        # spellings) or moved in from a local.
        n = self.fresh("bn")
        r = self.rng.below(6)
        arr = lambda: f"[{self.tracked()}, {self.tracked()}]"
        if r == 0:
            self.emit(f"let mut {n}: Bin = Bin.Bare;")
        elif r in (1, 2, 3):
            self.emit(f"let mut {n}: Bin = Bin.Packed({arr()});")
        else:
            src = self.fresh("bsrc")
            self.emit(f"let mut {src}: Array[Tracked, 2] = {arr()};")
            self.emit(f"let mut {n}: Bin = Bin.Packed({src});")
        self.add(n, USERENUMARR, True)

    def make_slot_str(self):
        n = self.fresh("gs")
        if self.rng.chance(4, 5):
            self.emit(f"let mut {n}: Slot[String] = Slot.Filled({self.lit()});")
        else:
            self.emit(f"let mut {n}: Slot[String] = Slot.Blank;")
        self.add(n, SLOTSTR, True)

    def make_slot_tracked(self):
        n = self.fresh("gt")
        if self.rng.chance(4, 5):
            self.emit(f"let mut {n}: Slot[Tracked] = Slot.Filled({self.tracked()});")
        else:
            self.emit(f"let mut {n}: Slot[Tracked] = Slot.Blank;")
        self.add(n, SLOTTRACKED, True)

    def make_wrap_tracked(self):
        n = self.fresh("gw")
        self.emit(f"let mut {n}: Wrap[Tracked] = Wrap {{ item: {self.tracked()} }};")
        self.add(n, WRAPTRACKED, True)

    PRODUCERS = (
        make_str, make_vecstr, make_pair, make_payload, make_optstr,
        make_indexed_vecstr, make_nested_vec, make_payload_vec, make_map,
        make_set, make_tree, make_tracked, make_vec_tracked, make_opt_tracked,
        make_crate, make_tup_tracked, make_vec_tup_tracked, make_opt_tup_vec,
        make_map_tracked, make_res_tracked, make_arr_str, make_arr_tracked,
        make_arr_arr_tracked, make_opt_arr_tracked, make_user_enum,
        make_slot_str, make_slot_tracked, make_wrap_tracked, make_user_enum_arr,
    )  # fmt: skip

    # ── transforms: each returns whether it applied ──────────────────────
    def move_str_into_vec(self):
        s = self.take(STR)
        if s is None:
            return False
        n = self.fresh("v")
        self.emit(f"let mut {n}: Vec[String] = Vec.new();")
        self.emit(f"{n}.push({s});")
        self.add(n, VECSTR, True)
        return True

    def conditional_move_str_into_vec(self):
        # Moved on half the rounds: `s` must drop exactly on the others.
        s = self.take(STR)
        if s is None:
            return False
        v = self.live(VECSTR, mut=True)
        if v is None:
            v = self.fresh("v")
            self.emit(f"let mut {v}: Vec[String] = Vec.new();")
            self.add(v, VECSTR, True)
        self.emit(f"if round % 2 == 0 {{ {v}.push({s}); }}")
        return True

    def for_owned_vecstr(self):
        v = self.take(VECSTR)
        if v is None:
            return False
        e = self.fresh("e")
        self.emit(f"for {e} in {v}.into_iter() {{ acc = acc + {e}.len(); }}")
        return True

    def for_borrow_vecstr(self):
        v = self.live(VECSTR)
        if v is None:
            return False
        e = self.fresh("e")
        self.emit(f"for {e} in {v}.iter() {{ acc = acc + {e}.len(); }}")
        return True

    def pair_into_vec(self):
        s = self.take(STR)
        if s is None:
            return False
        n = self.fresh("vp")
        self.emit(f"let mut {n}: Vec[(i64, String)] = Vec.new();")
        self.emit(f"{n}.push(({self.i64()}, {s}));")
        self.add(n, VECPAIR)
        return True

    def destructure_payload(self):
        pl = self.take(PAYLOAD)
        if pl is None:
            return False
        tag, name, items = self.fresh("tg"), self.fresh("nm"), self.fresh("it")
        self.emit(f"let Payload {{ tag: {tag}, name: {name}, items: {items} }} = {pl};")
        self.emit(f"acc = acc + {tag} + {name}.len();")
        self.add(items, VECSTR)
        return True

    def match_optstr(self):
        o = self.take(OPTSTR)
        if o is None:
            return False
        x = self.fresh("x")
        self.emit(f"match {o} {{ Some({x}) => {{ acc = acc + {x}.len(); }}, None => {{}} }}")
        return True

    def pass_owned_str(self):
        s = self.take(STR)
        if s is None:
            return False
        self.emit(f"acc = acc + take_str({s});")
        return True

    def roundtrip_vecstr(self):
        v = self.take(VECSTR)
        if v is None:
            return False
        n = self.fresh("v")
        self.emit(f"let {n}: Vec[String] = echo_vec({v});")
        self.add(n, VECSTR)
        return True

    def par_capture(self):
        # Two `shared` holders read in a `par` block.
        if self.used_par:
            return False
        a = self.take(STR)
        if a is None:
            return False
        b = self.take(STR) or self.str_binding()
        self.used_par = True
        ha, hb = self.fresh("ha"), self.fresh("hb")
        self.emit(f"let {ha}: Holder = Holder {{ s: {a} }};")
        self.emit(f"let {hb}: Holder = Holder {{ s: {b} }};")
        self.emit(f"let ({ha}n, {hb}n) = par {{ hold_len({ha}), hold_len({hb}) }};")
        self.emit(f"acc = acc + {ha}n + {hb}n;")
        return True

    def spawn_capture_vecstr(self):
        # Clones of a Vec moved into spawned tasks: each frees in its task.
        if self.used_par:
            return False
        v = self.take(VECSTR)
        if v is None:
            return False
        self.used_par = True
        g, hs, h = self.fresh("pool"), self.fresh("hs"), self.fresh("h")
        self.emit(f"let {g} = TaskGroup.new();")
        self.emit(f"let mut {hs}: Vec[TaskHandle[i64]] = Vec.new();")
        for t in range(2 + self.rng.below(2)):
            c = self.fresh("c")
            self.emit(f"let {c}: Vec[String] = {v}.clone();")
            self.emit(f"{hs}.push({g}.spawn(|| band({c}, {t})));")
        self.emit(f"for {h} in {hs}.into_iter() {{ acc = acc + {h}.join(); }}")
        return True

    def ref_peek_str(self):
        s = self.live(STR)
        if s is None:
            return False
        self.emit(f"acc = acc + peek({s});")
        return True

    def mut_grow_vec(self):
        v = self.live(VECSTR, mut=True)
        if v is None:
            return False
        self.emit(f"grow(mut {v});")
        return True

    # Displacement of a user-`impl Drop` value: overwritten, reassigned or
    # moved through a variant rather than dropped at a scope's end.
    def reassign_tracked(self):
        v = self.live(TRACKED, mut=True)
        if v is None:
            return False
        self.emit(f"{v} = {self.tracked()};")
        return True

    def reassign_crate_field(self):
        v = self.live(CRATE, mut=True)
        if v is None:
            return False
        self.emit(f"{v}.item = {self.tracked()};")
        return True

    def reassign_opt_tracked(self):
        v = self.live(OPTTRACKED, mut=True)
        if v is None:
            return False
        if self.rng.chance(1, 2):
            self.emit(f"{v} = None;")
        else:
            self.emit(f"{v} = Some({self.tracked()});")
        return True

    def tracked_into_opt_then_displace(self):
        t = self.take(TRACKED)
        if t is None:
            return False
        n = self.fresh("ot")
        self.emit(f"let mut {n}: Option[Tracked] = Some({t});")
        if self.rng.chance(1, 2):
            self.emit(f"{n} = None;")
            self.emit(f"if {n}.is_none() {{ acc = acc + 1; }}")
        else:
            self.add(n, OPTTRACKED, True)
        return True

    def match_opt_tracked(self):
        o = self.take(OPTTRACKED)
        if o is None:
            return False
        x = self.fresh("mx")
        self.emit(f"match {o} {{ Some({x}) => {{ acc = acc + tracked_len({x}); }}, None => {{}} }}")
        return True

    def discard_opt_tracked_payload(self):
        # `_` drops the binding, not the obligation: the body still runs.
        o = self.take(OPTTRACKED)
        if o is None:
            return False
        self.emit(f"match {o} {{ Some(_) => {{ acc = acc + 1; }}, None => {{}} }}")
        return True

    def tracked_into_vec(self):
        t = self.take(TRACKED)
        if t is None:
            return False
        n = self.fresh("vt")
        self.emit(f"let mut {n}: Vec[Tracked] = Vec.new();")
        self.emit(f"{n}.push({t});")
        self.add(n, VECTRACKED, True)
        return True

    def for_owned_vec_tracked(self):
        v = self.take(VECTRACKED)
        if v is None:
            return False
        e = self.fresh("et")
        self.emit(f"for {e} in {v}.into_iter() {{ acc = acc + tracked_len({e}); }}")
        return True

    def ref_peek_tracked(self):
        v = self.live(TRACKED)
        if v is None:
            return False
        self.emit(f"acc = acc + tracked_peek({v});")
        return True

    def par_capture_tracked(self):
        # A `Tracked` moved into a `par` branch and dropped there. Only one
        # branch may: each drop prints, and two would both write stdout.
        if self.used_par:
            return False
        a = self.take(TRACKED)
        if a is None:
            return False
        self.used_par = True
        r = self.fresh("pr")
        self.emit(f"let ({r}a, {r}b) = par {{ tracked_len({a}), {self.i64()} * 2 }};")
        self.emit(f"acc = acc + {r}a + {r}b;")
        return True

    def ref_peek_tuple_elem(self):
        v = self.live(OPTTUPVEC)
        if v is None:
            return False
        x = self.fresh("tv")
        self.emit(
            f"match {v} {{ Some({x}) => {{ acc = acc + peek_vec({x}.0) + {x}.1; }}, None => {{}} }}"
        )
        self.kill(v)
        return True

    def match_opt_tup_vec(self):
        o = self.take(OPTTUPVEC)
        if o is None:
            return False
        x = self.fresh("otx")
        self.emit(f"match {o} {{ Some({x}) => {{ acc = acc + tup_vec_len({x}); }}, None => {{}} }}")
        return True

    def move_out_tuple_elem(self):
        t = self.take(TUPTRACKED)
        if t is None:
            return False
        n = self.fresh("mo")
        self.emit(f"let {n}: Tracked = {t}.1;")
        self.emit(f"acc = acc + tracked_len({n});")
        return True

    def for_owned_vec_tup_tracked(self):
        v = self.take(VECTUPTRACKED)
        if v is None:
            return False
        e = self.fresh("vte")
        self.emit(f"for {e} in {v}.into_iter() {{ acc = acc + tup_tracked_len({e}); }}")
        return True

    def reassign_res_tracked(self):
        v = self.live(RESTRACKED, mut=True)
        if v is None:
            return False
        if self.rng.chance(1, 2):
            self.emit(f"{v} = Err({self.i64()});")
        else:
            self.emit(f"{v} = Ok({self.tracked()});")
        return True

    def match_res_tracked(self):
        r = self.take(RESTRACKED)
        if r is None:
            return False
        x = self.fresh("rx")
        self.emit(
            f"match {r} {{ Ok({x}) => {{ acc = acc + tracked_len({x}); }}, Err({x}e) => {{ acc = acc + {x}e; }} }}"
        )
        return True

    TRANSFORMS = (
        move_str_into_vec, for_owned_vecstr, for_borrow_vecstr, pair_into_vec,
        destructure_payload, match_optstr, pass_owned_str, roundtrip_vecstr,
        par_capture, spawn_capture_vecstr, ref_peek_str, mut_grow_vec,
        conditional_move_str_into_vec, reassign_tracked, reassign_crate_field,
        reassign_opt_tracked, tracked_into_opt_then_displace, match_opt_tracked,
        tracked_into_vec, for_owned_vec_tracked, ref_peek_tracked,
        par_capture_tracked, match_opt_tup_vec, ref_peek_tuple_elem,
        move_out_tuple_elem, for_owned_vec_tup_tracked, reassign_res_tracked,
        match_res_tracked, discard_opt_tracked_payload,
    )  # fmt: skip

    # ── sinks: read every live binding, so a freed value is observable ───
    def sink(self, name: str, ty: int) -> str:
        f = self.fresh
        if ty == STR:
            return f"acc = acc + {name}.len();"
        if ty == VECSTR:
            e = f("e")
            return f"for {e} in {name}.iter() {{ acc = acc + {e}.len(); }}"
        if ty == PAIR:
            return f"acc = acc + {name}.0 + {name}.1.len();"
        if ty == VECPAIR:
            e = f("e")
            return f"for {e} in {name}.iter() {{ acc = acc + {e}.0 + {e}.1.len(); }}"
        if ty == PAYLOAD:
            return f"acc = acc + {name}.tag + {name}.name.len();"
        if ty == OPTSTR:
            x = f("x")
            return f"match {name} {{ Some({x}) => {{ acc = acc + {x}.len(); }}, None => {{}} }}"
        if ty == VECVECSTR:
            i, e = f("iv"), f("e")
            return f"for {i} in {name}.iter() {{ for {e} in {i}.iter() {{ acc = acc + {e}.len(); }} }}"
        if ty == VECPAYLOAD:
            p = f("p")
            return f"for {p} in {name}.iter() {{ acc = acc + {p}.tag + {p}.name.len(); }}"
        if ty in (MAPSTR, SETSTR, MAPTRACKED):
            # A borrowed read: the container itself dies at scope exit and
            # must cascade into every key and value exactly once.
            return f"acc = acc + {name}.len();"
        if ty == TRACKED:
            # Owned into the callee: the callee is the drop site.
            return f"acc = acc + tracked_len({name});"
        if ty == VECTRACKED:
            e = f("et")
            return f"for {e} in {name}.into_iter() {{ acc = acc + tracked_len({e}); }}"
        if ty == OPTTRACKED:
            x = f("xt")
            return f"match {name} {{ Some({x}) => {{ acc = acc + tracked_len({x}); }}, None => {{}} }}"
        if ty == CRATE:
            return f"acc = acc + {name}.lid + tracked_peek({name}.item);"
        if ty == TUPTRACKED:
            return f"acc = acc + tup_tracked_len({name});"
        if ty == VECTUPTRACKED:
            e = f("vte")
            return f"for {e} in {name}.into_iter() {{ acc = acc + tup_tracked_len({e}); }}"
        if ty == OPTTUPVEC:
            x = f("otx")
            return f"match {name} {{ Some({x}) => {{ acc = acc + tup_vec_len({x}); }}, None => {{}} }}"
        if ty == RESTRACKED:
            x = f("rsx")
            return f"match {name} {{ Ok({x}) => {{ acc = acc + tracked_len({x}); }}, Err({x}e) => {{ acc = acc + {x}e; }} }}"
        if ty == ARRSTR:
            return f"acc = acc + {name}[0].len();"
        if ty == ARRTRACKED:
            return f"acc = acc + {name}[0].name.len();"
        if ty == ARRARRTRACKED:
            return f"acc = acc + arr_inner_peek({name}[0]);"
        if ty == OPTARRTRACKED:
            x = f("oax")
            return f"match {name} {{ Some({x}) => {{ acc = acc + {x}[0].name.len(); }}, None => {{}} }}"
        # Half the enum sinks borrow rather than match: a matched scrutinee
        # is moved into its arm, and only a borrowed one is still live at
        # scope exit, where the enum's own drop is scheduled.
        if ty == USERENUMARR:
            if self.rng.chance(1, 2):
                return f"acc = acc + bin_peek({name});"
            x = f("bnx")
            return f"match {name} {{ Packed({x}) => {{ acc = acc + {x}[0].name.len(); }}, Bare => {{}} }}"
        if ty == USERENUM:
            x = f("pcx")
            return (
                f"match {name} {{ Two({x}t, {x}s) => {{ acc = acc + tracked_len({x}t) + {x}s.len(); }}, "
                f"Note({x}n) => {{ acc = acc + {x}n; }}, Nothing => {{}} }}"
            )
        if ty == SLOTSTR:
            if self.rng.chance(1, 2):
                return f"acc = acc + slot_str_peek({name});"
            x = f("gsx")
            return f"match {name} {{ Filled({x}) => {{ acc = acc + {x}.len(); }}, Blank => {{}} }}"
        if ty == SLOTTRACKED:
            if self.rng.chance(1, 2):
                return f"acc = acc + slot_tracked_peek({name});"
            x = f("gtx")
            return f"match {name} {{ Filled({x}) => {{ acc = acc + tracked_len({x}); }}, Blank => {{}} }}"
        if ty == WRAPTRACKED:
            return f"acc = acc + {name}.item.tag + tracked_peek({name}.item);"
        raise AssertionError(ty)

    def program(self) -> str:
        for _ in range(3 + self.rng.below(4)):
            self.PRODUCERS[self.rng.below(len(self.PRODUCERS))](self)
        for _ in range(6 + self.rng.below(10)):
            order = list(range(len(self.TRANSFORMS) + 1))
            for i in range(len(order) - 1, 0, -1):
                j = self.rng.below(i + 1)
                order[i], order[j] = order[j], order[i]
            if not any(k < len(self.TRANSFORMS) and self.TRANSFORMS[k](self) for k in order):
                self.PRODUCERS[self.rng.below(len(self.PRODUCERS))](self)
        scope, self.scope = self.scope, []
        for name, ty, _ in scope:
            self.emit(self.sink(name, ty))
        body = "\n".join(self.body)
        # The body loops: a double free shows on the second free, and
        # leaks accumulate.
        return (
            f"{PREAMBLE}\nfn main() {{\n    let mut acc: i64 = 0;\n    let mut round: i64 = 0;\n"
            f"    while round < 40 {{\n{body}\n        round = round + 1;\n    }}\n    println(acc);\n}}\n"
        )


# Parameters borrow unless declared `own` (D5): the `own` ones are the
# helpers a value is moved INTO, so the callee is its drop site.
PREAMBLE = """struct Payload { tag: i64, name: String, items: Vec[String] }

sync struct Holder { s: String }

shared enum Tree { Leaf(String), Node(Tree) }

fn take_str(s: own String) -> i64 { s.len() }

fn echo_vec(v: own Vec[String]) -> Vec[String] { v }

fn hold_len(h: Holder) -> i64 { h.s.len() }

fn peek(s: String) -> i64 { s.len() }

fn grow(v: mut ref Vec[String]) {
    v.push("grow_appended_payload_kept_long_for_lsan_xx".to_string());
}

fn band(data: own Vec[String], lo: i64) -> i64 {
    let mut acc: i64 = 0;
    for e in data.iter() { acc = acc + e.len() + lo; }
    acc
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

fn new_tracked(tag: i64, name: own String) -> Tracked {
    println(f"N{tag}");
    Tracked { tag: tag, name: name }
}

fn tracked_len(t: own Tracked) -> i64 { t.name.len() }

fn tracked_peek(t: Tracked) -> i64 { t.name.len() }

struct Crate { lid: i64, item: Tracked }

fn new_crate(tag: i64, name: own String) -> Crate {
    Crate { lid: tag, item: new_tracked(tag, name) }
}

fn tup_tracked_len(t: own (i64, Tracked)) -> i64 { t.0 + t.1.name.len() }

fn tup_vec_len(t: own (Vec[String], i64)) -> i64 {
    let mut acc: i64 = t.1;
    for e in t.0.iter() { acc = acc + e.len(); }
    acc
}

fn peek_vec(v: Vec[String]) -> i64 { v.len() }

fn arr_inner_peek(a: Array[Tracked, 2]) -> i64 { a[1].name.len() }

enum Slot[T] { Filled(T), Blank }

struct Wrap[T] { item: T }

enum Parcel { Two(Tracked, String), Note(i64), Nothing }

enum Bin { Packed(Array[Tracked, 2]), Bare }

fn slot_str_peek(s: Slot[String]) -> i64 {
    match s {
        Filled(x) => x.len(),
        Blank => 0,
    }
}

fn slot_tracked_peek(s: Slot[Tracked]) -> i64 {
    match s {
        Filled(t) => t.name.len(),
        Blank => 0,
    }
}

fn bin_peek(b: Bin) -> i64 {
    match b {
        Packed(a) => a[1].name.len(),
        Bare => 0,
    }
}
"""


# ── judging ──────────────────────────────────────────────────────────────

SURFACES = ("interp", "seq", "autopar")
TOKEN = re.compile(r"^([ND])(-?\d+)$")
ASAN = re.compile(r"ERROR: (AddressSanitizer|LeakSanitizer): ?([A-Za-z-]*)")


def drop_log(stdout: str) -> dict[int, list[int]]:
    """tag -> [constructed, dropped]."""
    counts: dict[int, list[int]] = collections.defaultdict(lambda: [0, 0])
    for line in stdout.splitlines():
        m = TOKEN.match(line.strip())
        if m:
            counts[int(m.group(2))][0 if m.group(1) == "N" else 1] += 1
    return counts


def imbalance(counts) -> tuple[str, str] | None:
    bad = [(t, n, d) for t, (n, d) in sorted(counts.items()) if n != d]
    if not bad:
        return None
    over = any(d > n for _, n, d in bad)
    under = any(n > d for _, n, d in bad)
    kind = "drop-imbalance-both" if over and under else "drop-ran-too-often" if over else "drop-never-ran"
    detail = "".join(f"  tag {t}: constructed {n}x, dropped {d}x\n" for t, n, d in bad[:6])
    return kind, detail


class Karac:
    def __init__(self, exe: str, work: Path, timeout: int):
        self.exe, self.work, self.timeout = exe, work, timeout
        self.n = 0

    def _run(self, args, src: str, env: dict) -> subprocess.CompletedProcess | None:
        self.n += 1
        d = self.work / f"p{self.n}"
        d.mkdir(parents=True, exist_ok=True)
        (d / "main.kara").write_text(src)
        e = dict(os.environ, KARAC_HASH_SEED="1", **env)
        try:
            return subprocess.run(
                [self.exe, *args, "main.kara"], cwd=d, env=e, capture_output=True,
                text=True, errors="replace", timeout=self.timeout, check=False,
            )
        except subprocess.TimeoutExpired:
            return None
        finally:
            shutil.rmtree(d, ignore_errors=True)

    def check(self, src: str) -> str | None:
        """None when `karac check` accepts `src`, else the first error's code."""
        p = self._run(["check"], src, {})
        if p is None:
            return "check-timeout"
        if p.returncode == 0:
            return None
        return error_code(p.stdout + p.stderr) or f"check-exit-{p.returncode}"

    def run(self, surface: str, src: str):
        if surface == "interp":
            return self._run(["__mir-run"], src, {"KARAC_AUTO_PAR": "0"})
        env = {"ASAN_OPTIONS": "exitcode=86:detect_leaks=1", "KARAC_THREADS": "4"}
        env["KARAC_AUTO_PAR"] = "1" if surface == "autopar" else "0"
        return self._run(["__mir-llvm-asan"], src, env)


def judge(karac: Karac, src: str, surface: str, unordered: bool, want: str | None):
    """(kind, payload). kind is clean / invalid / finding; a clean run's
    payload is its stdout, a finding's is (signature, detail)."""
    p = karac.run(surface, src)
    if p is None:
        return "finding", (f"timeout@{surface}", "no exit within the timeout")
    out, err = p.stdout, p.stderr
    m = ASAN.search(err)
    if m:
        kind = m.group(2) or m.group(1)
        return "finding", (f"asan:{kind}@{surface}", excerpt(err, m.start()))
    if p.returncode == 3 and "Linker failed" in err:
        return "invalid", "link-failed"
    if p.returncode != 0:
        if "error[" in err and not out:
            # Refused by the compiler after `check` accepted it.
            code = error_code(err)
            return "finding", (f"codegen-refused:{code}@{surface}", excerpt(err, 0))
        return "finding", (f"exit-{p.returncode}@{surface}", excerpt(err, 0))
    bad = imbalance(drop_log(out))
    if bad:
        return "finding", (f"{bad[0]}@{surface}", bad[1])
    if want is not None:
        same = sorted(out.splitlines()) == sorted(want.splitlines()) if unordered else out == want
        if not same:
            return "finding", (f"diverges-from-interp@{surface}", diff(want, out))
    return "clean", out


def error_code(text: str) -> str | None:
    """The first diagnostic's most specific code: `E_NOT_CROSS_TASK`
    inside `error[typecheck]: …: error[E_NOT_CROSS_TASK]: …`."""
    for line in text.splitlines():
        codes = re.findall(r"error\[([A-Za-z0-9_-]+)\]", line)
        if codes:
            return codes[-1]
    return None


def excerpt(err: str, at: int) -> str:
    lines = err[at:].splitlines()
    return "\n".join(lines[:24]) + "\n"


def diff(a: str, b: str) -> str:
    import difflib

    d = difflib.unified_diff(a.splitlines(), b.splitlines(), "interp", "compiled", lineterm="", n=1)
    return "\n".join(list(d)[:40]) + "\n"


def judge_all(karac: Karac, src: str, unordered: bool):
    """Every surface: (findings, invalid reason or None)."""
    findings = []
    k, interp = judge(karac, src, "interp", unordered, None)
    if k == "finding":
        findings.append(interp)
        interp = None
    for s in ("seq", "autopar"):
        k, r = judge(karac, src, s, unordered, interp)
        if k == "invalid":
            return findings, r
        if k == "finding":
            findings.append(r)
    return findings, None


# ── shrinking ────────────────────────────────────────────────────────────

def shrink(karac: Karac, src: str, sig: str, unordered: bool, budget: float) -> str:
    """Greedy line deletion inside `main`'s loop, keeping each deletion that
    still type-checks and still reproduces `sig` on its surface."""
    surface = sig.rsplit("@", 1)[1]
    lines = src.splitlines(keepends=True)
    start = next(i for i, l in enumerate(lines) if l.startswith("    while round"))
    deadline = time.monotonic() + budget
    i = len(lines) - 1
    while i > start and time.monotonic() < deadline:
        l = lines[i]
        if l.startswith("        ") and not l.startswith("        round = "):
            trial = "".join(lines[:i] + lines[i + 1:])
            if karac.check(trial) is None:
                want = None
                if surface != "interp":
                    k, want = judge(karac, trial, "interp", unordered, None)
                    if k != "clean":
                        want = None
                k, r = judge(karac, trial, surface, unordered, want)
                if k == "finding" and r[0] == sig:
                    lines.pop(i)
        i -= 1
    return "".join(lines)


# ── driver ───────────────────────────────────────────────────────────────

def canary(src: str) -> str:
    """`src` with one `Tracked` drop token printed twice: a body that ran
    twice. The drop-log oracle must report it on every surface."""
    return src.replace('fn drop(mut ref self) { println(f"D{self.tag}"); }',
                       'fn drop(mut ref self) { println(f"D{self.tag}"); if self.tag == 1 { println("D1"); } }')


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--count", type=int, default=200)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--out", default=str(REPO / "target" / "drop-fuzz"))
    ap.add_argument("--karac", default=str(REPO / "target" / "release" / "karac"))
    ap.add_argument("--timeout", type=int, default=120, help="seconds per surface run")
    ap.add_argument("--no-shrink", action="store_true")
    ap.add_argument("--shrink-seconds", type=float, default=300)
    ap.add_argument("--canary", action="store_true",
                    help="check that a planted double drop is reported, then exit")
    ap.add_argument("--print", action="store_true", help="print program --seed and exit")
    ap.add_argument("--verbose", action="store_true")
    a = ap.parse_args()

    if a.print:
        print(Gen(a.seed).program(), end="")
        return 0
    if not Path(a.karac).exists():
        print(f"drop-fuzz: no karac at {a.karac} (cargo build --release --features llvm)", file=sys.stderr)
        return 2
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="karac-drop-fuzz-"))
    karac = Karac(a.karac, work, a.timeout)

    if a.canary:
        reported = []
        for seed in range(a.seed, a.seed + 50):
            src = Gen(seed).program()
            if "new_tracked(1," not in src or karac.check(src) is not None:
                continue
            bad = canary(src)
            for s in SURFACES:
                k, r = judge(karac, bad, s, True, None)
                reported.append((s, k == "finding" and r[0] == f"drop-ran-too-often@{s}"))
            break
        shutil.rmtree(work, ignore_errors=True)
        ok = reported and all(r for _, r in reported)
        print(f"drop-fuzz canary: {reported or 'no program built tag 1'} -> {'reported' if ok else 'MISSED'}")
        return 0 if ok else 2

    invalid: collections.Counter = collections.Counter()
    findings: dict[str, list] = {}
    judged = 0
    t0 = time.monotonic()
    for k in range(a.count):
        seed = a.seed + k
        g = Gen(seed)
        src = g.program()
        unordered = g.used_par
        why = karac.check(src)
        if why is not None:
            invalid[f"rejected by check: {why}"] += 1
            if invalid[f"rejected by check: {why}"] == 1:
                (out / f"rejected-{why}-seed{seed}.kara").write_text(src)
            continue
        found, why = judge_all(karac, src, unordered)
        if why is not None:
            invalid[why] += 1
            continue
        judged += 1
        for sig, detail in found:
            first = sig not in findings
            findings.setdefault(sig, []).append(seed)
            if first:
                small = src if a.no_shrink else shrink(karac, src, sig, unordered, a.shrink_seconds)
                name = re.sub(r"[^A-Za-z0-9_.-]", "_", sig)
                (out / f"{name}.kara").write_text(small)
                (out / f"{name}.txt").write_text(f"seed {seed}\n{sig}\n\n{detail}")
        if a.verbose:
            print(f"seed {seed}: {', '.join(s for s, _ in found) or 'clean'}", flush=True)
    shutil.rmtree(work, ignore_errors=True)

    lines = [
        f"# drop-fuzz: seeds {a.seed}..{a.seed + a.count - 1}",
        "",
        (
            f"judged {judged} of {a.count} programs on {', '.join(SURFACES)} "
            f"in {time.monotonic() - t0:.0f} s"
        ),
        "",
        "## Findings" if findings else "## Findings: none",
    ]
    for sig, seeds in sorted(findings.items()):
        lines.append(f"- `{sig}`: {len(seeds)} program(s), first seed {seeds[0]}; repro `{re.sub(r'[^A-Za-z0-9_.-]', '_', sig)}.kara`")
    if invalid:
        lines += ["", "## Not judged"]
        lines += [f"- {why}: {n}" for why, n in invalid.most_common()]
    report = "\n".join(lines) + "\n"
    (out / "report.md").write_text(report)
    print(report, end="")
    if judged == 0:
        print("drop-fuzz: VOID, no program was judged", file=sys.stderr)
        return 2
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
