// Shared by the checks that instantiate a module themselves, to give it what
// the module imports as `env.memory`.
//
// The module declares the least memory it starts with and the most it can grow
// to, and a host creates the memory with exactly those (`hollowscript.js` does
// the same for `HollowScript.fromBytes`, and for the `tooling` build, whose
// 16 MiB shadow stack makes its minimum a little over 17 MiB). A check that
// builds a `HollowScript` or a `HollowScriptTooling` from raw exports needs
// the memory among them, because the module does not export what it imports.

import { readMemoryLimits } from "../hollowscript.js";

/**
 * Instantiates the module in `bytes` (or `compiled`, a `WebAssembly.Module`
 * made from the same bytes, to share the compilation) with `imports` and a
 * memory made from the limits the module declares. Answers `{ instance,
 * exports, memory }`, `exports` being the instance's exports with `memory`
 * among them.
 */
export async function instantiate(bytes, imports, compiled = undefined) {
  const limits = readMemoryLimits(bytes);
  if (limits === null) {
    const result = await WebAssembly.instantiate(compiled ?? bytes, imports);
    const instance = result instanceof WebAssembly.Instance ? result : result.instance;
    return { instance, exports: instance.exports, memory: instance.exports.memory };
  }
  const memory = new WebAssembly.Memory({
    initial: limits.initial,
    ...(limits.maximum === null ? {} : { maximum: limits.maximum }),
  });
  const withMemory = { ...imports, env: { ...imports.env, memory } };
  const result = await WebAssembly.instantiate(compiled ?? bytes, withMemory);
  const instance = result instanceof WebAssembly.Instance ? result : result.instance;
  return { instance, exports: { ...instance.exports, memory }, memory };
}
