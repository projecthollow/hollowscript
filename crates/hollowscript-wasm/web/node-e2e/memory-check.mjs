// Checks the memory ceiling of the WebAssembly build. The language's own
// memory limit (`maxMemoryBytes`) counts what a value is worth by a formula,
// not what the module really holds, which is several times more and grows with
// how the value was built, so the module's linear memory is bounded by a
// maximum of its own: it imports `env.memory` declaring at most 256 MiB, the
// host cannot supply a memory with a larger maximum, and a program that
// allocates for real is stopped there. A failed allocation aborts Rust, which
// is a trap, and a module that has trapped is gone: the wrapper refuses every
// later call with `HollowScriptTrap`.
//
// Usage: node memory-check.mjs <vm.wasm> <tooling.wasm> [more vm .wasm files]
// Every file after the second is a further build of the VM (an optimised one,
// say) checked the same way.
import { readFile } from "node:fs/promises";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { strictEqual, ok, throws } from "node:assert";
import {
  HollowScript,
  HollowScriptFault,
  HollowScriptTooling,
  HollowScriptTrap,
  readMemoryLimits,
} from "../hollowscript.js";

const MiB = 1024 * 1024;
const PAGE = 65536;
const CEILING = 256 * MiB;

const [vmPath, toolingPath, ...extraPaths] = process.argv.slice(2);
const tooling = await HollowScriptTooling.fromBytes(await readFile(toolingPath));
const source = readFileSync(fileURLToPath(new URL("memory-sample.hws", import.meta.url)), "utf8");
const diagnostics = tooling.check(source, "memory-sample.hws");
strictEqual(diagnostics.length, 0, JSON.stringify(diagnostics));
const program = tooling.compile(source, "memory-sample.hws");

function noop() {}
const imports = (memory) => ({
  env: {
    memory,
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: noop,
    hs_wasm_poll_cancel: () => 0,
  },
});

