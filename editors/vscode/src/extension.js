// A thin client: it finds the `hollowscript` command, spawns
// `hollowscript lsp` (which runs the hollowscript-lsp binary with its
// standard input and output connected straight through) and hands the
// connection to vscode-languageclient. It implements no language feature
// itself: diagnostics, formatting, hover, go to definition, references,
// rename, signature help, completion, the outline, folding, semantic tokens
// and quick fixes all come from the server, over the capabilities it
// advertises in `initialize` (crates/hollowscript-lsp/src/server.rs).

"use strict";

const path = require("node:path");
const { MemberService } = require("./members");
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
const positions = require("./positions");
const { createTraceOutput } = require("./trace");
const { windowsServerUri, windowsServerPath } = require("./uri");
/** @param {vscode.Uri} uri */
function toProtocolUri(uri) { return process.platform === "win32" && uri.scheme === "file" ? windowsServerUri(uri.fsPath) ?? uri.toString() : uri.toString(); }
/** @param {string} value */
function fromProtocolUri(value) { const file = process.platform === "win32" ? windowsServerPath(value) : undefined; return file === undefined ? vscode.Uri.parse(value) : vscode.Uri.file(file); }

const CLIENT_ID = "hollowscript";
const CLIENT_NAME = "HollowScript Language Server";

/** @type {LanguageClient | undefined} */
let client;
let serverHasMemberCompletion = false;
let legacyPositions = true;
/** @type {vscode.LogOutputChannel} */
let output;
/** @type {vscode.LogOutputChannel} */
let traceOutput;
/** Serialises start, stop and restart so two never interleave. */
let pending = Promise.resolve();
/** @type {import("./members").MemberService} */
let actionAnalysis;

/** v0.1.1 refactoring only visits open documents. Load the declaring module
 * before references or rename so the exported declaration is included too.
 * This opens a document in memory, without showing it or changing its text.
 * @param {vscode.TextDocument} document @param {vscode.Position} position
 * @param {vscode.CancellationToken} token
 */
