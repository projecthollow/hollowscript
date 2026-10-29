import { parentPort, workerData } from "node:worker_threads";
import { readFile } from "node:fs/promises";
import { HollowScript } from "../hollowscript.js";

const cancelFlag = new Int32Array(workerData.cancelBuffer);
const wasmBytes = await readFile(workerData.wasmPath);
const bytecode = await readFile(workerData.bytecodePath);

const vm = await HollowScript.fromBytes(wasmBytes, { cancelFlag });

let printed = "";
vm.bindCapability("print", (args) => {
  printed += args[0];
});
// A namespace capability's two functions, proving JS binding is not
// special-cased to a single-function capability like "print".
vm.bindCapability("clock.now", () => "2026-09-23T00:00:00Z");
vm.bindCapability("clock.today", () => "2026-09-23");
// A capability declared Result<String, String> (section 8.2): the handler
// answers a plain JS object with one field, "Ok" or "Err", which
// hollowscript.js turns into the language's own Result value; the program
// pattern-matches it, so a "missing" answer is not a fault.
vm.bindCapability("cache", (args) => {
  const [key] = args;
  return key.includes("found") ? { Ok: "cached value" } : { Err: "no such key" };
});

const instance = vm.loadProgram(bytecode);
const runResult = instance.call("run");
const todayResult = instance.call("today");
const foundResult = instance.call("lookup", ["found-key"]);
const missingResult = instance.call("lookup", ["missing-key"]);

parentPort.postMessage({
  kind: "run-result",
  runResult,
  printed,
  todayResult,
  foundResult,
  missingResult,
});

// Now call the spinning export; the main thread sets the cancel flag from
// outside while this call is blocked inside the WebAssembly instance,
// which is exactly the situation postMessage cannot reach.
try {
  instance.call("spin");
  parentPort.postMessage({ kind: "spin-result", error: null });
} catch (error) {
  parentPort.postMessage({ kind: "spin-result", error: error.message });
}

instance.dispose();
