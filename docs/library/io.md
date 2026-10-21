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

`stdin.read_line()` returns the next line with its `\n`, or the last line without one when the input does not end in a newline; at the end of input it returns `Ok` of an empty string. `stdin.lines()` returns an iterator over the remaining lines of standard input, each a `Result[String, IoError]` without its `\n` or `\r\n`, and it ends at the end of input. Iterate it with `for line in stdin.lines()`: making it reads nothing, and each turn of the loop reads one line with `reads(Stdin)` and `blocks`. It has no adaptor or terminal methods. The reads share one buffer, so `read_line` followed by `lines()` continues where the first read stopped. The prelude functions `print`, `println` and `eprintln` take any `Display` value and have `writes(Stdout)` or `writes(Stderr)` ([README](README.md#the-prelude)).

**UTF-8 and stdin.** `stdin.read_line` and `stdin.read_to_string` return `Result[String, IoError]`: invalid UTF-8 on stdin is `IoError.InvalidUtf8`, which keeps `String`'s UTF-8 invariant. A bytes-level stdin read is deferred ([deferred.md](../deferred.md)).

**Buffering.** Stdin is line-buffered, and so is stdout when it is a terminal. Stdout on a pipe or a file may hold more than a line, but it is written out before a child process starts, before anything is written to stderr, at `stdout.flush()` and at exit, so its order against those is the program's. File I/O is block-buffered. `stdout.flush()` forces out buffered output, including a partial line, which is needed when a prompt without a newline (`print("Guess: ")`) comes before a stdin read. `stderr.flush()` is the stderr counterpart. Both return `()`, not a `Result`. Typed buffering traits are deferred ([deferred.md](../deferred.md)).

## Files

The `fs` alias reaches the path-based functions:

```kara
fn fs.read_to_string(path: Str) -> Result[String, IoError] with reads(FileSystem) blocks
fn fs.read_lines(path: Str) -> Result[Vec[String], IoError] with reads(FileSystem) blocks
fn fs.write(path: Str, content: Str) -> Result[(), IoError] with writes(FileSystem) blocks

fn fs.exists(path: Str) -> bool with reads(FileSystem) blocks
fn fs.metadata(path: Str) -> Result[Metadata, IoError] with reads(FileSystem) blocks
fn fs.read_dir(path: Str) -> Result[Vec[String], IoError] with reads(FileSystem) blocks

fn fs.create_dir(path: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.create_dir_all(path: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.remove_file(path: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.remove_dir(path: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.remove_dir_all(path: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.rename(from: Str, to: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.copy(from: Str, to: Str) -> Result[i64, IoError] with reads(FileSystem) writes(FileSystem) blocks

fn fs.read_bytes(path: Str) -> Result[Vec[u8], IoError] with reads(FileSystem) blocks
fn fs.write_bytes(path: Str, content: Slice[u8]) -> Result[(), IoError] with writes(FileSystem) blocks

fn fs.symlink(target: Str, link: Str) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.read_link(path: Str) -> Result[String, IoError] with reads(FileSystem) blocks
fn fs.symlink_metadata(path: Str) -> Result[Metadata, IoError] with reads(FileSystem) blocks
fn fs.set_permissions(path: Str, mode: i64) -> Result[(), IoError] with writes(FileSystem) blocks
fn fs.set_modified(path: Str, ms: i64) -> Result[(), IoError] with writes(FileSystem) blocks
```

`read_bytes` and `write_bytes` are the byte forms of `read_to_string` and `write`: the whole file as bytes, with no UTF-8 check.

`read_lines` splits on `\n` and drops a trailing `\r` from each line.

**`exists` returns `false` on any error**, a permission error included, so it cannot tell "absent" from "cannot look". A tool that must know the difference calls `metadata` and matches `NotFound`.

**`metadata`** follows symbolic links and returns a `Metadata` (in `std.io`, and in the prelude):

```kara
struct Metadata { len: i64, is_file: bool, is_dir: bool, modified_ms: i64, is_symlink: bool, mode: i64 }
```

`len` is the size in bytes and `modified_ms` the last modification time in milliseconds since the Unix epoch (negative before it). `mode` is the permission bits, `0` to `0o7777` (the file-type bits are not included). On Windows, which has no Unix modes, `mode` is made from the read-only flag: `0o666` or `0o444` for a file, `0o777` or `0o555` for a directory. Fields will be added, so build a `Metadata` only by calling `metadata`, and read its fields by name rather than destructuring the whole struct.

**`symlink_metadata`** is `metadata` without following a symbolic link: for a link it describes the link itself, with `is_symlink` true and `is_file` and `is_dir` false. `metadata` follows links, so its `is_symlink` is always false.

**`symlink(target, link)`** creates `link` as a symbolic link to `target`. The target is stored exactly as given, need not exist, and if relative is resolved from the link's directory, not the current one. On Windows, creating a link needs Developer Mode or administrator rights (otherwise `PermissionDenied`), and the link is a directory link if the target is an existing directory. Where the platform has no symbolic links the call fails with `Unsupported`. **`read_link`** returns the stored target; a target that is not valid UTF-8 fails with `InvalidUtf8`.

**`set_permissions(path, mode)`** sets the permission bits; a mode outside `0..=0o7777` fails with `InvalidInput`. On Windows only the owner-write bit (`0o200`) is used: without it the file becomes read-only. **`set_modified(path, ms)`** sets the modification time and leaves the access time unchanged. Both follow symbolic links. `set_modified` opens the file to change it, so on Unix the file must be readable (a directory, listable). A tool that restores an archive therefore sets the time before it removes read permission.

**`read_dir`** returns the names of the entries (not paths), without `.` and `..`, **sorted byte-wise on their UTF-8 names**, so a program lists a directory the same way on every OS. A name that is not valid UTF-8 makes the call fail with `InvalidUtf8`.

`create_dir` fails with `AlreadyExists` if the path exists; `create_dir_all` creates the missing parents and succeeds if the directory is already there. `remove_dir` removes an empty directory only; `remove_dir_all` removes a directory and everything under it, without following symbolic links out of it.

**`rename`** replaces `to` if it exists. On one file system it is atomic, which makes "write a temporary file, then rename it over the target" the way to update a file safely. Across file systems it fails with an `IoError` (`Other`). On Windows it fails if `to` is a directory, and replacing an existing file can fail while another process holds it open. `copy` copies a file's contents and permissions and returns the number of bytes copied.

**Paths are borrowed.** Every function that takes a path or a name takes it as `Str`, and so do the `File` constructors. Opening a file reads the path and does not consume it, so the caller keeps its `String`.

**`File`** (in `std.io`) is an open file. Dropping it closes it.

| Method | Signature | Notes |
|---|---|---|
| `open` | `fn open(path: Str) -> Result[File, IoError] with reads(FileSystem) blocks` | Opens for reading |
| `create` | `fn create(path: Str) -> Result[File, IoError] with writes(FileSystem) blocks` | Opens for writing, creating the file |
| `append` | `fn append(path: Str) -> Result[File, IoError] with writes(FileSystem) blocks` | Opens for writing at the end |
| `options` | `fn options() -> OpenOptions` | Options for `open`, with nothing set; see below |
| `read` | `fn read(self, buf: mut Slice[u8]) -> Result[i64, IoError] with reads(self) blocks` | Reads into `buf`; returns the byte count |
| `write` | `fn write(self, buf: Slice[u8]) -> Result[i64, IoError] with writes(self) blocks` | Returns the byte count written |
| `flush` | `fn flush(self) -> Result[(), IoError] with writes(self) blocks` | Pushes buffered bytes to the OS |
| `sync_all` | `fn sync_all(self) -> Result[(), IoError] with writes(self) blocks` | Flushes and makes contents and metadata durable |
| `sync_data` | `fn sync_data(self) -> Result[(), IoError] with writes(self) blocks` | As `sync_all`, but may skip metadata |
| `seek` | `fn seek(self, whence: SeekFrom, offset: i64) -> Result[i64, IoError] with reads(self) blocks` | Moves the cursor; returns the new position from the start. `f.seek(SeekFrom.Current, 0)` is the current position |
| `set_len` | `fn set_len(self, len: i64) -> Result[(), IoError] with writes(self) blocks` | Cuts the file to `len` bytes, or extends it with zero bytes. The cursor does not move |

```kara
enum SeekFrom { Start, Current, End }
```

`Start` counts from byte 0 and rejects a negative offset with `InvalidInput`; `Current` and `End` accept negative offsets, so `f.seek(SeekFrom.End, -4)` addresses the last four bytes. `flush` does not make data durable: it pushes userspace buffers to the OS, and only `sync_all` or `sync_data` survive a power failure.

`set_len` with a negative `len` is `InvalidInput`. If `set_len` cuts the file below the cursor, the next write fills the gap between the new end and the cursor with zero bytes, as any write past the end does.

**`OpenOptions`** (in `std.io`) is the general way to open a file. `File.options()` starts with nothing set; each method sets or clears one option and returns the options; `open` opens the file. Building the options touches no file, so only `open` has effects.

| Method | Signature | Notes |
|---|---|---|
| `read` | `fn read(own self, on: bool) -> OpenOptions` | Open for reading |
| `write` | `fn write(own self, on: bool) -> OpenOptions` | Open for writing |
| `append` | `fn append(own self, on: bool) -> OpenOptions` | Write at the end of the file; implies `write` |
| `truncate` | `fn truncate(own self, on: bool) -> OpenOptions` | Cut the file to length 0 on opening |
| `create` | `fn create(own self, on: bool) -> OpenOptions` | Create the file if it is absent |
| `create_new` | `fn create_new(own self, on: bool) -> OpenOptions` | Create the file; fail with `AlreadyExists` if it is there |
| `open` | `fn open(self, path: Str) -> Result[File, IoError] with reads(FileSystem) writes(FileSystem) blocks` | Opens `path` with these options |

`open` has both the reading and the writing effect, because which one it uses depends on options set at run time. A combination that opens nothing is `Err(InvalidInput)` from `open`, not a panic: no access mode (none of `read`, `write`, `append`); `truncate`, `create` or `create_new` without `write` or `append`; `create_new` with `truncate`; and `append` with `truncate`.

The constructors above are shorthands:

- `File.open(p)` is `File.options().read(true).open(p)`.
- `File.create(p)` is `File.options().write(true).create(true).truncate(true).open(p)`.
- `File.append(p)` is `File.options().append(true).create(true).open(p)`.

The equivalence is in behaviour, not effects: `File.open` has `reads(FileSystem)` only, and `File.create` and `File.append` have `writes(FileSystem)` only, where `OpenOptions.open` has both because its options are run-time values.

A file updated in place opens with `File.options().read(true).write(true).open(p)`, so a mistyped path is `NotFound` rather than a new empty file; add `.create(true)` to create it when it is absent. A lock file is `File.options().write(true).create_new(true).open(p)`: only one opener can create it. Reads and writes at an offset that leave the cursor alone (`pread` / `pwrite`) and advisory locks are deferred ([deferred.md](../deferred.md#positional-file-io-and-advisory-locking)).

**Buffered I/O** (in `std.io`). `BufReader[R]` and `BufWriter[W]` wrap an open `File`, or a child process's pipe (`ChildStdout` or `ChildStderr` for a `BufReader`, `ChildStdin` for a `BufWriter`; [Processes](#processes)), with an in-memory buffer, so that many small reads or writes cost few system calls. Construction touches no file, so the constructors have no effect.

| Method | Signature |
|---|---|
| `BufReader.new` | `fn new(reader: own R) -> BufReader[R]` |
| `BufReader.with_capacity` | `fn with_capacity(reader: own R, cap: i64) -> BufReader[R]` |
| `read_line` | `fn read_line(self, buf: mut ref String) -> Result[i64, IoError] with reads(self) blocks` |
| `read_to_string` | `fn read_to_string(self, buf: mut ref String) -> Result[i64, IoError] with reads(self) blocks` |
| `read` | `fn read(self, buf: mut Slice[u8]) -> Result[i64, IoError] with reads(self) blocks` |
| `lines` | `fn lines(self) -> LinesIter` |
| `fill_buf` | `fn fill_buf(mut ref self) -> Result[Slice[u8], IoError] with reads(self) blocks` |
| `consume` | `fn consume(mut ref self, n: i64)` |
| `BufWriter.new` | `fn new(writer: own W) -> BufWriter[W]` |
| `BufWriter.with_capacity` | `fn with_capacity(writer: own W, cap: i64) -> BufWriter[W]` |
| `write` | `fn write(self, buf: Slice[u8]) -> Result[i64, IoError] with writes(self) blocks` |
| `write_all` | `fn write_all(self, buf: Slice[u8]) -> Result[(), IoError] with writes(self) blocks` |
| `flush` | `fn flush(self) -> Result[(), IoError] with writes(self) blocks` |

`read_line` and `read_to_string` append to `buf` and return the number of bytes read. `lines` returns an iterator over the remaining lines; making it reads nothing, and its `next` reads from the reader and has `blocks`.

`fill_buf` refills the buffer if it is empty and returns the bytes buffered but not yet consumed, without copying them (empty at the end of the input); `consume(n)` marks `n` of them read, and an `n` past the buffered length is clamped. **The slice `fill_buf` returns is a view of the reader's buffer and borrows the reader**, so a `read`, `read_line` or `consume` while the slice is alive is a borrow error; finish with the slice first, as in `let n = { let b = r.fill_buf()?; ...; b.len() }; r.consume(n);`.

**Dropping a `BufWriter` flushes it and ignores any error.** Call `flush()` before the drop to see a write error. The drop has the effects of `flush`, charged to the scope where it runs ([core-semantics.md §12](../core-semantics.md#12-effects-soundness-defaults-c10), item 3).

## Errors

```kara
enum IoError {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    InvalidInput,
    UnexpectedEof,
    InvalidUtf8,
    Interrupted,
    Unsupported,
    Other(String),
}

enum VarError {
    NotPresent,
    NotUnicode,
}
```

`InvalidInput` is an argument the operation refuses: a negative length or offset, or `OpenOptions` that open nothing. `Unsupported` is an operation this platform does not provide. `Other` carries the system's message for every other failure.

Both are prelude types. `IoError` covers the standard streams and files; `VarError` covers environment-variable lookup ([time-random-env.md](time-random-env.md#env)). They are kept separate so that each has a tight set of variants: a handler for `env.var` does not have to consider `NotFound` or `PermissionDenied`. The two overlap on one point (`IoError.InvalidUtf8` and `VarError.NotUnicode` describe the same condition from different sources), and the library provides `impl From[VarError] for IoError` (`NotPresent` becomes `NotFound`, `NotUnicode` becomes `InvalidUtf8`), so a function that uses `?` on both can return `IoError` alone.

## Networking

v1 networking is blocking TCP and UDP, in `std.net`, and the HTTP client, which speaks HTTPS (below). HTTP servers, TLS servers and WebSocket come with the services track (M4a); see [deferred.md](../deferred.md#m4a-services).

```kara
impl TcpListener {
    fn bind(addr: Str) -> Result[TcpListener, NetError] with sends(Network) receives(Network) blocks
    fn accept(self) -> Result[TcpStream, NetError] with receives(self) blocks
    fn local_addr(self) -> Result[String, NetError]
}

impl TcpStream {
    fn connect(addr: Str) -> Result[TcpStream, NetError] with sends(Network) receives(Network) blocks
    fn read(self, buf: mut Slice[u8]) -> Result[i64, NetError] with receives(self) blocks
    fn write(self, buf: Slice[u8]) -> Result[i64, NetError] with sends(self) blocks
    fn write_all(self, buf: Slice[u8]) -> Result[i64, NetError] with sends(self) blocks
    fn try_clone(self) -> Result[TcpStream, NetError]
    fn shutdown_write(self) -> Result[(), NetError] with sends(self) blocks
    fn set_read_timeout(self, ms: Option[i64]) -> Result[(), NetError]
}

impl UdpSocket {
    fn bind(addr: Str) -> Result[UdpSocket, NetError] with sends(Network) receives(Network) blocks
    fn send_to(self, buf: Slice[u8], addr: Str) -> Result[i64, NetError] with sends(self) blocks
    fn recv_from(self, buf: mut Slice[u8]) -> Result[(i64, String), NetError] with receives(self) blocks
    fn connect(self, addr: Str) -> Result[(), NetError] with sends(self) blocks
    fn send(self, buf: Slice[u8]) -> Result[i64, NetError] with sends(self) blocks
    fn recv(self, buf: mut Slice[u8]) -> Result[i64, NetError] with receives(self) blocks
    fn local_addr(self) -> Result[String, NetError]
    fn set_read_timeout(self, ms: Option[i64]) -> Result[(), NetError]
    fn try_clone(self) -> Result[UdpSocket, NetError]
}

enum NetError {
    Interrupted,
    AddrInUse,
    ConnectionRefused,
    PermissionDenied,
    TimedOut,
    WouldBlock,
    InvalidInput,
    Other(i32),
}

type TcpError = NetError;
```

`addr` is an IP address and port, such as `"127.0.0.1:8080"`; port 0 asks the OS for a free port, and `local_addr` returns the address the socket got, as `"ip:port"`. Dropping a socket closes it. `try_clone` returns a second handle to the same socket; since the compiler cannot tell the two handles apart, their operations conflict as if they were one value.

**TCP.** `read` returns the number of bytes read, and 0 at the end of the stream. `write` may write fewer bytes than given; `write_all` keeps writing, retrying on `Interrupted`, until every byte is sent or an error occurs.

**UDP.** A `UdpSocket` sends and receives datagrams, each one `send_to` or `send` call. UDP does not guarantee delivery or order: a datagram may be lost, duplicated or overtaken by a later one. `recv_from` waits for one datagram, stores it in `buf` and returns the number of bytes stored and the sender's address. A datagram longer than `buf` is cut to fit and the rest is lost, which is not an error, so the count is the bytes stored, not the datagram's size. `connect` sets the peer that `send` sends to, and after it `recv` takes datagrams from that peer only.

**Timeouts.** `set_read_timeout(Some(ms))` makes a later `read`, `recv` or `recv_from` that waits longer than `ms` milliseconds fail with `NetError.TimedOut`; `None` waits for ever, the default. A timeout is how a UDP program avoids waiting for ever on a lost datagram. A timeout below 1 ms fails with `InvalidInput`. Inside a task, a `TcpStream.read` waits for data without the timeout for now ([deferred.md](../deferred.md#networking-beyond-v1)).

**Errors.** `NetError` is the error of every socket operation. `Interrupted` is a call a signal interrupted, which is safe to retry. `TimedOut` is a read timeout expiring. `WouldBlock` is a non-blocking socket with nothing to do; v1 sockets block, so no v1 operation returns it, and it exists so that matches written now stay exhaustive when non-blocking sockets come. `InvalidInput` is an argument the operation refuses: an address that does not parse, or a timeout below 1 ms. `Other` carries the OS error code, or -1 when the OS gave none. `TcpError` is the earlier name of `NetError`, kept until M3.

### HTTP client

```kara
impl Client {
    fn new() -> Client
    fn get(self, url: String) -> Result[Response, HttpError] with sends(Network) receives(Network) blocks
    fn post(self, url: String, body: String) -> Result[Response, HttpError] with sends(Network) receives(Network) blocks
    fn request(self, method: String, url: String) -> RequestBuilder
}

impl RequestBuilder {
    fn header(own self, name: String, value: String) -> RequestBuilder
    fn body(own self, b: String) -> RequestBuilder
    fn timeout(own self, ms: i64) -> RequestBuilder
    fn send(self) -> Result[Response, HttpError] with sends(Network) receives(Network) blocks
}

impl Response {
    fn status(self) -> i64
    fn body(self) -> String
    fn bytes(self) -> Vec[u8]
    fn header(self, name: String) -> Option[String]
    fn headers(self) -> Vec[(String, String)]
}

impl HttpError {
    fn message(self) -> String
}
```

A URL is `http://` or `https://`. The client follows up to five redirects, and a status outside 2xx is an `Err`. `header` compares names without case.

**HTTPS.** An `https://` URL is fetched over TLS, and the server's certificate must chain to a trusted root. The trusted roots are the Mozilla bundle, built into every program so it needs no system CA store, plus the PEM certificates in the file that the environment variable `KARAC_HTTP_EXTRA_CA` names, for a private CA or a test's own. A certificate that does not verify is an `Err` naming the failure; there is no way to turn verification off.

## Processes

```kara
fn exit(code: i32) -> Never
```

`exit` is imported by name, `import std.process.{exit};`. It flushes standard output and standard error, then ends the process with `code`. No `Drop` body, `defer` or `errdefer` runs, in any frame. It declares no effects, since ending the process is not a resource access, and it exists on every target. It may be called anywhere, a `Drop` body and the panic handler included.

Exit codes and `main`'s return value are in [design.md §10](../design.md#the-main-function-and-exit-codes).

### Running programs

```kara
impl Command {
    fn new(program: Str) -> Command
    fn arg(own self, a: Str) -> Command
    fn args(own self, a: Slice[Str]) -> Command
    fn env(own self, key: Str, value: Str) -> Command
    fn env_remove(own self, key: Str) -> Command
    fn env_clear(own self) -> Command
    fn current_dir(own self, path: Str) -> Command
    fn stdin(own self, s: own Stdio) -> Command
    fn stdout(own self, s: own Stdio) -> Command
    fn stderr(own self, s: own Stdio) -> Command

    fn spawn(self) -> Result[Child, IoError] with sends(ProcessTable)
    fn status(self) -> Result[ExitStatus, IoError] with sends(ProcessTable) blocks
    fn output(self) -> Result[Output, IoError] with sends(ProcessTable) blocks
}

struct Output { status: ExitStatus, stdout: Vec[u8], stderr: Vec[u8] }
```

A `Command` describes a program to run. Building one touches nothing; `spawn` starts the program and returns a `Child`, `status` runs it to the end, and `output` runs it to the end and collects everything it wrote. There is no shell: each argument reaches the program as it was given, with no splitting, quoting or globbing. A program name without a `/` is looked up on the parent's `PATH` when the command starts, and `current_dir` does not change that lookup. The environment is inherited, changed by `env`, `env_remove` and `env_clear` in the order they were called. On Windows, arguments are quoted by the MSVC C runtime's convention, so a program that parses its command line differently may see different arguments.

`args` takes a `Slice[Str]`, such as `["-l", "-a"]`. To pass a `Vec[String]`, call `arg` for each element:

```kara
let mut cmd = Command.new("echo");
for w in words {
    cmd = cmd.arg(w);
}
```

`spawn` fails with `NotFound` when there is no such program or working directory, `PermissionDenied` when the program cannot be run, and `Unsupported` on targets that cannot start processes (wasm). `status` and `output` fail the same ways. They never fail because the program did: a non-zero exit is an `Ok` status.

```kara
impl Stdio {
    fn inherit() -> Stdio
    fn null() -> Stdio
    fn piped() -> Stdio
    fn from_file(f: own File) -> Stdio
    fn from_stdout(o: own ChildStdout) -> Stdio
    fn from_stderr(e: own ChildStderr) -> Stdio
}
```

`Stdio` says where one of a child's standard streams goes. `inherit` shares the parent's stream, `null` is the null device (reads see end of file, writes are discarded), and `piped` makes a pipe that the parent takes from the `Child`. `from_file` connects the stream to an open file, which is a shell's `<`, `>`, `>>` and `2>`: open the file the way the redirection needs (`File.create`, or `OpenOptions` with `append` for `>>`). `from_stdout` and `from_stderr` connect it to another child's pipe, which is a shell's `|`. A stream that is not set is inherited, except that `output` pipes stdout and stderr and gives stdin the null device. A command can be run more than once, and each run gets its own copy of a file. A pipe can go to only one child, so running a command a second time with a `from_stdout` stream fails with `InvalidInput`.

`Stdio` is a type with constructors rather than an enum, so it can gain kinds later without breaking a `match`. Before, it was an enum: `karac fix` rewrites `Stdio.Inherit`, `Stdio.Null` and `Stdio.Piped` to the constructors.

```kara
impl Child {
    fn id(self) -> i64
    fn wait(self) -> Result[ExitStatus, IoError] with sends(ProcessTable) blocks
    fn try_wait(self) -> Result[Option[ExitStatus], IoError] with sends(ProcessTable)
    fn kill(self) -> Result[(), IoError] with sends(ProcessTable)
    fn wait_with_output(own self) -> Result[Output, IoError] with sends(ProcessTable) blocks
    fn stdin(self) -> Option[ChildStdin]
    fn stdout(self) -> Option[ChildStdout]
    fn stderr(self) -> Option[ChildStderr]
}
```

`wait` blocks until the child ends. If the parent has not taken a piped stdin, `wait` closes it first, so a child that reads until end of file can finish. Once the child has ended, waiting again returns the same status. `try_wait` returns `None` while the child runs. `kill` ends the child (`SIGKILL` on Unix, `TerminateProcess` on Windows), and the child must still be waited for. `wait_with_output` waits and collects the stdout and stderr pipes the parent has not taken. It reads both at once, so a child that fills one pipe while the parent reads the other cannot deadlock. `stdin`, `stdout` and `stderr` hand over a piped stream's handle the first time they are called, and return `None` after that or when the stream was not piped.

Dropping a `Child` neither kills nor waits for it: the process keeps running, and on Unix it stays in the process table until the parent exits. Wait for every child you start. The pipes that were not taken close when the `Child` is dropped.

```kara
impl ChildStdout {        // ChildStderr has the same methods
    fn read(self, buf: mut Slice[u8]) -> Result[i64, IoError] with receives(self) blocks
    fn read_to_end(self, buf: mut ref Vec[u8]) -> Result[i64, IoError] with receives(self) blocks
    fn read_to_string(self, buf: mut ref String) -> Result[i64, IoError] with receives(self) blocks
}

impl ChildStdin {
    fn write(self, buf: Slice[u8]) -> Result[i64, IoError] with sends(self) blocks
    fn write_all(self, buf: Slice[u8]) -> Result[(), IoError] with sends(self) blocks
    fn flush(self) -> Result[(), IoError] with sends(self) blocks
    fn close(self) -> Result[(), IoError]
}
```

The pipe handles read and write bytes as `File` does, and a `BufReader` or `BufWriter` takes one as it takes a `File`: `BufReader.new(child.stdout().unwrap()).lines()` reads a child's output line by line. `read` waits for at least one byte and returns 0 when the child has closed its end. `read_to_end` and `read_to_string` append everything up to end of file and return the count. `read_to_string` fails with `InvalidUtf8` when the bytes are not UTF-8, and leaves `buf` unchanged when it does. Each pipe is its own resource, so reading one child's stdout does not conflict with writing another child's stdin. Dropping a `ChildStdin` closes the pipe, which the child reads as end of file. `close` does the same thing explicitly. Closing twice is `Ok`, and a write after closing fails with `InvalidInput`.

A child that writes more than a pipe holds (64 KiB on Linux) blocks until the parent reads. A parent that writes to a child's stdin and only then reads its stdout can deadlock with it. Use `output` or `wait_with_output` to read both streams of a child together, or write and read from separate tasks.

```kara
impl ExitStatus {
    fn success(self) -> bool
    fn code(self) -> Option[i32]
    fn signal(self) -> Option[i32]
}
```

`success` is true when the child exited with code 0. `code` is the exit code, or `None` when a signal ended the child. `signal` is that signal on Unix, and is always `None` on Windows. A shell's `$?` is `code`, or 128 plus `signal`. `ExitStatus` is `Copy`. Its fields are not part of the API: the earlier `code` and `success` fields are now methods, and `karac check` says so where one is read.

The shell pipeline `ls | wc -l`:

```kara
fn count_entries() -> Result[String, IoError] {
    let ls = Command.new("ls").stdout(Stdio.piped()).spawn()?;
    let wc = Command.new("wc").arg("-l")
        .stdin(Stdio.from_stdout(ls.stdout().unwrap()))
        .stdout(Stdio.piped())
        .spawn()?;
    let out = wc.wait_with_output()?;
    ls.wait()?;
    Ok(String.from_utf8(out.stdout).unwrap_or(String.new()))
}
```

Signals other than `kill`, process groups, pseudo-terminals, detached daemons, a free-standing pipe pair, non-UTF-8 arguments and environment, and a `wait` that suspends a task instead of blocking a thread are not in v1; see [deferred.md](../deferred.md#processes-beyond-v1).

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
