// run_browser.mjs — drive the wasm_browser build under node, with a mock
// DOM standing in for the browser. Proves the client leg of the SSR
// example hydrates: `hydrate(n)` runs the SHARED component through the
// `DomSink` provider, whose host fns mutate our mock instead of a real
// `document`.
//
//   karac build ssr_counter.kara --target=wasm_browser   # emits .wasm + .js
//   node run_browser.mjs                                  # -> HYDRATED ...
//
// The real browser equivalent (a live `document`) is index.html.

import { instantiate } from "./ssr_counter.js";

// Stand-in for the SSR'd DOM. In the browser these slots are <output> and
// <span> elements; the host fns below would set their textContent.
const dom = { count: null, parity: null };

const handle = await instantiate({
  dom_set_count(value /* bigint */, _ctx) {
    dom.count = Number(value);
  },
  dom_set_parity(value /* bigint */, _ctx) {
    dom.parity = Number(value) === 0 ? "even" : "odd";
  },
});

// Hydrate with a fresh count, exactly as a client-side event handler would.
const returned = handle.exports.hydrate(7n);
if (returned !== 7n) throw new Error("hydrate returned " + returned);
if (dom.count !== 7 || dom.parity !== "odd") {
  throw new Error("DOM not hydrated: " + JSON.stringify(dom));
}

// Re-render (e.g. an "increment" click) updates the same DOM in place.
handle.exports.hydrate(10n);
if (dom.count !== 10 || dom.parity !== "even") {
  throw new Error("re-hydration failed: " + JSON.stringify(dom));
}

console.log("HYDRATED", JSON.stringify(dom));
