// Runs test/integration/suite in a real VS Code with this extension loaded
// from source (dist/ built by `npm run build:dev`), against the repository's
// own `hollowscript` build.
//
//   npm run test:integration
//
// VSCODE_EXECUTABLE   an installed VS Code (or Insiders) binary to use.
//                     Without it, @vscode/test-electron downloads the
//                     oldest version this extension supports into
//                     .vscode-test/, and VSCODE_VERSION overrides which.
// HOLLOWSCRIPT_BIN    the `hollowscript` to test against. Default: the
//                     repository's target/release, then target/debug.

import { runTests } from "@vscode/test-electron";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const repo = path.join(root, "..", "..");
const exe = process.platform === "win32" ? "hollowscript.exe" : "hollowscript";

const bin =
  process.env.HOLLOWSCRIPT_BIN ??
  [path.join(repo, "target", "release", exe), path.join(repo, "target", "debug", exe)].find(existsSync);
if (!bin || !existsSync(bin)) {
  console.error("No hollowscript binary: run 'cargo build --release -p hollowscript-cli -p hollowscript-lsp' first.");
  process.exit(2);
}

const userData = mkdtempSync(path.join(os.tmpdir(), "hws-vscode-user-"));
try {
  await runTests({
    extensionDevelopmentPath: root,
    extensionTestsPath: path.join(root, "test", "integration", "suite", "index.js"),
    ...(process.env.VSCODE_EXECUTABLE
      ? { vscodeExecutablePath: process.env.VSCODE_EXECUTABLE }
      : { version: process.env.VSCODE_VERSION ?? "1.91.0" }),
    launchArgs: [
      path.join(root, "test", "fixtures"),
      "--disable-extensions",
      "--disable-workspace-trust",
      "--skip-welcome",
      "--skip-release-notes",
      "--user-data-dir",
      userData,
    ],
    // The extension finds `hollowscript` on PATH exactly as it would for a
    // user who ran the installer.
    extensionTestsEnv: {
      PATH: `${path.dirname(bin)}${path.delimiter}${process.env.PATH ?? ""}`,
    },
  });
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  rmSync(userData, { recursive: true, force: true });
}
