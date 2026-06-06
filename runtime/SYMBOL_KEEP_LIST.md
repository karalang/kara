# Runtime symbol keep-list

Pre-flight audit of every load-bearing symbol declared in `runtime/src/`,
produced for the Phase 1 binary-size optimization slice (`strip -x` post-link
+ `panic = "abort"`) and re-run for Phase 2 (cross-archive LTO +
`-Wl,-dead_strip` on macOS / `-Wl,--gc-sections` on Linux, both landed
2026-05-12).

The audit covers the attribute set: `#[used]`, `#[link_section(…)]`,
`#[ctor]`, `#[dtor]`, `#[no_mangle]`, and `extern "C"` declarations.

**Phase 2 outcome.** The runtime declares zero `#[used]` /
`#[link_section]` / `#[ctor]` / `#[dtor]` attributes, so every reachable
runtime symbol is anchored through a direct call from codegen-emitted IR.
DCE preserves them through entry-point reachability with no explicit
keep-list directive needed at the linker level. Validated on macOS
2026-05-12: `cargo test --features llvm` clean (215 codegen tests,
848 typechecker tests, 28 walker tests, etc.); LeetCode #1665 bench
binary 2.5 MB → 375 KB without correctness regression.

## Audit method

```sh
rg '#\[used\]|#\[link_section|#\[ctor\]|#\[dtor\]|#\[no_mangle\]|extern "C"' runtime/src/
```

Re-run on every runtime change; this file should grow only when a new
`#[no_mangle]` or `extern "C"` declaration lands in `runtime/src/`.

## Findings

### Attributes NOT present in the runtime

These attribute kinds were searched and produced **zero** matches as of
2026-05-12 (re-confirmed from the original 2026-05-07 sweep):

- `#[used]`
- `#[link_section(…)]`
- `#[ctor]`
- `#[dtor]`

Implication: no static-init / static-fini / forced-keep machinery exists in
the runtime. A future `#[ctor]` or `#[link_section]` addition (e.g., a panic-
handler section, a static registration table) would need an explicit
keep-list entry here *before* it lands, paired with whatever linker flag
preserves the section across DCE on each target.

### `#[no_mangle] extern "C"` exports — runtime entry points

Every symbol below is called by codegen-emitted LLVM IR through its `cc`-style
ABI signature. Stripping any of these from the static archive (`#[used]` is
unnecessary since `#[no_mangle]` + a referencing call site already pins the
symbol) — or DCE-stripping them from the linked executable — would cause
runtime link failures or undefined behavior.

`strip -x` (Phase 1) keeps **all** of these intact: `-x` strips local
non-global symbols only; `#[no_mangle] extern "C"` exports are emitted as
external-linkage globals (Mach-O `EXT`, ELF `STB_GLOBAL`) and are
preserved by definition.

LTO + `-Wl,-dead_strip` (Phase 2) needs explicit per-symbol awareness. The
keep-list pattern is each symbol below; the Phase 2 PR's checklist must
verify none gets stripped.

