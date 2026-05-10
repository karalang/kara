// Slice E — Node reference impl for the three-language Parallax bench.
//
// `http` (Node stdlib, no Express) + `Promise.all` for fan-out.
// Single-process per F4 — README footnotes that cluster mode would
// scale roughly linearly with worker count at the cost of process
// orchestration.
//
// **Sleep substitute.** Per the F5 design lock, providers should
// approximate 2/5/8/12 ms latency. Kāra's stdlib has no `sleep_ms`
// so its impl uses CPU-bound busy loops; Node mirrors the busy-loop
// shape (not `setTimeout`) at the same iteration counts so the four
// impls stay apples-to-apples. README footnotes the deviation.
//
// **`BOUND_PORT=<n>` line.** Mirrors Kāra's runtime convention so
// `bench.sh` can use one port-discovery helper across all four impls.

'use strict';

const http = require('http');

const FETCH_PROFILE_WORK = 700000;
const FETCH_ORDERS_WORK = 4000000;
const FETCH_NOTIFS_WORK = 1700000;
const FETCH_RECOMMEND_WORK = 2700000;

function busyLoop(n) {
  let sum = 0;
  for (let i = 0; i < n; i++) {
    sum = sum + i;
  }
  return sum;
}

async function fetchProfileName(userId) {
  busyLoop(FETCH_PROFILE_WORK);
  void userId;
  return 'Alice';
}

async function fetchLatestOrderId(userId) {
  busyLoop(FETCH_ORDERS_WORK);
  void userId;
  return 1001;
}

async function fetchTopNotificationKind(userId) {
  busyLoop(FETCH_NOTIFS_WORK);
  void userId;
  return 1;
}

async function fetchTopRecommendationId(userId) {
  busyLoop(FETCH_RECOMMEND_WORK);
  void userId;
  return 7001;
}

async function getDashboard(userId) {
  // Promise.all is Node's idiomatic fan-out + join. With single-process
  // event loop + CPU-bound busy loops, the four awaits resolve serially
  // on the same thread — the parallelism is async-task scheduling, not
  // real cores. F4: this is the honest single-process default.
  const [profileName, orderId, notifKind, recId] = await Promise.all([
    fetchProfileName(userId),
    fetchLatestOrderId(userId),
    fetchTopNotificationKind(userId),
    fetchTopRecommendationId(userId),
  ]);
  return {
    profile: { user_id: userId, name: profileName },
    latest_order: { order_id: orderId },
    top_notification: { kind: notifKind },
    top_recommendation: { item_id: recId },
  };
}

const server = http.createServer(async (req, res) => {
  void req.url;
  const d = await getDashboard(1);
  const body = JSON.stringify(d);
  res.statusCode = 200;
  res.setHeader('content-type', 'application/json');
  res.end(body);
});

server.listen(0, '127.0.0.1', () => {
  const addr = server.address();
  process.stdout.write(`BOUND_PORT=${addr.port}\n`);
});
