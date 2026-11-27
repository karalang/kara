# core-semantics §12 and §14: sentence map with lane verdicts

One row per normative sentence of `docs/core-semantics.md` §12 (effects) and
§14 (providers), numbered as in `corpus/CORE_SENTENCE_MAP.md` (cs.931 to
cs.953, cs.972 to cs.1066; headings, list numerals and `Pin:` lines left
out). The same method as `conformance/core_c8_c9/SENTENCE_MAP.md`: each
sentence that can be run has a cell here (42 cells), run by
`conformance/cases.py` on every lane:

```bash
CONF_LANES=interp,jit,build,asan python3 conformance/cases.py conformance/core_c12_c14
CONF_LANES=tsan python3 conformance/cases.py conformance/core_c12_c14   # the cells that run tasks
```

A `.refused` cell is judged by `karac check` alone, and an effect or provider
rule is mostly a refusal, so most rows carry no TSan verdict: only the seven
cells whose `par` branches run carry one (3 runs each, `KARAC_THREADS=4`).
Rows with no cell are covered by the existing pins `CORE_SENTENCE_MAP.md`
names, or make no runnable claim.

## Findings

- **B-2026-11-27-20 (fixed).** A program that declared `trait C` failed before
  it ran, on every lane: the library's `C.from_iter(..)` under
  `[C: FromIterator[T]]` was read as a call on the program's trait ("trait `C`
  has no method `from_iter`"). Found by `p_hidden_param_through_callees`.
- **B-2026-11-27-21 (open, ruling).** cs.951: "With no effect clause, a call
  through [an escaping function value] has every effect." A call through a
  field does, but a direct call through an `escaping` parameter is charged its
  argument's effects at the call site, as a non-escaping one is (cs.948). That
  is sound (a printing argument still conflicts), but it is not what the
  sentence says. `e_unresolved_callee_has_every_effect` pins today's behaviour.

## Map