#### `runtime/src/lib.rs`

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_par_run` | `unsafe extern "C" fn(branches: *const KaracBranch, count: usize)` | `par {}` block executor — fixed-size thread pool, fail-fast cancellation. |
| `karac_error_trace_push` | `unsafe extern "C" fn(file_ptr: *const u8, file_len: usize, line: u32, col: u32)` | Push a frame onto the global `?` error-return trace at every `?` failure site. |
| `karac_error_trace_clear` | `extern "C" fn()` | Reset the global `?` error-return trace at every `?` success site. |
| `karac_provider_push` / `karac_provider_pop` / `karac_provider_lookup` / `karac_provider_set_stack_head` / `karac_provider_get_stack_head` | Provider-stack ops | Per-thread `with` block provider injection. Codegen emits push/pop bracketing every `with` scope; `lookup` resolves resource accesses; the get/set head pair lets `par {}` workers inherit the parent thread's provider stack. |
| `karac_runtime_get_current_frame` / `karac_runtime_for_each_active_frame` | Frame walkers | Read the active-frame chain for debug metadata + spawn-site introspection. |
| `karac_runtime_has_debug_metadata` | `extern "C" fn() -> bool` | True iff codegen emitted a non-default `KARAC_SPAWN_SITES` table; queries the linked-in static. |
| `karac_runtime_list_par_blocks_into` | `unsafe extern "C" fn(out: *mut KaracVec)` | Read the spawn-site table into a `Vec<KaracSpawnSiteEntry>` for tooling. |
| `karac_runtime_json_parse` / `karac_runtime_json_stringify` / `karac_runtime_json_free_value` / `karac_runtime_json_free_string` | JSON FFI | Slice F's `std.json` backing impl. Drag the `serde_json` subgraph into the link only if a Kāra program imports `std.json` — Phase 2 DCE strips them otherwise. |
| `karac_runtime_http_response_set_body` / `_set_status` / `karac_runtime_http_request_path` / `_request_method` / `_request_body_ptr` / `_request_body_len` / `_request_header` | HTTP FFI getters/setters | Slice B's `std.http` request/response accessors. |
| `karac_runtime_serve_http` / `karac_runtime_serve_http_static` | HTTP FFI server | Slice B's `Server.serve` / `Server.serve_static`. Roots the tokio + hyper + h2 subgraph; Phase 2 DCE strips the entire subgraph when no Kāra program imports `std.http`. |
| `karac_vec_sort_by` | `unsafe extern "C" fn(data: *mut u8, len: i64, elem_size: i64, cmp: extern "C" fn(*mut u8, *const u8, *const u8) -> i64, ctx: *mut u8)` | `Vec.sort_by` backing impl. 8-byte and 16-byte element fast paths inline-monomorphize via `slice::sort_by`; fallback path is index-sort + permute. |

Plus one `extern "C"` block importing libc:

| Imported symbol | Signature | Purpose |
|---|---|---|
| `atexit` (POSIX libc) | `extern "C" fn(callback: extern "C" fn()) -> i32` | Registers `print_trace_at_exit` to flush the `?` trace on normal program exit. Lazy/idempotent — programs that never push a trace frame skip the registration. |

And one private `extern "C"` callback registered with `atexit`:

| Symbol | Signature | Purpose |
|---|---|---|
| `print_trace_at_exit` | `extern "C" fn()` | Module-private. The `extern "C"` ABI is the one `atexit(3)` requires; the symbol is not a public runtime export. Reachable only after `register_trace_atexit_once` arms it. |

#### `runtime/src/event_loop.rs`

Phase 6 line 17 slice 1 (2026-05-17) — runtime FFI surface for the
network event loop. Codegen emits calls into these entry points at
the network-effect call boundaries surfaced by the state-machine
transform (phase 6 line 18). Unix-only on the fd-registration entries;
`poll` and `wake` are cross-platform.

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_runtime_event_loop_register_fd` (unix) | `extern "C" fn(raw_fd: i32, direction: u8, parked: *mut c_void) -> u64` | Register a raw fd with the process-global event loop; returns a non-zero token on success. |
| `karac_runtime_event_loop_deregister_fd` (unix) | `extern "C" fn(raw_fd: i32, token: u64) -> i32` | Remove a previously registered fd; 0 = success, -1 = error. |
| `karac_runtime_event_loop_poll` | `unsafe extern "C" fn(max_wait_nanos: i64, wakeups_out: *mut KaracWakeup, max_wakeups: usize) -> usize` | Drive the loop once and write ready entries into the caller's buffer. -1 = block, 0 = non-blocking, n>0 = wait n ns. Returns 0 immediately when the background poller thread is running (slice 3+). |
| `karac_runtime_event_loop_wake` | `extern "C" fn() -> i32` | Wake the loop from a non-event-loop thread. Backed by `mio::Waker` (eventfd / pipe / IOCP-post). |
| `karac_runtime_event_loop_start_background_thread` | `extern "C" fn() -> i32` | Slice 3. Spawn the background poller thread; idempotent (a second call returns 0 without re-spawning). |
| `karac_runtime_event_loop_take_wakeups` | `unsafe extern "C" fn(out: *mut KaracWakeup, max: usize, timeout_nanos: i64) -> usize` | Slice 3. Drain wakeups from the background poller's queue. -1 = block, 0 = non-blocking, n>0 = wait n ns. |
| `karac_runtime_event_loop_shutdown_background_thread` | `extern "C" fn() -> i32` | Slice 3. Signal the background poller to stop, unblock its poll via the waker, join, and clear the global slot. -1 if not running. |
| `karac_runtime_scheduler_start_dispatcher` | `extern "C" fn() -> i32` | Slice 4. Spawn the scheduler dispatcher thread; auto-starts the background poller if not running. Idempotent. |
| `karac_runtime_scheduler_shutdown_dispatcher` | `extern "C" fn() -> i32` | Slice 4. Signal the dispatcher to stop, join the thread, clear the global slot. -1 if not running. Does NOT stop the poller. |
| `karac_runtime_scheduler_stats_snapshot` | `unsafe extern "C" fn(out: *mut KaracSchedulerStats) -> i32` | Slice 5. Read the dispatcher's atomic counters (polls / ready_observations / err_observations / pending_observations) into a caller-allocated buffer. -1 if not running. |
| `karac_runtime_tcp_bind` (unix) | `unsafe extern "C" fn(addr_ptr: *const u8, addr_len: i64) -> i32` | Slice 8. Backs `TcpListener.bind(addr)`. Binds a TCP listener, prints `BOUND_PORT=<n>` for ephemeral binds, returns the raw fd. -1 on error. |
| `karac_runtime_tcp_accept` (unix) | `extern "C" fn(listener_fd: i32) -> i32` | Slice 8. Backs `TcpListener.accept(self)`'s syscall step (after codegen-emitted park). Raw `accept(2)` — no parking, no event-loop interaction. Returns new connection fd; -1 on error. |
| `karac_runtime_tcp_read` (unix) | `unsafe extern "C" fn(stream_fd: i32, buf_ptr: *mut u8, buf_len: i64) -> i64` | Slice 9. Backs `TcpStream.read(self, buf)`'s syscall step (after codegen-emitted park). Raw `read(2)`. Returns byte count; 0 on EOF; -1 on error. |
| `karac_runtime_tcp_write` (unix) | `unsafe extern "C" fn(stream_fd: i32, buf_ptr: *const u8, buf_len: i64) -> i64` | Slice 9. Backs `TcpStream.write(self, buf)`'s syscall step (after codegen-emitted park). Raw `write(2)`. Returns byte count; -1 on error. |
| `karac_runtime_tcp_close` (unix) | `extern "C" fn(fd: i32) -> i32` | Slice 9d. Closes a TCP fd at scope exit. Backs `@TcpStream.drop` / `@TcpListener.drop` hand-rolled codegen bodies. `-1` fd is a no-op (matches the per-method "no-fd" sentinel convention used by `bind` / `accept`). Returns 0. |
| `karac_runtime_ws_send_text` (unix) | `unsafe extern "C" fn(fd: i32, msg_ptr: *const u8, msg_len: i64) -> i64` | Slice 9e.1. Encodes a single RFC 6455 TEXT frame (FIN=1, opcode=0x1, MASK=0 — server→client) and writes header + payload to `fd`. Returns `msg_len` on success, -1 on any write error. v1 limitations: text-only, FIN=1 unfragmented; binary / fragmentation / control frames land in slice 9e.3. |
| `karac_runtime_ws_recv_text` (unix) | `unsafe extern "C" fn(fd: i32, out_ptr: *mut u8, out_max_len: i64) -> i64` | Slice 9e.1. Reads one client→server RFC 6455 TEXT frame from `fd`, validates header (FIN=1, opcode=0x1, MASK=1, RSV=000), unmasks payload, writes up to `out_max_len` bytes into `out_ptr`. Returns payload byte count on success, 0 on graceful EOF before complete frame, -1 on protocol error / IO error / oversize payload. |
| `karac_runtime_ws_accept` (unix) | `extern "C" fn(listener_fd: i32) -> i32` | Slice 9e.2. Accepts a TCP connection on `listener_fd`, reads HTTP/1.1 request, computes `Sec-WebSocket-Accept` from the request's `Sec-WebSocket-Key` (SHA-1 + Base64 per RFC 6455 §4.2), writes the 101 Switching Protocols response. Returns the upgraded connection fd on success, -1 on any failure. |
| `karac_runtime_ws_send_binary` (unix) | `unsafe extern "C" fn(fd: i32, msg_ptr: *const u8, msg_len: i64) -> i64` | Slice 9e.3. BINARY counterpart to `_send_text`: encodes opcode 0x2 single-frame, unmasked. Same convention otherwise. |
| `karac_runtime_ws_recv_binary` (unix) | `unsafe extern "C" fn(fd: i32, out_ptr: *mut u8, out_max_len: i64) -> i64` | Slice 9e.3. BINARY counterpart to `_recv_text`: accepts opcode 0x2 instead of 0x1. Transparently handles inbound control frames (ping → pong reply, pong → discard, close → close-response + return 0) per RFC 6455 §5.5. Slice 9e.4 added fragmentation reassembly to both `_recv_text` and `_recv_binary`. |
| `karac_runtime_ws_send_text_masked` (unix) | `unsafe extern "C" fn(fd: i32, msg_ptr: *const u8, msg_len: i64) -> i64` | Slice 9e.4. Client-side masked text send (RFC 6455 §5.1 client→server convention). Generates a random 4-byte mask key per call via `/dev/urandom` (clock-derived LCG fallback). Same `msg_len`/-1 return ABI as `_send_text`. |
| `karac_runtime_ws_send_binary_masked` (unix) | `unsafe extern "C" fn(fd: i32, msg_ptr: *const u8, msg_len: i64) -> i64` | Slice 9e.4. BINARY counterpart to `_send_text_masked`. |

