// Writes THIRD-PARTY-NOTICES.md: the name, version, licence and full licence
// text of every npm package esbuild actually bundles into dist/extension.js,
// read from esbuild's own metafile rather than from `npm ls`, so a package
// that is installed but tree-shaken away is not listed and one that is
// bundled cannot be missed.
//
//   node scripts/notices.mjs           rewrite the file
//   node scripts/notices.mjs --check   exit 1 if the file is out of date

import * as esbuild from "esbuild";
import { readFileSync, readdirSync, writeFileSync, existsSync } from "node:fs";
import path from "node:path";
import { options } from "./esbuild-options.mjs";

const OUT = "THIRD-PARTY-NOTICES.md";

/** @returns {Promise<string>} */
export async function render() {
  const result = await esbuild.build({ ...options({ production: true }), write: false, metafile: true });
  const metafile = /** @type {import("esbuild").Metafile} */ (result.metafile);
  /** @type {Map<string, string>} package directory by name */
  const packages = new Map();
  for (const input of Object.keys(metafile.inputs)) {
    const match = /^(.*node_modules\/((?:@[^/]+\/)?[^/]+))\//.exec(input);
    if (match) packages.set(match[2], match[1]);
  }
  const names = [...packages.keys()].sort();
  const lines = [
    "# Third-party notices",
    "",
    "The HollowScript extension bundles the npm packages below into",
    "`dist/extension.js`. This list is generated from the bundler's own record",
    "of what it included (`npm run notices`), so it names exactly what ships.",
    "",
    "| Package | Version | Licence |",
    "|---|---|---|",
  ];
  /** @type {string[]} */
  const texts = [];
  for (const name of names) {
    const dir = /** @type {string} */ (packages.get(name));
    const manifest = JSON.parse(readFileSync(path.join(dir, "package.json"), "utf8"));
    lines.push(`| \`${name}\` | ${manifest.version} | ${manifest.license} |`);
    const licenceFile = readdirSync(dir).find((f) => /^(licen[cs]e|copying)(\.|$)/i.test(f));
    if (!licenceFile) throw new Error(`${name}: no licence file in ${dir}`);
    const text = readFileSync(path.join(dir, licenceFile), "utf8").replace(/\r\n/g, "\n").trim();
    texts.push(`## ${name} ${manifest.version}`, "", "```", text, "```", "");
  }
  return [...lines, "", ...texts].join("\n");
}

const text = await render();
if (process.argv.includes("--check")) {
  const current = existsSync(OUT) ? readFileSync(OUT, "utf8") : "";
  if (current !== text) {
    console.error(`${OUT} is out of date; run 'npm run notices'.`);
    process.exit(1);
  }
} else {
  writeFileSync(OUT, text);
}
