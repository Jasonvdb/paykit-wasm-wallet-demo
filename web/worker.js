// The wallet engine's worker: the only place the WebAssembly module runs.
//
// The page starts this file as a dedicated module worker. The server sends it with its own
// Content Security Policy, the only one that allows 'wasm-unsafe-eval'. It answers the
// page's messages, one at a time, in order:
//
//   request  { id, op, args }      op is load, start, requestPayment, pay, state or reset
//   reply    { id, ok: true, value }
//          | { id, ok: false, error: { name, message } }
//
// and reports, without being asked:
//
//   { event: "violation", directive, blocked }   a Content Security Policy violation here
//
// Only plain data crosses: numbers, strings, and objects and arrays of them. Keys and
// wallet state never leave the worker.

import init, * as engine from "./pkg/paykit_wasm_wallet_demo.js";

const WASM_URL = new URL("./pkg/paykit_wasm_wallet_demo_bg.wasm", import.meta.url);

self.addEventListener("securitypolicyviolation", (event) => {
  self.postMessage({
    event: "violation",
    directive: String(event.effectiveDirective),
    blocked: String(event.blockedURI),
  });
});

// The module's load, started by the first "load".
let loading = null;

// Fetches, compiles and instantiates the module, and says how long that took and how big
// the module is.
async function load() {
  const started = performance.now();
  await init({ module_or_path: WASM_URL });
  const initMs = performance.now() - started;
  return { init_ms: initMs, wasm_bytes: await wasmBytes() };
}

// The module's size, from the browser's record of the fetch, or from a HEAD request where
// the record does not say.
async function wasmBytes() {
  const [entry] = performance.getEntriesByName(WASM_URL.href);
  if (entry?.decodedBodySize > 0) return entry.decodedBodySize;
  const response = await fetch(WASM_URL, { method: "HEAD" });
  const length = Number.parseInt(response.headers.get("content-length") ?? "", 10);
  return Number.isSafeInteger(length) ? length : null;
}

// Each engine function returns its result as a JSON string; the page gets it parsed.
const OPS = {
  load: () => (loading ??= load()),
  start: (storeMode) => engine.start(storeMode),
  requestPayment: (amountSats, memo) => engine.requestPayment(amountSats, memo),
  pay: (requestId) => engine.pay(requestId),
  state: () => engine.state(),
  reset: () => engine.reset(),
};

async function answer({ id, op, args }) {
  try {
    if (!Object.hasOwn(OPS, op) || !Array.isArray(args)) {
      throw new TypeError(`unknown request: ${String(op)}`);
    }
    if (op !== "load") {
      if (loading === null) throw new Error("the module is not loaded");
      await loading;
    }
    const result = await OPS[op](...args);
    const value = typeof result === "string" ? JSON.parse(result) : result;
    self.postMessage({ id, ok: true, value });
  } catch (error) {
    // A RuntimeError here is a WebAssembly trap, such as a Rust panic: the page then
    // replaces this worker, since the instance cannot be trusted any more.
    self.postMessage({
      id,
      ok: false,
      error: { name: String(error?.name ?? "Error"), message: String(error?.message ?? error) },
    });
  }
}

// One call at a time: the engine is not built for overlapping calls, and the page does not
// need them.
let queue = Promise.resolve();
self.addEventListener("message", (event) => {
  queue = queue.then(() => answer(event.data));
});
