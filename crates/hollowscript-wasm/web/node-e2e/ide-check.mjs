// An end-to-end check of the editor services in the `tooling` build:
// lexical highlighting, the keyword list, the standard capability
// declarations and the analysis handle (hover, completion, definition and
// the outline). Run it against a `tooling` .wasm; see README.md in this
// directory.
//
// Every offset a service answers is a UTF-16 code unit offset, half-open, so
// the checks below slice the JavaScript source string with them directly
// and put a non-BMP character (a surrogate pair in UTF-16, four bytes in
// UTF-8) before the text of interest: a byte offset, or a code point
// offset, would slice the wrong text.

import { readFile } from "node:fs/promises";
import { HollowScriptTooling } from "../hollowscript.js";

const wasmBytes = await readFile(process.argv[2]);
const t = await HollowScriptTooling.fromBytes(wasmBytes);

function fail(message) {
  console.error(`FAIL: ${message}`);
  process.exit(1);
}

function check(condition, message) {
  if (!condition) {
    fail(message);
  }
}

function same(actual, expected, what) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    fail(`${what}: expected ${e}, got ${a}`);
  }
}

/** The highlighted ranges of `source` as `[text, class]` pairs. */
function classes(source) {
  return t.highlight(source).map((r) => [source.slice(r.start, r.end), r.class]);
}

// ---- highlight ----------------------------------------------------------

same(
  classes("constant total: Int = 1 + 2.5 // a note\n"),
  [
    ["constant", "keyword"],
    ["total", "identifier"],
    [":", "punctuation"],
    ["Int", "identifier"],
    ["=", "operator"],
    ["1", "number"],
    ["+", "operator"],
    ["2.5", "number"],
    ["// a note", "comment"],
  ],
  "highlight(declaration)",
);

same(
  classes('"hi ${name}!" tasks.where(`status:open`)'),
  [
    ['"hi ${', "string"],
    ["name", "identifier"],
    ['}!"', "string"],
    ["tasks", "identifier"],
    [".", "punctuation"],
    ["where", "identifier"],
    ["(", "punctuation"],
    ["`status:open`", "filter"],
    [")", "punctuation"],
  ],
  "highlight(string interpolation and filter)",
);

// The offsets are UTF-16: the face is two code units, so `name` starts at 7,
// not at 9 (its byte offset) and not at 6 (its code point offset).
const emoji = '"\u{1f600}" + name';
const ranges = t.highlight(emoji);
const last = ranges[ranges.length - 1];
check(last.class === "identifier", `expected an identifier last, got ${JSON.stringify(last)}`);
check(
  last.start === 7 && last.end === 11 && emoji.slice(last.start, last.end) === "name",
  `expected name at [7, 11) in UTF-16 units, got ${JSON.stringify(last)}`,
);
const quoted = ranges[0];
check(
  quoted.start === 0 && quoted.end === 4 && quoted.class === "string",
  `expected the string at [0, 4), got ${JSON.stringify(quoted)}`,
);

// Text that does not parse still highlights, and a character the language
// has no token for is simply not covered.
same(
  classes("fn ( # name"),
  [
    ["fn", "keyword"],
    ["(", "punctuation"],
    ["name", "identifier"],
  ],
  "highlight(broken)",
);
same(t.highlight(""), [], "highlight(empty)");

// ---- keywords -----------------------------------------------------------

const keywords = t.keywords();
check(keywords.length === 27, `expected 27 keywords, got ${keywords.length}`);
same(
  keywords.map((k) => k.word),
  [...keywords.map((k) => k.word)].sort(),
  "keywords are alphabetical",
);
same(
  keywords.filter((k) => k.future).map((k) => k.word),
  ["assert", "test"],
  "keywords(future)",
);
for (const word of ["and", "false", "from", "not", "or", "true", "while"]) {
  check(keywords.some((k) => k.word === word && k.future === false), `missing keyword ${word}`);
}


