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
                      Flags: --bin (default) or --lib; --force overrides
                      the abort when kara.toml / src/main.kara /
                      src/lib.kara already exist in the CWD form.
    test [<filter>]   Run the project's tests. Walks the project root,
                      discovers `_test.kara` files, and invokes every
                      `test_*` function via the interpreter. Output is
                      JSONL on stdout (see `docs/design.md § Testing`).
                      Optional positional substring filter limits which
                      tests run by qualified ID
                      (`<module_path>::<fn_name>`).
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
    --example NAME     Run examples/<NAME>.kara (or examples/<NAME>/src/main.kara)
    --output=json      Structured JSON output on stdout
    --output=jsonl     Streaming JSONL output on stdout
    --sequential       Disable parallel execution in `par` blocks
    -h, --help         Print this message"
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
    -h, --help              Print this message

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
        "init" => {
            "\
karac init - Scaffold a new Kara project

USAGE:
    karac init [<name>] [--bin | --lib] [--force]

ARGS:
    <name>    When provided, creates `./<name>/` and scaffolds there.
              Must match `[a-z][a-z0-9_]*` and not be a reserved keyword;
              the same string is used as the package name. When omitted,
              scaffolds into the current directory and derives the package
              name from the directory basename.

FLAGS:
    --bin              Binary project (default): writes `src/main.kara`.
    --lib              Library project: writes `src/lib.kara` with a
                       sample `pub fn add`.
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
    <filter>   Optional substring matched against each test's
               fully-qualified ID (`<module_path>::<fn_name>`). Only tests
               whose ID contains this substring run; the others are
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
