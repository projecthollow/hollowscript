"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { windowsServerUri, windowsServerPath } = require("../../src/uri");

test("Windows documents and imports share the released server URI format", () => {
  const file = "D:\\a\\a space\\part#one\\café.hws";
  const uri = windowsServerUri(file);
  assert.equal(uri, "file://d:%5Ca%5Ca%20space%5Cpart%23one%5Ccaf%C3%A9.hws");
  assert.equal(windowsServerPath(uri), "d:\\a\\a space\\part#one\\café.hws");
});
test("UNC paths round trip without becoming a URI authority", () => {
  const file = "\\\\server\\share\\main.hws";
  const uri = windowsServerUri(file);
  assert.ok(uri);
  assert.equal(windowsServerPath(uri), file);
});
test("standard file and non-file URIs keep the standard parser", () => {
  for (const uri of ["file:///d%3A/a/main.hws", "file:///tmp/main.hws", "untitled:main.hws", "file://%invalid"]) {
    assert.equal(windowsServerPath(uri), undefined);
  }
  assert.equal(windowsServerUri("/tmp/main.hws"), undefined);
});
