# Windows IOCP event-loop backend — design + scoping (Phase 6, line 13)

> **Status:** implementation in progress (2026-06-07 scoping). **Steps 0–4
> (plain) DONE + verified on unix / cross-checked for Windows, 2026-06-15:** the
> i64 fd ABI widening (step 0), the `#[cfg(windows)]` register/deregister bodies
> (steps 1–2), all `tcp_*` socket I/O (step 3), and the plain-TCP WebSocket
> surface (`ws_send_*`/`ws_recv_*`/`ws_accept`, step 4) are written and
> cross-compile clean (`cargo check --target x86_64-pc-windows-msvc … --features
> net,test-helpers`: 0 errors, 0 warnings), with the unix path fully green. See
> the Implementation plan below for exactly what landed. **Remaining: step 4b
> (TLS-over-Windows — deferred to the Windows box), step 5 (un-gate unit tests),
> step 6 (10k loopback run).** Those need a real Windows box (no Windows runtime
> testing — or `ring`/TLS cross-compile — is possible from macOS). Authored on
> macOS via the `cargo check --target x86_64-pc-windows-msvc` cross-check loop.
> Tracks phase-6-runtime.md "Open work front" **Slice 10 — Windows IOCP
> integration** and its sibling **Windows IOCP cancel-sweep** (line 19).

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
2. **Core registration** `#[cfg(windows)]` — ✅ **DONE 2026-06-15.**
   `register_fd` / `register_fd_cancel` / `deregister_fd` ungated + given
   `#[cfg(windows)]` bodies; the unix wake-on-register + cancel-race guard is
   shared verbatim via the new platform-agnostic `finish_register` helper.
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

4b. **(deferred) TLS-over-Windows**: `#[cfg(windows)]` bodies for `tls.rs`'s
   `tls_listener_bind` / `tls_accept` / `tls_read` / `tls_write` / `tls_close` /
   `tls_client_connect`, plus `ws_accept_tls` + its handshake worker pool, plus
   the `ws_send_data_frame` / `ws_recv_data_frame` TLS dispatch branch. Prereq:
   widen the TLS `SESSIONS` map key past `i32` (a `SOCKET` is u64 — make it a
   `cfg`-aliased `SessionKey` = `RawFd` on unix / `RawSocket` on windows, zero
   unix change). Best done **on the Windows box** alongside step 6, since rustls
   handshake behaviour (and `ring`) need native validation anyway.
5. **Un-gate event-loop unit tests** for Windows; green on `windows-latest`.
6. **10k loopback functional run** on a Windows box (x86-64 EC2/Azure dev
   instance, or a Win-on-ARM VM). Flip the M3 Windows parity clause + line 13
   *and* line 19 (cancel-sweep rides along).

## Open questions to resolve on Windows (need a real run loop)

- Does accept-readiness fire correctly when the **listener** is wrapped as
  `mio::net::TcpStream` vs requiring `TcpListener`? (Step 1 PoC assumes
  `TcpStream` for both; confirm or split.)
- Exact non-blocking setup point for accepted sockets (mio requires it; verify
  no readiness is lost between accept and register).
- `into_raw_socket()` lifetime correctness under the dispatcher's concurrent
  deregister + the cancel-sweep's `take_registration_with_cancel` claim — the
  double-close hazard, validated only by an ASAN-equivalent / leak check on
  Windows + the 10k churn run.
- Whether `shard_of_fd(raw_fd)` (fd-hash sharding) distributes Windows SOCKET
  handle values acceptably (handles aren't sequential like unix fds).

## Cross-links

- Tracker: `docs/implementation_checklist/phase-6-runtime.md` — Open work front
  "Slice 10 — Windows IOCP integration" (line 13) + "Windows IOCP cancel-sweep"
  (line 19); detailed "Slice 6 — M3 ... cross-platform parity".
- `docs/roadmap.md` M3 (parity) row — Windows IOCP is the remaining v1-launch
  parity gate.
- Prereq met: Windows CI baseline is green (componentize test fix, `842db493`).