async function checkBuild(path, label) {
  const bytes = await readFile(path);
  console.log(`-- ${label}: ${path}`);

  // What the module declares: it imports its memory, with a maximum of 256 MiB.
  const limits = readMemoryLimits(bytes);
  ok(limits !== null, "the module must import its memory");
  strictEqual(limits.maximum * PAGE, CEILING, "the declared maximum is the 256 MiB ceiling");
  console.log(`declared: initial ${limits.initial} pages, maximum ${limits.maximum} pages`);

  // The host cannot get around it: WebAssembly itself refuses a memory whose
  // maximum is larger, or that has none.
  const compiled = await WebAssembly.compile(bytes);
  for (const memory of [
    new WebAssembly.Memory({ initial: limits.initial, maximum: limits.maximum + 1 }),
    new WebAssembly.Memory({ initial: limits.initial }),
  ]) {
    let refused = false;
    try {
      await WebAssembly.instantiate(compiled, imports(memory));
    } catch (error) {
      refused = error instanceof WebAssembly.LinkError;
    }
    ok(refused, "a memory with a larger maximum, or none, must not link");
  }

  // The wrapper makes the memory, with the module's maximum by default and a
  // lower one when the host asks.
  const vm = await HollowScript.fromBytes(bytes);
  strictEqual(vm.memoryCeilingBytes, CEILING);
  const lower = await HollowScript.fromBytes(bytes, { maxMemoryBytes: 64 * MiB });
  strictEqual(lower.memoryCeilingBytes, 64 * MiB);
  const rounded = await HollowScript.fromBytes(bytes, { maxMemoryBytes: 64 * MiB + 1 });
  strictEqual(rounded.memoryCeilingBytes, 64 * MiB + PAGE);
  for (const bad of [CEILING + 1, (limits.initial - 1) * PAGE, 0, -1, 1.5, "64"]) {
    let refused;
    try {
      await HollowScript.fromBytes(bytes, { maxMemoryBytes: bad });
    } catch (error) {
      refused = error;
    }
    ok(refused instanceof RangeError, `maxMemoryBytes ${bad} must be a RangeError`);
  }

  // An ordinary program runs under either ceiling.
  for (const host of [vm, lower]) {
    const instance = host.loadProgram(program, { resultFormat: "json" });
    strictEqual(instance.call("small"), 7);
    instance.dispose();
  }

  // The language's own limit still faults first for a program that stays
  // inside the ceiling: a fault, not a trap, and the instance carries on.
  {
    const host = await HollowScript.fromBytes(bytes);
    host.setLimits({ steps: 1e9, memory: 32 * MiB });
    const instance = host.loadProgram(program, { resultFormat: "json" });
    let fault;
    try {
      instance.call("doubling", [40]);
    } catch (error) {
      fault = error;
    }
    ok(fault instanceof HollowScriptFault, `expected a fault, got ${fault}`);
    strictEqual(fault.code, "HS1002");
    strictEqual(instance.call("small"), 7);
    instance.dispose();
  }

  // The finding: 600,000 pairs is under a 32 MiB memory limit and well over
  // it in the module's memory. Report what it takes, as text and as JSON.
  for (const format of ["text", "json"]) {
    const host = await HollowScript.fromBytes(bytes);
    host.setLimits({ steps: 1e10, memory: 32 * MiB, output: 64 * MiB });
    const instance = host.loadProgram(program, { resultFormat: format });
    let outcome;
    try {
      instance.call("pairs");
      outcome = "returned";
    } catch (error) {
      outcome = error instanceof HollowScriptTrap ? "trapped" : `${error.constructor.name}`;
    }
    console.log(
      `600,000 pairs, ${format}: ${outcome}, module memory ${(host.memoryBytes / MiB).toFixed(0)} MiB of ${host.memoryCeilingBytes / MiB}`,
    );
    ok(host.memoryBytes <= CEILING);
  }

  // An allocation loop is stopped at the ceiling: the logical limit is
  // raised out of the way, so only the real ceiling can end it.
  for (const [host, ceiling] of [
    [await HollowScript.fromBytes(bytes), CEILING],
    [await HollowScript.fromBytes(bytes, { maxMemoryBytes: 64 * MiB }), 64 * MiB],
  ]) {
    host.setLimits({ steps: 1e10, memory: 2 ** 40, output: 1 * MiB });
    const instance = host.loadProgram(program, { resultFormat: "json" });
    const started = performance.now();
    let trap;
    try {
      instance.call("doubling", [60]);
    } catch (error) {
      trap = error;
    }
    const elapsed = performance.now() - started;
    ok(trap instanceof HollowScriptTrap, `expected a trap, got ${trap}`);
    ok(trap.cause instanceof WebAssembly.RuntimeError, "the trap is the cause");
    ok(host.memoryBytes <= ceiling, `${host.memoryBytes} is past the ceiling ${ceiling}`);
    ok(host.memoryBytes > ceiling / 4, `${host.memoryBytes} is nowhere near the ceiling ${ceiling}`);
    console.log(
      `allocation loop, ceiling ${ceiling / MiB} MiB: ${trap.cause.message} after ${elapsed.toFixed(0)} ms, module memory ${(host.memoryBytes / MiB).toFixed(0)} MiB`,
    );

    // After the failure the module is gone, and that is defined: every later
    // call is refused, the instance and its owner alike, and releasing what a
    // call allocated is a no-op, not a second error.
    for (const again of [
      () => instance.call("small"),
      () => host.loadProgram(program),
      () => host.version(),
      () => host.capabilityNames(program),
    ]) {
      throws(again, HollowScriptTrap);
    }
    instance.dispose();
    instance.dispose();
  }

  // A fresh instance has a fresh memory and is untouched by any of it.
  const fresh = await HollowScript.fromBytes(bytes);
  const instance = fresh.loadProgram(program, { resultFormat: "json" });
  strictEqual(instance.call("small"), 7);
  ok(fresh.memoryBytes < 16 * MiB);
  instance.dispose();
}

await checkBuild(vmPath, "VM only");
for (const extra of extraPaths) {
  await checkBuild(extra, "another build of the VM");
}

