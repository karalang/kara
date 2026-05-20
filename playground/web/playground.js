// Browser playground shell — tracker line 703.
//
// Loads the wasm-bindgen module produced by `wasm-pack build --target web
// --out-dir web/pkg` (see playground/Cargo.toml + playground/src/lib.rs),
// then wires up: textarea editor ↔ Run button ↔ wasm `run(source)` ↔
// stdout / diagnostics rendering. Diagnostics are clickable and move the
// textarea selection to the offset they report.
//
// Slice 3 shipped the editor / run / output / diagnostics chain.
// Slice 4 adds URL-share: Share button compresses the editor source
// via `CompressionStream("deflate-raw")`, base64url-encodes the bytes,
// and writes `location.hash = "code=..."`. On load, an inverse path
// hydrates the editor before init runs.

import init, { run } from "./pkg/karac_playground.js";

const DEFAULT_SOURCE = `// Welcome to the Kāra playground.
// Edit the source on the left and hit Run.

fn main() {
    let name = "world";
    println("Hello, {name}!");
}
`;

const PHASE_LABELS = {
  parse: "parse",
  resolve: "resolve",
  typecheck: "type",
  effect: "effect",
  ownership: "ownership",
  runtime: "runtime",
};

const editor = document.getElementById("editor");
const stdoutEl = document.getElementById("stdout");
const diagnosticsEl = document.getElementById("diagnostics");
const statusEl = document.getElementById("status");
const runBtn = document.getElementById("run");
const shareBtn = document.getElementById("share");

async function boot() {
  // Hydrate the editor from `location.hash` *before* init runs so a
  // shared link lands in the exact state the sender saw. Falls back to
  // DEFAULT_SOURCE on missing / malformed hashes — the user sees a
  // notice via the status line rather than a silent reset.
  const hydrated = await hydrateFromUrl();
  editor.value = hydrated.source;
  try {
    await init();
  } catch (e) {
    setStatus(`failed to load wasm module: ${e}`, "err");
    return;
  }
  runBtn.disabled = false;
  runBtn.textContent = "Run";
  shareBtn.disabled = false;
  setStatus(hydrated.statusText, hydrated.statusKind);
  runBtn.addEventListener("click", onRun);
  shareBtn.addEventListener("click", onShare);
  editor.addEventListener("keydown", onEditorKey);
}

function setStatus(text, kind) {
  statusEl.textContent = text;
  statusEl.classList.remove("ok", "err");
  if (kind) statusEl.classList.add(kind);
}

function renderResult(result) {
  stdoutEl.textContent = result.stdout.join("");

  diagnosticsEl.innerHTML = "";
  for (const d of result.diagnostics) {
    const li = document.createElement("li");
    li.className = `diagnostic phase-${d.phase}`;

    const phase = document.createElement("span");
    phase.className = "phase";
    phase.textContent = PHASE_LABELS[d.phase] ?? d.phase;
    li.appendChild(phase);

    const loc = document.createElement("span");
    loc.className = "location";
    loc.textContent = `${d.line}:${d.column}`;
    li.appendChild(loc);

    const msg = document.createElement("span");
    msg.className = "message";
    msg.textContent = d.message;
    li.appendChild(msg);

    li.addEventListener("click", () => focusOffset(d.offset, d.length));
    diagnosticsEl.appendChild(li);
  }

  if (result.ok) {
    setStatus("OK.", "ok");
  } else if (result.diagnostics.length === 0) {
    setStatus("Finished with no output.", null);
  } else {
    const summary = summarizeDiagnostics(result.diagnostics);
    setStatus(summary, "err");
  }
}

function summarizeDiagnostics(diagnostics) {
  const counts = {};
  for (const d of diagnostics) {
    counts[d.phase] = (counts[d.phase] ?? 0) + 1;
  }
  const parts = Object.entries(counts).map(
    ([phase, n]) => `${n} ${PHASE_LABELS[phase] ?? phase}`,
  );
  return parts.join(", ");
}

function focusOffset(offset, length) {
  editor.focus();
  editor.setSelectionRange(offset, offset + Math.max(length, 1));
}

