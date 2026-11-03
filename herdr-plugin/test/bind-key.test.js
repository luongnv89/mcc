"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const b = require("../bind-key.js");

const SCRIPT = path.join(__dirname, "..", "bind-key.js");
const CONFIG = `[keys]
new_tab = "prefix+c"

[[keys.command]]
key = "prefix+i"
type = "plugin_action"
command = "herdr-github.toggle"
description = "GitHub panel"

[ui]
sidebar_width = 30
`;

test("boundKey finds the key of an action's binding", () => {
  assert.equal(b.boundKey(CONFIG, "herdr-github.toggle"), "prefix+i");
  assert.equal(b.boundKey(CONFIG, "mcc-usage.toggle"), null);
  assert.equal(b.boundKey("", "x.toggle"), null);
});

test("usedKeys reads bindings, defaults when commented, and digit ranges", () => {
  const used = b.usedKeys(CONFIG);
  assert.ok(used.has("prefix+c") && used.has("prefix+i"));
  assert.ok(!used.has("prefix+u"));
  const defaults = b.usedKeys('# new_tab = "prefix+c"\n# switch_tab = "prefix+1..9"\n# clear_pane = ""   # e.g. "prefix+ctrl+k"\n', true);
  assert.ok(defaults.has("prefix+c") && defaults.has("prefix+1") && defaults.has("prefix+9"));
  assert.ok(!defaults.has("prefix+ctrl+k"), "a key in a trailing comment is only an example");
  assert.ok(!b.usedKeys('# new_tab = "prefix+c"\n').has("prefix+c"), "a commented line in your config binds nothing");
});

test("pickKey takes the preferred key when free, else the next free one", () => {
  assert.equal(b.pickKey(new Set(), "prefix+u"), "prefix+u");
  assert.equal(b.pickKey(new Set(["prefix+u", "prefix+a"]), "prefix+u"), "prefix+b");
});

// End to end, against a throwaway config dir; the running server is left alone.
function run(xdg, ...args) {
  const env = { ...process.env, XDG_CONFIG_HOME: xdg, HERDR_BIND_KEY_NO_RELOAD: "1", HERDR_PLUGIN_ID: "mcc-usage" };
  delete env.HERDR_ENV;
  try { return { code: 0, out: execFileSync("node", [SCRIPT, ...args], { env, encoding: "utf8" }) }; }
  catch (e) { return { code: e.status, out: e.stdout }; }
}
const haveHerdr = (() => { try { execFileSync(process.env.HERDR_BIN_PATH || "herdr", ["--version"], { stdio: "ignore" }); return true; } catch { return false; } })();

test("binds prefix+u once, keeps a backup, and leaves the rest of the config alone", { skip: !haveHerdr && "herdr not installed" }, () => {
  const xdg = fs.mkdtempSync(path.join(os.tmpdir(), "bind-key-"));
  const file = path.join(xdg, "herdr", "config.toml");
  fs.mkdirSync(path.dirname(file));
  fs.writeFileSync(file, CONFIG);

  const first = run(xdg);
  assert.equal(first.code, 0, first.out);
  assert.match(first.out, /on prefix\+u/);
  const text = fs.readFileSync(file, "utf8");
  assert.ok(text.startsWith(CONFIG));
  assert.equal(b.boundKey(text, "mcc-usage.toggle"), "prefix+u");
  assert.equal(fs.readFileSync(`${file}.bak-mcc-usage`, "utf8"), CONFIG);

  const again = run(xdg);
  assert.match(again.out, /already on prefix\+u/);
  assert.equal(fs.readFileSync(file, "utf8"), text);
});

test("refuses a key that's taken", { skip: !haveHerdr && "herdr not installed" }, () => {
  const xdg = fs.mkdtempSync(path.join(os.tmpdir(), "bind-key-"));
  const file = path.join(xdg, "herdr", "config.toml");
  fs.mkdirSync(path.dirname(file));
  fs.writeFileSync(file, CONFIG);
  const r = run(xdg, "prefix+i");
  assert.equal(r.code, 1);
  assert.match(r.out, /prefix\+i is taken/);
  assert.equal(fs.readFileSync(file, "utf8"), CONFIG);
});

test("creates the config when there is none", { skip: !haveHerdr && "herdr not installed" }, () => {
  const xdg = fs.mkdtempSync(path.join(os.tmpdir(), "bind-key-"));
  const r = run(xdg);
  assert.equal(r.code, 0, r.out);
  assert.equal(b.boundKey(fs.readFileSync(path.join(xdg, "herdr", "config.toml"), "utf8"), "mcc-usage.toggle"), "prefix+u");
});