Repr-C type also exported: `KaracSchedulerStats { polls: u64, ready_observations: u64, err_observations: u64, pending_observations: u64 }` — written by `karac_runtime_scheduler_stats_snapshot` into the caller-allocated buffer.

Repr-C type also exported: `KaracWakeup { token: u64, parked: *mut c_void, direction: u8 }` — written by `karac_runtime_event_loop_poll` into the caller-allocated buffer.

#### `runtime/src/scheduler.rs`

Phase 6 line 218 slice 3 (2026-05-27) — fresh-task dispatch for `spawn()`
and (future-slice) `TaskGroup`. Codegen at every `spawn(closure)` call
site emits a wrapper conforming to the `SpawnFn` signature, calls
`karac_runtime_spawn` to submit it, and stores the returned handle
address (as `i64`) into the `TaskHandle[T].task_id` field. `.join()`
codegen calls back into `karac_runtime_task_join` against the same
handle. Cross-platform.

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_runtime_spawn` | `unsafe extern "C" fn(fn_ptr: SpawnFn, env: *mut c_void, result_size: usize, result_align: usize) -> *mut KaracTaskHandle` | Submit a fresh task to the global worker pool. Allocates a handle + per-task result buffer; pushes a 1-task `Task` onto the same MPMC queue `karac_par_run` drains. |
| `karac_runtime_spawn_coro` | `unsafe extern "C" fn(wrap_fn: CoroSpawnFn, env: *mut c_void) -> *mut KaracTaskHandle` | A2 slice 5a — density-optimal non-blocking coroutine spawn. Allocates a handle + a bound `KaracParkSlot`; enqueues a worker that only *ramps* the coroutine (`wrap_fn(env, slot, cancel)` — register fd + suspend + return) and is freed immediately. The dispatcher drives the parked coroutine; its body signals the slot at completion. `karac_runtime_task_join` on the handle waits on that slot (not the worker). Unit result. |
| `karac_runtime_task_join` | `unsafe extern "C" fn(handle: *mut KaracTaskHandle, out_slot: *mut u8) -> u8` | Block until the task reaches a terminal state, memcpy the result into `*out_slot` on `COMPLETED`, free the handle. Returns a `TASK_STATE_*` discriminant (`COMPLETED` = 1, `PANICKED` = 2, `CANCELLED` = 3). |
| `karac_runtime_task_handle_free` | `unsafe extern "C" fn(handle: *mut KaracTaskHandle)` | Release a handle without joining. Caller must ensure the task has reached a terminal state (e.g. via prior `karac_runtime_task_state` poll). TaskGroup-side cleanup will route through here in slice 5. |
| `karac_runtime_task_state` | `unsafe extern "C" fn(handle: *const KaracTaskHandle) -> u8` | Non-blocking peek at the task's state. Returns `PENDING` while in flight, or one of the terminal discriminants. |
| `karac_runtime_taskgroup_cancel` | `unsafe extern "C" fn(group: *mut KaracTaskGroupHandle)` | A2 slice 5b-1. Flip every registered child's per-task `cancel` flag. Backs the user-callable `TaskGroup.cancel()`. Inert until the dispatcher routes the per-task flag to parked coroutines (slice 5c). |

`SpawnFn` typedef: `unsafe extern "C" fn(env: *mut c_void, result_out: *mut u8, cancel: *const AtomicBool)`. Codegen-emitted wrappers read captured environments from `env`, run the user closure body, and memcpy the T-typed return value into `*result_out`.

`KaracTaskHandle` is opaque to codegen (handle pointers flow through `i64` casts only); the runtime owns its layout (`state: AtomicU8`, `result_buf: *mut u8`, `result_layout: Layout`, `cancel: AtomicBool`, `notify_mutex: Mutex<()>`, `notify_cv: Condvar`).

`TASK_STATE_*` discriminants pinned: `PENDING = 0`, `COMPLETED = 1`, `PANICKED = 2`, `CANCELLED = 3` (regression-guarded by `tests::task_state_constants_pinned`). `PANICKED` is API-future-proofing for `catch_panic[T]` integration; under v1's `panic = "abort"` release profile a panicking spawn closure aborts the process before reaching the terminal-state write.

#### `runtime/src/clone.rs`

| Symbol | Signature (C ABI) | Purpose |
|---|---|---|
| `karac_string_clone` | `unsafe extern "C" fn(src: *const c_void, dst: *mut c_void)` | Deep-copy a Kāra `String { data, len, cap }` value, with the static-literal `cap == 0` special case that forces the clone onto the heap. |

#### `runtime/src/map.rs`

The type-erased open-addressing hash map's full C ABI surface. Emitted by
codegen for every `Map[K, V]` operation; concrete `hash_fn` / `eq_fn` are
codegen-monomorphized and passed in at construction.

| Symbol | Purpose |
|---|---|
| `karac_map_new` | Construct a fresh map. |
| `karac_map_free` | Destroy a map and free its storage. |
| `karac_map_free_with_drop_vec` | Same as `karac_map_free` but walks live buckets first and frees per-entry key and/or value `Vec[T]` / `String` data buffers per the `drop_key` / `drop_val` i32 flags. Selected when either side follows the Vec/String `{ptr, len, cap}` layout — `Set[Vec[T]]` / `Set[String]` (key only), `Map[String, V]` / `Map[Vec[T], V]` (key only), `Map[K, Vec[U]]` (val only), `Map[String, Vec[U]]` (both). Both flags zero is equivalent to plain `karac_map_free`. |
| `karac_map_insert` | Insert without surfacing the displaced value. |
| `karac_map_insert_old` | Insert + surface the displaced value (`Map.insert → Option[V]`). |
| `karac_map_get` | Read a value by key. |
| `karac_map_remove` | Remove by key, return presence boolean. |
| `karac_map_remove_old` | Remove by key, surface the removed value (`Map.remove → Option[V]`). |
| `karac_map_contains` | Presence check. |
| `karac_map_entry` | Probe-and-insert-on-vacant for `entry(k).or_insert*` chains. |
| `karac_map_lookup_slot` | Read-only slot lookup for `entry(k).and_modify` chains. |
| `karac_map_len` | Live-entry count. |
| `karac_map_clear` | Drop every entry; keep the bucket allocation. |
| `karac_map_iter_new` | Construct a forward iterator. |
| `karac_map_iter_next` | Advance the iterator; copies key+val out. |
| `karac_map_iter_free` | Destroy the iterator. |

(All declared `pub unsafe extern "C"` with `#[no_mangle]`.)

