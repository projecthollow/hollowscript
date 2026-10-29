"use strict";

// Checks on what the published package contains and declares.

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { execFileSync, spawnSync } = require("node:child_process");

const ROOT = path.join(__dirname, "..", "..");
const REPO = path.join(ROOT, "..", "..");
const manifest = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8"));

test("the licence texts are the repository's own, unchanged", () => {
  for (const name of ["LICENSE-APACHE", "LICENSE-MIT"]) {
    assert.equal(
      fs.readFileSync(path.join(ROOT, name), "utf8"),
      fs.readFileSync(path.join(REPO, name), "utf8"),
      `${name} differs from the repository root's; copy it again`
    );
  }
});

test("THIRD-PARTY-NOTICES.md lists exactly what the bundle contains", () => {
  const result = spawnSync(process.execPath, ["scripts/notices.mjs", "--check"], { cwd: ROOT, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});

test("the manifest is a preview, dual licensed, and claims only .hws", () => {
  assert.equal(manifest.preview, true);
  assert.equal(manifest.publisher, "hollowscript");
  assert.equal(manifest.name, "hollowscript");
  assert.equal(manifest.license, "Apache-2.0 OR MIT");
  assert.match(manifest.description, /^Preview/);
  const languages = manifest.contributes.languages;
  assert.deepEqual(languages.map((/** @type {{ extensions: string[] }} */ l) => l.extensions).flat(), [".hws"]);
  // Bytecode containers (.hsb, magic "HSBC") are binary: never a text language.
  assert.ok(!JSON.stringify(manifest.contributes).includes(".hsb"));
  // The executable path must never come from a workspace.
  assert.equal(manifest.contributes.configuration.properties["hollowscript.executablePath"].scope, "machine");
  // Every dependency is pinned to one exact version.
  for (const [name, range] of Object.entries({ ...manifest.dependencies, ...manifest.devDependencies })) {
    assert.match(String(range), /^\d+\.\d+\.\d+$/, `${name} is not pinned: ${range}`);
  }
});

/** The `hollowscript` binary to check snippets with, if one is built. */
function hollowscript() {
  const candidates = [
    process.env.HOLLOWSCRIPT_BIN,
    path.join(REPO, "target", "release", os.platform() === "win32" ? "hollowscript.exe" : "hollowscript"),
    path.join(REPO, "target", "debug", os.platform() === "win32" ? "hollowscript.exe" : "hollowscript"),
  ];
  return candidates.find((c) => c && fs.existsSync(c));
}

/** Snippets that insert an expression rather than a statement or declaration. */
const EXPRESSION_SNIPPETS = new Set(["Closure"]);

test("every snippet, with its default text, is syntactically valid", (t) => {
  const bin = hollowscript();
  if (!bin) {
    t.skip("no hollowscript binary built (cargo build -p hollowscript-cli)");
    return;
  }
  const snippets = JSON.parse(fs.readFileSync(path.join(ROOT, "snippets", "hollowscript.json"), "utf8"));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "hws-snippets-"));
  try {
    for (const [name, snippet] of Object.entries(snippets)) {
      const body = [snippet.body].flat().join("\n");
      const text = body
        .replace(/\$\{\d+\|([^,|]*)[^|]*\|\}/g, "$1")
        .replace(/\$\{\d+:([^{}]*(?:\{[^{}]*\}[^{}]*)*)\}/g, "$1")
        .replace(/\$\{\d+:([^{}]*)\}/g, "$1")
        .replace(/\$\d+/g, "");
      // A closure is an expression: it cannot begin a statement (section 18.3).
      const source = EXPRESSION_SNIPPETS.has(name) ? `constant value = (${text})` : text;
      const file = path.join(dir, "snippet.hws");
      fs.writeFileSync(file, source + "\n");
      /** @type {import("node:child_process").SpawnSyncReturns<string>} */
      const result = spawnSync(bin, ["format", file], { encoding: "utf8" });
      assert.equal(result.status, 0, `snippet '${name}' does not parse:\n${text}\n${result.stdout}${result.stderr}`);
    }
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the production bundle has no source map", () => {
  execFileSync(process.execPath, ["scripts/build.mjs"], { cwd: ROOT });
  assert.deepEqual(fs.readdirSync(path.join(ROOT, "dist")), ["extension.js"]);
  assert.ok(!fs.readFileSync(path.join(ROOT, "dist", "extension.js"), "utf8").includes("sourceMappingURL"));
});
