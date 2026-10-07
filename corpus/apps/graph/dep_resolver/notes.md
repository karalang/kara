lines: 373

build: ok
interp: same
mirror: agrees

stmt-par: L332-L335 four fs.write calls, each to its own manifest file (reg_web/reg_cyclic/reg_conflict/reg_broken.txt)

shared types: none

workarounds:
- Registry and resolver merged into one struct. I first wrote `struct Registry { releases: Vec[Release] }` held as a field of `Resolver { registry: Registry, chosen: ... }`, and read releases as `self.registry.releases[idx]` and `resolver.registry.releases[...]`. The build failed with `error[chained_field_receiver]: chained field receivers (a.b.c…) are deferred to v1.x in codegen, so self.registry.releases as the receiver of an index expression checks clean but fails karac build`. Next I tried an accessor, `fn release(ref self, idx: i64) -> ref Release { ref self.releases[idx] }`. That failed too: `error[ownership]: this borrow-return form is not yet supported` (help points at B-2026-06-07-5), and it was the only error left, so it blocked the build. In the end I removed `Registry`: `Resolver` now holds `releases: Vec[Release]` directly (L174), and `load`/`best_match` became `Resolver` methods.
- Index reads by value were rejected and stopped the build: `error[E_INDEX_MOVE_NON_COPY]: cannot move out of an index expression`. This hit `let rel = self.releases[i]` and passing `pieces[1]` / `head[1]` (a `Vec[String]` element) to a parser that took an owned `String`. I used the borrow spelling the spec gives, `let rel = ref self.releases[i]` (L198, L222, L232, L262, L311). I changed `Version.parse` and `Constraint.parse` to take `text: ref String`. I added `.clone()` on `pieces[0]` / `head[0]` where the name goes into a struct field (L151, L170). These are ownership-flavoured, but they stopped the build, so I fixed them.
- (Not a workaround, an observation) `karac run --interp` prints `note[effect]: mutual recursion group resolved by fixed-point inference: Constraint.parse, Version.parse`. The two are not mutually recursive: `Constraint.parse` calls `Version.parse`, which calls `i64.parse`. The two methods probably get conflated because they share the name `parse`. Separately, the AOT binary prints an "Error return trace" to stderr for a handled `?` error; stdout is unaffected.

ignored diagnostics: 10
