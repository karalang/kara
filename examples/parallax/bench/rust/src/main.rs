// Slice E — Rust reference impl for the three-language Parallax bench.
//
// `tokio` + `hyper` + manual `tokio::join!` for fan-out. Same provider
// busy-loop kernel sizes as the Kāra impl (`server.kara`) so the four
// impls stay apples-to-apples. Single-binary, no async closures or
// state — just the `GET /dashboard/<n>` shape.
//
// **Sleep substitute.** Per the F5 design lock, providers should
// approximate 2/5/8/12 ms latency. Kāra has no `sleep_ms` in stdlib
// so its impl uses CPU-bound busy loops; for fairness, this impl
// mirrors the busy-loop shape (not `tokio::time::sleep`) at the same
// iteration counts. README footnotes the deviation.
//
// **Path parsing.** `req.uri().path()` is read but the user_id is
// passed unmodified from the wrk-generated URL into `get_dashboard`.
// User_id only feeds the busy-loop addend, so the bench's load is
// effectively user_id-invariant.
//
// **`BOUND_PORT=<n>` line.** Mirrors Kāra's runtime convention so
// `bench.sh` can use a single port-discovery helper across all four
// impls.

use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

const FETCH_PROFILE_WORK: i64 = 700_000;
const FETCH_ORDERS_WORK: i64 = 4_000_000;
const FETCH_NOTIFS_WORK: i64 = 1_700_000;
const FETCH_RECOMMEND_WORK: i64 = 2_700_000;

fn busy_loop(n: i64) -> i64 {
    let mut sum: i64 = 0;
    let mut i: i64 = 0;
    while i < n {
        sum = sum.wrapping_add(i);
        i += 1;
    }
    sum
}

async fn fetch_profile_name(user_id: i64) -> &'static str {
    let _ = busy_loop(FETCH_PROFILE_WORK).wrapping_add(user_id);
    "Alice"
}

async fn fetch_latest_order_id(user_id: i64) -> i64 {
    let _ = busy_loop(FETCH_ORDERS_WORK).wrapping_add(user_id);
    1001
}

async fn fetch_top_notification_kind(user_id: i64) -> i64 {
    let _ = busy_loop(FETCH_NOTIFS_WORK).wrapping_add(user_id);
    1
}

async fn fetch_top_recommendation_id(user_id: i64) -> i64 {
    let _ = busy_loop(FETCH_RECOMMEND_WORK).wrapping_add(user_id);
    7001
}

struct Dashboard {
    profile_name: &'static str,
    order_id: i64,
    notif_kind: i64,
    rec_id: i64,
}

async fn get_dashboard(user_id: i64) -> Dashboard {
    // Fan-out + join. Each branch runs on the tokio worker pool;
    // `tokio::join!` polls them concurrently and returns when all
    // four complete. CPU-bound busy loops will saturate worker
    // threads — F4: tokio's default multi-thread runtime uses
    // num_cpus workers, matching Go's GOMAXPROCS default.
    let (profile_name, order_id, notif_kind, rec_id) = tokio::join!(
        tokio::task::spawn_blocking(move || futures_block_on(fetch_profile_name(user_id))),
        tokio::task::spawn_blocking(move || futures_block_on(fetch_latest_order_id(user_id))),
        tokio::task::spawn_blocking(move || futures_block_on(fetch_top_notification_kind(user_id))),
        tokio::task::spawn_blocking(move || futures_block_on(fetch_top_recommendation_id(user_id))),
    );
    Dashboard {
        profile_name: profile_name.unwrap(),
        order_id: order_id.unwrap(),
        notif_kind: notif_kind.unwrap(),
        rec_id: rec_id.unwrap(),
    }
}

// Tiny ad-hoc executor for the spawn_blocking closures so the
// per-fetch fns can stay async (mirrors the typical "I/O looks
// async, but compute we're benching is sync" shape). Using
// `block_on` from a thread that isn't a tokio worker would deadlock;
// `spawn_blocking` runs on the blocking-pool, so block-on-self
// would also deadlock. The fetch fns return immediately after
// the busy loop, so a noop poll loop is sufficient.
fn futures_block_on<F: std::future::Future>(mut fut: F) -> F::Output {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    fn noop_raw_waker() -> std::task::RawWaker {
        const VTABLE: std::task::RawWakerVTable = std::task::RawWakerVTable::new(
            |_| noop_raw_waker(),
            |_| {},
            |_| {},
            |_| {},
        );
        std::task::RawWaker::new(std::ptr::null(), &VTABLE)
    }

    let waker = unsafe { Waker::from_raw(noop_raw_waker()) };
    let mut cx = Context::from_waker(&waker);
    // SAFETY: fut is not moved after this point; we own it on the stack.
    let mut pinned = unsafe { Pin::new_unchecked(&mut fut) };
    loop {
        if let Poll::Ready(out) = pinned.as_mut().poll(&mut cx) {
            return out;
        }
        std::hint::spin_loop();
    }
}

fn dashboard_to_json(user_id: i64, d: &Dashboard) -> String {
    format!(
        "{{\"profile\":{{\"user_id\":{},\"name\":\"{}\"}},\"latest_order\":{{\"order_id\":{}}},\"top_notification\":{{\"kind\":{}}},\"top_recommendation\":{{\"item_id\":{}}}}}",
        user_id, d.profile_name, d.order_id, d.notif_kind, d.rec_id
    )
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let _path = req.uri().path().to_string();
    let user_id: i64 = 1;
    let d = get_dashboard(user_id).await;
    let body = dashboard_to_json(user_id, &d);
    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .unwrap())
}

#[tokio::main]
async fn main() {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await.expect("bind failed");
    let local = listener.local_addr().expect("local_addr failed");
    println!("BOUND_PORT={}", local.port());
    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(p) => p,
            Err(_) => continue,
        };
        let io = TokioIo::new(stream);
        tokio::spawn(async move {
            let svc = service_fn(handle);
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await;
        });
    }
}
