// Test source or an installed VSIX with an isolated profile. Control the host
// directly so Restricted Mode is not disabled by test-electron.
// HOLLOWSCRIPT_BIN, VSCODE_EXECUTABLE / VSCODE_VERSION, VSIX_PATH,
// TEST_RESTRICTED=1, TEST_MISSING_SERVER=1, TEST_KEEP_PROFILE=1.
import { downloadAndUnzipVSCode, resolveCliArgsFromVSCodeExecutablePath } from "@vscode/test-electron";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync, readdirSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const repo = path.resolve(root, "../..");
const exe = process.platform === "win32" ? "hollowscript.exe" : "hollowscript";
const bin = process.env.HOLLOWSCRIPT_BIN ?? [path.join(repo, "target/release", exe), path.join(repo, "target/debug", exe)].find(existsSync);
if ((!bin || !existsSync(bin)) && !process.env.TEST_MISSING_SERVER) throw new Error("Set HOLLOWSCRIPT_BIN to the released v0.1.1 CLI.");
const profile = mkdtempSync(path.join(os.tmpdir(), "hws-vscode-acceptance-"));
const userData = path.join(profile, "user-data");
const extensions = path.join(profile, "extensions");
mkdirSync(path.join(userData, "User"), { recursive: true });
writeFileSync(path.join(userData, "User/settings.json"), JSON.stringify({
  "security.workspace.trust.enabled": Boolean(process.env.TEST_RESTRICTED),
  "security.workspace.trust.startupPrompt": "never",
  "hollowscript.executablePath": process.env.TEST_MISSING_SERVER ? path.join(profile, "missing-hollowscript") : bin,
  "hollowscript.trace.server": "verbose",
}));

/** @param {string} command @param {string[]} args @param {NodeJS.ProcessEnv} env */
function run(command, args, env = process.env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: "inherit", env, shell: false });
    const timer = setTimeout(() => { child.kill(); reject(new Error("VS Code acceptance timed out")); }, 120000);
    child.on("error", (error) => { clearTimeout(timer); reject(error); });
    child.on("exit", (code) => { clearTimeout(timer); code === 0 ? resolve(code) : reject(new Error(`VS Code exited ${code}`)); });
  });
}

try {
  const executable = process.env.VSCODE_EXECUTABLE ?? await downloadAndUnzipVSCode({ version: process.env.VSCODE_VERSION ?? "1.91.0" });
  let extensionRoot = root;
  if (process.env.VSIX_PATH) {
    const [defaultCli, ...args] = resolveCliArgsFromVSCodeExecutablePath(executable, { reuseMachineInstall: true });
    const cli = process.env.VSCODE_CLI ?? defaultCli;
    const install = [...args, "--install-extension", path.resolve(process.env.VSIX_PATH), "--extensions-dir", extensions, "--user-data-dir", userData];
    if (process.platform === "win32") {
      const base = path.dirname(executable);
      const candidates = [path.join(base, "resources/app/out/cli.js"), ...readdirSync(base, { withFileTypes: true }).filter((entry) => entry.isDirectory()).map((entry) => path.join(base, entry.name, "resources/app/out/cli.js"))].filter(existsSync);
      if (candidates.length !== 1) throw new Error(`Expected one VS Code CLI entry point, found ${candidates.length}`);
      await run(executable, [candidates[0], ...install], { ...process.env, ELECTRON_RUN_AS_NODE: "1" });
    } else await run(cli, install);
    const installed = readdirSync(extensions).find((name) => name.startsWith("soulstack.hollowscript-"));
    if (!installed) throw new Error("The VSIX was not installed");
    extensionRoot = path.join(extensions, installed);
  }
  await run(executable, [
    path.join(root, "test/fixtures"),
    "--no-sandbox", "--disable-gpu-sandbox", "--disable-updates", "--skip-welcome", "--skip-release-notes",
    "--user-data-dir", userData, "--extensions-dir", extensions,
    `--extensionDevelopmentPath=${extensionRoot}`,
    `--extensionTestsPath=${path.join(root, "test/integration/suite/index.js")}`,
  ], { ...process.env, PATH: `${bin ? path.dirname(bin) : ""}${path.delimiter}${process.env.PATH ?? ""}`, TEST_EXTENSION_ROOT: extensionRoot });
} catch (error) {
  console.error(error);
  process.exitCode = 1;
} finally {
  if (process.env.TEST_KEEP_PROFILE) console.log(`Acceptance profile: ${profile}`);
  else rmSync(profile, { recursive: true, force: true });
}
