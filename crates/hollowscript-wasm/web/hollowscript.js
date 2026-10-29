// A small wrapper over the WebAssembly build of the HollowScript VM.
//
// The module exports a hand-written C surface over its own linear memory
// (see `src/abi.rs`); this turns that into something a page can use. There
// is no generated glue and no build step beyond `cargo build --target
// wasm32-unknown-unknown`: the whole boundary is the functions this file
// calls, plus the four it supplies as imports (`env.hs_capability_call`,
// `env.hs_capability_result_len`, `env.hs_capability_take_result`,
// `env.hs_wasm_poll_cancel`), documented alongside `web/README.md`, and the
// linear memory it imports (`env.memory`), which this file creates with the
// maximum the module declares (256 MiB) or a lower one the host asks for.
//
// Values cross as text. A result comes back in the rendering
// `corpus/FORMAT.md` fixes, and arguments go in as a JSON array of scalars,
// because a caller outside the VM cannot hold a value of a container kind:
// it is a handle into one instance's heap. A capability's arguments and
// result cross the same way, in both directions.
//
// A Rust panic inside the module is a defect, never something a program
// can trigger by itself (see `docs/design/security.md`), but the
// `wasm32-unknown-unknown` target this module is built for has no
// unwinding support: a panic there is a `trap`, which does not return
// control to whichever exported function was called and leaves no
// promise about the state of anything the interpreter was working on (the
// shadow-stack pointer is never restored, so every later call would start
// from a corrupted stack). This file therefore watches every call into the
// module: the first one that traps (a `WebAssembly.RuntimeError`, which is
// also what a program that allocates past the memory ceiling ends in, since
// Rust aborts when an allocation fails, or the engine's own stack overflow) marks the whole `HollowScript` or
// `HollowScriptTooling` instance dead, not only the `Instance`,
// `AsyncInstance` or `Analysis` handle that was mid-call, throws a
// `HollowScriptTrap`, and makes every later call on anything that came from
// that instance throw a `HollowScriptTrap` too, before it reaches the
// module. A host that sees one discards the instance and loads a fresh one
// (`load`/`fromBytes`).

const encoder = new TextEncoder();
const decoder = new TextDecoder();

const WASM_PAGE = 65536;

/**
 * The limits of the linear memory a WebAssembly module imports as
 * `env.memory`, read from its import section: `{ initial, maximum }` in
 * pages of 64 KiB, `maximum` being `null` when the module declares none, or
 * `null` altogether for a module that imports no memory (one that defines
 * its own). The JavaScript API cannot report these before a memory is made,
 * and a memory has to be made first, with at least `initial` pages and no
 * more than `maximum`, so this reads them from the bytes.
 */
export function readMemoryLimits(bytes) {
  const data = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let at = 8; // the magic number and the version
  const u32 = () => {
    let result = 0;
    let scale = 1;
    for (;;) {
      if (at >= data.length) {
        throw new Error("the WebAssembly module is truncated");
      }
      const byte = data[at];
      at += 1;
      result += (byte & 0x7f) * scale;
      if ((byte & 0x80) === 0) {
        return result;
      }
      scale *= 128;
    }
  };
  const name = () => {
    const length = u32();
    at += length;
  };
  const limits = () => {
    const flags = data[at];
    at += 1;
    const initial = u32();
    return { initial, maximum: (flags & 1) !== 0 ? u32() : null };
  };
  while (at < data.length) {
    const id = data[at];
    at += 1;
    const size = u32();
    const end = at + size;
    if (id !== 2) {
      at = end;
      continue;
    }
    const count = u32();
    for (let i = 0; i < count; i += 1) {
      name(); // module
      name(); // field
      const kind = data[at];
      at += 1;
      switch (kind) {
        case 0: // function: a type index
          u32();
          break;
        case 1: // table: a reference type and limits
          at += 1;
          limits();
          break;
        case 2: // memory
          return limits();
        case 3: // global: a value type and a mutability
          at += 2;
          break;
        case 4: // tag: an attribute and a type index
          at += 1;
          u32();
          break;
        default:
          throw new Error("the WebAssembly module has an import kind this reader does not know");
      }
    }
    return null;
  }
  return null;
}

/**
 * Compiles and instantiates the module in `bytes` with `imports`, giving it a
 * linear memory of its own making when it imports one: with the maximum the
 * module declares (so a program cannot make the module's memory grow past it,
 * and the host cannot ask for more), or `maxMemoryBytes` (rounded up to whole
 * pages) when the host wants a lower ceiling. Answers the guarded exports and
 * the ceiling in bytes.
 */
async function instantiateModule(bytes, imports, maxMemoryBytes) {
  const wanted = checkMaxMemoryBytes(maxMemoryBytes);
  const limits = readMemoryLimits(bytes);
  if (limits === null) {
    if (wanted !== null) {
      throw new RangeError("maxMemoryBytes was given, but this module defines its own memory");
    }
    const { instance } = await WebAssembly.instantiate(bytes, imports);
    return { exports: guardExports(instance.exports), ceilingBytes: null };
  }
  const declared = limits.maximum ?? 65536;
  let maximum = declared;
  if (wanted !== null) {
    maximum = Math.ceil(wanted / WASM_PAGE);
    if (maximum > declared) {
      throw new RangeError(
        `maxMemoryBytes ${wanted} is above the ${declared * WASM_PAGE} bytes this module allows`,
      );
    }
    if (maximum < limits.initial) {
      throw new RangeError(
        `maxMemoryBytes ${wanted} is below the ${limits.initial * WASM_PAGE} bytes this module needs to start`,
      );
    }
  }
  const memory = new WebAssembly.Memory({ initial: limits.initial, maximum });
  const { instance } = await WebAssembly.instantiate(bytes, {
    ...imports,
    env: { ...imports.env, memory },
  });
  return {
    exports: guardExports({ ...instance.exports, memory }),
    ceilingBytes: maximum * WASM_PAGE,
  };
}

/** Checks the `maxMemoryBytes` option: a positive safe integer, or `null`/`undefined` for none. */
function checkMaxMemoryBytes(maxMemoryBytes) {
  if (maxMemoryBytes === null || maxMemoryBytes === undefined) {
    return null;
  }
  if (!Number.isSafeInteger(maxMemoryBytes) || maxMemoryBytes <= 0) {
    throw new RangeError("maxMemoryBytes must be a positive integer number of bytes");
  }
  return maxMemoryBytes;
}

/** Fetches the module at `url` as bytes. */
async function fetchModule(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`could not fetch ${url}: ${response.status} ${response.statusText}`);
  }
  return new Uint8Array(await response.arrayBuffer());
}

/** A monotonic millisecond clock, for the wall-clock deadline. */
const now = () =>
  typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();

/**
 * Checks a deadline in milliseconds: a finite number greater than zero, or
 * `null`/`undefined` for none. Anything else is a host bug, refused here
 * rather than turned into a deadline that can never or always fire.
 */
function checkDeadlineMs(deadlineMs) {
  if (deadlineMs === null || deadlineMs === undefined) {
    return null;
  }
  if (typeof deadlineMs !== "number" || !Number.isFinite(deadlineMs) || deadlineMs <= 0) {
    throw new RangeError("deadlineMs must be a finite number of milliseconds greater than 0");
  }
  return deadlineMs;
}

/**
 * What a call into the module throws once the module has trapped, and on the
 * call that trapped it. A trap is not an ordinary failure: WebAssembly
 * abandons the Rust frames it was in without running anything, so the module
 * is left in a state nothing can vouch for. The instance (and every handle
 * made from it) is dead from then on; discard it and load a new one.
 *
 * - `cause`: the trap itself (a `WebAssembly.RuntimeError`, or a `RangeError`
 *   for the engine's own stack overflow), the first one the instance met
 * - `fatal`: always `true`; a host that handles errors generically can test it
 */
export class HollowScriptTrap extends Error {
  constructor(message, cause) {
    super(message, { cause });
    this.name = "HollowScriptTrap";
    this.fatal = true;
  }
}

/** Marks the guarded exports object, and holds the state of its trap. */
const GUARD = Symbol("hollowscript.guard");

/**
 * Whether `error` is the engine reporting that a call into WebAssembly died:
 * a `WebAssembly.RuntimeError` (a trap: an unreachable, an out of bounds
 * access, an integer overflow or a failed allocation that aborted) or a
 * `RangeError` the engine raises when WebAssembly overflows the native stack.
 * An error a host's own capability handler throws is neither.
 */
function isTrap(error) {
  if (typeof WebAssembly !== "undefined" && error instanceof WebAssembly.RuntimeError) {
    return true;
  }
  return error instanceof RangeError && /call stack/i.test(String(error.message));
}

/**
 * Wraps every function the module exports so that the first trap marks the
 * instance dead and every call after it is refused with a clear error, as the
 * file's header describes. Idempotent: an already guarded object is returned
 * as it is, so every handle made from one instance shares one state.
 */
