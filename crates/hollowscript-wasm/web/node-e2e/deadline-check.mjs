// Checks the ways a host without a Worker (or with one) stops a run: the
// wall-clock `deadlineMs`, the `shouldCancel` callback, and that the shared
// cancel flag still works beside them. Everything here runs on one thread:
// the program is a tight loop that never yields, so the only thing that can
// end it is the VM asking the host at a safepoint.
import { readFile } from "node:fs/promises";
import { HollowScript, createCancelFlag, cancel } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = await readFile(process.argv[3]);
const loadBytecode = await readFile(process.argv[4]);

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

function timed(run) {
  const start = performance.now();
  let error = null;
  try {
    run();
  } catch (e) {
    error = e;
  }
  return { error, elapsed: performance.now() - start };
}

// A deadline: an endless loop is stopped quickly with the cancelled fault.
{
  const vm = await HollowScript.fromBytes(wasmBytes, { deadlineMs: 100 });
  const instance = vm.loadProgram(bytecode);
  const { error, elapsed } = timed(() => instance.call("spin"));
  console.log(`deadlineMs 100: spin() -> ${error && error.message} after ${elapsed.toFixed(0)} ms`);
  if (!error || !error.message.includes("cancelled")) {
    fail("a deadline did not stop the loop with the cancelled fault");
  }
  if (elapsed < 90) {
    fail(`the deadline fired early, after ${elapsed} ms`);
  }
  if (elapsed > 2000) {
    fail(`the deadline did not stop the run in good time: ${elapsed} ms for a 100 ms budget`);
  }
  // The deadline is per run: the next call has a fresh budget and works.
  const answer = instance.call("quick", [41]);
  if (answer !== "Int(42)") {
    fail(`the instance was not usable after a deadline, quick(41) -> ${answer}`);
  }
  instance.dispose();
}

// setDeadline changes the budget of every run that follows, and null removes it.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const instance = vm.loadProgram(bytecode);
  vm.setDeadline(50);
  const { error, elapsed } = timed(() => instance.call("spin"));
  if (!error || !error.message.includes("cancelled") || elapsed > 2000) {
    fail("setDeadline(50) did not stop the loop quickly");
  }
  vm.setDeadline(null);
  vm.setLimits({ steps: 5000 });
  const limited = timed(() => instance.call("spin"));
  if (!limited.error || !limited.error.message.includes("step limit")) {
    fail(`with no deadline the step limit should end the loop, got ${limited.error && limited.error.message}`);
  }
  instance.dispose();
}

// A deadline also ends a program whose load itself never finishes.
{
  const vm = await HollowScript.fromBytes(wasmBytes, { deadlineMs: 100 });
  const { error, elapsed } = timed(() => vm.loadProgram(loadBytecode));
  console.log(`deadlineMs 100: loadProgram() -> ${error && error.message} after ${elapsed.toFixed(0)} ms`);
  if (!error || !error.message.includes("cancelled") || elapsed > 2000) {
    fail("a deadline did not stop a load that never finishes");
  }
}

// The batched path honours the same deadline.
{
  const vm = await HollowScript.fromBytes(wasmBytes, { deadlineMs: 100 });
  const instance = vm.loadProgramBatched(bytecode);
  const { error, elapsed } = timed(() => instance.call("spin"));
  if (!error || !error.message.includes("cancelled") || elapsed > 2000) {
    fail("a deadline did not stop a batched call");
  }
  instance.dispose();
}

// shouldCancel: asked at safepoints, here after a fixed number of asks, which
// is deterministic and independent of the machine's speed.
{
  let asked = 0;
  const vm = await HollowScript.fromBytes(wasmBytes, {
    shouldCancel: () => {
      asked += 1;
      return asked >= 25;
    },
  });
  const instance = vm.loadProgram(bytecode);
  asked = 0;
  const { error } = timed(() => instance.call("spin"));
  console.log(`shouldCancel: spin() -> ${error && error.message} after ${asked} polls`);
  if (!error || !error.message.includes("cancelled")) {
    fail("shouldCancel did not stop the loop");
  }
  if (asked !== 25) {
    fail(`the VM kept running after shouldCancel answered true: ${asked} polls`);
  }
  instance.dispose();
}

// A shouldCancel that throws cancels the run instead of crossing into the VM.
{
  const vm = await HollowScript.fromBytes(wasmBytes, {
    shouldCancel: () => {
      throw new Error("the host's own bug");
    },
  });
  const instance = vm.loadProgram(bytecode);
  const { error } = timed(() => instance.call("spin"));
  if (!error || !error.message.includes("cancelled")) {
    fail("a throwing shouldCancel did not cancel the run");
  }
  // The module is still enterable: nothing trapped.
  const answer = instance.call("quick", [1]);
  if (answer !== "Int(2)") {
    fail(`the instance was not usable after a throwing shouldCancel, got ${answer}`);
  }
  instance.dispose();
}

// The existing flag path keeps working, and is consulted first.
{
  const cancelFlag = createCancelFlag();
  let asked = 0;
  const vm = await HollowScript.fromBytes(wasmBytes, {
    cancelFlag,
    shouldCancel: () => {
      asked += 1;
      return false;
    },
  });
  const instance = vm.loadProgram(bytecode);
  cancel(cancelFlag);
  asked = 0;
  const { error, elapsed } = timed(() => instance.call("spin"));
  if (!error || !error.message.includes("cancelled") || elapsed > 2000) {
    fail("the shared flag no longer stops a run");
  }
  if (asked !== 0) {
    fail("shouldCancel was asked although the flag was already set (section 11.7: token first)");
  }
  instance.dispose();
}

// Bad options are refused, not turned into a deadline that cannot fire.
for (const bad of [0, -5, Number.NaN, Number.POSITIVE_INFINITY, "100"]) {
  let refused = false;
  try {
    await HollowScript.fromBytes(wasmBytes, { deadlineMs: bad });
  } catch (e) {
    refused = e instanceof RangeError;
  }
  if (!refused) {
    fail(`deadlineMs ${String(bad)} was accepted`);
  }
}
{
  let refused = false;
  try {
    await HollowScript.fromBytes(wasmBytes, { shouldCancel: true });
  } catch (e) {
    refused = e instanceof TypeError;
  }
  if (!refused) {
    fail("a shouldCancel that is not a function was accepted");
  }
}

console.log("all checks passed");
