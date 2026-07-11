# The Effect System

This is the feature that defines Kāra. The effect system tracks *what your code does to the outside world* — and uses that knowledge to verify correctness, generate better diagnostics, and automatically parallelize work.

## The idea

Every interaction with the outside world is an **effect**: reading a file, writing to a database, sending a network request, allocating memory. In most languages, these are invisible — a function might do anything and the caller has no way to know.

In Kāra, effects are tracked. The compiler knows which functions read from the filesystem, which write to a database, which send network requests. This information flows through the type system and enables powerful guarantees.

## Effects = verbs + resources

An effect is a **verb** applied to a **resource**:

```
reads(FileSystem)       — reads from the filesystem
writes(Database)        — writes to a database
sends(Net)              — sends data over the network
receives(Net)           — receives data from the network
allocates(Heap)         — allocates memory
panics                  — might crash (no resource needed)
```

These six are the **resource verbs**: `reads`, `writes`, `sends`, `receives`, `allocates`, `panics`. They answer *"can these two operations conflict?"* — which is what drives the auto-parallelization below.

Resources are user-defined. You declare what exists in your domain:

```kara
effect resource FileSystem;
effect resource Database;
effect resource Cache;
effect resource Net;
```

One verb can name several resources at once — `writes(Display, Audio)` is shorter than writing `writes` twice.

### Execution verbs: `blocks` and `suspends`

The resource verbs say *what* a function touches. Two more verbs say *how it runs* — information the scheduler needs to decide *where* to place a task:

- **`blocks`** — the call may park the OS thread in a kernel wait (a `sleep`, a synchronous file read, a contended lock). While it waits, that thread can do nothing else, so the scheduler routes blocking tasks to a separate pool.
- **`suspends`** — the call may cooperatively yield: the task steps aside and the thread is freed to run other work, resuming later. This is Kāra's async — there is **no `async`/`await`, no `Future`, no function coloring.** You write a plain call; the compiler inserts the yield point because the callee is declared `suspends`.

```kara,ignore
fn sleep(d: Duration) with blocks { ... }                          // parks the thread
fn http_get(url: String) -> Response with sends(Net) suspends { ... }   // yields while waiting
fn compute(x: f64) -> f64 { ... }                                  // neither — runs anywhere
```

Execution verbs take no resource — a function either may block/suspend or it may not. They don't take part in conflict analysis (placement is a separate axis from conflict), and like resource verbs they're inferred on private functions but **must be declared on public ones**: hiding whether a function blocks would defeat the point.

That's the full set: six resource verbs plus these two execution verbs, eight in all.

## Private functions: effects are inferred

For internal functions, the compiler figures out the effects automatically:

```kara,ignore
fn load_data(path: String) -> String {
    read_file(path)     // compiler infers: reads(FileSystem)
}

fn save_report(data: String) {
    write_file("report.txt", data)    // compiler infers: writes(FileSystem)
    println("Saved.");                // compiler infers: writes(Stdout)
}
```

You write normal code. The compiler tracks what it does. No annotation needed.

## Public functions: effects are declared

At API boundaries, you declare your effects explicitly. This is a contract with your callers:

```kara,ignore
pub fn fetch_user(id: u64) -> Result[User, Error]
    with reads(Database) sends(Net)
{
    let cached = check_cache(id);
    match cached {
        Some(user) => Ok(user),
        None => load_from_api(id),
    }
}
```

The `with` clause lists every effect the function may produce. The compiler verifies that the body doesn't exceed the declared effects — if you add a `write_file` call inside, the compiler will reject it because `writes(FileSystem)` isn't declared.

This is the key insight: **effects are the primary interface of Kāra.** They tell callers what a function does to the world. Ownership and layout are implementation details the compiler manages; effects are what you declare and what gets verified.

## Why this matters

### 1. The compiler catches mistakes

If your function claims `reads(Database)` but you accidentally added a line that writes to it, the compiler tells you. You either fix the code or update the declaration.

### 2. Automatic parallelization

Two function calls with non-conflicting effects can run in parallel:

```kara,ignore
fn generate_report(id: u64) -> Report
    with reads(UserDB) reads(OrderDB) reads(Analytics)
{
    let user = fetch_user(id);          // reads(UserDB)
    let orders = fetch_orders(id);      // reads(OrderDB)
    let stats = fetch_analytics(id);    // reads(Analytics)
    build_report(user, orders, stats)
}
```

The three fetches read from different resources. The compiler can prove they don't interfere with each other and run them concurrently — without you writing any threading code. We'll cover this in [Chapter 14](./ch14-concurrency.md).

### 3. Documentation that can't lie

The `with` clause is a machine-checked description of what a function does. It can't go stale like a comment. It can't be wrong like a docstring. If the declaration says `reads(Database)`, the function reads from the database and does nothing else that isn't declared.

## Effect groups

For common combinations, define groups:

```kara
effect group io = reads(FileSystem) + writes(FileSystem) + reads(Env);
```

Effect group names are Value-class (snake_case) — the same naming class as effect verbs and `let` bindings. Then use the group in declarations:

```kara,ignore
pub fn process(path: String) -> Result[Data, Error] with io {
    // can read and write files, read environment variables
}
```

## What's next

The effect system goes deeper — effect polymorphism, parameterized resources, providers, conflict detection. But the core idea is what matters: **declare what your code does to the world, and the compiler verifies it.** Everything else builds on that.
