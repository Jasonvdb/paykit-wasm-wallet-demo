// Smoke test: drives the demo page in one browser engine with Playwright.
//
//   node test/smoke.mjs chromium|firefox|webkit [--store=sealed|memory] [--negative]
//
// It builds nothing: run ./build.sh first. It starts serve.mjs on a free loopback port and:
//
// 1. checks the two Content Security Policy headers: the worker's allows
//    'wasm-unsafe-eval', the page's does not;
// 2. opens the page and waits for the engine to be ready, then checks which store runs;
// 3. makes Bob request 1,000 sats and Alice pay it, then checks that the mock balances moved
//    (Alice 100,000 -> 99,000, Bob 0 -> 1,000), that the request is no longer open, and that
//    the same Payment Receipt shows on both sides;
// 4. reloads the page. With the sealed store, the engine must start again and show that
//    Payment Receipt under "from the previous session", read back from the sealed store;
//    with the memory store, it must start again with nothing to read back;
// 5. clicks "Reset demo" and checks that a new world starts on the same kind of store;
// 6. fails on any console error, page error, Content Security Policy violation, or error
//    line in the page's log.
//
// --negative runs the negative control instead: the server sends the worker's policy
// without 'wasm-unsafe-eval', and the engine must fail to start with a CompileError, which
// the page must show.
//
// The store mode is --store, or SMOKE_STORE, or by default "sealed", except for WebKit on
// macOS: there, Playwright's WebKit asks for Keychain access in a system dialog the first
// time a page stores a WebCrypto key in IndexedDB, which the sealed store does, and the test
// would hang on it. So WebKit on macOS defaults to "memory", with a warning. On Linux,
// Playwright's WebKit has no Origin Private File System: the page falls back to memory, and
// this test expects that fallback.

import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { chromium, firefox, webkit } from "playwright";

const ENGINES = { chromium, firefox, webkit };
const WAIT_MS = 120_000;

const args = process.argv.slice(2);
const engineName = args.find((arg) => !arg.startsWith("--"));
const negative = args.includes("--negative");
const storeArg = args.find((arg) => arg.startsWith("--store="))?.slice("--store=".length);

if (!Object.hasOwn(ENGINES, engineName ?? "")) {
  console.error("usage: node test/smoke.mjs chromium|firefox|webkit [--store=sealed|memory] [--negative]");
  process.exit(2);
}

let storeMode = storeArg ?? process.env.SMOKE_STORE;
if (storeMode === undefined) {
  storeMode = "sealed";
  if (engineName === "webkit" && process.platform === "darwin") {
    storeMode = "memory";
    console.warn(
      "WARNING: WebKit on macOS runs with ?store=memory: storing a WebCrypto key in IndexedDB " +
        "makes macOS ask for Keychain access in a dialog, and the test would wait on it. " +
        "The sealed store is NOT tested in this run.",
    );
  }
}
if (storeMode !== "sealed" && storeMode !== "memory") {
  console.error(`unknown store mode: ${storeMode}`);
  process.exit(2);
}
// What the page should report: the sealed store, except where the engine has no Origin
// Private File System and the page falls back to memory.
const noOpfs = engineName === "webkit" && process.platform === "linux";
const expectedStore = storeMode === "sealed" && !noOpfs ? "sealed-opfs" : "memory";

function check(condition, what) {
  if (!condition) throw new Error(`check failed: ${what}`);
  console.log(`ok - ${what}`);
}

// Starts serve.mjs on a free port and resolves with its base URL and process.
function startServer() {
  const serve = fileURLToPath(new URL("../serve.mjs", import.meta.url));
  const child = spawn(process.execPath, [serve, ...(negative ? ["--no-wasm-eval"] : [])], {
    env: { ...process.env, PORT: "0" },
    stdio: ["ignore", "pipe", "inherit"],
  });
  return new Promise((resolve, reject) => {
    let output = "";
    child.stdout.on("data", (chunk) => {
      output += chunk;
      const match = /listening on (http:\/\/127\.0\.0\.1:\d+\/)/.exec(output);
      if (match) resolve({ base: match[1], child });
    });
    child.on("exit", (code) => reject(new Error(`serve.mjs exited with ${code}`)));
  });
}

async function checkHeaders(base) {
  const page = (await fetch(base)).headers.get("content-security-policy") ?? "";
  const worker = (await fetch(new URL("worker.js", base))).headers.get("content-security-policy") ?? "";
  check(page.includes("default-src 'none'") && !page.includes("wasm-unsafe-eval"), "the page's policy is strict and has no 'wasm-unsafe-eval'");
  check(worker.includes("default-src 'none'"), "the worker script has a policy of its own");
  check(worker.includes("'wasm-unsafe-eval'") === !negative, negative ? "the worker's policy lacks 'wasm-unsafe-eval' (negative control)" : "the worker's policy allows 'wasm-unsafe-eval'");
  const wasm = await fetch(new URL("pkg/paykit_wasm_wallet_demo_bg.wasm", base), { method: "HEAD" });
  check(wasm.headers.get("content-type") === "application/wasm", "the .wasm is served as application/wasm");
}

const text = (page, selector) => page.locator(selector).textContent();
const number = async (page, selector) => Number((await text(page, selector)).replaceAll(",", ""));

async function waitForEngine(page) {
  await page.waitForSelector('#engine-state:is([data-state="ready"], [data-state="stopped"])', { timeout: WAIT_MS });
  const state = await page.locator("#engine-state").getAttribute("data-state");
  if (state !== "ready") throw new Error(`the engine stopped: ${await text(page, "#stopped-reason")}`);
}

