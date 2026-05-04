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

The six built-in verbs are: `reads`, `writes`, `sends`, `receives`, `allocates`, `panics`.

Resources are user-defined. You declare what exists in your domain:

```kara
effect resource FileSystem;
effect resource Database;
effect resource Cache;
effect resource Net;
```

## Private functions: effects are inferred

For internal functions, the compiler figures out the effects automatically:

```kara
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

```kara
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

```kara
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
effect group IO = reads(FileSystem) + writes(FileSystem) + reads(Env);
```

Then use the group in declarations:

```kara
pub fn process(path: String) -> Result[Data, Error] with IO {
    // can read and write files, read environment variables
}
```

## What's next

The effect system goes deeper — effect polymorphism, parameterized resources, providers, conflict detection. But the core idea is what matters: **declare what your code does to the world, and the compiler verifies it.** Everything else builds on that.
