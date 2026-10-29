// A check that `setLimits` refuses what is not a limit and changes nothing when
// it does, and that the export behind it refuses the same.
//
// `setLimits({ steps: -1 })` used to reach the module as an unsigned 64-bit
// integer, 2^64 - 1, which is a limit nothing ever reaches. Run it against the
// plain .wasm and the endless loop `deadline-sample.hws` built to bytecode; see
// README.md in this directory.

import { readFile } from "node:fs/promises";
import { HollowScript, HollowScriptFault } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

const wasmBytes = await readFile(process.argv[2]);
const spin = new Uint8Array(await readFile(process.argv[3]));

const vm = await HollowScript.fromBytes(wasmBytes);

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

function refuses(limits, kind) {
  try {
    vm.setLimits(limits);
  } catch (e) {
    if (!(e instanceof kind)) fail(`${JSON.stringify(limits, (k, v) => (typeof v === "bigint" ? `${v}n` : v))}: expected a ${kind.name}, got ${e?.name}: ${e?.message}`);
    return;
  }
  fail(`${JSON.stringify(limits, (k, v) => (typeof v === "bigint" ? `${v}n` : v))}: was accepted`);
}

/** How many steps the endless loop gets: the limit it faults at. */
function stepLimitOfSpin() {
  const instance = vm.loadProgram(spin);
  try {
    instance.call("spin");
  } catch (e) {
    if (e instanceof HollowScriptFault && e.code === "HS1001") return e.data.limit;
    fail(`spin faulted with ${e?.code ?? e?.message}`);
  }
  return fail("spin did not fault");
}

vm.setLimits({ steps: 5000 });
if (stepLimitOfSpin() !== 5000) fail("the step limit was not set to 5000");

// Each of these is refused, and refuses the valid limit given beside it too.
refuses({ steps: -1 }, RangeError);
refuses({ steps: -1, output: 100 }, RangeError);
refuses({ memory: -5 }, RangeError);
refuses({ output: 1.5 }, RangeError);
refuses({ depth: NaN }, RangeError);
refuses({ depth: Infinity }, RangeError);
refuses({ steps: 2 ** 53 }, RangeError);
refuses({ steps: 2n ** 63n }, RangeError);
refuses({ steps: -1n }, RangeError);
refuses({ steps: "10" }, TypeError);
refuses({ steps: null }, TypeError);
refuses({ steps: {} }, TypeError);
if (stepLimitOfSpin() !== 5000) fail("a refused setLimits changed the step limit");

// The export refuses what a wrapper that did not check would pass it.
const imports = {
  env: {
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: () => {},
    hs_wasm_poll_cancel: () => 0,
  },
};
const { exports: moduleExports } = await instantiate(wasmBytes, imports);
for (const value of [0xffffffffffffffffn, 1n << 63n]) {
  const status = moduleExports.hs_wasm_set_limits(value, 0n, 0n, 0n);
  if (status === 0) fail(`hs_wasm_set_limits accepted ${value}`);
}
if (moduleExports.hs_wasm_set_limits((1n << 63n) - 1n, 0n, 0n, 0n) !== 0) {
  fail("hs_wasm_set_limits refused 2^63 - 1");
}

// The largest limit and 0 (unchanged) are fine, as numbers and as BigInts.
vm.setLimits({ steps: 7000n });
if (stepLimitOfSpin() !== 7000) fail("a BigInt limit was not set");
vm.setLimits({ steps: 0, memory: 0, output: 0, depth: 0 });
if (stepLimitOfSpin() !== 7000) fail("0 changed a limit");
vm.setLimits();
vm.setLimits({ steps: (1n << 63n) - 1n });
console.log("all checks passed");
