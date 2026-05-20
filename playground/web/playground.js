// Browser playground shell — tracker line 703.
//
// Loads the wasm-bindgen module produced by `wasm-pack build --target web
// --out-dir web/pkg` (see playground/Cargo.toml + playground/src/lib.rs),
// then wires up: textarea editor ↔ Run button ↔ wasm `run(source)` ↔
// stdout / diagnostics rendering. Diagnostics are clickable and move the
// textarea selection to the offset they report.
//
// Slice 3 ships the editor / run / output / diagnostics chain.
// Slice 4 layers the URL-share encode/decode pipeline on top of this.

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

editor.value = DEFAULT_SOURCE;

async function boot() {
  try {
    await init();
  } catch (e) {
    setStatus(`failed to load wasm module: ${e}`, "err");
    return;
  }
  runBtn.disabled = false;
  runBtn.textContent = "Run";
  // Share button stays disabled until slice 4 lands; the button is
  // visible so the UX is the final shape, just inert in slice 3.
  setStatus("Ready.", null);
  runBtn.addEventListener("click", onRun);
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

boot();
