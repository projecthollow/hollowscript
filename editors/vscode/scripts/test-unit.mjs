// Enumerate files explicitly: directory arguments to `node --test` differ
// between supported Node releases, and shell glob expansion differs on Windows.
import { readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import path from "node:path";

const files = readdirSync("test/unit").filter((name) => name.endsWith(".test.js")).sort().map((name) => path.join("test/unit", name));
if (files.length === 0) throw new Error("No unit tests found");
const result = spawnSync(process.execPath, ["--test", ...files], { stdio: "inherit" });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
