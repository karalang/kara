lines: 307
build: ok
interp: same (stdout byte-identical to ./prog; both also print an "Error return trace: source.orig.kara:267:15" note to stderr for the deliberately failing third pair)
mirror: agrees
stmt-par: none
shared types: none
workarounds: none. Two build-blocking errors were mine, not compiler bugs, and I fixed them the way the diagnostics said: (1) a `match` arm body that was a bare assignment (`EditKind.Keep => stats.unchanged = stats.unchanged + 1,`) is a parse error, so those arms were wrapped in braces (L177-179, L201-202); (2) `let e = edits[k];` in make_hunk failed with error[E_INDEX_MOVE_NON_COPY] (a typecheck error, which stops the build), so it became `let e = ref edits[k];` (L195).
ignored diagnostics: 13 (8 error[borrow_projection_copy] + 5 error[ownership] from `karac check`; `karac build` prints 2 of them as warnings and still builds)
