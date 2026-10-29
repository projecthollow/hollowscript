"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const discovery = require("../../src/discovery");

/**
 * A fake host whose only executable files are `files`.
 * @param {Partial<import("../../src/discovery").DiscoveryHost & { realpath: (file: string) => string }>} overrides
 * @param {string[]} files
 */
function host(overrides, files) {
  const set = new Set(files);
  return {
    platform: /** @type {NodeJS.Platform} */ ("linux"),
    env: {},
    homedir: "/profile",
    isExecutableFile: (/** @type {string} */ f) => set.has(f),
    realpath: (/** @type {string} */ f) => f,
    ...overrides,
  };
}

test("PATH is searched first, in order", () => {
  const h = host({ env: { PATH: "/a:/b:/c" } }, ["/b/hollowscript", "/c/hollowscript", "/profile/.hollowscript/bin/hollowscript"]);
  const r = discovery.findServer(undefined, h);
  assert.equal(r.found, true);
  assert.equal(r.found && r.command, "/b/hollowscript");
  assert.equal(r.found && r.source, "PATH");
});

test("falls back to ~/.hollowscript/bin", () => {
  const h = host({ env: { PATH: "/usr/bin" } }, ["/profile/.hollowscript/bin/hollowscript"]);
  const r = discovery.findServer("", h);
  assert.deepEqual(r.found && [r.command, r.source], ["/profile/.hollowscript/bin/hollowscript", "install directory"]);
});

test("macOS and Linux: HOLLOWSCRIPT_HOME/bin is honoured, as install.sh honours it", () => {
  const h = host({ env: { PATH: "", HOLLOWSCRIPT_HOME: "/opt/hs" } }, ["/opt/hs/bin/hollowscript"]);
  const r = discovery.findServer(undefined, h);
  assert.equal(r.found && r.command, "/opt/hs/bin/hollowscript");
});

test("macOS and Linux: HOLLOWSCRIPT_INSTALL_DIR is install.ps1's, not install.sh's, so it is not searched", () => {
  const h = host({ env: { PATH: "", HOLLOWSCRIPT_INSTALL_DIR: "/opt/hs" } }, ["/opt/hs/hollowscript"]);
  const r = discovery.findServer(undefined, h);
  assert.equal(r.found, false);
});

test("Windows: HOLLOWSCRIPT_INSTALL_DIR is honoured, as install.ps1 honours it", () => {
  const env = { Path: "", LOCALAPPDATA: "D:\\L" };
  const h = host({ platform: "win32", env: { ...env, HOLLOWSCRIPT_INSTALL_DIR: "E:\\hs" }, homedir: "D:\\u" }, ["E:\\hs\\hollowscript.exe"]);
  const r = discovery.findServer(undefined, h);
  assert.equal(r.found && r.command, "E:\\hs\\hollowscript.exe");
});

test("an explicit path wins, and ~ is expanded", () => {
  const h = host({ env: { PATH: "/a" } }, ["/a/hollowscript", "/profile/dev/hollowscript"]);
  const r = discovery.findServer("~/dev/hollowscript", h);
  assert.deepEqual(r.found && [r.command, r.source], ["/profile/dev/hollowscript", "setting"]);
});

test("a wrong explicit path is reported, not silently replaced by PATH", () => {
  const h = host({ env: { PATH: "/a" } }, ["/a/hollowscript"]);
  const r = discovery.findServer("/nope/hollowscript", h);
  assert.equal(r.found, false);
  assert.equal(!r.found && r.configured, "/nope/hollowscript");
});

test("a bare name in the setting is looked up on PATH", () => {
  const h = host({ env: { PATH: "/a:/b" } }, ["/b/hollowscript-nightly"]);
  const r = discovery.findServer("hollowscript-nightly", h);
  assert.equal(r.found && r.command, "/b/hollowscript-nightly");
});

test("nothing found lists every place searched", () => {
  const h = host({ env: { PATH: "/a" } }, []);
  const r = discovery.findServer(undefined, h);
  assert.equal(r.found, false);
  assert.deepEqual(r.searched, ["/a/hollowscript", "/profile/.hollowscript/bin/hollowscript"]);
});

test("Windows: Path in any case, PATHEXT, and %LOCALAPPDATA%\\hollowscript\\bin", () => {
  const env = { Path: "C:\\tools;\"C:\\quoted dir\"", PATHEXT: ".COM;.EXE;.CMD", LOCALAPPDATA: "D:\\Profiles\\u\\AppData\\Local" };
  const onPath = host({ platform: "win32", env, homedir: "D:\\Profiles\\u" }, ["C:\\quoted dir\\hollowscript.exe"]);
  const hit = discovery.findServer(undefined, onPath);
  assert.equal(hit.found && hit.command, "C:\\quoted dir\\hollowscript.exe");

  const installed = host({ platform: "win32", env, homedir: "D:\\Profiles\\u" }, ["D:\\Profiles\\u\\AppData\\Local\\hollowscript\\bin\\hollowscript.exe"]);
  const r = discovery.findServer(undefined, installed);
  assert.equal(r.found && r.command, "D:\\Profiles\\u\\AppData\\Local\\hollowscript\\bin\\hollowscript.exe");
  assert.equal(r.found && r.source, "install directory");
});

test("the install command matches the platform", () => {
  assert.equal(discovery.installCommand("darwin"), "curl -fsSL https://get.hollowscript.org/install.sh | sh");
  assert.equal(discovery.installCommand("linux"), "curl -fsSL https://get.hollowscript.org/install.sh | sh");
  assert.equal(discovery.installCommand("win32"), "irm https://get.hollowscript.org/install.ps1 | iex");
});

test("the hollowscript-lsp hint mirrors the CLI's own search", () => {
  const sibling = host({ env: { PATH: "/x" }, realpath: () => "/real/bin/hollowscript" }, ["/real/bin/hollowscript-lsp"]);
  assert.equal(discovery.findLanguageServerBinary("/link/hollowscript", sibling), "/real/bin/hollowscript-lsp");

  const override = host({ env: { HOLLOWSCRIPT_LSP_PATH: "/o/lsp" } }, ["/o/lsp", "/b/hollowscript-lsp"]);
  assert.equal(discovery.findLanguageServerBinary("/b/hollowscript", override), "/o/lsp");

  const none = host({ env: { PATH: "/x" } }, []);
  assert.equal(discovery.findLanguageServerBinary("/b/hollowscript", none), undefined);
});
