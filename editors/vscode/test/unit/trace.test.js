"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { createTraceOutput } = require("../../src/trace");

test("tracing follows the extension setting, without global Trace logging", () => {
  let setting = "off";
  /** @type {string[]} */
  const lines = [];
  /** @type {((event: { affectsConfiguration: (key: string) => boolean }) => void) | undefined} */
  let configuration;
  let disposed = false;
  class Emitter {
    /** @type {Set<(level: number) => void>} */
    listeners = new Set();
    /** @param {(level: number) => void} listener */
    event = (listener) => { this.listeners.add(listener); return { dispose: () => this.listeners.delete(listener) }; };
    /** @param {number} value */
    fire(value) { for (const listener of this.listeners) listener(value); }
    dispose() { this.listeners.clear(); }
  }
  const host = /** @type {typeof import("vscode")} */ (/** @type {unknown} */ ({
    LogLevel: { Info: 3, Trace: 1 }, EventEmitter: Emitter,
    window: { createOutputChannel: () => ({ name: "Trace", appendLine: (/** @type {string} */ line) => lines.push(line), dispose: () => { disposed = true; } }) },
    workspace: {
      getConfiguration: () => ({ get: () => setting }),
      onDidChangeConfiguration: (/** @type {typeof configuration} */ listener) => { configuration = listener; return { dispose: () => { configuration = undefined; } }; },
    },
  }));
  const channel = createTraceOutput(host, "Trace");
  /** @type {number[]} */
  const levels = [];
  channel.onDidChangeLogLevel((value) => levels.push(value));
  assert.equal(channel.logLevel, host.LogLevel.Info);
  channel.trace("private source");
  assert.deepEqual(lines, []);
  setting = "verbose";
  configuration?.({ affectsConfiguration: (key) => key === "hollowscript.trace.server" });
  assert.equal(channel.logLevel, host.LogLevel.Trace);
  channel.trace("Sending %s", "textDocument/hover");
  assert.deepEqual(lines, ["Sending textDocument/hover"]);
  setting = "off";
  configuration?.({ affectsConfiguration: (key) => key === "hollowscript.trace.server" });
  channel.trace("private source");
  assert.equal(lines.length, 1);
  assert.deepEqual(levels, [host.LogLevel.Trace, host.LogLevel.Info]);
  channel.dispose();
  assert.equal(configuration, undefined);
  assert.equal(disposed, true);
});