async function openDeclaration(document, position, token) {
  if (!client || token.isCancellationRequested) return;
  if (legacyPositions) position = positions.toServer(document, position);
  const result = await client.sendRequest("textDocument/definition", {
    textDocument: { uri: toProtocolUri(document.uri) },
    position: { line: position.line, character: position.character },
  }, token);
  const targets = /** @type {({uri?: string, targetUri?: string} | null)[]} */ ([result].flat());
  for (const target of targets) {
    const uri = target?.uri ?? target?.targetUri;
    if (!uri || token.isCancellationRequested) continue;
    const parsed = fromProtocolUri(uri);
    if (parsed.scheme === "file") await vscode.workspace.openTextDocument(parsed);
  }
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
    ? `HollowScript: the executable path '${result.configured}' cannot be used. Update it in Settings, or clear it to search for an installed CLI.`
    : "HollowScript: install the CLI to enable diagnostics, formatting and navigation. Syntax highlighting and member suggestions are available already.";
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
  const lsp = discovery.findLanguageServerBinary(command, discovery.realHost());
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
  const result = discovery.findServer(typeof setting === "string" ? setting : undefined, discovery.realHost());
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
    uriConverters: { code2Protocol: toProtocolUri, protocol2Code: fromProtocolUri },
    outputChannel: output,
    traceOutputChannel: traceOutput,
    revealOutputChannelOn: RevealOutputChannelOn.Never,
    middleware: {
      async didChange(event, next) {
        if (!legacyPositions || !client) return next(event);
        // v0.1.1 also applies incremental edits in code points. Full document
        // changes are valid under incremental sync and avoid corrupting its
        // stored source when a replacement crosses a surrogate pair.
        await client.sendNotification("textDocument/didChange", {
          textDocument: { uri: toProtocolUri(event.document.uri), version: event.document.version },
          contentChanges: [{ text: event.document.getText() }],
        });
      },
      async provideHover(document, position, token, next) {
        const hover = await next(document, legacyPositions ? positions.toServer(document, position) : position, token);
        if (legacyPositions && hover?.range) hover.range = positions.range(document, hover.range);
        return hover;
      },
      async provideDefinition(document, position, token, next) {
        const result = await next(document, legacyPositions ? positions.toServer(document, position) : position, token);
        return legacyPositions ? positions.locations(document, result) : result;
      },
      async prepareRename(document, position, token, next) {
        const result = await next(document, legacyPositions ? positions.toServer(document, position) : position, token);
        if (!legacyPositions || !result) return result;
        return "range" in result ? { ...result, range: positions.range(document, result.range) } : positions.range(document, result);
      },
      provideSignatureHelp(document, position, context, token, next) {
        return next(document, legacyPositions ? positions.toServer(document, position) : position, context, token);
      },
      provideCompletionItem(document, position, context, token, next) {
        return next(document, legacyPositions ? positions.toServer(document, position) : position, context, token);
      },
      async provideDocumentSymbols(document, token, next) {
        const result = await next(document, token);
        if (legacyPositions && result) positions.symbols(document, result);
        return result;
      },
      async provideDocumentSemanticTokens(document, token, next) {
        const result = await next(document, token);
        return legacyPositions ? positions.semanticTokens(document, result) : result;
      },
      async provideCodeActions(document, range, context, token, next) {
        if (context.diagnostics.length === 0) return next(document, range, context, token);
        const version = document.version;
        const open = new Map(vscode.workspace.textDocuments.filter((d) => d.uri.scheme === "file").map((d) => [d.uri.fsPath, d.getText()]));
        let precise;
        try { precise = await actionAnalysis.diagnostics(document.uri.fsPath, document.getText(), open, token); }
        catch { return []; }
        if (token.isCancellationRequested || document.version !== version) return [];
        const diagnostics = context.diagnostics.flatMap((diagnostic) => {
          const code = typeof diagnostic.code === "object" ? diagnostic.code.value : diagnostic.code;
          const index = precise.findIndex((d) => d.code === code && d.message === diagnostic.message && document.positionAt(d.start).line === diagnostic.range.start.line);
          const match = index < 0 ? undefined : precise.splice(index, 1)[0];
          if (!match) return [];
          const adjusted = new vscode.Diagnostic(new vscode.Range(document.positionAt(match.start), document.positionAt(match.end)), diagnostic.message, diagnostic.severity);
          adjusted.code = diagnostic.code;
          adjusted.source = diagnostic.source;
          if (legacyPositions) adjusted.range = positions.range(document, adjusted.range, true);
          return [adjusted];
        });
        const actions = await next(document, legacyPositions ? positions.range(document, range, true) : range, { ...context, diagnostics }, token);
        if (!legacyPositions || !actions) return actions;
        return Promise.all(actions.map(async (action) => {
          if ("edit" in action && action.edit) action.edit = await positions.edits(action.edit) ?? undefined;
          return action;
        }));
      },
      async provideReferences(document, position, context, token, next) {
        await openDeclaration(document, position, token);
        const result = await next(document, legacyPositions ? positions.toServer(document, position) : position, context, token);
        return legacyPositions ? /** @type {vscode.Location[] | undefined | null} */ (await positions.locations(document, result)) : result;
      },
      async provideRenameEdits(document, position, newName, token, next) {
        await openDeclaration(document, position, token);
        const edit = await next(document, legacyPositions ? positions.toServer(document, position) : position, newName, token);
        return legacyPositions ? positions.edits(edit) : edit;
      },
    },
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
    legacyPositions = next.initializeResult?.capabilities.positionEncoding !== "utf-16";
    serverHasMemberCompletion = next.initializeResult?.capabilities.completionProvider?.triggerCharacters?.includes(".") ?? false;
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
  serverHasMemberCompletion = false;
  legacyPositions = true;
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
  traceOutput = createTraceOutput(vscode, `${CLIENT_NAME} Trace`);
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
  actionAnalysis = new MemberService(path.join(__dirname, "member-worker.js"));
  context.subscriptions.push(actionAnalysis);
  const members = new MemberService(path.join(__dirname, "member-worker.js"));
  context.subscriptions.push(members, vscode.languages.registerCompletionItemProvider(
    { scheme: "file", language: "hollowscript" },
    {
      async provideCompletionItems(document, position, token) {
        if (serverHasMemberCompletion) return [];
        const source = document.getText();
        const offset = document.offsetAt(position);
        // Only member contexts are handled here; ordinary completion comes
        // from the installed server. The canonical lexer rejects comments
        // and literal text inside the worker.
        if (!/\.\s*[A-Za-z_0-9]*$/.test(source.slice(Math.max(0, offset - 256), offset))) return [];
        const version = document.version;
        const open = new Map(vscode.workspace.textDocuments.filter((d) => d.uri.scheme === "file").map((d) => [d.uri.fsPath, d.getText()]));
        try {
          const items = await members.complete(document.uri.fsPath, source, offset, open, token);
          if (token.isCancellationRequested || document.version !== version) return [];
          const kinds = { method: vscode.CompletionItemKind.Method, field: vscode.CompletionItemKind.Field, enumMember: vscode.CompletionItemKind.EnumMember };
          return items.filter((i) => Object.hasOwn(kinds, i.kind)).map((i) => {
            const item = new vscode.CompletionItem(i.label, kinds[/** @type {keyof typeof kinds} */ (i.kind)]);
            item.detail = i.detail;
            item.range = document.getWordRangeAtPosition(position, /[A-Za-z_][A-Za-z_0-9]*/g) ?? new vscode.Range(position, position);
            return item;
          });
        } catch (error) {
          output.warn(`Member completion unavailable: ${String(error)}`);
          return [];
        }
      },
    }, "."
  ));
  return serialise(start);
}

function deactivate() {
  return serialise(stop);
}

module.exports = { activate, deactivate };