// The log line "<label>: <ms> ms" whose label starts with `prefix`, as milliseconds.
async function loggedMs(page, prefix) {
  const lines = await page.locator("#log li").allTextContents();
  const line = lines.find((entry) => entry.trim().startsWith(prefix));
  const match = line && /([\d.]+) ms$/.exec(line);
  return match ? Number(match[1]) : null;
}

async function main() {
  const { base, child } = await startServer();
  const browser = await ENGINES[engineName].launch();
  const problems = [];
  try {
    await checkHeaders(base);
    const context = await browser.newContext();
    const page = await context.newPage();
    page.on("console", (message) => {
      if (message.type() === "error") problems.push(`console error: ${message.text()}`);
      else if (/content.security.policy/i.test(message.text())) problems.push(`policy message: ${message.text()}`);
    });
    page.on("pageerror", (error) => problems.push(`page error: ${error.message}`));
    const url = storeMode === "memory" ? `${base}?store=memory` : base;

    if (negative) {
      await page.goto(url);
      await page.waitForSelector('#engine-state[data-state="stopped"]', { timeout: WAIT_MS });
      const reason = await text(page, "#stopped-reason");
      check(reason.includes("CompileError"), `the engine fails to start with a CompileError, and the page says so ("${reason}")`);
      check(await page.locator("#restart").isVisible(), 'the page offers "Restart engine"');
      console.log(`PASS ${engineName} negative control`);
      return;
    }

    await page.goto(url);
    await waitForEngine(page);
    const store = await page.locator("#store-kind").getAttribute("data-kind");
    console.log(`store: ${store} (${await text(page, "#store-reason")})`);
    check(store === expectedStore, `the page runs the ${expectedStore} store`);
    check((await number(page, "#alice-balance")) === 100_000, "Alice starts with 100,000 sats");
    check((await number(page, "#bob-balance")) === 0, "Bob starts with 0 sats");

    await page.fill("#amount", "1000");
    await page.fill("#memo", "smoke test");
    await page.click("#request-button");
    const request = page.locator("#open-requests li[data-request-id]");
    await request.first().waitFor({ timeout: WAIT_MS });
    check((await request.count()) === 1, "Alice's wallet holds the open Payment Request");
    check((await request.locator(".amount").textContent()) === "1,000 sats", "the request is for 1,000 sats");
    check((await request.locator(".memo").textContent()) === "smoke test", "the request carries the memo as its Payment Reference");

    await request.getByRole("button", { name: "Pay" }).click();
    const aliceReceipt = page.locator("#alice-receipts li[data-receipt-id]");
    const bobReceipt = page.locator("#bob-receipts li[data-receipt-id]");
    await aliceReceipt.first().waitFor({ timeout: WAIT_MS });
    await bobReceipt.first().waitFor({ timeout: WAIT_MS });
    await page.waitForSelector("#request-button:not([disabled])", { timeout: WAIT_MS });
    check((await number(page, "#alice-balance")) === 99_000, "Alice's mock balance moved to 99,000 sats");
    check((await number(page, "#bob-balance")) === 1_000, "Bob's mock balance moved to 1,000 sats");
    check((await page.locator("#open-requests li[data-request-id]").count()) === 0, "the request is no longer open");
    const receiptId = await aliceReceipt.getAttribute("data-receipt-id");
    check(receiptId.length > 0, `Alice's wallet stored the Payment Receipt (${receiptId})`);
    check((await bobReceipt.getAttribute("data-receipt-id")) === receiptId, "Bob's wallet shows the same Payment Receipt");
    check((await aliceReceipt.locator(".amount").textContent()) === "1,000 sats", "the Payment Receipt is for 1,000 sats");

    const timings = {
      load: await loggedMs(page, "module fetched"),
      start: await loggedMs(page, "start ("),
      request: await loggedMs(page, "Bob requests 1000 sats:"),
      pay: await loggedMs(page, "Alice pays 1000 sats:"),
    };

    await page.reload();
    await waitForEngine(page);
    check((await number(page, "#alice-balance")) === 100_000, "after the reload, a new world starts");
    if (expectedStore === "sealed-opfs") {
      check(await page.locator("#previous").isVisible(), "after the reload, the previous session is shown");
      const previous = page.locator(`#previous-receipts li[data-receipt-id="${receiptId}"]`);
      check((await previous.count()) === 1, "the Payment Receipt was read back from the sealed store");
    } else {
      check(await page.locator("#previous").isHidden(), "after the reload, there is nothing to read back (memory store)");
    }

    // "Reset demo" ends this world (closing its stores) and starts another in the same tab.
    await page.click("#reset");
    await page.waitForSelector("#reset:not([disabled])", { timeout: WAIT_MS });
    check((await page.locator("#engine-state").getAttribute("data-state")) === "ready", "after Reset demo, the engine is ready again");
    check((await page.locator("#store-kind").getAttribute("data-kind")) === expectedStore, "after Reset demo, the same store kind runs");

    const errorLines = await page.locator("#log li.error").allTextContents();
    problems.push(...errorLines.map((line) => `page log: ${line}`));
    check(problems.length === 0, `no console error, page error or policy violation${problems.length ? `:\n  ${problems.join("\n  ")}` : ""}`);
    console.log(`timings (ms): ${JSON.stringify(timings)}`);
    console.log(`PASS ${engineName} store=${store} ${browser.version()}`);
  } finally {
    await browser.close();
    child.kill("SIGTERM");
  }
}

main().catch((error) => {
  console.error(`FAIL ${engineName}: ${error.message}`);
  process.exit(1);
});
