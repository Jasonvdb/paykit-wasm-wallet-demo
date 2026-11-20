// The page: it shows the two wallets and talks to the wallet engine in worker.js with
// plain-data messages ({ id, op, args } out, { id, ok, value | error } back). No WebAssembly
// runs here.
//
// If the worker reports an error event, or a call fails with a RuntimeError (a WebAssembly
// trap, which is what a Rust panic becomes), the engine is treated as stopped: its
// WebAssembly instance cannot be trusted any more, and an async call that panicked never
// answers. The page terminates the worker, fails its pending calls, says "Wallet engine
// stopped", and offers "Restart engine", which starts a new worker.
//
// ?store=memory keeps both wallets' state in memory instead of the sealed store.

const STORE_MODE = new URLSearchParams(location.search).get("store") === "memory" ? "memory" : "sealed";
const CALL_TIMEOUT_MS = 120_000;

const $ = (id) => document.getElementById(id);

// The current engine: { worker, pending: Map(id -> { resolve, reject, timer }), stopped }.
let engine = null;
let nextId = 1;
// Whether a call is in progress, so that the buttons wait for it.
let busy = false;

document.addEventListener("securitypolicyviolation", (event) => {
  log(`Content Security Policy violation on the page: ${event.effectiveDirective} ${event.blockedURI}`, true);
});

// Starts a worker and wires up its replies and its failures.
function spawn() {
  const worker = new Worker("worker.js", { type: "module" });
  const current = { worker, pending: new Map(), stopped: false };
  worker.addEventListener("message", (event) => {
    const message = event.data;
    if (message?.event === "violation") {
      log(`Content Security Policy violation in the worker: ${message.directive} ${message.blocked}`, true);
      return;
    }
    const call = current.pending.get(message?.id);
    if (call === undefined) return;
    current.pending.delete(message.id);
    clearTimeout(call.timer);
    if (message.ok) {
      call.resolve(message.value);
    } else {
      const error = new Error(message.error.message);
      error.name = message.error.name;
      call.reject(error);
      if (error.name === "RuntimeError") stop(current, `${error.name}: ${error.message}`);
    }
  });
  worker.addEventListener("error", (event) => {
    event.preventDefault();
    stop(current, event.message || "the worker failed to load or crashed");
  });
  worker.addEventListener("messageerror", () => stop(current, "a message from the worker could not be read"));
  return current;
}

// Calls `op` on the engine and resolves with its value.
function call(op, ...args) {
  const current = engine;
  if (current === null || current.stopped) return Promise.reject(new Error("the engine is stopped"));
  const id = nextId++;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      current.pending.delete(id);
      reject(new Error(`no answer to ${op} in ${CALL_TIMEOUT_MS / 1000} s`));
    }, CALL_TIMEOUT_MS);
    current.pending.set(id, { resolve, reject, timer });
    current.worker.postMessage({ id, op, args });
  });
}

// Ends `current` for good: terminates its worker and fails its pending calls.
function stop(current, reason) {
  if (current.stopped) return;
  current.stopped = true;
  current.worker.terminate();
  for (const pending of current.pending.values()) {
    clearTimeout(pending.timer);
    pending.reject(new Error("the engine stopped"));
  }
  current.pending.clear();
  if (current === engine) {
    setEngineState("stopped");
    $("stopped-reason").textContent = reason;
    $("stopped").hidden = false;
    log(`engine stopped: ${reason}`, true);
  }
}

// Starts a new engine: a worker, the module's load, and a new world.
async function boot() {
  if (engine !== null) stop(engine, "restarted");
  $("stopped").hidden = true;
  setEngineState("loading");
  const current = (engine = spawn());
  try {
    const loaded = await timedCall("load the WebAssembly module", "load");
    $("wasm-size").textContent = loaded.wasm_bytes === null ? "unknown" : formatBytes(loaded.wasm_bytes);
    $("load-time").textContent = `${loaded.init_ms.toFixed(0)} ms`;
    log(`module fetched, compiled and instantiated in ${loaded.init_ms.toFixed(1)} ms`);
    showStarted(await timedCall(`start (${STORE_MODE} store)`, "start", STORE_MODE));
    await refresh();
    setEngineState("ready");
  } catch (error) {
    if (current === engine) stop(current, `${error.name}: ${error.message}`);
  }
}

// Calls `op`, logging the engine's own steps and the whole call's time.
async function timedCall(label, op, ...args) {
  const started = performance.now();
  try {
    const value = await call(op, ...args);
    for (const step of value?.steps ?? []) log(`  ${step.step}: ${step.ms.toFixed(1)} ms`);
    log(`${label}: ${(performance.now() - started).toFixed(1)} ms`);
    return value;
  } catch (error) {
    log(`${label} failed: ${error.message}`, true);
    throw error;
  }
}

