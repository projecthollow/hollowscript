// Checks that what the VM writes out for a host is bounded: a value that
// shares its sub-values (fifty objects, about 2^50 paths) used to be written
// as JSON with no charge and no safepoint, so a small program could make the
// module allocate until it trapped. Now the result of a call, of a read and
// of a load, a queued command and the arguments of a capability call all stop
// at the output limit with HS1004, a cancellation reaches the writing, the
// instance carries on afterwards, and an `Int` above 2^53 - 1 crosses to a
// capability handler as a BigInt instead of as a different number.
//
// Usage: node bounds-check.mjs <vm.wasm> <tooling.wasm>
// The programs are compiled with the tooling build and run on the VM-only one.
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { strictEqual, deepStrictEqual } from "node:assert";
import { HollowScript, HollowScriptFault, HollowScriptTooling } from "../hollowscript.js";

const here = (name) => fileURLToPath(new URL(name, import.meta.url));
const vmBytes = await readFile(process.argv[2]);
const toolingBytes = await readFile(process.argv[3]);
const tooling = await HollowScriptTooling.fromBytes(toolingBytes);

const sinkCapabilities = [
  { name: "sink", params: [{ list: { list: { list: { list: "Int" } } } }] },
  { name: "echo", params: ["Int"], result: "Int" },
];

function compile(file, capabilities = []) {
  const source = readFileSync(here(file), "utf8");
  const diagnostics = tooling.check(source, file, capabilities);
  if (diagnostics.length > 0) {
    throw new Error(`${file}: ${JSON.stringify(diagnostics)}`);
  }
  return tooling.compile(source, file, capabilities);
}

import { readFileSync } from "node:fs";
const tree = compile("bounds-sample.hws");
const loadTree = compile("bounds-load-sample.hws");
const sink = compile("bounds-sink-sample.hws", sinkCapabilities);

const OUTPUT = 256 * 1024;
const MiB = 1024 * 1024;

function expectFault(run, code, what) {
  const started = performance.now();
  let error;
  try {
    run();
  } catch (e) {
    error = e;
  }
  const elapsed = performance.now() - started;
  if (!(error instanceof HollowScriptFault)) {
    throw new Error(`${what}: expected a HollowScriptFault, got ${error}`);
  }
  strictEqual(error.code, code, `${what}: ${error.message}`);
  console.log(`${what} -> ${error.code} after ${elapsed.toFixed(0)} ms`);
  if (elapsed > 5000) {
    throw new Error(`${what}: took ${elapsed} ms`);
  }
  return error;
}

// ---- results ---------------------------------------------------------------
{
  const vm = await HollowScript.fromBytes(vmBytes, { deadlineMs: 5000 });
  vm.setLimits({ steps: 1e9, memory: 32 * MiB, output: OUTPUT });

  const fault = expectFault(
    () => vm.loadProgram(loadTree, { resultFormat: "json" }),
    "HS1004",
    "load result",
  );
  strictEqual(fault.class, "resource");
  deepStrictEqual(
    { option: fault.data.option, limit: fault.data.limit },
    { option: "maxOutputBytes", limit: OUTPUT },
  );

  const instance = vm.loadProgram(tree, { resultFormat: "json" });
  expectFault(() => instance.call("tree"), "HS1004", "call result");
  expectFault(() => instance.read("held"), "HS1004", "read of a constant");
  // The run had finished, so the instance is as after any call that returned.
  strictEqual(instance.call("small"), 7);

  if (vm.memoryBytes > 64 * MiB) {
    throw new Error(`the module grew to ${vm.memoryBytes} bytes`);
  }
  console.log(`wasm memory after the three faults: ${(vm.memoryBytes / MiB).toFixed(1)} MiB`);

  // The text format keeps its own, older bound: cut off, not faulted.
  const text = vm.loadProgram(tree);
  const cut = text.call("tree");
  if (!cut.endsWith("(truncated: rendered output exceeds the limit)")) {
    throw new Error("the text result should end in the truncation marker");
  }
  text.dispose();
  instance.dispose();
}

// ---- a cancellation reaches the writing, as it reaches a run ----------------
{
  // The run is a few hundred steps, so it reaches no safepoint (the first is
  // at 1,024 steps) and never asks. The writing of its result asks every 1,024
  // pieces, so it is what sees the host's callback say stop, here on the third
  // ask. The output limit is far above anything the writing could reach in
  // that time, so only the cancellation can stop it.
  let asked = 0;
  const vm = await HollowScript.fromBytes(vmBytes, { shouldCancel: () => ++asked > 2 });
  vm.setLimits({ steps: 1e9, memory: 32 * MiB, output: 2 ** 40 });
  const instance = vm.loadProgram(tree, { resultFormat: "json" });
  asked = 0;
  expectFault(() => instance.call("tree"), "HS1006", "cancelled while writing a result");
  strictEqual(asked, 3, "the writing asks the host, and stops at the first yes");
  if (vm.memoryBytes > 64 * MiB) {
    throw new Error(`the module grew to ${vm.memoryBytes} bytes`);
  }
  instance.dispose();
}

// ---- capability arguments ---------------------------------------------------
{
  const vm = await HollowScript.fromBytes(vmBytes, { deadlineMs: 5000 });
  vm.setLimits({ steps: 1e9, memory: 32 * MiB, output: OUTPUT });
  const received = [];
  vm.bindCapability("sink", (args) => {
    received.push(args);
  });
  vm.bindCapability("echo", (args) => args[0]);
  const instance = vm.loadProgram(sink, { resultFormat: "json" });

  expectFault(() => instance.call("spread"), "HS1004", "shared capability argument");
  strictEqual(received.length, 0, "the handler must not be handed the oversized argument");

  // Distinct 64-bit Ints reach the handler distinct, as BigInts, and a safe
  // Int stays a number.
  instance.call("exact");
  strictEqual(received.length, 1);
  deepStrictEqual(received[0], [
    { list: [{ list: [{ list: [{ list: [9007199254740993n, 9223372036854775807n, 9007199254740991] }] }] }] },
  ]);

  // A handler can answer with the BigInt it was handed.
  strictEqual(instance.call("bounce", [9007199254740991]), 9007199254740991);
  instance.dispose();
}

// ---- the batched path -------------------------------------------------------
{
  const vm = await HollowScript.fromBytes(vmBytes, { deadlineMs: 5000 });
  vm.setLimits({ steps: 1e9, memory: 32 * MiB, output: OUTPUT });

  const exact = vm.loadProgramBatched(sink, { resultFormat: "json" });
  const first = exact.call("exact");
  strictEqual(first.suspended, true);
  deepStrictEqual(first.commands[0].args, [
    { list: [{ list: [{ list: [{ list: [9007199254740993n, 9223372036854775807n, 9007199254740991] }] }] }] },
  ]);
  exact.dispose();

  const spread = vm.loadProgramBatched(sink, { resultFormat: "json" });
  expectFault(() => spread.call("spread"), "HS1004", "shared queued command");
  spread.dispose();

  const result = vm.loadProgramBatched(tree, { resultFormat: "json" });
  expectFault(() => result.call("tree"), "HS1004", "batched result");
  result.dispose();
}

console.log("all checks passed");