function guardExports(exports) {
  if (exports[GUARD] !== undefined) {
    return exports;
  }
  const state = { trap: null };
  const guarded = { [GUARD]: state };
  for (const [name, value] of Object.entries(exports)) {
    if (typeof value !== "function") {
      guarded[name] = value;
      continue;
    }
    guarded[name] = (...args) => {
      if (state.trap !== null) {
        // Releasing something from a module that is gone does nothing, so
        // that a `finally` that frees a buffer, or a `dispose()`, does not
        // replace the error that matters.
        if (INERT_AFTER_TRAP.has(name)) {
          return 0;
        }
        throw new HollowScriptTrap(
          `this WebAssembly instance trapped earlier (${state.trap.message}) and cannot be used again; discard it and load a new one`,
          state.trap,
        );
      }
      try {
        return value(...args);
      } catch (error) {
        if (isTrap(error)) {
          state.trap = error;
          throw new HollowScriptTrap(
            `the WebAssembly module trapped (${error.message}) and cannot be used again; discard this instance and load a new one`,
            error,
          );
        }
        throw error;
      }
    };
  }
  return guarded;
}

/** The exports that do nothing, rather than throw, once the module has trapped. */
const INERT_AFTER_TRAP = new Set([
  "hs_wasm_free",
  "hs_wasm_dispose",
  "hs_wasm_analysis_dispose",
]);

/** Whether the instance behind `exports` has trapped. */
function hasTrapped(exports) {
  return exports[GUARD].trap !== null;
}

/** The largest limit of chapter 11: 2^63 - 1. */
const MAX_LIMIT = (1n << 63n) - 1n;

/**
 * Checks one limit given to `setLimits` and answers it as a `BigInt`, which is
 * what the module's 64-bit parameter takes. A `Number` must be a safe integer
 * (a larger limit is given as a `BigInt`, because a `Number` cannot hold it
 * exactly), and neither kind may be negative or above [`MAX_LIMIT`].
 */
function checkLimit(name, value) {
  let limit;
  if (typeof value === "bigint") {
    limit = value;
  } else if (typeof value === "number") {
    if (!Number.isSafeInteger(value)) {
      throw new RangeError(
        `${name} must be an integer no larger than 2^53 - 1 as a number (give a larger limit as a BigInt), not ${value}`,
      );
    }
    limit = BigInt(value);
  } else {
    throw new TypeError(`${name} must be a number or a BigInt`);
  }
  if (limit < 0n || limit > MAX_LIMIT) {
    throw new RangeError(`${name} must be from 0 (unchanged) to ${MAX_LIMIT}, not ${limit}`);
  }
  return limit;
}

/** The `hs_wasm_set_result_format` values: text (the default) and JSON. */
const FORMAT_TEXT = 0;
const FORMAT_JSON = 1;

/**
 * Checks the `resultFormat` option of `loadProgram` and
 * `loadProgramBatched`: `"text"` (the default) or `"json"`.
 */
function checkResultFormat(options) {
  const format = options?.resultFormat ?? "text";
  if (format === "text") {
    return FORMAT_TEXT;
  }
  if (format === "json") {
    return FORMAT_JSON;
  }
  throw new RangeError('resultFormat must be "text" or "json"');
}

/**
 * Whether `value` is `{ bigint: "<decimal digits>" }`, the form an `Int` that
 * a JavaScript number cannot hold exactly takes in a JSON result.
 */
function isBigIntTag(value) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const keys = Object.keys(value);
  return keys.length === 1 && keys[0] === "bigint" && typeof value.bigint === "string";
}

/** Sets `parent[key]`, as an own data property even for a key like `__proto__`. */
function put(parent, key, value) {
  Object.defineProperty(parent, key, {
    value,
    writable: true,
    enumerable: true,
    configurable: true,
  });
}

/**
 * Parses a JSON result (`hs_wasm_set_result_format(1)`) and turns every
 * `{ bigint: "..." }` in it into a `BigInt`, wherever the VM wrote an `Int`
 * above 2^53 - 1 in magnitude: a plain value, a list element, a set element,
 * a map key or value, a record field or an enum payload. The walk follows the
 * encoding's own structure rather than looking for the shape everywhere, so a
 * record whose field is named `bigint` is not mistaken for the tag, and it
 * keeps its own stack, because a program can build a result nested as deeply
 * as its memory limit allows.
 */
function parseResult(text) {
  return decodeExactInts(JSON.parse(text));
}

/**
 * The arguments of a capability call (a JSON array, one value of the encoding
 * for each argument), with every `{ bigint: "..." }` turned into a `BigInt`
 * as `parseResult` does for a result, so a handler is handed every `Int`
 * exactly: `9007199254740993` is not `9007199254740992`.
 */
function parseArguments(text) {
  const args = JSON.parse(text);
  for (let i = 0; i < args.length; i += 1) {
    args[i] = decodeExactInts(args[i]);
  }
  return args;
}

/**
 * Writes `value` as JSON text, with a `BigInt` as the exact integer literal
 * (`JSON.rawJSON`, where the platform has it), so that a handler can answer
 * with an `Int` it was handed above 2^53 - 1 and a host can pass one in.
 * The module's own reader keeps every digit of an integer.
 */
function toJson(value) {
  return JSON.stringify(value, (_key, item) => {
    if (typeof item !== "bigint") {
      return item;
    }
    if (typeof JSON.rawJSON !== "function") {
      throw new RangeError("this platform cannot write a BigInt as JSON; use a number");
    }
    return JSON.rawJSON(item.toString());
  });
}

/** `root`, a value of the JSON encoding, with each `{ bigint }` tag turned into a `BigInt`. */
function decodeExactInts(root) {
  const holder = { value: root };
  const positions = [[holder, "value"]];
  while (positions.length > 0) {
    const [parent, key] = positions.pop();
    const value = parent[key];
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      continue;
    }
    if (isBigIntTag(value)) {
      put(parent, key, BigInt(value.bigint));
      continue;
    }
    const keys = Object.keys(value);
    if (keys.length !== 1) {
      continue;
    }
    const tag = keys[0];
    const body = value[tag];
    switch (tag) {
      case "list":
      case "set":
        for (let i = 0; i < body.length; i += 1) {
          positions.push([body, i]);
        }
        break;
      case "map":
        for (const pair of body) {
          positions.push([pair, 0], [pair, 1]);
        }
        break;
      case "record":
        for (const field of Object.keys(body)) {
          positions.push([body, field]);
        }
        break;
      case "enum":
        for (let i = 0; i < body.payload.length; i += 1) {
          positions.push([body.payload, i]);
        }
        break;
      default:
        break;
    }
  }
  return holder.value;
}

/**
 * The result of a call in the format an instance was loaded with: the text
 * of `corpus/FORMAT.md` (or `null` for a function that returns nothing), or,
 * for `"json"`, the parsed value (or `undefined` for a function that returns
 * nothing, which is not the same as `null`, the value `none`).
 */
function decodeResult(text, format) {
  if (format === FORMAT_JSON) {
    return text === "" ? undefined : parseResult(text);
  }
  return text === "" ? null : text;
}

/**
 * A fault of the running program (chapter 8.1): the error a call, a resume
 * or a load throws when the program faults, in place of a bare `Error`. It is
 * an `Error`, so code that only reads `message` is unaffected, and it carries
 * the fault of section 17.1's diagnostic record:
 *
 * - `code`: the diagnostic code, `"HS1008"`; `"HS1006"` for a cancelled run
 * - `severity`: always `"error"`
 * - `message`: the text for a person
 * - `class`: section 8.1's class, `"resource"`, `"domain"`, `"host"` or
 *   `"internal"`
 * - `file`, `line`, `column`: where, 1-based, the column in code points; all
 *   `null` when the program was built without source positions
 * - `spanStartByte`, `spanEndByte`: the half-open span in bytes of that file's
 *   UTF-8 text (the VM holds no source, so it cannot count UTF-16 units;
 *   `byteOffsetToUtf16` converts one against the source the host kept)
 * - `frames`: the innermost call frames, `{ function, file, line, column,
 *   spanStartByte, spanEndByte }` each (empty until the VM reports them)
 * - `data`: for the limit faults HS1001 to HS1005, `{ option, limit, used }`
 *   (a `BigInt` where a number would not be exact), and `null` otherwise
 */
export class HollowScriptFault extends Error {
  constructor(fault) {
    super(fault.message);
    this.name = "HollowScriptFault";
    this.severity = fault.severity;
    this.code = fault.code;
    this.class = fault.class;
    this.file = fault.file;
    this.line = fault.line;
    this.column = fault.column;
    this.spanStartByte = fault.spanStartByte;
    this.spanEndByte = fault.spanEndByte;
    this.frames = fault.frames;
    this.data =
      fault.data === null
        ? null
        : {
            option: fault.data.option,
            limit: isBigIntTag(fault.data.limit) ? BigInt(fault.data.limit.bigint) : fault.data.limit,
            used: isBigIntTag(fault.data.used) ? BigInt(fault.data.used.bigint) : fault.data.used,
          };
  }
}

