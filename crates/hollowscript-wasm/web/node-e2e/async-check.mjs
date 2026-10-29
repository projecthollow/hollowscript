// Simulates a host that cannot answer a capability call synchronously at
// all (an `await fetch(...)`, or a message round trip to a main thread with
// no `SharedArrayBuffer`): every "tick" capability call is answered after a
// real `setTimeout`, and the program (async-sample.hws) calls it eight
// times in a loop, so this also proves the same run can be suspended and
// resumed more than once, not just at its very first capability call.
import { readFile } from "node:fs/promises";
import { HollowScript, HollowScriptFault } from "../hollowscript.js";

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

const wasmBytes = await readFile(process.argv[2]);
const bytecode = await readFile(process.argv[3]);

// No cancelFlag: this path needs no SharedArrayBuffer at all.
const vm = await HollowScript.fromBytes(wasmBytes);
const instance = vm.loadProgramBatched(bytecode);

let ticks = 0;
let result = instance.call("run");
while (result.suspended) {
  const answers = [];
  for (const command of result.commands) {
    if (command.name !== "tick") {
      throw new Error(`unexpected capability call: ${command.name}`);
    }
    ticks += 1;
    // The actual async work: nothing here is blocking anything else in the
    // process while this awaits, which is the whole point of this path.
    await delay(5);
    const [n] = command.args;
    answers.push(n * n);
  }
  result = instance.resume(answers);
}

console.log("ticks ->", ticks);
console.log("result ->", result.value);

if (ticks !== 8) {
  console.error(`FAIL: expected 8 capability calls, got ${ticks}`);
  process.exitCode = 1;
}
// sum of i*i for i in 1..=8
const expected = [1, 2, 3, 4, 5, 6, 7, 8].reduce((sum, i) => sum + i * i, 0);
if (result.value !== `Int(${expected})`) {
  console.error(`FAIL: expected Int(${expected}), got ${result.value}`);
  process.exitCode = 1;
}

instance.dispose();

// The same run with structured results: the value itself, not its text.
{
  const json = vm.loadProgramBatched(bytecode, { resultFormat: "json" });
  let jsonResult = json.call("run");
  while (jsonResult.suspended) {
    jsonResult = json.resume(jsonResult.commands.map((command) => command.args[0] ** 2));
  }
  console.log("json result ->", jsonResult.value);
  if (jsonResult.value !== expected) {
    console.error(`FAIL: expected ${expected} in the JSON format, got ${jsonResult.value}`);
    process.exitCode = 1;
  }

  // A host that answers a call with an Error fails that call (HS1012), and
  // the fault is an object with its code, not only a message.
  const failing = json.call("run");
  try {
    json.resume([new Error("the service is down")]);
    console.error("FAIL: an Error answer should fault the run");
    process.exitCode = 1;
  } catch (error) {
    console.log("fault ->", error.code, error.message);
    if (!(error instanceof HollowScriptFault) || error.code !== "HS1012") {
      console.error(`FAIL: expected a HollowScriptFault with HS1012, got ${error}`);
      process.exitCode = 1;
    }
    if (!error.message.includes("the service is down")) {
      console.error(`FAIL: the host's own message is lost: ${error.message}`);
      process.exitCode = 1;
    }
  }
  if (!failing.suspended) {
    console.error("FAIL: the run did not suspend");
    process.exitCode = 1;
  }
  json.dispose();
}

if (process.exitCode === undefined || process.exitCode === 0) {
  console.log("all checks passed");
}
