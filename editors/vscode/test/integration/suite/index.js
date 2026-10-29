"use strict";

// Runs inside the VS Code extension host. Each check drives the editor the
// way a user would and asserts on what the language server returned.

const assert = require("node:assert/strict");
const path = require("node:path");
const vscode = require("vscode");

const FIXTURES = path.join(__dirname, "..", "..", "fixtures");

/** @param {number} ms */
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Polls `probe` until it returns a truthy value or the time runs out.
 * @template T
 * @param {() => T | undefined | false | PromiseLike<T | undefined | false>} probe
 * @param {string} what
 * @returns {Promise<T>}
 */
async function eventually(probe, what, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = await probe();
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(200);
  }
}

/** @param {string} name */
async function open(name) {
  const document = await vscode.workspace.openTextDocument(path.join(FIXTURES, name));
  await vscode.window.showTextDocument(document);
  return document;
}

/** @type {[string, () => Promise<void>][]} */
const tests = [];
/** @param {string} name @param {() => Promise<void>} body */
const test = (name, body) => tests.push([name, body]);

test("activates for a .hws file", async () => {
  const document = await open("sample.hws");
  assert.equal(document.languageId, "hollowscript");
  const extension = vscode.extensions.getExtension("hollowscript.hollowscript");
  assert.ok(extension, "extension not found");
  await extension.activate();
  assert.ok(extension.isActive);
});

test("publishes diagnostics from the server", async () => {
  const document = await open("broken.hws");
  const diagnostics = await eventually(() => {
    const list = vscode.languages.getDiagnostics(document.uri);
    return list.length > 0 ? list : undefined;
  }, "diagnostics");
  const codes = diagnostics.map((d) => String(typeof d.code === "object" ? d.code.value : d.code));
  assert.ok(codes.includes("HS0205"), `codes: ${codes.join(", ")}`);
  const error = diagnostics.find((d) => String(d.code) === "HS0205");
  assert.equal(error?.range.start.line, 1);
  assert.equal(error?.severity, vscode.DiagnosticSeverity.Error);
});

test("formats a document", async () => {
  const document = await open("sample.hws");
  const edits = await eventually(
    () =>
      /** @type {Thenable<vscode.TextEdit[] | undefined>} */ (
        vscode.commands.executeCommand("vscode.executeFormatDocumentProvider", document.uri, { tabSize: 4, insertSpaces: true })
      ),
    "formatting edits"
  );
  const edit = new vscode.WorkspaceEdit();
  edit.set(document.uri, edits);
  assert.ok(await vscode.workspace.applyEdit(edit));
  assert.ok(document.getText().includes("constant answer = double(21)"), document.getText());
  await vscode.commands.executeCommand("workbench.action.files.revert");
});

test("hovers, goes to a definition and lists symbols", async () => {
  const document = await open("sample.hws");
  const position = new vscode.Position(4, 21);
  const hovers = await eventually(async () => {
    /** @type {vscode.Hover[]} */
    const list = await vscode.commands.executeCommand("vscode.executeHoverProvider", document.uri, position);
    return list.length > 0 ? list : undefined;
  }, "hover");
  const text = hovers
    .flatMap((h) => h.contents)
    .map((c) => (typeof c === "string" ? c : c.value))
    .join("\n");
  assert.ok(text.includes("fn(Int): Int"), text);

  /** @type {(vscode.Location | vscode.LocationLink)[]} */
  const definitions = await vscode.commands.executeCommand("vscode.executeDefinitionProvider", document.uri, position);
  const first = definitions[0];
  const range = "targetRange" in first ? first.targetRange : first.range;
  assert.equal(range.start.line, 0);

  /** @type {vscode.DocumentSymbol[]} */
  const symbols = await vscode.commands.executeCommand("vscode.executeDocumentSymbolProvider", document.uri);
  assert.deepEqual(symbols.map((s) => s.name).sort(), ["answer", "double"]);
});

test("restarts the language server on command", async () => {
  await vscode.commands.executeCommand("hollowscript.restartServer");
  const document = await open("broken.hws");
  // Edit the document so fresh diagnostics must come from the new server.
  const edit = new vscode.WorkspaceEdit();
  edit.insert(document.uri, new vscode.Position(0, 0), "constant unused = 2\n");
  await vscode.workspace.applyEdit(edit);
  await eventually(() => {
    const list = vscode.languages.getDiagnostics(document.uri);
    return list.some((d) => d.range.start.line === 2 && String(d.code) === "HS0205");
  }, "diagnostics after restart");
  await vscode.commands.executeCommand("workbench.action.files.revert");
});

async function run() {
  let failed = 0;
  for (const [name, body] of tests) {
    try {
      await body();
      console.log(`  ok   ${name}`);
    } catch (error) {
      failed += 1;
      console.log(`  FAIL ${name}\n${error instanceof Error ? error.stack : String(error)}`);
    }
  }
  if (failed > 0) throw new Error(`${failed} of ${tests.length} integration tests failed`);
  console.log(`  ${tests.length} integration tests passed`);
}

module.exports = { run };
