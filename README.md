# Kāra

Kāra is a systems programming language for the age of AI-written code. You declare intent; the compiler handles what language models get wrong (ownership, effects, concurrency) and reports every decision as structured output that an agent can read.

Questions, ideas or design feedback? [Start a GitHub Discussion](https://github.com/karalang/kara/discussions/new/choose). Want to send a patch? [CONTRIBUTING.md](CONTRIBUTING.md) covers the build, the gates and the invariants worth knowing first.

---

## Hello, Kāra

```kara
fn main() {
    println("Hello, world!");
}
```

```bash
karac run hello.kara        # Hello, world!
```

Three things Kāra does that other languages leave to you:

- **Effects drive concurrency.** Functions say what they touch. The compiler checks `par` blocks against those effects, and runs independent work in parallel by itself where it can prove it safe and the work is big enough to pay for a task. There is no `async`/`await` and no function colouring.
- **Ownership without lifetime annotations.** Parameters borrow by default, `own T` takes ownership, and `mut ref T` writes. A returned view's source is inferred from the signature, so there is no `'a` syntax.
- **Every compiler decision as JSON.** Effects, ownership, concurrency and fixes are queryable, so agents (and you) read exactly what the compiler decided.

### The whole idea in one program

Two reads from two data sources, written as plain sequential code. The compiler proves the reads independent, which lets it run them concurrently, and the providers that back them are chosen at run time, so a test can swap in fakes:

```kara
// Two data sources behind one trait. A provider supplies each at run time.
trait Source {
    fn load(self) -> Vec[i64];
}
effect resource UserDB: Source;
effect resource OrderDB: Source;

struct Fixed { rows: Vec[i64] }
impl Source for Fixed {
    fn load(self) -> Vec[i64] { self.rows.clone() }
}

// Private functions: the compiler infers each one's effects from its body.
//   fetch_users  -> reads(UserDB)
//   fetch_orders -> reads(OrderDB)
fn fetch_users() -> Vec[i64] { UserDB.load() }
fn fetch_orders() -> Vec[i64] { OrderDB.load() }

// Parameters borrow by default: no copy, and no lifetime annotation.
fn total(users: Vec[i64], orders: Vec[i64]) -> i64 {
    users.len() + orders.len()
}

fn report() {
    // Two reads of two different resources: nothing shared, no conflict.
    // The compiler proves them independent, so it may run them at once.
    let users = fetch_users();            // statement 0
    let orders = fetch_orders();          // statement 1
    println(f"{total(users, orders)}");   // statement 2: waits on 0 and 1
}

fn main() {
    providers {
        UserDB => Fixed { rows: [1, 2, 3] },
        OrderDB => Fixed { rows: [10, 20] },
    } in {
        report();
    }
}
```

Don't take the concurrency on faith. Ask the compiler what it did:

```bash
$ karac query concurrency app.kara.report      # trimmed
{"function":"report","total_statements":3,
 "parallel_groups":[{"statements":[0,1],"reason":"independent reads on different resources"}], ...,
 "auto_par":[{"function":"report{OrderDB=Fixed, UserDB=Fixed}","kind":"statements","tasks":false,
   "reason":"0 of its 2 statements nest loops or recurse"}]}
```

`parallel_groups` is the proof: the two reads may run at once. `auto_par` is the cost call for this build, made per provider: reads this small cost less than a task, so they run in order. Give `load` real work (nested loops, or recursion) and the same source runs the two reads as concurrent tasks. Either way there is no `par`, no `async` and no thread code in the source.

---

## What makes Kāra different

### An AI-first compiler interface

**Structured diagnostics with machine-applicable fixes.** `karac check --output=json` reports each diagnostic with its phase, code, span and, where the compiler knows the answer, the fix as an exact edit. `karac fix` applies it. Here is the loop on a common mistake, using a value after giving it away:

```kara
fn archive(log: own Vec[String]) -> i64 {
    log.len()
}

fn main() {
    let log = ["boot".to_string(), "ready".to_string()];
    let n = archive(log);
    println(f"{n} archived, last was {log[1]}");
}
```

```bash
$ karac check archive.kara --output=json      # trimmed to the diagnostic
{"diagnostics":[{"severity":"error","phase":"ownership","code":"E0500",
  "class":"OWNERSHIP_MOVE_AFTER_USE","line":8,"column":39,
  "message":"value 'log' moved here, used again here (moved at line 7:21)",
  "replacement":{"offset":151,"length":0,"text":".clone()"}}]}

$ karac fix archive.kara
applied 1 fix(es) to archive.kara

$ karac run archive.kara
2 archived, last was ready
```

No prose was parsed anywhere in that loop.

**Query API.** `karac query <kind> <file>[.<function>]` exposes what the compiler inferred:

| Query | Returns |
|---|---|
| `effects` | inferred and declared effects per function, or the whole program's effect graph |
| `ownership` | parameter modes and closure captures |
| `concurrency` | which statements and loops run in parallel, and why the others do not |
| `cost-summary` | every silent runtime cost the compiler inserted |
| `monomorphization` | which generic instances exist, and where they come from |
| `affected-by` | the callers, callees and tests a change reaches |

**Explanations on demand.** `karac explain --class=<CLASS>` returns the explanation behind any diagnostic class, grounded in the specification. `karac fmt` gives one canonical formatting, so agent-written diffs stay semantic.

### Effects, not async

Every function's effects are known: inferred for private functions, declared for public ones. The compiler uses them to check concurrency, not to colour functions:

- `par { }` and `par for` run branches in parallel, and a conflict between branches is a compile error, not a race.
- `TaskGroup` runs a dynamic number of tasks, such as one per accepted connection. A panic in an isolating group ends only its own task.
- Loops and independent statements are parallelised automatically, only where the effects prove it safe. The result is deterministic: what runs concurrently is decided at compile time.
- Resources are provided, not global: `with_provider` and `providers { } in { }` swap a database, clock or random source for a fake in tests, with no mocking framework.

### Ownership without lifetimes

```kara
struct User { name: String, age: i64 }

// A returned view: the compiler infers that it borrows from `u`.
fn name_of(u: User) -> ref String { ref u.name }

// Two candidates: the result borrows from both, still with no annotation.
fn longer(a: String, b: String) -> ref String {
    if a.len() > b.len() { ref a } else { ref b }
}
```

Moving a value while a view of it is alive is caught at compile time:

```
error[ownership]: cannot move `u` while a borrow into it (a returned reference
  still points at it) is still live
```

The rule is conservative: when a result may borrow from several parameters, it borrows from all of them. Kāra may reject a program a finer analysis would accept, but never accepts one that dangles. Types that need shared ownership say so with `shared struct` and `shared enum`, which are reference-counted. The full model is [docs/core-semantics.md](docs/core-semantics.md).

### Services in the standard library

HTTP/1.1, TLS and WebSocket servers ship in the standard library, with blocking-style code and no function colouring:

```kara
#[allow(prelude_shadow)]
struct Response { status: i64, body: String }

fn handle(req: Request) -> Response {
    Response { status: 200, body: req.path() }
}

fn main() {
    let _ = Server.serve("127.0.0.1:8080", handle);
}
```

More in [`examples/std_net`](examples/std_net).

## Performance

The compiler is held to measured bars before each release: run time and build time against the earlier compiler, binary size, an automatic-parallelism speedup floor, and a load test of 100,000 idle connections with tail-latency limits. Cross-language benchmarks against Rust, C and Go live in **[kara-katas](https://github.com/karalang/kara-katas)**, with the data and chart generator versioned beside the code. Measured on the current compiler: 444 single-file algorithm programs on an Apple M5 ([BENCHMARKS.md](https://github.com/karalang/kara-katas/blob/main/BENCHMARKS.md)), as Kāra's run time divided by the other language's, median:

| Lane | vs | Median |
|---|---|---|
| Sequential | Rust with overflow checks (Kāra always checks) | 1.00× |
| Sequential | Rust / C / Go | 1.03× / 1.15× / 0.93× |
| Automatic parallelism | Rust + rayon, 40 programs | 1.08× (Kāra faster on 14) |

The spread matters as much as the median: 21 programs run over 2× Rust's time (the worst, #163, at 10.2×), and the slowest parallel program is 2.41× rayon's. Builds take about 1.3× `rustc`'s time. These are single-file kernels on one shared laptop, not application benchmarks.

## Targets

- **Native**: Linux x86_64, Linux aarch64 and Apple-silicon macOS in 0.8. Windows follows in a later release.
- **WebAssembly**: plain `wasm32` (`karac build --target=wasm_wasi`) for compute programs.

## Docs

- **[docs/design.md](docs/design.md)**: the language specification.
- **[docs/core-semantics.md](docs/core-semantics.md)**: the normative core (moves, borrows, drops, patterns, providers), each rule pinned by a program in `corpus/core/`.
- **[docs/library/](docs/library/)**: the standard library.
- **[docs/syntax.md](docs/syntax.md)** and **[docs/glossary.md](docs/glossary.md)**: quick reference and terminology.
- **[docs/deferred.md](docs/deferred.md)**: designs for features scheduled after this release.
- **[docs/roadmap.md](docs/roadmap.md)**: what is done and what comes next.

## Project status

Pre-1.0. The current release, [0.8.1](https://github.com/karalang/kara/releases/tag/v0.8.1), is a beta (use it rather than 0.8.0, whose soundness holes it closes; see [CHANGELOG.md](CHANGELOG.md)): the language and tools are complete enough to build real programs, and 1.0 follows outside feedback. The compiler lowers every program to a mid-level IR (MIR), which both the interpreter and the LLVM backend run, and its corpus of thousands of programs with expected outputs is the release gate. `dyn Trait`, effect variables for stored callbacks, and provider overrides for the file system, network and standard streams are in; what remains is release packaging and dogfooding.

Not there yet: Windows and Intel-Mac binaries, the browser WebAssembly target, an HTTP/2 server, and a package ecosystem (C libraries are callable through `extern "C"`). Every bug found is logged in the public ledger, [docs/bugs-v2.jsonl](docs/bugs-v2.jsonl).

## Prior art

| Language | What Kāra takes |
|---|---|
| **Rust** | Ownership, enums, pattern matching, traits, `Result` |
| **Swift** | Parameters that borrow by default, with consumption written (`own`) |
| **Koka** | An effect system, simplified: no handlers, providers instead |
| **Go** | Blocking-style I/O on lightweight tasks |

## Getting started

### Install

Prebuilt binaries for Linux x86_64, Linux aarch64 and Apple-silicon macOS are on the [releases page](https://github.com/karalang/kara/releases). [karac.dev](https://karac.dev/guides/getting-started/) has a copy-paste install that picks your platform. Pick your platform's tarball (`x86_64-linux`, `aarch64-linux` or `aarch64-macos`), check it, unpack it anywhere and put its `bin/` on your PATH:

```bash
V=0.8.1; P=x86_64-linux
curl -LO https://github.com/karalang/kara/releases/download/v$V/karac-$V-$P.tar.gz
curl -LO https://github.com/karalang/kara/releases/download/v$V/karac-$V-$P.tar.gz.sha256
sha256sum -c karac-$V-$P.tar.gz.sha256     # macOS: shasum -a 256 -c
tar xzf karac-$V-$P.tar.gz
xattr -dr com.apple.quarantine karac-$V-$P     # macOS only: clear Gatekeeper's quarantine
export PATH="$PWD/karac-$V-$P/bin:$PATH"
karac --version
```

The Linux binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12 and later); 0.8.0's needed 2.39 (Ubuntu 24.04). On RHEL 9, Amazon Linux 2023 or another distro with an older glibc, build from source. `karac build` needs a C linker (`build-essential` or the Xcode command-line tools); `karac run` does not.

### Build from source

You need a [Rust toolchain](https://rustup.rs) and LLVM 22 (`llvm-22-dev libpolly-22-dev` from [apt.llvm.org](https://apt.llvm.org) on Debian/Ubuntu, or `bash scripts/ci-install-llvm22.sh`; `brew install llvm@22` on macOS; point `llvm-sys` at it with `LLVM_SYS_221_PREFIX=$(llvm-config-22 --prefix)` or `$(brew --prefix llvm@22)` if it is not found):

```bash
git clone https://github.com/karalang/kara
cd kara
cargo build --release --features llvm    # the compiler and the JIT runner, in target/release/
# The runtime archives `karac build` links: the lean one first, then the full one.
cargo rustc -p karac-runtime --release --no-default-features --features net --crate-type staticlib
cp target/release/libkarac_runtime.a target/release/libkarac_runtime_min.a
cargo rustc -p karac-runtime --release --crate-type staticlib
export PATH="$PWD/target/release:$PATH"
karac --version
```

`karac build` needs a C linker (`build-essential` or the Xcode command-line tools).

### Editor support

The language server is `kara-lsp`, a separate binary: build it with `cargo build --release -p kara-lsp` (it lands beside `karac` in `target/release/`) and point your editor's LSP client at it. It gives diagnostics, hover with types and effects, and go-to-definition. Each 0.8.x release also ships a VS Code extension (`kara-vscode-*-<platform>.vsix`) with `kara-lsp` bundled; install it with `code --install-extension <file>.vsix`. The extension's source is in [editors/vscode](editors/vscode). The WebAssembly and optional archives, and the full test gate, are in [CLAUDE.md](CLAUDE.md) and [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

See [CONTRIBUTING.md](CONTRIBUTING.md) for how to build, test, and submit changes.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
