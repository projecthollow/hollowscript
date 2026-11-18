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

/** @param {string} source @returns {Promise<import("vscode").TextDocument>} */
async function scratch(source) {
  const document = await open("members.hws");
  const edit = new vscode.WorkspaceEdit();
  edit.replace(document.uri, new vscode.Range(document.positionAt(0), document.positionAt(document.getText().length)), source);
  assert.ok(await vscode.workspace.applyEdit(edit));
  return document;
}

test("activates for a .hws file", async () => {
  const document = await open("sample.hws");
  assert.equal(document.languageId, "hollowscript");
  const extension = vscode.extensions.getExtension("soulstack.hollowscript");
  assert.ok(extension, "extension not found");
  await extension.activate();
  assert.ok(extension.isActive);
  if (process.env.TEST_EXTENSION_ROOT) assert.equal(extension.extensionPath, process.env.TEST_EXTENSION_ROOT);
  if (process.env.TEST_RESTRICTED) assert.equal(vscode.workspace.isTrusted, false, "must run in Restricted Mode");
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

test("rename preserves Unicode before a reference", async () => {
  const source = 'constant x = 7\nconstant text = "😀${x}"\nreturn text\n';
  const document = await scratch(source);
  const edit = /** @type {vscode.WorkspaceEdit} */ (await eventually(async () => {
    const result = await vscode.commands.executeCommand("vscode.executeDocumentRenameProvider", document.uri, new vscode.Position(0, 9), "value");
    return result || undefined;
  }, "Unicode rename"));
  assert.ok(await vscode.workspace.applyEdit(edit));
  assert.ok(document.getText().includes('"😀${value}"'), document.getText());
  const reference = document.positionAt(document.getText().indexOf("${value}") + 2);
  const definitions = /** @type {vscode.Location[]} */ (await vscode.commands.executeCommand("vscode.executeDefinitionProvider", document.uri, reference));
  assert.equal(definitions[0]?.range.start.character, 9);
  const semantic = /** @type {vscode.SemanticTokens} */ (await vscode.commands.executeCommand("vscode.provideDocumentSemanticTokens", document.uri));
  let line = 0, character = 0, found = false;
  for (let i = 0; i < semantic.data.length; i += 5) {
    line += semantic.data[i];
    character = (semantic.data[i] === 0 ? character : 0) + semantic.data[i + 1];
    if (line === reference.line && character === reference.character) found = true;
  }
  assert.ok(found, "semantic token must use the UTF-16 column after the emoji");
  const replacement = new vscode.WorkspaceEdit();
  replacement.replace(document.uri, new vscode.Range(reference, reference.translate(0, 5)), "missing");
  assert.ok(await vscode.workspace.applyEdit(replacement));
  await eventually(() => vscode.languages.getDiagnostics(document.uri).some((d) => String(d.code) === "HS0205" && d.range.start.line === 1), "Unicode edit synchronisation");
  await vscode.commands.executeCommand("workbench.action.files.revert");
});

test("member completion works with the released v0.1.1 server", async () => {
  for (const [source, expected, kind] of [
    ['constant s = "hi"\nreturn s.tr', "trim", vscode.CompletionItemKind.Method],
    ['return "😀".trim().', "length", vscode.CompletionItemKind.Method],
    ['constant xs = [1]\nreturn xs.', "append", vscode.CompletionItemKind.Method],
    ['return ["a": 1].', "get", vscode.CompletionItemKind.Method],
    ['return set([1]).', "union", vscode.CompletionItemKind.Method],
    ['constant r: Result<Int, String> = success(1)\nreturn r.', "unwrapOr", vscode.CompletionItemKind.Method],
    ['import Person from "./util"\nconstant p: Person = { name: "A", age: 1 }\nreturn p.', "age", vscode.CompletionItemKind.Field],
    ['import Choice from "./util"\nreturn Choice.', "First", vscode.CompletionItemKind.EnumMember],
    ['return json.', "parse", vscode.CompletionItemKind.Method],
  ]) {
    const document = await scratch(/** @type {string} */ (source));
    const list = await eventually(async () => {
      /** @type {vscode.CompletionList} */
      const result = await vscode.commands.executeCommand("vscode.executeCompletionItemProvider", document.uri, document.positionAt(document.getText().length), ".");
      return result?.items.some((i) => (typeof i.label === "string" ? i.label : i.label.label) === expected && i.kind === kind) ? result : undefined;
    }, `member ${expected}`);
    assert.ok(list.items.some((i) => i.detail));
  }
  await vscode.commands.executeCommand("workbench.action.files.revert");
});

test("references, rename, signature help, folding, semantic tokens and quick fixes", async () => {
  const source = 'import twice from "./util"\nconstant answer = twice(21)\nreturn answer\n';
  const document = await scratch(source);
  // Windows may preserve CRLF when applying text to an existing document.
  // Compute offsets from the editor's actual text, as a provider caller does.
  const call = document.positionAt(document.getText().indexOf("twice(21)"));
  /** @type {vscode.Location[]} */
  const references = await eventually(async () => {
    const result = /** @type {vscode.Location[]} */ (await vscode.commands.executeCommand("vscode.executeReferenceProvider", document.uri, call));
    return result?.length >= 2 ? result : undefined;
  }, "cross-file references");
  assert.ok(references.some((r) => r.uri.toString() === document.uri.toString()));
  const definitions = /** @type {vscode.Location[]} */ (await vscode.commands.executeCommand("vscode.executeDefinitionProvider", document.uri, call));
  assert.ok(definitions.some((r) => r.uri.fsPath.endsWith("util.hws")));
  /** @type {vscode.WorkspaceEdit} */
  const rename = await vscode.commands.executeCommand("vscode.executeDocumentRenameProvider", document.uri, call, "doubleValue");
  assert.ok(rename.entries().length >= 2, "rename must reach the imported declaration");
  /** @type {vscode.SignatureHelp} */
  const signature = await vscode.commands.executeCommand("vscode.executeSignatureHelpProvider", document.uri, document.positionAt(document.getText().indexOf("21)")));
  assert.ok(signature.signatures.some((s) => s.label.includes("Int")));
  const sample = await open("sample.hws");
  /** @type {vscode.FoldingRange[]} */
  const folds = await vscode.commands.executeCommand("vscode.executeFoldingRangeProvider", sample.uri);
  assert.ok(folds.length > 0);
  /** @type {vscode.SemanticTokens} */
  const tokens = await vscode.commands.executeCommand("vscode.provideDocumentSemanticTokens", sample.uri);
  assert.ok(tokens.data.length > 0);
  const broken = await scratch('effect print("hello")\n');
  await eventually(() => vscode.languages.getDiagnostics(broken.uri).some((d) => String(d.code) === "HS0205"), "missing uses diagnostic");
  /** @type {(vscode.CodeAction | vscode.Command)[]} */
  const actions = await eventually(async () => {
    const result = /** @type {vscode.CodeAction[]} */ (await vscode.commands.executeCommand("vscode.executeCodeActionProvider", broken.uri, new vscode.Range(0, 7, 0, 12), vscode.CodeActionKind.QuickFix.value));
    return result.some((a) => a.title.includes("uses")) ? result : undefined;
  }, "missing uses quick fix");
  assert.ok(actions.some((a) => a.title.includes("uses")));
  for (const [source, code, title] of [
    ['fn apply(f: effect fn(String)) {\n    f("hello")\n}\n', 'HS0401', "effect"],
    ['constant unused = 1\nreturn 2\n', 'HS0801', "unused"],
  ]) {
    const target = await scratch(source);
    await eventually(() => vscode.languages.getDiagnostics(target.uri).some((d) => String(d.code) === code), `diagnostic ${code}`);
    const fix = await eventually(async () => {
      const fixes = /** @type {vscode.CodeAction[]} */ (await vscode.commands.executeCommand("vscode.executeCodeActionProvider", target.uri, new vscode.Range(0, 0, target.lineCount - 1, 0), vscode.CodeActionKind.QuickFix.value));
      return fixes.find((a) => a.title.includes(title) && a.edit);
    }, `${title} quick fix`);
    assert.ok(fix?.edit, `missing ${title} fix`);
    assert.ok(await vscode.workspace.applyEdit(fix.edit));
    await eventually(() => !vscode.languages.getDiagnostics(target.uri).some((d) => String(d.code) === code), `fixed ${code}`);
  }
  await vscode.commands.executeCommand("workbench.action.files.revert");
});

async function run() {
  let failed = 0;
  const selected = process.env.TEST_MISSING_SERVER ? tests.filter(([name]) => name.startsWith("activates") || name.startsWith("member completion")) : tests;
  for (const [name, body] of selected) {
    try {
      await body();
      console.log(`  ok   ${name}`);
    } catch (error) {
      failed += 1;
      console.log(`  FAIL ${name}\n${error instanceof Error ? error.stack : String(error)}`);
    }
  }
  if (failed > 0) throw new Error(`${failed} of ${tests.length} integration tests failed`);
  console.log(`  ${selected.length} integration tests passed`);
}

module.exports = { run };
