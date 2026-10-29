// A check that loading and disposing programs in a loop does not grow the
// module's memory, and that a handle to a disposed instance stays refused
// once its place has been reused.
//
// Every load used to add an entry to the module's table of instances that a
// dispose emptied but never reused, about 1.5 KiB for each, 144 MiB in 100,000
// cycles. The table now reuses an emptied place, and a handle carries a
// generation so that a handle to what used to be in the place is refused.
// Run it against the plain .wasm and a bytecode file; see README.md in this
// directory.

import { readFile } from "node:fs/promises";
import { HollowScript } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = new Uint8Array(await readFile(process.argv[3]));
const cycles = Number(process.argv[4] ?? 20000);

const imports = {
  env: {
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: () => {},
    hs_wasm_poll_cancel: () => 0,
  },
};
const { exports, memory } = await instantiate(wasmBytes, imports);
const vm = new HollowScript(exports, null);

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

// Warm up, so that what is measured is growth and not the first allocations.
for (let i = 0; i < 100; i += 1) {
  vm.loadProgram(bytecode).dispose();
}
const before = memory.buffer.byteLength;
for (let i = 0; i < cycles; i += 1) {
  vm.loadProgram(bytecode).dispose();
}
const after = memory.buffer.byteLength;
const grown = after - before;
console.log(`${cycles} loads and disposes: memory ${before} -> ${after} bytes (grew ${grown})`);
// A leak of 1.5 KiB a cycle is 30 MB over 20,000; allow for one page-sized
// step of the allocator and nothing like that.
if (grown > 1024 * 1024) {
  fail(`the module's memory grew by ${grown} bytes over ${cycles} loads and disposes`);
}

// A stale handle is refused after its position is reused.
const first = vm.loadProgram(bytecode);
first.dispose();
const second = vm.loadProgram(bytecode);
first.dispose();
if (second.loadResult !== "Int(42)") {
  fail(`the second instance's load result is ${second.loadResult}`);
}
second.dispose();
console.log("all checks passed");
