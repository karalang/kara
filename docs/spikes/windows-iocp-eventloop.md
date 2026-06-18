# Windows IOCP event-loop backend — design + scoping (Phase 6, line 13)

> **Status: ALL STEPS DONE (0–6 + 4b), end-to-end validated natively on a
> Windows Server 2025 box (2026-06-17).** The full IOCP event-loop port, the TLS
> surface, AND the complete AOT toolchain now work on Windows: a real
> karac-compiled WebSocket server runs the **10k loopback functional run clean —
> 10,000/10,000 connections @ concurrency 16, ~1,400/s, zero fd/handle leak, no
> wedge — on the default multi-shard config.** Getting there required four
> native-only bug fixes the macOS cross-check could never surface (each found by
> *running*, not compiling):
>
> 1. **Core registration delivered no readiness** (Problem 3): the
>    borrow-register-`into_raw_socket`-forget model tore down mio's per-source
>    AFD state immediately. First fixed by keeping the source alive, then
>    superseded by the per-socket model in fix 4.
> 2. **`fatal.rs` link failure**: referenced the POSIX `write` symbol; MSVC
>    exposes it as `_write`, so any AOT binary / test exe failed to **link**.
> 3. **Plain `ws_accept` left the accepted socket non-blocking** (Windows
>    inherits the listener's flag), so the synchronous WS handshake hit
>    `WouldBlock` and failed ~40% of upgrades. Fixed (mirrors the TLS path).
> 4. **The concurrency wedge** (Problem 4): a *new mio source per park* left
>    overlapping stale AFD polls on a socket, so a stale completion consumed the
>    socket's readiness against a dead token and the re-parked task wedged (cdb:
>    all reactor threads idle in `GetQueuedCompletionStatusEx`, sockets in
>    CloseWait). Fixed by **one persistent mio source per socket + `reregister`**
>    across parks (`windows_iocp_bridge`), so a socket carries exactly one
>    `SockState` / AFD poll for its lifetime. This also makes multi-shard safe.
>
> Plus the **AOT toolchain port** (the spike under-scoped this — the karac
> linker driver was unix-only): a `#[cfg(windows)]` clang/lld-link path in
> `src/codegen/driver.rs` (+ Windows system libs + `legacy_stdio_definitions` +
> `/OPT:REF` + `.lib` archive naming), and two codegen fixes — `stdout` →
> `__acrt_iob_func` (MSVC has no `stdout` data symbol) and the `printf` symbol.
> `ring` 0.17 + rustls compile natively under MSVC with no nasm; LLVM 18.1 built
> from source **with `LLVM_TARGETS_TO_BUILD=all`** (inkwell references every
> backend's initializer — X86-only fails to link karac).
>
> **Green on Windows:** `cargo test -p karac-runtime --lib --features
> tls,net,test-helpers` = **249 passed / 0 failed**; fmt + clippy clean (`net`
> and `tls`); a pure-compute AOT program + the `ws_echo` server both build, link,
> and run natively; the 10k loopback run passes. The previously `#[cfg(unix)]`
> event-loop + TLS unit tests are un-gated and green (the dispatcher/accept-loop/
> repark ones caught Problem 3). **This flips the M3 Windows parity clause + line
> 13 + line 19** (cancel-sweep rides the now-validated registration path). Tracks
> phase-6-runtime.md "Open work front" **Slice 10 — Windows IOCP integration**
> and **Windows IOCP cancel-sweep** (line 19).

## Goal

The network event loop's fd-registration FFIs in `runtime/src/event_loop.rs`
are `#[cfg(unix)]`-only. On Windows the runtime crate compiles (the fns are
simply absent), but any AOT karac binary that parks on a socket fails to
**link** (unresolved `karac_runtime_event_loop_register_fd` etc.). Line 13 =
add the `#[cfg(windows)]` implementations so the event loop runs on Windows,
flipping the M3 cross-platform-parity gate's Windows clause.

This is **not** a flag flip. Two substantive problems below.

## Architecture recap (what's already cross-platform)

The core is mio-based and **platform-agnostic** — it needs no changes:

- `EventLoop`, `FdState`, the sharded `event_loops()`, the dispatcher, the
  cancel-sweep (`collect_cancelled`/`sweep_cancelled`), `KaracParkSlot`, the
  token packing, `take_wakeups` — all plain Rust over `mio::{Poll, Registry,
  Waker, Token, Interest, Events}`. mio already maps `Poll`/`Waker` to IOCP on
  Windows.
- `EventLoop::register{,_with_cancel}` / `deregister` are generic over
  `S: mio::event::Source` (`event_loop.rs:237,251,310`). They call
  `self.registry.register(source, token, interest)`. **The source is the only
  platform-specific ingredient.**

So the entire Windows port is confined to:
1. The thin FFI wrappers that *build the source* (`register_fd`,
   `register_fd_cancel`, `deregister_fd`) — `SourceFd` on unix.
2. The socket I/O layer (`tcp_*`, `ws_*`) that creates/uses raw fds.
3. A cross-cutting **fd ABI width** fix (see Problem 2).

The dispatcher, cancel-sweep, and park/wake logic are untouched — which also
means the **line-19 cancel-sweep "just works" on Windows once line 13 lands**,
since `register_fd_cancel` is one of the three wrappers being ported and the
sweep itself is platform-agnostic. (The line-19 nav entry can close together
with line 13.)

## Problem 1 — registration model: `SourceFd` is unix-only

`register_fd_impl` (`event_loop.rs:807`) builds `mio::unix::SourceFd(&raw_fd)`
— the readiness-model raw-fd wrapper mio uses for epoll/kqueue. **There is no
`SourceFd` on Windows.** IOCP is completion-based; mio's Windows backend
implements *readiness* semantics over IOCP via `\Device\Afd` (wepoll-style),
but exposes that only through its **owned socket types** (`mio::net::*`), not a
borrow-a-raw-handle `SourceFd`.

### Decision: bridge raw `SOCKET` → `mio::net::TcpStream`/`TcpListener`

mio 1.2 (`mio-1.2.0/src/io_source.rs`, `net/tcp.rs`) gives `mio::net::TcpStream`
/ `TcpListener`:
- `FromRawSocket` / `IntoRawSocket` (Windows), mirroring the unix
  `FromRawFd`/`IntoRawFd` the codebase already uses (`event_loop.rs:1619` etc.).
- `event::Source` impls that drive the AFD/IOCP readiness shim.

So the Windows wrapper does:

```rust
// raw SOCKET -> std socket (no-op handle adopt) -> mio source
let std_sock = unsafe { std::net::TcpStream::from_raw_socket(sock) };
let mut source = mio::net::TcpStream::from_std(std_sock);
ev.register_with_cancel(&mut source, dir, None, parked, cancel)?;
// CRITICAL: do NOT let `source` close the socket — the runtime owns its
// lifetime via the parked-task model. Recover the handle without dropping:
let _ = source.into_raw_socket(); // == mem::forget + as_raw_socket
```

The readiness semantics mio synthesizes mean **the dispatcher's wakeup-routing,
one-shot `take_registration`, and cancel-sweep all behave identically to unix**
— that is the whole reason to go through mio rather than hand-rolling IOCP.

**Lifetime hazard (the one to get right):** `mio::net::TcpStream`'s `Drop`
**closes** the socket. Both `register` and `deregister` reconstruct a source
from the raw handle, so each must release it with `into_raw_socket()` (no
destructor) — exactly the no-destructor discipline the unix path already uses
with `IntoRawFd` to avoid double-close. Getting this wrong = the Windows analog
of the `tcp_close` double-free that wedged the macOS demo (see
phase-6-runtime.md RESOLUTION (2)). This is the single highest-risk line.

**Listener vs stream:** `register_fd` is called for both the listener (parked on
accept-readiness) and per-connection streams (parked on read-readiness). Decide
whether to wrap as `TcpStream` uniformly (readiness interest is the same) or
distinguish `TcpListener` for the listener. PoC starts with `TcpStream` for both
— validate on Windows that accept-readiness fires correctly through it; fall back
to `TcpListener` for the listener fd if not.

**Non-blocking:** mio requires non-blocking sockets. The unix accept/bind path
sets `O_NONBLOCK`; the Windows socket creation path must set the socket
non-blocking (`FIONBIO` via `ioctlsocket`, or `socket2`'s `set_nonblocking`)
before registration. socket2 is already a dependency.

## Problem 2 — fd ABI width: `i32` is too narrow for a Windows `SOCKET`

Every fd FFI uses **`i32`**: `register_fd(raw_fd: i32, ...)`,
`deregister_fd(raw_fd: i32, ...)`, `tcp_accept(listener_fd: i32) -> i32`,
`tcp_close(fd: i32)`, etc. That is sound for unix `RawFd` (= `i32`) but **a
Windows `RawSocket` is `u64`** (`SOCKET` is a pointer-sized kernel handle).
Truncating it to `i32` is **unsound** — socket handles are usually small but not
guaranteed to fit in 31 bits.

### Decision: widen the fd ABI to `i64` uniformly (both platforms)

- Make every fd FFI param/return **`i64`**. Unix casts `i64 -> RawFd (i32)`
  internally; Windows casts `i64 -> RawSocket (u64)`. A uniform signature avoids
  per-platform ABI divergence in codegen (codegen emits one signature, not two).
- **This touches codegen**, not just the runtime: the FFI declarations and call
  sites that pass/receive fds (search `register_fd` / `tcp_accept` / `park_on_fd`
  emit sites in `src/codegen/` — `tcp.rs`, `declarations.rs`, `call_dispatch.rs`,
  `method_call.rs`). Each `i32` fd operand becomes `i64`.
- **It is fully testable on macOS/Linux** (it's a unix-observable ABI change):
  widen, then the existing `tests/park_and_wake.rs` + codegen E2E validate the
  i64 path on unix. So the widening is a **prep slice that lands and is verified
  on unix first**, leaving the Windows port to add only `#[cfg(windows)]` bodies
  against an already-uniform signature.

> **Sequencing caveat (concurrent agents):** the codegen FFI decl/call-site edits
> overlap files other agents are actively editing (the returned-borrows agent:
> `codegen.rs`/`call_dispatch.rs`/`method_call.rs`; owned-temp: codegen). Do the
> i64-widening prep slice when those are quiescent or coordinate, to avoid a
> collision on shared codegen files. Until then, the Windows bodies can be written
> against the *target* i64 signature in a branch and the widening rebased in.

## Problem 3 — Windows readiness state lifetime (found natively 2026-06-17)

**This is the bug Problem 1's "lifetime hazard" hand-waved at, and it is fatal
to the borrow-and-forget model — not a leak/double-close, but a *no readiness
is ever delivered* wedge.** Unlike unix `SourceFd` (stateless — epoll holds the
readiness state in the kernel), mio's Windows backend stores the AFD/IOCP
readiness association **inside** the `mio::net::TcpStream`'s `IoSourceState`
(`mio-1.2.0/src/sys/windows/mod.rs`: `InternalState { sock_state:
Pin<Arc<Mutex<SockState>>> }`), and `impl Drop for InternalState` calls
`sock_state.mark_delete()`, tearing the AFD poll down. The step-1/2 code did:

```rust
let mut source = source_from_socket(sock);     // owns IoSourceState
ev.register_with_cancel(&mut source, …)?;       // arms AFD
let _ = windows_iocp_bridge::release(source);   // into_raw_socket() → DROPS state → mark_delete
```

So the AFD association was destroyed **the instant `register_fd` returned**, and
the dispatcher's shard poll never delivered a single wakeup — every parked task
(listener *and* connected stream) hung. `cargo check` is blind to this; only the
native run of the dispatcher/accept-loop unit tests surfaced it (they wedged at 0
wakeups, the watchdog `abort()`ing the test binary).

### Fix: keep the owned source alive for the registration's lifetime

`windows_iocp_bridge::sources()` is a `Mutex<HashMap<u64 /*packed token*/,
mio::net::TcpStream>>`. `register_fd_impl` (Windows) `store`s the owned source
under its packed token instead of releasing it; `deregister_fd` `take`s the
**same** owned source back out (it carries the live `IoSourceState` — a fresh
wrapper would deregister a stateless source, a no-op that leaks the AFD poll) and
only then `release`s it (recovering the raw handle without closing). This makes
mio's Windows readiness flow to the dispatcher exactly as epoll does on unix. A
listening socket wrapped as `mio::net::TcpStream` **does** deliver accept-
readiness (AFD polls the raw handle for READABLE), so the spike's open
listener-vs-stream question resolves to "TcpStream works for both" — no
`TcpListener` split needed.

**Lesson for the spike's risk register:** "compiles under `cargo check --target
x86_64-pc-windows-msvc`" proved far weaker than assumed for the registration
model. The unix `SourceFd` mental model (borrow a raw fd, register, forget)
silently does not port to a stateful readiness backend; the i64-ABI/`RawSocket`
mechanics were the easy 90%, and the source-lifetime was the load-bearing 10%
that only a real run could catch.

> **Update:** the keep-the-source-alive fix made the *unit tests* and
> *sequential* runs pass, but a deeper variant of the same class — **Problem 4**
> — surfaced only under concurrent load in the real 10k AOT run.

## Problem 4 — per-park source churn → stale AFD polls → lost readiness (found via cdb, 2026-06-17)

Problem 3's fix kept one owned source alive **per registration token**, i.e. it
created a *fresh* `mio::net::TcpStream` (hence a fresh `SockState` and a fresh
`IOCTL_AFD_POLL`) on **every re-park of the same socket** — the natural
translation of the unix register/deregister-per-park codegen contract. That
passed unit tests and sequential loopback, but **wedged under concurrency** (the
10k AOT run): the server stopped accepting, sockets piled up in `CloseWait`, and
a cdb thread dump showed **every reactor thread idle in
`GetQueuedCompletionStatusEx`** with the main task parked — readiness that was
never delivered.

Root cause: mio's `deregister` only `mark_delete`s the old `SockState` (the
actual AFD-poll cancel is deferred to the selector's next `poll()`), so during a
re-park a socket could transiently carry **two** `SockState`s / AFD polls. Per
the AFD contract "there can be only one active `AFD_POLL` per (socket,
completion port)", the stale poll could complete and **consume the socket's
readiness against a now-dead token** — `take_registration` returns `None`, the
event is dropped, and the freshly-parked task never wakes. More shards hit it
faster (a connection fd and the listener hash to different shards, so the churn
overlaps across reactors); a single shard only delayed it.

### Fix: one persistent mio source per socket + `reregister`

`windows_iocp_bridge::sources` is now keyed by **`RawSocket`, not by token**.
The first park of a socket adopts it into a source and `register`s; **every
re-park `reregister`s the SAME source** (`EventLoop::reregister_with_cancel` →
`mio::Registry::reregister`, which reuses the existing `SockState` and re-arms it
in place — no second AFD poll). `deregister_fd` no longer tears the source down;
it just drops the by-token routing entry. The source — and its single
`SockState` — is reclaimed only by `karac_runtime_tcp_close` (which
`take_by_sock` + `release`s it before `closesocket`, avoiding double-ownership).
Result: exactly one AFD poll per socket for its whole lifetime, no stale
completion, no lost wakeup. **Validated: 10,000/10,000 @ concurrency 16, multi-
shard default, zero leak; the runtime's 249 unit tests stay green** (the rework
is behind the same register/deregister FFI). Two interim mitigations (a 1-shard
Windows default and a bounded poll backstop) were tried and **reverted** once the
real fix landed — multi-shard is clean without them.

**Lesson:** the unix register/deregister-per-park rhythm is cheap there
(`SourceFd` is stateless) but actively harmful on a stateful, deferred-cancel
readiness backend. On Windows, registration identity must track the **socket**,
not each park.

## FFI surface inventory

Platform-agnostic — **already compile/run on Windows, no change** (no raw fd):
`event_loop_wake`, `start_background_thread`, `shutdown_background_thread`,
`take_wakeups`, `scheduler_start_dispatcher`, `request_cancel_sweep`,
`scheduler_shutdown_dispatcher`, `scheduler_stats_snapshot`,
`park_slot_{new,bind_cancel,cancel_ptr,wait,signal,free}`.

fd-based — **need `#[cfg(windows)]` bodies** (RawFd→RawSocket + the bridge):
- Core registration: `event_loop_register_fd`, `event_loop_register_fd_cancel`,
  `event_loop_deregister_fd`, `event_loop_poll` (its fd handling).
- Socket I/O: `tcp_bind`, `tcp_accept`, `tcp_connect`, `tcp_read`, `tcp_write`,
  `tcp_close`, `test_bind_and_print_port`.
- WebSocket: `ws_send_*` (text/binary/masked), `ws_recv_text`, `ws_recv_binary`,
  `ws_accept`, `ws_accept_tls`.

The socket-I/O + WS layer is **mostly portable** — it's `std::net` /
`socket2` / framing logic. The unix-specific parts are narrow: `FromRawFd` →
`FromRawSocket`, `IntoRawFd` → `IntoRawSocket`, `O_NONBLOCK` → `ioctlsocket`,
and `libc::signal(SIGPIPE, SIG_IGN)` (`event_loop.rs:676`) which is a unix
no-op on Windows (Windows has no SIGPIPE; gate it out). `ws_accept_tls` adds
rustls, which is cross-platform.

## Validation strategy (what catches what)

1. **`cargo check --target x86_64-pc-windows-msvc` (works from macOS, confirmed
   exit 0).** Type-checks all `#[cfg(windows)]` code without a Windows box.
   Necessary but **insufficient** — proves it compiles, not that it works.
2. **`windows-latest` CI runner (`cargo test --all`).** Runs `event_loop.rs`
   unit tests. **Un-gate the existing event-loop unit tests for Windows** (many
   are `#[cfg(unix)]` today) so the IOCP path gets unit coverage. Note CI does
   **not** run `--features llvm`, so the real-binary park-on-fd E2E does not run
   in CI — see (3).
3. **A ~10k loopback functional run on a Windows box** (post-implementation).
   This is the only thing that catches the **wedge / wakeup-mis-routing** class
   of bug (which unit tests missed on Linux/macOS — see phase-6-runtime.md
   accept-loop wedge). Concurrency correctness shows at small N; 10k matches the
   macOS functional-parity precedent. Do NOT rely on CI alone for correctness.

## Windows box setup — what was actually used (2026-06-17) + step-6 blocker

Steps 4b/5 and the native validation of 1–4 were done on a **Windows Server 2025
Datacenter x86-64** box. What got installed and what's now blocking step 6:

**Toolchain installed (worked).** VS **2022 C++ Build Tools** (`winget install
--id Microsoft.VisualStudio.2022.BuildTools --source winget --override "--quiet
--wait --add Microsoft.VisualStudio.Workload.VCTools --add
Microsoft.VisualStudio.Component.Windows11SDK.22621 --includeRecommended"`) +
rustup (`Rustlang.Rustup`). **Pin `--source winget`** — the bare `winget install`
hit an `msstore` source cert error (`0x8a15005e`). Default host toolchain is
already `x86_64-pc-windows-msvc`; no extra target add needed for native work.
**`ring` 0.17 + `rustls` build natively under MSVC with no nasm** — the feared
cross-compile yak-shave does not exist *on the box*. fmt + clippy + the full
`--features tls,net,test-helpers` lib test suite all run with just cargo+MSVC.

**Step-6 blocker — LLVM 18.1 on Windows.** Step 6 (10k loopback) needs
`--features llvm` to emit real AOT binaries, and that is the unresolved problem:
- The official **llvm.org Windows installer ships no `llvm-config` and no dev
  libs**, so `llvm-sys` (the `llvm18-1-prefer-dynamic` / `LLVM_SYS_181_PREFIX`
  path) can't build against it. CI never exercises this — the `codegen-e2e` /
  `memory-sanitizer` LLVM jobs are **ubuntu-only** (apt `llvm-18-dev`); the
  `windows-latest` matrix leg runs plain `cargo test` (no `--features llvm`). So
  there is **no known-good prebuilt** to copy.
- A **from-source LLVM 18.1 build** needs ~25–30 GB (source + Release build tree
  + install prefix) on top of the OS (~27 GB) and a growing `target/`, plus
  cmake/ninja/python and ~1–3 h. The box this ran on had a **60 GB EBS volume
  fully allocated to C:** (no slack), leaving ~37 GB free — **too tight for the
  source build**. Options: (a) grow the EBS volume to ~100 GB in the AWS console
  (live-resizable) then `Resize-Partition C` and build LLVM from source; or
  (b) find/produce a prebuilt LLVM 18.1 *with* `llvm-config` + libs (~5 GB,
  fits) — the cheaper path if one can be sourced.

**Instance sizing (unchanged guidance).** x86-64 (matches `windows-latest`, dodges
LLVM-on-ARM); `m6i.2xlarge` (8 vCPU / 32 GB, non-burstable) or `t3.2xlarge`
Unlimited; **provision ≥100 GB gp3 up front** if you intend to build LLVM — the
30 GB base AMI root + VS tools + LLVM source build blow well past 60 GB.

**Prereqs.** Windows CI baseline is green (commit `842db493`). The Windows box
clones from origin, so **push the 4b/5 work to `main` (via a worktree + ff)** so
the box / CI actually has it before driving step 6.

**Driving step 6.** On the instance (with LLVM 18.1 in place + `LLVM_SYS_181_
PREFIX` set): build the four runtime archives natively (see CLAUDE.md), then run
the 10k loopback functional program and confirm no wedge / fd-leak. The
registration path it exercises is already unit-validated (Problem 3 fix), so
step 6 is now a *load/leak* confirmation, not a first-light correctness probe.

## Implementation plan (suggested order)

0. **(prep, on unix) — ✅ DONE 2026-06-15.** Widen fd ABI `i32 → i64` across
   runtime FFIs + codegen call sites; verified on macOS (`park_and_wake` 262 +
   codegen E2E 1605 + `memory_sanitizer` 27 + tcp/ws/tls codegen suites, fmt +
   `clippy --all-targets --features llvm -D warnings` clean). What landed (so the
   Windows port adds only `#[cfg(windows)]` bodies against a uniform signature):
   - **Kāra stdlib fd field** `i32 → i64` in `TcpListener`/`TcpStream`
     (`runtime/stdlib/tcp.kara`), `TlsListener`/`TlsStream` (`tls.kara`),
     `WebSocket` + `from_fd` (`ws.kara`). (`File`'s fd is **not** on the socket
     path — left i32; the Windows port widens it separately if needed.)
   - **Runtime FFI signatures** (`runtime/src/event_loop.rs` + `tls.rs`): every
     fd PARAM widened to i64; fd-RETURNS (`tcp_bind`/`tcp_connect`/`tcp_accept`/
     `ws_accept`/`ws_accept_tls`/`tls_listener_bind`/`tls_accept`/
     `tls_client_connect`/`test_bind_and_print_port`) widened to i64. Status-code
     returns (`deregister_fd`/`tcp_close`/`tls_close`) stay i32. Unix bodies
     narrow `i64 -> RawFd (i32)` at the top via `as RawFd`; `mio::unix::SourceFd`
     and the TLS `SESSIONS` map stay i32-keyed (register + lookup narrow
     identically). **Windows: alias `RawHandle = RawSocket (u64)` and narrow to
     that instead — the cast sites are already isolated at each body's top.**
   - **Codegen**: FFI declarations in `Codegen::new` (`src/codegen.rs`); the
     `karac_park_on_fd` state-struct fd field (`declarations.rs::
     synthesize_park_on_fd_layout` + the poll-fn fd load); the hardcoded socket
     struct layouts (`types_lowering.rs::llvm_type_for_name`) **and** the
     `struct_types`/`struct_field_type_names` seeding in `declarations.rs`
     (both must agree — the Result `Ok(x)` destructure reads the seeding); the
     hand-rolled `build_fd_construct_result` / `extract_fd_*` / `from_fd` packs;
     the hand-rolled drop bodies (`synth_drop.rs`). Signature-pinned IR tests
     updated (`tests/{ws_framing,tls_codegen,codegen}.rs`).
1. **fd-type abstraction** — ✅ **DONE 2026-06-15.** The `windows_iocp_bridge`
   (`source_from_socket` / `release`) is wired; `register_fd_impl` /
   `deregister_fd` narrow the i64 fd ABI to `RawSocket` (u64) and route through
   it. `shard_of_handle` is the `RawSocket` sibling of `shard_of_fd`.
2. **Core registration** `#[cfg(windows)]` — compiled 2026-06-15; ✅ **made
   actually work 2026-06-17.** `register_fd` / `register_fd_cancel` /
   `deregister_fd` ungated + given `#[cfg(windows)]` bodies; the unix
   wake-on-register + cancel-race guard is shared verbatim via the
   platform-agnostic `finish_register` helper. **The 2026-06-15 version
   compiled but delivered no readiness** (Problem 3): fixed by keeping the owned
   mio source alive in `windows_iocp_bridge::sources` for the registration's
   lifetime (store at register, take+release at deregister). Validated by the
   un-gated dispatcher/accept-loop/repark unit tests passing natively.
3. **Socket I/O** `#[cfg(windows)]` — ✅ **DONE 2026-06-15.** `tcp_bind` /
   `tcp_accept` / `tcp_connect` / `tcp_read` / `tcp_write` / `tcp_close` +
   `test_bind_and_print_port` mirrored with `FromRawSocket`/`IntoRawSocket`, with
   `set_nonblocking(true)` at each socket-creation point (mio AFD requires it; no
   SIGPIPE on Windows so nothing to gate out).
4. **WebSocket** `#[cfg(windows)]` — ✅ **plain-TCP surface DONE 2026-06-15**;
   **TLS-over-WS deferred** (see follow-on below). `ws_send_*` / `ws_recv_*` /
   `ws_accept` mirrored (the generic frame/handshake helpers were ungated from
   `#[cfg(unix)]` so both platforms share them; `ws_generate_mask_key` is now
   cross-platform). `ws_accept_tls` and the `#[cfg(feature = "tls")]`
   `lookup_session` dispatch are NOT ported — that is **step 4b** below.

   > **Verification (steps 1–4 plain):** `cargo check --target
   > x86_64-pc-windows-msvc -p karac-runtime --no-default-features --features
   > net,test-helpers` is **0 errors, 0 warnings**. The unix side stayed green
   > (fmt + clippy clean; `park_and_wake` / `ws_framing` / `tcp_listener` /
   > `tcp_stream` / `tls_codegen` E2E + 253 runtime unit tests all pass — the
   > `finish_register` extraction + ws-helper ungating are zero-behaviour
   > refactors). The **full (TLS) build cannot be cross-checked from macOS** —
   > `ring`'s C/asm won't cross-compile for `windows-msvc` from the Mac; that arm
   > validates natively on the Windows box.

4b. **TLS-over-Windows** — ✅ **DONE + validated natively 2026-06-17.** The TLS
   FFI surface is now cross-platform (the `#[cfg(unix)]` / `#[cfg(all(unix,
   feature = "tls"))]` gates dropped): `tls.rs`'s `tls_listener_bind` /
   `tls_accept` / `tls_read` / `tls_write` / `tls_close` / `tls_client_connect`,
   plus `event_loop.rs`'s `ws_accept_tls` + the handshake worker pool
   (`WsHandshakePool` / `ws_handshake_conn_tls` / `ws_handshake_pool_for` /
   `ws_handshake_worker`), `TlsConnIo` / `DeadlineStream`, and the
   `ws_send_data_frame` / `ws_recv_data_frame` TLS-dispatch branch (added to the
   Windows bodies). The TLS `SESSIONS` key + the pool's work/done queues + the
   handshake-fd plumbing are all the **cfg-aliased `crate::tls::SessionKey`**
   (`RawFd` i32 on unix / `RawSocket` u64 on windows; zero unix change), and the
   raw-handle ops go through `tls.rs`'s `tcp{stream,listener}_{from,into,as}_key`
   helpers (`pub(crate)`, shared with the WS-over-TLS path). `ring`/`rustls`
   compile natively under MSVC (no nasm). Validated: the un-gated `tls::tests`
   (`round_trip_echo`, `round_trip_via_client_connect_ffi`,
   `ws_accept_tls_succeeds_with_nonblocking_listener`, handshake-failure +
   config-parse cases) all pass natively. The unix TLS path is zero-behaviour by
   construction (CI confirms on push — `ring` can't cross-compile *to* linux from
   the Windows box, the mirror of the original macOS limitation).
5. **Un-gate event-loop unit tests** — ✅ **DONE 2026-06-17.** The previously
   `#[cfg(unix)]`-gated event-loop + TLS unit tests are un-gated and green
   natively (`net`: 221 passed; `tls`: 249 passed; fmt + clippy clean for both
   feature sets). Tests that use raw-fd APIs go through new cross-platform test
   helpers (`loopback_pair` / `listener_as_handle` / `stream_{from,into,as}_handle`
   / `listener_{from,into}_handle`); genuinely-unix-only ones stay gated with a
   rationale (`reactor_init_masks_sigpipe` = SIGPIPE/`sigaction`;
   `shutdown_drain_*` = `UnixStream::pair`/`SourceFd`; `net_construct_error_codes_
   surface_from_real_syscalls` = `SO_REUSEADDR` has *hijack* semantics on Windows
   so an in-use rebind succeeds, not `EADDRINUSE`; `shard_of_fd` has a `RawSocket`
   twin). CI's Linux-only clippy never lints `#[cfg(windows)]` code — running it
   natively also fixed 6 pre-existing `missing_safety_doc` lints on the Windows WS
   FFIs. **CI wiring DONE:** the `test` matrix's `windows-latest` leg runs
   `cargo test --all` with default features (`tls` ⊇ `net`), and the un-gated
   tests live in a plain `#[cfg(test)] mod tests` — so the runner already
   exercises them (verified: the module is not OS-gated). The clippy gap is now
   closed by a dedicated **`windows-lint`** job (`.github/workflows/ci.yml`):
   `cargo clippy -p karac-runtime --all-targets --features tls,net,test-helpers
   -- -D warnings` on `windows-latest`, guarding the `#[cfg(windows)]` runtime
   surface against regressions the ubuntu `lint` job structurally can't see.
   (The `#[cfg(windows)]` codegen linker driver stays uncovered pending LLVM 18
   on the Windows runner — the same yak-shave that keeps all `--features llvm`
   E2E off Windows CI.)
5a. **AOT toolchain port (unscoped by the original plan)** — ✅ **DONE
   2026-06-17.** Step 6 needs real AOT binaries, which surfaced that the karac
   *build pipeline* was unix-only — not just the runtime. What landed:
   - **LLVM 18.1.8 built from source** on the box (the official Windows binaries
     ship no `llvm-config` / dev libs for `llvm-sys`). **Must use
     `LLVM_TARGETS_TO_BUILD=all`** — inkwell references every backend's
     `LLVMInitialize*Target`, so an X86-only build fails to link karac. Static
     link (llvm-sys's `prefer-dynamic` *falls back to static on MSVC* — dynamic
     libLLVM is unsupported there, so no dylib yak-shave). `-DLLVM_ENABLE_DIA_SDK=
     OFF` avoids needing the ATL component.
   - **Linker-driver port** (`src/codegen/driver.rs`): a `#[cfg(windows)]`
     `link_executable_windows` using **clang** as the driver (it auto-detects the
     VS toolchain + CRT + `mainCRTStartup→main` entry), the Windows system import
     libs the runtime references (`ws2_32 bcrypt advapi32 userenv ntdll dbghelp
     kernel32` + `legacy_stdio_definitions` for the header-inline `printf`),
     `/OPT:REF` for dead-strip, no `strip`. `resolve_runtime_path` learned the
     `karac_runtime.lib` MSVC archive name. Codegen already emits a correct COFF
     object (LLVM default host triple). (`winmm` was later added to this list for
     the reactor's `timeBeginPeriod(1)` timer-resolution call — `8f0c56c6`, see
     `windows-iocp-scale-investigation.md` Finding 2; the driver names system libs
     explicitly, so it must carry every lib the runtime's `#[link]` directives name.)
   - **Two codegen stdio fixes** (`src/codegen.rs` / `control_flow.rs`): MSVC's
     UCRT has **no `stdout`/`stderr` data symbol** (`<stdio.h>` macros over
     `__acrt_iob_func(n)`) — codegen now emits that call on Windows instead of
     loading a `stdout` global; and `printf` resolves via
     `legacy_stdio_definitions` (above). Without these, AOT programs fail to link
     (`lld-link: error: undefined symbol: stdout` / `printf`).
6. **10k loopback functional run** — ✅ **DONE 2026-06-17.** `examples/std_net/
   ws_echo.kara` (plain-WS echo → the IOCP `WebSocket.accept` park path) built
   AOT and driven by `examples/std_net/ws_loop_client.py` (10k short-lived WS
   round-trips). **10,000/10,000 @ concurrency 16, ~1,400/s, zero handle leak, no
   wedge — default multi-shard config** (after the Problem 4 fix). This is the run
   that caught Problem 4 (and bug 3, the non-blocking-accept). Flips the M3
   Windows parity clause + line 13 + line 19.

   > **Scale re-validation — 250k, 2026-06-17.** Re-ran the same AOT `ws_echo`
   > server at **250,000/250,000 @ concurrency 16, 1,455/s, 171.8s** on a Windows
   > Server 2025 box (default multi-shard, 8 shards/8 cores). Server handle count
   > **flat at 91→96 across the whole run** (zero handle/socket leak), 5 MiB RSS
   > steady, 16 ESTABLISHED in-flight throughout, and a post-run liveness probe
   > still PASSed (no wedge). 25× the original step-6 scale with no degradation —
   > the per-socket persistent-source model (Problem 4 fix) holds at scale.
   > Driver: `examples/std_net/ws_loop_client_abortive.py`, a sibling of the step-6
   > client that uses an **abortive (RST) close** (`SO_LINGER 0`) so the client's
   > ephemeral ports never park in TIME_WAIT — the only thing that makes a
   > >16k-connection loopback run fit the default ~16,384-port pool without OS
   > tuning. The server-side IOCP register→park→wake→deregister cycle is identical
   > (the final `recv` returns ECONNRESET instead of a 0-length read; same
   > deregister/close path).

## Open questions — resolved natively 2026-06-17

- **Listener wrapped as `mio::net::TcpStream` vs `TcpListener`?** ✅ Resolved:
  `TcpStream` works for both. AFD polls the raw handle for READABLE, which is
  accept-readiness for a listening socket — `dispatcher_drives_parked_task_to_
  completion_on_wakeup` (listener) and the accept-loop burst test both pass with
  the single `TcpStream` wrapper. No split needed.
- **Non-blocking setup / readiness lost between accept and register?** ✅ The
  existing `set_nonblocking(true)` at each socket-creation point is sufficient;
  no readiness is lost (accept-loop burst test drains 40 connections without
  wedging). Windows accepted sockets do not inherit non-blocking from the
  listener (unlike BSD), so the `set_nonblocking(false)` reset in `ws_accept_tls`
  is a harmless no-op there.
- **`into_raw_socket()` lifetime correctness.** ✅ The real hazard turned out to
  be the *opposite* of a double-close: releasing the source too **early** tore
  down readiness (Problem 3). Fixed by keeping the source alive until deregister.
  Concurrent-churn leak/UAF validation under load still wants the 10k run (step
  6) for full confidence, but the unit-level register→wake→deregister→re-register
  cycle (repark + accept-loop tests) exercises the claim path and is green.
- **`shard_of_handle` distribution for Windows `SOCKET` values?** Functionally
  fine at unit scale (the shard test covers the full `u64` handle space); even
  distribution under real load is a step-6 observation, not a correctness gate.

## Cross-links

- Tracker: `docs/implementation_checklist/phase-6-runtime.md` — Open work front
  "Slice 10 — Windows IOCP integration" (line 13) + "Windows IOCP cancel-sweep"
  (line 19); detailed "Slice 6 — M3 ... cross-platform parity".
- `docs/roadmap.md` M3 (parity) row — Windows IOCP is the remaining v1-launch
  parity gate.
- Prereq met: Windows CI baseline is green (componentize test fix, `842db493`).