| id | § | sentence | cell(s) | lanes |
|---|---|---|---|---|
| cs.931 | §12 | The effect system is as in `design.md`, with four defaults made sound: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.933 | §12 | A call whose callee cannot be resolved statically has every effect. | `e_unresolved_callee_has_every_effect` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.935 | §12 | An `extern` function must declare its effects; otherwise it has every effect. | `e_extern_undeclared_every_effect` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.937 | §12 | A drop's effects are those of the dropped type's drop glue, transitively. | `e_drop_effects_charged` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.938 | §12 | They are charged to the scope where §7 places the drop. | `e_drop_effects_charged` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.940 | §12 | Recursive functions get their effects from a fixpoint over the call-graph strongly connected component, starting empty and iterating to a… | `e_recursive_fixpoint`, `e_recursive_pure_runs_apart` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.941 | §12 | Callees are identified by definition id, never by name. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.943 | §12 | A resource rooted at a value, such as `sends(tx)` or `writes(self.cache)`, is keyed by that value for conflict checking (D6, 2026-10-07). | `e_value_rooted_two_channels_apart`, `e_value_rooted_same_channel_conflicts` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.944 | §12 | Two channels, two connections or two files are two resources. | `e_value_rooted_two_channels_apart` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.945 | §12 | When the compiler cannot tell whether two such values are the same, they conflict. | `e_unknown_identity_conflicts` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.946 | §12 | A program-wide resource such as `Network` stays a capability ("may use the network"), not one shared key, so two calls on separate connec… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.947 | §12 | **Effect polymorphism needs no annotation** (D6, 2026-10-07). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.948 | §12 | - A call through a non-escaping function-typed parameter has the effects of the argument passed at each call site, and they are charged t… | `e_param_closure_effects_at_call_site`, `e_param_closure_printing_conflicts` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.949 | §12 | - The effects of a generic call are computed on the monomorphised instance. | `e_generic_monomorphised`, `e_generic_monomorphised_loud_conflicts` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.950 | §12 | - A call through an escaping function value has the effects its type declares (`escaping Fn(Request) -> Response with reads(Db)`); storin… | `e_escaping_value_declared_effects`, `e_escaping_value_exceeding_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.951 | §12 | With no effect clause, a call through it has every effect (default 1). | `e_unresolved_callee_has_every_effect` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓; **diverges from the sentence** (B-2026-11-27-21) |
| cs.952 | §12 | Effect variables for stored callbacks arrive with the services track. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.953 | §12 | - The `with _` annotation is removed. | `e_with_underscore_removed` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.972 | §14.1 | **14.1 Declarations.** `effect resource R: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.973 | §14.1 | T1 + T2;` declares a provider-rooted resource whose providers must implement every bound. | `p_provider_must_implement_every_bound`, `p_two_bounds_both_callable` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.974 | §14.1 | `effect resource R;` with no bound is annotation-only: it takes part in effects and conflicts but cannot be bound with `with_provider` (a… | `p_annotation_only_cannot_bind` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.975 | §14.1 | Program-rooted resources (`Env`, `Clock`, `RandomSource`) are declared by the library with a library trait each, and have a default provi… | `p_program_rooted_default_and_override` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.976 | §14.2 | **14.2 Calls on a resource.** `R.m(args)` calls the trait method `m` on the provider bound to `R`. | `p_two_bounds_both_callable` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.977 | §14.2 | The receiver decides the verb: a bare `self` is `reads(R)`, a `mut ref self` is `writes(R)`, and an `own self` method cannot be called th… | `p_own_self_through_resource_refused`, `p_writes_verb_from_mut_ref_self`, `p_reads_verb_runs_apart` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.978 | §14.2 | A method that no bound declares is an error at the call, even when the provider type in some binding has it: the bound is the interface, … | `p_unbound_method_refused` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.979 | §14.3 | **14.3 The provider is a hidden borrowed parameter.** Every function whose effects include `reads(R)` or `writes(R)` takes one hidden par… | `p_hidden_param_through_callees` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.980 | §14.3 | `ref P` if it only reads, `mut ref P` if it writes. | `p_reads_verb_runs_apart`, `p_writes_verb_from_mut_ref_self` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.981 | §14.3 | A function that reaches `R` only through callees takes it too, and passes it on. | `p_hidden_param_through_callees` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.982 | §14.3 | Private functions get this from effect inference (§12); a public function's declared effects are part of its calling convention, so an un… | `p_pub_undeclared_resource_refused` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.983 | §14.3 | Each function is specialised for each provider type the program binds to `R`, as a generic is (§12, "computed on the monomorphised instan… | `p_specialised_per_provider` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.984 | §14.3 | Taking such a function as a value captures the provider in scope, as a closure would (14.5). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.985 | §14.4 | **14.4 `with_provider[R](p, f)`.** `p` moves into the call, which owns it. | `p_with_provider_value_then_drop` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.986 | §14.4 | `f` is a non-escaping closure. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.987 | §14.4 | Inside `f`, `R` is bound to `p`, borrowed as 14.3 says, and the call supplies the hidden argument, so `R` is not among the enclosing func… | `p_provider_not_in_enclosing_effects` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.988 | §14.4 | The value of the call is `f`'s value. | `p_with_provider_value_then_drop` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.989 | §14.4 | `p` drops when the call returns, after `f`'s value is computed (§7). | `p_with_provider_value_then_drop` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.990 | §14.4 | `P` must implement every bound of `R`. | `p_provider_must_implement_every_bound` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.991 | §14.4 | An inner `with_provider[R]` shadows an outer one for its extent. | `p_inner_shadows_outer` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.992 | §14.5 | **14.5 A provider cannot leave its scope.** A closure that uses `R` captures the hidden reference, which is a view of `p` (§5). | `p_closure_escape_refused` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.993 | §14.5 | So returning it, storing it, sending it, or handing it to a task that may outlive the call is the §5.7 / §9.1 error, and the diagnostic n… | `p_closure_escape_refused` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.994 | §14.5 | Plain data computed from `R` leaves freely. | `p_plain_data_leaves` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.995 | §14.5 | The same holds for a program-rooted resource under an override (14.7): a callback stored in a field, such as an event bus's handler list,… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.996 | §14.5 | Outside an override it uses the static default and captures nothing. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.997 | §14.5 | In v1, test such a callback by calling it directly under the override, or store one that does not use the overridden resource; a struct t… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.998 | §14.5 | (Recorded 2026-10-30 from the v1 integration pin, `tests/fixtures/v1_service`.) A `dyn` value is the same: it uses the providers in scope… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.999 | §14.5 | A `with_provider` around a later call through the value does not help, because the value has no room to carry the provider ([design.md § … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1000 | §14.6 | **14.6 No provider is a compile error.** A call that needs `R` where nothing supplies it is an error at that call (`E_NO_PROVIDER`), nami… | `p_no_provider_is_compile_error` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1001 | §14.6 | This replaces `deferred.md`'s runtime panic. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1002 | §14.6 | For a provider-rooted `R`, `main` and a `test` body supply nothing unless they enter a `with_provider[R]` (or `#[with_provider]`, 14.8). | `p_no_provider_is_compile_error` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1003 | §14.6 | A function value whose type declares no effects has every effect (§12 default 1) except provider-rooted resources: by 14.5 no such value … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1004 | §14.7 | **14.7 Program-rooted resources.** `Env`, `Clock` and `RandomSource` have a default provider, a static value of a library type, in scope … | `p_program_rooted_default_and_override` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1005 | §14.7 | A program that never overrides one compiles every function against that default only and pays nothing. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1006 | §14.7 | Inside `with_provider[Env](fake, // ..)` the override is an ordinary provider: a closure that uses `Env` there captures `ref fake` and ca… | `p_program_rooted_default_and_override` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1007 | §14.7 | `FileSystem`, `Network`, `Stdout`, `Stderr` and `Stdin` are program-rooted the same way (14.10 to 14.13). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1008 | §14.7 | A default is declared after the bounds, `effect resource Env: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1009 | §14.7 | EnvProvider = OsEnv {};`, with a constant expression of a type that implements every bound; a resource with a default is program-rooted, … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1010 | §14.7 | The library's provider traits (`EnvProvider`, `ClockProvider`, `RandomProvider`) give every method a default body that panics, naming the… | `p_trait_default_body_panics` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1011 | §14.7 | So a fake states only the methods it uses, a test never reaches the real environment, clock or random source by accident, and a method ad… | `p_trait_default_body_panics` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1012 | §14.7 | A draw advances the source, so `RandomProvider`'s methods take `mut ref self` and charge `writes(RandomSource)` (14.2): two draws keep th… | `p_random_draw_is_writes` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1013 | §14.7 | (Ruled 2026-10-20 by the review thread, asked by Thread C.) | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1014 | §14.8 | **14.8 Sugar.** `providers { R1 => e1, R2 => e2 } in { body }` evaluates `e1`, `e2`, ... in order (a `?` among them propagates), then nes… | `p_providers_sugar_in_order` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1015 | §14.8 | `#[with_provider(R, e)]` on a `test` runs the body inside `with_provider[R](p, // body)`, where `p` is `e` evaluated afresh for each test… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1016 | §14.8 | `e` is an expression (`FakeClock { t: 42 }`, `FakeClock.at(1_700_000_000)`, `make_db()`), except that a path naming a function is called … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1017 | §14.8 | `InMemoryUserDB.new` means `InMemoryUserDB.new()`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1018 | §14.8 | If `p` is a `Result`, an `Err` fails the test before the body runs, with reason `provider_construction_failed`, as does a panic while eva… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1019 | §14.8 | (Clarified 2026-10-22.) | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1020 | §14.9 | **14.9 Concurrency.** Branches and tasks inside the call borrow the provider like any captured place. | `p_reads_verb_runs_apart` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.1021 | §14.9 | Several that only read `R` share a `ref P`, which requires `P` to be `CrossTask` (`E_NOT_CROSS_TASK` at the `with_provider`). | `p_reads_verb_runs_apart`, `p_not_cross_task_provider_refused` | interp ✓ jit ✓ build ✓ asan ✓ tsan ✓ |
| cs.1022 | §14.9 | A branch that writes `R` conflicts with any other that uses it (§11.2), with `R` as the place. | `p_writes_verb_from_mut_ref_self` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1023 | §14.10 | **14.10 Overriding the standard streams.** `Stdout`, `Stderr` and `Stdin` are program-rooted, with the bounds `StdoutProvider`, `StderrPr… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1024 | §14.10 | `write(mut ref self, s: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1025 | §14.10 | Str)` and `flush(mut ref self)` are the output methods; `read_line` and `read_to_string` the input ones. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1026 | §14.10 | Every method takes `mut ref self`, because each call advances the stream, so every use charges `writes(R)` (14.2) and two uses keep their… | `p_stdin_read_is_writes` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1027 | §14.10 | That includes reading standard input, which is `writes(Stdin)`, as a draw is `writes(RandomSource)`: two reads of one stream never run in… | `p_stdin_read_is_writes` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1028 | §14.10 | Under `with_provider[Stdout](fake, // ..)`, `print`, `println` and every other library writer to standard output call `fake.write` with t… | `p_stdout_override_captures_println` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1029 | §14.10 | Outside an override they write to the process's stream directly, with no intermediate `String`. | `p_stdout_override_captures_println` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1030 | §14.10 | **The panic path never uses a provider.** A panic message, a failed `assert`, `dbg` and the runtime's own warnings always go to the proce… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1031 | §14.11 | **14.11 Overriding `FileSystem`.** `FileSystem`'s bound is `FileSystemProvider`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1032 | §14.11 | Each path operation in [library/io.md](library/io.md) (`read_to_string`, `write`, `read_lines`, `exists`, `create_dir`, `remove_file`, `r… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1033 | §14.11 | **Handles.** `File` is one type whatever provides it. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1034 | §14.11 | A file opened under an override belongs to that override's provider: reads, writes and its drop go to that provider. | `p_fs_file_after_provider_returns` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1035 | §14.11 | Once the `with_provider` call has returned, every operation on such a file returns `Err(IoError.ProviderUnavailable)` and its drop does n… | `p_fs_file_after_provider_returns` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1036 | §14.11 | While that provider is live, its files reach it even inside a nested `with_provider[FileSystem]`: each `with_provider` call registers its… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1037 | §14.11 | A file opened outside every override is the operating system's and works anywhere. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1038 | §14.11 | `BufReader` and `BufWriter` over such a file behave the same way. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1039 | §14.11 | A fake implements handles through the trait's `open`, `read_handle`, `write_handle` and `close` methods, which name a handle by the fake'… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1040 | §14.11 | On a fake's file, `flush` and the `sync` methods return `Ok(())`, and `seek` and `set_len` return `Err(IoError.Unsupported)`. | `p_fs_file_after_provider_returns` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1041 | §14.11 | The library calls a fake's handle methods with every other resource at its default, so a handle method that reaches a resource overridden… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1042 | §14.11 | (Chosen 2026-10-28 over an associated `File` type per provider, which would make `File` mean a different type in each specialisation.) | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1043 | §14.12 | **14.12 Overriding `Network`.** `Network`'s bound is `NetworkProvider`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1044 | §14.12 | In v1 it covers the HTTP client and plain TCP: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1045 | §14.12 | `send(mut ref self, req: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1046 | §14.12 | Request) -> Result[Response, HttpError]`, through which `http.get`, `Client` and `RequestBuilder.send` route, and the TCP `connect`, `acc… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1047 | §14.12 | Under an override of `Network`, TLS, WebSocket, `UdpSocket` and the `Server.serve` family are refused by the check (`E_PROVIDER_UNSUPPORT… | `p_network_override_unsupported_refused` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1048 | §14.12 | Test a handler by calling it directly with a `Request`. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1049 | §14.12 | A fake's methods run to completion: they never suspend, and the scheduler's reactor never sees a fake's handles. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1050 | §14.12 | **Shapes.** `Request` is a value a program can build (`Request.new(method, url)`, `with_header`, `with_body`), so a fake's `send` and a t… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1051 | §14.12 | The TCP methods are `bind`, `connect`, `accept`, `read_stream`, `write_stream`, `shutdown` and `close`, naming handles by the fake's own … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1052 | §14.12 | On a fake's socket, `try_clone` and `local_addr` return `Err(NetError.Unsupported)`, and `set_read_timeout` returns `Ok(())` and changes … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1053 | §14.12 | In a program that binds `Network`, stream reads and writes charge `Network` rather than the socket value, so two `par` branches on two so… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1054 | §14.12 | **Effects.** Network calls keep the verbs the library declares (`sends(Network)`, `receives(Network)`), so two clients of the real networ… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1055 | §14.12 | Under an override they still conflict through the provider: each routed call borrows the fake `mut ref` (14.3), so two `par` branches tha… | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1056 | §14.13 | **14.13 `#[test(requires: [R, ..])]`.** Each `R` must be program-rooted (an error otherwise, naming `R`), so that a test which runs uses … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1057 | §14.13 | `karac test` skips such a test unless `KARA_RESOURCE_<NAME>=1` is set for each `R`, reporting `unsatisfied_requires`; `karac test --all` … | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1058 | §14.14 | **14.14 Program-rooted resources declared by a program.** A program may declare `effect resource R: | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1059 | §14.14 | T = e;` (14.7). | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1060 | §14.14 | `e` is a constant expression, evaluated once before `main`, into one static value per resource. | `p_default_static_never_dropped` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1061 | §14.14 | The static is never dropped, so a `Drop` impl on its type never runs for it. | `p_default_static_never_dropped` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1062 | §14.14 | **A default with state is read-only.** If the default's type has fields, calls on `R` outside an override may use only `self` methods, be… | `p_stateful_default_read_only` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1063 | §14.14 | A default with no fields, such as the library's `OsRandom`, may have `mut ref self` methods. | — | existing pin (CORE_SENTENCE_MAP.md) |
| cs.1064 | §14.14 | Under an override either kind of method may be called on the override's provider. | `p_stateful_default_read_only` | interp ✓ jit ✓ build ✓ asan ✓ |
| cs.1065 | §14.14 | (Rulings 2026-10-28, asked by Thread C.) | — | existing pin (CORE_SENTENCE_MAP.md) |