function onRun() {
  runBtn.disabled = true;
  runBtn.textContent = "Running…";
  setStatus("Running…", null);
  // Defer to the next tick so the status update paints before the
  // (potentially long) synchronous wasm call.
  requestAnimationFrame(() => {
    try {
      const result = run(editor.value);
      renderResult(result);
    } catch (e) {
      stdoutEl.textContent = "";
      diagnosticsEl.innerHTML = "";
      setStatus(`internal error: ${e}`, "err");
    } finally {
      runBtn.disabled = false;
      runBtn.textContent = "Run";
    }
  });
}

function onEditorKey(e) {
  // Cmd/Ctrl+Enter shortcut for Run.
  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
    e.preventDefault();
    if (!runBtn.disabled) onRun();
    return;
  }
  // Cmd/Ctrl+S triggers Share (and prevents the browser save dialog).
  if ((e.metaKey || e.ctrlKey) && e.key === "s") {
    e.preventDefault();
    if (!shareBtn.disabled) onShare();
    return;
  }
  // Tab inserts a literal tab character; without this, Tab navigates
  // focus out of the editor, which is hostile to keyboard-only users
  // composing code.
  if (e.key === "Tab" && !e.metaKey && !e.ctrlKey && !e.altKey) {
    e.preventDefault();
    const start = editor.selectionStart;
    const end = editor.selectionEnd;
    editor.setRangeText("\t", start, end, "end");
  }
}

// ── URL share (slice 4) ─────────────────────────────────────────────
//
// Encoding: raw DEFLATE → base64url, written into `location.hash` as
// `#code=<payload>`. Raw DEFLATE (not zlib) drops the 2-byte header
// for a marginally shorter URL; base64url replaces `+/` with `-_` and
// strips trailing `=` padding so the payload survives URL parsers
// without escaping. The fragment is the right home (not a query
// string) because it never reaches the server — the playground is a
// static-hosted page and the source stays in the browser.

async function onShare() {
  try {
    const payload = await compressToBase64Url(editor.value);
    const url = `${location.origin}${location.pathname}#code=${payload}`;
    // Update the address bar in-place; `history.replaceState` avoids
    // pushing a navigation entry per share click.
    history.replaceState(null, "", `#code=${payload}`);
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(url);
      setStatus(`Share URL copied (${url.length} chars).`, "ok");
    } else {
      // Clipboard API not available (insecure context or older browser).
      // The URL is in the address bar; users can copy it manually.
      setStatus(`Share URL in address bar (${url.length} chars).`, null);
    }
  } catch (e) {
    setStatus(`share failed: ${e}`, "err");
  }
}

async function hydrateFromUrl() {
  const hash = location.hash.startsWith("#") ? location.hash.slice(1) : "";
  if (!hash) {
    return { source: DEFAULT_SOURCE, statusText: "Ready.", statusKind: null };
  }
  const params = new URLSearchParams(hash);
  const payload = params.get("code");
  if (!payload) {
    return { source: DEFAULT_SOURCE, statusText: "Ready.", statusKind: null };
  }
  try {
    const source = await decompressFromBase64Url(payload);
    return {
      source,
      statusText: "Loaded shared source from URL.",
      statusKind: null,
    };
  } catch (e) {
    return {
      source: DEFAULT_SOURCE,
      statusText: `Failed to decode shared URL (${e}); loaded default.`,
      statusKind: "err",
    };
  }
}

async function compressToBase64Url(text) {
  const stream = new Blob([text])
    .stream()
    .pipeThrough(new CompressionStream("deflate-raw"));
  const compressed = await new Response(stream).arrayBuffer();
  return base64UrlEncode(new Uint8Array(compressed));
}

async function decompressFromBase64Url(b64u) {
  const bytes = base64UrlDecode(b64u);
  const stream = new Blob([bytes])
    .stream()
    .pipeThrough(new DecompressionStream("deflate-raw"));
  return await new Response(stream).text();
}

function base64UrlEncode(bytes) {
  // Chunk to avoid `Maximum call stack size` on very long inputs.
  let bin = "";
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode.apply(
      null,
      bytes.subarray(i, Math.min(i + CHUNK, bytes.length)),
    );
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function base64UrlDecode(s) {
  let p = s.replace(/-/g, "+").replace(/_/g, "/");
  // Pad back to a multiple of 4.
  while (p.length % 4 !== 0) p += "=";
  const bin = atob(p);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

boot();
