import { Worker } from "node:worker_threads";
import { fileURLToPath } from "node:url";
import path from "node:path";

const here = path.dirname(fileURLToPath(import.meta.url));
const cancelBuffer = new SharedArrayBuffer(4);
const cancelFlag = new Int32Array(cancelBuffer);

const worker = new Worker(path.join(here, "worker.mjs"), {
  workerData: {
    cancelBuffer,
    wasmPath: process.argv[2],
    bytecodePath: process.argv[3],
  },
});

let sawRunResult = false;
let sawSpinResult = false;

worker.on("message", (message) => {
  if (message.kind === "run-result") {
    sawRunResult = true;
    console.log("run() ->", message.runResult);
    console.log("printed ->", message.printed);
    if (message.runResult !== 'String("2026-09-23T00:00:00Z")') {
      console.error("FAIL: unexpected run() result");
      process.exitCode = 1;
    }
    if (message.printed !== "hello from wasm") {
      console.error("FAIL: print capability did not capture the argument");
      process.exitCode = 1;
    }
    console.log("today() ->", message.todayResult);
    if (message.todayResult !== 'String("2026-09-23")') {
      console.error("FAIL: clock.today() (the namespace's second function) did not answer correctly");
      process.exitCode = 1;
    }
    console.log("lookup(found) ->", message.foundResult);
    if (message.foundResult !== 'String("cached value")') {
      console.error("FAIL: a Result-typed capability's Ok(value) did not reach the program");
      process.exitCode = 1;
    }
    console.log("lookup(missing) ->", message.missingResult);
    if (message.missingResult !== 'String("missing: no such key")') {
      console.error("FAIL: a Result-typed capability's Err(reason) did not reach the program");
      process.exitCode = 1;
    }
    // Now trigger the spinning call and cancel it from this thread while
    // the worker is synchronously blocked running it.
    setTimeout(() => {
      Atomics.store(cancelFlag, 0, 1);
    }, 50);
  } else if (message.kind === "spin-result") {
    sawSpinResult = true;
    console.log("spin() error ->", message.error);
    if (!message.error || !message.error.includes("cancelled")) {
      console.error("FAIL: spin() was not cancelled with the expected message");
      process.exitCode = 1;
    }
    worker.terminate();
  }
});

worker.on("error", (error) => {
  console.error("FAIL: worker error", error);
  process.exitCode = 1;
});

worker.on("exit", () => {
  if (!sawRunResult || !sawSpinResult) {
    console.error("FAIL: did not see both results");
    process.exitCode = 1;
  }
  if (process.exitCode === undefined || process.exitCode === 0) {
    console.log("all checks passed");
  }
});
