"use strict";
const { format } = require("node:util");

/** Keep the documented trace setting independent of VS Code's global log
 * level. Language client 11 otherwise silently disables tracing at Info and
 * enables it at Trace even when trace.server is off.
 * @param {typeof import("vscode")} vscode
 * @param {string} name
 * @returns {import("vscode").LogOutputChannel}
 */
function createTraceOutput(vscode, name) {
  const channel = vscode.window.createOutputChannel(name);
  const changed = new vscode.EventEmitter();
  const level = () => vscode.workspace.getConfiguration("hollowscript").get("trace.server", "off") === "off"
    ? vscode.LogLevel.Info : vscode.LogLevel.Trace;
  const subscription = vscode.workspace.onDidChangeConfiguration((event) => {
    if (event.affectsConfiguration("hollowscript.trace.server")) changed.fire(level());
  });
  /** @param {string | Error} message @param {unknown[]} args */
  const write = (message, ...args) => {
    if (level() === vscode.LogLevel.Trace) channel.appendLine(format(message, ...args));
  };
  return {
    name: channel.name,
    get logLevel() { return level(); },
    onDidChangeLogLevel: changed.event,
    trace: write, debug: write, info: write, warn: write, error: write,
    append: (value) => channel.append(value),
    appendLine: (value) => channel.appendLine(value),
    replace: (value) => channel.replace(value),
    clear: () => channel.clear(), hide: () => channel.hide(),
    /** @param {import("vscode").ViewColumn | boolean} [columnOrFocus] @param {boolean} [preserveFocus] */
    show(columnOrFocus, preserveFocus) {
      if (typeof columnOrFocus === "number") channel.show(columnOrFocus, preserveFocus);
      else channel.show(columnOrFocus);
    },
    dispose() { subscription.dispose(); changed.dispose(); channel.dispose(); },
  };
}
module.exports = { createTraceOutput };