/**
 * Converts a byte offset into `text`, which is how a fault's span counts
 * (`spanStartByte`), into UTF-16 code units, which is how a JavaScript
 * string, and so a text editor, counts. An offset past the end gives the
 * length of `text`; one inside a character gives the start of that character.
 */
export function byteOffsetToUtf16(text, byteOffset) {
  let bytes = 0;
  let units = 0;
  for (const character of text) {
    const point = character.codePointAt(0);
    const width = point < 0x80 ? 1 : point < 0x800 ? 2 : point < 0x10000 ? 3 : 4;
    if (bytes + width > byteOffset) {
      break;
    }
    bytes += width;
    units += character.length;
  }
  return units;
}

/**
 * One loaded WebAssembly build of the VM, with the capability functions a
 * page has bound and, optionally, the ways a run is cancelled: the flag a
 * controller thread sets, a callback the VM asks at every safepoint, and a
 * wall-clock deadline for each run.
 */
export class HollowScript {
  #exports;
  #capabilities = new Map();
  #activeCapabilities = [];
  #pendingCapabilityResult = new Uint8Array(0);
  #cancelFlag;
  #shouldCancel;
  #deadlineMs;
  #deadlineAt = null;
  #ceilingBytes = null;

  constructor(exports, cancelFlag, { shouldCancel, deadlineMs, ceilingBytes = null } = {}) {
    this.#exports = guardExports(exports);
    this.#ceilingBytes = ceilingBytes;
    this.#cancelFlag = cancelFlag ?? null;
    this.#shouldCancel = HollowScript.#checkShouldCancel(shouldCancel);
    this.#deadlineMs = checkDeadlineMs(deadlineMs);
  }

