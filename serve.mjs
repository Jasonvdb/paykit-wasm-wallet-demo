// A small static file server for the demo, with no dependencies. It serves web/ on the
// loopback interface only, with the right MIME types and a strict Content Security Policy.
//
//   node serve.mjs                      # http://127.0.0.1:8080/
//   PORT=9000 node serve.mjs            # another port; PORT=0 picks a free one
//   node serve.mjs --no-wasm-eval       # negative control: the worker may not compile wasm
//
// Two policies, because a dedicated worker does not inherit its page's policy: it runs under
// the policy sent with its own script. Only the worker runs WebAssembly, so only the
// worker's policy allows 'wasm-unsafe-eval' (compiling WebAssembly, without allowing
// JavaScript eval). A worker script sent without a policy would run with none at all.
//
// It prints "listening on http://127.0.0.1:<port>/" once it is ready; http://localhost:<port>/
// works too. Both are secure contexts, which the Origin Private File System needs.

import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "web");
const HOST = "127.0.0.1";
const PORT = Number.parseInt(process.env.PORT ?? "8080", 10);
const WASM_EVAL = !process.argv.includes("--no-wasm-eval");

// The page: its own scripts and styles only, nothing inline, no network but its own origin,
// and workers only from its own origin.
const PAGE_POLICY = [
  "default-src 'none'",
  "script-src 'self'",
  "style-src 'self'",
  "img-src 'self'",
  "worker-src 'self'",
  "connect-src 'self'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join("; ");

// The worker: its own scripts, WebAssembly compilation, and fetches to its own origin (the
// glue fetches the .wasm). The simulated network is inside the wasm, so nothing else.
const WORKER_POLICY = [
  "default-src 'none'",
  WASM_EVAL ? "script-src 'self' 'wasm-unsafe-eval'" : "script-src 'self'",
  "connect-src 'self'",
  "base-uri 'none'",
  "form-action 'none'",
  "frame-ancestors 'none'",
].join("; ");

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  // WebAssembly.instantiateStreaming refuses any other type.
  ".wasm": "application/wasm",
  ".svg": "image/svg+xml",
  ".png": "image/png",
};

const server = createServer(async (request, response) => {
  if (request.method !== "GET" && request.method !== "HEAD") {
    response.writeHead(405, { allow: "GET, HEAD" }).end();
    return;
  }
  let path;
  try {
    path = decodeURIComponent(new URL(request.url, "http://localhost").pathname);
  } catch {
    response.writeHead(400).end();
    return;
  }
  const relative = path.endsWith("/") ? `${path}index.html` : path;
  const file = normalize(join(ROOT, relative));
  // Never serve anything outside web/, such as a "/../Cargo.toml" request.
  if (!file.startsWith(ROOT + sep)) {
    response.writeHead(403).end();
    return;
  }
  const type = TYPES[extname(file)];
  if (type === undefined) {
    response.writeHead(404).end();
    return;
  }
  let body;
  try {
    body = await readFile(file);
  } catch {
    response.writeHead(404).end();
    return;
  }
  const isWorker = file === join(ROOT, "worker.js");
  response.writeHead(200, {
    "content-type": type,
    "content-length": body.length,
    "content-security-policy": isWorker ? WORKER_POLICY : PAGE_POLICY,
    "x-content-type-options": "nosniff",
    "referrer-policy": "no-referrer",
    "cache-control": "no-store",
  });
  response.end(request.method === "HEAD" ? undefined : body);
});

server.listen(PORT, HOST, () => {
  const { port } = server.address();
  const note = WASM_EVAL ? "" : " (negative control: worker without 'wasm-unsafe-eval')";
  console.log(`listening on http://127.0.0.1:${port}/${note}`);
});

// Stop cleanly on Ctrl-C or when a test runner ends the process.
for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => {
    server.closeAllConnections();
    server.close(() => process.exit(0));
  });
}
