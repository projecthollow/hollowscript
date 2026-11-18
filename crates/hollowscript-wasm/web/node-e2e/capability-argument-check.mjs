// Checks that the language, not the host, enforces the argument rule of a
// standard capability. Section 16.4: `random.int(min, upTo)` faults with HS1011
// if `min >= upTo`. A JavaScript handler can only fail with HS1012 (the status
// it returns carries a message, not a code), so the VM checks the arguments
// before it calls any host: the handler is never called for an empty range, the
// fault is the same one the command line and a Rust host give, and the same
// instance carries on afterwards. Synchronous and batched.
//
// Usage: node capability-argument-check.mjs <vm.wasm> <random-sample.hsb>
import { readFile } from "node:fs/promises";
import { strictEqual, ok } from "node:assert";
import { HollowScript, HollowScriptFault } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = new Uint8Array(await readFile(process.argv[3]));

function expectFault(what, run) {
  try {
    run();
  } catch (error) {
    ok(error instanceof HollowScriptFault, `${what}: expected a HollowScriptFault, got ${error?.name}: ${error?.message}`);
    return error;
  }
  throw new Error(`${what}: did not throw`);
}

// ---- synchronous: a handler that counts its calls ----
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const seen = [];
  // The naive handler a host would write: it does no checking of its own, so
  // before this rule `roll(5, 5)` quietly answered 5.
  vm.bindCapability("random.int", ([min, upTo]) => {
    seen.push([min, upTo]);
    return min + Math.floor(0.5 * (upTo - min));
  });
  const instance = vm.loadProgram(bytecode);

  for (const args of [[5, 5], [9, 2], [0, 0], [-3, -3], [2 ** 53 - 1, -(2 ** 53 - 1)]]) {
    const fault = expectFault(`roll(${args})`, () => instance.call("roll", args));
    strictEqual(fault.code, "HS1011", `roll(${args}) is ${fault.code}: ${fault.message}`);
    strictEqual(fault.class, "domain");
    ok(/random\.int/.test(fault.message), fault.message);
    strictEqual(seen.length, 0, `the handler was called for roll(${args})`);
  }
  console.log(`sync: empty ranges fault with HS1011, handler calls ${seen.length}`);

  // The same instance still works, and the handler is called once for a valid range.
  strictEqual(instance.call("roll", [1, 7]), "Int(4)");
  strictEqual(seen.length, 1);
  strictEqual(instance.call("roll", [3, 4]), "Int(3)", "a range of one value is valid");
  strictEqual(seen.length, 2);
  instance.dispose();
}

// ---- synchronous: a handler that throws still gives HS1012, the host's own failure ----
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  vm.bindCapability("random.int", () => {
    throw new RangeError("no entropy today");
  });
  const instance = vm.loadProgram(bytecode);
  const fault = expectFault("a failing handler", () => instance.call("roll", [1, 7]));
  strictEqual(fault.code, "HS1012");
  // And an empty range is still HS1011, whatever the handler would have done.
  strictEqual(expectFault("empty range, failing handler", () => instance.call("roll", [5, 5])).code, "HS1011");
  instance.dispose();
}

// ---- batched: the call is a fault, not a suspension ----
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const instance = vm.loadProgramBatched(bytecode);
  const fault = expectFault("batched roll(5, 5)", () => instance.call("roll", [5, 5]));
  strictEqual(fault.code, "HS1011");
  const valid = instance.call("roll", [1, 7]);
  strictEqual(valid.suspended, true, "a valid call still suspends");
  strictEqual(valid.commands.length, 1);
  strictEqual(valid.commands[0].name, "random.int");
  deepEqualArgs(valid.commands[0].args, [1, 7]);
  instance.dispose();
}

function deepEqualArgs(actual, expected) {
  strictEqual(JSON.stringify(actual), JSON.stringify(expected));
}

console.log("all checks passed");
