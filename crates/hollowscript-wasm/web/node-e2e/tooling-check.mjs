import { readFile } from "node:fs/promises";
import { deepStrictEqual, strictEqual } from "node:assert";
import { HollowScriptTooling } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const t = await HollowScriptTooling.fromBytes(wasmBytes);

const diags = t.check("return missing");
console.log("check ->", JSON.stringify(diags));
if (!diags.some((d) => d.code === "HS0205")) {
  console.error("FAIL: expected HS0205");
  process.exit(1);
}

// Regression check: a program that only uses the standard always-granted
// capabilities (print, clock, random) must check with no declaration at
// all, the same as `hollowscript check` and a plain `hollowscript-capi`
// run with no HsCapabilityFn. This used to answer HS0404 instead.
const standardSource = 'uses print\neffect print("Hello, world!")\n';
const standardDiags = t.check(standardSource);
console.log("check(standard) ->", JSON.stringify(standardDiags));
if (standardDiags.length !== 0) {
  console.error(`FAIL: expected no diagnostics for the standard capabilities, got ${JSON.stringify(standardDiags)}`);
  process.exit(1);
}
const standardBytes = t.compile(standardSource);
console.log("compile(standard) -> bytes:", standardBytes.length);
if (standardBytes.length === 0) {
  console.error("FAIL: compile(standard) produced no bytes");
  process.exit(1);
}

// A host-declared capability beyond the standard set: refused without a
// declaration, accepted with one.
const hostSource = 'uses tasks\nreturn effect tasks("x")\n';
const undeclared = t.check(hostSource);
if (!undeclared.some((d) => d.code === "HS0404")) {
  console.error(`FAIL: expected HS0404 for an undeclared host capability, got ${JSON.stringify(undeclared)}`);
  process.exit(1);
}
const declared = t.check(hostSource, "main.hws", [
  { name: "tasks", params: ["String"], result: "String" },
]);
console.log("check(declared tasks) ->", JSON.stringify(declared));
if (declared.length !== 0) {
  console.error(`FAIL: expected no diagnostics once tasks is declared, got ${JSON.stringify(declared)}`);
  process.exit(1);
}

const formatted = t.format("constant x=1");
console.log("format ->", JSON.stringify(formatted));
if (formatted.output.join("\n") !== "constant x = 1\n") {
  console.error("FAIL: unexpected format output");
  process.exit(1);
}

const bytes = t.compile("export fn one(): Int { return 1 }");
console.log("compile -> bytes:", bytes.length);
if (bytes.length === 0) {
  console.error("FAIL: compile produced no bytes");
  process.exit(1);
}

try {
  t.compile("return missing");
  console.error("FAIL: compile should have thrown");
  process.exit(1);
} catch (e) {
  console.log("compile error (expected) ->", e.message.slice(0, 60));
}

// A capability declaring a record parameter and result (Studio's
// `entities.get`, which returns a record: the four-scalar-only params/result
// this boundary used to take could not express it, HS0320). The declaration
// uses the same tag the value encoding itself uses for a record value.
const recordSource = [
  "uses entities",
  "export fn run(): Int {",
  '    constant e = effect entities.get("player-1")',
  "    return e.x + e.y",
  "}",
  "",
].join("\n");
const recordCapabilities = [
  {
    name: "entities",
    function: "get",
    params: ["String"],
    result: { record: { id: "String", x: "Int", y: "Int" } },
  },
];
const recordDiags = t.check(recordSource, "main.hws", recordCapabilities);
console.log("check(record capability) ->", JSON.stringify(recordDiags));
if (recordDiags.length !== 0) {
  console.error(`FAIL: expected no diagnostics for a record-declaring capability, got ${JSON.stringify(recordDiags)}`);
  process.exit(1);
}
const recordBytes = t.compile(recordSource, "main.hws", recordCapabilities);
console.log("compile(record capability) -> bytes:", recordBytes.length);
if (recordBytes.length === 0) {
  console.error("FAIL: compile(record capability) produced no bytes");
  process.exit(1);
}