  static #checkShouldCancel(shouldCancel) {
    if (shouldCancel === null || shouldCancel === undefined) {
      return null;
    }
    if (typeof shouldCancel !== "function") {
      throw new TypeError("shouldCancel must be a function returning a boolean");
    }
    return shouldCancel;
  }

  /**
   * Instantiates the module at `url`. Every run this instance performs (the
   * load of a program, each `call`, each `resume`) is cancellable, by any of
   * these options, all of which are optional and which compose: the first
   * to say stop ends the run with the fault HS1006, in the order of section
   * 11.7 (the token, then the host's callback).
   *
   * - `cancelFlag`: an `Int32Array` over a `SharedArrayBuffer` from
   *   `createCancelFlag()`, shared with whatever other thread may call
   *   `cancel(flag)` on it. For a host that runs the VM in a Worker.
   * - `shouldCancel`: a function called at every safepoint (at least once
   *   per 1,024 steps of the program's own work, and after every capability
   *   call) that answers `true` to stop the run. For a host with no Worker.
   *   It must be quick, and it must not call into the instance (HS1204). If
   *   it throws, the run is cancelled: a host's own bug never lets a run
   *   carry on unwatched.
   * - `deadlineMs`: a wall-clock budget in milliseconds for each run, from
   *   the moment the run starts (a `call`, a `resume`, or the load of a
   *   program) on a monotonic clock. Wall-clock time is not a language limit
   *   (section 11.1) but the host's job; this is the convenience for it.
   *   Time a batched run spends suspended, waiting for the host's answer, is
   *   not charged: each `call` and each `resume` has the whole budget.
   * - `maxMemoryBytes`: a ceiling on the module's linear memory, in bytes
   *   (rounded up to whole 64 KiB pages), lower than the 256 MiB the module
   *   declares and allows. The logical `memory` limit of `setLimits` is not a
   *   bound on this (the real footprint is several times larger and depends on
   *   how a value was built), so a host that runs scripts for others and
   *   cares how much memory one instance can take sets this. When an
   *   allocation fails at the ceiling the module traps: see
   *   `HollowScriptTrap`. Must be at least what the module needs to start
   *   (about 1.1 MiB for the VM, more for the `tooling` build) and at most
   *   the 256 MiB it declares, or a `RangeError`.
   */
  static async load(url, options = {}) {
    return HollowScript.fromBytes(await fetchModule(url), options);
  }

  /** Instantiates the module from bytes already in hand, as `load` does. */
  static async fromBytes(bytes, { cancelFlag, shouldCancel, deadlineMs, maxMemoryBytes } = {}) {
    return HollowScript.#instantiate(bytes, cancelFlag, {
      shouldCancel,
      deadlineMs,
      maxMemoryBytes,
    });
  }

  static async #instantiate(bytes, cancelFlag, options) {
    // The import functions close over `vm`, filled in once instantiation
    // has produced it: WebAssembly only ever calls an import after
    // instantiation finishes, by which point `vm` and its `#exports` (in
    // particular `memory`) exist, so this is not a use of a value before
    // it is set, only a definition written before its first use.
    let vm;
    // Validated before the module is fetched or compiled, so a bad option is
    // refused at once and not after the work of instantiating.
    HollowScript.#checkShouldCancel(options.shouldCancel);
    checkDeadlineMs(options.deadlineMs);
    const imports = {
      env: {
        hs_capability_call: (index, argsPtr, argsLen) =>
          vm.#handleCapabilityCall(index, argsPtr, argsLen),
        hs_capability_result_len: () => vm.#pendingCapabilityResult.length,
        hs_capability_take_result: (destPtr) => vm.#takeCapabilityResult(destPtr),
        hs_wasm_poll_cancel: () => vm.#pollCancel(),
      },
    };
    const { exports, ceilingBytes } = await instantiateModule(
      bytes,
      imports,
      options.maxMemoryBytes,
    );
    vm = new HollowScript(exports, cancelFlag, { ...options, ceilingBytes });
    return vm;
  }

  /**
   * Whether this instance has trapped, in which case every call on it, and
   * on any `Instance` or `AsyncInstance` made from it, throws a
   * `HollowScriptTrap`. Discard it and load a new one.
   */
  get dead() {
    return hasTrapped(this.#exports);
  }

  /**
   * The `env.hs_wasm_poll_cancel` import, asked at every safepoint:
   * non-zero stops the run with the fault HS1006. Consulted in the order of
   * section 11.7: the shared flag first, then the wall-clock deadline, then
   * the host's own callback. Never lets an exception cross into WebAssembly,
   * which would leave the module unable to be entered again: a callback that
   * throws cancels the run instead.
   */
  #pollCancel() {
    if (this.#cancelFlag && Atomics.load(this.#cancelFlag, 0) !== 0) {
      return 1;
    }
    if (this.#deadlineAt !== null && now() >= this.#deadlineAt) {
      return 1;
    }
    if (this.#shouldCancel !== null) {
      try {
        return this.#shouldCancel() ? 1 : 0;
      } catch {
        return 1;
      }
    }
    return 0;
  }

  /**
   * Sets the wall-clock budget of every run that follows, as the
   * `deadlineMs` option does, or removes it with `null`. A run already in
   * progress keeps the deadline it started with.
   */
  setDeadline(deadlineMs) {
    this.#deadlineMs = checkDeadlineMs(deadlineMs);
  }

  /**
   * The size of the module's linear memory in bytes, which only grows: the
   * real footprint of this instance, as opposed to the logical `memory`
   * limit of `setLimits` (the two are not the same, see `web/README.md`).
   */
  get memoryBytes() {
    return this.#exports.memory.buffer.byteLength;
  }

  /**
   * The most the module's linear memory can grow to, in bytes: the 256 MiB
   * the module declares, or the lower `maxMemoryBytes` the host asked for
   * (rounded up to whole pages). `null` for a module that defines its own
   * memory and so has no ceiling this wrapper knows.
   */
  get memoryCeilingBytes() {
    return this.#ceilingBytes;
  }

  /** The version of the VM build. */
  version() {
    this.#exports.hs_wasm_version();
    return this.#takeText();
  }

  /**
   * The resource limits of chapter 11 for every run that follows. A limit
   * left out, or given as 0, keeps the value it had. Each is a non-negative
   * integer, as a `Number` (up to 2^53 - 1) or a `BigInt`, and no more than
   * 2^63 - 1, the largest a limit can be: anything else (a negative number, a
   * fraction, `NaN`, a string, a value above that) throws a `RangeError` or a
   * `TypeError`, and none of the limits changes. A negative number used to be
   * passed to the module as an unsigned 64-bit integer, which made `steps: -1`
   * the largest limit there is.
   */
  setLimits({ steps = 0, memory = 0, output = 0, depth = 0 } = {}) {
    const values = Object.entries({ steps, memory, output, depth }).map(([name, value]) =>
      checkLimit(name, value),
    );
    const status = this.#exports.hs_wasm_set_limits(...values);
    if (status !== 0) {
      throw new RangeError(this.#takeText() || "the limits were refused");
    }
  }

  /**
   * Registers `handler` as a capability's implementation. `name` is a
   * capability's `uses` name ("print"), or "capability.function" for a
   * member of a namespace capability ("clock.now"), exactly as
   * `capabilityNames()` reports it. `handler` is called with a plain JS
   * array of the arguments (numbers, strings, booleans, null) and must
   * return a JSON-compatible scalar (or nothing, for a capability with no
   * result), or, for a capability declared Result<T, E> (section 8.2), a
   * plain object with exactly one field, { Ok: value } or { Err: value },
   * which the program sees as that Result value, not a fault. Throwing
   * fails the call itself, and the program sees the fault HS1012 with
   * `error.message`; this is different from answering { Err: ... }, which
   * is an ordinary value the program pattern-matches. A handler that throws
   * never lets the exception itself cross back into WebAssembly, which would leave the
   * module's internal state unable to be entered again: this wrapper
   * always catches it and reports a failure instead.
   */
  bindCapability(name, handler) {
    this.#capabilities.set(name, handler);
  }

  /**
   * The capability names `bytecode` calls, in the order it calls them,
   * which is the order a host must have bound with `bindCapability` before
   * `loadProgram`. Reading this needs no instance: it only decodes the
   * bytecode's own capability table, the same one `loadProgram` will bind
   * against.
   */
  capabilityNames(bytecode) {
    const [pointer, length] = this.#upload(bytecode);
    let status;
    try {
      status = this.#exports.hs_wasm_capability_names(pointer, length);
    } finally {
      this.#exports.hs_wasm_free(pointer, length);
    }
    const text = this.#takeText();
    if (status !== 0) {
      throw new Error(text || "the bytecode was refused");
    }
    return text === "" ? [] : JSON.parse(text);
  }

  /**
   * Verifies `bytecode` (a Uint8Array) and runs its initialisers. Throws
   * with the reason if the bytes are refused, a capability the program
   * calls has no handler bound, or initialisation faults (including with
   * HS1006 if `cancelFlag` was already set): the load-time fault, exactly
   * as `Instance.call`'s own fault is thrown. On success, the returned
   * `Instance`'s `loadResult` is section 12.3's load result: the rendered
   * value a top-level `return` in the entry module produced, or `null` for
   * a module that falls off the end without one.
   *
   * A fault, here and in every later `call`, is thrown as a
   * `HollowScriptFault`, an `Error` that also carries the diagnostic code and
   * the position.
   *
   * `resultFormat` is `"text"` (the default: every result is the rendering
   * `corpus/FORMAT.md` fixes, as a string) or `"json"`: every result of this
   * instance, its `loadResult`, `call` and `read`, is then the value itself,
   * parsed (see "Structured results" in `web/README.md`), and a function that
   * returns nothing answers `undefined`.
   */
  loadProgram(bytecode, options) {
    const format = checkResultFormat(options);
    const names = this.capabilityNames(bytecode);
    const handlers = names.map((name) => {
      const handler = this.#capabilities.get(name);
      if (handler === undefined) {
        throw new Error(
          `no capability handler is bound for '${name}'; call bindCapability('${name}', ...) before loadProgram`,
        );
      }
      return handler;
    });
    const [pointer, length] = this.#upload(bytecode);
    let handle;
    const previous = this.#activeCapabilities;
    this.#activeCapabilities = handlers;
    try {
      handle = this._run(format, () => this.#exports.hs_wasm_load(pointer, length));
    } finally {
      this.#exports.hs_wasm_free(pointer, length);
      this.#activeCapabilities = previous;
    }
    const text = this.#takeText();
    if (handle === 0) {
      throw this._failure(text || "the bytecode was refused");
    }
    const loadResult = decodeResult(text, format);
    return new Instance(this.#exports, handle, this, handlers, loadResult, format);
  }

  /**
   * As `loadProgram`, but on the batched (suspend/resume) path, for a host
   * that cannot answer a capability call synchronously at all: `await
   * fetch(...)`, or a message round trip to a main thread the page is not
   * cross-origin isolated enough to share a `SharedArrayBuffer` with (so
   * `HollowScript.load`'s `cancelFlag` and `bindCapability` cannot be used
   * to block a Worker on an answer). Returns an `AsyncInstance`, whose
   * `call`/`resume` answer `{ suspended: true, commands }` instead of
   * blocking, for as long as the program keeps calling a capability; no
   * `SharedArrayBuffer` is needed anywhere in this path, because nothing in
   * it ever blocks.
   *
   * Throws with the reason if the bytes are refused. Unlike `loadProgram`,
   * this never throws for an unbound capability: nothing is bound up
   * front, since every call is answered by the host's own code driving
   * `call`/`resume`, not by a registered handler.
   *
   * `resultFormat` is as `loadProgram`'s: with `"json"`, `loadResult` and the
   * `value` of every finished `call` and `resume` are parsed values.
   */
  loadProgramBatched(bytecode, options) {
    const format = checkResultFormat(options);
    const names = this.capabilityNames(bytecode);
    const [pointer, length] = this.#upload(bytecode);
    let handle;
    try {
      handle = this._run(format, () => this.#exports.hs_wasm_load_batched(pointer, length));
    } finally {
      this.#exports.hs_wasm_free(pointer, length);
    }
    const text = this.#takeText();
    if (handle === 0) {
      throw this._failure(text || "the bytecode was refused");
    }
    // The module's own top-level statements can themselves call a
    // capability, in which case the instance is already suspended the
    // moment it is handed back; `AsyncInstance` reads that starting state
    // the same way `call`/`resume` read what either of them left pending,
    // and there is no load result to report yet in that case (`resume`
    // will eventually answer one, once the run finishes).
    const initial = readPendingCommands(this.#exports, handle, names, this);
    const loadResult = decodeResult(initial.length > 0 ? "" : text, format);
    return new AsyncInstance(this.#exports, handle, this, names, initial, loadResult, format);
  }

  // ---- the boundary itself ----------------------------------------------

  /** Copies `bytes` into the module's memory; the caller frees it. */
  #upload(bytes) {
    const length = bytes.length;
    if (length === 0) {
      return [0, 0];
    }
    const pointer = this.#exports.hs_wasm_alloc(length);
    new Uint8Array(this.#exports.memory.buffer, pointer, length).set(bytes);
    return [pointer, length];
  }

  /** The text the last call produced, taken and released. */
  #takeText() {
    const length = Number(this.#exports.hs_wasm_text_len());
    if (length === 0) {
      return "";
    }
    const pointer = this.#exports.hs_wasm_take_text();
    const bytes = new Uint8Array(
      this.#exports.memory.buffer,
      pointer,
      length,
    ).slice();
    this.#exports.hs_wasm_free(pointer, length);
    return decoder.decode(bytes);
  }

  /**
   * The `env.hs_capability_call` import: dispatches to the handler this
   * instance's `#activeCapabilities` holds at `index`, which
   * `loadProgram`/`Instance.call` set for the length of the one VM call
   * that reaches this. Never lets a handler's exception cross back into
   * WebAssembly: it is caught here and turned into a failure result
   * instead, which is what keeps a page's own bug from leaving the module
   * unable to be entered again.
   */
  #handleCapabilityCall(index, argsPtr, argsLen) {
    const bytes = new Uint8Array(
      this.#exports.memory.buffer,
      argsPtr,
      argsLen,
    ).slice();
    let ok = true;
    let resultText;
    try {
      const handler = this.#activeCapabilities[index];
      if (handler === undefined) {
        throw new Error(`no capability handler is bound for index ${index}`);
      }
      const args = parseArguments(decoder.decode(bytes));
      const result = handler(args);
      resultText = toJson(result === undefined ? null : result);
    } catch (error) {
      ok = false;
      resultText = error instanceof Error ? error.message : String(error);
    }
    this.#pendingCapabilityResult = encoder.encode(resultText);
    return ok ? 0 : 1;
  }

  /** The `env.hs_capability_take_result` import. */
  #takeCapabilityResult(destPtr) {
    new Uint8Array(
      this.#exports.memory.buffer,
      destPtr,
      this.#pendingCapabilityResult.length,
    ).set(this.#pendingCapabilityResult);
  }

  /** Shared with `Instance`, which needs the same helpers. */
  _upload(bytes) {
    return this.#upload(bytes);
  }

  _takeText() {
    return this.#takeText();
  }

  /**
   * Runs `run`, one slice of a program (the load, a call or a resume), with
   * the deadline armed from now if the instance has one and with its result
   * written in `format`. The format is set for this slice alone: outside it
   * the module is always in the text format. Shared with `Instance` and
   * `AsyncInstance`, which start the runs.
   */
  _run(format, run) {
    const previous = this.#deadlineAt;
    this.#deadlineAt = this.#deadlineMs === null ? null : now() + this.#deadlineMs;
    this.#exports.hs_wasm_set_result_format(format);
    try {
      return run();
    } finally {
      this.#exports.hs_wasm_set_result_format(FORMAT_TEXT);
      this.#deadlineAt = previous;
    }
  }

  /**
   * The error for a failed call: a `HollowScriptFault` if the program faulted
   * (the module keeps the structured fault until it is read), otherwise a
   * plain `Error` with `message`, as it always was for a call that was
   * refused. Read once, straight after the failing call.
   */
  _failure(message) {
    this.#exports.hs_wasm_fault_json();
    const json = this.#takeText();
    return json === "" ? new Error(message) : new HollowScriptFault(JSON.parse(json));
  }

  _withActiveCapabilities(handlers, run) {
    const previous = this.#activeCapabilities;
    this.#activeCapabilities = handlers;
    try {
      return run();
    } finally {
      this.#activeCapabilities = previous;
    }
  }
}

/**
 * A cancellation flag (section 11.7): an `Int32Array` of length 1 over a
 * fresh `SharedArrayBuffer`. Pass it as `HollowScript.load`'s `cancelFlag`
 * option and share the same array (structured clone preserves a
 * `SharedArrayBuffer`'s identity) with whatever thread should be able to
 * cancel a run in progress, typically by `postMessage`-ing it from a
 * controller to the Worker that runs the VM and back is not needed: the
 * controller keeps its own reference and calls `cancel(flag)` directly.
 *
 * `SharedArrayBuffer` needs the page to be cross-origin isolated (the
 * `Cross-Origin-Opener-Policy: same-origin` and
 * `Cross-Origin-Embedder-Policy: require-corp` response headers); without
 * that, the constructor throws and cancellation from another thread is not
 * available; a page can still run the VM, just without that.
 */
export function createCancelFlag() {
  return new Int32Array(new SharedArrayBuffer(4));
}

/**
 * Sets `flag` (from `createCancelFlag()`), so that every run of every
 * instance it was passed to stops at its next safepoint with the fault
 * HS1006. Safe to call from any thread that holds the same array, which is
 * the whole point: a controller thread calls this while a Worker is
 * synchronously blocked running a script's call, which is the one
 * situation `postMessage` cannot reach (the Worker's event loop is not
 * running), so `Atomics.store` on shared memory is what makes cancellation
 * from another thread possible at all here.
 */
export function cancel(flag) {
  Atomics.store(flag, 0, 1);
}

/** A loaded program. */
class Instance {
  #exports;
  #handle;
  #vm;
  #capabilityHandlers;
  #format;

  constructor(exports, handle, vm, capabilityHandlers, loadResult, format) {
    this.#exports = exports;
    this.#handle = handle;
    this.#vm = vm;
    this.#capabilityHandlers = capabilityHandlers;
    this.#format = format;
    /**
     * Section 12.3's load result: the rendered value a top-level `return`
     * in the entry module produced, or `null` for a module that falls off
     * the end without one. Set once, by `loadProgram`, and never changes
     * after (a later `call` has its own, separate result). With
     * `resultFormat: "json"`, the parsed value, or `undefined` for none.
     */
    this.loadResult = loadResult;
  }

  /**
   * Calls an exported function. `args` is an array of numbers, strings,
   * booleans and nulls, which is what a scalar argument can be.
   * Returns the result as text, or `null` for a function that returns
   * nothing; with `resultFormat: "json"`, the parsed value, or `undefined`
   * for a function that returns nothing. Throws a `HollowScriptFault`, whose
   * message is the fault's text, if the program faults, including with
   * HS1006 if the VM's cancellation flag was set from another thread while
   * this call was running.
   */
  call(name, args = []) {
    // Everything that can throw is done before anything is uploaded:
    // `toJson` throws for a circular argument and for a `BigInt` on a platform that cannot write one, and
    // a buffer already uploaded by then was never freed.
    const encodedName = encoder.encode(name);
    const encodedArgs = encoder.encode(toJson(args));
    const nameBuffer = this.#vm._upload(encodedName);
    const argsBuffer = this.#vm._upload(encodedArgs);
    let status;
    try {
      status = this.#vm._withActiveCapabilities(this.#capabilityHandlers, () =>
        this.#vm._run(this.#format, () =>
          this.#exports.hs_wasm_call(
            this.#handle,
            nameBuffer[0],
            nameBuffer[1],
            argsBuffer[0],
            argsBuffer[1],
          ),
        ),
      );
    } finally {
      this.#exports.hs_wasm_free(nameBuffer[0], nameBuffer[1]);
      this.#exports.hs_wasm_free(argsBuffer[0], argsBuffer[1]);
    }
    const text = this.#vm._takeText();
    if (status !== 0) {
      throw this.#vm._failure(text);
    }
    return decodeResult(text, this.#format);
  }

  /**
   * Reads an exported constant, as text, or, with `resultFormat: "json"`,
   * as the parsed value.
   */
  read(name) {
    const buffer = this.#vm._upload(encoder.encode(name));
    let status;
    try {
      status = this.#vm._run(this.#format, () =>
        this.#exports.hs_wasm_read(this.#handle, buffer[0], buffer[1]),
      );
    } finally {
      this.#exports.hs_wasm_free(buffer[0], buffer[1]);
    }
    const text = this.#vm._takeText();
    if (status !== 0) {
      throw this.#vm._failure(text);
    }
    return this.#format === FORMAT_JSON ? parseResult(text) : text;
  }

  /** Ends the instance and releases everything it holds. */
  dispose() {
    this.#exports.hs_wasm_dispose(this.#handle);
    this.#handle = 0;
  }
}

/** The `Status` values `abi.rs` answers; only the ones JS needs to compare against. */
const STATUS_OK = 0;
const STATUS_FAULTED = 2;
const STATUS_SUSPENDED = 4;

/**
 * Reads the capability calls `handle` is currently paused at
 * (`hs_wasm_commands_json`), draining them, and returns them as plain JS
 * objects: `{ function, name, args }`, where `name` is `names[function]`
 * (the same qualified name `capabilityNames()` lists) and `args` is the
 * plain JS array of the call's own arguments, each in the encoding with an
 * `Int` above 2^53 - 1 as a `BigInt`.
 */
function readPendingCommands(exports, handle, names, vm) {
  const status = exports.hs_wasm_commands_json(handle);
  const text = vm._takeText();
  if (status === STATUS_FAULTED) {
    // The arguments were too large to hand over (the output limit): a fault
    // of the run, which cannot be resumed, so the instance is to be disposed.
    throw vm._failure(text);
  }
  if (status !== STATUS_OK) {
    throw new Error(text || "could not read the pending capability calls");
  }
  const raw = text === "" ? [] : JSON.parse(text);
  return raw.map((command) => ({
    function: command.function,
    name: names[command.function],
    args: command.args.map(decodeExactInts),
  }));
}

/**
 * One capability answer for `AsyncInstance.resume`: either a plain
 * JSON-compatible value (or, for a capability declared `Result<T, E>`, a
 * one-field object `{ Ok: value }` or `{ Err: value }`, exactly as the
 * synchronous path's capability results already work), meaning that call
 * succeeded, or an `Error`, meaning the call itself failed (HS1012, not a
 * value the program can match).
 * @typedef {*} AsyncCommandResult
 */

/**
 * The result `AsyncInstance.call`/`resume` answers: either
 * `{ suspended: false, value }` (`value` is the rendered result, or `null`
 * for a function that returns nothing, exactly as `Instance.call`'s own
 * result is; with `resultFormat: "json"`, the parsed value, or `undefined`
 * for a function that returns nothing), or `{ suspended: true, commands }`, the same shape
 * `readPendingCommands` answers, which the host must eventually answer
 * with `resume` (one `AsyncCommandResult` per command, in the same order)
 * to make any further progress.
 */

/**
 * A program loaded on the batched (suspend/resume) path
 * (`HollowScript.loadProgramBatched`). Neither `call` nor `resume` ever
 * blocks waiting for a capability: instead, either may answer
 * `{ suspended: true, commands }`, which the host answers in its own time
 * (however long an `await` takes) by calling `resume`, as many times as
 * the program keeps calling a capability before it finishes or faults.
 */
class AsyncInstance {
  #exports;
  #handle;
  #vm;
  #names;
  #format;

  constructor(exports, handle, vm, names, initialCommands, loadResult, format) {
    this.#exports = exports;
    this.#handle = handle;
    this.#vm = vm;
    this.#names = names;
    this.#format = format;
    /**
     * Set only when `loadProgramBatched` itself found the instance already
     * suspended (the module's own top-level statements called a
     * capability); `call` refuses to start a new run until this is
     * resumed, exactly as the synchronous path's `HS1204`/`Busy` would.
     */
    this.pendingCommands =
      initialCommands.length > 0 ? initialCommands : null;
    /**
     * Section 12.3's load result, as `Instance.loadResult` documents it, or
     * `null` if the module's own top-level statements were still suspended
     * when `loadProgramBatched` returned (there is no result yet in that
     * case; resuming to completion answers one from `resume` instead).
     */
    this.loadResult = loadResult;
  }

  /**
   * Calls an exported function, as `Instance.call` does, except that this
   * may answer `{ suspended: true, commands }` instead of the result, if
   * the program calls a capability before it finishes; resume the same
   * run with `resume` to continue.
   */
  call(name, args = []) {
    if (this.pendingCommands !== null) {
      throw new Error(
        "this instance is suspended; call resume() with the pending commands' answers before starting a new call",
      );
    }
    // As `Instance.call`: nothing is uploaded until nothing can throw.
    const encodedName = encoder.encode(name);
    const encodedArgs = encoder.encode(toJson(args));
    const nameBuffer = this.#vm._upload(encodedName);
    const argsBuffer = this.#vm._upload(encodedArgs);
    let status;
    try {
      status = this.#vm._run(this.#format, () =>
        this.#exports.hs_wasm_call_batched(
          this.#handle,
          nameBuffer[0],
          nameBuffer[1],
          argsBuffer[0],
          argsBuffer[1],
        ),
      );
    } finally {
      this.#exports.hs_wasm_free(nameBuffer[0], nameBuffer[1]);
      this.#exports.hs_wasm_free(argsBuffer[0], argsBuffer[1]);
    }
    return this.#settle(status);
  }

  /**
   * Continues the run `call` (or a previous `resume`) paused at, with
   * `results`, one `AsyncCommandResult` per command `pendingCommands` (or
   * the last `resume`'s own answer) listed, in the same order. May itself
   * answer `{ suspended: true, commands }` again.
   */
  resume(results) {
    if (this.pendingCommands === null) {
      throw new Error("this instance is not suspended; call call() to start a run");
    }
    if (results.length !== this.pendingCommands.length) {
      throw new Error(
        `expected ${this.pendingCommands.length} result(s), one per pending command, got ${results.length}`,
      );
    }
    const encoded = results.map((result) =>
      result instanceof Error ? { error: result.message } : result,
    );
    const buffer = this.#vm._upload(encoder.encode(toJson(encoded)));
    let status;
    try {
      status = this.#vm._run(this.#format, () =>
        this.#exports.hs_wasm_resume_json(this.#handle, buffer[0], buffer[1]),
      );
    } finally {
      this.#exports.hs_wasm_free(buffer[0], buffer[1]);
    }
    return this.#settle(status);
  }

  #settle(status) {
    if (status === STATUS_SUSPENDED) {
      this.pendingCommands = readPendingCommands(
        this.#exports,
        this.#handle,
        this.#names,
        this.#vm,
      );
      return { suspended: true, commands: this.pendingCommands };
    }
    const text = this.#vm._takeText();
    if (status === STATUS_FAULTED) {
      this.pendingCommands = null;
      throw this.#vm._failure(text);
    }
    if (status !== STATUS_OK) {
      // Refused or busy: the run is where it was, so `pendingCommands` still
      // names what it is waiting for, and the text is a reason, not a result.
      throw this.#vm._failure(text);
    }
    this.pendingCommands = null;
    return { suspended: false, value: decodeResult(text, this.#format) };
  }

  /** Ends the instance and releases everything it holds. */
  dispose() {
    this.#exports.hs_wasm_dispose(this.#handle);
    this.#handle = 0;
  }
}

