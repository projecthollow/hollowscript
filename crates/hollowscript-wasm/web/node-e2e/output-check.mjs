// Checks that what a host-bound `print` writes counts against `maxOutputBytes`
// (sections 11.6 and 16.2): the limit is checked before the host's function is
// called, so a call that would pass it writes nothing and the run faults with
// HS1004. Uses sample.hws, whose `run` prints "hello from wasm" (15 bytes and
// a line feed, 16 in all) and so needs the same capabilities main.mjs binds.
import { readFile } from "node:fs/promises";
import { strictEqual } from "node:assert";
import { HollowScript, HollowScriptFault } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = await readFile(process.argv[3]);

async function run(output) {
  const vm = await HollowScript.fromBytes(wasmBytes);
  let printed = "";
  vm.bindCapability("print", (args) => {
    printed += args[0] + "\n";
  });
  vm.bindCapability("clock.now", () => "2026-09-23T00:00:00Z");
  vm.bindCapability("clock.today", () => "2026-09-23");
  vm.bindCapability("cache", () => ({ Err: "no such key" }));
  if (output !== undefined) {
    vm.setLimits({ output });
  }
  const instance = vm.loadProgram(bytecode);
  try {
    instance.call("run");
    return { printed, fault: null };
  } catch (e) {
    return { printed, fault: e };
  } finally {
    instance.dispose();
  }
}

// The line and its line feed are 16 bytes, which fit a limit of 16 exactly.
const fits = await run(16);
strictEqual(fits.fault, null);
strictEqual(fits.printed, "hello from wasm\n");

// One byte less and the call writes nothing: the host's function is not called.
const over = await run(15);
console.log("output limit 15 ->", over.fault && over.fault.code, JSON.stringify(over.printed));
if (!(over.fault instanceof HollowScriptFault)) {
  throw new Error("a print over the output limit did not fault");
}
strictEqual(over.fault.code, "HS1004");
strictEqual(over.fault.class, "resource");
strictEqual(over.fault.data.option, "maxOutputBytes");
strictEqual(over.fault.data.limit, 15);
strictEqual(over.fault.data.used, 0);
strictEqual(over.printed, "", "the host's print was called for a line over the limit");

// With the default limit (16 MiB) an ordinary program is unaffected.
const normal = await run(undefined);
strictEqual(normal.fault, null);
strictEqual(normal.printed, "hello from wasm\n");

console.log("all checks passed");
