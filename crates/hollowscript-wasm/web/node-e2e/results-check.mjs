// Checks structured results and faults: `resultFormat: "json"` answers the
// value itself (an `Int` above 2^53 as a BigInt, so none is ever read back as
// a different one), and a fault is a `HollowScriptFault` with its code and
// position rather than only a message.
import { readFile } from "node:fs/promises";
import { deepStrictEqual, strictEqual } from "node:assert";
import { HollowScript, HollowScriptFault, byteOffsetToUtf16 } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const bytecode = await readFile(process.argv[3]);
const loadFaultBytecode = await readFile(process.argv[4]);

const BIG = 9007199254740993n;

// The text format is what it always was, and the default.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const instance = vm.loadProgram(bytecode);
  strictEqual(instance.loadResult, "[Int(1), Int(9007199254740993)]");
  strictEqual(instance.call("big"), "Int(9007199254740993)");
  strictEqual(instance.call("nothing"), null);
  strictEqual(instance.read("answer"), "Int(9007199254740993)");
  instance.dispose();
}

// The JSON format: the value itself, every Int exact.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const instance = vm.loadProgram(bytecode, { resultFormat: "json" });
  deepStrictEqual(instance.loadResult, { list: [1, BIG] });
  strictEqual(instance.call("small"), 9007199254740991);
  strictEqual(instance.call("big"), BIG);
  strictEqual(instance.call("negative"), -BIG);
  strictEqual(instance.call("floating"), 3);
  strictEqual(instance.call("word"), 'a"b');
  strictEqual(instance.call("absent"), null);
  strictEqual(instance.call("nothing"), undefined, "a function that returns nothing is undefined, not null");
  strictEqual(instance.read("answer"), BIG);
  deepStrictEqual(instance.call("numbers"), { list: [1, BIG] });
  deepStrictEqual(instance.call("point"), { record: { x: 1, y: BIG } });
  deepStrictEqual(instance.call("names"), { map: [[2, "two"], [BIG, "big"]] });
  deepStrictEqual(instance.call("tags"), { set: [2, BIG] });
  deepStrictEqual(instance.call("circle"), {
    enum: { name: "Shape", variant: "Circle", payload: [BIG] },
  });
  deepStrictEqual(instance.call("dot"), {
    enum: { name: "Shape", variant: "Dot", payload: [] },
  });
  // A record with a field called `bigint` is a record, not the tag.
  deepStrictEqual(instance.call("tricky"), { record: { bigint: "123" } });
  console.log("json results -> big:", instance.call("big"), "numbers:", JSON.stringify(instance.call("small")));

  // A result nested as deeply as a program can build one does not overflow
  // the host's stack while it is read back.
  const depth = 20000;
  let tree = instance.call("deep", [depth]);
  let seen = 0;
  while (tree.enum.variant === "Node") {
    tree = tree.enum.payload[0];
    seen += 1;
  }
  strictEqual(seen, depth);
  instance.dispose();
}

// A fault is an object with its code, class and position, and still an Error.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  const instance = vm.loadProgram(bytecode);
  let fault = null;
  try {
    instance.call("boom", [0]);
  } catch (e) {
    fault = e;
  }
  console.log("fault ->", fault.code, `${fault.file}:${fault.line}:${fault.column}`, fault.message);
  if (!(fault instanceof HollowScriptFault) || !(fault instanceof Error)) {
    throw new Error("a fault is not a HollowScriptFault");
  }
  strictEqual(fault.name, "HollowScriptFault");
  strictEqual(fault.code, "HS1008");
  strictEqual(fault.severity, "error");
  strictEqual(fault.class, "domain");
  strictEqual(fault.message, "Division by zero.");
  // The name the program was built under, and the line of `div(10, n)`.
  strictEqual(fault.file, "results-sample.hws");
  strictEqual(fault.line, 72);
  strictEqual(fault.column, 5);
  strictEqual(fault.data, null);
  if (!(fault.spanEndByte > fault.spanStartByte)) {
    throw new Error("a fault has no span");
  }
  deepStrictEqual(fault.frames, []);

  // A limit fault carries its data.
  vm.setLimits({ steps: 5000 });
  try {
    instance.call("spin");
    throw new Error("spin() should have faulted");
  } catch (e) {
    strictEqual(e.code, "HS1001");
    strictEqual(e.class, "resource");
    strictEqual(e.data.option, "maxSteps");
    strictEqual(e.data.limit, 5000);
    strictEqual(typeof e.data.used, "number");
  }

  // A call that is refused (not a fault of the program) stays a plain Error.
  try {
    instance.call("nothing_here");
    throw new Error("a missing export should be refused");
  } catch (e) {
    if (e instanceof HollowScriptFault) {
      throw new Error("a refused call is not a fault of the program");
    }
    strictEqual(e.message, "the program exports no function named 'nothing_here'");
  }

  // The instance is still usable, and a fault is not remembered by the next call.
  strictEqual(instance.call("small"), "Int(9007199254740991)");
  instance.dispose();
}

// A fault while loading is a HollowScriptFault too, and so is a cancelled run.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  try {
    vm.loadProgram(loadFaultBytecode);
    throw new Error("the load should have faulted");
  } catch (e) {
    if (!(e instanceof HollowScriptFault)) {
      throw e;
    }
    strictEqual(e.code, "HS1008");
    strictEqual(e.line, 5);
  }
  try {
    vm.loadProgram(new Uint8Array([1, 2, 3]));
    throw new Error("bytes that are not bytecode should be refused");
  } catch (e) {
    if (e instanceof HollowScriptFault) {
      throw new Error("a refused container is not a fault of the program");
    }
  }
  const watched = await HollowScript.fromBytes(wasmBytes, { deadlineMs: 50 });
  const instance = watched.loadProgram(bytecode);
  try {
    instance.call("spin");
    throw new Error("spin() should have been cancelled");
  } catch (e) {
    strictEqual(e.code, "HS1006");
    strictEqual(e.class, "resource");
  }
  instance.dispose();
}

// A bad format is refused.
{
  const vm = await HollowScript.fromBytes(wasmBytes);
  let refused = false;
  try {
    vm.loadProgram(bytecode, { resultFormat: "xml" });
  } catch (e) {
    refused = e instanceof RangeError;
  }
  strictEqual(refused, true);
}

// A fault's span is in bytes; the helper converts it for a string.
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 0), 0);
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 1), 1);
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 3), 1, "inside a character is the start of it");
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 5), 3);
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 6), 4);
strictEqual(byteOffsetToUtf16("a\u{1f600}b", 600), 4);
strictEqual(byteOffsetToUtf16("é€", 2), 1);
strictEqual(byteOffsetToUtf16("é€", 5), 2);

console.log("all checks passed");