/**
 * The lexical class of one highlighted range.
 * @typedef {"keyword" | "identifier" | "number" | "string" | "filter" | "operator" | "punctuation" | "comment"} HighlightClass
 */

/**
 * One classified range of a source text, half-open and in UTF-16 code
 * units, so it can go straight to a web editor's own range type.
 * @typedef {{ start: number, end: number, class: HighlightClass }} HighlightRange
 */

/**
 * One reserved word. `future` marks a word reserved for a later version of
 * the language, which is an error to write today.
 * @typedef {{ word: string, future: boolean }} KeywordInfo
 */

/**
 * The type of the smallest expression at an offset: its half-open range in
 * UTF-16 code units and its type as the checker renders it.
 * @typedef {{ start: number, end: number, type: string }} HoverResult
 */

/**
 * What a completion is.
 * @typedef {"variable" | "constant" | "function" | "parameter" | "type" | "enum" | "enumMember" | "capability" | "namespace" | "keyword"} CompletionKind
 */

/**
 * One completion: the text to insert, what it is, and a signature or type to
 * show beside it (empty when there is none).
 * @typedef {{ label: string, kind: CompletionKind, detail: string }} CompletionItem
 */

/**
 * Where a name was declared: the module's canonical path (the entry name for
 * the module being analysed) and the half-open range of the name in that
 * module's text, in UTF-16 code units.
 * @typedef {{ file: string, start: number, end: number }} DefinitionResult
 */

