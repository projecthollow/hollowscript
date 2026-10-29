// A deterministic mutation check of the container reader and the verifier, run
// against the WebAssembly module itself.
//
// Every four-byte window of a small container is overwritten in turn with the
// largest integers (0xFFFFFFFF, 0x7FFFFFFF and 0x80000000), the checksum is made
// valid again so that the mutant reaches the decoder and the verifier instead of
// stopping at the digest, and the module loads it. A mutant must be refused or
// load and run; it must never trap the module. A string id of 0xFFFFFFFF in a
// constant used to make the module resize a cache to four billion entries, which
// trapped it after a 316-byte container verified.
//
// Run it against the plain .wasm and one or more bytecode files; see README.md
// in this directory.

import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { HollowScript, HollowScriptTrap } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

const HEADER = 56;
const wasmBytes = await readFile(process.argv[2]);
const containers = await Promise.all(process.argv.slice(3).map((path) => readFile(path)));
if (containers.length === 0) {
  console.error("FAIL: give at least one bytecode file");
  process.exit(1);
}

const module = await WebAssembly.compile(wasmBytes);
const imports = {
  env: {
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: () => {},
    hs_wasm_poll_cancel: () => 0,
  },
};

/** Makes the SHA-256 in the header the digest of the rest. */
function fixChecksum(bytes) {
  const hash = createHash("sha256");
  hash.update(bytes.subarray(0, 24));
  hash.update(bytes.subarray(HEADER));
  bytes.set(hash.digest(), 24);
}

const hostile = [0xffffffff, 0x7fffffff, 0x80000000];
let loaded = 0;
let refused = 0;
const traps = [];
for (const [index, original] of containers.entries()) {
  for (let at = HEADER; at + 4 <= original.length; at += 1) {
    for (const value of hostile) {
      const mutant = new Uint8Array(original);
      new DataView(mutant.buffer).setUint32(at, value, true);
      fixChecksum(mutant);
      const { exports } = await instantiate(wasmBytes, imports, module);
      const vm = new HollowScript(exports, null);
      vm.setLimits({ steps: 100000, memory: 4000000 });
      try {
        const program = vm.loadProgram(mutant);
        loaded += 1;
        for (const name of ["main", "run", "spin"]) {
          try {
            program.call(name);
          } catch (e) {
            if (e instanceof HollowScriptTrap) throw e;
          }
        }
      } catch (e) {
        if (e instanceof HollowScriptTrap) {
          traps.push(`container ${index}, offset ${at}, value 0x${value.toString(16)}: ${e.cause?.message}`);
        } else {
          refused += 1;
        }
      }
    }
  }
}
console.log(`${loaded} mutants loaded, ${refused} refused, ${traps.length} trapped the module`);
if (traps.length > 0) {
  console.error(`FAIL: ${traps.slice(0, 5).join("\n      ")}`);
  process.exit(1);
}
console.log("all checks passed");
