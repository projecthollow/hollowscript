// Checks a packaged .vsix before it is published anywhere:
//
// - it holds exactly the allow-listed files (see .vscodeignore), so no
//   source, test, source map or node_modules file slipped in;
// - Open VSX can read it: the manifest is parsed with ovsx's own reader,
//   the same code `ovsx publish` runs, and has the fields and licence the
//   registry requires;
// - it is small.
//
//   node scripts/check-vsix.mjs [hollowscript-<version>.vsix]

import { createRequire } from "node:module";
import { readFileSync, statSync } from "node:fs";

const require = createRequire(import.meta.url);
const { readZip, readVSIXPackage } = require("ovsx/lib/zip.js");
const { isLicenseOk } = require("ovsx/lib/check-license.js");

const pkg = JSON.parse(readFileSync("package.json", "utf8"));
const vsix = process.argv[2] ?? `${pkg.name}-${pkg.version}.vsix`;

const EXPECTED = [
  "[content_types].xml",
  "extension.vsixmanifest",
  "extension/changelog.md",
  "extension/dist/extension.js",
  "extension/images/icon.png",
  "extension/language-configuration.json",
  "extension/license-apache",
  "extension/license-mit",
  "extension/license.md",
  "extension/package.json",
  "extension/readme.md",
  "extension/snippets/hollowscript.json",
  "extension/syntaxes/hollowscript.tmlanguage.json",
  "extension/third-party-notices.md",
];

let failures = 0;
/** @param {string} name @param {boolean} ok @param {string} detail */
function check(name, ok, detail) {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: ${detail}`);
  if (!ok) failures += 1;
}

/** @type {Map<string, Buffer>} */
const entries = await readZip(vsix, () => true);
const names = [...entries.keys()].sort();
const unexpected = names.filter((n) => !EXPECTED.includes(n));
const missing = EXPECTED.filter((n) => !names.includes(n));
check("contents", unexpected.length === 0 && missing.length === 0, `unexpected [${unexpected.join(", ")}], missing [${missing.join(", ")}]`);
const maps = names.filter((n) => n.endsWith(".map"));
check("no source maps", maps.length === 0, maps.join(", ") || "none");
const bundle = entries.get("extension/dist/extension.js")?.toString("utf8") ?? "";
check("bundle has no source map reference", !bundle.includes("sourceMappingURL"), `${bundle.length} bytes`);

const manifest = await readVSIXPackage(vsix);
check(
  "Open VSX manifest",
  manifest.publisher === pkg.publisher && manifest.name === pkg.name && manifest.version === pkg.version,
  `${manifest.publisher}.${manifest.name}@${manifest.version}`
);
check("Open VSX licence", await isLicenseOk(".", manifest), String(manifest.license));
const preview = /** @type {{ preview?: boolean }} */ (manifest).preview;
check("preview flag", preview === true, String(preview));

const size = statSync(vsix).size;
check("size", size < 1024 * 1024, `${(size / 1024).toFixed(1)} KiB`);

console.log(failures === 0 ? `${vsix}: all checks passed` : `${vsix}: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);
