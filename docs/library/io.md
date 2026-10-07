# Input and output

**I/O is blocking in v1.** A read or write occupies the calling thread until it completes, so every file and network operation has `blocks`. Non-blocking networking, coroutines and the `suspends` effect come with the services track (M4a); see [deferred.md](../deferred.md#m4a-services).

**Effects.** Each function lists its effects ([design.md §12](../design.md#resources)):
- **`FileSystem` and `Network` are capabilities**, not conflict keys: `reads(FileSystem)` means "may read files" and `sends(Network)` "may use the network". Path-taking functions such as `fs.write(path, content)` carry `writes(FileSystem)` (and `blocks`, like all I/O) and do not conflict with each other, so two `par` branches that write the same path by name are not detected.
- **An open file or connection is keyed by its value**: `f.write(buf)` has `writes(f)` and `conn.read(buf)` has `receives(conn)`. Operations on two different files or connections do not conflict, and conflicting operations on the same one do ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10), item 5).
- **The standard streams** are the resources `Stdin`, `Stdout` and `Stderr`.

## Standard streams

The streams are reached through the prelude aliases `stdin`, `stdout` and `stderr`, which resolve to the resources `Stdin`, `Stdout` and `Stderr`. A local binding with the same name shadows an alias. There is no `io` module: the functions live on the aliases.

```kara
// stdin
fn stdin.read_line() -> Result[String, IoError] with reads(Stdin) blocks
fn stdin.read_to_string() -> Result[String, IoError] with reads(Stdin) blocks
fn stdin.lines() -> StdinLines

// stdout and stderr
fn stdout.print(s: Str) with writes(Stdout)
fn stdout.println(s: Str) with writes(Stdout)
fn stdout.flush() with writes(Stdout)
fn stderr.print(s: Str) with writes(Stderr)
fn stderr.println(s: Str) with writes(Stderr)
fn stderr.flush() with writes(Stderr)
```

`stdin.lines()` returns an iterator over the remaining lines of standard input. Making it reads nothing; its `next` has `reads(Stdin)` and `blocks`. The prelude functions `print`, `println` and `eprintln` take any `Display` value and have `writes(Stdout)` or `writes(Stderr)` ([README](README.md#the-prelude)).

**UTF-8 and stdin.** `stdin.read_line` and `stdin.read_to_string` return `Result[String, IoError]`: invalid UTF-8 on stdin is `IoError.InvalidUtf8`, which keeps `String`'s UTF-8 invariant. A bytes-level stdin read is deferred ([deferred.md](../deferred.md)).

**Buffering.** Stdin and stdout are line-buffered; file I/O is block-buffered. `stdout.flush()` forces out buffered output, which is needed when a prompt without a newline (`print("Guess: ")`) comes before a stdin read. `stderr.flush()` is the stderr counterpart. Both return `()`, not a `Result`. Typed buffering traits are deferred ([deferred.md](../deferred.md)).

## Files

The `fs` alias reaches the path-based functions:

```kara
fn fs.read_to_string(path: Str) -> Result[String, IoError] with reads(FileSystem) blocks
fn fs.write(path: Str, content: Str) -> Result[(), IoError] with writes(FileSystem) blocks
```

**Paths are borrowed.** Every function that takes a path or a name takes it as `Str`, and so do the `File` constructors. Opening a file reads the path and does not consume it, so the caller keeps its `String`.

**`File`** (in `std.io`) is an open file. Dropping it closes it.

| Method | Signature | Notes |
|---|---|---|
| `open` | `fn open(path: Str) -> Result[File, IoError] with reads(FileSystem) blocks` | Opens for reading |
| `create` | `fn create(path: Str) -> Result[File, IoError] with writes(FileSystem) blocks` | Opens for writing, creating the file |
| `append` | `fn append(path: Str) -> Result[File, IoError] with writes(FileSystem) blocks` | Opens for writing at the end |
| `read` | `fn read(self, buf: mut Slice[u8]) -> Result[i64, IoError] with reads(self) blocks` | Reads into `buf`; returns the byte count |
| `write` | `fn write(self, buf: Slice[u8]) -> Result[i64, IoError] with writes(self) blocks` | Returns the byte count written |
| `flush` | `fn flush(self) -> Result[(), IoError] with writes(self) blocks` | Pushes buffered bytes to the OS |
| `sync_all` | `fn sync_all(self) -> Result[(), IoError] with writes(self) blocks` | Flushes and makes contents and metadata durable |
| `sync_data` | `fn sync_data(self) -> Result[(), IoError] with writes(self) blocks` | As `sync_all`, but may skip metadata |
| `seek` | `fn seek(self, whence: SeekFrom, offset: i64) -> Result[i64, IoError] with reads(self) blocks` | Moves the cursor; returns the new position from the start. `f.seek(SeekFrom.Current, 0)` is the current position |

```kara
enum SeekFrom { Start, Current, End }
```

`Start` counts from byte 0 and rejects a negative offset; `Current` and `End` accept negative offsets, so `f.seek(SeekFrom.End, -4)` addresses the last four bytes. `flush` does not make data durable: it pushes userspace buffers to the OS, and only `sync_all` or `sync_data` survive a power failure.

**Buffered I/O** (in `std.io`). `BufReader[R]` and `BufWriter[W]` wrap an open `File` with an in-memory buffer, so that many small reads or writes cost few system calls. Construction touches no file, so the constructors have no effect.

| Method | Signature |
|---|---|
| `BufReader.new` | `fn new(reader: own R) -> BufReader[R]` |
| `BufReader.with_capacity` | `fn with_capacity(reader: own R, cap: i64) -> BufReader[R]` |
| `read_line` | `fn read_line(self, buf: mut ref String) -> Result[i64, IoError] with reads(self) blocks` |
| `read_to_string` | `fn read_to_string(self, buf: mut ref String) -> Result[i64, IoError] with reads(self) blocks` |
| `read` | `fn read(self, buf: mut Slice[u8]) -> Result[i64, IoError] with reads(self) blocks` |
| `lines` | `fn lines(self) -> LinesIter` |
| `fill_buf` | `fn fill_buf(self) -> Result[Slice[u8], IoError] with reads(self) blocks` |
| `consume` | `fn consume(self, n: i64)` |
| `BufWriter.new` | `fn new(writer: own W) -> BufWriter[W]` |
| `BufWriter.with_capacity` | `fn with_capacity(writer: own W, cap: i64) -> BufWriter[W]` |
| `write` | `fn write(self, buf: Slice[u8]) -> Result[i64, IoError] with writes(self) blocks` |
| `write_all` | `fn write_all(self, buf: Slice[u8]) -> Result[(), IoError] with writes(self) blocks` |
| `flush` | `fn flush(self) -> Result[(), IoError] with writes(self) blocks` |

`read_line` and `read_to_string` append to `buf` and return the number of bytes read. `lines` returns an iterator over the remaining lines; making it reads nothing, and its `next` reads from the reader and has `blocks`.

**Dropping a `BufWriter` flushes it and ignores any error.** Call `flush()` before the drop to see a write error. The drop has the effects of `flush`, charged to the scope where it runs ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10), item 3).

## Errors

```kara
enum IoError {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    UnexpectedEof,
    InvalidUtf8,
    Interrupted,
    Other(String),
}

enum VarError {
    NotPresent,
    NotUnicode,
}
```

Both are prelude types. `IoError` covers the standard streams and files; `VarError` covers environment-variable lookup ([time-random-env.md](time-random-env.md#env)). They are kept separate so that each has a tight set of variants: a handler for `env.var` does not have to consider `NotFound` or `PermissionDenied`. The two overlap on one point (`IoError.InvalidUtf8` and `VarError.NotUnicode` describe the same condition from different sources), and the library provides `impl From[VarError] for IoError` (`NotPresent` becomes `NotFound`, `NotUnicode` becomes `InvalidUtf8`), so a function that uses `?` on both can return `IoError` alone.

## Networking

v1 networking is blocking TCP, in `std.net`. HTTP clients and servers, TLS and WebSocket come with the services track (M4a); see [deferred.md](../deferred.md#m4a-services).

```kara
impl TcpListener {
    fn bind(addr: Str) -> Result[TcpListener, TcpError] with sends(Network) receives(Network) blocks
    fn accept(self) -> Result[TcpStream, TcpError] with receives(self) blocks
}

impl TcpStream {
    fn connect(addr: Str) -> Result[TcpStream, TcpError] with sends(Network) receives(Network) blocks
    fn read(self, buf: mut Slice[u8]) -> Result[i64, TcpError] with receives(self) blocks
    fn write(self, buf: Slice[u8]) -> Result[i64, TcpError] with sends(self) blocks
    fn write_all(self, buf: Slice[u8]) -> Result[i64, TcpError] with sends(self) blocks
    fn try_clone(self) -> Result[TcpStream, TcpError]
    fn shutdown_write(self) -> Result[(), TcpError] with sends(self) blocks
}

enum TcpError {
    Interrupted,
    AddrInUse,
    ConnectionRefused,
    PermissionDenied,
    Other(i32),
}
```

`addr` is a host and port, such as `"127.0.0.1:8080"`; port 0 asks the OS for a free port. `read` returns the number of bytes read. `write` may write fewer bytes than given; `write_all` keeps writing, retrying on `Interrupted`, until every byte is sent or an error occurs. `try_clone` returns a second handle to the same connection; since the compiler cannot tell the two handles apart, their operations conflict as if they were one value. Dropping a `TcpListener` or `TcpStream` closes it. `TcpError.Other` carries the OS error code.

## Processes

```kara
fn exit(code: i32) -> Never
```

`std.process.exit` flushes standard output and standard error, then ends the process with `code`. No `Drop` body, `defer` or `errdefer` runs, in any frame. It declares no effects, since ending the process is not a resource access, and it exists on every target. It may be called anywhere, a `Drop` body and the panic handler included.

Exit codes and `main`'s return value are in [design.md §10](../design.md#the-main-function-and-exit-codes). Starting child processes is not yet specified.

## Examples

A guessing-game skeleton:

```kara
fn main() -> Result[(), IoError] {
    print("Guess: ");
    stdout.flush();
    let line = stdin.read_line()?;
    match i64.parse(line.trim()) {
        Ok(guess) => println(f"You guessed {guess}"),
        Err(_) => println("not a number"),
    }
    Ok(())
}
```

Its inferred effects are `reads(Stdin), writes(Stdout), blocks`. Like any private function, `main` has its effects inferred ([design.md §12](../design.md#12-effects)). It has no `panics`: `parse` returns a `Result` that the `match` handles, and `?` propagates rather than panicking. `blocks` comes from the stdin read, which parks the thread until input or end of file arrives.

A command-line tool skeleton:

```kara
fn main() -> Result[(), IoError] {
    let args = env.args();
    if args.len() < 2 {
        eprintln("usage: tool <path>");
        return Err(IoError.Other("missing argument"));
    }
    let content = fs.read_to_string(args[1])?;
    println(content);
    Ok(())
}
```

Its inferred effects are `reads(Env), reads(FileSystem), writes(Stdout), writes(Stderr), blocks, panics`. The `blocks` comes from the file read, and the `panics` from the `args[1]` index.
