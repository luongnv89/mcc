#!/usr/bin/env node
// Binds a key to this plugin's toggle action in Herdr's config.toml, then reloads the
// config. Herdr's plugin manifest can't declare keybindings, so this action does it
// once, when you ask: it never rebinds a key that's taken and does nothing when the
// toggle already has a key. `node bind-key.js prefix+x` asks for a specific key.
"use strict";
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const HERDR = process.env.HERDR_BIN_PATH || "herdr";
const PLUGIN = process.env.HERDR_PLUGIN_ID || "mcc-usage";
const ACTION = `${PLUGIN}.toggle`;
const PREFERRED = "prefix+u";
const DESCRIPTION = "mcc usage panel";

const configPath = (env = process.env) =>
  path.join(env.XDG_CONFIG_HOME || path.join(os.homedir(), ".config"), "herdr", "config.toml");

// The key bound to `action` in a config, or null.
function boundKey(config, action) {
  const blocks = config.split(/^\s*\[\[keys\.command\]\]\s*$/m).slice(1);
  for (const block of blocks) {
    const body = block.split(/^\s*\[/m)[0];
    const command = /^\s*command\s*=\s*"([^"]*)"/m.exec(body)?.[1];
    if (command === action) return /^\s*key\s*=\s*"([^"]*)"/m.exec(body)?.[1] ?? null;
  }
  return null;
}

// Every key a config's bindings use, lowercased. Commented `name = "key"` lines count
// when `commented` is set: that's how `herdr --default-config` lists the defaults.
// "prefix+1..9" stands for prefix+1 to prefix+9.
function usedKeys(text, commented = false) {
  const keys = new Set();
  const line = commented ? /^\s*#?\s*[\w.]+\s*=\s*(.+)$/ : /^\s*[\w.]+\s*=\s*(.+)$/;
  for (const raw of text.split("\n")) {
    const m = line.exec(raw);
    if (!m) continue;
    for (const [, key] of m[1].replace(/#.*$/, "").matchAll(/"([^"]+)"/g)) {
      const k = key.trim().toLowerCase();
      if (!/^(prefix|ctrl|alt|shift|super|cmd)\+/.test(k)) continue;
      const range = /^(.*\+)1\.\.9$/.exec(k);
      if (range) for (let d = 1; d <= 9; d++) keys.add(range[1] + d);
      else keys.add(k);
    }
  }
  return keys;
}

// The preferred key when it's free, else the first free prefix+letter, else prefix+alt+letter.
function pickKey(used, preferred) {
  const letters = "abcdefghijklmnopqrstuvwxyz".split("");
  const candidates = [preferred, ...letters.map((l) => `prefix+${l}`), ...letters.map((l) => `prefix+alt+${l}`)];
  return candidates.find((k) => k && !used.has(k.toLowerCase())) ?? null;
}

const block = (key, action, description) =>
  `\n[[keys.command]]\nkey = "${key}"\ntype = "plugin_action"\ncommand = "${action}"\ndescription = "${description}"\n`;

// `herdr config check`: exit 0 and "config: ok", or exit 1 and the issues it found.
// A herdr that can't run the check counts as ok.
function checkConfig() {
  try {
    return { ok: true, text: execFileSync(HERDR, ["config", "check"], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }) };
  } catch (e) {
    if (e.status == null) return { ok: true, text: "" };
    return { ok: false, text: `${e.stdout || ""}${e.stderr || ""}` };
  }
}

function tell(title, body) {
  process.stdout.write(`${title}${body ? `: ${body}` : ""}\n`);
  if (!process.env.HERDR_ENV) return;
  try { execFileSync(HERDR, ["notification", "show", title, "--body", body || ""], { stdio: "ignore" }); } catch {}
}

function main() {
  const file = configPath();
  const config = fs.existsSync(file) ? fs.readFileSync(file, "utf8") : "";
  const existing = boundKey(config, ACTION);
  if (existing) return tell(`${DESCRIPTION}: already on ${existing}`, `set in ${file}`);

  let defaults = "";
  try { defaults = execFileSync(HERDR, ["--default-config"], { encoding: "utf8" }); } catch {}
  const used = new Set([...usedKeys(defaults, true), ...usedKeys(config)]);
  const asked = process.argv[2];
  if (asked && used.has(asked.toLowerCase())) {
    process.exitCode = 1;
    return tell(`${asked} is taken`, "pick another key");
  }
  const key = asked || pickKey(used, PREFERRED);
  if (!key) { process.exitCode = 1; return tell(`${DESCRIPTION}: no free key`, `add one to ${file} yourself`); }

  const before = checkConfig();
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const backup = `${file}.bak-${PLUGIN}`;
  if (config) fs.writeFileSync(backup, config);
  fs.writeFileSync(file, config + block(key, ACTION, DESCRIPTION));

  // A binding that gives the config a new problem goes back out. Problems the config
  // already had aren't this action's to fix.
  const after = checkConfig();
  if (!after.ok && after.text !== before.text) {
    if (config) fs.writeFileSync(file, config); else fs.rmSync(file, { force: true });
    process.exitCode = 1;
    const issue = after.text.split("\n").find((l) => l.trim() && !/^config:/.test(l)) ?? after.text;
    return tell(`${DESCRIPTION}: not bound`, `Herdr rejected it: ${issue.trim()}`);
  }

  // HERDR_BIND_KEY_NO_RELOAD=1 (the tests) leaves the running server alone.
  if (!process.env.HERDR_BIND_KEY_NO_RELOAD) try { execFileSync(HERDR, ["server", "reload-config"], { stdio: "ignore" }); } catch {}
  tell(`${DESCRIPTION} on ${key}`, `added to ${file}${config ? ` (backup: ${path.basename(backup)})` : ""}`);
}

if (require.main === module) main();
else module.exports = { boundKey, usedKeys, pickKey, block, configPath, checkConfig };
