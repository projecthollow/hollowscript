"use strict";
const vscode = require("vscode");

// Released v0.1.1 counts code points. VS Code counts UTF-16 code units,
// including when an ASCII identifier follows a non-BMP string character.
/** @param {vscode.TextDocument} document @param {vscode.Position} position */
function toServer(document, position) {
  const text = document.lineAt(Math.min(position.line, document.lineCount - 1)).text;
  return new vscode.Position(position.line, [...text.slice(0, position.character)].length);
}
/** @param {vscode.TextDocument} document @param {vscode.Position} position */
function toClient(document, position) {
  const text = document.lineAt(Math.min(position.line, document.lineCount - 1)).text;
  return new vscode.Position(position.line, [...text].slice(0, position.character).join("").length);
}
/** @param {vscode.TextDocument} document @param {vscode.Range} range @param {boolean} [outgoing] */
function range(document, range, outgoing = false) {
  const convert = outgoing ? toServer : toClient;
  return new vscode.Range(convert(document, range.start), convert(document, range.end));
}
/** @param {vscode.WorkspaceEdit | undefined | null} edit */
async function edits(edit) {
  if (!edit) return edit;
  const result = new vscode.WorkspaceEdit();
  for (const [uri, changes] of edit.entries()) {
    const document = await vscode.workspace.openTextDocument(uri);
    result.set(uri, changes.map((change) => new vscode.TextEdit(range(document, change.range), change.newText)));
  }
  return result;
}
/** @param {vscode.TextDocument} document
 * @param {vscode.Definition | vscode.DefinitionLink[] | undefined | null} result
 * @returns {Promise<vscode.Definition | vscode.DefinitionLink[] | undefined | null>}
 */
async function locations(document, result) {
  if (!result) return result;
  const list = await Promise.all([result].flat().map(async (location) => {
    if ("uri" in location) {
      const target = await vscode.workspace.openTextDocument(location.uri);
      return new vscode.Location(location.uri, range(target, location.range));
    }
    const target = await vscode.workspace.openTextDocument(location.targetUri);
    return { ...location, targetRange: range(target, location.targetRange), targetSelectionRange: range(target, location.targetSelectionRange ?? location.targetRange), originSelectionRange: location.originSelectionRange ? range(document, location.originSelectionRange) : undefined };
  }));
  return Array.isArray(result) ? /** @type {vscode.Location[] | vscode.DefinitionLink[]} */ (list) : /** @type {vscode.Location} */ (list[0]);
}
/** @param {vscode.TextDocument} document @param {vscode.SemanticTokens | undefined | null} result */
function semanticTokens(document, result) {
  if (!result) return result;
  const data = new Uint32Array(result.data);
  let line = 0;
  let character = 0;
  let previous = 0;
  for (let i = 0; i < data.length; i += 5) {
    const delta = data[i];
    line += delta;
    character = (delta === 0 ? character : 0) + data[i + 1];
    const converted = toClient(document, new vscode.Position(line, character)).character;
    data[i + 1] = delta === 0 ? converted - previous : converted;
    previous = converted;
  }
  return new vscode.SemanticTokens(data, result.resultId);
}
/** @param {vscode.TextDocument} document @param {vscode.DocumentSymbol[] | vscode.SymbolInformation[]} items */
function symbols(document, items) {
  for (const symbol of items) {
    if ("location" in symbol) symbol.location.range = range(document, symbol.location.range);
    else {
      symbol.range = range(document, symbol.range);
      symbol.selectionRange = range(document, symbol.selectionRange);
      if (symbol.children) symbols(document, symbol.children);
    }
  }
}
module.exports = { toServer, toClient, range, edits, locations, semanticTokens, symbols };
