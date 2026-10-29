// A thin client: it finds the `hollowscript` command, spawns
// `hollowscript lsp` (which runs the hollowscript-lsp binary with its
// standard input and output connected straight through) and hands the
// connection to vscode-languageclient. It implements no language feature
// itself: diagnostics, formatting, hover, go to definition, references,
// rename, signature help, completion, the outline, folding, semantic tokens
// and quick fixes all come from the server, over the capabilities it
// advertises in `initialize` (crates/hollowscript-lsp/src/server.rs).

"use strict";

const fs = require("node:fs");
const os = require("node:os");
const { execFile } = require("node:child_process");
const vscode = require("vscode");
const {
  LanguageClient,
  TransportKind,
  CloseAction,
  ErrorAction,
  RevealOutputChannelOn,
} = require("vscode-languageclient/node");
const discovery = require("./discovery");

const CLIENT_ID = "hollowscript";
const CLIENT_NAME = "HollowScript Language Server";

/** @type {LanguageClient | undefined} */
let client;
/** @type {vscode.LogOutputChannel} */
let output;
/** @type {vscode.LogOutputChannel} */
let traceOutput;
/** Serialises start, stop and restart so two never interleave. */
let pending = Promise.resolve();

/**
 * The real file system and environment, for discovery.
 * @returns {import("./discovery").DiscoveryHost & { realpath: (file: string) => string }}
 */
function realHost() {
  return {
    platform: process.platform,
    env: process.env,
    homedir: os.homedir(),
    isExecutableFile(file) {
      try {
        if (!fs.statSync(file).isFile()) return false;
        if (process.platform !== "win32") fs.accessSync(file, fs.constants.X_OK);
        return true;
      } catch {
        return false;
      }
    },
    realpath: (file) => fs.realpathSync(file),
  };
}

/**
 * Runs `<command> --version` for the log. Never throws.
 * @param {string} command
 * @returns {Promise<string | undefined>}
 */
function versionOf(command) {
  return new Promise((resolve) => {
    execFile(command, ["--version"], { timeout: 5000, windowsHide: true }, (error, stdout) => {
      resolve(error ? undefined : stdout.trim());
    });
  });
}

/**
 * Tells the user the command could not be found, with the install command
 * for their platform and a way to point at an existing binary.
 * @param {import("./discovery").NotFound} result
 */
async function reportMissing(result) {
  const install = discovery.installCommand(process.platform);
  output.error(
    result.configured
      ? `hollowscript.executablePath is set to '${result.configured}', which is not an executable file.`
      : "The 'hollowscript' command was not found."
  );
  output.info(`Looked in:\n  ${result.searched.join("\n  ")}`);
  output.info(`Install it with:\n  ${install}`);

  const message = result.configured
    ? `HollowScript: the 'hollowscript.executablePath' setting ('${result.configured}') is not an executable file. Fix the setting, or clear it to search PATH and ~/.hollowscript/bin.`
    : `HollowScript: the 'hollowscript' command was not found, so diagnostics, formatting and navigation are off (syntax highlighting still works). Install it with: ${install}`;
  const actions = result.configured
    ? ["Open Settings", "Retry"]
    : ["Copy Install Command", "Open Install Guide", "Open Settings", "Retry"];
  const choice = await vscode.window.showErrorMessage(message, ...actions);
  switch (choice) {
    case "Copy Install Command":
      await vscode.env.clipboard.writeText(install);
      vscode.window.showInformationMessage(
        "Install command copied. Run it in a terminal, then run 'HollowScript: Restart Language Server'."
      );
      break;
    case "Open Install Guide":
      await vscode.env.openExternal(vscode.Uri.parse(discovery.INSTALL_GUIDE_URL));
      break;
    case "Open Settings":
      await vscode.commands.executeCommand(
        "workbench.action.openSettings",
        "hollowscript.executablePath"
      );
      break;
    case "Retry":
      await vscode.commands.executeCommand("hollowscript.restartServer");
      break;
    default:
      break;
  }
}

/**
 * Tells the user the server stopped while starting, which in practice means
 * `hollowscript lsp` could not find or run the hollowscript-lsp binary.
 * @param {string} command
 */
