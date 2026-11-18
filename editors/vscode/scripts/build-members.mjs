// Build the canonical v0.1.1 language tooling used only for member completion.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const metadata = JSON.parse(execFileSync("cargo", ["metadata", "--no-deps", "--format-version", "1"], { cwd: repo, encoding: "utf8" }));
execFileSync("cargo", ["build", "--locked", "--profile", "dist", "--target", "wasm32-unknown-unknown", "-p", "hollowscript-wasm", "--features", "tooling"], { cwd: repo, stdio: "inherit" });
mkdirSync("dist", { recursive: true });
copyFileSync(path.join(metadata.target_directory, "wasm32-unknown-unknown/dist/hollowscript_wasm.wasm"), "dist/members.wasm");
