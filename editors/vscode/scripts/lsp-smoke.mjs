// End-to-end check of the exact command the extension runs: spawns
// `<hollowscript> lsp`, speaks LSP to it over standard input and output the
// way vscode-languageclient does, and checks each feature the extension
// relies on against test/fixtures. No VS Code needed.
//
//   node scripts/lsp-smoke.mjs [path/to/hollowscript]
//
// Without an argument it uses HOLLOWSCRIPT_BIN, then the repository's own
// target/release build, then the same search the extension makes.

import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const discovery = require("../src/discovery.js");
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.join(here, "..");
const exe = process.platform === "win32" ? "hollowscript.exe" : "hollowscript";

function findBinary() {
  const explicit = process.argv[2] ?? process.env.HOLLOWSCRIPT_BIN;
  if (explicit) return explicit;
  const built = path.join(root, "..", "..", "target", "release", exe);
  if (existsSync(built)) return built;
  const found = discovery.findServer(undefined, {
    platform: process.platform,
    env: process.env,
    homedir: os.homedir(),
    isExecutableFile: existsSync,
  });
  if (!found.found) {
    console.error(`hollowscript not found. Install it: ${discovery.installCommand(process.platform)}`);
    process.exit(2);
  }
  return found.command;
}

const binary = findBinary();
console.log(`server: ${binary} lsp`);
const child = spawn(binary, ["lsp"], { stdio: ["pipe", "pipe", "inherit"] });

let buffer = Buffer.alloc(0);
let nextId = 1;
/** @type {Map<number, (message: any) => void>} */
const waiting = new Map();
/** @type {((message: any) => boolean)[]} */
const notificationWaiters = [];
/** @type {any[]} */
const notifications = [];

child.stdout.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const headerEnd = buffer.indexOf("\r\n\r\n");
    if (headerEnd < 0) return;
    const header = buffer.subarray(0, headerEnd).toString("ascii");
    const length = Number(/Content-Length: (\d+)/i.exec(header)?.[1]);
    if (buffer.length < headerEnd + 4 + length) return;
    const body = JSON.parse(buffer.subarray(headerEnd + 4, headerEnd + 4 + length).toString("utf8"));
    buffer = buffer.subarray(headerEnd + 4 + length);
    if (body.id !== undefined && waiting.has(body.id)) {
      /** @type {(message: any) => void} */ (waiting.get(body.id))(body);
      waiting.delete(body.id);
    } else if (body.method) {
      notifications.push(body);
      for (let i = notificationWaiters.length - 1; i >= 0; i--) {
        if (notificationWaiters[i](body)) notificationWaiters.splice(i, 1);
      }
    }
  }
});

/** @param {object} message */
function send(message) {
  const json = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...message }), "utf8");
  child.stdin.write(`Content-Length: ${json.length}\r\n\r\n`);
  child.stdin.write(json);
}

/** @param {string} method @param {unknown} params */
function request(method, params) {
  const id = nextId++;
  return /** @type {Promise<any>} */ (
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`${method}: no response in 10s`)), 10000);
      waiting.set(id, (message) => {
        clearTimeout(timer);
        if (message.error) reject(new Error(`${method}: ${JSON.stringify(message.error)}`));
        else resolve(message.result);
      });
      send({ id, method, params });
    })
  );
}

/** @param {(message: any) => boolean} predicate */
function waitForNotification(predicate) {
  const already = notifications.find(predicate);
  if (already) return Promise.resolve(already);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("notification: none in 10s")), 10000);
    notificationWaiters.push((message) => {
      if (!predicate(message)) return false;
      clearTimeout(timer);
      resolve(message);
      return true;
    });
  });
}

let failures = 0;
/** @param {string} name @param {boolean} ok @param {string} detail */
function check(name, ok, detail) {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: ${detail}`);
  if (!ok) failures += 1;
}

/** @param {string} name */
function open(name) {
  const file = path.join(root, "test", "fixtures", name);
  const uri = pathToFileURL(file).href;
  const text = readFileSync(file, "utf8");
  send({
    method: "textDocument/didOpen",
    params: { textDocument: { uri, languageId: "hollowscript", version: 1, text } },
  });
  return { uri, text };
}

try {
  const init = await request("initialize", {
    processId: process.pid,
    clientInfo: { name: "lsp-smoke" },
    rootUri: pathToFileURL(path.join(root, "test", "fixtures")).href,
    capabilities: {},
  });
  const caps = init.capabilities;
  check(
    "initialize",
    Boolean(caps.hoverProvider && caps.documentFormattingProvider && caps.semanticTokensProvider),
    Object.keys(caps).sort().join(", ")
  );
  send({ method: "initialized", params: {} });

  const broken = open("broken.hws");
  const published = await waitForNotification(
    (m) => m.method === "textDocument/publishDiagnostics" && m.params.uri === broken.uri
  );
  const codes = published.params.diagnostics.map((/** @type {any} */ d) => `${d.code} (line ${d.range.start.line + 1})`);
  check("diagnostics", codes.some((/** @type {string} */ c) => c.startsWith("HS0205")), codes.join(", "));

  const sample = open("sample.hws");
  const clean = await waitForNotification(
    (m) => m.method === "textDocument/publishDiagnostics" && m.params.uri === sample.uri
  );
  check("no diagnostics on a valid file", clean.params.diagnostics.length === 0, `${clean.params.diagnostics.length} reported`);

  const edits = await request("textDocument/formatting", {
    textDocument: { uri: sample.uri },
    options: { tabSize: 4, insertSpaces: true },
  });
  const formatted = edits?.[0]?.newText ?? "";
  check("formatting", formatted.includes("constant answer = double(21)"), JSON.stringify(formatted.split("\n")[4]));

  const hover = await request("textDocument/hover", {
    textDocument: { uri: sample.uri },
    position: { line: 4, character: 21 },
  });
  const hoverText = JSON.stringify(hover?.contents ?? null);
  check("hover", hoverText.includes("fn(Int): Int"), hoverText);

  const definition = await request("textDocument/definition", {
    textDocument: { uri: sample.uri },
    position: { line: 4, character: 21 },
  });
  const target = [definition].flat()[0];
  check("definition", target?.range?.start?.line === 0, JSON.stringify(target?.range ?? null));

  const symbols = await request("textDocument/documentSymbol", { textDocument: { uri: sample.uri } });
  const names = (symbols ?? []).map((/** @type {any} */ s) => s.name);
  check("outline", names.includes("double") && names.includes("answer"), names.join(", "));

  const tokens = await request("textDocument/semanticTokens/full", { textDocument: { uri: sample.uri } });
  check("semantic tokens", (tokens?.data?.length ?? 0) > 0, `${(tokens?.data?.length ?? 0) / 5} tokens`);

  await request("shutdown", null);
  send({ method: "exit" });
  const code = await new Promise((resolve) => child.on("exit", resolve));
  check("shutdown and exit", code === 0, `exit code ${code}`);
} catch (error) {
  failures += 1;
  console.error(`FAIL ${error instanceof Error ? error.message : String(error)}`);
  child.kill();
}

console.log(failures === 0 ? "all checks passed" : `${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
