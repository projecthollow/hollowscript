// A check that the front end of the `tooling` build survives the deepest
// nesting section 1.10 allows, for every construct, through every entry point
// (`check`, `format`, `compile`, `analyse`), and that one step past the limit
// is the diagnostic HS0113 or HS0114 and not a trap of the module.
//
// `wasm32-unknown-unknown` has no threads, so the front end runs on the
// module's own shadow stack, which the linker sizes at 1 MiB unless told
// otherwise (`crates/hollowscript-wasm/build.rs` tells it 16 MiB). A program
// of 235 nested closures, a 3.7 KB source and within the 250-level limit, used
// to trap the module with `memory access out of bounds`. Run it against a
// `tooling` .wasm; see README.md in this directory.

import { readFile } from "node:fs/promises";
import { HollowScriptTooling, HollowScriptTrap } from "../hollowscript.js";
import { instantiate } from "./instantiate.mjs";

const wasmBytes = await readFile(process.argv[2]);
const mod = await WebAssembly.compile(wasmBytes);
const imports = {
  env: {
    hs_capability_call: () => 1,
    hs_capability_result_len: () => 0,
    hs_capability_take_result: () => {},
    hs_wasm_poll_cancel: () => 0,
  },
};

async function fresh() {
  const { exports } = await instantiate(wasmBytes, imports, mod);
  return new HollowScriptTooling(exports);
}

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

const links = (count) => "1 + ".repeat(count);

// One generator per construct, each nesting `n` levels deep.
const shapes = {
  closures: (n) => "return " + "fn() { return ".repeat(n) + "1" + " }".repeat(n) + "\n",
  closureInChain: (n) => "return " + "1 + 1 + 1 + fn() { return ".repeat(n) + "1" + " }".repeat(n) + "\n",
  closureCalls: (n) => {
    let s = "1";
    for (let i = 0; i < n; i += 1) s = `(fn(x: Int): Int { return x })(${s})`;
    return "return " + s + "\n";
  },
  parentheses: (n) => "return " + "(".repeat(n) + "1" + ")".repeat(n) + "\n",
  lists: (n) => "return " + "[".repeat(n) + "1" + "]".repeat(n) + "\n",
  records: (n) => "return " + "{ a: ".repeat(n) + "1" + " }".repeat(n) + "\n",
  blocks: (n) => "if true { ".repeat(n) + "return 1" + " }".repeat(n) + "\n",
  loops: (n) => "while true { ".repeat(n) + "break" + " }".repeat(n) + "\n",
  matches: (n) => "match 1 { 1 { ".repeat(n) + "return 1" + " } else { return 2 } }".repeat(n) + "\n",
  interpolation: (n) => {
    let s = "1";
    for (let i = 0; i < n; i += 1) s = '"${' + s + '}"';
    return "return " + s + "\n";
  },
  filters: (n) => "return `" + "(".repeat(n) + "status:open" + ")".repeat(n) + "`\n",
  types: (n) => "constant x: " + "[".repeat(n) + "Int" + "]".repeat(n) + " = []\n",
  typeArguments: (n) => "fn f(x: " + "Map<String, ".repeat(n) + "Int" + ">".repeat(n) + "): Int {\n return 1\n}\n",
  functionTypes: (n) => "fn f(x: " + "fn(".repeat(n) + "Int" + "): Int".repeat(n) + "): Int {\n return 1\n}\n",
  recordTypes: (n) => "fn f(x: " + "{ a: ".repeat(n) + "Int" + " }".repeat(n) + "): Int {\n return 1\n}\n",
  patterns: (n) =>
    "fn f(x: " + "[".repeat(n) + "Int" + "]".repeat(n) + "): Int {\n match x {\n " +
    "[".repeat(n) + "a" + "]".repeat(n) + " { return 1 }\n else { return 0 }\n }\n}\n",
  longChain: (n) => "return " + links(n) + "1\n",
  elseIfChain: (n) => "if true { return 1 }" + " else if true { return 2 }".repeat(n) + "\n",
  callChain: (n) => "fn f(): fn() { return f }\nreturn f" + "()".repeat(n) + "\n",
  weightedParens1: (n) => "return " + "(1 + ".repeat(n) + "1" + ")".repeat(n) + "\n",
  weightedParens3: (n) => "return " + "(1 + 1 + 1 + ".repeat(n) + "1" + ")".repeat(n) + "\n",
  weightedParens9: (n) => "return " + `(${links(9)}`.repeat(n) + "1" + ")".repeat(n) + "\n",
};

const operations = {
  check: (t, source) => t.check(source),
  format: (t, source) => t.format(source),
  compile: (t, source) => {
    try {
      t.compile(source);
    } catch (e) {
      if (e instanceof HollowScriptTrap) throw e;
    }
  },
  analyse: (t, source) => t.analyse(source).dispose(),
};

const isLimit = (d) => d.code === "HS0113" || d.code === "HS0114";

// The finding's own reproduction first: it traps, and then every call after it
// throws, on a build whose stack is the linker's default.
{
  const t = await fresh();
  const source = "return " + "fn() { return ".repeat(235) + "1" + " }".repeat(235) + "\n";
  const diagnostics = t.check(source);
  if (diagnostics.length !== 0) fail(`235 nested closures should check cleanly, got ${JSON.stringify(diagnostics)}`);
  if (t.dead) fail("the instance is dead after a legal program");
  if (t.check("return 1").length !== 0) fail("the instance does not work after 235 nested closures");
  console.log("235 nested closures: ok, and the instance still works");
}

// The deepest legal nesting of a shape: the largest n whose check answers no
// structural-limit diagnostic, found by bisection on an instance of its own.
async function deepestLegal(generate) {
  const t = await fresh();
  let low = 1;
  let high = 600;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if (t.check(generate(middle)).some(isLimit)) high = middle - 1;
    else low = middle;
  }
  return low;
}

for (const [name, generate] of Object.entries(shapes)) {
  const deepest = await deepestLegal(generate);
  // A shape whose limit is never reached in 600 levels is a shape the checker
  // refuses early for another reason; it still must not trap, which the loop
  // below checks at the depth found.
  for (const [operationName, operation] of Object.entries(operations)) {
    const t = await fresh();
    try {
      operation(t, generate(deepest));
      operation(t, generate(deepest + 1));
    } catch (e) {
      fail(`${name} at ${deepest} levels, ${operationName}: ${e.name}: ${e.message}`);
    }
    if (t.dead) fail(`${name}, ${operationName}: the instance is dead`);
    if (t.check("return 1").length !== 0) fail(`${name}, ${operationName}: the instance stopped working`);
  }
  const past = (await fresh()).check(generate(deepest + 1));
  if (deepest < 600 && !past.some(isLimit)) fail(`${name}: ${deepest + 1} levels did not answer HS0113 or HS0114`);
  console.log(`${name.padEnd(16)} deepest legal ${String(deepest).padStart(3)}: ok through check, format, compile and analyse`);
}

console.log("all checks passed");
