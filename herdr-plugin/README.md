# mcc usage for Herdr

A [Herdr](https://herdr.dev) plugin that shows the 5-hour and weekly Claude Code usage
of every account [mcc](../README.md) manages, in a small pane at the bottom-left of the
tab, right under the sidebar's agent list.

```
 ▾ mcc usage                                        updated 4m ago  ↻ refresh
──────────────────────────────────────────────────────────────────────────────
 default    5h ██░░░░░░░░░░  14%   3h55    wk █░░░░░░░░░░░   7%   5d16h
 csdaisy    5h █░░░░░░░░░░░  12%   5m      wk ░░░░░░░░░░░░   2%   2d9h
 csluong    5h ············    —           wk ██░░░░░░░░░░  16%   5d16h  ⚠ 11h ago
 csluong: login expired · mcc login csluong
 r refresh · c collapse · q close
```

Each row shows a bar, the percent used and the time until the window resets. As in
`mcc usage`, `!` marks 80% or more and `!!` marks 95% or more. A ⚠ marks an account
whose last live lookup failed or whose login ends soon, with a line under the table
naming the `mcc login` command that fixes it. When the numbers are more than an hour
old, the row says how old they are.

## Install

You need Node 18 or later, Herdr 0.9.5 or later, and `mcc` on your `PATH`. If Herdr's
server can't find `mcc`, set `MCC_BIN` to its full path.

```bash
herdr plugin link /path/to/mcc/herdr-plugin
```

Then bind a key to it:

```bash
herdr plugin action invoke mcc-usage.bind-key
```

This adds `prefix+u` to `~/.config/herdr/config.toml` and reloads the config. If
`prefix+u` is taken, it uses the first free `prefix+` letter instead, and it says which
key it picked. If the panel already has a key, it changes nothing. It saves the old
config as `config.toml.bak-mcc-usage`. To choose the key yourself, run
`node bind-key.js prefix+x` in this folder, or add the binding by hand and run
`herdr server reload-config`:

```toml
[[keys.command]]
key = "prefix+u"
type = "plugin_action"
command = "mcc-usage.toggle"
description = "mcc usage panel"
```

Herdr plugins can't declare keys in their manifest, so linking or installing the
plugin doesn't bind one. Without a key, run `herdr plugin action invoke mcc-usage.toggle`.

## Use

| Key or click                 | Does                                           |
| ---------------------------- | ---------------------------------------------- |
| `r`, or click **↻ refresh**  | Look every account's usage up live             |
| `c`, Enter, or click the title | Collapse or expand                           |
| `↑` `↓` / wheel              | Scroll when the accounts don't fit             |
| `q`                          | Close the panel                                |

Running the toggle action again in the same tab also closes the panel.

**Collapse** shrinks the pane so the panes above get the space back. The title row
then shows the highest 5-hour and weekly percent across accounts. Herdr keeps a split
at least a tenth of its height, so a collapsed pane still shows a few rows. Expanding
fits the pane to its rows again, up to 60% of the split. The panel remembers whether
it was collapsed.

**Refresh** runs `mcc usage --all`, which looks up every account's usage live with
that account's own login and saves the result to mcc's cache. It sends each
account's OAuth token to Anthropic's undocumented, rate-limited usage endpoint, so it
only runs when you ask for it. For the details, see `mcc usage --help`. Between
refreshes, the panel rereads mcc's cache (`mcc usage --json-v2`, a local file read)
every 30 seconds. That picks up whatever `mcc statusline` caches as you work.

## Why a pane and not part of the sidebar

Herdr plugins (v1) can open terminal panes but can't draw inside Herdr's own sidebar.
So the panel is an ordinary split pane placed at the tab's bottom-left corner, next to
the bottom of the sidebar. You can move or resize it like any other pane.

## Development

```bash
node --test test/*.test.js
node panel.js            # run outside Herdr to see the panel in your terminal
```