function reportStartFailure(command) {
  const lsp = discovery.findLanguageServerBinary(command, realHost());
  if (!lsp) {
    output.error(
      `'${command} lsp' needs the hollowscript-lsp binary next to it (or HOLLOWSCRIPT_LSP_PATH pointing at it), and none was found. Reinstall or update HollowScript.`
    );
  }
  vscode.window
    .showErrorMessage(
      "HollowScript: the language server stopped while starting. See the output for details.",
      "Show Output",
      "Restart"
    )
    .then((choice) => {
      if (choice === "Show Output") output.show(true);
      if (choice === "Restart") vscode.commands.executeCommand("hollowscript.restartServer");
    });
}

/**
 * Creates and starts a client, if the command can be found.
 */
async function start() {
  const configured = vscode.workspace
    .getConfiguration("hollowscript")
    .inspect("executablePath");
  // Only a user (global) value counts: the setting is machine-scoped, but
  // read it defensively all the same so a workspace can never choose the
  // program this extension runs.
  const setting = configured?.globalValue;
  const result = discovery.findServer(typeof setting === "string" ? setting : undefined, realHost());
  if (!result.found) {
    void reportMissing(result);
    return;
  }

  const command = result.command;
  const version = await versionOf(command);
  output.info(`Using ${command} (from ${result.source})${version ? `: ${version}` : ""}`);

  let initialised = false;
  let reported = false;
  let restarts = 0;
  /** @type {import("vscode-languageclient/node").ServerOptions} */
  const serverOptions = {
    command,
    args: ["lsp"],
    transport: TransportKind.stdio,
  };
  /** @type {import("vscode-languageclient/node").LanguageClientOptions} */
  const clientOptions = {
    documentSelector: [{ scheme: "file", language: "hollowscript" }],
    outputChannel: output,
    traceOutputChannel: traceOutput,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
    initializationFailedHandler(error) {
      output.error(`Initialisation failed: ${error instanceof Error ? error.message : String(error)}`);
      return false;
    },
    errorHandler: {
      error: () => ({ action: ErrorAction.Continue, handled: true }),
      closed() {
        if (!initialised) {
          if (!reported) reportStartFailure(command);
          reported = true;
          return { action: CloseAction.DoNotRestart, handled: true };
        }
        restarts += 1;
        if (restarts > 4) {
          return {
            action: CloseAction.DoNotRestart,
            message:
              "The HollowScript language server stopped 5 times. Run 'HollowScript: Restart Language Server' to try again.",
          };
        }
        output.warn(`The language server stopped; restarting (${restarts} of 4).`);
        return { action: CloseAction.Restart, handled: true };
      },
    },
  };

  const next = new LanguageClient(CLIENT_ID, CLIENT_NAME, serverOptions, clientOptions);
  client = next;
  try {
    await next.start();
    initialised = true;
  } catch (error) {
    output.error(`Could not start the language server: ${error instanceof Error ? error.message : String(error)}`);
    if (client === next) client = undefined;
    // The close handler has already told the user when the process started
    // and then stopped; this covers a spawn that failed outright.
    if (!reported) reportStartFailure(command);
    reported = true;
    try {
      await next.dispose();
    } catch {
      // Already stopped.
    }
  }
}

async function stop() {
  const current = client;
  client = undefined;
  if (!current) return;
  try {
    await current.dispose(2000);
  } catch (error) {
    output.warn(`Stopping the language server: ${error instanceof Error ? error.message : String(error)}`);
  }
}

/** @param {() => Promise<void>} work */
function serialise(work) {
  pending = pending.then(work, work);
  return pending;
}

/**
 * @param {vscode.ExtensionContext} context
 */
function activate(context) {
  output = vscode.window.createOutputChannel(CLIENT_NAME, { log: true });
  traceOutput = vscode.window.createOutputChannel(`${CLIENT_NAME} Trace`, { log: true });
  context.subscriptions.push(
    output,
    traceOutput,
    vscode.commands.registerCommand("hollowscript.restartServer", () =>
      serialise(async () => {
        output.info("Restarting the language server.");
        await stop();
        await start();
      })
    ),
    vscode.commands.registerCommand("hollowscript.showOutput", () => output.show(true)),
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (event.affectsConfiguration("hollowscript.executablePath")) {
        void serialise(async () => {
          output.info("hollowscript.executablePath changed; restarting the language server.");
          await stop();
          await start();
        });
      }
    })
  );
  return serialise(start);
}

function deactivate() {
  return serialise(stop);
}

module.exports = { activate, deactivate };
