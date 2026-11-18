"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { MemberService, readModule } = require("../../src/members");
const token = { isCancellationRequested: false, onCancellationRequested() { return { dispose() {} }; } };

test("packaged worker completes typed members, imports and unsaved modules", async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), "hws-member-test-"));
  const service = new MemberService(path.resolve("dist/member-worker.js"));
  try {
    await fs.writeFile(path.join(root, "util.hws"), "export record Person = { age: Int }\nexport enum Choice<T> { First(T), Last }\n");
    const cases = [
      ['constant s = "hi"\nreturn s.', "trim", "method"],
      ['return "hi".trim().', "length", "method"],
      ['constant xs = [1]\nreturn xs.ap', "append", "method"],
      ['return ["a": 1].', "get", "method"],
      ['return set([1]).', "union", "method"],
      ['constant r: Result<Int, String> = success(1)\nreturn r.', "unwrapOr", "method"],
      ['return json.', "parse", "method"],
      ['return Json.', "Object", "enumMember"],
      ['import Person from "./util"\nconstant p: Person = { age: 1 }\nreturn p.', "age", "field"],
      ['import Choice from "./util"\nreturn Choice.', "First", "enumMember"],
      ['constant p = { child: { age: 1 } }\nreturn p.child.', "age", "field"],
      ['constant emoji = "😀"\nreturn emoji.', "trim", "method"],
    ];
    for (const [source, expected, kind] of cases) {
      const items = await service.complete(path.join(root, "main.hws"), source, source.length, new Map(), token);
      assert.ok(items.some((i) => i.label === expected && i.kind === kind), `${source}: ${JSON.stringify(items)}`);
    }
    const diagnosticSource = 'constant face = "😀"\neffect print("hi")\n';
    const diagnostics = await service.diagnostics(path.join(root, "main.hws"), diagnosticSource, new Map(), token);
    const missing = diagnostics.find((d) => d.code === "HS0205");
    assert.ok(missing);
    assert.equal(diagnosticSource.slice(missing.start, missing.end), "print");
    const source = 'import Person from "./util"\nconstant p: Person = { title: "A" }\nreturn p.';
    const items = await service.complete(path.join(root, "main.hws"), source, source.length, new Map([[path.join(root, "util.hws"), 'export record Person = { title: String }\n']]), token);
    assert.deepEqual(items.map((i) => i.label), ["title"]);
    for (const text of ['// s.', 'return "s.', 'return `s.', 'return missing.', 'constant s: String? = none\nreturn s.']) {
      const items = await service.complete(path.join(root, "main.hws"), text, text.length, new Map(), token);
      assert.equal(items.filter((i) => ["method", "field", "enumMember"].includes(i.kind)).length, 0, text);
    }
  } finally { service.dispose(); await fs.rm(root, { recursive: true, force: true }); }
});

test("module reads reject traversal, symbolic links, hard links and oversized files", async () => {
  const root = await fs.realpath(await fs.mkdtemp(path.join(os.tmpdir(), "hws-member-files-")));
  try {
    await fs.writeFile(path.join(root, "safe.hws"), "return 1\n");
    assert.equal(await readModule(root, "safe.hws"), "return 1\n");
    for (const name of ["../safe.hws", "/safe.hws", "dir/../../safe.hws", "x\\safe.hws", "safe.hws\0", "safe.txt"]) {
      assert.equal(await readModule(root, name), undefined, name);
    }
    await fs.symlink(path.join(root, "safe.hws"), path.join(root, "link.hws"));
    assert.equal(await readModule(root, "link.hws"), undefined);
    await fs.link(path.join(root, "safe.hws"), path.join(root, "hard.hws"));
    assert.equal(await readModule(root, "hard.hws"), undefined);
    await fs.writeFile(path.join(root, "big.hws"), " ".repeat(1024 * 1024 + 1));
    assert.equal(await readModule(root, "big.hws"), undefined);
  } finally { await fs.rm(root, { recursive: true, force: true }); }
});

test("cancellation terminates completion and returns no stale items", async () => {
  const service = new MemberService(path.resolve("dist/member-worker.js"));
  /** @type {(() => void) | undefined} */
  let cancel;
  try {
    const pending = service.complete(path.resolve("test/fixtures/sample.hws"), 'return "hi".', 12, new Map(), {
      isCancellationRequested: false,
      onCancellationRequested(callback) { cancel = () => callback(undefined); return { dispose() {} }; },
    });
    await new Promise((resolve) => setTimeout(resolve, 5));
    cancel?.();
    assert.deepEqual(await pending, []);
  } finally { service.dispose(); }
});

test("concurrent editor requests do not cancel each other", async () => {
  const service = new MemberService(path.resolve("dist/member-worker.js"));
  try {
    const file = path.resolve("test/fixtures/sample.hws");
    const source = 'effect print("hi")\n';
    const results = await Promise.all([
      service.diagnostics(file, source, new Map(), token),
      service.diagnostics(file, source, new Map(), token),
    ]);
    assert.ok(results.every((items) => items.some((d) => d.code === "HS0205")));
  } finally { service.dispose(); }
});
