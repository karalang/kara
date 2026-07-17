//! A2 slice 2b.3 — coroutine network-boundary E2E.
//!
//! Compiles a kara program in which a **free function** (`serve_one`) is a
//! network-boundary fn — it calls `listener.accept()`, a `sends(Network)
//! receives(Network)` park. With the coroutine path enabled
//! (`compile_to_object_with_coro` → `Codegen::set_coro_enabled`), `serve_one`
//! compiles as an LLVM coroutine: a ramp that registers the fd + `coro.suspend`s
//! (instead of the degenerate `emit_state_machine_poll_fn_for_key` body-splitter
//! that drops the post-park work — bug C), driven by the runtime dispatcher; the
//! caller (`main`) waits on a completion slot it passes in.
//!
//! What a green run proves (the 2b.3 acceptance gate, §6¾):
//!   1. **No hang.** `main` blocks on `park_slot_wait`; the dispatcher resumes
//!      the coroutine on fd-readiness and the body signals the slot at
//!      completion. A broken drive (caller-resume / dropped suspend) → timeout.
//!   2. **Bug C is fixed.** `serve_one` runs its post-park work (the `accept(2)`
//!      syscall on the resume edge, then `println(1)`). The degenerate path
//!      drops exactly this.
//!   3. **No UAF / clean frame lifetime.** The functional test asserts exit 0;
//!      the ASAN test (`coroutine_*_under_asan`) links `-fsanitize=address` and
//!      asserts a clean ASAN exit — the coro frame is `malloc`'d in the ramp and
//!      freed exactly once (by the dispatcher's shim `coro.destroy` on
//!      completion), never double-freed or used-after-free. (Drop *ordering*
//!      across suspends is slice 4; ASAN is necessary-but-not-sufficient there.)
//!
//! `main` is NOT a coroutine (it's the C-ABI entry, excluded from
//! `coro_fn_keys`) — its `bind` runs normally and the `BOUND_PORT=<n>` line the
//! runtime prints on an ephemeral `:0` bind drives the harness handshake, same
//! as `tests/tcp_listener.rs`.

#![cfg(feature = "llvm")]

