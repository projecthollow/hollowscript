// Checks of the JavaScript wrapper's housekeeping:
//
// 1. a call whose arguments cannot be turned into JSON (a circular
//    object, a value whose `toJSON` throws) throws and leaves nothing behind in
//    the module's memory. The name buffer used to be uploaded first and never
//    freed when the conversion then threw.
// 2. an `Analysis` that is garbage collected without `dispose()` gives its place
//    back, so forgetting to dispose does not hold one of the module's 64 places
//    for good.
//
// Run it against the `tooling` .wasm, the plain .wasm and a bytecode file; see
// README.md in this directory. It enables the engine's `gc()` for itself.

import { readFile } from "node:fs/promises";
import v8 from "node:v8";
import vm from "node:vm";
import { HollowScript, HollowScriptTooling } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

const toolingBytes = await readFile(process.argv[2]);
const plainBytes = await readFile(process.argv[3]);
const bytecode = new Uint8Array(await readFile(process.argv[4]));

v8.setFlagsFromString("--expose-gc");
const gc = vm.runInNewContext("gc");

const imports = {
  env: {
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: () => {},
    hs_wasm_poll_cancel: () => 0,
  },
};

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

/** How much the memory grows while `run` is called `times` times, after a warm-up. */
function growth(memory, times, run) {
  for (let i = 0; i < 50; i += 1) run();
  const before = memory.buffer.byteLength;
  for (let i = 0; i < times; i += 1) run();
  return memory.buffer.byteLength - before;
}

// ---- 1. no buffer is left behind when the arguments cannot be stringified ----
{
  const { exports, memory } = await instantiate(plainBytes, imports);
  const wrapper = new HollowScript(exports, null);
  const program = wrapper.loadProgram(bytecode);
  const longName = "f".repeat(4000);
  const circular = {};
  circular.self = circular;
  const unwritable = {
    toJSON() {
      throw new Error("this value cannot be written as JSON");
    },
  };
  // A BigInt is written exactly where the platform has `JSON.rawJSON`, and
  // throws where it has not, so it is not one of the values that cannot be
  // written on every platform.
  for (const [what, args] of [
    ["a value whose toJSON throws", [unwritable]],
    ["a circular object", [circular]],
  ]) {
    let threw = false;
    const grown = growth(memory, 20000, () => {
      try {
        program.call(longName, args);
      } catch {
        threw = true;
      }
    });
    if (!threw) fail(`call with ${what} did not throw`);
    // 20,000 leaked names of 4,000 bytes would be 80 MB.
    if (grown > 1024 * 1024) fail(`call with ${what}: the memory grew by ${grown} bytes`);
    console.log(`call with ${what}: threw, memory grew by ${grown} bytes`);
  }
}
{
  const { exports, memory } = await instantiate(toolingBytes, imports);
  const tooling = new HollowScriptTooling(exports);
  const source = "// ".concat("x".repeat(10000), "\nreturn 1\n");
  const circular = {};
  circular.self = circular;
  for (const [what, call] of [
    ["circular capabilities", () => tooling.check(source, "main.hws", circular)],
    ["circular modules", () => tooling.check(source, "main.hws", [], circular)],
    ["a circular schema", () => tooling.check(source, "main.hws", [], {}, circular)],
    ["circular capabilities (analyse)", () => tooling.analyse(source, "main.hws", circular)],
  ]) {
    let threw = false;
    const grown = growth(memory, 5000, () => {
      try {
        call();
      } catch {
        threw = true;
      }
    });
    if (!threw) fail(`${what} did not throw`);
    // 5,000 leaked sources of 10,000 bytes would be 50 MB.
    if (grown > 1024 * 1024) fail(`${what}: the memory grew by ${grown} bytes`);
    console.log(`${what}: threw, memory grew by ${grown} bytes`);
  }
}

// ---- 2. a forgotten analysis is disposed when it is collected ----
{
  const tooling = await HollowScriptTooling.fromBytes(toolingBytes);
  // 64 are held at once at most, so five rounds of 60 forgotten ones only work
  // if each round is given back.
  for (let round = 0; round < 5; round += 1) {
    for (let i = 0; i < 60; i += 1) {
      tooling.analyse("return 1\n");
    }
    gc();
    // Finalisation callbacks run in a later task.
    await new Promise((resolve) => setTimeout(resolve, 20));
    gc();
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  console.log("five rounds of 60 forgotten analyses were all given back");

  // A disposed one is not disposed a second time by the finaliser: a handle the
  // module hands out is never reused, so a stale dispose would be harmless
  // either way, but it must not throw. And the cap is still the cap for ones
  // that are held.
  const held = [];
  let refused = null;
  try {
    for (let i = 0; i < 70; i += 1) held.push(tooling.analyse("return 1\n"));
  } catch (e) {
    refused = e;
  }
  if (held.length !== 64 || refused === null) {
    fail(`the cap is 64 live analyses, held ${held.length}`);
  }
  for (const analysis of held) analysis.dispose();
  tooling.analyse("return 1\n").dispose();
}

console.log("all checks passed");
