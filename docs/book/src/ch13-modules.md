# Modules and Visibility

## File = module

In Kāra, every `.kara` file is a module. The directory structure defines the module tree — no `mod` declarations needed:

```
src/
  main.kara              // entry point
  db/
    connection.kara      // module: db.connection
    pool.kara            // module: db.pool
  auth/
    token.kara           // module: auth.token
```

The compiler discovers all `.kara` files automatically. No manifest of modules to maintain.

Module names are Value-class identifiers — always snake_case. This falls out of the identifier case-class rules introduced in [chapter 2](./ch02-variables-and-types.md#naming-identifiers); `db`, `connection`, `auth_token` are valid, `Db` or `AuthToken` as module names are compile errors.

## Three levels of visibility

| Keyword | Who can see it |
|---------|---------------|
| `pub` | Everyone, including users of your library |
| *(default)* | All files in your project |
| `private` | Files in the same directory only |

```kara
pub fn validate(input: String) -> bool { ... }     // public API
fn helper(s: String) -> String { ... }              // project-internal
private fn secret_impl() { ... }                    // same directory only
```

### Why default is project-internal

This will surprise you if you're coming from Rust or Java, where default = private to the module.

In Kāra, modules are directories. If the default were "private to this directory," you'd need `pub` on almost every cross-directory call within your own project. The current default covers the common case: internal code that your own files need to call.

You only annotate the boundaries:
- `pub` for things external users should see.
- `private` for helpers that shouldn't leak outside their directory.

## Imports

```kara
import db.connection.Connection;
import auth.token.Token;

// Multiple items from the same module
import std.collections.{Map, Set};

// Rename an imported item
import std.collections.Map as Dict;
```

Import paths are absolute from the crate root. Every file writes the same path for the same item, regardless of where it sits in the directory tree.

## Re-exports

Libraries can present a clean public surface:

```kara
// lib.kara
pub import db.connection.Connection;
pub import db.pool.Pool;
pub import auth.token.Token;

// Users write:
import mylib.Connection;    // not mylib.db.connection.Connection
```

Reorganize your internals without breaking users.

## The prelude

These are available everywhere without imports:

- **Types:** `Option`, `Result`, `Vec`, `String`, `StringSlice`, `Map`, `Set`, and all primitives.
- **Variants:** `Some`, `None`, `Ok`, `Err`.
- **Functions:** `print`, `println`, `eprintln`.
- **Builtins:** `todo`, `unreachable`, `dbg`, `assert`, `assert_eq`.

## Project layout

```
myproject/
  kara.toml             // project manifest (like Cargo.toml)
  src/
    main.kara           // executable entry point
    lib.kara            // library entry point (instead of main.kara)
  tests/
    db_test.kara        // integration tests
  examples/
    basic.kara          // runnable examples
```

Dependencies go in `kara.toml`:

```toml
[package]
name = "myproject"
version = "0.1.0"

[dependencies]
http = "1.2"
json = { version = "0.8", git = "https://github.com/example/json-kara" }
```