// A two-file program: the entry module imports a second one by its
// canonical path, supplied through `modules` (section 10.3). Before this
// loader existed, any import at all was "not found" (HS0601).
const importSource = [
  'import double from "./lib/helpers"',
  "export fn run(): Int {",
  "    return double(3)",
  "}",
  "",
].join("\n");
const modules = {
  "lib/helpers.hws": [
    "export fn double(n: Int): Int {",
    "    return n * 2",
    "}",
    "",
  ].join("\n"),
};
const withoutModules = t.check(importSource);
if (!withoutModules.some((d) => d.code === "HS0601")) {
  console.error(`FAIL: expected HS0601 with no modules supplied, got ${JSON.stringify(withoutModules)}`);
  process.exit(1);
}
const withModules = t.check(importSource, "main.hws", [], modules);
console.log("check(two-file import) ->", JSON.stringify(withModules));
if (withModules.length !== 0) {
  console.error(`FAIL: expected no diagnostics once the import resolves, got ${JSON.stringify(withModules)}`);
  process.exit(1);
}
const importBytes = t.compile(importSource, "main.hws", [], modules);
console.log("compile(two-file import) -> bytes:", importBytes.length);
if (importBytes.length === 0) {
  console.error("FAIL: compile(two-file import) produced no bytes");
  process.exit(1);
}

// Each diagnostic carries what the facade's own Diagnostic has: file, line,
// column, the span converted to UTF-16 code units (not bytes, not code
// points), severity, code, message and related. The source puts an emoji
// (a surrogate pair in UTF-16, four bytes in UTF-8) before the unresolved
// name, so a byte-for-UTF-16 mixup shows up as a wrong offset, not merely a
// missing field.
const locatedDiags = t.check('return "\u{1f600}" == missing', "main.hws");
console.log("check(location) ->", JSON.stringify(locatedDiags));
const located = locatedDiags.find((d) => d.code === "HS0205");
if (!located) {
  console.error(`FAIL: expected HS0205, got ${JSON.stringify(locatedDiags)}`);
  process.exit(1);
}
if (
  located.file !== "main.hws" ||
  located.line !== 1 ||
  typeof located.column !== "number" ||
  located.spanStartUtf16 !== 15 ||
  typeof located.spanEndUtf16 !== "number" ||
  !Array.isArray(located.related)
) {
  console.error(`FAIL: expected a full location on HS0205, got ${JSON.stringify(located)}`);
  process.exit(1);
}