/**
 * What an outline item declares.
 * @typedef {"capability" | "module" | "function" | "record" | "enum" | "constant" | "variable" | "field" | "enumMember"} SymbolKind
 */

/**
 * One entry of the outline. `start`/`end` is the whole declaration and
 * `selStart`/`selEnd` its name, all half-open in UTF-16 code units. `detail`
 * is a signature or a type (empty when there is none), `exported` whether the
 * declaration is marked `export`, and `children` a record's fields or an
 * enum's variants.
 * @typedef {{ name: string, kind: SymbolKind, detail: string, exported: boolean, start: number, end: number, selStart: number, selEnd: number, children: DocumentSymbol[] }} DocumentSymbol
 */

/**
 * One entry of the `capabilities` argument of `check`, `compile` and
 * `analyse`, as `standardCapabilities()` answers them: `function` is left out
 * for a capability that is a single function, `result` for a function with no
 * result, and `params` and `result` are type descriptions (see
 * `check`).
 * @typedef {{ name: string, function?: string, params: unknown[], result?: unknown }} CapabilityDeclaration
 */

/**
 * A loaded build of the `tooling` feature (`src/tooling.rs`,
 * `src/abi_tooling.rs`): checking, formatting and compiling source, in the
 * browser, without a server. A different `.wasm` file from the one
 * `HollowScript` loads, built with `cargo build -p hollowscript-wasm
 * --release --features tooling --target wasm32-unknown-unknown`; the
 * plain, feature-less build (what a published game ships) has none of
 * `hs_wasm_check`, `hs_wasm_format` or `hs_wasm_compile`, so this class
 * only works against that separate build.
 */
export class HollowScriptTooling {
  #exports;

  constructor(exports) {
    this.#exports = guardExports(exports);
  }

  /**
   * Whether this instance has trapped, in which case every call on it, and
   * on any `Analysis` made from it, throws a `HollowScriptTrap`. Discard it
   * and load a new one.
   */
  get dead() {
    return hasTrapped(this.#exports);
  }

  // `abi.rs`'s four `env` imports exist on every build, tooling or not,
  // because capability binding and cancellation are not behind the
  // `tooling` feature; check/format/compile never trigger a capability call
  // or a safepoint (there is no run, only front end and checker work), so
  // these never actually fire, but WebAssembly still requires every
  // declared import to be satisfied before it will instantiate at all.
  static #unusedImports = {
    env: {
      hs_capability_call: () => 1,
      hs_capability_result_len: () => 0,
      hs_capability_take_result: () => {},
      hs_wasm_poll_cancel: () => 0,
    },
  };

  /**
   * Instantiates the `tooling` module at `url`. `maxMemoryBytes` is as
   * `HollowScript.load`'s: a lower ceiling than the module's 256 MiB on its
   * linear memory (checking source can allocate for real too).
   */
  static async load(url, options = {}) {
    return HollowScriptTooling.fromBytes(await fetchModule(url), options);
  }

  /** Instantiates the `tooling` module from bytes already in hand. */
  static async fromBytes(bytes, { maxMemoryBytes } = {}) {
    const { exports } = await instantiateModule(
      bytes,
      HollowScriptTooling.#unusedImports,
      maxMemoryBytes,
    );
    return new HollowScriptTooling(exports);
  }

