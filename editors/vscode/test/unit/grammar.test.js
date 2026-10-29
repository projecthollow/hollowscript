"use strict";

// Tokenises HollowScript with the extension's own TextMate grammar through
// vscode-textmate and vscode-oniguruma, the same engine VS Code uses, and
// checks it against the specification's lexical grammar.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const textmate = require("vscode-textmate");
const oniguruma = require("vscode-oniguruma");

const ROOT = path.join(__dirname, "..", "..");
const SPEC = path.join(ROOT, "..", "..", "docs", "spec");
const GRAMMAR = path.join(ROOT, "syntaxes", "hollowscript.tmLanguage.json");

/** @type {Promise<import("vscode-textmate").IGrammar>} */
const grammarPromise = (async () => {
  const wasm = fs.readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm"));
  await oniguruma.loadWASM(wasm.buffer.slice(wasm.byteOffset, wasm.byteOffset + wasm.byteLength));
  const registry = new textmate.Registry({
    onigLib: Promise.resolve({
      createOnigScanner: (sources) => new oniguruma.OnigScanner(sources),
      createOnigString: (s) => new oniguruma.OnigString(s),
    }),
    loadGrammar: async (scopeName) =>
      scopeName === "source.hollowscript"
        ? textmate.parseRawGrammar(fs.readFileSync(GRAMMAR, "utf8"), GRAMMAR)
        : null,
  });
  const grammar = await registry.loadGrammar("source.hollowscript");
  if (!grammar) throw new Error("grammar did not load");
  return grammar;
})();

/**
 * @param {string} text
 * @returns {Promise<{ text: string, scopes: string[] }[][]>} tokens per line
 */
async function tokenise(text) {
  const grammar = await grammarPromise;
  let state = textmate.INITIAL;
  return text.split("\n").map((line) => {
    const result = grammar.tokenizeLine(line, state);
    state = result.ruleStack;
    return result.tokens.map((t) => ({ text: line.slice(t.startIndex, t.endIndex), scopes: t.scopes }));
  });
}

/**
 * The innermost scope of the first token whose text is `text` on `line`.
 * @param {{ text: string, scopes: string[] }[]} line
 * @param {string} text
 */
function scopeOf(line, text) {
  const token = line.find((t) => t.text === text);
  assert.ok(token, `no token '${text}' in ${JSON.stringify(line.map((t) => t.text))}`);
  return token.scopes;
}

test("the grammar's keywords are exactly section 1.3's 27 reserved words", async () => {
  const spec = fs.readFileSync(path.join(SPEC, "01-lexical-structure.md"), "utf8");
  const block = /\*\*Keywords\.\*\*[^`]*```\n([\s\S]*?)```/.exec(spec);
  assert.ok(block, "keyword block not found in the spec");
  const expected = block[1].split(/\s+/).filter(Boolean).sort();
  assert.equal(expected.length, 27);
  for (const word of expected) {
    const [line] = await tokenise(word);
    const scopes = line[0].scopes.join(" ");
    assert.match(scopes, /keyword|storage|constant\.language|invalid\.illegal\.reserved/, `${word}: ${scopes}`);
  }
});

test("an interpolation holding a record literal ends at its own closing brace", async () => {
  const [line] = await tokenise('return "a ${ { x: 1 }.x } b"');
  const b = line.find((t) => t.text.includes(" b"));
  assert.ok(b);
  assert.equal(b.scopes.at(-1), "string.quoted.double.hollowscript");
  const xs = line.filter((t) => t.text === "x");
  assert.equal(xs.length, 2);
  for (const x of xs) assert.ok(x.scopes.includes("meta.embedded.line.hollowscript"), x.scopes.join(" "));
});

test("interpolations nest, with strings inside them", async () => {
  const [line] = await tokenise('return "o ${"i ${n + 1}"} o"');
  const n = scopeOf(line, "n");
  assert.equal(n.filter((s) => s === "meta.embedded.line.hollowscript").length, 2);
  assert.equal(line.at(-1)?.scopes.at(-1), "punctuation.definition.string.end.hollowscript");
});

test("a string or filter literal never runs onto the next line", async () => {
  const lines = await tokenise('constant a = "open\nconstant b = `open\nconstant c = 1');
  assert.equal(scopeOf(lines[1], "constant").at(-1), "storage.type.hollowscript");
  assert.equal(scopeOf(lines[2], "constant").at(-1), "storage.type.hollowscript");
  const unterminated = await tokenise('constant a = "v ${1 +\nreturn a');
  assert.equal(scopeOf(unterminated[1], "return").at(-1), "keyword.control.hollowscript");
});

test("escapes follow sections 1.5 and 1.6", async () => {
  const [s] = await tokenise('"\\n \\u{1F600} \\$ \\q"');
  assert.equal(scopeOf(s, "\\n").at(-1), "constant.character.escape.hollowscript");
  assert.equal(scopeOf(s, "\\u{1F600}").at(-1), "constant.character.escape.hollowscript");
  assert.equal(scopeOf(s, "\\$").at(-1), "constant.character.escape.hollowscript");
  assert.equal(scopeOf(s, "\\q").at(-1), "invalid.illegal.escape.hollowscript");
  const [f] = await tokenise("`a \\` b \\d`");
  assert.equal(scopeOf(f, "\\`").at(-1), "constant.character.escape.hollowscript");
  assert.ok(!f.some((t) => t.text === "\\d" && t.scopes.at(-1)?.startsWith("constant.character")));
});

test("numbers, comments, declarations and tagged comments", async () => {
  const [line] = await tokenise("fn area(w: Int): Float { return 1_000 * 2.5e-3 } // note: x");
  assert.equal(scopeOf(line, "area").at(-1), "entity.name.function.hollowscript");
  assert.equal(scopeOf(line, "Int").at(-1), "support.type.hollowscript");
  assert.equal(scopeOf(line, "1_000").at(-1), "constant.numeric.integer.hollowscript");
  assert.equal(scopeOf(line, "2.5e-3").at(-1), "constant.numeric.float.hollowscript");
  assert.equal(scopeOf(line, "note").at(-1), "keyword.other.tag.hollowscript");
  const [decl] = await tokenise("record Point = { x: Int }");
  assert.equal(scopeOf(decl, "Point").at(-1), "entity.name.type.hollowscript");
});

test("every valid example in the specification tokenises with nothing marked invalid", async () => {
  let blocks = 0;
  for (const file of fs.readdirSync(SPEC).filter((f) => f.endsWith(".md"))) {
    const text = fs.readFileSync(path.join(SPEC, file), "utf8");
    for (const match of text.matchAll(/```hollow\n([\s\S]*?)```/g)) {
      blocks += 1;
      const lines = await tokenise(match[1]);
      for (const [i, line] of lines.entries()) {
        for (const token of line) {
          const bad = token.scopes.find((s) => s.startsWith("invalid."));
          assert.equal(bad, undefined, `${file}, block ${blocks}, line ${i + 1}: '${token.text}' is ${bad}`);
        }
      }
    }
  }
  assert.ok(blocks > 50, `only ${blocks} examples found`);
});