// A filter schema: with one, a filter literal is validated against the
// host's own description of its records (Appendix A, section A.7), so a field
// the host does not have is refused at check time, not met at run time.
const tasks = [{ name: "tasks", params: ["Filter"], result: "Int" }];
const schema = {
  fields: {
    status: { type: "enum", values: ["open", "done"], aliases: { closed: "done" } },
    title: { type: "text" },
    points: { type: "number" },
  },
};
const filterSource = (filter) => `uses tasks\nreturn effect tasks(\`${filter}\`)\n`;
if (t.check(filterSource("colour:red"), "main.hws", tasks).length !== 0) {
  console.error("FAIL: without a schema a literal is checked for syntax only");
  process.exit(1);
}
const unknownField = t.check(filterSource("colour:red"), "main.hws", tasks, {}, schema);
console.log("check(unknown field) ->", unknownField.map((d) => d.code).join(","));
if (unknownField.length !== 1 || unknownField[0].code !== "HS0708" || unknownField[0].line !== 2) {
  console.error(`FAIL: expected HS0708 on line 2, got ${JSON.stringify(unknownField)}`);
  process.exit(1);
}
for (const valid of ["status:open", "status:closed", "title:fix points>3", "plain words"]) {
  const diagnostics = t.check(filterSource(valid), "main.hws", tasks, {}, schema);
  if (diagnostics.length !== 0) {
    console.error(`FAIL: ${valid} should suit the schema, got ${JSON.stringify(diagnostics)}`);
    process.exit(1);
  }
}
for (const [invalid, code] of [["title>x", "HS0707"], ["status:blocked", "HS0709"], ["points:many", "HS0709"]]) {
  const diagnostics = t.check(filterSource(invalid), "main.hws", tasks, {}, schema);
  if (!diagnostics.some((d) => d.code === code)) {
    console.error(`FAIL: ${invalid} should be ${code}, got ${JSON.stringify(diagnostics)}`);
    process.exit(1);
  }
}
t.compile(filterSource("status:open"), "main.hws", tasks, {}, schema);
try {
  t.compile(filterSource("colour:red"), "main.hws", tasks, {}, schema);
  console.error("FAIL: compile should refuse a field the schema does not have");
  process.exit(1);
} catch (e) {
  if (!e.message.includes("HS0708")) {
    console.error(`FAIL: expected HS0708 from compile, got ${e.message}`);
    process.exit(1);
  }
}
const noFields = t.check(filterSource("status:open"), "main.hws", tasks, {}, { fields: {} });
if (!noFields.some((d) => d.code === "HS0708")) {
  console.error("FAIL: an empty schema is a schema, and refuses every field");
  process.exit(1);
}
const badSchema = t.check("return 1", "main.hws", [], {}, { fields: { Status: { type: "text" } } });
if (badSchema.length !== 1 || badSchema[0].code !== "HS1099" || !badSchema[0].message.includes("lower case")) {
  console.error(`FAIL: a malformed schema should say what is wrong, got ${JSON.stringify(badSchema)}`);
  process.exit(1);
}

// Filter literals: every literal, with its decoded source and its span in
// UTF-16 code units (an emoji earlier on the line is two of them, not four
// bytes), in source order, and from the modules a program imports.
const literalSource = [
  "uses tasks",
  'import saved from "./lib/queries"',
  'constant n = "\u{1f600}".length() + effect tasks(`status:open`)',
  'constant second = `title:"a\\`b"`',
  "return n + effect tasks(saved)",
  "",
].join("\n");
const literalModules = {
  "lib/queries.hws": '// \u00e9\nexport constant saved = `points>3`\n',
};
const literals = t.filterLiterals(literalSource, "main.hws", literalModules, tasks);
console.log("filterLiterals ->", JSON.stringify(literals));
const open = literalSource.indexOf("`status:open`");
deepStrictEqual(
  literals.map((l) => [l.file, l.source]),
  [
    ["lib/queries.hws", "points>3"],
    ["main.hws", "status:open"],
    ["main.hws", 'title:"a`b"'],
  ],
);
// The span is in UTF-16 code units of the file's own text, so it indexes a
// JavaScript string directly: slicing gives the literal as it was written.
const [fromModule, fromEntry, escaped] = literals;
strictEqual(
  literalModules["lib/queries.hws"].slice(fromModule.spanStartUtf16, fromModule.spanEndUtf16),
  "`points>3`",
);
strictEqual(fromEntry.spanStartUtf16, open);
strictEqual(literalSource.slice(fromEntry.spanStartUtf16, fromEntry.spanEndUtf16), "`status:open`");
strictEqual(fromEntry.line, 3);
strictEqual(
  literalSource.slice(escaped.spanStartUtf16, escaped.spanEndUtf16),
  '`title:"a\\`b"`',
);
// The column counts code points, so the emoji before the literal is one.
strictEqual(fromEntry.column, [...literalSource.split("\n")[2].slice(0, literalSource.split("\n")[2].indexOf("`"))].length + 1);
deepStrictEqual(t.filterLiterals("return 1\n"), []);
try {
  t.filterLiterals("return 1\n", "main.hws", "not an object");
  console.error("FAIL: malformed modules should throw, not answer an empty array");
  process.exit(1);
} catch (e) {
  if (!e.message.includes("modules")) {
    console.error(`FAIL: expected the reason, got ${e.message}`);
    process.exit(1);
  }
}

console.log("all checks passed");