  /**
   * Checks `source` (`entryName` defaults to "main.hws") and answers the
   * array of diagnostics `hollowscript-capi`'s `hs_check_json` also
   * answers: `[{severity, line, code, message}, ...]`. Checked against the
   * standard always-granted capabilities (`print`, `clock`, `random`; the
   * same ones the CLI grants with no `--allow`), plus whatever
   * `capabilities` declares beyond them: an array of
   * `{ name, function, params, result }` entries, `function` a string
   * (for one function of a namespace capability, such as "clock"'s "now")
   * or omitted for a plain function capability. `params` is an array of
   * *type descriptions*, and `result` one type description (or `{ ok, err
   * }` for a capability declared `Result<T, E>`, section 8.2, or omitted
   * for a function with no result). A type description is one of:
   *
   *   - `"Int"` / `"Float"` / `"Bool"` / `"String"` / `"Filter"` (a bare
   *     scalar name; `"Filter"` only valid in `params`)
   *   - `{ list: T }`, `{ set: T }`, `{ optional: T }`
   *   - `{ map: { key: T, value: T } }`
   *   - `{ record: { field: T, ... } }`
   *   - `{ enum: { name: "E", variants: { V: [T, ...], ... } } }`
   *
   * the same tags [`WasmModule::value_to_json`]'s own value encoding uses,
   * one level up: a capability whose declared result type is `{ record: {
   * x: "Int", y: "Int" } }` answers a value tagged the same way,
   * `{"record": {"x": 3, "y": 4}}`. The default, an empty array, declares
   * nothing beyond the standard set, which is enough for a program that
   * only `uses print`/`clock`/`random`.
   *
   * `modules` resolves every `import` beyond the entry module itself
   * (section 10.3), for a Code workspace of more than one file: a plain
   * object mapping each other module's own canonical path (relative to the
   * entry module, exactly as its own `import` line names it, extension
   * included, no leading `./`) to its source text, `{ "lib/helpers.hws":
   * "export fn double(n: Int): Int {\n    return n * 2\n}\n" }`. The
   * default, an empty object, is a single-file program: an `import` in
   * `source` is HS0601 ("not found"), exactly as before this parameter
   * existed.
   *
   * `filterSchema` is the host's description of the records its filters run
   * against (Appendix A, section A.6), `{ fields: { status: { type: "enum",
   * values: ["open", "done"], aliases: { closed: "done" } }, title: { type:
   * "text" } } }`, in the shape of section 19.4's `filterSchema`: `type` is
   * one of `enum`, `text`, `textMulti`, `date`, `number` and `boolean`;
   * `values` (enum only) and `aliases` are optional; a field name and an alias
   * input are lower case. With one, every filter literal is also validated
   * against it (section A.7), so a field the host does not have is HS0708 here
   * and not at run time, as are an operator the field does not allow (HS0707)
   * and a value that does not suit it (HS0709). The default, `null`, declares
   * no schema, and a literal is then checked for syntax only. `{ fields: {} }`
   * is a schema with no field, which refuses every predicate. A schema that is
   * malformed is a single HS1099 diagnostic naming what is wrong, never
   * silently read as something looser.
   */
  check(source, entryName = "main.hws", capabilities = [], modules = {}, filterSchema = null) {
    const status = this.#call(
      this.#exports.hs_wasm_check,
      source,
      entryName,
      capabilities,
      modules,
      filterSchema,
    );
    const json = JSON.parse(this.#takeText());
    if (status !== 0) {
      throw new Error("check() itself failed unexpectedly: " + JSON.stringify(json));
    }
    return json;
  }

  /**
   * Formats `source` to its canonical layout, needing no checker and no
   * capability interface either. Answers `{ output, diagnostics }`, where
   * `output` is the formatted text or `null` for a syntax error the
   * formatter cannot recover from.
   */
  format(source, entryName = "main.hws") {
    const sourceBuffer = this.#upload(encoder.encode(source));
    const nameBuffer = this.#upload(encoder.encode(entryName));
    try {
      this.#exports.hs_wasm_format(
        sourceBuffer[0],
        sourceBuffer[1],
        nameBuffer[0],
        nameBuffer[1],
      );
    } finally {
      this.#exports.hs_wasm_free(sourceBuffer[0], sourceBuffer[1]);
      this.#exports.hs_wasm_free(nameBuffer[0], nameBuffer[1]);
    }
    return JSON.parse(this.#takeText());
  }

  /**
   * Compiles `source` to a bytecode container (a Uint8Array), against the
   * same capability interface and `modules` loader `check` uses:
   * `HollowScript.fromBytes(vmWasm).loadProgram(compile(source))` is source
   * to a running program with no server and no CLI. Throws with the array
   * of diagnostics (as `check()` answers) if the program has an error. A
   * `filterSchema` (as `check` takes) applies here too, so a program `check`
   * refuses for a filter literal never becomes bytecode.
   */
  compile(source, entryName = "main.hws", capabilities = [], modules = {}, filterSchema = null) {
    const status = this.#call(
      this.#exports.hs_wasm_compile,
      source,
      entryName,
      capabilities,
      modules,
      filterSchema,
    );
    if (status !== 0) {
      throw new Error(JSON.stringify(JSON.parse(this.#takeText())));
    }
    const length = Number(this.#exports.hs_wasm_bytes_len());
    if (length === 0) {
      return new Uint8Array(0);
    }
    const pointer = this.#exports.hs_wasm_take_bytes();
    const bytes = new Uint8Array(
      this.#exports.memory.buffer,
      pointer,
      length,
    ).slice();
    this.#exports.hs_wasm_free(pointer, length);
    return bytes;
  }

  /**
   * Every filter literal of `source` and of the modules it imports, in
   * source order, for a host that evaluates filters and wants to know, before
   * running a program, which ones it can ask for:
   *
   * ```js
   * t.filterLiterals('uses tasks\nreturn effect tasks(`status:open`)\n', "main.hws", {}, [
   *   { name: "tasks", params: ["Filter"], result: "Int" },
   * ]);
   * // [{ source: "status:open", file: "main.hws", line: 2, column: 21,
   * //    spanStartUtf16: 31, spanEndUtf16: 44 }]
   * ```
   *
   * `source` is the filter's own text, with `\`` and `\\` unescaped: exactly
   * what a capability that is given the filter receives, as `{ filter:
   * "<text>" }`, so it is what a host keys what it evaluates by. The span is
   * of the whole literal, backticks included, in UTF-16 code units (what a
   * JavaScript string and a text editor count in); `line` and `column` are as
   * a diagnostic's are. `file` is `entryName` or the path of a module in
   * `modules`. A text that appears twice is listed twice, at each place; a
   * host that wants each once removes the repeats.
   *
   * The program need not check cleanly, and a literal is listed whether or
   * not it is valid: `check` says whether the program will run, and with
   * `filterSchema` whether each filter suits the host's records. A literal
   * that is not even a token (an unterminated backtick) is not listed.
   * `capabilities` and `modules` are as `check` takes them. Throws if either
   * is malformed, so an empty array always means there are none.
   */
  filterLiterals(source, entryName = "main.hws", modules = {}, capabilities = []) {
    const status = this.#call(
      this.#exports.hs_wasm_filter_literals,
      source,
      entryName,
      capabilities,
      modules,
    );
    const text = this.#takeText();
    if (status !== 0) {
      throw new Error(text);
    }
    return JSON.parse(text);
  }

  /**
   * Calls one of the exports that takes source and the JSON arguments of
   * `check`. `filterSchema` is only passed to the ones that take it:
   * `undefined` (as `filterLiterals` leaves it) sends none, and `null` sends
   * an empty schema, which declares none.
   */
  #call(fn, source, entryName, capabilities, modules, filterSchema) {
    // Every text is made before any is uploaded: `JSON.stringify` throws for a
    // circular argument or a `BigInt`, and a buffer already uploaded by then
    // was never freed.
    const texts = [
      source,
      entryName,
      JSON.stringify(capabilities),
      JSON.stringify(modules),
    ];
    if (filterSchema !== undefined) {
      texts.push(filterSchema === null ? "" : JSON.stringify(filterSchema));
    }
    const buffers = texts.map((text) => this.#upload(encoder.encode(text)));
    try {
      return fn(...buffers.flat());
    } finally {
      for (const [pointer, length] of buffers) {
        this.#exports.hs_wasm_free(pointer, length);
      }
    }
  }

  /**
   * Classifies every token and comment of `source` lexically, from the
   * lexer alone: stateless, needing no check and no capability interface,
   * and working on text that does not parse (an editor highlights on every
   * keystroke). The ranges are in order of where they start, never overlap,
   * and are half-open in UTF-16 code units. Whitespace and a character the
   * language has no token for are not covered.
   *
   * @param {string} source
   * @returns {HighlightRange[]}
   */
  highlight(source) {
    const buffer = this.#upload(encoder.encode(source));
    let status;
    try {
      status = this.#exports.hs_wasm_highlight(buffer[0], buffer[1]);
    } finally {
      this.#exports.hs_wasm_free(buffer[0], buffer[1]);
    }
    return this.#ideAnswer(status, "highlight");
  }

  /**
   * Every reserved word of the language, in alphabetical order, taken from
   * the lexer's own table, so a word the language adds cannot be missed.
   * Stateless.
   *
   * @returns {KeywordInfo[]}
   */
  keywords() {
    return this.#ideAnswer(this.#exports.hs_wasm_keywords(), "keywords");
  }

  /**
   * The declarations of the standard capabilities (`print`, `clock`,
   * `random`), in the shape the `capabilities` argument of `check`, `compile`
   * and `analyse` takes, so a host need not copy them by hand. `check`,
   * `compile` and `analyse` already grant them on top of whatever is passed,
   * so passing them again is harmless and changes nothing; this is for a page
   * that wants to show them, or to send them along with its own to something
   * else. Stateless.
   *
   * @returns {CapabilityDeclaration[]}
   */
  standardCapabilities() {
    return this.#ideAnswer(
      this.#exports.hs_wasm_standard_capabilities(),
      "standardCapabilities",
    );
  }

  /**
   * Checks `source` once and holds the result in an {@link Analysis}, so
   * that hover, completion, definition and the outline can be asked of it
   * many times without checking again. `entryName`, `capabilities` and
   * `modules` mean exactly what they mean for `check`: the entry module's own
   * name (default "main.hws"), the host capabilities declared beyond the
   * standard ones, and the other modules `import` resolves against.
   *
   * Every offset the analysis takes or answers is in UTF-16 code units, as a
   * web editor counts, and every range is half-open.
   *
   * The analysis keeps the checked program in the module's memory until it is
   * disposed, so call `dispose()` when it is out of date (an editor takes a
   * new one per edit) or no longer shown. At most 64 may be held at once;
   * past that, `analyse` throws.
   *
   * @param {string} source
   * @param {string} [entryName]
   * @param {unknown[]} [capabilities]
   * @param {Record<string, string>} [modules]
   * @returns {Analysis}
   */
  analyse(source, entryName = "main.hws", capabilities = [], modules = {}) {
    const buffers = [
      source,
      entryName,
      JSON.stringify(capabilities),
      JSON.stringify(modules),
    ].map((text) => this.#upload(encoder.encode(text)));
    let handle;
    try {
      handle = this.#exports.hs_wasm_analyse(
        buffers[0][0],
        buffers[0][1],
        buffers[1][0],
        buffers[1][1],
        buffers[2][0],
        buffers[2][1],
        buffers[3][0],
        buffers[3][1],
      );
    } finally {
      for (const [pointer, length] of buffers) {
        this.#exports.hs_wasm_free(pointer, length);
      }
    }
    if (handle === 0) {
      throw new Error(`analyse() failed: ${this.#takeText()}`);
    }
    return new Analysis(this.#exports, handle);
  }

  #ideAnswer(status, what) {
    const text = this.#takeText();
    if (status !== 0) {
      throw new Error(`${what}() failed: ${text}`);
    }
    return JSON.parse(text);
  }

  #upload(bytes) {
    const length = bytes.length;
    if (length === 0) {
      return [0, 0];
    }
    const pointer = this.#exports.hs_wasm_alloc(length);
    new Uint8Array(this.#exports.memory.buffer, pointer, length).set(bytes);
    return [pointer, length];
  }

  #takeText() {
    const length = Number(this.#exports.hs_wasm_text_len());
    if (length === 0) {
      return "[]";
    }
    const pointer = this.#exports.hs_wasm_take_text();
    const bytes = new Uint8Array(
      this.#exports.memory.buffer,
      pointer,
      length,
    ).slice();
    this.#exports.hs_wasm_free(pointer, length);
    return decoder.decode(bytes);
  }
}

/**
 * Disposes the analysis of an `Analysis` that was garbage collected without
 * `dispose()`. What it is given holds the module's exports and the number, and
 * must not hold the `Analysis` itself, or the `Analysis` would never be
 * collected. `null` where the engine has no `FinalizationRegistry`. A module
 * that has trapped refuses every call, and a handle already gone is harmless,
 * so whatever the dispose throws is dropped: there is no one to tell.
 */
const analysisFinaliser =
  typeof FinalizationRegistry === "function"
    ? new FinalizationRegistry(({ exports, handle }) => {
        try {
          exports.hs_wasm_analysis_dispose(handle);
        } catch {
          // Nothing to do: see above.
        }
      })
    : null;

/**
 * A checked program held by the `tooling` build, answering questions about
 * the module it was made from without checking again: see
 * {@link HollowScriptTooling#analyse}. It refers to the module's analysis by
 * a number the module handed out, never by a pointer, so a handle the module
 * does not know is refused rather than reaching memory, and disposing twice
 * is harmless.
 *
 * Every offset taken and answered is in UTF-16 code units, and every range is
 * half-open.
 */
export class Analysis {
  #exports;
  #handle;

