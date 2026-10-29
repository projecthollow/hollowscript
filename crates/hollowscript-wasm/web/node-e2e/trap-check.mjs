// A check that a module that traps is marked dead and refused from then on.
//
// A trap on `wasm32-unknown-unknown` abandons the Rust frames it was in
// without unwinding, so the shadow-stack pointer is never restored and every
// later call would run on a corrupted stack. `web/hollowscript.js` therefore
// marks the whole instance dead at the first trap and throws a
// `HollowScriptTrap` from every later call, before it reaches the module.
//
// A real trap is produced by handing an export a pointer outside the module's
// memory, which is the one thing a host that follows the documented calling
// convention never does. Run it against a `tooling` .wasm and, optionally, the
// plain .wasm; see README.md in this directory.

import { readFile } from "node:fs/promises";
import { HollowScript, HollowScriptTooling, HollowScriptTrap } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

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

function expectTrap(what, run) {
  try {
    run();
  } catch (e) {
    if (!(e instanceof HollowScriptTrap)) fail(`${what}: expected a HollowScriptTrap, got ${e?.name}: ${e?.message}`);
    if (e.fatal !== true) fail(`${what}: the error is not marked fatal`);
    return e;
  }
  return fail(`${what}: did not throw`);
}

/** The module's exports with `name` replaced by a call that traps for real. */
function trapping(exports, name, calls) {
  const wrapped = {};
  for (const [key, value] of Object.entries(exports)) {
    wrapped[key] =
      typeof value === "function"
        ? (...args) => {
            calls.push(key);
            return value(...args);
          }
        : value;
  }
  wrapped[name] = (...args) => {
    calls.push(name);
    return exports[name](0xfffffff0, 64, ...args.slice(2));
  };
  return wrapped;
}

// ---- the tooling build ----
{
  const bytes = await readFile(process.argv[2]);
  const { exports } = await instantiate(bytes, imports);
  const calls = [];
  const t = new HollowScriptTooling(trapping(exports, "hs_wasm_check", calls));
  const analysis = t.analyse("return 1\n");
  if (t.dead) fail("a new instance is dead");
  const first = expectTrap("the trapping call", () => t.check("return 1"));
  if (!(first.cause instanceof WebAssembly.RuntimeError)) fail("the first error does not carry the trap as its cause");
  if (!t.dead) fail("the instance is not dead after a trap");
  const before = calls.length;
  const later = expectTrap("format after the trap", () => t.format("return 1"));
  expectTrap("compile after the trap", () => t.compile("return 1"));
  expectTrap("analyse after the trap", () => t.analyse("return 1"));
  expectTrap("a handle made before the trap", () => analysis.hover(0));
  if (later.cause !== first.cause) fail("a later error does not carry the first trap");
  if (calls.length !== before) fail("a call after the trap reached the module");
  console.log("tooling: the first trap threw a HollowScriptTrap, every later call was refused before the module");
}

// ---- the plain virtual machine build ----
if (process.argv[3]) {
  const bytes = await readFile(process.argv[3]);
  const { exports: moduleExports } = await instantiate(bytes, imports);
  const calls = [];
  const exports = trapping(moduleExports, "hs_wasm_capability_names", calls);
  const vm = new HollowScript(exports, null);
  if (vm.dead) fail("a new VM instance is dead");
  if (typeof vm.version() !== "string") fail("version() does not answer");
  expectTrap("the trapping call", () => vm.capabilityNames(new Uint8Array([1, 2, 3])));
  if (!vm.dead) fail("the VM instance is not dead after a trap");
  const before = calls.length;
  expectTrap("version after the trap", () => vm.version());
  expectTrap("setLimits after the trap", () => vm.setLimits({ steps: 10 }));
  if (calls.length !== before) fail("a call after the trap reached the module");
  console.log("vm: the first trap threw a HollowScriptTrap, every later call was refused before the module");
}

// ---- the guard does not mistake a host's own error for a trap ----
{
  const exports = {
    memory: new WebAssembly.Memory({ initial: 1 }),
    hs_wasm_version: () => {
      throw new Error("an ordinary failure");
    },
    hs_wasm_text_len: () => 0,
  };
  const vm = new HollowScript(exports, null);
  try {
    vm.version();
  } catch (e) {
    if (e instanceof HollowScriptTrap) fail("an ordinary Error was treated as a trap");
  }
  if (vm.dead) fail("an ordinary Error marked the instance dead");
}

console.log("all checks passed");
