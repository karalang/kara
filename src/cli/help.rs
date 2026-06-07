pub(super) fn print_help() {
    println!(
        "\
karac - The Kara language compiler

USAGE:
    karac <command> [options] <file.kara>
    karac <file.kara>                  (shorthand for 'karac run')

COMMANDS:
    run <file>        Run a .kara program
    run --example NAME
                      Run an example from the examples/ directory.
                      Single-file examples: examples/<NAME>.kara
                      Project examples:     examples/<NAME>/src/main.kara
    check <file>      Type-check without executing
    build [file]      Compile (check + effects + ownership).
                      With no <file>, builds the current project: walks up
                      from CWD to find `kara.toml` and compiles every
                      `.kara` under `src/`. (Project-mode file walker lands
                      in CR-24 slice 3; slice 2 only verifies the manifest.)
    init [<name>]     Scaffold a new Kāra project. Bare `karac init`
                      scaffolds into the current directory; `karac init
                      <name>` creates `./<name>/` and scaffolds there.
                      Flags: --bin (default), --lib, or --backend;
                      --force overrides the abort when kara.toml /
                      src/main.kara / src/lib.kara already exist in the
                      CWD form.
    new <name>        Create a new Kāra project in `./<name>/`. Mirrors
                      `cargo new` vs `cargo init`: positional name
                      required; --backend (default) for HTTP server
                      skeleton, --lib for library, --cli for command-
                      line tool. --data reserved for the Kafka pipeline
                      scaffold (deferred — phase-8 line 63 sub-entry).
    test [<filter>]   Run the project's tests. Walks the project root,
                      discovers `_test.kara` files, and runs every
                      `test \"case name\" {{ body }}` declaration via the
                      interpreter. Output is JSONL on stdout (see
                      `docs/design.md § Testing`). Optional positional
                      substring filter matches against each case's name
                      string.
    query <kind> <target>
                      Query compiler analysis. Per-function kinds take
                      `<file>.<function>` as target; `cost-summary` takes
                      a bare `<file>` (whole-file aggregate).
                        effects          - inferred and declared effects
                        ownership        - parameter modes
                        concurrency      - parallelization opportunities
                        cost-summary     - static counts of compiler-emitted
                                           silent runtime costs (RC ops,
                                           Arc-provider wraps, borrow flags)
                        monomorphization - per-generic instantiation counts
                                           + per-instance type tuples
                        affected-by      - call-graph reach query (callers,
                                           callees, reaching tests)
    catalog <file>    Emit the file's public API surface as JSONL on stdout
                      (one record per exported item with signature shape,
                      generics, parameters with modes, return type,
                      declared effects, refinement constraints, and span).
    explain --concept=NAME
                      Print a concept-level explainer page. Available
                      concepts:
                        closures       - capture-mode inference + the
                                         own / ref / mut ref prefixes
    fmt <file>        Format a .kara file
    fix <file>        Apply machine-applicable suggestions (e.g. resolver
                      `did you mean` corrections) to a .kara file. Use
                      --dry-run to preview without writing.
    migrate shared-to-par <Type> [<file>]
                      Preemptively migrate a `shared struct` to `par struct`.
                      Rewrites the type definition (keyword rename, `mut `
                      strip, `Mutex[T]` wrap on every mut field) and every
                      consumer site (assigns, compound-assigns, reads, and
                      mutating method calls wrap in `lock self.field {{ ... }}`).
                      With `<file>` omitted, walks every module under the
                      project's `src/` (discovered via `kara.toml`). Dry-run
                      by default; use --apply to write. The dirty-workspace
                      guard refuses --apply on a non-clean tree unless
                      --force is set.
    repl              Launch the interactive REPL. Items (fn/struct/...)
                      accumulate across cells; statement cells run as the
                      body of an implicit `fn main()`. Type :help inside
                      the REPL for the meta-command list. Pass
                      `--auto-clone` to opt into cross-cell ergonomics
                      (auto-insert `.clone()` at consume sites; emits a
                      `perf[auto-clone-in-repl]` note on every insertion).
    doc               Render HTML documentation under dist/doc/ from the
                      `///` doc comments attached to each public item.
                      MVP — flat per-module layout, no cross-references.
    clean [--global]  Remove the project-local `dist/` cache. With
                      --global, instead removes the user-wide cache
                      at ~/.kara/cache/.
    cache <sub>       Inspect the global build-artifact cache.
                        info  - print cache root + entry count + bytes
                        key   - derive the cache-key digest for a key
                                tuple (--pkg / --version required)
    install <spec>    Build a binary package and install it into
                      ~/.kara/bin/. Spec accepts `path = ...`,
                      `git = ...`, or a registry-proxy reference.
                      (v1 surface — resolver wiring pending.)
    vendor            Copy resolved dependencies into ./vendor/.
                      Pairs with `karac build --offline`. (v1 surface
                      — resolver wiring pending.)
    help              Show this help
    version           Show version

OPTIONS:
    --output=json     Structured JSON output (on stdout)
    --output=jsonl    Streaming JSONL output (on stdout)
    --sequential      Disable parallel execution in par blocks
    -h, --help        Print help. After a subcommand (e.g. `karac init
                      --help`), prints help scoped to that subcommand."
    );
}