// ---- standardCapabilities ------------------------------------------------

const standard = t.standardCapabilities();
same(
  standard.map((c) => (c.function ? `${c.name}.${c.function}` : c.name)),
  [
    "print",
    "clock.now",
    "clock.today",
    "clock.unixMillis",
    "clock.monotonicMillis",
    "random.int",
    "random.float",
  ],
  "standardCapabilities(names)",
);
same(standard[0], { name: "print", params: ["String"] }, "standardCapabilities(print)");
same(
  standard.find((c) => c.function === "int"),
  { name: "random", function: "int", params: ["Int", "Int"], result: "Int" },
  "standardCapabilities(random.int)",
);
// What it answers is accepted back as the `capabilities` argument, alongside
// a host's own declaration, and a program using both checks cleanly.
const both = [...standard, { name: "tasks", params: ["String"], result: "Int" }];
same(
  t.check(
    'uses print, tasks\neffect print("${effect tasks("x")}")\n',
    "main.hws",
    both,
  ),
  [],
  "check(standard + host capabilities)",
);

// ---- analyse ---------------------------------------------------------------

// An emoji before everything: two UTF-16 code units, four bytes.
const source = [
  'constant face = "\u{1f600}"',
  "constant total = 1 + 2",
  "fn double(n: Int): Int {",
  "    return n * 2",
  "}",
  "return double(total)",
  "",
].join("\n");

function sliceOf(range) {
  return source.slice(range.start, range.end);
}

const analysis = t.analyse(source);

// hover: the smallest expression at the offset, as UTF-16 half-open ranges.
const sum = analysis.hover(source.indexOf("+ 2"));
check(
  sum && sum.type === "Int" && sliceOf(sum) === "1 + 2",
  `hover at the plus: expected Int over "1 + 2", got ${JSON.stringify(sum)}`,
);
const call = analysis.hover(source.indexOf("(total)"));
check(
  call && call.type === "Int" && sliceOf(call) === "double(total)",
  `hover at the call: expected Int over "double(total)", got ${JSON.stringify(call)}`,
);
const callee = analysis.hover(source.indexOf("double(total)"));
check(
  callee && callee.type === "fn(Int): Int" && sliceOf(callee) === "double",
  `hover at the callee: expected fn(Int): Int over "double", got ${JSON.stringify(callee)}`,
);
same(analysis.hover(source.indexOf("constant total")), null, "hover(keyword)");

// completions: scope by declaration order, then the prelude, then keywords.
const inFunction = analysis.completions(source.indexOf("n * 2"));
const labels = inFunction.map((c) => c.label);
for (const label of ["face", "total", "double", "n", "sqrt", "Int", "return"]) {
  check(labels.includes(label), `completions inside the function: missing ${label}`);
}
const at = (label) => inFunction.find((c) => c.label === label);
same(at("n"), { label: "n", kind: "parameter", detail: "Int" }, "completion n");
same(
  at("double"),
  { label: "double", kind: "function", detail: "fn(n: Int): Int" },
  "completion double",
);
same(at("total"), { label: "total", kind: "constant", detail: "Int" }, "completion total");
same(at("face").detail, "String", "completion face detail");
same(at("sqrt").kind, "function", "completion sqrt kind");
same(at("Int").kind, "type", "completion Int kind");
same(at("return").kind, "keyword", "completion return kind");
check(
  labels.indexOf("face") < labels.indexOf("total") &&
    labels.indexOf("total") < labels.indexOf("double") &&
    labels.indexOf("double") < labels.indexOf("sqrt") &&
    labels.indexOf("sqrt") < labels.indexOf("return"),
  `completions are in declaration order, then prelude, then keywords: ${labels.slice(0, 6)}`,
);
// Keywords come from the lexer's table: every usable one, no reserved one.
for (const k of keywords) {
  check(labels.includes(k.word) === !k.future, `completion keyword ${k.word}`);
}
// A name is not in scope before its declaration, nor a parameter outside its
// function.
const early = analysis.completions(source.indexOf("constant total")).map((c) => c.label);
check(!early.includes("total"), "total offered before it is declared");
check(!early.includes("n"), "n offered outside its function");
// Nothing to complete inside a string.
same(analysis.completions(source.indexOf("\u{1f600}") + 1), [], "completions(in a string)");

