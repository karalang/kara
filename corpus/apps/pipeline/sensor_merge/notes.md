lines: 335
build: ok
interp: same
mirror: agrees
stmt-par: L302-L304 three fs.write calls (one per sensor file); L310-L312 three fs.read_to_string calls (one per sensor file)
shared types: none
workarounds: none. Two ordinary first-draft corrections, neither caused by a compiler defect: (1) `&&` / `||` / `!` were rejected with "the `&&` operator is not used in Kāra; use `and` instead" (and the same for `or` / `not`); `karac fix` rewrote all 7 automatically. (2) `error[typecheck]: expected 'String', found 'ref String'` at L156/L160/L164/L167, where a `path: ref String` parameter was passed into an owned `String` payload of `PipelineError`; fixed with `path.to_string()`. This was a type error that blocked the build, not an ownership diagnostic.
ignored diagnostics: 0 (`karac check source.orig.kara` prints no `error[` lines; `karac build` printed none either)
