// The canonical parser and checker run off the extension host, with a hard
// deadline enforced by its parent. This adds member completion to the released
// v0.1.1 server without requiring a new native CLI or executing any script.
const { parentPort: nullablePort, workerData } = require("node:worker_threads");
const { readFileSync } = require("node:fs");
const path = require("node:path");
// HollowScriptTooling is injected by esbuild from the canonical web wrapper.

const parentPort = /** @type {import("node:worker_threads").MessagePort} */ (nullablePort);
let next = 0;
/** @type {Map<number, (source: string | undefined) => void>} */
const waiting = new Map();
parentPort.on("message", ({ id, source }) => {
  waiting.get(id)?.(source);
  waiting.delete(id);
});
/** @param {string} name @returns {Promise<string | undefined>} */
function readModule(name) {
  return new Promise((resolve) => {
    const id = next++;
    waiting.set(id, resolve);
    parentPort.postMessage({ read: name, id });
  });
}

async function main() {
  const tooling = await HollowScriptTooling.fromBytes(readFileSync(path.join(__dirname, "members.wasm")), { maxMemoryBytes: 128 * 1024 * 1024 });
  /** @type {Record<string, string>} */
  const modules = {};
  const seen = new Set([workerData.entryName]);
  /** @type {[string, string][]} */
  const queue = [[workerData.entryName, workerData.source]];
  while (queue.length) {
    const [name, source] = /** @type {[string, string]} */ (queue.shift());
    const analysis = tooling.analyse(source, name, [], modules);
    let imports;
    try { imports = analysis.documentSymbols().filter((s) => s.kind === "module").map((s) => s.name); }
    finally { analysis.dispose(); }
    for (const raw of imports) {
      if (!raw.startsWith("./") && !raw.startsWith("../")) continue;
      const canonical = path.posix.normalize(path.posix.join(path.posix.dirname(name), raw + ".hws"));
      if (canonical.startsWith("../") || path.posix.isAbsolute(canonical) || canonical.includes("\\") || seen.has(canonical)) continue;
      seen.add(canonical);
      if (seen.size > 128) throw new Error("member completion import limit exceeded");
      const imported = await readModule(canonical);
      if (imported !== undefined) {
        modules[canonical] = imported;
        queue.push([canonical, imported]);
      }
    }
  }
  const analysis = tooling.analyse(workerData.source, workerData.entryName, [], modules);
  try { parentPort.postMessage({ result: workerData.operation === "diagnostics" ? analysis.diagnostics() : analysis.completions(workerData.offset) }); }
  finally { analysis.dispose(); }
}
main().catch((error) => parentPort.postMessage({ error: String(error) }));