// definition: in this module, and in an imported one.
const def = analysis.definition(source.indexOf("total)"));
check(
  def && def.file === "main.hws" && sliceOf(def) === "total",
  `definition of total: ${JSON.stringify(def)}`,
);
check(
  def.start === source.indexOf("total = 1 + 2"),
  `definition of total starts at the declaration, got ${def.start}`,
);
same(analysis.definition(source.indexOf("return double")), null, "definition(keyword)");
same(analysis.definition(source.indexOf("constant face")), null, "definition(constant keyword)");

const util = [
  "// \u{1f600}",
  "export constant limit: Int = 10",
  "export fn twice(n: Int): Int {",
  "    return n * 2",
  "}",
  "",
].join("\n");
const importing = [
  'import twice, limit from "./util"',
  "return twice(limit)",
  "",
].join("\n");
const withImport = t.analyse(importing, "main.hws", [], { "util.hws": util });
const twiceDef = withImport.definition(importing.indexOf("twice(limit)"));
check(
  twiceDef &&
    twiceDef.file === "util.hws" &&
    util.slice(twiceDef.start, twiceDef.end) === "twice",
  `definition of an imported function: ${JSON.stringify(twiceDef)}`,
);
const limitDef = withImport.definition(importing.indexOf("limit)"));
check(
  limitDef &&
    limitDef.file === "util.hws" &&
    util.slice(limitDef.start, limitDef.end) === "limit",
  `definition of an imported constant: ${JSON.stringify(limitDef)}`,
);
const imported = withImport.completions(importing.indexOf("return"));
same(
  imported.find((c) => c.label === "twice"),
  { label: "twice", kind: "function", detail: "fn(n: Int): Int" },
  "completion of an imported function",
);
same(
  imported.find((c) => c.label === "limit"),
  { label: "limit", kind: "constant", detail: "Int" },
  "completion of an imported constant",
);
withImport.dispose();

// documentSymbols: a tree with `uses` and `import` items, details, `exported`
// and UTF-16 ranges.
const outlineSource = [
  "// \u{1f600}",
  "uses print",
  'import twice from "./util"',
  "export fn run(n: Int): Int {",
  "    return twice(n)",
  "}",
  "record Point = { x: Int, y: [Int] }",
  "enum Shape { Circle(Float), Empty }",
  "variable count = 0",
  "",
].join("\n");
const outline = t.analyse(outlineSource, "main.hws", [], { "util.hws": util });
const symbols = outline.documentSymbols();
same(
  symbols.map((s) => [s.name, s.kind, s.detail, s.exported]),
  [
    ["print", "capability", "", false],
    ["./util", "module", "twice", false],
    ["run", "function", "fn(n: Int): Int", true],
    ["Point", "record", "", false],
    ["Shape", "enum", "", false],
    ["count", "variable", "Int", false],
  ],
  "documentSymbols(top level)",
);
const run = symbols[2];
check(
  outlineSource.slice(run.start, run.start + 13) === "export fn run" &&
    outlineSource.slice(run.selStart, run.selEnd) === "run" &&
    outlineSource.slice(run.start, run.end).endsWith("}"),
  `documentSymbols(run) ranges: ${JSON.stringify(run)}`,
);
same(
  symbols[3].children.map((c) => [c.name, c.kind, c.detail]),
  [
    ["x", "field", "Int"],
    ["y", "field", "[Int]"],
  ],
  "documentSymbols(record fields)",
);
same(
  symbols[4].children.map((c) => [c.name, c.kind, c.detail]),
  [
    ["Circle", "enumMember", "(Float)"],
    ["Empty", "enumMember", ""],
  ],
  "documentSymbols(enum members)",
);
check(
  outlineSource.slice(symbols[4].children[1].selStart, symbols[4].children[1].selEnd) === "Empty",
  "documentSymbols(enum member range)",
);
same(symbols[0].children, [], "documentSymbols(leaf children)");
outline.dispose();

