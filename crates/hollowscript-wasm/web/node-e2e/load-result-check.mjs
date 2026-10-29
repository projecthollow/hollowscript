// Checks that HollowScript.loadProgram exposes section 12.3's load result
// (the value a top-level `return` in the entry module produced), which used
// to be silently discarded. load-result-sample.hws needs no capability, so
// this uses `hollowscript build` directly, unlike sample.hws.
import { readFile } from "node:fs/promises";
import { HollowScript } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = await readFile(process.argv[3]);

const vm = await HollowScript.fromBytes(wasmBytes);
const instance = vm.loadProgram(bytecode);

console.log("loadResult ->", instance.loadResult);
if (instance.loadResult !== "Int(42)") {
  console.error(`FAIL: expected loadResult to be "Int(42)", got ${instance.loadResult}`);
  process.exitCode = 1;
}

instance.dispose();

if (process.exitCode === undefined || process.exitCode === 0) {
  console.log("all checks passed");
}
