// The one esbuild configuration, shared by the build (scripts/build.mjs) and
// the notices generator (scripts/notices.mjs), so the notices always list
// exactly the packages the shipped bundle contains.

/**
 * @param {{ production: boolean }} mode
 * @returns {import("esbuild").BuildOptions}
 */
export function options({ production }) {
  return {
    entryPoints: ["src/extension.js"],
    bundle: true,
    format: "cjs",
    platform: "node",
    // VS Code 1.91, this extension's oldest supported version, runs
    // extensions on Node 20.
    target: "node20",
    outfile: "dist/extension.js",
    external: ["vscode"],
    // A source map is a development aid only; the published package has none.
    sourcemap: production ? false : "linked",
    minify: production,
    legalComments: production ? "none" : "inline",
    logLevel: "warning",
  };
}