// A program with errors analyses all the same: the answers are best effort
// over what checked.
const broken = t.analyse("constant ok = 1\nfn broken(: Int {\n");
check(
  broken.documentSymbols().some((s) => s.name === "ok"),
  "a program with a syntax error should still have an outline",
);
check(
  broken.completions(0).some((c) => c.label === "ok") === false,
  "ok is not in scope before its own declaration",
);
broken.dispose();

// A host capability declared to `analyse` is known to it.
const hostSource = 'uses tasks\nreturn effect tasks("x")\n';
const hosted = t.analyse(hostSource, "main.hws", [
  { name: "tasks", params: ["String"], result: "Int" },
]);
same(hosted.hover(hostSource.indexOf('("x")'))?.type, "Int", "hover(host capability call)");
hosted.dispose();

// ---- handles -------------------------------------------------------------

// dispose() is idempotent, and every query throws afterwards.
analysis.dispose();
analysis.dispose();
for (const [name, call] of [
  ["hover", () => analysis.hover(0)],
  ["completions", () => analysis.completions(0)],
  ["definition", () => analysis.definition(0)],
  ["documentSymbols", () => analysis.documentSymbols()],
]) {
  let threw = false;
  try {
    call();
  } catch (e) {
    threw = /disposed/.test(e.message);
  }
  check(threw, `${name}() after dispose should throw a disposed error`);
}

// An offset that is not a non-negative integer is refused here, rather than
// wrapping into a huge unsigned number inside the module.
const fresh = t.analyse("return 1\n");
for (const bad of [-1, 1.5, NaN, 2 ** 32]) {
  let threw = false;
  try {
    fresh.hover(bad);
  } catch (e) {
    threw = e instanceof RangeError;
  }
  check(threw, `hover(${bad}) should throw a RangeError`);
}
// An offset past the end is the end of the text, not an error.
same(fresh.hover(10_000), null, "hover(past the end)");
fresh.dispose();

// A handle is looked up by number: handles are not reused, so a stale
// analysis never reaches a newer one.
const a = t.analyse("constant a = 1\n");
a.dispose();
const b = t.analyse("constant b = 2\n");
let stale = false;
try {
  a.documentSymbols();
} catch (e) {
  stale = /disposed/.test(e.message);
}
check(stale, "a disposed analysis stays disposed after another is made");
same(b.documentSymbols().map((s) => s.name), ["b"], "the newer analysis is unaffected");
b.dispose();

// Malformed arguments are refused with a reason, and nothing is leaked: the
// limit of live analyses is never reached by failures.
for (let i = 0; i < 100; i += 1) {
  let threw = false;
  try {
    t.analyse("return 1", "main.hws", "not an array");
  } catch (e) {
    threw = /analyse\(\) failed/.test(e.message);
  }
  check(threw, "analyse with a malformed capability list should throw");
}

// The limit on live analyses is reported, and disposing makes room.
const held = [];
let limitMessage = null;
for (let i = 0; i < 200 && limitMessage === null; i += 1) {
  try {
    held.push(t.analyse("return 1\n"));
  } catch (e) {
    limitMessage = e.message;
  }
}
check(
  limitMessage !== null && /dispose/.test(limitMessage) && held.length === 64,
  `expected the limit of 64 live analyses to be reported, got ${held.length} and ${limitMessage}`,
);
held.pop().dispose();
t.analyse("return 1\n").dispose();
for (const h of held) {
  h.dispose();
}

console.log("all checks passed");
