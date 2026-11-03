#!/usr/bin/env node
// Opens the mcc usage panel at the bottom-left of the focused tab (the corner next to
// the sidebar's agent list), or closes it when that tab already has one. Panels register
// their pane id under the plugin's state dir, because Herdr's pane list doesn't say
// which plugin owns a pane. The panel sizes itself once it starts.
"use strict";
const { execFileSync } = require("node:child_process");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");

const HERDR = process.env.HERDR_BIN_PATH || "herdr";
const PLUGIN = process.env.HERDR_PLUGIN_ID || "mcc-usage";
const REGISTRY = path.join(process.env.HERDR_PLUGIN_STATE_DIR || path.join(os.tmpdir(), "mcc-usage"), "panes");

const herdr = (args) => JSON.parse(execFileSync(HERDR, args, { encoding: "utf8" })).result;

// The pane whose bottom-left corner is the tab's: leftmost column, lowest bottom edge.
function bottomLeft(layout) {
  const left = layout.panes.filter((p) => p.rect.x === layout.area.x);
  left.sort((a, b) => (b.rect.y + b.rect.height) - (a.rect.y + a.rect.height));
  return left[0] ?? null;
}

function main() {
  const snap = herdr(["api", "snapshot"]).snapshot;
  const byId = new Map(snap.panes.map((p) => [p.pane_id, p]));
  const here = byId.get(process.env.HERDR_PANE_ID) || byId.get(snap.focused_pane_id);
  const tab = here?.tab_id;

  const panels = [];
  for (const file of fs.existsSync(REGISTRY) ? fs.readdirSync(REGISTRY) : []) {
    const id = fs.readFileSync(path.join(REGISTRY, file), "utf8").trim();
    if (byId.has(id)) panels.push(byId.get(id));
    else fs.rmSync(path.join(REGISTRY, file), { force: true });
  }

  const open = tab ? panels.filter((p) => p.tab_id === tab) : [];
  if (open.length) {
    for (const p of open) herdr(["plugin", "pane", "close", p.pane_id]);
    return;
  }

  const layout = tab && snap.layouts.find((l) => l.tab_id === tab);
  const target = (layout && bottomLeft(layout)) || here;
  // Not --cwd: the pane's command is resolved from its working directory, which must stay the plugin root.
  const args = ["plugin", "pane", "open", "--plugin", PLUGIN, "--entrypoint", "panel",
    "--placement", "split", "--direction", "down", "--no-focus"];
  if (target) args.push("--target-pane", target.pane_id);
  herdr(args);
}

if (require.main === module) main();
else module.exports = { bottomLeft };