pub(super) fn has_help_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

pub(super) fn print_subcommand_help(subcmd: &str) {
    let text = match subcmd {
        "run" => {
            "\
karac run - Run a .kara program through the interpreter

USAGE:
    karac run <file.kara> [options]
    karac run --example NAME [options]
    karac <file.kara>                  (shorthand)

OPTIONS:
    --example NAME      Run examples/<NAME>.kara (or examples/<NAME>/src/main.kara)
    --output=json       Structured JSON output on stdout
    --output=jsonl      Streaming JSONL output on stdout
    --sequential        Disable parallel execution in `par` blocks
    --manifest=<path>   Override manifest discovery — load the supplied
                        kara.toml instead of walking upward from the
                        script's directory. Mutually exclusive with
                        --no-manifest. Accepts the space-separated
                        `--manifest <path>` form too.
    --no-manifest       Skip manifest discovery entirely (run stdlib-
                        only, ignore any enclosing project's
                        [package].profile and karac-toolchain.toml pin).
                        Mutually exclusive with --manifest.
    --timeout DURATION  Opt-in wall-clock cap on the interpreter. No
                        default — long-running services / daemons /
                        REPLs are legitimate workloads, so a default
                        would silently break real operations. Useful
                        for CI smoke tests, scripted invocations, and
                        ad-hoc `karac run` where forgetting about a
                        runaway costs real laptop battery. On
                        timeout, exits with code 124 (matching GNU
                        timeout(1)). Duration formats: '60' (bare
                        integer = seconds), '500ms', '5m', '1h'.
                        Accepts the `=`-separated `--timeout=60s`
                        form too.
    -h, --help          Print this message

MANIFEST DISCOVERY:
    By default, `karac run path/to/foo.kara` walks upward from
    `dirname(path/to/foo.kara)` looking for kara.toml — the script is
    treated as belonging to the project containing it. The project's
    [package].profile becomes the pipeline's active profile, and any
    karac-toolchain.toml pin along the ancestor chain is enforced.
    Scripts outside any project (no kara.toml found anywhere in the
    ancestor chain) run stdlib-only without comment."
        }
        "check" => {
            "\
karac check - Type-check a .kara file without executing it

USAGE:
    karac check <file.kara> [options]

OPTIONS:
    --output=json           Structured JSON output on stdout
    --output=jsonl          Streaming JSONL output on stdout
    --profiles=<list|all>   Run the full pipeline once per profile and
                            group diagnostics per profile. Comma-separated
                            list (`embedded,kernel`) or `all` for every
                            known profile (default, embedded, kernel).
                            Useful for CI matrices that advertise
                            multi-profile compatibility. Exits non-zero
                            if any profile fails.
    --targets=<list|all>    Run the full pipeline once per v1 compilation
                            target (native, wasm_browser, wasm_wasi, gpu),
                            parameterizing the target-provided resource
                            set each time. Diagnostics are tagged with
                            the producing target; findings identical on
                            every target are reported once as
                            target-agnostic. Defaults to the discovered
                            manifest's `[build].targets` when the flag is
                            absent. Mutually exclusive with --profiles.
                            Exits non-zero if any target fails.
    --concurrency-report    Print a human-readable summary of the auto-par
                            analyzer's per-function parallel groups to
                            stdout alongside the check output. Same shape
                            as `karac build --concurrency-report`.
    -h, --help              Print this message"
        }
        "build" => {
            "\
karac build - Compile a .kara file or the current Kara project

USAGE:
    karac build [<file.kara>] [options]

With a file, compiles that single file. Without a file, builds the current
project: walks up from CWD to find `kara.toml` and compiles every `.kara`
under `src/`. (The multi-file pipeline lands in CR-24 slice 3; slice 2 only
verifies the manifest.)

OPTIONS:
    --output=json           Structured JSON output on stdout
    --output=jsonl          Streaming JSONL output on stdout
    --concurrency-report    Print a human-readable summary of the auto-par
                            analyzer's per-function parallel groups (with the
                            calls in source order, their reads/writes effects,
                            and the analyzer's reason for parallelizing) to
                            stdout alongside the binary build.
    --release               Strip debug-only runtime instrumentation from the
                            binary: contracts (requires / ensures / old /
                            invariant) and the `?`-error-return trace — both
                            checked/recorded in debug builds, stripped in
                            release per design.md. Optimization is already -O2
                            by default (see KARAC_OPT_LEVEL), so --release
                            removes runtime cost rather than turning the
                            optimizer on. Works in both single-file and project
                            mode. Composes (OR) with the KARAC_STRIP_CONTRACTS
                            and KARAC_STRIP_ERROR_TRACE env vars.
    --enable-hot-swap       Emit PLT-style indirection for extern-public
                            module symbols so the AOT artifact format stays
                            forward-compatible with the post-v1 continuous-
                            PGO + shared-object reload story. Off by default.
                            Incompatible with embedded / kernel profiles.
    --no-proxy              Opt out of the registry proxy at
                            proxy.kara-lang.org (override the URL with the
                            KARAC_REGISTRY_PROXY env var). Registry / git
                            deps would then have to be fetched
                            direct-from-source — a v1.1.x carve-out; today
                            the flag is honored at the parse layer and
                            surfaces a confirmation note.
    --offline               Read every transitive path dependency from
                            `./vendor/<name>/` instead of the manifest-
                            declared path, and refuse any network access.
                            Run `karac vendor` first to populate
                            `./vendor/` from the current resolution.
                            Implies --no-proxy (the redundant note is
                            suppressed). See the OFFLINE section below.
    --target=<name|triple>  Compilation target. A v1 target NAME
                            (native, wasm_wasi, wasm_browser; gpu is
                            recognized but not standalone-buildable)
                            selects the cross-compile target:
                            `--target=wasm_wasi` emits `<stem>.wasm`,
                            a WASI preview-1 command module;
                            `--target=wasm_browser` emits `<stem>.wasm`
                            plus `<stem>.js` ES-module glue (host fn
                            imports + WASI polyfill) and `<stem>.d.ts`
                            TypeScript declarations for browser hosts.
                            Project mode (no file argument) lands the
                            same set as `dist/wasm/<pkg>.{wasm,js,d.ts}`
                            with the package name from kara.toml.
                            Any other value is a target TRIPLE selecting which
                            `[target.<triple>.*]` overlay merges onto
                            the manifest (dependencies, dev-
                            dependencies, profile). Precedence:
                            --target=<triple> > [build].target from the
                            manifest > host triple. Accepts the space-
                            separated form `--target <triple>` too.
                            See the TARGETS section.
    --bindings=<mode>       WASM output shape: browser (emit `.js`
                            ES-module glue + `.d.ts` TypeScript
                            declarations next to the `.wasm`),
                            component (a single embedded-WIT Component
                            Model `.wasm` that wasmtime/jco-class hosts
                            run directly; componentized via the
                            external `wasm-tools` binary — install with
                            `cargo install wasm-tools`, pin the exact
                            version via `[toolchain] wasm-tools = <v>`
                            in kara.toml, point KARAC_WASM_TOOLS at a
                            specific binary), or none (raw `.wasm`,
                            no glue).
                            Default is inferred from the target:
                            wasm_browser -> browser, wasm_wasi ->
                            component. Ignored on non-WASM targets.
                            Works in single-file and project mode.
                            Accepts `--bindings <mode>` too.
    --target-cpu=<name>     CPU baseline override for codegen (e.g.
                            `apple-m4`, `x86-64-v3`, `neoverse-v1`).
                            `--target-cpu=help` lists the supported
                            CPUs for the active target; any other
                            unknown name is a hard error carrying that
                            same listing. Precedence: this flag >
                            KARAC_TARGET_CPU env var > `[release]
                            target-cpu` in kara.toml > the per-target
                            default baseline (aarch64-darwin: apple-m1,
                            x86_64-linux: x86-64, aarch64-linux:
                            generic+v8a, x86_64-darwin: core2).
                            Widening the baseline narrows the deploy
                            set in exchange for sharper codegen.
                            Accepts `--target-cpu <name>` too.
    --target-features=<list>
                            Feature-string override for codegen — a
                            comma-separated `+feat`/`-feat` list (e.g.
                            `+aes,-sve`) appended after the per-target
                            default features; later entries win, so
                            `-feat` disables a default. Every entry
                            needs its `+`/`-` prefix and must name a
                            feature for the active target — hard error
                            otherwise. `--target-features=help` lists
                            them. Own precedence chain: this flag >
                            KARAC_TARGET_FEATURES env var > `[release]
                            target-features` in kara.toml > defaults.
                            Composes with --target-cpu. Accepts
                            `--target-features <list>` too. On wasm
                            targets `+simd128` is the default (Vector
                            ops lower to v128); `-simd128` opts back
                            down to an MVP-clean scalarized module.
    --monomorphization-budget=warn:N,error:M
                            Cap per-generic instantiations. After typecheck
                            (before codegen), any generic instantiated >= N
                            times emits warning[monomorphization-budget];
                            >= M fails the build. Either threshold may be
                            given alone; warn must be <= error. Off by
                            default (opt-in; default thresholds are a v1.x
                            follow-up). Single-file build only. Inspect the
                            same counts with `karac query monomorphization`.
    -h, --help              Print this message

TARGETS:
    v1 target names (`native`, `wasm_browser`, `wasm_wasi`, `gpu`)
    select the COMPILATION target: `#[target(...)]`-gated items,
    the per-target provided-resource effect gate (E0411), codegen,
    and the link step all key on it. `--target=wasm_wasi` builds a
    headless WASM module (`<stem>.wasm`, runnable under wasmtime /
    node:wasi). `--target=wasm_browser` builds the same wasip1
    module flavor plus `<stem>.js` ES-module glue and `<stem>.d.ts`
    TypeScript declarations: every `host fn` becomes a WASM import
    under the `kara_host` namespace the glue wires to your
    implementations, and the glue's inline WASI polyfill replaces
    the WASI host (works in browsers and node; bundlers need no
    custom loader). In project mode the artifacts land as
    `dist/wasm/<pkg>.{wasm,js,d.ts}` named from kara.toml's
    `[package].name`. Both targets require the wasm runtime
    archive (`libkarac_runtime_wasm.a`) and a wasm linker (wasm-ld
    or rust-lld; override with KARAC_WASM_LD).

    Any other `--target=<triple>` value selects the active target
    triple for overlay merge. Any `[target.\"<triple>\".dependencies]`,
    `[target.\"<triple>\".dev-dependencies]`, and
    `[target.\"<triple>\".profile]` table that matches the active
    triple is merged into the corresponding base table before dep
    resolution. Overlay entries with the same name as a base entry
    win (most-specific = later). With no `--target=` flag and no
    `[build].target` in the manifest, the host triple is used
    (matching `karac cache key` defaults).

OFFLINE:
    `--offline` redirects every transitive `path` dependency to
    `<project>/vendor/<dep-name>/`. The vendored manifest is the source
    of truth — version mismatches with the declared path don't apply.
    If `./vendor/` does not exist when `--offline` is set on a project
    with declared dependencies, the build halts with
    `error[E_OFFLINE_NO_VENDOR_DIR]` and points the operator at
    `karac vendor`. A vendor directory present but missing one entry
    surfaces `error[E_OFFLINE_VENDOR_ENTRY_MISSING]` naming the
    offending dependency.

    v1 status:
        Path-source deps are fully wired through `--offline`. Registry
        and git deps in offline mode still surface
        `E_REGISTRY_DEP_UNSUPPORTED` / `E_GIT_DEP_UNSUPPORTED`
        (promoted to a hard error in offline mode rather than the
        default warning) — registry / git vendoring lands alongside
        the package-fetch surface (tracker line 845)."
        }
        "query" => {
            "\
karac query - Query compiler analysis

USAGE:
    karac query <kind> [flags] <target>
        <target> = <file.kara>.<function>   for per-function kinds
                 = <file.kara>              for cost-summary, attributes, queries,
                                            monomorphization

KINDS:
    effects            Inferred and declared effects
    ownership          Parameter modes (own / ref / mut ref)
    concurrency        Parallelization opportunities
    cost-summary       Whole-file static counts of every silent
                       runtime cost the compiler emitted: RC ops
                       (Rc/Arc), Arc-provider wraps, borrow-flag
                       fields, partition-guard sites, auto-clone
                       insertions. Per design.md § Compiler Query
                       API. v1 reports static counts only —
                       runtime attribution is post-v1.
    attributes         JSON list of every multi-segment attribute
                       (`#[diagnostic::*]`, `#[karafmt::*]`, etc.).
                       Tool-facing read surface for the
                       tool-namespaced-attribute work. Accepts
                       `--tool=PREFIX` to filter by first-segment
                       match (`--tool=karafmt` returns every
                       `#[karafmt::*]` occurrence).
    queries            Compiler queries channel — JSON envelope
                       collating every pipeline-phase-emitted
                       `CompilerQuery`. v1 ships an empty array
                       while the catalogue is being populated; the
                       command surface is stable so external
                       tooling can integrate now. See design.md
                       § Specification Layers > Compiler Queries.
    monomorphization   Per-generic instantiation table — one entry
                       per generic function with its distinct
                       `(T1..Tk)` tuples plus the first call site
                       that produced each. Surfaces the cost named
                       in design.md § Effect Polymorphism > Cost
                       Properties. v1 reports type tuples; the
                       per-instance `effects` slot is reserved and
                       always empty in v1.
    affected-by        Call-graph reach query. Target forms:
                         <file.kara>                whole-file seed
                         <file.kara>:<line>         single line seed
                         <file.kara>:<lo>-<hi>      inclusive range
                         <file.kara>:<fn|Type.fn>   single fn seed
                       Emits one JSONL record with `callers`,
                       `callees`, and reaching `tests` arrays. See
                       design.md § AI-First Compiler Interface and
                       `docs/deferred.md § karac query affected-by`.

OPTIONS:
    --tool=PREFIX                 attributes only: first-segment match filter
    --tests-only                  affected-by only: emit just the `tests` array
    --direction=callers|callees|all
                                  affected-by only (default `all`):
                                  `callers` suppresses callees,
                                  `callees` suppresses callers + tests.
    -h, --help                    Print this message"
        }
        "fmt" => {
            "\
karac fmt - Format a .kara file and print the result to stdout

USAGE:
    karac fmt <file.kara>

OPTIONS:
    -h, --help         Print this message"
        }
        "fix" => {
            "\
karac fix - Apply machine-applicable suggestions to a .kara file

USAGE:
    karac fix <file.kara> [--dry-run]

DETAILS:
    Runs the full single-file pipeline (resolve → typecheck → lower →
    ownership → ...) and applies every diagnostic that carries a precise
    byte-range replacement. Coverage today:
      - Resolver: `did you mean` corrections on undefined names /
        undefined types / unknown imports / unknown items.
      - Ownership: closure prefix rewrites (e.g. `mut ref` → `ref` when
        the closure body never mutates the capture; N0507 perf note).
    Each `TextEdit { offset, length, replacement }` is applied in
    reverse byte-offset order so earlier edits don't invalidate later
    offsets. Other diagnostic kinds carry descriptive (sentence)
    suggestions and are NOT auto-applied; they remain visible through
    `karac check`.

OPTIONS:
    -n, --dry-run      Print the would-be rewrites instead of writing
                       them to disk. Each line shows
                       `<file>:<line>:<col>: \\`old\\` -> \\`new\\``.
    -h, --help         Print this message"
        }
        "migrate" => {
            "\
karac migrate - Preemptive type migration tool

USAGE:
    karac migrate shared-to-par <Type> [<file.kara>] [--apply] [--force] [--no-atomic]

DETAILS:
    Rewrites a `shared struct <Type>` definition to `par struct <Type>`,
    wrapping each bare `mut` field in an atomic wrapper:
      - Keyword rename:    `shared struct Foo` → `par struct Foo`
      - Mut keyword strip: `mut field: T`     → `field: T`
      - Field type wrap:   `field: T`         → `field: Atomic[T]` / `Mutex[T]`
    Same edit emitter the `E_CONCURRENT_SHARED_STRUCT` fix-diff path
    uses (`karac fix` against a fired diagnostic), but invoked
    preemptively against the type definition rather than at first
    concurrent access.

    In project-mode the L215c Atomic[T] heuristic runs by default: a mut
    field whose type is in the lock-free Copy set (`i32`, `i64`, `u32`,
    `u64`, `usize`, `isize`, `bool`) and whose every observed workspace
    write is a bare `=` assignment is wrapped as `Atomic[T]`; everything
    else stays `Mutex[T]`. Pass `--no-atomic` to wrap every mut field as
    `Mutex[T]` instead. Single-file mode is always all-Mutex (no
    workspace visibility for the classifier).

    Consumer sites are rewritten to match the wrapper. `Mutex[T]` fields
    get `lock self.field { ... }` blocks around every read, assign,
    compound-assign, and mutating method call. `Atomic[T]` fields get
    `.store(v, MemoryOrdering.Release)` for writes and
    `.load(MemoryOrdering.Acquire)` for reads. Annotated bindings
    (`let c: Counter = ...`) fire from parse-only data; inferred bindings
    (`let c = make_counter()`) require the file to typecheck.

    Pass `<file.kara>` to migrate a single file. Omit it to run in
    project-mode: the tool discovers the project root via `kara.toml`
    and walks every `.kara` module under `src/`. Exactly one walked
    module must contain `shared struct <Type>`.

    Defaults to dry-run mode: prints each edit's offset, original text,
    and replacement to stdout in source order. `--apply` writes the
    rewrite back to disk. In `--apply` mode the workspace dirty-check
    refuses to run when `git status --porcelain` reports any
    modifications, unless `--force` is passed.

OPTIONS:
    --apply        Write the rewrite back to disk (default: dry-run).
    --force        Bypass the workspace dirty-check guard. Only honored
                   in `--apply` mode (dry-run never writes).
    --no-atomic    Opt out of the L215c Atomic[T] heuristic (on by
                   default in project-mode) and wrap every mut field as
                   `Mutex[T]` with lock-block consumer wraps. `--atomic`
                   is still accepted as an explicit (now redundant)
                   opt-in. No effect in single-file mode, which is
                   always all-Mutex.
    -h, --help     Print this message

EXAMPLES:
    karac migrate shared-to-par Counter
        # project-mode dry-run — walk every module under ./src/
        # (Atomic[T] heuristic on by default)
    karac migrate shared-to-par Counter src/main.kara
        # single-file dry-run (always all-Mutex)
    karac migrate shared-to-par Counter --apply --force
        # project-mode write, even with uncommitted changes
    karac migrate shared-to-par Counter --no-atomic --apply
        # project-mode write, all fields wrapped as Mutex[T]"
        }
        "init" => {
            "\
karac init - Scaffold a new Kara project

USAGE:
    karac init [<name>] [--bin | --lib | --backend] [--force]

ARGS:
    <name>    When provided, creates `./<name>/` and scaffolds there.
              Must match `[a-z][a-z0-9_]*` and not be a reserved keyword;
              the same string is used as the package name. When omitted,
              scaffolds into the current directory and derives the package
              name from the directory basename.

FLAGS:
    --bin              Binary project (default): writes `src/main.kara`
                       with a `Hello, world!` entry point.
    --lib              Library project: writes `src/lib.kara` with a
                       sample `pub fn add`.
    --backend          Backend HTTP server skeleton: writes `src/main.kara`
                       with a `std.http` server on 127.0.0.1:8080, a
                       `/health` endpoint, and manual path-dispatch.
                       Equivalent to `karac new <name> --backend`.
    --force            In the current-directory form, overwrite an existing
                       `kara.toml`, `src/main.kara`, or `src/lib.kara`.
                       `.gitignore` is never overwritten. Has no effect
                       with the positional form (`karac init <name>`
                       always targets a fresh directory).
    -h, --help         Print this message

EXAMPLES:
    karac init                Scaffold a binary project in the current dir
    karac init my_app         Create ./my_app/ as a binary project
    karac init my_lib --lib   Create ./my_lib/ as a library project"
        }
        "new" => {
            "\
karac new - Create a new Kara project in `./<name>/`

USAGE:
    karac new <name> [--backend | --lib | --cli] [--force]

ARGS:
    <name>    Project name (required). Creates `./<name>/` and scaffolds
              into it. Must match `[a-z][a-z0-9_]*` and not be a reserved
              keyword. The same string is used as the package name.

FLAGS:
    --backend          Backend HTTP server skeleton (default): `src/main.kara`
                       binds `std.http`'s `Server.serve` on 127.0.0.1:8080,
                       dispatches manually on `req.path()`, ships a
                       `/health` endpoint. Reinforces the v1
                       \"default-being-backend\" positioning.
    --lib              Library project: `src/lib.kara` with a sample
                       `pub fn add`.
    --cli              Command-line tool: `src/main.kara` with a
                       `Hello, world!` entry point (same shape as
                       `karac init --bin`).
    --data             Reserved for the Kafka pipeline scaffold (consumer +
                       processor + sink). Currently surfaces a structured
                       \"deferred\" diagnostic — the underlying Kafka client
                       surface is not yet shipped. Tracked at phase-8 line 63
                       sub-entry.
    --force            No effect for `karac new` — the positional `<name>`
                       form always targets a fresh directory. Flag accepted
                       for shape compatibility with `karac init`.
    -h, --help         Print this message

EXAMPLES:
    karac new my_api              Create ./my_api/ with the backend skeleton
    karac new my_lib --lib        Create ./my_lib/ as a library
    karac new my_cli --cli        Create ./my_cli/ as a CLI tool

NOTES:
    `karac new` is to `karac init` what `cargo new` is to `cargo init`:
    `new` creates a fresh directory, `init` initializes the current one.
    Both share the same scaffolder; the only differences are the default
    template and the positional-name requirement."
        }
        "repl" => {
            "\
karac repl - Launch the interactive REPL

USAGE:
    karac repl [--auto-clone]

FLAGS:
    --auto-clone   Opt into cross-cell ownership ergonomics. When a cell
                   reuses a binding that an earlier cell consumed, the
                   REPL rewrites the consume site to insert `.clone()` and
                   emits a `perf[auto-clone-in-repl]` note (never silent).
                   Inherited from `phase-5-diagnostics.md` § \"`--auto-clone`
                   opt-in mode\". The rewrite goes into the cell's
                   recorded source so subsequent compilations and `:save`
                   exports see the cloned form. Off by default; without
                   the flag the existing notebook-aware UAM diagnostic
                   surfaces unchanged.
    -h, --help     Print this message"
        }
        "test" => {
            "\
karac test - Run the project's tests

USAGE:
    karac test [<filter>] [--all]

ARGS:
    <filter>   Optional substring matched against each test case's
               name — the string literal between `test` and `{` in a
               `test \"case name\" { body }` declaration. Only cases
               whose name contains this substring run; the others are
               silently dropped (they do not appear as `test_skip`).

OPTIONS:
    --all      Promote skipped tests to failures. By default, tests
               gated by `#[test(requires = [...])]` are skipped silently
               when their resources are unavailable. With `--all`, the
               runner emits `test_fail` for them instead and the
               process exits non-zero. Use in CI when every required
               service must be live.

OUTPUT:
    JSONL on stdout, one event per line. Event types: `run_start`,
    `test_pass`, `test_fail`, `test_skip`, `summary`. See
    `docs/design.md § Testing › Test runner output format` for the full
    schema and forward-compatibility rules.

RESOURCE PROBES:
    For each resource in a test's `requires` list, the runner checks
    (in order):
      1. `[test.resources]` shell command in `kara.toml` — available iff
         the command exits 0.
      2. Env var `KARA_RESOURCE_<UPPER_DOTTED_PATH>` (with `.` → `_`).
         Available iff the variable is set and non-empty.

EXIT CODE:
    0 if every test passed or was skipped under permitted conditions.
    Non-zero if any test failed, or if any test was skipped under `--all`."
        }
        "cache" => {
            "\
karac cache - Inspect the global build-artifact cache

USAGE:
    karac cache info [--output=json|jsonl]
    karac cache key --pkg NAME --version V [--edition E] [--profile P]
                    [--target-triple T] [--compiler-version C]
                    [--output=json|jsonl]

SUB-MODES:
    info    Print the cache root, populated entry count, and total bytes.
            Useful for eyeballing how much disk the cache currently holds.
    key     Derive and print the cache-key digest for the given five-tuple
            (compiler-version, package-version, edition, profile, target-triple)
            plus the package name. `--pkg` and `--version` are required;
            other axes fall back to the active toolchain (compiler version
            from this karac binary, host target triple, edition `2026`,
            profile `default`). Useful for CI to verify expected keys
            without populating the cache first.

OPTIONS (cache info):
    --output=json    JSON envelope on stdout: {\"status\":\"ok\",\"command\":
                     \"cache_info\",\"root\":...,\"entries\":...,\"bytes\":...}
    --output=jsonl   Emits one `cache_info` event with the same fields.
    -h, --help       Print this message.

OPTIONS (cache key):
    --pkg=NAME              Package name (required).
    --version=V             Package version (required).
    --edition=E             Edition slot in the key (default `2026`).
    --profile=P             Profile slot (default `default`).
    --target-triple=T       Target triple slot (default = active host).
    --compiler-version=C    Compiler-version slot (default = active karac).
    --output=json|jsonl     Structured output as above.
    -h, --help              Print this message.

CACHE ROOT:
    Resolved from $KARAC_BUILD_CACHE_ROOT if set (non-empty); else
    ~/.kara/cache/build/. Eviction via `karac clean --global` (which
    targets the umbrella ~/.kara/cache/ root). This subcommand never
    mutates the cache.

V1.1 NOTE:
    Today's compiler does whole-program codegen — no per-dep artifacts
    are written to this cache yet. The subcommand surfaces the typed
    cache protocol so tooling can integrate against it from day one.
    See `docs/implementation_checklist/phase-5-diagnostics.md`."
        }
        "clean" => {
            "\
karac clean - Remove build artifact caches

USAGE:
    karac clean              Remove the project-local `dist/` directory.
    karac clean --global     Remove the user-wide cache at `~/.kara/cache/`.

Bare form deletes ./dist (project artifacts, intermediate IR, link output).
--global form deletes the shared dependency cache that backs cross-project
artifact reuse per `design.md § Package System > Build artifact cache`.
Both forms are idempotent — a missing directory is reported and treated
as success."
        }
        "install" => {
            "\
karac install - Build and install a binary package into ~/.kara/bin/

USAGE:
    karac install <bin-spec>

The <bin-spec> mirrors the manifest dependency-entry vocabulary in a
CLI-friendly key=value form:
    path=<filesystem-path>      Build from a local source directory.
                                  e.g. `karac install path=./tools/my_tool`
    git=<url>                   Build from a git repository (default branch).
                                  e.g. `karac install git=https://example.com/my_tool.git`
    <name>                      Registry-proxy reference — latest compatible.
                                  e.g. `karac install my_tool`
    <name>@<version>            Pinned registry-proxy reference. <version>
                                  accepts the same comparator syntax as a
                                  manifest dependency entry (`1.2`, `^1.0`,
                                  `=1.2.3`, `>=1.0, <1.5`).
                                  e.g. `karac install my_tool@^1.0`

NAMES:
    Package names follow the manifest / scaffolder convention:
    `[a-z][a-z0-9_]*` — lowercase identifiers, no hyphens. Hyphenated or
    mixed-case input produces an `E_INSTALL_INVALID_NAME` diagnostic with
    a snake_case suggestion when one applies.

INSTALL ROOT:
    `<HOME>/.kara/bin/` by default. Override via `KARAC_INSTALL_ROOT`
    (whitespace-only values ignored — same precedence rule the build
    cache uses for `KARAC_BUILD_CACHE_ROOT`). The directory is created
    on first install.

v1 status:
    Path sources (`path=...`) are fully wired — the project is built
    through the existing pipeline (dep resolution, MSRV check, codegen,
    link) and the produced executable is copied into the install root.
    Git / registry sources surface a forward-compat
    `E_INSTALL_GIT_UNSUPPORTED` / `E_INSTALL_REGISTRY_UNSUPPORTED`
    diagnostic — they activate without spec changes once the
    package-fetch slice lands (tracker line 845). See
    `docs/implementation_checklist/phase-5-diagnostics.md`."
        }
        "explain" => {
            "\
karac explain - Print a concept-level explainer page

USAGE:
    karac explain --concept=NAME

CONCEPTS:
    closures           Closure capture-mode inference (Rule 2),
                       the explicit own / ref / mut ref prefixes
                       (Rule 2½), the K2 conflict table with the
                       exact diagnostic-redirect wording the
                       ownership checker emits, outer-scope routing
                       for own-captured roots, and the
                       `karac query ownership <fn>` inspection
                       surface for per-function inferred capture
                       modes.

OPTIONS:
    -h, --help         Print this message

SEE ALSO:
    karac query ownership <file>.<function>
                       Per-function JSON of inferred parameter modes
                       and per-closure capture modes against a real
                       source file."
        }
        "catalog" => {
            "\
karac catalog - Emit the file's public API surface as JSONL

USAGE:
    karac catalog <file.kara>

Walks the parsed program and prints one JSON record per exported item on
its own line (`fn`, `struct`, `enum`, `trait`, `const`, `type_alias`,
`distinct_type`, `effect_resource`, `extern_fn`, plus `impl_method` rows
for `pub` methods inside `impl` blocks). Each record carries the item's
signature shape: generics with bounds, parameters with modes and types,
return type, declared effect row, refinement constraints, and source
span. Public-surface only — non-`pub` items are skipped because their
inferred reported-tier effect rows are not stable enough to index.

DOWNSTREAM CONSUMERS:
    LLM agents, IDE plugins, doc generators that need a machine-readable
    view of the project's exported API surface.

OPTIONS:
    -h, --help         Print this message"
        }
        "vendor" => {
            "\
karac vendor - Copy resolved dependencies into ./vendor/

USAGE:
    karac vendor [options]

Air-gap workflow for regulated environments and offline CI. Pairs with
`karac build --offline`, which reads dependencies only from ./vendor/
and refuses any network access.

OPTIONS:
    --no-proxy        Opt out of the registry proxy at
                      proxy.kara-lang.org. v1.1.x carve-out; the flag
                      is plumbed today and the path-dep copy is
                      unaffected.
    -h, --help        Print this message

Path-deps are copied verbatim today; registry / git vendoring lands
alongside the registry-proxy fetch surface (tracker line 851)."
        }
        "update" => {
            "\
karac update - Re-resolve dependencies and rewrite kara.lock

USAGE:
    karac update [<pkg>] [options]

OPTIONS:
    --output=json     Structured JSON output on stdout
    --output=jsonl    Streaming JSONL output on stdout
    --no-proxy        Opt out of the registry proxy at
                      proxy.kara-lang.org. v1.1.x carve-out; the flag
                      is plumbed today.
    -h, --help        Print this message

Bare form refreshes every locked package; surgical form targets one
package by name (validation lands in a follow-up slice).

v1.1 status: path-deps are manifest-pinned, so bumping isn't meaningful
today — both forms re-derive the lockfile from the current manifest.
Real version-bumping ships alongside the registry-proxy fetch surface."
        }
        _ => {
            print_help();
            return;
        }
    };
    println!("{text}");
}
