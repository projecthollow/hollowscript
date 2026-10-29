// Finds the `hollowscript` command this extension starts as `hollowscript lsp`.
//
// Pure Node: nothing here imports `vscode`, so the unit tests in test/unit
// exercise it directly with a fake file system and environment.

"use strict";

const path = require("node:path");

/** The command an installed release puts on disk. */
const COMMAND = "hollowscript";

/** The install one-liners, as the project's own install scripts document them. */
const INSTALL_COMMANDS = {
  unix: "curl -fsSL https://get.hollowscript.org/install.sh | sh",
  windows: "irm https://get.hollowscript.org/install.ps1 | iex",
};

const INSTALL_GUIDE_URL = "https://hollowscript.org";

/**
 * @typedef {object} DiscoveryHost
 * @property {NodeJS.Platform} platform
 * @property {Record<string, string | undefined>} env
 * @property {string} homedir
 * @property {(file: string) => boolean} isExecutableFile
 */

/**
 * @typedef {object} Found
 * @property {true} found
 * @property {string} command  the absolute path to run
 * @property {"setting" | "PATH" | "install directory"} source
 * @property {string[]} searched
 *
 * @typedef {object} NotFound
 * @property {false} found
 * @property {string[]} searched  every place looked, in order, for the log
 * @property {string | undefined} configured  the setting's value, when it was set but wrong
 */

/**
 * The install command for a platform.
 * @param {NodeJS.Platform} platform
 */
function installCommand(platform) {
  return platform === "win32" ? INSTALL_COMMANDS.windows : INSTALL_COMMANDS.unix;
}

/**
 * The file names `name` may have on disk. On Windows that is every
 * extension in PATHEXT (as the shell would try them), `.exe` first.
 * @param {string} name
 * @param {DiscoveryHost} host
 */
function candidateNames(name, host) {
  if (host.platform !== "win32" || path.win32.extname(name) !== "") {
    return [name];
  }
  const pathext = (host.env.PATHEXT || ".COM;.EXE;.BAT;.CMD")
    .split(";")
    .map((ext) => ext.trim().toLowerCase())
    .filter((ext) => ext !== "");
  const ordered = [".exe", ...pathext.filter((ext) => ext !== ".exe")];
  return ordered.map((ext) => name + ext);
}

/**
 * The directories of PATH, in order, without empty entries.
 * @param {DiscoveryHost} host
 */
function pathDirectories(host) {
  const pathApi = host.platform === "win32" ? path.win32 : path.posix;
  // Windows environment variables are case-insensitive; Node exposes PATH as
  // `Path` there as often as not.
  const value =
    host.env.PATH ??
    Object.entries(host.env).find(([key]) => key.toUpperCase() === "PATH")?.[1] ??
    "";
  return value
    .split(pathApi.delimiter)
    .map((dir) => dir.replace(/^"(.*)"$/, "$1"))
    .filter((dir) => dir !== "");
}

/**
 * The directories an installer puts the command in, as scripts/install.sh
 * and scripts/install.ps1 choose them. On Windows, install.ps1 uses
 * HOLLOWSCRIPT_INSTALL_DIR if set, otherwise %LOCALAPPDATA%\hollowscript\bin.
 * On macOS and Linux, install.sh uses $HOLLOWSCRIPT_HOME/bin if
 * HOLLOWSCRIPT_HOME is set, otherwise ~/.hollowscript/bin. ~/.hollowscript/bin
 * is searched last on every platform.
 * @param {DiscoveryHost} host
 */
function installDirectories(host) {
  const pathApi = host.platform === "win32" ? path.win32 : path.posix;
  /** @type {string[]} */
  const dirs = [];
  if (host.platform === "win32") {
    if (host.env.HOLLOWSCRIPT_INSTALL_DIR) {
      dirs.push(host.env.HOLLOWSCRIPT_INSTALL_DIR);
    }
    const localAppData =
      host.env.LOCALAPPDATA || pathApi.join(host.homedir, "AppData", "Local");
    dirs.push(pathApi.join(localAppData, "hollowscript", "bin"));
  } else if (host.env.HOLLOWSCRIPT_HOME) {
    dirs.push(pathApi.join(host.env.HOLLOWSCRIPT_HOME, "bin"));
  }
  dirs.push(pathApi.join(host.homedir, ".hollowscript", "bin"));
  return [...new Set(dirs)];
}