// The tooling build has the same contract, and can be given a lower ceiling.
{
  const bytes = await readFile(toolingPath);
  const limits = readMemoryLimits(bytes);
  strictEqual(limits.maximum * PAGE, CEILING);
  console.log(`-- tooling: declared initial ${limits.initial} pages, maximum ${limits.maximum} pages`);
  const lower = await HollowScriptTooling.fromBytes(bytes, { maxMemoryBytes: 64 * MiB });
  strictEqual(lower.check("return 1").length, 0);
  let refused;
  try {
    await HollowScriptTooling.fromBytes(bytes, { maxMemoryBytes: CEILING + PAGE });
  } catch (error) {
    refused = error;
  }
  ok(refused instanceof RangeError);

  // The module's `build.rs` carries both link arguments, the memory import with
  // its maximum and the 16 MiB stack of this build, and neither replaces the
  // other: the maximum is the ceiling above, and the shadow stack lives in the
  // linear memory, so the module cannot start with less than the stack.
  const STACK = 16 * MiB;
  ok(limits.initial * PAGE >= STACK, `the tooling module must declare at least its ${STACK} byte stack`);
  ok(limits.initial * PAGE < STACK + MiB, "the stack and the data are the whole of the initial memory");

  // A ceiling below what the module needs to start is refused by the wrapper
  // with an error that says so, before anything is instantiated: it is never a
  // trap, and the module is not left half made.
  for (const tooSmall of [PAGE, MiB, STACK, (limits.initial - 1) * PAGE]) {
    let error;
    try {
      await HollowScriptTooling.fromBytes(bytes, { maxMemoryBytes: tooSmall });
    } catch (e) {
      error = e;
    }
    ok(error instanceof RangeError, `maxMemoryBytes ${tooSmall} must be a RangeError`);
    ok(!(error instanceof HollowScriptTrap), "refusing a ceiling is not a trap");
    ok(
      error.message.includes(`${limits.initial * PAGE}`) && /needs to start/.test(error.message),
      `the error says what the module needs: ${error.message}`,
    );
  }
  console.log(
    `tooling: a ceiling below ${limits.initial * PAGE} bytes is refused: ${
      await HollowScriptTooling.fromBytes(bytes, { maxMemoryBytes: STACK }).catch((e) => e.message)
    }`,
  );

  // Exactly what it needs to start leaves it nothing to allocate with, so the
  // first call that allocates ends in the defined trap of a failed allocation
  // (`HollowScriptTrap`, the instance dead), not in a raw engine error. A
  // ceiling a few MiB above it is usable: the deepest legal nesting runs on the
  // 16 MiB stack, which is inside the memory the module starts with, and the
  // module answers.
  const exact = await HollowScriptTooling.fromBytes(bytes, { maxMemoryBytes: limits.initial * PAGE });
  let exactError;
  try {
    exact.check("return 1\n");
  } catch (e) {
    exactError = e;
  }
  ok(exactError instanceof HollowScriptTrap, `a ceiling with no headroom ends in a trap: ${exactError}`);
  strictEqual(exact.dead, true);
  const headroom = await HollowScriptTooling.fromBytes(bytes, {
    maxMemoryBytes: limits.initial * PAGE + 4 * MiB,
  });
  const nested = "return " + "fn() { return ".repeat(235) + "1" + " }".repeat(235) + "\n";
  strictEqual(headroom.check(nested).length, 0);
  strictEqual(headroom.format("return 1\n").diagnostics.length, 0);
  strictEqual(headroom.dead, false);
  console.log(
    `tooling: a ceiling of ${limits.initial * PAGE} bytes starts and traps on first use; with 4 MiB more it checks 235 nested closures`,
  );
}

// The VM-only module, too, is refused a ceiling below its own start, with the
// same clear error and no trap.
{
  const bytes = await readFile(vmPath);
  const limits = readMemoryLimits(bytes);
  for (const tooSmall of [PAGE, (limits.initial - 1) * PAGE]) {
    let error;
    try {
      await HollowScript.fromBytes(bytes, { maxMemoryBytes: tooSmall });
    } catch (e) {
      error = e;
    }
    ok(error instanceof RangeError && /needs to start/.test(error.message), `${error}`);
  }
  const headroom = await HollowScript.fromBytes(bytes, {
    maxMemoryBytes: limits.initial * PAGE + 4 * MiB,
  });
  strictEqual(headroom.dead, false);
  strictEqual(typeof headroom.version(), "string");
  console.log(`vm: a ceiling of ${limits.initial * PAGE} bytes (initial) is the least that is accepted; with 4 MiB more it answers`);
}

console.log("all checks passed");
