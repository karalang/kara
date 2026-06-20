// verify_browser.mjs — drive the real Slipstream build in headless Chrome over
// the Chrome DevTools Protocol and assert the wind tunnel actually runs.
//
// Node's mock-DOM harness cannot exercise this demo: it needs SharedArrayBuffer
// + a real Web Worker pool + a <canvas>, and the whole point is a continuous
// render loop the host keeps waking with no `await`. So this drives an actual
// browser and watches the rendered canvas.
//
// Requires: a recent Chrome/Chromium and node >= 22 (global WebSocket/fetch).
// Run:  ./build.sh --build && node verify_browser.mjs
// Exits 0 on PASS, 1 on failure, 2 on a missing-prerequisite skip.
//
// What it asserts (the regression gate for the browser kernel):
//   1. The page loads cross-origin-isolated (SharedArrayBuffer available).
//   2. The frame counter advances — the blocking render loop runs on a worker
//      the host keeps waking (no `await`).
//   3. The canvas has real, non-uniform content (the vorticity field), not a
//      blank or flat frame.
//   4. The canvas EVOLVES — two samples taken seconds apart differ. This is the
//      property unique to Slipstream on this spine: the grid is state carried
//      and advanced across frames, so the wake develops over time. A static
//      (recompute-from-scratch) demo would fail this only if its inputs moved;
//      here nothing moves but the fluid, so a changing canvas proves the
//      carried grid is genuinely integrating.
//   5. SOAK — let the loop run to several hundred frames and confirm it is still
//      advancing and still non-uniform at the end (catches the leak/OOM and
//      deadlock classes that only surface in a real browser, cf. Fathom).

import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PORT = 8741; // static server
const CDP_PORT = 9397; // chrome remote-debugging
const PAGE_URL = `http://127.0.0.1:${PORT}/index.html`;
const HERE = new URL(".", import.meta.url).pathname;

function findChrome() {
  const candidates = [
    process.env.CHROME,
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "google-chrome",
    "chromium",
    "chromium-browser",
  ].filter(Boolean);
  for (const c of candidates) {
    if (c.includes("/")) {
      try {
        if (spawnSync(c, ["--version"]).status === 0) return c;
      } catch {}
    } else {
      const r = spawnSync("which", [c]);
      if (r.status === 0) return r.stdout.toString().trim();
    }
  }
  return null;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function waitForHttp(url, tries = 50) {
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url);
      if (r.ok || r.status === 404) return true;
    } catch {}
    await sleep(100);
  }
  return false;
}

// Minimal CDP client over a single WebSocket, flatten mode (sessionId per msg).
class CDP {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener("message", (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        msg.error ? reject(new Error(JSON.stringify(msg.error))) : resolve(msg.result);
      }
    });
  }
  // Per-call timeout: a `Runtime.evaluate` issued while the page's main thread is
  // briefly busy (spinning up the worker pool at start-up) does not return until
  // the thread is free, so without a per-call timeout one stuck evaluate would
  // hang the whole run. On timeout we drop the pending entry and reject so the
  // caller can retry; a late response is then ignored (`pending.has(id)` false).
  send(method, params = {}, sessionId, timeoutMs = 8000) {
    const id = ++this.id;
    const payload = { id, method, params };
    if (sessionId) payload.sessionId = sessionId;
    this.ws.send(JSON.stringify(payload));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP ${method} timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
    });
  }
}

let server, chrome, userDataDir;
function cleanup() {
  try { chrome?.kill("SIGKILL"); } catch {}
  try { server?.kill("SIGKILL"); } catch {}
  try { if (userDataDir) rmSync(userDataDir, { recursive: true, force: true }); } catch {}
}
process.on("exit", cleanup);

const WATCHDOG = setTimeout(() => {
  console.error(`FAIL: watchdog — verify exceeded 150s (last stage: ${lastStage})`);
  process.exit(3);
}, 150000);
let lastStage = "start";
const stage = (s) => { lastStage = s; console.error(`[stage] ${s}`); };