mod common;

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::{Mutex, Once, OnceLock};
    use std::time::{Duration, Instant};

    /// The straight-line single-park handler exercised by both tests. `serve_one`
    /// is a network-boundary *free* fn (parks on `accept`); with the coroutine
    /// path on it compiles as a dispatcher-driven coroutine. `println(1)` proves
    /// the post-park resume edge ran (bug C); `println(2)` proves the drive
    /// returned to `main`.
    const HANDLER_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_one(listener);
            println(2);
        }
    "#;

    /// 2b.4 spawn variant: the coroutine handler is driven inside a *spawned*
    /// task (`spawn(|| ...)` → a worker-pool wrapper) rather than inline in
    /// `main`; `main` joins the handle. This services the connection
    /// functionally (the spawn wrapper runs 2b.3's coro-drive — ramp +
    /// `park_slot_wait` — on a pool worker, which the dispatcher unblocks). It
    /// is thread-blocking (one worker per concurrent handler); the
    /// density-optimal non-blocking spawn — wrapper ramps and returns, the
    /// TaskHandle's completion bound to the coroutine slot — is slice 5.
    const SPAWN_HANDLER_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let h: TaskHandle[i64] = spawn(|| { serve_one(listener); 0 });
            let _r: i64 = h.join();
            println(2);
        }
    "#;

    /// 2b.4(b) method-handler variant: the coroutine handler is an impl method
    /// (`Server.serve`), driven through the method-call intercept's coro
    /// ramp-drive (the receiver is the ramp's `self` arg). Same dispatcher-driven
    /// slot-wait drive as the free-fn path.
    const METHOD_HANDLER_SRC: &str = r#"
        struct Acceptor { listener: TcpListener }
        impl Acceptor {
            fn run(self) {
                let _stream = self.listener.accept().unwrap();
                println(1);
            }
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let a = Acceptor { listener: listener };
            a.run();
            println(2);
        }
    "#;

    /// Slice 3: a park inside a `while` LOOP. One *static* `coro.suspend` (the
    /// single `accept` call site) resumed N times across the loop back-edge —
    /// the canonical CoroSplit multi-resume case, and the real echo-handler
    /// shape. Exercises frame-residency of loop locals (`count`, `listener`)
    /// across a back-edge suspend, and the parked-record alloca being
    /// re-registered each iteration.
    const LOOP_HANDLER_SRC: &str = r#"
        fn serve_n(listener: TcpListener, n: i64) {
            let mut count: i64 = 0;
            while count < n {
                let _stream = listener.accept().unwrap();
                println(1);
                count = count + 1;
            }
            println(9);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_n(listener, 2);
            println(2);
        }
    "#;

    /// Slice 3: a park inside an `if` BRANCH. CoroSplit must thread the suspend
    /// through the conditional CFG and keep the post-`if` join (`println(9)`)
    /// reachable on the resume edge.
    const IF_HANDLER_SRC: &str = r#"
        fn serve_if(listener: TcpListener, doit: bool) {
            if doit {
                let _stream = listener.accept().unwrap();
                println(1);
            } else {
                println(0);
            }
            println(9);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_if(listener, true);
            println(2);
        }
    "#;

    /// Slice 5a — **density-optimal non-blocking spawn**. The closure body is
    /// a *tail* coroutine-handler call (`tg.spawn(|| serve_one(listener))`), so
    /// codegen routes it through `karac_runtime_spawn_coro`: the pool worker
    /// *ramps and returns* (no `park_slot_wait` blocking it), and the
    /// `TaskHandle`'s completion is bound to the coroutine's `KaracParkSlot`.
    /// The `TaskGroup`'s scope-exit drop joins the child — waiting on the
    /// coroutine slot, not a blocked worker. Contrast `SPAWN_HANDLER_SRC`,
    /// whose `{ serve_one(listener); 0 }` tail is `0` (not the coro call), so
    /// it stays on the 2b.4 thread-blocking path.
    const SPAWN_NONBLOCK_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_one(listener));
            println(2);
        }
    "#;

    /// Multi-capture non-blocking spawn — the Relay-bench regression. The
    /// closure captures BOTH the moved `listener` AND a heap-builtin `String`
    /// (`addr`), and the handler uses `addr` *after* the park (`accept`). The
    /// tail call is the coroutine handler, so this routes through
    /// `karac_runtime_spawn_coro` (the worker ramps + returns while the
    /// coroutine is still parked).
    ///
    /// The bug this pins (found dogfooding the Relay passthrough proxy, whose
    /// `handle(client, upstream_addr)` captured the moved `TcpStream` + a
    /// `String` upstream address): the spawn wrapper re-registered a
    /// `FreeVecBuffer` cleanup for the moved-in `String` capture and drained it
    /// on wrapper return — which, on the non-blocking coro path, happens while
    /// the coroutine is STILL PARKED. So the `String`'s buffer was freed out
    /// from under the live coroutine; when the coroutine resumed and read
    /// `addr`, it touched freed memory (the proxy returned an empty body; ASAN
    /// flags the use-after-free). A single `TcpStream`-only capture masked it
    /// (no heap buffer to free → `cap > 0` guard skipped). The fix makes the
    /// coroutine the sole owner of its moved-in heap-builtin captures (it frees
    /// them at its own completion); the wrapper no longer frees on the coro
    /// path. Fix: `src/codegen/task_group.rs` (`use_coro_spawn` ⇒ skip the
    /// wrapper `vec_caps` re-registration).
    ///
    /// `addr` is ≥36 bytes so the `String` is heap-backed, not SSO/inline —
    /// LeakSanitizer (and the macOS use-after-free detector) only see the bug on
    /// a real heap buffer (`reference_lsan_short_string_leaks`). The post-park
    /// `addr.len()` print (`38`) is the discriminating signal: it reads the
    /// captured `String` on the resume edge. Pre-fix that read is a UAF; post-fix
    /// it prints the correct length.
    const SPAWN_MULTI_CAPTURE_SRC: &str = r#"
        fn serve_one(listener: TcpListener, addr: String) {
            let _stream = listener.accept().unwrap();
            println(addr.len());
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let addr: String = "127.0.0.1:9000/upstream/backend/path/x";
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_one(listener, addr));
            println(2);
        }
    "#;

    /// B-2026-06-17-3 — **discarded FREE-spawn coroutine** (the `ws_echo_freespawn`
    /// shape). `spawn(|| serve_one(listener, tx))` is a free `spawn` (no
    /// `TaskGroup`) whose discarded `TaskHandle` codegen marks detached; its tail
    /// is a coroutine handler, so it lowers through `karac_runtime_spawn_coro`. A
    /// free-spawn coro handle has no joiner AND no group sweep, so it self-reaps
    /// via the slot: `karac_runtime_task_detach` arms the bound slot, and the
    /// coroutine's completion signal frees handle+slot.
    ///
    /// `main` does **not** join the discarded handle (that's the point), but it
    /// must stay alive long enough for the handler to service the connection,
    /// complete, and be reaped — otherwise process exit races the reap. A fixed
    /// `sleep_ms(2000)` is the barrier: the handler accepts + prints `7` within
    /// milliseconds of the client connecting, then completes (→ `coro_return` →
    /// slot signal → reap frees handle+slot), all comfortably before `main` wakes
    /// 2s later. That margin makes the run deterministic for **both** assertions:
    /// `7` appears in stdout (handler serviced the connection), and — because the
    /// reap has long finished by exit — Linux LeakSanitizer sees no leaked handle
    /// or slot (pre-fix, the free-spawn coro handle + park slot leak, so LSan
    /// fails). The barrier is `sleep`, not a channel, because moving a `Sender`
    /// into a free-spawn coroutine surfaced a separate drop-order problem (the
    /// channel closed before the send landed) unrelated to this reap — since fixed
    /// in `691117f6` (B-2026-06-17-9; coroutine owns its moved channel-end params),
    /// covered by `coroutine_free_spawn_channel_send_lands_before_close`. The sleep
    /// barrier stays here regardless: it keeps this reap test independent of the
    /// channel path and deterministic for the LSan timing window.
    const FREE_SPAWN_NONBLOCK_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            println(7);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            spawn(|| serve_one(listener));
            sleep_ms(2000);
        }
    "#;

    /// Slice 5c — **live mid-flight cancellation**. `serve_buf` is a non-blocking
    /// spawned coroutine handler that parks on `accept` of a listener **no client
    /// ever connects to** — so it stays parked on an idle fd that will never
    /// produce a readiness wakeup. `main` immediately `tg.cancel()`s, then the
    /// `TaskGroup`'s scope-exit drop joins the child. For the join to return
    /// (rather than hang forever), cancellation must reach the parked coroutine:
    /// `tg.cancel()` flips the child's flag + requests a dispatcher cancel-sweep,
    /// which finds the idle parked coroutine, drives `coro.destroy` (the per-park
    /// destroy edge: deregister fd + drop live heap locals + signal the slot), and
    /// wakes the joiner. A `Vec[i64]` (`buf`) is live across the park, so the
    /// destroy edge must free it exactly once — the ASAN gate. `buf.len()` after
    /// the park is never reached (the coroutine is torn down at the park), so the
    /// only stdout line is `main`'s `7`: its presence proves the join did NOT hang.
    /// The cancel may also race *ahead* of the handler reaching its park; the
    /// register-time race guard (`register_fd_cancel` requesting a sweep when the
    /// flag is already set) covers that ordering too, so the test is deterministic
    /// without any sleep.
    const CANCEL_HANDLER_SRC: &str = r#"
        fn serve_buf(listener: TcpListener) {
            let mut buf: Vec[i64] = Vec.new();
            buf.push(1);
            buf.push(2);
            buf.push(3);
            let _stream = listener.accept().unwrap();
            println(buf.len());
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_buf(listener));
            tg.cancel();
            println(7);
        }
    "#;

    /// Slice 5c-4 (defer-on-cancel): a spawned coroutine parked on an idle
    /// `accept` with a user `defer { println(9); }` registered *before* the park
    /// (so it is live across the suspend). `tg.cancel()` + the cancel-sweep tear
    /// the coroutine down at the park; the per-park destroy edge must now run the
    /// `defer` body (`9` on stdout) — proving cancel routes through the error-path
    /// cleanup drain, not just the heap-drop drain. The post-park body
    /// (`buf.len()` ⇒ `3`) must stay ABSENT (torn down at the park), and `7`
    /// proves main's scope-exit join woke. Discriminating signal: `defer` on a
    /// torn-down coroutine fires ONLY if the destroy edge drains user defers, so
    /// observing `9` is the load-bearing assertion for this slice.
    const CANCEL_DEFER_SRC: &str = r#"
        fn serve_buf(listener: TcpListener) {
            let mut buf: Vec[i64] = Vec.new();
            buf.push(1);
            buf.push(2);
            buf.push(3);
            defer { println(9); }
            let _stream = listener.accept().unwrap();
            println(buf.len());
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_buf(listener));
            tg.cancel();
            println(7);
        }
    "#;

    /// Owned-user-`Drop` coroutine-param leak regression (the `ws_idle_holder`
    /// connection-reap leak class, minimized). `serve_one` is coroutine-compiled
    /// (it parks on `accept`) and takes an **owned** param `c: Conn` whose
    /// `impl Drop` is observable (`println(7)`). The ownership model for a
    /// coroutine handler is: the *coroutine* owns its by-value params and runs
    /// their `Drop` at completion (and on the destroy/cancel edge); every caller
    /// suppresses its own drop. Without that, the caller-drops-by-value model
    /// breaks at a spawn boundary — the spawned wrapper ramps and returns (or
    /// the parent moved the value into the task), so *nobody* drops the param and
    /// the resource (an fd, for a `WebSocket`) leaks on every disconnect.
    ///
    /// Inline (synchronous) call shape — the IR test asserts `serve_one`'s
    /// coroutine clones drop `c` and `main` does NOT (caller suppression, else a
    /// double-drop).
    const OWNED_DROP_INLINE_SRC: &str = r#"
        struct Conn { id: i64 }
        impl Drop for Conn {
            fn drop(mut ref self) { println(self.id); }
        }
        fn serve_one(listener: TcpListener, c: Conn) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let c = Conn { id: 7 };
            serve_one(listener, c);
            println(2);
        }
    "#;

    /// Spawn shape of the owned-user-`Drop` coroutine-param leak — the real
    /// `tg.spawn(|| handle_connection(ws))` topology. `c: Conn` is moved into the
    /// non-blocking spawn (tail coroutine call) and must be dropped EXACTLY ONCE
    /// when the spawned coroutine completes: zero drops == the leak; two drops ==
    /// the double-free the naive "drop every owned struct param" fix caused. The
    /// `Drop` body prints `7`, so the functional test can count occurrences — an
    /// fd-close `Drop` (`TcpListener`/`WebSocket`) is invisible to LeakSanitizer
    /// (no heap) and to macOS ASAN (no LSan), which is why a sentinel print, not
    /// an ASAN green, is the load-bearing leak gate here.
    const OWNED_DROP_SPAWN_SRC: &str = r#"
        struct Conn { id: i64 }
        impl Drop for Conn {
            fn drop(mut ref self) { println(7); }
        }
        fn serve_one(listener: TcpListener, c: Conn) {
            let _stream = listener.accept().unwrap();
            println(1);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let c = Conn { id: 0 };
            let mut tg = TaskGroup.new();
            tg.spawn(|| serve_one(listener, c));
            println(2);
        }
    "#;

    /// Channel-`Sender` moved into a non-blocking (coroutine) free-spawn must be
    /// dropped by the COROUTINE at its completion — AFTER the handler's `send` —
    /// not by the spawn wrapper at ramp-return time. `serve_one` parks on
    /// `accept`, so the wrapper ramps and returns while the coroutine is still
    /// parked; the captured `tx` is moved into the coroutine frame as an owned
    /// param. Before the fix the wrapper's channel-end cleanup frame dropped
    /// `tx` immediately on ramp-return (`drain_top_frame_with_emit`), CLOSING the
    /// channel before the resumed coroutine ran `tx.send(7)` — so `rx.recv()`
    /// observed the closed-sentinel `0` instead of `7`. The fix makes the
    /// coroutine the owner of its channel-end params (drops at real completion,
    /// after the send) and suppresses the wrapper's early drop at the move site,
    /// mirroring the owned-user-`Drop` param ownership transfer. A green run
    /// prints `7` (the sent value), proving send-before-close ordering.
    const CHANNEL_SPAWN_SRC: &str = r#"
        fn serve_one(listener: TcpListener, tx: Sender[i64]) {
            let _stream = listener.accept().unwrap();
            tx.send(7);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            let (tx, rx): (Sender[i64], Receiver[i64]) = Channel.new();
            spawn(|| serve_one(listener, tx));
            let v: i64 = rx.recv();
            println(v);
        }
    "#;

    /// Coro-frame heap-overflow regression. A **fixed-size `Array[u8, 4096]`
    /// local live across a park** — the exact shape of the ws_idle_holder
    /// flagship handler's `recv_text` buffer. The array is touched at BOTH
    /// ends (`buf[0]`, `buf[4095]`) before the `accept` park, forcing
    /// full-extent frame residency across the suspend; after resume the whole
    /// 4096-byte extent is written then summed. Before the fix the coro
    /// frame's state struct sized this field at the 8-byte i64 default
    /// (`llvm_type_for_name("Array")` dropped the `[u8, 4096]` generic args),
    /// so the post-resume writes overflowed the frame slot into the adjacent
    /// heap chunk — `corrupted size vs. prev_size` / `double free or
    /// corruption` on glibc, ASAN heap-buffer-overflow on every OS. The sum
    /// of 4096 ones is `4096`, printed to prove the writes/reads landed in
    /// the buffer (not over a neighbour). Driven by the connect-only
    /// `service_n_connections` helper — the park is a plain-TCP `accept`, so
    /// no client payload is needed to make the buffer live across the suspend.
    const ARRAY_BUF_HANDLER_SRC: &str = r#"
        fn serve(listener: TcpListener) {
            let mut buf: Array[u8, 4096] = [0u8; 4096];
            buf[0] = 7u8;
            buf[4095] = 9u8;
            let _stream = listener.accept().unwrap();
            let mut i: i64 = 0;
            while i < 4096 {
                buf[i] = 1u8;
                i = i + 1;
            }
            let mut sum: i64 = 0;
            let mut j: i64 = 0;
            while j < 4096 {
                sum = sum + (buf[j] as i64);
                j = j + 1;
            }
            println(sum);
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve(listener);
            println(2);
        }
    "#;

    /// Slice 4: a **heap local live across a park**. `buf` (a `Vec[i64]`,
    /// stand-in for a read buffer) is allocated + filled BEFORE the `accept`
    /// park and used AFTER it (`buf.len()`), so CoroSplit must spill it into the
    /// coro frame — it is live across the suspend. On normal completion the
    /// body-end scope cleanup frees the buffer; on a destroy/cancel at the park
    /// the per-park destroy edge must free it too (else leak). This exercises
    /// the drop-across-suspend correctness slice 3 deliberately did not (slice 3
    /// handlers held only `i64` / fd locals).
    const HEAP_HANDLER_SRC: &str = r#"
        fn serve_buf(listener: TcpListener) {
            let mut buf: Vec[i64] = Vec.new();
            buf.push(1);
            buf.push(2);
            buf.push(3);
            let _stream = listener.accept().unwrap();
            println(buf.len());
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            serve_buf(listener);
            println(2);
        }
    "#;

    /// B-2026-06-19 — **non-unit coroutine return value carried across the
    /// inline ramp+wait boundary.** `probe` is a network-boundary coroutine
    /// (parks on `accept`) that returns `bool`; it is driven *inline* from its
    /// caller (`if not probe(...)`), which branches on the result — the exact
    /// shape `examples/relay/bench/kara/server.kara`'s `relay_response` has.
    /// Before the fix the inline-drive call discarded `probe`'s real return and
    /// yielded a hard-coded `i64 0` — wrong VALUE and wrong TYPE: branching on
    /// the `i64` failed LLVM verification (`Branch condition is not 'i1'
    /// type!`). These two sources differ ONLY in the literal `probe` returns, so
    /// they pin the VALUE (not just "it compiles"): a fix that still hard-coded
    /// 0 would make both print the same marker.
    ///
    /// `probe` returns **false** → `not probe(...)` is true → prints `7`.
    const CORO_BOOL_FALSE_SRC: &str = r#"
        fn probe(listener: TcpListener) -> bool {
            let _stream = listener.accept().unwrap();
            return false;
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            if not probe(listener) {
                println(7);
            } else {
                println(8);
            }
            println(9);
        }
    "#;

    /// Sibling of `CORO_BOOL_FALSE_SRC` — `probe` returns **true** → `not
    /// probe(...)` is false → prints `8`. The marker difference (7 vs 8) between
    /// the two programs is the value-correctness assertion: the real `bool`
    /// flows back from the inline-driven coroutine, not a hard-coded 0.
    const CORO_BOOL_TRUE_SRC: &str = r#"
        fn probe(listener: TcpListener) -> bool {
            let _stream = listener.accept().unwrap();
            return true;
        }
        fn main() {
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            if not probe(listener) {
                println(7);
            } else {
                println(8);
            }
            println(9);
        }
    "#;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    static RUNTIME_BUILT: Once = Once::new();
    static mut RUNTIME_PATH: Option<PathBuf> = None;

    #[allow(static_mut_refs)]
    fn runtime_path() -> Option<PathBuf> {
        RUNTIME_BUILT.call_once(|| {
            // Build with `test-helpers` (a superset — same as
            // `tests/park_and_wake.rs`) so concurrent runs don't strip the
            // test-helper FFIs out of the shared `libkarac_runtime.a` and break
            // sibling network tests; this E2E itself uses only always-on FFIs.
            let output = Command::new("cargo")
                .args([
                    "build",
                    "-p",
                    "karac-runtime",
                    "--release",
                    "--features",
                    "test-helpers",
                ])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    let p = workspace_root().join("target/release/libkarac_runtime.a");
                    if p.exists() {
                        unsafe {
                            RUNTIME_PATH = Some(p);
                        }
                    }
                }
            }
        });
        unsafe { RUNTIME_PATH.clone() }
    }

    /// Whether the host toolchain can produce + run an ASAN-linked executable.
    /// Mirrors `tests/memory_sanitizer.rs::asan_available`; skipping is preferred
    /// over failing so hosts without a sanitizer-capable `cc` stay green.
    fn asan_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| {
            if std::env::var("KARAC_SKIP_ASAN_TESTS").is_ok() {
                return false;
            }
            let probe_c = "/tmp/karac_coro_asan_probe.c";
            let probe_exe = "/tmp/karac_coro_asan_probe";
            if std::fs::write(probe_c, "int main(void){return 0;}\n").is_err() {
                return false;
            }
            let link_ok = Command::new("cc")
                .args(["-fsanitize=address", probe_c, "-o", probe_exe])
                .output()
                .ok()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let run_ok = link_ok
                && Command::new(probe_exe)
                    .output()
                    .ok()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            let _ = std::fs::remove_file(probe_c);
            let _ = std::fs::remove_file(probe_exe);
            run_ok
        })
    }

    /// Compile `src` with the coroutine path enabled and link it. `sanitizer`
    /// (e.g. `["-fsanitize=address"]`) routes through
    /// `link_executable_with_sanitizer`; `None` uses the plain linker.
    ///
    /// Unlike the `tcp_listener.rs` harness this populates the full
    /// network-boundary pipeline state (`callee_network_yield_effect` /
    /// `yield_points` / `state_struct_layouts` / `call_effect_subs` /
    /// `callee_purely_polymorphic_effects`) so `serve_one` lands in
    /// `state_struct_layouts` and is picked up by `coro_fn_keys`. Mirrors
    /// `tests/codegen.rs::ir_for_with_state_struct_layouts`, then routes through
    /// `compile_to_object_with_coro`.
    fn compile_link_coro(
        src: &str,
        exe_path: &Path,
        sanitizer: Option<&[&str]>,
    ) -> Result<(), String> {
        use karac::cli::{
            build_call_effect_subs_table, build_callee_network_yield_effect_table,
            build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
            build_yield_points_table,
        };
        use karac::codegen::{
            compile_to_object_with_coro, link_executable, link_executable_with_sanitizer,
        };

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            return Err(format!("parse errors: {:?}", parsed.errors));
        }
        let resolved = karac::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return Err(format!("resolve errors: {:?}", resolved.errors));
        }
        let typed = karac::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return Err(format!("typecheck errors: {:?}", typed.errors));
        }
        let method_types = typed.method_callee_types.clone();
        let call_type_subs = typed.call_type_subs.clone();
        let pattern_binding_types = typed.pattern_binding_types.clone();
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck_with_typecheck_data(
            &parsed.program,
            karac::effectchecker::PublicEffectsPolicy::default(),
            karac::manifest::CompileProfile::Default,
            method_types.clone(),
            call_type_subs,
        );
        parsed.program.callee_network_yield_effect =
            build_callee_network_yield_effect_table(&effects);
        parsed.program.yield_points = build_yield_points_table(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
        );
        parsed.program.state_struct_layouts = build_state_struct_layouts(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
            &pattern_binding_types,
        );
        parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
        parsed.program.callee_purely_polymorphic_effects =
            build_callee_purely_polymorphic_effects_set(&effects);

        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let obj = format!("/tmp/karac_coro_e2e_{pid}_{nanos}.o");
        compile_to_object_with_coro(&parsed.program, &obj, Some(&ownership), None)
            .map_err(|e| format!("codegen failed: {e}"))?;
        let link_res = match sanitizer {
            Some(flags) => link_executable_with_sanitizer(&obj, exe_path.to_str().unwrap(), flags),
            None => link_executable(&obj, exe_path.to_str().unwrap()),
        };
        link_res.map_err(|e| format!("link failed: {e}"))?;
        let _ = std::fs::remove_file(&obj);
        Ok(())
    }

    /// Drain the child's stdout in a thread: extract the `BOUND_PORT=<n>` port
    /// (via the channel) and collect every line for post-hoc marker assertions.
    #[allow(clippy::type_complexity)]
    fn spawn_stdout_reader(
        stdout: std::process::ChildStdout,
    ) -> (
        std::sync::mpsc::Receiver<u16>,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        let (tx, rx) = std::sync::mpsc::channel::<u16>();
        let handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut lines = Vec::new();
            let mut sent = false;
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if !sent {
                            if let Some(rest) = line.trim().strip_prefix("BOUND_PORT=") {
                                if let Ok(p) = rest.parse::<u16>() {
                                    let _ = tx.send(p);
                                    sent = true;
                                }
                            }
                        }
                        lines.push(line.trim_end().to_string());
                    }
                    Err(_) => break,
                }
            }
            lines
        });
        (rx, handle)
    }

    /// Spawn `exe_path`, await its `BOUND_PORT`, connect a client to trigger the
    /// coroutine's `accept`, then wait for the binary to exit. `asan_options`,
    /// when `Some`, is set as `ASAN_OPTIONS` on the child. Returns the exit
    /// status and the collected stdout lines. Panics on the failure modes that
    /// indicate a broken drive (no port / no connect / hang).
    fn service_one_connection(
        exe_path: &Path,
        asan_options: Option<&str>,
    ) -> (std::process::ExitStatus, Vec<String>) {
        let mut cmd = Command::new(exe_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(opts) = asan_options {
            cmd.env("ASAN_OPTIONS", opts);
        }
        let mut child = cmd.spawn().expect("failed to spawn coro e2e binary");

        let stdout = child.stdout.take().expect("child stdout missing");
        let (rx, join) = spawn_stdout_reader(stdout);

        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        // Connect to trigger the accept. Retry briefly to absorb the race
        // between bind's BOUND_PORT print and the coroutine's fd registration.
        let connect_started = Instant::now();
        let mut connected = false;
        for _ in 0..20 {
            if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                connected = true;
                break;
            }
            if connect_started.elapsed() > Duration::from_secs(3) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !connected {
            let _ = child.kill();
            let _ = child.wait();
            panic!("could not connect to 127.0.0.1:{port} to trigger accept");
        }

        // The binary must exit within a timeout: the dispatcher resumed the
        // coroutine, the accept completed on the resume edge, the body signalled
        // the slot, and `main`'s slot-wait returned. A hang means the drive is
        // broken (dropped suspend / unsignalled slot).
        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within 15s after connect — the \
                             coroutine drive did not complete (dropped suspend or \
                             unsignalled completion slot)."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };
        let lines = join.join().unwrap_or_default();
        (exit_status, lines)
    }

    /// Like `service_one_connection` but establishes `n` connections — for
    /// handlers whose loop services multiple connections (one static
    /// `coro.suspend` resumed across the loop back-edge). A small gap between
    /// connects lets the coroutine re-park on the next `accept`; the listener
    /// backlog absorbs any overlap. Returns the exit status + stdout lines.
    fn service_n_connections(
        exe_path: &Path,
        n: usize,
        asan_options: Option<&str>,
    ) -> (std::process::ExitStatus, Vec<String>) {
        let mut cmd = Command::new(exe_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(opts) = asan_options {
            cmd.env("ASAN_OPTIONS", opts);
        }
        let mut child = cmd.spawn().expect("failed to spawn coro e2e binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (rx, join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");
        for k in 0..n {
            let started = Instant::now();
            let mut connected = false;
            for _ in 0..40 {
                if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                    connected = true;
                    break;
                }
                if started.elapsed() > Duration::from_secs(3) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            if !connected {
                let _ = child.kill();
                let _ = child.wait();
                panic!("could not establish connection {k} of {n} to 127.0.0.1:{port}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within 15s after {n} connects — \
                             the loop coroutine drive did not complete."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };
        let lines = join.join().unwrap_or_default();
        (exit_status, lines)
    }

    /// Spawn `exe_path`, await its `BOUND_PORT` (proving the listener bound), then
    /// **never connect** — the spawned handler stays parked on an idle `accept`.
    /// The binary must still exit within the timeout: it does so only if
    /// `tg.cancel()` + the dispatcher cancel-sweep tear the parked coroutine down
    /// and wake the `TaskGroup` join. A hang ⇒ cancellation never reached the idle
    /// parked coroutine (the slice-5c failure mode). Returns the exit status +
    /// stdout lines. `asan_options`, when `Some`, is set as `ASAN_OPTIONS`.
    fn run_cancel_until_exit(
        exe_path: &Path,
        asan_options: Option<&str>,
    ) -> (std::process::ExitStatus, Vec<String>) {
        let mut cmd = Command::new(exe_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(opts) = asan_options {
            cmd.env("ASAN_OPTIONS", opts);
        }
        let mut child = cmd.spawn().expect("failed to spawn coro cancel e2e binary");
        let stdout = child.stdout.take().expect("child stdout missing");
        let (rx, join) = spawn_stdout_reader(stdout);
        // Confirm the listener bound (so the handler genuinely parks on an idle
        // accept), then deliberately do NOT connect.
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("binary did not emit BOUND_PORT line within 15s");
            }
        };
        assert!(port > 0, "BOUND_PORT must be a non-zero ephemeral port");

        let wait_started = Instant::now();
        let exit_status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if wait_started.elapsed() > Duration::from_secs(15) {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!(
                            "binary did not exit within 15s — TaskGroup.cancel() did \
                             not tear down the idle parked coroutine, so the group \
                             join hung (slice 5c cancel-sweep / per-task routing broken)."
                        );
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        };
        let lines = join.join().unwrap_or_default();
        (exit_status, lines)
    }

    #[test]
    fn coroutine_loop_handler_services_multiple_connections() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_loop_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(LOOP_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_n_connections(&exe_path, 2, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "loop coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // Two accept iterations each printed `1` (the resume edge ran per
        // iteration), the loop exited (`9`), and main resumed (`2`).
        let ones = lines.iter().filter(|l| *l == "1").count();
        assert!(
            ones == 2,
            "expected 2 per-iteration `1` markers (one per accept across the loop \
             back-edge), got {ones}; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "9") && lines.iter().any(|l| l == "2"),
            "loop did not exit + main did not resume; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_loop_handler_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_loop_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(LOOP_HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_n_connections(&exe_path, 2, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit across two loop iterations == the coroutine frame is
        // re-registered + resumed across the back-edge with no UAF/double-free,
        // and freed exactly once at completion.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the looping coroutine (exit \
             {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().filter(|l| *l == "1").count() == 2,
            "looping coroutine did not service both connections under ASAN; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_if_branch_handler_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_if_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(IF_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "if-branch coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The park inside the taken `if` branch ran (`1`), the post-`if` join
        // was reached on the resume edge (`9`), and main resumed (`2`).
        assert!(
            lines.iter().any(|l| l == "1")
                && lines.iter().any(|l| l == "9")
                && lines.iter().any(|l| l == "2"),
            "if-branch coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_fn_services_real_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!(
                "skip: libkarac_runtime.a not built \
                 (run `cargo build -p karac-runtime --release`)"
            );
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_e2e_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "binary exited non-success {exit_status:?} — coroutine frame lifetime \
             or drive bug. stdout lines: {lines:?}"
        );
        // Bug-C fix: the post-park work ran (resume edge), and the drive
        // returned to main.
        assert!(
            lines.iter().any(|l| l == "1"),
            "expected `1` (serve_one's post-accept println — the resume edge) in \
             stdout; got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "2"),
            "expected `2` (main's println after the coroutine drive returned) in \
             stdout; got {lines:?}"
        );
    }

    /// phase-8 line 153 **Phase 2** — the active span survives a *real*
    /// suspend/resume. `serve_one` parks directly on `accept`, so it compiles
    /// as a dispatcher-driven coroutine; `main` wraps the drive in
    /// `with_span(s, || ...)`, which (inlined at codegen) sets the per-thread
    /// active-span register to `7` on the thread that runs the ramp and takes
    /// the `coro.suspend`. The dispatcher resumes the coroutine on a *different*
    /// worker thread whose register is `0`, so the post-resume `Log.info` would
    /// be stamped `span_id=0` (rendered with no suffix) WITHOUT Phase 2. With
    /// the frame snapshot in `emit_coro_park_suspend` + the resume-edge restore,
    /// `7` is reinstalled before the post-park body runs, so the log line
    /// carries `span_id=7`. This is the literal "state-machine transform
    /// preserves the active span" gate. (The `with_span` closure here is purely
    /// an inlining vehicle — `build_state_struct_layouts` only keys top-level
    /// fns / impl methods, never closures, so `serve_one` is the sole
    /// coroutine; the `karac_tracing_*` accessors are unconditional runtime
    /// externs present in both the lean and full archives, so the lean-archive
    /// link this no-TLS program selects resolves them fine.)
    const TRACING_ACTIVE_SPAN_HANDLER_SRC: &str = r#"
        fn serve_one(listener: TcpListener) {
            let _stream = listener.accept().unwrap();
            Log.info("after-resume");
        }
        fn main() {
            let s = Span.root("req", 7);
            let listener = TcpListener.bind("127.0.0.1:0").unwrap();
            with_span(s, || { serve_one(listener); });
            println(2);
        }
    "#;

    #[test]
    fn coroutine_preserves_active_span_across_suspend() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_span_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(TRACING_ACTIVE_SPAN_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "active-span coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The post-resume log line carries the pre-suspend active span (7),
        // proving the frame save/restore reinstalled it on the resuming worker
        // thread. Without Phase 2 the line is `[info] after-resume` (span_id=0
        // → no suffix), so this assertion is what fails on a regression.
        assert!(
            lines.iter().any(|l| l == "[info] after-resume span_id=7"),
            "expected the post-resume log line stamped with the preserved active \
             span (`[info] after-resume span_id=7`); got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "2"),
            "expected `2` (main resumed after the coroutine drive returned); \
             got {lines:?}"
        );
    }

    #[test]
    fn coroutine_spawned_free_fn_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_spawn_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(SPAWN_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "spawned-coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // The spawned handler ran its post-park work (`1`) and `main` resumed
        // after `join()` (`2`).
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "spawned coroutine did not complete + join; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_nonblocking_spawn_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_nbspawn_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(SPAWN_NONBLOCK_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "non-blocking spawned-coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The handler ramped on a worker (which returned), the dispatcher drove
        // it to completion (`1`), and the TaskGroup drop joined it by waiting on
        // the coroutine slot before `main` returned. `2` is main's own line.
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "non-blocking spawned coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_nonblocking_spawn_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_nbspawn_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(SPAWN_NONBLOCK_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the non-blocking drive's new lifetimes are sound:
        // the runtime-owned `KaracParkSlot` is freed exactly once (in
        // `task_join` after the slot fires), the coro frame is freed once by the
        // dispatcher, and the handle/env are freed once — no UAF/double-free
        // across the worker→dispatcher→joiner handoff.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the non-blocking spawn path (exit \
             {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "non-blocking spawned coroutine did not complete under ASAN; \
             stdout lines: {lines:?}"
        );
    }

    /// Relay-bench regression (functional): a non-blocking spawned coroutine
    /// that captures BOTH the moved `listener` AND a heap `String`, and reads
    /// the `String` *after* the park. Pre-fix the wrapper freed the captured
    /// `String`'s buffer on ramp-return (while the coroutine was still parked),
    /// so the post-park `addr.len()` read hit freed memory; post-fix the
    /// coroutine owns the buffer and the read returns the correct length.
    #[test]
    fn coroutine_multi_capture_string_serviced() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_multicap_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(SPAWN_MULTI_CAPTURE_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "multi-capture spawned-coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // `38` is the captured `String`'s length, read on the post-park resume
        // edge — pre-fix this was a use-after-free on the wrapper-freed buffer.
        // `2` is main's own line.
        assert!(
            lines.iter().any(|l| l == "38") && lines.iter().any(|l| l == "2"),
            "multi-capture coroutine did not read its captured String post-park \
             (expected `38` + `2`); stdout lines: {lines:?}"
        );
    }

    /// Relay-bench regression (ASAN): same multi-capture spawn under
    /// `-fsanitize=address`. A clean exit proves the captured `String`'s buffer
    /// is freed exactly once (by the coroutine at its completion), with no
    /// use-after-free on the post-park read and no double-free. Pre-fix the
    /// wrapper's premature free was a use-after-free ASAN catches even on macOS
    /// (where LeakSanitizer is absent); the ≥36-byte payload keeps the buffer
    /// heap-backed so the detector sees it.
    #[test]
    fn coroutine_multi_capture_string_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_multicap_asan_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(
            SPAWN_MULTI_CAPTURE_SRC,
            &exe_path,
            Some(&["-fsanitize=address"]),
        ) {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the multi-capture coroutine spawn \
             path (exit {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "38") && lines.iter().any(|l| l == "2"),
            "multi-capture coroutine did not complete under ASAN (expected `38` + \
             `2`); stdout lines: {lines:?}"
        );
    }

    /// Slice 5c trigger (functional): a spawned coroutine parked on an idle
    /// `accept` is torn down by `tg.cancel()` + the dispatcher cancel-sweep, so
    /// the `TaskGroup` join returns instead of hanging. The runner never connects;
    /// `7` (main's line, printed after `tg.cancel()`, before the scope-exit join)
    /// reaching stdout proves the join woke. A broken per-task cancel route or a
    /// missing sweep ⇒ the join hangs ⇒ the runner times out.
    #[test]
    fn coroutine_taskgroup_cancel_tears_down_idle_parked_handler() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_cancel_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(CANCEL_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = run_cancel_until_exit(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "cancel binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // `7` proves main's scope-exit TaskGroup join returned (cancellation woke
        // it); the post-park `buf.len()` line must be ABSENT (the coroutine was
        // torn down at the park, never running its post-park body).
        assert!(
            lines.iter().any(|l| l == "7"),
            "main did not reach its post-cancel line — the group join hung; \
             stdout lines: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "3"),
            "the cancelled coroutine ran its post-park body (buf.len()==3) — it \
             should have been torn down at the park; stdout lines: {lines:?}"
        );
    }

    /// Slice 5c trigger (ASAN): same idle-park cancellation, with a `Vec[i64]`
    /// live across the park. The per-park destroy edge must free it **exactly
    /// once** — a leak (Linux `detect_leaks=1`) means the destroy edge skipped the
    /// drop; a double-free (sweep + a stray normal wakeup both invoking `poll_fn`)
    /// means the one-shot `take_registration` claim failed. Either trips ASAN.
    #[test]
    fn coroutine_taskgroup_cancel_idle_parked_handler_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_cancel_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(CANCEL_HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = run_cancel_until_exit(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the heap local live across the park was freed exactly
        // once on the destroy edge (no leak, no double-free), and the
        // sweep→destroy→slot-signal→join handoff is UAF-free.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the mid-flight-cancel path (exit \
             {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "7"),
            "main did not reach its post-cancel line under ASAN — the group join \
             hung; stdout lines: {lines:?}"
        );
    }

    /// Slice 5c-4: user `defer` fires when a parked coroutine is cancelled. The
    /// destroy edge routes through the error-path cleanup drain, so a `defer`
    /// live across the park runs on teardown. `9` present ⇒ defer fired; `3`
    /// absent ⇒ the post-park body did not run (torn down at the park); `7`
    /// present ⇒ main's join woke. A destroy edge that drained only heap drops
    /// (pre-5c-4 behaviour) would NOT print `9`.
    #[test]
    fn coroutine_taskgroup_cancel_runs_user_defer() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_cancel_defer_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(CANCEL_DEFER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = run_cancel_until_exit(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "cancel binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "9"),
            "the cancelled coroutine did not run its `defer` body (expected `9`) — \
             the destroy edge skipped user defers; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "7"),
            "main did not reach its post-cancel line — the group join hung; \
             stdout lines: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "3"),
            "the cancelled coroutine ran its post-park body (buf.len()==3) — it \
             should have been torn down at the park; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_drops_owned_user_drop_param() {
        // I1 + I2 at the IR level. A coroutine-compiled fn that takes an owned
        // user-`Drop` param (`c: Conn`) must run that param's `Drop` on its own
        // completion + destroy edges (the coroutine owns by-value params across
        // the spawn boundary, where the caller cannot), and the caller must NOT
        // also drop it (else double-free). `karac_drop_<Type>` is the user-drop
        // wrapper emitted by `emit_user_drop_wrapper`.
        let ir = compile_coro_split_ir(OWNED_DROP_INLINE_SRC).expect("coro-split IR");

        // (I1) The destroy/cancel edge of the coroutine drops the owned param —
        // a coroutine cancelled while parked must still free the resource it owns.
        let destroy = extract_fn_ir(&ir, "@serve_one.destroy(")
            .unwrap_or_else(|| panic!("no serve_one.destroy clone in IR:\n{ir}"));
        assert!(
            destroy.contains("@karac_drop_Conn"),
            "coroutine destroy edge must drop its owned user-Drop param `c`; \
             serve_one.destroy:\n{destroy}"
        );

        // (I1) The normal-completion path (the resume clone runs the body to its
        // end, where `emit_scope_cleanup` drains the param) also drops it.
        let resume = extract_fn_ir(&ir, "@serve_one.resume(")
            .unwrap_or_else(|| panic!("no serve_one.resume clone in IR:\n{ir}"));
        assert!(
            resume.contains("@karac_drop_Conn"),
            "coroutine completion path must drop its owned user-Drop param `c`; \
             serve_one.resume:\n{resume}"
        );

        // (I2) `main` calls `serve_one` synchronously but must NOT drop `c` — the
        // coroutine now owns it. A `@karac_drop_Conn` in `@main` would be the
        // double-free the general-param-loop attempt caused (proven this session).
        let main_ir =
            extract_fn_ir(&ir, "@main(").unwrap_or_else(|| panic!("no @main in IR:\n{ir}"));
        assert!(
            !main_ir.contains("@karac_drop_Conn"),
            "caller `main` must suppress its drop of the owned arg passed to a \
             coroutine (the coroutine owns + drops it); @main:\n{main_ir}"
        );
    }

    #[test]
    fn coroutine_spawn_drops_owned_user_drop_param_exactly_once() {
        // The end-to-end leak gate for the `ws_idle_holder` reap leak. A
        // `Conn` moved into a non-blocking spawn (`tg.spawn(|| serve_one(.., c))`)
        // must have its `Drop` run exactly once when the spawned coroutine
        // completes. The `Drop` prints `7`; we count it. Zero `7`s == the fd leak
        // (the bug); two `7`s == the double-free regression.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_owned_drop_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(OWNED_DROP_SPAWN_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "owned-drop spawn binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // Handler ran (`1`) and main resumed (`2`).
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "owned-drop spawn coroutine did not complete; stdout lines: {lines:?}"
        );
        // The owned `Conn` param's `Drop` ran EXACTLY ONCE.
        let drop_count = lines.iter().filter(|l| *l == "7").count();
        assert_eq!(
            drop_count, 1,
            "owned user-Drop coroutine param must drop exactly once \
             (0 == reap leak, 2 == double-free); saw {drop_count}; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_spawn_channel_send_lands_before_close() {
        // Send-before-close ordering for a `Sender` moved into a non-blocking
        // free-spawn coroutine. The handler `tx.send(7)`s AFTER its `accept`
        // park; the wrapper ramps and returns while the coroutine is parked, so
        // the wrapper must NOT drop (close) `tx` on ramp-return — the coroutine
        // owns it and closes it only after the send. A broken ownership transfer
        // closes the channel early and `rx.recv()` returns the sentinel `0`.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_chan_spawn_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(CHANNEL_SPAWN_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "channel-spawn binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // `rx.recv()` must observe the sent value `7`, never the closed-sentinel
        // `0` (the bug: wrapper closed the channel before the coroutine's send).
        assert!(
            lines.iter().any(|l| l == "7"),
            "receiver must observe the sent value 7, not the closed-channel \
             sentinel 0 (channel closed before the coroutine's send landed); \
             stdout lines: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "0"),
            "receiver observed the closed-channel sentinel 0 — the captured \
             Sender was dropped (closing the channel) before the coroutine's \
             send landed; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_spawn_channel_single_close_under_asan() {
        // ASAN/LSan companion to `..._send_lands_before_close`. The functional
        // test asserts the *value* (`7`, not `0`) but runs WITHOUT a sanitizer,
        // so a latent double-close/double-free that still happens to print `7`
        // would slip past it. The send-before-close fix has two halves that must
        // agree: the coroutine now drops (closes) the captured `Sender` at its
        // completion, AND the spawn wrapper suppresses its own `DropChannelEnd`
        // at the move site. If the alloca-keyed suppression ever fails to match
        // the wrapper's registration, BOTH drop the same channel end — a
        // double-`drop_sender` (double-free of the `KaracChannel` once the
        // refcount underflows). That edge is coro-specific (the non-coro
        // `asan_channel_*` cases never ramp-and-resume across a suspend), so it
        // needs its own sanitized run. A clean ASAN exit proves the single-close
        // invariant on the actual coroutine path; on Linux LSan the same run
        // proves the close isn't *under*-counted either (no channel leak).
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_chan_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(CHANNEL_SPAWN_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the captured `Sender` is dropped (channel closed)
        // exactly once — by the coroutine at completion, not also by the wrapper.
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the coro channel-move path (exit \
             {exit_status:?}) — likely a double-close of the captured Sender; \
             stdout lines: {lines:?}"
        );
        // And the value still lands (the send ran before the close).
        assert!(
            lines.iter().any(|l| l == "7") && !lines.iter().any(|l| l == "0"),
            "coro channel-move did not deliver the sent value under ASAN; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_method_handler_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_method_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(METHOD_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "method-handler coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "method-handler coroutine did not complete; stdout lines: {lines:?}"
        );
    }

    /// Compile `src` with the coroutine path on and run the coro lowering
    /// passes, returning post-CoroSplit IR (the `.resume`/`.destroy` clones).
    /// Mirrors `compile_link_coro`'s full network-boundary pipeline population
    /// but routes to `compile_to_ir_with_coro_split` instead of object emission.
    fn compile_coro_split_ir(src: &str) -> Result<String, String> {
        use karac::cli::{
            build_call_effect_subs_table, build_callee_network_yield_effect_table,
            build_callee_purely_polymorphic_effects_set, build_state_struct_layouts,
            build_yield_points_table,
        };
        use karac::codegen::compile_to_ir_with_coro_split;

        let mut parsed = karac::parse(src);
        if !parsed.errors.is_empty() {
            return Err(format!("parse errors: {:?}", parsed.errors));
        }
        let resolved = karac::resolve(&parsed.program);
        if !resolved.errors.is_empty() {
            return Err(format!("resolve errors: {:?}", resolved.errors));
        }
        let typed = karac::typecheck(&parsed.program, &resolved);
        if !typed.errors.is_empty() {
            return Err(format!("typecheck errors: {:?}", typed.errors));
        }
        let method_types = typed.method_callee_types.clone();
        let call_type_subs = typed.call_type_subs.clone();
        let pattern_binding_types = typed.pattern_binding_types.clone();
        karac::lower(&mut parsed.program, &typed);
        let effects = karac::effectcheck_with_typecheck_data(
            &parsed.program,
            karac::effectchecker::PublicEffectsPolicy::default(),
            karac::manifest::CompileProfile::Default,
            method_types.clone(),
            call_type_subs,
        );
        parsed.program.callee_network_yield_effect =
            build_callee_network_yield_effect_table(&effects);
        parsed.program.yield_points = build_yield_points_table(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
        );
        parsed.program.state_struct_layouts = build_state_struct_layouts(
            &parsed.program,
            &parsed.program.callee_network_yield_effect,
            &method_types,
            &pattern_binding_types,
        );
        parsed.program.call_effect_subs = build_call_effect_subs_table(&effects);
        parsed.program.callee_purely_polymorphic_effects =
            build_callee_purely_polymorphic_effects_set(&effects);
        let ownership = karac::ownershipcheck(&parsed.program, &typed);
        super::common::assert_ownership_clean(&ownership, src);

        // Load concurrency analysis — the CLI `karac build` path passes it, and
        // it is what surfaced the coro+auto-par interaction (a coroutine body
        // that auto-parallelizes would emit `coro.suspend` into a `__par_branch`
        // worker fn referencing the outer ramp's frame `%hdl` — invalid IR). The
        // coro_e2e harness historically passed `None`, so it could not catch
        // this; thread it through to match the real build.
        let concurrency = karac::concurrency_analyze(&parsed.program, &effects);
        compile_to_ir_with_coro_split(&parsed.program, None, Some(&concurrency))
    }

    /// Extract one `define ... @<sig_marker>...{ ... }` function body from module
    /// IR text. `sig_marker` is the `@name(`-style signature fragment (e.g.
    /// `@serve_buf.destroy(`); the slice runs from the preceding `define` to the
    /// function's closing `\n}`.
    fn extract_fn_ir<'a>(ir: &'a str, sig_marker: &str) -> Option<&'a str> {
        let at = ir.find(sig_marker)?;
        let start = ir[..at].rfind("\ndefine").map(|p| p + 1).unwrap_or(0);
        let end = ir[at..].find("\n}").map(|p| at + p + 2).unwrap_or(ir.len());
        Some(&ir[start..end])
    }

    /// Extract one `<label>:` basic block (up to the next blank-line-separated
    /// label) from a function-body IR slice.
    fn extract_block<'a>(fn_ir: &'a str, label: &str) -> Option<&'a str> {
        let marker = format!("\n{label}:");
        let start = fn_ir.find(&marker)? + 1;
        let end = fn_ir[start..]
            .find("\n\n")
            .map(|p| start + p)
            .unwrap_or(fn_ir.len());
        Some(&fn_ir[start..end])
    }

    /// Slice 4 — **destroy/cancel-edge heap drop is emitted**. A coroutine that
    /// holds a heap local (`Vec[i64] buf`) across the `accept` park must, on the
    /// per-park destroy edge (the path a future slice-5 cancel triggers), (a)
    /// deregister the fd before the frame is freed — else the dispatcher dangles
    /// a pointer into the freed frame — and (b) free the heap buffer — else a
    /// mid-flight cancel leaks it. This inspects the CoroSplit `.destroy` clone
    /// directly: today the destroy edge is emitted-but-unreached (the only
    /// runtime `coro.destroy` lands on the final suspend's free-only edge), so a
    /// structural assertion is the right gate until slice 5 wires a live cancel.
    /// The non-heap contrast handler proves the buffer free is tied to the live
    /// heap local, not emitted unconditionally.
    /// Regression: a coroutine-compiled function body must NOT be
    /// auto-parallelized. Surfaced flipping coroutines on by default for
    /// `karac build` (which loads concurrency analysis): a coroutine whose body
    /// is several independent statements would have its group lifted into a
    /// `__par_branch_*` worker fn, emitting the `coro.suspend` + frame-`%hdl`
    /// references there while `coro.begin` stayed in the outer ramp — "basic
    /// block in another function" / "does not dominate", failing module
    /// verification. Fix: `compile_function_body` falls back to sequential when
    /// `coro_ctx` is set. `compile_coro_split_ir` now loads concurrency analysis,
    /// so a regressed fix would fail this compile (verify) outright.
    #[test]
    fn coroutine_body_not_auto_parallelized() {
        const SRC: &str = r#"
            fn serve(l: TcpListener) {
                let s = l.accept().unwrap();
                println(1);
                println(2);
            }
            fn main() {
                let l = TcpListener.bind("127.0.0.1:0").unwrap();
                serve(l);
            }
        "#;
        let ir = match compile_coro_split_ir(SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro body with auto-par-able statements failed to compile: {e}"),
        };
        // serve is a coroutine (parks) ...
        assert!(
            ir.contains("@serve.destroy(") || ir.contains("@serve.resume("),
            "serve must be coroutine-compiled (CoroSplit clones present); IR:\n{ir}"
        );
        // ... and its body was NOT sharded onto par workers.
        assert!(
            !ir.contains("@__par_branch"),
            "a coroutine body must not be auto-parallelized into par-branch \
             worker fns; IR:\n{ir}"
        );
    }

    #[test]
    fn nonblocking_spawn_uses_spawn_coro_ffi() {
        let ir = match compile_coro_split_ir(SPAWN_NONBLOCK_SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro-split IR for non-blocking spawn failed: {e}"),
        };
        // The tail-coroutine `tg.spawn(|| serve_one(listener))` must lower to
        // the non-blocking primitive, not the thread-blocking `karac_runtime_spawn`.
        assert!(
            ir.contains("call ptr @karac_runtime_spawn_coro("),
            "non-blocking spawn must call karac_runtime_spawn_coro; IR:\n{ir}"
        );
        assert!(
            !ir.contains("call ptr @karac_runtime_spawn("),
            "tail-coroutine spawn must NOT call the blocking karac_runtime_spawn; IR:\n{ir}"
        );
        // The wrapper must ramp WITHOUT a nested `park_slot_wait` — that is the
        // worker-freeing density property. It hands the runtime-owned slot to
        // the coroutine ramp and returns; the blocking 2b.4 wrapper would call
        // `park_slot_new` + `park_slot_wait` here.
        let wrap = extract_fn_ir(&ir, "@__spawn_wrap_0(")
            .unwrap_or_else(|| panic!("no spawn wrapper in IR:\n{ir}"));
        assert!(
            !wrap.contains("@karac_runtime_park_slot_wait"),
            "the non-blocking spawn wrapper must NOT block on park_slot_wait \
             (the density property); wrapper:\n{wrap}"
        );
        assert!(
            !wrap.contains("@karac_runtime_park_slot_new"),
            "the non-blocking spawn wrapper must use the runtime-owned slot, \
             not allocate its own; wrapper:\n{wrap}"
        );
        // It does invoke the coroutine ramp (the handler runs).
        assert!(
            wrap.contains("@serve_one"),
            "the wrapper must call the coroutine ramp `serve_one`; wrapper:\n{wrap}"
        );
    }

    /// Slice 5c — **cooperative cancellation mechanism is emitted**. The resume
    /// shim (`__kara_coro_resume`) checks the cancel flag the dispatcher passes
    /// and, when set, `coro.destroy`s instead of resuming; the coroutine's
    /// per-park destroy edge then `park_slot_signal`s the completion slot so a
    /// cancelled coroutine's waiter wakes instead of hanging. Both are inert
    /// until a cancel flag is actually set (the dispatcher passes a
    /// never-cancelled flag today), so this is verified structurally. The
    /// trigger half — per-task cancel routing + `TaskGroup.cancel()` + the
    /// dispatcher sweep for parked-but-never-ready coroutines — is the follow-on.
    #[test]
    fn coroutine_cancel_mechanism_emitted() {
        let ir = compile_coro_split_ir(HANDLER_SRC).expect("coro-split IR");

        // The resume shim checks the cancel flag (a load + a teardown branch)
        // before resuming.
        let shim = extract_fn_ir(&ir, "@__kara_coro_resume(")
            .unwrap_or_else(|| panic!("no resume shim in IR:\n{ir}"));
        assert!(
            shim.contains("cancel.flag") && shim.contains("cancel.teardown"),
            "resume shim must load the cancel flag and branch to a teardown path; \
             shim:\n{shim}"
        );

        // The coroutine's `.destroy` clone signals the completion slot on its
        // per-park destroy edge — without this, cancelling a parked coroutine
        // hangs its waiter forever.
        let destroy = extract_fn_ir(&ir, "@serve_one.destroy(")
            .unwrap_or_else(|| panic!("no serve_one.destroy clone:\n{ir}"));
        let dblock = extract_block(destroy, "kara.coro.destroy.0")
            .unwrap_or_else(|| panic!("no kara.coro.destroy.0 block:\n{destroy}"));
        assert!(
            dblock.contains("karac_runtime_park_slot_signal"),
            "the per-park destroy edge must signal the completion slot so a \
             cancelled coroutine's waiter wakes; destroy block:\n{dblock}"
        );
    }

    #[test]
    fn coroutine_heap_local_freed_on_destroy_edge() {
        let ir = match compile_coro_split_ir(HEAP_HANDLER_SRC) {
            Ok(ir) => ir,
            Err(e) => panic!("coro-split IR for heap handler failed: {e}"),
        };

        let destroy = extract_fn_ir(&ir, "@serve_buf.destroy(")
            .unwrap_or_else(|| panic!("no serve_buf.destroy clone in IR:\n{ir}"));

        // The per-park destroy edge exists and, in its own block, tears down the
        // fd registration (the dispatcher's reference into the frame).
        let destroy_block = extract_block(destroy, "kara.coro.destroy.0").unwrap_or_else(|| {
            panic!("no kara.coro.destroy.0 block in serve_buf.destroy:\n{destroy}")
        });
        assert!(
            destroy_block.contains("karac_runtime_event_loop_deregister_fd"),
            "destroy edge must deregister the fd before the frame is freed; \
             kara.coro.destroy.0 block:\n{destroy_block}"
        );

        // The live heap buffer is freed on the destroy clone (FreeVecBuffer:
        // `is_heap` cap-guard → `cleanup.free` → `free(%cleanup.data)`), ahead of
        // the shared frame free.
        assert!(
            destroy.contains("cleanup.free") && destroy.contains("cleanup.data"),
            "destroy clone must free the Vec buffer live across the park; \
             serve_buf.destroy:\n{destroy}"
        );
        assert!(
            destroy.contains("call void @karac_free_buf(ptr %cleanup.data"),
            "destroy clone must release the Vec buffer pointer (recycling-aware \
             karac_free_buf since the large-buffer cache); \
             serve_buf.destroy:\n{destroy}"
        );
        // …and the frame itself is freed exactly once, after the heap drops.
        assert!(
            destroy.contains("call void @free(ptr %hdl)"),
            "destroy clone must free the coro frame; serve_buf.destroy:\n{destroy}"
        );

        // Contrast: a handler with NO heap local across the park has a destroy
        // edge that deregisters + frees the frame but emits no buffer free —
        // the drop is wired to the live heap local, not unconditional.
        let ir_no_heap = compile_coro_split_ir(HANDLER_SRC).expect("coro-split IR for non-heap");
        let destroy_no_heap = extract_fn_ir(&ir_no_heap, "@serve_one.destroy(")
            .unwrap_or_else(|| panic!("no serve_one.destroy clone:\n{ir_no_heap}"));
        assert!(
            !destroy_no_heap.contains("cleanup.free"),
            "non-heap handler's destroy edge must not emit a buffer free; \
             serve_one.destroy:\n{destroy_no_heap}"
        );
    }

    #[test]
    fn coroutine_heap_local_across_park_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_heap_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HEAP_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "heap-local coroutine binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        // The post-park resume edge ran and read the frame-resident buffer
        // (`buf.len()` == 3), then main resumed (`2`).
        assert!(
            lines.iter().any(|l| l == "3") && lines.iter().any(|l| l == "2"),
            "heap-local handler did not print buf.len()==3 then main's 2; \
             stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_heap_local_across_park_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_heap_asan_{pid}_{nanos}"));

        if let Err(e) =
            compile_link_coro(HEAP_HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"]))
        {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        // On Linux, `detect_leaks=1` makes this the load-bearing leak gate: the
        // frame-resident `Vec` buffer is freed exactly once on the completion
        // path (body-end scope cleanup), with no leak and no UAF/double-free
        // against the coro frame. macOS Apple-clang ASAN lacks LeakSanitizer, so
        // there it covers UAF/double-free only.
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "ASAN reported a memory error for the heap-local-across-park coroutine \
             (exit {exit_status:?}); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "3") && lines.iter().any(|l| l == "2"),
            "heap-local handler did not complete under ASAN; stdout lines: {lines:?}"
        );
    }

    #[test]
    fn coroutine_free_fn_services_connection_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_asan_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(HANDLER_SRC, &exe_path, Some(&["-fsanitize=address"])) {
            // Runtime archive not ASAN-linkable on this host — skip, don't fail.
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }

        // macOS Apple-clang ASAN lacks LeakSanitizer (`detect_leaks` unsupported)
        // — keep UAF / double-free / heap-overflow coverage there; enable the
        // leak arm on Linux. `exitcode=23` makes an ASAN error a non-success
        // exit. Same posture as `tests/memory_sanitizer.rs`.
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the coroutine frame lifetime is sound: the frame is
        // freed exactly once (dispatcher shim `coro.destroy` on completion), the
        // caller-owned slot is freed exactly once, no UAF on the resume/destroy
        // edges. An ASAN error would exit 23 (or signal) → !success().
        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the coroutine path (exit {exit_status:?}); \
             stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "1") && lines.iter().any(|l| l == "2"),
            "coroutine did not complete under ASAN; stdout lines: {lines:?}"
        );
    }

    /// B-2026-06-17-3 — a **discarded free-spawn coroutine** must self-reap its
    /// handle + park slot via the slot-signal reap path, with no UAF / double-free
    /// when the completion signal frees them. Drives `FREE_SPAWN_NONBLOCK_SRC`
    /// (free `spawn(|| serve_one(listener, tx))`, coro-lowered, handle discarded)
    /// over a real connection under ASAN.
    ///
    /// **Leak detection is OFF here, by design.** A free spawn is fire-and-forget:
    /// the reap runs on the dispatcher at the coroutine's completion, *after* the
    /// handler's `tx.send` unblocks `main` — so `main` can reach process exit
    /// before the dispatcher finishes the reap, which LeakSanitizer at exit would
    /// flag as a (false) leak even post-fix. There is no user-observable event
    /// that happens-after the reap to gate exit on, so the leak arm is inherently
    /// racy for this shape (same reason the free-spawn cases carry no at-exit LSan
    /// test). The deterministic no-leak / exactly-once-free guarantee is pinned by
    /// the runtime unit tests `detached_free_spawn_coro_reaps_*`; this E2E's job is
    /// the UAF / double-free coverage of the reap firing under a *real* coroutine.
    #[test]
    fn coroutine_discarded_free_spawn_self_reaps_under_asan() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_freespawn_asan_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(
            FREE_SPAWN_NONBLOCK_SRC,
            &exe_path,
            Some(&["-fsanitize=address"]),
        ) {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }

        // The 2s `sleep_ms` barrier makes the reap finish well before exit, so the
        // leak arm IS deterministic here (unlike the channel-barrier shape): on
        // Linux enable LeakSanitizer — pre-fix the free-spawn coro handle + park
        // slot leak (LSan fails); post-fix they're reaped (clean). macOS Apple-clang
        // ASAN has no LSan, so it keeps only UAF / double-free / heap-overflow
        // coverage. `exitcode=23` turns any ASAN error into a non-success exit.
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_one_connection(&exe_path, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        // Clean ASAN exit == the slot-signal reap freed handle+slot soundly: no
        // UAF on the freed slot, no double-free against any other freer, and (on
        // Linux) no leak. An ASAN/LSan error would exit 23 (or signal) → !success().
        assert!(
            exit_status.success(),
            "ASAN/LSan reported a memory error in the free-spawn coro reap path \
             (exit {exit_status:?}); stdout lines: {lines:?}"
        );
        // `7` is the handler's print after it serviced the connection — proof the
        // free-spawn coroutine ran to completion (the path that fires the reap).
        // The 2s barrier guarantees it is flushed long before exit.
        assert!(
            lines.iter().any(|l| l == "7"),
            "free-spawn coroutine did not service the connection under ASAN \
             (expected `7`); stdout lines: {lines:?}"
        );
    }

    // ── A2 WS-over-TLS coroutine E2E ────────────────────────────────────
    //
    // The coro gate above covers only plain TCP. This pins the FLAGSHIP
    // shape — a TLS WebSocket handler (`ws.recv_text` / `ws.send_text`,
    // lowered via `lower_websocket_io`) spawned per-connection via
    // `tg.spawn` — actually EXECUTING its recv/send body as a coroutine,
    // not merely establishing the connection. A real `wss://` client
    // (rustls + a hand-rolled WS frame) handshakes, sends a text frame, and
    // asserts the echo round-trips through `recv_text` + `send_text`. That
    // only happens if `lower_websocket_io`'s `karac_park_on_fd` lowers to a
    // coroutine suspend/resume (register fd → `coro.suspend` → the
    // `ws_recv_text`/`ws_send_text` syscall on the resume edge) — the exact
    // wiring this gate guards.

    /// rustls `ServerCertVerifier` that skips chain validation — the
    /// checked-in fixture cert is a CA cert that webpki rejects as an
    /// end-entity. We verify karac's WS-over-TLS coroutine wiring, not
    /// rustls validation. (Same posture as `tests/http_server.rs`.)
    #[derive(Debug)]
    struct NoVerify;
    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _e: &rustls::pki_types::CertificateDer<'_>,
            _i: &[rustls::pki_types::CertificateDer<'_>],
            _n: &rustls::pki_types::ServerName<'_>,
            _o: &[u8],
            _t: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _m: &[u8],
            _c: &rustls::pki_types::CertificateDer<'_>,
            _d: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::ED25519,
            ]
        }
    }

    /// Open a `wss://` connection to `127.0.0.1:<port>` (rustls, NoVerify),
    /// upgrade to WebSocket, send `payload` as one masked TEXT frame, and
    /// return the server's echoed frame payload. The round-trip succeeds
    /// only if the handler's `recv_text` + `send_text` executed.
    fn wss_echo_roundtrip(port: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        use std::io::{Read, Write};
        use std::sync::Arc;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("client config: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| format!("server name: {e}"))?;
        let mut conn = rustls::ClientConnection::new(Arc::new(config), server_name)
            .map_err(|e| format!("client conn: {e}"))?;

        // Retry the TCP connect briefly to absorb the race between the
        // server's BOUND_PORT print and its accept-fd registration.
        let mut sock = {
            let started = Instant::now();
            loop {
                match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                    Ok(s) => break s,
                    Err(_) if started.elapsed() < Duration::from_secs(3) => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => return Err(format!("tcp connect: {e}")),
                }
            }
        };
        sock.set_read_timeout(Some(Duration::from_secs(8))).ok();
        sock.set_write_timeout(Some(Duration::from_secs(8))).ok();
        let mut tls = rustls::Stream::new(&mut conn, &mut sock);

        // WS upgrade handshake.
        let req = "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                   Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                   Sec-WebSocket-Version: 13\r\n\r\n";
        tls.write_all(req.as_bytes())
            .map_err(|e| format!("handshake write: {e}"))?;
        let mut hdr = Vec::new();
        let mut b = [0u8; 1];
        loop {
            let n = tls
                .read(&mut b)
                .map_err(|e| format!("handshake read: {e}"))?;
            if n == 0 {
                return Err("server closed during WS handshake".into());
            }
            hdr.push(b[0]);
            if hdr.ends_with(b"\r\n\r\n") {
                break;
            }
            if hdr.len() > 8192 {
                return Err("handshake response too long".into());
            }
        }
        let status = String::from_utf8_lossy(&hdr);
        let line0 = status.lines().next().unwrap_or("");
        if !line0.starts_with("HTTP/1.1 101") {
            return Err(format!("no 101 upgrade: {line0:?}"));
        }

        // Send one masked TEXT frame.
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut frame = vec![0x81u8];
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, &p) in payload.iter().enumerate() {
            frame.push(p ^ mask[i % 4]);
        }
        tls.write_all(&frame)
            .map_err(|e| format!("frame write: {e}"))?;

        // Read the echoed (server→client, unmasked) frame.
        let mut h2 = [0u8; 2];
        tls.read_exact(&mut h2)
            .map_err(|e| format!("echo header read: {e}"))?;
        let mut len = (h2[1] & 0x7f) as usize;
        if len == 126 {
            let mut ext = [0u8; 2];
            tls.read_exact(&mut ext)
                .map_err(|e| format!("ext len: {e}"))?;
            len = u16::from_be_bytes(ext) as usize;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            tls.read_exact(&mut ext)
                .map_err(|e| format!("ext len: {e}"))?;
            len = u64::from_be_bytes(ext) as usize;
        }
        let mut body = vec![0u8; len];
        tls.read_exact(&mut body)
            .map_err(|e| format!("echo body read: {e}"))?;
        Ok(body)
    }

    #[test]
    fn coroutine_ws_over_tls_handler_executes() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let cert_path = workspace_root().join("tests/fixtures/tls/cert.pem");
        let key_path = workspace_root().join("tests/fixtures/tls/key.pem");
        let (Ok(cert_pem), Ok(key_pem)) = (
            std::fs::read_to_string(&cert_path),
            std::fs::read_to_string(&key_path),
        ) else {
            eprintln!("skip: tls fixtures not present at tests/fixtures/tls/");
            return;
        };
        fn kara_escape(s: &str) -> String {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        }
        // The flagship handler shape: recv a text frame, echo it back, loop.
        // `tg.spawn(|| handle_ws(ws))` is the non-blocking coroutine spawn.
        let src = format!(
            r#"
            fn handle_ws(ws: WebSocket) {{
                let mut buf: Array[u8, 4096] = [0u8; 4096];
                loop {{
                    let r = ws.recv_text(mut buf);
                    match r {{
                        Result.Ok(n) => {{
                            if n == 0 {{ break; }}
                            let _s = ws.send_text(buf);
                        }}
                        Result.Err(_) => {{ break; }}
                    }}
                }}
            }}
            fn main() {{
                let cert: String = "{cert}";
                let key: String = "{key}";
                let listener: TlsListener =
                    TlsListener.bind_tls("127.0.0.1:0", cert, key).unwrap();
                let mut tg: TaskGroup = TaskGroup.new();
                loop {{
                    match WebSocket.accept_tls(listener) {{
                        Result.Ok(ws) => {{ tg.spawn(|| handle_ws(ws)); }}
                        Result.Err(_) => {{}}
                    }}
                }}
            }}
        "#,
            cert = kara_escape(&cert_pem),
            key = kara_escape(&key_pem),
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_wss_{pid}_{nanos}"));
        if let Err(e) = compile_link_coro(&src, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        // The server runs an infinite accept loop, so we spawn + kill it
        // (it never returns like the TCP tests' `main`).
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wss coro server");
        let stdout = child.stdout.take().expect("child stdout");
        let (rx, _join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT within 15s");
            }
        };

        let payload = b"PING-coro-ws-over-tls";
        let result = wss_echo_roundtrip(port, payload);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        match result {
            Ok(body) => assert!(
                body.starts_with(payload),
                "the WS-over-TLS handler must echo the recv'd bytes (recv_text + \
                 send_text executed as a coroutine); got first {} bytes: {:?}",
                body.len().min(payload.len()),
                &body[..body.len().min(payload.len())]
            ),
            Err(e) => panic!(
                "WSS echo round-trip failed — the spawned WS handler did not execute \
                 its recv/send body (coroutine suspend/resume not driving the WS-over-TLS \
                 path): {e}"
            ),
        }
    }

    /// Concurrent WS-over-TLS gate — the single-shot test above almost always
    /// hits the good path; this fires 16 connections at once, all of which must
    /// recv+echo. Regression gate for the accept-loop resume race.
    ///
    /// The race (fixed): `WebSocket.accept_tls` runs a SELF-WAITING runtime
    /// function — it drains the accept backlog into an async handshake pool,
    /// then loops on a 5 ms re-drain until a completed handshake is available.
    /// Codegen used to emit a park-on-listener-readiness *before* it; after the
    /// first accept drained the backlog, a pending connection's **handshake
    /// completion does not make the listener readable**, so that park never
    /// resumed and the accept loop wedged while completed handshakes sat ready —
    /// their handlers never spawned (~half wedged under concurrency). Flipping
    /// coroutines on by default (3eda2b06) replaced the degenerate re-entering
    /// poll drive with that single park, exposing it. Fix: `lower_websocket_
    /// accept_tls` drops the redundant park in the inline case (`coro_ctx` is
    /// None — the canonical accept loop runs on its own thread, so the
    /// function's correct self-wait blocks only that thread, never the shared
    /// dispatcher). See task #21.
    #[test]
    fn coroutine_ws_over_tls_concurrent_handlers_all_execute() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let cert_path = workspace_root().join("tests/fixtures/tls/cert.pem");
        let key_path = workspace_root().join("tests/fixtures/tls/key.pem");
        let (Ok(cert_pem), Ok(key_pem)) = (
            std::fs::read_to_string(&cert_path),
            std::fs::read_to_string(&key_path),
        ) else {
            eprintln!("skip: tls fixtures not present");
            return;
        };
        fn kara_escape(s: &str) -> String {
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
        }
        let src = format!(
            r#"
            fn handle_ws(ws: WebSocket) {{
                let mut buf: Array[u8, 4096] = [0u8; 4096];
                loop {{
                    let r = ws.recv_text(mut buf);
                    match r {{
                        Result.Ok(n) => {{ if n == 0 {{ break; }} let _s = ws.send_text(buf); }}
                        Result.Err(_) => {{ break; }}
                    }}
                }}
            }}
            fn main() {{
                let cert: String = "{cert}";
                let key: String = "{key}";
                let listener: TlsListener =
                    TlsListener.bind_tls("127.0.0.1:0", cert, key).unwrap();
                let mut tg: TaskGroup = TaskGroup.new();
                loop {{
                    match WebSocket.accept_tls(listener) {{
                        Result.Ok(ws) => {{ tg.spawn(|| handle_ws(ws)); }}
                        Result.Err(_) => {{}}
                    }}
                }}
            }}
        "#,
            cert = kara_escape(&cert_pem),
            key = kara_escape(&key_pem),
        );

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_wss_conc_{pid}_{nanos}"));
        if let Err(e) = compile_link_coro(&src, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }
        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wss coro server");
        let stdout = child.stdout.take().expect("child stdout");
        let (rx, _join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT within 15s");
            }
        };

        // Fire N concurrent wss echo round-trips; every one must complete.
        const N: usize = 16;
        let results: std::sync::Arc<Mutex<Vec<bool>>> =
            std::sync::Arc::new(Mutex::new(vec![false; N]));
        let mut handles = Vec::new();
        for i in 0..N {
            let results = std::sync::Arc::clone(&results);
            handles.push(std::thread::spawn(move || {
                let ok = matches!(
                    wss_echo_roundtrip(port, b"PINGconc"),
                    Ok(body) if body.starts_with(b"PINGconc")
                );
                results.lock().unwrap_or_else(|p| p.into_inner())[i] = ok;
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let oks = results
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter(|&&b| b)
            .count();
        assert_eq!(
            oks, N,
            "only {oks}/{N} concurrent WS-over-TLS handlers echoed — the rest \
             wedged (coroutine resume race / accept-path handshake-pool mismatch)"
        );
    }

    #[test]
    fn coroutine_array_buffer_handler_services_connection() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_arrbuf_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(ARRAY_BUF_HANDLER_SRC, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let (exit_status, lines) = service_n_connections(&exe_path, 1, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "array-buffer coroutine binary exited non-success {exit_status:?}; \
             stdout lines: {lines:?}"
        );
        // The post-resume writes filled the whole 4096-byte buffer with 1s;
        // the sum proves they landed in-bounds (not over a neighbour or a
        // truncated 8-byte slot).
        assert!(
            lines.iter().any(|l| l == "4096"),
            "expected `4096` (sum over the fully-written Array[u8, 4096] \
             buffer) in stdout; got {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "2"),
            "expected `2` (main resumed after the coroutine drive); got {lines:?}"
        );
    }

    #[test]
    fn coroutine_array_buffer_handler_under_asan() {
        // Coro-frame heap-overflow regression under ASAN. A fixed-size
        // `Array[u8, 4096]` local held live across the `accept` park is the
        // ws_idle_holder flagship handler's `recv_text`-buffer shape. With
        // the frame-sizing bug the state-struct slot was 8 bytes (the i64
        // default) and the post-resume full-extent write overflowed into the
        // adjacent heap chunk — ASAN reports heap-buffer-overflow regardless
        // of the host allocator (silent on the macOS default malloc, aborts
        // under glibc / under ASAN everywhere). A clean ASAN exit proves the
        // frame slot is now the full `[4096 x i8]`.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        if !asan_available() {
            eprintln!("skip: ASAN unavailable on this host");
            return;
        }
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_arrbuf_asan_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(
            ARRAY_BUF_HANDLER_SRC,
            &exe_path,
            Some(&["-fsanitize=address"]),
        ) {
            eprintln!("skip: ASAN compile/link failed: {e}");
            return;
        }
        let asan_options = if cfg!(target_os = "macos") {
            "abort_on_error=0:exitcode=23"
        } else {
            "detect_leaks=1:abort_on_error=0:exitcode=23"
        };

        let (exit_status, lines) = service_n_connections(&exe_path, 1, Some(asan_options));
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "ASAN reported a memory error in the Array[u8, 4096] coroutine \
             handler (exit {exit_status:?}) — the coro frame slot for the \
             fixed-size array is undersized and the post-resume write \
             overflows it. stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "4096"),
            "array-buffer coroutine did not complete its in-bounds writes \
             under ASAN; stdout lines: {lines:?}"
        );
    }

    // ── Plain (non-TLS) WebSocket E2E round-trip (phase-8 line 128) ──────
    //
    // The WS-over-TLS echo round-trip above proves the framing surface
    // through a real compiled binary on the TLS path; these cover the
    // canonical plain-TCP path (`WebSocket.accept` + `recv_text` /
    // `send_text`) end-to-end, plus the line-128 handshake-validation
    // hardening reaching a real binary. Supersedes the IR-grep-only
    // posture of `tests/ws_framing.rs` for the plain path.

    /// Connect to a kara WS server on `port` (retrying briefly to absorb
    /// the BOUND_PORT-print-vs-accept-registration race), perform the
    /// RFC 6455 upgrade, send one masked TEXT frame carrying `payload`,
    /// and return the server's echoed (unmasked) payload bytes.
    fn ws_plain_echo_roundtrip(port: u16, payload: &[u8]) -> Result<Vec<u8>, String> {
        use std::io::{Read, Write};
        let mut sock = {
            let started = Instant::now();
            loop {
                match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                    Ok(s) => break s,
                    Err(_) if started.elapsed() < Duration::from_secs(3) => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => return Err(format!("tcp connect: {e}")),
                }
            }
        };
        sock.set_read_timeout(Some(Duration::from_secs(8))).ok();
        sock.set_write_timeout(Some(Duration::from_secs(8))).ok();

        // RFC 6455 §4.2 upgrade handshake.
        let req = "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                   Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                   Sec-WebSocket-Version: 13\r\n\r\n";
        sock.write_all(req.as_bytes())
            .map_err(|e| format!("handshake write: {e}"))?;
        let mut hdr = Vec::new();
        let mut b = [0u8; 1];
        loop {
            let n = sock
                .read(&mut b)
                .map_err(|e| format!("handshake read: {e}"))?;
            if n == 0 {
                return Err("server closed during WS handshake".into());
            }
            hdr.push(b[0]);
            if hdr.ends_with(b"\r\n\r\n") {
                break;
            }
            if hdr.len() > 8192 {
                return Err("handshake response too long".into());
            }
        }
        let status = String::from_utf8_lossy(&hdr);
        let line0 = status.lines().next().unwrap_or("");
        if !line0.starts_with("HTTP/1.1 101") {
            return Err(format!("no 101 upgrade: {line0:?}"));
        }

        // Send one masked client→server TEXT frame (RFC 6455 §5).
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut frame = vec![0x81u8]; // FIN=1, opcode=0x1 (text)
        if payload.len() < 126 {
            frame.push(0x80 | payload.len() as u8);
        } else {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, &p) in payload.iter().enumerate() {
            frame.push(p ^ mask[i % 4]);
        }
        sock.write_all(&frame)
            .map_err(|e| format!("frame write: {e}"))?;

        // Read the echoed (server→client, unmasked) frame.
        let mut h2 = [0u8; 2];
        sock.read_exact(&mut h2)
            .map_err(|e| format!("echo header read: {e}"))?;
        let mut len = (h2[1] & 0x7f) as usize;
        if len == 126 {
            let mut ext = [0u8; 2];
            sock.read_exact(&mut ext)
                .map_err(|e| format!("ext len: {e}"))?;
            len = u16::from_be_bytes(ext) as usize;
        } else if len == 127 {
            let mut ext = [0u8; 8];
            sock.read_exact(&mut ext)
                .map_err(|e| format!("ext len: {e}"))?;
            len = u64::from_be_bytes(ext) as usize;
        }
        let mut body = vec![0u8; len];
        sock.read_exact(&mut body)
            .map_err(|e| format!("echo body read: {e}"))?;
        Ok(body)
    }

    /// Connect to a kara WS server on `port`, send the raw HTTP request
    /// `request` verbatim, and return the response's HTTP status line.
    /// Used to assert the §4.2.1 rejection statuses (400 / 426) emitted
    /// by a real compiled binary.
    fn ws_plain_handshake_status(port: u16, request: &str) -> Result<String, String> {
        use std::io::{Read, Write};
        let mut sock = {
            let started = Instant::now();
            loop {
                match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                    Ok(s) => break s,
                    Err(_) if started.elapsed() < Duration::from_secs(3) => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(e) => return Err(format!("tcp connect: {e}")),
                }
            }
        };
        sock.set_read_timeout(Some(Duration::from_secs(8))).ok();
        sock.set_write_timeout(Some(Duration::from_secs(8))).ok();
        sock.write_all(request.as_bytes())
            .map_err(|e| format!("request write: {e}"))?;
        let mut hdr = Vec::new();
        let mut b = [0u8; 1];
        loop {
            let n = sock
                .read(&mut b)
                .map_err(|e| format!("response read: {e}"))?;
            if n == 0 {
                break;
            }
            hdr.push(b[0]);
            if hdr.ends_with(b"\r\n\r\n") {
                break;
            }
            if hdr.len() > 8192 {
                return Err("response too long".into());
            }
        }
        let status = String::from_utf8_lossy(&hdr);
        Ok(status.lines().next().unwrap_or("").to_string())
    }

    /// The canonical plain-TCP WebSocket echo path through a real
    /// compiled kara binary: `WebSocket.accept(listener)` then a
    /// `recv_text` → `send_text` echo loop, spawned per-connection on a
    /// `TaskGroup`. Proves the runtime framing FFI + codegen wiring
    /// round-trips end-to-end, not just at the IR-shape level.
    #[test]
    fn e2e_plain_websocket_text_echo_roundtrip() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn handle_ws(ws: WebSocket) {
                let mut buf: Array[u8, 4096] = [0u8; 4096];
                loop {
                    let r = ws.recv_text(mut buf);
                    match r {
                        Result.Ok(n) => {
                            if n == 0 { break; }
                            let _s = ws.send_text(buf);
                        }
                        Result.Err(_) => { break; }
                    }
                }
            }
            fn main() {
                let listener: TcpListener = TcpListener.bind("127.0.0.1:0").unwrap();
                let mut tg: TaskGroup = TaskGroup.new();
                loop {
                    match WebSocket.accept(listener) {
                        Result.Ok(ws) => { tg.spawn(|| handle_ws(ws)); }
                        Result.Err(_) => {}
                    }
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_plain_ws_{pid}_{nanos}"));
        if let Err(e) = compile_link_coro(src, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn plain ws server");
        let stdout = child.stdout.take().expect("child stdout");
        let (rx, _join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT within 15s");
            }
        };

        let payload = b"PING-plain-ws-echo";
        let result = ws_plain_echo_roundtrip(port, payload);
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        match result {
            Ok(body) => assert!(
                body.starts_with(payload),
                "plain WS handler must echo the recv'd bytes (recv_text + send_text \
                 through a real binary); got first {} bytes: {:?}",
                body.len().min(payload.len()),
                &body[..body.len().min(payload.len())]
            ),
            Err(e) => panic!("plain WS echo round-trip failed: {e}"),
        }
    }

    /// The line-128 handshake hardening reaching a real compiled binary:
    /// an otherwise-valid upgrade carrying an unsupported
    /// `Sec-WebSocket-Version` must be answered with `426 Upgrade
    /// Required` (RFC 6455 §4.4), and a request missing `Upgrade`/
    /// `Connection` with `400 Bad Request` — never a spurious 101.
    #[test]
    fn e2e_plain_websocket_handshake_rejects_bad_request() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let src = r#"
            fn handle_ws(ws: WebSocket) {
                let mut buf: Array[u8, 1024] = [0u8; 1024];
                let _r = ws.recv_text(mut buf);
            }
            fn main() {
                let listener: TcpListener = TcpListener.bind("127.0.0.1:0").unwrap();
                let mut tg: TaskGroup = TaskGroup.new();
                loop {
                    match WebSocket.accept(listener) {
                        Result.Ok(ws) => { tg.spawn(|| handle_ws(ws)); }
                        Result.Err(_) => {}
                    }
                }
            }
        "#;

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_plain_ws_reject_{pid}_{nanos}"));
        if let Err(e) = compile_link_coro(src, &exe_path, None) {
            panic!("compile/link failed: {e}");
        }

        let mut child = Command::new(&exe_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn plain ws server");
        let stdout = child.stdout.take().expect("child stdout");
        let (rx, _join) = spawn_stdout_reader(stdout);
        let port = match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(p) => p,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&exe_path);
                panic!("server did not emit BOUND_PORT within 15s");
            }
        };

        // Bad version → 426 (each handshake gets its own accepted conn).
        let bad_version = "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n\
                           Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                           Sec-WebSocket-Version: 7\r\n\r\n";
        let bad_version_status = ws_plain_handshake_status(port, bad_version);

        // Missing Upgrade/Connection → 400.
        let not_ws = "GET / HTTP/1.1\r\nHost: localhost\r\n\
                      Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                      Sec-WebSocket-Version: 13\r\n\r\n";
        let not_ws_status = ws_plain_handshake_status(port, not_ws);

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&exe_path);

        let bad_version_status = bad_version_status.expect("bad-version handshake status");
        assert!(
            bad_version_status.starts_with("HTTP/1.1 426"),
            "unsupported Sec-WebSocket-Version must get 426 from the real binary; got: {bad_version_status:?}"
        );
        let not_ws_status = not_ws_status.expect("non-ws handshake status");
        assert!(
            not_ws_status.starts_with("HTTP/1.1 400"),
            "request missing Upgrade/Connection must get 400 from the real binary; got: {not_ws_status:?}"
        );
    }

    /// B-2026-06-19 — a `-> bool` coroutine driven inline from another coroutine
    /// must compile (no `Branch condition is not 'i1' type!` verifier crash) AND
    /// carry its REAL return value back. `probe` returns `false`, so `not
    /// probe(...)` takes the `then` branch and prints `7` (never `8`).
    #[test]
    fn coroutine_bool_return_false_branches_then() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_bool_false_{pid}_{nanos}"));

        // Compiling at all proves the verifier crash is fixed (the always-`i64 0`
        // condition no longer reaches `br`).
        if let Err(e) = compile_link_coro(CORO_BOOL_FALSE_SRC, &exe_path, None) {
            panic!("compile/link failed (verifier crash regressed?): {e}");
        }

        // One connection triggers `probe`'s `accept` park; the dispatcher
        // resumes it, it returns false, and `main` branches on the real value.
        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "coro bool-return binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "7"),
            "`probe` returned false → `not probe(...)` must take the then-branch \
             (print 7); stdout lines: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "8"),
            "false return must NOT take the else-branch (8); stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "9"),
            "main did not resume after the inline coro drive; stdout lines: {lines:?}"
        );
    }

    /// Value-correctness sibling: identical program except `probe` returns
    /// `true`, so `not probe(...)` is false and the binary prints `8` (never
    /// `7`). The 7-vs-8 difference between the two tests is what distinguishes a
    /// real fix from a still-hard-coded-0 one — a fix that ignored `probe`'s
    /// value would print `7` in BOTH.
    #[test]
    fn coroutine_bool_return_true_branches_else() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let Some(rt) = runtime_path() else {
            eprintln!("skip: libkarac_runtime.a not built");
            return;
        };
        std::env::set_var("KARAC_RUNTIME", &rt);

        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let exe_path = PathBuf::from(format!("/tmp/karac_coro_bool_true_{pid}_{nanos}"));

        if let Err(e) = compile_link_coro(CORO_BOOL_TRUE_SRC, &exe_path, None) {
            panic!("compile/link failed (verifier crash regressed?): {e}");
        }

        let (exit_status, lines) = service_one_connection(&exe_path, None);
        let _ = std::fs::remove_file(&exe_path);

        assert!(
            exit_status.success(),
            "coro bool-return binary exited non-success {exit_status:?}; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "8"),
            "`probe` returned true → `not probe(...)` is false, must take the \
             else-branch (print 8); stdout lines: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l == "7"),
            "true return must NOT take the then-branch (7) — that would be the \
             always-0 bug; stdout lines: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l == "9"),
            "main did not resume after the inline coro drive; stdout lines: {lines:?}"
        );
    }
}
