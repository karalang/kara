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
                        effects        - inferred and declared effects
                        ownership      - parameter modes
                        concurrency    - parallelization opportunities
                        cost-summary   - static counts of compiler-emitted
                                         silent runtime costs (RC ops,
                                         Arc-provider wraps, borrow flags)
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
    -h, --help              Print this message"
        }
        "query" => {
            "\
karac query - Query compiler analysis

USAGE:
    karac query <kind> <target>
        <target> = <file.kara>.<function>   for per-function kinds
                 = <file.kara>              for cost-summary

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

OPTIONS:
    -h, --help         Print this message"
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
        _ => {
            print_help();
            return;
        }
    };
    println!("{text}");
}
