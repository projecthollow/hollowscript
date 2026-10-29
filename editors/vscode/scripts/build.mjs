// Bundles src/extension.js and vscode-languageclient into dist/extension.js,
// the single file the published extension runs.
//
//   node scripts/build.mjs          production: minified, no source map
//   node scripts/build.mjs --dev    readable, with a source map
//
// dist/ is emptied first, so a production build never ships a source map
// left over from a development one.

import * as esbuild from "esbuild";
import { rmSync } from "node:fs";
import { options } from "./esbuild-options.mjs";

const production = !process.argv.includes("--dev");
rmSync("dist", { recursive: true, force: true });
await esbuild.build(options({ production }));
