"use strict";
const { Worker } = require("node:worker_threads");
const fs = require("node:fs/promises");
const { constants } = require("node:fs");
const path = require("node:path");

const MAX_SOURCE = 1024 * 1024;
const MAX_TOTAL = 8 * 1024 * 1024;

/** Read only regular, singly linked files below the entry module's root.
 * @param {string} root @param {string} name @returns {Promise<string | undefined>}
 */
async function readModule(root, name) {
  if (!name.endsWith(".hws") || name.includes("\\") || name.includes("\0") || path.isAbsolute(name)) return;
  const parts = name.split("/");
  if (parts.some((p) => p === ".." || p === "." || p === "")) return;
  let file = root;
  try {
    for (const part of parts) {
      file = path.join(file, part);
      if ((await fs.lstat(file)).isSymbolicLink()) return;
    }
    if (!(await fs.lstat(file)).isFile()) return;
    const handle = await fs.open(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0) | (constants.O_NONBLOCK ?? 0));
    try {
      const stat = await handle.stat();
      if (!stat.isFile() || stat.nlink !== 1 || stat.size > MAX_SOURCE) return;
      // Recheck the resolved path after opening to reject path replacement.
      const real = await fs.realpath(file);
      if (real !== file) return;
      const buffer = Buffer.alloc(MAX_SOURCE + 1);
      const { bytesRead } = await handle.read(buffer, 0, buffer.length, 0);
      if (bytesRead > MAX_SOURCE) return;
      return new TextDecoder("utf-8", { fatal: true }).decode(buffer.subarray(0, bytesRead));
    } finally { await handle.close(); }
  } catch { return; }
}

class MemberService {
  /** @param {string} workerPath */
  constructor(workerPath) {
    this.workerPath = workerPath;
    /** @type {Set<() => void>} */
    this.cancellations = new Set();
    this.disposed = false;
  }
  dispose() { this.disposed = true; for (const cancel of this.cancellations) cancel(); this.cancellations.clear(); }

  /** @param {string} file @param {string} source @param {Map<string, string>} openDocuments
   * @param {import("vscode").CancellationToken} token
   * @returns {Promise<{ code: string, message: string, start: number, end: number }[]>}
   */
  async diagnostics(file, source, openDocuments, token) {
    return /** @type {{ code: string, message: string, start: number, end: number }[]} */ (await this.request(file, source, 0, openDocuments, token, "diagnostics"));
  }

  /** @param {string} file @param {string} source @param {number} offset
   * @param {Map<string, string>} openDocuments @param {import("vscode").CancellationToken} token
   * @returns {Promise<{ label: string, kind: string, detail: string }[]>}
   */
  async complete(file, source, offset, openDocuments, token) {
    return /** @type {{ label: string, kind: string, detail: string }[]} */ (await this.request(file, source, offset, openDocuments, token, "completion"));
  }

  /** @param {string} file @param {string} source @param {number} offset
   * @param {Map<string, string>} openDocuments @param {import("vscode").CancellationToken} token
   * @param {string} operation @returns {Promise<unknown[]>}
   */
  async request(file, source, offset, openDocuments, token, operation) {
    if (this.disposed || token.isCancellationRequested || Buffer.byteLength(source) > MAX_SOURCE) return [];
    const root = await fs.realpath(path.dirname(file));
    if (this.disposed || token.isCancellationRequested) return [];
    return new Promise((resolve) => {
      // Keep automatic and explicit editor requests independent, while
      // bounding concurrent analyses per service.
      if (this.cancellations.size >= 2) this.cancellations.values().next().value?.();
      const worker = new Worker(this.workerPath, {
        workerData: { source, offset, operation, entryName: path.basename(file) },
        resourceLimits: { maxOldGenerationSizeMb: 128, stackSizeMb: 4 },
      });
      let done = false;
      let total = Buffer.byteLength(source);
      const finish = (/** @type {unknown[]} */ result) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        subscription.dispose();
        this.cancellations.delete(cancel);
        void worker.terminate();
        resolve(result);
      };
      const cancel = () => finish([]);
      const timer = setTimeout(cancel, 3000);
      let subscription = { dispose() {} };
      this.cancellations.add(cancel);
      subscription = token.onCancellationRequested(cancel);
      worker.on("error", cancel);
      worker.on("exit", cancel);
      worker.on("message", async (message) => {
        if (done) return;
        if (message.result) return finish(message.result);
        if (message.error) return cancel();
        if (typeof message.read !== "string") return;
        let imported = await readModule(root, message.read);
        const open = openDocuments.get(path.join(root, message.read)) ?? openDocuments.get(path.join(path.dirname(file), message.read));
        if (imported !== undefined && open !== undefined) imported = open;
        if (done) return;
        if (imported !== undefined && Buffer.byteLength(imported) > MAX_SOURCE) imported = undefined;
        if (imported !== undefined) total += Buffer.byteLength(imported);
        if (total > MAX_TOTAL) return cancel();
        worker.postMessage({ id: message.id, source: imported });
      });
    });
  }
}
module.exports = { MemberService, readModule };