  /** @param {WebAssembly.Exports} exports @param {number} handle */
  constructor(exports, handle) {
    this.#exports = exports;
    this.#handle = handle;
    // A handle the page forgot to dispose holds one of the module's 64
    // places for the life of the module, and the cap then refuses every later
    // `analyse`. So the engine is asked to dispose it once the object is
    // unreachable. That is a safety net and not a plan: the engine decides
    // when, if ever, and a page that makes analyses faster than it collects
    // garbage still reaches the cap, so call `dispose()` yourself (an editor
    // disposes the old analysis when it takes a new one).
    if (analysisFinaliser !== null) {
      analysisFinaliser.register(this, { exports, handle }, this);
    }
  }

  /**
   * The type of the smallest expression at `offset`, or `null` where the
   * offset is not inside one (a keyword, whitespace between statements).
   * The type is written as the checker renders it, `Int`, `[String]`,
   * `fn(Int): Int`.
   *
   * @param {number} offset
   * @returns {HoverResult | null}
   */
  hover(offset) {
    return this.#ask("hover", this.#exports.hs_wasm_analysis_hover, offset);
  }

  /**
   * What can be written at `offset`: the names in scope there in the order
   * they are declared, then the prelude's names and types, then the keywords.
   * A name comes into scope where the language says it does, so one declared
   * further down is not offered. The list is the same wherever in a word the
   * cursor is, so filter it by what has been typed. It is empty where a name
   * cannot be written: in a comment, in the text of a string or a filter, and
   * after a `.`. **Not done:** completing a field or method after a `.`, which
   * needs the type of the receiver.
   *
   * @param {number} offset
   * @returns {CompletionItem[]}
   */
  completions(offset) {
    return this.#ask(
      "completions",
      this.#exports.hs_wasm_analysis_completions,
      offset,
    );
  }

  /**
   * Where the name at `offset` was declared, or `null` where the offset is
   * not on a name that has a declaration (a keyword, a prelude name, a field
   * or method name, or text inside a comment or a string). `file` is the
   * module's canonical path, so it is one of the keys of `modules` for a name
   * brought in by `import`, and the entry name for one declared here.
   *
   * @param {number} offset
   * @returns {DefinitionResult | null}
   */
  definition(offset) {
    return this.#ask(
      "definition",
      this.#exports.hs_wasm_analysis_definition,
      offset,
    );
  }

  /**
   * The outline of the entry module as a tree: one item per name a `uses` line
   * declares, per `import` line (named by its path, with the imported names as
   * `detail`), and per top-level `fn`, `record`, `enum`, `constant` or
   * `variable`. It is built from the syntax tree, so it is there for a file
   * that does not check.
   *
   * @returns {DocumentSymbol[]}
   */
  documentSymbols() {
    return this.#ask("documentSymbols", this.#exports.hs_wasm_analysis_symbols);
  }

  /**
   * Releases the checked program. Idempotent: later calls do nothing, and
   * every other method throws once this has been called.
   */
  dispose() {
    if (this.#handle !== 0) {
      if (analysisFinaliser !== null) {
        analysisFinaliser.unregister(this);
      }
      this.#exports.hs_wasm_analysis_dispose(this.#handle);
      this.#handle = 0;
    }
  }

  #ask(what, fn, ...offsets) {
    if (this.#handle === 0) {
      throw new Error(`${what}() on an analysis that has been disposed`);
    }
    for (const offset of offsets) {
      if (!Number.isInteger(offset) || offset < 0 || offset > 0xffffffff) {
        throw new RangeError(
          `${what}() needs an offset that is a non-negative integer, got ${offset}`,
        );
      }
    }
    const status = fn(this.#handle, ...offsets);
    const length = Number(this.#exports.hs_wasm_text_len());
    let text = "";
    if (length > 0) {
      const pointer = this.#exports.hs_wasm_take_text();
      const bytes = new Uint8Array(
        this.#exports.memory.buffer,
        pointer,
        length,
      ).slice();
      this.#exports.hs_wasm_free(pointer, length);
      text = decoder.decode(bytes);
    }
    if (status !== 0) {
      throw new Error(`${what}() failed: ${text}`);
    }
    return JSON.parse(text);
  }
}