### Other `extern "C" fn` types in the surface

These appear as **type signatures** rather than declared symbols — they are
function-pointer types passed in by the codegen at runtime, so the runtime
itself never declares the underlying symbol; the compiler emits the
monomorphized concrete function and hands a pointer through the C ABI.

- `KaracBranch::func` — `unsafe extern "C" fn(*mut c_void, *const AtomicBool)`. Each `par {}` branch's body, emitted per-call-site by codegen.
- `KaracMap` `hash_fn` — `unsafe extern "C" fn(*const c_void) -> u64`. Per-K-type hash function emitted by codegen.
- `KaracMap` `eq_fn` — `unsafe extern "C" fn(*const c_void, *const c_void) -> bool`. Per-K-type equality function emitted by codegen.

Phase 2 LTO/DCE relevance: these are *compiler*-emitted symbols — the keep-list
guarantee for them lives in `src/codegen.rs`, not in this runtime audit.
Today they are reachable through indirect calls only, so cross-archive LTO
cannot prove them dead via local visibility analysis. If a future Phase 2
configuration starts running whole-program devirtualization, these may need
an explicit keep-list at the codegen end.

## Summary

- **Total `#[no_mangle]` exports (2026-05-26 audit):** 59.
  - 4 carried over from 2026-05-07: `karac_par_run`, `karac_error_trace_push`, `karac_error_trace_clear`, `karac_string_clone`.
  - 15 `karac_map_*` (unchanged from 2026-05-07).
  - 18 added 2026-05-07 → 2026-05-12: 5 `karac_provider_*`, `karac_runtime_get_current_frame`, `karac_runtime_for_each_active_frame`, `karac_runtime_has_debug_metadata`, `karac_runtime_list_par_blocks_into`, 4 `karac_runtime_http_*` getters/setters, 2 `karac_runtime_serve_http*`, 4 `karac_runtime_json_*`, `karac_vec_sort_by`.
  - 4 added 2026-05-17 (slice 1): `karac_runtime_event_loop_register_fd` (unix), `karac_runtime_event_loop_deregister_fd` (unix), `karac_runtime_event_loop_poll`, `karac_runtime_event_loop_wake` — phase 6 line 17 slice 1 (network event loop FFI surface).
  - 3 added 2026-05-17 (slice 3): `karac_runtime_event_loop_start_background_thread`, `karac_runtime_event_loop_take_wakeups`, `karac_runtime_event_loop_shutdown_background_thread` — phase 6 line 17 slice 3 (background poller thread + wakeup queue).
  - 2 added 2026-05-17 (slice 4): `karac_runtime_scheduler_start_dispatcher`, `karac_runtime_scheduler_shutdown_dispatcher` — phase 6 line 17 slice 4 (scheduler dispatcher thread that drives parked tasks on wakeup).
  - 1 added 2026-05-17 (slice 5): `karac_runtime_scheduler_stats_snapshot` — phase 6 line 17 slice 5 (atomic-counter snapshot for diagnostics + tests).
  - 2 added 2026-05-25 (slice 8): `karac_runtime_tcp_bind` (unix), `karac_runtime_tcp_accept` (unix) — phase 6 line 17 slice 8 (stdlib `TcpListener` syscall surface).
  - 2 added 2026-05-25 (slice 9): `karac_runtime_tcp_read` (unix), `karac_runtime_tcp_write` (unix) — phase 6 line 17 slice 9 (stdlib `TcpStream` syscall surface).
  - 1 added 2026-05-25 (slice 9d): `karac_runtime_tcp_close` (unix) — phase 6 line 17 slice 9d (close-on-drop for `TcpStream` / `TcpListener`; consumed by hand-rolled `@TcpStream.drop` / `@TcpListener.drop` codegen bodies).
  - 2 added 2026-05-25 (slice 9e.1): `karac_runtime_ws_send_text` (unix), `karac_runtime_ws_recv_text` (unix) — phase 6 line 17 slice 9e.1 (stdlib `WebSocket` framing protocol; text frames only, server-side convention).
  - 1 added 2026-05-25 (slice 9e.2): `karac_runtime_ws_accept` (unix) — phase 6 line 17 slice 9e.2 (stdlib `WebSocket.accept` RFC 6455 §4.2 HTTP upgrade handshake: accept + read HTTP request + SHA-1 + Base64 + write 101 response).
  - 2 added 2026-05-25 (slice 9e.3): `karac_runtime_ws_send_binary` (unix), `karac_runtime_ws_recv_binary` (unix) — phase 6 line 17 slice 9e.3 (binary frame surface + transparent control-frame handling in both `_recv_text` and `_recv_binary`).
  - 2 added 2026-05-26 (slice 9e.4): `karac_runtime_ws_send_text_masked` (unix), `karac_runtime_ws_send_binary_masked` (unix) — phase 6 line 17 slice 9e.4 (client-side masked send + fragmentation reassembly retrofitted into both `_recv_text` and `_recv_binary`).
  - 1 added 2026-06-06 (A2 slice 5b-1): `karac_runtime_taskgroup_cancel` — cooperative cancellation, language surface + flag plumbing (`TaskGroup.cancel()`). Inert until slice 5c routes the per-task flag into the dispatcher. (The 2026-05-26 audit total above predates the A2 spawn_coro / park_slot / taskgroup symbols and is a dated snapshot, not a live count.)
- **Total libc `extern "C"` imports:** 1 (`atexit`).
- **Total private `extern "C"` callbacks:** 1 (`print_trace_at_exit`, registered with `atexit`).
- **`#[used]` / `#[link_section(…)]` / `#[ctor]` / `#[dtor]`:** none.

Phase 1 (`strip -x` + `panic = "abort"`) preserves every global export
because `-x` strips local non-globals only; `#[no_mangle] extern "C"`
exports are external-linkage globals and survive by definition.

Phase 2 (`-Wl,-dead_strip` on Mach-O / `-Wl,--gc-sections` on ELF + thin
LTO across the workspace release profile) does **not** treat the
`#[no_mangle]` set as automatic keep-list roots — it computes reachability
from the entry point. Every symbol above is anchored through a direct call
from codegen-emitted IR when the program actually uses that subsystem:
e.g., `karac_runtime_serve_http` is unreached on a program that never
imports `std.http`, and the linker correctly drops the entire tokio +
hyper + h2 transitive subgraph. Programs that *do* import `std.http` pull
in only that arm. Validated on the LeetCode #1665 sort-heavy bench
(`kara-katas/leetcode/1601-1700/1665-.../bench/`) — binary dropped from
2.5 MB → 375 KB without correctness regression.