// Shows which store is in use, and what the previous session left.
function showStarted(started) {
  $("store-kind").textContent = started.store.kind;
  $("store-kind").dataset.kind = started.store.kind;
  $("store-reason").textContent = `Store: ${started.store.reason}.`;
  // Not an error line: the demo goes on without it, and where the sealed store cannot be
  // opened at all (no Origin Private File System), the store line above already says why.
  if (started.previous_error) log(`note: nothing read back from a previous session (${started.previous_error})`);
  const previous = started.previous;
  $("previous").hidden = previous === null;
  if (previous !== null) {
    fill($("previous-receipts"), previous.alice_receipts, receiptRow, "Alice's wallet held no Payment Receipt");
    log(
      `read back from the sealed store: ${previous.alice_receipts.length} Payment Receipt(s) in Alice's wallet, ` +
        `${previous.bob_receipted} receipted payment(s) in Bob's`,
    );
  }
}

// Reads the wallets and redraws them.
async function refresh() {
  const state = await call("state");
  $("alice-balance").textContent = state.alice_sats.toLocaleString("en");
  $("bob-balance").textContent = state.bob_sats.toLocaleString("en");
  fill($("open-requests"), state.open_requests, openRequestRow, "No open Payment Request");
  fill($("alice-receipts"), state.alice_receipts, receiptRow, "No Payment Receipt yet");
  fill($("bob-receipts"), state.bob_receipts, receiptRow, "No Payment Receipt yet");
}

// Runs `action` with the buttons disabled, then redraws.
async function run(action) {
  if (busy) return;
  busy = true;
  setButtons();
  try {
    await action();
    await refresh();
  } catch {
    // Already logged by timedCall; a stopped engine is shown by stop().
  } finally {
    busy = false;
    setButtons();
  }
}

$("request-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const amount = Number($("amount").value);
  const memo = $("memo").value;
  run(() => timedCall(`Bob requests ${amount} sats`, "requestPayment", amount, memo));
});

$("reset").addEventListener("click", () => {
  run(async () => {
    $("previous").hidden = true;
    showStarted(await timedCall("reset the demo", "reset"));
  });
});

$("restart").addEventListener("click", () => boot());

// --- Drawing. Text goes in with textContent only, never as HTML: a memo is user input.

function openRequestRow(request) {
  const li = document.createElement("li");
  li.dataset.requestId = request.request_id;
  li.append(span("amount", `${request.amount_sats.toLocaleString("en")} sats`), span("memo", request.memo));
  const button = document.createElement("button");
  button.type = "button";
  button.textContent = "Pay";
  button.disabled = busy || engineState() !== "ready";
  button.addEventListener("click", () =>
    run(() => timedCall(`Alice pays ${request.amount_sats} sats`, "pay", request.request_id)),
  );
  li.append(button, span("id", `request ${short(request.request_id)}`));
  return li;
}

function receiptRow(receipt) {
  const li = document.createElement("li");
  li.dataset.receiptId = receipt.receipt_id;
  const amount = receipt.amount_sats === null ? "? sats" : `${receipt.amount_sats.toLocaleString("en")} sats`;
  const time = receipt.time_ms === null ? "" : new Date(receipt.time_ms).toLocaleTimeString();
  li.append(span("amount", amount), span("memo", receipt.memo), span("time", time));
  li.append(span("id", `Payment Receipt ${receipt.receipt_id}`));
  if (receipt.proof_id) li.append(span("id", `Payment Proof ${receipt.proof_id}`));
  return li;
}

function fill(list, items, row, emptyText) {
  list.replaceChildren(...items.map(row));
  if (items.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = emptyText;
    list.append(li);
  }
}

function span(className, text) {
  const element = document.createElement("span");
  element.className = className;
  element.textContent = text;
  return element;
}

function short(hex) {
  return `${hex.slice(0, 12)}...`;
}

function log(text, isError = false) {
  const li = document.createElement("li");
  li.textContent = text;
  if (isError) li.className = "error";
  $("log").append(li);
  li.scrollIntoView({ block: "nearest" });
}

function engineState() {
  return $("engine-state").dataset.state;
}

function setEngineState(state) {
  $("engine-state").dataset.state = state;
  $("engine-state").textContent = state;
  setButtons();
}

function setButtons() {
  const enabled = !busy && engineState() === "ready";
  $("request-button").disabled = !enabled;
  $("reset").disabled = !enabled;
  for (const button of document.querySelectorAll("#open-requests button")) button.disabled = !enabled;
}

function formatBytes(bytes) {
  return `${(bytes / (1024 * 1024)).toFixed(2)} MiB`;
}

boot();