async function main() {
  const chromePath = findChrome();
  if (!chromePath) {
    console.error("SKIP: no Chrome/Chromium found (set $CHROME).");
    process.exit(2);
  }
  stage("serve");

  server = spawn("python3", [join(HERE, "serve.py"), String(PORT)], { stdio: "ignore" });
  if (!(await waitForHttp(PAGE_URL))) throw new Error("static server never came up");

  userDataDir = mkdtempSync(join(tmpdir(), "slipstream-cdp-"));
  chrome = spawn(chromePath, [
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    "--disable-dev-shm-usage",
    "--no-first-run",
    "--no-default-browser-check",
    `--user-data-dir=${userDataDir}`,
    `--remote-debugging-port=${CDP_PORT}`,
    "about:blank",
  ], { stdio: "ignore" });

  stage("cdp-endpoint");
  let version;
  for (let i = 0; i < 60; i++) {
    try {
      version = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json/version`)).json();
      break;
    } catch {}
    await sleep(100);
  }
  if (!version) throw new Error("Chrome CDP endpoint never came up");

  stage("cdp-ws");
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener("open", res, { once: true });
    ws.addEventListener("error", rej, { once: true });
  });
  const cdp = new CDP(ws);

  stage("create-target");
  const { targetId } = await cdp.send("Target.createTarget", { url: PAGE_URL });
  const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
  await cdp.send("Page.enable", {}, sessionId);
  await cdp.send("Runtime.enable", {}, sessionId);
  stage("attached");

  const retry = async (fn, label, attempts = 6) => {
    let last;
    for (let i = 0; i < attempts; i++) {
      try { return await fn(); }
      catch (e) {
        last = e;
        if (!/timed out/.test(e.message)) throw e;
        await sleep(300);
      }
    }
    throw new Error(`${label} kept timing out (${attempts} attempts): ${last?.message}`);
  };

  const evalJs = (expr) => retry(async () => {
    const r = await cdp.send("Runtime.evaluate", {
      expression: expr, returnByValue: true, awaitPromise: true,
    }, sessionId);
    if (r.exceptionDetails) throw new Error("page JS threw: " + JSON.stringify(r.exceptionDetails));
    return r.result.value;
  }, "evalJs");

  stage("isolation");
  let isolated = false;
  for (let i = 0; i < 60; i++) {
    isolated = await evalJs("self.crossOriginIsolated === true");
    if (isolated) break;
    await sleep(100);
  }
  if (!isolated) throw new Error("page is NOT cross-origin isolated (no SharedArrayBuffer)");

  const frameCount = () => evalJs(
    `(() => { const o = document.getElementById('overlay'); const m = o && o.textContent.match(/frames:\\s*(\\d+)/); return m ? +m[1] : 0; })()`
  );

  // A content fingerprint over the RAW 256×96 framebuffer (getImageData reads the
  // backing store at internal resolution, not the CSS-scaled size), plus the
  // per-channel range so we can assert non-uniformity.
  const fingerprint = () => evalJs(`(() => {
    const c = document.getElementById('screen');
    const g = c.getContext('2d');
    const d = g.getImageData(0, 0, c.width, c.height).data;
    let h = 0, lo = 255, hi = 0;
    for (let i = 0; i < d.length; i += 17) { h = (h * 31 + d[i]) >>> 0; if (d[i] < lo) lo = d[i]; if (d[i] > hi) hi = d[i]; }
    return h + ':' + lo + ':' + hi;
  })()`);

  // Frames must advance.
  stage("frames");
  let f0 = 0, f1 = 0;
  const framesDeadline = Date.now() + 45000;
  while (Date.now() < framesDeadline) {
    try {
      const a = await frameCount();
      await sleep(700);
      const b = await frameCount();
      if (b > a) { f0 = a; f1 = b; break; }
    } catch {}
    await sleep(300);
  }
  if (!(f1 > f0)) {
    throw new Error(`render loop never advanced (frames stayed ${f0} -> ${f1}) within 45s`);
  }

  // Canvas must have real, non-uniform content.
  stage("content");
  const fpEarly = await fingerprint();
  const [hEarly, loE, hiE] = fpEarly.split(":").map(Number);
  if (hiE - loE < 8) throw new Error(`canvas looks blank/uniform: ${fpEarly}`);

  // Canvas must EVOLVE — the carried grid is integrating, so the wake develops.
  stage("evolving");
  let fpMid = fpEarly, evolved = false;
  for (let i = 0; i < 12; i++) {
    await sleep(700);
    fpMid = await fingerprint();
    if (fpMid.split(":")[0] !== fpEarly.split(":")[0]) { evolved = true; break; }
  }
  if (!evolved) {
    throw new Error(`canvas did not evolve over time (carried grid not integrating): ${fpEarly}`);
  }

  // SOAK — run out to several hundred frames; confirm still advancing and still
  // non-uniform (no leak/OOM, no deadlock).
  stage("soak");
  const soakTarget = Math.max(f1 + 250, 350);
  const soakDeadline = Date.now() + 70000;
  let fSoak = f1;
  while (fSoak < soakTarget && Date.now() < soakDeadline) {
    await sleep(800);
    try { fSoak = await frameCount(); } catch {}
  }
  if (fSoak < soakTarget) {
    throw new Error(`soak stalled: only reached frame ${fSoak} of ${soakTarget} before 70s`);
  }
  await sleep(800);
  const fAfter = await frameCount();
  if (!(fAfter > fSoak)) {
    throw new Error(`render loop stopped advancing after soak (stuck at ${fSoak})`);
  }
  const fpLate = await fingerprint();
  const [hLate, loL, hiL] = fpLate.split(":").map(Number);
  if (hiL - loL < 8) throw new Error(`canvas went blank/uniform during soak: ${fpLate}`);

  // Angle-of-attack control: scroll steepens / flattens the wing. The wing draws
  // as grey pixels (~150,155,165); the vertical extent of those pixels grows
  // monotonically with the wing slope and is INDEPENDENT of the fluid evolution,
  // so it isolates input-driven change from the constantly-moving wake. We drive
  // it with WHEEL (not keydown): headless Chrome turns a few CDP key dispatches
  // into a self-sustaining keydown flood that wedges the renderer (the
  // documented Fathom artifact), whereas wheel events are clean. This proves the
  // event-data channel (std.web.events.wheel) reaches the wasm render loop and
  // moves the simulation's boundary.
  stage("angle");
  const wingHeight = () => evalJs(`(() => {
    const c = document.getElementById('screen');
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
    let lo = 1e9, hi = -1;
    for (let y = 0; y < c.height; y++) {
      for (let x = 0; x < c.width; x++) {
        const i = (y * c.width + x) * 4;
        if (Math.abs(d[i]-150) < 10 && Math.abs(d[i+1]-155) < 10 && Math.abs(d[i+2]-165) < 10) {
          if (y < lo) lo = y; if (y > hi) hi = y;
        }
      }
    }
    return hi < 0 ? 0 : (hi - lo + 1);
  })()`);
  const rect = await evalJs(`(() => { const r = document.getElementById('screen').getBoundingClientRect();
    return { left: r.left, top: r.top, w: r.width, h: r.height }; })()`);
  const wcx = Math.round(rect.left + rect.w / 2), wcy = Math.round(rect.top + rect.h / 2);
  const wheelAt = (dy) => cdp.send("Input.dispatchMouseEvent",
    { type: "mouseWheel", x: wcx, y: wcy, deltaX: 0, deltaY: dy }, sessionId, 60000);

  const hMid = await wingHeight();
  for (let i = 0; i < 6; i++) { await wheelAt(-240); await sleep(120); }  // steepen
  await sleep(600);
  const hSteep = await wingHeight();
  for (let i = 0; i < 12; i++) { await wheelAt(240); await sleep(120); }  // flatten
  await sleep(600);
  const hFlat = await wingHeight();
  if (!(hSteep > hMid + 2)) {
    throw new Error(`scroll-up did not steepen the wing (grey height ${hMid} -> ${hSteep})`);
  }
  if (!(hFlat < hSteep - 2)) {
    throw new Error(`scroll-down did not flatten the wing (grey height ${hSteep} -> ${hFlat})`);
  }

  console.log(
    `PASS — isolated, frames ${f0}->${f1}->${fSoak}->${fAfter}, ` +
    `content ${fpEarly} --evolves--> ${fpMid} --soak(${fAfter} frames)--> ${fpLate}, ` +
    `wing angle: grey-height ${hMid} --steepen--> ${hSteep} --flatten--> ${hFlat}`
  );
  clearTimeout(WATCHDOG);
  ws.close();
}

main().then(() => { clearTimeout(WATCHDOG); process.exit(0); }).catch((e) => {
  clearTimeout(WATCHDOG);
  console.error("FAIL:", e.message);
  process.exit(1);
});