/**
 * Expands a leading `~` in a setting to the home directory.
 * @param {string} value
 * @param {DiscoveryHost} host
 */
function expandHome(value, host) {
  const pathApi = host.platform === "win32" ? path.win32 : path.posix;
  if (value === "~") return host.homedir;
  if (value.startsWith("~/") || (host.platform === "win32" && value.startsWith("~\\"))) {
    return pathApi.join(host.homedir, value.slice(2));
  }
  return value;
}

/**
 * Finds the `hollowscript` command.
 *
 * 1. `hollowscript.executablePath`, when set. A bare name (no directory) is
 *    looked up on PATH; anything else must be an existing file. A setting
 *    that is set but wrong is reported as not found rather than silently
 *    falling back, so the user sees their own mistake.
 * 2. `hollowscript` on PATH.
 * 3. The installer's default directory.
 *
 * @param {string | undefined} configured
 * @param {DiscoveryHost} host
 * @returns {Found | NotFound}
 */
function findServer(configured, host) {
  const pathApi = host.platform === "win32" ? path.win32 : path.posix;
  /** @type {string[]} */
  const searched = [];

  /** @param {string} dir @param {string} name */
  const lookIn = (dir, name) => {
    for (const candidate of candidateNames(pathApi.join(dir, name), host)) {
      searched.push(candidate);
      if (host.isExecutableFile(candidate)) return candidate;
    }
    return undefined;
  };

  /** @param {string} name */
  const onPath = (name) => {
    for (const dir of pathDirectories(host)) {
      const hit = lookIn(dir, name);
      if (hit) return hit;
    }
    return undefined;
  };

  const setting = (configured ?? "").trim();
  if (setting !== "") {
    const expanded = expandHome(setting, host);
    const isBareName = !expanded.includes("/") && !expanded.includes("\\");
    if (isBareName) {
      const hit = onPath(expanded);
      if (hit) return { found: true, command: hit, source: "setting", searched };
    } else {
      const absolute = pathApi.resolve(expanded);
      for (const candidate of candidateNames(absolute, host)) {
        searched.push(candidate);
        if (host.isExecutableFile(candidate)) {
          return { found: true, command: candidate, source: "setting", searched };
        }
      }
    }
    return { found: false, searched, configured: setting };
  }

  const fromPath = onPath(COMMAND);
  if (fromPath) return { found: true, command: fromPath, source: "PATH", searched };

  for (const dir of installDirectories(host)) {
    const hit = lookIn(dir, COMMAND);
    if (hit) return { found: true, command: hit, source: "install directory", searched };
  }

  return { found: false, searched, configured: undefined };
}

/**
 * `hollowscript lsp` starts a separate `hollowscript-lsp` binary: the one
 * named by HOLLOWSCRIPT_LSP_PATH, else the one next to the `hollowscript`
 * executable, else one on PATH (crates/hollowscript-cli, `lsp_binary_path`).
 * This mirrors that search so a failure to start can say why. It is advice
 * for the log only: the CLI's own search is what decides.
 *
 * @param {string} command  the resolved `hollowscript` path
 * @param {DiscoveryHost & { realpath: (file: string) => string }} host
 * @returns {string | undefined}  where it would be found, if anywhere
 */
function findLanguageServerBinary(command, host) {
  const pathApi = host.platform === "win32" ? path.win32 : path.posix;
  if (host.env.HOLLOWSCRIPT_LSP_PATH) {
    return host.isExecutableFile(host.env.HOLLOWSCRIPT_LSP_PATH)
      ? host.env.HOLLOWSCRIPT_LSP_PATH
      : undefined;
  }
  const name = host.platform === "win32" ? "hollowscript-lsp.exe" : "hollowscript-lsp";
  let real = command;
  try {
    real = host.realpath(command);
  } catch {
    // Keep the unresolved path.
  }
  const sibling = pathApi.join(pathApi.dirname(real), name);
  if (host.isExecutableFile(sibling)) return sibling;
  for (const dir of pathDirectories(host)) {
    const candidate = pathApi.join(dir, name);
    if (host.isExecutableFile(candidate)) return candidate;
  }
  return undefined;
}

module.exports = {
  COMMAND,
  INSTALL_COMMANDS,
  INSTALL_GUIDE_URL,
  installCommand,
  installDirectories,
  findServer,
  findLanguageServerBinary,
};
